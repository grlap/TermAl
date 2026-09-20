// Isolated-store contract and strict Save regressions for tm-cnsv.8.
// The CLI double supplies receipts; production state/reset/persistence paths
// remain real. No live TermAl or Engram store is accessed.
// Nested under engram_host_adapter to reuse its private scripted transports.
use super::*;
use std::time::Instant;

fn setup() -> (AppState, String, String, PathBuf) {
    setup_with_state(test_app_state())
}

fn setup_with_state(state: AppState) -> (AppState, String, String, PathBuf) {
    let root = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("absence-project");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join(".engram-project"), "fixture-ready\n").unwrap();
    let database = work_database_path(&root, "fixture-ready");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    fs::write(&database, "test database").unwrap();
    let project = create_test_project(&state, &root, "Absence reconciliation");
    let session = create_test_project_session(&state, Agent::Codex, &project, &root);
    {
        let mut inner = state.inner.lock().unwrap();
        let mut settings = real_fixture_engram_settings(&root);
        settings.authority_store_key = Some(EngramAuthorityStoreKey {
            database_path: normalize_user_facing_path(&fs::canonicalize(database).unwrap()),
            project_id: "fixture-ready".to_owned(),
        });
        inner
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .unwrap()
            .engram = Some(settings);
        let record = inner
            .sessions
            .iter_mut()
            .find(|r| r.session.id == session)
            .unwrap();
        record.session.status = SessionStatus::Error;
        record.runtime = SessionRuntime::None;
        record.engram.routing_token = Some("retained-token".to_owned());
        record.engram.active_grant_id = Some("retained-grant".to_owned());
        state.commit_locked(&mut inner).unwrap();
    }
    (state, project, session, root)
}

fn receipt(root: &FsPath, session: &str) -> Value {
    json!({
        "schema_version": 1, "scope": "control_session_inspect", "mutation_enabled": false,
        "project_id": "fixture-ready",
        "database": normalize_user_facing_path(&fs::canonicalize(work_database_path(root, "fixture-ready")).unwrap()),
        "session_id": session, "retained_grant_id": "retained-grant",
        "host_path_policy": {"stored":"case_sensitive", "resolved":"case_sensitive", "status":"matched"},
        "session_present": false, "session_grants_present": false, "retained_grant_present": false
    })
}

fn write_receipt(root: &FsPath, value: &Value) {
    fs::write(
        root.join("session-inspection.json"),
        serde_json::to_vec(value).unwrap(),
    )
    .unwrap();
}

fn target(state: &AppState, project: &str, session: &str) -> EngramBindingTarget {
    let mut inner = state.inner.lock().unwrap();
    let owner = inner.engram_project_resets.claim(project).unwrap();
    let record = inner
        .sessions
        .iter_mut()
        .find(|r| r.session.id == session)
        .unwrap();
    record.engram.project_reset_in_progress = true;
    assert!(record.engram.begin_checkpoint(Some(owner)));
    project_engram_binding_target_during_owned_reset_locked(&inner, session, project, owner)
        .unwrap()
        .unwrap()
}

fn next_settings(state: &AppState, project: &str) -> EngramProjectSettings {
    let inner = state.inner.lock().unwrap();
    let mut settings = inner.find_project(project).unwrap().engram.clone().unwrap();
    settings.turn_gated_control = false;
    settings
}

fn add_absent_session(state: &AppState, project: &str, root: &FsPath) -> String {
    let session = create_test_project_session(state, Agent::Codex, project, root);
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&session).unwrap();
    let record = &mut inner.sessions[index];
    record.session.status = SessionStatus::Error;
    record.runtime = SessionRuntime::None;
    record.engram.routing_token = Some("second-token".to_owned());
    record.engram.active_grant_id = Some("second-grant".to_owned());
    state.commit_locked(&mut inner).unwrap();
    session
}

fn write_target_receipts(root: &FsPath, sessions: &[(&str, &str)]) {
    for (session, grant) in sessions {
        let mut proof = receipt(root, session);
        proof["retained_grant_id"] = json!(grant);
        fs::write(
            root.join(format!("session-inspection-{session}.json")),
            proof.to_string(),
        )
        .unwrap();
    }
}

