//! DPU machines (M3/M4): the ignition that boots a machine as a given DPU,
//! and aarch64 VMs on a libvirt hypervisor that boot with it: stock RHCOS
//! aarch64 disk image + the ignition via fw_cfg, BlueField-3 SMBIOS identity
//! (HBN's platform check), a management NIC on the hypervisor's LAN bridge
//! and, optionally, a NIC on the wire to its host's PFs (M4) and a p0 uplink
//! NIC on the fabric bridge to the leaf (M6).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use crate::cluster::{self, bfb_registry_url, fetch_bfcfg, get_dpu};
use crate::cmd::{CommandExt, command};
use crate::ignition::{DpuMachineArgs, authorize_ssh_key, dpu_ignition, write_ignition};
use crate::mac::dpu_vm_nic_mac;

const LIBVIRT_IMAGES: &str = "/var/lib/libvirt/images";
const IGNITION_VALIDATE_IMAGE: &str = "quay.io/coreos/ignition-validate:release";

/// Where the DPU's ignition comes from.
#[derive(Args)]
pub struct IgnitionSourceArgs {
    /// bfb-registry base URL. Default: the control-plane node's NodePort.
    #[arg(long, env = "BFB_REGISTRY")]
    bfb_registry: Option<String>,
    /// SSH public key added for core. Default: ~/.ssh/id_ed25519.pub.
    #[arg(long, env = "SSH_PUBKEY")]
    ssh_pubkey: Option<PathBuf>,
}

/// `ignition`: the ignition that boots a machine as a DPU of the management cluster.
#[derive(Args)]
pub struct IgnitionArgs {
    /// The DPU object; it must have a bf.cfg, i.e. be waiting for the OS install.
    dpu: String,
    out: PathBuf,
    #[command(flatten)]
    machine: DpuMachineArgs,
    #[command(flatten)]
    source: IgnitionSourceArgs,
}

pub fn run_ignition(ignition_args: &IgnitionArgs) -> Result<()> {
    build_dpu_ignition(
        &ignition_args.dpu,
        &ignition_args.machine,
        &ignition_args.source,
        &ignition_args.out,
    )
}

/// Fetches the DPU's bf.cfg, turns it into the DPU machine's ignition with
/// our SSH key for core, writes it to `out` and validates it.
fn build_dpu_ignition(
    dpu: &str,
    machine: &DpuMachineArgs,
    source: &IgnitionSourceArgs,
    out: &Path,
) -> Result<()> {
    let bfb_registry = source
        .bfb_registry
        .clone()
        .map_or_else(bfb_registry_url, Ok)?;
    let dpu_object = get_dpu(dpu)?;
    let Some(bfcfg_path) = dpu_object.bfcfg_path else {
        bail!("DPU {dpu} has no bf.cfg yet (phase {})", dpu_object.phase);
    };
    let bfcfg = fetch_bfcfg(&bfb_registry, &bfcfg_path)?;
    let host_node = machine
        .wire_mac
        .as_ref()
        .map(|_wire_mac| dpu_object.host_node.as_str());
    let mut target = dpu_ignition(&bfcfg, machine, host_node)?;
    authorize_ssh_key(
        &mut target.ignition,
        &read_ssh_pubkey(source.ssh_pubkey.as_deref())?,
    )?;
    write_ignition(out, &target.ignition)?;
    let ignition_bytes =
        fs::read(out).with_context(|| format!("reading back {}", out.display()))?;
    command(
        "podman",
        ["run", "--rm", "-i", IGNITION_VALIDATE_IMAGE, "-"],
    )
    .run_with_stdin(&ignition_bytes)
    .context("validating the ignition")?;
    println!("{}: {}", out.display(), target.summary());
    Ok(())
}

fn read_ssh_pubkey(explicit_path: Option<&Path>) -> Result<String> {
    let path = match explicit_path {
        Some(path) => path.to_owned(),
        None => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?)
            .join(".ssh/id_ed25519.pub"),
    };
    let key = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    Ok(key.trim().to_owned())
}

