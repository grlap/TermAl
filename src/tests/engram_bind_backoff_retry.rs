//! Transient first-bind failure at the real root dispatch/delivery boundary.
//! Uses the existing control fault fixture and retry tick, not a provider
//! subprocess or an independent test clock.

use super::*;

#[test]
fn bind_real_settings_reset_settles_the_actual_caller_after_card_save() {
    bind_real_settings_reset_case(false, None, false);
}

#[test]
fn bind_real_settings_reset_settles_the_actual_withdrawal_recheck() {
    bind_real_settings_reset_case(true, None, false);
}

#[test]
fn bind_real_settings_reset_does_not_resynthesize_a_cancelled_mailbox_wake() {
    bind_real_settings_reset_case(true, Some(false), false);
}

#[test]
fn bind_real_settings_reset_preserves_the_coalesced_mailbox_head_without_a_stale_wake() {
    bind_real_settings_reset_case(true, Some(true), false);
}

#[test]
fn bind_real_settings_reset_does_not_leave_its_delegation_followup_running() {
    bind_real_settings_reset_case(false, None, true);
}

fn bind_real_settings_reset_case(withdrawal: bool, mailbox_coalescing: Option<bool>, delegation_child: bool) {
    let mut responses = Vec::new();
    if delegation_child {
        responses.extend([bind_reply("initial-parent"), bind_reply("initial-child"),
            grant_reply("initial-grant"), begin_reply("initial-grant"), checkpoint_reply("initial-grant")]);
    }
    responses.extend([
        bind_failure(), bind_reply("reset-bind"), bind_reply("recovered-bind"),
        grant_reply("recovered-grant"), begin_reply("recovered-grant"),
    ]);
    let (mut state, mut session, receiver, transport) = root_fixture(responses);
    let mut delegation = None;
    if delegation_child {
        let parent = session.clone();
        let created = state.create_read_only_delegation(&parent, CreateDelegationRequest {
            prompt: "initial delegation turn".to_owned(), title: Some("reset-race followup".to_owned()),
            cwd: None, agent: Some(Agent::Codex), model: None, mode: Some(DelegationMode::Explorer),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        }).unwrap();
        session = created.delegation.child_session_id;
        delegation = Some((parent, created.delegation.id));
        assert!(receive_synchronous_engram_prompt(&state, &receiver, "initial delegation prompt").is_ok());
        let runtime = {
            let inner = state.inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&session).unwrap()].runtime.runtime_token().unwrap()
        };
        crate::tests::delegation_support::finish_delegation_child_with_assistant_text(
            &state, &session, "## Result\nStatus: completed\n\nSummary:\ninitial completed",
        );
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].session.status = SessionStatus::Active;
        }
        state.finish_turn_ok_if_runtime_matches(&session, &runtime).unwrap();
        state.refresh_delegation_for_child_session(&session).unwrap();
    }
    let (project, mut settings, root) = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let project = inner.sessions[index].session.project_id.clone().unwrap();
        let project_record = inner.projects.iter_mut().find(|p| p.id == project).unwrap();
        let root = PathBuf::from(&project_record.root_path);
        fs::write(root.join(".engram-project"), "fixture-ok\n").unwrap();
        let settings = project_record.engram.as_mut().unwrap();
        settings.binary_path = Some(real_engram_control_fixture_path().to_string_lossy().into_owned());
        (project, settings.clone(), root)
    };
    let mut mailbox_sender = None;
    if mailbox_coalescing.is_some() {
        state.mailbox_store = Arc::new(MailboxStore::open(
            &resolve_coordination_persistence_path(state.persistence_path.as_ref()),
        ).unwrap());
        mailbox_sender = Some(test_session_id(&state, Agent::Claude));
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].set_auto_dispatch_blocked(true);
    }
    if withdrawal {
        arm_bind_backoff(&state, &session, Duration::from_secs(3600));
    }
    let send = |key: &str, topic: &str| SendMailboxMessageRequest {
        target_session_id: session.clone(), message: "durable reset-race body".to_owned(),
        idempotency_key: key.to_owned(), topic: Some(topic.to_owned()), state_stamp: None,
        class: Some("routine".to_owned()),
    };
    let first_wake = mailbox_sender.as_ref().map(|sender| {
        state.append_mailbox_message_and_notify(sender, send("reset-first-wake", "reset-original-topic")).unwrap()
    });
    let dispatch = if let Some((parent, id)) = &delegation {
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].engram.project_reset_in_progress = true;
        }
        state.followup_delegation(parent, id, "reset-race delegation followup".to_owned()).unwrap();
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].engram.project_reset_in_progress = false;
            inner.sessions[index].engram.routing_token = None;
            inner.sessions[index].engram.rebind_required = true;
        }
        state.start_next_queued_turn_off_lock(&session, false, false).unwrap().unwrap().dispatch
    } else if mailbox_sender.is_some() {
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].set_auto_dispatch_blocked(false);
        }
        state.start_next_queued_turn_off_lock(&session, false, false).unwrap().unwrap().dispatch
    } else {
        root_dispatch(&state, &session, false)
    };
    let original_generation = dispatch.engram_dispatch_generation().unwrap();
    let original_turn = dispatch.active_turn_generation();
    let runtime = dispatch.runtime_token().clone();
    let prompt = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session).unwrap()].queued_prompts[0].pending_prompt.id.clone()
    };
    let cancelled = withdrawal && mailbox_coalescing != Some(true);
    let mut expected_sequence = first_wake.as_ref().map(|wake| wake.sequence);
    if withdrawal {
        require_unprepared_owned_bind_dispatch(&state, &session, &dispatch);
        if mailbox_coalescing == Some(true) {
            let second = state.append_mailbox_message_and_notify(mailbox_sender.as_ref().unwrap(),
                send("reset-second-wake", "reset-current-topic")).unwrap();
            assert_eq!(second.mailbox_id, first_wake.as_ref().unwrap().mailbox_id);
            expected_sequence = Some(second.sequence);
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert_eq!(record.queued_prompts.len(), 1);
            assert_eq!(record.queued_prompts[0].pending_prompt.id, prompt);
            assert!(record.queued_prompts[0].pending_prompt.text.contains("reset-current-topic"));
        } else {
            state.cancel_queued_prompt(&session, &prompt).unwrap();
        }
    }
    let new_home = root.join("reset-home");
    fs::create_dir_all(&new_home).unwrap();
    settings.home = Some(new_home.to_string_lossy().into_owned());
    let caller_gate = BindDispositionGate::new(&state, &session, if withdrawal { "withdrawal" } else { "caller" });
    let reset_gate = gate_next_engram_project_reset_fence(&project);
    std::thread::scope(|scope| {
        let delivery = scope.spawn(|| deliver_turn_dispatch(&state, dispatch));
        let before_reset = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| caller_gate.wait()));
        if let Err(error) = before_reset {
            drop(caller_gate);
            drop(reset_gate);
            let _ = delivery.join();
            std::panic::resume_unwind(error);
        }
        let reset = scope.spawn(|| {
            let result = state.update_project_engram_settings(&project, settings);
            abort_engram_project_reset_fence_gate(&project, &fence_gate_detail(&result));
            result
        });
        let premise = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            reset_gate.wait_until_entered();
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(record.engram.project_reset_in_progress);
            assert_ne!(record.engram.dispatch_generation, original_generation);
            assert_eq!(record.active_turn_generation, original_turn);
            assert!(record.runtime.matches_runtime_token(&runtime));
            assert_eq!(record.session.status, SessionStatus::Active);
            assert!(record.engram.pending_dispatch.is_none());
            assert!(record.engram.active_grant_id.is_none());
            assert!(record.engram.uncertain_grant_id.is_none());
            assert!(receiver.try_recv().is_err());
        }));
        caller_gate.release();
        let outcome = delivery.join();
        let fenced_status = {
            let inner = state.inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&session).unwrap()].session.status
        };
        let fenced_public = state.summary_snapshot().sessions.into_iter()
            .find(|record| record.id == session).expect("public session remains");
        let fenced_saved: PersistedSessionRecord = serde_json::from_str(&persisted_session_json(&state, &session)).unwrap();
        let no_provider_while_fenced = receiver.try_recv().is_err();
        reset_gate.release();
        let reset_result = reset.join();
        if let Err(error) = premise { std::panic::resume_unwind(error); }
        let outcome = outcome.expect("delivery worker should settle");
        reset_result.expect("reset worker should settle").expect("real settings reset should finish");
        assert!(no_provider_while_fenced);
        assert_eq!(fenced_status, SessionStatus::Error, "same-owned invalidated admission must reject at the actual caller: {outcome:?}");
        assert_eq!(fenced_public.status, SessionStatus::Error);
        assert!(fenced_public.live_activity.is_none());
        assert_eq!(fenced_saved.session.status, SessionStatus::Error);
        assert!(fenced_saved.session.live_activity.is_none());
        assert_eq!(fenced_saved.queued_prompts.iter().all(|head| head.pending_prompt.id != prompt), cancelled || delegation_child);
        assert!(matches!(outcome, TurnDispatchDeliveryOutcome::Rejected(_)));
        if let Some((parent, id)) = &delegation {
            state.sync_delegation_attempt_for_child_session(&session);
            let public = serde_json::to_value(state.get_delegation(parent, id).unwrap()).unwrap();
            assert_eq!(public["delegation"]["status"], "failed", "ordinary reset-invalidated followup takes the existing terminal rejection path: {public}");
            assert_eq!(public["delegation"]["result"]["status"], "failed");
            assert!(fenced_saved.queued_prompts.is_empty());
            let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
            let stored: String = connection.query_row("SELECT value_json FROM delegations WHERE id = ?1", [id], |row| row.get(0)).unwrap();
            let stored: DelegationRecord = serde_json::from_str(&stored).unwrap();
            assert_eq!(stored.status, DelegationStatus::Failed);
            assert_eq!(stored.result.unwrap().status, DelegationStatus::Failed);
            let parent = state.get_session(parent).unwrap();
            assert!(parent.session.messages.iter().any(|message| matches!(message,
                Message::ParallelAgents { agents, .. } if agents.iter().any(|agent|
                    agent.id == *id && agent.source == ParallelAgentSource::Delegation && agent.status == ParallelAgentStatus::Error))));
        }
    });
    assert_eq!(transport.requests().iter().filter(|request| request.request["operation"] == "turn_begin").count(), usize::from(delegation_child));
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(!record.engram.project_reset_in_progress);
    assert_ne!(record.session.status, SessionStatus::Active, "reset release must not leave the undelivered owner stranded");
    if cancelled || delegation_child {
        assert!(record.queued_prompts.iter().all(|head| head.pending_prompt.id != prompt));
        assert!(record.queued_prompts.is_empty(), "cancelled wake must not be regenerated by rejection or reset release");
    } else {
        assert_eq!(record.queued_prompts.front().unwrap().pending_prompt.id, prompt);
        assert!(record.queued_prompts.front().unwrap().is_engram_retained(), "Prepared prompt must remain explicitly recoverable after reset release");
        assert!(record.queued_prompts.front().unwrap().engram_waiting || record.queued_prompts.front().unwrap().engram_interrupted,
            "Prepared reset-release recovery must be an explicit hold, not merely a retained wire record: {:?}", record.queued_prompts);
        assert!(record.session.queue_paused);
        if let Some(sequence) = expected_sequence {
            assert_eq!(record.queued_prompts.len(), 1, "reset settlement must not add a stale mailbox wake");
            let current = &record.queued_prompts[0].pending_prompt;
            assert_eq!(current.source.as_ref().unwrap().mailbox.as_ref().unwrap().sequence, sequence);
            assert!(current.text.contains("reset-current-topic"));
            assert!(!current.text.contains("reset-original-topic"));
        }
    }
}

