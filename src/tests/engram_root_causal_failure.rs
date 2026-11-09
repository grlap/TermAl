//! Historical causal diagnostics beside ordinary root queue ownership.
//! Cards below are synthetic passive specimens, not new control applications.
//! Real SQLite cases prove local committed bytes; no publication executor is recreated.
use super::super::uncertain_begin::OrdinaryRoot;
use super::*;

#[path = "engram_root_causal_publication.rs"]
mod causal_publication;

#[path = "engram_closing_checkpoint_retry_transient.rs"]
mod closing_checkpoint_retry_transient;

#[path = "engram_root_causal_rebind.rs"]
mod causal_rebind;

#[path = "engram_root_causal_value_free.rs"]
mod causal_value_free;

fn historical_card(cause: Option<EngramCausalFailure>) -> EngramControlCard {
    EngramControlCard {
        causal_failure: cause,
        schema_version: 1,
        stage: EngramControlStage::Checkpoint,
        assurance: "turn_gated".into(),
        decision: EngramControlCardDecision::Degraded,
        dispatch: EngramControlCardDispatch::Withheld,
        refusal_code: None,
        defer_code: None,
        grant_id: Some("historical-grant".into()),
        directives: Vec::new(),
        delivered_range: None,
        latency_ms: EngramControlLatencyCard::default(),
        fail_mode: EngramControlFailMode::Enforced,
        repair_armed: false,
        next_intent: None,
        source_root: None,
    }
}

