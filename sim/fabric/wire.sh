#!/bin/bash
# The "PCIe link" between a host VM and its DPU VM when they run on different
# hypervisors (M4+): a bridge on each hypervisor joined by a VXLAN. Run it on
# both, with local/remote swapped. The host's igb NICs (hv side) or the DPU's
# wire NIC (DPU side) are then attached to <bridge> by libvirt.
#
# The bridge must act as a wire, not a switch: the DPU sends traffic between
# two of the host's VFs back down the same link, so a learning bridge would
# see the host's MACs on both sides and drop frames. ageing_time 0 makes it
# forget every MAC at once (it floods, which on a 2-3 port wire is free).
# The host NICs are isolated from each other by libvirt (m2b-switch-vm.py).
#
# usage: wire.sh up <bridge> <vni> <local-ip> <remote-ip> | wire.sh down <bridge>
set -euo pipefail

ACTION=${1:?usage: wire.sh up <bridge> <vni> <local-ip> <remote-ip> | down <bridge>}
BR=${2:?bridge}
VX=vx${BR#br-}

if [[ "${ACTION}" == down ]]; then
    ip link del "${VX}" 2>/dev/null || true
    ip link del "${BR}" 2>/dev/null || true
    exit 0
fi
VNI=${3:?vni} LOCAL=${4:?local-ip} REMOTE=${5:?remote-ip}

ip link show "${BR}" &>/dev/null || ip link add "${BR}" type bridge
ip link set "${BR}" type bridge ageing_time 0
ip link set "${BR}" up
if ! ip link show "${VX}" &>/dev/null; then
    ip link add "${VX}" mtu 1450 type vxlan id "${VNI}" local "${LOCAL}" remote "${REMOTE}" dstport 4789
fi
ip link set "${VX}" master "${BR}" up
echo "wire ${BR} <-> ${VX} (VNI ${VNI}, ${LOCAL} -> ${REMOTE})"
