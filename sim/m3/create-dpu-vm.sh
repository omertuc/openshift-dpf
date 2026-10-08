#!/bin/bash
# Create an aarch64 VM on a libvirt hypervisor that boots as the DPU
# <dpu-name> (M3/M4): stock RHCOS aarch64 disk image + the DPU's ignition via
# fw_cfg, BlueField-3 SMBIOS identity (HBN's platform check), a management
# NIC on the hypervisor's LAN bridge and, with --wire-bridge, a NIC on the
# bridge that links it to its host's PFs (M4).
#
# usage: create-dpu-vm.sh [options] <dpu-name>
#   --hypervisor SSH   ssh target of the aarch64 hypervisor (default: aarchv)
#   --name NAME        VM name (default: dpusim-<dpu-name>)
#   --vcpus N          (default: 8)
#   --memory MiB       (default: 16384)
#   --mgmt-bridge BR   LAN bridge on the hypervisor (default: mgmt-br)
#   --wire-bridge BR   bridge linked to the host's PFs (M4; default: none)
#   --delete           remove the VM and its disk/ignition instead
# env:   KUBECONFIG (management cluster); see build-ignition.sh
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HV=aarchv
NAME=""
VCPUS=8
MEMORY=16384
MGMT_BRIDGE=mgmt-br
WIRE_BRIDGE=""
DELETE=false
IMAGES=/var/lib/libvirt/images
RHCOS_STREAM=${RHCOS_STREAM:-4.22}

while [[ $# -gt 1 ]]; do
    case "$1" in
        --hypervisor) HV=$2; shift 2 ;;
        --name) NAME=$2; shift 2 ;;
        --vcpus) VCPUS=$2; shift 2 ;;
        --memory) MEMORY=$2; shift 2 ;;
        --mgmt-bridge) MGMT_BRIDGE=$2; shift 2 ;;
        --wire-bridge) WIRE_BRIDGE=$2; shift 2 ;;
        --delete) DELETE=true; shift ;;
        *) echo "unknown option $1" >&2; exit 1 ;;
    esac
done
DPU=${1:?usage: create-dpu-vm.sh [options] <dpu-name>}
NAME=${NAME:-dpusim-${DPU}}
# libvirt domain names are fine long; keep the disk/ignition names the same.
DISK=${IMAGES}/${NAME}.qcow2
IGN=${IMAGES}/${NAME}.ign

if ${DELETE}; then
    ssh "${HV}" "virsh destroy ${NAME} 2>/dev/null; virsh undefine --nvram ${NAME} 2>/dev/null; rm -f ${DISK} ${IGN}; echo removed ${NAME}"
    exit 0
fi

# Stable MACs per DPU so reruns keep their DHCP leases.
mac() {
    local h
    h=$(printf '%s/%s' "${DPU}" "$1" | sha256sum)
    echo "52:54:00:${h:0:2}:${h:2:2}:${h:4:2}"
}
MGMT_MAC=$(mac mgmt)
WIRE_MAC=$(mac wire)

ign=$(mktemp)
trap 'rm -f "${ign}"' EXIT
if [[ -n "${WIRE_BRIDGE}" ]]; then
    SIM_WIRE_MAC=${WIRE_MAC} "${HERE}/build-ignition.sh" "${DPU}" "${MGMT_MAC}" "${ign}"
else
    "${HERE}/build-ignition.sh" "${DPU}" "${MGMT_MAC}" "${ign}"
fi
scp -q "${ign}" "${HV}:${IGN}"

# The base image is shared by every DPU VM on the hypervisor (thin overlays).
ssh "${HV}" bash -s -- "${IMAGES}" "${RHCOS_STREAM}" <<'EOF'
set -euo pipefail
images=$1 stream=$2
base=${images}/rhcos-${stream}-aarch64-qemu.qcow2
if [[ ! -f ${base} ]]; then
    url=https://mirror.openshift.com/pub/openshift-v4/aarch64/dependencies/rhcos/${stream}/latest
    f=$(curl -sSf ${url}/sha256sum.txt | grep -oE "rhcos-[^ ]*qemu.aarch64.qcow2.gz" | head -1)
    curl -sSf ${url}/${f} | gunzip > ${base}.tmp && mv ${base}.tmp ${base}
fi
EOF

wire_net=""
if [[ -n "${WIRE_BRIDGE}" ]]; then
    wire_net="--network bridge=${WIRE_BRIDGE},model=virtio,mac=${WIRE_MAC}"
fi
# shellcheck disable=SC2087 # expand locally on purpose
ssh "${HV}" bash -s <<EOF
set -euo pipefail
# QEMU reads the ignition (it holds bootstrap credentials): qemu-owned, 0600.
chown qemu:qemu ${IGN}
chmod 0600 ${IGN}
chcon -t virt_content_t ${IGN}
qemu-img create -q -f qcow2 -F qcow2 -b ${IMAGES}/rhcos-${RHCOS_STREAM}-aarch64-qemu.qcow2 ${DISK} 60G
virt-install --name ${NAME} --arch aarch64 --machine virt --boot uefi --import \\
    --vcpus ${VCPUS} --memory ${MEMORY} \\
    --osinfo detect=on,require=off \\
    --disk path=${DISK},format=qcow2,bus=virtio \\
    --network bridge=${MGMT_BRIDGE},model=virtio,mac=${MGMT_MAC} ${wire_net} \\
    --sysinfo smbios,system.manufacturer=Nvidia,system.product=BlueField-3,baseBoard.manufacturer=Nvidia,baseBoard.product=BlueField-3 \\
    --qemu-commandline="-fw_cfg name=opt/com.coreos/config,file=${IGN}" \\
    --graphics none --noautoconsole
EOF
echo "VM ${NAME} on ${HV}: mgmt ${MGMT_MAC}${WIRE_BRIDGE:+, wire ${WIRE_MAC} on ${WIRE_BRIDGE}}"
echo "it joins the DPU cluster as Node ${DPU}; its address is that Node's InternalIP (DHCP on ${MGMT_BRIDGE})"
