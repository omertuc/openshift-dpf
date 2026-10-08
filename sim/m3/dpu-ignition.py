#!/usr/bin/env python3
"""Turn the bf.cfg DPF generated for a DPU into an ignition config that boots a
plain aarch64 machine (VM or Arm server) as that DPU (M3).

On OpenShift the bf.cfg is the DPF HCP provisioner's "live" ignition, which a
BlueField installs from: it writes /etc/hostname and embeds the "target"
ignition the installed RHCOS boots with. A plain machine has no BlueField
hardware (devlink, SFs, rshim/tmfifo, NIC firmware), so this keeps the target
ignition as generated, adds the hostname, and masks only the units that need
that hardware. Everything else (kubelet bootstrap, MCO firstboot and the
rebase onto the BlueField OCP image) runs as on a real DPU.

It also stands in for what the masked units and the BlueField itself set up:
- dpf-ovs-sim.service builds DPF's OVS bridges (see dpf-ovs-sim.sh)
- with --mgmt-mac, the machine's NIC with that MAC becomes pf0vf0, the port of
  the br-comm-ch management bridge (on a BlueField, the representor of the
  host VF the hostagent bridges into the host network); br-comm-ch takes that
  MAC so DHCP keeps handing out the machine's address

  and pf0vf0 takes the MTU of the network the machine is on (--mgmt-mtu;
  inside a BlueField it is 9000)

- with --wire-mac, the NIC with that MAC is the link to the host (M4), a
  VLAN trunk that dpf-ovs-sim.sh splits into representors; NetworkManager
  leaves it and the representors alone

- with --host-node, the MAC OVN-K's --simulate-dpu mode expects on the host
  PF (52:54:00 + sha256(<host node>\0host)[0:2] + :00) goes into sim.env

usage: dpu-ignition.py [--mgmt-mac MAC [--mgmt-mtu MTU]] [--wire-mac MAC [--host-node NAME]] <bf.cfg> <out.ign>
"""
import argparse
import base64
import hashlib
import os
import gzip
import json
import sys
import urllib.parse

# Units that need BlueField hardware or the rshim/tmfifo link to the host.
HARDWARE_UNITS = {
    "tmfifo-agent-link.service",   # waits for the hostagent over tmfifo
    "setup-vfs-devlink.service",   # host VFs and devlink on the BlueField PFs
    "devlink-activate.service",
    "install-dpu-agent.service",   # fetches the dpu-agent over tmfifo
    "dpu-agent.service",
    "dpu-sf-gate.service",         # holds kubelet until the SFs exist
    "dpu-fw-upgrade.service",
    "pf-monitor.service",
    "bfupsignal.service",
    "dpf-ovs.service",             # OVS bridges over p0/pf0hpf
    "report-machineosurl.service",  # reports to the hostagent over tmfifo
}

# Drop-ins that make MCO firstboot require the units above.
HARDWARE_DROPINS = {
    "/etc/systemd/system/machine-config-daemon-firstboot.service.d/10-mcd-firstboot-dpuagent.conf",
    "/etc/systemd/system/machine-config-daemon-pull.service.d/10-require-setup-vfs.conf",
}


def file_contents(ign, path):
    for f in ign.get("storage", {}).get("files", []):
        if f["path"] != path:
            continue
        meta, data = f["contents"]["source"][len("data:"):].split(",", 1)
        raw = base64.b64decode(data) if meta.endswith(";base64") else urllib.parse.unquote_to_bytes(data)
        if f["contents"].get("compression") == "gzip":
            raw = gzip.decompress(raw)
        return raw
    raise KeyError(f"{path} not in ignition")


def data_url(raw):
    return "data:;base64," + base64.b64encode(raw).decode()


HERE = os.path.dirname(os.path.abspath(__file__))

OVS_SIM_UNIT = """[Unit]
Description=DPF OVS setup without BlueField hardware (simulation)
After=network.target openvswitch.service
Requires=openvswitch.service
# The boot image has no OVS; it comes with the node image MCO rebases onto.
ConditionPathExists=/usr/bin/ovs-vsctl

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/local/bin/dpf-ovs-sim.sh

[Install]
WantedBy=multi-user.target
"""


