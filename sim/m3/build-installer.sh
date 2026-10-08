#!/bin/bash
# Build a kexec-able RHCOS aarch64 installer that turns a machine into the DPU
# (for machines without hypervisor access; with it, use create-dpu-vm.sh)
# <dpu-name> (M3): fetch the DPU's bf.cfg from bfb-registry, turn it into a
# target ignition (build-ignition.sh), and embed it in the live initramfs so
# coreos-installer writes it to <dest-device> with nothing to fetch.
#
# usage: build-installer.sh <dpu-name> <mgmt-mac> [dest-device]
# env:   KUBECONFIG (management cluster), M3_DIR (live images; default .bin/m3),
#        BFB_REGISTRY (default http://<control-plane IP>:<bfb-registry NodePort>),
#        SSH_PUBKEY (added for core; default ~/.ssh/id_ed25519.pub),
#        SIM_WIRE_MAC (NIC linked to the host's PFs, M4)
# out:   $M3_DIR/<dpu-name>/{kernel,initramfs.img,sha256}
set -euo pipefail

DPU=$1
MGMT_MAC=$2
DEST=${3:-/dev/vda}
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
M3_DIR=${M3_DIR:-${HERE}/../../.bin/m3}

kernel=$(ls "${M3_DIR}"/rhcos-*-aarch64-live-kernel.aarch64 | tail -1)
initramfs=$(ls "${M3_DIR}"/rhcos-*-aarch64-live-initramfs.aarch64.img | tail -1)
rootfs=$(ls "${M3_DIR}"/rhcos-*-aarch64-live-rootfs.aarch64.img | tail -1)

out="${M3_DIR}/${DPU}"
mkdir -p "${out}"
chmod 700 "${out}"
"${HERE}/build-ignition.sh" "${DPU}" "${MGMT_MAC}" "${out}/dpu.ign"

cp "${kernel}" "${out}/kernel"
cp "${initramfs}" "${out}/live-initramfs.img"
podman run --rm -v "${out}:/data:Z" -w /data quay.io/coreos/coreos-installer:release \
    pxe customize --dest-device "${DEST}" --dest-ignition dpu.ign -o custom-initramfs.img live-initramfs.img >/dev/null
cat "${out}/custom-initramfs.img" "${rootfs}" > "${out}/initramfs.img"
rm -f "${out}/custom-initramfs.img" "${out}/live-initramfs.img"
(cd "${out}" && sha256sum kernel initramfs.img > sha256)
echo "${out}: kexec -s -l kernel --initrd=initramfs.img --append='ignition.firstboot ignition.platform.id=metal console=tty0 console=ttyAMA0,115200n8'"
