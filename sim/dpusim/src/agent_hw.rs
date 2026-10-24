//! Zero Trust DPU machines run DPF's real dpu-agent, whose last steps configure BlueField
//! hardware a VM doesn't have: NVConfig (devlink ports, mlxconfig), reboot handling
//! (/dev/mst, mlxfwreset), the kernel command line check, SFs (mlnx-sf) and VF MACs
//! (smart_nic). The agent alone sees a BlueField instead, in its own mount namespace
//! (a drop-in on dpu-agent.service): stand-in tools first in its PATH, and sysfs, /dev/mst
//! and /proc/cmdline overlays that `dpusim agent-hw` prepares before it starts. The rest of
//! the machine sees the real VM.
//!
//! The PCI address and the p0 uplink are the ones the agent expects (0000:03:00.0 is
//! hardcoded in its SF and VF MAC steps); in the VM that address is some other device,
//! whose sysfs directory the agent alone sees replaced.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use clap::Args;

/// Where `dpusim agent-hw` puts what the agent sees.
const RUNTIME: &str = "/run/dpusim-agent-hw";
/// The BlueField's PF0, as the agent's SF and VF MAC steps hardcode it.
const PF0_PCI: &str = "0000:03:00.0";
const FLAVOR: &str = "/etc/dpf/dpuflavor.yaml";
const MST_DEVICE: &str = "mt41692_pciconf0";

/// mlxconfig, also for the whole machine (see `MLXCONFIG_ON_DPU`).
pub const MLXCONFIG: &str = include_str!("agent_hw/mlxconfig");
/// DPF's node-problem-detector queries the DPU mode with mlxconfig, outside the agent's namespace.
pub const MLXCONFIG_ON_DPU: &str = "/usr/local/bin/mlxconfig";

/// The stand-in tools, first in the agent's PATH: (path, script).
pub const TOOLS: [(&str, &str); 5] = [
    (
        "/usr/local/libexec/dpusim-agent-hw/devlink",
        include_str!("agent_hw/devlink"),
    ),
    ("/usr/local/libexec/dpusim-agent-hw/mlxconfig", MLXCONFIG),
    (
        "/usr/local/libexec/dpusim-agent-hw/mlxfwreset",
        include_str!("agent_hw/mlxfwreset"),
    ),
    (
        "/usr/local/libexec/dpusim-agent-hw/mlxprivhost",
        include_str!("agent_hw/mlxprivhost"),
    ),
    (
        "/usr/local/libexec/dpusim-agent-hw/mlnx-sf",
        include_str!("agent_hw/mlnx-sf"),
    ),
];

/// Prepares the overlays before dpu-agent.service starts.
pub const SETUP_UNIT: &str = "[Unit]
Description=Simulated BlueField hardware for the dpu-agent (simulation)
After=dpf-ovs-sim.service
Before=dpu-agent.service
ConditionPathExists=/etc/dpf/dpuflavor.yaml

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/local/bin/dpusim agent-hw

[Install]
RequiredBy=dpu-agent.service
";

/// The dpu-agent's view: stand-in tools first, and the overlays (colons in paths are escaped).
pub const AGENT_DROPIN_PATH: &str =
    "/etc/systemd/system/dpu-agent.service.d/50-dpusim-agent-hw.conf";
pub const AGENT_DROPIN: &str = "# dpusim: the dpu-agent alone sees simulated BlueField hardware
[Unit]
Requires=dpusim-agent-hw.service
After=dpusim-agent-hw.service

[Service]
Environment=PATH=/usr/local/libexec/dpusim-agent-hw:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin
BindReadOnlyPaths=/run/dpusim-agent-hw/cmdline:/proc/cmdline
BindPaths=/run/dpusim-agent-hw/pci:/sys/bus/pci/devices/0000\\:03\\:00.0
BindPaths=/run/dpusim-agent-hw/p0:/sys/class/net/p0
BindPaths=/run/dpusim-agent-hw/mst:/dev/mst
";

/// `agent-hw`: run on the DPU machine before the dpu-agent starts.
#[derive(Args)]
pub struct AgentHwArgs {}

pub fn run(_args: &AgentHwArgs) -> Result<()> {
    let runtime = Path::new(RUNTIME);
    // PF0's PCI function: only its p0 uplink netdev.
    fs::create_dir_all(runtime.join("pci/net/p0"))?;
    // p0 (the uplink NIC, or the veth dpu-ovs made): a switchdev uplink with no VFs yet.
    let p0 = runtime.join("p0");
    fs::create_dir_all(p0.join("smart_nic"))?;
    fs::write(p0.join("phys_switch_id"), "1\n")?;
    fs::write(p0.join("phys_port_name"), "p0\n")?;
    if let Ok(address) = fs::read_to_string("/sys/class/net/p0/address") {
        fs::write(p0.join("address"), address)?;
    }
    // One MST device, and /dev/mst itself for the overlay to land on.
    fs::create_dir_all(runtime.join("mst"))?;
    fs::write(runtime.join("mst").join(MST_DEVICE), "")?;
    fs::create_dir_all("/dev/mst").context("creating /dev/mst")?;
    // The kernel command line the flavor asks for, as if the agent had set it.
    let cmdline = fs::read_to_string("/proc/cmdline").context("reading /proc/cmdline")?;
    let flavor = fs::read_to_string(FLAVOR).with_context(|| format!("reading {FLAVOR}"))?;
    let wanted = kernel_parameters(&flavor);
    fs::write(
        runtime.join("cmdline"),
        format!("{} {}\n", cmdline.trim(), wanted.join(" ")),
    )?;
    println!(
        "dpu-agent sees a BlueField: PF0 {PF0_PCI} with uplink p0, /dev/mst/{MST_DEVICE}, {} kernel parameters",
        wanted.len()
    );
    Ok(())
}

/// The flavor's spec.grub.kernelParameters (a YAML list, read without a YAML parser: the items
/// of the `kernelParameters:` key, indented deeper than it).
fn kernel_parameters(flavor: &str) -> Vec<String> {
    let mut lines = flavor.lines();
    let Some(key_indent) = lines
        .by_ref()
        .find_map(|line| (line.trim_start() == "kernelParameters:").then(|| indent(line)))
    else {
        return Vec::new();
    };
    lines
        .take_while(|line| {
            let depth = indent(line);
            line.trim().is_empty() || depth > key_indent || line.trim_start().starts_with("- ")
        })
        .filter_map(|line| line.trim().strip_prefix("- "))
        .map(|item| item.trim().trim_matches(['\'', '"']).to_owned())
        .collect()
}

fn indent(line: &str) -> usize {
    line.chars().take_while(|c| c.is_whitespace()).count()
}

#[cfg(test)]
mod tests {
    use super::kernel_parameters;

    #[test]
    fn reads_the_flavor_kernel_parameters() {
        let flavor = "spec:
  dpuMode: zero-trust
  grub:
    kernelParameters:
    - console=hvc0
    - 'earlycon=pl011,0x13010000'
    - hugepages=3072
  nvconfig:
  - device: '*'
";
        assert_eq!(
            kernel_parameters(flavor),
            [
                "console=hvc0",
                "earlycon=pl011,0x13010000",
                "hugepages=3072"
            ]
        );
    }

    #[test]
    fn no_kernel_parameters() {
        assert!(kernel_parameters("spec:\n  dpuMode: dpu\n").is_empty());
    }
}
