//! Ordinary queue, Stop, restart and delivery boundaries preserve passive history.
//! Disposable SQLite, the real queue/wake owners and provider channels are used;
//! Observation transport is scripted; stored control JSON is never executable.
use super::*;

type OrdinaryRuntimeFixture = (
    AppState,
    String,
    mpsc::Receiver<CodexRuntimeCommand>,
    Arc<ScriptedEngramControlTransport>,
);

fn ordinary_root_fixture() -> OrdinaryRuntimeFixture {
    let (state, receiver) = test_app_state_with_delegation_codex_runtime("ordinary-queue-runtime");
    let root = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("ordinary-queue-project");
    fs::create_dir_all(&root).unwrap();
    let project = create_test_project(&state, &root, "Ordinary queue");
    let session = create_test_project_session(&state, Agent::Codex, &project, &root);
    enable_test_project_engram(&state, &project, &root);
    let transport = ScriptedEngramControlTransport::new([]);
    state.install_control_test_transport(transport.clone());
    (state, session, receiver, transport)
}

fn boundary_session(profile: &str) -> OrdinaryRuntimeFixture {
    let (state, session, receiver, transport) = ordinary_root_fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        let project_id = inner.sessions[inner.find_session_index(&session).unwrap()]
            .session
            .project_id
            .clone()
            .unwrap();
        let project = inner
            .projects
            .iter_mut()
            .find(|p| p.id == project_id)
            .unwrap();
        match profile {
            "plain" => project.engram = None,
            "base-off" => project.engram.as_mut().unwrap().execution_observability = false,
            "disabled" => project.engram.as_mut().unwrap().enabled = false,
            "on" => {}
            _ => panic!("unknown boundary profile"),
        }
    }
    (state, session, receiver, transport)
}

#[test]
fn off_boundary_global_public_stop_during_cleanup_keeps_its_pause() {
    for profile in ["plain", "on"] {
        let (state, session, _receiver, transport) = boundary_session(profile);
        {
            let mut inner = state.inner.lock().unwrap();
            let record = inner.session_mut(&session).unwrap();
            record.runtime = SessionRuntime::None;
            record.session.status = SessionStatus::Active;
        }
        let gate = install_test_stop_fence_gate(&state, &session);
        let options = StopSessionOptions {
            pause_automatic_resumes_on_success: false,
            ..Default::default()
        };
        let (_, claim) = state
            .begin_requested_stop_session(&session, &options)
            .unwrap();
        let claim = claim.expect("the first asynchronous Stop owns cleanup");
        std::thread::scope(|scope| {
            // Drop the gate before scope joins if an assertion unwinds.
            let gate = gate;
            let worker = scope
                .spawn(|| state.stop_local_session_with_options(&session, options, Some(claim)));
            gate.wait_until_claimed();
            let generation = {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                assert!(!record.engram.operator_paused);
                record.runtime_stop_generation
            };
            state.request_stop_session(&session).unwrap();
            {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                assert!(record.engram.operator_paused && record.orchestrator_auto_dispatch_blocked);
                assert_eq!(record.runtime_stop_generation, generation);
            }
            gate.release();
            worker.join().unwrap().unwrap();
        });
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(
            record.engram.operator_paused && record.orchestrator_auto_dispatch_blocked,
            "{profile}"
        );
        assert!(!record.runtime_stop_in_progress);
        assert!(transport.requests().is_empty());
    }
}

#[test]
fn off_boundary_global_runtime_exit_preserves_only_a_real_operator_pause() {
    for profile in ["plain", "on"] {
        for paused in [false, true] {
            let (state, session, _receiver, transport) = boundary_session(profile);
            let (runtime, _input) = test_codex_runtime_handle("boundary-runtime-exit");
            let token = {
                let mut inner = state.inner.lock().unwrap();
                let record = inner.session_mut(&session).unwrap();
                record.runtime = SessionRuntime::Codex(runtime);
                record.set_auto_dispatch_blocked(paused);
                record.engram.operator_paused = paused;
                record.runtime.runtime_token().unwrap()
            };
            state
                .handle_runtime_exit_if_matches(&session, &token, None)
                .unwrap();
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(matches!(record.runtime, SessionRuntime::None));
            assert_eq!(record.engram.operator_paused, paused, "{profile}");
            assert_eq!(
                record.orchestrator_auto_dispatch_blocked, paused,
                "{profile}"
            );
            assert!(transport.requests().is_empty());
        }
    }
}

#[test]
fn off_boundary_ordinary_characterization_matrix() {
    let mut observations = Vec::new();
    for profile in ["plain", "on", "base-off", "disabled"] {
        for action in [
            "send",
            "human-stop",
            "human-priority",
            "resume",
            "pause",
            "manual-recovery",
        ] {
            let (state, session, receiver, _transport) = boundary_session(profile);
            if matches!(action, "human-stop" | "human-priority") {
                let runtime = {
                    let mut inner = state.inner.lock().unwrap();
                    let record = inner.session_mut(&session).unwrap();
                    record.session.status = SessionStatus::Active;
                    std::mem::replace(&mut record.runtime, SessionRuntime::None)
                };
                state.stop_session(&session).unwrap();
                state
                    .inner
                    .lock()
                    .unwrap()
                    .session_mut(&session)
                    .unwrap()
                    .runtime = runtime;
            }
            if matches!(action, "human-priority" | "manual-recovery" | "pause") {
                queue_test_engram_prompt(
                    &state,
                    &session,
                    "automatic queued work",
                    QueuedPromptSource::Orchestrator,
                    None,
                );
            } else if action == "resume" {
                queue_test_engram_prompt(
                    &state,
                    &session,
                    "explicit boundary work",
                    QueuedPromptSource::User,
                    None,
                );
            }
            if matches!(action, "resume" | "pause" | "manual-recovery") {
                state
                    .inner
                    .lock()
                    .unwrap()
                    .session_mut(&session)
                    .unwrap()
                    .set_auto_dispatch_blocked(true);
            }
            let result = if action == "resume" {
                state.resume_session_queue(&session).map(|_| ())
            } else if action == "pause" {
                let result = state.dispatch_next_queued_turn(&session, false);
                observations.push((
                    profile,
                    action,
                    result.is_ok_and(|dispatch| dispatch.is_none()) && receiver.try_recv().is_err(),
                ));
                continue;
            } else {
                let request = SendMessageRequest {
                    text: "explicit boundary work".into(),
                    expanded_text: None,
                    attachments: Vec::new(),
                    source_session_id: None,
                    source_mailbox: None,
                };
                let result = if action.starts_with("human") {
                    state.dispatch_human_turn(&session, request)
                } else {
                    state.dispatch_turn(&session, request)
                };
                result.and_then(|result| match result {
                    DispatchTurnResult::Dispatched(dispatch)
                    | DispatchTurnResult::DispatchedAfterQueue(dispatch) => {
                        deliver_turn_dispatch(&state, dispatch).into_public_result()
                    }
                    DispatchTurnResult::Queued => Err(ApiError::conflict("explicit work was held")),
                })
            };
            let delivered = matches!(receiver.try_recv(), Ok(CodexRuntimeCommand::Prompt { command, .. })
                if command.prompt.contains("explicit boundary work") && !command.prompt.contains("automatic queued work"));
            observations.push((
                profile,
                action,
                result.is_ok() && delivered && receiver.try_recv().is_err(),
            ));
        }
        let (state, sender, target) = crate::tests::mailboxes::mailbox_test_state();
        if profile != "plain" {
            let root = state
                .test_temp_root
                .as_ref()
                .unwrap()
                .path()
                .join("boundary-mailbox-project");
            fs::create_dir_all(&root).unwrap();
            let project = create_test_project(&state, &root, "Boundary mailbox");
            enable_test_project_engram(&state, &project, &root);
            let mut inner = state.inner.lock().unwrap();
            for id in [&sender, &target] {
                inner.session_mut(id).unwrap().session.project_id = Some(project.clone());
            }
            let settings = inner
                .projects
                .iter_mut()
                .find(|p| p.id == project)
                .unwrap()
                .engram
                .as_mut()
                .unwrap();
            settings.execution_observability = profile == "on";
            settings.enabled = profile != "disabled";
        }
        state.install_control_test_transport(ScriptedEngramControlTransport::new([]));
        for sequence in 1..=2 {
            if sequence == 2 {
                let mut inner = state.inner.lock().unwrap();
                let record = inner.session_mut(&target).unwrap();
                record.session.status = SessionStatus::Idle;
                record.set_auto_dispatch_blocked(true);
            }
            state
                .append_mailbox_message_and_notify(
                    &sender,
                    SendMailboxMessageRequest {
                        target_session_id: target.clone(),
                        message: format!("body {sequence}"),
                        idempotency_key: format!("boundary-{sequence}"),
                        topic: None,
                        state_stamp: None,
                        class: None,
                    },
                )
                .unwrap();
        }
        let inner = state.inner.lock().unwrap();
        observations.push((
            profile,
            "coalescing",
            inner.sessions[inner.find_session_index(&target).unwrap()]
                .queued_prompts
                .len()
                == 1,
        ));
    }
    assert!(
        observations.iter().all(|(_, _, passed)| *passed),
        "master behavior matrix: {observations:?}"
    );
}

