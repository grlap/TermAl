// Owns bounded diagnostic reasons for authenticated native relay refusals.
// Does not grant authority, settle application outcomes, retry or alter receipts.
// New module following the separate domain-mailbox error boundary's pattern.

const MAILBOX_RELAY_ERROR_BYTES: usize = 512;
const MAILBOX_RELAY_RAW_ERROR_BYTES: usize = 4096;

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum MailboxRelayRefusalReason {
    PersistenceSettling,
    RpcLimitReached,
    ReceiptInvalid,
    ReceiptGap,
    AuthorityChanged,
    UpgradeRequired,
    ReasonUnavailable,
}

impl MailboxRelayRefusalReason {
    fn accepts_status(self, status: StatusCode) -> bool {
        match self {
            Self::PersistenceSettling => status == StatusCode::LOCKED,
            Self::RpcLimitReached => status == StatusCode::TOO_MANY_REQUESTS,
            Self::ReceiptInvalid => status == StatusCode::BAD_REQUEST,
            Self::ReceiptGap | Self::AuthorityChanged | Self::UpgradeRequired => status == StatusCode::CONFLICT,
            Self::ReasonUnavailable => status.is_client_error() && status != StatusCode::NOT_FOUND,
        }
    }

    fn advice(self) -> &'static str {
        match self {
            Self::PersistenceSettling => "native_mailbox_home_persistence_settling; home federation persistence is settling; retry the unchanged original operation",
            Self::RpcLimitReached => "native_mailbox_home_rpc_limit_reached; home RPC capacity is busy; retry the unchanged original operation later",
            Self::ReceiptInvalid => "native_mailbox_home_receipt_invalid; use the unchanged receipt issued to this participant and mailbox; read again if none is available",
            Self::ReceiptGap => "native_mailbox_home_receipt_gap; process and acknowledge the earlier visible page from the current cursor first",
            Self::AuthorityChanged => "native_mailbox_home_authority_changed; reconcile the original operation with its original identity and home",
            Self::UpgradeRequired => "native_mailbox_home_upgrade_required; both hosts must support the native relay protocol; no local fallback",
            Self::ReasonUnavailable => "native_mailbox_home_reason_unavailable; authenticated native mailbox relay refused; the home reason is unavailable; retain the original request and report the refusal if it persists",
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MailboxRelayErrorEnvelope {
    native_mailbox_error: MailboxRelayErrorBody,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MailboxRelayErrorBody {
    version: u16,
    request_id: String,
    request_digest: String,
    reason: MailboxRelayRefusalReason,
}

// Parse request metadata only; operation content is skipped, not decoded or copied.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MailboxRelayErrorRequestMetadata {
    version: u16,
    request_id: String,
    #[serde(rename = "operation")]
    _operation: serde::de::IgnoredAny,
}

fn mailbox_relay_refusal_reason(status: StatusCode, bytes: &[u8]) -> MailboxRelayRefusalReason {
    use MailboxRelayRefusalReason::*;
    if bytes.len() > MAILBOX_RELAY_RAW_ERROR_BYTES {
        return ReasonUnavailable;
    }
    let Ok(body) = serde_json::from_slice::<ErrorResponse>(bytes) else {
        return ReasonUnavailable;
    };
    let reason = match body.error.as_str() {
        "federation persistence is settling; retry" => PersistenceSettling,
        "federation peer RPC limit reached" => RpcLimitReached,
        "mailbox receipt was not issued to this participant and mailbox; read again" => ReceiptInvalid,
        "mailbox receipt skips an unacknowledged page; read from the current cursor and process it first" => ReceiptGap,
        FEDERATION_STALE_AUTHORITY | NATIVE_MAILBOX_RELAY_LINK_CHANGED => AuthorityChanged,
        "native mailbox relay upgrade required" => UpgradeRequired,
        _ => ReasonUnavailable,
    };
    if reason.accepts_status(status) { reason } else { ReasonUnavailable }
}

// Called only after request authentication, before the response MAC. The same
// boundary sees both admission-time and handler-time refusals. Remote prose
// never crosses this diagnostic boundary, even for an unknown refusal.
fn mailbox_relay_refusal_body(request: &[u8], status: StatusCode, body: &[u8]) -> Vec<u8> {
    let metadata = serde_json::from_slice::<MailboxRelayErrorRequestMetadata>(request).ok()
        .filter(|value| value.version == MAILBOX_RELAY_VERSION && mailbox_relay_uuid(&value.request_id));
    let Some(metadata) = metadata else {
        return br#"{"error":"native mailbox relay reason unavailable"}"#.to_vec();
    };
    serde_json::to_vec(&MailboxRelayErrorEnvelope {
        native_mailbox_error: MailboxRelayErrorBody {
            version: 1,
            request_id: metadata.request_id,
            request_digest: sha256_hex(request),
            reason: mailbox_relay_refusal_reason(status, body),
        },
    }).expect("bounded relay error serializes")
}

// The caller has verified the response MAC and current authority. Diagnostics
// grant no retry or outcome authority. Existing 404 capability/resource and
// 5xx unknown-outcome classification stays in federation_rpc_authenticated.
fn mailbox_relay_verified_refusal(
    request: &MailboxRelayRequest,
    request_body: &[u8],
    status: StatusCode,
    bytes: &[u8],
) -> ApiError {
    let decode = || -> Option<MailboxRelayRefusalReason> {
        if bytes.len() > MAILBOX_RELAY_ERROR_BYTES { return None; }
        let envelope: MailboxRelayErrorEnvelope = serde_json::from_slice(bytes).ok()?;
        let body = envelope.native_mailbox_error;
        (body.version == 1 && body.request_id == request.request_id
            && body.request_digest == sha256_hex(request_body)
            && body.reason.accepts_status(status)).then_some(body.reason)
    };
    let reason = decode().unwrap_or(MailboxRelayRefusalReason::ReasonUnavailable);
    ApiError::from_status(status, reason.advice())
}
