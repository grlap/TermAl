// Owns domain RPC bounds, sanitized failures and retained peer lookup.
// Does not own SSH/link lifetimes or replay mutations. New extensions of
// federation_rpc.rs and federation_runtime.rs for central mailbox routing.

// A bridge recognizes this complete semicolon-delimited clause. Ordinary
// explicit admission refusals must not acquire an unknown-outcome label.
const FEDERATION_MAILBOX_UNKNOWN_OUTCOME: &str = "central mailbox outcome may be unknown";

fn federation_mailbox_unknown_outcome(status: StatusCode, detail: &str) -> ApiError {
    let status = if status.is_server_error() { status } else { StatusCode::SERVICE_UNAVAILABLE };
    ApiError::from_status(status, format!(
        "{detail}; {FEDERATION_MAILBOX_UNKNOWN_OUTCOME}; retry the original request with the same idempotencyKey; no local mailbox fallback or sender queue was created"
    ))
}

impl FederationRpcRoute {
    fn mailbox_lease(&self) -> Option<&FederationLinkLease> {
        match self {
            Self::Mailbox(lease, _) => Some(lease),
            _ => None,
        }
    }

    fn response_limit(&self) -> usize {
        match self {
            Self::Mailbox(_, _) | Self::MailboxRelay(_, _) => FEDERATION_MAILBOX_RESPONSE_BYTES,
            Self::ExecutorWake(_, _) => 32 * 1024,
            Self::Binding(_, _) => FEDERATION_FRAME_LIMIT,
            _ => 64 * 1024,
        }
    }

    // Only used after starting HTTP. A central append can have committed even
    // when its reply cannot be read, decoded, allocated or accepted locally.
    fn response_failure(&self, error: ApiError) -> ApiError {
        if let Self::Mailbox(_, request) = self {
            federation_mailbox_read_advice(request,
                federation_mailbox_unknown_outcome(error.status, &error.message))
        } else if matches!(self, Self::MailboxRelay(_, _)) {
            federation_mailbox_unknown_outcome(error.status, "native mailbox relay transport/response unavailable")
        } else if matches!(self, Self::Binding(_, _)) {
            federation_binding_unknown_outcome()
        } else {
            error
        }
    }
}

fn federation_mailbox_rpc_error(status: StatusCode) -> ApiError {
    let message = match status {
        StatusCode::BAD_REQUEST => {
            "central mailbox request refused; check protocol, participant and limits"
        }
        StatusCode::FORBIDDEN => "central mailbox pair authentication failed",
        StatusCode::NOT_FOUND => "central mailbox or qualified participant not found",
        StatusCode::CONFLICT => {
            "central mailbox authority, idempotency or receipt conflict; reconcile the original request"
        }
        StatusCode::PAYLOAD_TOO_LARGE => "central mailbox request exceeds negotiated size",
        StatusCode::LOCKED => "central mailbox persistence is settling; retry",
        StatusCode::TOO_MANY_REQUESTS => "central mailbox RPC limit reached; retry",
        _ => return federation_mailbox_unknown_outcome(
            StatusCode::SERVICE_UNAVAILABLE, "central mailbox response unavailable"),
    };
    ApiError::from_status(status, message)
}
