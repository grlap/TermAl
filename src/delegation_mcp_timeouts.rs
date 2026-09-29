// How long the delegation MCP bridge, and Codex above it, wait on the calls
// whose server work outlasts an ordinary request: one list of those calls
// (`DelegationLongCall`) with what each may spend, the allowance the bridge
// derives from it, the single-attempt POST that waits it out, the error for an
// outcome left unknown, the test injector of the turn-delivery budget, the
// span of a default delegation wait and the poll sleep that keeps it, and the
// `tool_timeout_sec` Codex is given for the whole delegation server. It also
// holds two request bodies every bridge call shares, because each must know
// the wait it was given: the response decoder (`decode_response_within`,
// which `decode_response` calls with the ordinary timeout) and the
// safe-replay POST within a budget (`post_json_with_safe_replay_within`).
// Does not own the server budgets these cover (`review_freeze_process.rs`,
// `engram_source_roots.rs`, `acceptance_evaluation_api.rs`,
// `engram_host_adapter.rs`, the Codex child lifecycle constants), the tools
// that make these calls (`delegation_mcp.rs`, `delegation_mcp_source_root.rs`,
// `delegation_mcp_review_freeze.rs`), the bridge's ordinary request wrappers
// and its replay classification and backoff, or the Codex config the tool
// timeout is written into (`delegation_mcp.rs`). New module: the follow-up
// allowance, the response decoder and the safe-replay POST body moved here
// from `delegation_mcp.rs` (which is already past its size limit), each
// taking the wait it names; the rest is new, derived from the doubled
// load-sensitive limits.

/// The serial recovery waits a follow-up may make before its admission.
fn termal_delegation_followup_recovery_allowance() -> Duration {
    CODEX_CHILD_RELEASE_WAIT_TIMEOUT
        + CODEX_THREAD_RECONCILIATION_REPLY_TIMEOUT * 2
        + CODEX_CHILD_RESULT_FENCE_TIMEOUT
        + CODEX_CHILD_UNARCHIVE_REPLY_TIMEOUT
}

/// What delivering a turn inline may spend: a spawn, a follow-up and a mailbox
/// send that wakes an idle target deliver a turn before they answer, so a
/// gated admission and, for a session holding a claim, the turn's begin-time
/// source capture run within them.
fn termal_delegation_turn_delivery_budget() -> Duration {
    Duration::from_millis(ENGRAM_DISPATCH_BUDGET_MS) + REVIEW_FREEZE_TIMEOUT
}

