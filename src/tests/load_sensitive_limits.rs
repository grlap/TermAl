// The operation limits and the relations the limits and waits
// derived from them must keep. The limits live in their owners
// (engram_host_adapter.rs, engram_readiness.rs, review_freeze_process.rs,
// engram_source_roots.rs, acceptance_evaluation_api.rs); the bridge waits in
// delegation_mcp_timeouts.rs. Exact values belong to those owners.
use super::*;

#[test]
fn load_sensitive_limits_keep_their_operation_and_capture_relations() {
    // Ordinary connection teardown outlasts the longest transport call.
    assert!(ENGRAM_CONTROL_SETTLE_TIMEOUT > Duration::from_millis(ENGRAM_MAX_CALL_TIMEOUT_MS));
}

#[test]
fn a_default_delegation_wait_answers_within_the_codex_cut() {
    // A Codex caller that keeps the default wait on one delegation gets the
    // tool's own answer, `timedOut: true` or the result: the wait, its last
    // status read and the result read all fit the cut.
    // The cut is the longest wait plus the margin, so in any build it is at
    // least the span and the margin.
    assert!(
        termal_delegation_mcp_codex_tool_timeout()
            >= termal_delegation_default_wait_span() + TERMAL_DELEGATION_MCP_CODEX_TOOL_MARGIN
    );
    // Whichever is longer, the longest long-call allowance or the default
    // wait's span, sets the cut. Both branches are checked with chosen
    // inputs, since this build's own allowances are not a release build's
    // (a long explicit audit can outlast an ordinary tracker call).
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
            > acceptance_evaluation_request_tracker_budget()
                + acceptance_evaluation_request_overrun_budget()
                + spawn
    );
    assert!(wait(DelegationLongCall::EvaluationSubmit) > acceptance_evaluation_submit_budget());
    // The freeze check outlasts the checker's watch. Naming's issued-call
    // overrun is exercised through the bridge's sender seam.
    assert!(
        wait(DelegationLongCall::ReviewFreeze)
            > REVIEW_FREEZE_TIMEOUT + REVIEW_FREEZE_OBSERVER_GRACE
    );
}
