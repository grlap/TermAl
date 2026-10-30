// Invocation/authority regressions through the actual recorder/check owners.
// Protocol fixtures supply native creation scope explicitly; no command runs.
use super::*;

fn begin(recorder: &mut SessionRecorder, native: &str, command: &str) -> CommandInvocation {
    let reference = recorder.command_created(Some(native), "reused-display-key");
    recorder.command_invocation(&reference, CommandInvocationEvent::Dispatch {
        command, ran: Some(command), cwd: None,
    }).unwrap();
    reference
}

fn finish(recorder: &mut SessionRecorder, reference: &CommandInvocation) {
    recorder.command_invocation(reference, CommandInvocationEvent::Finish {
        command: SIZE_TEST, reported_command: Some(SIZE_TEST),
        output: "running 3 tests\ntest result: ok. 3 passed",
        status: CommandStatus::Success, exit: EngramCommandExit::Code(0), runtime_output_cut: false,
    }).unwrap();
}

#[test]
fn a_retained_original_cannot_acquire_a_later_binding_or_grant() {
    let turn = CheckedTurn::start("invocation-original-authority", true);
    let authority = turn.record(|record| (record.engram.work_binding.clone(), record.engram.active_grant_id.clone()));
    turn.record_mut(|record| { record.engram.work_binding = None; record.engram.active_grant_id = None; });
    let mut recorder = turn.recorder();
    test_command_scope(&mut recorder, CommandProducer::Acp);
    let original = begin(&mut recorder, "native-original", SIZE_TEST);
    assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
    turn.record_mut(|record| { record.engram.work_binding = authority.0; record.engram.active_grant_id = authority.1; });
    let repeated = recorder.command_created(Some("native-original"), "reused-display-key");
    assert_eq!(original.key, repeated.key);
    recorder.command_invocation(&repeated, CommandInvocationEvent::Dispatch {
        command: SIZE_TEST, ran: Some(SIZE_TEST), cwd: None,
    }).unwrap();
    finish(&mut recorder, &original);
    assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
    let inner = turn.state.inner.lock().unwrap();
    let rows: Vec<_> = inner.test_run_observations.values()
        .filter(|row| row.summary.owner_session_id.as_deref() == Some(turn.session_id.as_str())).collect();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].work_binding.is_none() && rows[0].grant_identity.is_none());
    drop(inner);
    assert!(turn.finish().get("verification_evidence").is_none());
}

#[test]
fn a_description_and_old_terminal_cannot_change_a_distinct_same_key_check() {
    let turn = CheckedTurn::start("invocation-distinct-originals", true);
    let mut recorder = turn.recorder();
    test_command_scope(&mut recorder, CommandProducer::Acp);
    let first = begin(&mut recorder, "native-first", SIZE_TEST);
    turn.wait_for_snapshots();
    finish(&mut recorder, &first);
    turn.wait_for_snapshots();
    let second = begin(&mut recorder, "native-second", SIZE_TEST);
    assert_ne!(first.key, second.key);
    turn.wait_for_snapshots();
    finish(&mut recorder, &first);
    recorder.command_invocation(&first, CommandInvocationEvent::Describe { ran: Some("echo old"), cwd: None }).unwrap();
    turn.record(|record| {
        let check = record.engram.active_turn_checks.iter().find(|check| check.key == second.key).unwrap();
        assert!(check.end.is_none() && !check.overlapped);
        assert_eq!(check.command.normalized, SIZE_TEST);
    });
    finish(&mut recorder, &second);
    finish(&mut recorder, &second);
    turn.wait_for_snapshots();
    assert_eq!(turn.finish()["verification_evidence"].as_array().map(Vec::len), Some(2));
}

#[test]
fn an_update_without_creation_stays_nonqualifying_when_creation_arrives_late() {
    let turn = CheckedTurn::start("invocation-update-before-create", true);
    let mut recorder = turn.recorder();
    let mut acp = AcpTurnState::default();
    let (tx, _rx) = std::sync::mpsc::channel();
    for kind in ["tool_call_update", "tool_call", "tool_call_update"] {
        test_acp_session_update(&json!({"sessionUpdate":kind, "toolCallId":"missing-create",
            "status":"in_progress", "rawInput":{"command":SIZE_TEST}}),
            &turn.state, &turn.session_id, &tx, &mut acp, &mut recorder, AcpAgent::Cursor).unwrap();
    }
    assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
    let line = turn.record(|record| record.engram.pending_source_root_line.clone()).unwrap();
    assert!(line.contains("without its creation") || line.contains("no retained tool_call creation"), "{line}");
    assert!(turn.state.inner.lock().unwrap().test_run_observations.values()
        .all(|row| row.summary.owner_session_id.as_deref() != Some(turn.session_id.as_str())));
    assert!(turn.finish().get("verification_evidence").is_none());
}

