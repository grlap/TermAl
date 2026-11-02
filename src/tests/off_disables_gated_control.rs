//! OFF removes control admission from ordinary execution, preserving old debt.
//! Disposable SQLite, the real queue/wake owners and provider channels are used;
//! only the control transport is scripted. No live authority is contacted.
use super::*;

fn boundary_session(profile: &str) -> RootFixture {
    let (state, session, receiver, transport) = root_fixture([
        bind_reply("boundary-route"), grant_reply("boundary-grant"), begin_reply("boundary-grant"),
    ]);
    {
        let mut inner = state.inner.lock().unwrap();
        let project_id = inner.sessions[inner.find_session_index(&session).unwrap()].session.project_id.clone().unwrap();
        let project = inner.projects.iter_mut().find(|p| p.id == project_id).unwrap();
        match profile {
            "plain" => project.engram = None,
            "base-off" => project.engram.as_mut().unwrap().turn_gated_control = false,
            "disabled" => project.engram.as_mut().unwrap().enabled = false,
            "on" => {},
            _ => panic!("unknown boundary profile"),
        }
    }
    (state, session, receiver, transport)
}

#[test]
fn off_boundary_reset_marks_retained_heads_when_disabled_or_globally_killed() {
    const CHILD_MARKER: &str = "TERMAL_TEST_RETAINED_RESET_NEGATIVE_CHILD";
    let Ok(mode) = std::env::var(CHILD_MARKER) else {
        let module = module_path!().split_once("::").unwrap().1;
        let filter = format!("{module}::off_boundary_reset_marks_retained_heads_when_disabled_or_globally_killed");
        for mode in ["disabled", "kill"] {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command.args(["--exact", &filter, "--nocapture"])
                .env(CHILD_MARKER, mode)
                .env(ENGRAM_GLOBAL_DISABLE_ENV, if mode == "kill" { "1" } else { "0" });
            let (status, stderr) = phase_sync::CapturedStderrProcess::spawn(&mut command)
                .wait_with_stderr("negative reset child publishes its retained-head result");
            assert!(status.success(), "{mode}: {stderr}");
            assert!(stderr.contains(&format!("negative reset {mode}: verified")),
                "the exact child test must execute its assertions: {stderr}");
            eprintln!("negative reset {mode}: verified by isolated child");
        }
        return;
    };
    assert!(matches!(mode.as_str(), "disabled" | "kill"));
    assert_eq!(engram_globally_disabled(), mode == "kill");
    let (state, session, receiver, transport) = boundary_session("on");
    let (project, root) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        (record.session.project_id.clone().unwrap(), PathBuf::from(&record.session.workdir))
    };
    fs::write(root.join(".engram-project"), "fixture-ready\n").unwrap();
    let mut admitted = real_fixture_engram_settings(&root);
    admitted.authority_store_key = Some(materialize_fixture_engram_store(&root, &root));
    {
        let mut inner = state.inner.lock().unwrap();
        inner.projects.iter_mut().find(|p| p.id == project).unwrap().engram = Some(admitted);
        state.commit_locked(&mut inner).unwrap();
    }
    queue_test_engram_prompt(&state, &session, "original retained reset input", QueuedPromptSource::User, None);
    let (project, mut settings, original) = {
        let mut inner = state.inner.lock().unwrap();
        let record = inner.session_mut(&session).unwrap();
        record.runtime = SessionRuntime::None;
        record.session.status = SessionStatus::Idle;
        record.queued_prompts[0].engram_waiting = true;
        record.queued_prompts[0].engram_interrupted = false;
        record.set_auto_dispatch_blocked(false);
        sync_pending_prompts(record);
        assert!(record.queued_prompts[0].is_engram_retained());
        assert!(!record.session.queue_paused);
        assert!(!record.session.pending_prompts[0].engram_interrupted);
        let original = serde_json::to_value(&record.queued_prompts[0]).unwrap();
        let project = record.session.project_id.clone().unwrap();
        let settings = inner.find_project(&project).unwrap().engram.clone().unwrap();
        state.commit_locked(&mut inner).unwrap();
        (project, settings, original)
    };
    if mode == "disabled" { settings.enabled = false; }
    else { settings.turn_gated_control = false; }
    state.update_project_engram_settings(&project, settings).unwrap();
    let inner = state.inner.lock().unwrap();
    assert!(!ordinary_off_execution_applies(&inner, &session));
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.queued_prompts.len(), 1);
    let mut expected = original;
    expected["engram_interrupted"] = json!(true);
    assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), expected);
    assert!(record.orchestrator_auto_dispatch_blocked && record.session.queue_paused);
    assert!(record.session.pending_prompts[0].engram_interrupted);
    assert_eq!(record.session.preview, "Engram control is off; recovered authorization retained. Cancel the queued prompt before continuing.");
    assert!(receiver.try_recv().is_err());
    assert!(transport.requests().is_empty());
    eprintln!("negative reset {mode}: verified");
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
            pause_automatic_resumes_on_success: false, ..Default::default()
        };
        let (_, claim) = state.begin_requested_stop_session(&session, &options).unwrap();
        let claim = claim.expect("the first asynchronous Stop owns cleanup");
        std::thread::scope(|scope| {
            // Drop the gate before scope joins if an assertion unwinds.
            let gate = gate;
            let worker = scope.spawn(|| state.stop_local_session_with_options(&session,
                options, Some(claim)));
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
        assert!(record.engram.operator_paused && record.orchestrator_auto_dispatch_blocked, "{profile}");
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
            state.handle_runtime_exit_if_matches(&session, &token, None).unwrap();
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(matches!(record.runtime, SessionRuntime::None));
            assert_eq!(record.engram.operator_paused, paused, "{profile}");
            assert_eq!(record.orchestrator_auto_dispatch_blocked, paused, "{profile}");
            assert!(transport.requests().is_empty());
        }
    }
}

