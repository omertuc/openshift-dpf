#!/bin/bash
# A simulated top-of-rack leaf for DPU VMs (M6): an FRR container on the
# hypervisor with one port (swpN) per DPU, each on its own bridge
# (br-dpufabN) that the DPU VM's p0 uplink NIC also sits on
# (create-dpu-vm.sh --fabric-bridge br-dpufabN). HBN on each DPU peers with it
# over BGP unnumbered (remote-as external), as with a real leaf switch, so the
# DPUs learn each other's loopbacks and OVN VTEP subnets.
#
# Run on the hypervisor: leaf.sh up <N> | leaf.sh down <N>   (N = number of DPU ports)
set -euo pipefail

ACTION=${1:?usage: leaf.sh up|down <ports>}
PORTS=${2:-2}
NAME=dpusim-leaf
IMAGE=${FRR_IMAGE:-quay.io/frrouting/frr:10.3.1}
CONF=/var/lib/dpusim-leaf
ASN=${LEAF_ASN:-65000}

if [[ "${ACTION}" == down ]]; then
    podman rm -f "${NAME}" >/dev/null 2>&1 || true
    for n in $(seq 0 $((PORTS - 1))); do
        ip link del "lf${n}" 2>/dev/null || true
        ip link del "br-dpufab${n}" 2>/dev/null || true
    done
    exit 0
fi

mkdir -p "${CONF}"
sed -i 's/^bgpd=no/bgpd=yes/' "${CONF}/daemons" 2>/dev/null || true
if [[ ! -f "${CONF}/daemons" ]]; then
    podman run --rm --entrypoint cat "${IMAGE}" /etc/frr/daemons | sed 's/^bgpd=no/bgpd=yes/' > "${CONF}/daemons"
fi
{
    echo "frr defaults datacenter"
    echo "hostname ${NAME}"
    echo "!"
    echo "router bgp ${ASN}"
    echo " bgp router-id 10.255.0.1"
    echo " bgp bestpath as-path multipath-relax"
    echo " neighbor fabric peer-group"
    echo " neighbor fabric remote-as external"
    for n in $(seq 0 $((PORTS - 1))); do
        echo " neighbor swp${n} interface peer-group fabric"
    done
    echo " address-family ipv4 unicast"
    echo "  redistribute connected"
    echo " exit-address-family"
    echo " address-family ipv6 unicast"
    echo "  neighbor fabric activate"
    echo " exit-address-family"
    echo "!"
} > "${CONF}/frr.conf"
touch "${CONF}/vtysh.conf"

# (Re)start first: a restart gets a fresh network namespace, and FRR picks up
# the ports attached afterwards at runtime.
podman rm -f "${NAME}" >/dev/null 2>&1 || true
podman run -d --name "${NAME}" --privileged --network none \
    -v "${CONF}:/etc/frr:Z" "${IMAGE}" >/dev/null
pid=$(podman inspect -f '{{.State.Pid}}' "${NAME}")

for n in $(seq 0 $((PORTS - 1))); do
    br=br-dpufab${n}
    ip link show "${br}" &>/dev/null || ip link add "${br}" mtu 9000 type bridge
    ip link set "${br}" up
    ip link del "lf${n}" 2>/dev/null || true
    ip link add "lf${n}" mtu 9000 type veth peer name "swp${n}" mtu 9000
    ip link set "lf${n}" master "${br}" up
    ip link set "swp${n}" netns "${pid}"
    nsenter -t "${pid}" -n ip link set "swp${n}" up
done
nsenter -t "${pid}" -n sysctl -qw net.ipv4.ip_forward=1 net.ipv6.conf.all.forwarding=1
ports=""
for n in $(seq 0 $((PORTS - 1))); do ports="${ports} swp${n}->br-dpufab${n}"; done
echo "leaf ${NAME} (AS ${ASN}) up with ports:${ports}"
