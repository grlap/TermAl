//! Ordinary receipt ownership through the real queue, write-ahead and Stop owners.
//! DeliveryWriter is an existing no-write acknowledgement double; the separate
//! SQLite case below is the on-disk witness, not evidence that the double writes.
use super::super::uncertain_begin::OrdinaryRoot;
use super::*;
use crate::tests::prompt_delivery::{DeliveryWriter, HeldFence};

fn receipt(turn: &OrdinaryRoot, id: &str) -> PromptDelivery {
    turn.record(|record| record.prompt_deliveries[id].clone())
}

pub(super) fn mark_pending_receipt(turn: &OrdinaryRoot, id: &str) {
    turn.edit(|record| {
        let sequence = next_prompt_delivery_sequence(record);
        record.prompt_deliveries.insert(
            id.into(),
            PromptDelivery {
                remote_prompt: None,
                federation_operation: None,
                federation_handoff_started: false,
                disposition: PromptDeliveryDisposition::Pending,
                attempt: 1,
                write_ahead: Uuid::new_v4().to_string(),
                sequence,
                message_id: None,
                turn_generation: None,
                attempt_key: None,
                reason: None,
                entry: None,
                write_ahead_acknowledged: false,
                resume: None,
                runtime_write: PromptRuntimeWrite::Untracked,
            },
        );
        sync_prompt_delivery_view(record);
    });
}

fn written_ahead(
    turn: &OrdinaryRoot,
    writer: &DeliveryWriter,
) -> (String, Box<PersistFence>, String) {
    let id = turn.enqueue_durable("receipted ordinary prompt", true);
    assert!(
        turn.state
            .request_queued_prompt_durability(&turn.session, &id)
    );
    writer.take(HeldFence::Durability).finish(Ok(()));
    let fence = writer.take(HeldFence::WriteAhead);
    let delivery = receipt(turn, &id);
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::HandingOff);
    assert!(delivery.turn_generation.is_none());
    turn.assert_no_prompt();
    (id, fence, delivery.write_ahead)
}

#[test]
fn ordinary_receipt_acknowledgement_hands_off_once_and_duplicate_ack_is_inert() {
    let mut turn = OrdinaryRoot::new("ordinary-receipt-once");
    let writer = DeliveryWriter::attach(&mut turn.state);
    let (id, fence, nonce) = written_ahead(&turn, &writer);
    turn.state
        .settle_prompt_delivery_write_ahead(&turn.session, &id, &nonce, Ok(()));
    assert!(turn.prompt().prompt.contains("receipted ordinary prompt"));
    let before = turn.saved();
    turn.state
        .settle_prompt_delivery_write_ahead(&turn.session, &id, &nonce, Ok(()));
    turn.state
        .return_unpromoted_write_ahead_to_pending(&turn.session, &id, &nonce);
    assert_eq!(
        turn.saved(),
        before,
        "promoted receipt cannot be reset or sent twice"
    );
    turn.assert_no_prompt();
    turn.assert_no_control();
    drop(fence);
}

#[test]
fn ordinary_stop_claim_or_pause_before_ack_keeps_the_unsent_receipt_pending() {
    for stop in [false, true] {
        let mut turn = OrdinaryRoot::new("ordinary-receipt-stop");
        let writer = DeliveryWriter::attach(&mut turn.state);
        let (id, fence, nonce) = written_ahead(&turn, &writer);
        if stop {
            // User input survives Stop; ordinary workflow entries obey Stop's
            // separate discard policy and cannot be used as a retained oracle.
            turn.edit(|record| {
                record.queued_prompts.front_mut().unwrap().source = QueuedPromptSource::User;
                record.session.status = SessionStatus::Active;
            });
            let (_, claim) = turn
                .state
                .begin_requested_stop_session(&turn.session, &StopSessionOptions::default())
                .unwrap();
            assert!(
                claim.is_some(),
                "actual Stop owner established before acknowledgement"
            );
        } else {
            turn.edit(|record| record.set_auto_dispatch_blocked(true));
        }
        turn.state
            .settle_prompt_delivery_write_ahead(&turn.session, &id, &nonce, Ok(()));
        let delivery = receipt(&turn, &id);
        assert_eq!(delivery.disposition, PromptDeliveryDisposition::Pending);
        assert_ne!(delivery.write_ahead, nonce);
        assert!(!delivery.write_ahead_acknowledged && delivery.resume.is_none());
        turn.record(|record| {
            assert!(record.orchestrator_auto_dispatch_blocked || record.runtime_stop_in_progress);
            assert_eq!(record.queued_prompts.front().unwrap().pending_prompt.id, id);
        });
        turn.assert_no_prompt();
        turn.assert_no_control();
        drop(fence);
    }
}

#[test]
fn ordinary_receipt_ignores_unused_control_refusal_and_defer() {
    for decision in ["refuse", "defer"] {
        let mut turn = OrdinaryRoot::new("ordinary-receipt-unused-policy");
        turn.transport
            .responses
            .lock()
            .unwrap()
            .push_back(ScriptedEngramControlResponse::Reply(Ok(
                json!({"decision":decision}),
            )));
        let writer = DeliveryWriter::attach(&mut turn.state);
        let (id, fence, nonce) = written_ahead(&turn, &writer);
        turn.state
            .settle_prompt_delivery_write_ahead(&turn.session, &id, &nonce, Ok(()));
        turn.prompt();
        assert!(receipt(&turn, &id).turn_generation.is_some());
        assert_eq!(turn.transport.responses.lock().unwrap().len(), 1);
        turn.assert_no_prompt();
        turn.assert_no_control();
        drop(fence);
    }
}

