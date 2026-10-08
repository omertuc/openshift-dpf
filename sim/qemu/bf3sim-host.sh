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
    # p0 VF 0 is a BlueField's host<->DPU channel and stays untagged; p1 VF 0
    # is an ordinary VF (200).
    pf=${netdev#p}
    for vf in $(seq $(( pf == 0 ? 1 : 0 )) $((NUM_VFS - 1))); do
        ip link set "${netdev}" vf "${vf}" vlan $(( (pf + 1) * 100 + vf ))
    done
    echo "${netdev}: ${dev} now reads $(cat "${dev}/device") with $(cat "${dev}/sriov_numvfs") VFs"
done

# The host PF p0 gets its address by DHCP from the DPU (M4), with the MAC
# OVN-K's --simulate-dpu mode expects for the host gateway, derived from the
# node name: 52:54:00 + sha256(<node>\0host)[0:2] + :00.
node=$(hostname -s)
h=$(printf '%s\0host' "${node}" | sha256sum)
gw_mac="52:54:00:${h:0:2}:${h:2:2}:00"
if ! nmcli -t -f NAME connection show | grep -qx p0; then
    nmcli connection add type ethernet ifname p0 con-name p0 ipv4.method auto ipv4.never-default yes \
        ipv4.dhcp-timeout 2147483647 ipv6.method disabled ethernet.cloned-mac-address "${gw_mac}" >/dev/null
fi
echo "p0: DHCP from the DPU as ${gw_mac}"