#[derive(Subcommand)]
pub enum DpuVmCommand {
    /// Create a VM that boots as the DPU. The DPU object must have a bf.cfg.
    Create(CreateVmArgs),
    /// Remove the VM and its disk and ignition, and its Node and the per-node
    /// DPF objects in the DPU cluster.
    Delete(DeleteVmArgs),
}

#[derive(Args)]
pub struct DeleteVmArgs {
    #[command(flatten)]
    target: VmTarget,
    /// Also delete the DPU object, so DPF provisions it again and a new VM
    /// can join as it (`dpu-vm create` once it reaches "DPU Cluster Config").
    #[arg(long)]
    reprovision: bool,
}

#[derive(Args)]
pub struct VmTarget {
    /// The DPU object the VM plays.
    dpu: String,
    /// ssh target of the aarch64 hypervisor.
    #[arg(long, default_value = "aarchv")]
    hypervisor: String,
    /// VM name. Default: dpusim-<dpu>.
    #[arg(long)]
    name: Option<String>,
}

impl VmTarget {
    fn vm_name(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| format!("dpusim-{}", self.dpu))
    }

    fn disk_path(&self) -> String {
        format!("{LIBVIRT_IMAGES}/{}.qcow2", self.vm_name())
    }

    fn ignition_path(&self) -> String {
        format!("{LIBVIRT_IMAGES}/{}.ign", self.vm_name())
    }

    /// Stable per DPU, so reruns keep their DHCP leases.
    fn nic_mac(&self, role: &str) -> String {
        dpu_vm_nic_mac(&self.dpu, role)
    }
}

#[derive(Args)]
pub struct CreateVmArgs {
    #[command(flatten)]
    target: VmTarget,
    #[arg(long, default_value_t = 8)]
    vcpus: u32,
    /// Memory in MiB.
    #[arg(long, default_value_t = 16384)]
    memory: u32,
    /// LAN bridge on the hypervisor.
    #[arg(long, default_value = "mgmt-br")]
    mgmt_bridge: String,
    /// Bridge linked to the host's PFs (M4).
    #[arg(long)]
    wire_bridge: Option<String>,
    /// Bridge to the fabric leaf; that NIC is p0 (M6).
    #[arg(long)]
    fabric_bridge: Option<String>,
    /// RHCOS stream of the base disk image.
    #[arg(long, env = "RHCOS_STREAM", default_value = "4.22")]
    rhcos_stream: String,
    /// See `ignition --dpu-binary`.
    #[arg(long, env = "DPUSIM_DPU_BINARY")]
    dpu_binary: Option<PathBuf>,
    #[command(flatten)]
    source: IgnitionSourceArgs,
}

impl CreateVmArgs {
    /// A NIC of the VM: (bridge, MAC).
    fn nic(&self, bridge: Option<&String>, role: &str) -> Option<(String, String)> {
        bridge.map(|bridge| (bridge.clone(), self.target.nic_mac(role)))
    }

    fn machine(&self) -> DpuMachineArgs {
        DpuMachineArgs {
            mgmt_mac: Some(self.target.nic_mac("mgmt")),
            mgmt_mtu: 1500,
            wire_mac: self
                .nic(self.wire_bridge.as_ref(), "wire")
                .map(|(_bridge, mac)| mac),
            fabric_mac: self
                .nic(self.fabric_bridge.as_ref(), "fabric")
                .map(|(_bridge, mac)| mac),
            dpu_binary: self.dpu_binary.clone(),
        }
    }
}

pub fn run(dpu_vm_command: &DpuVmCommand) -> Result<()> {
    match dpu_vm_command {
        DpuVmCommand::Create(create_args) => create(create_args),
        DpuVmCommand::Delete(delete_args) => delete(delete_args),
    }
}

/// Runs commands on the hypervisor over ssh.
struct Hypervisor<'a> {
    ssh_target: &'a str,
}

impl Hypervisor<'_> {
    fn command(&self, words: &[&str]) -> Result<Command> {
        let remote_command = shlex::try_join(words.iter().copied())
            .with_context(|| format!("quoting {words:?} for ssh"))?;
        Ok(command("ssh", [self.ssh_target, &remote_command]))
    }

    fn run(&self, words: &[&str]) -> Result<()> {
        self.command(words)?.run()
    }

    fn read(&self, words: &[&str]) -> Result<String> {
        self.command(words)?.read()
    }

    fn succeeds(&self, words: &[&str]) -> Result<bool> {
        self.command(words)?.succeeds()
    }

    fn copy_to(&self, local_path: &Path, remote_path: &str) -> Result<()> {
        Command::new("scp")
            .arg("-q")
            .arg(local_path)
            .arg(format!("{}:{remote_path}", self.ssh_target))
            .run()
    }
}

