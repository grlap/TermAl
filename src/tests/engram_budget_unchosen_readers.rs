// Owns the witnesses that four production readers of the Engram budget clock
// take no clock on a path that never uses one: a queue drain with nothing to
// start, a transport failure for a session without Engram control, a queued
// transport failure with no current owner, and boot recovery with no targets.
// Each runs on a fresh state whose clock was never chosen and asserts that
// nothing read it. Does not own the eligible branches' timing behaviour
// (the bind-retry, circuit-breaker and boot-recovery tests do) or the clock
// boundary itself (its parent, src/tests/engram_budget_clock_boundary.rs).
// New file, nested under that parent.
use super::*;

fn plain_session() -> (AppState, String) {
    let (state, _, session, _) = super::super::work_visualizer::fixture();
    assert_eq!(
        super::snapshots(&state),
        0,
        "the fixture itself reads no budget clock"
    );
    (state, session)
}

#[test]
fn an_empty_queue_drain_without_engram_takes_no_budget_clock() {
    let (state, session) = plain_session();
    let started = state
        .start_next_queued_turn_inner_off_lock(&session, false, false, None)
        .expect("the drain succeeds");
    assert!(started.is_none(), "an empty queue starts nothing");
    assert_eq!(
        super::snapshots(&state),
        0,
        "a drain that reaches no bind-retry check reads no budget clock"
    );
}

#[test]
fn a_transport_failure_without_engram_control_takes_no_budget_clock() {
    let (state, session) = plain_session();
    state.record_engram_transport_failure(
        &session,
        &EngramTransportError::transport("scripted transport failure"),
    );
    assert_eq!(
        super::snapshots(&state),
        0,
        "a session without Engram control records nothing and reads no budget clock"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.engram.consecutive_transport_failures, 0);
    assert!(record.engram.next_bind_retry_at.is_none());
}

#[test]
fn a_queued_transport_failure_without_an_owner_takes_no_budget_clock() {
    let (state, session) = plain_session();
    state.record_queued_engram_transport_failure(
        &session,
        &EngramTransportError::transport("scripted transport failure"),
        None,
    );
    assert_eq!(
        super::snapshots(&state),
        0,
        "a failure with no current queue owner records nothing and reads no budget clock"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.engram.consecutive_transport_failures, 0);
    assert!(record.engram.next_bind_retry_at.is_none());
}

#[test]
fn boot_recovery_with_no_targets_takes_no_budget_clock() {
    let state = test_app_state();
    state.recover_prepared_engram_sessions_after_boot(EngramBootRecoveryPlan {
        targets: Vec::new(),
        budget: Duration::from_secs(5),
    });
    assert_eq!(
        super::snapshots(&state),
        0,
        "a plan with no targets has no work to time and reads no budget clock"
    );
}
