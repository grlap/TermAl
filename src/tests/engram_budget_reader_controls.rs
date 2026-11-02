//! Shared-clock controls for the surviving provider-capacity ACK owner.
//! Removed control Bind retry/circuit-breaker timing has no executor to test.
use super::*;

fn capacity_ack_fixture() -> (AppState, String, EngramBudgetClock, mpsc::Receiver<PersistRequest>) {
    let (mut state, _, session, _) = super::super::work_visualizer::fixture();
    let clock = state.select_test_scripted_engram_budget_clock();
    state.shutdown_persist_blocking();
    let (tx, receiver) = mpsc::channel();
    state.persist_tx = tx;
    {
        let mut inner = state.inner.lock().unwrap();
        let record = inner.session_mut(&session).unwrap();
        // Only the ACK image owner is exercised here. This record supplies
        // neither a runtime handoff nor permission to resume a turn.
        record.provider_capacity_retry = Some(ProviderCapacityRetry {
            nonce: "first-capacity-image".into(),
            server_instance: state.server_instance_id.clone(),
            runtime_id: "capacity-ack-fixture".into(),
            generation: record.active_turn_generation,
            stop_generation: record.runtime_stop_generation,
            pause_epoch: record.auto_dispatch_pause_epoch,
            settings: provider_capacity_settings(record),
            attempts: 0, due_at: Some("2026-11-02T12:00:00Z".into()),
            recovering: false, dispatched: false,
            original_preview: "native provider capacity failure".into(),
            saved: false, fence: None,
        });
    }
    (state, session, clock, receiver)
}

fn request_capacity_image(state: &AppState, session: &str, receiver: &mpsc::Receiver<PersistRequest>)
    -> (Box<PersistFence>, Value)
{
    let expected = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(session).unwrap();
        let expected = serde_json::to_value(&inner.sessions[index].provider_capacity_retry).unwrap();
        state.request_provider_capacity_ack_locked(&mut inner, index);
        expected
    };
    let PersistRequest::Fence(fence) = crate::tests::phase_sync::receive(
        receiver, "the actual provider-capacity image acknowledgement",
    ) else { panic!("the capacity owner must request a fence") };
    assert!(fence.retry_acknowledgement, "this fence belongs to the actual retry ACK owner");
    match &fence.target {
        PersistFenceTarget::ProviderCapacityRecovery { session_id, content } => {
            assert_eq!(session_id, session);
            assert_eq!(content, &expected);
        }
        _ => panic!("the capacity owner must acknowledge its exact content"),
    }
    (fence, expected)
}

#[test]
fn capacity_acknowledgement_uses_the_chosen_clock_once_for_its_exact_image() {
    for advance in [Duration::ZERO, Duration::from_secs(17)] {
        let (state, session, clock, receiver) = capacity_ack_fixture();
        clock.advance(advance);
        let now = clock.now();
        let baseline = super::snapshots(&state);
        let (fence, expected) = request_capacity_image(&state, &session, &receiver);
        assert_eq!(super::snapshots(&state), baseline + 1);
        assert_eq!(fence.completion.deadline, now + Duration::from_secs(20));
        assert_eq!(expected["nonce"], "first-capacity-image");
        assert!(fence.completion.poll_at(clock.now()).is_none());
        clock.advance(Duration::from_secs(20));
        assert!(matches!(fence.completion.poll_at(clock.now()), Some(Err(PersistFenceError::Deadline))));
        assert!(matches!(receiver.try_recv(), Err(mpsc::TryRecvError::Empty)), "one ACK request, no automatic retry");
    }
}

#[test]
fn a_later_capacity_image_keeps_its_own_content_and_original_clock_cut() {
    let (state, session, clock, receiver) = capacity_ack_fixture();
    let baseline = super::snapshots(&state);
    let (first, first_content) = request_capacity_image(&state, &session, &receiver);
    clock.advance(Duration::from_secs(1));
    {
        let mut inner = state.inner.lock().unwrap();
        let record = inner.session_mut(&session).unwrap();
        let retry = record.provider_capacity_retry.as_mut().unwrap();
        retry.nonce = "successor-capacity-image".into();
        retry.attempts = 1;
    }
    let (second, second_content) = request_capacity_image(&state, &session, &receiver);
    assert_eq!(super::snapshots(&state), baseline + 2);
    assert_eq!(first_content["nonce"], "first-capacity-image");
    assert_eq!(second_content["nonce"], "successor-capacity-image");
    assert_eq!(second_content["attempts"], 1);
    assert_eq!(second.completion.deadline.duration_since(first.completion.deadline), Duration::from_secs(1));
    clock.advance(Duration::from_secs(19));
    assert!(matches!(first.completion.poll_at(clock.now()), Some(Err(PersistFenceError::Deadline))));
    assert!(second.completion.poll_at(clock.now()).is_none(), "an older image's deadline cannot expire its successor");
    assert!(matches!(receiver.try_recv(), Err(mpsc::TryRecvError::Empty)));
}