#[test]
fn off_boundary_remote_project_does_not_assert_local_off_authority() {
    let f = fixture();
    {
        let mut inner = f.state.inner.lock().unwrap();
        let project = inner
            .projects
            .iter_mut()
            .find(|p| p.id == f.target.project_id)
            .unwrap();
        project.remote_id = "remote-owner".into();
        project.engram = None;
    }
    let wire = serde_json::to_value(f.state.snapshot()).unwrap();
    let project = wire["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == f.target.project_id)
        .unwrap();
    assert!(
        project.get("engramControlEnabled").is_none(),
        "a proxy cannot derive its owner's control mode"
    );
}

#[test]
fn off_pair_started_turn_releases_the_operator_pause() {
    let f = fixture();
    switch_off(&f);
    {
        let mut inner = f.state.inner.lock().unwrap();
        let record = inner.session_mut(&f.session).unwrap();
        record.set_auto_dispatch_blocked(true);
        record.engram.operator_paused = true;
    }
    let dispatch = match f
        .state
        .dispatch_turn(
            &f.session,
            SendMessageRequest {
                text: "explicit ordinary recovery".into(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .unwrap()
    {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("ordinary recovery must start"),
    };
    deliver_turn_dispatch(&f.state, dispatch).unwrap();
    receive_prompt(&f, "explicit ordinary recovery");
    {
        let inner = f.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
        assert!(!record.session.queue_paused);
        assert!(
            !record.engram.operator_paused,
            "a started turn cannot retain an invisible Stop latch"
        );
    }
    queue_test_engram_prompt(
        &f.state,
        &f.session,
        "Human after recovery",
        QueuedPromptSource::Human,
        None,
    );
    {
        let mut inner = f.state.inner.lock().unwrap();
        inner.session_mut(&f.session).unwrap().session.status = SessionStatus::Idle;
    }
    let HumanPromptDrain::Started(dispatch) =
        f.state.start_human_queued_turn(&f.session, false).unwrap()
    else {
        panic!("the recovered queue must automatically drain its Human prompt");
    };
    deliver_turn_dispatch(&f.state, dispatch.dispatch).unwrap();
    receive_prompt(&f, "Human after recovery");
    assert!(f.transport.requests().is_empty());
}

fn off_orphaned_workflow_after_history(boot_scan: bool) {
    let f = fixture();
    let historical = queue_retained(&f, "unknown historical User", false);
    queue_test_engram_prompt(
        &f.state,
        &f.session,
        "orphaned workflow resume",
        QueuedPromptSource::Orchestrator,
        None,
    );
    switch_off(&f);
    {
        let mut inner = f.state.inner.lock().unwrap();
        let record = inner.session_mut(&f.session).unwrap();
        record.set_auto_dispatch_blocked(false);
        record.session.status = SessionStatus::Idle;
        record.runtime = SessionRuntime::None;
        f.state.commit_locked(&mut inner).unwrap();
    }
    f.state.shutdown_persist_blocking();
    *f.state.inner.lock().unwrap() = load_state(f.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let loaded_history = {
        let inner = f.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
        assert_eq!(
            record.queued_prompts[0].pending_prompt.id,
            historical.pending_prompt.id
        );
        serde_json::to_value(&record.queued_prompts[0]).unwrap()
    };
    let (runtime, prompts, _process) = test_shared_codex_runtime("off-orphan-after-history");
    install_single_wire_codex_fixture(&f.state, Some(runtime));
    if boot_scan {
        f.state.dispatch_orphaned_workflow_prompts();
    } else {
        let started = f
            .state
            .start_orphaned_workflow_turn(&f.session)
            .unwrap()
            .expect("the locked orphan check must ignore non-runnable history");
        deliver_turn_dispatch(&f.state, started.dispatch).unwrap();
    }
    assert!(
        matches!(prompts.try_recv(), Ok(CodexRuntimeCommand::Prompt { command, .. })
        if command.prompt.contains("orphaned workflow resume")),
        "restart must activate the committed workflow"
    );
    assert!(prompts.try_recv().is_err());
    let inner = f.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(
        serde_json::to_value(&record.queued_prompts[0]).unwrap(),
        loaded_history
    );
    assert!(f.transport.requests().is_empty());
}

#[test]
fn off_pair_boot_scan_runs_workflow_after_unknown_history() {
    off_orphaned_workflow_after_history(true);
}

#[test]
fn off_pair_locked_orphan_check_runs_workflow_after_unknown_history() {
    off_orphaned_workflow_after_history(false);
}

#[derive(Clone)]
struct OrdinaryProject {
    project_id: String,
}

struct OffFixture {
    state: AppState,
    session: String,
    receiver: mpsc::Receiver<CodexRuntimeCommand>,
    transport: Arc<ScriptedEngramControlTransport>,
    target: OrdinaryProject,
    debt: Value,
}

fn fixture() -> OffFixture {
    let (state, session, receiver, transport) = ordinary_root_fixture();
    let project_id = {
        let mut inner = state.inner.lock().unwrap();
        let record = inner.session_mut(&session).unwrap();
        record
            .engram
            .historical_control
            .insert("engramOpenGrantId".into(), json!("original-open-grant"));
        record
            .engram
            .historical_control
            .insert("engramRoutingToken".into(), json!("original-routing-token"));
        record.engram.historical_control.insert(
            "engramAdmissionRetry".into(),
            json!({"original": "passive historical retry", "requestKey": "original-key"}),
        );
        record.set_auto_dispatch_blocked(true);
        let project = record.session.project_id.clone().unwrap();
        state.commit_locked(&mut inner).unwrap();
        project
    };
    let debt = ledger(&state);
    assert!(transport.requests().is_empty());
    OffFixture {
        state,
        session,
        receiver,
        transport,
        target: OrdinaryProject { project_id },
        debt,
    }
}

fn ledger(state: &AppState) -> Value {
    let inner = state.inner.lock().unwrap();
    serde_json::to_value(
        &inner
            .sessions
            .iter()
            .find(|record| {
                record
                    .engram
                    .historical_control
                    .contains_key("engramOpenGrantId")
            })
            .expect("fixture history remains on its original session")
            .engram
            .historical_control,
    )
    .unwrap()
}

fn switch_off(f: &OffFixture) {
    // Seed the state after the switch, without invoking settings reset/recovery.
    let mut inner = f.state.inner.lock().unwrap();
    inner
        .projects
        .iter_mut()
        .find(|p| p.id == f.target.project_id)
        .unwrap()
        .engram
        .as_mut()
        .unwrap()
        .execution_observability = false;
    f.state.commit_locked(&mut inner).unwrap();
}

fn assert_debt_unchanged(f: &OffFixture) {
    assert_eq!(ledger(&f.state), f.debt);
    let loaded = load_state(f.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&f.session).unwrap()];
    assert_eq!(
        serde_json::to_value(&record.engram.historical_control).unwrap(),
        f.debt,
        "ordinary execution preserves the exact stored history after SQLite reload"
    );
}

fn queue_retained(f: &OffFixture, text: &str, never_promoted: bool) -> QueuedPromptRecord {
    queue_test_engram_prompt(&f.state, &f.session, text, QueuedPromptSource::User, None);
    let mut inner = f.state.inner.lock().unwrap();
    let record = inner.session_mut(&f.session).unwrap();
    let queued = record.queued_prompts.back_mut().unwrap();
    queued.engram_waiting = true;
    queued.engram_interrupted = true;
    queued.promotion_disposition_known = never_promoted;
    queued.promoted_message_index = None;
    queued.engram_ordinary_handoff = !never_promoted;
    queued.engram_bind = Some(json!({"connection":{"session_id":f.session},
        "request":{"operation":"session_bind", "idempotency_key":format!("original-bind-{}",queued.pending_prompt.id)},
        "historicalExtension":{"preserve":[1,2,3]}}));
    let saved = queued.clone();
    sync_pending_prompts(record);
    f.state.commit_locked(&mut inner).unwrap();
    saved
}

fn receive_prompt(f: &OffFixture, text: &str) {
    let CodexRuntimeCommand::Prompt {
        session_id,
        command,
    } = f
        .receiver
        .try_recv()
        .expect("eligible ordinary work must reach the provider without control settlement")
    else {
        panic!("expected a provider prompt");
    };
    assert_eq!(session_id, f.session);
    assert!(
        command.prompt.contains(text),
        "wrong provider content: {}",
        command.prompt
    );
    assert!(
        f.receiver.try_recv().is_err(),
        "the same head must dispatch only once"
    );
}

#[test]
fn off_empty_resume_after_cancel_preserves_intended_checkpoint_debt() {
    let f = fixture();
    let head = queue_retained(&f, "cancelled original head", true);
    switch_off(&f);
    f.state
        .cancel_queued_prompt(&f.session, &head.pending_prompt.id)
        .unwrap();
    {
        let inner = f.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
        assert!(record.queued_prompts.is_empty());
        assert!(record.orchestrator_auto_dispatch_blocked);
    }
    let (phase_tx, phase_rx) = mpsc::channel();
    f.state.agent_readiness_cache.write().unwrap().phase_sink = Some(phase_tx);
    let result = f.state.resume_session_queue(&f.session);
    assert!(
        result.is_ok(),
        "OFF empty Resume must not require settlement: {:?}",
        result.err()
    );
    let timing = phase_rx
        .try_iter()
        .find(|phase| {
            phase.phase == "resume_handler"
                && phase.session_id.as_deref() == Some(f.session.as_str())
        })
        .expect("the existing Resume timing observation must cover the OFF path");
    eprintln!(
        "ordinary OFF Resume observed {} microseconds",
        timing.elapsed.as_micros()
    );
    assert!(
        !f.state
            .inner
            .lock()
            .unwrap()
            .sessions
            .iter()
            .find(|r| r.session.id == f.session)
            .unwrap()
            .orchestrator_auto_dispatch_blocked
    );
    assert!(f.receiver.try_recv().is_err());
    assert!(f.transport.requests().is_empty());
    assert_debt_unchanged(&f);
}

#[test]
fn off_known_never_promoted_user_head_dispatches_and_completion_keeps_old_debt() {
    let f = fixture();
    let head = queue_retained(&f, "known unsent retained user text", true);
    switch_off(&f);
    f.state
        .resume_session_queue(&f.session)
        .expect("OFF must admit the proven unsent head");
    receive_prompt(&f, &head.pending_prompt.text);
    let token = {
        let inner = f.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
        let retained = record
            .queued_prompts
            .iter()
            .find(|q| q.pending_prompt.id == head.pending_prompt.id)
            .expect("old control intent stays in place");
        assert_eq!(
            serde_json::to_value(&retained.engram_bind).unwrap(),
            serde_json::to_value(&head.engram_bind).unwrap()
        );
        assert_eq!(retained.pending_prompt.text, head.pending_prompt.text);
        record.runtime.runtime_token().unwrap()
    };
    f.state
        .finish_turn_ok_if_runtime_matches(&f.session, &token)
        .unwrap();
    assert!(
        f.receiver.try_recv().is_err(),
        "completion must not replay the retained head"
    );
    assert!(
        f.transport.requests().is_empty(),
        "OFF completion must not checkpoint the old grant"
    );
    assert_debt_unchanged(&f);
}

#[test]
fn off_unknown_handoff_stays_in_place_while_eligible_fifo_drains() {
    let f = fixture();
    let unknown = queue_retained(&f, "unknown provider handoff: never replay", false);
    queue_test_engram_prompt(
        &f.state,
        &f.session,
        "first eligible ordinary head",
        QueuedPromptSource::User,
        None,
    );
    queue_test_engram_prompt(
        &f.state,
        &f.session,
        "second eligible ordinary head",
        QueuedPromptSource::User,
        None,
    );
    switch_off(&f);
    f.state
        .resume_session_queue(&f.session)
        .expect("OFF must skip historical unknown handoff");
    receive_prompt(&f, "first eligible ordinary head");
    let token = {
        let inner = f.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            serde_json::to_value(&unknown).unwrap()
        );
        record.runtime.runtime_token().unwrap()
    };
    f.state
        .finish_turn_ok_if_runtime_matches(&f.session, &token)
        .unwrap();
    receive_prompt(&f, "second eligible ordinary head");
    let inner = f.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
    assert_eq!(
        serde_json::to_value(&record.queued_prompts[0]).unwrap(),
        serde_json::to_value(&unknown).unwrap()
    );
    drop(inner);
    assert!(f.transport.requests().is_empty());
    assert_debt_unchanged(&f);
}

#[test]
fn off_real_mailbox_append_wake_dispatches_without_settlement() {
    let mut f = fixture();
    let path = resolve_coordination_persistence_path(f.state.persistence_path.as_ref());
    f.state.mailbox_store = Arc::new(MailboxStore::open(&path).unwrap());
    let sender = test_session_id(&f.state, Agent::Claude);
    switch_off(&f);
    {
        let mut inner = f.state.inner.lock().unwrap();
        let index = inner.find_session_index(&f.session).unwrap();
        inner
            .session_mut_by_index(index)
            .unwrap()
            .set_auto_dispatch_blocked(false);
        f.state.commit_locked(&mut inner).unwrap();
    }
    let receipt = f
        .state
        .append_mailbox_message_and_notify(
            &sender,
            SendMailboxMessageRequest {
                target_session_id: f.session.clone(),
                message: "original durable mailbox content".into(),
                idempotency_key: "off-original-mail".into(),
                topic: Some("ordinary wake".into()),
                state_stamp: None,
                class: Some("routine".into()),
            },
        )
        .unwrap();
    receive_prompt(&f, &receipt.mailbox_id);
    assert!(f.transport.requests().is_empty());
    assert_debt_unchanged(&f);
}

#[test]
fn off_local_runtime_reset_keeps_original_checkpoint_identity_and_evidence() {
    let f = fixture();
    switch_off(&f);
    let (runtime, _input) = test_codex_runtime_handle("off-local-reset-runtime");
    let token = RuntimeToken::Codex(runtime.runtime_id.clone());
    {
        let mut inner = f.state.inner.lock().unwrap();
        let index = inner.find_session_index(&f.session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.runtime = SessionRuntime::Codex(runtime);
        record.runtime_reset_required = true;
        f.state.commit_locked(&mut inner).unwrap();
    }
    f.state
        .prepare_dedicated_runtime_reset_off_lock(&f.session, false)
        .expect("OFF local reset must not wait on control debt");
    f.state
        .handle_runtime_exit_if_matches(&f.session, &token, None)
        .unwrap();
    {
        let inner = f.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
        assert!(matches!(record.runtime, SessionRuntime::None));
        assert!(!record.runtime_reset_required);
    }
    assert!(f.transport.requests().is_empty());
    assert_debt_unchanged(&f);
}

#[test]
fn ordinary_human_send_resumes_operator_pause_but_preserves_real_stop() {
    for (stopping, operator_pause) in [(false, false), (true, false), (false, true)] {
        let f = fixture();
        switch_off(&f);
        {
            let mut inner = f.state.inner.lock().unwrap();
            let record = inner.session_mut(&f.session).unwrap();
            record.set_auto_dispatch_blocked(operator_pause);
            record.engram.operator_paused = operator_pause;
            record.runtime_stop_in_progress = stopping;
            f.state.commit_locked(&mut inner).unwrap();
        }
        let result = f
            .state
            .dispatch_human_turn(
                &f.session,
                SendMessageRequest {
                    text: "fresh ordinary human input".into(),
                    expanded_text: None,
                    attachments: Vec::new(),
                    source_session_id: None,
                    source_mailbox: None,
                },
            )
            .unwrap();
        if stopping {
            assert!(matches!(result, DispatchTurnResult::Queued));
            assert!(f.receiver.try_recv().is_err());
        } else {
            let dispatch = match result {
                DispatchTurnResult::Dispatched(dispatch)
                | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
                DispatchTurnResult::Queued => {
                    panic!("historical data cannot block a fresh Human turn")
                }
            };
            deliver_turn_dispatch(&f.state, dispatch).unwrap();
            receive_prompt(&f, "fresh ordinary human input");
            let token = {
                let inner = f.state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
                assert!(!record.engram.operator_paused);
                record.runtime.runtime_token().unwrap()
            };
            f.state
                .finish_turn_ok_if_runtime_matches(&f.session, &token)
                .unwrap();
            assert!(f.receiver.try_recv().is_err());
        }
        assert!(f.transport.requests().is_empty());
        assert_debt_unchanged(&f);
    }
}

#[test]
fn off_legacy_missing_promotion_evidence_is_skipped_even_without_handoff_bit() {
    let f = fixture();
    let mut unknown = queue_retained(&f, "legacy delivery evidence is missing", false);
    {
        let mut inner = f.state.inner.lock().unwrap();
        let index = inner.find_session_index(&f.session).unwrap();
        inner.sessions[index].queued_prompts[0].engram_ordinary_handoff = false;
        unknown.engram_ordinary_handoff = false;
        f.state.commit_locked(&mut inner).unwrap();
    }
    queue_test_engram_prompt(
        &f.state,
        &f.session,
        "known ordinary successor",
        QueuedPromptSource::User,
        None,
    );
    switch_off(&f);
    f.state.resume_session_queue(&f.session).unwrap();
    receive_prompt(&f, "known ordinary successor");
    let inner = f.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
    assert_eq!(
        serde_json::to_value(&record.queued_prompts[0]).unwrap(),
        serde_json::to_value(&unknown).unwrap()
    );
    drop(inner);
    assert!(f.transport.requests().is_empty());
    assert_debt_unchanged(&f);
}

#[test]
fn off_startup_reload_preserves_unknown_head_and_wakes_ordinary_fifo_without_control() {
    let f = fixture();
    let unknown = queue_retained(&f, "unknown delivery retained across OFF boot", false);
    queue_test_engram_prompt(
        &f.state,
        &f.session,
        "ordinary prompt awaiting a new wake",
        QueuedPromptSource::User,
        None,
    );
    switch_off(&f);
    let workdir = {
        let mut inner = f.state.inner.lock().unwrap();
        let record = inner.session_mut(&f.session).unwrap();
        record.session.status = SessionStatus::Idle;
        record.session.preview =
            "Engram recovered authorization retained. Resume to recover the exact queued prompt."
                .into();
        f.state.commit_locked(&mut inner).unwrap();
        inner
            .find_project(&f.target.project_id)
            .unwrap()
            .root_path
            .clone()
    };
    // Use the same real startup constructor as the boot-recovery suite. Join
    // the old writer before reopening its disposable SQLite store.
    f.state.shutdown_persist_blocking();
    let transport = ScriptedEngramControlTransport::new([]);
    let state = boot_on_test_temp_root(f.state.test_temp_root.as_ref().unwrap(), || {
        AppState::new_with_paths(
            workdir,
            f.state.persistence_path.as_ref().clone(),
            f.state.orchestrator_templates_path.as_ref().clone(),
        )
    })
    .expect("OFF state should restart without settling historical control");
    state.install_control_test_transport(transport.clone());
    let (runtime, receiver, _process) = test_shared_codex_runtime("off-reloaded-ordinary-runtime");
    install_single_wire_codex_fixture(&state, Some(runtime));
    let restarted = OffFixture {
        state,
        session: f.session.clone(),
        receiver,
        transport,
        target: f.target.clone(),
        debt: f.debt.clone(),
    };
    // The complete transcript lets the canonical loader derive promotion
    // disposition. The unknown handoff marker still prevents any replay.
    let mut normalized_unknown = unknown.clone();
    normalized_unknown.promotion_disposition_known = true;
    {
        let inner = restarted.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&restarted.session).unwrap()];
        assert!(
            !record.orchestrator_auto_dispatch_blocked,
            "OFF boot must not derive a queue pause from historical control"
        );
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            serde_json::to_value(&normalized_unknown).unwrap()
        );
    }
    assert!(restarted.transport.requests().is_empty());
    let dispatch = match restarted
        .state
        .dispatch_turn(
            &restarted.session,
            SendMessageRequest {
                text: "fresh activation after OFF restart".into(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .unwrap()
    {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => {
            panic!("an ordinary wake must activate the restored OFF queue")
        }
    };
    deliver_turn_dispatch(&restarted.state, dispatch).unwrap();
    receive_prompt(&restarted, "ordinary prompt awaiting a new wake");
    {
        let inner = restarted.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&restarted.session).unwrap()];
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            serde_json::to_value(&normalized_unknown).unwrap()
        );
    }
    assert!(restarted.transport.requests().is_empty());
    assert_debt_unchanged(&restarted);
    restarted.state.shutdown_persist_blocking();
}

#[test]
fn ordinary_deferred_inventory_restores_normalized_pause_and_respects_new_epoch() {
    for owner in ["retired", "operator", "failed-mailbox", "unresolved-codex"] {
        let original_pause = owner != "retired";
        for successor in [None, Some(false), Some(true)] {
            let f = if owner == "failed-mailbox" {
                failed_mailbox_pause_fixture()
            } else {
                fixture()
            };
            if owner != "failed-mailbox" {
                queue_retained(&f, "unknown provider attempt during startup", false);
            }
            {
                let mut inner = f.state.inner.lock().unwrap();
                let record = inner.session_mut(&f.session).unwrap();
                record.engram.operator_paused = owner == "operator";
                if owner == "unresolved-codex" {
                    record.session.status = SessionStatus::Active;
                    record.external_session_id = Some("unresolved-ordinary-codex-thread".into());
                    record.session.external_session_id = record.external_session_id.clone();
                    // A persisted external Codex thread requires its native
                    // state; keep the same quarantine validation on reload.
                    set_record_codex_thread_state(record, CodexThreadState::Active);
                    record.session.preview =
                        "Ordinary provider connection interrupted; inspect before retry.".into();
                } else if owner != "failed-mailbox" {
                    record.session.status = SessionStatus::Idle;
                    record.session.preview="Engram recovered authorization retained. Resume to recover the exact queued prompt.".into();
                }
                record.set_auto_dispatch_blocked(true);
                f.state.commit_locked(&mut inner).unwrap();
            }
            f.state.shutdown_persist_blocking();
            let workdir = f
                .state
                .inner
                .lock()
                .unwrap()
                .find_project(&f.target.project_id)
                .unwrap()
                .root_path
                .clone();
            let state = boot_on_test_temp_root(f.state.test_temp_root.as_ref().unwrap(), || {
                AppState::new_before_listen_with_paths(
                    workdir,
                    f.state.persistence_path.as_ref().clone(),
                    f.state.orchestrator_templates_path.as_ref().clone(),
                )
            })
            .unwrap();
            let reached = AtomicBool::new(false);
            state
                .load_delegation_inventory_observed(|phase| {
                    if phase == "before_publish" {
                        reached.store(true, Ordering::SeqCst);
                        if let Some(paused) = successor {
                            let mut inner = state.inner.lock().unwrap();
                            let record = inner.session_mut(&f.session).unwrap();
                            record.set_auto_dispatch_blocked(paused);
                            record.engram.operator_paused = paused;
                        }
                    }
                })
                .unwrap();
            assert!(reached.load(Ordering::SeqCst));
            let inner = state.inner.lock().unwrap();
            assert!(inner.delegation_inventory_complete());
            let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
            assert_eq!(
                record.orchestrator_auto_dispatch_blocked,
                successor.unwrap_or(original_pause),
                "owner={owner}, successor={successor:?}"
            );
            assert_eq!(
                record.session.queue_paused,
                successor.unwrap_or(original_pause)
            );
            assert!(
                record
                    .queued_prompts
                    .iter()
                    .any(|queued| queued.engram_ordinary_handoff)
            );
            assert_eq!(
                serde_json::to_value(&record.engram.historical_control).unwrap(),
                f.debt
            );
            drop(inner);
            state.shutdown_persist_blocking();
        }
    }
}

#[test]
fn off_review_stop_preserves_the_historical_pending_dispatch() {
    let f = fixture();
    switch_off(&f);
    {
        let mut inner = f.state.inner.lock().unwrap();
        let record = inner.session_mut(&f.session).unwrap();
        record.set_auto_dispatch_blocked(false);
        record
            .engram
            .historical_control
            .insert("engramDispatchGeneration".into(), json!(1));
        // This independent passive field has no current dispatch owner.
        f.state.commit_locked(&mut inner).unwrap();
    }
    let dispatch = match f
        .state
        .dispatch_turn(
            &f.session,
            SendMessageRequest {
                text: "ordinary turn to stop".into(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .unwrap()
    {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("ordinary OFF turn must start"),
    };
    deliver_turn_dispatch(&f.state, dispatch).unwrap();
    receive_prompt(&f, "ordinary turn to stop");
    {
        let mut inner = f.state.inner.lock().unwrap();
        let record = inner.session_mut(&f.session).unwrap();
        // Exercise the canonical missing-runtime Stop completion; physical
        // provider cleanup has separate real-process Stop coverage.
        record.runtime = SessionRuntime::None;
        f.state.commit_locked(&mut inner).unwrap();
    }
    f.state.stop_session(&f.session).unwrap();
    {
        let inner = f.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
        assert_eq!(
            record.engram.historical_control["engramDispatchGeneration"],
            json!(1)
        );
        assert!(record.engram.operator_paused && record.orchestrator_auto_dispatch_blocked);
    }
    assert!(f.receiver.try_recv().is_err());
    assert!(f.transport.requests().is_empty());
    let mut expected = f.debt.clone();
    expected["engramDispatchGeneration"] = json!(1);
    assert_eq!(ledger(&f.state), expected);
    let loaded = load_state(f.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(
            &loaded.sessions[loaded.find_session_index(&f.session).unwrap()]
                .engram
                .historical_control
        )
        .unwrap(),
        expected
    );
}

#[test]
fn off_review_selected_unknown_head_never_sends_its_successor() {
    let f = fixture();
    let unknown = queue_retained(&f, "selected unknown attempt", false);
    queue_test_engram_prompt(
        &f.state,
        &f.session,
        "unselected successor",
        QueuedPromptSource::User,
        None,
    );
    switch_off(&f);
    let selection = {
        let inner = f.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
        HumanPromptSelection {
            prompt: projected_pending_prompts(record)[0].clone(),
            session_mutation_stamp: record.mutation_stamp,
        }
    };
    let result = f.state.resume_selected_human_prompt(&f.session, selection);
    assert!(matches!(result, Err(error) if error.status == StatusCode::CONFLICT));
    assert!(
        f.receiver.try_recv().is_err(),
        "the selected unknown row authorizes no successor"
    );
    let current = {
        let inner = f.state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&f.session).unwrap()].queued_prompts[0].clone()
    };
    assert_eq!(
        serde_json::to_value(current).unwrap(),
        serde_json::to_value(unknown).unwrap()
    );
}

fn failed_mailbox_pause_fixture() -> OffFixture {
    let mut f = fixture();
    let path = resolve_coordination_persistence_path(f.state.persistence_path.as_ref());
    f.state.mailbox_store = Arc::new(MailboxStore::open(&path).unwrap());
    let sender = test_session_id(&f.state, Agent::Claude);
    switch_off(&f);
    {
        let mut inner = f.state.inner.lock().unwrap();
        inner
            .session_mut(&f.session)
            .unwrap()
            .set_auto_dispatch_blocked(false);
        f.state.commit_locked(&mut inner).unwrap();
    }
    let receipt = f
        .state
        .append_mailbox_message_and_notify(
            &sender,
            SendMailboxMessageRequest {
                target_session_id: f.session.clone(),
                message: "mailbox payload that fails".into(),
                idempotency_key: "ordinary-failed-mailbox".into(),
                topic: None,
                state_stamp: None,
                class: Some("routine".into()),
            },
        )
        .unwrap();
    receive_prompt(&f, &receipt.mailbox_id);
    queue_retained(&f, "separate historical unknown attempt", false);
    let token = {
        let inner = f.state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&f.session).unwrap()]
            .runtime
            .runtime_token()
            .unwrap()
    };
    f.state
        .fail_turn_if_runtime_matches(
            &f.session,
            &token,
            "failed mailbox payload; inspect before retry",
        )
        .unwrap();
    let pause = {
        let inner = f.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
        (
            record.orchestrator_auto_dispatch_blocked,
            record.engram.operator_paused,
        )
    };
    assert_eq!(
        pause,
        (true, false),
        "the real mailbox failure owns an ordinary pause"
    );
    assert!(f.receiver.try_recv().is_err());
    f
}