// Clean up thread-local hooks even if a fixture assertion unwinds.
struct AbsenceTestHooks;
impl Drop for AbsenceTestHooks {
    fn drop(&mut self) {
        TEST_ENGRAM_ABSENCE_NOW.with(|clock| clock.set(None));
        TEST_ENGRAM_ABSENCE_STORE_CHECK.with(|hook| hook.borrow_mut().take());
    }
}

#[test]
fn absence_selectors_reach_supported_scripts_literally() {
    let (state, project, session, root) = setup();
    let selector = " -grant & | ^ %NAME% \"quoted\" ; $value ";
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram.active_grant_id = Some(selector.to_owned());
        state.commit_locked(&mut inner).unwrap();
    }
    let target = target(&state, &project, &session);
    let mut proof = receipt(&root, &session);
    proof["retained_grant_id"] = json!(selector);
    write_receipt(&root, &proof);
    state
        .inspect_absent_engram_reset_session(&target, Instant::now() + ENGRAM_READINESS_TIMEOUT)
        .unwrap();
    let argv = fs::read_to_string(root.join("session-inspection-argv.txt")).unwrap();
    let expected = format!("--retained-grant-id={selector}");
    assert_eq!(
        argv.lines().filter(|line| *line == expected).count(),
        1,
        "{argv}"
    );
    assert_eq!(
        fs::read_to_string(root.join("session-inspection-targets.txt"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[cfg(windows)]
#[test]
fn absence_windows_batch_wrappers_are_refused_without_launch() {
    for extension in ["cmd", "BaT"] {
        let (state, project, session, root) = setup();
        let wrapper = root.join(format!("harmless.{extension}"));
        fs::write(&wrapper, "@echo launched>batch-was-launched\r\n").unwrap();
        {
            let mut inner = state.inner.lock().unwrap();
            inner
                .projects
                .iter_mut()
                .find(|p| p.id == project)
                .unwrap()
                .engram
                .as_mut()
                .unwrap()
                .binary_path = Some(wrapper.to_string_lossy().into_owned());
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].engram.active_grant_id = Some("grant&echo harmless".to_owned());
            state.commit_locked(&mut inner).unwrap();
        }
        let target = target(&state, &project, &session);
        let error = state
            .inspect_absent_engram_reset_session(&target, Instant::now() + ENGRAM_READINESS_TIMEOUT)
            .err()
            .unwrap();
        assert!(
            error
                .message
                .contains("does not support Windows .cmd/.bat wrappers"),
            "{}",
            error.message
        );
        assert!(!root.join("batch-was-launched").exists());
        assert!(!root.join("session-inspection-args.txt").exists());
        let inner = state.inner.lock().unwrap();
        assert_eq!(
            inner.sessions[inner.find_session_index(&session).unwrap()]
                .engram
                .active_grant_id
                .as_deref(),
            Some("grant&echo harmless")
        );
    }
}

#[test]
fn absence_best_effort_disable_and_retirement_never_probe() {
    for retire in [false, true] {
        let (state, project, session, root) = setup();
        if retire {
            let mut inner = state.inner.lock().unwrap();
            inner
                .projects
                .iter_mut()
                .find(|p| p.id == project)
                .unwrap()
                .engram
                .as_mut()
                .unwrap()
                .work_authority_grant = Some("old-work-authority".to_owned());
            state.commit_locked(&mut inner).unwrap();
        }
        write_receipt(&root, &receipt(&root, &session));
        let transport =
            ScriptedEngramControlTransport::new([remote_error_reply("control_session_not_bound")]);
        install_control_only_transport(&state, transport.clone());
        let mut next = next_settings(&state, &project);
        next.enabled = retire;
        next.work_authority_grant = None;
        state
            .update_project_engram_settings(&project, next)
            .unwrap();
        assert!(!root.join("session-inspection-args.txt").exists());
        assert_eq!(
            transport
                .requests()
                .iter()
                .filter(|r| r.request["operation"] == "turn_checkpoint")
                .count(),
            1
        );
        state.shutdown_persist_blocking();
        let persisted = load_state(state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        let record = persisted
            .sessions
            .iter()
            .find(|r| r.session.id == session)
            .unwrap();
        assert_eq!(
            record.engram.routing_token.as_deref(),
            Some("retained-token")
        );
        assert_eq!(
            record.engram.active_grant_id.as_deref(),
            Some("retained-grant")
        );
        assert!(record.engram.rebind_required);
        assert!(
            record
                .session
                .messages
                .iter()
                .any(|message| matches!(message,
            Message::EngramControl { card, .. } if card.stage == EngramControlStage::Checkpoint
                && card.decision == EngramControlCardDecision::Degraded
                && card.refusal_code.as_deref() == Some("control_session_not_bound")
                && card.repair_armed))
        );
        let saved = persisted
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap();
        assert_eq!(saved.enabled, retire);
        assert!(saved.work_authority_grant.is_none());
        let inner = state.inner.lock().unwrap();
        assert!(!inner.engram_project_resets.contains(&project));
    }
}

#[test]
fn absence_multi_target_save_is_atomic_on_success_refusal_and_budget_exhaustion() {
    for outcome in ["success", "checkpoint_refusal", "budget"] {
        let _hooks = AbsenceTestHooks;
        let (state, project, first, root) = setup();
        let second = add_absent_session(&state, &project, &root);
        write_target_receipts(
            &root,
            &[(&first, "retained-grant"), (&second, "second-grant")],
        );
        let transport = ScriptedEngramControlTransport::new([
            remote_error_reply("control_session_not_bound"),
            remote_error_reply(if outcome == "checkpoint_refusal" {
                "control_session_token_mismatch"
            } else {
                "control_session_not_bound"
            }),
        ]);
        install_control_only_transport(&state, transport.clone());
        if outcome == "budget" {
            // Advance only the admission clock, not wall time: first proof is
            // accepted at t=9; second preflight reaches t=11. Restarting a new
            // ten-second budget at the second target would incorrectly launch.
            let start = Instant::now();
            TEST_ENGRAM_ABSENCE_NOW.with(|clock| clock.set(Some(start)));
            let mut checks = 0;
            TEST_ENGRAM_ABSENCE_STORE_CHECK.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    checks += 1;
                    if checks == 2 || checks == 3 {
                        let elapsed = if checks == 2 { 9 } else { 11 };
                        TEST_ENGRAM_ABSENCE_NOW
                            .with(|clock| clock.set(Some(start + Duration::from_secs(elapsed))));
                    }
                }))
            });
        }
        let result =
            state.update_project_engram_settings(&project, next_settings(&state, &project));
        if outcome == "success" {
            result.unwrap();
        } else {
            let error = result.err().unwrap();
            assert_eq!(error.status, StatusCode::CONFLICT);
            assert!(
                error.message.contains(if outcome == "budget" {
                    "shared 10 second Save budget"
                } else {
                    "control_session_token_mismatch"
                }),
                "{}",
                error.message
            );
        }
        let requests = transport.requests();
        let checkpoints = requests
            .iter()
            .filter(|r| r.request["operation"] == "turn_checkpoint")
            .collect::<Vec<_>>();
        assert_eq!(
            checkpoints.len(),
            2,
            "every target must still be checkpointed"
        );
        let targets = fs::read_to_string(root.join("session-inspection-targets.txt")).unwrap();
        let inspected = targets.lines().collect::<Vec<_>>();
        assert_eq!(inspected.len(), if outcome == "success" { 2 } else { 1 });
        assert_eq!(inspected[0], checkpoints[0].connection.session_id);
        if outcome == "success" {
            assert_eq!(inspected[1], checkpoints[1].connection.session_id);
        }
        {
            let inner = state.inner.lock().unwrap();
            assert!(!inner.engram_project_resets.contains(&project));
            for session in [&first, &second] {
                let record = &inner.sessions[inner.find_session_index(session).unwrap()];
                assert!(!record.engram.checkpoint_in_progress);
                assert!(!record.engram.project_reset_in_progress);
            }
        }
        state.shutdown_persist_blocking();
        let persisted = load_state(state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        assert_eq!(
            persisted
                .find_project(&project)
                .unwrap()
                .engram
                .as_ref()
                .unwrap()
                .turn_gated_control,
            outcome != "success"
        );
        for (session, token, grant) in [
            (&first, "retained-token", "retained-grant"),
            (&second, "second-token", "second-grant"),
        ] {
            let record = persisted
                .sessions
                .iter()
                .find(|r| &r.session.id == session)
                .unwrap();
            assert_eq!(
                record.engram.active_grant_id.as_deref(),
                (outcome != "success").then_some(grant)
            );
            assert_eq!(
                record.engram.routing_token.as_deref(),
                (outcome != "success").then_some(token)
            );
        }
    }
}

