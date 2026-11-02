//! Ordinary root dispatch, queue delivery, completion and historical data.
//! Reuses the shared local runtime owner; no control admission is recreated.
use super::*;
use super::uncertain_begin::OrdinaryRoot;

#[path = "off_disables_gated_control.rs"]
mod off_disables_gated_control;

#[path = "engram_admission_availability_contract.rs"]
mod admission_availability_contract;

#[path = "engram_admission_deadline_recovery.rs"]
mod admission_deadline_recovery;

#[path = "engram_root_causal_failure.rs"]
mod causal_failure;

#[path = "engram_root_recovery_corrections.rs"]
mod recovery_corrections;

#[path = "engram_refusal_healing_rebind.rs"]
mod refusal_healing_rebind;

#[path = "engram_post_receipt.rs"]
mod post_receipt;

#[path = "engram_retained_disposition.rs"]
mod retained_disposition;

#[path = "engram_abort_retry.rs"]
mod abort_retry;

#[path = "engram_bind_backoff_retry.rs"]
mod bind_backoff_retry;

#[path = "engram_admission_retry.rs"]
mod admission_retry;

#[path = "engram_acknowledged_mailbox_wake.rs"]
mod acknowledged_mailbox_wake;

#[path = "engram_mailbox_continuation_coverage.rs"]
mod mailbox_continuation_coverage;

#[path = "engram_delegation_held.rs"]
mod delegation_held;

#[path = "engram_receipted_admission.rs"]
mod receipted_admission;

#[path = "engram_capacity_retry_admission.rs"]
mod capacity_retry_admission;

#[path = "engram_project_reset_wait.rs"]
mod project_reset_wait;


// Data only: opaque saved queue metadata is never decoded as an executable
// control request or used to authorize the provider handoff.
fn passive_evaluate_bytes(with_begin: bool, effects: Value) -> Value {
    json!({
        "connection":{"session":"historical-connection"},
        "settings":{"turnGatedControl":true,"unknown":[null,"żółw"]},
        "request":{"operation":"turn_evaluate","routing_token":"historical-token",
            "idempotency_key":"original-unknown-evaluate","intent_fingerprint":"original-intent",
            "purpose":"original task","requested_effects":effects,"resource_intents":[]},
        "operation_generation":17,"begun_grant_id":null,
        "prepared_begin":if with_begin { json!({"idempotency_key":"original-unknown-begin",
            "grant_id":"historical-grant","delivery_tokens":["original-delivery"],
            "issued_no_later_than":"2026-10-01T01:02:03Z","phase":"prepared","delivered_range":null}) } else { Value::Null }
    })
}

#[test]
fn root_ordinary_dispatch_does_not_consult_a_scripted_control_defer() {
    let turn = OrdinaryRoot::new("ordinary-root-unused-defer");
    let replies = [
        json!({"routing_token":"root-token"}),
        json!({"decision":"defer","reason":"busy"}),
        json!({"decision":"grant","grant_id":"unused-grant"}),
        json!({"decision":"begin","receipt":{"grant_id":"unused-grant"}}),
    ];
    turn.transport.responses.lock().unwrap().extend(replies.into_iter()
        .map(|reply| ScriptedEngramControlResponse::Reply(Ok(reply))));
    let dispatch = turn.prepare("Ordinary input ignores old control replies.");
    deliver_turn_dispatch(&turn.state, dispatch).unwrap();
    let prompt = turn.prompt();
    assert!(prompt.prompt.contains("Ordinary input ignores old control replies."));
    turn.assert_no_prompt();
    turn.assert_no_control();
    assert_eq!(turn.transport.responses.lock().unwrap().len(), 4);
    turn.record(|record| {
        assert!(record.queued_prompts.is_empty());
        assert_eq!(record.session.status, SessionStatus::Active);
    });
}

#[test]
fn root_ordinary_successor_preserves_unknown_evaluate_bytes_without_replaying_them() {
    let turn = OrdinaryRoot::new("ordinary-root-unknown-evaluate");
    let id = turn.enqueue("original uncertain provider attempt", QueuedPromptSource::User);
    let expected = turn.edit(|record| {
        let head = record.queued_prompts.front_mut().unwrap();
        head.promotion_disposition_known = false;
        head.engram_ordinary_handoff = true;
        head.engram_interrupted = true;
        head.engram_evaluate = Some(passive_evaluate_bytes(false, json!(["observe"])));
        record.engram.historical_control.insert("engramDispatchGeneration".into(), json!(17));
        serde_json::to_value(&record.queued_prompts[0]).unwrap()
    });
    deliver_turn_dispatch(&turn.state, turn.prepare("Independent ordinary successor.")).unwrap();
    assert!(turn.prompt().prompt.contains("Independent ordinary successor."));
    turn.assert_no_prompt();
    turn.assert_no_control();
    turn.record(|record| {
        assert_eq!(record.engram.historical_control["engramDispatchGeneration"], 17);
        let retained = record.queued_prompts.iter().find(|row| row.pending_prompt.id == id).unwrap();
        assert_eq!(serde_json::to_value(retained).unwrap(), expected);
        assert!(!record.session.messages.iter().any(|message| message.id() == id));
    });
}