#[test]
fn bind_invalidated_caller_preserves_newer_turn_stop_owner_and_held_state() {
    for change in ["turn", "runtime", "stop-owner", "held"] {
        let (state, session, receiver, transport) = root_fixture([bind_failure()]);
        let dispatch = root_dispatch(&state, &session, false);
        let runtime = dispatch.runtime_token().clone();
        let turn = dispatch.active_turn_generation();
        let gate = BindDispositionGate::new(&state, &session, "caller");
        std::thread::scope(|scope| {
            let delivery = scope.spawn(|| deliver_turn_dispatch(&state, dispatch));
            let control = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                gate.wait();
                let stop_owner = if change == "stop-owner" {
                    Some(state.claim_turn_terminalization_if_runtime_matches(&session, &runtime, turn).unwrap().unwrap())
                } else { None };
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&session).unwrap();
                let record = inner.session_mut_by_index(index).unwrap();
                match change {
                    "turn" => record.active_turn_generation += 1,
                    "runtime" => {
                        let SessionRuntime::Codex(handle) = &mut record.runtime else { panic!("fixture runtime is Codex"); };
                        handle.runtime_id.push_str("-successor");
                    }
                    "held" => {
                        record.session.status = SessionStatus::Idle;
                        record.set_auto_dispatch_blocked(true);
                    }
                    _ => {}
                }
                state.commit_locked(&mut inner).unwrap();
                let record = &inner.sessions[index];
                (serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap(), record.active_turn_generation,
                    record.runtime.matches_runtime_token(&runtime), stop_owner)
            }));
            gate.release();
            let outcome = delivery.join().expect("guarded delivery finishes");
            let (before, generation, runtime_before, stop_owner) = control.unwrap_or_else(|error| std::panic::resume_unwind(error));
            assert!(matches!(outcome, TurnDispatchDeliveryOutcome::Superseded));
            {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                assert_eq!(serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap(), before, "stale caller must not mutate {change}");
                assert_eq!(record.active_turn_generation, generation);
                assert_eq!(record.runtime.matches_runtime_token(&runtime), runtime_before);
                if change == "runtime" {
                    let SessionRuntime::Codex(handle) = &record.runtime else { panic!("successor runtime must remain"); };
                    assert!(handle.runtime_id.ends_with("-successor"));
                }
                if let Some(owner) = stop_owner {
                    assert!(record.runtime_stop_is_owned_by(RuntimeStopOwnerKind::LostRuntimeTerminalization, &runtime, owner));
                }
            }
            if let Some(owner) = stop_owner { state.release_turn_terminalization_if_owned(&session, &runtime, owner); }
        });
        assert!(receiver.try_recv().is_err());
        assert_eq!(transport.requests().len(), 1);
    }
}

#[test]
fn bind_invalidated_caller_settles_before_actual_immediate_teardown() {
    bind_reset_failure_or_teardown_case(false);
}

#[test]
fn bind_invalidated_caller_settles_after_actual_settings_persistence_failure() {
    bind_reset_failure_or_teardown_case(true);
}