#[test]
fn off_boundary_ordinary_characterization_matrix() {
    let mut observations = Vec::new();
    for profile in ["plain", "on", "base-off", "disabled"] {
        for action in ["send", "human-stop", "human-priority", "resume", "pause", "manual-recovery"] {
            let (state, session, receiver, _transport) = boundary_session(profile);
            if matches!(action, "human-stop" | "human-priority") {
                let runtime = {
                    let mut inner = state.inner.lock().unwrap();
                    let record = inner.session_mut(&session).unwrap();
                    record.session.status = SessionStatus::Active;
                    std::mem::replace(&mut record.runtime, SessionRuntime::None)
                };
                state.stop_session(&session).unwrap();
                state.inner.lock().unwrap().session_mut(&session).unwrap().runtime = runtime;
            }
            if matches!(action, "human-priority" | "manual-recovery" | "pause") {
                queue_test_engram_prompt(&state, &session, "automatic queued work", QueuedPromptSource::Orchestrator, None);
            } else if action == "resume" {
                queue_test_engram_prompt(&state, &session, "explicit boundary work", QueuedPromptSource::User, None);
            }
            if matches!(action, "resume" | "pause" | "manual-recovery") {
                state.inner.lock().unwrap().session_mut(&session).unwrap().set_auto_dispatch_blocked(true);
            }
            let result = if action == "resume" {
                state.resume_session_queue(&session).map(|_| ())
            } else if action == "pause" {
                let result = state.dispatch_next_queued_turn(&session, false);
                observations.push((profile, action, result.is_ok_and(|dispatch| dispatch.is_none())
                    && receiver.try_recv().is_err()));
                continue;
            } else {
                let request = SendMessageRequest { text: "explicit boundary work".into(),
                    expanded_text: None, attachments: Vec::new(), source_session_id: None, source_mailbox: None };
                let result = if action.starts_with("human") {
                    state.dispatch_human_turn(&session, request)
                } else { state.dispatch_turn(&session, request) };
                result.and_then(|result| match result {
                    DispatchTurnResult::Dispatched(dispatch) | DispatchTurnResult::DispatchedAfterQueue(dispatch) =>
                        deliver_turn_dispatch(&state, dispatch).into_public_result(),
                    DispatchTurnResult::Queued => Err(ApiError::conflict("explicit work was held")),
                })
            };
            let delivered = matches!(receiver.try_recv(), Ok(CodexRuntimeCommand::Prompt { command, .. })
                if command.prompt.contains("explicit boundary work") && !command.prompt.contains("automatic queued work"));
            observations.push((profile, action, result.is_ok() && delivered && receiver.try_recv().is_err()));
        }
        let (state, sender, target) = crate::tests::mailboxes::mailbox_test_state();
        if profile != "plain" {
            let root = state.test_temp_root.as_ref().unwrap().path().join("boundary-mailbox-project");
            fs::create_dir_all(&root).unwrap();
            let project = create_test_project(&state, &root, "Boundary mailbox");
            enable_test_project_engram(&state, &project, &root);
            let mut inner = state.inner.lock().unwrap();
            for id in [&sender, &target] {
                inner.session_mut(id).unwrap().session.project_id = Some(project.clone());
            }
            let settings = inner.projects.iter_mut().find(|p| p.id == project).unwrap().engram.as_mut().unwrap();
            settings.turn_gated_control = profile == "on";
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
            state.append_mailbox_message_and_notify(&sender, SendMailboxMessageRequest {
                target_session_id: target.clone(), message: format!("body {sequence}"),
                idempotency_key: format!("boundary-{sequence}"), topic: None, state_stamp: None, class: None,
            }).unwrap();
        }
        let inner = state.inner.lock().unwrap();
        observations.push((profile, "coalescing", inner.sessions[inner.find_session_index(&target).unwrap()].queued_prompts.len() == 1));
    }
    assert!(observations.iter().all(|(_, _, passed)| *passed), "master behavior matrix: {observations:?}");
}

#[test]
fn off_boundary_remote_project_does_not_assert_local_off_authority() {
    let f = fixture();
    {
        let mut inner = f.state.inner.lock().unwrap();
        let project = inner.projects.iter_mut().find(|p| p.id == f.target.project_id).unwrap();
        project.remote_id = "remote-owner".into();
        project.engram = None;
    }
    let wire = serde_json::to_value(f.state.snapshot()).unwrap();
    let project = wire["projects"].as_array().unwrap().iter().find(|p| p["id"] == f.target.project_id).unwrap();
    assert!(project.get("engramControlEnabled").is_none(), "a proxy cannot derive its owner's control mode");
}

#[test]
fn off_boundary_session_projection_forwards_the_remote_owner_and_refreshes_local_settings() {
    let (state, session, _receiver, _transport) = boundary_session("base-off");
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&session).unwrap();
    let mut upstream = AppState::wire_session_for_inner(&inner, &state.server_instance_id, &inner.sessions[index]);
    assert_eq!(upstream.engram_control_off, Some(true));
    let first = state.immutable_state_snapshot_locked(&inner);
    assert_eq!(first.sessions.iter().find(|s| s.id == session).unwrap().engram_control_off, Some(true));
    let project = upstream.project_id.clone().unwrap();
    inner.projects.iter_mut().find(|p| p.id == project).unwrap().engram.as_mut().unwrap().enabled = false;
    let changed = state.immutable_state_snapshot_locked(&inner);
    assert_eq!(changed.sessions.iter().find(|s| s.id == session).unwrap().engram_control_off, Some(false));
    for effective in [Some(false), Some(true), None] {
        upstream.engram_control_off = effective;
        let proxy = upsert_remote_proxy_session_record(&mut inner, "remote-owner", &upstream, None);
        let record = &inner.sessions[inner.find_session_index(&proxy).unwrap()];
        assert!(!ordinary_off_execution_applies(&inner, &proxy));
        assert_eq!(AppState::wire_session_for_inner(&inner, &state.server_instance_id, record).engram_control_off, effective);
        let summary = AppState::wire_session_summary_for_inner(&inner, &state.server_instance_id, record);
        let localized = localize_remote_session_summary("second-proxy", "localized", None, &summary);
        assert_eq!(localized.engram_control_off, effective);
        let cut = state.immutable_state_snapshot_locked(&inner);
        assert_eq!(cut.sessions.iter().find(|s| s.id == proxy).unwrap().engram_control_off, effective);
        let cold = state.snapshot_from_inner(&inner);
        assert_eq!(cold.sessions.iter().find(|s| s.id == proxy).unwrap().engram_control_off, effective);
    }
}