#[test]
fn absence_declaration_change_inside_probe_refuses_and_retains_authority() {
    let (state, project, session, root) = setup();
    write_receipt(&root, &receipt(&root, &session));
    fs::write(
        root.join("session-inspection-rewrite-marker"),
        "another-project",
    )
    .unwrap();
    install_control_only_transport(
        &state,
        ScriptedEngramControlTransport::new([remote_error_reply("control_session_not_bound")]),
    );
    let error = state
        .update_project_engram_settings(&project, next_settings(&state, &project))
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error
            .message
            .contains("declaration changed during absence inspection"),
        "{}",
        error.message
    );
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(
        record.engram.routing_token.as_deref(),
        Some("retained-token")
    );
    assert_eq!(
        record.engram.active_grant_id.as_deref(),
        Some("retained-grant")
    );
    assert!(!inner.engram_project_resets.contains(&project));
}

#[test]
fn absence_receipt_requires_complete_exact_identity_and_no_authority() {
    let (state, project, session, root) = setup();
    let target = target(&state, &project, &session);
    let valid = receipt(&root, &session);
    let admit = |value: Value| -> Result<(), String> {
        let receipt = serde_json::from_value(value).map_err(|e| e.to_string())?;
        validate_engram_absence_receipt(&receipt, &target).map_err(|e| e.message)
    };
    admit(valid.clone()).unwrap();
    for (pointer, replacement) in [
        ("/schema_version", json!(2)),
        ("/scope", json!("readiness")),
        ("/mutation_enabled", json!(true)),
        ("/session_id", json!("another-session")),
        ("/retained_grant_id", json!("another-grant")),
        ("/project_id", json!("another-project")),
        ("/database", json!("relative.db")),
        ("/database", json!(root.join("wrong.db"))),
        ("/host_path_policy/status", json!("unbound")),
        ("/host_path_policy/stored", json!("")),
        ("/host_path_policy/resolved", json!("different")),
        ("/session_present", json!(true)),
        ("/session_grants_present", json!(true)),
        ("/retained_grant_present", json!(true)),
    ] {
        let mut invalid = valid.clone();
        *invalid.pointer_mut(pointer).unwrap() = replacement;
        assert!(admit(invalid).is_err(), "must refuse {pointer}");
    }
    for field in valid.as_object().unwrap().keys() {
        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(admit(missing).is_err(), "must require {field}");
    }
    let mut unbound_target = target.clone();
    unbound_target.settings.authority_store_key = None;
    assert!(
        validate_engram_absence_receipt(&serde_json::from_value(valid).unwrap(), &unbound_target)
            .is_err()
    );
}