fn bind_reset_failure_or_teardown_case(persist_failure: bool) {
    let (state, session, receiver, transport) = root_fixture([bind_failure()]);
    let (project, old_settings, root) = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let project = inner.sessions[index].session.project_id.clone().unwrap();
        let project_record = inner.projects.iter_mut().find(|p| p.id == project).unwrap();
        let root = PathBuf::from(&project_record.root_path);
        fs::write(root.join(".engram-project"), "fixture-ok\n").unwrap();
        let settings = project_record.engram.as_mut().unwrap();
        settings.binary_path = Some(real_engram_control_fixture_path().to_string_lossy().into_owned());
        (project, settings.clone(), root)
    };
    state.shutdown_persist_blocking();
    let dispatch = root_dispatch(&state, &session, false);
    let generation = dispatch.engram_dispatch_generation().unwrap();
    let turn = dispatch.active_turn_generation();
    let runtime = dispatch.runtime_token().clone();
    let mut reset_state = state.clone();
    let mut settings = EngramProjectSettings::default();
    if persist_failure {
        let failed_path = root.join("failed-settings-store.sqlite");
        fs::create_dir_all(&failed_path).unwrap();
        reset_state.persistence_path = Arc::new(failed_path);
        let new_home = root.join("failed-reset-home");
        fs::create_dir_all(&new_home).unwrap();
        settings = old_settings.clone();
        settings.home = Some(new_home.to_string_lossy().into_owned());
    }
    let caller_gate = BindDispositionGate::new(&state, &session, "caller");
    let reset_gate = gate_next_engram_project_reset_fence(&project);
    std::thread::scope(|scope| {
        let delivery = scope.spawn(|| deliver_turn_dispatch(&state, dispatch));
        if let Err(error) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| caller_gate.wait())) {
            drop(caller_gate);
            drop(reset_gate);
            let _ = delivery.join();
            std::panic::resume_unwind(error);
        }
        let reset = scope.spawn(|| {
            let result = reset_state.update_project_engram_settings(&project, settings);
            abort_engram_project_reset_fence_gate(&project, &fence_gate_detail(&result));
            result
        });
        let premise = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            reset_gate.wait_until_entered();
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(record.engram.project_reset_in_progress);
            assert_ne!(record.engram.dispatch_generation, generation);
            assert_eq!(record.active_turn_generation, turn);
            assert!(record.runtime.matches_runtime_token(&runtime));
            assert_eq!(record.session.status, SessionStatus::Active);
            assert!(!record.runtime_stop_in_progress, "project fence precedes the runtime Stop owner");
            assert!(receiver.try_recv().is_err());
            serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap()
        }));
        if persist_failure {
            reset_gate.release();
            let result = reset.join();
            caller_gate.release();
            let outcome = delivery.join();
            let before = premise.unwrap_or_else(|error| std::panic::resume_unwind(error));
            assert_eq!(before["session"]["status"], "active");
            let error = match result.expect("failing reset worker finishes") {
                Err(error) => error,
                Ok(_) => panic!("directory-backed settings store must reject the real commit"),
            };
            assert!(error.message.contains("failed to persist Engram project settings"));
            let outcome = outcome.expect("invalidated caller finishes");
            assert!(matches!(outcome, TurnDispatchDeliveryOutcome::Rejected(_)));
            let inner = state.inner.lock().unwrap();
            assert_eq!(inner.find_project(&project).unwrap().engram, Some(old_settings));
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert_eq!(record.session.status, SessionStatus::Error);
            assert!(matches!(record.runtime, SessionRuntime::None));
            assert!(!record.engram.project_reset_in_progress);
            drop(inner);
            let summary = state.summary_snapshot().sessions.into_iter().find(|record| record.id == session).unwrap();
            let detail = state.get_session(&session).unwrap().session;
            let saved: PersistedSessionRecord = serde_json::from_str(&persisted_session_json(&state, &session)).unwrap();
            assert_eq!(summary.status, SessionStatus::Error);
            assert_eq!(detail.status, SessionStatus::Error);
            assert_eq!(saved.session.status, SessionStatus::Error);
            assert!(summary.live_activity.is_none());
            assert!(detail.live_activity.is_none());
            assert!(saved.session.live_activity.is_none());
        } else {
            caller_gate.release();
            let outcome = delivery.join();
            let after = {
                let inner = state.inner.lock().unwrap();
                serde_json::to_value(PersistedSessionRecord::from_record(&inner.sessions[inner.find_session_index(&session).unwrap()])).unwrap()
            };
            let fenced_public = state.summary_snapshot().sessions.into_iter().find(|record| record.id == session).unwrap();
            reset_gate.release();
            let result = reset.join().expect("teardown reset worker finishes");
            assert_eq!(premise.unwrap_or_else(|error| std::panic::resume_unwind(error))["session"]["status"], "active");
            let outcome = outcome.expect("reset-invalidated caller finishes before teardown");
            assert!(matches!(outcome, TurnDispatchDeliveryOutcome::Rejected(_)));
            assert_eq!(after["session"]["status"], "error");
            assert_eq!(fenced_public.status, SessionStatus::Error);
            assert!(fenced_public.live_activity.is_none());
            result.expect("immediate disabling reset completes");
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(matches!(record.runtime, SessionRuntime::None));
            assert_ne!(record.session.status, SessionStatus::Active);
            assert!(!record.engram.project_reset_in_progress);
            drop(inner);
            assert!(state.get_session(&session).unwrap().session.live_activity.is_none());
        }
    });
    assert!(receiver.try_recv().is_err());
    assert!(!transport.requests().iter().any(|request| request.request["operation"] == "turn_begin"));
}

fn acknowledge_bind_retry(state: &AppState, session: &str) {
    abort_retry::await_settlement_acknowledgement(state, session);
    state.engram_abort_retry_tick(chrono::Utc::now());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    assert!(record.engram.bind_retry.as_ref().unwrap().acknowledged);
}

fn bind_failure() -> ScriptedEngramControlResponse {
    ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline("bind reply lost")))
}

fn require_unprepared_owned_bind_dispatch(state: &AppState, session: &str, dispatch: &TurnDispatch) {
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    let pending = record.engram.pending_dispatch.as_ref().unwrap();
    let EngramDispatchEvaluation::RetainedBindFailure { proof, .. } = &pending.evaluated else {
        panic!("real bind backoff must establish typed retained proof before promotion returns");
    };
    assert_eq!(proof.phase, EngramBindRetryPhase::BeforePreparation);
    assert_eq!(record.session.status, SessionStatus::Active);
    assert!(record.runtime.matches_runtime_token(dispatch.runtime_token()));
    assert_eq!(record.active_turn_generation, dispatch.active_turn_generation());
    assert!(record.engram.admission_in_progress.is_none());
    assert!(record.engram.active_grant_id.is_none());
    assert!(record.engram.uncertain_grant_id.is_none());
    assert!(record.queued_prompts[0].engram_evaluate.is_none());
    assert!(!record.queued_prompts[0].is_engram_retained());
}

#[test]
fn bind_returned_dispatch_real_mailbox_coalescing_settles_and_admits_only_new_content() {
    let (mut state, session, receiver, transport) = root_fixture([
        bind_reply("fresh-bind"), grant_reply("fresh-grant"), begin_reply("fresh-grant"),
    ]);
    state.mailbox_store = Arc::new(MailboxStore::open(
        &resolve_coordination_persistence_path(state.persistence_path.as_ref()),
    ).unwrap());
    let sender = test_session_id(&state, Agent::Claude);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].set_auto_dispatch_blocked(true);
    }
    arm_bind_backoff(&state, &session, Duration::from_secs(3600));
    let send = |key: &str, topic: &str| SendMailboxMessageRequest {
        target_session_id: session.clone(), message: "durable body only".to_owned(),
        idempotency_key: key.to_owned(), topic: Some(topic.to_owned()), state_stamp: None,
        class: Some("routine".to_owned()),
    };
    let first = state.append_mailbox_message_and_notify(&sender, send("first-bind-wake", "original-topic")).unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].set_auto_dispatch_blocked(false);
    }
    let dispatch = state.start_next_queued_turn_off_lock(&session, false, false).unwrap().unwrap().dispatch;
    require_unprepared_owned_bind_dispatch(&state, &session, &dispatch);
    let (original_id, original_index, original_generation) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        (record.queued_prompts[0].pending_prompt.id.clone(), record.queued_prompts[0].promoted_message_index.unwrap(), record.engram.dispatch_generation)
    };
    let second = state.append_mailbox_message_and_notify(&sender, send("second-bind-wake", "new-topic")).unwrap();
    assert_eq!(first.mailbox_id, second.mailbox_id);
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(record.queued_prompts[0].pending_prompt.id, original_id);
        let source = record.queued_prompts[0].pending_prompt.source.as_ref().unwrap().mailbox.as_ref().unwrap();
        assert_eq!(source.sequence, second.sequence);
        assert!(record.queued_prompts[0].pending_prompt.text.contains("new-topic"));
    }
    assert!(transport.requests().is_empty());
    assert!(receiver.try_recv().is_err());
    let mut body_events = state.subscribe_delta_events();
    let outcome = deliver_turn_dispatch(&state, dispatch);
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_ne!(record.session.status, SessionStatus::Active, "same owned coalesced head must not strand its undelivered turn Active");
        assert!(record.engram.pending_dispatch.is_none());
        let rows: Vec<_> = record.session.messages.iter().filter(|message| message.id() == original_id).collect();
        assert_eq!(rows.len(), 1, "withdrawal must preserve one prompt identity");
        let Message::Text { text, expanded_text, attachments, source, .. } = rows[0] else { panic!("promoted prompt must stay text"); };
        let current = &record.queued_prompts[0];
        assert_eq!(text, &current.pending_prompt.text, "coalesced transcript payload must equal current undelivered prompt");
        assert_eq!(expanded_text, &current.pending_prompt.expanded_text);
        assert_eq!(serde_json::to_value(attachments).unwrap(), serde_json::to_value(&current.pending_prompt.attachments).unwrap());
        assert_eq!(source, &current.pending_prompt.source);
        assert!(record.engram.bind_retry.is_some(), "one ordinary continuation must establish fresh backoff proof, not a manual hold: outcome={outcome:?}, blocked={}, stop={}, preview={}, queue={:?}, engram={:?}", record.orchestrator_auto_dispatch_blocked, record.runtime_stop_in_progress, record.session.preview, record.queued_prompts, record.engram);
        let retry = record.engram.bind_retry.as_ref().unwrap();
        assert!(retry.proof.dispatch_generation > original_generation);
        assert_eq!(retry.proof.promotion_index, Some(original_index));
        assert_eq!(current.promoted_message_index, Some(original_index));
        assert!(current.promotion_disposition_known);
        assert_eq!(global_message_index(record, cached_message_index_on_record(record, &original_id).unwrap()), original_index);
        assert!(record.queued_prompts[0].pending_prompt.text.contains("new-topic"));
    }
    let mut published_update = false;
    while let Ok(payload) = body_events.try_recv() {
        if let DeltaEvent::MessageUpdated { message_id, message_index, message, session_seq, body_seq_epoch, .. } = serde_json::from_str(&payload).unwrap() {
            if message_id == original_id {
                assert_eq!(message_index, original_index);
                assert!(session_seq.is_some());
                assert_eq!(body_seq_epoch.as_deref(), Some(state.server_instance_id.as_str()));
                let Message::Text { text, .. } = message else { panic!("prompt update must be text"); };
                assert!(text.contains("new-topic"));
                published_update = true;
            }
        }
    }
    assert!(published_update, "payload correction uses the paired body publication path");
    assert!(transport.requests().is_empty());
    assert!(receiver.try_recv().is_err());
    acknowledge_bind_retry(&state, &session);
    elapse_bind_backoff(&state, &session);
    state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::hours(2));
    let CodexRuntimeCommand::Prompt { command, .. } = receiver.try_recv().unwrap() else { panic!("fresh head must deliver"); };
    assert!(command.prompt.contains("new-topic"));
    assert!(!command.prompt.contains("original-topic"));
    assert!(receiver.try_recv().is_err());
}

