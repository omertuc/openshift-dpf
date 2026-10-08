//! MAC addresses the simulation derives from names, so they stay stable
//! across reruns (DHCP leases) and agree between the tools that compute them.

use anyhow::{Result, bail};
use sha2::{Digest, Sha256};

/// A locally administered QEMU MAC: 52:54:00 followed by `tail`.
fn qemu_mac(tail: impl Iterator<Item = u8>) -> String {
    let tail_octets: Vec<String> = tail.map(|octet| format!("{octet:02x}")).collect();
    format!("52:54:00:{}", tail_octets.join(":"))
}

/// The MAC of a DPU VM's NIC with the given role (mgmt, wire, fabric):
/// 52:54:00 + sha256("<dpu>/<role>")[0:3].
pub fn dpu_vm_nic_mac(dpu: &str, role: &str) -> String {
    qemu_mac(Sha256::digest(format!("{dpu}/{role}")).into_iter().take(3))
}

/// The host gateway MAC OVN-K's `--simulate-dpu` mode (SimulatedDPUOps)
/// derives for the host PF of `host_node`: 52:54:00 + sha256("<node>\0host")[0:2] + :00.
/// The host PF is given it, so its DHCP lease from the DPU matches.
pub fn simulated_host_pf_mac(host_node: &str) -> String {
    let digest = Sha256::digest(format!("{host_node}\0host"));
    qemu_mac(digest.into_iter().take(2).chain([0]))
}

/// Accepts `aa:bb:cc:dd:ee:ff` in any case, returned lowercase.
pub fn parse_mac(text: &str) -> Result<String> {
    let octets: Vec<&str> = text.split(':').collect();
    let well_formed = octets.len() == 6
        && octets
            .iter()
            .all(|octet| octet.len() == 2 && octet.chars().all(|digit| digit.is_ascii_hexdigit()));
    if !well_formed {
        bail!("{text:?} is not a MAC address like 52:54:00:12:34:56");
    }
    Ok(text.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected values from the shell/Python implementations these replace.
    #[test]
    fn dpu_vm_nic_mac_matches_create_dpu_vm_sh() {
        assert_eq!(
            dpu_vm_nic_mac("52-54-00-d6-62-89-mt26sim62371", "mgmt"),
            "52:54:00:eb:c7:53"
        );
    }

    #[test]
    fn simulated_host_pf_mac_matches_bf3sim_host_sh() {
        assert_eq!(
            simulated_host_pf_mac("52-54-00-d6-62-89"),
            "52:54:00:bd:b5:00"
        );
    }

    #[test]
    fn parse_mac_lowercases_and_rejects_garbage() {
        assert_eq!(
            parse_mac("52:54:00:AA:fb:35").ok().as_deref(),
            Some("52:54:00:aa:fb:35")
        );
        assert!(parse_mac("52:54:00:aa:fb").is_err());
        assert!(parse_mac("52:54:00:aa:fb:zz").is_err());
    }
}
