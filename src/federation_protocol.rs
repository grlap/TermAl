// Owns federation wire envelopes and their asserted-identity comparisons.
// Does not select peers or grant authority. Split from federation_link.rs.

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationHello {
    protocol_version: u16,
    server_instance_id: String,
    challenge: String,
    host_id: String,
    host_generation: u64,
    enrollment_generation: u64,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationOpenLink {
    protocol_version: u16,
    receiver_instance_id: String,
    challenge: String,
    incarnation: String,
    central: FederationHostIdentity,
    enrollment_generation: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationRequestAuthority {
    caller: FederationHostIdentity,
    central: FederationHostIdentity,
    enrollment_generation: u64,
}

impl FederationRequestAuthority {
    fn outgoing(snapshot: &FederationPeerSnapshot) -> Self {
        Self {
            caller: snapshot.local.clone(),
            central: snapshot.central.clone(),
            enrollment_generation: snapshot.enrollment_generation,
        }
    }

    fn ensure_expected(&self, snapshot: &FederationPeerSnapshot) -> Result<(), ApiError> {
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
struct FederationHealth {
    protocol_version: u16,
    host_id: String,
    host_generation: u64,
    enrollment_generation: u64,
    link_incarnation: String,
}