#[test]
fn bind_withdrawal_retires_only_its_exact_known_unprepared_promotion() {
    for change in ["exact", "different-marker", "turn", "retained"] {
        let (state, session, receiver, transport) = root_fixture([]);
        arm_bind_backoff(&state, &session, Duration::from_secs(3600));
        let dispatch = root_dispatch(&state, &session, false);
        require_unprepared_owned_bind_dispatch(&state, &session, &dispatch);
        let EngramTurnDeliveryPreparation::RetainedBindRetry(proof) = state.prepare_engram_turn_delivery_off_lock(&session, dispatch.engram_dispatch_generation().unwrap()) else { panic!("typed failure required"); };
        let original_index = proof.promotion_index.unwrap();
        let expected_marker = if change == "different-marker" { original_index + 1 } else { original_index };
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            record.queued_prompts[0].pending_prompt.text = "current undelivered content".to_owned();
            record.queued_prompts[0].promoted_message_index = Some(expected_marker);
            if change == "turn" { record.active_turn_generation += 1; }
            if change == "retained" { record.queued_prompts[0].engram_interrupted = true; }
        }
        let outcome = state.park_engram_bind_retry(&session, proof, dispatch.runtime_token(), dispatch.active_turn_generation());
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        let head = &record.queued_prompts[0];
        if change == "exact" {
            assert_eq!(outcome, EngramAuthorizationParkOutcome::Withdrawn);
            assert_eq!(head.promoted_message_index, None, "retirement precedes any fresh admission");
            assert!(head.promotion_disposition_known);
            assert_eq!(record.session.status, SessionStatus::Idle);
        } else {
            assert_eq!(head.promoted_message_index, Some(expected_marker), "independent promotion/retention remains intact");
        }
        assert!(record.engram.pending_dispatch.is_none());
        assert!(record.engram.bind_retry.is_none());
        assert!(transport.requests().is_empty());
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn bind_returned_dispatch_real_cancel_empty_queue_settles_without_restoring_prompt() {
    bind_returned_dispatch_cancel_case(false);
}

#[test]
fn bind_returned_dispatch_real_cancel_keeps_successor_and_admits_it_fresh() {
    bind_returned_dispatch_cancel_case(true);
}

fn bind_returned_dispatch_cancel_case(successor: bool) {
    let (state, session, receiver, transport) = root_fixture([
        bind_reply("fresh-bind"), grant_reply("fresh-grant"), begin_reply("fresh-grant"),
    ]);
    arm_bind_backoff(&state, &session, Duration::from_secs(3600));
    let dispatch = root_dispatch(&state, &session, false);
    require_unprepared_owned_bind_dispatch(&state, &session, &dispatch);
    let original = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session).unwrap()].queued_prompts[0].pending_prompt.id.clone()
    };
    if successor {
        assert!(matches!(state.dispatch_turn(&session, SendMessageRequest {
            text: "independent successor".to_owned(), expanded_text: None, attachments: Vec::new(),
            source_session_id: None, source_mailbox: None,
        }).unwrap(), DispatchTurnResult::Queued));
    }
    state.cancel_queued_prompt(&session, &original).unwrap();
    let _outcome = deliver_turn_dispatch(&state, dispatch);
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_ne!(record.session.status, SessionStatus::Active, "real non-admission Cancel must settle the still-owned undelivered turn");
        assert!(record.engram.pending_dispatch.is_none());
        assert!(record.queued_prompts.iter().all(|head| head.pending_prompt.id != original));
        assert_eq!(record.queued_prompts.len(), usize::from(successor));
        assert_eq!(record.engram.bind_retry.is_some(), successor);
    }
    assert!(transport.requests().is_empty());
    assert!(receiver.try_recv().is_err());
    if successor {
        acknowledge_bind_retry(&state, &session);
        elapse_bind_backoff(&state, &session);
        state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::hours(2));
        let CodexRuntimeCommand::Prompt { command, .. } = receiver.try_recv().unwrap() else { panic!("fresh successor must deliver"); };
        assert!(command.prompt.contains("independent successor"));
        assert!(!command.prompt.contains("Root controlled turn"));
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn bind_park_is_selected_above_the_already_saved_dispatch_card_watermark() {
    let (mut state, session, receiver, _) = root_fixture([
        bind_failure(),
        bind_reply("bound"),
        grant_reply("grant"),
        begin_reply("grant"),
    ]);
    let dispatch = root_dispatch(&state, &session, false);
    let generation = dispatch.engram_dispatch_generation().unwrap();
    let runtime = dispatch.runtime_token().clone();
    let turn = dispatch.active_turn_generation();
    state.shutdown_persist_blocking();
    let (tx, rx) = mpsc::channel();
    state.persist_tx = tx;
    let EngramTurnDeliveryPreparation::RetainedBindRetry(proof) =
        state.prepare_engram_turn_delivery_off_lock(&session, generation)
    else {
        panic!("real failed bind must retain typed proof");
    };
    let mut cache = SqlitePersistConnectionCache::new();
    let mut batch = PersistFenceBatch::default();
    let card = collect_persist_delta_from_shared_state(&state.inner, 0);
    let mut watermark = card.watermark;
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &card,
        &mut batch,
    )
    .unwrap();
    assert_eq!(
        state.park_engram_bind_retry(&session, proof, &runtime, turn),
        EngramAuthorizationParkOutcome::Parked
    );
    let parked = collect_persist_delta_from_shared_state(&state.inner, watermark);
    assert!(
        parked
            .changed_sessions
            .iter()
            .any(|saved| saved.session.id == session && saved.engram_bind_retry.is_some()),
        "the parked journal must be selected after the card watermark was consumed"
    );
    watermark = parked.watermark;
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &parked,
        &mut batch,
    )
    .unwrap();
    while let Ok(request) = rx.try_recv() {
        batch.accept(request);
    }
    let ack = collect_persist_delta_from_shared_state(&state.inner, watermark);
    watermark = ack.watermark;
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &ack,
        &mut batch,
    )
    .unwrap();
    let saved: PersistedSessionRecord =
        serde_json::from_str(&persisted_session_json(&state, &session)).unwrap();
    assert!(saved.engram_bind_retry.is_some());
    state.engram_abort_retry_tick(chrono::Utc::now());
    {
        let inner = state.inner.lock().unwrap();
        assert!(
            inner.sessions[inner.find_session_index(&session).unwrap()]
                .engram
                .bind_retry
                .as_ref()
                .unwrap()
                .acknowledged
        );
    }
    elapse_bind_backoff(&state, &session);
    let worker = state.clone();
    let task = std::thread::spawn(move || {
        worker.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::seconds(10))
    });
    let guard = phase_sync::PollGuard::new();
    loop {
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(request) => {
                batch.accept(request);
                let delta = collect_persist_delta_from_shared_state(&state.inner, watermark);
                watermark = delta.watermark;
                persist_delta_with_fences(
                    &mut cache,
                    state.persistence_path.as_path(),
                    &delta,
                    &mut batch,
                )
                .unwrap();
            }
            Err(mpsc::RecvTimeoutError::Timeout) if task.is_finished() => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                guard.wait(format_args!("incremental writer must settle the retry"))
            }
            Err(error) => panic!("writer disconnected: {error}"),
        }
    }
    task.join().unwrap();
    assert!(matches!(
        receiver.try_recv(),
        Ok(CodexRuntimeCommand::Prompt { .. })
    ));
    assert!(receiver.try_recv().is_err());
    state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::seconds(100));
    assert!(receiver.try_recv().is_err());
}

