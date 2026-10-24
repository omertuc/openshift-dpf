//! The leaf router a simulated cluster's DPUs are cabled to, standing in for the top-of-rack
//! switch: an FRR container on the hypervisor, one per cluster. Each DPU VM's p0 uplink
//! (`dpu-vm create --uplink-bridge`) is on its own bridge with one of the leaf's ports, and HBN on
//! the DPU peers with the leaf over BGP unnumbered (remote-as external), as with a real leaf. The
//! leaf learns the DPUs' VTEP subnets and loopbacks and gives them a default route.
//!
//! The leaf's other side is the lab LAN, with an address from the lab's DHCP server: the
//! cluster's nodes route the VTEP subnet through it (openshift-dpf's SIMULATED_DPUS_ONLY), and it
//! NATs whatever else the DPUs send out, with TCP clamped to the LAN's MTU.

use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use sha2::{Digest, Sha256};

use crate::cmd::{CommandExt, command};
use crate::links::{delete_link_if_present, ensure_bridge, ip};
use crate::mac::dpu_vm_nic_mac;

const FRR_IMAGE: &str = "quay.io/frrouting/frr:10.3.1";
const CONFIG_ROOT: &str = "/var/lib/dpusim-leaf";
const LEAF_ASN: u32 = 65000;
const PORT_MTU: &str = "9000";
/// TCP segments the leaf lets through to and from the LAN, which is 1500.
const LAN_MSS: &str = "1360";
const LAN_PORT: &str = "lan0";

#[derive(Subcommand)]
pub enum LeafCommand {
    /// Start the cluster's leaf unless it runs, with its LAN port and its DPU ports.
    Up {
        cluster: String,
        /// The hypervisor's LAN bridge.
        #[arg(long, default_value = "mgmt-br")]
        lan_bridge: String,
    },
    /// Give the leaf a port for a DPU, on the bridge `dsu-<port>` its uplink goes on.
    Attach {
        cluster: String,
        /// Up to 8 lowercase letters and digits, e.g. the lease id.
        port: String,
        #[arg(long, default_value = "mgmt-br")]
        lan_bridge: String,
    },
    /// Remove a DPU's port; the leaf goes with its last one.
    Detach { cluster: String, port: String },
    /// Print the leaf's LAN address, the cluster's nodes' next hop to the VTEP subnet.
    Address { cluster: String },
}

pub fn run(leaf_command: LeafCommand) -> Result<()> {
    match leaf_command {
        LeafCommand::Up {
            cluster,
            lan_bridge,
        } => Leaf::new(&cluster)?.up(&lan_bridge).map(drop),
        LeafCommand::Attach {
            cluster,
            port,
            lan_bridge,
        } => Leaf::new(&cluster)?.attach(&port, &lan_bridge),
        LeafCommand::Detach { cluster, port } => Leaf::new(&cluster)?.detach(&port),
        LeafCommand::Address { cluster } => {
            println!("{}", Leaf::new(&cluster)?.address()?);
            Ok(())
        }
    }
}

/// The bridge a DPU's uplink and its leaf port are on.
pub fn port_bridge(port: &str) -> String {
    format!("dsu-{port}")
}

struct Leaf {
    cluster: String,
}

impl Leaf {
    fn new(cluster: &str) -> Result<Self> {
        let valid = !cluster.is_empty()
            && cluster.len() <= 40
            && cluster
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if !valid {
            bail!("cluster {cluster:?}: up to 40 lowercase letters, digits and dashes");
        }
        Ok(Self {
            cluster: cluster.to_owned(),
        })
    }

    fn container(&self) -> String {
        format!("dpusim-leaf-{}", self.cluster)
    }

    fn config_dir(&self) -> PathBuf {
        PathBuf::from(CONFIG_ROOT).join(&self.cluster)
    }

    /// The ports it has, kept so a restarted leaf gets them all back.
    fn ports_file(&self) -> PathBuf {
        self.config_dir().join("ports")
    }

    /// The hypervisor end of its LAN port (interface names are at most 15 characters).
    fn lan_link(&self) -> String {
        let digest = Sha256::digest(self.cluster.as_bytes());
        let tag = digest.iter().take(4).fold(String::new(), |mut tag, byte| {
            let _ = write!(tag, "{byte:02x}");
            tag
        });
        format!("dl-{tag}")
    }

    fn lan_mac(&self) -> String {
        dpu_vm_nic_mac(&format!("leaf-{}", self.cluster), "lan")
    }

    fn pid(&self) -> Option<String> {
        let running = command(
            "podman",
            ["inspect", "-f", "{{.State.Running}} {{.State.Pid}}"],
        )
        .arg(self.container())
        .read()
        .unwrap_or_default();
        match running.split_whitespace().collect::<Vec<_>>()[..] {
            ["true", pid] => Some(pid.to_owned()),
            _ => None,
        }
    }

