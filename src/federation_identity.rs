// Owns domain identity, enrollment configuration and immutable link snapshots.
// Does not own credentials, transport, placement mutation or SQLite writes.
// New foundation shared by the registry and federation link; not an extraction.

use super::*;

pub(crate) const FEDERATION_STALE_AUTHORITY: &str =
    "federation authority changed; refresh enrollment and retry";

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum FederationRole {
    #[default]
    Standalone,
    Central,
    Satellite,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum FederationMembershipState {
    Enrolled,
    Verified,
    Active,
    Draining,
    Revoked,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationHostIdentity {
    pub(crate) host_id: String,
    pub(crate) host_generation: u64,
}

impl FederationHostIdentity {
    pub(crate) fn validate(&self) -> Result<(), ApiError> {
        let canonical = Uuid::parse_str(&self.host_id)
            .ok()
            .filter(|id| !id.is_nil())
            .map(|id| id.to_string());
        if canonical.as_deref() != Some(self.host_id.as_str()) || self.host_generation == 0 {
            return Err(ApiError::bad_request("invalid federation host identity"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationSettings {
    #[serde(default)]
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) role: FederationRole,
    #[serde(default)]
    pub(crate) central: Option<FederationHostIdentity>,
}

impl FederationSettings {
    pub(crate) fn validate(&self, local: &FederationHostIdentity) -> Result<(), ApiError> {
        local.validate()?;
        match (&self.role, &self.central) {
            (FederationRole::Standalone, None) if !self.enabled => Ok(()),
            (FederationRole::Central, Some(central)) if central == local => central.validate(),
            (FederationRole::Satellite, Some(central)) if central.host_id != local.host_id => {
                central.validate()
            }
            _ => Err(ApiError::bad_request(
                "invalid federation role or central binding",
            )),
        }
    }
}

/// References are lookup tokens, never paths or raw credential material.
pub(crate) fn validate_federation_reference(value: &str) -> Result<(), ApiError> {
    if value.is_empty()
        || value.len() > 160
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(ApiError::bad_request(
            "invalid federation credential reference",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationSshEndpoint {
    pub(crate) ssh_host: String,
    pub(crate) ssh_user: String,
    pub(crate) ssh_port: u16,
    pub(crate) known_hosts_ref: String,
    pub(crate) host_key_ref: String,
    pub(crate) satellite_federation_port: u16,
    pub(crate) central_federation_port: u16,
    pub(crate) satellite_reverse_forward_port: u16,
}

// New locator updates use durable DNS names. Legacy numeric records remain
// readable until an authorized address-only transition replaces them.
pub(crate) fn validate_federation_ssh_hostname(host: &str) -> Result<(), ApiError> {
    let dns = host.strip_suffix('.').unwrap_or(host);
    // OpenSSH's system resolver also accepts inet_aton-style numeric forms,
    // including mixed decimal/hex components that Rust's IpAddr rejects.
    let numeric_ipv4 = dns.split('.').count() <= 4
        && dns.split('.').all(|part| {
            if let Some(hex) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
                !hex.is_empty() && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
            } else {
                !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())
            }
        });
    if dns.is_empty()
        || dns.len() > 253
        || host.parse::<std::net::IpAddr>().is_ok()
        || numeric_ipv4
        || !dns.bytes().any(|byte| byte.is_ascii_alphabetic())
        || dns.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        return Err(ApiError::bad_request(
            "federation SSH locator requires a DNS hostname",
        ));
    }
    Ok(())
}

impl FederationSshEndpoint {
    pub(crate) fn validate(&self) -> Result<(), ApiError> {
        // These become separate argv values. Reject option-like tokens and
        // control/whitespace characters before a transport sees the record.
        for value in [&self.ssh_host, &self.ssh_user] {
            if value.is_empty()
                || value.len() > 255
                || value.starts_with('-')
                || value
                    .chars()
                    .any(|ch| ch.is_control() || ch.is_whitespace())
            {
                return Err(ApiError::bad_request("invalid federation SSH endpoint"));
            }
        }
        validate_federation_reference(&self.known_hosts_ref)?;
        validate_federation_reference(&self.host_key_ref)?;
        if [
            self.ssh_port,
            self.satellite_federation_port,
            self.central_federation_port,
            self.satellite_reverse_forward_port,
        ]
        .contains(&0)
            || self.satellite_federation_port == self.satellite_reverse_forward_port
        {
            return Err(ApiError::bad_request(
                "invalid or conflicting federation ports",
            ));
        }
        Ok(())
    }
}

/// An exact authority snapshot is not a placement grant or an authenticated caller.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationPeerSnapshot {
    pub(crate) local: FederationHostIdentity,
    pub(crate) local_role: FederationRole,
    pub(crate) peer: FederationHostIdentity,
    pub(crate) central: FederationHostIdentity,
    pub(crate) enrollment_generation: u64,
    pub(crate) record_generation: u64,
    pub(crate) transport_generation: u64,
    pub(crate) pair_capability_ref: String,
    pub(crate) endpoint: FederationSshEndpoint,
    pub(crate) membership: FederationMembershipState,
}

#[cfg_attr(not(test), allow(dead_code))]
impl FederationPeerSnapshot {
    /// Channel reuse comparison only. Authority still requires a current check
    /// under the state lock, including the channel's fixed link incarnation.
    pub(crate) fn same_transport_as(&self, other: &Self) -> bool {
        self.local == other.local
            && self.local_role == other.local_role
            && self.peer == other.peer
            && self.central == other.central
            && self.enrollment_generation == other.enrollment_generation
            && self.transport_generation == other.transport_generation
            && self.pair_capability_ref == other.pair_capability_ref
            && self.endpoint == other.endpoint
    }

    pub(crate) fn validate(&self) -> Result<(), ApiError> {
        self.local.validate()?;
        self.peer.validate()?;
        self.central.validate()?;
        self.endpoint.validate()?;
        validate_federation_reference(&self.pair_capability_ref)?;
        let valid_role = match self.local_role {
            FederationRole::Central => self.central == self.local,
            FederationRole::Satellite => self.central == self.peer,
            FederationRole::Standalone => false,
        };
        if !valid_role
            || self.local.host_id == self.peer.host_id
            || self.enrollment_generation == 0
            || self.record_generation == 0
            || self.transport_generation == 0
            || self.membership == FederationMembershipState::Revoked
        {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        Ok(())
    }
}

/// The creation UUID, rather than a display name or reusable local id, scopes
/// session access and future mailbox membership for a coordinated session.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationSessionIdentity {
    pub(crate) host_id: String,
    pub(crate) session_id: String,
    pub(crate) creation_id: String,
}

// The existing native-id syntax check is shared with explicit mailbox parsing;
// syntax does not establish a session's creation identity or current authority.
pub(crate) fn federation_native_session_id_valid(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= 160
        && !session_id
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace() || ch == '/')
}

impl FederationSessionIdentity {
    pub(crate) fn validate(&self) -> Result<(), ApiError> {
        FederationHostIdentity {
            host_id: self.host_id.clone(),
            host_generation: 1,
        }
        .validate()?;
        let creation = Uuid::parse_str(&self.creation_id)
            .ok()
            .filter(|id| !id.is_nil());
        if creation.map(|id| id.to_string()).as_deref() != Some(self.creation_id.as_str())
            || !federation_native_session_id_valid(&self.session_id)
        {
            return Err(ApiError::bad_request("invalid federation session identity"));
        }
        Ok(())
    }
}