#[test]
fn invalid_bind_proof_holds_its_owned_prepared_turn_but_never_a_newer_turn() {
    for change in ["settings", "disabled", "grant", "head", "content", "turn"] {
        let successor = change == "turn";
        let (state, session, receiver, _) = root_fixture([bind_failure()]);
        let dispatch = root_dispatch(&state, &session, false);
        let EngramTurnDeliveryPreparation::RetainedBindRetry(proof) = state
            .prepare_engram_turn_delivery_off_lock(
                &session,
                dispatch.engram_dispatch_generation().unwrap(),
            )
        else {
            panic!("failed bind proof");
        };
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            if change == "head" {
                inner.sessions[index].queued_prompts[0]
                    .pending_prompt
                    .id
                    .push_str("-successor");
            } else if change == "content" {
                inner.sessions[index].queued_prompts[0]
                    .pending_prompt
                    .text
                    .push_str(" different intent");
            } else if change == "turn" {
                inner.sessions[index].active_turn_generation += 1;
            } else if change == "grant" {
                inner.sessions[index].engram.uncertain_grant_id = Some("appeared-grant".to_owned());
            } else if change == "disabled" {
                inner.sessions[index].engram.disabled_reason = Some("control_disabled".to_owned());
            } else {
                let project = inner.sessions[index].session.project_id.clone().unwrap();
                inner
                    .projects
                    .iter_mut()
                    .find(|p| p.id == project)
                    .unwrap()
                    .engram
                    .as_mut()
                    .unwrap()
                    .deadline_ms = Some(777);
            }
        }
        let outcome = state.park_engram_bind_retry(
            &session,
            proof,
            dispatch.runtime_token(),
            dispatch.active_turn_generation(),
        );
        assert_eq!(
            outcome,
            if successor {
                EngramAuthorizationParkOutcome::Superseded
            } else {
                EngramAuthorizationParkOutcome::Parked
            }
        );
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.engram.bind_retry.is_none());
        assert_eq!(
            record.session.status,
            if successor {
                SessionStatus::Active
            } else {
                SessionStatus::Idle
            }
        );
        assert_eq!(record.queued_prompts[0].engram_waiting, !successor);
        assert_eq!(
            record.engram.uncertain_grant_id.as_deref(),
            (change == "grant").then_some("appeared-grant")
        );
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn a_stale_bind_journal_cannot_mark_a_successor_on_stop_or_restore() {
    for restore in [false, true] {
        let (state, session, _, _) = root_fixture([bind_failure()]);
        deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
        let cancelled = {
            let inner = state.inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&session).unwrap()].queued_prompts[0]
                .pending_prompt
                .id
                .clone()
        };
        state.cancel_queued_prompt(&session, &cancelled).unwrap();
        queue_test_engram_prompt(
            &state,
            &session,
            "innocent successor",
            QueuedPromptSource::User,
            None,
        );
        let mut saved = {
            let inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            assert!(
                inner.sessions[index].engram.bind_retry.is_some(),
                "exercise the actual cancelled journal gap"
            );
            PersistedSessionRecord::from_record(&inner.sessions[index])
        };
        if restore {
            saved.queued_prompts[0].promotion_disposition_known = true;
            let restored = saved.into_record().unwrap();
            assert!(!restored.queued_prompts[0].engram_waiting);
            assert!(!restored.queued_prompts[0].engram_interrupted);
        } else {
            assert!(!state.stop_engram_abort_retry(&session).unwrap());
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(record.engram.bind_retry.is_none());
            assert!(!record.queued_prompts[0].engram_interrupted);
        }
    }
}

#[test]
fn a_tick_between_card_finish_and_repark_preserves_retry_attempt_lineage() {
    let (state, session, _, _) = root_fixture([bind_failure(), bind_failure()]);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    acknowledge_bind_retry(&state, &session);
    elapse_bind_backoff(&state, &session);
    let owner = {
        let inner = state.inner.lock().unwrap();
        EngramQueuedAdmissionOwner::capture(
            &inner.sessions[inner.find_session_index(&session).unwrap()],
        )
        .unwrap()
    };
    let dispatch = state
        .dispatch_next_queued_turn_for_bind_retry(&session, owner)
        .unwrap()
        .unwrap();
    let EngramTurnDeliveryPreparation::RetainedBindRetry(proof) = state
        .prepare_engram_turn_delivery_off_lock(
            &session,
            dispatch.engram_dispatch_generation().unwrap(),
        )
    else {
        panic!("second failure proof");
    };
    state.engram_abort_retry_tick(chrono::Utc::now());
    assert_eq!(
        state.park_engram_bind_retry(
            &session,
            proof,
            dispatch.runtime_token(),
            dispatch.active_turn_generation()
        ),
        EngramAuthorizationParkOutcome::Parked
    );
    let inner = state.inner.lock().unwrap();
    assert_eq!(
        inner.sessions[inner.find_session_index(&session).unwrap()]
            .engram
            .bind_retry
            .as_ref()
            .unwrap()
            .attempts,
        2
    );
}

#[test]
fn promotion_losing_optional_runtime_proof_holds_only_its_exact_attempt() {
    for successor in [false, true] {
        let (state, session, receiver, _) = root_fixture([bind_failure()]);
        queue_test_engram_prompt(
            &state,
            &session,
            "owned prompt",
            QueuedPromptSource::User,
            None,
        );
        let prepared = state
            .prepare_next_queued_turn_engram_off_lock(&session)
            .unwrap();
        assert!(matches!(
            prepared.pending_engram.as_ref().unwrap().evaluated,
            EngramDispatchEvaluation::RetainedBindFailure { .. }
        ));
        let (replacement, replacement_rx) = test_codex_runtime_handle("root-proof-loss");
        let started = {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            if successor {
                inner.sessions[index].queued_prompts[0]
                    .pending_prompt
                    .id
                    .push_str("-successor");
                inner.sessions[index].engram.dispatch_generation += 1;
            } else {
                inner.sessions[index].runtime = SessionRuntime::Codex(replacement);
            }
            let started = state
                .start_next_queued_turn_locked(&mut inner, index, false, prepared.pending_engram)
                .expect("proof loss is not a terminal start failure");
            state.commit_locked(&mut inner).unwrap();
            started
        };
        if successor {
            assert!(started.is_none());
        } else {
            assert!(matches!(
                deliver_turn_dispatch(&state, started.unwrap().dispatch),
                TurnDispatchDeliveryOutcome::Held { error: None }
            ));
        }
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.session.status, SessionStatus::Idle);
        assert!(record.engram.bind_retry.is_none());
        assert_eq!(record.queued_prompts[0].engram_interrupted, !successor);
        assert!(receiver.try_recv().is_err());
        assert!(replacement_rx.try_recv().is_err());
    }
}

#[test]
fn a_first_delegation_followup_losing_promotion_proof_remains_held_not_failed() {
    let (state, parent, receiver, _) = root_fixture([
        bind_reply("parent"),
        bind_reply("child"),
        grant_reply("initial"),
        begin_reply("initial"),
        checkpoint_reply("initial"),
        bind_failure(),
    ]);
    let created = state
        .create_read_only_delegation(
            &parent,
            CreateDelegationRequest {
                prompt: "initial".to_owned(),
                title: Some("proof-loss followup".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Explorer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .unwrap();
    let delegation = created.delegation.id;
    let child = created.delegation.child_session_id;
    assert!(receive_synchronous_engram_prompt(&state, &receiver, "initial child prompt").is_ok());
    let runtime = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&child).unwrap()]
            .runtime
            .runtime_token()
            .unwrap()
    };
    crate::tests::delegation_support::finish_delegation_child_with_assistant_text(
        &state,
        &child,
        "## Result\nStatus: completed\n\nSummary:\ninitial completed",
    );
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&child).unwrap();
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    state
        .finish_turn_ok_if_runtime_matches(&child, &runtime)
        .unwrap();
    state.refresh_delegation_for_child_session(&child).unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&child).unwrap();
        inner.sessions[index].engram.project_reset_in_progress = true;
    }
    state
        .followup_delegation(&parent, &delegation, "first followup".to_owned())
        .unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&child).unwrap();
        inner.sessions[index].engram.project_reset_in_progress = false;
        inner.sessions[index].engram.routing_token = None;
        inner.sessions[index].engram.rebind_required = true;
    }
    let prepared = state
        .prepare_next_queued_turn_engram_off_lock(&child)
        .unwrap();
    assert!(matches!(
        prepared.pending_engram.as_ref().unwrap().evaluated,
        EngramDispatchEvaluation::RetainedBindFailure { .. }
    ));
    let (replacement, replacement_rx) = test_codex_runtime_handle("followup-proof-loss");
    let started = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&child).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Codex(replacement);
        let started = state
            .start_next_queued_turn_locked(&mut inner, index, false, prepared.pending_engram)
            .expect("optional proof loss must not terminalize first followup")
            .unwrap();
        state.commit_locked(&mut inner).unwrap();
        started
    };
    let delivery = deliver_turn_dispatch(&state, started.dispatch);
    let response = state
        .delegation_status_after_followup_turn(&parent, &delegation, delivery)
        .unwrap();
    assert_eq!(
        response.turn.as_ref().unwrap().state,
        DelegationTurnDeliveryState::Held
    );
    assert_eq!(
        serde_json::to_value(&response).unwrap()["delegation"]["status"],
        "held"
    );
    assert!(response.delegation.result.is_none());
    assert!(replacement_rx.try_recv().is_err());
    assert!(receiver.try_recv().is_err());
}

