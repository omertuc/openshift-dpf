//! Turns the bf.cfg DPF generated for a DPU into an ignition config that boots
//! a plain aarch64 machine (VM or Arm server) as that DPU.
//!
//! On OpenShift the bf.cfg is the DPF HCP provisioner's "live" ignition, which
//! a BlueField installs from: it writes /etc/hostname and embeds the "target"
//! ignition the installed RHCOS boots with. A plain machine has no BlueField
//! hardware (devlink, SFs, rshim/tmfifo, NIC firmware), so this keeps the
//! target ignition as generated, adds the hostname, and masks only the units
//! that need that hardware. Everything else (kubelet bootstrap, MCO firstboot
//! and the rebase onto the BlueField OCP image) runs as on a real DPU.
//!
//! It also stands in for what the masked units and the BlueField itself set up:
//! - `dpf-ovs-sim.service` runs `dpusim dpu-ovs`, which builds DPF's OVS bridges,
//!   and `dpf-ovs-sim-system-ports.service` keeps DPF from turning its ports
//!   into `dpdk` ones (`dpu-ovs --keep-system-ports`);
//!   the aarch64 dpusim binary goes into the ignition for both
//! - with a management NIC, the machine's NIC with that MAC becomes pf0vf0, the
//!   port of the br-comm-ch management bridge (on a BlueField, the
//!   representor of the host VF the hostagent bridges into the host network);
//!   br-comm-ch takes that MAC so DHCP keeps handing out the machine's
//!   address, and pf0vf0 takes the MTU of the network the machine is on
//!   (inside a BlueField it is 9000)
//! - with a wire NIC, the NIC with that MAC is the link to the host, a
//!   VLAN trunk that `dpu-ovs` splits into representors; NetworkManager leaves
//!   it and the representors alone. With the host's node name, the MAC OVN-K's
//!   `--simulate-dpu` mode expects on the host PF goes into sim.env
//! - with a tmfifo NIC, the NIC with that MAC is the rshim tmfifo link to the
//!   host (host-trusted): named tmfifo_net0, DPF's own units on it fetch and run
//!   the dpu-agent from the host agent, as on a BlueField, so they stay
//! - with an uplink NIC, the NIC with that MAC is the BlueField's p0 uplink to the leaf router
//!   (see leaf): named p0, it is the port `dpu-ovs` puts in br-sfc, and HBN peers with the leaf
//!   over it; NetworkManager leaves it alone
//! - mlxconfig answers DPF's checks as a BlueField in DPU mode would (see agent_hw)
//! - a static pod advertises simulated SFs (`nvidia.com/bf_sf`, device IDs `sim-sf-N`), which
//!   services like HBN request; the patched ovs-cni plugs them (openshift-dpf `sim/ovs-cni`)

use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use clap::Args;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde_json::{Map, Value, json};

use crate::agent_hw;
use crate::dpu_ovs::{SIM_ENV_PATH, SIM_HOST_PF_MAC, SIM_WIRE_MAC};
use crate::mac::{parse_mac, simulated_host_pf_mac};

/// Units that need BlueField hardware or the rshim/tmfifo link to the host.
const HARDWARE_UNITS: [&str; 11] = [
    "tmfifo-agent-link.service", // waits for the hostagent over tmfifo
    "setup-vfs-devlink.service", // host VFs and devlink on the BlueField PFs
    "devlink-activate.service",
    "install-dpu-agent.service", // fetches the dpu-agent over tmfifo
    "dpu-agent.service",
    "dpu-sf-gate.service", // holds kubelet until the SFs exist
    "dpu-fw-upgrade.service",
    "pf-monitor.service",
    "bfupsignal.service",
    "dpf-ovs.service",             // OVS bridges over p0/pf0hpf
    "report-machineosurl.service", // reports to the hostagent over tmfifo
];

/// Drop-ins that make MCO firstboot require the units above.
const HARDWARE_DROPINS: [&str; 2] = [
    AGENT_DROPIN,
    "/etc/systemd/system/machine-config-daemon-pull.service.d/10-require-setup-vfs.conf",
];