#[test]
fn off_boundary_session_projection_uses_the_existing_inherited_project() {
    let (state, session, receiver, _transport) = boundary_session("plain");
    let (outer, nested) = create_test_nested_delegations_before_engram(&state, &receiver, &session);
    let (project, root) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        (record.session.project_id.clone().unwrap(), PathBuf::from(&record.session.workdir))
    };
    enable_test_project_engram(&state, &project, &root);
    let mut inner = state.inner.lock().unwrap();
    inner.projects.iter_mut().find(|p| p.id == project).unwrap().engram.as_mut().unwrap().turn_gated_control = false;
    for child in [&outer.child_session_id, &nested.child_session_id] {
        inner.session_mut(child).unwrap().session.project_id = None;
    }
    let index = inner.find_session_index(&nested.child_session_id).unwrap();
    assert!(ordinary_off_execution_applies(&inner, &nested.child_session_id));
    assert_eq!(AppState::wire_session_summary_for_inner(&inner, &state.server_instance_id, &inner.sessions[index]).engram_control_off, Some(true));
    inner.session_mut(&nested.child_session_id).unwrap().session.project_id = Some("missing-project".into());
    assert!(!ordinary_off_execution_applies(&inner, &nested.child_session_id));
}

#[test]
fn off_boundary_indexed_projection_tracks_ancestry_and_inventory_without_session_stamps() {
    fn assert_modes(state: &AppState, inner: &StateInner, expected: &[(&str, bool)]) {
        let first = state.immutable_state_snapshot_locked(inner);
        let warm = state.immutable_state_snapshot_locked(inner);
        let cold = state.snapshot_from_inner(inner);
        let full = state.full_snapshot_from_inner(inner);
        for &(id, off) in expected {
            let cached = first.sessions.iter().find(|s| s.id == id).unwrap();
            let reused = warm.sessions.iter().find(|s| s.id == id).unwrap();
            assert_eq!(cached.engram_control_off, Some(off), "cached {id}");
            assert!(Arc::ptr_eq(cached, reused), "an unchanged cut reuses {id}");
            assert_eq!(cold.sessions.iter().find(|s| s.id == id).unwrap().engram_control_off, Some(off), "cold {id}");
            assert_eq!(full.sessions.iter().find(|s| s.id == id).unwrap().engram_control_off, Some(off), "full {id}");
            let record = &inner.sessions[inner.find_session_index(id).unwrap()];
            assert_eq!(AppState::wire_session_for_inner(inner, &state.server_instance_id, record).engram_control_off,
                Some(off), "targeted {id}");
        }
    }
    let (state, session, receiver, transport) = boundary_session("plain");
    let (outer, nested) = create_test_nested_delegations_before_engram(&state, &receiver, &session);
    let (project, root) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        (record.session.project_id.clone().unwrap(), PathBuf::from(&record.session.workdir))
    };
    enable_test_project_engram(&state, &project, &root);
    let source = delegation_inventory_file_identity(state.persistence_path.as_path()).unwrap();
    let mut inner = state.inner.lock().unwrap();
    for child in [&outer.child_session_id, &nested.child_session_id] {
        inner.session_mut(child).unwrap().session.project_id = None;
    }
    let stamps = inner.sessions.iter().map(|record| record.mutation_stamp).collect::<Vec<_>>();
    inner.projects.iter_mut().find(|p| p.id == project).unwrap().engram.as_mut().unwrap().turn_gated_control = false;
    let ready = [(&*session, true), (&*outer.child_session_id, true), (&*nested.child_session_id, true)];
    assert_modes(&state, &inner, &ready);
    inner.projects.iter_mut().find(|p| p.id == project).unwrap().engram.as_mut().unwrap().enabled = false;
    let unavailable = [(&*session, false), (&*outer.child_session_id, false), (&*nested.child_session_id, false)];
    assert_modes(&state, &inner, &unavailable);
    inner.projects.iter_mut().find(|p| p.id == project).unwrap().engram.as_mut().unwrap().enabled = true;
    assert_modes(&state, &inner, &ready);
    // A cycle is unresolved, even when another root in the cut is Base/OFF.
    let edge = inner.delegations.iter_mut().find(|d| d.child_session_id == outer.child_session_id).unwrap();
    let original_parent = std::mem::replace(&mut edge.parent_session_id, nested.child_session_id.clone());
    assert_modes(&state, &inner, &[(&session, true), (&outer.child_session_id, false), (&nested.child_session_id, false)]);
    inner.delegations.iter_mut().find(|d| d.child_session_id == outer.child_session_id).unwrap().parent_session_id = original_parent;
    assert_modes(&state, &inner, &ready);
    let nested_index = inner.find_session_index(&nested.child_session_id).unwrap();
    inner.sessions[nested_index].session.project_id = Some("missing-project".into());
    assert_modes(&state, &inner, &[(&session, true), (&outer.child_session_id, true), (&nested.child_session_id, false)]);
    inner.sessions[nested_index].session.project_id = None;
    assert_modes(&state, &inner, &ready);
    inner.delegation_inventory = DelegationInventoryState::Loading {
        token: "projection-loading".into(), source, worker_started: false, queue_pause_images: Vec::new(),
    };
    assert_modes(&state, &inner, &unavailable);
    inner.delegation_inventory = DelegationInventoryState::Ready;
    assert_modes(&state, &inner, &ready);
    assert_eq!(inner.sessions.iter().map(|record| record.mutation_stamp).collect::<Vec<_>>(), stamps,
        "project, ancestry and inventory inputs change independently of session stamps");
    assert!(transport.requests().is_empty());
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
    let dispatch = match f.state.dispatch_turn(&f.session, SendMessageRequest {
        text: "explicit ordinary recovery".into(), expanded_text: None, attachments: Vec::new(),
        source_session_id: None, source_mailbox: None,
    }).unwrap() {
        DispatchTurnResult::Dispatched(dispatch) | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("ordinary recovery must start"),
    };
    deliver_turn_dispatch(&f.state, dispatch).unwrap();
    receive_prompt(&f, "explicit ordinary recovery");
    {
        let inner = f.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
        assert!(!record.session.queue_paused);
        assert!(!record.engram.operator_paused, "a started turn cannot retain an invisible Stop latch");
    }
    queue_test_engram_prompt(&f.state, &f.session, "Human after recovery", QueuedPromptSource::Human, None);
    {
        let mut inner = f.state.inner.lock().unwrap();
        inner.session_mut(&f.session).unwrap().session.status = SessionStatus::Idle;
    }
    let HumanPromptDrain::Started(dispatch) = f.state.start_human_queued_turn(&f.session, false).unwrap() else {
        panic!("the recovered queue must automatically drain its Human prompt");
    };
    deliver_turn_dispatch(&f.state, dispatch.dispatch).unwrap();
    receive_prompt(&f, "Human after recovery");
    assert!(f.transport.requests().is_empty());
}

