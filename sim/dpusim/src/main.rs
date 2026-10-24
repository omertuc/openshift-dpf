//! dpusim: the simulated BlueField DPUs dpusimaas runs (sim/README.md). One static binary that
//! runs wherever its part of the simulation lives: on the hypervisors (DPU and host VMs, the links
//! between them, the leaf routers the DPUs are cabled to) and on the DPU VM (dpu-ovs, agent-hw).

mod agent_hw;
mod bf3_domain;
mod cmd;
mod dpu_ovs;
mod dpu_vm;
mod host_vm;
mod ignition;
mod leaf;
mod links;
mod mac;
mod trunk;
mod wire;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Simulated BlueField DPUs (see sim/README.md)")]
struct DpusimCli {
    #[command(subcommand)]
    command: DpusimCommand,
}

#[derive(Subcommand)]
enum DpusimCommand {
    /// Hypervisor: the bridge + VXLAN that links a host VM to its DPU VM.
    #[command(subcommand)]
    Wire(wire::WireCommand),
    /// Hypervisor: aarch64 VMs that boot as DPUs from DPF's bf.cfg.
    #[command(subcommand)]
    DpuVm(dpu_vm::DpuVmCommand),
    /// Hypervisor: x86 host VMs for host-trusted pairs (patched QEMU, installed through a BMC).
    #[command(subcommand)]
    HostVm(host_vm::HostVmCommand),
    /// Hypervisor: the leaf router a cluster's DPUs are cabled to.
    #[command(subcommand)]
    Leaf(leaf::LeafCommand),
    /// DPU: build DPF's OVS bridges and the simulated representors.
    DpuOvs(dpu_ovs::DpuOvsArgs),
    /// DPU: the BlueField hardware the dpu-agent alone sees.
    AgentHw(agent_hw::AgentHwArgs),
}

fn main() -> Result<()> {
    match DpusimCli::parse().command {
        DpusimCommand::Wire(wire_command) => wire::run(wire_command),
        DpusimCommand::DpuVm(dpu_vm_command) => dpu_vm::run(&dpu_vm_command),
        DpusimCommand::HostVm(host_vm_command) => host_vm::run(&host_vm_command),
        DpusimCommand::Leaf(leaf_command) => leaf::run(leaf_command),
        DpusimCommand::DpuOvs(dpu_ovs_args) => dpu_ovs::run(&dpu_ovs_args),
        DpusimCommand::AgentHw(agent_hw_args) => agent_hw::run(&agent_hw_args),
    }
}
