//! Remote (SSH-proxied) backend tests: settings validation, event bridge
//! retry, snapshot/delta sync, orchestrator mirroring/proxying, SSE
//! fallback resync, applied-revision tracking, error-body sanitization,
//! and review scope forwarding.
//!
//! Remote terminal stream forwarding tests live together below in the
//! second block extracted from the same region.
//!
//! Extracted from `tests.rs` so each domain lives in its own sibling
//! module under `tests/`. Three short `state_inner_*` tests covering
//! mutation-stamp / session_mut / record_removed_session helpers were
//! intentionally left in mod.rs because they are not remote-specific.

use super::*;

// Tests that persists remote settings.
#[test]
fn persists_remote_settings() {
    let state = test_app_state();

    let updated = state
        .update_app_settings(UpdateAppSettingsRequest {
            default_codex_reasoning_effort: None,
            default_claude_approval_mode: None,
            default_claude_effort: None,
            remotes: Some(vec![
                RemoteConfig::local(),
                RemoteConfig {
                    id: "ssh-lab".to_owned(),
                    name: "SSH Lab".to_owned(),
                    transport: RemoteTransport::Ssh,
                    enabled: true,
                    host: Some("example.com".to_owned()),
                    port: Some(2222),
                    user: Some("alice".to_owned()),
                },
            ]),
        })
        .unwrap();

    assert_eq!(updated.preferences.remotes.len(), 2);
    assert_eq!(updated.preferences.remotes[1].id, "ssh-lab");
    assert_eq!(
        updated.preferences.remotes[1].transport,
        RemoteTransport::Ssh
    );

    let reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    assert_eq!(
        reloaded_inner.preferences.remotes,
        updated.preferences.remotes
    );
}

// Tests that rejects remote settings with unsafe remote ID.
#[test]
fn rejects_remote_settings_with_unsafe_remote_id() {
    let state = test_app_state();

    let error = match state.update_app_settings(UpdateAppSettingsRequest {
        default_codex_reasoning_effort: None,
        default_claude_approval_mode: None,
        default_claude_effort: None,
        remotes: Some(vec![
            RemoteConfig::local(),
            RemoteConfig {
                id: "ssh/lab".to_owned(),
                name: "SSH Lab".to_owned(),
                transport: RemoteTransport::Ssh,
                enabled: true,
                host: Some("example.com".to_owned()),
                port: Some(22),
                user: Some("alice".to_owned()),
            },
        ]),
    }) {
        Ok(_) => panic!("unsafe remote id should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error.message,
        "remote id `ssh/lab` contains unsupported characters"
    );
}

// Tests that rejects remote settings with invalid SSH host.
#[test]
fn rejects_remote_settings_with_invalid_ssh_host() {
    let state = test_app_state();

    let error = match state.update_app_settings(UpdateAppSettingsRequest {
        default_codex_reasoning_effort: None,
        default_claude_approval_mode: None,
        default_claude_effort: None,
        remotes: Some(vec![
            RemoteConfig::local(),
            RemoteConfig {
                id: "ssh-lab".to_owned(),
                name: "SSH Lab".to_owned(),
                transport: RemoteTransport::Ssh,
                enabled: true,
                host: Some("-oProxyCommand=touch/tmp/pwned".to_owned()),
                port: Some(22),
                user: Some("alice".to_owned()),
            },
        ]),
    }) {
        Ok(_) => panic!("host injection should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, "remote `SSH Lab` has an invalid SSH host");
}

// Tests that rejects remote settings with invalid SSH user.
#[test]
fn rejects_remote_settings_with_invalid_ssh_user() {
    let state = test_app_state();

    let error = match state.update_app_settings(UpdateAppSettingsRequest {
        default_codex_reasoning_effort: None,
        default_claude_approval_mode: None,
        default_claude_effort: None,
        remotes: Some(vec![
            RemoteConfig::local(),
            RemoteConfig {
                id: "ssh-lab".to_owned(),
                name: "SSH Lab".to_owned(),
                transport: RemoteTransport::Ssh,
                enabled: true,
                host: Some("example.com".to_owned()),
                port: Some(22),
                user: Some("alice@example.com".to_owned()),
            },
        ]),
    }) {
        Ok(_) => panic!("invalid SSH user should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, "remote `SSH Lab` has an invalid SSH user");
}

// Tests that remote connection issue message hides transport details.
#[test]
fn remote_connection_issue_message_hides_transport_details() {
    assert_eq!(
        remote_connection_issue_message("SSH Lab"),
        "Could not connect to remote \"SSH Lab\" over SSH. Check the host, network, and SSH settings, then try again."
    );
}

// Tests that local SSH start issue message hides transport details.
#[test]
fn local_ssh_start_issue_message_hides_transport_details() {
    assert_eq!(
        local_ssh_start_issue_message("SSH Lab"),
        "Could not start the local SSH client for remote \"SSH Lab\". Verify OpenSSH is installed and available on PATH, then try again."
    );
}

// Tests that remote SSH command args insert double dash before target.
#[test]
fn remote_ssh_command_args_insert_double_dash_before_target() {
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(2222),
        user: Some("alice".to_owned()),
    };

    let args = remote_ssh_command_args(&remote, 47001, RemoteProcessMode::ManagedServer)
        .expect("SSH args should build");

    let separator_index = args
        .iter()
        .position(|arg| arg == "--")
        .expect("SSH args should include `--` before the target");
    assert_eq!(args[separator_index + 1], "alice@example.com");
    assert_eq!(&args[separator_index + 2..], ["termal", "server"]);
}

// Tests that removing remote stops event bridge worker and resets started guard.
#[test]
fn removing_remote_stops_event_bridge_worker_and_resets_started_guard() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let connection = Arc::new(RemoteConnection {
        config: Mutex::new(remote.clone()),
        forwarded_port: 47001,
        process: Mutex::new(None),
        event_bridge_started: AtomicBool::new(false),
        event_bridge_shutdown: AtomicBool::new(false),
        supports_inline_orchestrator_templates: Mutex::new(None),
    });
    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(remote.id.clone(), connection.clone());

    connection.start_event_bridge(state.remote_registry.client.client().clone(), state.clone());
    assert!(connection.event_bridge_started.load(Ordering::SeqCst));

    state.remote_registry.reconcile(&[RemoteConfig::local()]);

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        if !connection.event_bridge_started.load(Ordering::SeqCst) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "event bridge worker should stop after the remote is removed"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    assert!(
        !state
            .remote_registry
            .connections
            .lock()
            .expect("remote registry mutex poisoned")
            .contains_key(&remote.id)
    );

    connection.start_event_bridge(state.remote_registry.client.client().clone(), state.clone());
    assert!(connection.event_bridge_started.load(Ordering::SeqCst));

    connection.stop_event_bridge();

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        if !connection.event_bridge_started.load(Ordering::SeqCst) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "event bridge started guard should reset after shutdown"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

// Tests that event-bridge retry boundaries clear fallback resync tracking so a
// restarted remote can recover even if its revision counter drops.
#[test]
fn remote_event_bridge_retry_clears_fallback_resync_tracking() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let connection = Arc::new(RemoteConnection {
        config: Mutex::new(remote.clone()),
        forwarded_port: 47001,
        process: Mutex::new(None),
        event_bridge_started: AtomicBool::new(false),
        event_bridge_shutdown: AtomicBool::new(false),
        supports_inline_orchestrator_templates: Mutex::new(None),
    });
    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(remote.id.clone(), connection.clone());

    state.note_remote_sse_fallback_resync(&remote.id, 4);
    assert!(state.should_skip_remote_sse_fallback_resync(&remote.id, 4));

    connection.start_event_bridge(state.remote_registry.client.client().clone(), state.clone());

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        if !state.should_skip_remote_sse_fallback_resync(&remote.id, 4) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "event bridge retry should clear stale fallback tracking"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    connection.stop_event_bridge();

    let shutdown_deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        if !connection.event_bridge_started.load(Ordering::SeqCst) {
            break;
        }
        assert!(
            std::time::Instant::now() < shutdown_deadline,
            "event bridge worker should stop after shutdown"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that event-bridge retry boundaries also clear stale applied remote
// revisions so restarted remotes can resume syncing below the old watermark.
#[test]
fn remote_event_bridge_retry_clears_applied_revision_tracking() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let connection = Arc::new(RemoteConnection {
        config: Mutex::new(remote.clone()),
        forwarded_port: 47001,
        process: Mutex::new(None),
        event_bridge_started: AtomicBool::new(false),
        event_bridge_shutdown: AtomicBool::new(false),
        supports_inline_orchestrator_templates: Mutex::new(None),
    });
    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(remote.id.clone(), connection.clone());

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.note_remote_applied_revision(&remote.id, 4);
        assert!(inner.should_skip_remote_applied_revision(&remote.id, 4));
    }

    connection.start_event_bridge(state.remote_registry.client.client().clone(), state.clone());

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let still_skipping = {
            let inner = state.inner.lock().expect("state mutex poisoned");
            inner.should_skip_remote_applied_revision(&remote.id, 4)
        };
        if !still_skipping {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "event bridge retry should clear stale applied revision tracking"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    connection.stop_event_bridge();

    let shutdown_deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        if !connection.event_bridge_started.load(Ordering::SeqCst) {
            break;
        }
        assert!(
            std::time::Instant::now() < shutdown_deadline,
            "event bridge worker should stop after shutdown"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote snapshot sync localizes orchestrators and creates missing proxy sessions.
#[test]
fn remote_snapshot_sync_localizes_orchestrators_and_creates_missing_proxy_sessions() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    state
        .apply_remote_state_snapshot(
            &remote.id,
            sample_remote_orchestrator_state(
                "remote-project-1",
                "/remote/repo",
                1,
                OrchestratorInstanceStatus::Running,
            ),
        )
        .expect("remote snapshot should apply");

    let snapshot = state.snapshot();
    let orchestrator = snapshot
        .orchestrators
        .iter()
        .find(|instance| {
            instance.remote_orchestrator_id.as_deref() == Some("remote-orchestrator-1")
        })
        .cloned()
        .expect("remote orchestrator should be mirrored");
    assert_ne!(orchestrator.id, "remote-orchestrator-1");
    assert_eq!(orchestrator.remote_id.as_deref(), Some(remote.id.as_str()));
    assert_eq!(orchestrator.project_id, local_project_id);
    assert_eq!(
        orchestrator.template_snapshot.project_id.as_deref(),
        Some(local_project_id.as_str())
    );
    assert_eq!(orchestrator.session_instances.len(), 3);
    assert_eq!(orchestrator.pending_transitions.len(), 1);

    let localized_session_ids = orchestrator
        .session_instances
        .iter()
        .map(|session| session.session_id.clone())
        .collect::<HashSet<_>>();
    assert!(localized_session_ids.contains(&orchestrator.pending_transitions[0].source_session_id));
    assert!(
        localized_session_ids.contains(&orchestrator.pending_transitions[0].destination_session_id)
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    for remote_session_id in ["remote-session-1", "remote-session-2", "remote-session-3"] {
        let index = inner
            .find_remote_session_index(&remote.id, remote_session_id)
            .expect("remote mirrored session should exist");
        assert_eq!(
            inner.sessions[index].session.project_id.as_deref(),
            Some(local_project_id.as_str())
        );
        assert!(localized_session_ids.contains(&inner.sessions[index].session.id));
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Regression test for the latent empty-string fallback in
// `localize_remote_orchestrator_instance`. After `delete_project` clears a
// detached orchestrator's `project_id` to `""`, a subsequent remote re-sync
// against the (now unmapped) remote project must NOT revive `Some("")` via
// the `.or(existing_local_project_id)` fallback and must NOT persist `""`
// back into `template_snapshot.project_id`. Before the fix, the fallback
// accepted `Some("")` as a valid existing local project id and
// `sync_remote_orchestrators_inner` wrote it onto the template snapshot;
// after the fix, the detached state is filtered at capture time, the
// localization errors out, and the orchestrator instance is rolled back to
// its pre-sync (still-detached) state with its stale template snapshot
// preserved.
#[test]
fn delete_project_then_resync_does_not_revive_empty_string_project_id_on_orchestrator() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    // Seed the local state with a localized remote orchestrator via the
    // normal sync path so `find_remote_orchestrator_index` has an entry
    // to return on the second sync.
    state
        .apply_remote_state_snapshot(
            &remote.id,
            sample_remote_orchestrator_state(
                "remote-project-1",
                "/remote/repo",
                1,
                OrchestratorInstanceStatus::Running,
            ),
        )
        .expect("initial remote snapshot should apply");
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let initial = inner
            .orchestrator_instances
            .iter()
            .find(|instance| {
                instance.remote_orchestrator_id.as_deref() == Some("remote-orchestrator-1")
            })
            .expect("initial remote orchestrator should be mirrored");
        assert_eq!(initial.project_id, local_project_id);
        assert_eq!(
            initial.template_snapshot.project_id.as_deref(),
            Some(local_project_id.as_str())
        );
    }

    // Delete the local project. `delete_project` clears the orchestrator
    // instance's `project_id` to `""` but leaves its
    // `template_snapshot.project_id` pointing at the (now-stale) local id
    // and leaves the orchestrator in the live instances list. This is
    // exactly the "latent empty-string fallback" setup the bug report
    // describes.
    state
        .delete_project(&local_project_id)
        .expect("delete_project should succeed");
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let detached = inner
            .orchestrator_instances
            .iter()
            .find(|instance| {
                instance.remote_orchestrator_id.as_deref() == Some("remote-orchestrator-1")
            })
            .expect("detached orchestrator should remain visible");
        assert_eq!(detached.project_id, "");
        assert_eq!(
            detached.template_snapshot.project_id.as_deref(),
            Some(local_project_id.as_str()),
            "delete_project must not touch template_snapshot.project_id",
        );
    }

    // Apply the same remote snapshot again with a newer revision. The local
    // project mapping is gone, so `local_project_id_for_remote_project`
    // returns `None`. Before the fix, the `.or(existing_local_project_id)`
    // branch would then yield `Some("")` from the detached instance and
    // `sync_remote_orchestrators_inner` would write `""` into
    // `template_snapshot.project_id`. `sync_remote_state_inner` swallows
    // the localization error and rolls back `inner.orchestrator_instances`,
    // so the bug would be invisible to the caller but durable in state.
    state
        .apply_remote_state_snapshot(
            &remote.id,
            sample_remote_orchestrator_state(
                "remote-project-1",
                "/remote/repo",
                2,
                OrchestratorInstanceStatus::Running,
            ),
        )
        .expect("second remote snapshot should apply (errors are swallowed)");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let orchestrator = inner
        .orchestrator_instances
        .iter()
        .find(|instance| {
            instance.remote_orchestrator_id.as_deref() == Some("remote-orchestrator-1")
        })
        .expect("orchestrator should still exist after failed re-sync");

    // The orchestrator is still detached (delete_project set this).
    assert_eq!(orchestrator.project_id, "");
    // Regression guard: template_snapshot.project_id must NOT be revived as
    // Some(""). Before the fix this assertion would fail because
    // `sync_remote_orchestrators_inner` wrote the empty `local_project_id`
    // onto the template snapshot before the caller rolled back.
    assert_ne!(
        orchestrator.template_snapshot.project_id.as_deref(),
        Some(""),
        "template_snapshot.project_id must never be revived as an empty string"
    );
    // The rollback restores the stale but non-empty local id from the
    // first sync — proof that the second sync's bad write was discarded.
    assert_eq!(
        orchestrator.template_snapshot.project_id.as_deref(),
        Some(local_project_id.as_str()),
    );

    drop(inner);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that mirrored remote orchestrators never enqueue local pending prompts during resume.
#[test]
fn remote_mirrored_orchestrators_do_not_enqueue_local_pending_prompts_on_resume() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    state
        .apply_remote_state_snapshot(
            &remote.id,
            sample_remote_orchestrator_state(
                "remote-project-1",
                "/remote/repo",
                1,
                OrchestratorInstanceStatus::Running,
            ),
        )
        .expect("remote snapshot should apply");

    let destination_local_session_id = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-2")
            .expect("remote mirrored destination session should exist");
        inner.sessions[index].session.id.clone()
    };
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&destination_local_session_id)
            .expect("destination session should exist");
        inner.sessions[index].session.status = SessionStatus::Active;
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .resume_pending_orchestrator_transitions()
        .expect("mirrored remote orchestrators should be ignored during local resume");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let destination = inner
        .sessions
        .iter()
        .find(|record| record.session.id == destination_local_session_id)
        .expect("destination session should still exist");
    assert_eq!(destination.session.status, SessionStatus::Active);
    assert!(destination.queued_prompts.is_empty());
    assert!(destination.session.pending_prompts.is_empty());
    assert!(matches!(destination.runtime, SessionRuntime::None));
    let orchestrator = inner
        .orchestrator_instances
        .iter()
        .find(|instance| {
            instance.remote_id.as_deref() == Some(remote.id.as_str())
                && instance.remote_orchestrator_id.as_deref() == Some("remote-orchestrator-1")
        })
        .expect("mirrored remote orchestrator should still exist");
    assert_eq!(orchestrator.pending_transitions.len(), 1);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote mirrored orchestrators are skipped by local pending-transition dispatch.
#[test]
fn remote_mirrored_orchestrators_skip_pending_transition_dispatch() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-remote-orchestrator-next-action-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let project_id = create_test_project(&state, &project_root, "Remote Next Action");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();

    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let instance_index = inner
        .orchestrator_instances
        .iter()
        .position(|instance| instance.id == orchestrator.id)
        .expect("orchestrator instance should exist");
    inner.orchestrator_instances[instance_index].pending_transitions = vec![PendingTransition {
        id: "pending-local-remote-1".to_owned(),
        transition_id: "planner-to-builder".to_owned(),
        source_session_id: planner_session_id,
        destination_session_id: builder_session_id.clone(),
        completion_revision: 7,
        rendered_prompt: "Use this plan and implement it.".to_owned(),
        created_at: "2026-04-03 12:00:00".to_owned(),
    }];
    assert!(matches!(
        next_pending_transition_action(&inner, &HashSet::new()),
        Some(PendingTransitionAction::Deliver {
            destination_session_id,
            ..
        }) if destination_session_id == builder_session_id
    ));
    inner.orchestrator_instances[instance_index].remote_id = Some("ssh-lab".to_owned());
    inner.orchestrator_instances[instance_index].remote_orchestrator_id =
        Some("remote-orchestrator-1".to_owned());
    assert!(
        next_pending_transition_action(&inner, &HashSet::new()).is_none(),
        "remote mirrored orchestrators should not enqueue local pending actions"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_file(state.orchestrator_templates_path.as_path());
    let _ = fs::remove_dir_all(project_root);
}

// Tests that remote mirrored orchestrators are skipped by deadlock detection.
#[test]
fn remote_mirrored_orchestrators_skip_deadlock_detection() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-remote-orchestrator-deadlock-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let project_id = create_test_project(&state, &project_root, "Remote Deadlock");
    let template = state
        .create_orchestrator_template(sample_deadlocked_orchestrator_template_draft())
        .expect("deadlock template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let source_a_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "source-a")
        .expect("source-a session should be mapped")
        .session_id
        .clone();
    let source_b_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "source-b")
        .expect("source-b session should be mapped")
        .session_id
        .clone();
    let consolidate_a_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "consolidate-a")
        .expect("consolidate-a session should be mapped")
        .session_id
        .clone();
    let consolidate_b_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "consolidate-b")
        .expect("consolidate-b session should be mapped")
        .session_id
        .clone();

    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let instance_index = inner
        .orchestrator_instances
        .iter()
        .position(|instance| instance.id == orchestrator.id)
        .expect("orchestrator instance should exist");
    {
        let instance = &mut inner.orchestrator_instances[instance_index];
        instance.remote_id = Some("ssh-lab".to_owned());
        instance.remote_orchestrator_id = Some("remote-orchestrator-1".to_owned());
        instance.pending_transitions = vec![
            PendingTransition {
                id: "pending-consolidate-a".to_owned(),
                transition_id: "source-a-to-consolidate-a".to_owned(),
                source_session_id: source_a_session_id,
                destination_session_id: consolidate_a_session_id.clone(),
                completion_revision: 3,
                rendered_prompt: "Source A input.".to_owned(),
                created_at: "2026-04-03 12:05:00".to_owned(),
            },
            PendingTransition {
                id: "pending-consolidate-b".to_owned(),
                transition_id: "source-b-to-consolidate-b".to_owned(),
                source_session_id: source_b_session_id,
                destination_session_id: consolidate_b_session_id.clone(),
                completion_revision: 4,
                rendered_prompt: "Source B input.".to_owned(),
                created_at: "2026-04-03 12:06:00".to_owned(),
            },
        ];
    }

    let deadlocked_session_ids = detect_deadlocked_consolidate_session_ids(
        &inner,
        &inner.orchestrator_instances[instance_index],
    );
    assert_eq!(
        deadlocked_session_ids.into_iter().collect::<HashSet<_>>(),
        HashSet::from([
            consolidate_a_session_id.clone(),
            consolidate_b_session_id.clone(),
        ])
    );
    assert!(
        !mark_deadlocked_orchestrator_instances(&mut inner, &HashSet::new()),
        "remote mirrored orchestrators should not be marked as deadlocked"
    );
    let instance = &inner.orchestrator_instances[instance_index];
    assert_eq!(instance.status, OrchestratorInstanceStatus::Running);
    assert!(instance.error_message.is_none());
    assert_eq!(instance.pending_transitions.len(), 2);
    for session_id in [&consolidate_a_session_id, &consolidate_b_session_id] {
        let index = inner
            .find_session_index(session_id)
            .expect("consolidate session should exist");
        assert_eq!(inner.sessions[index].session.status, SessionStatus::Idle);
    }
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_file(state.orchestrator_templates_path.as_path());
    let _ = fs::remove_dir_all(project_root);
}

// Tests that remote OrchestratorsUpdated deltas localize ids and preserve proxy identity.
#[test]
fn remote_orchestrators_updated_delta_localizes_ids_and_preserves_proxy_identity() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    state
        .apply_remote_state_snapshot(
            &remote.id,
            sample_remote_orchestrator_state(
                "remote-project-1",
                "/remote/repo",
                1,
                OrchestratorInstanceStatus::Running,
            ),
        )
        .expect("initial remote snapshot should apply");

    let initial_snapshot = state.snapshot();
    let local_orchestrator_id = initial_snapshot
        .orchestrators
        .iter()
        .find(|instance| {
            instance.remote_orchestrator_id.as_deref() == Some("remote-orchestrator-1")
        })
        .expect("remote orchestrator should be mirrored")
        .id
        .clone();
    let mut delta_receiver = state.subscribe_delta_events();

    let remote_delta_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Paused,
    );
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::OrchestratorsUpdated {
                revision: 2,
                orchestrators: remote_delta_state.orchestrators.clone(),
                sessions: remote_delta_state.sessions.clone(),
            },
        )
        .expect("remote orchestrator delta should apply");

    let snapshot = state.snapshot();
    let orchestrator = snapshot
        .orchestrators
        .iter()
        .find(|instance| {
            instance.remote_orchestrator_id.as_deref() == Some("remote-orchestrator-1")
        })
        .expect("remote orchestrator should remain mirrored");
    assert_eq!(orchestrator.id, local_orchestrator_id);
    assert_eq!(orchestrator.status, OrchestratorInstanceStatus::Paused);
    assert_eq!(orchestrator.project_id, local_project_id);
    let expected_local_session_ids = orchestrator
        .session_instances
        .iter()
        .map(|instance| instance.session_id.clone())
        .collect::<HashSet<_>>();

    let delta_payload = delta_receiver
        .try_recv()
        .expect("localized orchestrator delta should be published");
    let delta: DeltaEvent =
        serde_json::from_str(&delta_payload).expect("delta payload should decode");
    match delta {
        DeltaEvent::OrchestratorsUpdated {
            revision,
            orchestrators,
            sessions,
        } => {
            assert_eq!(revision, snapshot.revision);
            let localized = orchestrators
                .iter()
                .find(|instance| {
                    instance.remote_orchestrator_id.as_deref() == Some("remote-orchestrator-1")
                })
                .expect("localized delta should contain the mirrored orchestrator");
            assert_eq!(localized.id, local_orchestrator_id);
            assert_eq!(localized.status, OrchestratorInstanceStatus::Paused);
            assert_eq!(
                sessions
                    .iter()
                    .map(|session| session.id.clone())
                    .collect::<HashSet<_>>(),
                expected_local_session_ids
            );
            assert!(sessions.iter().all(|session| {
                session.project_id.as_deref() == Some(local_project_id.as_str())
            }));
        }
        _ => panic!("unexpected delta variant"),
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote OrchestratorsUpdated deltas can create missing proxy sessions from their payload.
#[test]
fn remote_orchestrators_updated_delta_creates_missing_proxy_sessions_from_payload_sessions() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let remote_delta_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    );
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::OrchestratorsUpdated {
                revision: 1,
                orchestrators: remote_delta_state.orchestrators.clone(),
                sessions: remote_delta_state.sessions.clone(),
            },
        )
        .expect("remote orchestrator delta should create missing proxy sessions");

    let snapshot = state.snapshot();
    let orchestrator = snapshot
        .orchestrators
        .iter()
        .find(|instance| {
            instance.remote_id.as_deref() == Some(remote.id.as_str())
                && instance.remote_orchestrator_id.as_deref() == Some("remote-orchestrator-1")
        })
        .cloned()
        .expect("remote orchestrator should be mirrored from the delta payload");
    assert_eq!(orchestrator.project_id, local_project_id);
    assert_eq!(orchestrator.session_instances.len(), 3);

    let localized_session_ids = orchestrator
        .session_instances
        .iter()
        .map(|session| session.session_id.clone())
        .collect::<HashSet<_>>();
    let inner = state.inner.lock().expect("state mutex poisoned");
    for remote_session_id in ["remote-session-1", "remote-session-2", "remote-session-3"] {
        let index = inner
            .find_remote_session_index(&remote.id, remote_session_id)
            .expect("remote mirrored session should exist after delta localization");
        assert_eq!(
            inner.sessions[index].session.project_id.as_deref(),
            Some(local_project_id.as_str())
        );
        assert!(localized_session_ids.contains(&inner.sessions[index].session.id));
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that stale remote snapshots cannot overwrite newer orchestrator bridge deltas.
#[test]
fn stale_remote_snapshot_does_not_overwrite_newer_orchestrator_delta_state() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let remote_delta_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Paused,
    );
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::OrchestratorsUpdated {
                revision: 2,
                orchestrators: remote_delta_state.orchestrators.clone(),
                sessions: remote_delta_state.sessions.clone(),
            },
        )
        .expect("remote orchestrator delta should apply");
    let revision_after_delta = state.snapshot().revision;

    state
        .apply_remote_state_snapshot(
            &remote.id,
            sample_remote_orchestrator_state(
                "remote-project-1",
                "/remote/repo",
                1,
                OrchestratorInstanceStatus::Running,
            ),
        )
        .expect("stale remote snapshot should be ignored");

    let snapshot = state.snapshot();
    assert_eq!(snapshot.revision, revision_after_delta);
    let orchestrator = snapshot
        .orchestrators
        .iter()
        .find(|instance| {
            instance.remote_id.as_deref() == Some(remote.id.as_str())
                && instance.remote_orchestrator_id.as_deref() == Some("remote-orchestrator-1")
        })
        .expect("remote orchestrator should remain mirrored");
    assert_eq!(orchestrator.project_id, local_project_id);
    assert_eq!(orchestrator.status, OrchestratorInstanceStatus::Paused);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that failed remote OrchestratorsUpdated deltas roll back eager proxy-session localization.
