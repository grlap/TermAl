// Owns the versioned central-mailbox operation contract and existing limits.
// Does not authenticate channels or store mail and sender retry intents. New
// domain protocol beside mailboxes.rs; standalone mailbox APIs stay unchanged.

use super::*;

pub(crate) const FEDERATION_MAILBOX_SCHEMA_VERSION: &str = "2";
pub(crate) const FEDERATION_MAILBOX_WIRE_VERSION: u16 = 2;
pub(crate) const FEDERATION_MAILBOX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
// Preserve the existing 200-message ceiling, including worst-case JSON escape
// expansion of 256 KiB bodies and both 4 KiB metadata fields. This bound is
// mailbox-specific; handshake/health responses retain L1's 64 KiB ceiling.
pub(crate) const FEDERATION_MAILBOX_RESPONSE_BYTES: usize = 320 * 1024 * 1024;
pub(crate) const FEDERATION_MAILBOX_PAGE_LIMIT: u64 = 200;
pub(crate) const FEDERATION_REMOTE_WAKE_CAUSE: &str =
    "remote wake ownership pending; see message details for current state";

pub(crate) fn new_federation_native_creation_id() -> String {
    Uuid::new_v4().to_string()
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationMailboxBinding {
    pub(crate) central: FederationHostIdentity,
    pub(crate) identity: FederationSessionIdentity,
    pub(crate) placement_generation: u64,
}

impl FederationMailboxBinding {
    pub(crate) fn validate(&self) -> Result<(), ApiError> {
        self.central.validate()?;
        self.identity.validate()?;
        if self.placement_generation == 0 {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        Ok(())
    }

    pub(crate) fn participant_key(&self) -> String {
        // A receipt's existing participant column now scopes it to the full
        // principal AND authority epoch, without trusting a display alias.
        let encoded = serde_json::to_vec(self).expect("mailbox identity serializes");
        format!("domain-session-{}", sha256_hex(&encoded))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationMailboxParticipant {
    pub(crate) binding: FederationMailboxBinding,
    pub(crate) session_id: String,
    pub(crate) host_name: String,
    pub(crate) name: String,
}

impl FederationMailboxParticipant {
    pub(crate) fn validate(&self) -> Result<(), ApiError> {
        self.binding.validate()?;
        if self.session_id != self.binding.participant_key()
            || [&self.name, &self.host_name].iter().any(|value| {
                value.is_empty()
                    || value.chars().count() > 160
                    || value.chars().any(char::is_control)
            })
        {
            return Err(ApiError::bad_request(
                "invalid mailbox participant directory entry",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationMailboxActor {
    pub(crate) binding: FederationMailboxBinding,
    pub(crate) name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationMailboxRequest {
    pub(crate) protocol_version: u16,
    pub(crate) schema_version: String,
    pub(crate) central: FederationHostIdentity,
    pub(crate) actor: Option<FederationMailboxActor>,
    pub(crate) operation: FederationMailboxOperation,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "operation",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(crate) enum FederationMailboxOperation {
    Protocol,
    Register,
    Directory {
        // Local transport preference only: the signed URI opts in. The JSON
        // body remains exactly the old {"operation":"directory"} shape.
        #[serde(skip)]
        alias_basis: bool,
    },
    GetOrCreate {
        target: String,
    },
    Send {
        target: String,
        message: String,
        key: String,
        topic: Option<String>,
        state_stamp: Option<String>,
    },
    SendExact {
        delivery: FederationDelivery,
    },
    CancelExact {
        delivery: FederationDelivery,
    },
    List,
    Read {
        mailbox_id: String,
        after_sequence: Option<u64>,
        limit: u64,
        issue_receipt: bool,
    },
    ReadKeyed {
        mailbox_id: String,
        after_sequence: Option<u64>,
        limit: u64,
        issue_receipt: bool,
        read_key: String,
        resolve: bool,
    },
    ReadMessage {
        message_id: String,
    },
    Acknowledge {
        mailbox_id: String,
        receipt: String,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationMailboxResponse {
    pub(crate) protocol_version: u16,
    pub(crate) schema_version: String,
    pub(crate) central: FederationHostIdentity,
    pub(crate) value: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) alias_basis: Option<FederationMailboxAliasBasis>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FederationMailboxLimits {
    pub(crate) page_bytes: usize,
    frozen_keys: usize,
    read_key_ttl_ms: u64,
    pub(crate) body_bytes: usize,
    pub(crate) metadata_bytes: usize,
    display_name_chars: usize,
    page_messages: u64,
    pub(crate) request_bytes: usize,
    pub(crate) response_bytes: usize,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FederationMailboxProtocol {
    read_incarnation: String,
    read_epoch_ms: u64,
    pub(crate) read_key: Option<String>,
    pub(crate) protocol_version: u16,
    pub(crate) schema_version: String,
    pub(crate) central: FederationHostIdentity,
    pub(crate) limits: FederationMailboxLimits,
    receipt_only_acknowledgement: bool,
    pub(crate) sender_intent_registry: bool,
    remote_wake_delivery: bool,
    remote_wake_cause: String,
}

pub(crate) fn federation_mailbox_protocol(
    central: FederationHostIdentity,
    cache: &mut FederationMailboxReadCache,
) -> FederationMailboxProtocol {
    FederationMailboxProtocol {
        read_incarnation: cache.incarnation.clone(),
        read_epoch_ms: federation_mailbox_read_now_ms(),
        read_key: cache.offer_key(),
        protocol_version: FEDERATION_MAILBOX_WIRE_VERSION,
        schema_version: FEDERATION_MAILBOX_SCHEMA_VERSION.to_owned(),
        central,
        limits: FederationMailboxLimits {
            page_bytes: FEDERATION_MAILBOX_PAGE_BYTES,
            frozen_keys: FEDERATION_MAILBOX_FROZEN_KEYS,
            read_key_ttl_ms: FEDERATION_MAILBOX_READ_TTL_MS,
            body_bytes: MAX_MAILBOX_BODY_BYTES,
            metadata_bytes: MAX_MAILBOX_METADATA_BYTES,
            display_name_chars: 160,
            page_messages: FEDERATION_MAILBOX_PAGE_LIMIT,
            request_bytes: FEDERATION_MAILBOX_REQUEST_BYTES,
            response_bytes: FEDERATION_MAILBOX_RESPONSE_BYTES,
        },
        receipt_only_acknowledgement: true,
        sender_intent_registry: true,
        remote_wake_delivery: false,
        remote_wake_cause: FEDERATION_REMOTE_WAKE_CAUSE.to_owned(),
    }
}

pub(crate) fn federation_mailbox_unavailable() -> ApiError {
    ApiError::from_status(
        StatusCode::SERVICE_UNAVAILABLE,
        "central TermAl host unavailable; no canonical result is known; inspect sender intent status and retry the exact delivery",
    )
}

pub(crate) fn federation_mailbox_display_name(value: &str) -> String {
    let sanitized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let sanitized: String = sanitized
        .chars()
        .filter(|ch| !ch.is_control())
        .take(160)
        .collect();
    if sanitized.is_empty() {
        "Session".to_owned()
    } else {
        sanitized
    }
}

pub(crate) fn federation_mailbox_root_locked<'a>(
    inner: &'a StateInner,
    session_id: &str,
) -> Result<&'a SessionRecord, ApiError> {
    inner.require_delegation_inventory()?;
    let index = inner
        .find_session_index(session_id)
        .ok_or_else(|| ApiError::not_found("session not found"))?;
    let record = &inner.sessions[index];
    let indexed_child = {
        let children = inner
            .delegation_catalog_children
            .lock()
            .expect("catalog child index poisoned");
        children
            .ready(inner.delegations.len())
            .then(|| children.children.contains_key(session_id))
    };
    if record.hidden
        || !record.is_local_session()
        || record.session.parent_delegation_id.is_some()
        || indexed_child.unwrap_or_else(|| {
            inner
                .find_delegation_index_by_child_session_id(session_id)
                .is_some()
        })
    {
        return Err(ApiError::bad_request(
            "mailbox participant must be a local root session",
        ));
    }
    Ok(record)
}
