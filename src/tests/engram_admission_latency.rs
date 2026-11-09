//! Historical admission measurements stay readable without a scheduling executor.
//! Synthetic cards are passive specimens. SQLite cases below use the real store;
//! ordinary Resume, pause and FIFO assertions use the existing runtime owners.
use super::*;
use crate::tests::engram_host_adapter::uncertain_begin::OrdinaryRoot;

fn historical_latency_card(measured: bool) -> EngramControlCard {
    let error = EngramTransportError::transport("historical response unavailable");
    let mut cause =
        EngramCausalFailure::error(&error, "turn_evaluate", Some("original-attempt"), &[]);
    cause.failure_class = EngramCausalFailureClass::LocalState;
    cause.original_code = Some("dispatch_budget_exhausted".into());
    cause.continuation_id = Some("original-head".into());
    cause.turn_generation = Some(17);
    cause.authority_fingerprint = Some("original-authority".into());
    cause.continuation_reason = "Historical measured admission; no automatic re-evaluation.".into();
    EngramControlCard {
        causal_failure: Some(cause),
        schema_version: 1,
        stage: EngramControlStage::Dispatch,
        assurance: "turn_gated".into(),
        decision: EngramControlCardDecision::Refuse,
        dispatch: EngramControlCardDispatch::Withheld,
        refusal_code: Some("dispatch_budget_exhausted".into()),
        defer_code: None,
        grant_id: None,
        directives: Vec::new(),
        delivered_range: None,
        latency_ms: EngramControlLatencyCard {
            evaluate: measured.then_some(18_248),
            admission_fence: measured.then_some(12_248),
            total: 25_315,
            ..EngramControlLatencyCard::default()
        },
        fail_mode: EngramControlFailMode::Enforced,
        repair_armed: false,
        next_intent: None,
        source_root: None,
    }
}

fn store_historical_card(turn: &OrdinaryRoot, card: EngramControlCard) -> String {
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
    // The fixture's real SQLite store supplies this image, not DeliveryWriter.
    persist_locked_state_to_sqlite(&turn.state.persistence_path, &inner).unwrap();
    id
}

fn stored_historical_card(turn: &OrdinaryRoot, id: &str) -> EngramControlCard {
    let count = turn.record(|record| record.session.messages.len());
    load_persisted_message_range(&turn.state.persistence_path, &turn.session, 0, count)
        .unwrap()
        .into_iter()
        .find_map(|(_, message)| match message {
            Message::EngramControl {
                id: actual, card, ..
            } if actual == id => Some(card),
            _ => None,
        })
        .expect("the original latency card read from SQLite")
}

