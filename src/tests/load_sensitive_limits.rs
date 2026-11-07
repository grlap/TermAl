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
    // Ordinary connection teardown outlasts the longest transport call.
    assert!(ENGRAM_CONTROL_SETTLE_TIMEOUT > Duration::from_millis(ENGRAM_MAX_CALL_TIMEOUT_MS));
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
fn the_long_calls_wait_out_the_server_work_they_cover() {
    let bridge = TermalDelegationMcpBridge::new(
        "session-parent".to_owned(),
        "http://127.0.0.1:1".to_owned(),
    )
    .unwrap();
    let wait = |call| bridge.allowance(call);
    // A mailbox send that wakes a session holding a claim delivers its turn
    // before it answers. Ordinary delivery retains the review-freeze budget.
    assert!(wait(DelegationLongCall::MailboxSend) > REVIEW_FREEZE_TIMEOUT);
    // A follow-up delivers the re-armed child turn after its recovery.
    assert!(
        wait(DelegationLongCall::Followup)
            > termal_delegation_followup_recovery_allowance() + REVIEW_FREEZE_TIMEOUT
    );
    // A spawn delivers the child's first ordinary turn; an evaluation
    // request additionally reads the tracker before spawning its evaluator.
    let spawn = REVIEW_FREEZE_TIMEOUT;
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
