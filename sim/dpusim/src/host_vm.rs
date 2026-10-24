//! Host VMs for host-trusted pairs: an x86 VM that a cluster installs like any bare-metal
//! worker (through its BMC's virtual media), whose two igb NICs are a BlueField-3's PFs (the
//! patched QEMU, see bf3_domain) on the wire to its DPU VM. It's created powered off with an empty disk
//! and an empty CD-ROM for the BMC's virtual media.
//!
//! It also gets the BlueField's other link to its host, rshim's tmfifo, as a NIC with the MAC a
//! BlueField host's tmfifo interface has. What makes the host's OS treat all of that as a
//! BlueField comes from the cluster (openshift-dpf's SIMULATED_DPUS_ONLY MachineConfig).

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use crate::bf3_domain::bf3sim_domain;
use crate::cmd::CommandExt;
use crate::dpu_vm::{Hypervisor, link_mtu};
use crate::mac::{HOST_TMFIFO_MAC, dpu_vm_nic_mac};

const LIBVIRT_IMAGES: &str = "/var/lib/libvirt/images";
/// The storage pool sushy-tools uploads virtual media to (its default).
const MEDIA_POOL: &str = "default";

#[derive(Subcommand)]
pub enum HostVmCommand {
    /// Create the host VM, powered off, ready to be installed through its BMC.
    Create(Box<CreateHostVmArgs>),
    /// Remove the host VM and its disk.
    Delete(HostVmTarget),
}

#[derive(Args)]
pub struct HostVmTarget {
    /// VM name.
    name: String,
    /// ssh target of the x86 hypervisor, or `local` when running on it.
    #[arg(long)]
    hypervisor: String,
}

#[derive(Args)]
pub struct CreateHostVmArgs {
    #[command(flatten)]
    target: HostVmTarget,
    /// The DPU serial both PFs report in their VPD.
    #[arg(long)]
    serial: String,
    /// LAN bridge for the host's own NIC (the one it's installed and joins the cluster through).
    #[arg(long)]
    mgmt_bridge: String,
    /// Bridge to the DPU VM (the wire); the two PFs are on it.
    #[arg(long)]
    wire_bridge: String,
    /// Bridge to the DPU VM's tmfifo NIC (the DPU agent's link to the host agent).
    #[arg(long)]
    tmfifo_bridge: Option<String>,
    /// The patched QEMU (`make qemu` in dpusimaas).
    #[arg(long, default_value = "/usr/libexec/qemu-kvm-bf3sim")]
    emulator: String,
    #[arg(long, default_value_t = 8)]
    vcpus: u32,
    /// Memory in MiB.
    #[arg(long, default_value_t = 16384)]
    memory: u32,
    /// Disk size in GiB.
    #[arg(long, default_value_t = 120)]
    disk: u32,
}

impl HostVmTarget {
    fn disk(&self) -> String {
        format!("{LIBVIRT_IMAGES}/{}.qcow2", self.name)
    }

    /// Stable per VM, so recreating it keeps its DHCP lease.
    pub fn nic_mac(&self, role: &str) -> String {
        dpu_vm_nic_mac(&self.name, role)
    }
}

pub fn run(command: &HostVmCommand) -> Result<()> {
    match command {
        HostVmCommand::Create(create_args) => create(create_args),
        HostVmCommand::Delete(target) => delete(target),
    }
}

fn create(args: &CreateHostVmArgs) -> Result<()> {
    let target = &args.target;
    let hypervisor = Hypervisor {
        ssh_target: &target.hypervisor,
    };
    if hypervisor.succeeds(&["virsh", "dominfo", &target.name])? {
        bail!("{} already exists on {}", target.name, target.hypervisor);
    }
    let (p0_mac, p1_mac) = (target.nic_mac("p0"), target.nic_mac("p1"));

    hypervisor.run(&[
        "qemu-img",
        "create",
        "-q",
        "-f",
        "qcow2",
        &target.disk(),
        &format!("{}G", args.disk),
    ])?;
    // Define a plain domain first: libvirt assigns the PCI addresses the BlueField conversion needs.
    let tmfifo_mtu = link_mtu(&hypervisor, args.tmfifo_bridge.as_deref())?;
    let plain = hypervisor.read(
        &virt_install_args(args, &p0_mac, &p1_mac, tmfifo_mtu)
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    )?;
    define(&hypervisor, &plain)?;
    let defined = hypervisor.read(&["virsh", "dumpxml", "--inactive", &target.name])?;
    let bf3 = bf3sim_domain(&defined, &args.emulator, &args.serial, &p0_mac, &p1_mac)?;
    define(&hypervisor, &bf3)?;
    println!(
        "host VM {} on {}: mgmt {} on {}, PFs {p0_mac}/{p1_mac} on {} (serial {}){}",
        target.name,
        target.hypervisor,
        target.nic_mac("mgmt"),
        args.mgmt_bridge,
        args.wire_bridge,
        args.serial,
        args.tmfifo_bridge
            .as_ref()
            .map(|bridge| format!(", tmfifo on {bridge}"))
            .unwrap_or_default()
    );
    Ok(())
}