/// In Zero Trust the dpu-agent needs no host: it comes from the bfb-registry and talks to the
/// management cluster itself, and tmfifo-agent-link.service returns at once. Host-trusted, it
/// comes from and talks to the host agent over tmfifo. A DPU machine that has its agent's way
/// out (Zero Trust, or a tmfifo NIC) keeps these.
const AGENT_UNITS: [&str; 3] = [
    "tmfifo-agent-link.service",
    "install-dpu-agent.service",
    "dpu-agent.service",
];
/// What else a DPU machine with a tmfifo NIC keeps.
const TMFIFO_UNITS: [&str; 1] = ["report-machineosurl.service"];
const AGENT_DROPIN: &str =
    "/etc/systemd/system/machine-config-daemon-firstboot.service.d/10-mcd-firstboot-dpuagent.conf";

const BR_COMM_CH_CONNECTION: &str =
    "/etc/NetworkManager/system-connections/br-comm-ch.nmconnection";
const PF0VF0_CONNECTION: &str = "/etc/NetworkManager/system-connections/pf0vf0.nmconnection";
const DPUSIM_ON_DPU: &str = "/usr/local/bin/dpusim";

const OVS_SIM_UNIT: &str = "[Unit]
Description=DPF OVS setup without BlueField hardware (simulation)
After=network.target openvswitch.service
Requires=openvswitch.service
# The boot image has no OVS; it comes with the node image MCO rebases onto.
ConditionPathExists=/usr/bin/ovs-vsctl

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/local/bin/dpusim dpu-ovs

[Install]
WantedBy=multi-user.target
";

const OVS_SIM_SYSTEM_PORTS_UNIT: &str = "[Unit]
Description=Keep DPF's OVS ports plain (system) ports (simulation)
After=dpf-ovs-sim.service
Requires=openvswitch.service
ConditionPathExists=/usr/bin/ovs-vsctl

[Service]
ExecStart=/usr/local/bin/dpusim dpu-ovs --keep-system-ports
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
";

/// The SFs a BlueField has for DPF's services (sim/sf-device-plugin).
const SF_DEVICE_PLUGIN_POD: &str = "/etc/kubernetes/manifests/dpusim-sf-device-plugin.yaml";
const SF_DEVICE_PLUGIN_POD_YAML: &str = "apiVersion: v1
kind: Pod
metadata:
  name: dpusim-sf-device-plugin
  namespace: kube-system
