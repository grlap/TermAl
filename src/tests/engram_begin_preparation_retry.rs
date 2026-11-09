//! Ordinary receipt WAL, explicit Resume, cancellation and late acknowledgements.
//! DeliveryWriter acknowledges without writing; these cases prove callback ordering,
//! not a SQLite durability image or the removed control preparation schedule.
use super::*;
use crate::tests::engram_host_adapter::uncertain_begin::OrdinaryRoot;
use crate::tests::prompt_delivery::{DeliveryWriter, HeldFence};
use axum::body::{Body, to_bytes};
use axum::http::Request;
use tower::ServiceExt;

fn wait_acknowledged(turn: &OrdinaryRoot, id: &str) {
    let guard = crate::tests::phase_sync::PollGuard::new();
    while !turn.record(|record| {
        record
            .queued_prompt_durability_holds
            .get(id)
            .is_some_and(|hold| hold.acknowledged)
    }) {
        guard.wait("the ordinary content durability acknowledgement");
    }
}

#[test]
fn ordinary_receipted_resume_waits_for_content_and_write_ahead_then_sends_once() {
    let mut turn = OrdinaryRoot::new("ordinary-receipt-wal");
    let writer = DeliveryWriter::attach(&mut turn.state);
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let id = turn.enqueue_durable("receipted ordinary prompt", true);
    assert!(
        turn.state
            .request_queued_prompt_durability(&turn.session, &id)
    );
    let content = writer.take(HeldFence::Durability);
    assert!(
        turn.state
            .resume_local_session_queue(&turn.session, None)
            .is_err()
    );
    turn.assert_no_prompt();
    content.finish(Ok(()));
    wait_acknowledged(&turn, &id);
    turn.assert_no_prompt();
    let outcome = turn
        .state
        .resume_local_session_queue(&turn.session, None)
        .unwrap();
    assert!(matches!(
        outcome,
        Some(TurnDispatchDeliveryOutcome::Scheduled)
    ));
    let write_ahead = writer.take(HeldFence::WriteAhead);
    let nonce = turn.record(|record| record.prompt_deliveries[&id].write_ahead.clone());
    assert!(matches!(
        turn.state
            .resume_local_session_queue(&turn.session, None)
            .unwrap(),
        Some(TurnDispatchDeliveryOutcome::Scheduled)
    ));
    assert_eq!(
        turn.record(|record| record.prompt_deliveries[&id].write_ahead.clone()),
        nonce,
        "a second Resume observes the same pending operation"
    );
    turn.assert_no_prompt();
    turn.record(|record| {
        assert_eq!(
            record.prompt_deliveries[&id].disposition,
            PromptDeliveryDisposition::HandingOff
        );
        assert!(record.prompt_deliveries[&id].turn_generation.is_none());
    });
    write_ahead.finish(Ok(()));
    let command = turn.prompt();
    assert!(command.prompt.contains("receipted ordinary prompt"));
    assert!(command.delivery_receipt.is_some());
    turn.record(|record| {
        assert!(
            record.queued_prompts.is_empty()
                || record
                    .queued_prompts
                    .iter()
                    .all(|entry| entry.promoted_message_index.is_some())
        );
        assert_eq!(
            record.prompt_deliveries[&id].turn_generation,
            Some(command.active_turn_generation)
        );
        assert_eq!(record.prompt_deliveries[&id].write_ahead, nonce);
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
    drop(writer);
}

#[test]
fn ordinary_failed_writer_keeps_the_exact_content_and_actionable_resume_hold() {
    let mut turn = OrdinaryRoot::new("ordinary-writer-stopped");
    let writer = DeliveryWriter::attach(&mut turn.state);
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let id = turn.enqueue_durable("exact unsent ordinary content", false);
    let before = turn.record(|record| serde_json::to_value(&record.queued_prompts[0]).unwrap());
    assert!(
        turn.state
            .request_queued_prompt_durability(&turn.session, &id)
    );
    writer
        .take(HeldFence::Durability)
        .finish(Err(PersistFenceError::WorkerStopped));
    let guard = crate::tests::phase_sync::PollGuard::new();
    while !turn.record(|record| record.queued_prompt_durability_holds[&id].failure.is_some()) {
        guard.wait("the stopped writer's actual failed acknowledgement");
    }
    let error = turn
        .state
        .resume_local_session_queue(&turn.session, None)
        .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("WorkerStopped"), "{}", error.message);
    turn.record(|record| {
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            before
        );
        assert!(!record.queued_prompt_durability_holds[&id].acknowledged);
        assert!(record.orchestrator_auto_dispatch_blocked);
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
    drop(writer);
}

#[test]
fn ordinary_late_content_ack_after_cancel_never_releases_or_mutates_the_successor() {
    let turn = OrdinaryRoot::new("ordinary-late-ack-cancel");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let old = turn.enqueue_durable("cancelled before acknowledgement", false);
    let nonce = turn.record(|record| record.queued_prompt_durability_holds[&old].nonce.clone());
    let next = turn.enqueue_durable("independent successor", false);
    turn.state
        .cancel_queued_prompt(&turn.session, &old)
        .unwrap();
    let before = turn.saved();
    turn.state
        .settle_queued_prompt_durability(&turn.session, &old, &nonce, Ok(()));
    assert_eq!(turn.saved(), before);
    turn.record(|record| {
        assert_eq!(record.queued_prompts[0].pending_prompt.id, next);
        assert!(!record.queued_prompt_durability_holds[&next].acknowledged);
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_acknowledgement_cannot_cross_a_changed_content_identity() {
    let mut turn = OrdinaryRoot::new("ordinary-changed-wal-content");
    let writer = DeliveryWriter::attach(&mut turn.state);
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let id = turn.enqueue_durable("original bytes", false);
    let old_nonce = turn.record(|record| record.queued_prompt_durability_holds[&id].nonce.clone());
    turn.edit(|record| record.queued_prompts[0].pending_prompt.text = "changed bytes".into());
    turn.state
        .settle_queued_prompt_durability(&turn.session, &id, &old_nonce, Ok(()));
    let newer = writer.take(HeldFence::Durability);
    turn.record(|record| {
        let hold = &record.queued_prompt_durability_holds[&id];
        assert_ne!(hold.nonce, old_nonce);
        assert!(!hold.acknowledged);
        assert_eq!(hold.content["pending_prompt"]["text"], "changed bytes");
    });
    turn.assert_no_prompt();
    newer.finish(Err(PersistFenceError::WorkerStopped));
    let guard = crate::tests::phase_sync::PollGuard::new();
    while !turn.record(|record| record.queued_prompt_durability_holds[&id].failure.is_some()) {
        guard.wait("the changed content's failed acknowledgement settles before teardown");
    }
    drop(writer);
    turn.assert_no_control();
}

#[tokio::test]
async fn ordinary_human_resume_checks_the_selected_head_and_delivers_its_content() {
    let turn = OrdinaryRoot::new("ordinary-human-resume");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let id = turn.enqueue("selected ordinary content", QueuedPromptSource::User);
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
                    json!({"origin":"human", "humanChoice":selection}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert!(turn.prompt().prompt.contains("selected ordinary content"));
    turn.record(|record| {
        assert!(
            !record
                .queued_prompts
                .iter()
                .any(|entry| entry.pending_prompt.id == id)
        );
        assert!(record.human_queue_choices.is_empty());
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}