fn off_orphaned_workflow_after_history(boot_scan: bool) {
    let f = fixture();
    let historical = queue_retained(&f, "unknown historical User", false);
    queue_test_engram_prompt(&f.state, &f.session, "orphaned workflow resume", QueuedPromptSource::Orchestrator, None);
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
    *f.state.inner.lock().unwrap() = load_state(f.state.persistence_path.as_path()).unwrap().unwrap();
    let loaded_history = {
        let inner = f.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
        assert_eq!(record.queued_prompts[0].pending_prompt.id, historical.pending_prompt.id);
        serde_json::to_value(&record.queued_prompts[0]).unwrap()
    };
    let (runtime, prompts, _process) = test_shared_codex_runtime("off-orphan-after-history");
    install_single_wire_codex_fixture(&f.state, Some(runtime));
    if boot_scan {
        f.state.dispatch_orphaned_workflow_prompts();
    } else {
        let started = f.state.start_orphaned_workflow_turn(&f.session).unwrap()
            .expect("the locked orphan check must ignore non-runnable history");
        deliver_turn_dispatch(&f.state, started.dispatch).unwrap();
    }
    assert!(matches!(prompts.try_recv(), Ok(CodexRuntimeCommand::Prompt { command, .. })
        if command.prompt.contains("orphaned workflow resume")), "restart must activate the committed workflow");
    assert!(prompts.try_recv().is_err());
    let inner = f.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), loaded_history);
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

#[test]
fn off_pair_session_snapshot_keeps_disabled_integration_outside_the_off_lane() {
    let f = fixture();
    {
        let mut inner = f.state.inner.lock().unwrap();
        let settings = inner.projects.iter_mut().find(|p| p.id == f.target.project_id)
            .unwrap().engram.as_mut().unwrap();
        settings.turn_gated_control = true;
        settings.enabled = false;
    }
    let wire = serde_json::to_value(f.state.snapshot()).unwrap();
    let project = wire["projects"].as_array().unwrap().iter()
        .find(|p| p["id"] == f.target.project_id).unwrap();
    assert_eq!(project["engram"]["turnGatedControl"], true);
    let session = wire["sessions"].as_array().unwrap().iter()
        .find(|session| session["id"] == f.session).unwrap();
    assert_eq!(session["engramControlOff"], false);
    assert!(project.get("engramControlEnabled").is_none());
}

struct OffFixture {
    state: AppState,
    session: String,
    receiver: mpsc::Receiver<CodexRuntimeCommand>,
    transport: Arc<ScriptedEngramControlTransport>,
    target: EngramBindingTarget,
    debt: Value,
}

fn fixture() -> OffFixture {
    let (state, session, receiver, transport) = root_fixture([]);
    let target = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let project = inner.sessions[index].session.project_id.clone().unwrap();
        let root = PathBuf::from(&inner.find_project(&project).unwrap().root_path);
        let authority = materialize_fixture_engram_store(&root, &root);
        inner
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .unwrap()
            .engram
            .as_mut()
            .unwrap()
            .authority_store_key = Some(authority);
        let record = inner.session_mut_by_index(index).unwrap();
        record.engram.routing_token = Some("original-routing-token".into());
        record.engram.active_grant_id = Some("original-open-grant".into());
        record.engram.begins_recorded = true;
        record.engram.dispatch_generation = 1;
        record.active_turn_generation = 1;
        record.set_auto_dispatch_blocked(true);
        let target =
            AppState::engram_binding_target_for_session_shape_locked(&inner, &session, false)
                .unwrap()
                .unwrap();
        let original = EngramExistingCheckpoint {
            id: "original-checkpoint".into(),
            authority: engram_abort_authority(&target),
            connection: target.connection.clone(),
            settings: target.settings.clone(),
            work_binding: target.work_binding.clone(),
            request: EngramControlRequest::TurnCheckpoint {
                routing_token: "original-routing-token".into(),
                grant_id: "original-open-grant".into(),
                next_intent: EngramNextIntent::Wait,
                report: EngramTurnReport::default(),
                idempotency_key: "original-checkpoint-key".into(),
            },
            dispatch_generation: 1,
            active_turn_generation: 1,
            runtime_episode: format!("{:?}", inner.sessions[index].runtime.runtime_token()),
            checkpoint_owner_generation: None,
        };
        let operation = state
            .prepare_existing_checkpoint_protection(&inner, &session, &target, original)
            .unwrap();
        operation.validate().unwrap();
        assert_eq!(
            operation.protection.phase,
            EngramSessionObligationPhase::Intended
        );
        assert!(matches!(
            operation.progress,
            EngramExistingCheckpointProgress::Retained
        ));
        inner
            .engram_session_obligations
            .checkpoint_operations
            .push(operation.clone());
        inner.engram_session_obligations.validate().unwrap();
        inner
            .session_mut_by_index(index)
            .unwrap()
            .engram
            .checkpoint_operation = Some(operation);
        state.commit_locked(&mut inner).unwrap();
        target
    };
    let debt = ledger(&state);
    assert!(transport.requests().is_empty());
    OffFixture {
        state,
        session,
        receiver,
        transport,
        target,
        debt,
    }
}

fn ledger(state: &AppState) -> Value {
    serde_json::to_value(&state.inner.lock().unwrap().engram_session_obligations).unwrap()
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
        .turn_gated_control = false;
    f.state.commit_locked(&mut inner).unwrap();
}

fn assert_debt_unchanged(f: &OffFixture) {
    assert_eq!(
        ledger(&f.state),
        f.debt,
        "OFF must not settle or rewrite historical evidence"
    );
    let loaded = load_state(f.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&loaded.engram_session_obligations).unwrap(),
        f.debt
    );
    let inner = f.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
    assert_eq!(
        record.engram.active_grant_id.as_deref(),
        Some("original-open-grant")
    );
    assert_eq!(
        record.engram.routing_token.as_deref(),
        Some("original-routing-token")
    );
    assert_eq!(
        serde_json::to_value(&record.engram.checkpoint_operation).unwrap(),
        f.debt["checkpointOperations"][0]
    );
}