#[test]
fn remote_orchestrators_updated_delta_rolls_back_proxy_sessions_when_localization_fails() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let mut delta_receiver = state.subscribe_delta_events();
    let (initial_session_count, initial_next_session_number) = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        (inner.sessions.len(), inner.next_session_number)
    };

    let mut invalid_delta_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    );
    invalid_delta_state
        .sessions
        .retain(|session| session.id != "remote-session-3");

    let error = state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::OrchestratorsUpdated {
                revision: 1,
                orchestrators: invalid_delta_state.orchestrators.clone(),
                sessions: invalid_delta_state.sessions.clone(),
            },
        )
        .expect_err("invalid remote orchestrator delta should fail localization");
    assert!(
        error
            .to_string()
            .contains("remote session `remote-session-3` not found"),
        "unexpected error: {error:#}"
    );
    assert!(
        delta_receiver.try_recv().is_err(),
        "failed remote delta should not publish a localized update"
    );

    let snapshot = state.snapshot();
    assert!(
        !snapshot
            .orchestrators
            .iter()
            .any(|instance| instance.remote_id.as_deref() == Some(remote.id.as_str()))
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.sessions.len(), initial_session_count);
    assert_eq!(inner.next_session_number, initial_next_session_number);
    assert!(
        inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .is_none()
    );
    assert!(
        inner
            .find_remote_session_index(&remote.id, "remote-session-2")
            .is_none()
    );
    assert!(
        inner
            .find_remote_session_index(&remote.id, "remote-session-3")
            .is_none()
    );
    drop(inner);

    let persisted: Value = serde_json::from_slice(
        &fs::read(state.persistence_path.as_path()).expect("persisted state file should exist"),
    )
    .expect("persisted state should deserialize");
    let persisted_sessions = persisted["sessions"]
        .as_array()
        .expect("persisted sessions should be present");
    assert_eq!(persisted_sessions.len(), initial_session_count);
    assert!(!persisted_sessions.iter().any(|candidate| {
        candidate["remoteSessionId"] == Value::String("remote-session-1".to_owned())
            || candidate["remoteSessionId"] == Value::String("remote-session-2".to_owned())
    }));
    let persisted_orchestrator_instances = persisted["orchestratorInstances"].as_array();
    assert!(persisted_orchestrator_instances.map_or(true, |instances| {
        !instances
            .iter()
            .any(|instance| instance["remoteId"] == Value::String(remote.id.clone()))
    }));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote deltas sharing a revision still apply sequentially.