#[test]
fn off_review_failed_mailbox_pause_survives_reset_release() {
    let f = failed_mailbox_pause_fixture();
    let generation = {
        let mut inner = f.state.inner.lock().unwrap();
        let generation = inner
            .engram_project_resets
            .claim(&f.target.project_id)
            .unwrap();
        generation
    };
    assert!(
        f.state
            .inner
            .lock()
            .unwrap()
            .engram_project_resets
            .release(&f.target.project_id, generation)
    );
    let paused = {
        let inner = f.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
        record.orchestrator_auto_dispatch_blocked && record.session.queue_paused
    };
    assert!(
        paused,
        "reset release must not erase a failed-mailbox pause"
    );
    assert!(f.receiver.try_recv().is_err());
    resume_failed_mailbox(&f);
}

#[test]
fn off_review_failed_mailbox_pause_survives_reload() {
    let f = failed_mailbox_pause_fixture();
    f.state.shutdown_persist_blocking();
    let loaded = load_state(f.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&f.session).unwrap()];
    assert!(
        record.orchestrator_auto_dispatch_blocked && record.session.queue_paused,
        "reload must not erase a failed-mailbox pause beside old control intent"
    );
    assert!(!record.engram.operator_paused);
    *f.state.inner.lock().unwrap() = loaded;
    resume_failed_mailbox(&f);
}