fn queue_retained(f: &OffFixture, text: &str, never_promoted: bool) -> QueuedPromptRecord {
    queue_test_engram_prompt(&f.state, &f.session, text, QueuedPromptSource::User, None);
    let mut inner = f.state.inner.lock().unwrap();
    let index = inner.find_session_index(&f.session).unwrap();
    let record = inner.session_mut_by_index(index).unwrap();
    let queued = record.queued_prompts.back_mut().unwrap();
    queued.engram_waiting = true;
    queued.engram_interrupted = true;
    queued.promotion_disposition_known = never_promoted;
    queued.promoted_message_index = None;
    queued.engram_ordinary_handoff = !never_promoted;
    queued.engram_bind = Some(EngramQueuedBind {
        connection: f.target.connection.clone(),
        settings: f.target.settings.clone(),
        request: EngramControlRequest::SessionBind {
            external_ref: f.target.external_ref.clone(),
            title: "Original retained prompt".into(),
            assurance: ENGRAM_CONTROL_ASSURANCE.into(),
            mediated_effects: Vec::new(),
            capability_map_revision: 1,
            work_binding: None,
            idempotency_key: format!("original-bind-{}", queued.pending_prompt.id),
        },
        operation_generation: Some(1),
    });
    let saved = queued.clone();
    if record.orchestrator_auto_dispatch_blocked && !record.engram.operator_paused {
        // Use the existing control owner, rather than infer pause provenance
        // from the mere presence of old authorization in the queue.
        assert!(hold_engram_queued_authorization_for_resume(record));
    }
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
fn off_fresh_human_send_ignores_control_holds_and_resumes_operator_pause_but_preserves_stop() {
    for (stopping, operator_pause, control_pause) in [
        (false, false, false), (true, false, false),
        (false, true, false), (false, false, true),
    ] {
        let f = fixture();
        switch_off(&f);
        {
            let mut inner = f.state.inner.lock().unwrap();
            let index = inner.find_session_index(&f.session).unwrap();
            let record = &mut inner.sessions[index];
            record.set_auto_dispatch_blocked(operator_pause || control_pause);
            record.engram.operator_paused = operator_pause;
            record.engram.admission_in_progress =
                Some(Arc::new(std::sync::atomic::AtomicBool::new(false)));
            record.engram.bind_in_progress = true;
            record.engram.checkpoint_in_progress = true;
            record.engram.source_observation_preparations = 1;
            record.engram_boot_recovery_pending = true;
            record.runtime_stop_in_progress = stopping;
            f.state.commit_locked(&mut inner).unwrap();
        }
        let result = f
            .state
            .dispatch_human_turn(
                &f.session,
                SendMessageRequest {
                    text: "fresh human without old control admission".into(),
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
                    panic!("a control RPC must not hold a fresh OFF turn")
                }
            };
            deliver_turn_dispatch(&f.state, dispatch).unwrap();
            receive_prompt(&f, "fresh human without old control admission");
            let token = {
                let inner = f.state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
                assert_eq!(record.engram.dispatch_generation, 1);
                assert!(record.engram.bind_in_progress && record.engram.checkpoint_in_progress);
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
        let inner = f.state.inner.lock().unwrap();
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
    let state = AppState::new_with_paths_engram_transport_and_clock_for_test(
        workdir,
        f.state.persistence_path.as_ref().clone(),
        f.state.orchestrator_templates_path.as_ref().clone(),
        transport.clone(),
        EngramBudgetClock::scripted(),
    )
    .expect("OFF state should restart without settling historical control");
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
        assert!(!record.engram_boot_recovery_pending);
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
fn off_startup_resolves_the_lane_after_inventory_and_preserves_a_new_operator_pause() {
    for new_pause in [false, true] {
        let f = fixture();
        queue_retained(&f, "unknown provider attempt during startup", false);
        switch_off(&f);
        f.state.shutdown_persist_blocking();
        let workdir = {
            let inner = f.state.inner.lock().unwrap();
            inner.find_project(&f.target.project_id).unwrap().root_path.clone()
        };
        let state = boot_on_test_temp_root(f.state.test_temp_root.as_ref().unwrap(), || {
            AppState::new_before_listen_with_paths(workdir,
                f.state.persistence_path.as_ref().clone(),
                f.state.orchestrator_templates_path.as_ref().clone())
        }).unwrap();
        let transport = ScriptedEngramControlTransport::new([]);
        state.install_control_test_transport(transport.clone());
        {
            let inner = state.inner.lock().unwrap();
            assert!(!inner.delegation_inventory_complete());
            assert!(!ordinary_off_execution_applies(&inner, &f.session));
            assert!(inner.sessions[inner.find_session_index(&f.session).unwrap()].orchestrator_auto_dispatch_blocked);
        }
        let observed = AtomicBool::new(false);
        state.load_delegation_inventory_observed(|phase| {
            if phase == "before_publish" {
                observed.store(true, Ordering::SeqCst);
                if new_pause {
                    let mut inner = state.inner.lock().unwrap();
                    let record = inner.session_mut(&f.session).unwrap();
                    record.set_auto_dispatch_blocked(true);
                    record.engram.operator_paused = true;
                }
            }
        }).unwrap();
        assert!(observed.load(Ordering::SeqCst));
        {
            let inner = state.inner.lock().unwrap();
            assert!(inner.delegation_inventory_complete());
            assert!(ordinary_off_execution_applies(&inner, &f.session));
            let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
            assert_eq!(record.orchestrator_auto_dispatch_blocked, new_pause);
            assert_eq!(record.engram.operator_paused, new_pause);
            assert!(record.queued_prompts[0].engram_ordinary_handoff);
        }
        assert!(transport.requests().is_empty());
        state.shutdown_persist_blocking();
    }
}

#[test]
fn on_retained_debt_still_blocks_and_clean_turn_still_evaluates_and_begins() {
    for scenario in [
        "empty",
        "never_promoted",
        "unknown_handoff",
        "mailbox",
        "reset",
    ] {
        let mut f = fixture();
        let saved_head = match scenario {
            "never_promoted" => Some(queue_retained(&f, "ON retained head", true)),
            "unknown_handoff" => {
                let head = queue_retained(&f, "ON unknown handoff", false);
                queue_test_engram_prompt(
                    &f.state,
                    &f.session,
                    "ON successor stays held",
                    QueuedPromptSource::User,
                    None,
                );
                Some(head)
            }
            _ => None,
        };
        match scenario {
            "mailbox" => {
                let path = resolve_coordination_persistence_path(f.state.persistence_path.as_ref());
                f.state.mailbox_store = Arc::new(MailboxStore::open(&path).unwrap());
                let sender = test_session_id(&f.state, Agent::Claude);
                {
                    let mut inner = f.state.inner.lock().unwrap();
                    let index = inner.find_session_index(&f.session).unwrap();
                    inner
                        .session_mut_by_index(index)
                        .unwrap()
                        .set_auto_dispatch_blocked(false);
                    f.state.commit_locked(&mut inner).unwrap();
                }
                f.state
                    .append_mailbox_message_and_notify(
                        &sender,
                        SendMailboxMessageRequest {
                            target_session_id: f.session.clone(),
                            message: "ON mailbox stays held".into(),
                            idempotency_key: "on-original-mail".into(),
                            topic: None,
                            state_stamp: None,
                            class: None,
                        },
                    )
                    .unwrap();
            }
            "reset" => assert!(
                f.state
                    .prepare_dedicated_runtime_reset_off_lock(&f.session, false)
                    .is_err()
            ),
            _ => assert!(
                f.state.resume_session_queue(&f.session).is_err(),
                "ON {scenario} must still refuse unresolved control debt"
            ),
        }
        assert!(
            f.receiver.try_recv().is_err(),
            "ON {scenario} must not deliver"
        );
        if let Some(head) = saved_head {
            let inner = f.state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
            assert_eq!(
                record.queued_prompts[0].pending_prompt.id,
                head.pending_prompt.id
            );
            assert!(record.orchestrator_auto_dispatch_blocked);
        }
        assert_eq!(
            ledger(&f.state)["checkpointOperations"][0]["original"],
            f.debt["checkpointOperations"][0]["original"],
            "ON keeps the original checkpoint identity"
        );
    }

    let (state, session, receiver, transport) = root_fixture([
        bind_reply("on-token"),
        grant_reply("on-grant"),
        begin_reply("on-grant"),
    ]);
    let dispatch = root_dispatch(&state, &session, false);
    deliver_turn_dispatch(&state, dispatch).unwrap();
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    assert_eq!(
        operations(&transport),
        ["session_bind", "turn_evaluate", "turn_begin"]
    );
}

#[test]
fn off_review_stop_preserves_the_historical_pending_dispatch() {
    let f = fixture();
    switch_off(&f);
    {
        let mut inner = f.state.inner.lock().unwrap();
        let record = inner.session_mut(&f.session).unwrap();
        record.set_auto_dispatch_blocked(false);
        record.engram.pending_dispatch = Some(pending_engram_grant(1, "old-pending-grant"));
        f.state.commit_locked(&mut inner).unwrap();
    }
    let dispatch = match f.state.dispatch_turn(&f.session, SendMessageRequest {
        text: "ordinary turn to stop".into(), expanded_text: None, attachments: Vec::new(),
        source_session_id: None, source_mailbox: None,
    }).unwrap() {
        DispatchTurnResult::Dispatched(dispatch) | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("ordinary OFF turn must start"),
    };
    deliver_turn_dispatch(&f.state, dispatch).unwrap();
    receive_prompt(&f, "ordinary turn to stop");
    {
        let mut inner = f.state.inner.lock().unwrap();
        let record = inner.session_mut(&f.session).unwrap();
        assert_eq!(record.unmediated_turn_generation, Some(record.active_turn_generation));
        // Exercise the canonical missing-runtime Stop completion; physical
        // provider cleanup has separate real-process Stop coverage.
        record.runtime = SessionRuntime::None;
        f.state.commit_locked(&mut inner).unwrap();
    }
    f.state.stop_session(&f.session).unwrap();
    let observed = {
        let inner = f.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
        (record.engram.pending_dispatch.as_ref().map(|pending| pending.intent_fingerprint.clone()),
            record.engram.dispatch_generation, record.engram.rebind_required,
            record.engram.uncertain_grant_id.clone(), record.engram.operator_paused)
    };
    assert_eq!(observed, (Some("pending-grant-1".into()), 1, false, None, true));
    assert!(f.receiver.try_recv().is_err());
    assert!(f.transport.requests().is_empty());
    assert_debt_unchanged(&f);
}

#[test]
fn off_review_selected_unknown_head_never_sends_its_successor() {
    let f = fixture();
    let unknown = queue_retained(&f, "selected unknown attempt", false);
    queue_test_engram_prompt(&f.state, &f.session, "unselected successor", QueuedPromptSource::User, None);
    switch_off(&f);
    let selection = {
        let inner = f.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
        HumanPromptSelection { prompt: projected_pending_prompts(record)[0].clone(),
            session_mutation_stamp: record.mutation_stamp }
    };
    let result = f.state.resume_selected_human_prompt(&f.session, selection);
    assert!(matches!(result, Err(error) if error.status == StatusCode::CONFLICT));
    assert!(f.receiver.try_recv().is_err(), "the selected unknown row authorizes no successor");
    let current = {
        let inner = f.state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&f.session).unwrap()].queued_prompts[0].clone()
    };
    assert_eq!(serde_json::to_value(current).unwrap(), serde_json::to_value(unknown).unwrap());
}

fn failed_mailbox_pause_fixture() -> OffFixture {
    let mut f = fixture();
    let path = resolve_coordination_persistence_path(f.state.persistence_path.as_ref());
    f.state.mailbox_store = Arc::new(MailboxStore::open(&path).unwrap());
    let sender = test_session_id(&f.state, Agent::Claude);
    switch_off(&f);
    {
        let mut inner = f.state.inner.lock().unwrap();
        inner.session_mut(&f.session).unwrap().set_auto_dispatch_blocked(false);
        f.state.commit_locked(&mut inner).unwrap();
    }
    let receipt = f.state.append_mailbox_message_and_notify(&sender, SendMailboxMessageRequest {
        target_session_id: f.session.clone(), message: "mailbox payload that fails".into(),
        idempotency_key: "ordinary-failed-mailbox".into(), topic: None, state_stamp: None,
        class: Some("routine".into()),
    }).unwrap();
    receive_prompt(&f, &receipt.mailbox_id);
    queue_retained(&f, "separate historical unknown attempt", false);
    let token = {
        let inner = f.state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&f.session).unwrap()].runtime.runtime_token().unwrap()
    };
    f.state.fail_turn_if_runtime_matches(&f.session, &token, "failed mailbox payload; inspect before retry").unwrap();
    let pause = {
        let inner = f.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
        (record.orchestrator_auto_dispatch_blocked, record.engram.operator_paused)
    };
    assert_eq!(pause, (true, false), "the real mailbox failure owns an ordinary pause");
    assert!(f.receiver.try_recv().is_err());
    f
}