#[test]
fn root_ordinary_send_preserves_passive_unknown_binding_bytes_without_replay() {
    let turn = OrdinaryRoot::new("ordinary-root-passive-bind");
    let original = json!({"request":{"operation":"session_bind",
        "external_ref":format!("termal:{}", turn.session),"title":"original binding",
        "assurance":"turn_gated","mediated_effects":["observe"],"capability_map_revision":1,
        "idempotency_key":"original-unknown-bind","work_binding":null},
        "connection":{"session":"original-connection"},"future":[null,true,{"text":"żółw"}]});
    turn.edit(|record| {
        record.engram.historical_control.insert("engramRetainedBind".into(), original.clone());
    });
    deliver_turn_dispatch(&turn.state, turn.prepare("Ordinary send beside passive history.")).unwrap();
    turn.prompt();
    turn.assert_no_prompt();
    turn.assert_no_control();
    turn.record(|record| assert_eq!(record.engram.historical_control["engramRetainedBind"], original));
    let saved = turn.saved();
    assert_eq!(saved["engramRetainedBind"], original,
        "saved historical data stays readable, without rebuilding the deleted process-local bind owner");
}

#[test]
fn root_default_work_service_call_budget_is_twenty_seconds() {
    assert_eq!(EngramProjectSettings::default().call_timeout(), Duration::from_secs(20));
}

#[test]
fn a_project_may_bound_an_engram_call_up_to_twenty_seconds() {
    let state = test_app_state();
    let update = |deadline_ms| state.update_project_engram_settings("no-such-project",
        EngramProjectSettings { deadline_ms: Some(deadline_ms), ..Default::default() })
        .err().expect("an unknown project is never updated");
    for refused in [0, 20_001] {
        let error = update(refused);
        assert_eq!(error.status, StatusCode::BAD_REQUEST, "{refused}");
        assert!(error.message.contains("Engram deadline must be between 1 and 20000 ms"),
            "{refused}: {}", error.message);
    }
    let error = update(20_000);
    assert_eq!(error.status, StatusCode::NOT_FOUND, "{}", error.message);
    assert!(error.message.contains("project not found"), "{}", error.message);
}

#[test]
fn root_unknown_prepared_begin_bytes_stay_passive_beside_a_fresh_ordinary_turn() {
    let turn = OrdinaryRoot::new("ordinary-root-unknown-begin");
    let id = turn.enqueue("original uncertain provider attempt", QueuedPromptSource::User);
    let expected = turn.edit(|record| {
        let head = record.queued_prompts.front_mut().unwrap();
        head.promotion_disposition_known = false;
        head.engram_ordinary_handoff = true;
        head.engram_interrupted = true;
        head.engram_evaluate = Some(passive_evaluate_bytes(true, json!(["observe"])));
        record.engram.historical_control.insert("engramDispatchGeneration".into(), json!(17));
        serde_json::to_value(&record.queued_prompts[0]).unwrap()
    });
    deliver_turn_dispatch(&turn.state, turn.prepare("Fresh ordinary input.")).unwrap();
    turn.prompt();
    turn.assert_no_prompt();
    turn.record(|record| {
        assert_eq!(record.engram.historical_control["engramDispatchGeneration"], 17);
        let retained = record.queued_prompts.iter().find(|row| row.pending_prompt.id == id).unwrap();
        assert_eq!(serde_json::to_value(retained).unwrap(), expected);
        assert!(retained.engram_ordinary_handoff);
    });
    turn.assert_no_control();
}

