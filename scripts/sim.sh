#!/bin/bash
# sim.sh - Simulated DPUs (M0): DPF host-trusted provisioning without hardware
#
# Uses NVIDIA's mock-dms (doca-platform test/mock/dms) in place of the hostagent
# and kwok to keep fake Nodes Ready:
# - fake host Nodes in the management cluster (kwok-controller in kube-system)
# - fake DPU Nodes in the hosted cluster (kwok-hosted, runs in the management
#   cluster because the hosted cluster has no workers)
# Hosts are labeled dpf.openshift.io/sim-level (SIM_LEVEL):
# - m0: mock-dms creates the DPU Node directly
# - m1: sim-dpu joins it from the ignition DPF generated, through the DPF HCP
#   provisioner's CSR approval
# sim-dpu also stands in for the SF device plugin and sfc-controller on all
# simulated DPU Nodes.

set -e
set -o pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Standalone: env.sh and utils.sh require .env and aicli, which the simulation
# does not need.
log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] [$1] $2"
}

MANIFESTS_DIR="${MANIFESTS_DIR:-${SCRIPT_DIR}/../manifests}"

DPF_OPERATOR_NAMESPACE="${DPF_OPERATOR_NAMESPACE:-dpf-operator-system}"
SIM_DOCA_PLATFORM_DIR="${SIM_DOCA_PLATFORM_DIR:-${HOME}/repos/doca-platform}"
SIM_MOCK_DMS_IMAGE="${SIM_MOCK_DMS_IMAGE:-}"
SIM_DPU_IMAGE="${SIM_DPU_IMAGE:-}"
SIM_HOSTAGENT_IMAGE="${SIM_HOSTAGENT_IMAGE:-}"
SIM_NUM_HOSTS="${SIM_NUM_HOSTS:-2}"
SIM_HOST_PREFIX="${SIM_HOST_PREFIX:-sim-host}"
SIM_HOST_MCP="${SIM_HOST_MCP:-worker-dpu}"
# m0: mock-dms creates a kwok DPU Node; m1: sim-dpu joins it from the real ignition;
# m2a: m1 plus the real hostagent against fake hardware instead of mock-dms
SIM_LEVEL="${SIM_LEVEL:-m0}"
KWOK_VERSION="${KWOK_VERSION:-v0.8.0}"
KWOK_RELEASE_URL="https://github.com/kubernetes-sigs/kwok/releases/download/${KWOK_VERSION}"

# -----------------------------------------------------------------------------
# kwok
# -----------------------------------------------------------------------------
deploy_kwok_management() {
    log "INFO" "Deploying kwok ${KWOK_VERSION} to the management cluster"
    oc apply -f "${KWOK_RELEASE_URL}/kwok.yaml"
    oc apply -f "${KWOK_RELEASE_URL}/stage-fast.yaml"
    oc -n kube-system rollout status deployment/kwok-controller --timeout=300s
}

# The DPUCluster kubeconfig secret is injected by the provisioner operator.
get_dpucluster_kubeconfig_secret() {
    oc get dpucluster -n "${DPF_OPERATOR_NAMESPACE}" -o jsonpath='{.items[0].spec.kubeconfig}'
}

deploy_kwok_hosted() {
    local secret
    secret=$(get_dpucluster_kubeconfig_secret)
    if [[ -z "${secret}" ]]; then
        log "ERROR" "DPUCluster has no kubeconfig yet; is the DPFHCPProvisioner Ready?"
        return 1
    fi

    local hosted_kubeconfig
    hosted_kubeconfig=$(mktemp)
    oc get secret -n "${DPF_OPERATOR_NAMESPACE}" "${secret}" \
        -o jsonpath='{.data.super-admin\.conf}' | base64 -d > "${hosted_kubeconfig}"

    log "INFO" "Installing kwok CRDs and stages into the hosted cluster"
    oc --kubeconfig "${hosted_kubeconfig}" apply -f "${KWOK_RELEASE_URL}/kwok.yaml"
    oc --kubeconfig "${hosted_kubeconfig}" apply -f "${KWOK_RELEASE_URL}/stage-fast.yaml"
    # The hosted cluster has no workers, so its kwok-controller can never run.
    oc --kubeconfig "${hosted_kubeconfig}" -n kube-system scale deployment/kwok-controller --replicas=0
    rm -f "${hosted_kubeconfig}"

    log "INFO" "Deploying kwok-hosted in the management cluster"
    sed -e "s|<NAMESPACE>|${DPF_OPERATOR_NAMESPACE}|g" \
        -e "s|<KUBECONFIG_SECRET>|${secret}|g" \
        -e "s|<KWOK_VERSION>|${KWOK_VERSION}|g" \
        "${MANIFESTS_DIR}/sim/kwok-hosted.yaml" | oc apply -f -
    oc -n "${DPF_OPERATOR_NAMESPACE}" rollout status deployment/kwok-hosted --timeout=300s
}