#[test]
fn remote_same_revision_deltas_apply_in_sequence() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");

    let local_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let local_session_id = upsert_remote_proxy_session_record(
            &mut inner,
            &remote.id,
            &remote_session,
            Some(local_project_id),
        );
        state
            .commit_locked(&mut inner)
            .expect("remote proxy session should persist");
        local_session_id
    };
    let mut delta_receiver = state.subscribe_delta_events();

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageCreated {
                revision: 2,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-1".to_owned(),
                message_index: 0,
                message: Message::Text {
                    attachments: Vec::new(),
                    id: "message-1".to_owned(),
                    timestamp: "2026-04-05 10:00:00".to_owned(),
                    author: Author::Assistant,
                    text: "First remote message.".to_owned(),
                    expanded_text: None,
                },
                preview: "First remote message.".to_owned(),
                status: SessionStatus::Active,
            },
        )
        .expect("first same-revision delta should apply");
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::CommandUpdate {
                revision: 2,
                session_id: "remote-session-1".to_owned(),
                message_id: "command-1".to_owned(),
                message_index: 1,
                command: "echo ok".to_owned(),
                command_language: Some("bash".to_owned()),
                output: "ok".to_owned(),
                output_language: Some("text".to_owned()),
                status: CommandStatus::Success,
                preview: "echo ok".to_owned(),
            },
        )
        .expect("second same-revision delta should apply");

    let first_delta: DeltaEvent = serde_json::from_str(
        &delta_receiver
            .try_recv()
            .expect("first same-revision delta should publish"),
    )
    .expect("first delta payload should decode");
    match first_delta {
        DeltaEvent::MessageCreated { message_id, .. } => assert_eq!(message_id, "message-1"),
        _ => panic!("unexpected first delta variant"),
    }

    let second_delta: DeltaEvent = serde_json::from_str(
        &delta_receiver
            .try_recv()
            .expect("second same-revision delta should publish"),
    )
    .expect("second delta payload should decode");
    // A missing remote command message is localized by creating the message
    // first, so the published delta is normalized to MessageCreated.
    match second_delta {
        DeltaEvent::MessageCreated { message_id, .. } => assert_eq!(message_id, "command-1"),
        _ => panic!("unexpected second delta variant"),
    }

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    assert_eq!(session.preview, "echo ok");
    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(session.messages.len(), 2);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "First remote message."
    ));
    assert!(matches!(
        session.messages.get(1),
        Some(Message::Command {
            command,
            output,
            status: CommandStatus::Success,
            ..
        }) if command == "echo ok" && output == "ok"
    ));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote project orchestrator creation proxies to the remote backend and localizes the result.