spec:
  hostNetwork: true
  priorityClassName: system-node-critical
  securityContext:
    runAsUser: 0
  containers:
  - name: plugin
    image: quay.io/otuchfel/sim-dpu:sf-device-plugin
    args: [\"--count=16\"]
    securityContext:
      privileged: true
    volumeMounts:
    - name: device-plugins
      mountPath: /var/lib/kubelet/device-plugins
  volumes:
  - name: device-plugins
    hostPath:
      path: /var/lib/kubelet/device-plugins
";

/// The NICs of the machine that plays the DPU, and the dpusim binary it runs.
#[derive(Args, Clone)]
pub struct DpuMachineArgs {
    /// The NIC that becomes pf0vf0, the management port.
    #[arg(long, value_parser = parse_mac)]
    pub mgmt_mac: Option<String>,
    /// MTU of the network the management NIC is on.
    #[arg(long, default_value_t = 1500)]
    pub mgmt_mtu: u32,
    /// The NIC linked to the host's PFs.
    #[arg(long, value_parser = parse_mac)]
    pub wire_mac: Option<String>,
    /// The NIC that is the rshim tmfifo link to the host agent (host-trusted).
    #[arg(long, value_parser = parse_mac)]
    pub tmfifo_mac: Option<String>,
    /// The NIC that is the p0 uplink to the leaf router.
    #[arg(long, value_parser = parse_mac)]
    pub uplink_mac: Option<String>,
    /// The aarch64 dpusim binary to install on the DPU. Default: this binary
    /// on aarch64, else target/aarch64-unknown-linux-musl/release/dpusim of
    /// this crate.
    #[arg(long, env = "DPUSIM_DPU_BINARY")]
    pub dpu_binary: Option<PathBuf>,
}

/// The ignition a DPU machine boots with, and what went into it.
pub struct DpuIgnition {
    pub ignition: Value,
    hostname: String,
    masked_unit_count: usize,
}

impl DpuIgnition {
    pub fn summary(&self) -> String {
        format!(
            "DPU {}, masked {} hardware units",
            self.hostname, self.masked_unit_count
        )
    }
}

/// Builds the DPU machine's ignition from DPF's bf.cfg (`bfcfg`, JSON).
pub fn dpu_ignition(
    bfcfg: &str,
    machine: &DpuMachineArgs,
    host_node: Option<&str>,
) -> Result<DpuIgnition> {
    let live: Value = serde_json::from_str(bfcfg).context("parsing the bf.cfg as an ignition")?;
    let target_text = file_contents(&live, "/var/target.ign")?;
    let mut target: Value =
        serde_json::from_slice(&target_text).context("parsing /var/target.ign")?;
    let hostname = file_contents(&live, "/etc/hostname")?;
    let dpusim_binary = read_dpu_binary(machine.dpu_binary.as_deref())?;

    let mut added_files = sim_files(&target, machine, host_node, hostname.clone(), dpusim_binary)?;
    // What DPF's installer (update_ignition.py) copies from the live system into the target.
    let mut installer_files = installer_files(&live);
    if machine.mgmt_mac.is_some() {
        with_control_plane_mtu(&mut installer_files, machine.mgmt_mtu);
    }
    let zero_trust = installer_files.iter().any(|file| {
        file.path == DPF_ENVIRONMENT
            && String::from_utf8_lossy(&file.contents)
                .lines()
                .any(|line| line.trim() == "DPUMode=zero-trust")
    });
    added_files.extend(installer_files);
    let agent = Agent {
        runs: zero_trust || machine.tmfifo_mac.is_some(),
        tmfifo: machine.tmfifo_mac.is_some(),
    };
    if agent.runs {
        added_files.extend(agent_hw_files());
    }
    replace_files(&mut target, &added_files, agent.runs)?;
    let masked_unit_count = mask_hardware_units(&mut target, agent)?;
    Ok(DpuIgnition {
        ignition: target,
        hostname: String::from_utf8_lossy(&hostname).trim().to_owned(),
        masked_unit_count,
    })
}

/// Whether the DPU machine runs DPF's dpu-agent, and has the tmfifo link to the host agent.
#[derive(Clone, Copy)]
struct Agent {
    runs: bool,
    tmfifo: bool,
}

impl Agent {
    fn keeps(self, unit: &str) -> bool {
        (self.runs && AGENT_UNITS.contains(&unit)) || (self.tmfifo && TMFIFO_UNITS.contains(&unit))
    }
}

/// A file to put into the ignition.
struct IgnitionFile {
    path: &'static str,
    mode: u32,
    contents: Vec<u8>,
    gzip: bool,
}

impl IgnitionFile {
    fn new(path: &'static str, contents: impl Into<Vec<u8>>) -> Self {
        Self {
            path,
            mode: 0o644,
            contents: contents.into(),
            gzip: false,
        }
    }

    fn mode(self, mode: u32) -> Self {
        Self { mode, ..self }
    }

    fn gzip(self) -> Self {
        Self { gzip: true, ..self }
    }

    fn to_json(&self) -> Result<Value> {
        let contents = if self.gzip {
            json!({"compression": "gzip", "source": data_url(&gzip(&self.contents)?)})
        } else {
            json!({"source": data_url(&self.contents)})
        };
        Ok(json!({"path": self.path, "mode": self.mode, "overwrite": true, "contents": contents}))
    }
}

/// Everything the simulation adds to the target ignition.
fn sim_files(
    target: &Value,
    machine: &DpuMachineArgs,
    host_node: Option<&str>,
    hostname: Vec<u8>,
    dpusim_binary: Vec<u8>,
) -> Result<Vec<IgnitionFile>> {
    let base_files = [
        IgnitionFile::new("/etc/hostname", hostname),
        // DOCA telemetry (DTS) mounts /sys/class/fwctl, which exists once the
        // fwctl class is loaded (on a BlueField, by the mlx5 fwctl driver).
        IgnitionFile::new("/etc/modules-load.d/fwctl.conf", "fwctl\n"),
        IgnitionFile::new(DPUSIM_ON_DPU, dpusim_binary)
            .mode(0o755)
            .gzip(),
        IgnitionFile::new(SF_DEVICE_PLUGIN_POD, SF_DEVICE_PLUGIN_POD_YAML),
        IgnitionFile::new(agent_hw::MLXCONFIG_ON_DPU, agent_hw::MLXCONFIG).mode(0o755),
    ];
    let mgmt_files = machine
        .mgmt_mac
        .as_deref()
        .map(|mgmt_mac| mgmt_nic_files(target, mgmt_mac, machine.mgmt_mtu))
        .transpose()?
        .unwrap_or_default();
    let wire_files = machine
        .wire_mac
        .as_deref()
        .map(|wire_mac| wire_nic_files(wire_mac, host_node))
        .unwrap_or_default();
    let tmfifo_files = machine.tmfifo_mac.as_deref().map(tmfifo_nic_file);
    let uplink_files = machine
        .uplink_mac
        .as_deref()
        .map(uplink_nic_files)
        .unwrap_or_default();
    Ok(base_files
        .into_iter()
        .chain(mgmt_files)
        .chain(wire_files)
        .chain(tmfifo_files)
        .chain(uplink_files)
        .collect())
}

fn mgmt_nic_files(target: &Value, mgmt_mac: &str, mgmt_mtu: u32) -> Result<Vec<IgnitionFile>> {
    let link = format!("[Match]\nMACAddress={mgmt_mac}\n\n[Link]\nName=pf0vf0\nNamePolicy=\n");
    let br_comm_ch = String::from_utf8(file_contents(target, BR_COMM_CH_CONNECTION)?)
        .with_context(|| format!("decoding {BR_COMM_CH_CONNECTION}"))?
        .replace(
            "cloned-mac-address=stable",
            &format!("cloned-mac-address={mgmt_mac}"),
        );
    let pf0vf0 = String::from_utf8(file_contents(target, PF0VF0_CONNECTION)?)
        .with_context(|| format!("decoding {PF0VF0_CONNECTION}"))?
        .replace("mtu=9000", &format!("mtu={mgmt_mtu}"));
    Ok(vec![
        IgnitionFile::new("/etc/systemd/network/05-sim-pf0vf0.link", link),
        IgnitionFile::new(BR_COMM_CH_CONNECTION, br_comm_ch).mode(0o600),
        IgnitionFile::new(PF0VF0_CONNECTION, pf0vf0).mode(0o600),
    ])
}

fn wire_nic_files(wire_mac: &str, host_node: Option<&str>) -> Vec<IgnitionFile> {
    let host_pf_line = host_node
        .map(|node| format!("{SIM_HOST_PF_MAC}={}\n", simulated_host_pf_mac(node)))
        .unwrap_or_default();
    let sim_env = format!("{SIM_WIRE_MAC}={wire_mac}\n{host_pf_line}");
    let unmanaged = format!(
        "[keyfile]\nunmanaged-devices=mac:{wire_mac};interface-name:dpuwire;interface-name:rep*;interface-name:pf0hpf-w\n"
    );
    vec![
        IgnitionFile::new(SIM_ENV_PATH, sim_env),
        IgnitionFile::new(
            "/etc/NetworkManager/conf.d/99-dpf-sim-unmanaged.conf",
            unmanaged,
        ),
    ]
}

fn uplink_nic_files(uplink_mac: &str) -> Vec<IgnitionFile> {
    vec![
        IgnitionFile::new(
            "/etc/systemd/network/05-sim-p0.link",
            format!("[Match]\nMACAddress={uplink_mac}\n\n[Link]\nName=p0\nNamePolicy=\n"),
        ),
        IgnitionFile::new(
            "/etc/NetworkManager/conf.d/99-dpf-sim-uplink.conf",
            // Appended to the other sim NICs (a second `unmanaged-devices=` would replace them).
            format!("[keyfile]\nunmanaged-devices+=mac:{uplink_mac}\n"),
        ),
    ]
}

fn tmfifo_nic_file(tmfifo_mac: &str) -> IgnitionFile {
    IgnitionFile::new(
        "/etc/systemd/network/05-sim-tmfifo.link",
        format!("[Match]\nMACAddress={tmfifo_mac}\n\n[Link]\nName=tmfifo_net0\nNamePolicy=\n"),
    )
}

/// A DPU machine that runs the dpu-agent: the stand-in tools and the drop-in that give it its simulated
/// BlueField (see agent_hw).
fn agent_hw_files() -> Vec<IgnitionFile> {
    agent_hw::TOOLS
        .iter()
        .map(|(path, script)| IgnitionFile::new(path, *script).mode(0o755))
        .chain([IgnitionFile::new(
            agent_hw::AGENT_DROPIN_PATH,
            agent_hw::AGENT_DROPIN,
        )])
        .collect()
}

const DPF_ENVIRONMENT: &str = "/etc/dpf/environment";
const AGENT_BOOTSTRAP_KUBECONFIG: &str = "/var/lib/dpf/dpuagent/bootstrap-kubeconfig";

/// The files DPF's installer copies from the live (install) system into the installed one, as
/// update_ignition.py does: the DPU's identity, and in Zero Trust the dpu-agent's bootstrap
/// kubeconfig. Whatever the live ignition doesn't have is left out.
fn installer_files(live: &Value) -> Vec<IgnitionFile> {
    [
        (DPF_ENVIRONMENT, 0o644),
        (AGENT_BOOTSTRAP_KUBECONFIG, 0o600),
    ]
    .into_iter()
    .filter_map(|(path, mode)| {
        file_contents(live, path)
            .ok()
            .map(|contents| IgnitionFile::new(path, contents).mode(mode))
    })
    .collect()
}

/// The DPU's control-plane link is the machine's management NIC (pf0vf0), at the MTU of the network
/// it's on, not a BlueField's 9000: the dpu-agent checks br-comm-ch against ControlPlaneMTU.
fn with_control_plane_mtu(files: &mut [IgnitionFile], mgmt_mtu: u32) {
    for file in files.iter_mut().filter(|file| file.path == DPF_ENVIRONMENT) {
        let environment: String = String::from_utf8_lossy(&file.contents)
            .lines()
            .map(|line| match line.strip_prefix("ControlPlaneMTU=") {
                Some(_) => format!("ControlPlaneMTU={mgmt_mtu}\n"),
                None => format!("{line}\n"),
            })
            .collect();
        file.contents = environment.into_bytes();
    }
}

/// Drops the hardware drop-ins and any file `added_files` replaces, then
/// appends `added_files`.
fn replace_files(target: &mut Value, added_files: &[IgnitionFile], agent_runs: bool) -> Result<()> {
    let replaced_paths: BTreeSet<&str> = added_files
        .iter()
        .map(|file| file.path)
        .chain(
            HARDWARE_DROPINS
                .into_iter()
                .filter(|dropin| !(agent_runs && *dropin == AGENT_DROPIN)),
        )
        .collect();
    let added_json = added_files
        .iter()
        .map(IgnitionFile::to_json)
        .collect::<Result<Vec<Value>>>()?;
    let files = array_at(target, &["storage", "files"])?;
    files.retain(|file| {
        file.get("path")
            .and_then(Value::as_str)
            .is_none_or(|path| !replaced_paths.contains(path))
    });
    files.extend(added_json);
    Ok(())
}

/// Masks the hardware units, adds dpf-ovs-sim.service, and returns how many
/// units it masked.
fn mask_hardware_units(target: &mut Value, agent: Agent) -> Result<usize> {
    let masked = |name: &str| HARDWARE_UNITS.contains(&name) && !agent.keeps(name);
    let units = array_at(target, &["systemd", "units"])?;
    let masked_units: Vec<String> = units
        .iter_mut()
        .filter_map(|unit| {
            let name = unit.get("name").and_then(Value::as_str)?.to_owned();
            masked(&name).then(|| {
                *unit = json!({"name": name, "mask": true});
                name
            })
        })
        .collect();
    units.push(json!({"name": "dpf-ovs-sim.service", "enabled": true, "contents": OVS_SIM_UNIT}));
    if agent.runs {
        units.push(json!({"name": "dpusim-agent-hw.service", "enabled": true, "contents": agent_hw::SETUP_UNIT}));
    }
    units.push(json!({
        "name": "dpf-ovs-sim-system-ports.service",
        "enabled": true,
        "contents": OVS_SIM_SYSTEM_PORTS_UNIT,
    }));
    let missing_units: Vec<&str> = HARDWARE_UNITS
        .into_iter()
        .filter(|unit| masked(unit) && !masked_units.iter().any(|name| name == unit))
        .collect();
    if !missing_units.is_empty() {
        eprintln!("warning: not in the target ignition: {missing_units:?}");
    }
    Ok(masked_units.len())
}

/// The array at `keys` under `value`, creating the objects and array on the way.
fn array_at<'a>(value: &'a mut Value, keys: &[&str]) -> Result<&'a mut Vec<Value>> {
    let leaf = keys.iter().try_fold(value, |node, key| {
        let object = node
            .as_object_mut()
            .with_context(|| format!("ignition parent of {key:?} is not an object"))?;
        Ok::<_, anyhow::Error>(object.entry(*key).or_insert(Value::Null))
    })?;
    if leaf.is_null() {
        *leaf = Value::Array(Vec::new());
    }
    leaf.as_array_mut()
        .with_context(|| format!("ignition {} is not an array", keys.join(".")))
}

