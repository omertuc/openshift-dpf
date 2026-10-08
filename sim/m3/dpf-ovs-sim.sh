#!/bin/bash
# DPF's DPU OVS setup (the DPUFlavor rawConfigScript, dpf-ovs.service) for a
# DPU without BlueField hardware (M3): the same bridges and patches on the
# BlueField image's OVS-DOCA netdev datapath, without DOCA/DPDK ports or
# hardware offload. That datapath only takes DOCA, internal and patch ports,
# so the BlueField ports (p0, p1, pf0hpf) become internal ports: tap devices
# in the kernel that M4 wires to the host (pf0hpf) and the fabric (p0, p1).
set -euo pipefail

MTU=${MTU:-9000}

_ovs-vsctl() {
  ovs-vsctl --timeout 15 "$@"
}

_ovs-vsctl --may-exist add-br br-sfc -- set bridge br-sfc datapath_type=netdev fail_mode=secure
_ovs-vsctl --may-exist add-br br-hbn -- set bridge br-hbn datapath_type=netdev fail_mode=secure
_ovs-vsctl --may-exist add-port br-sfc p0 -- set Interface p0 type=internal mtu_request="${MTU}" \
  -- set Port p0 external_ids:dpf-type=physical
_ovs-vsctl --may-exist add-port br-sfc p1 -- set Interface p1 type=internal mtu_request="${MTU}" \
  -- set Port p1 external_ids:dpf-type=physical

_ovs-vsctl set Open_vSwitch . external-ids:ovn-bridge-datapath-type=netdev
# br-dpu is the bridge ovnkube manages (br-ex in OVN-K docs).
_ovs-vsctl --may-exist add-br br-dpu -- set bridge br-dpu datapath_type=netdev
_ovs-vsctl br-set-external-id br-dpu bridge-id br-dpu
_ovs-vsctl br-set-external-id br-dpu bridge-uplink pbrdputobrovn
_ovs-vsctl set Interface br-dpu mtu_request="${MTU}"
_ovs-vsctl --may-exist add-port br-dpu pf0hpf -- set Interface pf0hpf type=internal mtu_request="${MTU}"

# br-ovn sits between the sfc-controller managed br-sfc and OVN-K.
_ovs-vsctl --may-exist add-br br-ovn -- set bridge br-ovn datapath_type=netdev
_ovs-vsctl set Interface br-ovn mtu_request="${MTU}"
_ovs-vsctl --may-exist add-port br-ovn pbrovntobrdpu -- set Interface pbrovntobrdpu type=patch options:peer=pbrdputobrovn
_ovs-vsctl --may-exist add-port br-dpu pbrdputobrovn -- set Interface pbrdputobrovn type=patch options:peer=pbrovntobrdpu

for br in br-ovn br-dpu p0 p1 pf0hpf; do
  ip link set "${br}" up
done

# OVN-K's --simulate-dpu mode finds representors by name or alias: rep0-0 is
# the host PF's (pf0hpf on a BlueField).
ip link set pf0hpf alias rep0-0
