//! aarch64 VMs on a libvirt hypervisor that boot as a DPU from the bf.cfg DPF installed it with:
//! stock RHCOS aarch64 disk image + the ignition (see ignition) via fw_cfg, BlueField-3 SMBIOS
//! identity (HBN's platform check), a management NIC on the hypervisor's LAN bridge, the p0 uplink
//! to the leaf router (see leaf) and, host-trusted, NICs on the links to its host (its PFs, and
//! rshim's tmfifo).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use crate::cmd::{CommandExt, command};
use crate::ignition::{DpuMachineArgs, authorize_ssh_key, dpu_ignition, write_ignition};
use crate::mac::{DPU_TMFIFO_MAC, dpu_vm_nic_mac};

const LIBVIRT_IMAGES: &str = "/var/lib/libvirt/images";
const IGNITION_VALIDATE_IMAGE: &str = "quay.io/coreos/ignition-validate:release";

/// Where the DPU's ignition comes from.
#[derive(Args)]
pub struct IgnitionSourceArgs {
    /// The bf.cfg DPF installed the DPU with (through its BMC or its host's DMS).
    #[arg(long)]
    bfcfg: PathBuf,
    /// The DPU's host node, for the host PF MAC OVN-K expects (with a wire).
    #[arg(long)]
    host_node: Option<String>,
    /// SSH public key added for core. Default: ~/.ssh/id_ed25519.pub.
    #[arg(long, env = "SSH_PUBKEY")]
    ssh_pubkey: Option<PathBuf>,
}

/// Turns the bf.cfg into the DPU machine's ignition with our SSH key for core, writes it to `out`
/// and validates it.
fn build_dpu_ignition(
    machine: &DpuMachineArgs,
    source: &IgnitionSourceArgs,
    out: &Path,
) -> Result<()> {
    let path = &source.bfcfg;
    let bfcfg = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut target = dpu_ignition(&bfcfg, machine, source.host_node.as_deref())?;
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
    /// Create a VM that boots as the DPU.
    Create(Box<CreateVmArgs>),
    /// Remove the VM and its disk and ignition.
    Delete(VmTarget),
}

#[derive(Args)]
pub struct VmTarget {
    /// The DPU the VM plays (its NICs' MACs derive from it).
    dpu: String,
    /// ssh target of the aarch64 hypervisor, or `local` when running on it.
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
    /// Bridge linked to the host's PFs.
    #[arg(long)]
    wire_bridge: Option<String>,
    /// Bridge linked to the host's tmfifo NIC (host-trusted: the dpu-agent's way to the host
    /// agent); that NIC is tmfifo_net0.
    #[arg(long)]
    tmfifo_bridge: Option<String>,
    /// Bridge to the leaf router (`leaf attach`); that NIC is the BlueField's p0 uplink.
    #[arg(long)]
    uplink_bridge: Option<String>,
    /// BlueField serial number for the VM's SMBIOS (system and baseboard), as on a real DPU.
    #[arg(long)]
    serial: Option<String>,
    /// RHCOS stream of the base disk image.
    #[arg(long, env = "RHCOS_STREAM", default_value = "4.22")]
    rhcos_stream: String,
    /// The aarch64 dpusim binary to install on the DPU. Default: this binary on aarch64, else
    /// target/aarch64-unknown-linux-musl/release/dpusim of this crate.
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
            tmfifo_mac: self
                .tmfifo_bridge
                .as_ref()
                .map(|_bridge| DPU_TMFIFO_MAC.to_owned()),
            uplink_mac: self
                .nic(self.uplink_bridge.as_ref(), "uplink")
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

/// Runs commands on the hypervisor over ssh, or here when it's `local`.
pub(crate) struct Hypervisor<'a> {
    pub(crate) ssh_target: &'a str,
}

impl Hypervisor<'_> {
    pub(crate) fn is_local(&self) -> bool {
        self.ssh_target == "local"
    }

    pub(crate) fn command(&self, words: &[&str]) -> Result<Command> {
        if self.is_local() {
            let (program, args) = words.split_first().context("empty command")?;
            let mut local = Command::new(program);
            local.args(args);
            return Ok(local);
        }
        let remote_command = shlex::try_join(words.iter().copied())
            .with_context(|| format!("quoting {words:?} for ssh"))?;
        Ok(command("ssh", [self.ssh_target, &remote_command]))
    }

    pub(crate) fn run(&self, words: &[&str]) -> Result<()> {
        self.command(words)?.run()
    }

    pub(crate) fn read(&self, words: &[&str]) -> Result<String> {
        self.command(words)?.read()
    }

    pub(crate) fn succeeds(&self, words: &[&str]) -> Result<bool> {
        self.command(words)?.succeeds()
    }

    fn copy_to(&self, local_path: &Path, remote_path: &str) -> Result<()> {
        if self.is_local() {
            fs::copy(local_path, remote_path)
                .with_context(|| format!("copying {} to {remote_path}", local_path.display()))?;
            return Ok(());
        }
        Command::new("scp")
            .arg("-q")
            .arg(local_path)
            .arg(format!("{}:{remote_path}", self.ssh_target))
            .run()
    }
}