fn delete(target: &HostVmTarget) -> Result<()> {
    let hypervisor = Hypervisor {
        ssh_target: &target.hypervisor,
    };
    if hypervisor.succeeds(&["virsh", "dominfo", &target.name])? {
        if hypervisor
            .read(&["virsh", "domstate", &target.name])?
            .trim()
            != "shut off"
        {
            hypervisor.run(&["virsh", "destroy", &target.name])?;
        }
        let uuid = hypervisor.read(&["virsh", "domuuid", &target.name])?;
        // Virtual media a Redfish BMC (sushy-tools) inserted: <image>-<uuid>.img in its pool.
        let media_suffix = format!("-{}.img", uuid.trim());
        // `vol-list --name` is too new for RHEL 9's virsh: the names are the first column.
        for volume in hypervisor
            .read(&["virsh", "vol-list", MEDIA_POOL])?
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .filter(|volume| volume.ends_with(&media_suffix))
        {
            hypervisor.run(&["virsh", "vol-delete", "--pool", MEDIA_POOL, volume])?;
        }
        hypervisor.run(&["virsh", "undefine", "--nvram", &target.name])?;
    }
    // Through libvirt when it knows the disk, so its pool doesn't keep listing it.
    if !hypervisor.succeeds(&["virsh", "vol-delete", &target.disk()])? {
        hypervisor.run(&["rm", "-f", &target.disk()])?;
    }
    println!("removed {}", target.name);
    Ok(())
}

fn virt_install_args(
    args: &CreateHostVmArgs,
    p0_mac: &str,
    p1_mac: &str,
    tmfifo_mtu: Option<u32>,
) -> Vec<String> {
    let target = &args.target;
    [
        "virt-install",
        "--print-xml",
        "--import",
        "--name",
        &target.name,
        "--arch=x86_64",
        "--machine=q35",
        "--boot=uefi,hd,cdrom",
        &format!("--vcpus={}", args.vcpus),
        &format!("--memory={}", args.memory),
        "--osinfo=detect=off,name=rhel9.6",
        &format!("--disk=path={},format=qcow2,bus=virtio", target.disk()),
        "--disk=device=cdrom,bus=sata",
        &format!(
            "--network=bridge={},model=virtio,mac={}",
            args.mgmt_bridge,
            target.nic_mac("mgmt")
        ),
        &format!(
            "--network=bridge={},model=igb,mac={p0_mac}",
            args.wire_bridge
        ),
        &format!(
            "--network=bridge={},model=igb,mac={p1_mac}",
            args.wire_bridge
        ),
    ]
    .into_iter()
    .map(str::to_owned)
    .chain(args.tmfifo_bridge.iter().map(|bridge| {
        let mtu = tmfifo_mtu
            .map(|mtu| format!(",mtu.size={mtu}"))
            .unwrap_or_default();
        format!("--network=bridge={bridge},model=virtio,mac={HOST_TMFIFO_MAC}{mtu}")
    }))
    // Every host's tmfifo NIC has the same MAC, as on BlueField hosts; each is on its own link.
    .chain(
        [
            "--check=mac_in_use=off",
            "--graphics=none",
            "--noautoconsole",
        ]
        .map(str::to_owned),
    )
    .collect()
}

fn define(hypervisor: &Hypervisor, domain_xml: &str) -> Result<()> {
    hypervisor
        .command(&["virsh", "define", "/dev/stdin"])?
        .run_with_stdin(domain_xml.as_bytes())
        .context("defining the domain")
}
