//! dpusim: the tooling for simulated BlueField DPUs (sim/README.md). One
//! static binary that runs wherever its part of the simulation lives: on the
//! workstation (DPU VMs, ignition, image builds), on the hypervisors (wire,
//! leaf, VM XML), on the host VM (bf3-host) and on the DPU VM (dpu-ovs).

mod bf3_host;
mod cluster;
mod cmd;
mod dpu_ovs;
mod dpu_vm;
mod ignition;
mod images;
mod leaf;
mod links;
mod m2b;
mod mac;
mod trunk;
mod wire;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Tooling for simulated BlueField DPUs (see sim/README.md)")]
struct DpusimCli {
    #[command(subcommand)]
    command: DpusimCommand,
}

#[derive(Subcommand)]
enum DpusimCommand {
    /// Hypervisor: the bridge + VXLAN that links a host VM to its DPU VM (M4).
    #[command(subcommand)]
    Wire(wire::WireCommand),
    /// Hypervisor: the FRR leaf switch the DPU uplinks peer with (M6).
    #[command(subcommand)]
    Leaf(leaf::LeafCommand),
    /// Workstation: aarch64 VMs that boot as DPUs (M3/M4).
    #[command(subcommand)]
    DpuVm(dpu_vm::DpuVmCommand),
    /// Workstation: the ignition that boots a machine as a DPU of the cluster.
    Ignition(dpu_vm::IgnitionArgs),
    /// Turn a DPU's bf.cfg file into the ignition that boots a machine as it.
    BfcfgToIgnition(ignition::BfcfgToIgnitionArgs),
    /// DPU: build DPF's OVS bridges and the simulated representors.
    DpuOvs(dpu_ovs::DpuOvsArgs),
    /// Host VM: make the patched-QEMU igb pair look like a BlueField-3 (M2b).
    Bf3Host(bf3_host::Bf3HostArgs),
    /// Hypervisor: print a host VM's domain XML switched to the patched QEMU (M2b).
    M2bSwitchVm(m2b::M2bSwitchVmArgs),
    /// Workstation: build and push the patched images.
    #[command(subcommand)]
    Image(images::ImageCommand),
}

fn main() -> Result<()> {
    match DpusimCli::parse().command {
        DpusimCommand::Wire(wire_command) => wire::run(wire_command),
        DpusimCommand::Leaf(leaf_command) => leaf::run(leaf_command),
        DpusimCommand::DpuVm(dpu_vm_command) => dpu_vm::run(&dpu_vm_command),
        DpusimCommand::Ignition(ignition_args) => dpu_vm::run_ignition(&ignition_args),
        DpusimCommand::BfcfgToIgnition(convert_args) => {
            ignition::run_bfcfg_to_ignition(&convert_args)
        }
        DpusimCommand::DpuOvs(dpu_ovs_args) => dpu_ovs::run(&dpu_ovs_args),
        DpusimCommand::Bf3Host(host_args) => bf3_host::run(&host_args),
        DpusimCommand::M2bSwitchVm(switch_args) => m2b::run(&switch_args),
        DpusimCommand::Image(image_command) => images::run(&image_command),
    }
}