fn resume_failed_mailbox(f: &OffFixture) {
    let text = {
        let inner = f.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
        assert!(
            record.session.preview.contains("failed mailbox payload"),
            "the ordinary error preview remains visible"
        );
        record
            .queued_prompts
            .iter()
            .find(|queued| queued.source == QueuedPromptSource::Mailbox)
            .expect("the failed wake stays queued")
            .pending_prompt
            .text
            .clone()
    };
    f.state.resume_session_queue(&f.session).unwrap();
    receive_prompt(f, &text);
    assert!(f.transport.requests().is_empty());
}

#[test]
fn ordinary_load_retires_only_proven_historical_pause_and_preserves_ordinary_owners() {
    let retired = [
        "Engram recovered authorization retained. Resume to recover the exact queued prompt.",
        "Engram control is off; recovered authorization retained. Cancel the queued prompt before continuing.",
        "Engram settings changed. Authorization retained; cancel or reconcile before continuing.",
    ];
    for (preview, status, operator, stopped, capacity, released) in [
        (retired[0], SessionStatus::Idle, false, false, false, true),
        (retired[1], SessionStatus::Idle, false, false, false, true),
        (retired[2], SessionStatus::Idle, false, false, false, true),
        (
            "arbitrary stored preview",
            SessionStatus::Idle,
            false,
            false,
            false,
            false,
        ),
        (
            "Engram control is off",
            SessionStatus::Idle,
            false,
            false,
            false,
            false,
        ),
        (
            "Ready for a prompt.",
            SessionStatus::Idle,
            false,
            false,
            false,
            false,
        ),
        (retired[0], SessionStatus::Error, false, false, false, false),
        (
            retired[0],
            SessionStatus::Active,
            false,
            false,
            false,
            false,
        ),
        (retired[0], SessionStatus::Idle, true, false, false, false),
        (retired[0], SessionStatus::Idle, false, true, false, false),
        (retired[0], SessionStatus::Idle, false, false, true, false),
    ] {
        let f = fixture();
        queue_retained(&f, "known ordinary input", true);
        {
            let mut inner = f.state.inner.lock().unwrap();
            let record = inner.session_mut(&f.session).unwrap();
            record.session.preview = preview.into();
            record.session.status = status;
            record.set_auto_dispatch_blocked(true);
            record.engram.operator_paused = operator;
            record.engram.stopped_prompt_id = stopped.then(|| "original-stopped-prompt".into());
            if capacity {
                record.provider_capacity_retry = Some(ProviderCapacityRetry {
                    nonce: "ordinary-capacity-owner".into(),
                    server_instance: f.state.server_instance_id.clone(),
                    runtime_id: "ordinary-capacity-runtime".into(),
                    generation: record.active_turn_generation,
                    stop_generation: record.runtime_stop_generation,
                    pause_epoch: record.auto_dispatch_pause_epoch,
                    settings: provider_capacity_settings(record),
                    attempts: 0,
                    due_at: None,
                    recovering: false,
                    dispatched: false,
                    original_preview: preview.into(),
                    saved: false,
                    fence: None,
                });
            }
            f.state.commit_locked(&mut inner).unwrap();
        }
        let loaded = load_state(f.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        let record = &loaded.sessions[loaded.find_session_index(&f.session).unwrap()];
        assert_eq!(
            record.orchestrator_auto_dispatch_blocked, !released,
            "preview={preview}, operator={operator}, stopped={stopped}, capacity={capacity}"
        );
        assert_eq!(record.session.queue_paused, !released);
        assert_eq!(
            serde_json::to_value(&record.engram.historical_control).unwrap(),
            f.debt
        );
        assert_eq!(
            record.queued_prompts[0].pending_prompt.text,
            "known ordinary input"
        );
        assert!(f.transport.requests().is_empty());
    }
}

#[test]
fn off_review_reload_keeps_receipt_settlement_and_legacy_normalization() {
    let f = fixture();
    let head = queue_retained(&f, "ambiguous receipted attempt", false);
    switch_off(&f);
    {
        let mut inner = f.state.inner.lock().unwrap();
        let record = inner.session_mut(&f.session).unwrap();
        record.session.status = SessionStatus::Idle;
        record.session.preview =
            "Engram recovered authorization retained. Resume to recover the exact queued prompt."
                .into();
        record.prompt_deliveries.insert(
            head.pending_prompt.id.clone(),
            PromptDelivery {
                federation_operation: None,
                federation_handoff_started: false,
                disposition: PromptDeliveryDisposition::HandingOff,
                attempt: 1,
                write_ahead: "original-unknown-write-ahead".into(),
                sequence: 1,
                message_id: Some(head.pending_prompt.id.clone()),
                turn_generation: Some(1),
                attempt_key: Some("original-unknown-attempt".into()),
                reason: None,
                entry: Some(head.clone()),
                write_ahead_acknowledged: true,
                resume: None,
                runtime_write: PromptRuntimeWrite::Untracked,
            },
        );
        f.state.commit_locked(&mut inner).unwrap();
    }
    f.state.shutdown_persist_blocking();
    let loaded = load_state(f.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&f.session).unwrap()];
    assert!(
        record.orchestrator_auto_dispatch_blocked && record.session.queue_paused,
        "ordinary ambiguous receipt settlement independently restores its hold"
    );
    let delivery = &record.prompt_deliveries[&head.pending_prompt.id];
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Ambiguous);
    assert_eq!(delivery.write_ahead, "original-unknown-write-ahead");
    assert!(delivery.entry.is_none());
    assert!(
        record
            .queued_prompts
            .iter()
            .all(|queued| queued.pending_prompt.id != head.pending_prompt.id),
        "OFF must not reinsert the row that restart receipt settlement retired"
    );
    assert!(projected_pending_prompts(record).is_empty());
}