#[test]
fn concurrent_capacity_preserves_owned_ends_and_a_surviving_scope_loss() {
    let turn = CheckedTurn::start("invocation-concurrent-capacity", true);
    let mut recorder = turn.recorder();
    test_command_scope(&mut recorder, CommandProducer::Acp);
    let owned = begin(&mut recorder, "owned-first", SIZE_TEST);
    turn.wait_for_snapshots();
    recorder.command_invocation(&owned, CommandInvocationEvent::Finish {
        command: SIZE_TEST, reported_command: Some(SIZE_TEST), output: "still running",
        status: CommandStatus::Running, exit: EngramCommandExit::NotFinished, runtime_output_cut: false,
    }).unwrap();
    assert!(!recorder.recorder_state.command_messages[&owned.key].invocation.as_ref().unwrap().terminal);
    assert!(turn.record(|record| record.engram.active_turn_checks[0].end.is_none()));
    for index in 1..COMMAND_INVOCATION_LIMIT {
        let reference = recorder.command_created(Some(&format!("reserved-{index}")), "reserved");
        assert!(!matches!(reference.provenance, EngramObservationProvenance::Unproven));
    }
    assert_eq!(recorder.recorder_state.command_messages.values().filter(|entry| entry.invocation.is_some()).count(), COMMAND_INVOCATION_LIMIT);
    let refused = recorder.command_created(Some("overflowed"), "overflowed");
    assert!(matches!(refused.provenance, EngramObservationProvenance::Unproven));
    let line = turn.record(|record| record.engram.pending_source_root_line.clone()).unwrap();
    assert!(line.contains("unresolved command identity capacity is full"), "{line}");
    finish(&mut recorder, &owned);
    turn.wait_for_snapshots();
    turn.record(|record| {
        let check = record.engram.active_turn_checks.iter().find(|check| check.key == owned.key).unwrap();
        assert_eq!(check.end.as_ref().unwrap().exit, EngramCommandExit::Code(0));
        assert!(!check.overlapped);
    });
    let repeat = recorder.command_created(Some("overflowed"), "overflowed");
    assert!(matches!(repeat.provenance, EngramObservationProvenance::Unproven));
    // A new wrapper consults its surviving runtime owner, not its default map.
    let mut replacement = turn.recorder();
    test_command_scope(&mut replacement, CommandProducer::Acp);
    let resumed = replacement.command_created(Some("overflowed"), "overflowed");
    assert!(matches!(resumed.provenance, EngramObservationProvenance::Unproven));
    let (runtime, _receiver) = test_acp_runtime_handle(AcpAgent::Cursor, "capacity-replacement-reader");
    turn.record_mut(|record| record.runtime = SessionRuntime::Acp(runtime));
    test_command_scope(&mut replacement, CommandProducer::Acp);
    let after_runtime_replacement = replacement.command_created(Some("overflowed"), "overflowed");
    assert!(matches!(after_runtime_replacement.provenance, EngramObservationProvenance::Unproven));
    let mut fresh_scope = replacement.recorder_state.command_scope.clone().unwrap();
    fresh_scope.session = "genuinely-new-native-session".to_owned();
    replacement.command_scope(fresh_scope);
    let fresh = replacement.command_created(Some("new-native-call"), "reused-display-key");
    assert!(!matches!(fresh.provenance, EngramObservationProvenance::Unproven));
    assert_ne!(fresh.key, refused.key);
    assert_eq!(turn.finish()["verification_evidence"].as_array().map(Vec::len), Some(1));
}

