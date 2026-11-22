// Owns federation wire envelopes and their asserted-identity comparisons.
// Does not select peers or grant authority. Split from federation_link.rs.

use super::*;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationRequestAuthority {
    pub(crate) caller: FederationHostIdentity,
    pub(crate) central: FederationHostIdentity,
    pub(crate) enrollment_generation: u64,
}

impl FederationRequestAuthority {
    pub(crate) fn outgoing(snapshot: &FederationPeerSnapshot) -> Self {
        Self {
            caller: snapshot.local.clone(),
            central: snapshot.central.clone(),
            enrollment_generation: snapshot.enrollment_generation,
        }
    }

    pub(crate) fn ensure_expected(
        &self,
        snapshot: &FederationPeerSnapshot,
    ) -> Result<(), ApiError> {
        if self.caller != snapshot.peer
            || self.central != snapshot.central
            || self.enrollment_generation != snapshot.enrollment_generation
        {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        Ok(())
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationHealth {
    pub(crate) protocol_version: u16,
    pub(crate) host_id: String,
    pub(crate) host_generation: u64,
    pub(crate) enrollment_generation: u64,
    pub(crate) link_incarnation: String,
}