#[test]
fn absence_inspection_nonzero_invalid_and_oversized_output_never_clear_state() {
    let (state, project, session, root) = setup();
    let target = target(&state, &project, &session);
    write_receipt(&root, &receipt(&root, &session));
    state
        .inspect_absent_engram_reset_session(&target, Instant::now() + ENGRAM_READINESS_TIMEOUT)
        .unwrap();
    let args = fs::read_to_string(root.join("session-inspection-args.txt")).unwrap();
    assert!(args.contains(&format!("--target-session-id={session}")));
    assert!(args.contains("--retained-grant-id=retained-grant"));
    assert!(args.contains("--json"));
    fs::write(root.join("session-inspection-exit"), "fail").unwrap();
    assert!(
        state
            .inspect_absent_engram_reset_session(&target, Instant::now() + ENGRAM_READINESS_TIMEOUT)
            .is_err()
    );
    fs::remove_file(root.join("session-inspection-exit")).unwrap();
    let mut padded = receipt(&root, &session);
    padded["padding"] = json!("x".repeat(ENGRAM_DIAGNOSTIC_REPORT_LIMIT));
    for (text, reason) in [
        ("invalid json".to_owned(), "invalid receipt"),
        (padded.to_string(), "receipt is oversized"),
    ] {
        fs::write(root.join("session-inspection.json"), text).unwrap();
        let error = state
            .inspect_absent_engram_reset_session(&target, Instant::now() + ENGRAM_READINESS_TIMEOUT)
            .err()
            .unwrap();
        assert!(error.message.contains(reason), "{}", error.message);
    }
    let inner = state.inner.lock().unwrap();
    let record = inner
        .sessions
        .iter()
        .find(|r| r.session.id == session)
        .unwrap();
    assert_eq!(
        record.engram.active_grant_id.as_deref(),
        Some("retained-grant")
    );
}