fn delete(delete_args: &DeleteVmArgs) -> Result<()> {
    let target = &delete_args.target;
    let hypervisor = Hypervisor {
        ssh_target: &target.hypervisor,
    };
    let vm_name = target.vm_name();
    if hypervisor.succeeds(&["virsh", "dominfo", &vm_name])? {
        if hypervisor.read(&["virsh", "domstate", &vm_name])?.trim() != "shut off" {
            hypervisor.run(&["virsh", "destroy", &vm_name])?;
        }
        hypervisor.run(&["virsh", "undefine", "--nvram", &vm_name])?;
    }
    hypervisor.run(&["rm", "-f", &target.disk_path(), &target.ignition_path()])?;
    println!("removed {vm_name}");
    cluster::forget_dpu_node(&target.dpu)?;
    if delete_args.reprovision {
        cluster::reprovision_dpu(&target.dpu)?;
    }
    Ok(())
}

fn create(create_args: &CreateVmArgs) -> Result<()> {
    let target = &create_args.target;
    let hypervisor = Hypervisor {
        ssh_target: &target.hypervisor,
    };
    let machine = create_args.machine();
    let local_ignition =
        tempfile::NamedTempFile::new().context("creating a temporary ignition file")?;
    build_dpu_ignition(
        &target.dpu,
        &machine,
        &create_args.source,
        local_ignition.path(),
    )?;
    let ignition_path = target.ignition_path();
    hypervisor.copy_to(local_ignition.path(), &ignition_path)?;
    // QEMU reads the ignition (it holds bootstrap credentials): qemu-owned, 0600.
    hypervisor.run(&["chown", "qemu:qemu", &ignition_path])?;
    hypervisor.run(&["chmod", "0600", &ignition_path])?;
    hypervisor.run(&["chcon", "-t", "virt_content_t", &ignition_path])?;

    let base_image = ensure_base_image(&hypervisor, &create_args.rhcos_stream)?;
    hypervisor.run(&[
        "qemu-img",
        "create",
        "-q",
        "-f",
        "qcow2",
        "-F",
        "qcow2",
        "-b",
        &base_image,
        &target.disk_path(),
        "60G",
    ])?;
    let install_args = virt_install_args(create_args, &ignition_path);
    hypervisor.run(&install_args.iter().map(String::as_str).collect::<Vec<_>>())?;

    let extra_nics: String = [
        create_args
            .nic(create_args.wire_bridge.as_ref(), "wire")
            .map(|(bridge, mac)| format!(", wire {mac} on {bridge}")),
        create_args
            .nic(create_args.fabric_bridge.as_ref(), "fabric")
            .map(|(bridge, mac)| format!(", fabric p0 {mac} on {bridge}")),
    ]
    .into_iter()
    .flatten()
    .collect();
    println!(
        "VM {} on {}: mgmt {}{extra_nics}",
        target.vm_name(),
        target.hypervisor,
        target.nic_mac("mgmt")
    );
    println!(
        "it joins the DPU cluster as Node {}; its address is that Node's InternalIP (DHCP on {})",
        target.dpu, create_args.mgmt_bridge
    );
    Ok(())
}

