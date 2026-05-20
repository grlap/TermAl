//! Remote (SSH-proxied) backend tests.
//!
//! TermAl's remote feature lets a user register a `RemoteConfig` pointing at
//! another machine and then SSH-proxy into a remote TermAl backend running
//! there. The local UI still talks to a single local origin; the local backend
//! forwards HTTP calls and bridges SSE events over an SSH-forwarded port.
//!
//! Remote state is mirrored into local state as a proxy copy: remote projects,
//! sessions, and orchestrator instances are projected onto local IDs (remote
//! uses its own project_id namespace, so every sync "localizes" remote IDs to
//! local ones before applying). Updates arrive either as SSE `state` events
//! carrying a full snapshot or `delta` events carrying an incremental change.
//!
//! If the SSE stream drops, the event bridge reconnects with backoff. Across
//! reconnects, applied-revision tracking (plus fallback-resync tracking for
//! SSE-fallback full resyncs) prevents replaying the same revision. Creating,
//! stopping, and forking orchestrators flows remote -> local-id-translation
//! -> UI so remote instances show up locally.
//!
//! Security matters here: a remote backend might be misbehaving or
//! compromised, so error bodies are sanitized and size-capped before logging,
//! terminal stream output is capped before forwarding, 429s are annotated to
//! surface remote throttling, and SSE framing is preserved (with JSON
//! fallback for older remotes). Central production helpers in `src/remote.rs`:
//! `sync_remote_state_inner`, `apply_remote_state_snapshot`,
//! `apply_remote_delta_event_locked`, `decode_remote_json`,
//! `forward_remote_terminal_stream_reader`, `cap_terminal_response_output`.

use super::*;