#[test]
fn root_reload_keeps_begun_history_without_reconstructing_a_control_executor() {
    let turn = OrdinaryRoot::new("ordinary-root-begun-history");
    let id = turn.enqueue("original uncertain provider attempt", QueuedPromptSource::User);
    let expected = turn.edit(|record| {
        let head = record.queued_prompts.front_mut().unwrap();
        head.promotion_disposition_known = false;
        head.engram_ordinary_handoff = true;
        let mut raw = passive_evaluate_bytes(true, json!(["observe"]));
        raw["begun_grant_id"] = json!("historical-grant");
        head.engram_evaluate = Some(raw);
        record.engram.historical_control.insert("engramDispatchGeneration".into(), json!(17));
        serde_json::to_value(&record.queued_prompts[0]).unwrap()
    });
    let loaded = turn.record(|record| PersistedSessionRecord::from_record(record).into_record().unwrap());
    let row = loaded.queued_prompts.iter().find(|row| row.pending_prompt.id == id).unwrap();
    // Complete transcript residency resolves the legacy knowledge bit; the
    // original handoff and exact saved operation remain non-replayable.
    let mut expected = expected;
    expected["promotion_disposition_known"] = json!(true);
    assert_eq!(serde_json::to_value(row).unwrap(), expected);
    assert!(row.promotion_disposition_known && row.engram_ordinary_handoff);
    assert_eq!(row.engram_evaluate.as_ref().unwrap()["begun_grant_id"], "historical-grant");
    assert_eq!(row.engram_evaluate.as_ref().unwrap()["prepared_begin"]["idempotency_key"], "original-unknown-begin");
    assert_eq!(loaded.engram.historical_control["engramDispatchGeneration"], 17);
    assert!(ordinary_queue_position(&loaded).is_none(), "unknown provider delivery is never replayable");
    assert!(matches!(loaded.runtime, SessionRuntime::None));
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn root_stop_before_ordinary_handoff_prevents_late_provider_delivery_without_control_rpc() {
    let turn = OrdinaryRoot::new("ordinary-root-stop-before-handoff");
    let dispatch = turn.prepare("The unsent prompt.");
    turn.state.request_stop_session(&turn.session).unwrap();
    deliver_turn_dispatch(&turn.state, dispatch).unwrap();
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn root_ordinary_dispatch_does_not_rebind_an_older_control_effect_set() {
    for queued in [false, true] {
        let turn = OrdinaryRoot::new("ordinary-root-passive-effect-set");
        let original = json!({"routing_token":"old-token","declared_mediated_effects":["observe","communicate"],
            "refusal":{"code":"control_assurance_insufficient","effect":"mutate_local",
                "required_assurance":"turn_gated","satisfaction":"rebind"}});
        turn.edit(|record| { record.engram.historical_control.insert("engramEffectHistory".into(), original.clone()); });
        let dispatch = if queued {
            turn.enqueue("Ordinary queued input.", QueuedPromptSource::User);
            turn.state.start_next_queued_turn_off_lock(&turn.session, false, false).unwrap().unwrap().dispatch
        } else { turn.prepare("Ordinary direct input.") };
        deliver_turn_dispatch(&turn.state, dispatch).unwrap();
        turn.prompt();
        turn.assert_no_prompt();
        turn.assert_no_control();
        turn.record(|record| assert_eq!(record.engram.historical_control["engramEffectHistory"], original));
    }
}

#[test]
fn retired_control_policy_and_assurance_refusal_bytes_do_not_gate_ordinary_turns() {
    for refusal in [
        json!({"decision":"refuse","directive":{"code":"control_assurance_insufficient"}}),
        json!({"decision":"refuse","directive":{"code":"control_assurance_insufficient",
            "effect":"mutate_local","required_assurance":"turn_gated",
            "declared_mediated_effects":["observe","communicate","mutate_local"]}}),
        json!({"decision":"refuse","directive":{"code":"control_assurance_insufficient",
            "effect":"mutate_local","required_assurance":"action_gated",
            "declared_mediated_effects":["observe","communicate"]}}),
    ] {
        let turn = OrdinaryRoot::new("ordinary-root-passive-refusal");
        turn.transport.responses.lock().unwrap().push_back(ScriptedEngramControlResponse::Reply(Ok(refusal.clone())));
        turn.edit(|record| { record.engram.historical_control.insert("engramRefusal".into(), refusal.clone()); });
        deliver_turn_dispatch(&turn.state, turn.prepare("Ordinary input.")).unwrap();
        turn.prompt();
        turn.assert_no_prompt();
        turn.assert_no_control();
        assert_eq!(turn.transport.responses.lock().unwrap().len(), 1);
        turn.record(|record| assert_eq!(record.engram.historical_control["engramRefusal"], refusal));
    }
}

#[test]
fn ordinary_turn_preserves_passive_older_effect_bytes_without_reevaluation() {
    let turn = OrdinaryRoot::new("ordinary-root-older-effects");
    let id = turn.enqueue("original uncertain provider attempt", QueuedPromptSource::User);
    let expected = turn.edit(|record| {
        let row = &mut record.queued_prompts[0];
        row.promotion_disposition_known = false;
        row.engram_ordinary_handoff = true;
        row.engram_evaluate = Some(passive_evaluate_bytes(true, json!(["observe","communicate"])));
        serde_json::to_value(row).unwrap()
    });
    deliver_turn_dispatch(&turn.state, turn.prepare("An independent ordinary task.")).unwrap();
    turn.prompt();
    turn.assert_no_prompt();
    turn.record(|record| {
        let row = record.queued_prompts.iter().find(|row| row.pending_prompt.id == id).unwrap();
        assert_eq!(row.engram_evaluate.as_ref().unwrap()["request"]["requested_effects"], json!(["observe","communicate"]));
        assert_eq!(serde_json::to_value(row).unwrap(), expected);
    });
    turn.assert_no_control();
}

#[test]
fn ordinary_turn_preserves_passive_mutating_effect_bytes_without_a_new_grant() {
    let turn = OrdinaryRoot::new("ordinary-root-mutating-effects");
    let id = turn.enqueue("original uncertain provider attempt", QueuedPromptSource::User);
    let expected = turn.edit(|record| {
        let row = &mut record.queued_prompts[0];
        row.promotion_disposition_known = false;
        row.engram_ordinary_handoff = true;
        row.engram_evaluate = Some(passive_evaluate_bytes(true, json!(["observe","communicate","mutate_local"])));
        serde_json::to_value(row).unwrap()
    });
    deliver_turn_dispatch(&turn.state, turn.prepare("An independent ordinary task.")).unwrap();
    turn.prompt();
    turn.assert_no_prompt();
    turn.record(|record| {
        let row = record.queued_prompts.iter().find(|row| row.pending_prompt.id == id).unwrap();
        assert_eq!(row.engram_evaluate.as_ref().unwrap()["request"]["requested_effects"],
            json!(["observe","communicate","mutate_local"]));
        assert_eq!(serde_json::to_value(row).unwrap(), expected);
    });
    turn.assert_no_control();
}

#[test]
fn repeated_retired_control_refusals_are_never_polled_by_ordinary_dispatch() {
    let turn = OrdinaryRoot::new("ordinary-root-unpolled-refusals");
    let refusal = json!({"decision":"refuse","directive":{"code":"control_assurance_insufficient",
        "effect":"mutate_local","required_assurance":"turn_gated"}});
    turn.transport.responses.lock().unwrap().extend((0..5).map(|_| ScriptedEngramControlResponse::Reply(Ok(refusal.clone()))));
    deliver_turn_dispatch(&turn.state, turn.prepare("Ordinary input beside old refusals.")).unwrap();
    turn.prompt();
    turn.assert_no_prompt();
    turn.assert_no_control();
    assert_eq!(turn.transport.responses.lock().unwrap().len(), 5);
    turn.record(|record| assert!(!record.session.messages.iter().any(|message| matches!(message,
        Message::EngramControl { card, .. } if card.refusal_code.as_deref() == Some("control_assurance_insufficient")))));
}

#[test]
fn root_direct_and_queued_turns_deliver_and_complete_without_a_control_lifecycle() {
    for queued in [false, true] {
        let turn = OrdinaryRoot::new("ordinary-root-delivery-completion");
        let operation = super::turn_checks::checkpoint_result_recovery::passive_checkpoint(
            &turn.root, &turn.session);
        let original = serde_json::to_value(&operation).unwrap();
        {
            let mut inner = turn.state.inner.lock().unwrap();
            super::turn_checks::checkpoint_result_recovery::install_historical_checkpoint(
                &mut inner, &turn.session, &operation);
            turn.state.commit_locked(&mut inner).unwrap();
        }
        let dispatch = if queued {
            turn.enqueue("Ordinary queued input.", QueuedPromptSource::User);
            turn.state.start_next_queued_turn_off_lock(&turn.session, false, false).unwrap().unwrap().dispatch
        } else { turn.prepare("Ordinary direct input.") };
        turn.assert_no_prompt();
        turn.assert_no_control();
        deliver_turn_dispatch(&turn.state, dispatch).unwrap();
        turn.prompt();
        turn.assert_no_prompt();
        let token = turn.record(|record| record.runtime.runtime_token().unwrap());
        turn.state.finish_turn_ok_if_runtime_matches(&turn.session, &token).unwrap();
        turn.assert_no_control();
        turn.record(|record| {
            assert_eq!(record.session.status, SessionStatus::Idle);
            assert!(record.queued_prompts.is_empty());
            assert_eq!(record.engram.historical_control["engramCheckpointOperation"], original);
            assert!(!record.session.messages.iter().any(|message| matches!(message,
                Message::EngramControl { card, .. } if card.stage == EngramControlStage::Checkpoint)));
        });
    }
}

#[test]
fn retired_control_denials_and_transport_failures_cannot_withhold_ordinary_root_turns() {
    for queued in [false, true] {
        for failure in ["evaluate", "defer", "begin", "bind", "protocol", "fatal"] {
            let turn = OrdinaryRoot::new("ordinary-root-unused-errors");
            let reply = match failure {
                "evaluate" => Ok(json!({"decision":"refuse","code":"policy_denied"})),
                "defer" => Ok(json!({"decision":"defer","reason":"not_ready"})),
                "begin" => Ok(json!({"decision":"refuse","code":"begin_policy_denied"})),
                "bind" => Err(EngramTransportError::transport("unavailable")),
                "protocol" => Err(EngramTransportError::protocol("old binding response malformed")),
                "fatal" => Ok(json!({"code":"control_disabled"})),
                _ => unreachable!(),
            };
            turn.transport.responses.lock().unwrap().push_back(ScriptedEngramControlResponse::Reply(reply));
            turn.edit(|record| {
                record.engram.historical_control.insert("engramOldFailure".into(), json!(failure));
                if failure == "fatal" {
                    record.engram.historical_control.insert("engramDisabledReason".into(), json!("control_disabled"));
                }
            });
            let dispatch = if queued {
                turn.enqueue("Ordinary queued input.", QueuedPromptSource::User);
                turn.state.start_next_queued_turn_off_lock(&turn.session, false, false).unwrap().unwrap().dispatch
            } else { turn.prepare("Ordinary direct input.") };
            deliver_turn_dispatch(&turn.state, dispatch).unwrap();
            turn.prompt();
            turn.assert_no_prompt();
            turn.assert_no_control();
            assert_eq!(turn.transport.responses.lock().unwrap().len(), 1, "{failure}");
            turn.record(|record| {
                assert_eq!(record.session.status, SessionStatus::Active);
                assert_eq!(record.engram.historical_control["engramOldFailure"], failure);
                if failure == "fatal" {
                    assert_eq!(record.engram.historical_control["engramDisabledReason"], "control_disabled");
                }
            });
        }
    }
}

#[test]
fn root_queued_mailbox_and_orchestrator_turns_need_no_control_admission() {
    for source in [QueuedPromptSource::Mailbox, QueuedPromptSource::Orchestrator] {
        let turn = OrdinaryRoot::new("ordinary-root-automatic-wake");
        turn.enqueue("Automatic root wake", source);
        let dispatch = turn.state.start_next_queued_turn_off_lock(&turn.session, false, false)
            .unwrap().unwrap().dispatch;
        deliver_turn_dispatch(&turn.state, dispatch).unwrap();
        assert!(turn.prompt().prompt.contains("Automatic root wake"));
        turn.assert_no_prompt();
        turn.assert_no_control();
        turn.record(|record| {
            assert!(record.queued_prompts.is_empty());
            assert_eq!(record.session.prompt_history.last().map(String::as_str), Some("Automatic root wake"));
        });
    }
}

#[test]
fn queued_mailbox_coalescing_preserves_selected_payload_for_ordinary_roots_and_children() {
    for child in [false, true] {
        let turn = OrdinaryRoot::new("ordinary-root-child-coalescing");
        let session = if child {
            let created = turn.state.create_read_only_delegation(&turn.session, CreateDelegationRequest {
                prompt: "Set up the child".into(), title: None, cwd: None,
                agent: Some(Agent::Codex), model: None, mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            }).unwrap();
            let session = created.delegation.child_session_id;
            let command = receive_synchronous_engram_prompt(&turn.state, &turn.receiver, "actual child setup").unwrap();
            assert!(matches!(command, CodexRuntimeCommand::Prompt { session_id, .. } if session_id == session));
            let token = {
                let inner = turn.state.inner.lock().unwrap();
                inner.sessions[inner.find_session_index(&session).unwrap()].runtime.runtime_token().unwrap()
            };
            turn.state.finish_turn_ok_if_runtime_matches(&session, &token).unwrap();
            session
        } else { turn.session.clone() };
        let source = |sequence| Some(MessageSource::mailbox(turn.session.clone(), "Sender".into(),
            MailboxMessageSource { mailbox_id:"coalesced-mailbox".into(),
                message_id:format!("mail-{sequence}"), sequence, unread_count:sequence }));
        let id = {
            let mut inner = turn.state.inner.lock().unwrap();
            let id = inner.next_message_id();
            let record = inner.session_mut(&session).unwrap();
            record.engram.context_nudge_pending = false;
            record.engram.historical_control.insert("engramRoutingToken".into(), json!("historical-token"));
            queue_prompt_on_record_with_source(record, PendingPrompt {
                engram_interrupted: false, is_engram_retained: false, attachments: Vec::new(),
                id: id.clone(), timestamp: stamp_now(), text: "Sequence 1".into(),
                expanded_text: None, source: source(1),
            }, Vec::new(), QueuedPromptSource::Mailbox);
            let queued = record.queued_prompts.front_mut().unwrap();
            queued.pending_prompt.text = "Sequence 2".into();
            queued.pending_prompt.source = source(2);
            sync_pending_prompts(record);
            turn.state.commit_locked(&mut inner).unwrap();
            id
        };
        let dispatch = turn.state.start_next_queued_turn_off_lock(&session, false, false).unwrap().unwrap().dispatch;
        deliver_turn_dispatch(&turn.state, dispatch).unwrap();
        let command = receive_synchronous_engram_prompt(&turn.state, &turn.receiver, "coalesced ordinary handoff").unwrap();
        assert!(matches!(command, CodexRuntimeCommand::Prompt { session_id, .. } if session_id == session));
        turn.assert_no_prompt();
        let inner = turn.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.queued_prompts.is_empty());
        assert!(record.session.messages.iter().any(|message| matches!(message,
            Message::Text { id: actual, text, source: actual_source, .. }
                if actual == &id && text == "Sequence 2" && actual_source == &source(2))));
        assert_eq!(record.engram.historical_control["engramRoutingToken"], "historical-token");
        turn.assert_no_control();
    }
}

#[test]
fn root_completion_drains_the_busy_ordinary_queue_despite_an_unused_control_refusal() {
    for reply in [json!({"decision":"grant","grant_id":"unused"}), json!({"decision":"refuse","code":"policy_denied"})] {
        let turn = OrdinaryRoot::new("ordinary-root-busy-completion");
        turn.transport.responses.lock().unwrap().push_back(ScriptedEngramControlResponse::Reply(Ok(reply)));
        deliver_turn_dispatch(&turn.state, turn.prepare("First ordinary turn.")).unwrap();
        turn.prompt();
        let queued = turn.state.dispatch_turn(&turn.session, SendMessageRequest {
            text: "Queued while busy".into(), expanded_text: None, attachments: Vec::new(),
            source_session_id: None, source_mailbox: None,
        }).unwrap();
        assert!(matches!(queued, DispatchTurnResult::Queued));
        let token = turn.record(|record| record.runtime.runtime_token().unwrap());
        turn.state.finish_turn_ok_if_runtime_matches(&turn.session, &token).unwrap();
        assert!(turn.prompt().prompt.contains("Queued while busy"));
        turn.assert_no_prompt();
        turn.state.finish_turn_ok_if_runtime_matches(&turn.session, &token).unwrap();
        turn.record(|record| {
            assert_eq!(record.session.status, SessionStatus::Idle);
            assert!(record.queued_prompts.is_empty());
            assert_eq!(record.session.prompt_history.iter().filter(|text| text.as_str() == "Queued while busy").count(), 1);
        });
        turn.assert_no_control();
        assert_eq!(turn.transport.responses.lock().unwrap().len(), 1);
    }
}

#[test]
fn stop_successor_uses_the_current_mailbox_head_without_a_control_evaluation_window() {
    let turn = OrdinaryRoot::new("ordinary-root-stop-successor");
    let source = |sequence| Some(MessageSource::mailbox("sender".into(), "Sender".into(),
        MailboxMessageSource { mailbox_id:"stop-mailbox".into(),
            message_id:format!("mail-{sequence}"), sequence, unread_count:sequence }));
    let id = {
        let mut inner = turn.state.inner.lock().unwrap();
        let id = inner.next_message_id();
        let record = inner.session_mut(&turn.session).unwrap();
        record.engram.historical_control.insert("engramRoutingToken".into(), json!("historical-token"));
        queue_prompt_on_record_with_source(record, PendingPrompt {
            engram_interrupted: false, is_engram_retained: false, attachments: Vec::new(),
            id: id.clone(), timestamp: stamp_now(), text: "Sequence 1".into(),
            expanded_text: None, source: source(1),
        }, Vec::new(), QueuedPromptSource::Mailbox);
        record.session.status = SessionStatus::Active;
        let head = record.queued_prompts.front_mut().unwrap();
        head.pending_prompt.text = "Sequence 2".into();
        head.pending_prompt.source = source(2);
        sync_pending_prompts(record);
        turn.state.commit_locked(&mut inner).unwrap();
        id
    };
    turn.state.stop_session_with_options(&turn.session, StopSessionOptions {
        dispatch_queued_prompts_on_success: true, pause_automatic_resumes_on_success: false,
        orchestrator_stop_instance_id: None,
    }).unwrap();
    assert!(turn.prompt().prompt.contains("Sequence 2"));
    turn.assert_no_prompt();
    turn.record(|record| {
        assert!(record.queued_prompts.is_empty());
        assert!(record.session.messages.iter().any(|message| matches!(message,
            Message::Text { id: actual, text, source: actual_source, .. }
                if actual == &id && text == "Sequence 2" && actual_source == &source(2))));
        assert_eq!(record.engram.historical_control["engramRoutingToken"], "historical-token");
    });
    turn.assert_no_control();
}

#[test]
fn root_disabled_and_base_only_projects_preserve_ordinary_dispatch() {
    for queued in [false, true] {
        for settings_mode in ["absent", "disabled", "base"] {
            let turn = OrdinaryRoot::new("ordinary-root-base-settings");
            {
                let mut inner = turn.state.inner.lock().unwrap();
                let project = inner.projects.iter_mut().find(|project| project.id == turn.project).unwrap();
                match settings_mode {
                    "absent" => project.engram = None,
                    "disabled" => project.engram.as_mut().unwrap().enabled = false,
                    "base" => project.engram.as_mut().unwrap().execution_observability = false,
                    _ => unreachable!(),
                }
            }
            let dispatch = if queued {
                turn.enqueue("Ordinary queued input.", QueuedPromptSource::User);
                turn.state.start_next_queued_turn_off_lock(&turn.session, false, false).unwrap().unwrap().dispatch
            } else { turn.prepare("Ordinary direct input.") };
            deliver_turn_dispatch(&turn.state, dispatch).unwrap();
            turn.prompt();
            turn.assert_no_prompt();
            turn.assert_no_control();
        }
    }
}

#[test]
fn ordinary_handoff_with_a_missing_delegation_never_creates_a_root_binding() {
    let turn = OrdinaryRoot::new("ordinary-root-damaged-child");
    turn.edit(|record| record.session.parent_delegation_id = Some("missing-delegation".into()));
    let dispatch = turn.prepare("An ordinary handoff grants no root authority.");
    assert!(matches!(deliver_turn_dispatch(&turn.state, dispatch), TurnDispatchDeliveryOutcome::Delivered));
    assert!(turn.prompt().prompt.contains("An ordinary handoff grants no root authority."));
    turn.assert_no_prompt();
    turn.assert_no_control();
    turn.record(|record| {
        assert_eq!(record.session.parent_delegation_id.as_deref(), Some("missing-delegation"));
        assert_eq!(record.session.status, SessionStatus::Active);
        assert!(record.engram.historical_control.is_empty(),
            "ordinary delivery cannot invent historical retry or root binding state");
    });
}

#[test]
fn ordinary_dispatch_and_completion_preserve_open_historical_grant_bytes() {
    let turn = OrdinaryRoot::new("ordinary-root-historical-open-grant");
    let historical = json!({"engramRoutingToken":"pre-crash-token","engramActiveGrantId":"pre-crash-grant",
        "engramRebindRequired":true,"engramBootRecoveryPending":true});
    turn.edit(|record| {
        for (key, value) in historical.as_object().unwrap() {
            record.engram.historical_control.insert(key.clone(), value.clone());
        }
    });
    let loaded = turn.record(|record| PersistedSessionRecord::from_record(record).into_record().unwrap());
    assert_eq!(serde_json::to_value(&loaded.engram.historical_control).unwrap(), historical);
    assert!(matches!(loaded.runtime, SessionRuntime::None));
    turn.assert_no_prompt();
    turn.assert_no_control();
    deliver_turn_dispatch(&turn.state, turn.prepare("Fresh ordinary prompt.")).unwrap();
    turn.prompt();
    let token = turn.record(|record| record.runtime.runtime_token().unwrap());
    turn.state.finish_turn_ok_if_runtime_matches(&turn.session, &token).unwrap();
    turn.record(|record| assert_eq!(serde_json::to_value(&record.engram.historical_control).unwrap(), historical));
    turn.assert_no_control();
}

#[test]
fn loading_unused_hidden_remote_and_base_only_history_constructs_no_runtime_or_observation_job() {
    let turn = OrdinaryRoot::new("ordinary-root-history-session-shapes");
    let mut histories = Vec::new();
    for kind in ["token", "grant", "rebind", "hidden", "remote", "partial-remote", "base"] {
        let id = create_test_project_session(&turn.state, Agent::Codex, &turn.project, &turn.root);
        let mut inner = turn.state.inner.lock().unwrap();
        let record = inner.session_mut(&id).unwrap();
        match kind {
            "token" => { record.engram.historical_control.insert("engramRoutingToken".into(), json!("old-token")); }
            "grant" => { record.engram.historical_control.insert("engramActiveGrantId".into(), json!("open-grant")); }
            _ => { record.engram.historical_control.insert("engramRebindRequired".into(), json!(true)); }
        }
        if kind == "hidden" { record.hidden = true; }
        if matches!(kind, "remote" | "partial-remote") {
            record.remote_id = Some("other-host".into());
            if kind == "remote" { record.remote_session_id = Some("remote-session".into()); }
        }
        let expected = record.engram.historical_control.clone();
        let saved = PersistedSessionRecord::from_record(record);
        if kind == "base" {
            let project = inner.projects.iter_mut().find(|project| project.id == turn.project).unwrap();
            project.engram.as_mut().unwrap().execution_observability = false;
        }
        histories.push((kind.to_owned(), id, saved, expected));
    }
    for _ in 0..40 {
        let id = create_test_project_session(&turn.state, Agent::Codex, &turn.project, &turn.root);
        let saved = {
            let inner = turn.state.inner.lock().unwrap();
            PersistedSessionRecord::from_record(&inner.sessions[inner.find_session_index(&id).unwrap()])
        };
        histories.push(("unused".to_owned(), id, saved, BTreeMap::new()));
    }
    for (kind, id, saved, expected) in histories {
        let restored = saved.into_record();
        if kind == "partial-remote" {
            let error = restored.err().expect("an incomplete proxy identity cannot become a local runtime");
            assert!(format!("{error:#}").contains("remoteId requires remoteSessionId"), "{error:#}");
            continue;
        }
        let loaded = restored.unwrap();
        assert_eq!(loaded.session.id, id);
        assert_eq!(loaded.engram.historical_control, expected);
        assert!(matches!(loaded.runtime, SessionRuntime::None));
        assert!(loaded.engram.unadmitted_active_turn.is_none());
        assert!(loaded.engram.unadmitted_closing_jobs.is_empty());
        assert!(loaded.provider_capacity_retry.is_none());
    }
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn passive_boot_retry_bytes_cannot_withhold_a_fresh_ordinary_prompt() {
    let turn = OrdinaryRoot::new("ordinary-root-passive-boot-retry");
    let historical = json!({"engramRebindRequired":true,"engramBeginsRecorded":false,
        "engramBootRecoveryPending":true,"engramAdmissionRetry":{"code":"unknown_control_schema","future":[null,true]}});
    turn.edit(|record| {
        for (key, value) in historical.as_object().unwrap() {
            record.engram.historical_control.insert(key.clone(), value.clone());
        }
    });
    let loaded = turn.record(|record| PersistedSessionRecord::from_record(record).into_record().unwrap());
    assert_eq!(serde_json::to_value(&loaded.engram.historical_control).unwrap(), historical);
    deliver_turn_dispatch(&turn.state, turn.prepare("An independent ordinary prompt.")).unwrap();
    turn.prompt();
    turn.assert_no_prompt();
    turn.assert_no_control();
    turn.record(|record| assert_eq!(serde_json::to_value(&record.engram.historical_control).unwrap(), historical));
}

#[test]
fn ordinary_completion_does_not_checkpoint_or_erase_preexisting_grant_bytes_or_reactivate_a_saved_setting() {
    for saved_setting in [false, true] {
        let turn = OrdinaryRoot::new("ordinary-root-removed-setting");
        {
            let mut inner = turn.state.inner.lock().unwrap();
            let settings = inner.projects.iter_mut().find(|project| project.id == turn.project).unwrap().engram.as_mut().unwrap();
            let mut raw = serde_json::to_value(&*settings).unwrap();
            raw["turnGatedControl"] = json!(saved_setting);
            *settings = serde_json::from_value(raw).unwrap();
            assert!(serde_json::to_value(settings).unwrap().get("turnGatedControl").is_none());
        }
        let historical = json!({"engramRoutingToken":"historical-token","engramActiveGrantId":"historical-grant"});
        turn.edit(|record| {
            for (key, value) in historical.as_object().unwrap() {
                record.engram.historical_control.insert(key.clone(), value.clone());
            }
        });
        deliver_turn_dispatch(&turn.state, turn.prepare("Fresh ordinary input.")).unwrap();
        turn.prompt();
        let token = turn.record(|record| record.runtime.runtime_token().unwrap());
        turn.state.finish_turn_ok_if_runtime_matches(&turn.session, &token).unwrap();
        turn.record(|record| assert_eq!(serde_json::to_value(&record.engram.historical_control).unwrap(), historical));
        turn.assert_no_control();
    }
}


#[test]
fn ordinary_root_completion_drains_independent_busy_prompts_once_in_fifo_order() {
    let turn = OrdinaryRoot::new("ordinary-root-completion-fifo");
    deliver_turn_dispatch(&turn.state, turn.prepare("First prompt.")).unwrap();
    assert!(turn.prompt().prompt.contains("First prompt."));
    for text in ["FIFO second.", "FIFO third."] {
        let result = turn.state.dispatch_turn(&turn.session, SendMessageRequest {
            text: text.into(), expanded_text: None, attachments: Vec::new(),
            source_session_id: None, source_mailbox: None,
        }).unwrap();
        assert!(matches!(result, DispatchTurnResult::Queued));
    }
    let token = turn.record(|record| record.runtime.runtime_token().unwrap());
    for text in ["FIFO second.", "FIFO third."] {
        turn.state.finish_turn_ok_if_runtime_matches(&turn.session, &token).unwrap();
        assert!(turn.prompt().prompt.contains(text), "the next handoff is the original FIFO head");
        turn.assert_no_prompt();
    }
    turn.state.finish_turn_ok_if_runtime_matches(&turn.session, &token).unwrap();
    turn.record(|record| {
        assert!(record.queued_prompts.is_empty());
        assert_eq!(record.session.status, SessionStatus::Idle);
        let selected = record.session.prompt_history.iter()
            .filter(|text| matches!(text.as_str(), "FIFO second." | "FIFO third."))
            .map(String::as_str).collect::<Vec<_>>();
        assert_eq!(selected, ["FIFO second.", "FIFO third."]);
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}
