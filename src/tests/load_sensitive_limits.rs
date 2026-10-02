// The doubled load-sensitive limits and the relations the limits and waits
// derived from them must keep. The limits live in their owners
// (engram_host_adapter.rs, engram_readiness.rs, review_freeze_process.rs,
// engram_source_roots.rs, acceptance_evaluation_api.rs); the bridge waits in
// delegation_mcp_timeouts.rs. New module, so one place pins them all.
use super::*;

#[test]
fn load_sensitive_limits_are_doubled_and_keep_their_relations() {
    // The doubled limits and those derived from them, pinned.
    assert_eq!(REVIEW_FREEZE_TIMEOUT, Duration::from_secs(40));
    assert_eq!(ENGRAM_MAX_CALL_TIMEOUT_MS, 20_000);
    assert_eq!(ENGRAM_READINESS_TIMEOUT, Duration::from_secs(20));
    assert_eq!(
        ACCEPTANCE_EVALUATION_POLICY_READ_TIMEOUT,
        Duration::from_secs(20)
    );
    assert_eq!(ENGRAM_CONTROL_SETTLE_TIMEOUT, Duration::from_secs(21));
    assert_eq!(ENGRAM_SOURCE_ROOT_NAMING_BUDGET, Duration::from_secs(102));
    // Lifecycle arbitration outlasts the longest call and a whole admission.
    assert!(ENGRAM_CONTROL_SETTLE_TIMEOUT > Duration::from_millis(ENGRAM_MAX_CALL_TIMEOUT_MS));
    assert!(ENGRAM_CONTROL_SETTLE_TIMEOUT > Duration::from_millis(ENGRAM_DISPATCH_BUDGET_MS));
    // A naming call holds two full captures and the commit's reserve; what
    // its reads take comes out of the second capture.
    assert!(
        ENGRAM_SOURCE_ROOT_NAMING_BUDGET
            >= REVIEW_FREEZE_TIMEOUT * 2
                + ENGRAM_SOURCE_ROOT_COMMIT_RESERVE
                + Duration::from_millis(ENGRAM_MAX_CALL_TIMEOUT_MS)
    );
}

#[test]
fn a_default_delegation_wait_answers_within_the_codex_cut() {
    // A Codex caller that keeps the default wait on one delegation gets the
    // tool's own answer, `timedOut: true` or the result: the wait, its last
    // status read and the result read all fit the cut.
    assert_eq!(
        termal_delegation_default_wait_span(),
        Duration::from_secs(300 + 30 + 30)
    );
    // The cut is the longest wait plus the margin, so in any build it is at
    // least the span and the margin. (A test build gives tracker commands a
    // longer timeout, so its evaluation request allows longer still.)
    assert!(
        termal_delegation_mcp_codex_tool_timeout()
            >= termal_delegation_default_wait_span() + TERMAL_DELEGATION_MCP_CODEX_TOOL_MARGIN
    );
    // Whichever is longer, the longest long-call allowance or the default
    // wait's span, sets the cut. Both branches are checked with chosen
    // inputs, since this build's own allowances are not a release build's
    // (its tracker commands wait 30 s, not 6 s).
    let span = termal_delegation_default_wait_span();
    let margin = TERMAL_DELEGATION_MCP_CODEX_TOOL_MARGIN;
    assert_eq!(
        termal_delegation_codex_tool_timeout_for(span - Duration::from_secs(40), span),
        span + margin
    );
    assert_eq!(
        termal_delegation_codex_tool_timeout_for(span + Duration::from_secs(40), span),
        span + Duration::from_secs(40) + margin
    );
}

#[test]
fn a_delegation_wait_never_sleeps_past_its_deadline() {
    // The default wait's span holds only if a round never sleeps past the
    // deadline, whatever poll interval the caller asks (up to 30 s).
    let now = std::time::Instant::now();
    let poll = Duration::from_secs(30);
    assert_eq!(
        termal_delegation_wait_poll_sleep(poll, now + Duration::from_secs(1), now),
        Duration::from_secs(1)
    );
    assert_eq!(
        termal_delegation_wait_poll_sleep(poll, now, now + Duration::from_secs(5)),
        Duration::ZERO
    );
    // Short of the deadline, a round sleeps its poll interval.
    assert_eq!(
        termal_delegation_wait_poll_sleep(
            Duration::from_millis(100),
            now + Duration::from_secs(1),
            now
        ),
        Duration::from_millis(100)
    );
}