#[test]
fn sequential_terminal_calls_do_not_accumulate_active_capacity_debt() {
    let turn = CheckedTurn::start("invocation-sequential-capacity", true);
    let mut recorder = turn.recorder();
    test_command_scope(&mut recorder, CommandProducer::Acp);
    for index in 0..(COMMAND_INVOCATION_LIMIT * 2 + 1) {
        let reference = recorder.command_created(Some(&format!("sequential-{index}")), "same-label");
        assert!(!matches!(reference.provenance, EngramObservationProvenance::Unproven));
        recorder.command_invocation(&reference, CommandInvocationEvent::Finish {
            command: "echo done", reported_command: Some("echo done"), output: "done",
            status: CommandStatus::Success, exit: EngramCommandExit::Code(0), runtime_output_cut: false,
        }).unwrap();
    }
    assert!(!recorder.recorder_state.command_creation_coverage_lost);
    assert!(recorder.recorder_state.command_messages.values().filter_map(|entry| entry.invocation.as_ref()).all(|entry| entry.terminal));
    let next = begin(&mut recorder, "actual-test-after-sequential-calls", SIZE_TEST);
    turn.wait_for_snapshots();
    finish(&mut recorder, &next);
    turn.wait_for_snapshots();
    assert_eq!(turn.finish()["verification_evidence"].as_array().map(Vec::len), Some(1));
}

#[test]
fn native_turn_disambiguates_reused_codex_ids_and_turnless_loss_stays_local() {
    let turn = CheckedTurn::start("invocation-native-turn", true);
    let mut recorder = turn.recorder();
    test_command_scope(&mut recorder, CommandProducer::Codex);
    let first = recorder.command_created(Some("reused-native-item"), "same-command-key");
    let unrelated = recorder.command_created(Some("unrelated-item"), "other-key");
    let mut next_scope = recorder.recorder_state.command_scope.clone().unwrap();
    next_scope.turn = Some("second-native-turn".to_owned());
    recorder.command_scope(next_scope.clone());
    let second = recorder.command_created(Some("reused-native-item"), "same-command-key");
    assert_ne!(first.key, second.key);
    assert_eq!(recorder.command_lookup(Some("reused-native-item"), "same-command-key").unwrap().key, second.key);
    next_scope.turn = None;
    recorder.command_scope(next_scope);
    assert!(recorder.command_lookup(Some("reused-native-item"), "same-command-key").is_none());
    assert_eq!(recorder.command_lookup(Some("unrelated-item"), "other-key").unwrap().key, unrelated.key);
    assert!(turn.finish().get("verification_evidence").is_none());
}

#[test]
fn terminal_retirement_keeps_ambiguity_on_only_the_retained_native_id() {
    for capacity_retirement in [false, true] {
        let turn = CheckedTurn::start("invocation-local-retirement", true);
        let mut recorder = turn.recorder();
        test_command_scope(&mut recorder, CommandProducer::Codex);
        let first = recorder.command_created(Some("colliding-item"), "first");
        let mut scope = recorder.recorder_state.command_scope.clone().unwrap();
        scope.turn = Some("second-native-turn".to_owned());
        recorder.command_scope(scope.clone());
        let second = recorder.command_created(Some("colliding-item"), "second");
        recorder.command_invocation(&first, CommandInvocationEvent::Release).unwrap();
        if capacity_retirement {
            for index in 2..COMMAND_INVOCATION_LIMIT {
                recorder.command_created(Some(&format!("reserved-{index}")), "reserved");
            }
        } else {
            reset_recorder_state_fields(&mut recorder.recorder_state);
        }
        let unrelated = recorder.command_created(Some("unrelated-after-retirement"), "unrelated");
        assert!(!recorder.recorder_state.command_messages.contains_key(&first.key));
        assert!(recorder.recorder_state.command_messages.contains_key(&second.key));
        scope.turn = None;
        recorder.command_scope(scope.clone());
        assert!(recorder.command_lookup(Some("colliding-item"), "second").is_none(),
            "retiring the competing proof cannot clear the retained reference's ambiguity");
        assert_eq!(recorder.command_lookup(Some("unrelated-after-retirement"), "unrelated").unwrap().key, unrelated.key);
        scope.turn = Some("second-native-turn".to_owned());
        recorder.command_scope(scope);
        assert_eq!(recorder.command_lookup(Some("colliding-item"), "second").unwrap().key, second.key);
        assert!(!recorder.recorder_state.command_creation_coverage_lost);
        assert!(turn.finish().get("verification_evidence").is_none());
    }
}

