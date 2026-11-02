// Invocation-local producer guarantees used by factual reporting.
// These never settle an earlier attempt or authorize a resend.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngramInvocationApplication {
    Unknown,
    CertifiedNotApplied,
}

impl EngramInvocationApplication {
    /// Only an actual producer reply carries this no-write contract.
    fn from_remote_code(code: Option<&str>) -> Self {
        match code {
            Some("control_request_expired" | "control_store_busy") => Self::CertifiedNotApplied,
            _ => Self::Unknown,
        }
    }
}

impl EngramTransportError {
    fn invocation_application(&self) -> EngramInvocationApplication {
        if self.kind == EngramTransportErrorKind::Remote
            && self.causal_failure.is_none()
        {
            EngramInvocationApplication::from_remote_code(self.code.as_deref())
        } else {
            EngramInvocationApplication::Unknown
        }
    }
}