/// The decoded contents of the ignition file at `path`.
pub fn file_contents(ignition: &Value, path: &str) -> Result<Vec<u8>> {
    let file = ignition
        .pointer("/storage/files")
        .and_then(Value::as_array)
        .and_then(|files| {
            files
                .iter()
                .find(|file| file.get("path").and_then(Value::as_str) == Some(path))
        })
        .with_context(|| format!("{path} not in ignition"))?;
    let source = file
        .pointer("/contents/source")
        .and_then(Value::as_str)
        .with_context(|| format!("{path} has no contents.source"))?;
    let raw =
        decode_data_url(source).with_context(|| format!("decoding the data URL of {path}"))?;
    match file
        .pointer("/contents/compression")
        .and_then(Value::as_str)
    {
        Some("gzip") => gunzip(&raw).with_context(|| format!("decompressing {path}")),
        None | Some("") => Ok(raw),
        Some(other) => bail!("{path} has unsupported compression {other:?}"),
    }
}

fn decode_data_url(source: &str) -> Result<Vec<u8>> {
    let (media_type, data) = source
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(','))
        .with_context(|| {
            format!(
                "source {:?} is not a data URL",
                source.chars().take(40).collect::<String>()
            )
        })?;
    if media_type.ends_with(";base64") {
        BASE64.decode(data).context("decoding base64")
    } else {
        Ok(percent_encoding::percent_decode_str(data).collect())
    }
}

