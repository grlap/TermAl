//! Runtime-loss isolation with real profile slots and attached session routes.
//! Passive historical data never causes a control recovery on process exit.

use super::*;

#[test]
fn two_profiles_runtime_exit_ends_only_its_own_sessions_without_control_recovery() {
    let state = test_app_state();
    state.select_test_scripted_engram_budget_clock();
    let root = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("two-profile-project");
    fs::create_dir_all(&root).unwrap();
    let project = create_test_project(&state, &root, "Two-profile exit");
    {
        let mut inner = state.inner.lock().unwrap();
        inner
            .projects
            .iter_mut()
            .find(|entry| entry.id == project)
            .unwrap()
            .engram = Some(EngramProjectSettings {
            enabled: true,
            execution_observability: false,
            ..EngramProjectSettings::default()
        });
    }
    let default_owner = phase_sync::ParkedProcess::spawn();
    let read_only_owner = phase_sync::ParkedProcess::spawn();
    let (default, _, _) =
        test_shared_codex_runtime_with_process("exit-default", default_owner.process.clone());
    let (read_only, _, _) =
        test_shared_codex_runtime_with_process("exit-read-only", read_only_owner.process.clone());
    *state
        .shared_codex_runtime_slot(SharedCodexProfile::Default)
        .lock()
        .unwrap() = Some(default.clone());
    *state
        .shared_codex_runtime_slot(SharedCodexProfile::ReadOnlySandbox)
        .lock()
        .unwrap() = Some(read_only.clone());
    let transport = ScriptedEngramControlTransport::new([]);
    state.install_control_test_transport(transport.clone());
    let mut sessions = Vec::new();
    for (profile, runtime) in [
        (SharedCodexProfile::Default, &default),
        (SharedCodexProfile::ReadOnlySandbox, &read_only),
    ] {
        for attached in [true, false] {
            let id = create_test_project_session(&state, Agent::Codex, &project, &root);
            {
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&id).unwrap();
                let record = &mut inner.sessions[index];
                record.codex_thread_profile = Some(profile);
                set_record_external_session_id(record, Some(format!("thread-{id}")));
                record.engram.historical_control.insert(
                    "profileExitHistory".into(),
                    json!({"routing_token":format!("before-{id}"),
                        "grant_id":"unsettled-original", "future":{"untouched":true}}),
                );
                if attached {
                    record.session.status = SessionStatus::Active;
                    record.runtime = SessionRuntime::Codex(
                        spawn_codex_runtime(
                            state.clone(),
                            id.clone(),
                            record.session.workdir.clone(),
                            profile,
                        )
                        .unwrap(),
                    );
                    runtime.sessions.lock().unwrap().insert(
                        id.clone(),
                        SharedCodexSessionState {
                            thread_id: record.external_session_id.clone(),
                            ..Default::default()
                        },
                    );
                    runtime
                        .thread_sessions
                        .lock()
                        .unwrap()
                        .insert(format!("thread-{id}"), id.clone());
                }
            }
            sessions.push((id, profile, attached));
        }
    }
    state
        .handle_shared_codex_runtime_exit("exit-read-only", None)
        .unwrap();
    let calls = transport.requests();
    assert!(
        calls.is_empty(),
        "an ordinary process exit performs no status read or control bind"
    );
    for (id, profile, attached) in &sessions {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(id).unwrap()];
        if *profile == SharedCodexProfile::ReadOnlySandbox {
            assert!(matches!(record.runtime, SessionRuntime::None));
            assert_eq!(
                record.session.status,
                if *attached {
                    SessionStatus::Error
                } else {
                    SessionStatus::Idle
                }
            );
        } else {
            assert_eq!(
                matches!(record.runtime, SessionRuntime::Codex(_)),
                *attached
            );
            assert!(!calls.iter().any(|call| call.connection.session_id == *id));
            if *attached {
                assert_eq!(record.session.status, SessionStatus::Active);
                assert!(default.sessions.lock().unwrap().contains_key(id));
                assert_eq!(
                    default
                        .thread_sessions
                        .lock()
                        .unwrap()
                        .get(&format!("thread-{id}")),
                    Some(id)
                );
            }
        }
        assert_eq!(
            record.engram.historical_control.get("profileExitHistory"),
            Some(&json!({"routing_token":format!("before-{id}"),
                "grant_id":"unsettled-original", "future":{"untouched":true}}))
        );
    }
    assert!(
        state
            .running_shared_codex_runtime(SharedCodexProfile::ReadOnlySandbox)
            .is_none()
    );
    assert_eq!(
        state
            .running_shared_codex_runtime(SharedCodexProfile::Default)
            .unwrap()
            .runtime_id,
        "exit-default"
    );
    phase_sync::process_exit(&read_only_owner.process, "exited read-only app-server");
    assert!(default_owner.process.try_wait().unwrap().is_none());
    state
        .handle_shared_codex_runtime_exit("exit-read-only", None)
        .unwrap();
    assert_eq!(
        transport.requests().len(),
        calls.len(),
        "duplicate exit must not create control work"
    );
}