#[test]
fn off_review_failed_mailbox_pause_survives_reset_release() {
    let f = failed_mailbox_pause_fixture();
    let generation = {
        let mut inner = f.state.inner.lock().unwrap();
        let generation = inner.engram_project_resets.claim(&f.target.project_id).unwrap();
        inner.session_mut(&f.session).unwrap().engram.project_reset_in_progress = true;
        generation
    };
    f.state.release_engram_project_reset_fence(&f.target.project_id, generation, std::slice::from_ref(&f.session));
    let paused = {
        let inner = f.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
        record.orchestrator_auto_dispatch_blocked && record.session.queue_paused
    };
    assert!(paused, "reset release must not erase a failed-mailbox pause");
    assert!(f.receiver.try_recv().is_err());
    resume_failed_mailbox(&f);
}

#[test]
fn off_review_failed_mailbox_pause_survives_reload() {
    let f = failed_mailbox_pause_fixture();
    f.state.shutdown_persist_blocking();
    let loaded = load_state(f.state.persistence_path.as_path()).unwrap().unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&f.session).unwrap()];
    assert!(record.orchestrator_auto_dispatch_blocked && record.session.queue_paused,
        "reload must not erase a failed-mailbox pause beside old control intent");
    assert!(!record.engram.operator_paused);
    *f.state.inner.lock().unwrap() = loaded;
    resume_failed_mailbox(&f);
}