#[test]
fn absence_evidence_rejects_changed_generation_token_grant_status_and_store() {
    let (state, project, session, root) = setup();
    let target = target(&state, &project, &session);
    write_receipt(&root, &receipt(&root, &session));
    let evidence = state
        .inspect_absent_engram_reset_session(&target, Instant::now() + ENGRAM_READINESS_TIMEOUT)
        .unwrap();
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&session).unwrap();
    inner.sessions[index].engram.dispatch_generation += 1;
    assert!(evidence.validate_locked(&inner).is_err());
    inner.sessions[index].engram.dispatch_generation -= 1;
    inner.sessions[index].active_turn_generation += 1;
    assert!(evidence.validate_locked(&inner).is_err());
    inner.sessions[index].active_turn_generation -= 1;
    inner.sessions[index].engram.routing_token = Some("successor-token".into());
    assert!(evidence.validate_locked(&inner).is_err());
    inner.sessions[index].engram.routing_token = target.routing_token.clone();
    inner.sessions[index].engram.active_grant_id = Some("successor-grant".into());
    assert!(evidence.validate_locked(&inner).is_err());
    inner.sessions[index].engram.active_grant_id = target.active_grant_id.clone();
    inner.sessions[index].session.status = SessionStatus::Active;
    assert!(evidence.validate_locked(&inner).is_err());
    inner.sessions[index].session.status = SessionStatus::Error;
    evidence.validate_locked(&inner).unwrap();
    inner.sessions[index].engram.checkpoint_owner_generation = None;
    assert!(evidence.validate_locked(&inner).is_err());
    inner.sessions[index].engram.checkpoint_owner_generation =
        target.project_reset_owner_generation;
    let project_index = inner.projects.iter().position(|p| p.id == project).unwrap();
    let old_settings = inner.projects[project_index].engram.clone();
    inner.projects[project_index].engram.as_mut().unwrap().home = Some("different-home".into());
    assert!(evidence.validate_locked(&inner).is_err());
    inner.projects[project_index].engram = old_settings;
    drop(inner);
    fs::write(root.join(".engram-project"), "different-project").unwrap();
    assert!(evidence.validate_store_off_lock().is_err());
}