fn save_card(turn: &OrdinaryRoot, card: EngramControlCard) -> String {
    let mut inner = turn.state.inner.lock().unwrap();
    let id = inner.next_message_id();
    inner
        .session_mut(&turn.session)
        .unwrap()
        .session
        .messages
        .push(Message::EngramControl {
            id: id.clone(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            card,
        });
    turn.state.commit_locked(&mut inner).unwrap();
    persist_locked_state_to_sqlite(&turn.state.persistence_path, &inner).unwrap();
    id
}

fn durable_card(turn: &OrdinaryRoot, id: &str) -> Value {
    let count = turn.record(|record| record.session.messages.len());
    load_persisted_message_range(&turn.state.persistence_path, &turn.session, 0, count)
        .unwrap()
        .into_iter()
        .find_map(|(_, message)| match message {
            Message::EngramControl {
                id: actual, card, ..
            } if actual == id => Some(serde_json::to_value(card).unwrap()),
            _ => None,
        })
        .expect("the actual SQLite transcript retained its original card")
}

#[test]
fn ordinary_completion_drains_fifo_without_replaying_or_overwriting_historical_causes() {
    for operation in ["turn_evaluate", "turn_begin", "turn_checkpoint"] {
        let turn = OrdinaryRoot::new("ordinary-historical-cause");
        let error = EngramTransportError::transport("historical response frame missing");
        let cause = EngramCausalFailure::error(&error, operation, Some("original-request"), &[]);
        let id = save_card(&turn, historical_card(Some(cause)));
        let original = durable_card(&turn, &id);
        deliver_turn_dispatch(&turn.state, turn.prepare("first ordinary input")).unwrap();
        turn.prompt();
        for text in ["FIFO second", "FIFO third"] {
            assert!(matches!(
                turn.state
                    .dispatch_turn(
                        &turn.session,
                        SendMessageRequest {
                            text: text.into(),
                            expanded_text: None,
                            attachments: Vec::new(),
                            source_session_id: None,
                            source_mailbox: None,
                        }
                    )
                    .unwrap(),
                DispatchTurnResult::Queued
            ));
        }
        let token = turn.record(|record| record.runtime.runtime_token().unwrap());
        for expected in ["FIFO second", "FIFO third"] {
            turn.state
                .finish_turn_ok_if_runtime_matches(&turn.session, &token)
                .unwrap();
            assert!(turn.prompt().prompt.contains(expected));
            turn.assert_no_prompt();
        }
        turn.state
            .finish_turn_ok_if_runtime_matches(&turn.session, &token)
            .unwrap();
        {
            let inner = turn.state.inner.lock().unwrap();
            persist_locked_state_to_sqlite(&turn.state.persistence_path, &inner).unwrap();
        }
        assert_eq!(durable_card(&turn, &id), original);
        turn.record(|record| assert!(record.queued_prompts.is_empty()));
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}

#[test]
fn ordinary_row_fence_reads_its_exact_queue_image_from_real_wal_snapshots() {
    let turn = OrdinaryRoot::new("ordinary-row-wal");
    turn.enqueue("exact own-row prompt", QueuedPromptSource::User);
    let target = turn.record(|record| PersistFenceTarget::OrdinarySessionRow {
        session_id: turn.session.clone(),
        content: ordinary_session_row_persisted_content(&PersistedSessionRecord::from_record(
            record,
        )),
    });
    {
        let inner = turn.state.inner.lock().unwrap();
        persist_locked_state_to_sqlite(&turn.state.persistence_path, &inner).unwrap();
    }
    let writer = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    writer.execute_batch("PRAGMA journal_mode=WAL").unwrap();
    let reader = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    assert!(target.is_already_durable(&reader).unwrap());
    let original: String = writer
        .query_row(
            "SELECT value_json FROM sessions WHERE id=?1",
            [&turn.session],
            |row| row.get(0),
        )
        .unwrap();
    let mut changed: Value = serde_json::from_str(&original).unwrap();
    changed["queuedPrompts"][0]["pending_prompt"]["text"] = json!("different own-row bytes");
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    writer
        .execute(
            "UPDATE sessions SET value_json=?1 WHERE id=?2",
            rusqlite::params![changed.to_string(), turn.session],
        )
        .unwrap();
    assert!(
        !target.is_already_durable(&writer).unwrap(),
        "a different own-row cannot satisfy its fence"
    );
    assert!(
        target.is_already_durable(&reader).unwrap(),
        "uncommitted sibling image is not published"
    );
    writer.execute_batch("ROLLBACK").unwrap();
    assert!(target.is_already_durable(&reader).unwrap());
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    writer
        .execute(
            "UPDATE sessions SET value_json=?1 WHERE id=?2",
            rusqlite::params![changed.to_string(), turn.session],
        )
        .unwrap();
    writer.execute_batch("COMMIT").unwrap();
    assert!(!target.is_already_durable(&reader).unwrap());
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_work_service_diagnostics_keep_not_started_distinct_from_unknown_application() {
    for (error, expected) in [
        (
            EngramTransportError::spawn_failed("fixture worker absent"),
            EngramRemoteApplication::NotStarted,
        ),
        (
            EngramTransportError::transport("fixture response lost"),
            EngramRemoteApplication::Unknown,
        ),
        (
            EngramTransportError::remote(EngramControlErrorBody {
                code: "access_denied".into(),
                message: "fixture refusal".into(),
            }),
            EngramRemoteApplication::Unknown,
        ),
    ] {
        let cause =
            EngramCausalFailure::error(&error, "execution_observe", Some("original-request"), &[]);
        assert_eq!(cause.remote_application, expected);
        assert_eq!(cause.attempt_id.as_deref(), Some("original-request"));
        assert_eq!(cause.operation, "execution_observe");
    }
}

#[test]
fn ordinary_historical_request_secrets_are_redacted_before_real_sqlite_storage() {
    let turn = OrdinaryRoot::new("ordinary-cause-redaction");
    let error = EngramTransportError::transport(
        "response frame missing capability-sentinel password = 'secret with spaces'",
    );
    let cause = EngramCausalFailure::error(
        &error,
        "historical_request",
        Some("original-attempt"),
        &["capability-sentinel"],
    );
    let id = save_card(&turn, historical_card(Some(cause)));
    let serialized = serde_json::to_string(&durable_card(&turn, &id)).unwrap();
    assert!(serialized.contains("response frame missing") && serialized.contains("redacted"));
    assert!(
        !serialized.contains("capability-sentinel") && !serialized.contains("secret with spaces")
    );
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_public_stop_keeps_historical_cause_and_owns_the_unsent_queue() {
    let turn = OrdinaryRoot::new("ordinary-cause-stop");
    let id = save_card(
        &turn,
        historical_card(Some(EngramCausalFailure::error(
            &EngramTransportError::transport("historical lost report"),
            "turn_checkpoint",
            Some("old-key"),
            &[],
        ))),
    );
    let original = durable_card(&turn, &id);
    turn.enqueue("unsent original", QueuedPromptSource::User);
    // No external provider is running. Exercise the actual public Stop claim
    // and its synchronous completion, without spawning a background interrupt.
    turn.edit(|record| {
        record.session.status = SessionStatus::Active;
        record.runtime = SessionRuntime::None;
    });
    let options = StopSessionOptions::default();
    let (_, claim) = turn
        .state
        .begin_requested_stop_session(&turn.session, &options)
        .unwrap();
    assert!(claim.is_some());
    turn.state
        .stop_local_session_with_options(&turn.session, options, claim)
        .unwrap();
    turn.record(|record| {
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(record.engram.operator_paused);
        assert_eq!(
            record.queued_prompts.front().unwrap().pending_prompt.text,
            "unsent original"
        );
    });
    assert_eq!(durable_card(&turn, &id), original);
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn historical_card_without_cause_loads_without_inventing_failure_details() {
    let wire = serde_json::to_value(historical_card(None)).unwrap();
    assert!(wire.get("causalFailure").is_none());
    let loaded: EngramControlCard = serde_json::from_value(wire).unwrap();
    assert!(loaded.causal_failure.is_none());
    assert!(
        serde_json::to_value(loaded)
            .unwrap()
            .get("causalFailure")
            .is_none()
    );
}

#[test]
fn ordinary_stale_completion_or_dispatch_cannot_decorate_or_consume_a_replacement() {
    for paused in [false, true] {
        let turn = OrdinaryRoot::new("ordinary-stale-cause");
        let dispatch = turn.prepare("superseded input");
        let token = turn.record(|record| record.runtime.runtime_token().unwrap());
        turn.edit(|record| {
            record.runtime = SessionRuntime::None;
            record.session.status = SessionStatus::Idle;
            if paused {
                record.set_auto_dispatch_blocked(true);
            }
        });
        turn.enqueue("independent replacement", QueuedPromptSource::User);
        let before = turn.saved();
        assert!(matches!(
            deliver_turn_dispatch(&turn.state, dispatch),
            TurnDispatchDeliveryOutcome::Superseded
        ));
        turn.state
            .finish_turn_ok_if_runtime_matches(&turn.session, &token)
            .unwrap();
        assert_eq!(turn.saved(), before);
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}

#[test]
fn root_causal_redaction_is_bounded_and_never_upgrades_remote_errors() {
    for text in [
        "EOF; password=hidden",
        "EOF; PASSWORD : 'hidden spaced'",
        "EOF; Authorization: Bearer hidden",
        "EOF; routing_token\":\"hidden\"",
        "EOF; secret = hidden",
        "EOF; token=hidden",
        "EOF; token   = 'hidden spaced'",
        "EOF; \"TOKEN\": \"hidden\"",
    ] {
        let redacted = engram_causal_text(text, &[]);
        assert!(redacted.starts_with("EOF; "));
        assert!(!redacted.contains("hidden"));
    }
    assert_eq!(
        engram_causal_text("EOF\u{1b}[lost] capability", &["capability"]),
        "EOF [lost] [redacted]"
    );
    assert!(engram_causal_text(&"ż".repeat(900), &[]).len() <= 1024);
    let remote = EngramTransportError::remote(EngramControlErrorBody {
        code: "control_unavailable".to_owned(),
        message: "worker stopped after write".to_owned(),
    });
    let cause = EngramCausalFailure::error(&remote, "turn_begin", None, &[]);
    assert_eq!(cause.remote_application, EngramRemoteApplication::Unknown);
    assert_eq!(cause.failure_class, EngramCausalFailureClass::Remote);
    assert!(cause.attempt_id.is_none());
}

#[test]
fn root_causal_local_preparation_preserves_error_but_not_prior_application_certainty() {
    let error =
        EngramTransportError::local_state("Failed to persist prepared evaluate: writer stopped");
    let cause = EngramCausalFailure::local(&error, "turn_evaluate", "Evaluate durable preparation");
    assert_eq!(cause.failure_class, EngramCausalFailureClass::LocalState);
    assert_eq!(
        cause.remote_application,
        EngramRemoteApplication::NotStarted
    );
    assert!(cause.message.contains("writer stopped"));
    let mut earlier = EngramTransportError::transport("lost reply to an earlier operation");
    earlier.causal_failure = Some(EngramCausalFailure::error(
        &earlier,
        "turn_checkpoint",
        Some("prior-key"),
        &[],
    ));
    let carried =
        EngramCausalFailure::local(&earlier, "turn_evaluate", "Evaluate durable preparation");
    assert_eq!(carried.operation, "turn_checkpoint");
    assert_eq!(carried.remote_application, EngramRemoteApplication::Unknown);
}