#[test]
fn a_retained_mailbox_bind_uses_the_common_handoff_to_settle_its_one_wake() {
    let (base, session, receiver, _) = root_fixture([
        bind_failure(),
        bind_reply("bound"),
        grant_reply("grant"),
        begin_reply("grant"),
    ]);
    let path = resolve_coordination_persistence_path(base.persistence_path.as_ref());
    let state = AppState {
        mailbox_store: Arc::new(MailboxStore::open(&path).unwrap()),
        ..base
    };
    let sender = test_session_id(&state, Agent::Claude);
    let receipt = crate::tests::mailbox_acknowledged_wake::send(
        &state,
        &sender,
        &session,
        "retained-bind-wake",
    );
    assert!(receiver.try_recv().is_err());
    let prompt_id = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.queued_prompts.len(), 1);
        assert!(record.engram.bind_retry.is_some());
        record.queued_prompts[0].pending_prompt.id.clone()
    };
    acknowledge_bind_retry(&state, &session);
    elapse_bind_backoff(&state, &session);
    state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::seconds(10));
    assert!(matches!(
        receiver.try_recv(),
        Ok(CodexRuntimeCommand::Prompt { .. })
    ));
    assert!(receiver.try_recv().is_err());
    let message = state
        .mailbox_store
        .read_message(&session, &receipt.message_id)
        .unwrap();
    assert_eq!(message.notification_state, "deliveredToIdleSession");
    // The common accepted provider handoff owns the durable wake completion;
    // recovery cannot append another copy of the retained boundary afterward.
    state
        .reconcile_never_woken_mailbox_notifications_for_session(&session)
        .unwrap();
    state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::seconds(100));
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(record.queued_prompts.is_empty());
    assert_eq!(
        record
            .session
            .messages
            .iter()
            .filter(|message| message.id() == prompt_id)
            .count(),
        1
    );
    assert!(receiver.try_recv().is_err());
}

#[test]
fn a_transient_first_bind_failure_is_observed_parked_before_provider_handoff() {
    let (state, session, receiver, transport) =
        root_fixture([ScriptedEngramControlResponse::Reply(Err(
            EngramTransportError::deadline("first bind reply lost"),
        ))]);
    let dispatch = root_dispatch(&state, &session, false);
    deliver_turn_dispatch(&state, dispatch).expect("the original prompt is retained");
    assert!(
        receiver.try_recv().is_err(),
        "no prompt reached the provider"
    );
    assert_eq!(operations(&transport), ["session_bind"]);

    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    let head = record
        .queued_prompts
        .front()
        .expect("original prompt retained");
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(head.engram_waiting);
    assert!(!head.engram_interrupted);
    assert!(head.engram_bind.is_some());
    assert!(head.engram_evaluate.is_none());
    assert!(record.engram.active_grant_id.is_none());
    assert!(record.engram.uncertain_grant_id.is_none());
    assert!(record.engram.next_bind_retry_at.is_some());
    let prompt_id = head.pending_prompt.id.clone();
    state.persist_internal_locked(&inner).unwrap();
    drop(inner);
    let saved: PersistedSessionRecord =
        serde_json::from_str(&persisted_session_json(&state, &session)).unwrap();
    assert!(saved.orchestrator_auto_dispatch_blocked);
    assert!(saved.queued_prompts[0].engram_waiting);
    assert_eq!(saved.queued_prompts[0].pending_prompt.id, prompt_id);
}

#[test]
fn a_transient_first_bind_failure_retries_after_backoff_without_resume() {
    let (state, session, receiver, transport) = root_fixture([
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline(
            "first bind reply lost",
        ))),
        bind_reply("replayed-bind-token"),
        grant_reply("first-delivered-grant"),
        begin_reply("first-delivered-grant"),
    ]);
    let dispatch = root_dispatch(&state, &session, false);
    deliver_turn_dispatch(&state, dispatch).expect("the original prompt is retained");
    assert!(
        receiver.try_recv().is_err(),
        "first admission never delivered"
    );
    assert_eq!(operations(&transport), ["session_bind"]);
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(record.queued_prompts[0].engram_waiting);
    }
    // The backoff runs out on the scripted clock; no Resume, new prompt, or
    // forged delivery receipt is supplied.
    elapse_bind_backoff(&state, &session);

    let due = chrono::Utc::now() + chrono::Duration::seconds(10);
    state.engram_abort_retry_tick(due);
    abort_retry::await_settlement_acknowledgement(&state, &session);
    state.engram_abort_retry_tick(due);
    assert!(
        matches!(receiver.try_recv(), Ok(CodexRuntimeCommand::Prompt { .. })),
        "the existing retry tick must deliver the retained prompt without Resume"
    );
    assert!(receiver.try_recv().is_err(), "exactly one provider prompt");
    let requests = transport.requests();
    assert_eq!(
        requests[0].request, requests[1].request,
        "exact bind replay"
    );
    assert_eq!(
        operations(&transport),
        [
            "session_bind",
            "session_bind",
            "turn_evaluate",
            "turn_begin"
        ]
    );
}

#[test]
fn bind_retry_before_wire_preparation_keeps_the_explicit_phase() {
    let (state, session, receiver, _) = root_fixture([]);
    let transport = ScriptedEngramControlTransport::new_with_work_bindings(
        [
            bind_reply("bound"),
            grant_reply("grant"),
            begin_reply("grant"),
        ],
        [
            Err(EngramTransportError::deadline("work focus unavailable")),
            Ok(None),
        ],
    );
    state.install_control_test_transport(transport.clone());
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.queued_prompts[0].engram_bind.is_none());
        assert_eq!(
            record.engram.bind_retry.as_ref().unwrap().proof.phase,
            EngramBindRetryPhase::BeforePreparation
        );
    }
    assert!(transport.requests().is_empty());
    acknowledge_bind_retry(&state, &session);
    elapse_bind_backoff(&state, &session);
    state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::seconds(10));
    assert!(matches!(
        receiver.try_recv(),
        Ok(CodexRuntimeCommand::Prompt { .. })
    ));
    assert!(receiver.try_recv().is_err());
}

#[test]
fn repeated_bind_failures_keep_exact_wire_identity_and_obey_circuit_backoff() {
    let (state, session, receiver, transport) = root_fixture([
        bind_failure(),
        bind_failure(),
        bind_failure(),
        bind_reply("bound"),
        grant_reply("grant"),
        begin_reply("grant"),
    ]);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    for expected_attempts in 1..=3 {
        acknowledge_bind_retry(&state, &session);
        // The backoff is measured on the state's scripted clock, the one
        // elapse_bind_backoff advances, so it stays pending however fast
        // the iterations run.
        let clock_now = state.engram_budget_clock().now();
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            assert_eq!(
                record.engram.bind_retry.as_ref().unwrap().attempts,
                expected_attempts
            );
            record.engram.next_bind_retry_at = Some(clock_now + Duration::from_secs(60));
            if expected_attempts == 3 {
                assert!(record.engram.circuit_open);
            }
        }
        let due = chrono::Utc::now() + chrono::Duration::seconds(3600);
        state.engram_abort_retry_tick(due);
        assert_eq!(
            transport.requests().len(),
            expected_attempts as usize,
            "a due scheduler cannot bypass live bind/circuit backoff"
        );
        elapse_bind_backoff(&state, &session);
        state.engram_abort_retry_tick(due);
    }
    assert!(matches!(
        receiver.try_recv(),
        Ok(CodexRuntimeCommand::Prompt { .. })
    ));
    assert!(receiver.try_recv().is_err());
    let requests = transport.requests();
    for replay in &requests[1..4] {
        assert_eq!(requests[0].request, replay.request);
        assert_eq!(requests[0].connection, replay.connection);
    }
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(
        record.engram.bind_retry.is_none(),
        "evaluate retires bind-only scheduling"
    );
}

