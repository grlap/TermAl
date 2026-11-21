// Owns bounded diagnostic reasons for authenticated native relay refusals, the
// home's relay failure records, and a relay request's phase trace with its
// thread-local scope (frozen at the request's deadline).
// Does not grant authority, settle application outcomes, retry or alter receipts.
// New module following the separate domain-mailbox error boundary's pattern.

const MAILBOX_RELAY_ERROR_BYTES: usize = 512;
const MAILBOX_RELAY_RAW_ERROR_BYTES: usize = 4096;

// Process-local home observations, separate from the peer's refusal envelope.
// No response body is inspected or retained by this diagnostic.
const MAILBOX_RELAY_HOME_RPC_FAILURES: usize = 64;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
enum MailboxRelayHomeRpcStage {
    ReplayCounter,
    RequestAdmission,
    RpcCapacity,
    HandlerResponse,
    HandlerDeadline,
    PostHandlerAuthority,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MailboxRelayHomeRpcFailure {
    at: String,
    operation: &'static str,
    status: u16,
    stage: MailboxRelayHomeRpcStage,
    request_id: String,
    // The fixed phase still running when the record was made, and the
    // elapsed time of each phase finished before it. Names and ms only.
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<&'static str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    phases: Vec<MailboxRelayPhaseElapsed>,
}

// A relay request's fixed phases, so a slow or expired request names where it
// waited: admission, dispatch, origin, operation, connection, storeId, state,
// authority, postAuthority. No payload, key or peer text is ever recorded.
const MAILBOX_RELAY_PHASES: usize = 16;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
struct MailboxRelayPhaseElapsed {
    phase: &'static str,
    elapsed_ms: u64,
}

#[derive(Default)]
struct MailboxRelayPhaseTrace {
    state: Mutex<MailboxRelayPhaseState>,
}

type MailboxRelayPhaseSnapshot = (
    Option<MailboxRelayPhaseElapsed>,
    Vec<MailboxRelayPhaseElapsed>,
);

#[derive(Default)]
struct MailboxRelayPhaseState {
    current: Option<(&'static str, std::time::Instant)>,
    finished: Vec<MailboxRelayPhaseElapsed>,
    // Taken once when the request's deadline expires. An abandoned job that
    // keeps running cannot move the expired request's phase on.
    frozen: Option<MailboxRelayPhaseSnapshot>,
}

impl MailboxRelayPhaseState {
    fn snapshot(&self, now: std::time::Instant) -> MailboxRelayPhaseSnapshot {
        let current = self.current.map(|(phase, began)| MailboxRelayPhaseElapsed {
            phase,
            elapsed_ms: now.saturating_duration_since(began).as_millis() as u64,
        });
        (current, self.finished.clone())
    }
}

impl MailboxRelayPhaseTrace {
    fn enter(&self, phase: &'static str) {
        let now = std::time::Instant::now();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.frozen.is_some() {
            return;
        }
        if let Some((previous, began)) = state.current.take()
            && state.finished.len() < MAILBOX_RELAY_PHASES
        {
            state.finished.push(MailboxRelayPhaseElapsed {
                phase: previous,
                elapsed_ms: now.saturating_duration_since(began).as_millis() as u64,
            });
        }
        state.current = Some((phase, now));
    }

    /// The phase still running, with its elapsed time so far, and the
    /// finished phases before it; after an expiry, the phases at expiry.
    fn snapshot(&self) -> MailboxRelayPhaseSnapshot {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &state.frozen {
            Some(frozen) => frozen.clone(),
            None => state.snapshot(std::time::Instant::now()),
        }
    }

