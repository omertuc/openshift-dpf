#!/bin/bash
# Build the ignition that boots a machine as the DPU <dpu-name>: fetch the
# DPU's bf.cfg from bfb-registry and turn it into a target ignition
# (dpu-ignition.py), plus an SSH key for core.
#
# usage: build-ignition.sh <dpu-name> <mgmt-mac> <out.ign>
# env:   KUBECONFIG (management cluster),
#        BFB_REGISTRY (default http://<control-plane IP>:<bfb-registry NodePort>),
#        SSH_PUBKEY (added for core; default ~/.ssh/id_ed25519.pub),
#        SIM_WIRE_MAC (NIC linked to the host's PFs, M4)
set -euo pipefail

DPU=$1
MGMT_MAC=$2
OUT=$3
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NS=dpf-operator-system
SSH_PUBKEY=${SSH_PUBKEY:-${HOME}/.ssh/id_ed25519.pub}

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
host_node=$(oc get dpu -n "${NS}" "${DPU}" -o jsonpath='{.spec.dpuNodeName}')

tmp=$(mktemp -d)
trap 'rm -rf "${tmp}"' EXIT
curl -sSf -m 60 -o "${tmp}/bf.cfg" "${BFB_REGISTRY}${bfcfg}"
python3 -I "${HERE}/dpu-ignition.py" --mgmt-mac "${MGMT_MAC}" \
    ${SIM_WIRE_MAC:+--wire-mac "${SIM_WIRE_MAC}" --host-node "${host_node}"} \
    "${tmp}/bf.cfg" "${OUT}"
python3 -I - "${OUT}" "${SSH_PUBKEY}" <<'EOF'
import json, sys
path, keyfile = sys.argv[1:]
ign = json.load(open(path))
key = open(keyfile).read().strip()
for u in ign["passwd"]["users"]:
    if u["name"] == "core" and key not in u.setdefault("sshAuthorizedKeys", []):
        u["sshAuthorizedKeys"].append(key)
json.dump(ign, open(path, "w"))
EOF
chmod 600 "${OUT}"
podman run --rm -i quay.io/coreos/ignition-validate:release - < "${OUT}"
