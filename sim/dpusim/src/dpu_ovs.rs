//! DPF's DPU OVS setup (the DPUFlavor rawConfigScript, dpf-ovs.service) for a
//! DPU without BlueField hardware: the same bridges and patches, on OVS's
//! kernel datapath (OVS-DOCA's netdev datapath only takes DOCA, internal and
//! patch ports), with veths for the ports a BlueField has. Runs on the DPU at
//! boot (dpf-ovs-sim.service, installed by the DPU ignition).
//!
//! Host link (M4): with SIM_WIRE_MAC set in /etc/dpf/sim.env, the NIC with
//! that MAC is the BlueField's PCIe side, the VLAN trunk described in
//! `trunk`. A VLAN-filtering bridge turns the trunk into one netdev per
//! representor.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;

use anyhow::{Context, Result, bail};

use crate::cmd::{CommandExt, command};
use crate::links::{ensure_bridge, ensure_veth, ip, is_veth, link_exists, link_with_mac};
use crate::trunk::{TrunkedVf, trunked_vfs};

pub const SIM_ENV_PATH: &str = "/etc/dpf/sim.env";
/// MAC of the NIC that is the trunk to the host.
pub const SIM_WIRE_MAC: &str = "SIM_WIRE_MAC";
/// MAC the host PF has, which its representor carries too.
pub const SIM_HOST_PF_MAC: &str = "SIM_HOST_PF_MAC";

const HOST_PF_REPRESENTOR: &str = "rep0-0";
const HOST_PF_REPRESENTOR_WIRE_END: &str = "pf0hpf-w";
const HOST_PF_NETDEV: &str = "pf0hpf";
const TRUNK_BRIDGE: &str = "dpuwire";
const UPLINKS: [&str; 2] = ["p0", "p1"];

/// Settings from /etc/dpf/sim.env, falling back to the environment.
struct SimEnv {
    file_values: BTreeMap<String, String>,
}

impl SimEnv {
    fn load() -> Result<Self> {
        let text = match fs::read_to_string(SIM_ENV_PATH) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error).with_context(|| format!("reading {SIM_ENV_PATH}")),
        };
        let file_values = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| {
                (
                    key.trim().to_owned(),
                    value.trim().trim_matches('"').to_owned(),
                )
            })
            .collect();
        Ok(Self { file_values })
    }

    fn get(&self, key: &str) -> Option<String> {
        self.file_values
            .get(key)
            .cloned()
            .or_else(|| std::env::var(key).ok())
            .filter(|value| !value.is_empty())
    }

    fn get_number<T: std::str::FromStr>(&self, key: &str, default: T) -> Result<T>
    where
        T::Err: std::error::Error + Send + Sync + 'static,
    {
        self.get(key)
            .map_or(Ok(default), |value| value.parse())
            .with_context(|| format!("parsing {key}"))
    }
}

pub fn run() -> Result<()> {
    let sim_env = SimEnv::load()?;
    let mtu: u32 = sim_env.get_number("MTU", 9000)?;
    let vfs_per_pf: u16 = sim_env.get_number("SIM_NUM_VFS", 7)?;

    command("modprobe", ["openvswitch"]).run()?;
    create_representors(mtu, vfs_per_pf, sim_env.get(SIM_HOST_PF_MAC).as_deref())?;
    if let Some(wire_mac) = sim_env.get(SIM_WIRE_MAC) {
        connect_trunk(&wire_mac, vfs_per_pf)?;
    }
    UPLINKS
        .into_iter()
        .try_for_each(|uplink| prepare_uplink(uplink, mtu))?;
    build_ovs_bridges()?;
    ["br-ovn", "br-dpu"]
        .into_iter()
        .try_for_each(|bridge| ip(["link", "set", bridge, "mtu", &mtu.to_string(), "up"]))
}

/// The host PF's representor and one per trunked VF.
///
/// The host PF's representor is pf0hpf on a BlueField. OVN-K's
/// `--simulate-dpu` mode wants it as an OVS port named rep0-0 and derives the
/// host gateway MAC from the host's node name instead of reading it; DPF's
/// cniprovisioner (sim mode) looks for a netdev named pf0hpf and serves the
/// host PF its DHCP lease by that MAC. So: rep0-0 is the OVS port, pf0hpf a
/// dummy carrying the MAC, and both use the MAC the host PF is given.
fn create_representors(mtu: u32, vfs_per_pf: u16, host_pf_mac: Option<&str>) -> Result<()> {
    ensure_veth(HOST_PF_REPRESENTOR, HOST_PF_REPRESENTOR_WIRE_END, mtu)?;
    if !link_exists(HOST_PF_NETDEV)? {
        ip(["link", "add", HOST_PF_NETDEV, "type", "dummy"])?;
    }
    ip(["link", "set", HOST_PF_NETDEV, "up"])?;
    if let Some(host_pf_mac) = host_pf_mac {
        ip(["link", "set", HOST_PF_REPRESENTOR, "address", host_pf_mac])?;
        ip(["link", "set", HOST_PF_NETDEV, "address", host_pf_mac])?;
    }
    trunked_vfs(vfs_per_pf).try_for_each(|trunked_vf| {
        ensure_veth(
            &trunked_vf.representor(),
            &trunked_vf.representor_wire_end(),
            mtu,
        )
    })
}