#[test]
fn historical_latency_spans_and_missing_measurements_round_trip_without_double_counting() {
    for measured in [false, true] {
        let turn = OrdinaryRoot::new("historical-latency-sqlite");
        let original = historical_latency_card(measured);
        let id = store_historical_card(&turn, original.clone());
        let loaded = stored_historical_card(&turn, &id);
        assert_eq!(loaded, original);
        assert_eq!(loaded.latency_ms.total, 25_315);
        assert_eq!(loaded.latency_ms.evaluate, measured.then_some(18_248));
        assert_eq!(
            loaded.latency_ms.admission_fence,
            measured.then_some(12_248)
        );
        let value = serde_json::to_value(&loaded).unwrap();
        assert_eq!(value["latencyMs"]["total"], 25_315);
        assert_eq!(value["latencyMs"].get("evaluate").is_some(), measured);
        assert_eq!(value["latencyMs"].get("admissionFence").is_some(), measured);
        assert_eq!(
            value["causalFailure"]["controlProcess"],
            json!({"identity":"unavailable", "exitState":"unavailable"})
        );
        // No removed latency-hold formatter is recreated, and no new elapsed
        // cost is attributed to this synthetic saved specimen.
        let restored = turn.record(|record| {
            PersistedSessionRecord::from_record(record)
                .into_record()
                .unwrap()
        });
        let card = restored
            .session
            .messages
            .iter()
            .find_map(|message| match message {
                Message::EngramControl {
                    id: actual, card, ..
                } if actual == &id => Some(card),
                _ => None,
            })
            .unwrap();
        assert_eq!(card, &original);
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}

#[test]
fn historical_latency_or_stale_attribution_never_decides_the_current_fifo() {
    for variant in [
        "exhausted",
        "grant",
        "grantless",
        "transport",
        "other_code",
        "within_budget",
        "other_prompt",
        "other_generation",
        "other_authority",
        "unattributed",
        "checkpoint",
    ] {
        let turn = OrdinaryRoot::new("historical-latency-ordinary-fifo");
        let mut card = historical_latency_card(true);
        match variant {
            "exhausted" => {}
            "grant" => card.decision = EngramControlCardDecision::Grant,
            "grantless" => card.dispatch = EngramControlCardDispatch::SentWithoutGrant,
            "transport" => {
                card.causal_failure.as_mut().unwrap().failure_class =
                    EngramCausalFailureClass::Transport
            }
            "other_code" => card.refusal_code = Some("control_unavailable".into()),
            "within_budget" => card.latency_ms.total = 1,
            "other_prompt" => {
                card.causal_failure.as_mut().unwrap().continuation_id = Some("another-head".into())
            }
            "other_generation" => {
                card.causal_failure.as_mut().unwrap().turn_generation = Some(u64::MAX)
            }
            "other_authority" => {
                card.causal_failure.as_mut().unwrap().authority_fingerprint =
                    Some("another-authority".into())
            }
            "unattributed" => {
                let cause = card.causal_failure.as_mut().unwrap();
                cause.continuation_id = None;
                cause.turn_generation = None;
                cause.authority_fingerprint = None;
            }
            "checkpoint" => card.stage = EngramControlStage::Checkpoint,
            _ => unreachable!(),
        }
        let id = store_historical_card(&turn, card.clone());
        turn.edit(|record| record.set_auto_dispatch_blocked(true));
        let first = turn.enqueue("first ordinary input", QueuedPromptSource::User);
        let second = turn.enqueue("second ordinary input", QueuedPromptSource::User);
        let held = turn.saved();
        assert!(
            turn.state
                .dispatch_next_queued_turn(&turn.session, false)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            turn.saved(),
            held,
            "{variant}: history cannot lend a pause bypass"
        );
        turn.assert_no_prompt();
        turn.state
            .resume_local_session_queue(&turn.session, None)
            .unwrap();
        let command = turn.prompt();
        assert!(command.prompt.contains("first ordinary input"));
        assert!(!command.prompt.contains("second ordinary input"));
        let token = turn.record(|record| {
            assert_eq!(
                record.queued_prompts.front().unwrap().pending_prompt.id,
                second
            );
            assert!(
                !record
                    .queued_prompts
                    .iter()
                    .any(|row| row.pending_prompt.id == first)
            );
            record.runtime.runtime_token().unwrap()
        });
        turn.assert_no_prompt();
        turn.state
            .finish_turn_ok_if_runtime_matches(&turn.session, &token)
            .unwrap();
        assert!(turn.prompt().prompt.contains("second ordinary input"));
        turn.assert_no_prompt();
        turn.state
            .finish_turn_ok_if_runtime_matches(&turn.session, &token)
            .unwrap();
        turn.record(|record| {
            assert_eq!(record.session.status, SessionStatus::Idle);
            assert!(record.queued_prompts.is_empty());
        });
        {
            let inner = turn.state.inner.lock().unwrap();
            persist_locked_state_to_sqlite(&turn.state.persistence_path, &inner).unwrap();
        }
        assert_eq!(
            stored_historical_card(&turn, &id),
            card,
            "{variant}: ordinary completion does not rewrite historical diagnostics"
        );
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}

#[test]
fn historical_admission_window_bytes_never_schedule_or_renew_a_paused_ordinary_head() {
    let turn = OrdinaryRoot::new("historical-admission-window");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let id = turn.enqueue(
        "exact ordinary input after operator Resume",
        QueuedPromptSource::User,
    );
    let history = json!({
        "engramAdmissionRetry":{"code":"dispatch_budget_exhausted", "promptId":id,
            "dueAt":"2026-10-01T01:02:03Z", "future":[null,"żółw"]},
        "engramBeginPreparationLineage":{"promptId":id, "admittedAt":"2026-10-01T01:00:00Z",
            "deadline":"2026-10-01T01:02:00Z", "ended":true},
        "engramDispatchGeneration":17
    });
    turn.edit(|record| {
        for (key, value) in history.as_object().unwrap() {
            record
                .engram
                .historical_control
                .insert(key.clone(), value.clone());
        }
    });
    let head = turn.record(|record| serde_json::to_value(&record.queued_prompts[0]).unwrap());
    for _ in 0..3 {
        // Service the actual ordinary queue. There is no admission retry tick,
        // reconstructed Begin, or old budget whose deadline grants release.
        assert!(
            turn.state
                .dispatch_next_queued_turn(&turn.session, false)
                .unwrap()
                .is_none()
        );
        turn.record(|record| {
            assert_eq!(
                serde_json::to_value(&record.queued_prompts[0]).unwrap(),
                head
            );
            assert_eq!(
                serde_json::to_value(&record.engram.historical_control).unwrap(),
                history
            );
            assert!(record.orchestrator_auto_dispatch_blocked);
        });
        turn.assert_no_prompt();
    }
    let loaded = turn.record(|record| {
        PersistedSessionRecord::from_record(record)
            .into_record()
            .unwrap()
    });
    assert!(
        loaded.orchestrator_auto_dispatch_blocked,
        "unproven/operator pause is not released merely because old window bytes exist"
    );
    assert_eq!(
        serde_json::to_value(&loaded.engram.historical_control).unwrap(),
        history
    );
    turn.state
        .resume_local_session_queue(&turn.session, None)
        .unwrap();
    assert!(
        turn.prompt()
            .prompt
            .contains("exact ordinary input after operator Resume")
    );
    turn.record(|record| {
        assert_eq!(
            serde_json::to_value(&record.engram.historical_control).unwrap(),
            history
        )
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}
