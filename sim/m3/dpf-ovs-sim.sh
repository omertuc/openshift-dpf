#!/bin/bash
# DPF's DPU OVS setup (the DPUFlavor rawConfigScript, dpf-ovs.service) for a
# DPU without BlueField hardware: the same bridges and patches, on OVS's
# kernel datapath (OVS-DOCA's netdev datapath only takes DOCA, internal and
# patch ports), with veths for the ports a BlueField has.
#
# Host link (M4): with SIM_WIRE_MAC set (/etc/dpf/sim.env), the NIC with that
# MAC is the BlueField's PCIe side, a VLAN trunk to the host's PFs: untagged
# frames are the host PF (rep0-0), VLAN 100+N host p0 VF N
# (rep0-N), VLAN 200+N host p1 VF N (rep1-N). The host tags each VF's traffic
# (ip link set p0 vf N vlan 100+N), so a VLAN-filtering bridge turns the
# trunk into one netdev per representor. VF 0 is a BlueField's host<->DPU
# channel (br-comm-ch); here the machine's own NIC plays it, so representors
# start at VF 1.
set -euo pipefail

if [[ -r /etc/dpf/sim.env ]]; then
  . /etc/dpf/sim.env
fi
MTU=${MTU:-9000}
SIM_NUM_VFS=${SIM_NUM_VFS:-7}

_ovs-vsctl() {
  ovs-vsctl --timeout 15 "$@"
}

modprobe openvswitch

# veth <OVS-facing end> <other end>
veth() {
  ip link show "$1" &>/dev/null || ip link add "$1" mtu "${MTU}" type veth peer name "$2" mtu "${MTU}"
  ip link set "$1" up
  ip link set "$2" up
}

# The host PF's representor (pf0hpf on a BlueField). OVN-K's --simulate-dpu
# mode wants it as an OVS port named rep0-0 and derives the host gateway MAC
# from the host's node name instead of reading it; DPF's cniprovisioner (sim
# mode) looks for a netdev named pf0hpf and serves the host PF its DHCP lease
# by that MAC. So: rep0-0 is the OVS port, pf0hpf a dummy carrying the MAC,
# and both use SIM_HOST_PF_MAC, the MAC the host PF is given.
veth rep0-0 pf0hpf-w
ip link show pf0hpf &>/dev/null || ip link add pf0hpf type dummy
ip link set pf0hpf up
if [[ -n "${SIM_HOST_PF_MAC:-}" ]]; then
  ip link set rep0-0 address "${SIM_HOST_PF_MAC}"
  ip link set pf0hpf address "${SIM_HOST_PF_MAC}"
fi
for pf in 0 1; do
  for vf in $(seq 1 $((SIM_NUM_VFS - 1))); do
    veth "rep${pf}-${vf}" "rep${pf}-${vf}w"
  done
done

if [[ -n "${SIM_WIRE_MAC:-}" ]]; then
  wire=$(ip -o link | awk -v mac="${SIM_WIRE_MAC,,}" 'index(tolower($0), mac) {sub(":$", "", $2); print $2; exit}')
  if [[ -z "${wire}" ]]; then
    echo "no NIC with MAC ${SIM_WIRE_MAC}" >&2
    exit 1
  fi
  ip link show dpuwire &>/dev/null || ip link add dpuwire type bridge vlan_filtering 1
  ip link set dpuwire up
  ip link set "${wire}" master dpuwire
  ip link set "${wire}" up
  ip link set pf0hpf-w master dpuwire
  for pf in 0 1; do
    for vf in $(seq 1 $((SIM_NUM_VFS - 1))); do
      vid=$(( (pf + 1) * 100 + vf ))
      ip link set "rep${pf}-${vf}w" master dpuwire
      bridge vlan del dev "rep${pf}-${vf}w" vid 1 2>/dev/null || true
      bridge vlan add dev "rep${pf}-${vf}w" vid "${vid}" pvid untagged
      bridge vlan add dev "${wire}" vid "${vid}"
    done
  done
fi

# Fabric uplinks: a NIC already named p0/p1 (create-dpu-vm.sh --fabric-bridge,
# M6) is the uplink itself; otherwise a veth whose far end goes nowhere.
for port in p0 p1; do
  if ip -d link show "${port}" 2>/dev/null | grep -q " veth "; then
    ip link set "${port}" up
  elif ip link show "${port}" &>/dev/null; then
    ip link set "${port}" mtu "${MTU}" up 2>/dev/null || ip link set "${port}" up
  else
    veth "${port}" "${port}-fab"
  fi
done

_ovs-vsctl set Open_vSwitch . external-ids:ovn-bridge-datapath-type=system
for br in br-sfc br-hbn; do
  _ovs-vsctl --may-exist add-br "${br}" -- set bridge "${br}" datapath_type=system fail_mode=secure
done
for port in p0 p1; do
  _ovs-vsctl --may-exist add-port br-sfc "${port}" -- set Interface "${port}" type=system \
    -- set Port "${port}" external_ids:dpf-type=physical
done

# br-dpu is the bridge ovnkube manages (br-ex in OVN-K docs).
_ovs-vsctl --may-exist add-br br-dpu -- set bridge br-dpu datapath_type=system
_ovs-vsctl br-set-external-id br-dpu bridge-id br-dpu
_ovs-vsctl br-set-external-id br-dpu bridge-uplink pbrdputobrovn
_ovs-vsctl --may-exist add-port br-dpu rep0-0 -- set Interface rep0-0 type=system

# br-ovn sits between the sfc-controller managed br-sfc and OVN-K.
_ovs-vsctl --may-exist add-br br-ovn -- set bridge br-ovn datapath_type=system
_ovs-vsctl --may-exist add-port br-ovn pbrovntobrdpu -- set Interface pbrovntobrdpu type=patch options:peer=pbrdputobrovn
_ovs-vsctl --may-exist add-port br-dpu pbrdputobrovn -- set Interface pbrdputobrovn type=patch options:peer=pbrovntobrdpu

for br in br-ovn br-dpu; do
  ip link set "${br}" mtu "${MTU}" up
done
