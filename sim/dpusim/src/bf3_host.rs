//! M2b host side: makes the patched-QEMU igb pair (p0/p1, functions .0/.1 of
//! one slot, VPD with the DPU serial) look like a BlueField-3 to DPF's host
//! components. The igb driver keeps seeing an igb (real PF/VF netdevs and
//! SR-IOV); only the PFs' sysfs "device" file, which DPF matches on, is
//! overlaid. Runs at boot (bf3sim-host.service), before kubelet.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::Args;

use crate::cmd::{CommandExt, command};
use crate::links::ip;
use crate::mac::simulated_host_pf_mac;
use crate::trunk::trunked_vfs;

const BF3_DEVICE_ID: &str = "0xa2dc";
const OVERLAY_DIR: &str = "/run/bf3sim";
const HOST_PFS: [&str; 2] = ["p0", "p1"];
/// The NetworkManager connection, named like the PF, that gets p0 its address.
const HOST_PF_CONNECTION: &str = "p0";

#[derive(Args)]
pub struct Bf3HostArgs {
    /// VFs to create on each PF.
    #[arg(long, env = "NUM_VFS", default_value_t = 7)]
    num_vfs: u16,
}

pub fn run(host_args: &Bf3HostArgs) -> Result<()> {
    let overlay = Path::new(OVERLAY_DIR).join("device");
    fs::create_dir_all(OVERLAY_DIR).with_context(|| format!("creating {OVERLAY_DIR}"))?;
    fs::write(&overlay, format!("{BF3_DEVICE_ID}\n"))
        .with_context(|| format!("writing {}", overlay.display()))?;

    HOST_PFS
        .into_iter()
        .try_for_each(|host_pf| disguise_pf(host_pf, &overlay, host_args.num_vfs))?;
    // Tag each VF's traffic with its own VLAN so the DPU can tell the VFs
    // apart on the one wire (M4); see `trunk`.
    trunked_vfs(host_args.num_vfs).try_for_each(|trunked_vf| {
        ip([
            "link",
            "set",
            &trunked_vf.host_pf(),
            "vf",
            &trunked_vf.vf.to_string(),
            "vlan",
            &trunked_vf.vlan_id()?.to_string(),
        ])
    })?;
    configure_host_pf_dhcp()
}

fn read_sysfs(path: &Path) -> Result<String> {
    fs::read_to_string(path)
        .map(|text| text.trim().to_owned())
        .with_context(|| format!("reading {}", path.display()))
}

/// Gives the PF its VFs and makes its PCI device id read as a BlueField-3's.
fn disguise_pf(host_pf: &str, overlay: &Path, num_vfs: u16) -> Result<()> {
    let device_link = PathBuf::from(format!("/sys/class/net/{host_pf}/device"));
    let pci_device = fs::canonicalize(&device_link)
        .with_context(|| format!("resolving {}", device_link.display()))?;
    if !pci_device.join("vpd").exists() {
        bail!(
            "{host_pf} ({}) has no VPD; is the VM on the patched QEMU?",
            pci_device.display()
        );
    }
    let numvfs_path = pci_device.join("sriov_numvfs");
    if read_sysfs(&numvfs_path)? == "0" {
        fs::write(&numvfs_path, num_vfs.to_string())
            .with_context(|| format!("writing {}", numvfs_path.display()))?;
    }
    let device_id_path = pci_device.join("device");
    if !is_mountpoint(&device_id_path)? {
        Command::new("mount")
            .arg("--bind")
            .arg(overlay)
            .arg(&device_id_path)
            .run()?;
    }
    println!(
        "{host_pf}: {} now reads {} with {} VFs",
        pci_device.display(),
        read_sysfs(&device_id_path)?,
        read_sysfs(&numvfs_path)?
    );
    Ok(())
}

fn is_mountpoint(path: &Path) -> Result<bool> {
    Command::new("mountpoint").arg("-q").arg(path).succeeds()
}

/// The host PF p0 gets its address by DHCP from the DPU (M4), with the MAC
/// OVN-K's `--simulate-dpu` mode expects for the host gateway, derived from
/// the node name.
fn configure_host_pf_dhcp() -> Result<()> {
    let hostname = read_sysfs(Path::new("/proc/sys/kernel/hostname"))?;
    let node = hostname.split('.').next().unwrap_or(&hostname);
    let gateway_mac = simulated_host_pf_mac(node);
    let connections = command("nmcli", ["-t", "-f", "NAME", "connection", "show"]).read()?;
    if !connections.lines().any(|name| name == HOST_PF_CONNECTION) {
        command(
            "nmcli",
            [
                "connection",
                "add",
                "type",
                "ethernet",
                "ifname",
                "p0",
                "con-name",
                HOST_PF_CONNECTION,
                "ipv4.method",
                "auto",
                "ipv4.never-default",
                "yes",
                "ipv4.dhcp-timeout",
                "2147483647",
                "ipv6.method",
                "disabled",
                "ethernet.cloned-mac-address",
                &gateway_mac,
            ],
        )
        .run_silently()?;
    }
    println!("p0: DHCP from the DPU as {gateway_mac}");
    Ok(())
}
