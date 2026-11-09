//! Ordinary queue/delivery ownership beside immutable historical metadata.
//! The old containment admission and its recovery RPCs have no executor here.
use super::super::uncertain_begin::OrdinaryRoot;
use super::*;

#[path = "engram_project_containment_begin_replay.rs"]
mod begin_replay;
#[path = "engram_project_containment_bind_successor.rs"]
mod bind_successor;
#[path = "engram_project_containment_cycles.rs"]
mod cycles;
#[path = "engram_project_containment_decision_persistence.rs"]
mod decision_persistence;
#[path = "engram_project_containment_parked_retry.rs"]
mod parked_retry;
#[path = "engram_project_containment_transfer_settings.rs"]
mod transfer_settings;

// Reuse the shared ordinary runtime and historical-entry fixtures. This only
// installs passive imported data; it creates no grant/replay/queue authority.
fn ordinary_with_history(label: &str) -> (OrdinaryRoot, Value) {
    let turn = OrdinaryRoot::new(label);
    let mut original = super::obligations::entry(&turn.root);
    original.project_id = turn.project.clone();
    original.local_session_id = turn.session.clone();
    original.connection.session_id = turn.session.clone();
    original.external_ref = format!("termal:session:{}", turn.session);
    let ledger = EngramSessionObligationLedger {
        schema_version: ENGRAM_SESSION_OBLIGATION_LEDGER_VERSION,
        entries: vec![original],
        checkpoint_operations: Vec::new(),
    };
    ledger.validate().unwrap();
    let expected = serde_json::to_value(&ledger).unwrap();
    {
        let mut inner = turn.state.inner.lock().unwrap();
        inner.engram_session_obligations = ledger;
        turn.state.commit_locked(&mut inner).unwrap();
    }
    (turn, expected)
}

fn assert_history(turn: &OrdinaryRoot, expected: &Value) {
    assert_eq!(
        serde_json::to_value(&turn.state.inner.lock().unwrap().engram_session_obligations).unwrap(),
        *expected
    );
    let loaded = load_state(turn.state.persistence_path.as_ref())
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&loaded.engram_session_obligations).unwrap(),
        *expected,
        "original metadata and request bytes stay passive through SQLite"
    );
    turn.assert_no_control();
}

fn observation_settings(turn: &OrdinaryRoot) -> EngramProjectSettings {
    turn.state
        .inner
        .lock()
        .unwrap()
        .find_project(&turn.project)
        .unwrap()
        .engram
        .clone()
        .unwrap()
}

fn persist_fixture(turn: &OrdinaryRoot) {
    let inner = turn.state.inner.lock().unwrap();
    persist_locked_state_to_sqlite(turn.state.persistence_path.as_ref(), &inner).unwrap();
}

fn reload_fixture(turn: &OrdinaryRoot) {
    turn.state.shutdown_persist_blocking();
    let runtime = turn.record(|record| record.runtime.clone());
    let mut loaded = load_state(turn.state.persistence_path.as_ref())
        .unwrap()
        .unwrap();
    let record = loaded.session_mut(&turn.session).unwrap();
    // Reattach only the isolated live provider fixture. No persisted receipt
    // acknowledgement or former control-session authority is restored.
    record.runtime = runtime;
    record.engram.context_nudge_pending = false;
    *turn.state.inner.lock().unwrap() = loaded;
    turn.state
        .install_test_engram_transport(turn.transport.clone());
}

fn row_fence(turn: &OrdinaryRoot) -> PersistFenceTarget {
    let content = turn.record(|record| {
        ordinary_session_row_persisted_content(&PersistedSessionRecord::from_record(record))
    });
    PersistFenceTarget::OrdinarySessionRow {
        session_id: turn.session.clone(),
        content,
    }
}

// Seed a current receipt specimen through the existing queue constructor.
// The handing-off state is synthetic; only owner/fence behavior is tested.
fn fixture_handoff_receipt(turn: &OrdinaryRoot, text: &str) -> (String, String) {
    let id = turn.enqueue_durable(text, true);
    let nonce = "fixture-original-write-ahead".to_owned();
    turn.edit(|record| {
        let delivery = record.prompt_deliveries.get_mut(&id).unwrap();
        delivery.disposition = PromptDeliveryDisposition::HandingOff;
        delivery.attempt = 1;
        delivery.write_ahead = nonce.clone();
        delivery.write_ahead_acknowledged = false;
        delivery.turn_generation = None;
        delivery.resume = None;
    });
    (id, nonce)
}

