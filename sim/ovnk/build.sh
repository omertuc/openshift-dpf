#!/bin/bash
# Build the simulate-dpu OVN-K DPU image (see Containerfile).
#
# usage: build.sh <ovn-kubernetes checkout with simulate-dpu> <base image> <target image> [authfile]
#   base image: the arm64 DPU OVN-K image from the provisioner's "ovn"
#     DPUServiceTemplate (.spec.helmChart.values.dpuManifests.image)
#   authfile: pull secret for the base image (e.g. the cluster's
#     openshift-config/pull-secret)
set -euo pipefail

SRC=$1
BASE=$2
TARGET=$3
AUTH=${4:+--authfile=$4}
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

ctx=$(mktemp -d)
trap 'rm -rf "${ctx}"' EXIT
(
    cd "${SRC}/go-controller"
    GOTOOLCHAIN=${GOTOOLCHAIN:-go1.26.6} GOOS=linux GOARCH=arm64 GOFLAGS='' hack/build-go.sh \
        cmd/ovnkube cmd/ovn-k8s-cni-overlay hybrid-overlay/cmd/hybrid-overlay-node \
        cmd/ovnkube-trace cmd/ovnkube-identity cmd/ovnkube-observ
)
mkdir -p "${ctx}/bin"
cp "${SRC}"/go-controller/_output/go/bin/* "${ctx}/bin/"
commit=$(git -C "${SRC}" rev-parse HEAD)
# shellcheck disable=SC2086
podman build ${AUTH} --platform linux/arm64 -f "${HERE}/Containerfile" \
    --build-arg BASE_IMAGE="${BASE}" --build-arg SOURCE_COMMIT="${commit}" \
    -t "${TARGET}" "${ctx}"
podman push "${TARGET}"