#[test]
fn off_review_wait_resume_starts_beside_a_retained_only_queue() {
    for known in [false, true] {
        let f = fixture();
        let head = queue_retained(&f, "historical attempt is not runnable", known);
        switch_off(&f);
        if known {
            f.state.resume_session_queue(&f.session).unwrap();
            receive_prompt(&f, &head.pending_prompt.text);
            let token = {
                let inner = f.state.inner.lock().unwrap();
                inner.sessions[inner.find_session_index(&f.session).unwrap()]
                    .runtime
                    .runtime_token()
                    .unwrap()
            };
            f.state
                .finish_turn_ok_if_runtime_matches(&f.session, &token)
                .unwrap();
        } else {
            let mut inner = f.state.inner.lock().unwrap();
            inner
                .session_mut(&f.session)
                .unwrap()
                .set_auto_dispatch_blocked(false);
        }
        let queued = {
            let mut inner = f.state.inner.lock().unwrap();
            let queued = queue_delegation_wait_resume_locked(
                &mut inner,
                &f.session,
                "completed wait ordinary resume".into(),
            );
            f.state.commit_locked(&mut inner).unwrap();
            queued
        };
        assert!(queued.prompt_queued);
        assert!(
            queued.dispatch_requested,
            "retained history cannot strand delegation or test-run fan-in"
        );
        let dispatch = f
            .state
            .dispatch_next_queued_turn(&f.session, false)
            .unwrap()
            .unwrap();
        deliver_turn_dispatch(&f.state, dispatch).unwrap();
        receive_prompt(&f, "completed wait ordinary resume");
        let inner = f.state.inner.lock().unwrap();
        assert!(
            inner.sessions[inner.find_session_index(&f.session).unwrap()]
                .queued_prompts
                .iter()
                .any(|queued| queued.pending_prompt.id == head.pending_prompt.id)
        );
    }
}

