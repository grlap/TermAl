// Owns bounded authenticated mailbox error classification and correlation.
// No remote prose acquires retry authority; transport verification comes first.
const FEDERATION_MAILBOX_ERROR_BYTES: usize = 4096;

fn federation_mailbox_upgrade_required() -> ApiError {
    ApiError::bad_request("upgradeRequired; upgrade both mailbox endpoints before retrying")
        .with_kind(ApiErrorKind::FederationMailboxUpgradeRequired)
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationMailboxErrorEnvelope {
    mailbox_error: FederationMailboxErrorBody,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationMailboxErrorBody {
    class: String,
    effect: String,
    operation: String,
    request_digest: String,
}

impl FederationMailboxOperation {
    fn error_operation(&self) -> &'static str {
        match self {
            Self::Protocol => "protocol",
            Self::Register => "register",
            Self::Directory { .. } => "directory",
            Self::GetOrCreate { .. } => "getOrCreate",
            Self::Send { .. } => "send",
            Self::SendExact { .. } => "sendExact",
            Self::CancelExact { .. } => "cancelExact",
            Self::List => "list",
            Self::Read { .. } | Self::ReadKeyed { .. } => "read",
            Self::ReadMessage { .. } => "readMessage",
            Self::Acknowledge { .. } => "acknowledge",
        }
    }
}

fn federation_mailbox_error_response(
    request: &FederationMailboxRequest, error: ApiError,
) -> Response {
    let (status, class, effect) = match error.kind {
        Some(ApiErrorKind::FederationMailboxUpgradeRequired) =>
            (StatusCode::BAD_REQUEST, "upgradeRequired", "notEntered"),
        Some(ApiErrorKind::FederationMailboxReadExpired) =>
            (StatusCode::GONE, "read_key_expired", "expired"),
        Some(ApiErrorKind::FederationMailboxReadConflict) =>
            (StatusCode::CONFLICT, "read_key_conflict", "unknown"),
        Some(ApiErrorKind::FederationMailboxWriterAdmission) =>
            (StatusCode::SERVICE_UNAVAILABLE, "writer_admission_timeout", "notCommitted"),
        Some(ApiErrorKind::FederationMailboxDirectoryFence) =>
            (StatusCode::SERVICE_UNAVAILABLE, "directory_fence_unavailable", "notEntered"),
        _ if error.status == StatusCode::CONFLICT =>
            (StatusCode::CONFLICT, "authority_changed", "unknown"),
        _ if error.status == StatusCode::BAD_REQUEST =>
            (StatusCode::BAD_REQUEST, "request_refused", "unknown"),
        _ if error.status.is_server_error() =>
            (StatusCode::SERVICE_UNAVAILABLE, "operation_unavailable", "unknown"),
        _ => return error.into_response(),
    };
    let envelope = FederationMailboxErrorEnvelope {
        mailbox_error: FederationMailboxErrorBody {
            class: class.to_owned(), effect: effect.to_owned(),
            operation: request.operation.error_operation().to_owned(),
            request_digest: sha256_hex(&serde_json::to_vec(request).expect("mailbox request serializes")),
        },
    };
    (status, Json(envelope)).into_response()
}

fn federation_mailbox_unknown_response(
    request: Option<&FederationMailboxRequest>, class: &'static str,
) -> Response {
    let Some(request) = request else {
        return federation_mailbox_unknown_outcome(StatusCode::SERVICE_UNAVAILABLE,
            "central mailbox request unavailable").into_response();
    };
    (StatusCode::SERVICE_UNAVAILABLE, Json(FederationMailboxErrorEnvelope {
        mailbox_error: FederationMailboxErrorBody {
            class: class.to_owned(), effect: "unknown".to_owned(),
            operation: request.operation.error_operation().to_owned(),
            request_digest: sha256_hex(&serde_json::to_vec(request).expect("mailbox request serializes")),
        },
    })).into_response()
}

fn federation_mailbox_verified_error(
    request: &FederationMailboxRequest, status: StatusCode, bytes: &[u8],
) -> ApiError {
    let decode = || -> Option<ApiError> {
        if bytes.len() > FEDERATION_MAILBOX_ERROR_BYTES { return None; }
        let envelope: FederationMailboxErrorEnvelope = serde_json::from_slice(bytes).ok()?;
        let body = envelope.mailbox_error;
        if body.operation != request.operation.error_operation()
            || body.request_digest != sha256_hex(&serde_json::to_vec(request).ok()?) {
            return None;
        }
        match (status, body.class.as_str(), body.effect.as_str()) {
            (StatusCode::BAD_REQUEST, "upgradeRequired", "notEntered") =>
                Some(federation_mailbox_upgrade_required()),
            (StatusCode::BAD_REQUEST, "request_refused", "unknown") =>
                Some(federation_mailbox_rpc_error(status)),
            (StatusCode::GONE, "read_key_expired", "expired") => Some(federation_mailbox_read_expired()),
            (StatusCode::CONFLICT, "read_key_conflict", "unknown") => Some(ApiError::conflict(
                "readKey input conflict; preserve the identical original read")
                .with_kind(ApiErrorKind::FederationMailboxReadConflict)),
            (StatusCode::SERVICE_UNAVAILABLE, "writer_admission_timeout", "notCommitted") =>
                Some(ApiError::from_status(status, "central mailbox writer_admission_timeout; requested mailbox write not committed; retry the identical request")
                    .with_kind(ApiErrorKind::FederationMailboxWriterAdmission)),
            (StatusCode::SERVICE_UNAVAILABLE, "directory_fence_unavailable", "notEntered") =>
                Some(ApiError::from_status(status, "central mailbox directory_fence_unavailable; requested mailbox operation not entered; directory durability unresolved; retry the identical request")
                    .with_kind(ApiErrorKind::FederationMailboxDirectoryFence)),
            (StatusCode::CONFLICT | StatusCode::SERVICE_UNAVAILABLE, "authority_changed", "unknown") => Some(
                ApiError::from_status(status, format!("central mailbox authority_changed; {FEDERATION_MAILBOX_UNKNOWN_OUTCOME}; reconcile the original request"))),
            (StatusCode::SERVICE_UNAVAILABLE, "handler_deadline" | "transport_loss" | "operation_unavailable", "unknown") =>
                Some(federation_mailbox_unknown_outcome(status, &format!("central mailbox {}", body.class))),
            _ => None,
        }
    };
    let error = decode().unwrap_or_else(|| {
        // A malformed authenticated envelope is not a no-write certificate.
        if bytes.windows(b"mailboxError".len()).any(|part| part == b"mailboxError") {
            federation_mailbox_unknown_outcome(StatusCode::SERVICE_UNAVAILABLE, "invalid central mailbox error envelope")
        } else if matches!(status, StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY) {
            // A peer without the required v2 refusal envelope is incompatible.
            // Stop at this shared boundary; never resend using an older wire.
            federation_mailbox_upgrade_required()
        } else {
            federation_mailbox_rpc_error(status)
        }
    });
    federation_mailbox_read_advice(request, error)
}

fn federation_mailbox_read_advice(request: &FederationMailboxRequest, mut error: ApiError) -> ApiError {
    if error.message.contains(FEDERATION_MAILBOX_UNKNOWN_OUTCOME) {
        let advice = match &request.operation {
            FederationMailboxOperation::ReadKeyed { read_key, .. } =>
                Some(if read_key.len() <= 160 && read_key.bytes().all(|byte|
                    byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'-')) {
                    format!("resolve the identical read with readKey {read_key}; pending is not acknowledgement authority")
                } else { "resolve the identical original readKey; pending is not acknowledgement authority".to_owned() }),
            FederationMailboxOperation::Read { .. } =>
                Some("repeat the logical mailbox read; do not acknowledge until a receipt is issued".to_owned()),
            _ => None,
        };
        if let Some(advice) = advice {
            error.message = error.message.replace("retry the original request with the same idempotencyKey", &advice);
            if !error.message.contains("readKey") && matches!(request.operation, FederationMailboxOperation::ReadKeyed { .. }) {
                error.message.push_str("; "); error.message.push_str(&advice);
            }
        }
    }
    error
}