    /// A command in the leaf's network namespace, run from the hypervisor.
    fn in_netns(pid: &str, args: &[&str]) -> Result<String> {
        command("nsenter", ["-t", pid, "-n"]).args(args).read()
    }

    fn vtysh(&self, lines: &[&str]) -> Result<()> {
        let mut vtysh = command("podman", ["exec"]);
        vtysh.arg(self.container()).arg("vtysh");
        let header = [
            "configure terminal".to_owned(),
            format!("router bgp {LEAF_ASN}"),
        ];
        for line in header
            .iter()
            .map(String::as_str)
            .chain(lines.iter().copied())
        {
            vtysh.args(["-c", line]);
        }
        vtysh.run()
    }

    fn ports(&self) -> Vec<String> {
        fs::read_to_string(self.ports_file())
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn save_ports(&self, ports: &[String]) -> Result<()> {
        let path = self.ports_file();
        fs::write(&path, format!("{}\n", ports.join("\n")))
            .with_context(|| format!("writing {}", path.display()))
    }

    /// Starts the leaf unless it runs, and returns its PID (the handle on its netns).
    fn up(&self, lan_bridge: &str) -> Result<String> {
        if let Some(pid) = self.pid() {
            return Ok(pid);
        }
        let config_dir = self.config_dir();
        fs::create_dir_all(&config_dir)
            .with_context(|| format!("creating {}", config_dir.display()))?;
        let daemons = command("podman", ["run", "--rm", "--entrypoint", "cat", FRR_IMAGE])
            .arg("/etc/frr/daemons")
            .read()?;
        for (file, contents) in [
            ("daemons", enable_bgpd(&daemons)),
            ("frr.conf", frr_config(&self.cluster)),
            ("vtysh.conf", String::new()),
        ] {
            let path = config_dir.join(file);
            fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))?;
        }
        command("podman", ["rm", "--force", "--ignore"])
            .arg(self.container())
            .run_ignoring_failure()?;
        command(
            "podman",
            ["run", "-d", "--privileged", "--network", "none", "--name"],
        )
        .arg(self.container())
        .arg("-v")
        .arg(format!("{}:/etc/frr:Z", config_dir.display()))
        .arg(FRR_IMAGE)
        .run()?;
        let pid = self
            .pid()
            .with_context(|| format!("{} didn't start", self.container()))?;
        Self::in_netns(
            &pid,
            &[
                "sysctl",
                "-qw",
                "net.ipv4.ip_forward=1",
                "net.ipv6.conf.all.forwarding=1",
            ],
        )?;
        Self::in_netns(&pid, &["ip", "link", "set", "lo", "up"])?;
        self.attach_lan(&pid, lan_bridge)?;
        for port in self.ports() {
            self.plug(&pid, &port)?;
        }
        Ok(pid)
    }

    /// The LAN port: an address from the lab's DHCP server (udhcpc keeps renewing it), the
    /// default route, and NAT for everything but the LAN.
    fn attach_lan(&self, pid: &str, lan_bridge: &str) -> Result<()> {
        let lan_link = self.lan_link();
        delete_link_if_present(&lan_link)?;
        ip([
            "link", "add", &lan_link, "type", "veth", "peer", "name", LAN_PORT,
        ])?;
        ip(["link", "set", &lan_link, "master", lan_bridge, "up"])?;
        ip(["link", "set", LAN_PORT, "netns", pid])?;
        Self::in_netns(
            pid,
            &[
                "ip",
                "link",
                "set",
                LAN_PORT,
                "address",
                &self.lan_mac(),
                "up",
            ],
        )?;
        command("podman", ["exec", "-d"])
            .arg(self.container())
            .args(["udhcpc", "-f", "-i", LAN_PORT])
            .run()?;
        let lan_subnet = (0..60)
            .find_map(|_attempt| {
                let subnet = lan_subnet(pid);
                if subnet.is_none() {
                    sleep(Duration::from_secs(1));
                }
                subnet
            })
            .with_context(|| format!("{} got no address on {lan_bridge}", self.container()))?;
        command("nsenter", ["-t", pid, "-n", "nft", "-f", "-"])
            .run_with_stdin(nat_rules(&lan_subnet).as_bytes())
            .context("setting up the leaf's NAT")
    }

    /// Plugs port `port` into its bridge with a fresh veth, and peers with whatever is there.
    fn plug(&self, pid: &str, port: &str) -> Result<()> {
        let bridge = port_bridge(port);
        let hypervisor_end = format!("dsp-{port}");
        let leaf_end = format!("swp{port}");
        ensure_bridge(&bridge, ["mtu", PORT_MTU], [])?;
        ip(["link", "set", &bridge, "up"])?;
        let plugged = Self::in_netns(pid, &["ip", "-o", "link", "show"])?
            .lines()
            .any(|line| line.contains(&format!(" {leaf_end}@")));
        if !plugged {
            delete_link_if_present(&hypervisor_end)?;
            ip([
                "link",
                "add",
                &hypervisor_end,
                "mtu",
                PORT_MTU,
                "type",
                "veth",
                "peer",
                "name",
                &leaf_end,
                "mtu",
                PORT_MTU,
            ])?;
            ip(["link", "set", &hypervisor_end, "master", &bridge, "up"])?;
            ip(["link", "set", &leaf_end, "netns", pid])?;
            Self::in_netns(pid, &["ip", "link", "set", &leaf_end, "up"])?;
        }
        self.vtysh(&[&format!("neighbor {leaf_end} interface peer-group dpus")])
    }

    fn attach(&self, port: &str, lan_bridge: &str) -> Result<()> {
        let valid = !port.is_empty()
            && port.len() <= 8
            && port
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
        if !valid {
            bail!("port {port:?}: up to 8 lowercase letters and digits");
        }
        let pid = self.up(lan_bridge)?;
        let mut ports = self.ports();
        if !ports.iter().any(|known| known == port) {
            ports.push(port.to_owned());
            self.save_ports(&ports)?;
        }
        self.plug(&pid, port)?;
        println!(
            "{} port swp{port} on {}",
            self.container(),
            port_bridge(port)
        );
        Ok(())
    }

    fn detach(&self, port: &str) -> Result<()> {
        if self.pid().is_some() {
            // Gone already if the leaf restarted without it.
            let _ = self.vtysh(&[&format!("no neighbor swp{port} interface")]);
        }
        delete_link_if_present(&format!("dsp-{port}"))?;
        delete_link_if_present(&port_bridge(port))?;
        let ports: Vec<String> = self
            .ports()
            .into_iter()
            .filter(|known| known != port)
            .collect();
        if !ports.is_empty() {
            return self.save_ports(&ports);
        }
        command("podman", ["rm", "--force", "--ignore"])
            .arg(self.container())
            .run_ignoring_failure()?;
        delete_link_if_present(&self.lan_link())?;
        let config_dir = self.config_dir();
        if config_dir.exists() {
            fs::remove_dir_all(&config_dir)
                .with_context(|| format!("removing {}", config_dir.display()))?;
        }
        println!("{} removed with its last port", self.container());
        Ok(())
    }

    fn address(&self) -> Result<String> {
        let pid = self
            .pid()
            .with_context(|| format!("{} isn't running", self.container()))?;
        let addresses = Self::in_netns(&pid, &["ip", "-4", "-o", "addr", "show", LAN_PORT])?;
        addresses
            .split_whitespace()
            .skip_while(|word| *word != "inet")
            .nth(1)
            .and_then(|cidr| cidr.split('/').next())
            .map(str::to_owned)
            .with_context(|| format!("{} has no LAN address", self.container()))
    }
}

