// Owns the positive control for the queue drain's bind-retry due check: with
// a real released bind-retry owner on the chosen scripted clock, that owner's
// drain admits nothing while the backoff is still pending, and the same drain
// with the same owner admits the head once the clock has passed it. Between
// the two drains only the chosen clock moves, so the refusal is the due
// check's. Does not own the bind-retry park, withdrawal and cancel paths (the
// parent, src/tests/engram_bind_backoff_retry.rs) or the no-use witnesses
// (src/tests/engram_budget_unchosen_readers.rs). New file, nested under that
// parent to reuse its root-dispatch fixture.
use super::*;

#[test]
fn a_released_bind_retry_admits_only_once_its_backoff_is_due_on_the_chosen_clock() {
    let (state, session, receiver, transport) = root_fixture([
        bind_reply("fresh-bind"),
        grant_reply("fresh-grant"),
        begin_reply("fresh-grant"),
    ]);
    arm_bind_backoff(&state, &session, Duration::from_secs(3600));
    let dispatch = root_dispatch(&state, &session, false);
    require_unprepared_owned_bind_dispatch(&state, &session, &dispatch);
    let original = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session).unwrap()].queued_prompts[0]
            .pending_prompt
            .id
            .clone()
    };
    assert!(matches!(
        state
            .dispatch_turn(
                &session,
                SendMessageRequest {
                    text: "due-check successor".to_owned(),
                    expanded_text: None,
                    attachments: Vec::new(),
                    source_session_id: None,
                    source_mailbox: None,
                },
            )
            .unwrap(),
        DispatchTurnResult::Queued
    ));
    state.cancel_queued_prompt(&session, &original).unwrap();
    let _outcome = deliver_turn_dispatch(&state, dispatch);
    acknowledge_bind_retry(&state, &session);
    let owner = {
        let inner = state.inner.lock().unwrap();
        EngramQueuedAdmissionOwner::capture(
            &inner.sessions[inner.find_session_index(&session).unwrap()],
        )
        .expect("the successor head has an owner")
    };
    let drain = || {
        state
            .start_next_queued_turn_off_lock_for_owner(
                &session,
                true,
                false,
                Some(QueuedDrainOwner::BindRetry(owner.clone())),
            )
            .expect("the drain itself succeeds")
    };

    // Not yet due on the chosen clock: the released owner's drain admits nothing.
    assert!(
        drain().is_none(),
        "a backoff still pending on the chosen clock admits nothing"
    );
    assert!(receiver.try_recv().is_err());
    assert!(transport.requests().is_empty());

    // Due: only the chosen clock moves, and the same owner's drain admits the head.
    elapse_bind_backoff(&state, &session);
    let started = drain().expect("the head is admitted once its backoff is due");
    let _delivered = deliver_turn_dispatch(&state, started.dispatch);
    let CodexRuntimeCommand::Prompt { command, .. } = receiver.try_recv().unwrap() else {
        panic!("the admitted head reaches the runtime");
    };
    assert!(command.prompt.contains("due-check successor"));
    assert!(receiver.try_recv().is_err());
}
