//! Ordinary content/WAL cancellation, stale owners and real Stop arbitration.
use super::*;
use crate::tests::engram_host_adapter::uncertain_begin::OrdinaryRoot;

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
            PersistRequest::Fence(fence)
                if matches!(
                    &fence.target,
                    PersistFenceTarget::PromptDeliveryWriteAhead { .. }
                ) =>
            {
                let delta = collect_persist_delta_from_shared_state(&turn.state.inner, 0);
                persist_delta_via_cache(cache, turn.state.persistence_path.as_path(), &delta)
                    .unwrap();
                let connection = cache
                    .connection_for(turn.state.persistence_path.as_path())
                    .unwrap();
                let durable = fence.target.is_already_durable(connection).unwrap();
                return (fence, durable);
            }
            request => batch.accept(request),
        };
        let delta = collect_persist_delta_from_shared_state(&turn.state.inner, 0);
        persist_delta_with_fences(
            cache,
            turn.state.persistence_path.as_path(),
            &delta,
            &mut batch,
        )
        .unwrap();
    }
}

#[test]
fn cancel_during_real_write_ahead_ack_cannot_send_or_block_the_replacement() {
    let mut turn = OrdinaryRoot::new("cancel-real-wal");
    turn.state.shutdown_persist_blocking();
    let (tx, rx) = mpsc::channel();
    turn.state.persist_tx = tx;
    let id = turn.enqueue_durable("cancel this exact ordinary content", true);
    assert!(
        turn.state
            .request_queued_prompt_durability(&turn.session, &id)
    );
    let mut cache = SqlitePersistConnectionCache::new();
    let (fence, durable) = take_real_ordinary_write_ahead(&turn, &rx, &mut cache);
    let nonce = turn.record(|record| record.prompt_deliveries[&id].write_ahead.clone());
    let empty_before_ack = !turn
        .receiver
        .try_iter()
        .any(|command| matches!(command, CodexRuntimeCommand::Prompt { .. }));
    let canceled = turn.state.cancel_queued_prompt(&turn.session, &id);
    // Always release the real boundary before asserting either outcome.
    fence.finish(if durable {
        Ok(())
    } else {
        Err(PersistFenceError::WriteFailed(
            "exact WAL read-back was absent".into(),
        ))
    });
    canceled.unwrap();
    turn.state
        .settle_prompt_delivery_write_ahead(&turn.session, &id, &nonce, Ok(()));
    assert!(durable && empty_before_ack);
    turn.assert_no_prompt();
    let dispatch = turn.prepare("replacement after cancellation");
    deliver_turn_dispatch(&turn.state, dispatch)
        .into_public_result()
        .unwrap();
    assert!(
        turn.prompt()
            .prompt
            .contains("replacement after cancellation")
    );
    turn.record(|record| assert!(record.queued_prompts.is_empty()));
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn a_canceled_ordinary_owner_never_mutates_or_delivers_its_replacement() {
    let turn = OrdinaryRoot::new("exact-ordinary-successor");
    let id = turn.enqueue("old ordinary prompt", QueuedPromptSource::User);
    let owner = turn.record(|record| QueuedTurnOwner::capture(record).unwrap());
    turn.state.cancel_queued_prompt(&turn.session, &id).unwrap();
    let replacement = turn.enqueue("new ordinary prompt", QueuedPromptSource::Mailbox);
    let before = turn.saved();
    assert!(
        turn.state
            .dispatch_next_queued_turn_for_activation(&turn.session, owner)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        turn.saved(),
        before,
        "an old continuation cannot change the successor"
    );
    turn.assert_no_prompt();
    let started = turn
        .state
        .start_next_queued_turn_off_lock(&turn.session, false, false)
        .unwrap()
        .unwrap();
    deliver_turn_dispatch(&turn.state, started.dispatch)
        .into_public_result()
        .unwrap();
    assert!(turn.prompt().prompt.contains("new ordinary prompt"));
    turn.record(|record| {
        assert!(record.queued_prompts.is_empty());
        assert_ne!(replacement, id);
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_write_ahead_failures_keep_exact_pending_content_without_a_handoff() {
    for fault in [
        PersistFenceError::Deadline,
        PersistFenceError::WorkerStopped,
        PersistFenceError::WriteFailed("controlled ordinary WAL failure".into()),
    ] {
        let mut turn = OrdinaryRoot::new("strict-ordinary-wal-failure");
        turn.state.shutdown_persist_blocking();
        let (tx, rx) = mpsc::channel();
        turn.state.persist_tx = tx;
        let id = turn.enqueue_durable("exact ordinary content — źródło 🦀", true);
        assert!(
            turn.state
                .request_queued_prompt_durability(&turn.session, &id)
        );
        let mut cache = SqlitePersistConnectionCache::new();
        let (fence, durable) = take_real_ordinary_write_ahead(&turn, &rx, &mut cache);
        let nonce = turn.record(|record| record.prompt_deliveries[&id].write_ahead.clone());
        let empty_before_ack = !turn
            .receiver
            .try_iter()
            .any(|command| matches!(command, CodexRuntimeCommand::Prompt { .. }));
        fence.finish(Err(fault));
        let settled = phase_sync::PollGuard::new();
        while !turn.record(|record| {
            record.prompt_deliveries[&id].disposition == PromptDeliveryDisposition::Pending
        }) {
            settled.wait("ordinary WAL failure returns to pending");
        }
        // The superseded successful callback must not rescue this failed nonce.
        turn.state
            .settle_prompt_delivery_write_ahead(&turn.session, &id, &nonce, Ok(()));
        let delta = collect_persist_delta_from_shared_state(&turn.state.inner, 0);
        persist_delta_with_fences(
            &mut cache,
            turn.state.persistence_path.as_path(),
            &delta,
            &mut PersistFenceBatch::default(),
        )
        .unwrap();
        assert!(durable && empty_before_ack);
        let loaded = load_state(turn.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        let record = &loaded.sessions[loaded.find_session_index(&turn.session).unwrap()];
        assert_eq!(
            record.prompt_deliveries[&id].disposition,
            PromptDeliveryDisposition::Pending
        );
        assert_ne!(record.prompt_deliveries[&id].write_ahead, nonce);
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(record.queued_prompts[0].pending_prompt.id, id);
        assert_eq!(
            record.queued_prompts[0].pending_prompt.text,
            "exact ordinary content — źródło 🦀"
        );
        assert!(record.queued_prompts[0].promoted_message_index.is_none());
        assert!(record.session.prompt_history.is_empty());
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}

#[tokio::test]
async fn ordinary_unsent_handoff_keeps_the_real_stop_owner_until_its_outcome() {
    for failed_stop in [true, false] {
        super::ordinary_regressions::ordinary_stop_case(failed_stop, true).await;
    }
}