#[test]
fn an_unproven_terminal_cannot_taint_an_independently_owned_check() {
    for already_finished in [false, true] {
        let turn = CheckedTurn::start("invocation-unproven-terminal", true);
        let mut recorder = turn.recorder();
        test_command_scope(&mut recorder, CommandProducer::Codex);
        let owned = begin(&mut recorder, "owned-test", SIZE_TEST);
        turn.wait_for_snapshots();
        if already_finished { finish(&mut recorder, &owned); }
        let mixed = turn.record(|record| record.engram.active_turn_mixed_attribution.clone());
        for code in [Some(0), Some(101), None] {
            let item = json!({"type":"commandExecution", "id":format!("unmatched-{code:?}"),
                "command":SIZE_TEST, "status":"failed", "exitCode":code,
                "aggregatedOutput":"unmatched result"});
            test_codex_item_completed(&item, &turn.state, &turn.session_id,
                &mut CodexTurnState::default(), &mut recorder).unwrap();
            turn.record(|record| {
                assert_eq!(record.engram.active_turn_checks.len(), 1);
                let check = &record.engram.active_turn_checks[0];
                assert_eq!(check.key, owned.key);
                assert_eq!(check.end.as_ref().map(|end| end.exit), already_finished.then_some(EngramCommandExit::Code(0)));
                assert!(!check.overlapped);
                assert_eq!(record.engram.active_turn_mixed_attribution, mixed);
            });
        }
        if !already_finished { finish(&mut recorder, &owned); }
        turn.wait_for_snapshots();
        assert_eq!(turn.finish()["verification_evidence"].as_array().map(Vec::len), Some(1));
    }
}

#[test]
fn irreversible_owner_loss_can_admit_new_credit_without_recovering_old_facts() {
    for prior in ["completed", "unresolved", "overflow-refused"] {
        let turn = CheckedTurn::start(&format!("invocation-owner-loss-{prior}"), true);
        let authority = turn.record(|record| (record.engram.work_binding.clone(), record.engram.active_grant_id.clone()));
        turn.record_mut(|record| { record.engram.work_binding = None; record.engram.active_grant_id = None; });
        let mut recorder = turn.recorder();
        test_command_scope(&mut recorder, CommandProducer::Acp);
        if prior == "overflow-refused" {
            for index in 0..COMMAND_INVOCATION_LIMIT {
                recorder.command_created(Some(&format!("pending-{index}")), "pending");
            }
        }
        let old = recorder.command_created(Some("replayed-native-id"), "same-display-key");
        if prior != "overflow-refused" {
            recorder.command_invocation(&old, CommandInvocationEvent::Dispatch {
                command: SIZE_TEST, ran: Some(SIZE_TEST), cwd: None,
            }).unwrap();
            if prior == "completed" { finish(&mut recorder, &old); }
        }
        assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
        let old_rows: Vec<_> = turn.state.inner.lock().unwrap().test_run_observations.values()
            .filter(|row| row.summary.owner_session_id.as_deref() == Some(turn.session_id.as_str())).cloned().collect();
        drop(recorder);
        // Actual persisted reconstruction contains neither the reference map
        // nor this process-local loss fact. Model that owner loss only; a
        // production wrapper replacement is tested separately and keeps it.
        let restored = turn.record(|record| PersistedSessionRecord::from_record(record).into_record().unwrap());
        assert!(restored.command_creation_loss.is_none());
        turn.record_mut(|record| {
            record.command_creation_loss = restored.command_creation_loss.clone();
            record.engram.work_binding = authority.0;
            record.engram.active_grant_id = authority.1;
        });
        let mut resumed = turn.recorder();
        test_command_scope(&mut resumed, CommandProducer::Acp);
        let new = begin(&mut resumed, "replayed-native-id", SIZE_TEST);
        assert_ne!(old.key, new.key);
        turn.wait_for_snapshots();
        finish(&mut resumed, &new);
        turn.wait_for_snapshots();
        {
            let inner = turn.state.inner.lock().unwrap();
            for old_row in &old_rows {
                assert_eq!(inner.test_run_observations.get(&old_row.execution_id), Some(old_row), "new reference must not change original {prior} facts");
                assert!(old_row.work_binding.is_none() && old_row.grant_identity.is_none() && old_row.evidence.is_empty());
            }
        }
        assert_eq!(turn.finish()["verification_evidence"].as_array().map(Vec::len), Some(1), "{prior}: accepted NEW credit risk, never inherited OLD credit");
    }
}