/// The LAN's subnet once the leaf has an address on it.
fn lan_subnet(pid: &str) -> Option<String> {
    let routes = Leaf::in_netns(
        pid,
        &[
            "ip", "-4", "-o", "route", "show", "dev", LAN_PORT, "proto", "kernel",
        ],
    )
    .ok()?;
    routes.split_whitespace().next().map(str::to_owned)
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

/// The DPUs (peer group `dpus`, added per port) get a default route; the leaf takes theirs.
fn frr_config(cluster: &str) -> String {
    format!(
        "frr defaults datacenter
hostname leaf-{cluster}
!
router bgp {LEAF_ASN}
 bgp router-id 10.255.0.1
 bgp bestpath as-path multipath-relax
 neighbor dpus peer-group
 neighbor dpus remote-as external
 address-family ipv4 unicast
  neighbor dpus default-originate
 exit-address-family
 address-family ipv6 unicast
  neighbor dpus activate
 exit-address-family
!
"
    )
}

/// NAT out the LAN for anything but the LAN itself (the cluster's nodes route the VTEP subnet to
/// the leaf, so their tunnels must keep their addresses), and TCP clamped to the LAN's MTU.
fn nat_rules(lan_subnet: &str) -> String {
    format!(
        "table ip dpusim_leaf
delete table ip dpusim_leaf
table ip dpusim_leaf {{
  chain postrouting {{
    type nat hook postrouting priority srcnat;
    oifname \"{LAN_PORT}\" ip daddr != {lan_subnet} masquerade
  }}
  chain forward {{
    type filter hook forward priority mangle;
    tcp flags syn tcp option maxseg size set {LAN_MSS}
  }}
}}
"
    )
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
    fn names_fit_and_differ_per_cluster() -> Result<()> {
        let (one, two) = (Leaf::new("simtest")?, Leaf::new("other-cluster")?);
        assert!(one.lan_link().len() <= 15);
        assert_ne!(one.lan_link(), two.lan_link());
        assert_ne!(one.lan_mac(), two.lan_mac());
        assert!(Leaf::new("Bad_Name").is_err());
        Ok(())
    }

    #[test]
    fn nat_leaves_the_lan_alone() {
        assert!(nat_rules("10.6.135.0/24").contains("ip daddr != 10.6.135.0/24 masquerade"));
    }
}