#[test]
fn restarted_bind_journal_never_reconstructs_live_non_delivery_proof() {
    let (state, session, receiver, transport) = root_fixture([bind_failure()]);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    acknowledge_bind_retry(&state, &session);
    let saved = {
        let inner = state.inner.lock().unwrap();
        PersistedSessionRecord::from_record(
            &inner.sessions[inner.find_session_index(&session).unwrap()],
        )
    };
    assert!(saved.engram_bind_retry.as_ref().unwrap().acknowledged);
    let saved_attempts = saved.engram_bind_retry.as_ref().unwrap().attempts;
    let mut loaded = saved.into_record().unwrap();
    assert!(loaded.engram.bind_retry.is_none());
    assert!(loaded.engram.bind_retry_runtime.is_none());
    // The acknowledged journal becomes the parked-admission retry of its
    // head (`engram_admission_retry.rs`), which replays the exact retained
    // bind through ordinary admission; no live non-delivery proof is made.
    let rebuilt = loaded.engram.admission_retry.as_ref().expect("rebuilt retry");
    assert!(rebuilt.acknowledged);
    assert_eq!(rebuilt.attempts, saved_attempts);
    assert!(loaded.queued_prompts[0].engram_waiting);
    assert!(
        loaded.engram.recovered_admission,
        "explicit cold recovery remains available"
    );
    assert!(loaded.orchestrator_auto_dispatch_blocked);
    assert_eq!(
        engram_bind_retry_step(
            &mut loaded,
            None,
            chrono::Utc::now() + chrono::Duration::seconds(3600),
            state.engram_budget_clock().now(),
        ),
        EngramAbortRetryStep::Wait
    );
    assert_eq!(operations(&transport), ["session_bind"]);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn stop_head_content_runtime_and_settings_changes_disarm_bind_retry() {
    for change in ["stop", "head", "content", "runtime", "settings"] {
        let (state, session, receiver, transport) = root_fixture([bind_failure()]);
        deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
        acknowledge_bind_retry(&state, &session);
        elapse_bind_backoff(&state, &session);
        if change == "stop" {
            state.request_stop_session(&session).unwrap();
        } else {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            match change {
                "head" => inner.sessions[index].queued_prompts[0]
                    .pending_prompt
                    .id
                    .push_str("-successor"),
                "content" => inner.sessions[index].queued_prompts[0]
                    .pending_prompt
                    .text
                    .push_str(" changed"),
                "runtime" => inner.sessions[index].clear_runtime(),
                "settings" => {
                    let project_id = engram_project_for_session_locked(&inner, &session)
                        .unwrap()
                        .id
                        .clone();
                    inner
                        .projects
                        .iter_mut()
                        .find(|project| project.id == project_id)
                        .unwrap()
                        .engram
                        .as_mut()
                        .unwrap()
                        .deadline_ms = Some(777);
                }
                _ => unreachable!(),
            }
        }
        state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::seconds(3600));
        assert_eq!(operations(&transport), ["session_bind"], "{change}");
        assert!(receiver.try_recv().is_err(), "{change}");
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.engram.bind_retry.is_none(), "{change}");
        assert!(record.orchestrator_auto_dispatch_blocked, "{change}");
    }
}

#[test]
fn an_unknown_evaluate_or_begin_never_becomes_a_bind_only_retry() {
    for lost in ["turn_evaluate", "turn_begin"] {
        let mut replies = vec![bind_failure(), bind_reply("bound")];
        if lost == "turn_begin" {
            replies.push(grant_reply("grant"));
        }
        replies.push(ScriptedEngramControlResponse::Reply(Err(
            EngramTransportError::deadline("authorization reply lost"),
        )));
        let (state, session, receiver, transport) = root_fixture(replies);
        deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
        acknowledge_bind_retry(&state, &session);
        elapse_bind_backoff(&state, &session);
        state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::seconds(3600));
        let requests_before_tick = transport.requests().len();
        state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::seconds(7200));
        assert_eq!(transport.requests().len(), requests_before_tick, "{lost}");
        assert!(receiver.try_recv().is_err(), "{lost}");
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.engram.bind_retry.is_none(), "{lost}");
        assert!(
            record.queued_prompts[0].engram_evaluate.is_some(),
            "unknown authority stays retained"
        );
        // A lost evaluate is the parked-admission retry's to replay with its
        // key (`engram_admission_retry.rs`); a possibly begun grant is not.
        assert_eq!(
            record.engram.admission_retry.is_some(),
            lost == "turn_evaluate",
            "{lost}"
        );
    }
}

#[test]
fn pending_and_failed_retry_acknowledgements_never_release_the_head() {
    let (mut state, session, receiver, transport) = root_fixture([
        bind_failure(),
        bind_reply("bound"),
        grant_reply("grant"),
        begin_reply("grant"),
    ]);
    let dispatch = root_dispatch(&state, &session, false);
    state.shutdown_persist_blocking();
    let (tx, rx) = mpsc::channel();
    state.persist_tx = tx;
    deliver_turn_dispatch(&state, dispatch).unwrap();
    let next_fence = || loop {
        if let PersistRequest::Fence(fence) = receive(&rx, "bind retry acknowledgement") {
            break fence;
        }
    };
    let fence = next_fence();
    assert!(
        matches!(&fence.target, PersistFenceTarget::EngramAdmission { content, .. }
        if !content["bindRetry"].is_null())
    );
    elapse_bind_backoff(&state, &session);
    let due = chrono::Utc::now() + chrono::Duration::seconds(10);
    state.engram_abort_retry_tick(due);
    assert_eq!(
        operations(&transport),
        ["session_bind"],
        "pending ACK is not authority"
    );
    fence.finish(Err(PersistFenceError::Deadline));
    state.engram_abort_retry_tick(due);
    assert_eq!(
        operations(&transport),
        ["session_bind"],
        "failed ACK is not authority"
    );
    let next = next_fence();
    // This small fault fixture simulates a successful fence completion; the
    // queued Delta below is not durable read-back. The separate incremental
    // writer witness exercises the actual journal write and acknowledgement.
    {
        let inner = state.inner.lock().unwrap();
        state.persist_internal_locked(&inner).unwrap();
    }
    next.finish(Ok(()));
    state.engram_abort_retry_tick(due);
    assert_eq!(
        operations(&transport),
        ["session_bind"],
        "acknowledgement alone does not dispatch"
    );
    // Restore the synchronous persistence fallback for the new admission's
    // ordinary bind/evaluate fences; no missing writer is treated as success.
    drop(rx);
    state.engram_abort_retry_tick(due);
    assert!(matches!(
        receiver.try_recv(),
        Ok(CodexRuntimeCommand::Prompt { .. })
    ));
    assert!(receiver.try_recv().is_err());
}

#[test]
fn a_due_tick_and_explicit_resume_share_one_admission_flight() {
    let (state, session, receiver, _) = root_fixture([]);
    let (replay, gate) = gated_engram_step("session_bind", bind_reply("bound"));
    let transport = GatedEngramControlTransport::new([
        immediate_engram_step("session_bind", bind_failure()),
        replay,
        immediate_engram_step("turn_evaluate", grant_reply("grant")),
        immediate_engram_step("turn_begin", begin_reply("grant")),
    ]);
    state.install_control_test_transport(transport.clone());
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    acknowledge_bind_retry(&state, &session);
    elapse_bind_backoff(&state, &session);
    let due = chrono::Utc::now() + chrono::Duration::seconds(10);
    std::thread::scope(|scope| {
        let tick = scope.spawn(|| state.engram_abort_retry_tick(due));
        gate.wait();
        state.resume_session_queue(&session).unwrap();
        state.engram_abort_retry_tick(due);
        assert_eq!(transport.requests().len(), 2, "one retained bind flight");
        assert!(receiver.try_recv().is_err());
        gate.release();
        tick.join().unwrap();
    });
    assert!(matches!(
        receiver.try_recv(),
        Ok(CodexRuntimeCommand::Prompt { .. })
    ));
    state.engram_abort_retry_tick(due);
    assert!(receiver.try_recv().is_err());
    assert_eq!(transport.requests().len(), 4);
}