// Pins that update_app_settings round-trips a remote config list through
// the preferences and the persistence layer with the SSH transport and
// its host/port/user preserved.
// Guards against silently dropping or corrupting remotes on save/load.
#[test]
fn persists_remote_settings() {
    let state = test_app_state();

    let updated = state
        .update_app_settings(UpdateAppSettingsRequest {
            default_codex_model: None,
            default_claude_model: None,
            default_cursor_model: None,
            default_gemini_model: None,
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

// Pins that remote ids containing path-unsafe characters are rejected
// with a 400 before anything is persisted.
// Guards against remote ids being used as filesystem/route components
// and against shell-metacharacter injection via the id.
#[test]
fn rejects_remote_settings_with_unsafe_remote_id() {
    let state = test_app_state();

    let error = match state.update_app_settings(UpdateAppSettingsRequest {
        default_codex_model: None,
        default_claude_model: None,
        default_cursor_model: None,
        default_gemini_model: None,
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

// Pins that an SSH host starting with `-` (i.e. something that parses as
// an SSH option like `-oProxyCommand=...`) is rejected with a 400.
// Guards against command-injection through hostnames that would otherwise
// be expanded into the ssh argv and executed.
#[test]
fn rejects_remote_settings_with_invalid_ssh_host() {
    let state = test_app_state();

    let error = match state.update_app_settings(UpdateAppSettingsRequest {
        default_codex_model: None,
        default_claude_model: None,
        default_cursor_model: None,
        default_gemini_model: None,
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

// Pins that an SSH user containing `@` (which would mangle the final
// user@host target) is rejected with a 400.
// Guards against remote-user fields that could redirect the SSH target
// or otherwise break the argv assembled for ssh.
#[test]
fn rejects_remote_settings_with_invalid_ssh_user() {
    let state = test_app_state();

    let error = match state.update_app_settings(UpdateAppSettingsRequest {
        default_codex_model: None,
        default_claude_model: None,
        default_cursor_model: None,
        default_gemini_model: None,
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

// Pins the exact user-facing error text produced when a remote cannot
// be reached, referencing only "SSH" and the remote name.
// Guards against leaking transport internals (exit codes, stderr, ports)
// into UI error strings.
#[test]
fn remote_connection_issue_message_hides_transport_details() {
    assert_eq!(
        remote_connection_issue_message("SSH Lab"),
        "Could not connect to remote \"SSH Lab\" over SSH. Check the host, network, and SSH settings, then try again."
    );
}

// Pins the exact user-facing error text when the local ssh binary
// cannot be spawned, mentioning OpenSSH/PATH without raw OS errors.
// Guards against the UI surfacing errno/spawn details instead of a
// sanitized hint.
#[test]
fn local_ssh_start_issue_message_hides_transport_details() {
    assert_eq!(
        local_ssh_start_issue_message("SSH Lab"),
        "Could not start the local SSH client for remote \"SSH Lab\". Verify OpenSSH is installed and available on PATH, then try again."
    );
}

// Pins the spawn-error mapping used by `RemoteConnection::start_process`.
// Guards against local OpenSSH spawn failures losing their structured
// recoverability tag while still showing the sanitized UI message.
#[test]
fn local_ssh_start_error_tags_spawn_failure_as_remote_connection_unavailable() {
    let error = local_ssh_start_error(
        "SSH Lab",
        std::io::Error::new(std::io::ErrorKind::NotFound, "ssh missing"),
    );

    assert_eq!(error.status, StatusCode::BAD_GATEWAY);
    assert_eq!(
        error.message,
        "Could not start the local SSH client for remote \"SSH Lab\". Verify OpenSSH is installed and available on PATH, then try again."
    );
    assert!(matches!(
        error.kind,
        Some(ApiErrorKind::RemoteConnectionUnavailable)
    ));
}

// Pins that the assembled ssh argv places a literal `--` separator
// before `alice@example.com` and the remote `termal server` command.
// Guards against user/host strings being mistakenly interpreted as ssh
// options if the `--` ever disappears.
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

// Pins that reconciling a remote out of the config list stops its SSE
// event-bridge worker, removes it from the registry, and that the
// `event_bridge_started` atomic flips back to false after shutdown.
// Guards against orphaned worker threads or a stuck "already started"
// guard that would block a later start_event_bridge.
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

// Pins that starting the event bridge clears per-remote SSE fallback
// resync watermarks so that a restarted remote whose revision counter
// has decreased (e.g. reset) can still produce a recovery resync.
// Guards against a stale fallback watermark silently suppressing the
// only resync path after a reconnect.
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

// Pins that starting the event bridge also clears the applied-revision
// watermark on StateInner, so deltas at or below the old revision from
// a restarted remote are no longer skipped as duplicates.
// Guards against permanently-wedged dedupe state after a remote restart
// causes its revision sequence to reset.
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

// Pins that apply_remote_state_snapshot rewrites remote orchestrator
// and session ids to fresh local ids, creates proxy session records for
// every remote session referenced by the orchestrator, and rewrites
// pending transitions and the template snapshot project_id to point at
// local ids.
// Guards against remote ids leaking into local state and against
// dangling pending transitions after a snapshot apply.
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

    let snapshot = state.full_snapshot();
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

// Pins that after delete_project detaches a remote-mirrored orchestrator
// (setting its project_id to ""), a fresh remote snapshot resync must
// not revive Some("") via the existing-local-project-id fallback and
// must not write "" onto template_snapshot.project_id; the failed
// localization rolls back cleanly to the detached state.
// Guards against a latent fallback in localize_remote_orchestrator_instance
// that could durably persist empty-string project ids into state.
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

// Pins that a SUCCESSFUL broad snapshot sync queues a delete tombstone for
// any local proxy session that dropped out of the remote snapshot. Catches a
// regression from `StateInner::retain_sessions` to a plain `sessions.retain(...)`
// without the `record_removed_session` side effect — memory state would look
// correct but SQLite would retain orphan rows forever because the persist thread
// drives its DELETEs off `removed_session_ids`.
#[test]
fn successful_remote_snapshot_sync_queues_tombstones_for_dropped_proxy_sessions() {
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

    // First snapshot: establish remote-session-1 + remote-session-2.
    state
        .apply_remote_state_snapshot(
            &remote.id,
            sample_remote_orchestrator_state(
                "remote-project-1",
                "/remote/repo",
                1,
                OrchestratorInstanceStatus::Stopped,
            ),
        )
        .expect("initial remote snapshot should apply");

    let dropped_local_session_id = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert!(
            inner.removed_session_ids.is_empty(),
            "setup should start without queued tombstones"
        );
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-2")
            .expect("remote-session-2 should be mirrored after the first snapshot");
        inner.sessions[index].session.id.clone()
    };

    // Second snapshot: drop remote-session-2 AND drop the orchestrator so
    // orchestrator localization does not reference the missing session and
    // cannot fail. The sync runs cleanly; `retain_sessions` should queue a
    // tombstone for remote-session-2's local proxy.
    let mut trimmed_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Stopped,
    );
    trimmed_state
        .sessions
        .retain(|session| session.id != "remote-session-2");
    trimmed_state.orchestrators.clear();

    state
        .apply_remote_state_snapshot(&remote.id, trimmed_state)
        .expect("clean snapshot should apply without error");

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .find_remote_session_index(&remote.id, "remote-session-2")
            .is_none(),
        "dropped remote session should be removed from the mirror"
    );
    assert!(
        inner
            .removed_session_ids
            .contains(&dropped_local_session_id),
        "retain_sessions must queue a DELETE tombstone for the dropped proxy \
         session so the persist thread's targeted DELETE WHERE id = ? path \
         fires on the next tick",
    );

    drop(inner);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that the remote snapshot rollback restores the tombstone accumulator
// alongside the session list. A failed orchestrator localization happens after
// broad sync has already retained remote sessions and queued DELETE tombstones
// for any local proxy sessions missing from the snapshot.
//
// The assertion is tightened to exact deep equality of the pre-call state
// (sessions, orchestrator_instances, next_session_number, removed_session_ids,
// and the length of `sessions`) so a future refactor that silently stops
// queueing tombstones in flight — or that partially rolls back state — does
// not slip past this test by matching only one field's expected value.
#[test]
fn failed_remote_snapshot_sync_restores_session_tombstones() {
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
        .expect("initial remote snapshot should apply");

    // Seed a dummy tombstone before the failing call. If rollback correctly
    // restores `removed_session_ids` from its pre-call snapshot, this entry
    // must still be present after the sync aborts. If rollback were
    // incomplete (e.g. someone dropped `removed_session_ids` from the
    // capture struct), the tombstone queued DURING the failed sync for
    // `remote-session-2` would remain in the vec alongside this dummy —
    // and the equality assertion below would fail loudly.
    const DUMMY_TOMBSTONE: &str = "pre-sync-ghost-session";
    let (
        removed_local_session_id,
        pre_sessions,
        pre_orchestrator_instances,
        pre_next_session_number,
        pre_removed_session_ids,
    ) = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        assert!(
            inner.removed_session_ids.is_empty(),
            "test setup should start without queued tombstones"
        );
        inner.record_removed_session(DUMMY_TOMBSTONE.to_owned());
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-2")
            .expect("remote-session-2 should be mirrored after the first snapshot");
        let local_id = inner.sessions[index].session.id.clone();
        (
            local_id,
            inner.sessions.clone(),
            inner.orchestrator_instances.clone(),
            inner.next_session_number,
            inner.removed_session_ids.clone(),
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
        .retain(|session| session.id != "remote-session-2");

    state
        .apply_remote_state_snapshot(&remote.id, invalid_state)
        .expect("orchestrator sync errors are logged and swallowed");

    let inner = state.inner.lock().expect("state mutex poisoned");
    // Session the snapshot tried to drop must be restored.
    assert!(
        inner
            .find_remote_session_index(&remote.id, "remote-session-2")
            .is_some(),
        "rollback should restore the removed remote proxy session"
    );
    // No post-rollback tombstone for the restored session.
    assert!(
        !inner
            .removed_session_ids
            .contains(&removed_local_session_id),
        "rollback should not leave a stale tombstone for the restored session"
    );
    // Pre-existing dummy tombstone must survive.
    assert!(
        inner
            .removed_session_ids
            .iter()
            .any(|id| id == DUMMY_TOMBSTONE),
        "rollback should preserve the pre-sync tombstone accumulator \
         (including the dummy entry seeded before the failed call)"
    );
    // Full deep-equality check: every mutation performed during the failed
    // sync must be reversed. This is stronger than field-by-field spot
    // checks and will catch partial-rollback regressions (e.g. if someone
    // adds a mutation to `sync_remote_state_inner` and forgets to capture
    // it in `RemoteSyncRollback`).
    assert_eq!(
        inner.next_session_number, pre_next_session_number,
        "rollback should restore next_session_number"
    );
    assert_eq!(
        inner.removed_session_ids, pre_removed_session_ids,
        "rollback should restore the full removed_session_ids vec"
    );
    assert_eq!(
        inner.sessions.len(),
        pre_sessions.len(),
        "rollback should restore the session list length"
    );
    // Previously we compared only the list of session IDs, which
    // catches membership and ordering but silently allows
    // rollback to leave mutated fields behind (a partial restore
    // that touches `session.name`, `session.status`,
    // `session.preview`, `remote_id`, `remote_session_id`, or the
    // codex-prompt settings would still match id-by-id).
    //
    // Compare the full serialized shape of every session: the
    // `Session` struct (id, name, emoji, workdir, project_id,
    // model, approval policy, reasoning effort, sandbox mode,
    // cursor/claude/gemini settings, status, preview, messages,
    // pending prompts) plus the two remote-proxy metadata fields
    // on `SessionRecord`. `Session` implements `Serialize` but
    // not `PartialEq`, and `SessionRecord` contains runtime
    // handles (`SessionRuntime`) that cannot derive `PartialEq`
    // — serializing to `serde_json::Value` is the cheapest way
    // to get a deep-equality comparison on the durable surface
    // without threading trait bounds through the runtime types.
    fn session_comparable(record: &SessionRecord) -> Value {
        json!({
            "session": serde_json::to_value(&record.session)
                .expect("Session serialization should not fail"),
            "remote_id": record.remote_id.clone(),
            "remote_session_id": record.remote_session_id.clone(),
        })
    }
    let pre_sessions_summary: Vec<Value> = pre_sessions.iter().map(session_comparable).collect();
    let post_sessions_summary: Vec<Value> = inner.sessions.iter().map(session_comparable).collect();
    assert_eq!(
        post_sessions_summary, pre_sessions_summary,
        "rollback should restore full session content (Session fields + remote metadata), not just IDs"
    );
    // `OrchestratorInstance` derives `PartialEq` directly, so
    // the vec comparison asserts every instance field matches:
    // orchestrator id, template id, project id, status, sessions,
    // prompts, settings — the complete payload. A partial
    // rollback that restored the COUNT but not the body (e.g.
    // status drifted from Running to Paused, or a child session
    // id changed) is the exact regression the length-only check
    // let slip.
    assert_eq!(
        inner.orchestrator_instances, pre_orchestrator_instances,
        "rollback should restore full orchestrator instance content, not just the count"
    );

    drop(inner);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that resume_pending_orchestrator_transitions ignores orchestrators
// that belong to a remote, leaving their destination session untouched
// rather than queuing a pending prompt locally.
// Guards against double-execution of orchestrator transitions that the
// remote backend is already driving.
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

// Pins that next_pending_transition_action returns None for an
// orchestrator tagged with a remote_id/remote_orchestrator_id, even when
// the same state with only the remote fields cleared would produce a
// Deliver action.
// Guards against local dispatch driving transitions that the remote
// backend owns.
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

// Pins that detect_deadlocked_consolidate_session_ids still reports the
// cycle on a remote-mirrored orchestrator but mark_deadlocked_orchestrator_instances
// refuses to mutate it, leaving status/error/pending-transitions intact.
// Guards against local deadlock resolution clobbering an orchestrator
// whose deadlock state the remote is authoritative about.
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

// Pins that applying a remote OrchestratorsUpdated delta rewrites
// remote ids to the already-allocated local ids, preserves the local
// orchestrator id from the initial snapshot, and updates status without
// changing identity.
// Guards against duplicating a remote orchestrator or churning its
// local id on every delta.
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

    let initial_snapshot = state.full_snapshot();
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

    let mut remote_delta_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Paused,
    );
    let remote_session_with_transcript = remote_delta_state
        .sessions
        .iter_mut()
        .find(|session| session.id == "remote-session-1")
        .expect("sample remote session should exist");
    remote_session_with_transcript.messages = vec![remote_text_message(
        "remote-message-1",
        "Remote transcript should not be republished in orchestrator deltas.",
    )];
    remote_session_with_transcript.messages_loaded = true;
    remote_session_with_transcript.message_count = 1;
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

    let snapshot = state.full_snapshot();
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
            assert!(
                sessions
                    .iter()
                    .all(|session| { session.messages.is_empty() && !session.messages_loaded })
            );
        }
        _ => panic!("unexpected delta variant"),
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that an OrchestratorsUpdated delta carrying sessions which do
// not yet have proxy records creates those records from the delta's own
// session payload and localizes their project_id.
// Guards against dropped orchestrator deltas when the remote pushes
// session additions out-of-band from any prior snapshot.
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

    let mut remote_delta_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    );
    let remote_session_with_transcript = remote_delta_state
        .sessions
        .iter_mut()
        .find(|session| session.id == "remote-session-1")
        .expect("sample remote session should exist");
    remote_session_with_transcript.messages = vec![remote_text_message(
        "remote-message-1",
        "Remote transcript should not be republished in orchestrator deltas.",
    )];
    remote_session_with_transcript.messages_loaded = true;
    remote_session_with_transcript.message_count = 1;

    let mut delta_receiver = state.subscribe_delta_events();
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

    let delta_payload = delta_receiver
        .try_recv()
        .expect("localized orchestrator delta should be published");
    let delta: DeltaEvent =
        serde_json::from_str(&delta_payload).expect("delta payload should decode");
    match delta {
        DeltaEvent::OrchestratorsUpdated { sessions, .. } => {
            assert!(
                sessions
                    .iter()
                    .all(|session| session.messages.is_empty() && !session.messages_loaded)
            );
            let transcript_summary = sessions
                .iter()
                .find(|session| session.message_count == 1)
                .expect("localized delta should carry transcript count summary");
            assert!(transcript_summary.messages.is_empty());
        }
        _ => panic!("unexpected delta variant"),
    }

    let snapshot = state.full_snapshot();
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
        if remote_session_id == "remote-session-1" {
            assert!(inner.sessions[index].session.messages_loaded);
            assert_eq!(inner.sessions[index].session.messages.len(), 1);
        }
        assert!(localized_session_ids.contains(&inner.sessions[index].session.id));
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_orchestrators_updated_summary_sessions_preserve_unloaded_message_count() {
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

    let mut remote_delta_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    );
    let remote_session_summary = remote_delta_state
        .sessions
        .iter_mut()
        .find(|session| session.id == "remote-session-1")
        .expect("sample remote session should exist");
    make_remote_session_summary_only(remote_session_summary, 2);
    let remote_session_with_transcript = remote_delta_state
        .sessions
        .iter_mut()
        .find(|session| session.id == "remote-session-2")
        .expect("second sample remote session should exist");
    remote_session_with_transcript.messages = vec![remote_text_message(
        "remote-message-2",
        "Full transcript should still republish as a summary.",
    )];
    remote_session_with_transcript.messages_loaded = true;
    remote_session_with_transcript.message_count = 1;
    let mut expected_message_counts = remote_delta_state
        .sessions
        .iter()
        .map(|session| session.message_count)
        .collect::<Vec<_>>();
    expected_message_counts.sort_unstable();

    let mut delta_receiver = state.subscribe_delta_events();
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::OrchestratorsUpdated {
                revision: 1,
                orchestrators: remote_delta_state.orchestrators.clone(),
                sessions: remote_delta_state.sessions.clone(),
            },
        )
        .expect("remote orchestrator summary delta should create missing proxy sessions");

    let payload = delta_receiver
        .try_recv()
        .expect("localized orchestrator delta should be published");
    let delta: DeltaEvent = serde_json::from_str(&payload).expect("delta payload should decode");
    match delta {
        DeltaEvent::OrchestratorsUpdated { sessions, .. } => {
            assert_eq!(sessions.len(), expected_message_counts.len());
            assert!(
                sessions
                    .iter()
                    .all(|session| session.messages.is_empty() && !session.messages_loaded),
                "all republished orchestrator sessions should be metadata-first: {sessions:?}"
            );
            assert!(
                sessions.iter().all(|session| {
                    session.project_id.as_deref() == Some(local_project_id.as_str())
                }),
                "all republished sessions should use localized project ids: {sessions:?}"
            );
            let mut actual_message_counts = sessions
                .iter()
                .map(|session| session.message_count)
                .collect::<Vec<_>>();
            actual_message_counts.sort_unstable();
            assert_eq!(actual_message_counts, expected_message_counts);
            let localized_summary = sessions
                .iter()
                .find(|session| session.message_count == 2)
                .expect("localized delta should preserve remote summary count");
            assert_eq!(
                localized_summary.project_id.as_deref(),
                Some(local_project_id.as_str())
            );
            assert!(!localized_summary.messages_loaded);
            assert!(localized_summary.messages.is_empty());
        }
        _ => panic!("unexpected delta variant"),
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote summary session should be mirrored locally");
    let record = &inner.sessions[index];
    assert_eq!(
        record.session.project_id.as_deref(),
        Some(local_project_id.as_str())
    );
    assert!(!record.session.messages_loaded);
    assert!(record.session.messages.is_empty());
    assert_eq!(record.session.message_count, 2);
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that apply_remote_state_snapshot with a revision older than the
// last applied delta is a no-op: status and revision stay at the newer
// delta's values.
// Guards against a delayed SSE snapshot (retry, reconnect) reverting
// state the newer delta already landed.
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
    let revision_after_delta = state.full_snapshot().revision;

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

    let snapshot = state.full_snapshot();
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

// Pins that when an OrchestratorsUpdated delta references a remote
// session id that localization cannot resolve, the whole delta is
// rolled back: no new proxy sessions, unchanged next_session_number, no
// delta event published, and nothing persisted for this remote.
// Guards against partial state where eager session writes outlive the
// orchestrator that was supposed to own them.
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

    let snapshot = state.full_snapshot();
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

    let persisted_connection =
        rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    let persisted_sessions =
        load_session_records_from_sqlite(&persisted_connection, state.persistence_path.as_path())
            .expect("persisted sessions should load");
    assert_eq!(persisted_sessions.len(), initial_session_count);
    assert!(!persisted_sessions.iter().any(|candidate| {
        candidate.remote_session_id.as_deref() == Some("remote-session-1")
            || candidate.remote_session_id.as_deref() == Some("remote-session-2")
    }));
    let persisted = sqlite_metadata_state_value(state.persistence_path.as_path());
    let persisted_orchestrator_instances = persisted["orchestratorInstances"].as_array();
    assert!(persisted_orchestrator_instances.map_or(true, |instances| {
        !instances
            .iter()
            .any(|instance| instance["remoteId"] == Value::String(remote.id.clone()))
    }));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Seeds a remote proxy record directly through `StateInner`/upsert. Use this
// for protocol tests that do not need to exercise the remote delta apply path.
pub(super) fn seed_remote_proxy_session_via_state_inner_upsert(
    state: &AppState,
    remote: &RemoteConfig,
) -> String {
    let local_project_id = create_test_remote_project(
        state,
        remote,
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
}

pub(super) fn remote_text_message(message_id: &str, text: &str) -> Message {
    Message::Text {
        attachments: Vec::new(),
        id: message_id.to_owned(),
        timestamp: "2026-04-05 10:00:00".to_owned(),
        author: Author::Assistant,
        text: text.to_owned(),
        expanded_text: None,
    }
}

pub(super) fn remote_command_message(message_id: &str, command: &str, output: &str) -> Message {
    Message::Command {
        id: message_id.to_owned(),
        timestamp: "2026-04-05 10:00:00".to_owned(),
        author: Author::Assistant,
        command: command.to_owned(),
        command_language: Some("shell".to_owned()),
        output: output.to_owned(),
        output_language: Some("text".to_owned()),
        status: CommandStatus::Running,
    }
}

pub(super) fn remote_parallel_agents_message(
    message_id: &str,
    agents: Vec<ParallelAgentProgress>,
) -> Message {
    Message::ParallelAgents {
        id: message_id.to_owned(),
        timestamp: "2026-04-05 10:00:00".to_owned(),
        author: Author::Assistant,
        agents,
    }
}

pub(super) fn make_remote_session_summary_only(session: &mut Session, message_count: u32) {
    session.messages.clear();
    session.messages_loaded = false;
    session.message_count = message_count;
}

pub(super) fn spawn_remote_session_response_server(
    response: SessionResponse,
) -> (u16, Arc<Mutex<Vec<String>>>, std::thread::JoinHandle<()>) {
    spawn_remote_session_and_state_response_server(response, None)
}

pub(super) fn spawn_remote_state_response_server(
    response: StateResponse,
) -> (u16, Arc<Mutex<Vec<String>>>, std::thread::JoinHandle<()>) {
    let response_body = serde_json::to_string(&response).expect("state response should encode");
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = requests.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "remote state test listener");
            let request = read_test_http_request(&mut stream);
            requests_for_server
                .lock()
                .expect("requests mutex poisoned")
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

            if request.request_line.starts_with("GET /api/state ") {
                write_test_http_response(
                    &mut stream,
                    StatusCode::OK,
                    "application/json",
                    &response_body,
                );
                continue;
            }

            panic!("unexpected request: {}", request.request_line);
        }
    });

    (port, requests, server)
}

pub(super) fn spawn_remote_session_and_state_response_server(
    response: SessionResponse,
    state_response: Option<StateResponse>,
) -> (u16, Arc<Mutex<Vec<String>>>, std::thread::JoinHandle<()>) {
    let response_body = serde_json::to_string(&response).expect("session response should encode");
    let state_response_body = state_response
        .as_ref()
        .map(|state| serde_json::to_string(state).expect("state response should encode"));
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = requests.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let expected_request_count = 2 + if state_response_body.is_some() { 2 } else { 0 };
    let server = std::thread::spawn(move || {
        for _ in 0..expected_request_count {
            let mut stream = accept_test_connection(&listener, "remote session test listener");
            let request = read_test_http_request(&mut stream);
            requests_for_server
                .lock()
                .expect("requests mutex poisoned")
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
                .starts_with("GET /api/sessions/remote-session-1 ")
            {
                write_test_http_response(
                    &mut stream,
                    StatusCode::OK,
                    "application/json",
                    &response_body,
                );
                continue;
            }

            if request.request_line.starts_with("GET /api/state ") {
                let Some(body) = state_response_body.as_ref() else {
                    panic!("unexpected request: {}", request.request_line);
                };
                write_test_http_response(&mut stream, StatusCode::OK, "application/json", body);
                continue;
            }

            panic!("unexpected request: {}", request.request_line);
        }
    });

    (port, requests, server)
}

// Pins that proxying PUT /api/reviews/{id} to a remote carries the
// review scope and any other filter options in the query string of the
// forwarded request rather than in the body.
// Guards against silently dropping scope (which would let a review
// write target the wrong files) when forwarding to the remote.
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
