#!/bin/sh
# Simulated DPUs (SIMULATED_DPUS_ONLY): makes a DPU host VM's two PFs, the igb pair dpusimaas gives
# it (the only igb functions with VPD, which holds the DPU's serial), a BlueField-3's as DPF and
# OVN-Kubernetes see it. Runs at every boot, before kubelet (dpusim-bf3-host.service).
#   - Their PCI ids read as a BlueField-3's to whatever looks in sysfs (NFD, DPF's dpu-detector);
#     the igb driver still drives them.
#   - 7 VFs each, tagged for the one wire to the DPU: VLAN 100+N for PF0 VF N, 200+N for PF1 VF N.
#     PF0 VF 0 is the host<->DPU channel, which the DPU VM's own LAN NIC plays: it stays untagged.
#   - PF0 gets its address by DHCP from the DPU, with the MAC OVN-Kubernetes' --simulate-dpu
#     derives from the node name: 52:54:00 + sha256("<node>\0host")[0:2] + :00.
#   - The DPU's serial and dpusimaas' address go to /var/lib/dpf/dpusim/lease.env for the
#     simulated hostdriver (DPF's DMS pod, from the hostdriver-sim image).
set -eu
num_vfs=7
ids=/run/dpusim-bf3
mkdir -p "$ids" /var/lib/dpf/dpusim
echo 0x15b3 > "$ids/vendor"
echo 0xa2dc > "$ids/device"

pf=0
for dev in /sys/bus/pci/devices/*; do
    [ -e "$dev/vpd" ] && [ "$(basename "$(readlink -f "$dev/driver")")" = igb ] || continue
    netdev=$(ls "$dev/net")
    if [ "$pf" = 0 ]; then
        pf0=$netdev
        # VPD-R keyword SN: "SN", its length, the serial.
        serial=$(od -An -v -tx1 "$dev/vpd" | awk '
            { for (i = 1; i <= NF; i++) b[n++] = $i }
            END { for (i = 0; i + 2 < n; i++) if (b[i] == "53" && b[i + 1] == "4e") {
                len = strtonum("0x" b[i + 2]); for (j = 1; j <= len; j++) printf "%c", strtonum("0x" b[i + 2 + j]); exit } }')
    fi
    [ "$(cat "$dev/sriov_numvfs")" != 0 ] || echo "$num_vfs" > "$dev/sriov_numvfs"
    for id in vendor device; do
        mountpoint -q "$dev/$id" || mount --bind "$ids/$id" "$dev/$id"
    done
    vf=$((pf == 0 ? 1 : 0))
    while [ "$vf" -lt "$num_vfs" ]; do
        ip link set "$netdev" vf "$vf" vlan $(((pf + 1) * 100 + vf))
        vf=$((vf + 1))
    done
    echo "PF$pf $netdev: $(basename "$dev") reads $(cat "$dev/vendor"):$(cat "$dev/device"), $num_vfs VFs"
    pf=$((pf + 1))
done
[ "$pf" = 2 ] || { echo "found $pf of the 2 simulated BlueField PFs (igb with VPD)" >&2; exit 1; }

node=$(hostname -s)
mac=$(printf '%s\0host' "$node" | sha256sum | sed 's/^\(..\)\(..\).*/52:54:00:\1:\2:00/')
if ! nmcli -t -f NAME connection show | grep -qx dpusim-pf0; then
    nmcli connection add type ethernet ifname "$pf0" con-name dpusim-pf0 \
        ipv4.method auto ipv4.never-default yes ipv4.dhcp-timeout 2147483647 \
        ipv6.method disabled ethernet.cloned-mac-address "$mac" >/dev/null
fi
echo "PF0 $pf0: DHCP from the DPU as $mac (node $node)"

. /etc/dpusim/dpusimaas.env
printf 'DPUSIMAAS_URL=%s\nDPUSIM_SERIAL=%s\n' "$DPUSIMAAS_URL" "$serial" > /var/lib/dpf/dpusim/lease.env
echo "DPU $serial, dpusimaas at $DPUSIMAAS_URL"
