// Owns the domain registry and content-fenced membership/configuration changes.
// Does not own transport, credentials, onboarding probes or scheduling policy.
// New foundation; durable writes reuse the existing state persist worker.

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationExecutionProfile {
    protocol_version: u32,
    schema_version: String,
    binary_sha256: String,
    toolchain_identity: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationCapacity {
    max_sessions: u32,
    build_slots: u32,
    gate_slots: u32,
}

/// O1 supplies these results only after its real verification checklist.
/// Registry membership itself never produces a successful probe result.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationVerificationChecks {
    host_key: bool,
    capability: bool,
    protocol: bool,
    bidirectional_rpc: bool,
    isolated_wake: bool,
    platform_supervisor: bool,
}

#[cfg_attr(not(test), allow(dead_code))]
impl FederationVerificationChecks {
    fn complete(&self) -> bool {
        self.host_key
            && self.capability
            && self.protocol
            && self.bidirectional_rpc
            && self.isolated_wake
            && self.platform_supervisor
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationVerification {
    basis: String,
    link_incarnation: String,
    checks: FederationVerificationChecks,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationLiveness {
    link_incarnation: String,
    last_successful_rpc: Option<String>,
    last_successful_wake: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationPeerRecord {
    identity: FederationHostIdentity,
    enrollment_generation: u64,
    record_generation: u64,
    /// Membership bookkeeping does not recycle a verified channel. This fence
    /// advances on every transport/configuration invalidation, including ABA.
    transport_generation: u64,
    display_name: String,
    platform: String,
    arch: String,
    endpoint: FederationSshEndpoint,
    pair_capability_ref: String,
    profile: FederationExecutionProfile,
    capacity: FederationCapacity,
    membership: FederationMembershipState,
    verification: Option<FederationVerification>,
    #[serde(default)]
    last_observed: Option<FederationLiveness>,
}

impl FederationPeerRecord {
    fn validate(&self) -> Result<(), ApiError> {
        self.identity.validate()?;
        self.endpoint.validate()?;
        validate_federation_reference(&self.pair_capability_ref)?;
        if self.enrollment_generation == 0
            || self.record_generation == 0
            || self.transport_generation == 0
            || self.display_name.is_empty()
            || self.display_name.chars().count() > 160
            || self.display_name.chars().any(char::is_control)
            || !matches!(self.platform.as_str(), "windows" | "linux" | "macos")
            || self.arch.is_empty()
            || self.arch.len() > 64
            || self
                .arch
                .chars()
                .any(|ch| ch.is_control() || ch.is_whitespace())
            || self.profile.protocol_version == 0
            || self.profile.schema_version.is_empty()
            || self.profile.schema_version.len() > 64
            || self.profile.binary_sha256.len() != 64
            || !self
                .profile
                .binary_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || self.profile.toolchain_identity.is_empty()
            || self.profile.toolchain_identity.len() > 1024
        {
            return Err(ApiError::bad_request(
                "invalid federation enrollment record",
            ));
        }
        Ok(())
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn verification_basis(&self) -> String {
        sha256_hex(
            serde_json::to_vec(&json!({
                "identity": self.identity, "enrollmentGeneration": self.enrollment_generation,
                "platform": self.platform, "arch": self.arch, "endpoint": self.endpoint,
                "pairCapabilityRef": self.pair_capability_ref, "profile": self.profile,
                "capacity": self.capacity,
            }))
            .expect("registry fields serialize")
            .as_slice(),
        )
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn verified_on(&self, link: Option<&String>) -> bool {
        self.verification.as_ref().is_some_and(|proof| {
            proof.checks.complete()
                && proof.basis == self.verification_basis()
                && link == Some(&proof.link_incarnation)
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationState {
    /// Mailbox-only locators, never native actors or executor routes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    mailbox_references: BTreeMap<String, FederationMailboxReference>,
    /// C's participant directory is authority metadata, never mailbox history.
    /// Satellites keep this empty; their per-session binding has routing only.
    #[serde(default)]
    mailbox_directory: BTreeMap<String, FederationMailboxParticipant>,
    #[serde(default)]
    local: Option<FederationHostIdentity>,
    #[serde(default)]
    version: u64,
    #[serde(default)]
    admission_quarantine: Option<FederationQuarantineReason>,
    #[serde(default)]
    store_binding: String,
    #[serde(default)]
    peers: BTreeMap<String, FederationPeerRecord>,
    #[serde(default)]
    placements: BTreeMap<String, FederationPlacement>,
    #[serde(default)]
    onboarding: FederationOnboardingData,
    /// A reboot never reloads a live connection from persisted liveness.
    #[serde(skip)]
    links: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum FederationQuarantineReason {
    StoreBindingChanged,
    DeclaredRestore,
}

fn federation_next_generation(generation: u64) -> Result<u64, ApiError> {
    generation
        .checked_add(1)
        .ok_or_else(|| ApiError::conflict("federation generation exhausted"))
}

impl FederationState {
    fn validate(&self) -> Result<()> {
        for (id, reference) in &self.mailbox_references {
            reference.validate().map_err(|_| anyhow!("invalid persisted mailbox reference"))?;
            if id != &reference.id {
                return Err(anyhow!("invalid persisted mailbox reference key"));
            }
        }
        for (key, participant) in &self.mailbox_directory {
            participant.validate().map_err(|_| anyhow!("invalid persisted mailbox participant"))?;
            if key != &participant.session_id {
                return Err(anyhow!("invalid persisted mailbox directory key"));
            }
        }
        let mut central_ports = BTreeSet::new();
        let mut creation_ids = BTreeSet::new();
        for (id, record) in &self.peers {
            record
                .validate()
                .map_err(|_| anyhow!("invalid persisted federation peer"))?;
            if id != &record.identity.host_id
                || self
                    .local
                    .as_ref()
                    .is_some_and(|local| &local.host_id == id)
                || (record.membership != FederationMembershipState::Revoked
                    && !central_ports.insert(record.endpoint.central_federation_port))
            {
                return Err(anyhow!(
                    "conflicting persisted federation identity or channel"
                ));
            }
        }
        for (key, placement) in &self.placements {
            placement
                .identity
                .validate()
                .map_err(|_| anyhow!("invalid persisted placement identity"))?;
            placement
                .peer
                .validate()
                .map_err(|_| anyhow!("invalid persisted placement authority"))?;
            validate_federation_reference(key)
                .map_err(|_| anyhow!("invalid persisted placement key"))?;
            if key != &placement.admission_key
                || placement.placement_generation == 0
                || !creation_ids.insert(&placement.identity.creation_id)
                || placement.execution_basis.len() != 64
                || !placement
                    .execution_basis
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
                || !self.peers.contains_key(&placement.identity.host_id)
                || placement.peer.peer.host_id != placement.identity.host_id
            {
                return Err(anyhow!("invalid persisted federation placement"));
            }
        }
        Ok(())
    }
}
