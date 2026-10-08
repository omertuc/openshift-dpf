//! The host<->DPU wire (M4) is one VLAN trunk carrying all of the host's PF
//! and VF traffic: untagged is the host PF, VLAN 100+N host p0 VF N, VLAN
//! 200+N host p1 VF N. The host tags each VF (`bf3-host`), the DPU splits the
//! trunk back into one representor netdev per VF (`dpu-ovs`).
//!
//! p0 VF 0 is a BlueField's host<->DPU channel (br-comm-ch); the DPU VM's own
//! management NIC plays it, so it stays off the trunk. p1 VF 0 is an ordinary VF.

use anyhow::{Context, Result};

/// One of the host's VFs that crosses the trunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrunkedVf {
    pub pf: u8,
    pub vf: u16,
}

impl TrunkedVf {
    pub fn vlan_id(self) -> Result<u16> {
        u16::from(self.pf)
            .checked_add(1)
            .and_then(|pf_number| pf_number.checked_mul(100))
            .and_then(|pf_base| pf_base.checked_add(self.vf))
            .with_context(|| format!("VLAN id of p{} VF {} overflows", self.pf, self.vf))
    }

    /// The host netdev of the VF's PF.
    pub fn host_pf(self) -> String {
        format!("p{}", self.pf)
    }

    /// The DPU-side representor, the netdev OVN-K plugs into OVS.
    pub fn representor(self) -> String {
        format!("rep{}-{}", self.pf, self.vf)
    }

    /// The other end of the representor's veth, on the trunk bridge.
    pub fn representor_wire_end(self) -> String {
        format!("rep{}-{}w", self.pf, self.vf)
    }
}

/// The VFs on the trunk when each PF has `vfs_per_pf` VFs.
pub fn trunked_vfs(vfs_per_pf: u16) -> impl Iterator<Item = TrunkedVf> {
    [0_u8, 1].into_iter().flat_map(move |pf| {
        // p0 VF 0 is the host<->DPU channel.
        let first_vf = u16::from(pf == 0);
        (first_vf..vfs_per_pf).map(move |vf| TrunkedVf { pf, vf })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p0_vf0_stays_off_the_trunk() {
        let vlan_ids: Result<Vec<u16>> = trunked_vfs(3).map(TrunkedVf::vlan_id).collect();
        assert_eq!(vlan_ids.ok(), Some(vec![101, 102, 200, 201, 202]));
    }
}