/// Defines `DelegationLongCall` and its `ALL` from one list of variants, so
/// a call added to the enum is in `ALL`, and so in Codex's tool timeout, by
/// construction.
macro_rules! delegation_long_calls {
    ($($(#[$attribute:meta])* $variant:ident,)*) => {
        /// A delegation tool call whose server work outlasts an ordinary request.
        /// The one list the bridge's allowances and Codex's tool timeout both
        /// come from.
        #[derive(Clone, Copy, Debug)]
        enum DelegationLongCall {
            $($(#[$attribute])* $variant,)*
        }

        impl DelegationLongCall {
            const ALL: &'static [Self] = &[$(Self::$variant,)*];
        }
    };
}

delegation_long_calls! {
    /// Binds the parent to Engram, recovering its binding first when that
    /// needs a rebind, and then the new child, off the admission path
    /// (`engram_unqueued_bind_worst_case`), and delivers the child's first
    /// turn.
    Spawn,
    /// Reads the tracker within its budget, then spawns the evaluator.
    EvaluationRequest,
    /// May run the tracker twice, and waits for the writer before and after.
    EvaluationSubmit,
    /// Bounded as a whole by the naming budget, so a failure the bridge reports
    /// never leaves a name the server kept.
    SourceRootNaming,
    /// The server watches its checker child for the freeze budget and a grace.
    ReviewFreeze,
    /// Recovers the child's thread, then delivers its re-armed turn.
    Followup,
    /// Wakes an idle target and delivers its turn; its safe replays share the
    /// allowance.
    MailboxSend,
}

impl DelegationLongCall {
    /// What the call may spend beyond an ordinary request, where delivering a
    /// turn inline may spend `turn_delivery`.
    fn budget(self, turn_delivery: Duration) -> Duration {
        // A spawn binds the parent, which may recover a binding, then the new
        // child, which has none to recover.
        let binds = engram_unqueued_bind_worst_case(true) + engram_unqueued_bind_worst_case(false);
        match self {
            Self::Spawn => binds + turn_delivery,
            Self::EvaluationRequest => {
                acceptance_evaluation_request_tracker_budget() + binds + turn_delivery
            }
            Self::EvaluationSubmit => acceptance_evaluation_submit_budget(),
            Self::SourceRootNaming => ENGRAM_SOURCE_ROOT_NAMING_BUDGET,
            Self::ReviewFreeze => REVIEW_FREEZE_TIMEOUT + REVIEW_FREEZE_OBSERVER_GRACE,
            Self::Followup => termal_delegation_followup_recovery_allowance() + turn_delivery,
            Self::MailboxSend => turn_delivery,
        }
    }
}

/// How much longer than the bridge's own wait Codex waits on a TermAl
/// delegation tool call, so the bridge's answer, a timeout included, still
/// reaches it.
const TERMAL_DELEGATION_MCP_CODEX_TOOL_MARGIN: Duration = Duration::from_secs(5);

/// The longest a `termal_wait_delegations` call on one delegation with the
/// default wait may take: the wait, which never sleeps past its deadline,
/// then its last status read, which may start at the deadline, then the
/// result read when that read finds the delegation finished. Each further
/// delegation a wait names adds a status read and a result read.
fn termal_delegation_default_wait_span() -> Duration {
    Duration::from_millis(TERMAL_DELEGATION_MCP_DEFAULT_WAIT_TIMEOUT_MS)
        + TERMAL_DELEGATION_MCP_HTTP_TIMEOUT * 2
}

/// How long a `termal_wait_delegations` round sleeps before the next: its
/// poll interval, but never past the wait's deadline, so the wait's span is
/// its timeout plus the reads after it (`termal_delegation_default_wait_span`).
fn termal_delegation_wait_poll_sleep(
    poll_interval: Duration,
    deadline: std::time::Instant,
    now: std::time::Instant,
) -> Duration {
    poll_interval.min(deadline.saturating_duration_since(now))
}

/// How long Codex may wait on one call of a TermAl delegation tool, its
/// `tool_timeout_sec` (Codex's own default is 60 s): the longest allowance the
/// bridge gives any long call (`DelegationLongCall::ALL`), or the span of a
/// default wait on one delegation if longer, and a margin. So for a call the
/// bridge starts at once, Codex never ends it while the bridge and the server
/// are still deciding, a failure it reports never leaves a result the server
/// kept, and a default wait on one delegation gives its own answer,
/// `timedOut` or the result. Codex's clock runs from the call, though, and the
/// bridge serves one call at a time: a call queued behind another, or one the
/// bridge first reads the caller's classification or resolves a prompt for,
/// can be cut by Codex and still run, as can a longer wait or one naming more
/// delegations.
fn termal_delegation_mcp_codex_tool_timeout() -> Duration {
    let turn_delivery = termal_delegation_turn_delivery_budget();
    let longest_allowance = DelegationLongCall::ALL
        .iter()
        .map(|call| call.budget(turn_delivery) + TERMAL_DELEGATION_MCP_HTTP_TIMEOUT)
        .max()
        .expect("at least one long call");
    termal_delegation_codex_tool_timeout_for(
        longest_allowance,
        termal_delegation_default_wait_span(),
    )
}

/// Codex's tool timeout for the longest long-call allowance and the span of a
/// default wait: whichever is longer, and the margin.
fn termal_delegation_codex_tool_timeout_for(
    longest_allowance: Duration,
    wait_span: Duration,
) -> Duration {
    longest_allowance.max(wait_span) + TERMAL_DELEGATION_MCP_CODEX_TOOL_MARGIN
}

/// `termal_delegation_mcp_codex_tool_timeout` in whole seconds, rounded up, as
/// Codex's config and the wait tool's description state it.
fn termal_delegation_mcp_codex_tool_timeout_secs() -> u64 {
    termal_delegation_mcp_codex_tool_timeout()
        .as_secs_f64()
        .ceil() as u64
}

/// A call whose outcome is unknown after a transport or decoding failure:
/// `message` says what to do and embeds the failure, and the wait that ran
/// out, for a transport failure, is kept as data rather than as a source, so a
/// rendering of the whole chain (`{:#}`, as the coordination CLI prints)
/// shows the failure once.
#[derive(Debug)]
struct TermalDelegationUnknownOutcome {
    message: String,
    /// The wait that ran out, for a transport failure. Production shows only
    /// `message`, which embeds the failure; the tests read this to prove
    /// which wait a call was given.
    #[cfg_attr(not(test), allow(dead_code))]
    transport_timeout: Option<Duration>,
}

impl std::fmt::Display for TermalDelegationUnknownOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TermalDelegationUnknownOutcome {}

/// Whether `err` leaves a call's outcome unknown: the request may have
/// reached the server (a transport failure), or the server answered success
/// with a body the bridge could not read. A refusal the server answered
/// (`TermalDelegationApiError`) is a known outcome.
fn termal_delegation_outcome_is_unknown(err: &anyhow::Error) -> bool {
    err.downcast_ref::<TermalDelegationTransportError>()
        .is_some()
        || err
            .downcast_ref::<TermalDelegationResponseError>()
            .is_some()
}

/// `message`, which embeds `err`, as a `TermalDelegationUnknownOutcome`.
fn termal_delegation_unknown_outcome(err: &anyhow::Error, message: String) -> anyhow::Error {
    TermalDelegationUnknownOutcome {
        message,
        transport_timeout: err
            .downcast_ref::<TermalDelegationTransportError>()
            .map(|transport| transport.timeout),
    }
    .into()
}

impl TermalDelegationMcpBridge {
    /// How long the bridge waits on `call`, its allowance: the call's budget,
    /// what it may spend beyond an ordinary request, plus the normal request
    /// timeout.
    fn allowance(&self, call: DelegationLongCall) -> Duration {
        call.budget(self.turn_delivery_budget) + self.request_timeout
    }

    /// A single-attempt POST for `call`, waited on for its allowance; a
    /// transport error names that wait.
    fn post_long_call(&self, call: DelegationLongCall, path: &str, body: &Value) -> Result<Value> {
        let allowance = self.allowance(call);
        self.decode_response_within(
            "POST",
            path,
            allowance,
            self.client
                .post(self.url(path))
                .timeout(allowance)
                .json(body)
                .send(),
        )
    }

    /// Test-only injection point for the turn-delivery budget, so a test that
    /// withholds a response waits its own request timeout, not a production
    /// wake budget.
    #[cfg(test)]
    fn with_turn_delivery_budget(mut self, budget: Duration) -> Self {
        self.turn_delivery_budget = budget;
        self
    }

    fn post_json_with_safe_replay_within(
        &self,
        path: &str,
        body: &Value,
        budget: Duration,
    ) -> Result<Value> {
        self.request_json_with_safe_replay_retry("POST", path, budget, |remaining| {
            self.client
                .post(self.url(path))
                .timeout(remaining)
                .json(body)
                .send()
        })
    }

    /// `decode_response` for a call sent with its own allowance, `timeout`, so
    /// a transport error names the wait that ran out rather than the ordinary
    /// request timeout.
    fn decode_response_within(
        &self,
        method: &'static str,
        path: &str,
        timeout: Duration,
        response: std::result::Result<reqwest::blocking::Response, reqwest::Error>,
    ) -> Result<Value> {
        let response = response.map_err(|source| TermalDelegationTransportError {
            method,
            path: path.to_owned(),
            phase: "sending request",
            timeout,
            source,
        })?;
        let status = response.status();
        let text = response
            .text()
            .map_err(|source| TermalDelegationTransportError {
                method,
                path: path.to_owned(),
                phase: "reading response body",
                timeout,
                source,
            })?;
        if status.is_success() {
            return serde_json::from_str(&text).map_err(|source| {
                TermalDelegationResponseError {
                    method,
                    path: path.to_owned(),
                    message: format!("failed to parse successful response JSON: {source}"),
                }
                .into()
            });
        }
        let message = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or(text);
        Err(TermalDelegationApiError { status, message }.into())
    }
}
