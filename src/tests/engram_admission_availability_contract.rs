//! Ordinary admission remains available without the deleted control lifecycle.
//! Local delivery/receipt/identity fences and explicit native verification remain separate.
use super::*;
use super::super::uncertain_begin::OrdinaryRoot;
use super::super::turn_checks::{
    EOCheckedTurn, SIZE_TEST, assert_ordinary_observation_has_no_native_credit,
};

#[path = "engram_ordinary_admission_regressions.rs"]
mod ordinary_regressions;
#[path = "engram_ordinary_boundary_regressions.rs"]
mod ordinary_boundaries;
#[path = "prompt_admission_observations.rs"]
mod prompt_observations;


#[test]
fn ordinary_all_queue_sources_ignore_unused_transport_protocol_and_policy_answers() {
    for source in [QueuedPromptSource::User, QueuedPromptSource::Mailbox, QueuedPromptSource::Orchestrator] {
        for kind in ["unavailable", "lost", "protocol", "local", "refuse", "defer"] {
            let turn = OrdinaryRoot::new("ordinary-availability");
            let reply = match kind {
                "unavailable" => Err(EngramTransportError::spawn_failed("fixture unavailable")),
                "lost" => Err(EngramTransportError::transport("fixture lost response")),
                "protocol" => Err(EngramTransportError::protocol("fixture invalid response")),
                "local" => Err(EngramTransportError::local_state("fixture old control preparation")),
                _ => Ok(json!({"decision":kind,"code":"policy_denied"})),
            };
            turn.transport.responses.lock().unwrap().push_back(ScriptedEngramControlResponse::Reply(reply));
            turn.enqueue("ordinary available prompt", source);
            let started = turn.state.start_next_queued_turn_off_lock(&turn.session, false, false)
                .unwrap().unwrap();
            deliver_turn_dispatch(&turn.state, started.dispatch).unwrap();
            assert!(turn.prompt().prompt.contains("ordinary available prompt"));
            assert_eq!(turn.transport.responses.lock().unwrap().len(), 1);
            turn.record(|record| assert!(record.queued_prompts.is_empty()));
            turn.assert_no_prompt();
            turn.assert_no_control();
        }
    }
}

