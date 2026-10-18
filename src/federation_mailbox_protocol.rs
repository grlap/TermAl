// Owns the versioned central-mailbox operation contract and existing limits.
// Does not authenticate channels, store mail, or queue sender intents. New
// domain protocol beside mailboxes.rs; standalone mailbox APIs stay unchanged.

const FEDERATION_MAILBOX_SCHEMA_VERSION: &str = "1";
const FEDERATION_MAILBOX_WIRE_VERSION: u16 = 1;
const FEDERATION_MAILBOX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
// Preserve the existing 200-message ceiling, including worst-case JSON escape
// expansion of 256 KiB bodies and both 4 KiB metadata fields. This bound is
// mailbox-specific; handshake/health responses retain L1's 64 KiB ceiling.
const FEDERATION_MAILBOX_RESPONSE_BYTES: usize = 320 * 1024 * 1024;
const FEDERATION_MAILBOX_PAGE_LIMIT: u64 = 200;
const FEDERATION_REMOTE_WAKE_CAUSE: &str = "remote wake delivery arrives with M4";

fn new_federation_native_creation_id() -> String {
    Uuid::new_v4().to_string()
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationMailboxBinding {
    central: FederationHostIdentity,
    identity: FederationSessionIdentity,
    placement_generation: u64,
}

impl FederationMailboxBinding {
    fn validate(&self) -> Result<(), ApiError> {
        self.central.validate()?;
        self.identity.validate()?;
        if self.placement_generation == 0 {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        Ok(())
    }

    fn participant_key(&self) -> String {
        // A receipt's existing participant column now scopes it to the full
        // principal AND authority epoch, without trusting a display alias.
        let encoded = serde_json::to_vec(self).expect("mailbox identity serializes");
        format!("domain-session-{}", sha256_hex(&encoded))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationMailboxParticipant {
    binding: FederationMailboxBinding,
    session_id: String,
    host_name: String,
    name: String,
}

impl FederationMailboxParticipant {
    fn validate(&self) -> Result<(), ApiError> {
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
struct FederationMailboxActor {
    binding: FederationMailboxBinding,
    name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationMailboxRequest {
    protocol_version: u16,
    schema_version: String,
    central: FederationHostIdentity,
    actor: Option<FederationMailboxActor>,
    operation: FederationMailboxOperation,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "operation",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum FederationMailboxOperation {
    Protocol,
    Register,
    Directory,
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
    List,
    Read {
        mailbox_id: String,
        after_sequence: Option<u64>,
        limit: u64,
        issue_receipt: bool,
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
struct FederationMailboxResponse {
    protocol_version: u16,
    schema_version: String,
    central: FederationHostIdentity,
    value: Value,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct FederationMailboxLimits {
    body_bytes: usize,
    metadata_bytes: usize,
    display_name_chars: usize,
    page_messages: u64,
    request_bytes: usize,
    response_bytes: usize,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct FederationMailboxProtocol {
    protocol_version: u16,
    schema_version: String,
    central: FederationHostIdentity,
    limits: FederationMailboxLimits,
    receipt_only_acknowledgement: bool,
    sender_intent_registry: bool,
    remote_wake_delivery: bool,
    remote_wake_cause: String,
}

fn federation_mailbox_protocol(central: FederationHostIdentity) -> FederationMailboxProtocol {
    FederationMailboxProtocol {
        protocol_version: FEDERATION_MAILBOX_WIRE_VERSION,
        schema_version: FEDERATION_MAILBOX_SCHEMA_VERSION.to_owned(),
        central,
        limits: FederationMailboxLimits {
            body_bytes: MAX_MAILBOX_BODY_BYTES,
            metadata_bytes: MAX_MAILBOX_METADATA_BYTES,
            display_name_chars: 160,
            page_messages: FEDERATION_MAILBOX_PAGE_LIMIT,
            request_bytes: FEDERATION_MAILBOX_REQUEST_BYTES,
            response_bytes: FEDERATION_MAILBOX_RESPONSE_BYTES,
        },
        receipt_only_acknowledgement: true,
        sender_intent_registry: false,
        remote_wake_delivery: false,
        remote_wake_cause: FEDERATION_REMOTE_WAKE_CAUSE.to_owned(),
    }
}

fn federation_mailbox_unavailable() -> ApiError {
    ApiError::from_status(
        StatusCode::SERVICE_UNAVAILABLE,
        "central TermAl host unavailable; retry the same request; no local mailbox result or sender queue was created",
    )
}

fn federation_mailbox_display_name(value: &str) -> String {
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

fn federation_mailbox_root_locked<'a>(
    inner: &'a StateInner,
    session_id: &str,
) -> Result<&'a SessionRecord, ApiError> {
    let index = inner
        .find_session_index(session_id)
        .ok_or_else(|| ApiError::not_found("session not found"))?;
    let record = &inner.sessions[index];
    if record.hidden
        || !record.is_local_session()
        || record.session.parent_delegation_id.is_some()
        || inner
            .find_delegation_index_by_child_session_id(session_id)
            .is_some()
    {
        return Err(ApiError::bad_request(
            "mailbox participant must be a local root session",
        ));
    }
    Ok(record)
}
