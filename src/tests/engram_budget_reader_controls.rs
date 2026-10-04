// Owns the positive controls for the two transport-failure readers of the
// Engram budget clock: on a session under Engram control with a chosen
// scripted clock, a counting failure times its bind retry at exactly the
// clock's now plus the existing delay, reading the clock once; a local-state
// or disabling failure reads none; a queued failure from its current owner is
// timed the same way, and one from a stale owner changes nothing and reads
// none. Does not own the no-use witnesses (the sibling
// src/tests/engram_budget_unchosen_readers.rs) or the clock boundary itself
// (the parent, src/tests/engram_budget_clock_boundary.rs). New file, nested
// under that parent.
use super::*;

fn controlled_session() -> (AppState, String, EngramBudgetClock) {
    let (state, project, session, root) = super::super::work_visualizer::fixture();
    let clock =
        super::super::engram_host_adapter::select_scripted_engram_budget_clock_before_enable(
            &state,
        );
    super::super::engram_host_adapter::enable_test_project_engram(&state, &project, &root);
    (state, session, clock)
}

fn engram_state(state: &AppState, session: &str) -> EngramSessionState {
    let inner = state.inner.lock().expect("state mutex poisoned");
    inner.sessions[inner.find_session_index(session).unwrap()]
        .engram
        .clone()
}

fn queue_head(state: &AppState, session: &str) -> EngramQueuedAdmissionOwner {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(session).unwrap();
    let record = inner.session_mut_by_index(index).unwrap();
    record.queued_prompts.push_back(QueuedPromptRecord {
        engram_waiting: false,
        promoted_message_index: None,
        promotion_disposition_known: true,
        engram_bind: None,
        engram_evaluate: None,
        engram_interrupted: false,
        source: QueuedPromptSource::User,
        attachments: Vec::new(),
        pending_prompt: PendingPrompt {
            engram_interrupted: false,
            is_engram_retained: false,
            attachments: Vec::new(),
            id: "budget-control-head".to_owned(),
            timestamp: stamp_now(),
            text: "queued head for the owner control".to_owned(),
            expanded_text: None,
            source: None,
        },
    });
    EngramQueuedAdmissionOwner::capture(record).expect("the queued head has an owner")
}

#[test]
fn a_counting_transport_failure_times_its_bind_retry_on_the_chosen_clock() {
    let (state, session, clock) = controlled_session();
    let baseline = super::snapshots(&state);
    let now = clock.now();
    state.record_engram_transport_failure(
        &session,
        &EngramTransportError::transport("scripted transport failure"),
    );
    assert_eq!(
        super::snapshots(&state),
        baseline + 1,
        "a counting failure reads the chosen clock once, to time its retry"
    );
    let engram = engram_state(&state, &session);
    assert_eq!(engram.consecutive_transport_failures, 1);
    assert_eq!(
        engram.next_bind_retry_at,
        Some(now + engram_bind_retry_delay(1)),
        "the retry is timed at the chosen clock's now plus the existing delay"
    );
    assert!(engram.rebind_required);

    state.record_engram_transport_failure(
        &session,
        &EngramTransportError::local_state("scripted local binding fault"),
    );
    assert_eq!(
        super::snapshots(&state),
        baseline + 1,
        "a local-state failure is no transport evidence and reads no clock"
    );
    state.record_engram_transport_failure(
        &session,
        &EngramTransportError::protocol("scripted unparsable response"),
    );
    assert_eq!(
        super::snapshots(&state),
        baseline + 1,
        "a disabling failure times no retry and reads no clock"
    );
    let engram = engram_state(&state, &session);
    assert!(engram.disabled_reason.is_some());
    assert!(engram.next_bind_retry_at.is_none());
}

#[test]
fn a_queued_transport_failure_times_only_its_current_owner() {
    let (state, session, clock) = controlled_session();
    let owner = queue_head(&state, &session);
    let baseline = super::snapshots(&state);
    let now = clock.now();
    state.record_queued_engram_transport_failure(
        &session,
        &EngramTransportError::transport("scripted transport failure"),
        Some(&owner),
    );
    assert_eq!(
        super::snapshots(&state),
        baseline + 1,
        "the current owner's counting failure reads the chosen clock once"
    );
    let timed = engram_state(&state, &session);
    assert_eq!(timed.consecutive_transport_failures, 1);
    assert_eq!(
        timed.next_bind_retry_at,
        Some(now + engram_bind_retry_delay(1))
    );

    // A successor admission supersedes the captured owner.
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session).unwrap();
        inner.session_mut_by_index(index).unwrap().engram.dispatch_generation += 1;
    }
    clock.advance(Duration::from_secs(60));
    state.record_queued_engram_transport_failure(
        &session,
        &EngramTransportError::transport("late scripted transport failure"),
        Some(&owner),
    );
    assert_eq!(
        super::snapshots(&state),
        baseline + 1,
        "a stale owner's late failure reads no clock"
    );
    let unchanged = engram_state(&state, &session);
    assert_eq!(
        unchanged.consecutive_transport_failures,
        timed.consecutive_transport_failures
    );
    assert_eq!(unchanged.next_bind_retry_at, timed.next_bind_retry_at);
}
