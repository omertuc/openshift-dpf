#!/bin/bash
# Build and push DPF's ovs-cni with simulated SFs for arm64 DPUs.
# usage: build.sh <doca-platform checkout at v26.4.1 with sim-sf.patch applied> [image]
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC=$1/third_party/forked/ovs-cni
IMAGE=${2:-quay.io/otuchfel/sim-dpu:ovs-cni-simsf-arm64}
(cd "${SRC}" && CGO_ENABLED=0 GOOS=linux GOARCH=arm64 go build -tags no_openssl -o "${HERE}/ovs" ./cmd/plugin)
podman build --platform linux/arm64 -t "${IMAGE}" -f "${HERE}/Containerfile" "${HERE}"
podman push "${IMAGE}"