#[test]
fn create_orchestrator_instance_proxies_remote_projects_and_localizes_response() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(local_project_id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;

    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    remote_state.orchestrators[0].id = "remote-orchestrator-created".to_owned();
    remote_state.orchestrators[0].template_id = template.id.clone();
    remote_state.orchestrators[0].template_snapshot = OrchestratorTemplate {
        id: template.id.clone(),
        name: template.name.clone(),
        description: template.description.clone(),
        project_id: Some("remote-project-1".to_owned()),
        sessions: template.sessions.clone(),
        transitions: template.transitions.clone(),
        created_at: template.created_at.clone(),
        updated_at: template.updated_at.clone(),
    };
    let remote_orchestrator = remote_state.orchestrators[0].clone();
    let remote_response = serde_json::to_string(&CreateOrchestratorInstanceResponse {
        orchestrator: remote_orchestrator,
        state: remote_state,
    })
    .expect("remote orchestrator response should encode");

    let captured = Arc::new(Mutex::new(None::<(String, String)>));
    let captured_for_server = captured.clone();
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = requests.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "test listener");
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                let bytes_read = stream.read(&mut chunk).expect("request should read");
                assert!(bytes_read > 0, "request closed before headers completed");
                buffer.extend_from_slice(&chunk[..bytes_read]);
                if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end;
                }
            };
            let headers = String::from_utf8_lossy(&buffer[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then_some(value.trim())
                        .and_then(|value| value.parse::<usize>().ok())
                })
                .unwrap_or(0);
            let body_start = header_end + 4;
            while buffer.len() < body_start + content_length {
                let bytes_read = stream.read(&mut chunk).expect("request body should read");
                if bytes_read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..bytes_read]);
            }

            let request_head = String::from_utf8_lossy(&buffer[..body_start]).to_string();
            let request_line = request_head
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();
            requests_for_server
                .lock()
                .expect("capture mutex poisoned")
                .push(request_line.clone());
            let body = String::from_utf8_lossy(&buffer[body_start..body_start + content_length])
                .to_string();

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 11\r\n\r\n{\"ok\":true}",
                    )
                    .expect("health response should write");
                continue;
            }

            if request_line.starts_with("POST /api/orchestrators ") {
                *captured_for_server.lock().expect("capture mutex poisoned") =
                    Some((request_line.clone(), body));
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            remote_response.len(),
                            remote_response
                        )
                        .as_bytes(),
                    )
                    .expect("orchestrator response should write");
                continue;
            }

            panic!("unexpected request: {request_line}");
        }
    });

    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(true),
                event_bridge_shutdown: AtomicBool::new(false),
                supports_inline_orchestrator_templates: Mutex::new(None),
            }),
        );

    let response = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id.clone(),
            project_id: Some(local_project_id.clone()),
            template: None,
        })
        .expect("remote orchestrator should be created");

    assert_ne!(response.orchestrator.id, "remote-orchestrator-created");
    assert_eq!(
        response.orchestrator.remote_id.as_deref(),
        Some(remote.id.as_str())
    );
    assert_eq!(
        response.orchestrator.remote_orchestrator_id.as_deref(),
        Some("remote-orchestrator-created")
    );
    assert_eq!(response.orchestrator.project_id, local_project_id);
    assert_eq!(response.orchestrator.template_id, template.id);
    assert_eq!(
        response
            .orchestrator
            .template_snapshot
            .project_id
            .as_deref(),
        Some(response.orchestrator.project_id.as_str())
    );
    assert_eq!(
        response.orchestrator.session_instances.len(),
        template.sessions.len()
    );
    assert!(
        response
            .state
            .orchestrators
            .iter()
            .any(|instance| instance.id == response.orchestrator.id)
    );

    let (request_line, body) = captured
        .lock()
        .expect("capture mutex poisoned")
        .clone()
        .expect("captured request should exist");
    assert!(request_line.starts_with("POST /api/orchestrators "));
    let parsed_body: Value = serde_json::from_str(&body).expect("request body should decode");
    assert_eq!(
        parsed_body["templateId"],
        Value::String(template.id.clone())
    );
    assert_eq!(
        parsed_body["projectId"],
        Value::String("remote-project-1".to_owned())
    );
    assert_eq!(
        parsed_body["template"]["projectId"],
        Value::String("remote-project-1".to_owned())
    );
    assert_eq!(
        parsed_body["template"]["name"],
        Value::String(template.name)
    );

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that direct remote orchestrator proxy creation localizes the launch and notes the applied revision.
#[test]
fn create_remote_orchestrator_proxy_localizes_launch_and_notes_revision() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(local_project_id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;
    let project = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .find_project(&local_project_id)
            .cloned()
            .expect("remote project should exist")
    };

    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    remote_state.orchestrators[0].id = "remote-orchestrator-created".to_owned();
    remote_state.orchestrators[0].template_id = template.id.clone();
    remote_state.orchestrators[0].template_snapshot = OrchestratorTemplate {
        id: template.id.clone(),
        name: template.name.clone(),
        description: template.description.clone(),
        project_id: Some("remote-project-1".to_owned()),
        sessions: template.sessions.clone(),
        transitions: template.transitions.clone(),
        created_at: template.created_at.clone(),
        updated_at: template.updated_at.clone(),
    };
    let remote_response = serde_json::to_string(&CreateOrchestratorInstanceResponse {
        orchestrator: remote_state.orchestrators[0].clone(),
        state: remote_state,
    })
    .expect("remote orchestrator response should encode");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "test listener");
            let mut buffer = [0u8; 4096];
            let bytes_read = stream.read(&mut buffer).expect("request should read");
            assert!(bytes_read > 0, "request should contain data");
            let request = String::from_utf8_lossy(&buffer[..bytes_read]);
            let request_line = request
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 11\r\n\r\n{\"ok\":true}",
                    )
                    .expect("health response should write");
                continue;
            }

            if request_line.starts_with("POST /api/orchestrators ") {
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            remote_response.len(),
                            remote_response
                        )
                        .as_bytes(),
                    )
                    .expect("orchestrator response should write");
                continue;
            }

            panic!("unexpected request: {request_line}");
        }
    });

    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(true),
                event_bridge_shutdown: AtomicBool::new(false),
                supports_inline_orchestrator_templates: Mutex::new(None),
            }),
        );

    let response = state
        .create_remote_orchestrator_proxy(&template, &project)
        .expect("remote orchestrator should be localized");

    assert_eq!(
        response.orchestrator.remote_id.as_deref(),
        Some(remote.id.as_str())
    );
    assert_eq!(
        response.orchestrator.remote_orchestrator_id.as_deref(),
        Some("remote-orchestrator-created")
    );
    assert_eq!(response.orchestrator.project_id, local_project_id);
    assert!(
        response
            .state
            .orchestrators
            .iter()
            .any(|instance| instance.id == response.orchestrator.id)
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .find_remote_orchestrator_index(&remote.id, "remote-orchestrator-created")
            .is_some()
    );
    assert!(inner.should_skip_remote_applied_revision(&remote.id, 2));
    assert!(!inner.should_skip_remote_applied_revision(&remote.id, 3));
    drop(inner);

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that direct remote orchestrator proxy creation rolls back mirrored sessions and orchestrators when localization fails.
#[test]
fn create_remote_orchestrator_proxy_rolls_back_on_localization_failure() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(local_project_id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;
    let project = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .find_project(&local_project_id)
            .cloned()
            .expect("remote project should exist")
    };
    let persisted_before = fs::read(state.persistence_path.as_path())
        .expect("initial state should already be persisted");
    let initial_next_session_number = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner.next_session_number
    };

    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    remote_state.orchestrators[0].id = "remote-orchestrator-broken".to_owned();
    remote_state.orchestrators[0].template_id = template.id.clone();
    remote_state.orchestrators[0].template_snapshot = OrchestratorTemplate {
        id: template.id.clone(),
        name: template.name.clone(),
        description: template.description.clone(),
        project_id: Some("remote-project-1".to_owned()),
        sessions: template.sessions.clone(),
        transitions: template.transitions.clone(),
        created_at: template.created_at.clone(),
        updated_at: template.updated_at.clone(),
    };
    remote_state
        .sessions
        .retain(|session| session.id != "remote-session-1");
    let remote_response = serde_json::to_string(&CreateOrchestratorInstanceResponse {
        orchestrator: remote_state.orchestrators[0].clone(),
        state: remote_state,
    })
    .expect("remote orchestrator response should encode");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "test listener");
            let mut buffer = [0u8; 4096];
            let bytes_read = stream.read(&mut buffer).expect("request should read");
            assert!(bytes_read > 0, "request should contain data");
            let request = String::from_utf8_lossy(&buffer[..bytes_read]);
            let request_line = request
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 11\r\n\r\n{\"ok\":true}",
                    )
                    .expect("health response should write");
                continue;
            }

            if request_line.starts_with("POST /api/orchestrators ") {
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            remote_response.len(),
                            remote_response
                        )
                        .as_bytes(),
                    )
                    .expect("orchestrator response should write");
                continue;
            }

            panic!("unexpected request: {request_line}");
        }
    });

    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(true),
                event_bridge_shutdown: AtomicBool::new(false),
                supports_inline_orchestrator_templates: Mutex::new(None),
            }),
        );

    let err = match state.create_remote_orchestrator_proxy(&template, &project) {
        Ok(_) => panic!("invalid remote orchestrator should fail localization"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::BAD_GATEWAY);
    assert!(
        err.message
            .contains("remote orchestrator could not be localized")
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.next_session_number, initial_next_session_number);
    assert!(
        inner
            .find_remote_orchestrator_index(&remote.id, "remote-orchestrator-broken")
            .is_none()
    );
    assert!(
        inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .is_none()
    );
    assert!(
        inner
            .find_remote_session_index(&remote.id, "remote-session-2")
            .is_none()
    );
    assert!(!inner.should_skip_remote_applied_revision(&remote.id, 2));
    drop(inner);

    let persisted_after = fs::read(state.persistence_path.as_path())
        .expect("rolled back state should stay persisted");
    assert_eq!(persisted_after, persisted_before);

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that stale create responses still materialize the launched remote
// orchestrator when a newer unrelated revision has already been applied.
#[test]
fn create_orchestrator_instance_materializes_stale_remote_launch_response() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(local_project_id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;

    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    remote_state.orchestrators[0].id = "remote-orchestrator-created".to_owned();
    remote_state.orchestrators[0].template_id = template.id.clone();
    remote_state.orchestrators[0].template_snapshot = OrchestratorTemplate {
        id: template.id.clone(),
        name: template.name.clone(),
        description: template.description.clone(),
        project_id: Some("remote-project-1".to_owned()),
        sessions: template.sessions.clone(),
        transitions: template.transitions.clone(),
        created_at: template.created_at.clone(),
        updated_at: template.updated_at.clone(),
    };
    let remote_response = serde_json::to_string(&CreateOrchestratorInstanceResponse {
        orchestrator: remote_state.orchestrators[0].clone(),
        state: remote_state,
    })
    .expect("remote orchestrator response should encode");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "test listener");
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                let bytes_read = stream.read(&mut chunk).expect("request should read");
                assert!(bytes_read > 0, "request closed before headers completed");
                buffer.extend_from_slice(&chunk[..bytes_read]);
                if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end;
                }
            };
            let headers = String::from_utf8_lossy(&buffer[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then_some(value.trim())
                        .and_then(|value| value.parse::<usize>().ok())
                })
                .unwrap_or(0);
            let body_start = header_end + 4;
            while buffer.len() < body_start + content_length {
                let bytes_read = stream.read(&mut chunk).expect("request body should read");
                if bytes_read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..bytes_read]);
            }

            let request_head = String::from_utf8_lossy(&buffer[..body_start]).to_string();
            let request_line = request_head
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 11\r\n\r\n{\"ok\":true}",
                    )
                    .expect("health response should write");
                continue;
            }

            if request_line.starts_with("POST /api/orchestrators ") {
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            remote_response.len(),
                            remote_response
                        )
                        .as_bytes(),
                    )
                    .expect("orchestrator response should write");
                continue;
            }

            panic!("unexpected request: {request_line}");
        }
    });

    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(true),
                event_bridge_shutdown: AtomicBool::new(false),
                supports_inline_orchestrator_templates: Mutex::new(None),
            }),
        );
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.note_remote_applied_revision(&remote.id, 3);
    }

    let response = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id.clone(),
            project_id: Some(local_project_id.clone()),
            template: None,
        })
        .expect("stale launch response should still materialize the orchestrator");

    assert_eq!(
        response.orchestrator.remote_orchestrator_id.as_deref(),
        Some("remote-orchestrator-created")
    );
    assert_eq!(response.orchestrator.project_id, local_project_id);
    assert!(
        response
            .state
            .orchestrators
            .iter()
            .any(|instance| instance.id == response.orchestrator.id)
    );
    assert_eq!(
        response.orchestrator.session_instances.len(),
        template.sessions.len()
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .find_remote_orchestrator_index(&remote.id, "remote-orchestrator-created")
            .is_some()
    );
    assert!(inner.should_skip_remote_applied_revision(&remote.id, 3));
    assert!(!inner.should_skip_remote_applied_revision(&remote.id, 4));
    drop(inner);

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote orchestrator launch reports an upgrade requirement when the remote ignores inline templates.
#[test]
fn remote_orchestrator_create_requires_upgrade_when_remote_lacks_inline_template_support() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(local_project_id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;

    let captured = Arc::new(Mutex::new(None::<String>));
    let captured_for_server = captured.clone();
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = requests.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "test listener");
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                let bytes_read = stream.read(&mut chunk).expect("request should read");
                assert!(bytes_read > 0, "request closed before headers completed");
                buffer.extend_from_slice(&chunk[..bytes_read]);
                if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end;
                }
            };
            let headers = String::from_utf8_lossy(&buffer[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then_some(value.trim())
                        .and_then(|value| value.parse::<usize>().ok())
                })
                .unwrap_or(0);
            let body_start = header_end + 4;
            while buffer.len() < body_start + content_length {
                let bytes_read = stream.read(&mut chunk).expect("request body should read");
                if bytes_read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..bytes_read]);
            }

            let request_head = String::from_utf8_lossy(&buffer[..body_start]).to_string();
            let request_line = request_head
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();
            requests_for_server
                .lock()
                .expect("capture mutex poisoned")
                .push(request_line.clone());
            let body = String::from_utf8_lossy(&buffer[body_start..body_start + content_length])
                .to_string();

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 11\r\n\r\n{\"ok\":true}",
                    )
                    .expect("health response should write");
                continue;
            }

            if request_line.starts_with("POST /api/orchestrators ") {
                *captured_for_server.lock().expect("capture mutex poisoned") = Some(body);
                let error_body =
                    "{\"error\":\"Inline template launch unavailable on this remote\"}";
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            error_body.len(),
                            error_body
                        )
                        .as_bytes(),
                    )
                    .expect("remote error response should write");
                continue;
            }

            panic!("unexpected request: {request_line}");
        }
    });

    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(true),
                event_bridge_shutdown: AtomicBool::new(false),
                supports_inline_orchestrator_templates: Mutex::new(None),
            }),
        );

    let err = match state.create_orchestrator_instance(CreateOrchestratorInstanceRequest {
        template_id: template.id.clone(),
        project_id: Some(local_project_id),
        template: None,
    }) {
        Ok(_) => panic!("old remote should require an upgrade for inline templates"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::BAD_GATEWAY);
    assert!(err.message.contains("must be upgraded"));
    // The cached capability should suppress only the post-404 diagnostic probe;
    // the normal pre-request availability probe still happens in ensure_available.
    assert_eq!(
        requests.lock().expect("capture mutex poisoned").clone(),
        vec![
            "GET /api/health HTTP/1.1".to_owned(),
            "POST /api/orchestrators HTTP/1.1".to_owned(),
        ]
    );
    let body = captured
        .lock()
        .expect("capture mutex poisoned")
        .clone()
        .expect("captured request should exist");
    let parsed_body: Value = serde_json::from_str(&body).expect("request body should decode");
    assert_eq!(
        parsed_body["templateId"],
        Value::String(template.id.clone())
    );
    assert_eq!(
        parsed_body["template"]["name"],
        Value::String(template.name.clone())
    );

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that a pre-cached unsupported inline-template capability still yields the upgrade message
// without issuing a second health probe after the remote returns 404. The initial
// ensure_available availability check is still expected before the launch attempt.
#[test]
fn remote_orchestrator_create_requires_upgrade_when_inline_template_support_is_precached_false() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(local_project_id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;

    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = requests.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "test listener");
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                let bytes_read = stream.read(&mut chunk).expect("request should read");
                assert!(bytes_read > 0, "request closed before headers completed");
                buffer.extend_from_slice(&chunk[..bytes_read]);
                if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end;
                }
            };
            let headers = String::from_utf8_lossy(&buffer[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then_some(value.trim())
                        .and_then(|value| value.parse::<usize>().ok())
                })
                .unwrap_or(0);
            let body_start = header_end + 4;
            while buffer.len() < body_start + content_length {
                let bytes_read = stream.read(&mut chunk).expect("request body should read");
                if bytes_read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..bytes_read]);
            }

            let request_head = String::from_utf8_lossy(&buffer[..body_start]).to_string();
            let request_line = request_head
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();
            requests_for_server
                .lock()
                .expect("capture mutex poisoned")
                .push(request_line.clone());

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 11\r\n\r\n{\"ok\":true}",
                    )
                    .expect("health response should write");
                continue;
            }

            if request_line.starts_with("POST /api/orchestrators ") {
                let error_body =
                    "{\"error\":\"Inline template launch unavailable on this remote\"}";
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            error_body.len(),
                            error_body
                        )
                        .as_bytes(),
                    )
                    .expect("remote error response should write");
                continue;
            }

            panic!("unexpected request: {request_line}");
        }
    });

    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(true),
                event_bridge_shutdown: AtomicBool::new(false),
                supports_inline_orchestrator_templates: Mutex::new(Some(false)),
            }),
        );

    assert_eq!(
        state
            .remote_registry
            .cached_supports_inline_orchestrator_templates(&remote),
        Some(false)
    );

    let err = match state.create_orchestrator_instance(CreateOrchestratorInstanceRequest {
        template_id: template.id.clone(),
        project_id: Some(local_project_id),
        template: None,
    }) {
        Ok(_) => panic!("old remote should require an upgrade for inline templates"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::BAD_GATEWAY);
    assert!(err.message.contains("must be upgraded"));
    // The cached Some(false) capability skips any extra post-404 health probe, but the
    // initial ensure_available probe still happens before the launch attempt.
    assert_eq!(
        requests.lock().expect("capture mutex poisoned").clone(),
        vec![
            "GET /api/health HTTP/1.1".to_owned(),
            "POST /api/orchestrators HTTP/1.1".to_owned(),
        ]
    );

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote state sync rolls back unmapped orchestrators instead of assigning an empty local project id.
#[test]
fn remote_snapshot_sync_skips_orchestrators_without_a_local_project_mapping() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.preferences.remotes.push(remote.clone());
        state
            .commit_locked(&mut inner)
            .expect("remote should persist");
    }

    state
        .apply_remote_state_snapshot(
            &remote.id,
            sample_remote_orchestrator_state(
                "remote-project-unmapped",
                "/remote/repo",
                1,
                OrchestratorInstanceStatus::Running,
            ),
        )
        .expect("snapshot should still apply even when orchestration localization fails");

    let snapshot = state.snapshot();
    assert!(snapshot.orchestrators.is_empty());
    assert!(snapshot.sessions.is_empty());

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.orchestrator_instances.is_empty());
    assert!(inner.sessions.is_empty());
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote orchestrator lifecycle actions proxy to the remote backend and resync local state.
#[test]
fn remote_orchestrator_lifecycle_actions_proxy_to_remote_backend_and_resync_local_state() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    state
        .apply_remote_state_snapshot(
            &remote.id,
            sample_remote_orchestrator_state(
                "remote-project-1",
                "/remote/repo",
                1,
                OrchestratorInstanceStatus::Running,
            ),
        )
        .expect("initial remote snapshot should apply");
    let local_orchestrator_id = state
        .snapshot()
        .orchestrators
        .into_iter()
        .find(|instance| {
            instance.remote_orchestrator_id.as_deref() == Some("remote-orchestrator-1")
        })
        .expect("remote orchestrator should be mirrored")
        .id;

    let captured = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured_for_server = captured.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let paused_state = serde_json::to_string(&sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Paused,
    ))
    .expect("paused state should encode");
    let resumed_state = serde_json::to_string(&sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        3,
        OrchestratorInstanceStatus::Running,
    ))
    .expect("resumed state should encode");
    let stopped_state = serde_json::to_string(&sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        4,
        OrchestratorInstanceStatus::Stopped,
    ))
    .expect("stopped state should encode");
    let server = std::thread::spawn(move || {
        let mut action_responses = vec![
            (
                "POST /api/orchestrators/remote-orchestrator-1/pause HTTP/1.1".to_owned(),
                paused_state,
            ),
            (
                "POST /api/orchestrators/remote-orchestrator-1/resume HTTP/1.1".to_owned(),
                resumed_state,
            ),
            (
                "POST /api/orchestrators/remote-orchestrator-1/stop HTTP/1.1".to_owned(),
                stopped_state,
            ),
        ]
        .into_iter();
        for _ in 0..6 {
            let mut stream = accept_test_connection(&listener, "test listener");
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                let bytes_read = stream.read(&mut chunk).expect("request should read");
                assert!(bytes_read > 0, "request closed before headers completed");
                buffer.extend_from_slice(&chunk[..bytes_read]);
                if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end;
                }
            };
            let request_head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
            let request_line = request_head
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();
            captured_for_server
                .lock()
                .expect("capture mutex poisoned")
                .push(request_line.clone());

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 11\r\n\r\n{\"ok\":true}",
                    )
                    .expect("health response should write");
                continue;
            }

            let (expected_request_line, response_body) = action_responses
                .next()
                .expect("action response should still be queued");
            assert_eq!(request_line, expected_request_line);
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                        response_body.len(),
                        response_body
                    )
                    .as_bytes(),
                )
                .expect("state response should write");
        }
    });
    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(false),
                event_bridge_shutdown: AtomicBool::new(false),
                supports_inline_orchestrator_templates: Mutex::new(None),
            }),
        );

    let paused = state
        .pause_orchestrator_instance(&local_orchestrator_id)
        .expect("pause should proxy successfully");
    assert_eq!(
        paused
            .orchestrators
            .iter()
            .find(|instance| instance.id == local_orchestrator_id)
            .expect("paused orchestrator should be present")
            .status,
        OrchestratorInstanceStatus::Paused
    );

    let resumed = state
        .resume_orchestrator_instance(&local_orchestrator_id)
        .expect("resume should proxy successfully");
    assert_eq!(
        resumed
            .orchestrators
            .iter()
            .find(|instance| instance.id == local_orchestrator_id)
            .expect("resumed orchestrator should be present")
            .status,
        OrchestratorInstanceStatus::Running
    );

    let stopped = state
        .stop_orchestrator_instance(&local_orchestrator_id)
        .expect("stop should proxy successfully");
    assert_eq!(
        stopped
            .orchestrators
            .iter()
            .find(|instance| instance.id == local_orchestrator_id)
            .expect("stopped orchestrator should be present")
            .status,
        OrchestratorInstanceStatus::Stopped
    );

    assert_eq!(
        captured.lock().expect("capture mutex poisoned").clone(),
        vec![
            "GET /api/health HTTP/1.1".to_owned(),
            "POST /api/orchestrators/remote-orchestrator-1/pause HTTP/1.1".to_owned(),
            "GET /api/health HTTP/1.1".to_owned(),
            "POST /api/orchestrators/remote-orchestrator-1/resume HTTP/1.1".to_owned(),
            "GET /api/health HTTP/1.1".to_owned(),
            "POST /api/orchestrators/remote-orchestrator-1/stop HTTP/1.1".to_owned(),
        ]
    );
    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote snapshot sync keeps the previous remote orchestrators when localization fails.