#[test]
fn loaded_history_creates_nothing_and_a_resumed_live_start_can_earn_new_credit() {
    let turn = CheckedTurn::start("invocation-acp-history-resume", true);
    let (runtime, _receiver) = test_acp_runtime_handle(AcpAgent::Cursor, "invocation-acp-reader");
    let token = RuntimeToken::Acp(runtime.runtime_id.clone());
    let tx = runtime.input_tx.clone();
    turn.record_mut(|record| { record.runtime = SessionRuntime::Acp(runtime); record.session.agent = Agent::Cursor; });
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState { current_session_id: Some("resumed-native-session".to_owned()), is_loading_history: true, ..Default::default() }));
    let mut recorder = turn.recorder();
    let mut acp = AcpTurnState::default();
    let frame = |kind: &str, id: &str| json!({"method":"session/update", "params":{
        "sessionId":"resumed-native-session", "update":{"sessionUpdate":kind, "toolCallId":id,
        "status":if kind == "tool_call" { "in_progress" } else { "completed" },
        "rawInput":{"command":SIZE_TEST}, "rawOutput":{"exitCode":0,"stdout":"running 3 tests\ntest result: ok. 3 passed"}}}});
    for kind in ["tool_call", "tool_call_update"] {
        handle_acp_notification("session/update", &frame(kind, "historical"), &turn.state, &turn.session_id,
            &token, &runtime_state, &tx, &mut acp, &mut recorder, AcpAgent::Cursor).unwrap();
    }
    assert!(recorder.recorder_state.command_messages.is_empty());
    assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
    runtime_state.lock().unwrap().is_loading_history = false;
    handle_acp_notification("session/update", &frame("tool_call", "new-live-start"), &turn.state, &turn.session_id,
        &token, &runtime_state, &tx, &mut acp, &mut recorder, AcpAgent::Cursor).unwrap();
    turn.wait_for_snapshots();
    let original_scope = recorder.recorder_state.command_scope.clone();
    let original_keys: BTreeSet<_> = recorder.recorder_state.command_messages.keys().cloned().collect();
    let mut foreign = frame("tool_call", "foreign");
    foreign["params"]["sessionId"] = json!("not-the-initialized-native-session");
    handle_acp_notification("session/update", &foreign, &turn.state, &turn.session_id,
        &token, &runtime_state, &tx, &mut acp, &mut recorder, AcpAgent::Cursor).unwrap();
    assert_eq!(recorder.recorder_state.command_scope, original_scope);
    assert_eq!(recorder.recorder_state.command_messages.keys().cloned().collect::<BTreeSet<_>>(), original_keys);
    handle_acp_notification("session/update", &frame("tool_call_update", "new-live-start"), &turn.state, &turn.session_id,
        &token, &runtime_state, &tx, &mut acp, &mut recorder, AcpAgent::Cursor).unwrap();
    turn.wait_for_snapshots();
    assert_eq!(turn.finish()["verification_evidence"].as_array().map(Vec::len), Some(1));
}

#[test]
fn a_replaced_readers_old_terminal_cannot_finish_the_new_same_key_invocation() {
    let turn = CheckedTurn::start("invocation-replaced-reader", true);
    let mut recorder = turn.recorder();
    test_command_scope(&mut recorder, CommandProducer::Acp);
    // A real Kimi-style creation reservation, before input-bearing dispatch.
    let old = recorder.command_created(Some("reused-native-id"), "same-display-key");
    let (runtime, _receiver) = test_acp_runtime_handle(AcpAgent::Cursor, "new-invocation-reader");
    turn.record_mut(|record| record.runtime = SessionRuntime::Acp(runtime));
    test_command_scope(&mut recorder, CommandProducer::Acp);
    let new = begin(&mut recorder, "reused-native-id", SIZE_TEST);
    assert_ne!(old.key, new.key);
    turn.wait_for_snapshots();
    finish(&mut recorder, &old);
    turn.record(|record| {
        let check = record.engram.active_turn_checks.iter().find(|check| check.key == new.key).unwrap();
        assert!(check.end.is_none() && !check.overlapped);
    });
    finish(&mut recorder, &new);
    turn.wait_for_snapshots();
    assert_eq!(turn.finish()["verification_evidence"].as_array().map(Vec::len), Some(1));
}

