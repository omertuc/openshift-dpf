#!/bin/bash
# Build and push NVIDIA's mock-dms (doca-platform test/mock/dms) with the
# simulation patches in this directory:
#   0001: --skip-dpu-cluster-node-selector (m1+: sim-dpu joins the DPU Node
#         from the real ignition instead of mock-dms creating it)
#   0002: --ignore-host-selector (m2a+: a real hostagent serves those hosts)
# Base: doca-platform public-main at ee931d25.
#
# usage: build.sh [image]   env: DOCA_PLATFORM_DIR (doca-platform checkout)
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IMAGE=${1:-quay.io/otuchfel/mock-dms:m2a}
SRC=${DOCA_PLATFORM_DIR:-${HOME}/repos/doca-platform}
BASE=ee931d25a436d77d78b28c38f00170bc190a3ece
work=$(mktemp -d)
trap 'git -C "${SRC}" worktree remove --force "${work}/src" 2>/dev/null || true; rm -rf "${work}"' EXIT
git -C "${SRC}" worktree add -q --detach "${work}/src" "${BASE}"
for p in "${HERE}"/0*.patch; do git -C "${work}/src" apply "${p}"; done
(cd "${work}/src" && podman build -f test/mock/dms/Dockerfile \
    --build-arg builder_image=docker.io/library/golang:1.26.6 \
    --build-arg base_image=gcr.io/distroless/static:nonroot \
    -t "${IMAGE}" .)
podman push "${IMAGE}"