def add_file(files, path, raw, mode=0o644):
    files[:] = [f for f in files if f["path"] != path]
    files.append({"path": path, "mode": mode, "overwrite": True, "contents": {"source": data_url(raw)}})


def simulated_host_pf_mac(host_node):
    """The host gateway MAC OVN-K's SimulatedDPUOps derives for index 0."""
    h = hashlib.sha256(host_node.encode() + b"\x00host").digest()
    return f"52:54:00:{h[0]:02x}:{h[1]:02x}:00"


def main(bfcfg, out, mgmt_mac=None, mgmt_mtu=1500, wire_mac=None, host_node=None):
    with open(bfcfg) as f:
        live = json.load(f)
    target = json.loads(file_contents(live, "/var/target.ign"))
    hostname = file_contents(live, "/etc/hostname")

    files = target.setdefault("storage", {}).setdefault("files", [])
    files[:] = [f for f in files if f["path"] not in HARDWARE_DROPINS]
    add_file(files, "/etc/hostname", hostname)
    with open(os.path.join(HERE, "dpf-ovs-sim.sh"), "rb") as f:
        add_file(files, "/usr/local/bin/dpf-ovs-sim.sh", f.read(), 0o755)
    if mgmt_mac:
        add_file(files, "/etc/systemd/network/05-sim-pf0vf0.link",
                 f"[Match]\nMACAddress={mgmt_mac}\n\n[Link]\nName=pf0vf0\nNamePolicy=\n".encode())
        brcomm = "/etc/NetworkManager/system-connections/br-comm-ch.nmconnection"
        conf = file_contents(target, brcomm).decode()
        conf = conf.replace("cloned-mac-address=stable", f"cloned-mac-address={mgmt_mac}")
        add_file(files, brcomm, conf.encode(), 0o600)
        port = "/etc/NetworkManager/system-connections/pf0vf0.nmconnection"
        conf = file_contents(target, port).decode().replace("mtu=9000", f"mtu={mgmt_mtu}")
        add_file(files, port, conf.encode(), 0o600)

    units = target.setdefault("systemd", {}).setdefault("units", [])
    masked = []
    for i, u in enumerate(units):
        if u["name"] in HARDWARE_UNITS:
            units[i] = {"name": u["name"], "mask": True}
            masked.append(u["name"])
    if wire_mac:
        env = f"SIM_WIRE_MAC={wire_mac}\n"
        if host_node:
            env += f"SIM_HOST_PF_MAC={simulated_host_pf_mac(host_node)}\n"
        add_file(files, "/etc/dpf/sim.env", env.encode())
        add_file(files, "/etc/NetworkManager/conf.d/99-dpf-sim-unmanaged.conf",
                 ("[keyfile]\nunmanaged-devices=mac:" + wire_mac +
                  ";interface-name:dpuwire;interface-name:rep*;interface-name:pf0hpf-w\n").encode())
    units.append({"name": "dpf-ovs-sim.service", "enabled": True, "contents": OVS_SIM_UNIT})
    missing = HARDWARE_UNITS - set(masked)
    if missing:
        print(f"warning: not in the target ignition: {sorted(missing)}", file=sys.stderr)

    with open(out, "w") as f:
        json.dump(target, f)
    print(f"{out}: DPU {hostname.decode().strip()}, masked {len(masked)} hardware units")


if __name__ == "__main__":
    p = argparse.ArgumentParser(usage=__doc__)
    p.add_argument("--mgmt-mac")
    p.add_argument("--mgmt-mtu", type=int, default=1500)
    p.add_argument("--wire-mac")
    p.add_argument("--host-node")
    p.add_argument("bfcfg")
    p.add_argument("out")
    a = p.parse_args()
    main(a.bfcfg, a.out, a.mgmt_mac, a.mgmt_mtu, a.wire_mac, a.host_node)