#[test]
fn off_review_retained_only_consolidation_does_not_hide_a_deadlock() {
    let f = fixture();
    switch_off(&f);
    let template = f
        .state
        .create_orchestrator_template(sample_deadlocked_orchestrator_template_draft())
        .unwrap()
        .template;
    let instance = f
        .state
        .create_orchestrator_instance_with_agent_setup_validator(
            CreateOrchestratorInstanceRequest {
                template_id: template.id,
                project_id: Some(f.target.project_id.clone()),
                template: None,
            },
            |_, _| Ok(()),
        )
        .unwrap()
        .orchestrator;
    let session = |template_id: &str| {
        instance
            .session_instances
            .iter()
            .find(|session| session.template_session_id == template_id)
            .unwrap()
            .session_id
            .clone()
    };
    let a = session("consolidate-a");
    let b = session("consolidate-b");
    queue_test_engram_prompt(
        &f.state,
        &a,
        "unknown consolidation history",
        QueuedPromptSource::User,
        None,
    );
    let (blocked, retained) = {
        let mut inner = f.state.inner.lock().unwrap();
        let record = inner.session_mut(&a).unwrap();
        let head = record.queued_prompts.back_mut().unwrap();
        head.engram_ordinary_handoff = true;
        head.engram_interrupted = true;
        head.promotion_disposition_known = false;
        let retained = head.clone();
        let index = inner
            .orchestrator_instances
            .iter()
            .position(|value| value.id == instance.id)
            .unwrap();
        let instance = &mut inner.orchestrator_instances[index];
        instance.status = OrchestratorInstanceStatus::Running;
        instance.pending_transitions = vec![
            PendingTransition {
                id: "pending-a".into(),
                transition_id: "source-a-to-consolidate-a".into(),
                source_session_id: session("source-a"),
                destination_session_id: a.clone(),
                completion_revision: 1,
                rendered_prompt: "source A result".into(),
                created_at: stamp_now(),
            },
            PendingTransition {
                id: "pending-b".into(),
                transition_id: "source-b-to-consolidate-b".into(),
                source_session_id: session("source-b"),
                destination_session_id: b.clone(),
                completion_revision: 2,
                rendered_prompt: "source B result".into(),
                created_at: stamp_now(),
            },
        ];
        (
            detect_deadlocked_consolidate_session_ids(&inner, &inner.orchestrator_instances[index]),
            retained,
        )
    };
    assert_eq!(
        blocked.into_iter().collect::<HashSet<_>>(),
        HashSet::from([a.clone(), b]),
        "non-runnable history must not hide the real consolidation cycle"
    );
    let (marked, current) = {
        let mut inner = f.state.inner.lock().unwrap();
        let marked = mark_deadlocked_orchestrator_instances(&mut inner, &HashSet::new());
        let record = &inner.sessions[inner.find_session_index(&a).unwrap()];
        (marked, record.queued_prompts.front().unwrap().clone())
    };
    assert!(marked);
    assert_eq!(
        serde_json::to_value(current).unwrap(),
        serde_json::to_value(retained).unwrap()
    );
    assert!(f.receiver.try_recv().is_err());
    assert!(f.transport.requests().is_empty());
}