#[test]
fn ordinary_history_never_gates_resume_or_consumes_the_fifo_successor() {
    for cancelled in [false, true] {
        for reload in [false, true] {
            let (turn, history) = ordinary_with_history("history-fifo-resume");
            turn.edit(|record| record.set_auto_dispatch_blocked(true));
            let first = turn.enqueue("first ordinary content", QueuedPromptSource::User);
            let second = turn.enqueue("second ordinary content", QueuedPromptSource::User);
            if cancelled {
                turn.state
                    .cancel_queued_prompt(&turn.session, &first)
                    .unwrap();
            }
            if reload {
                reload_fixture(&turn);
            }
            turn.state.resume_session_queue(&turn.session).unwrap();
            let prompt = turn.prompt();
            assert!(prompt.prompt.contains(if cancelled {
                "second ordinary content"
            } else {
                "first ordinary content"
            }));
            if !cancelled {
                assert!(!prompt.prompt.contains("second ordinary content"));
                turn.record(|record| {
                    assert!(
                        record
                            .queued_prompts
                            .iter()
                            .any(|entry| entry.pending_prompt.id == second)
                    )
                });
            }
            let queued_before = turn.saved()["queuedPrompts"].clone();
            let generation = turn.record(|record| {
                assert_eq!(record.session.status, SessionStatus::Active);
                record.active_turn_generation
            });
            let error = turn
                .state
                .resume_session_queue(&turn.session)
                .err()
                .expect("a repeated Resume must refuse while the turn is busy");
            assert_eq!(error.status, StatusCode::CONFLICT);
            assert!(error.message.contains("busy"), "{error:?}");
            turn.assert_no_prompt();
            assert_eq!(
                turn.saved()["queuedPrompts"],
                queued_before,
                "busy Resume neither consumes nor changes the FIFO successor"
            );
            turn.record(|record| {
                assert_eq!(record.session.status, SessionStatus::Active);
                assert_eq!(record.active_turn_generation, generation);
            });
            assert_history(&turn, &history);
        }
    }
}

#[tokio::test]
async fn ordinary_history_does_not_require_control_for_exact_ui_selected_resume() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let (turn, history) = ordinary_with_history("history-ui-resume");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    turn.enqueue("UI selected ordinary prompt", QueuedPromptSource::Human);
    turn.enqueue("later FIFO prompt", QueuedPromptSource::Human);
    let selection = turn.record(|record| HumanPromptSelection {
        prompt: projected_pending_prompts(record).remove(0),
        session_mutation_stamp: record.mutation_stamp,
    });
    let response = app_router(turn.state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{}/queue/resume", turn.session))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&ResumeQueueRequest {
                        origin: Some(ExplicitPromptOrigin::Human),
                        human_choice: Some(selection),
                    })
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let prompt = turn.prompt();
    assert!(prompt.prompt.contains("UI selected ordinary prompt"));
    assert!(!prompt.prompt.contains("later FIFO prompt"));
    turn.assert_no_prompt();
    assert_history(&turn, &history);
}

#[test]
fn ordinary_unknown_provider_handoff_is_not_replayed_by_history_or_resume() {
    let (turn, history) = ordinary_with_history("history-unknown-handoff");
    let first = turn.enqueue("possibly delivered", QueuedPromptSource::User);
    let second = turn.enqueue("untouched successor", QueuedPromptSource::User);
    turn.edit(|record| {
        record.queued_prompts[0].engram_ordinary_handoff = true;
        record.queued_prompts[0].promotion_disposition_known = false;
        record.set_auto_dispatch_blocked(true);
    });
    let original = turn.saved()["queuedPrompts"][0].clone();
    turn.state.resume_session_queue(&turn.session).unwrap();
    let prompt = turn.prompt();
    assert!(prompt.prompt.contains("untouched successor"));
    assert!(!prompt.prompt.contains("possibly delivered"));
    turn.assert_no_prompt();
    turn.record(|record| {
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(record.queued_prompts[0].pending_prompt.id, first);
        assert!(
            !record
                .queued_prompts
                .iter()
                .any(|row| row.pending_prompt.id == second)
        );
        assert!(record.queued_prompts[0].engram_ordinary_handoff);
    });
    assert_eq!(turn.saved()["queuedPrompts"][0], original);
    assert_history(&turn, &history);
}

#[test]
fn ordinary_stale_dispatch_does_not_deliver_after_provider_or_turn_replacement() {
    for change in ["provider", "turn"] {
        let (turn, history) = ordinary_with_history("history-stale-handoff");
        let dispatch = turn.prepare("old unsent input");
        turn.edit(|record| {
            if change == "provider" {
                record.runtime = SessionRuntime::None;
            } else {
                record.active_turn_generation += 1;
            }
        });
        let before = turn.saved();
        assert!(matches!(
            deliver_turn_dispatch(&turn.state, dispatch),
            TurnDispatchDeliveryOutcome::Superseded
        ));
        assert_eq!(turn.saved(), before);
        turn.assert_no_prompt();
        assert_history(&turn, &history);
    }
}

#[test]
fn ordinary_settings_and_public_stop_preserve_the_real_stop_owner_and_passive_history() {
    let (turn, history) = ordinary_with_history("history-stop-owner");
    turn.edit(|record| {
        record.runtime = SessionRuntime::None;
        record.session.status = SessionStatus::Active;
    });
    let options = StopSessionOptions::default();
    let (_, owner) = turn
        .state
        .begin_requested_stop_session(&turn.session, &options)
        .unwrap();
    let owner = owner.expect("ordinary Stop owns cleanup even with a missing runtime");
    let generation = turn.record(|record| record.runtime_stop_generation);
    let mut settings = observation_settings(&turn);
    settings.execution_observability = true;
    turn.state
        .update_project_engram_settings(&turn.project, settings)
        .unwrap();
    turn.record(|record| {
        assert!(record.runtime_stop_in_progress);
        assert_eq!(record.runtime_stop_generation, generation);
        assert_eq!(record.session.status, SessionStatus::Stopping);
    });
    turn.state
        .stop_local_session_with_options(&turn.session, options, Some(owner))
        .unwrap();
    turn.record(|record| {
        assert!(!record.runtime_stop_in_progress);
        assert!(record.engram.operator_paused && record.orchestrator_auto_dispatch_blocked);
    });
    turn.assert_no_prompt();
    assert_history(&turn, &history);
}
