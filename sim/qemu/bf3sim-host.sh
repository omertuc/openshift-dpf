#!/bin/bash
# M2b host side: make the patched-QEMU igb pair (p0/p1, functions .0/.1 of
# one slot, VPD with the DPU serial) look like a BlueField-3 to DPF's host
# components. The igb driver keeps seeing an igb (real PF/VF netdevs and
# SR-IOV); only the PFs' sysfs "device" file, which DPF matches on, is
# overlaid. Runs at boot (bf3sim-host.service), before kubelet.
set -euo pipefail

BF3_DEVICE_ID=0xa2dc
NUM_VFS=${NUM_VFS:-7}

mkdir -p /run/bf3sim
echo "${BF3_DEVICE_ID}" > /run/bf3sim/device

for netdev in p0 p1; do
    dev=$(readlink -f "/sys/class/net/${netdev}/device")
    if [[ ! -e "${dev}/vpd" ]]; then
        echo "${netdev} (${dev}) has no VPD; is the VM on the patched QEMU?" >&2
        exit 1
    fi
    if [[ $(cat "${dev}/sriov_numvfs") -eq 0 ]]; then
        echo "${NUM_VFS}" > "${dev}/sriov_numvfs"
    fi
    if ! mountpoint -q "${dev}/device"; then
        mount --bind /run/bf3sim/device "${dev}/device"
    fi
    # Tag each VF's traffic with its own VLAN so the DPU can tell the VFs
    # apart on the one wire (M4): VF N of p0 -> 100+N, of p1 -> 200+N.
    # VF 0 is a BlueField's host<->DPU channel and stays untagged.
    pf=${netdev#p}
    for vf in $(seq 1 $((NUM_VFS - 1))); do
        ip link set "${netdev}" vf "${vf}" vlan $(( (pf + 1) * 100 + vf ))
    done
    echo "${netdev}: ${dev} now reads $(cat "${dev}/device") with $(cat "${dev}/sriov_numvfs") VFs"
done