#[test]
fn stop_or_cancel_during_bind_replay_prevents_late_delivery_and_rearming() {
    for stop in [true, false] {
        let (state, session, receiver, _) = root_fixture([]);
        let (replay, gate) = gated_engram_step("session_bind", bind_failure());
        let transport = GatedEngramControlTransport::new([
            immediate_engram_step("session_bind", bind_failure()),
            replay,
        ]);
        state.install_control_test_transport(transport.clone());
        deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
        acknowledge_bind_retry(&state, &session);
        elapse_bind_backoff(&state, &session);
        let prompt_id = {
            let inner = state.inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&session).unwrap()].queued_prompts[0]
                .pending_prompt
                .id
                .clone()
        };
        let due = chrono::Utc::now() + chrono::Duration::seconds(10);
        std::thread::scope(|scope| {
            let tick = scope.spawn(|| state.engram_abort_retry_tick(due));
            gate.wait();
            if stop {
                state.request_stop_session(&session).unwrap();
            } else {
                state.cancel_queued_prompt(&session, &prompt_id).unwrap();
            }
            gate.release();
            tick.join().unwrap();
        });
        state.engram_abort_retry_tick(due + chrono::Duration::seconds(3600));
        assert!(receiver.try_recv().is_err(), "{stop}");
        assert_eq!(transport.requests().len(), 2, "{stop}");
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(
            record.engram.bind_retry.is_none(),
            "late reply cannot rearm"
        );
        if stop {
            assert!(record.orchestrator_auto_dispatch_blocked);
            assert!(record.queued_prompts[0].engram_interrupted);
        } else {
            assert!(record.queued_prompts.is_empty());
        }
    }
}

#[test]
fn a_late_postponement_cannot_charge_a_newer_bind_attempt_for_the_same_head() {
    let (state, session, receiver, transport) = root_fixture([bind_failure(), bind_failure()]);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    acknowledge_bind_retry(&state, &session);
    let old_owner = {
        let inner = state.inner.lock().unwrap();
        EngramQueuedAdmissionOwner::capture(
            &inner.sessions[inner.find_session_index(&session).unwrap()],
        )
        .unwrap()
    };
    elapse_bind_backoff(&state, &session);
    let due = chrono::Utc::now() + chrono::Duration::seconds(10);
    state.engram_abort_retry_tick(due);
    acknowledge_bind_retry(&state, &session);
    let before = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(!old_owner.matches(record));
        assert_eq!(record.engram.bind_retry.as_ref().unwrap().attempts, 2);
        serde_json::to_value(record.engram.bind_retry.as_ref().unwrap()).unwrap()
    };
    state.postpone_engram_bind_retry(&session, &old_owner, due);
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(
        before,
        serde_json::to_value(record.engram.bind_retry.as_ref().unwrap()).unwrap()
    );
    assert_eq!(transport.requests().len(), 2);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn authority_ack_expiry_parks_then_the_existing_tick_delivers_without_resume() {
    for expire in [false, true] {
        let label = format!("bind-authority-ack-{expire}");
        let (mut state, session, receiver, _) = root_fixture([]);
        // The fixture's scripted clock, which every target and fence of this
        // state reads; advancing it is the only way time passes here.
        let clock = state.engram_budget_clock();
        let binding = test_control_work_binding(&label, 1);
        let transport = ScriptedEngramControlTransport::new_with_work_bindings(
            [
                bind_reply("bound"),
                grant_reply("grant"),
                begin_reply("grant"),
            ],
            [Ok(Some(binding.clone()))],
        );
        transport.register_named_root_run(&binding);
        state.install_control_test_transport(transport.clone());
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let project = inner.sessions[index].session.project_id.clone().unwrap();
            let root = PathBuf::from(&inner.sessions[index].session.workdir);
            inner
                .projects
                .iter_mut()
                .find(|p| p.id == project)
                .unwrap()
                .engram
                .as_mut()
                .unwrap()
                .authority_store_key = Some(EngramAuthorityStoreKey {
                database_path: root.join("engram.db"),
                project_id: "github.com/example/source-root".to_owned(),
            });
        }
        let initial_now = clock.now();
        state.shutdown_persist_blocking();
        let (tx, rx) = mpsc::channel();
        state.persist_tx = tx;
        let worker = state.clone();
        let worker_session = session.clone();
        let task = std::thread::spawn(move || {
            deliver_turn_dispatch(&worker, root_dispatch(&worker, &worker_session, false)).unwrap();
        });
        let mut authority_fences = 0;
        let mut writer_watermark = 0;
        let mut batch = PersistFenceBatch::default();
        let mut cache = SqlitePersistConnectionCache::new();
        let guard = phase_sync::PollGuard::new();
        loop {
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(request) => {
                    if let PersistRequest::Fence(fence) = &request {
                        if let PersistFenceTarget::EngramWorkAuthority(image) = &fence.target {
                            authority_fences += 1;
                            if authority_fences == 1 {
                                assert_eq!(
                                    image.history.transition.as_ref().unwrap().phase,
                                    EngramAuthorityPhase::Prepared
                                );
                                assert_eq!(clock.now(), initial_now);
                                assert!(
                                    transport.requests().is_empty(),
                                    "the real Prepared ACK precedes bind transmission"
                                );
                                assert!(receiver.try_recv().is_err());
                                if expire {
                                    // This is the operation's shared clock, not a transport
                                    // error stub or a second timing mechanism. The real
                                    // exact-image fence has not been acknowledged yet.
                                    let remaining = fence
                                        .completion
                                        .deadline
                                        .saturating_duration_since(clock.now());
                                    assert!(!remaining.is_zero());
                                    clock.advance(remaining + Duration::from_millis(1));
                                }
                            }
                        }
                    }
                    batch.accept(request);
                    let delta =
                        collect_persist_delta_from_shared_state(&state.inner, writer_watermark);
                    persist_delta_with_fences(
                        &mut cache,
                        state.persistence_path.as_path(),
                        &delta,
                        &mut batch,
                    )
                    .unwrap();
                    writer_watermark = delta.watermark;
                }
                Err(mpsc::RecvTimeoutError::Timeout) if task.is_finished() => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    guard.wait(format_args!(
                        "the root admission must finish through its writer"
                    ));
                }
                Err(error) => panic!("writer ended before admission settled: {error}"),
            }
        }
        task.join().unwrap();
        assert!(
            authority_fences > 0,
            "the actual authority fence was reached"
        );
        // Subsequent operations use the fixture's existing off-lock SQLite
        // fallback after the connected writer above has finished its real ACKs.
        drop(rx);
        if expire {
            assert_eq!(authority_fences, 1, "no later phase gets a fresh budget");
            assert!(transport.requests().is_empty());
            assert!(receiver.try_recv().is_err());
            let (prompt_id, prepared) = {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                assert!(record.orchestrator_auto_dispatch_blocked);
                let head = &record.queued_prompts[0];
                assert!(head.engram_waiting);
                assert!(head.engram_evaluate.is_none());
                assert!(record.engram.bind_retry.is_some());
                assert!(record.engram.active_grant_id.is_none());
                (
                    head.pending_prompt.id.clone(),
                    serde_json::to_value(&head.engram_bind.as_ref().unwrap().request).unwrap(),
                )
            };
            acknowledge_bind_retry(&state, &session);
            let due = chrono::Utc::now() + chrono::Duration::seconds(10);
            state.engram_abort_retry_tick(due);
            assert!(
                transport.requests().is_empty(),
                "a due UTC tick still respects the unelapsed live bind backoff"
            );
            // Advancing the shared clock is not a scheduler trigger.
            clock.advance(Duration::from_secs(1));
            assert!(receiver.try_recv().is_err());
            state.engram_abort_retry_tick(due);
            let requests = transport.requests();
            assert_eq!(
                requests
                    .iter()
                    .find(|r| r.request["operation"] == "session_bind")
                    .unwrap()
                    .request,
                prepared,
                "the original prepared bind is replayed exactly"
            );
            {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                assert!(record.queued_prompts.is_empty());
                assert!(record.engram.bind_retry.is_none());
                assert!(
                    record
                        .session
                        .messages
                        .iter()
                        .any(|message| message.id() == prompt_id),
                    "the original prompt was promoted once"
                );
            }
        } else {
            assert_eq!(
                clock.now(),
                initial_now,
                "unadvanced logical time is the positive ACK control"
            );
        }
        assert!(
            matches!(receiver.try_recv(), Ok(CodexRuntimeCommand::Prompt { .. })),
            "the real provider boundary receives one prompt, without Resume"
        );
        assert!(receiver.try_recv().is_err());
        state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::seconds(3600));
        assert!(receiver.try_recv().is_err());
    }
}
