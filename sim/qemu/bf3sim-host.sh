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
    echo "${netdev}: ${dev} now reads $(cat "${dev}/device") with $(cat "${dev}/sriov_numvfs") VFs"
done