#[test]
fn absence_store_checks_leave_state_unlocked_and_reject_drift_before_commit() {
    // Pause at both post-probe and final pre-commit filesystem boundaries.
    // No sleeps: the worker cannot proceed until the test inspects/mutates state.
    for pause_at in [2, 3] {
        for drift in ["none", "generation", "store_key", "declaration"] {
            let (state, project, session, root) = setup();
            write_receipt(&root, &receipt(&root, &session));
            install_control_only_transport(
                &state,
                ScriptedEngramControlTransport::new([remote_error_reply(
                    "control_session_not_bound",
                )]),
            );
            let settings = next_settings(&state, &project);
            let worker_state = state.clone();
            let worker_project = project.clone();
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                let mut count = 0;
                let probe_state = worker_state.clone();
                TEST_ENGRAM_ABSENCE_STORE_CHECK.with(|hook| {
                    *hook.borrow_mut() = Some(Box::new(move || {
                        count += 1;
                        if count == pause_at {
                            entered_tx
                                .send(probe_state.inner.is_not_held_by_current_thread_for_test())
                                .unwrap();
                            release_rx.recv_timeout(DEADLOCK_GUARD).unwrap();
                        }
                    }));
                });
                let result = worker_state.update_project_engram_settings(&worker_project, settings);
                TEST_ENGRAM_ABSENCE_STORE_CHECK.with(|hook| hook.borrow_mut().take());
                result
            });
            let lock_available = entered_rx.recv_timeout(DEADLOCK_GUARD).unwrap();
            if lock_available {
                let mut inner = state.inner.lock().unwrap();
                if drift == "generation" {
                    let index = inner.find_session_index(&session).unwrap();
                    inner.sessions[index].engram.dispatch_generation += 1;
                } else if drift == "store_key" {
                    inner
                        .projects
                        .iter_mut()
                        .find(|p| p.id == project)
                        .unwrap()
                        .engram
                        .as_mut()
                        .unwrap()
                        .authority_store_key = None;
                }
            }
            if drift == "declaration" {
                fs::write(root.join(".engram-project"), "different-project").unwrap();
            }
            // Always release/join before asserting so a regression cannot leak
            // a waiting worker or poison the global state mutex.
            release_tx.send(()).unwrap();
            let result = worker.join().unwrap();
            assert!(
                lock_available,
                "filesystem check {pause_at} held the state lock"
            );
            if drift == "none" {
                result.unwrap();
            } else {
                assert!(result.is_err(), "must reject {drift} at check {pause_at}");
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                assert_eq!(
                    record.engram.routing_token.as_deref(),
                    Some("retained-token")
                );
                assert_eq!(
                    record.engram.active_grant_id.as_deref(),
                    Some("retained-grant")
                );
                assert!(
                    inner
                        .find_project(&project)
                        .unwrap()
                        .engram
                        .as_ref()
                        .unwrap()
                        .turn_gated_control
                );
                assert!(!inner.engram_project_resets.contains(&project));
            }
        }
    }
}