#[test]
fn remote_snapshot_sync_preserves_existing_orchestrators_when_localization_fails() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut initial_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    );
    let mut second_orchestrator = initial_state.orchestrators[0].clone();
    second_orchestrator.id = "remote-orchestrator-2".to_owned();
    second_orchestrator.status = OrchestratorInstanceStatus::Paused;
    initial_state.orchestrators.push(second_orchestrator);
    state
        .apply_remote_state_snapshot(&remote.id, initial_state)
        .expect("initial remote snapshot should apply");

    let initial_remote_orchestrator_ids = state
        .snapshot()
        .orchestrators
        .into_iter()
        .filter(|instance| instance.remote_id.as_deref() == Some(remote.id.as_str()))
        .filter_map(|instance| instance.remote_orchestrator_id)
        .collect::<HashSet<_>>();
    assert_eq!(
        initial_remote_orchestrator_ids,
        [
            "remote-orchestrator-1".to_owned(),
            "remote-orchestrator-2".to_owned()
        ]
        .into_iter()
        .collect::<HashSet<_>>()
    );

    let mut invalid_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    invalid_state.orchestrators[0].id = "remote-orchestrator-2".to_owned();
    let mut invalid_orchestrator = invalid_state.orchestrators[0].clone();
    invalid_orchestrator.id = "remote-orchestrator-3".to_owned();
    invalid_orchestrator.session_instances[0].session_id = "missing-remote-session".to_owned();
    invalid_state.orchestrators.push(invalid_orchestrator);

    state
        .apply_remote_state_snapshot(&remote.id, invalid_state)
        .expect("remote snapshot should still apply when orchestrator localization fails");

    let remote_orchestrator_ids = state
        .snapshot()
        .orchestrators
        .into_iter()
        .filter(|instance| instance.remote_id.as_deref() == Some(remote.id.as_str()))
        .filter_map(|instance| instance.remote_orchestrator_id)
        .collect::<HashSet<_>>();
    assert_eq!(
        remote_orchestrator_ids,
        [
            "remote-orchestrator-1".to_owned(),
            "remote-orchestrator-2".to_owned()
        ]
        .into_iter()
        .collect::<HashSet<_>>()
    );
    assert!(!remote_orchestrator_ids.contains("remote-orchestrator-3"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote snapshot sync preserves referenced sessions when orchestrator localization fails.
#[test]
fn remote_snapshot_sync_preserves_sessions_referenced_by_existing_orchestrators_when_localization_fails()
 {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let initial_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    );
    state
        .apply_remote_state_snapshot(&remote.id, initial_state)
        .expect("initial remote snapshot should apply");

    let (preserved_local_session_id, preserved_preview) = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("remote mirrored session should exist");
        (
            inner.sessions[index].session.id.clone(),
            inner.sessions[index].session.preview.clone(),
        )
    };

    let mut invalid_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    invalid_state
        .sessions
        .retain(|session| session.id != "remote-session-1");

    state
        .apply_remote_state_snapshot(&remote.id, invalid_state)
        .expect("remote snapshot should still apply when orchestrator localization fails");

    let snapshot = state.snapshot();
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.id == preserved_local_session_id)
            .expect("referenced mirrored session should remain")
            .preview,
        preserved_preview
    );
    let preserved_orchestrator = snapshot
        .orchestrators
        .iter()
        .find(|instance| {
            instance.remote_id.as_deref() == Some(remote.id.as_str())
                && instance.remote_orchestrator_id.as_deref() == Some("remote-orchestrator-1")
        })
        .expect("existing mirrored orchestrator should remain");
    assert!(
        preserved_orchestrator
            .session_instances
            .iter()
            .any(|instance| instance.session_id == preserved_local_session_id)
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that focused remote state sync rolls back eager proxy-session side effects when orchestrator localization fails.
#[test]
fn focused_remote_state_sync_rolls_back_proxy_sessions_when_orchestrator_localization_fails() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut initial_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    initial_remote_session.preview = "Before focused sync.".to_owned();

    let local_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let local_session_id = upsert_remote_proxy_session_record(
            &mut inner,
            &remote.id,
            &initial_remote_session,
            Some(local_project_id),
        );
        state
            .commit_locked(&mut inner)
            .expect("initial focused remote session should persist");
        local_session_id
    };
    let initial_session_count = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner.sessions.len()
    };

    let mut invalid_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    invalid_state
        .sessions
        .retain(|session| session.id != "remote-session-3");
    invalid_state
        .sessions
        .iter_mut()
        .find(|session| session.id == "remote-session-1")
        .expect("focused session should remain in the payload")
        .preview = "Focused sync updated.".to_owned();

    let target = RemoteSessionTarget {
        local_session_id: local_session_id.clone(),
        remote: remote.clone(),
        remote_session_id: "remote-session-1".to_owned(),
    };
    state
        .sync_remote_state_for_target(&target, invalid_state)
        .expect("focused remote sync should preserve the target session update");

    let snapshot = state.snapshot();
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.id == local_session_id)
            .expect("focused local session should remain")
            .preview,
        "Focused sync updated."
    );
    assert!(
        !snapshot
            .orchestrators
            .iter()
            .any(|instance| { instance.remote_id.as_deref() == Some(remote.id.as_str()) })
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.sessions.len(), initial_session_count);
    assert!(
        inner
            .find_remote_session_index(&remote.id, "remote-session-2")
            .is_none()
    );
    drop(inner);

    let persisted: Value = serde_json::from_slice(
        &fs::read(state.persistence_path.as_path()).expect("persisted state file should exist"),
    )
    .expect("persisted state should deserialize");
    let persisted_sessions = persisted["sessions"]
        .as_array()
        .expect("persisted sessions should be present");
    assert_eq!(persisted_sessions.len(), initial_session_count);
    assert!(!persisted_sessions.iter().any(|candidate| {
        candidate["remoteSessionId"] == Value::String("remote-session-2".to_owned())
    }));
    let persisted_focused = persisted_sessions
        .iter()
        .find(|candidate| {
            candidate["remoteSessionId"] == Value::String("remote-session-1".to_owned())
        })
        .expect("focused mirrored session should persist");
    assert_eq!(
        persisted_focused["session"]["preview"],
        Value::String("Focused sync updated.".to_owned())
    );
    let persisted_orchestrator_instances = persisted["orchestratorInstances"].as_array();
    assert!(persisted_orchestrator_instances.map_or(true, |instances| {
        !instances
            .iter()
            .any(|instance| instance["remoteId"] == Value::String(remote.id.clone()))
    }));
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that focused remote sync ignores stale remote revisions instead of
// rolling an already-mirrored session backward.
#[test]
fn focused_remote_state_sync_skips_stale_revision() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut initial_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    initial_remote_session.preview = "Newest preview.".to_owned();

    let local_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let local_session_id = upsert_remote_proxy_session_record(
            &mut inner,
            &remote.id,
            &initial_remote_session,
            Some(local_project_id),
        );
        state
            .commit_locked(&mut inner)
            .expect("initial focused remote session should persist");
        inner.note_remote_applied_revision(&remote.id, 3);
        local_session_id
    };

    let mut stale_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    stale_state
        .sessions
        .iter_mut()
        .find(|session| session.id == "remote-session-1")
        .expect("focused session should remain in the payload")
        .preview = "Stale preview should be skipped.".to_owned();

    let target = RemoteSessionTarget {
        local_session_id: local_session_id.clone(),
        remote: remote.clone(),
        remote_session_id: "remote-session-1".to_owned(),
    };
    state
        .sync_remote_state_for_target(&target, stale_state)
        .expect("stale focused sync should be ignored");

    let snapshot = state.snapshot();
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.id == local_session_id)
            .expect("focused local session should remain")
            .preview,
        "Newest preview."
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.should_skip_remote_applied_revision(&remote.id, 3));
    assert!(!inner.should_skip_remote_applied_revision(&remote.id, 4));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote snapshot sync removes missing proxy sessions.
#[test]
fn remote_snapshot_sync_removes_missing_proxy_sessions() {
    let state = test_app_state();
    let (kept_local_session_id, removed_local_session_id, local_session_id) = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let kept = inner.create_session(Agent::Codex, None, "/tmp".to_owned(), None, None);
        let removed = inner.create_session(Agent::Codex, None, "/tmp".to_owned(), None, None);
        let local = inner.create_session(Agent::Claude, None, "/tmp".to_owned(), None, None);

        let kept_index = inner
            .find_session_index(&kept.session.id)
            .expect("kept session should exist");
        inner.sessions[kept_index].remote_id = Some("ssh-lab".to_owned());
        inner.sessions[kept_index].remote_session_id = Some("remote-session-keep".to_owned());

        let removed_index = inner
            .find_session_index(&removed.session.id)
            .expect("removed session should exist");
        inner.sessions[removed_index].remote_id = Some("ssh-lab".to_owned());
        inner.sessions[removed_index].remote_session_id = Some("remote-session-gone".to_owned());

        (kept.session.id, removed.session.id, local.session.id)
    };

    let mut remote_state = state.snapshot();
    let mut remote_session = remote_state
        .sessions
        .iter()
        .find(|session| session.id == kept_local_session_id)
        .cloned()
        .expect("kept session should be present in the snapshot");
    remote_session.id = "remote-session-keep".to_owned();
    remote_session.preview = "Remote session still exists.".to_owned();
    remote_state.sessions = vec![remote_session];

    state
        .apply_remote_state_snapshot("ssh-lab", remote_state)
        .expect("remote snapshot should apply");

    let snapshot = state.snapshot();
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.id == kept_local_session_id)
    );
    assert!(
        !snapshot
            .sessions
            .iter()
            .any(|session| session.id == removed_local_session_id)
    );
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.id == local_session_id)
    );
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.id == kept_local_session_id)
            .expect("kept session should remain")
            .preview,
        "Remote session still exists."
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that marked remote fallback payloads dedupe repeated revisions but still
// resync immediately when a newer fallback revision arrives.
#[test]
fn remote_state_event_dedupes_marked_sse_fallback_resyncs_by_revision() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.preferences.remotes.push(remote.clone());
    }
    let (remote_local_session_id, local_session_id) = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let remote_record = inner.create_session(Agent::Codex, None, "/tmp".to_owned(), None, None);
        let local_record = inner.create_session(Agent::Claude, None, "/tmp".to_owned(), None, None);

        let remote_index = inner
            .find_session_index(&remote_record.session.id)
            .expect("remote session should exist");
        inner.sessions[remote_index].remote_id = Some("ssh-lab".to_owned());
        inner.sessions[remote_index].remote_session_id = Some("remote-session-keep".to_owned());

        (remote_record.session.id, local_record.session.id)
    };

    let mut first_full_state_response = state.snapshot();
    let mut first_remote_session = first_full_state_response
        .sessions
        .iter()
        .find(|session| session.id == remote_local_session_id)
        .cloned()
        .expect("remote session should be present in the snapshot");
    first_remote_session.id = "remote-session-keep".to_owned();
    first_remote_session.preview = "Hydrated from /api/state v1".to_owned();
    first_full_state_response.sessions = vec![first_remote_session];
    let first_full_state_response =
        serde_json::to_string(&first_full_state_response).expect("state response should encode");

    let mut second_full_state_response = state.snapshot();
    let mut second_remote_session = second_full_state_response
        .sessions
        .iter()
        .find(|session| session.id == remote_local_session_id)
        .cloned()
        .expect("remote session should be present in the snapshot");
    second_remote_session.id = "remote-session-keep".to_owned();
    second_remote_session.preview = "Hydrated from /api/state v2".to_owned();
    second_full_state_response.revision = second_full_state_response.revision.saturating_add(1);
    second_full_state_response.sessions = vec![second_remote_session];
    let second_full_state_response =
        serde_json::to_string(&second_full_state_response).expect("state response should encode");

    let captured = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured_for_server = captured.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        let mut state_responses =
            vec![first_full_state_response, second_full_state_response].into_iter();
        for _ in 0..4 {
            let mut stream = accept_test_connection(&listener, "test listener");
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                let bytes_read = stream.read(&mut chunk).expect("request should read");
                assert!(bytes_read > 0, "request closed before headers completed");
                buffer.extend_from_slice(&chunk[..bytes_read]);
                if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end;
                }
            };
            let request_head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
            let request_line = request_head
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();
            captured_for_server
                .lock()
                .expect("capture mutex poisoned")
                .push(request_line.clone());

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 11\r\n\r\n{\"ok\":true}",
                    )
                    .expect("health response should write");
                continue;
            }

            if request_line.starts_with("GET /api/state ") {
                let response = state_responses
                    .next()
                    .expect("state response should still be queued");
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            response.len(),
                            response
                        )
                        .as_bytes(),
                    )
                    .expect("state response should write");
                continue;
            }

            panic!("unexpected request: {request_line}");
        }
    });
    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(false),
                event_bridge_shutdown: AtomicBool::new(false),
                supports_inline_orchestrator_templates: Mutex::new(None),
            }),
        );

    let mut first_fallback_payload: Value =
        serde_json::from_str(EMPTY_STATE_EVENTS_PAYLOAD.as_str())
            .expect("fallback payload should parse");
    first_fallback_payload["revision"] = json!(4);
    let first_data_lines = serde_json::to_string_pretty(&first_fallback_payload)
        .expect("first fallback payload should encode")
        .lines()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    let mut second_fallback_payload = first_fallback_payload.clone();
    second_fallback_payload["revision"] = json!(5);
    let second_data_lines = serde_json::to_string_pretty(&second_fallback_payload)
        .expect("second fallback payload should encode")
        .lines()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    dispatch_remote_event(&state, "ssh-lab", "state", &first_data_lines)
        .expect("first fallback state payload should trigger a resync");
    dispatch_remote_event(&state, "ssh-lab", "state", &first_data_lines)
        .expect("duplicate fallback revision should be deduped");
    dispatch_remote_event(&state, "ssh-lab", "state", &second_data_lines)
        .expect("newer fallback revision should trigger another resync");

    let snapshot = state.snapshot();
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.id == remote_local_session_id)
    );
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.id == local_session_id)
    );
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.id == remote_local_session_id)
            .expect("remote mirrored session should remain")
            .preview,
        "Hydrated from /api/state v2"
    );
    assert_eq!(
        captured.lock().expect("capture mutex poisoned").clone(),
        vec![
            "GET /api/health HTTP/1.1".to_owned(),
            "GET /api/state HTTP/1.1".to_owned(),
            "GET /api/health HTTP/1.1".to_owned(),
            "GET /api/state HTTP/1.1".to_owned(),
        ]
    );
    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote fallback resync tracking is per remote, monotonic within a
// single event-stream lifetime, and resettable after disconnects.
#[test]
fn remote_sse_fallback_resync_tracking_is_per_remote_and_monotonic() {
    let state = test_app_state();

    assert!(!state.should_skip_remote_sse_fallback_resync("ssh-lab", 0));
    state.note_remote_sse_fallback_resync("ssh-lab", 0);
    assert!(state.should_skip_remote_sse_fallback_resync("ssh-lab", 0));
    assert!(state.should_skip_remote_sse_fallback_resync("ssh-lab", 0));
    assert!(!state.should_skip_remote_sse_fallback_resync("ssh-lab", 1));
    assert!(!state.should_skip_remote_sse_fallback_resync("ssh-lab-2", 0));

    state.note_remote_sse_fallback_resync("ssh-lab", 1);
    assert!(state.should_skip_remote_sse_fallback_resync("ssh-lab", 1));
    assert!(state.should_skip_remote_sse_fallback_resync("ssh-lab", 0));
    assert!(!state.should_skip_remote_sse_fallback_resync("ssh-lab-2", 1));

    state.clear_remote_sse_fallback_resync("ssh-lab");
    assert!(!state.should_skip_remote_sse_fallback_resync("ssh-lab", 1));
    assert!(!state.should_skip_remote_sse_fallback_resync("ssh-lab", 0));
    assert!(!state.should_skip_remote_sse_fallback_resync("ssh-lab-2", 1));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that StateInner revision tracking handles first insert, same revision,
// higher revision, and lower revision without regressing monotonic ordering.
#[test]
fn state_inner_remote_applied_revision_methods_cover_monotonic_cases() {
    let mut inner = StateInner::new();

    assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 1));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 1));

    inner.note_remote_applied_revision("ssh-lab", 1);
    assert!(inner.should_skip_remote_applied_revision("ssh-lab", 1));
    assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 2));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 1));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 2));

    inner.note_remote_applied_revision("ssh-lab", 4);
    assert!(inner.should_skip_remote_applied_revision("ssh-lab", 4));
    assert!(inner.should_skip_remote_applied_revision("ssh-lab", 3));
    assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 5));
    assert!(inner.should_skip_remote_applied_delta_revision("ssh-lab", 3));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 4));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 5));

    inner.note_remote_applied_revision("ssh-lab", 2);
    assert!(inner.should_skip_remote_applied_revision("ssh-lab", 4));
    assert!(inner.should_skip_remote_applied_revision("ssh-lab", 2));
    assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 5));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 4));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 5));
    assert!(!inner.should_skip_remote_applied_revision("ssh-lab-2", 1));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab-2", 1));
}



