#!/usr/bin/env bash
# Builds and pushes DPF's ovs-cni with simulated SFs (arm64, for the DPUs): doca-platform at
# $VERSION with sim-sf.patch. openshift-dpf's SIMULATED_DPUS_ONLY points DPF at it
# (SIMULATED_DPU_OVS_CNI_IMAGE).
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
VERSION=${VERSION:-v26.4.1}
IMAGE=${IMAGE:-quay.io/otuchfel/sim-dpu:ovs-cni-simsf-veth2-arm64}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

src=$work/doca-platform
git -c advice.detachedHead=false clone -q --depth 1 --branch "$VERSION" https://github.com/NVIDIA/doca-platform "$src"
git -C "$src" apply "$here/sim-sf.patch"
(cd "$src/third_party/forked/ovs-cni" && CGO_ENABLED=0 GOOS=linux GOARCH=arm64 \
    go build -tags no_openssl -o "$work/ovs" ./cmd/plugin)
cp "$here/Containerfile" "$work/"
podman build -q --platform linux/arm64 -t "$IMAGE" "$work" >/dev/null
podman push -q "$IMAGE"
echo "$IMAGE"