# -----------------------------------------------------------------------------
# mock-dms
# -----------------------------------------------------------------------------
# mock-dms runs with hostNetwork on a control-plane node; fake host Nodes point
# their InternalIP at that node so the provisioning controller reaches it.
get_control_plane_ip() {
    oc get nodes -l node-role.kubernetes.io/control-plane \
        -o jsonpath='{.items[0].status.addresses[?(@.type=="InternalIP")].address}'
}

deploy_mock_dms() {
    if [[ -z "${SIM_MOCK_DMS_IMAGE}" ]]; then
        log "ERROR" "SIM_MOCK_DMS_IMAGE must be set (repository:tag)"
        return 1
    fi
    local chart="${SIM_DOCA_PLATFORM_DIR}/test/mock/dms/chart"
    local cp_ip
    cp_ip=$(get_control_plane_ip)

    log "INFO" "Deploying mock-dms ${SIM_MOCK_DMS_IMAGE} (cert IP ${cp_ip})"
    oc adm policy add-scc-to-user privileged -n "${DPF_OPERATOR_NAMESPACE}" -z mock-dms-controller-manager
    helm upgrade --install --namespace "${DPF_OPERATOR_NAMESPACE}" mock-dms "${chart}" \
        --set controllerManager.manager.image.repository="${SIM_MOCK_DMS_IMAGE%:*}" \
        --set controllerManager.manager.image.tag="${SIM_MOCK_DMS_IMAGE##*:}" \
        --set "certIPAddresses={${cp_ip}}" \
        --set-json 'extraArgs=["--skip-dpu-cluster-node-selector=dpf.openshift.io/sim-level=m1","--ignore-host-selector=dpf.openshift.io/sim-level in (m2a,m2b)"]'
    # hostNetwork on a single control-plane node: two replicas cannot coexist.
    oc -n "${DPF_OPERATOR_NAMESPACE}" patch deployment mock-dms-controller-manager --type=merge \
        -p '{"spec":{"strategy":{"type":"Recreate","rollingUpdate":null}}}'
    oc -n "${DPF_OPERATOR_NAMESPACE}" rollout status deployment/mock-dms-controller-manager --timeout=300s

    # Hosts name the mock-dms pod, which changes on every rollout.
    oc annotate nodes -l 'dpf.openshift.io/sim-host=true,dpf.openshift.io/sim-level in (m0,m1)' --overwrite \
        "provisioning.dpu.nvidia.com/override-dms-pod-name=$(get_mock_dms_pod)"
}

get_mock_dms_pod() {
    oc get pods -n "${DPF_OPERATOR_NAMESPACE}" -l app.kubernetes.io/instance=mock-dms \
        --field-selector=status.phase=Running -o jsonpath='{.items[0].metadata.name}'
}

# -----------------------------------------------------------------------------
# sim-dpu (M1)
# -----------------------------------------------------------------------------
deploy_sim_dpu() {
    if [[ -z "${SIM_DPU_IMAGE}" ]]; then
        log "ERROR" "SIM_DPU_IMAGE must be set (repository:tag)"
        return 1
    fi
    local secret
    secret=$(get_dpucluster_kubeconfig_secret)
    log "INFO" "Deploying sim-dpu ${SIM_DPU_IMAGE}"
    sed -e "s|<NAMESPACE>|${DPF_OPERATOR_NAMESPACE}|g" \
        -e "s|<KUBECONFIG_SECRET>|${secret}|g" \
        -e "s|<SIM_DPU_IMAGE>|${SIM_DPU_IMAGE}|g" \
        "${MANIFESTS_DIR}/sim/sim-dpu.yaml" | oc apply -f -
    oc -n "${DPF_OPERATOR_NAMESPACE}" rollout status deployment/sim-dpu --timeout=300s
}

# -----------------------------------------------------------------------------
# Real hostagent against fake hardware (M2a)
# -----------------------------------------------------------------------------
# Serial numbers must be unique (the DPUDevice is named after it) and 12
# characters long (the fake VPD encodes that length).
sim_serial() {
    printf 'MT26SIM%05d' $(( $(cksum <<< "$1" | cut -d' ' -f1) % 100000 ))
}

