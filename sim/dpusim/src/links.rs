//! iproute2 helpers shared by the commands that build links.

use anyhow::{Context, Result};
use serde_json::Value;

use crate::cmd::{CommandExt, command};

/// `ip <args>`, failing unless it succeeds.
pub fn ip<const N: usize>(args: [&str; N]) -> Result<()> {
    command("ip", args).run()
}

pub fn link_exists(name: &str) -> Result<bool> {
    command("ip", ["link", "show", name]).succeeds()
}

pub fn delete_link_if_present(name: &str) -> Result<()> {
    if link_exists(name)? {
        ip(["link", "del", name])?;
    }
    Ok(())
}

/// Creates the veth pair `name`/`peer` with `mtu` unless `name` exists, and
/// brings both ends up.
pub fn ensure_veth(name: &str, peer: &str, mtu: u32) -> Result<()> {
    if !link_exists(name)? {
        let mtu = mtu.to_string();
        ip([
            "link", "add", name, "mtu", &mtu, "type", "veth", "peer", "name", peer, "mtu", &mtu,
        ])?;
    }
    ip(["link", "set", name, "up"])?;
    ip(["link", "set", peer, "up"])
}

/// Creates the bridge `name` unless it exists, with generic `ip link add`
/// arguments (like mtu) and bridge-specific ones (like `vlan_filtering`).
pub fn ensure_bridge<const L: usize, const B: usize>(
    name: &str,
    link_args: [&str; L],
    bridge_args: [&str; B],
) -> Result<()> {
    if !link_exists(name)? {
        command("ip", ["link", "add", name])
            .args(link_args)
            .args(["type", "bridge"])
            .args(bridge_args)
            .run()?;
    }
    Ok(())
}

fn links_json<const N: usize>(args: [&str; N]) -> Result<Vec<Value>> {
    let output = command("ip", ["-j"]).args(args).read()?;
    serde_json::from_str(&output).context("parsing ip -j output")
}

/// The netdev whose MAC is `mac`, if any.
pub fn link_with_mac(mac: &str) -> Result<Option<String>> {
    let links = links_json(["link", "show"])?;
    Ok(links
        .iter()
        .find(|link| {
            link.get("address")
                .and_then(Value::as_str)
                .is_some_and(|address| address.eq_ignore_ascii_case(mac))
        })
        .and_then(|link| link.get("ifname"))
        .and_then(Value::as_str)
        .map(str::to_owned))
}

pub fn is_veth(name: &str) -> Result<bool> {
    let links = links_json(["-d", "link", "show", name])?;
    Ok(links
        .iter()
        .any(|link| link.pointer("/linkinfo/info_kind").and_then(Value::as_str) == Some("veth")))
}