#[test]
fn ordinary_unsent_reset_rotates_once_and_a_failed_fresh_write_never_sends() {
    let mut turn = OrdinaryRoot::new("ordinary-receipt-reset");
    let writer = DeliveryWriter::attach(&mut turn.state);
    let (id, first_fence, nonce) = written_ahead(&turn, &writer);
    turn.edit(|record| {
        record
            .prompt_deliveries
            .get_mut(&id)
            .unwrap()
            .write_ahead_acknowledged = true
    });
    turn.state
        .return_unpromoted_write_ahead_to_pending(&turn.session, &id, &nonce);
    let pending = receipt(&turn, &id);
    assert_eq!(pending.disposition, PromptDeliveryDisposition::Pending);
    assert_ne!(pending.write_ahead, nonce);
    turn.state
        .return_unpromoted_write_ahead_to_pending(&turn.session, &id, &nonce);
    assert_eq!(receipt(&turn, &id).write_ahead, pending.write_ahead);
    assert!(
        turn.state
            .dispatch_next_queued_turn(&turn.session, false)
            .unwrap()
            .is_none()
    );
    let second_fence = writer.take(HeldFence::WriteAhead);
    let second = receipt(&turn, &id);
    assert_ne!(second.write_ahead, nonce);
    turn.state.settle_prompt_delivery_write_ahead(
        &turn.session,
        &id,
        &second.write_ahead,
        Err(PersistFenceError::WriteFailed("fixture disk full".into())),
    );
    assert_eq!(
        receipt(&turn, &id).disposition,
        PromptDeliveryDisposition::Pending
    );
    assert_ne!(receipt(&turn, &id).write_ahead, second.write_ahead);
    turn.assert_no_prompt();
    turn.assert_no_control();
    drop((first_fence, second_fence));
}

#[test]
fn ordinary_stale_nonce_cleanup_preserves_successor_promoted_and_settled_receipts() {
    for state in ["successor", "promoted", "accepted"] {
        let turn = OrdinaryRoot::new("ordinary-receipt-successor");
        let id = turn.enqueue("retained receipt", QueuedPromptSource::User);
        mark_pending_receipt(&turn, &id);
        turn.edit(|record| {
            let generation = record.active_turn_generation;
            let delivery = record.prompt_deliveries.get_mut(&id).unwrap();
            delivery.disposition = PromptDeliveryDisposition::HandingOff;
            delivery.write_ahead = if state == "successor" {
                "new-nonce"
            } else {
                "old-nonce"
            }
            .into();
            delivery.write_ahead_acknowledged = true;
            if state != "successor" {
                delivery.turn_generation = Some(generation);
            }
            if state == "accepted" {
                delivery.disposition = PromptDeliveryDisposition::Accepted;
            }
        });
        let before = turn.saved();
        turn.state
            .return_unpromoted_write_ahead_to_pending(&turn.session, &id, "old-nonce");
        turn.state
            .settle_prompt_delivery_write_ahead(&turn.session, &id, "old-nonce", Ok(()));
        assert_eq!(turn.saved(), before, "{state}");
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}

#[test]
fn ordinary_real_sqlite_distinguishes_unsaved_reset_from_saved_pending() {
    let turn = OrdinaryRoot::new("ordinary-receipt-sqlite");
    let id = turn.enqueue("crash boundary receipt", QueuedPromptSource::User);
    mark_pending_receipt(&turn, &id);
    turn.edit(|record| {
        let delivery = record.prompt_deliveries.get_mut(&id).unwrap();
        delivery.disposition = PromptDeliveryDisposition::HandingOff;
        delivery.write_ahead_acknowledged = true;
    });
    {
        let inner = turn.state.inner.lock().unwrap();
        persist_locked_state_to_sqlite(&turn.state.persistence_path, &inner).unwrap();
    }
    let disk = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    let read = || -> PersistedSessionRecord {
        let raw: String = disk
            .query_row(
                "SELECT value_json FROM sessions WHERE id=?1",
                [&turn.session],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str(&raw).unwrap()
    };
    let nonce = receipt(&turn, &id).write_ahead;
    // Deliberately edit memory without committing: this is a crash-cut specimen,
    // not a successful reset acknowledgement.
    {
        let mut inner = turn.state.inner.lock().unwrap();
        let record = inner.session_mut(&turn.session).unwrap();
        let delivery = record.prompt_deliveries.get_mut(&id).unwrap();
        delivery.disposition = PromptDeliveryDisposition::Pending;
        delivery.write_ahead = "fresh-reset".into();
        delivery.write_ahead_acknowledged = false;
        delivery.resume = None;
    }
    let old = read().into_record().unwrap();
    assert_eq!(
        old.prompt_deliveries[&id].disposition,
        PromptDeliveryDisposition::Ambiguous
    );
    assert!(
        !old.queued_prompts
            .iter()
            .any(|entry| entry.pending_prompt.id == id)
    );
    {
        let inner = turn.state.inner.lock().unwrap();
        persist_locked_state_to_sqlite(&turn.state.persistence_path, &inner).unwrap();
    }
    let saved = read().into_record().unwrap();
    assert_eq!(
        saved.prompt_deliveries[&id].disposition,
        PromptDeliveryDisposition::Pending
    );
    assert_eq!(saved.prompt_deliveries[&id].write_ahead, "fresh-reset");
    assert_eq!(saved.queued_prompts.front().unwrap().pending_prompt.id, id);
    assert_ne!(saved.prompt_deliveries[&id].write_ahead, nonce);
    turn.assert_no_prompt();
    turn.assert_no_control();
}