#[test]
fn absence_strict_save_reconciles_only_proven_absence_and_persists_settings() {
    for blocked_field in [
        None,
        Some("session_present"),
        Some("session_grants_present"),
        Some("retained_grant_present"),
    ] {
        let (state, project, session, root) = setup();
        let mut proof = receipt(&root, &session);
        if let Some(field) = blocked_field {
            proof[field] = json!(true);
        }
        write_receipt(&root, &proof);
        let transport =
            ScriptedEngramControlTransport::new([remote_error_reply("control_session_not_bound")]);
        install_control_only_transport(&state, transport.clone());
        let result =
            state.update_project_engram_settings(&project, next_settings(&state, &project));
        let inner = state.inner.lock().unwrap();
        let record = inner
            .sessions
            .iter()
            .find(|r| r.session.id == session)
            .unwrap();
        let settings = inner
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap();
        if blocked_field.is_none() {
            result.unwrap();
            assert!(!settings.turn_gated_control);
            assert!(record.engram.active_grant_id.is_none());
        } else {
            assert!(
                result
                    .err()
                    .unwrap()
                    .message
                    .contains("absence recovery refused")
            );
            assert!(settings.turn_gated_control);
            assert_eq!(
                record.engram.active_grant_id.as_deref(),
                Some("retained-grant")
            );
        }
        assert!(!inner.engram_project_resets.contains(&project));
        assert_eq!(
            transport
                .requests()
                .iter()
                .filter(|r| r.request["operation"] == "turn_checkpoint")
                .count(),
            1
        );
        assert!(
            !transport
                .requests()
                .iter()
                .any(|r| r.request["operation"] == "session_status")
        );
        drop(inner);
        state.shutdown_persist_blocking();
        let reloaded = load_state(state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        assert_eq!(
            reloaded
                .find_project(&project)
                .unwrap()
                .engram
                .as_ref()
                .unwrap()
                .turn_gated_control,
            blocked_field.is_some()
        );
        let persisted = reloaded
            .sessions
            .iter()
            .find(|r| r.session.id == session)
            .unwrap();
        assert_eq!(
            persisted.engram.active_grant_id.is_some(),
            blocked_field.is_some()
        );
    }
}

#[test]
fn absence_strict_save_failure_restores_retained_grant_for_fresh_proof() {
    let (mut state, project, session, root) = setup();
    write_receipt(&root, &receipt(&root, &session));
    install_control_only_transport(
        &state,
        ScriptedEngramControlTransport::new([remote_error_reply("control_session_not_bound")]),
    );
    let settings = next_settings(&state, &project);
    state.shutdown_persist_blocking();
    let failing_path = root.join("persistence-is-directory");
    fs::create_dir_all(&failing_path).unwrap();
    state.persistence_path = Arc::new(failing_path);
    assert!(
        state
            .update_project_engram_settings(&project, settings)
            .is_err()
    );
    let inner = state.inner.lock().unwrap();
    let record = inner
        .sessions
        .iter()
        .find(|r| r.session.id == session)
        .unwrap();
    assert_eq!(
        record.engram.active_grant_id.as_deref(),
        Some("retained-grant")
    );
    assert_eq!(
        record.engram.routing_token.as_deref(),
        Some("retained-token")
    );
    assert!(
        inner
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap()
            .turn_gated_control
    );
}

#[test]
fn absence_probe_never_follows_an_uncertain_or_different_checkpoint_failure() {
    for code in [
        "control_session_token_mismatch",
        "control_connection_superseded",
        "unknown",
    ] {
        let (state, project, _, root) = setup();
        install_control_only_transport(
            &state,
            ScriptedEngramControlTransport::new([remote_error_reply(code)]),
        );
        assert!(
            state
                .update_project_engram_settings(&project, next_settings(&state, &project))
                .is_err()
        );
        assert!(!root.join("session-inspection-args.txt").exists());
    }
    assert!(!engram_checkpoint_can_inspect_absence(
        &EngramTransportError::transport("control_session_not_bound")
    ));
}

#[test]
fn absence_strict_tier_upgrade_recovers_errored_reviewer_and_fresh_binds() {
    let (state, runtime_rx) = test_app_state_with_delegation_codex_runtime("absence-reviewer");
    let (state, project, parent, root) = setup_with_state(state);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&parent).unwrap();
        inner.sessions[index].session.status = SessionStatus::Idle;
        inner.sessions[index].engram = EngramSessionState::default();
    }
    let transport = ScriptedEngramControlTransport::new([
        bind_reply("parent-token"),
        bind_reply("child-token"),
        grant_reply("retained-grant"),
        begin_reply("retained-grant"),
        remote_error_reply("control_session_not_bound"),
        bind_reply("fresh-a"),
        bind_reply("fresh-b"),
    ]);
    install_control_only_transport(&state, transport.clone());
    let child = state
        .create_read_only_delegation(
            &parent,
            CreateDelegationRequest {
                prompt: "Review the fixture".to_owned(),
                title: Some("Errored reviewer".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .unwrap()
        .delegation
        .child_session_id;
    assert!(matches!(
        receive(&runtime_rx, "reviewer dispatched"),
        CodexRuntimeCommand::Prompt { .. }
    ));
    let next = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&child).unwrap();
        inner.sessions[index].session.status = SessionStatus::Error;
        inner.sessions[index].runtime = SessionRuntime::None;
        let settings = inner
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .unwrap()
            .engram
            .as_mut()
            .unwrap();
        let next = settings.clone();
        // Reproduce the retained private grant with the operator's current
        // saved tier false. Recovery must not require the old tier enabled.
        settings.turn_gated_control = false;
        state.commit_locked(&mut inner).unwrap();
        next
    };
    write_receipt(&root, &receipt(&root, &child));
    state
        .update_project_engram_settings(&project, next)
        .unwrap();
    let inner = state.inner.lock().unwrap();
    assert!(
        inner
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap()
            .turn_gated_control
    );
    let record = inner
        .sessions
        .iter()
        .find(|r| r.session.id == child)
        .unwrap();
    assert!(record.engram.active_grant_id.is_none());
    assert!(matches!(
        record.engram.routing_token.as_deref(),
        Some("fresh-a" | "fresh-b")
    ));
    assert_eq!(
        transport
            .requests()
            .iter()
            .filter(|r| r.request["operation"] == "turn_checkpoint")
            .count(),
        1
    );
}