#[test]
fn missing_codex_identity_has_explicit_foreground_and_nonqualifying_boundaries() {
    let turn = CheckedTurn::start("invocation-idless-boundaries", true);
    let mut recorder = turn.recorder();
    test_command_scope(&mut recorder, CommandProducer::Codex);
    let foreground = recorder.command_created(None, SIZE_TEST);
    recorder.command_invocation(&foreground, CommandInvocationEvent::Dispatch { command: SIZE_TEST, ran: Some(SIZE_TEST), cwd: None }).unwrap();
    turn.wait_for_snapshots();
    finish(&mut recorder, &foreground);
    turn.wait_for_snapshots();
    let repeated = recorder.command_created(None, SIZE_TEST);
    assert_eq!(repeated.key, foreground.key, "identical id-less frames cannot prove a second call");
    recorder.command_invocation(&repeated, CommandInvocationEvent::Dispatch { command: SIZE_TEST, ran: Some(SIZE_TEST), cwd: None }).unwrap();
    assert_eq!(turn.record(|record| record.engram.active_turn_checks.len()), 1);
    let mut no_turn = recorder.recorder_state.command_scope.clone().unwrap();
    no_turn.turn = None;
    recorder.command_scope(no_turn);
    let uncertain = recorder.command_created(None, SIZE_TEST);
    recorder.command_invocation(&uncertain, CommandInvocationEvent::Dispatch { command: SIZE_TEST, ran: Some(SIZE_TEST), cwd: None }).unwrap();
    assert_eq!(turn.record(|record| record.engram.active_turn_checks.len()), 1);
    let line = turn.record(|record| record.engram.pending_source_root_line.clone()).unwrap();
    assert!(line.contains("neither a native invocation identity nor a native event turn"), "{line}");
    assert_eq!(turn.finish()["verification_evidence"].as_array().map(Vec::len), Some(1));
}

#[test]
fn the_dynamic_recorder_forwards_the_producer_reference_and_real_checkpoint_path() {
    let turn = CheckedTurn::start("invocation-dynamic-recorder", true);
    let mut recorder = turn.recorder();
    test_command_scope(&mut recorder, CommandProducer::Codex);
    let mut item = json!({"type":"commandExecution", "id":"dynamic-native-item", "command":SIZE_TEST,
        "status":"inProgress", "cwd":turn.root});
    {
        let mut dynamic = DynTurnRecorderRef::new(&mut recorder);
        handle_codex_app_server_item_started(&item, &mut dynamic).unwrap();
    }
    let reference = recorder.command_lookup(Some("dynamic-native-item"), SIZE_TEST).unwrap();
    assert!(recorder.recorder_state.command_messages[&reference.key].invocation.is_some());
    turn.record(|record| assert_eq!(record.engram.active_turn_checks[0].key, reference.key));
    turn.wait_for_snapshots();
    item["status"] = json!("completed");
    item["exitCode"] = json!(0);
    item["aggregatedOutput"] = json!("running 3 tests\ntest result: ok. 3 passed");
    {
        let mut dynamic = DynTurnRecorderRef::new(&mut recorder);
        handle_codex_app_server_item_completed(&item, &turn.state, &turn.session_id,
            &mut CodexTurnState::default(), &mut dynamic).unwrap();
    }
    turn.wait_for_snapshots();
    assert_eq!(turn.finish()["verification_evidence"].as_array().map(Vec::len), Some(1));
}

#[test]
fn a_textless_codex_creation_keeps_its_original_facts_without_a_late_check() {
    let turn = CheckedTurn::start("invocation-textless-codex", true);
    let binding = turn.record(|record| record.engram.work_binding.clone());
    let mut recorder = turn.recorder();
    test_command_scope(&mut recorder, CommandProducer::Codex);
    handle_codex_app_server_item_started(&json!({"type":"commandExecution", "id":"textless-native-item",
        "status":"inProgress"}), &mut recorder).unwrap();
    assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
    turn.record_mut(|record| record.engram.work_binding = None);
    handle_codex_app_server_item_completed(&json!({"type":"commandExecution", "id":"textless-native-item",
        "command":SIZE_TEST, "status":"completed", "exitCode":0,
        "aggregatedOutput":"running 3 tests\ntest result: ok. 3 passed"}), &turn.state,
        &turn.session_id, &mut CodexTurnState::default(), &mut recorder).unwrap();
    assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
    {
        let inner = turn.state.inner.lock().unwrap();
        let rows: Vec<_> = inner.test_run_observations.values().filter(|row| row.summary.owner_session_id.as_deref() == Some(turn.session_id.as_str())).collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].work_binding, binding);
        assert!(rows[0].evidence.is_empty());
    }
    turn.record_mut(|record| record.engram.work_binding = binding);
    assert!(turn.finish().get("verification_evidence").is_none());
}