#[test]
fn ordinary_unknown_control_history_is_passive_but_unknown_provider_delivery_is_not_replayed() {
    for operation in ["session_bind", "turn_evaluate", "turn_begin"] {
        let turn = OrdinaryRoot::new("ordinary-passive-uncertainty");
        let id = turn.enqueue("unknown provider handoff", QueuedPromptSource::User);
        let original = json!({"operation":operation,"idempotency_key":"original-unknown",
            "routing_token":"passive-token","future":[null,{"text":"żółw"}]});
        let expected = turn.edit(|record| {
            record.engram.historical_control.insert("engramRetainedRequest".into(), original.clone());
            let queued = record.queued_prompts.front_mut().unwrap();
            queued.engram_ordinary_handoff = true;
            serde_json::to_value(queued).unwrap()
        });
        let independent = turn.enqueue("independent successor", QueuedPromptSource::User);
        turn.state.resume_session_queue(&turn.session).unwrap();
        assert!(turn.prompt().prompt.contains("independent successor"));
        turn.record(|record| {
            let retained = record.queued_prompts.iter().find(|entry| entry.pending_prompt.id == id).unwrap();
            assert_eq!(serde_json::to_value(retained).unwrap(), expected);
            assert_eq!(record.engram.historical_control["engramRetainedRequest"], original);
            assert!(!record.queued_prompts.iter().any(|entry| entry.pending_prompt.id == independent));
        });
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}

#[test]
fn ordinary_actual_local_persistence_failure_cannot_lend_provider_permission() {
    let mut turn = OrdinaryRoot::new("ordinary-local-save-failure");
    turn.state.shutdown_persist_blocking();
    let original = turn.state.persistence_path.clone();
    let failing = turn.root.join("row-save-is-directory");
    fs::create_dir(&failing).unwrap();
    turn.state.persistence_path = Arc::new(failing.clone());
    let outcome = turn.state.dispatch_turn(&turn.session, SendMessageRequest {
        text: "never sent after actual row save fails".into(), expanded_text: None,
        attachments: Vec::new(), source_session_id: None, source_mailbox: None,
    });
    assert!(outcome.is_err(), "the failed own-row commit cannot prepare a dispatch");
    turn.assert_no_prompt();
    turn.assert_no_control();
    turn.state.persistence_path = original;
    fs::remove_dir(failing).unwrap();
}

#[test]
fn ordinary_unknown_handoff_marker_cannot_decode_as_a_safe_absent_marker() {
    let turn = OrdinaryRoot::new("ordinary-unknown-marker");
    turn.enqueue("saved prompt", QueuedPromptSource::User);
    let mut value = turn.record(|record| serde_json::to_value(record.queued_prompts.front().unwrap()).unwrap());
    value["engram_ordinary_handoff"] = json!("future-phase");
    assert!(serde_json::from_value::<QueuedPromptRecord>(value).is_err());
}

#[test]
fn ordinary_saved_provider_handoff_remains_unreplayable_without_any_control_restore() {
    let turn = OrdinaryRoot::new("ordinary-saved-unknown-handoff");
    let id = turn.enqueue("original unknown delivery", QueuedPromptSource::User);
    turn.edit(|record| {
        record.queued_prompts.front_mut().unwrap().engram_ordinary_handoff = true;
        record.set_auto_dispatch_blocked(true);
    });
    let loaded = turn.record(|record| PersistedSessionRecord::from_record(record).into_record().unwrap());
    assert!(ordinary_queue_position(&loaded).is_none());
    assert!(matches!(loaded.runtime, SessionRuntime::None));
    assert!(loaded.engram.unadmitted_active_turn.is_none());
    let expected = serde_json::to_value(&loaded.queued_prompts[0]).unwrap();
    {
        let mut inner = turn.state.inner.lock().unwrap();
        let index = inner.find_session_index(&turn.session).unwrap();
        inner.sessions[index] = loaded;
    }
    // No runnable head is a no-op; unknown delivery is not a request to replay.
    assert!(turn.state.resume_local_session_queue(&turn.session, None).unwrap().is_none());
    turn.record(|record| {
        assert_eq!(record.queued_prompts[0].pending_prompt.id, id);
        assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), expected);
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_boot_shapes_preserve_history_without_constructing_a_runtime_or_retry() {
    let turn = OrdinaryRoot::new("ordinary-boot-data");
    for kind in ["idle", "active", "queued", "hidden", "remote"] {
        let id = create_test_project_session(&turn.state, Agent::Codex, &turn.project, &turn.root);
        let original = json!({"engramRoutingToken":"saved-token","engramActiveGrantId":"saved-grant",
            "engramRebindRequired":true,"engramBootRecoveryPending":true,"kind":kind});
        let saved = {
            let mut inner = turn.state.inner.lock().unwrap();
            let prompt_id = inner.next_message_id();
            let record = inner.session_mut(&id).unwrap();
            for (key, value) in original.as_object().unwrap() {
                record.engram.historical_control.insert(key.clone(), value.clone());
            }
            if kind == "active" { record.session.status = SessionStatus::Active; }
            if kind == "queued" {
                queue_prompt_on_record_with_source(record, PendingPrompt {
                    engram_interrupted: false, is_engram_retained: false, attachments: Vec::new(),
                    id: prompt_id, timestamp: stamp_now(), text: "saved queued work".into(),
                    expanded_text: None, source: None,
                }, Vec::new(), QueuedPromptSource::User);
            }
            if kind == "hidden" { record.hidden = true; }
            if kind == "remote" {
                record.remote_id = Some("remote-host".into());
                record.remote_session_id = Some("remote-root".into());
            }
            PersistedSessionRecord::from_record(record)
        };
        let loaded = saved.into_record().unwrap();
        assert_eq!(serde_json::to_value(&loaded.engram.historical_control).unwrap(), original);
        assert!(matches!(loaded.runtime, SessionRuntime::None));
        assert!(loaded.provider_capacity_retry.is_none());
        assert!(loaded.engram.unadmitted_active_turn.is_none());
        if kind == "queued" {
            assert_eq!(loaded.queued_prompts.front().unwrap().pending_prompt.text, "saved queued work");
        }
    }
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_first_turn_needs_no_lazy_rebind_or_saved_grant() {
    let turn = OrdinaryRoot::new("ordinary-first-with-history");
    turn.edit(|record| {
        record.engram.historical_control.insert("engramRoutingToken".into(), json!("saved-token"));
        record.engram.historical_control.insert("engramRebindRequired".into(), json!(true));
    });
    deliver_turn_dispatch(&turn.state, turn.prepare("first ordinary turn")).unwrap();
    turn.prompt();
    turn.record(|record| {
        assert_eq!(record.engram.historical_control["engramRoutingToken"], "saved-token");
        assert_eq!(record.engram.historical_control["engramRebindRequired"], true);
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_observed_command_facts_never_create_native_verification_credit() {
    let turn = EOCheckedTurn::start("ordinary-availability-uncredited", true);
    // Recorder-supplied fixture facts, not an executed command in this test.
    turn.run("ordinary-command", SIZE_TEST, EngramCommandExit::Code(0), || {});
    let request = turn.finish();
    assert_eq!(request["occurrence"]["observed_checks"].as_array().unwrap().len(), 1);
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}
