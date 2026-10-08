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

usage: dpu-ignition.py <bf.cfg> <out.ign>
"""
import base64
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


def main(bfcfg, out):
    with open(bfcfg) as f:
        live = json.load(f)
    target = json.loads(file_contents(live, "/var/target.ign"))
    hostname = file_contents(live, "/etc/hostname")

    files = target.setdefault("storage", {}).setdefault("files", [])
    files[:] = [f for f in files if f["path"] not in HARDWARE_DROPINS]
    files.append({
        "path": "/etc/hostname",
        "mode": 0o644,
        "overwrite": True,
        "contents": {"source": data_url(hostname)},
    })

    units = target.setdefault("systemd", {}).setdefault("units", [])
    masked = []
    for i, u in enumerate(units):
        if u["name"] in HARDWARE_UNITS:
            units[i] = {"name": u["name"], "mask": True}
            masked.append(u["name"])
    missing = HARDWARE_UNITS - set(masked)
    if missing:
        print(f"warning: not in the target ignition: {sorted(missing)}", file=sys.stderr)

    with open(out, "w") as f:
        json.dump(target, f)
    print(f"{out}: DPU {hostname.decode().strip()}, masked {len(masked)} hardware units")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2])