// Tests that raw remote error bodies are sanitized and capped before they reach the UI.
#[test]
fn decode_remote_json_sanitizes_and_caps_raw_error_bodies() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let noisy_body = format!(
        "<html>\0Service\tUnavailable\r\nDetails {}{}\u{7}</html>",
        "A".repeat(600),
        "B".repeat(32)
    );
    let response_body_len = noisy_body.as_bytes().len();
    let server = std::thread::spawn(move || {
        let mut stream = accept_test_connection(&listener, "test listener");
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let bytes_read = stream.read(&mut chunk).expect("request should read");
            assert!(bytes_read > 0, "request closed before headers completed");
            buffer.extend_from_slice(&chunk[..bytes_read]);
            if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        stream
            .write_all(
                format!(
                    "HTTP/1.1 503 Service Unavailable\r\nContent-Type: text/html\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                    response_body_len, noisy_body
                )
                .as_bytes(),
            )
            .expect("error response should write");
    });

    let client = BlockingHttpClient::builder()
        .build()
        .expect("test HTTP client should build");
    let response = client
        .get(format!("http://127.0.0.1:{port}/api/health"))
        .send()
        .expect("error response should still be returned");
    let err = match decode_remote_json::<HealthResponse>(response) {
        Ok(_) => panic!("raw non-JSON 503 should surface as an ApiError"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(err.message.contains("Service Unavailable Details"));
    assert!(err.message.chars().count() <= 512);
    assert!(err.message.ends_with("..."));
    assert!(!err.message.contains('\r'));
    assert!(!err.message.contains('\n'));
    assert!(!err.message.contains('\t'));
    assert!(!err.message.chars().any(|ch| ch.is_control()));

    join_test_server(server);
}

// Tests that structured remote JSON error messages are sanitized and capped before they reach the UI.
#[test]
fn decode_remote_json_sanitizes_structured_error_bodies() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let noisy_message = format!(
        "Remote\tfailure\r\n{}{}\u{7}",
        "A".repeat(600),
        "B".repeat(32)
    );
    let response_body = serde_json::to_string(&json!({
        "error": noisy_message,
    }))
    .expect("structured error response should encode");
    let response_body_len = response_body.as_bytes().len();
    let server = std::thread::spawn(move || {
        let mut stream = accept_test_connection(&listener, "test listener");
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let bytes_read = stream.read(&mut chunk).expect("request should read");
            assert!(bytes_read > 0, "request closed before headers completed");
            buffer.extend_from_slice(&chunk[..bytes_read]);
            if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        stream
            .write_all(
                format!(
                    "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                    response_body_len, response_body
                )
                .as_bytes(),
            )
            .expect("error response should write");
    });

    let client = BlockingHttpClient::builder()
        .build()
        .expect("test HTTP client should build");
    let response = client
        .get(format!("http://127.0.0.1:{port}/api/health"))
        .send()
        .expect("error response should still be returned");
    let err = match decode_remote_json::<HealthResponse>(response) {
        Ok(_) => panic!("structured JSON 503 should surface as an ApiError"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(err.message.contains("Remote failure"));
    assert!(err.message.chars().count() <= 512);
    assert!(err.message.ends_with("..."));
    assert!(!err.message.contains('\r'));
    assert!(!err.message.contains('\n'));
    assert!(!err.message.contains('\t'));
    assert!(!err.message.chars().any(|ch| ch.is_control()));

    join_test_server(server);
}

// Tests that oversized remote error bodies are rejected before they are fully decoded into a String.
#[test]
fn decode_remote_json_rejects_oversized_error_bodies() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let oversized_body = "X".repeat(70_000);
    let response_body_len = oversized_body.as_bytes().len();
    let server = std::thread::spawn(move || {
        let mut stream = accept_test_connection(&listener, "test listener");
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let bytes_read = stream.read(&mut chunk).expect("request should read");
            assert!(bytes_read > 0, "request closed before headers completed");
            buffer.extend_from_slice(&chunk[..bytes_read]);
            if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        stream
            .write_all(
                format!(
                    "HTTP/1.1 503 Service Unavailable\r\nContent-Type: text/plain\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                    response_body_len, oversized_body
                )
                .as_bytes(),
            )
            .expect("error response should write");
    });

    let client = BlockingHttpClient::builder()
        .build()
        .expect("test HTTP client should build");
    let response = client
        .get(format!("http://127.0.0.1:{port}/api/health"))
        .send()
        .expect("error response should still be returned");
    let err = match decode_remote_json::<HealthResponse>(response) {
        Ok(_) => panic!("oversized error response should surface as an ApiError"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(err.message, "remote error response too large");

    join_test_server(server);
}

// Tests that applied remote revisions are tracked per remote, stay monotonic,
// and can be reset when an event stream is re-established.
#[test]
fn remote_applied_revision_tracking_is_per_remote_and_monotonic() {
    let state = test_app_state();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 0));
        inner.note_remote_applied_revision("ssh-lab", 0);
        assert!(inner.should_skip_remote_applied_revision("ssh-lab", 0));
        assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 1));
        assert!(!inner.should_skip_remote_applied_revision("ssh-lab-2", 0));

        inner.note_remote_applied_revision("ssh-lab", 2);
        inner.note_remote_applied_revision("ssh-lab", 1);
        assert!(inner.should_skip_remote_applied_revision("ssh-lab", 2));
        assert!(inner.should_skip_remote_applied_revision("ssh-lab", 1));
        assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 3));
        assert!(!inner.should_skip_remote_applied_revision("ssh-lab-2", 2));
    }

    state.clear_remote_applied_revision("ssh-lab");
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 2));
    assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 0));
    assert!(!inner.should_skip_remote_applied_revision("ssh-lab-2", 2));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that non-fallback empty remote state payloads still apply directly.
#[test]
fn remote_state_event_applies_non_fallback_empty_snapshot_payload() {
    let state = test_app_state();
    let (removed_local_session_id, local_session_id) = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let removed = inner.create_session(Agent::Codex, None, "/tmp".to_owned(), None, None);
        let local = inner.create_session(Agent::Claude, None, "/tmp".to_owned(), None, None);

        let removed_index = inner
            .find_session_index(&removed.session.id)
            .expect("remote session should exist");
        inner.sessions[removed_index].remote_id = Some("ssh-lab".to_owned());
        inner.sessions[removed_index].remote_session_id = Some("remote-session-gone".to_owned());

        (removed.session.id, local.session.id)
    };

    let mut remote_state = empty_state_events_response();
    remote_state.revision = 1;
    let data_lines =
        vec![serde_json::to_string(&remote_state).expect("state payload should encode")];
    dispatch_remote_event(&state, "ssh-lab", "state", &data_lines)
        .expect("ordinary empty state payload should apply");

    let snapshot = state.snapshot();
    assert!(
        !snapshot
            .sessions
            .iter()
            .any(|session| session.id == removed_local_session_id)
    );
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.id == local_session_id)
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote review put sends scope via query params.
#[test]
fn remote_review_put_sends_scope_via_query_params() {
    let captured = Arc::new(Mutex::new(None::<(String, String)>));
    let captured_for_server = captured.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "test listener");
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                let bytes_read = stream.read(&mut chunk).expect("request should read");
                assert!(bytes_read > 0, "request closed before headers completed");
                buffer.extend_from_slice(&chunk[..bytes_read]);
                if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end;
                }
            };
            let headers = String::from_utf8_lossy(&buffer[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then_some(value.trim())
                        .and_then(|value| value.parse::<usize>().ok())
                })
                .unwrap_or(0);
            let body_start = header_end + 4;
            while buffer.len() < body_start + content_length {
                let bytes_read = stream.read(&mut chunk).expect("request body should read");
                if bytes_read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..bytes_read]);
            }

            let request_head = String::from_utf8_lossy(&buffer[..body_start]).to_string();
            let request_line = request_head
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();
            let body = String::from_utf8_lossy(&buffer[body_start..body_start + content_length])
                .to_string();

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 11\r\n\r\n{\"ok\":true}",
                    )
                    .expect("health response should write");
                continue;
            }

            if request_line.starts_with("PUT /api/reviews/change-set-1?") {
                *captured_for_server.lock().expect("capture mutex poisoned") =
                    Some((request_line.clone(), body));
                let response = serde_json::to_string(&ReviewDocumentResponse {
                    review_file_path: "/remote/.termal/reviews/change-set-1.json".to_owned(),
                    review: ReviewDocument {
                        version: 1,
                        change_set_id: "change-set-1".to_owned(),
                        revision: 0,
                        origin: None,
                        files: Vec::new(),
                        threads: Vec::new(),
                    },
                })
                .expect("review response should encode");
                let response_bytes = response.as_bytes();
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            response_bytes.len(),
                            response
                        )
                        .as_bytes(),
                    )
                    .expect("review response should write");
                continue;
            }

            panic!("unexpected request: {request_line}");
        }
    });

    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(false),
                event_bridge_shutdown: AtomicBool::new(false),
                supports_inline_orchestrator_templates: Mutex::new(None),
            }),
        );

    let response: ReviewDocumentResponse = state
        .remote_put_json_with_query_scope(
            &RemoteScope {
                remote,
                remote_project_id: None,
                remote_session_id: Some("remote-session-1".to_owned()),
            },
            "/api/reviews/change-set-1",
            Vec::new(),
            json!({
                "version": 1,
                "changeSetId": "change-set-1",
                "revision": 0,
                "threads": [],
            }),
        )
        .expect("remote review PUT should succeed");

    assert_eq!(
        response.review_file_path,
        "/remote/.termal/reviews/change-set-1.json"
    );
    let (request_line, body) = captured
        .lock()
        .expect("capture mutex poisoned")
        .clone()
        .expect("captured request should exist");
    assert!(request_line.contains("sessionId=remote-session-1"));
    assert!(!request_line.contains("projectId="));
    let parsed_body: Value = serde_json::from_str(&body).expect("review body should decode");
    assert_eq!(parsed_body.get("sessionId"), None);
    assert_eq!(parsed_body.get("projectId"), None);

    join_test_server(server);
}


#[tokio::test]
async fn remote_terminal_stream_rejects_successful_non_sse_without_json_fallback() {
    let saw_stream_request = Arc::new(AtomicBool::new(false));
    let saw_stream_request_for_server = saw_stream_request.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        loop {
            let mut stream = accept_test_connection(&listener, "remote terminal stream listener");
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                let bytes_read = stream.read(&mut chunk).expect("request should read");
                assert!(bytes_read > 0, "request closed before headers completed");
                buffer.extend_from_slice(&chunk[..bytes_read]);
                if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end;
                }
            };
            let headers = String::from_utf8_lossy(&buffer[..header_end]);
            let request_line = headers
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then_some(value.trim())
                        .and_then(|value| value.parse::<usize>().ok())
                })
                .unwrap_or(0);
            let body_start = header_end + 4;
            while buffer.len() < body_start + content_length {
                let bytes_read = stream.read(&mut chunk).expect("request body should read");
                if bytes_read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..bytes_read]);
            }

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 11\r\n\r\n{\"ok\":true}",
                    )
                    .expect("health response should write");
                continue;
            }

            if request_line.starts_with("POST /api/terminal/run/stream ") {
                saw_stream_request_for_server.store(true, Ordering::SeqCst);
                let body = "already accepted";
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            body.len(),
                            body
                        )
                        .as_bytes(),
                    )
                    .expect("html stream response should write");
                break;
            }

            if request_line.starts_with("POST /api/terminal/run ") {
                panic!("successful non-SSE stream responses must not fall back to JSON execution");
            }

            panic!("unexpected request: {request_line}");
        }
    });

    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-stream-html".to_owned(),
        name: "SSH Stream HTML".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Stream HTML",
        "remote-stream-html-project",
    );
    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(true),
                event_bridge_shutdown: AtomicBool::new(false),
                supports_inline_orchestrator_templates: Mutex::new(None),
            }),
        );
    let app = app_router(state);

    let response = request_response(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run/stream")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo remote",
                    "projectId": project_id,
                    "workdir": "/remote/repo",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let mut body = Box::pin(response.into_body().into_data_stream());
    let event = next_sse_event(&mut body).await;
    let (event_name, event_data) = parse_sse_event(&event);
    assert_eq!(event_name, "error");
    let payload: Value = serde_json::from_str(&event_data).expect("error event should decode");
    assert_eq!(
        payload["status"],
        Value::from(StatusCode::BAD_GATEWAY.as_u16())
    );
    assert!(
        payload["error"]
            .as_str()
            .unwrap()
            .contains("unexpected content type"),
        "unexpected SSE error payload: {payload}"
    );
    assert!(saw_stream_request.load(Ordering::SeqCst));
    join_test_server(server);
}

#[tokio::test]
async fn remote_terminal_stream_proxies_successful_sse_output() {
    let request_lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let request_lines_for_server = request_lines.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let remote_response = TerminalCommandResponse {
        command: "echo remote".to_owned(),
        duration_ms: 11,
        exit_code: Some(0),
        output_truncated: false,
        shell: "sh".to_owned(),
        stderr: String::new(),
        stdout: "remote chunk\nremote done\n".to_owned(),
        success: true,
        timed_out: false,
        workdir: "/remote/repo".to_owned(),
    };
    let server = std::thread::spawn(move || {
        loop {
            let mut stream = accept_test_connection_with_timeout(
                &listener,
                "remote terminal SSE listener",
                std::time::Duration::from_secs(10),
            );
            let request = read_test_http_request(&mut stream);
            request_lines_for_server
                .lock()
                .expect("request lines mutex poisoned")
                .push(request.request_line.clone());

            if request.request_line.starts_with("GET /api/health ") {
                write_test_http_response(
                    &mut stream,
                    StatusCode::OK,
                    "application/json",
                    r#"{"ok":true}"#,
                );
                continue;
            }

            if request
                .request_line
                .starts_with("POST /api/terminal/run/stream ")
            {
                let output = json!({ "stream": "stdout", "text": "remote chunk\n" });
                let complete = serde_json::to_string(&remote_response)
                    .expect("remote terminal response should encode");
                let body = format!(
                    "event: output\ndata: {output}\n\nevent: complete\ndata: {complete}\n\n"
                );
                write_test_http_response(&mut stream, StatusCode::OK, "text/event-stream", &body);
                break;
            }

            if request.request_line.starts_with("POST /api/terminal/run ") {
                panic!("successful remote stream should not fall back to JSON execution");
            }

            panic!("unexpected request: {}", request.request_line);
        }
    });

    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-stream-sse".to_owned(),
        name: "SSH Stream SSE".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Stream SSE",
        "remote-stream-sse-project",
    );
    insert_test_remote_connection(&state, &remote, port);
    let app = app_router(state);

    let response = request_response(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run/stream")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo remote",
                    "projectId": project_id,
                    "workdir": "/remote/repo",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let events = collect_sse_events(response).await;
    let output_index = events
        .iter()
        .position(|(event_name, _)| event_name == "output")
        .expect("remote output should be forwarded");
    let complete_events = events
        .iter()
        .enumerate()
        .filter(|(_, (event_name, _))| event_name == "complete")
        .collect::<Vec<_>>();
    assert_eq!(complete_events.len(), 1, "events: {events:?}");
    assert!(
        output_index < complete_events[0].0,
        "remote output should precede completion: {events:?}"
    );
    assert!(
        events.iter().all(|(event_name, _)| event_name != "error"),
        "successful remote stream should not emit an error: {events:?}"
    );
    let output: Value =
        serde_json::from_str(&events[output_index].1).expect("output event should decode");
    assert_eq!(output["stream"], Value::String("stdout".to_owned()));
    assert_eq!(output["text"], Value::String("remote chunk\n".to_owned()));
    let complete = serde_json::from_str::<TerminalCommandResponse>(&complete_events[0].1.1)
        .expect("complete event should decode");
    assert_eq!(complete.stdout, "remote chunk\nremote done\n");
    assert!(complete.success);

    join_test_server(server);
    let request_lines = request_lines.lock().expect("request lines mutex poisoned");
    assert!(
        request_lines
            .iter()
            .any(|line| line.starts_with("POST /api/terminal/run/stream ")),
        "remote stream route was not requested: {request_lines:?}"
    );
    assert!(
        request_lines
            .iter()
            .all(|line| !line.starts_with("POST /api/terminal/run ")),
        "remote JSON fallback should not run on successful SSE: {request_lines:?}"
    );
}