    /// Records the phases at the request's deadline expiry, once. Every later
    /// snapshot, for the expiry line and the failure record alike, returns it.
    fn freeze(&self) -> MailboxRelayPhaseSnapshot {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.frozen.is_none() {
            let at_expiry = state.snapshot(std::time::Instant::now());
            state.frozen = Some(at_expiry);
        }
        state.frozen.clone().unwrap_or_default()
    }
}

thread_local! {
    static MAILBOX_RELAY_PHASE_TRACE: std::cell::RefCell<Option<Arc<MailboxRelayPhaseTrace>>> =
        const { std::cell::RefCell::new(None) };
}

/// Installs a relay request's phase trace on the blocking job's thread for
/// its lifetime, so shared helpers can mark phases without new parameters.
struct MailboxRelayPhaseScope;

impl MailboxRelayPhaseScope {
    fn install(trace: Option<Arc<MailboxRelayPhaseTrace>>) -> Self {
        MAILBOX_RELAY_PHASE_TRACE.with(|slot| *slot.borrow_mut() = trace);
        Self
    }
}

impl Drop for MailboxRelayPhaseScope {
    fn drop(&mut self) {
        MAILBOX_RELAY_PHASE_TRACE.with(|slot| *slot.borrow_mut() = None);
    }
}

/// Marks the current relay request's phase; does nothing outside one.
fn mailbox_relay_phase(phase: &'static str) {
    MAILBOX_RELAY_PHASE_TRACE.with(|slot| {
        if let Some(trace) = slot.borrow().as_ref() {
            trace.enter(phase);
        }
    });
}

/// The bounded operation name and request id of a relay request body, read
/// without decoding or copying its operation content.
fn mailbox_relay_diagnostic_metadata(request: &[u8]) -> (&'static str, String) {
    let metadata = serde_json::from_slice::<MailboxRelayDiagnosticRequest<'_>>(request).ok();
    let request_id = metadata
        .as_ref()
        .map(|value| value.request_id)
        .filter(|id| mailbox_relay_uuid(id))
        .unwrap_or("unavailable");
    let operation = match metadata.as_ref().map(|value| value.operation.operation) {
        Some("observeTestRun") => "observeTestRun",
        Some("describe") => "describe",
        Some("send") => "send",
        Some("list") => "list",
        Some("read") => "read",
        Some("readMessage") => "readMessage",
        Some("acknowledge") => "acknowledge",
        Some("wake") => "wake",
        Some("inspectWake") => "inspectWake",
        _ => "unavailable",
    };
    (operation, request_id.to_owned())
}

#[derive(Default)]
struct MailboxRelayHomeRpcFailureHistory {
    records: VecDeque<MailboxRelayHomeRpcFailure>,
    evicted: u64,
}

#[derive(Default)]
struct MailboxRelayHomeRpcFailures {
    history: Mutex<MailboxRelayHomeRpcFailureHistory>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MailboxRelayDiagnosticRequest<'a> {
    #[serde(borrow)]
    request_id: &'a str,
    #[serde(borrow)]
    operation: MailboxRelayDiagnosticOperation<'a>,
}

#[derive(Deserialize)]
struct MailboxRelayDiagnosticOperation<'a> {
    #[serde(borrow)]
    operation: &'a str,
}

impl MailboxRelayHomeRpcFailures {
    #[cfg(test)]
    fn note(&self, request: &[u8], status: StatusCode, stage: MailboxRelayHomeRpcStage) {
        self.note_with_phases(request, status, stage, None);
    }

    fn note_with_phases(
        &self,
        request: &[u8],
        status: StatusCode,
        stage: MailboxRelayHomeRpcStage,
        trace: Option<&MailboxRelayPhaseTrace>,
    ) {
        if !status.is_server_error() { return; }
        // Borrow only the correlation fields; serde skips all other payloads.
        let (operation, request_id) = mailbox_relay_diagnostic_metadata(request);
        let (current, phases) = trace
            .map(MailboxRelayPhaseTrace::snapshot)
            .unwrap_or_default();
        let record = MailboxRelayHomeRpcFailure {
            at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            operation,
            status: status.as_u16(),
            stage,
            request_id,
            phase: current.map(|current| current.phase),
            phases: phases.into_iter().chain(current).collect(),
        };
        let mut history = self.history.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if history.records.len() == MAILBOX_RELAY_HOME_RPC_FAILURES {
            history.records.pop_front();
            history.evicted = history.evicted.saturating_add(1);
        }
        history.records.push_back(record);
    }

    fn snapshot(&self) -> (Vec<MailboxRelayHomeRpcFailure>, u64) {
        let history = self.history.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        (history.records.iter().cloned().collect(), history.evicted)
    }
}

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
