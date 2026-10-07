#!/bin/bash
# sim.sh - Simulated DPUs (M0): DPF host-trusted provisioning without hardware
#
# Uses NVIDIA's mock-dms (doca-platform test/mock/dms) in place of the hostagent
# and kwok to keep fake Nodes Ready:
# - fake host Nodes in the management cluster (kwok-controller in kube-system)
# - fake DPU Nodes in the hosted cluster (kwok-hosted, runs in the management
#   cluster because the hosted cluster has no workers)

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
SIM_NUM_HOSTS="${SIM_NUM_HOSTS:-2}"
SIM_HOST_PREFIX="${SIM_HOST_PREFIX:-sim-host}"
SIM_HOST_MCP="${SIM_HOST_MCP:-worker-dpu}"
SIM_DPU_NUM_SFS="${SIM_DPU_NUM_SFS:-64}"
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
        --set "certIPAddresses={${cp_ip}}"
    oc -n "${DPF_OPERATOR_NAMESPACE}" rollout status deployment -l app.kubernetes.io/instance=mock-dms --timeout=300s
}

# -----------------------------------------------------------------------------
# Fake host Nodes
# -----------------------------------------------------------------------------
create_sim_hosts() {
    local mock_dms_pod cp_ip
    mock_dms_pod=$(oc get pods -n "${DPF_OPERATOR_NAMESPACE}" \
        -l app.kubernetes.io/instance=mock-dms -o jsonpath='{.items[0].metadata.name}')
    cp_ip=$(get_control_plane_ip)
    # No MCD runs on a fake Node, so claim it is already on the pool's rendered
    # config; the maintenance operator needs the node's pool to pause it.
    local rendered
    rendered=$(oc get mcp "${SIM_HOST_MCP}" -o jsonpath='{.spec.configuration.name}')

    for i in $(seq 0 $((SIM_NUM_HOSTS - 1))); do
        local name="${SIM_HOST_PREFIX}-${i}"
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
  annotations:
    kwok.x-k8s.io/node: fake
    provisioning.dpu.nvidia.com/override-dms-pod-name: ${mock_dms_pod}
    machineconfiguration.openshift.io/currentConfig: ${rendered}
    machineconfiguration.openshift.io/desiredConfig: ${rendered}
    machineconfiguration.openshift.io/state: Done
EOF
        oc patch node "${name}" --subresource=status --type=merge \
            -p "{\"status\":{\"addresses\":[{\"type\":\"InternalIP\",\"address\":\"${cp_ip}\"}]}}"
    done
}

# -----------------------------------------------------------------------------
# Fake DPU node state in the hosted cluster
# -----------------------------------------------------------------------------
# What a real DPU provides and kwok cannot. Idempotent; run once the DPUs have
# joined the hosted cluster (phase "DPU Cluster Config" or later):
# - nvidia.com/bf_sf, normally advertised by the SF device plugin (HBN needs it)
# - an InternalIP outside the hosted cluster network (kwok uses its pod IP, which
#   the hosted API server rejects as an EndpointSlice address)
# - Ready ServiceInterfaces/ServiceChains, normally set by sfc-controller after
#   programming OVS; the host stays in maintenance until the chain is Ready
fake_dpu_node_state() {
    local hosted_kubeconfig secret host_ip now
    secret=$(get_dpucluster_kubeconfig_secret)
    hosted_kubeconfig=$(mktemp)
    oc get secret -n "${DPF_OPERATOR_NAMESPACE}" "${secret}" \
        -o jsonpath='{.data.super-admin\.conf}' | base64 -d > "${hosted_kubeconfig}"
    host_ip=$(get_control_plane_ip)
    now=$(date -u +%Y-%m-%dT%H:%M:%SZ)

    local node
    for node in $(oc --kubeconfig "${hosted_kubeconfig}" get nodes -l node-role.dpf.nvidia.com/fake=true -o name); do
        log "INFO" "Faking device resources and address on ${node}"
        oc --kubeconfig "${hosted_kubeconfig}" patch "${node}" --subresource=status --type=merge -p "{\"status\":{
            \"capacity\":{\"nvidia.com/bf_sf\":\"${SIM_DPU_NUM_SFS}\"},
            \"allocatable\":{\"nvidia.com/bf_sf\":\"${SIM_DPU_NUM_SFS}\"},
            \"addresses\":[{\"type\":\"InternalIP\",\"address\":\"${host_ip}\"}]}}"
    done

    local kind resource condition obj gen
    for kind in serviceinterface:ServiceInterfaceReconciled servicechain:ServiceChainReconciled; do
        resource=${kind%%:*}
        condition=${kind##*:}
        for obj in $(oc --kubeconfig "${hosted_kubeconfig}" get "${resource}" -n "${DPF_OPERATOR_NAMESPACE}" -o name); do
            gen=$(oc --kubeconfig "${hosted_kubeconfig}" get "${obj}" -n "${DPF_OPERATOR_NAMESPACE}" -o jsonpath='{.metadata.generation}')
            log "INFO" "Marking ${obj} Ready"
            oc --kubeconfig "${hosted_kubeconfig}" patch "${obj}" -n "${DPF_OPERATOR_NAMESPACE}" --subresource=status --type=merge -p "{\"status\":{
                \"observedGeneration\":${gen},
                \"conditions\":[
                  {\"type\":\"Ready\",\"status\":\"True\",\"reason\":\"Success\",\"message\":\"\",\"lastTransitionTime\":\"${now}\",\"observedGeneration\":${gen}},
                  {\"type\":\"${condition}\",\"status\":\"True\",\"reason\":\"Success\",\"message\":\"\",\"lastTransitionTime\":\"${now}\",\"observedGeneration\":${gen}}]}}"
        done
    done
    rm -f "${hosted_kubeconfig}"
}

delete_sim_hosts() {
    oc delete nodes -l dpf.openshift.io/sim-host=true --ignore-not-found
}

deploy_sim() {
    deploy_kwok_management
    deploy_mock_dms
    deploy_kwok_hosted
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
        fake-dpu-node-state)    fake_dpu_node_state ;;
        delete-sim-hosts)       delete_sim_hosts ;;
        deploy-sim)             deploy_sim ;;
        *)
            echo "Usage: $0 {deploy-sim|deploy-kwok-management|deploy-kwok-hosted|deploy-mock-dms|create-sim-hosts|fake-dpu-node-state|delete-sim-hosts}"
            exit 1
            ;;
    esac
fi
