//! Ordinary recovery boundaries that survive removal of grant-abort retries.
//! Receipt durability, WAL acknowledgement, operator pause, selected Human
//! Resume and exact owner checks remain real product prerequisites.
use super::*;
use crate::tests::engram_host_adapter::uncertain_begin::OrdinaryRoot;
use crate::tests::prompt_delivery::{DeliveryWriter, HeldFence};
use axum::body::{Body, to_bytes};
use axum::http::Request;
use tower::ServiceExt;

fn receipt_waiting_for_resume(label: &str) -> (OrdinaryRoot, DeliveryWriter, String) {
    let mut turn = OrdinaryRoot::new(label);
    let writer = DeliveryWriter::attach(&mut turn.state);
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let id = turn.enqueue_durable("Ordinary receipted input.", true);
    assert!(
        turn.state
            .request_queued_prompt_durability(&turn.session, &id)
    );
    writer.take(HeldFence::Durability).finish(Ok(()));
    let guard = phase_sync::PollGuard::new();
    while !turn.record(|record| record.queued_prompt_durability_holds[&id].acknowledged) {
        guard.wait("the receipt's actual content acknowledgement");
    }
    turn.assert_no_prompt();
    (turn, writer, id)
}

#[tokio::test]
async fn human_prompt_ui_resume_selects_the_exact_ordinary_head_once() {
    let turn = OrdinaryRoot::new("ordinary-human-recovery");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let id = turn.enqueue("Selected ordinary input.", QueuedPromptSource::User);
    let selection = turn.record(|record| HumanPromptSelection {
        prompt: projected_pending_prompts(record)[0].clone(),
        session_mutation_stamp: record.mutation_stamp,
    });
    let response = app_router(turn.state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{}/queue/resume", turn.session))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"origin":"human","humanChoice":selection}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert!(turn.prompt().prompt.contains("Selected ordinary input."));
    turn.assert_no_prompt();
    turn.record(|record| {
        assert!(
            !record
                .queued_prompts
                .iter()
                .any(|entry| entry.pending_prompt.id == id)
        );
        assert!(record.human_prompt_turn.is_none());
        assert!(record.human_queue_choices.is_empty());
    });
    turn.assert_no_control();
}

#[tokio::test]
async fn stale_human_resume_cannot_consume_a_replacement_ordinary_head() {
    let turn = OrdinaryRoot::new("ordinary-human-recovery-stale");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let old = turn.enqueue("Cancelled selection.", QueuedPromptSource::User);
    let selection = turn.record(|record| HumanPromptSelection {
        prompt: projected_pending_prompts(record)[0].clone(),
        session_mutation_stamp: record.mutation_stamp,
    });
    let next = turn.enqueue("Independent successor.", QueuedPromptSource::User);
    turn.state
        .cancel_queued_prompt(&turn.session, &old)
        .unwrap();
    let before = turn.saved();
    let response = app_router(turn.state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{}/queue/resume", turn.session))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"origin":"human","humanChoice":selection}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(turn.saved(), before);
    turn.record(|record| assert_eq!(record.queued_prompts[0].pending_prompt.id, next));
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_receipt_waits_for_its_wal_then_delivers_the_original_id_once() {
    let (turn, writer, id) = receipt_waiting_for_resume("ordinary-recovery-wal");
    let outcome = turn
        .state
        .resume_local_session_queue(&turn.session, None)
        .unwrap();
    assert!(matches!(
        outcome,
        Some(TurnDispatchDeliveryOutcome::Scheduled)
    ));
    let write_ahead = writer.take(HeldFence::WriteAhead);
    turn.record(|record| {
        let delivery = &record.prompt_deliveries[&id];
        assert_eq!(delivery.disposition, PromptDeliveryDisposition::HandingOff);
        assert!(delivery.turn_generation.is_none());
        assert!(!delivery.write_ahead_acknowledged);
    });
    turn.assert_no_prompt();
    write_ahead.finish(Ok(()));
    let command = turn.prompt();
    assert!(command.prompt.contains("Ordinary receipted input."));
    assert!(command.delivery_receipt.is_some());
    turn.record(|record| {
        assert_eq!(
            record.prompt_deliveries[&id].turn_generation,
            Some(command.active_turn_generation)
        );
        assert_eq!(
            record
                .session
                .messages
                .iter()
                .filter(|message| message.id() == id)
                .count(),
            1
        );
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
    drop(writer);
}

#[test]
fn failed_ordinary_wal_keeps_a_pending_receipt_and_requires_a_fresh_acknowledgement() {
    let (turn, writer, id) = receipt_waiting_for_resume("ordinary-recovery-failed-wal");
    turn.state.resume_session_queue(&turn.session).unwrap();
    let first = turn.record(|record| record.prompt_deliveries[&id].write_ahead.clone());
    writer
        .take(HeldFence::WriteAhead)
        .finish(Err(PersistFenceError::WriteFailed(
            "injected WAL failure".into(),
        )));
    let guard = phase_sync::PollGuard::new();
    while turn.record(|record| {
        record.prompt_deliveries[&id].disposition == PromptDeliveryDisposition::HandingOff
    }) {
        guard.wait("the failed WAL returns the receipt to pending");
    }
    turn.assert_no_prompt();
    turn.record(|record| {
        assert_eq!(
            record.prompt_deliveries[&id].disposition,
            PromptDeliveryDisposition::Pending
        );
        assert!(!record.prompt_deliveries[&id].write_ahead_acknowledged);
        assert_eq!(record.queued_prompts[0].pending_prompt.id, id);
    });
    turn.state.resume_session_queue(&turn.session).unwrap();
    let second = writer.take(HeldFence::WriteAhead);
    turn.record(|record| assert_ne!(record.prompt_deliveries[&id].write_ahead, first));
    turn.assert_no_prompt();
    second.finish(Ok(()));
    assert!(turn.prompt().prompt.contains("Ordinary receipted input."));
    turn.assert_no_prompt();
    turn.assert_no_control();
    drop(writer);
}

#[test]
fn newer_pause_during_ordinary_wal_keeps_the_unsent_receipt_owned_and_paused() {
    let (turn, writer, id) = receipt_waiting_for_resume("ordinary-recovery-pause-race");
    turn.state.resume_session_queue(&turn.session).unwrap();
    let write_ahead = writer.take(HeldFence::WriteAhead);
    let old_nonce = turn.record(|record| record.prompt_deliveries[&id].write_ahead.clone());
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    write_ahead.finish(Ok(()));
    let guard = phase_sync::PollGuard::new();
    while turn.record(|record| {
        record.prompt_deliveries[&id].disposition == PromptDeliveryDisposition::HandingOff
    }) {
        guard.wait("the stale WAL acknowledgement gives up its owner");
    }
    turn.record(|record| {
        let delivery = &record.prompt_deliveries[&id];
        assert_eq!(delivery.disposition, PromptDeliveryDisposition::Pending);
        assert!(!delivery.write_ahead_acknowledged);
        assert_ne!(delivery.write_ahead, old_nonce);
        assert!(delivery.turn_generation.is_none());
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert_eq!(record.queued_prompts[0].pending_prompt.id, id);
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
    drop(writer);
}

#[test]
fn cancelled_ordinary_wal_owner_never_resumes_the_successor() {
    let (turn, writer, old) = receipt_waiting_for_resume("ordinary-recovery-cancel-race");
    turn.state.resume_session_queue(&turn.session).unwrap();
    let write_ahead = writer.take(HeldFence::WriteAhead);
    let nonce = turn.record(|record| record.prompt_deliveries[&old].write_ahead.clone());
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let next = turn.enqueue("Successor remains paused.", QueuedPromptSource::User);
    turn.state
        .cancel_queued_prompt(&turn.session, &old)
        .unwrap();
    let before = turn.saved();
    // Call the exact production settlement owner synchronously after Cancel.
    // Resolving the original held fence afterwards is a stale duplicate.
    turn.state
        .settle_prompt_delivery_write_ahead(&turn.session, &old, &nonce, Ok(()));
    turn.assert_no_prompt();
    turn.record(|record| {
        assert_eq!(record.queued_prompts[0].pending_prompt.id, next);
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(
            !record.prompt_deliveries.contains_key(&old),
            "Cancel prunes the unsent receipt instead of retaining an owner for the successor"
        );
        assert!(
            !record
                .session
                .messages
                .iter()
                .any(|message| message.id() == old)
        );
    });
    assert_eq!(turn.saved(), before);
    write_ahead.finish(Ok(()));
    turn.assert_no_control();
    drop(writer);
}

#[test]
fn ordinary_resume_waits_for_the_existing_project_resource_fence() {
    let turn = OrdinaryRoot::new("ordinary-recovery-resource-fence");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let id = turn.enqueue("Same head after reset rollback.", QueuedPromptSource::User);
    let fence = turn
        .state
        .inner
        .lock()
        .unwrap()
        .engram_project_resets
        .claim(&turn.project)
        .unwrap();
    let before = turn.saved();
    assert_eq!(
        turn.state
            .resume_session_queue(&turn.session)
            .err()
            .expect("the live project reset must refuse Resume")
            .status,
        StatusCode::CONFLICT
    );
    assert_eq!(turn.saved(), before);
    turn.assert_no_prompt();
    assert!(
        turn.state
            .inner
            .lock()
            .unwrap()
            .engram_project_resets
            .release(&turn.project, fence)
    );
    turn.state.resume_session_queue(&turn.session).unwrap();
    assert!(
        turn.prompt()
            .prompt
            .contains("Same head after reset rollback.")
    );
    turn.record(|record| {
        assert_eq!(
            record
                .session
                .messages
                .iter()
                .filter(|message| message.id() == id)
                .count(),
            1
        )
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn historical_abort_settlement_roundtrip_never_constructs_a_retry_executor() {
    for acknowledged in [false, true] {
        let turn = OrdinaryRoot::new("ordinary-passive-abort-settlement");
        let history = json!({"prompt_id":"old-prompt","settled_grant_id":"old-grant",
            "acknowledged":acknowledged,"attempts":2,"future":[null,true]});
        turn.edit(|record| {
            record
                .engram
                .historical_control
                .insert("engramAbortRetry".into(), history.clone());
            record
                .engram
                .historical_control
                .insert("engramAbortRetryAcknowledged".into(), json!(acknowledged));
        });
        let loaded = turn.record(|record| {
            PersistedSessionRecord::from_record(record)
                .into_record()
                .unwrap()
        });
        assert_eq!(
            loaded.engram.historical_control["engramAbortRetry"],
            history
        );
        assert!(matches!(loaded.runtime, SessionRuntime::None));
        assert!(loaded.provider_capacity_retry.is_none());
        deliver_turn_dispatch(&turn.state, turn.prepare("Independent ordinary input.")).unwrap();
        turn.prompt();
        turn.assert_no_prompt();
        turn.record(|record| {
            assert_eq!(
                record.engram.historical_control["engramAbortRetry"],
                history
            )
        });
        turn.assert_no_control();
    }
}