fn resume_failed_mailbox(f: &OffFixture) {
    let text = {
        let inner = f.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
        assert!(record.session.preview.contains("failed mailbox payload"),
            "the ordinary error preview remains visible");
        record.queued_prompts.iter().find(|queued| queued.source == QueuedPromptSource::Mailbox)
            .expect("the failed wake stays queued").pending_prompt.text.clone()
    };
    f.state.resume_session_queue(&f.session).unwrap();
    receive_prompt(f, &text);
    assert!(f.transport.requests().is_empty());
}

#[test]
fn off_review_positive_control_pause_releases_at_off() {
    for (legacy_preview, reload) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut f = fixture();
        queue_retained(&f, "positively control-owned pause", true);
        {
            let mut inner = f.state.inner.lock().unwrap();
            let record = inner.session_mut(&f.session).unwrap();
            assert!(if legacy_preview {
                hold_engram_queued_authorization_when_control_off(record)
            } else {
                hold_engram_queued_authorization_for_resume(record)
            });
            f.state.commit_locked(&mut inner).unwrap();
        }
        switch_off(&f);
        assert!(f.state.ordinary_off_execution_applies(&f.session));
        let (paused, preview) = if reload {
            f.state.shutdown_persist_blocking();
            let loaded = load_state(f.state.persistence_path.as_path()).unwrap().unwrap();
            let record = &loaded.sessions[loaded.find_session_index(&f.session).unwrap()];
            let presentation = (record.orchestrator_auto_dispatch_blocked, record.session.preview.clone());
            *f.state.inner.lock().unwrap() = loaded;
            let (runtime, receiver, _process) = test_shared_codex_runtime("off-control-pause-reloaded");
            install_single_wire_codex_fixture(&f.state, Some(runtime));
            f.receiver = receiver;
            presentation
        } else {
            let generation = f.state.inner.lock().unwrap().engram_project_resets.claim(&f.target.project_id).unwrap();
            f.state.release_engram_project_reset_fence(&f.target.project_id, generation, std::slice::from_ref(&f.session));
            let inner = f.state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
            (record.orchestrator_auto_dispatch_blocked, record.session.preview.clone())
        };
        assert!(!paused, "a positively control-owned pause releases at OFF, reload={reload}");
        assert_eq!(preview, "Engram recovered authorization retained. Resume to recover the exact queued prompt.",
            "positive OFF must not tell the operator to cancel recoverable input");
        f.state.resume_session_queue(&f.session).unwrap();
        receive_prompt(&f, "positively control-owned pause");
        assert!(f.transport.requests().is_empty());
        eprintln!("positive OFF recovery preview legacy={legacy_preview} reload={reload}: verified");
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
        record.prompt_deliveries.insert(head.pending_prompt.id.clone(), PromptDelivery {
            federation_operation: None, federation_handoff_started: false,
            disposition: PromptDeliveryDisposition::HandingOff, attempt: 1,
            write_ahead: "original-unknown-write-ahead".into(), sequence: 1,
            message_id: Some(head.pending_prompt.id.clone()), turn_generation: Some(1),
            attempt_key: Some("original-unknown-attempt".into()), reason: None,
            entry: Some(head.clone()), write_ahead_acknowledged: true,
            resume: None, runtime_write: PromptRuntimeWrite::Untracked,
        });
        f.state.commit_locked(&mut inner).unwrap();
    }
    f.state.shutdown_persist_blocking();
    let loaded = load_state(f.state.persistence_path.as_path()).unwrap().unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&f.session).unwrap()];
    let delivery = &record.prompt_deliveries[&head.pending_prompt.id];
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Ambiguous);
    assert_eq!(delivery.write_ahead, "original-unknown-write-ahead");
    assert!(delivery.entry.is_none());
    assert!(record.queued_prompts.iter().all(|queued| queued.pending_prompt.id != head.pending_prompt.id),
        "OFF must not reinsert the row that restart receipt settlement retired");
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
                inner.sessions[inner.find_session_index(&f.session).unwrap()].runtime.runtime_token().unwrap()
            };
            f.state.finish_turn_ok_if_runtime_matches(&f.session, &token).unwrap();
        } else {
            let mut inner = f.state.inner.lock().unwrap();
            inner.session_mut(&f.session).unwrap().set_auto_dispatch_blocked(false);
        }
        let queued = {
            let mut inner = f.state.inner.lock().unwrap();
            let queued = queue_delegation_wait_resume_locked(&mut inner, &f.session,
                "completed wait ordinary resume".into());
            f.state.commit_locked(&mut inner).unwrap();
            queued
        };
        assert!(queued.prompt_queued);
        assert!(queued.dispatch_requested, "retained history cannot strand delegation or test-run fan-in");
        let dispatch = f.state.dispatch_next_queued_turn(&f.session, false).unwrap().unwrap();
        deliver_turn_dispatch(&f.state, dispatch).unwrap();
        receive_prompt(&f, "completed wait ordinary resume");
        let inner = f.state.inner.lock().unwrap();
        assert!(inner.sessions[inner.find_session_index(&f.session).unwrap()].queued_prompts
            .iter().any(|queued| queued.pending_prompt.id == head.pending_prompt.id));
    }
}

