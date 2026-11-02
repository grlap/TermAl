//! Ordinary durability before provider enqueue, plus passive transport diagnostics.
use super::*;
use crate::tests::engram_host_adapter::uncertain_begin::OrdinaryRoot;

#[path = "engram_prepared_resume_contract.rs"]
mod prepared_resume_contract;
#[path = "engram_begin_preparation_retry.rs"]
mod begin_preparation_retry;
#[path = "engram_retry_hold_producers.rs"]
mod retry_hold_producers;
#[path = "engram_admission_latency.rs"]
mod admission_latency;

// Drive ordinary content/WAL requests through the existing SQLite writer.
// The real worker is joined before its sender is replaced; no parallel
// writer or acknowledgement without an exact durable read-back is created.
fn take_real_ordinary_write_ahead(
    turn: &OrdinaryRoot,
    rx: &mpsc::Receiver<PersistRequest>,
    cache: &mut SqlitePersistConnectionCache,
) -> (Box<PersistFence>, bool) {
    let mut batch = PersistFenceBatch::default();
    loop {
        let request = phase_sync::receive(rx, "ordinary content or WAL persistence request");
        match request {
            PersistRequest::Fence(fence) if matches!(&fence.target,
                PersistFenceTarget::PromptDeliveryWriteAhead { .. }) =>
            {
                let delta = collect_persist_delta_from_shared_state(&turn.state.inner, 0);
                persist_delta_via_cache(cache, turn.state.persistence_path.as_path(), &delta).unwrap();
                let connection = cache.connection_for(turn.state.persistence_path.as_path()).unwrap();
                let durable = fence.target.is_already_durable(connection).unwrap();
                return (fence, durable);
            }
            request => {
                batch.accept(request);
            }
        }
        let delta = collect_persist_delta_from_shared_state(&turn.state.inner, 0);
        persist_delta_with_fences(cache, turn.state.persistence_path.as_path(), &delta, &mut batch).unwrap();
    }
}

#[test]
fn ordinary_healthy_delivery_needs_no_begin_request_or_grant() {
    let turn = OrdinaryRoot::new("ordinary-healthy-delivery");
    let dispatch = turn.prepare("healthy ordinary prompt");
    deliver_turn_dispatch(&turn.state, dispatch).into_public_result().unwrap();
    assert!(turn.prompt().prompt.contains("healthy ordinary prompt"));
    turn.record(|record| {
        assert!(record.queued_prompts.is_empty());
        assert!(record.prompt_deliveries.is_empty());
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn real_content_and_write_ahead_cuts_precede_handoff_but_are_not_provider_acceptance() {
    let mut turn = OrdinaryRoot::new("real-wal-before-provider");
    turn.state.shutdown_persist_blocking();
    let (tx, rx) = mpsc::channel();
    turn.state.persist_tx = tx;
    let id = turn.enqueue_durable("receipted ordinary handoff", true);
    assert!(turn.state.request_queued_prompt_durability(&turn.session, &id));
    let mut cache = SqlitePersistConnectionCache::new();
    let (fence, durable) = take_real_ordinary_write_ahead(&turn, &rx, &mut cache);
    let before = turn.record(|record| (
        record.prompt_deliveries[&id].disposition,
        record.prompt_deliveries[&id].turn_generation,
        record.queued_prompts[0].promoted_message_index,
    ));
    let provider_was_empty = !turn.receiver.try_iter().any(|command|
        matches!(command, CodexRuntimeCommand::Prompt { .. }));
    // The real SQLite proof releases this exact nonce, once.
    fence.finish(if durable { Ok(()) } else {
        Err(PersistFenceError::WriteFailed("exact WAL read-back was absent".into()))
    });
    assert!(durable && provider_was_empty);
    let command = turn.prompt();
    let delta = collect_persist_delta_from_shared_state(&turn.state.inner, 0);
    persist_delta_with_fences(&mut cache, turn.state.persistence_path.as_path(), &delta,
        &mut PersistFenceBatch::default()).unwrap();
    assert_eq!(before, (PromptDeliveryDisposition::HandingOff, None, None));
    assert!(command.prompt.ends_with("receipted ordinary handoff"));
    assert_eq!(command.delivery_receipt.as_ref().unwrap().prompt_id, id);
    turn.record(|record| {
        assert!(record.queued_prompts.is_empty());
        assert_eq!(record.prompt_deliveries[&id].disposition, PromptDeliveryDisposition::HandingOff);
        assert_eq!(record.prompt_deliveries[&id].turn_generation, Some(command.active_turn_generation));
    });
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&turn.session).unwrap()];
    assert!(record.queued_prompts.is_empty());
    assert_eq!(record.prompt_deliveries[&id].disposition, PromptDeliveryDisposition::Ambiguous,
        "channel enqueue and WAL ACK are not provider acceptance after restart");
    assert!(record.prompt_deliveries[&id].resume.is_none());
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn unacknowledged_ordinary_write_ahead_withholds_provider_delivery() {
    for fault in [PersistFenceError::Deadline, PersistFenceError::WorkerStopped,
        PersistFenceError::WriteFailed("ordinary preparation write rejected".into())] {
        let mut turn = OrdinaryRoot::new("ordinary-unacknowledged-preparation");
        turn.state.shutdown_persist_blocking();
        let (tx, rx) = mpsc::channel();
        turn.state.persist_tx = tx;
        let id = turn.enqueue_durable("exact unacknowledged ordinary prompt", true);
        assert!(turn.state.request_queued_prompt_durability(&turn.session, &id));
        let mut cache = SqlitePersistConnectionCache::new();
        let (fence, durable) = take_real_ordinary_write_ahead(&turn, &rx, &mut cache);
        let before = turn.record(|record| (
            serde_json::to_value(&record.queued_prompts[0].pending_prompt).unwrap(),
            record.prompt_deliveries[&id].write_ahead.clone(),
        ));
        let empty_before_ack = !turn.receiver.try_iter().any(|command|
            matches!(command, CodexRuntimeCommand::Prompt { .. }));
        fence.finish(Err(fault));
        let settled = phase_sync::PollGuard::new();
        while !turn.record(|record| record.prompt_deliveries[&id].disposition == PromptDeliveryDisposition::Pending) {
            settled.wait("ordinary preparation failure settles");
        }
        assert!(durable && empty_before_ack);
        turn.record(|record| {
            assert_eq!(serde_json::to_value(&record.queued_prompts[0].pending_prompt).unwrap(), before.0);
            assert!(record.queued_prompts[0].promoted_message_index.is_none());
            assert_ne!(record.prompt_deliveries[&id].write_ahead, before.1);
            assert!(record.prompt_deliveries[&id].turn_generation.is_none());
        });
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}

#[test]
fn passive_transport_diagnostics_keep_unknown_application_and_unavailable_process_evidence() {
    let key = "historical-unknown-attempt";
    let error = EngramTransportError::transport("original transport reply was lost");
    let cause = EngramCausalFailure::error(&error, "historical_begin", Some(key), &[]);
    let cause = serde_json::to_value(&cause).unwrap();
    assert_eq!(cause["failureClass"], "transport");
    assert_eq!(cause["message"], "original transport reply was lost");
    assert_eq!(cause["attemptId"], key);
    assert_eq!(cause["remoteApplication"], "unknown");
    assert_eq!(cause["controlProcess"], json!({"identity":"unavailable","exitState":"unavailable"}));
}
