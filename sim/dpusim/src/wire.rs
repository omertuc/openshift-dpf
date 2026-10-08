//! The "PCIe link" between a host VM and its DPU VM when they run on
//! different hypervisors (M4+): a bridge on each hypervisor joined by a VXLAN.
//! Run it on both, with local/remote swapped. The host's igb NICs (hv side) or
//! the DPU's wire NIC (DPU side) are then attached to the bridge by libvirt.
//!
//! The bridge must act as a wire, not a switch: the DPU sends traffic between
//! two of the host's VFs back down the same link, so a learning bridge would
//! see the host's MACs on both sides and drop frames. `ageing_time 0` makes it
//! forget every MAC at once (it floods, which on a 2-3 port wire is free).
//! The host NICs are isolated from each other by libvirt (`m2b-switch-vm`).

use std::net::IpAddr;

use anyhow::Result;
use clap::Subcommand;

use crate::links::{delete_link_if_present, ensure_bridge, ip, link_exists};

#[derive(Subcommand)]
pub enum WireCommand {
    /// Create (or fix up) the wire bridge and its VXLAN.
    Up {
        bridge: String,
        vni: u32,
        local_ip: IpAddr,
        remote_ip: IpAddr,
    },
    /// Remove the wire bridge and its VXLAN.
    Down { bridge: String },
}

/// br-dpusim -> vxdpusim, dpusim1 -> vxdpusim1.
fn vxlan_name(bridge: &str) -> String {
    format!("vx{}", bridge.strip_prefix("br-").unwrap_or(bridge))
}

pub fn run(wire_command: WireCommand) -> Result<()> {
    match wire_command {
        WireCommand::Up {
            bridge,
            vni,
            local_ip,
            remote_ip,
        } => up(&bridge, vni, local_ip, remote_ip),
        WireCommand::Down { bridge } => {
            delete_link_if_present(&vxlan_name(&bridge))?;
            delete_link_if_present(&bridge)
        }
    }
}

fn up(bridge: &str, vni: u32, local_ip: IpAddr, remote_ip: IpAddr) -> Result<()> {
    let vxlan = vxlan_name(bridge);
    ensure_bridge(bridge, [], [])?;
    ip(["link", "set", bridge, "type", "bridge", "ageing_time", "0"])?;
    ip(["link", "set", bridge, "up"])?;
    if !link_exists(&vxlan)? {
        ip([
            "link",
            "add",
            &vxlan,
            "mtu",
            "1450",
            "type",
            "vxlan",
            "id",
            &vni.to_string(),
            "local",
            &local_ip.to_string(),
            "remote",
            &remote_ip.to_string(),
            "dstport",
            "4789",
        ])?;
    }
    ip(["link", "set", &vxlan, "master", bridge, "up"])?;
    println!("wire {bridge} <-> {vxlan} (VNI {vni}, {local_ip} -> {remote_ip})");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vxlan_name_drops_br_prefix() {
        assert_eq!(vxlan_name("br-dpusim1"), "vxdpusim1");
        assert_eq!(vxlan_name("dpusim1"), "vxdpusim1");
    }
}
