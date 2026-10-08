//! What the DPU tools read from the management cluster (via `oc`, so they use
//! the same KUBECONFIG and login as everything else) and from bfb-registry.

use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;

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