#[test]
fn off_review_human_start_failure_restores_fifo_once() {
    human_failure_restores_fifo(false, "base-off");
}

#[test]
fn off_review_human_promotion_commit_failure_restores_fifo_once() {
    human_failure_restores_fifo(true, "base-off");
}

#[test]
fn off_boundary_global_human_failure_keeps_fifo_in_plain_and_on_sessions() {
    for profile in ["plain", "on"] {
        for promotion_commit in [false, true] {
            human_failure_restores_fifo(promotion_commit, profile);
        }
    }
}

fn human_failure_restores_fifo(promotion_commit: bool, profile: &str) {
    let mut f = fixture();
    let old = queue_retained(&f, "older unknown provider attempt", false);
    queue_test_engram_prompt(
        &f.state,
        &f.session,
        "selected Human payload",
        QueuedPromptSource::Human,
        None,
    );
    if profile == "base-off" {
        switch_off(&f);
    }
    if profile == "plain" {
        let mut inner = f.state.inner.lock().unwrap();
        inner
            .projects
            .iter_mut()
            .find(|p| p.id == f.target.project_id)
            .unwrap()
            .engram = None;
    }
    f.state.shutdown_persist_blocking();
    let expected = {
        let mut inner = f.state.inner.lock().unwrap();
        let record = inner.session_mut(&f.session).unwrap();
        record.set_auto_dispatch_blocked(false);
        record.session.status = SessionStatus::Idle;
        if !promotion_commit {
            record.session.agent = Agent::Claude;
            record.runtime = SessionRuntime::None;
        }
        let ids = record
            .queued_prompts
            .iter()
            .map(|entry| entry.pending_prompt.id.clone())
            .collect::<Vec<_>>();
        f.state.commit_locked(&mut inner).unwrap();
        ids
    };
    let reached = Arc::new(AtomicBool::new(false));
    if promotion_commit {
        let reached = reached.clone();
        TEST_HUMAN_BEFORE_PROMOTION.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                reached.store(true, Ordering::SeqCst);
                METADATA_COMMIT_INTEGRITY_FAULT.with(|fault| fault.set(true));
            }));
        });
    } else {
        f.state.agent_runtime_spawning_enabled = false;
    }
    let error = f
        .state
        .start_human_queued_turn(&f.session, false)
        .err()
        .expect("the requested failure must occur");
    let unused_fault = METADATA_COMMIT_INTEGRITY_FAULT.with(|fault| fault.replace(false));
    if promotion_commit {
        assert!(
            reached.load(Ordering::SeqCst) && !unused_fault,
            "the real promotion commit consumed its integrity failure"
        );
        assert!(error.to_string().contains("integrity"), "{error:#}");
    } else {
        assert!(error.to_string().contains("disabled"), "{error:#}");
    }
    let (ids, retained) = {
        let inner = f.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
        (
            record
                .queued_prompts
                .iter()
                .map(|entry| entry.pending_prompt.id.clone())
                .collect::<Vec<_>>(),
            record
                .queued_prompts
                .iter()
                .find(|entry| entry.pending_prompt.id == old.pending_prompt.id)
                .unwrap()
                .clone(),
        )
    };
    assert_eq!(
        ids, expected,
        "promotion_commit={promotion_commit}: exactly one owner restores queue order"
    );
    assert_eq!(
        serde_json::to_value(retained).unwrap(),
        serde_json::to_value(&old).unwrap()
    );
    assert!(f.receiver.try_recv().is_err());
    assert!(f.transport.requests().is_empty());
}