fn delete(target: &VmTarget) -> Result<()> {
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
    build_dpu_ignition(&machine, &create_args.source, local_ignition.path())?;
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
    let link_mtus = LinkMtus {
        wire: link_mtu(&hypervisor, create_args.wire_bridge.as_deref())?,
        tmfifo: link_mtu(&hypervisor, create_args.tmfifo_bridge.as_deref())?,
        uplink: link_mtu(&hypervisor, create_args.uplink_bridge.as_deref())?,
    };
    let install_args = virt_install_args(create_args, &ignition_path, &link_mtus);
    hypervisor.run(&install_args.iter().map(String::as_str).collect::<Vec<_>>())?;

    let extra_nics: String = [
        create_args
            .nic(create_args.wire_bridge.as_ref(), "wire")
            .map(|(bridge, mac)| format!(", wire {mac} on {bridge}")),
        create_args
            .tmfifo_bridge
            .as_ref()
            .map(|bridge| format!(", tmfifo {DPU_TMFIFO_MAC} on {bridge}")),
        create_args
            .nic(create_args.uplink_bridge.as_ref(), "uplink")
            .map(|(bridge, mac)| format!(", uplink p0 {mac} on {bridge}")),
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

/// The MTUs of the links to the host (`dpusim wire` bridges: a VXLAN's, less than the LAN's) and
/// to the leaf.
struct LinkMtus {
    wire: Option<u32>,
    tmfifo: Option<u32>,
    uplink: Option<u32>,
}

/// A link bridge's MTU, which the VM's NIC on it gets (virtio tells the guest), so the guest
/// doesn't send frames the VXLAN drops.
pub(crate) fn link_mtu(hypervisor: &Hypervisor, bridge: Option<&str>) -> Result<Option<u32>> {
    bridge
        .map(|bridge| {
            let mtu = hypervisor.read(&["cat", &format!("/sys/class/net/{bridge}/mtu")])?;
            mtu.trim()
                .parse()
                .with_context(|| format!("{bridge}'s MTU {mtu:?}"))
        })
        .transpose()
}

fn virt_install_args(
    create_args: &CreateVmArgs,
    ignition_path: &str,
    link_mtus: &LinkMtus,
) -> Vec<String> {
    let target = &create_args.target;
    let with_mtu = |nic: Option<(String, String)>, mtu: Option<u32>| {
        nic.map(|(bridge, mac)| (bridge, mac, mtu))
    };
    let nics = [
        Some((
            create_args.mgmt_bridge.clone(),
            target.nic_mac("mgmt"),
            None,
        )),
        with_mtu(
            create_args.nic(create_args.wire_bridge.as_ref(), "wire"),
            link_mtus.wire,
        ),
        with_mtu(
            create_args
                .tmfifo_bridge
                .clone()
                .map(|bridge| (bridge, DPU_TMFIFO_MAC.to_owned())),
            link_mtus.tmfifo,
        ),
        with_mtu(
            create_args.nic(create_args.uplink_bridge.as_ref(), "uplink"),
            link_mtus.uplink,
        ),
    ];
    let network_args = nics.into_iter().flatten().flat_map(|(bridge, mac, mtu)| {
        let mtu = mtu
            .map(|mtu| format!(",mtu.size={mtu}"))
            .unwrap_or_default();
        [
            "--network".to_owned(),
            format!("bridge={bridge},model=virtio,mac={mac}{mtu}"),
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
    let serial = create_args
        .serial
        .as_ref()
        .map(|serial| {
            (
                format!(",system.serial={serial}"),
                format!(",baseBoard.serial={serial}"),
            )
        })
        .unwrap_or_default();
    let tail_words = [
        format!(
            "--sysinfo=smbios,system.manufacturer=Nvidia,system.product=BlueField-3{},baseBoard.manufacturer=Nvidia,baseBoard.product=BlueField-3{}",
            serial.0, serial.1
        ),
        format!("--qemu-commandline=-fw_cfg name=opt/com.coreos/config,file={ignition_path}"),
        // Every DPU's tmfifo NIC has the same MAC, as on BlueFields; each is on its own link.
        "--check=mac_in_use=off".to_owned(),
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
    fn virt_install_puts_mgmt_wire_tmfifo_uplink_nics_in_order() -> Result<()> {
        let cli = CreateVmCli::try_parse_from([
            "create",
            "--bfcfg",
            "/tmp/bf.cfg",
            "--wire-bridge",
            "br-dpusim1",
            "--tmfifo-bridge",
            "dst-x",
            "--uplink-bridge",
            "dsu-x",
            "my-dpu",
        ])?;
        let link_mtus = LinkMtus {
            wire: Some(1450),
            tmfifo: Some(1450),
            uplink: Some(9000),
        };
        let install_args =
            virt_install_args(&cli.create_args, "/images/dpusim-my-dpu.ign", &link_mtus);
        let networks: Vec<&str> = install_args
            .iter()
            .zip(install_args.iter().skip(1))
            .filter(|(flag, _value)| *flag == "--network")
            .map(|(_flag, value)| value.as_str())
            .collect();
        let mac = |role| dpu_vm_nic_mac("my-dpu", role);
        let expected_networks = [
            format!("bridge=mgmt-br,model=virtio,mac={}", mac("mgmt")),
            format!(
                "bridge=br-dpusim1,model=virtio,mac={},mtu.size=1450",
                mac("wire")
            ),
            format!("bridge=dst-x,model=virtio,mac={DPU_TMFIFO_MAC},mtu.size=1450"),
            format!(
                "bridge=dsu-x,model=virtio,mac={},mtu.size=9000",
                mac("uplink")
            ),
        ];
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