create_sim_hostagent() {
    local host=$1
    if [[ -z "${SIM_HOSTAGENT_IMAGE}" || -z "${SIM_DPU_IMAGE}" ]]; then
        log "ERROR" "SIM_HOSTAGENT_IMAGE and SIM_DPU_IMAGE must be set"
        return 1
    fi
    local mtu bridge
    mtu=$(oc get dpfoperatorconfig -n "${DPF_OPERATOR_NAMESPACE}" -o jsonpath='{.items[0].spec.networking.controlPlaneMTU}')
    bridge=$(oc get dpfoperatorconfig -n "${DPF_OPERATOR_NAMESPACE}" -o jsonpath='{.items[0].spec.networking.dpuNodeOOBBridgeName}')
    sed -e "s|<NAMESPACE>|${DPF_OPERATOR_NAMESPACE}|g" "${MANIFESTS_DIR}/sim/sim-hardware.yaml" | oc apply -f -
    log "INFO" "Creating hostagent for ${host} (serial $(sim_serial "${host}"))"
    sed -e "s|<NAMESPACE>|${DPF_OPERATOR_NAMESPACE}|g" \
        -e "s|<HOST>|${host}|g" \
        -e "s|<SERIAL>|$(sim_serial "${host}")|g" \
        -e "s|<MTU>|${mtu:-1500}|g" \
        -e "s|<OOB_BRIDGE>|${bridge:-br-dpu}|g" \
        -e "s|<HOSTAGENT_IMAGE>|${SIM_HOSTAGENT_IMAGE}|g" \
        -e "s|<SIM_DPU_IMAGE>|${SIM_DPU_IMAGE}|g" \
        "${MANIFESTS_DIR}/sim/hostagent-m2a.yaml" | oc apply -f -
}

# -----------------------------------------------------------------------------
# Fake host Nodes
# -----------------------------------------------------------------------------
create_sim_hosts() {
    local dms_pod cp_ip
    cp_ip=$(get_control_plane_ip)
    # No MCD runs on a fake Node, so claim it is already on the pool's rendered
    # config; the maintenance operator needs the node's pool to pause it.
    local rendered
    rendered=$(oc get mcp "${SIM_HOST_MCP}" -o jsonpath='{.spec.configuration.name}')

    for i in $(seq 0 $((SIM_NUM_HOSTS - 1))); do
        local name="${SIM_HOST_PREFIX}-${i}"
        if [[ "${SIM_LEVEL}" == m2a ]]; then
            create_sim_hostagent "${name}"
            dms_pod="sim-hostagent-${name}"
        else
            dms_pod=$(get_mock_dms_pod)
        fi
        log "INFO" "Creating fake host Node ${name}"
        oc apply -f - <<EOF
apiVersion: v1
kind: Node
metadata:
  name: ${name}
  labels:
    feature.node.kubernetes.io/dpu-enabled: ""
    feature.node.kubernetes.io/dpu-deviceID: "0xa2d6"
    feature.node.kubernetes.io/dpu-oob-bridge-configured: "true"
    node-role.kubernetes.io/worker: ""
    node-role.kubernetes.io/${SIM_HOST_MCP}: ""
    dpf.openshift.io/sim-host: "true"
    dpf.openshift.io/sim-level: ${SIM_LEVEL}
  annotations:
    kwok.x-k8s.io/node: fake
    provisioning.dpu.nvidia.com/override-dms-pod-name: ${dms_pod}
    machineconfiguration.openshift.io/currentConfig: ${rendered}
    machineconfiguration.openshift.io/desiredConfig: ${rendered}
    machineconfiguration.openshift.io/state: Done
EOF
        oc patch node "${name}" --subresource=status --type=merge \
            -p "{\"status\":{\"addresses\":[{\"type\":\"InternalIP\",\"address\":\"${cp_ip}\"}]}}"
    done
}

delete_sim_hosts() {
    oc delete nodes -l dpf.openshift.io/sim-host=true --ignore-not-found
    oc delete pods -n "${DPF_OPERATOR_NAMESPACE}" -l app=sim-hostagent --ignore-not-found
}

deploy_sim() {
    deploy_kwok_management
    deploy_mock_dms
    deploy_kwok_hosted
    deploy_sim_dpu
    create_sim_hosts
}

# -----------------------------------------------------------------------------
# Command dispatcher
# -----------------------------------------------------------------------------
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    case "${1:-}" in
        deploy-kwok-management) deploy_kwok_management ;;
        deploy-kwok-hosted)     deploy_kwok_hosted ;;
        deploy-mock-dms)        deploy_mock_dms ;;
        create-sim-hosts)       create_sim_hosts ;;
        deploy-sim-dpu)         deploy_sim_dpu ;;
        delete-sim-hosts)       delete_sim_hosts ;;
        deploy-sim)             deploy_sim ;;
        *)
            echo "Usage: $0 {deploy-sim|deploy-kwok-management|deploy-kwok-hosted|deploy-mock-dms|create-sim-hosts|deploy-sim-dpu|delete-sim-hosts}"
            exit 1
            ;;
    esac
fi
