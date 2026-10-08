//! A simulated top-of-rack leaf for DPU VMs (M6): an FRR container on the
//! hypervisor with one port (swpN) per DPU, each on its own bridge
//! (br-dpufabN) that the DPU VM's p0 uplink NIC also sits on
//! (`dpu-vm create --fabric-bridge br-dpufabN`). HBN on each DPU peers with
//! it over BGP unnumbered (remote-as external), as with a real leaf switch, so
//! the DPUs learn each other's loopbacks and OVN VTEP subnets.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use crate::cmd::{CommandExt, command};
use crate::links::{delete_link_if_present, ensure_bridge, ip};

const LEAF_CONTAINER: &str = "dpusim-leaf";
const LEAF_CONFIG_DIR: &str = "/var/lib/dpusim-leaf";
const FABRIC_MTU: &str = "9000";

#[derive(Subcommand)]
pub enum LeafCommand {
    /// (Re)start the leaf container and attach its ports.
    Up(LeafUpArgs),
    /// Remove the leaf container, its ports and their bridges.
    Down {
        /// Number of DPU ports to remove.
        #[arg(default_value_t = 2)]
        ports: u16,
    },
}

#[derive(Args)]
pub struct LeafUpArgs {
    /// Number of DPU ports (swp0..swpN-1 on br-dpufab0..N-1).
    #[arg(default_value_t = 2)]
    ports: u16,
    #[arg(
        long,
        env = "FRR_IMAGE",
        default_value = "quay.io/frrouting/frr:10.3.1"
    )]
    image: String,
    #[arg(long, env = "LEAF_ASN", default_value_t = 65000)]
    asn: u32,
}

/// The leaf's port facing DPU `index`, inside the container.
fn switch_port(index: u16) -> String {
    format!("swp{index}")
}

/// The hypervisor end of that port's veth.
fn hypervisor_port(index: u16) -> String {
    format!("lf{index}")
}

fn fabric_bridge(index: u16) -> String {
    format!("br-dpufab{index}")
}

pub fn run(leaf_command: LeafCommand) -> Result<()> {
    match leaf_command {
        LeafCommand::Up(up_args) => up(&up_args),
        LeafCommand::Down { ports } => down(ports),
    }
}

fn remove_container() -> Result<()> {
    command("podman", ["rm", "--force", "--ignore", LEAF_CONTAINER]).run_silently()
}

fn down(ports: u16) -> Result<()> {
    remove_container()?;
    (0..ports).try_for_each(|index| {
        delete_link_if_present(&hypervisor_port(index))?;
        delete_link_if_present(&fabric_bridge(index))
    })
}

fn up(up_args: &LeafUpArgs) -> Result<()> {
    let config_dir = Path::new(LEAF_CONFIG_DIR);
    fs::create_dir_all(config_dir).with_context(|| format!("creating {}", config_dir.display()))?;
    write_daemons_file(config_dir, &up_args.image)?;
    let frr_conf = config_dir.join("frr.conf");
    fs::write(&frr_conf, frr_config(up_args.asn, up_args.ports))
        .with_context(|| format!("writing {}", frr_conf.display()))?;
    let vtysh_conf = config_dir.join("vtysh.conf");
    if !vtysh_conf.exists() {
        fs::write(&vtysh_conf, "").with_context(|| format!("writing {}", vtysh_conf.display()))?;
    }

    // (Re)start first: a restart gets a fresh network namespace, and FRR picks
    // up the ports attached afterwards at runtime.
    let container_pid = start_container(&up_args.image)?;
    (0..up_args.ports).try_for_each(|index| attach_port(index, &container_pid))?;
    command(
        "nsenter",
        [
            "-t",
            &container_pid,
            "-n",
            "sysctl",
            "-qw",
            "net.ipv4.ip_forward=1",
            "net.ipv6.conf.all.forwarding=1",
        ],
    )
    .run()?;

    let port_summary: Vec<String> = (0..up_args.ports)
        .map(|index| format!("{}->{}", switch_port(index), fabric_bridge(index)))
        .collect();
    println!(
        "leaf {LEAF_CONTAINER} (AS {}) up with ports: {}",
        up_args.asn,
        port_summary.join(" ")
    );
    Ok(())
}