#[tokio::test]
async fn remote_terminal_stream_falls_back_to_json_when_stream_route_is_404_or_405() {
    assert_remote_terminal_stream_fallback_for_status(StatusCode::NOT_FOUND).await;
    assert_remote_terminal_stream_fallback_for_status(StatusCode::METHOD_NOT_ALLOWED).await;
}

async fn assert_remote_terminal_stream_fallback_for_status(stream_status: StatusCode) {
    let request_lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let request_lines_for_server = request_lines.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let remote_response = serde_json::to_string(&TerminalCommandResponse {
        command: "echo fallback".to_owned(),
        duration_ms: 17,
        exit_code: Some(0),
        output_truncated: false,
        shell: "sh".to_owned(),
        stderr: String::new(),
        stdout: format!("fallback {}\n", stream_status.as_u16()),
        success: true,
        timed_out: false,
        workdir: "/remote/repo".to_owned(),
    })
    .expect("terminal response should encode");
    let server = std::thread::spawn(move || {
        loop {
            let mut stream = accept_test_connection_with_timeout(
                &listener,
                "remote terminal fallback listener",
                std::time::Duration::from_secs(10),
            );
            let request = read_test_http_request(&mut stream);
            request_lines_for_server
                .lock()
                .expect("request lines mutex poisoned")
                .push(request.request_line.clone());

            if request.request_line.starts_with("GET /api/health ") {
                write_test_http_response(
                    &mut stream,
                    StatusCode::OK,
                    "application/json",
                    r#"{"ok":true}"#,
                );
                continue;
            }

            if request
                .request_line
                .starts_with("POST /api/terminal/run/stream ")
            {
                write_test_http_response(
                    &mut stream,
                    stream_status,
                    "application/json",
                    r#"{"error":"stream route unavailable"}"#,
                );
                continue;
            }

            if request.request_line.starts_with("POST /api/terminal/run ") {
                let body: Value =
                    serde_json::from_str(&request.body).expect("fallback request should decode");
                assert_eq!(body["command"], Value::String("echo fallback".to_owned()));
                write_test_http_response(
                    &mut stream,
                    StatusCode::OK,
                    "application/json",
                    &remote_response,
                );
                break;
            }

            panic!("unexpected request: {}", request.request_line);
        }
    });

    let state = test_app_state();
    let remote = RemoteConfig {
        id: format!("ssh-stream-fallback-{}", stream_status.as_u16()),
        name: format!("SSH Stream Fallback {}", stream_status.as_u16()),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        &format!("Remote Stream Fallback {}", stream_status.as_u16()),
        &format!("remote-stream-fallback-project-{}", stream_status.as_u16()),
    );
    insert_test_remote_connection(&state, &remote, port);
    let app = app_router(state);

    let response = request_response(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run/stream")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo fallback",
                    "projectId": project_id,
                    "workdir": "/remote/repo",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let events = collect_sse_events(response).await;
    assert!(
        events.iter().all(|(event_name, _)| event_name != "error"),
        "fallback stream should not emit an error: {events:?}"
    );
    let complete_events = events
        .iter()
        .filter(|(event_name, _)| event_name == "complete")
        .collect::<Vec<_>>();
    assert_eq!(complete_events.len(), 1, "events: {events:?}");
    let complete = serde_json::from_str::<TerminalCommandResponse>(&complete_events[0].1)
        .expect("complete event should decode");
    assert_eq!(
        complete.stdout,
        format!("fallback {}\n", stream_status.as_u16())
    );
    assert!(complete.success);

    join_test_server(server);
    let request_lines = request_lines.lock().expect("request lines mutex poisoned");
    assert!(
        request_lines
            .iter()
            .any(|line| line.starts_with("POST /api/terminal/run/stream ")),
        "stream route was not attempted: {request_lines:?}"
    );
    assert!(
        request_lines
            .iter()
            .any(|line| line.starts_with("POST /api/terminal/run ")),
        "fallback JSON route was not attempted: {request_lines:?}"
    );
}

#[test]
fn terminal_sse_parser_handles_default_events_comments_and_multiline_data() {
    assert_eq!(
        parse_terminal_sse_frame("data: hello"),
        Some(("message".to_owned(), "hello".to_owned()))
    );
    assert_eq!(
        parse_terminal_sse_frame(
            ": keepalive\r\nevent: output\r\ndata: first\r\ndata: second\r\nid: ignored"
        ),
        Some(("output".to_owned(), "first\nsecond".to_owned()))
    );
    assert_eq!(
        find_sse_frame_delimiter(b"event: output\r\rrest"),
        Some(("event: output".len(), 2))
    );
}

#[test]
fn remote_terminal_stream_error_frames_are_normalized_to_bad_gateway() {
    let (tx, _rx) = tokio::sync::mpsc::channel(TERMINAL_STREAM_EVENT_QUEUE_CAPACITY);
    let mut forward_state = RemoteTerminalForwardState::new();

    let err = match handle_remote_terminal_sse_frame(
        r#"event: error
data: {"error":"backend unavailable","status":503}"#,
        &tx,
        &mut forward_state,
    ) {
        Ok(_) => panic!("remote embedded errors should not propagate remote-selected statuses"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::BAD_GATEWAY);
    assert!(
        err.message.contains("remote terminal stream error (503)"),
        "unexpected normalized error message: {}",
        err.message
    );
}

#[test]
fn remote_terminal_stream_error_frames_preserve_429_for_remote_annotation() {
    let (tx, _rx) = tokio::sync::mpsc::channel(TERMINAL_STREAM_EVENT_QUEUE_CAPACITY);
    let mut forward_state = RemoteTerminalForwardState::new();

    let err = match handle_remote_terminal_sse_frame(
        r#"event: error
data: {"error":"too many local terminal commands are already running; limit is 4","status":429}"#,
        &tx,
        &mut forward_state,
    ) {
        Ok(_) => panic!("remote embedded 429 should surface as a throttling error"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        err.message.contains("remote terminal stream error (429)"),
        "unexpected throttling error message: {}",
        err.message
    );

    let annotated = annotate_remote_terminal_429(err, "SSH Terminal Limit");
    assert_eq!(annotated.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        annotated
            .message
            .starts_with("remote SSH Terminal Limit: remote terminal stream error (429)"),
        "remote 429 should include the remote display-name prefix: {}",
        annotated.message
    );
}

#[test]
fn remote_terminal_stream_output_is_capped_before_forwarding() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(TERMINAL_STREAM_EVENT_QUEUE_CAPACITY);
    let mut forward_state = RemoteTerminalForwardState::new();
    let oversized_output = "x".repeat(TERMINAL_OUTPUT_MAX_BYTES + 128);
    let frame = format!(
        "event: output\ndata: {}\n",
        json!({ "stream": "stdout", "text": oversized_output })
    );

    assert!(
        handle_remote_terminal_sse_frame(&frame, &tx, &mut forward_state)
            .unwrap()
            .is_none()
    );

    let event = rx.try_recv().expect("capped output should be forwarded");
    match event {
        TerminalCommandStreamEvent::Output { stream, text } => {
            assert_eq!(stream, TerminalOutputStream::Stdout);
            assert_eq!(text.len(), TERMINAL_OUTPUT_MAX_BYTES);
        }
        _ => panic!("expected output event"),
    }
    assert!(forward_state.output_truncated);

    let complete = format!(
        "event: complete\ndata: {}\n",
        serde_json::to_string(&TerminalCommandResponse {
            command: "remote".to_owned(),
            duration_ms: 1,
            exit_code: Some(0),
            output_truncated: false,
            shell: "sh".to_owned(),
            stderr: String::new(),
            stdout: "ok".to_owned(),
            success: true,
            timed_out: false,
            workdir: "/repo".to_owned(),
        })
        .unwrap()
    );
    let response = handle_remote_terminal_sse_frame(&complete, &tx, &mut forward_state)
        .unwrap()
        .expect("complete event should finish remote stream");
    assert!(response.output_truncated);
}

#[test]
fn cap_terminal_response_output_uses_independent_stdout_and_stderr_budgets() {
    let stdout = "o".repeat(500 * 1024);
    let stderr = "e".repeat(100 * 1024);
    let mut response = TerminalCommandResponse {
        command: "remote".to_owned(),
        duration_ms: 1,
        exit_code: Some(0),
        output_truncated: false,
        shell: "sh".to_owned(),
        stderr: stderr.clone(),
        stdout: stdout.clone(),
        success: true,
        timed_out: false,
        workdir: "/repo".to_owned(),
    };

    assert!(!cap_terminal_response_output(&mut response));
    assert_eq!(response.stdout, stdout);
    assert_eq!(response.stderr, stderr);
}

#[test]
fn cap_terminal_response_output_truncates_both_streams_above_their_budgets() {
    // Regression guard for the "both streams exceed budget" truncation case.
    // Each stream should be truncated independently to
    // `TERMINAL_OUTPUT_MAX_BYTES`, and the function must report truncation.
    let stdout_fill = "a".repeat(TERMINAL_OUTPUT_MAX_BYTES * 2);
    let stderr_fill = "b".repeat(TERMINAL_OUTPUT_MAX_BYTES * 2);
    let mut response = TerminalCommandResponse {
        command: "remote".to_owned(),
        duration_ms: 1,
        exit_code: Some(0),
        output_truncated: false,
        shell: "sh".to_owned(),
        stderr: stderr_fill,
        stdout: stdout_fill,
        success: true,
        timed_out: false,
        workdir: "/repo".to_owned(),
    };

    assert!(cap_terminal_response_output(&mut response));
    assert_eq!(response.stdout.len(), TERMINAL_OUTPUT_MAX_BYTES);
    assert_eq!(response.stderr.len(), TERMINAL_OUTPUT_MAX_BYTES);
    assert!(response.stdout.bytes().all(|byte| byte == b'a'));
    assert!(response.stderr.bytes().all(|byte| byte == b'b'));
}

#[test]
fn remote_terminal_stream_forwards_stderr_even_when_stdout_filled_its_budget() {
    // Regression for the shared-counter bug: before per-stream tracking,
    // `RemoteTerminalForwardState.forwarded_output_bytes` was a single
    // counter shared across stdout + stderr, so a remote that sent a full
    // `TERMINAL_OUTPUT_MAX_BYTES` stdout chunk followed by a small stderr
    // chunk saw the live stderr event silently dropped and the final
    // completion response marked `output_truncated = true` even though the
    // per-stream `cap_terminal_response_output` would not have truncated
    // either stream.
    let (tx, mut rx) = tokio::sync::mpsc::channel(TERMINAL_STREAM_EVENT_QUEUE_CAPACITY);
    let mut forward_state = RemoteTerminalForwardState::new();

    let stdout_fill = "a".repeat(TERMINAL_OUTPUT_MAX_BYTES);
    let stdout_frame = format!(
        "event: output\ndata: {}\n",
        json!({ "stream": "stdout", "text": stdout_fill.clone() })
    );
    assert!(
        handle_remote_terminal_sse_frame(&stdout_frame, &tx, &mut forward_state)
            .unwrap()
            .is_none()
    );
    let stdout_event = rx.try_recv().expect("stdout event should be forwarded");
    match stdout_event {
        TerminalCommandStreamEvent::Output { stream, text } => {
            assert_eq!(stream, TerminalOutputStream::Stdout);
            assert_eq!(text.len(), TERMINAL_OUTPUT_MAX_BYTES);
        }
        _ => panic!("expected stdout output event"),
    }
    assert!(
        !forward_state.output_truncated,
        "stdout at exactly the per-stream budget must not trip truncation"
    );

    let stderr_frame = format!(
        "event: output\ndata: {}\n",
        json!({ "stream": "stderr", "text": "tiny stderr payload" })
    );
    assert!(
        handle_remote_terminal_sse_frame(&stderr_frame, &tx, &mut forward_state)
            .unwrap()
            .is_none()
    );
    let stderr_event = rx
        .try_recv()
        .expect("stderr event must be forwarded even after stdout has consumed its own budget");
    match stderr_event {
        TerminalCommandStreamEvent::Output { stream, text } => {
            assert_eq!(stream, TerminalOutputStream::Stderr);
            assert_eq!(text, "tiny stderr payload");
        }
        _ => panic!("expected stderr output event"),
    }
    assert!(
        !forward_state.output_truncated,
        "stderr within its per-stream budget must not trip truncation after a full stdout"
    );

    let complete_frame = format!(
        "event: complete\ndata: {}\n",
        serde_json::to_string(&TerminalCommandResponse {
            command: "remote".to_owned(),
            duration_ms: 1,
            exit_code: Some(0),
            output_truncated: false,
            shell: "sh".to_owned(),
            stderr: "tiny stderr payload".to_owned(),
            stdout: stdout_fill,
            success: true,
            timed_out: false,
            workdir: "/repo".to_owned(),
        })
        .unwrap()
    );
    let response = handle_remote_terminal_sse_frame(&complete_frame, &tx, &mut forward_state)
        .unwrap()
        .expect("complete event should finish remote stream");
    assert!(
        !response.output_truncated,
        "completion response must not be marked truncated when both streams fit \
         their per-stream budgets"
    );
}

#[test]
fn forward_remote_terminal_stream_reader_accepts_max_sized_completion_frame() {
    // Regression for the rejected-valid-completion-frame bug. A remote that
    // legitimately returns `TERMINAL_OUTPUT_MAX_BYTES` of newline-heavy
    // stdout serializes the completion frame to ~1 MiB + envelope. JSON
    // expands each `\n` byte to the two-char escape `\\n`, so the payload
    // alone weighs roughly `2 * TERMINAL_OUTPUT_MAX_BYTES` — already above
    // the pre-fix proxy cap of `TERMINAL_OUTPUT_MAX_BYTES * 2 = 1 MiB`.
    // Before the cap was raised the forwarder returned
    // `remote terminal stream frame exceeded the allowed size` instead of
    // delivering the command result. The new cap
    // (`TERMINAL_OUTPUT_MAX_BYTES * 16`) must let this frame through while
    // still enforcing a bounded upper limit.
    let (tx, mut rx) = tokio::sync::mpsc::channel(TERMINAL_STREAM_EVENT_QUEUE_CAPACITY);
    let cancellation = Arc::new(AtomicBool::new(false));

    let stdout_fill = "\n".repeat(TERMINAL_OUTPUT_MAX_BYTES);
    let response = TerminalCommandResponse {
        command: "cat big.log".to_owned(),
        duration_ms: 42,
        exit_code: Some(0),
        output_truncated: false,
        shell: "sh".to_owned(),
        stderr: String::new(),
        stdout: stdout_fill.clone(),
        success: true,
        timed_out: false,
        workdir: "/repo".to_owned(),
    };
    let payload = serde_json::to_string(&response).expect("response serializes");
    let frame = format!("event: complete\ndata: {payload}\n\n");

    // Regression: the frame must overflow the pre-fix cap (double the raw
    // output limit) but still fit within the new cap (16× the raw limit).
    let old_cap = TERMINAL_OUTPUT_MAX_BYTES * 2;
    assert!(
        frame.len() > old_cap,
        "expected newline-heavy completion frame to exceed the pre-fix cap \
         (frame = {} bytes, old cap = {old_cap} bytes)",
        frame.len()
    );
    assert!(
        frame.len() <= TERMINAL_REMOTE_SSE_PENDING_MAX_BYTES,
        "expected newline-heavy completion frame to fit within the post-fix cap \
         (frame = {} bytes, new cap = {} bytes)",
        frame.len(),
        TERMINAL_REMOTE_SSE_PENDING_MAX_BYTES
    );

    let mut reader = std::io::Cursor::new(frame.into_bytes());
    let forwarded = forward_remote_terminal_stream_reader(&mut reader, &tx, &cancellation)
        .expect("max-sized completion frame should be accepted");

    assert_eq!(forwarded.command, "cat big.log");
    // The proxy's post-decode `cap_terminal_response_output` leaves stdout
    // at its incoming size when it is already at the budget, so the full
    // 512 KiB of newlines round-trip through the forwarder unchanged.
    assert_eq!(forwarded.stdout.len(), TERMINAL_OUTPUT_MAX_BYTES);
    assert_eq!(forwarded.stdout, stdout_fill);
    assert!(forwarded.stderr.is_empty());
    // No intermediate output events were emitted — the frame is a single
    // completion event, so the channel stays empty.
    assert!(rx.try_recv().is_err());
}

#[test]
fn interruptible_remote_stream_reader_drains_partial_chunks_across_small_reads() {
    // Regression guard for `read_buffered`'s partial-chunk drain path.
    // Production callers in the SSE forwarder read with an 8 KiB scratch
    // that always equals or exceeds the `scratch` size used by
    // `read_remote_stream_response`, so a buffered chunk is always consumed
    // in one `Read::read` call. A future caller (or a test-time change to
    // the worker's scratch size) that uses a smaller destination buffer
    // would exercise the partial-drain path for the first time, and a
    // regression in `self.offset` tracking or `self.buffered.clear()`
    // would drop or duplicate bytes silently. Construct the reader
    // directly via `::new`, pre-load a single 10-byte chunk, and assert
    // that repeated 4-byte reads drain exactly the original chunk in the
    // correct order.
    let (chunk_tx, chunk_rx) = std::sync::mpsc::sync_channel::<io::Result<Vec<u8>>>(1);
    chunk_tx
        .send(Ok(b"abcdefghij".to_vec()))
        .expect("chunk send must succeed");
    drop(chunk_tx);
    let cancellation = Arc::new(AtomicBool::new(false));
    let mut reader = InterruptibleRemoteStreamReader::new(chunk_rx, cancellation);

    let mut buf = [0u8; 4];
    let mut collected = Vec::new();
    loop {
        match std::io::Read::read(&mut reader, &mut buf).expect("partial read must succeed") {
            0 => break,
            n => collected.extend_from_slice(&buf[..n]),
        }
    }
    assert_eq!(collected, b"abcdefghij");
}

#[test]
fn interruptible_remote_stream_reader_observes_cancellation_between_recv_timeouts() {
    // Adapter-level contract test: when the internal chunk channel is idle,
    // the reader's `recv_timeout` loop periodically wakes and re-checks the
    // cancellation flag. This is in isolation from the spawned worker —
    // the test wires up `::new` directly with an empty channel so no
    // `read_remote_stream_response` runs at all. It therefore only covers
    // the adapter's recv-timeout poll, NOT the production path where a
    // stalled remote parks a spawned worker inside `source.read()`. The
    // `interruptible_remote_stream_reader_spawn_unblocks_on_cancellation`
    // test below exercises the worker-thread side.
    let (chunk_tx, chunk_rx) = std::sync::mpsc::sync_channel::<io::Result<Vec<u8>>>(1);
    let cancellation = Arc::new(AtomicBool::new(false));
    let mut reader = InterruptibleRemoteStreamReader::new(chunk_rx, cancellation.clone());
    let (tx, _rx) = tokio::sync::mpsc::channel(TERMINAL_STREAM_EVENT_QUEUE_CAPACITY);
    let cancellation_for_thread = cancellation.clone();
    let cancel_thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(25));
        cancellation_for_thread.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(25));
        drop(chunk_tx);
    });

    let err = forward_remote_terminal_stream_reader(&mut reader, &tx, &cancellation)
        .err()
        .expect("idle recv_timeout must observe cancellation");

    cancel_thread.join().unwrap();
    assert_eq!(err.status, StatusCode::BAD_GATEWAY);
    assert_eq!(err.message, "terminal stream client disconnected");
}