#[test]
fn off_review_plain_claude_handoff_keeps_the_final_state_fence() {
    plain_handoff_keeps_the_final_state_fence(Agent::Claude, "base-off");
}

#[test]
fn off_review_plain_acp_handoff_keeps_the_final_state_fence() {
    plain_handoff_keeps_the_final_state_fence(Agent::Cursor, "base-off");
}

#[test]
fn off_boundary_global_final_handoff_fence_in_plain_and_on_sessions() {
    for profile in ["plain", "on"] {
        for agent in [Agent::Claude, Agent::Cursor] {
            plain_handoff_keeps_the_final_state_fence(agent, profile);
        }
    }
}

fn plain_handoff_keeps_the_final_state_fence(agent: Agent, profile: &str) {
    for change in ["current", "runtime", "generation", "stop", "status"] {
        let (state, session, _receiver, transport) = if profile == "base-off" {
            let f = fixture();
            switch_off(&f);
            (f.state, f.session, f.receiver, f.transport)
        } else {
            // The global fence is independent of admission. Give its ON
            // positive control no unrelated retained checkpoint to withhold it.
            boundary_session(profile)
        };
        let (runtime, receive): (SessionRuntime, Box<dyn Fn() -> Option<String>>) =
            if agent == Agent::Claude {
                let (runtime, receiver) = test_claude_runtime_handle("ordinary-final-fence");
                (
                    SessionRuntime::Claude(runtime),
                    Box::new(move || match receiver.try_recv() {
                        Ok(ClaudeRuntimeCommand::Prompt(command)) => Some(command.text),
                        Err(_) => None,
                        _ => panic!("unexpected Claude command"),
                    }),
                )
            } else {
                let (runtime, receiver) =
                    test_acp_runtime_handle(AcpAgent::Cursor, "ordinary-final-fence");
                (
                    SessionRuntime::Acp(runtime),
                    Box::new(move || match receiver.try_recv() {
                        Ok(AcpRuntimeCommand::Prompt(command)) => Some(command.prompt),
                        Err(_) => None,
                        _ => panic!("unexpected ACP command"),
                    }),
                )
            };
        let dispatch = {
            let mut inner = state.inner.lock().unwrap();
            let message = inner.next_message_id();
            let record = inner.session_mut(&session).unwrap();
            record.session.agent = agent;
            record.runtime = runtime;
            record.session.status = SessionStatus::Idle;
            record.set_auto_dispatch_blocked(false);
            let started = state
                .start_turn_on_record(
                    record,
                    message,
                    "ordinary final handoff payload".into(),
                    Vec::new(),
                    None,
                    None,
                    None,
                    false,
                )
                .unwrap();
            state.commit_locked(&mut inner).unwrap();
            started.dispatch
        };
        assert!(dispatch.human_prompt_id().is_none());
        let expected_runtime = dispatch.runtime_token().clone();
        let expected_generation = dispatch.active_turn_generation();
        {
            let mut inner = state.inner.lock().unwrap();
            let record = inner.session_mut(&session).unwrap();
            match change {
                "runtime" => record.runtime = SessionRuntime::None,
                "generation" => record.active_turn_generation += 1,
                "stop" => record.runtime_stop_in_progress = true,
                "status" => record.session.status = SessionStatus::Idle,
                _ => {}
            }
        }
        let outcome = handoff_prepared_turn_dispatch(&state, dispatch).unwrap();
        if change == "current" {
            assert!(matches!(
                outcome,
                HandoffPreparedTurnDispatchOutcome::Delivered
            ));
            let payload = receive().expect("the current ordinary owner delivers once");
            assert!(payload.ends_with("ordinary final handoff payload"));
        } else if change == "stop" {
            assert!(matches!(
                outcome,
                HandoffPreparedTurnDispatchOutcome::Scheduled
            ));
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            let [
                DeferredStopCallback::TurnHandoff {
                    active_turn_generation,
                    dispatch,
                },
            ] = record.deferred_stop_callbacks.as_slice()
            else {
                panic!("the current Stop must retain exactly one ordinary handoff");
            };
            assert_eq!(*active_turn_generation, expected_generation);
            let held = dispatch.0.lock().unwrap();
            let held = held.as_ref().expect("Stop still owns the unsent dispatch");
            assert_eq!(held.runtime_token(), &expected_runtime);
            assert_eq!(held.active_turn_generation(), expected_generation);
        } else {
            assert!(
                matches!(outcome, HandoffPreparedTurnDispatchOutcome::Superseded),
                "agent={agent:?}, change={change} must not enqueue a stale ordinary turn"
            );
        }
        assert!(
            receive().is_none(),
            "agent={agent:?}, change={change}: no duplicate or stale command"
        );
        assert!(transport.requests().is_empty());
    }
}

#[test]
fn ordinary_wire_omits_removed_control_selectors_and_imported_flags_cannot_gate_turns() {
    for imported in [false, true] {
        let (state, session, receiver, transport) = ordinary_root_fixture();
        let (project, settings) = {
            let inner = state.inner.lock().unwrap();
            let project = inner.sessions[inner.find_session_index(&session).unwrap()]
                .session
                .project_id
                .clone()
                .unwrap();
            let mut raw = serde_json::to_value(
                inner
                    .find_project(&project)
                    .unwrap()
                    .engram
                    .as_ref()
                    .unwrap(),
            )
            .unwrap();
            raw["turnGatedControl"] = json!(imported);
            raw["assurance"] = json!("turn_gated");
            (
                project,
                serde_json::from_value::<EngramProjectSettings>(raw).unwrap(),
            )
        };
        state
            .update_project_engram_settings(&project, settings)
            .unwrap();
        let wire = serde_json::to_value(state.snapshot()).unwrap();
        for session in wire["sessions"].as_array().unwrap() {
            assert!(session.get("engramControlOff").is_none());
        }
        let project = wire["projects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"] == project)
            .unwrap();
        assert!(project.get("engramControlEnabled").is_none());
        assert!(project["engram"].get("turnGatedControl").is_none());
        assert!(project["engram"].get("assurance").is_none());
        let dispatch = match state
            .dispatch_turn(
                &session,
                SendMessageRequest {
                    text: "ordinary imported-settings input".into(),
                    expanded_text: None,
                    attachments: Vec::new(),
                    source_session_id: None,
                    source_mailbox: None,
                },
            )
            .unwrap()
        {
            DispatchTurnResult::Dispatched(dispatch)
            | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
            DispatchTurnResult::Queued => {
                panic!("imported gating flags cannot block ordinary input")
            }
        };
        deliver_turn_dispatch(&state, dispatch).unwrap();
        assert!(
            matches!(receiver.try_recv(), Ok(CodexRuntimeCommand::Prompt { command, .. })
            if command.prompt.contains("ordinary imported-settings input"))
        );
        assert!(receiver.try_recv().is_err());
        assert!(transport.requests().is_empty());
    }
}
