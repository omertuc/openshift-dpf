#!/bin/bash
# Build the simulated-DPU ovn-kubernetes-dpf-utils image (arm64).
# usage: build.sh [image]   env: OVNK_DPF_DIR (ovn-kubernetes-dpf checkout)
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IMAGE=${1:-quay.io/otuchfel/sim-dpu:ovn-dpf-utils-simdpu-arm64}
SRC=${OVNK_DPF_DIR:-${HOME}/repos/ovn-kubernetes-dpf}
work=$(mktemp -d)
trap 'rm -rf "${work}"; git -C "${SRC}" worktree remove --force "${work}/src" 2>/dev/null || true' EXIT
git -C "${SRC}" worktree add -q --detach "${work}/src" v26.4.1-ocp-release-v4.22
git -C "${work}/src" apply "${HERE}/sim-dpu.patch"
(cd "${work}/src/dpf-utils" && CGO_ENABLED=0 GOOS=linux GOARCH=arm64 go build -trimpath -o "${work}/cniprovisioner" ./cmd/dpucniprovisioner)
cp "${HERE}/Containerfile" "${work}/"
podman build --platform linux/arm64 -t "${IMAGE}" "${work}"
podman push "${IMAGE}"