fn virt_install_args(create_args: &CreateVmArgs, ignition_path: &str) -> Vec<String> {
    let target = &create_args.target;
    let nics = [
        Some((create_args.mgmt_bridge.clone(), target.nic_mac("mgmt"))),
        create_args.nic(create_args.wire_bridge.as_ref(), "wire"),
        create_args.nic(create_args.fabric_bridge.as_ref(), "fabric"),
    ];
    let network_args = nics.into_iter().flatten().flat_map(|(bridge, mac)| {
        [
            "--network".to_owned(),
            format!("bridge={bridge},model=virtio,mac={mac}"),
        ]
    });
    let words = [
        "virt-install".to_owned(),
        "--name".to_owned(),
        target.vm_name(),
        "--arch=aarch64".to_owned(),
        "--machine=virt".to_owned(),
        "--boot=uefi".to_owned(),
        "--import".to_owned(),
        format!("--vcpus={}", create_args.vcpus),
        format!("--memory={}", create_args.memory),
        "--osinfo=detect=on,require=off".to_owned(),
        format!("--disk=path={},format=qcow2,bus=virtio", target.disk_path()),
    ];
    let tail_words = [
        "--sysinfo=smbios,system.manufacturer=Nvidia,system.product=BlueField-3,baseBoard.manufacturer=Nvidia,baseBoard.product=BlueField-3"
            .to_owned(),
        format!("--qemu-commandline=-fw_cfg name=opt/com.coreos/config,file={ignition_path}"),
        "--graphics=none".to_owned(),
        "--noautoconsole".to_owned(),
    ];
    words
        .into_iter()
        .chain(network_args)
        .chain(tail_words)
        .collect()
}

/// The RHCOS aarch64 QEMU image all DPU VMs on the hypervisor share (thin
/// overlays), downloaded on first use.
fn ensure_base_image(hypervisor: &Hypervisor, rhcos_stream: &str) -> Result<String> {
    let base_image = format!("{LIBVIRT_IMAGES}/rhcos-{rhcos_stream}-aarch64-qemu.qcow2");
    if hypervisor.succeeds(&["test", "-f", &base_image])? {
        return Ok(base_image);
    }
    let mirror = format!(
        "https://mirror.openshift.com/pub/openshift-v4/aarch64/dependencies/rhcos/{rhcos_stream}/latest"
    );
    let checksums = hypervisor.read(&["curl", "-sSf", &format!("{mirror}/sha256sum.txt")])?;
    let image_file = checksums
        .split_whitespace()
        .find(|word| word.starts_with("rhcos-") && word.ends_with("qemu.aarch64.qcow2.gz"))
        .with_context(|| format!("no RHCOS aarch64 QEMU image in {mirror}/sha256sum.txt"))?;
    let download = format!("{base_image}.download");
    let compressed_download = format!("{download}.gz");
    println!("downloading {image_file} to {}", hypervisor.ssh_target);
    hypervisor.run(&[
        "curl",
        "-sSf",
        "-o",
        &compressed_download,
        &format!("{mirror}/{image_file}"),
    ])?;
    hypervisor.run(&["gunzip", "-f", &compressed_download])?;
    hypervisor.run(&["mv", &download, &base_image])?;
    Ok(base_image)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct CreateVmCli {
        #[command(flatten)]
        create_args: CreateVmArgs,
    }

    #[test]
    fn virt_install_puts_mgmt_wire_fabric_nics_in_order() -> Result<()> {
        let cli = CreateVmCli::try_parse_from([
            "create",
            "--wire-bridge",
            "br-dpusim1",
            "--fabric-bridge",
            "br-dpufab1",
            "my-dpu",
        ])?;
        let install_args = virt_install_args(&cli.create_args, "/images/dpusim-my-dpu.ign");
        let networks: Vec<&str> = install_args
            .iter()
            .zip(install_args.iter().skip(1))
            .filter(|(flag, _value)| *flag == "--network")
            .map(|(_flag, value)| value.as_str())
            .collect();
        let expected_networks = [
            ("mgmt-br", "mgmt"),
            ("br-dpusim1", "wire"),
            ("br-dpufab1", "fabric"),
        ]
        .map(|(bridge, role)| {
            format!(
                "bridge={bridge},model=virtio,mac={}",
                dpu_vm_nic_mac("my-dpu", role)
            )
        });
        assert_eq!(networks, expected_networks);
        assert_eq!(
            install_args.iter().take(3).collect::<Vec<_>>(),
            ["virt-install", "--name", "dpusim-my-dpu"]
        );
        assert!(install_args.contains(
            &"--qemu-commandline=-fw_cfg name=opt/com.coreos/config,file=/images/dpusim-my-dpu.ign"
                .to_owned()
        ));
        Ok(())
    }
}