/// FRR's daemons file with bgpd on: kept from an earlier run, else the image's.
fn write_daemons_file(config_dir: &Path, image: &str) -> Result<()> {
    let daemons_path = config_dir.join("daemons");
    let daemons = if daemons_path.exists() {
        fs::read_to_string(&daemons_path)
            .with_context(|| format!("reading {}", daemons_path.display()))?
    } else {
        command(
            "podman",
            [
                "run",
                "--rm",
                "--entrypoint",
                "cat",
                image,
                "/etc/frr/daemons",
            ],
        )
        .read()?
    };
    fs::write(&daemons_path, enable_bgpd(&daemons))
        .with_context(|| format!("writing {}", daemons_path.display()))
}

fn enable_bgpd(daemons: &str) -> String {
    daemons
        .split_inclusive('\n')
        .map(|line| {
            line.strip_prefix("bgpd=no")
                .map_or_else(|| line.to_owned(), |rest| format!("bgpd=yes{rest}"))
        })
        .collect()
}

fn frr_config(asn: u32, ports: u16) -> String {
    let neighbors = (0..ports)
        .map(|index| {
            format!(
                " neighbor {} interface peer-group fabric\n",
                switch_port(index)
            )
        })
        .collect::<Vec<_>>()
        .concat();
    format!(
        "frr defaults datacenter
hostname {LEAF_CONTAINER}
!
router bgp {asn}
 bgp router-id 10.255.0.1
 bgp bestpath as-path multipath-relax
 neighbor fabric peer-group
 neighbor fabric remote-as external
{neighbors} address-family ipv4 unicast
  redistribute connected
 exit-address-family
 address-family ipv6 unicast
  neighbor fabric activate
 exit-address-family
!
"
    )
}

/// Starts the container fresh and returns its PID, the handle on its netns.
fn start_container(image: &str) -> Result<String> {
    remove_container()?;
    let volume = format!("{LEAF_CONFIG_DIR}:/etc/frr:Z");
    command(
        "podman",
        [
            "run",
            "-d",
            "--name",
            LEAF_CONTAINER,
            "--privileged",
            "--network",
            "none",
            "-v",
            &volume,
            image,
        ],
    )
    .run_silently()?;
    let container_pid = command(
        "podman",
        ["inspect", "-f", "{{.State.Pid}}", LEAF_CONTAINER],
    )
    .read()?;
    Ok(container_pid.trim().to_owned())
}

/// Plugs leaf port `index` into its fabric bridge with a fresh veth.
fn attach_port(index: u16, container_pid: &str) -> Result<()> {
    let bridge = fabric_bridge(index);
    let hypervisor_end = hypervisor_port(index);
    let leaf_end = switch_port(index);
    ensure_bridge(&bridge, ["mtu", FABRIC_MTU], [])?;
    ip(["link", "set", &bridge, "up"])?;
    delete_link_if_present(&hypervisor_end)?;
    ip([
        "link",
        "add",
        &hypervisor_end,
        "mtu",
        FABRIC_MTU,
        "type",
        "veth",
        "peer",
        "name",
        &leaf_end,
        "mtu",
        FABRIC_MTU,
    ])?;
    ip(["link", "set", &hypervisor_end, "master", &bridge, "up"])?;
    ip(["link", "set", &leaf_end, "netns", container_pid])?;
    command(
        "nsenter",
        [
            "-t",
            container_pid,
            "-n",
            "ip",
            "link",
            "set",
            &leaf_end,
            "up",
        ],
    )
    .run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_bgpd_flips_only_bgpd() {
        assert_eq!(
            enable_bgpd("zebra=yes\nbgpd=no\nospfd=no\n"),
            "zebra=yes\nbgpd=yes\nospfd=no\n"
        );
    }

    #[test]
    fn frr_config_matches_leaf_sh() {
        let expected = "frr defaults datacenter\nhostname dpusim-leaf\n!\nrouter bgp 65000\n bgp router-id 10.255.0.1\n bgp bestpath as-path multipath-relax\n neighbor fabric peer-group\n neighbor fabric remote-as external\n neighbor swp0 interface peer-group fabric\n neighbor swp1 interface peer-group fabric\n address-family ipv4 unicast\n  redistribute connected\n exit-address-family\n address-family ipv6 unicast\n  neighbor fabric activate\n exit-address-family\n!\n";
        assert_eq!(frr_config(65000, 2), expected);
    }
}
