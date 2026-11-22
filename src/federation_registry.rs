// Owns the domain registry and content-fenced membership/configuration changes.
// Does not own transport, credentials, onboarding probes or scheduling policy.
// New foundation; durable writes reuse the existing state persist worker.

use super::*;

// The federation data/behavior contract is independent of local SQLite layout
// and of the mailbox subprotocol's separately versioned message schema.
pub(crate) const FEDERATION_PROFILE_SCHEMA_VERSION: &str = "2";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationExecutionProfile {
    pub(crate) protocol_version: u32,
    pub(crate) schema_version: String,
    pub(crate) binary_sha256: String,
    pub(crate) toolchain_identity: String,
}

impl FederationExecutionProfile {
    // A different image/toolchain is evidence, not a different wire contract.
    pub(crate) fn compatible_with(&self, other: &Self) -> bool {
        self.protocol_version == other.protocol_version
            && self.schema_version == other.schema_version
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationCapacity {
    pub(crate) max_sessions: u32,
    pub(crate) build_slots: u32,
    pub(crate) gate_slots: u32,
}

/// O1 supplies these results only after its real verification checklist.
/// Registry membership itself never produces a successful probe result.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationVerificationChecks {
    pub(crate) host_key: bool,
    pub(crate) capability: bool,
    pub(crate) protocol: bool,
    pub(crate) bidirectional_rpc: bool,
    pub(crate) isolated_wake: bool,
    pub(crate) platform_supervisor: bool,
}

#[cfg_attr(not(test), allow(dead_code))]
impl FederationVerificationChecks {
    pub(crate) fn complete(&self) -> bool {
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
pub(crate) struct FederationVerification {
    pub(crate) basis: String,
    pub(crate) link_incarnation: String,
    pub(crate) checks: FederationVerificationChecks,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationLiveness {
    pub(crate) link_incarnation: String,
    pub(crate) last_successful_rpc: Option<String>,
    pub(crate) last_successful_wake: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationPeerRecord {
    pub(crate) identity: FederationHostIdentity,
    pub(crate) enrollment_generation: u64,
    pub(crate) record_generation: u64,
    /// Membership bookkeeping does not recycle a verified channel. This fence
    /// advances on every transport/configuration invalidation, including ABA.
    pub(crate) transport_generation: u64,
    pub(crate) display_name: String,
    pub(crate) platform: String,
    pub(crate) arch: String,
    pub(crate) endpoint: FederationSshEndpoint,
    pub(crate) pair_capability_ref: String,
    pub(crate) profile: FederationExecutionProfile,
    pub(crate) capacity: FederationCapacity,
    pub(crate) membership: FederationMembershipState,
    pub(crate) verification: Option<FederationVerification>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) requalification: Option<FederationRequalification>,
    #[serde(default)]
    pub(crate) last_observed: Option<FederationLiveness>,
}

impl FederationPeerRecord {
    pub(crate) fn validate(&self) -> Result<(), ApiError> {
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
    pub(crate) fn verification_basis(&self) -> String {
        sha256_hex(
            serde_json::to_vec(&json!({
                "identity": self.identity, "enrollmentGeneration": self.enrollment_generation,
                "platform": self.platform, "arch": self.arch, "endpoint": self.endpoint,
                "pairCapabilityRef": self.pair_capability_ref,
                "profile": {"protocolVersion": self.profile.protocol_version,
                    "schemaVersion": self.profile.schema_version},
                "capacity": self.capacity,
            }))
            .expect("registry fields serialize")
            .as_slice(),
        )
    }

    // Read-only migration witness for a pre-upgrade verification. Never used
    // to admit traffic: fresh authenticated qualification is still required.
    pub(crate) fn legacy_verification_basis(&self) -> String {
        sha256_hex(
            &serde_json::to_vec(&json!({
                "identity": self.identity, "enrollmentGeneration": self.enrollment_generation,
                "platform": self.platform, "arch": self.arch, "endpoint": self.endpoint,
                "pairCapabilityRef": self.pair_capability_ref, "profile": self.profile,
                "capacity": self.capacity,
            }))
            .expect("registry fields serialize"),
        )
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn verified_on(&self, link: Option<&String>) -> bool {
        self.verification.as_ref().is_some_and(|proof| {
            proof.checks.complete()
                && proof.basis == self.verification_basis()
                && link == Some(&proof.link_incarnation)
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationState {
    // Permission values belong to this existing durable federation owner.
    // An absent policy admits no direct prompts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) prompt_permissions: Option<FederationPromptPermissions>,
    /// Central immutable binding-install intents share the reservation's state
    /// transaction. Satellites retain only the installed per-session binding.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) binding_install_intents: BTreeMap<String, FederationBindingInstallIntent>,
    /// Mailbox-only locators, never native actors or executor routes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) mailbox_references: BTreeMap<String, FederationMailboxReference>,
    /// C's participant directory is authority metadata, never mailbox history.
    /// Satellites keep this empty; their per-session binding has routing only.
    #[serde(default)]
    pub(crate) mailbox_directory: BTreeMap<String, FederationMailboxParticipant>,
    #[serde(default)]
    pub(crate) local: Option<FederationHostIdentity>,
    #[serde(default)]
    pub(crate) version: u64,
    #[serde(default)]
    pub(crate) admission_quarantine: Option<FederationQuarantineReason>,
    #[serde(default)]
    pub(crate) store_binding: String,
    #[serde(default)]
    pub(crate) peers: BTreeMap<String, FederationPeerRecord>,
    #[serde(default)]
    pub(crate) placements: BTreeMap<String, FederationPlacement>,
    #[serde(default)]
    pub(crate) onboarding: FederationOnboardingData,
    /// A reboot never reloads a live connection from persisted liveness.
    #[serde(skip)]
    pub(crate) links: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum FederationQuarantineReason {
    StoreBindingChanged,
    DeclaredRestore,
}

pub(crate) fn federation_next_generation(generation: u64) -> Result<u64, ApiError> {
    generation
        .checked_add(1)
        .ok_or_else(|| ApiError::conflict("federation generation exhausted"))
}

impl FederationState {
    pub(crate) fn validate(&self) -> Result<()> {
        if let Some(permissions) = &self.prompt_permissions {
            permissions
                .validate()
                .map_err(|_| anyhow!("invalid persisted prompt permission policy"))?;
        }
        for (key, intent) in &self.binding_install_intents {
            intent
                .validate()
                .map_err(|_| anyhow!("invalid binding install intent"))?;
            let placement = self
                .placements
                .get(key)
                .ok_or_else(|| anyhow!("binding install lacks reservation"))?;
            if intent.admission_key != *key
                || intent.binding.identity != placement.identity
                || intent.binding.placement_generation != placement.placement_generation
                || intent.binding.central != placement.peer.central
                || intent.execution_basis != placement.execution_basis
                || intent.peer_enrollment_generation != placement.peer.enrollment_generation
                || intent.configuration_basis != placement.peer.configuration_basis()
            {
                return Err(anyhow!("binding install reservation changed"));
            }
        }
        for (id, reference) in &self.mailbox_references {
            reference
                .validate()
                .map_err(|_| anyhow!("invalid persisted mailbox reference"))?;
            if id != &reference.id {
                return Err(anyhow!("invalid persisted mailbox reference key"));
            }
        }
        for (key, participant) in &self.mailbox_directory {
            participant
                .validate()
                .map_err(|_| anyhow!("invalid persisted mailbox participant"))?;
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
                || placement.mailbox_only
                    && (placement.build_slots != 0 || placement.gate_slots != 0)
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