#[test]
fn off_review_retained_only_consolidation_does_not_hide_a_deadlock() {
    let f = fixture();
    switch_off(&f);
    let template = f.state.create_orchestrator_template(sample_deadlocked_orchestrator_template_draft())
        .unwrap().template;
    let instance = f.state.create_orchestrator_instance_with_agent_setup_validator(
        CreateOrchestratorInstanceRequest { template_id: template.id,
            project_id: Some(f.target.project_id.clone()), template: None }, |_, _| Ok(()),
    ).unwrap().orchestrator;
    let session = |template_id: &str| instance.session_instances.iter()
        .find(|session| session.template_session_id == template_id).unwrap().session_id.clone();
    let a = session("consolidate-a");
    let b = session("consolidate-b");
    queue_test_engram_prompt(&f.state, &a, "unknown consolidation history", QueuedPromptSource::User, None);
    let (blocked, retained) = {
        let mut inner = f.state.inner.lock().unwrap();
        let record = inner.session_mut(&a).unwrap();
        let head = record.queued_prompts.back_mut().unwrap();
        head.engram_ordinary_handoff = true;
        head.engram_interrupted = true;
        head.promotion_disposition_known = false;
        let retained = head.clone();
        let index = inner.orchestrator_instances.iter().position(|value| value.id == instance.id).unwrap();
        let instance = &mut inner.orchestrator_instances[index];
        instance.status = OrchestratorInstanceStatus::Running;
        instance.pending_transitions = vec![
            PendingTransition { id: "pending-a".into(), transition_id: "source-a-to-consolidate-a".into(),
                source_session_id: session("source-a"), destination_session_id: a.clone(),
                completion_revision: 1, rendered_prompt: "source A result".into(), created_at: stamp_now() },
            PendingTransition { id: "pending-b".into(), transition_id: "source-b-to-consolidate-b".into(),
                source_session_id: session("source-b"), destination_session_id: b.clone(),
                completion_revision: 2, rendered_prompt: "source B result".into(), created_at: stamp_now() },
        ];
        (detect_deadlocked_consolidate_session_ids(&inner, &inner.orchestrator_instances[index]), retained)
    };
    assert_eq!(blocked.into_iter().collect::<HashSet<_>>(), HashSet::from([a.clone(), b]),
        "non-runnable history must not hide the real consolidation cycle");
    let (marked, current) = {
        let mut inner = f.state.inner.lock().unwrap();
        let marked = mark_deadlocked_orchestrator_instances(&mut inner, &HashSet::new());
        let record = &inner.sessions[inner.find_session_index(&a).unwrap()];
        (marked, record.queued_prompts.front().unwrap().clone())
    };
    assert!(marked);
    assert_eq!(serde_json::to_value(current).unwrap(), serde_json::to_value(retained).unwrap());
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
        queue_test_engram_prompt(&f.state, &f.session, "selected Human payload", QueuedPromptSource::Human, None);
        if profile == "base-off" { switch_off(&f); }
        if profile == "plain" {
            let mut inner = f.state.inner.lock().unwrap();
            inner.projects.iter_mut().find(|p| p.id == f.target.project_id).unwrap().engram = None;
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
            let ids = record.queued_prompts.iter().map(|entry| entry.pending_prompt.id.clone()).collect::<Vec<_>>();
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
        let error = f.state.start_human_queued_turn(&f.session, false).err().expect("the requested failure must occur");
        let unused_fault = METADATA_COMMIT_INTEGRITY_FAULT.with(|fault| fault.replace(false));
        if promotion_commit {
            assert!(reached.load(Ordering::SeqCst) && !unused_fault,
                "the real promotion commit consumed its integrity failure");
            assert!(error.to_string().contains("integrity"), "{error:#}");
        } else {
            assert!(error.to_string().contains("disabled"), "{error:#}");
        }
        let (ids, retained) = {
            let inner = f.state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&f.session).unwrap()];
            (record.queued_prompts.iter().map(|entry| entry.pending_prompt.id.clone()).collect::<Vec<_>>(),
                record.queued_prompts.iter().find(|entry| entry.pending_prompt.id == old.pending_prompt.id).unwrap().clone())
        };
        assert_eq!(ids, expected, "promotion_commit={promotion_commit}: exactly one owner restores queue order");
        assert_eq!(serde_json::to_value(retained).unwrap(), serde_json::to_value(&old).unwrap());
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
            let (runtime, receive): (SessionRuntime, Box<dyn Fn() -> Option<String>>) = if agent == Agent::Claude {
                let (runtime, receiver) = test_claude_runtime_handle("ordinary-final-fence");
                (SessionRuntime::Claude(runtime), Box::new(move || match receiver.try_recv() {
                    Ok(ClaudeRuntimeCommand::Prompt(command)) => Some(command.text),
                    Err(_) => None,
                    _ => panic!("unexpected Claude command"),
                }))
            } else {
                let (runtime, receiver) = test_acp_runtime_handle(AcpAgent::Cursor, "ordinary-final-fence");
                (SessionRuntime::Acp(runtime), Box::new(move || match receiver.try_recv() {
                    Ok(AcpRuntimeCommand::Prompt(command)) => Some(command.prompt),
                    Err(_) => None,
                    _ => panic!("unexpected ACP command"),
                }))
            };
            let dispatch = {
                let mut inner = state.inner.lock().unwrap();
                let message = inner.next_message_id();
                let record = inner.session_mut(&session).unwrap();
                record.session.agent = agent;
                record.runtime = runtime;
                record.session.status = SessionStatus::Idle;
                record.set_auto_dispatch_blocked(false);
                let started = state.start_turn_on_record_with_control_mode(record, message,
                    "ordinary final handoff payload".into(), Vec::new(), None, None, None, None, false, profile != "base-off").unwrap();
                state.commit_locked(&mut inner).unwrap();
                started.dispatch
            };
            assert!(dispatch.human_prompt_id().is_none() && dispatch.engram_dispatch_generation().is_none());
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
                assert!(matches!(outcome, HandoffPreparedTurnDispatchOutcome::Delivered));
                let payload = receive().expect("the current ordinary owner delivers once");
                assert!(payload.ends_with("ordinary final handoff payload"));
            } else {
                assert!(matches!(outcome, HandoffPreparedTurnDispatchOutcome::Superseded),
                    "agent={agent:?}, change={change} must not enqueue a stale ordinary turn");
            }
            assert!(receive().is_none(), "agent={agent:?}, change={change}: no duplicate or stale command");
            assert!(transport.requests().is_empty());
        }
}