#[test]
fn interruptible_remote_stream_reader_spawn_unblocks_on_cancellation() {
    // Regression covering the real production spawn path: a stalled remote
    // that never emits bytes must still let the adapter-level reader return
    // `terminal stream client disconnected` once the cancellation flag
    // flips. The mock `BlockingSource::read` mirrors a hung reqwest body
    // read by parking inside `read()` until a release flag flips, so the
    // worker thread spawned by `InterruptibleRemoteStreamReader::spawn`
    // goes through `read_remote_stream_response` exactly as it would for a
    // real stalled remote. The pre-existing
    // `interruptible_remote_stream_reader_observes_cancellation_between_recv_timeouts`
    // sibling exercises only the channel adapter via `::new`, so without
    // this test a future edit could silently break the production spawn
    // path without failing any test.
    struct BlockingSource {
        read_called: Arc<AtomicBool>,
        release: Arc<AtomicBool>,
    }

    impl std::io::Read for BlockingSource {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            self.read_called.store(true, Ordering::SeqCst);
            loop {
                if self.release.load(Ordering::SeqCst) {
                    return Ok(0);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    let read_called = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let cancellation = Arc::new(AtomicBool::new(false));
    let source = BlockingSource {
        read_called: read_called.clone(),
        release: release.clone(),
    };

    let mut reader = InterruptibleRemoteStreamReader::spawn(source, cancellation.clone());
    let (tx, _rx) = tokio::sync::mpsc::channel(TERMINAL_STREAM_EVENT_QUEUE_CAPACITY);

    // Wait until the worker thread is actually parked inside `read()`.
    let start = std::time::Instant::now();
    while !read_called.load(Ordering::SeqCst) {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "worker thread never entered the mock BlockingSource::read"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    let cancel_thread = std::thread::spawn({
        let cancellation = cancellation.clone();
        move || {
            std::thread::sleep(Duration::from_millis(25));
            cancellation.store(true, Ordering::SeqCst);
        }
    });

    let err = forward_remote_terminal_stream_reader(&mut reader, &tx, &cancellation)
        .err()
        .expect("spawn-path forwarding must surface the disconnect error");

    cancel_thread.join().unwrap();
    // Let the worker thread drain its parked read so this test doesn't
    // leave a detached thread attached to a stale socket mock.
    release.store(true, Ordering::SeqCst);

    assert_eq!(err.status, StatusCode::BAD_GATEWAY);
    assert_eq!(err.message, "terminal stream client disconnected");
    assert!(
        read_called.load(Ordering::SeqCst),
        "the spawn path must actually drive read_remote_stream_response"
    );
}

#[test]
fn forward_remote_terminal_stream_reader_rejects_frame_past_new_cap() {
    // The cap still bounds memory: if a malicious or buggy remote sends an
    // unterminated SSE frame larger than the new cap, the forwarder must
    // surface the existing "exceeded the allowed size" error rather than
    // buffer indefinitely. Exercise the rejection path with a small
    // test-only cap via `forward_remote_terminal_stream_reader_capped` so
    // we do not have to push megabytes of bytes through the reader in debug
    // mode (the SSE delimiter scan is O(n²) on the growing pending buffer
    // because it rescans from zero after every 8 KiB read).
    const TEST_PENDING_CAP: usize = 64 * 1024;
    let (tx, _rx) = tokio::sync::mpsc::channel(TERMINAL_STREAM_EVENT_QUEUE_CAPACITY);
    let cancellation = Arc::new(AtomicBool::new(false));
    let mut reader = std::io::Cursor::new(vec![b'x'; TEST_PENDING_CAP + 1]);

    let err = forward_remote_terminal_stream_reader_capped(
        &mut reader,
        &tx,
        &cancellation,
        TEST_PENDING_CAP,
    )
    .err()
    .expect("frame exceeding the cap should be rejected");
    assert_eq!(err.status, StatusCode::BAD_GATEWAY);
    assert_eq!(
        err.message, "remote terminal stream frame exceeded the allowed size",
        "unexpected error for oversized frame: {}",
        err.message
    );
}

#[test]
fn forward_remote_terminal_stream_reader_uses_production_cap_at_least_as_large_as_max_frame() {
    // Cheap sanity check pinning the constant so a future edit cannot
    // accidentally shrink the cap below the worst-case newline-heavy
    // completion frame that the acceptance test above exercises.
    let minimum_required = 2 * TERMINAL_OUTPUT_MAX_BYTES + 4 * 1024;
    assert!(
        TERMINAL_REMOTE_SSE_PENDING_MAX_BYTES >= minimum_required,
        "pending cap regressed: {} < {}",
        TERMINAL_REMOTE_SSE_PENDING_MAX_BYTES,
        minimum_required
    );
}

#[test]
fn annotate_remote_terminal_429_prefixes_only_throttled_remote_errors() {
    let throttled = annotate_remote_terminal_429(
        ApiError::from_status(
            StatusCode::TOO_MANY_REQUESTS,
            "too many local terminal commands are already running; limit is 4",
        ),
        "SSH Terminal Limit",
    );
    assert_eq!(throttled.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        throttled.message,
        "remote SSH Terminal Limit: too many local terminal commands are already running; limit is 4"
    );

    let server_error = annotate_remote_terminal_429(
        ApiError::from_status(StatusCode::INTERNAL_SERVER_ERROR, "remote server exploded"),
        "SSH Terminal Limit",
    );
    assert_eq!(server_error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(server_error.message, "remote server exploded");
}

#[tokio::test]
async fn terminal_run_route_proxies_valid_remote_multibyte_commands() {
    let captured_body = Arc::new(Mutex::new(None::<String>));
    let captured_for_server = captured_body.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let command = format!("#{}", "\u{00e9}".repeat(TERMINAL_COMMAND_MAX_CHARS - 1));
    let remote_response = serde_json::to_string(&TerminalCommandResponse {
        command: command.clone(),
        duration_ms: 12,
        exit_code: Some(0),
        output_truncated: false,
        shell: "sh".to_owned(),
        stderr: String::new(),
        stdout: "ok\n".to_owned(),
        success: true,
        timed_out: false,
        workdir: "/remote/repo".to_owned(),
    })
    .expect("terminal response should encode");
    let server = std::thread::spawn(move || {
        // Loop until the terminal run request is captured rather than
        // hard-coding the number of proxy round-trips. A future change that
        // adds a capability probe, a binding step, or a retry would
        // otherwise produce a confusing dual failure (server thread
        // panicking on the accept deadline AND the proxy's next request
        // hitting a closed listener). This loop tolerates any number of
        // pre-run requests and terminates as soon as the terminal/run
        // request has been served.
        loop {
            let mut stream = accept_test_connection(&listener, "test listener");
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                let bytes_read = stream.read(&mut chunk).expect("request should read");
                assert!(bytes_read > 0, "request closed before headers completed");
                buffer.extend_from_slice(&chunk[..bytes_read]);
                if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end;
                }
            };
            let headers = String::from_utf8_lossy(&buffer[..header_end]);
            let request_line = headers
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then_some(value.trim())
                        .and_then(|value| value.parse::<usize>().ok())
                })
                .unwrap_or(0);
            let body_start = header_end + 4;
            while buffer.len() < body_start + content_length {
                let bytes_read = stream.read(&mut chunk).expect("request body should read");
                if bytes_read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..bytes_read]);
            }
            let body = String::from_utf8_lossy(&buffer[body_start..body_start + content_length])
                .to_string();

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 11\r\n\r\n{\"ok\":true}",
                    )
                    .expect("health response should write");
                continue;
            }

            if request_line.starts_with("POST /api/terminal/run ") {
                *captured_for_server.lock().expect("capture mutex poisoned") = Some(body);
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            remote_response.len(),
                            remote_response
                        )
                        .as_bytes(),
                    )
                    .expect("terminal response should write");
                break;
            }

            panic!("unexpected request: {request_line}");
        }
    });

    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Terminal",
        "remote-project-1",
    );
    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(true),
                event_bridge_shutdown: AtomicBool::new(false),
                supports_inline_orchestrator_templates: Mutex::new(None),
            }),
        );
    let app = app_router(state);

    let (status, response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": command,
                    "projectId": project_id,
                    "workdir": " /remote/repo ",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        response["command"].as_str().unwrap().chars().count(),
        TERMINAL_COMMAND_MAX_CHARS
    );
    let captured: Value = serde_json::from_str(
        captured_body
            .lock()
            .expect("capture mutex poisoned")
            .as_ref()
            .expect("remote request should be captured"),
    )
    .expect("remote request body should decode");
    assert_eq!(
        captured["workdir"],
        Value::String("/remote/repo".to_owned())
    );
    assert_eq!(
        captured["projectId"],
        Value::String("remote-project-1".to_owned())
    );
    assert_eq!(
        captured["command"].as_str().unwrap().chars().count(),
        TERMINAL_COMMAND_MAX_CHARS
    );

    join_test_server(server);
}

