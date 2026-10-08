#!/bin/bash
# Build a kexec-able RHCOS aarch64 installer that turns a machine into the DPU
# <dpu-name> (M3): fetch the DPU's bf.cfg from bfb-registry, turn it into a
# target ignition (dpu-ignition.py), and embed it in the live initramfs so
# coreos-installer writes it to <dest-device> with nothing to fetch.
#
# usage: build-installer.sh <dpu-name> <mgmt-mac> [dest-device]
# env:   KUBECONFIG (management cluster), M3_DIR (live images; default .bin/m3),
#        BFB_REGISTRY (default http://<control-plane IP>:<bfb-registry NodePort>),
#        SSH_PUBKEY (added for core; default ~/.ssh/id_ed25519.pub)
# out:   $M3_DIR/<dpu-name>/{kernel,initramfs.img,sha256}
set -euo pipefail

DPU=$1
MGMT_MAC=$2
DEST=${3:-/dev/vda}
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
M3_DIR=${M3_DIR:-${HERE}/../../.bin/m3}
NS=dpf-operator-system
SSH_PUBKEY=${SSH_PUBKEY:-${HOME}/.ssh/id_ed25519.pub}

kernel=$(ls "${M3_DIR}"/rhcos-*-aarch64-live-kernel.aarch64 | tail -1)
initramfs=$(ls "${M3_DIR}"/rhcos-*-aarch64-live-initramfs.aarch64.img | tail -1)
rootfs=$(ls "${M3_DIR}"/rhcos-*-aarch64-live-rootfs.aarch64.img | tail -1)

if [[ -z "${BFB_REGISTRY:-}" ]]; then
    ip=$(oc get nodes -l node-role.kubernetes.io/control-plane \
        -o jsonpath='{.items[0].status.addresses[?(@.type=="InternalIP")].address}')
    port=$(oc get svc -n "${NS}" bfb-registry -o jsonpath='{.spec.ports[0].nodePort}')
    BFB_REGISTRY="http://${ip}:${port}"
fi

bfcfg=$(oc get dpu -n "${NS}" "${DPU}" -o jsonpath='{.status.bfCFGFile}')
if [[ -z "${bfcfg}" ]]; then
    echo "DPU ${DPU} has no bf.cfg yet (phase $(oc get dpu -n "${NS}" "${DPU}" -o jsonpath='{.status.phase}'))" >&2
    exit 1
fi

out="${M3_DIR}/${DPU}"
mkdir -p "${out}"
chmod 700 "${out}"
curl -sSf -m 60 -o "${out}/bf.cfg" "${BFB_REGISTRY}${bfcfg}"
python3 -I "${HERE}/dpu-ignition.py" --mgmt-mac "${MGMT_MAC}" "${out}/bf.cfg" "${out}/dpu.ign"
python3 -I - "${out}/dpu.ign" "${SSH_PUBKEY}" <<'EOF'
import json, sys
path, keyfile = sys.argv[1:]
ign = json.load(open(path))
key = open(keyfile).read().strip()
for u in ign["passwd"]["users"]:
    if u["name"] == "core" and key not in u.setdefault("sshAuthorizedKeys", []):
        u["sshAuthorizedKeys"].append(key)
json.dump(ign, open(path, "w"))
EOF
podman run --rm -i quay.io/coreos/ignition-validate:release - < "${out}/dpu.ign"

cp "${kernel}" "${out}/kernel"
cp "${initramfs}" "${out}/live-initramfs.img"
podman run --rm -v "${out}:/data:Z" -w /data quay.io/coreos/coreos-installer:release \
    pxe customize --dest-device "${DEST}" --dest-ignition dpu.ign -o custom-initramfs.img live-initramfs.img >/dev/null
cat "${out}/custom-initramfs.img" "${rootfs}" > "${out}/initramfs.img"
rm -f "${out}/custom-initramfs.img" "${out}/live-initramfs.img"
(cd "${out}" && sha256sum kernel initramfs.img > sha256)
echo "${out}: kexec -s -l kernel --initrd=initramfs.img --append='ignition.firstboot ignition.platform.id=metal console=tty0 console=ttyAMA0,115200n8'"
