//! What the DPU tools read from the management cluster (via `oc`, so they use
//! the same KUBECONFIG and login as everything else) and from bfb-registry,
//! and the cleanup they do in the DPU cluster.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use base64::Engine;
use serde_json::Value;
use tempfile::NamedTempFile;

use crate::cmd::{CommandExt, command};

const DPF_NAMESPACE: &str = "dpf-operator-system";

fn oc_get_json<const N: usize>(args: [&str; N]) -> Result<Value> {
    let output = command("oc", ["get"])
        .args(args)
        .args(["-o", "json"])
        .read()?;
    serde_json::from_str(&output).context("parsing oc get output")
}

fn string_at(object: &Value, pointer: &str) -> Option<String> {
    object
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// The DPU object's fields the tools need.
pub struct DpuObject {
    /// bfb-registry path of the DPU's bf.cfg, once DPF has rendered it.
    pub bfcfg_path: Option<String>,
    pub phase: String,
    /// The host the DPU sits in.
    pub host_node: String,
}

pub fn get_dpu(dpu: &str) -> Result<DpuObject> {
    let dpu_object = oc_get_json(["dpu", "-n", DPF_NAMESPACE, dpu])?;
    Ok(DpuObject {
        bfcfg_path: string_at(&dpu_object, "/status/bfCFGFile").filter(|path| !path.is_empty()),
        phase: string_at(&dpu_object, "/status/phase").unwrap_or_default(),
        host_node: string_at(&dpu_object, "/spec/dpuNodeName")
            .with_context(|| format!("DPU {dpu} has no spec.dpuNodeName"))?,
    })
}

/// bfb-registry's NodePort on the first control-plane node.
pub fn bfb_registry_url() -> Result<String> {
    let control_plane = oc_get_json(["nodes", "-l", "node-role.kubernetes.io/control-plane"])?;
    let node_ip = control_plane
        .pointer("/items/0/status/addresses")
        .and_then(Value::as_array)
        .and_then(|addresses| {
            addresses
                .iter()
                .find(|address| address.get("type").and_then(Value::as_str) == Some("InternalIP"))
        })
        .and_then(|address| string_at(address, "/address"))
        .context("no control-plane node with an InternalIP")?;
    let service = oc_get_json(["svc", "-n", DPF_NAMESPACE, "bfb-registry"])?;
    let node_port = service
        .pointer("/spec/ports/0/nodePort")
        .and_then(Value::as_u64)
        .context("bfb-registry service has no nodePort")?;
    Ok(format!("http://{node_ip}:{node_port}"))
}

pub fn fetch_bfcfg(bfb_registry: &str, bfcfg_path: &str) -> Result<String> {
    let url = format!("{bfb_registry}{bfcfg_path}");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_mins(1)))
        .build()
        .into();
    agent
        .get(&url)
        .call()
        .and_then(|mut response| response.body_mut().read_to_string())
        .with_context(|| format!("fetching {url}"))
}

/// The DPU cluster's admin kubeconfig, from the secret its DPUCluster names,
/// in a temporary file.
fn dpu_cluster_kubeconfig() -> Result<NamedTempFile> {
    let dpu_clusters = oc_get_json(["dpucluster", "-n", DPF_NAMESPACE])?;
    let secret_name = string_at(&dpu_clusters, "/items/0/spec/kubeconfig")
        .context("no DPUCluster with spec.kubeconfig")?;
    let secret = oc_get_json(["secret", "-n", DPF_NAMESPACE, &secret_name])?;
    let encoded = string_at(&secret, "/data/super-admin.conf")
        .with_context(|| format!("secret {secret_name} has no super-admin.conf"))?;
    let kubeconfig = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("decoding the DPU cluster kubeconfig")?;
    let mut file = NamedTempFile::new()?;
    file.write_all(&kubeconfig)?;
    Ok(file)
}

fn dpu_cluster_oc<const N: usize>(kubeconfig: &Path, args: [&str; N]) -> std::process::Command {
    let mut oc = command("oc", ["--kubeconfig"]);
    oc.arg(kubeconfig).args(args);
    oc
}

/// What DPF keeps per DPU node in the DPU cluster, behind finalizers that only
/// that DPU's sfc-controller removes.
const PER_NODE_KINDS: [&str; 2] = [
    "serviceinterfaces.svc.dpu.nvidia.com",
    "servicechains.svc.dpu.nvidia.com",
];

/// Removes a DPU's Node from the DPU cluster, and the ServiceInterfaces and
/// ServiceChains DPF made for it. Those wait for the DPU's sfc-controller to
/// finalize them, which never happens once the DPU is gone; a new DPU of the
/// same name would finalize them instead, deleting its own fresh p0/p1 from
/// OVS (DPF then re-adds them as `type=dpdk`).
pub fn forget_dpu_node(dpu: &str) -> Result<()> {
    let kubeconfig = dpu_cluster_kubeconfig()?;
    let kubeconfig = kubeconfig.path();
    dpu_cluster_oc(
        kubeconfig,
        ["delete", "node", dpu, "--ignore-not-found", "--wait=false"],
    )
    .run()?;
    for kind in PER_NODE_KINDS {
        let output =
            dpu_cluster_oc(kubeconfig, ["get", kind, "-n", DPF_NAMESPACE, "-o", "json"]).read()?;
        let objects: Value = serde_json::from_str(&output).context("parsing oc get output")?;
        let names: Vec<String> = objects
            .pointer("/items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|object| string_at(object, "/spec/node").as_deref() == Some(dpu))
            .filter_map(|object| string_at(object, "/metadata/name"))
            .collect();
        for name in &names {
            let object = format!("{kind}/{name}");
            dpu_cluster_oc(
                kubeconfig,
                [
                    "delete",
                    &object,
                    "-n",
                    DPF_NAMESPACE,
                    "--ignore-not-found",
                    "--wait=false",
                ],
            )
            .run_silently()?;
            dpu_cluster_oc(
                kubeconfig,
                [
                    "patch",
                    &object,
                    "-n",
                    DPF_NAMESPACE,
                    "--type=merge",
                    "-p",
                    r#"{"metadata":{"finalizers":null}}"#,
                ],
            )
            .run_ignoring_failure()?;
        }
        println!("removed {} {kind} of {dpu}", names.len());
    }
    Ok(())
}

/// Deletes the DPU object; its DPUSet makes a new one, which DPF provisions
/// from the start (up to "DPU Cluster Config", where a new machine can join).
pub fn reprovision_dpu(dpu: &str) -> Result<()> {
    command(
        "oc",
        ["delete", "dpu", "-n", DPF_NAMESPACE, dpu, "--wait=false"],
    )
    .run()
}