/// Puts the wire NIC and the representors' wire ends on a VLAN-filtering
/// bridge: each VF's VLAN comes out untagged at its representor.
fn connect_trunk(wire_mac: &str, vfs_per_pf: u16) -> Result<()> {
    let Some(wire_nic) = link_with_mac(wire_mac)? else {
        bail!("no NIC with MAC {wire_mac}");
    };
    ensure_bridge(TRUNK_BRIDGE, [], ["vlan_filtering", "1"])?;
    ip(["link", "set", TRUNK_BRIDGE, "up"])?;
    ip(["link", "set", &wire_nic, "master", TRUNK_BRIDGE])?;
    ip(["link", "set", &wire_nic, "up"])?;
    ip([
        "link",
        "set",
        HOST_PF_REPRESENTOR_WIRE_END,
        "master",
        TRUNK_BRIDGE,
    ])?;
    trunked_vfs(vfs_per_pf).try_for_each(|trunked_vf| trunk_vf(trunked_vf, &wire_nic))
}

fn trunk_vf(trunked_vf: TrunkedVf, wire_nic: &str) -> Result<()> {
    let wire_end = trunked_vf.representor_wire_end();
    let vlan_id = trunked_vf.vlan_id()?.to_string();
    ip(["link", "set", &wire_end, "master", TRUNK_BRIDGE])?;
    command("bridge", ["vlan", "del", "dev", &wire_end, "vid", "1"]).run_ignoring_failure()?;
    command(
        "bridge",
        [
            "vlan", "add", "dev", &wire_end, "vid", &vlan_id, "pvid", "untagged",
        ],
    )
    .run()?;
    command("bridge", ["vlan", "add", "dev", wire_nic, "vid", &vlan_id]).run()
}

/// A NIC already named p0/p1 (`dpu-vm create --fabric-bridge`, M6) is the
/// uplink itself; otherwise a veth whose far end goes nowhere.
fn prepare_uplink(uplink: &str, mtu: u32) -> Result<()> {
    if !link_exists(uplink)? {
        return ensure_veth(uplink, &format!("{uplink}-fab"), mtu);
    }
    if is_veth(uplink)? {
        return ip(["link", "set", uplink, "up"]);
    }
    let mtu_text = mtu.to_string();
    if command("ip", ["link", "set", uplink, "mtu", &mtu_text, "up"]).succeeds()? {
        return Ok(());
    }
    ip(["link", "set", uplink, "up"])
}

fn ovs_vsctl<const N: usize>(args: [&str; N]) -> Result<()> {
    command("ovs-vsctl", ["--timeout", "15"]).args(args).run()
}

fn build_ovs_bridges() -> Result<()> {
    ovs_vsctl([
        "set",
        "Open_vSwitch",
        ".",
        "external-ids:ovn-bridge-datapath-type=system",
    ])?;
    ["br-sfc", "br-hbn"].into_iter().try_for_each(|bridge| {
        ovs_vsctl([
            "--may-exist",
            "add-br",
            bridge,
            "--",
            "set",
            "bridge",
            bridge,
            "datapath_type=system",
            "fail_mode=secure",
        ])
    })?;
    UPLINKS.into_iter().try_for_each(|uplink| {
        ovs_vsctl([
            "--may-exist",
            "add-port",
            "br-sfc",
            uplink,
            "--",
            "set",
            "Interface",
            uplink,
            "type=system",
            "--",
            "set",
            "Port",
            uplink,
            "external_ids:dpf-type=physical",
        ])
    })?;

    // br-dpu is the bridge ovnkube manages (br-ex in OVN-K docs).
    ovs_vsctl([
        "--may-exist",
        "add-br",
        "br-dpu",
        "--",
        "set",
        "bridge",
        "br-dpu",
        "datapath_type=system",
    ])?;
    ovs_vsctl(["br-set-external-id", "br-dpu", "bridge-id", "br-dpu"])?;
    ovs_vsctl([
        "br-set-external-id",
        "br-dpu",
        "bridge-uplink",
        "pbrdputobrovn",
    ])?;
    ovs_vsctl([
        "--may-exist",
        "add-port",
        "br-dpu",
        HOST_PF_REPRESENTOR,
        "--",
        "set",
        "Interface",
        HOST_PF_REPRESENTOR,
        "type=system",
    ])?;

    // br-ovn sits between the sfc-controller managed br-sfc and OVN-K.
    ovs_vsctl([
        "--may-exist",
        "add-br",
        "br-ovn",
        "--",
        "set",
        "bridge",
        "br-ovn",
        "datapath_type=system",
    ])?;
    ovs_vsctl([
        "--may-exist",
        "add-port",
        "br-ovn",
        "pbrovntobrdpu",
        "--",
        "set",
        "Interface",
        "pbrovntobrdpu",
        "type=patch",
        "options:peer=pbrdputobrovn",
    ])?;
    ovs_vsctl([
        "--may-exist",
        "add-port",
        "br-dpu",
        "pbrdputobrovn",
        "--",
        "set",
        "Interface",
        "pbrdputobrovn",
        "type=patch",
        "options:peer=pbrovntobrdpu",
    ])
}