fn data_url(raw: &[u8]) -> String {
    format!("data:;base64,{}", BASE64.encode(raw))
}

fn gunzip(compressed: &[u8]) -> Result<Vec<u8>> {
    let mut decompressed = Vec::new();
    GzDecoder::new(compressed)
        .read_to_end(&mut decompressed)
        .context("gunzipping")?;
    Ok(decompressed)
}

fn gzip(raw: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(raw).context("gzipping")?;
    encoder.finish().context("finishing gzip stream")
}

/// The aarch64 dpusim binary for the DPU.
fn read_dpu_binary(explicit_path: Option<&Path>) -> Result<Vec<u8>> {
    let path = match explicit_path {
        Some(path) => path.to_owned(),
        None if std::env::consts::ARCH == "aarch64" => {
            std::env::current_exe().context("finding this binary")?
        }
        None => Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/aarch64-unknown-linux-musl/release/dpusim"),
    };
    if !path.exists() {
        bail!(
            "the aarch64 dpusim binary for the DPU, {}, does not exist; build it with \
             `cargo build --release --target aarch64-unknown-linux-musl` in sim/dpusim, or pass --dpu-binary",
            path.display()
        );
    }
    fs::read(&path).with_context(|| format!("reading {}", path.display()))
}

/// Adds `key` to core's SSH keys.
pub fn authorize_ssh_key(ignition: &mut Value, key: &str) -> Result<()> {
    let users = array_at(ignition, &["passwd", "users"])?;
    users
        .iter_mut()
        .filter(|user| user.get("name").and_then(Value::as_str) == Some("core"))
        .try_for_each(|core_user| {
            let user_object = core_user
                .as_object_mut()
                .context("passwd user is not an object")?;
            let keys = array_at_key(user_object, "sshAuthorizedKeys")?;
            if !keys.iter().any(|existing| existing.as_str() == Some(key)) {
                keys.push(Value::String(key.to_owned()));
            }
            Ok(())
        })
}

fn array_at_key<'a>(object: &'a mut Map<String, Value>, key: &str) -> Result<&'a mut Vec<Value>> {
    object
        .entry(key)
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .with_context(|| format!("{key} is not an array"))
}

/// Writes the ignition readable only by its owner: it holds bootstrap credentials.
pub fn write_ignition(path: &Path, ignition: &Value) -> Result<()> {
    let serialized = serde_json::to_vec(ignition).context("serializing the ignition")?;
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut file| file.write_all(&serialized))
        .with_context(|| format!("writing {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restricting {} to its owner", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_plane_mtu_is_the_management_networks() {
        let mut files = vec![IgnitionFile::new(
            DPF_ENVIRONMENT,
            "DPUName=x\nDPUMode=trusted\nControlPlaneMTU=9000\n",
        )];
        with_control_plane_mtu(&mut files, 1500);
        let contents: Vec<String> = files
            .iter()
            .map(|file| String::from_utf8_lossy(&file.contents).into_owned())
            .collect();
        assert_eq!(
            contents,
            ["DPUName=x\nDPUMode=trusted\nControlPlaneMTU=1500\n"]
        );
    }
}