#[test]
fn the_docs_state_the_derived_limits() {
    // The prose states values the code derives; a change to one fails here.
    let doc = |path: &str| {
        fs::read_to_string(FsPath::new(env!("CARGO_MANIFEST_DIR")).join(path))
            .unwrap_or_else(|error| panic!("{path}: {error}"))
    };
    let naming = ENGRAM_SOURCE_ROOT_NAMING_BUDGET.as_secs();
    assert!(
        doc("docs/features/engram-host-adapter.md")
            .contains(&format!("One budget of {naming} seconds")),
        "the naming budget"
    );
    assert!(
        doc("docs/architecture.md").contains(&format!(
            "naming took longer than its {naming} s budget, so nothing was named; name it again"
        )),
        "the naming refusal"
    );
    let bridge = TermalDelegationMcpBridge::new(
        "session-parent".to_owned(),
        "http://127.0.0.1:1".to_owned(),
    )
    .unwrap();
    let followup = bridge.allowance(DelegationLongCall::Followup).as_secs();
    assert!(
        doc("docs/features/agent-delegation-sessions.md")
            .contains(&format!("request overhead (30 s): {followup} seconds")),
        "the follow-up allowance"
    );
    let freeze = REVIEW_FREEZE_TIMEOUT.as_secs();
    let observer = (REVIEW_FREEZE_TIMEOUT + REVIEW_FREEZE_OBSERVER_GRACE).as_secs();
    assert!(
        doc("docs/features/review-freeze-verification.md").contains(&format!(
            "({freeze} seconds internally, {observer} seconds for the"
        )),
        "the freeze budget"
    );
    let span = termal_delegation_default_wait_span().as_secs();
    let cut =
        (termal_delegation_default_wait_span() + TERMAL_DELEGATION_MCP_CODEX_TOOL_MARGIN).as_secs();
    let delegation_doc = doc("docs/features/agent-delegation-sessions.md");
    assert!(
        delegation_doc.contains(&format!("may take {span} seconds: the wait")),
        "the default wait's span"
    );
    assert!(
        delegation_doc.contains(&format!("at least {cut} seconds")),
        "the Codex tool timeout"
    );
}

#[test]
fn the_long_calls_wait_out_the_server_work_they_cover() {
    let bridge = TermalDelegationMcpBridge::new(
        "session-parent".to_owned(),
        "http://127.0.0.1:1".to_owned(),
    )
    .unwrap();
    let dispatch = Duration::from_millis(ENGRAM_DISPATCH_BUDGET_MS);
    let wait = |call| bridge.allowance(call);
    // A mailbox send that wakes a session holding a claim delivers its turn
    // before it answers: a gated admission and the turn's begin-time capture.
    assert!(wait(DelegationLongCall::MailboxSend) > dispatch + REVIEW_FREEZE_TIMEOUT);
    // A follow-up delivers the re-armed child turn after its recovery.
    assert!(
        wait(DelegationLongCall::Followup)
            > termal_delegation_followup_recovery_allowance() + dispatch + REVIEW_FREEZE_TIMEOUT
    );
    // A spawn binds the parent, which may recover a binding, and the new
    // child off the admission path, then delivers the child's first turn;
    // an evaluation request spawns its evaluator after its tracker reads.
    let spawn = engram_unqueued_bind_worst_case(true)
        + engram_unqueued_bind_worst_case(false)
        + dispatch
        + REVIEW_FREEZE_TIMEOUT;
    assert!(wait(DelegationLongCall::Spawn) > spawn);
    assert!(
        wait(DelegationLongCall::EvaluationRequest)
            > acceptance_evaluation_request_tracker_budget() + spawn
    );
    assert!(wait(DelegationLongCall::EvaluationSubmit) > acceptance_evaluation_submit_budget());
    // Naming outlasts its server budget; the freeze check outlasts the
    // checker's watch.
    assert!(wait(DelegationLongCall::SourceRootNaming) > ENGRAM_SOURCE_ROOT_NAMING_BUDGET);
    assert!(
        wait(DelegationLongCall::ReviewFreeze)
            > REVIEW_FREEZE_TIMEOUT + REVIEW_FREEZE_OBSERVER_GRACE
    );
}
