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

fn seed_remote_proxy_session_for_delta_test(state: &AppState, remote: &RemoteConfig) -> String {
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

fn remote_text_message(message_id: &str, text: &str) -> Message {
    Message::Text {
        attachments: Vec::new(),
        id: message_id.to_owned(),
        timestamp: "2026-04-05 10:00:00".to_owned(),
        author: Author::Assistant,
        text: text.to_owned(),
        expanded_text: None,
    }
}

fn make_remote_session_summary_only(session: &mut Session, message_count: u32) {
    session.messages.clear();
    session.messages_loaded = false;
    session.message_count = message_count;
}

fn spawn_remote_session_response_server(
    response: SessionResponse,
) -> (u16, Arc<Mutex<Vec<String>>>, std::thread::JoinHandle<()>) {
    spawn_remote_session_and_state_response_server(response, None)
}

fn spawn_remote_session_and_state_response_server(
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

#[test]
fn remote_session_created_delta_republishes_metadata_only_session_summary() {
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

    let mut remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    remote_session.messages = vec![remote_text_message(
        "remote-message-1",
        "Remote transcript should stay out of SessionCreated deltas.",
    )];
    remote_session.messages_loaded = true;
    remote_session.message_count = 1;

    let mut delta_receiver = state.subscribe_delta_events();
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: remote_session.id.clone(),
                session: remote_session,
            },
        )
        .expect("remote session create delta should apply");

    let (local_session_id, stored_message_count) = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("remote session should be mirrored locally");
        let record = &inner.sessions[index];
        assert!(record.session.messages_loaded);
        assert_eq!(record.session.messages.len(), 1);
        (record.session.id.clone(), record.session.messages.len())
    };

    let payload = delta_receiver
        .try_recv()
        .expect("localized sessionCreated delta should be published");
    let delta: DeltaEvent = serde_json::from_str(&payload).expect("delta should decode");
    match delta {
        DeltaEvent::SessionCreated {
            revision,
            session_id,
            session,
        } => {
            assert_eq!(revision, state.snapshot().revision);
            assert_eq!(session_id, local_session_id);
            assert_eq!(session.id, local_session_id);
            assert!(!session.messages_loaded);
            assert!(session.messages.is_empty());
            assert_eq!(session.message_count, stored_message_count as u32);
        }
        _ => panic!("expected sessionCreated delta"),
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_session_created_duplicate_redelivery_does_not_publish_non_advancing_delta() {
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

    let mut remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    remote_session.messages = vec![remote_text_message(
        "remote-message-1",
        "Remote transcript.",
    )];
    remote_session.messages_loaded = true;
    remote_session.message_count = 1;

    let mut delta_receiver = state.subscribe_delta_events();
    let session_created = || DeltaEvent::SessionCreated {
        revision: 2,
        session_id: remote_session.id.clone(),
        session: remote_session.clone(),
    };

    state
        .apply_remote_delta_event(&remote.id, session_created())
        .expect("first remote session create delta should apply");
    delta_receiver
        .try_recv()
        .expect("first sessionCreated delta should publish");

    state
        .apply_remote_delta_event(&remote.id, session_created())
        .expect("duplicate session create delta should be consumed");
    assert!(
        delta_receiver.try_recv().is_err(),
        "duplicate sessionCreated should not publish a non-advancing delta"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&2));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_session_created_summary_preserves_unloaded_message_count() {
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

    let mut remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    make_remote_session_summary_only(&mut remote_session, 2);

    let mut delta_receiver = state.subscribe_delta_events();
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: remote_session.id.clone(),
                session: remote_session,
            },
        )
        .expect("remote summary session create delta should apply");

    let local_session_id = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("remote session should be mirrored locally");
        let record = &inner.sessions[index];
        assert!(!record.session.messages_loaded);
        assert!(record.session.messages.is_empty());
        assert_eq!(record.session.message_count, 2);
        record.session.id.clone()
    };

    let payload = delta_receiver
        .try_recv()
        .expect("localized sessionCreated delta should be published");
    let delta: DeltaEvent = serde_json::from_str(&payload).expect("delta should decode");
    match delta {
        DeltaEvent::SessionCreated {
            session_id,
            session,
            ..
        } => {
            assert_eq!(session_id, local_session_id);
            assert_eq!(session.id, local_session_id);
            assert!(!session.messages_loaded);
            assert!(session.messages.is_empty());
            assert_eq!(session.message_count, 2);
        }
        _ => panic!("expected sessionCreated delta"),
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_summary_count_decrease_marks_cached_transcript_unloaded() {
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

    let mut full_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    full_remote_session.messages = vec![
        remote_text_message("remote-message-1", "First cached message."),
        remote_text_message("remote-message-2", "Second cached message."),
        remote_text_message("remote-message-3", "Third cached message."),
    ];
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = 3;
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: full_remote_session.id.clone(),
                session: full_remote_session.clone(),
            },
        )
        .expect("remote full session create delta should apply");

    let mut summary_session = full_remote_session;
    make_remote_session_summary_only(&mut summary_session, 1);
    let mut delta_receiver = state.subscribe_delta_events();
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 3,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote summary count decrease should apply");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(!record.session.messages_loaded);
    assert_eq!(record.session.messages.len(), 3);
    assert_eq!(record.session.message_count, 1);
    drop(inner);

    let payload = delta_receiver
        .try_recv()
        .expect("localized summary update should publish a delta");
    let delta: DeltaEvent = serde_json::from_str(&payload).expect("delta should decode");
    match delta {
        DeltaEvent::SessionCreated { session, .. } => {
            assert!(!session.messages_loaded);
            assert!(session.messages.is_empty());
            assert_eq!(session.message_count, 1);
        }
        _ => panic!("expected sessionCreated delta"),
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_summary_same_count_with_new_stamp_marks_cached_transcript_unloaded() {
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

    let mut full_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    full_remote_session.messages = vec![remote_text_message(
        "remote-message-1",
        "Cached text before same-count rewrite.",
    )];
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = 1;
    full_remote_session.session_mutation_stamp = Some(10);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: full_remote_session.id.clone(),
                session: full_remote_session.clone(),
            },
        )
        .expect("remote full session create delta should apply");

    let mut summary_session = full_remote_session;
    make_remote_session_summary_only(&mut summary_session, 1);
    summary_session.preview = "Same-count rewrite happened remotely.".to_owned();
    summary_session.session_mutation_stamp = Some(11);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 3,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote same-count summary with new stamp should apply");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(!record.session.messages_loaded);
    assert_eq!(record.session.messages.len(), 1);
    assert_eq!(record.session.message_count, 1);
    assert_eq!(
        record.session.preview,
        "Same-count rewrite happened remotely."
    );
    assert_eq!(record.session.session_mutation_stamp, Some(11));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_summary_same_count_with_same_stamp_preserves_cached_transcript() {
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

    let mut full_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    full_remote_session.messages = vec![remote_text_message(
        "remote-message-1",
        "Cached text still matches remote stamp.",
    )];
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = 1;
    full_remote_session.session_mutation_stamp = Some(10);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: full_remote_session.id.clone(),
                session: full_remote_session.clone(),
            },
        )
        .expect("remote full session create delta should apply");

    let mut summary_session = full_remote_session;
    make_remote_session_summary_only(&mut summary_session, 1);
    summary_session.session_mutation_stamp = Some(10);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 3,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote same-count summary with same stamp should apply");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(record.session.messages_loaded);
    assert_eq!(record.session.messages.len(), 1);
    assert_eq!(record.session.message_count, 1);
    assert!(matches!(
        &record.session.messages[0],
        Message::Text { text, .. } if text == "Cached text still matches remote stamp."
    ));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn get_session_hydrates_unloaded_remote_proxy_from_remote_owner() {
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

    let mut full_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    full_remote_session.messages = vec![remote_text_message(
        "remote-message-1",
        "Hydrated transcript from remote owner.",
    )];
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = 1;
    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        3,
        OrchestratorInstanceStatus::Running,
    );
    let mut remote_state_session = full_remote_session.clone();
    make_remote_session_summary_only(&mut remote_state_session, 1);
    remote_state.orchestrators.clear();
    remote_state.sessions = vec![remote_state_session];

    let mut summary_session = full_remote_session.clone();
    make_remote_session_summary_only(&mut summary_session, 1);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote summary session create delta should apply");

    let local_session_id = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("remote proxy session should exist");
        let record = &inner.sessions[index];
        assert!(!record.session.messages_loaded);
        assert!(record.session.messages.is_empty());
        assert_eq!(record.session.message_count, 1);
        record.session.id.clone()
    };

    let (port, requests, server) = spawn_remote_session_and_state_response_server(
        SessionResponse {
            revision: 3,
            session: full_remote_session,
            server_instance_id: "remote-instance".to_owned(),
        },
        Some(remote_state),
    );
    insert_test_remote_connection(&state, &remote, port);

    let response = state
        .get_session(&local_session_id)
        .expect("remote proxy should hydrate from owner");
    assert_eq!(response.session.id, local_session_id);
    assert_eq!(
        response.session.project_id.as_deref(),
        Some(local_project_id.as_str())
    );
    assert!(response.session.messages_loaded);
    assert_eq!(response.session.message_count, 1);
    assert_eq!(response.session.messages.len(), 1);
    assert!(matches!(
        &response.session.messages[0],
        Message::Text { text, .. } if text == "Hydrated transcript from remote owner."
    ));

    let stored = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("remote proxy session should still exist");
        inner.sessions[index].session.clone()
    };
    assert!(stored.messages_loaded);
    assert_eq!(stored.messages.len(), 1);

    let request_lines = requests.lock().expect("requests mutex poisoned").clone();
    assert!(
        request_lines
            .iter()
            .any(|line| line.starts_with("GET /api/sessions/remote-session-1 ")),
        "expected targeted remote session fetch, saw {request_lines:?}"
    );
    assert!(
        request_lines
            .iter()
            .any(|line| line.starts_with("GET /api/state ")),
        "expected remote state resync before applying newer session response, saw {request_lines:?}"
    );
    join_test_server(server);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn get_session_rejects_stale_remote_transcript_after_newer_state_snapshot() {
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

    let mut stale_full_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    stale_full_session.messages =
        vec![remote_text_message("remote-message-1", "Stale transcript.")];
    stale_full_session.messages_loaded = true;
    stale_full_session.message_count = 1;
    stale_full_session.preview = "Stale transcript.".to_owned();
    stale_full_session.session_mutation_stamp = Some(30);

    let mut summary_session = stale_full_session.clone();
    make_remote_session_summary_only(&mut summary_session, 1);
    summary_session.session_mutation_stamp = Some(20);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote summary session create delta should apply");

    let local_session_id = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("remote proxy session should exist");
        inner.sessions[index].session.id.clone()
    };

    let mut newer_remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        4,
        OrchestratorInstanceStatus::Running,
    );
    let mut newer_summary = stale_full_session.clone();
    make_remote_session_summary_only(&mut newer_summary, 1);
    newer_summary.preview = "Newer summary from state.".to_owned();
    newer_summary.session_mutation_stamp = Some(40);
    newer_remote_state.orchestrators.clear();
    newer_remote_state.sessions = vec![newer_summary];

    let (port, _requests, server) = spawn_remote_session_and_state_response_server(
        SessionResponse {
            revision: 3,
            session: stale_full_session,
            server_instance_id: "remote-instance".to_owned(),
        },
        Some(newer_remote_state),
    );
    insert_test_remote_connection(&state, &remote, port);

    let error = match state.get_session(&local_session_id) {
        Ok(_) => panic!("stale remote transcript should be rejected after newer state sync"),
        Err(error) => error,
    };
    assert!(
        error
            .message
            .contains("older than synchronized remote state revision 4"),
        "unexpected error: {}",
        error.message
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should still exist");
    let record = &inner.sessions[index];
    assert!(!record.session.messages_loaded);
    assert!(record.session.messages.is_empty());
    assert_eq!(record.session.message_count, 1);
    assert_eq!(record.session.preview, "Newer summary from state.");
    assert_eq!(record.session.session_mutation_stamp, Some(40));
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&4));
    drop(inner);

    join_test_server(server);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_message_delta_hydrates_unloaded_proxy_before_gap_check() {
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

    let mut full_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    full_remote_session.messages = vec![
        remote_text_message("remote-message-1", "Existing remote transcript."),
        remote_text_message(
            "remote-message-2",
            "New remote message from delta revision.",
        ),
    ];
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = 2;

    let mut summary_session = full_remote_session.clone();
    make_remote_session_summary_only(&mut summary_session, 1);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote summary session create delta should apply");

    let (port, _requests, server) = spawn_remote_session_response_server(SessionResponse {
        revision: 3,
        session: full_remote_session,
        server_instance_id: "remote-instance".to_owned(),
    });
    insert_test_remote_connection(&state, &remote, port);

    let mut delta_receiver = state.subscribe_delta_events();
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageCreated {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "remote-message-2".to_owned(),
                message_index: 1,
                message_count: 2,
                message: remote_text_message(
                    "remote-message-2",
                    "New remote message from delta revision.",
                ),
                preview: "New remote message from delta revision.".to_owned(),
                status: SessionStatus::Idle,
                session_mutation_stamp: None,
            },
        )
        .expect("unloaded proxy should hydrate instead of failing gap check");

    assert!(
        delta_receiver.try_recv().is_err(),
        "targeted hydration should publish a state snapshot, not replay the skipped delta"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(record.session.messages_loaded);
    assert_eq!(record.session.message_count, 2);
    assert_eq!(record.session.messages.len(), 2);
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&3));
    drop(inner);

    join_test_server(server);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_text_delta_targeted_hydration_accepts_newer_global_revision_with_matching_metadata() {
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

    let mut full_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    full_remote_session.messages = vec![remote_text_message("remote-message-1", "Hello world")];
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = 1;
    full_remote_session.preview = "Hello world".to_owned();
    full_remote_session.session_mutation_stamp = Some(10);

    let mut summary_session = full_remote_session.clone();
    make_remote_session_summary_only(&mut summary_session, 1);
    summary_session.preview = "Hello".to_owned();
    summary_session.session_mutation_stamp = Some(9);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote summary session create delta should apply");

    let (port, _requests, server) = spawn_remote_session_response_server(SessionResponse {
        // The remote revision is global, so a busy upstream can already be
        // ahead of the triggering delta even when this session's transcript is
        // exactly at the delta's post-state.
        revision: 5,
        session: full_remote_session,
        server_instance_id: "remote-instance".to_owned(),
    });
    insert_test_remote_connection(&state, &remote, port);

    let mut delta_receiver = state.subscribe_delta_events();
    let replayed_delta = || DeltaEvent::TextDelta {
        revision: 3,
        session_id: "remote-session-1".to_owned(),
        message_id: "remote-message-1".to_owned(),
        message_index: 0,
        message_count: 1,
        delta: " world".to_owned(),
        preview: Some("Hello world".to_owned()),
        session_mutation_stamp: Some(10),
    };

    state
        .apply_remote_delta_event(&remote.id, replayed_delta())
        .expect("unloaded proxy should hydrate before applying text delta");
    assert!(
        delta_receiver.try_recv().is_err(),
        "targeted hydration should not replay the triggering text delta"
    );

    state
        .apply_remote_delta_event(&remote.id, replayed_delta())
        .expect("replayed text delta should be skipped after hydration");
    assert!(
        delta_receiver.try_recv().is_err(),
        "same-revision replay should not publish a duplicate text delta"
    );

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageUpdated {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "remote-message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                message: remote_text_message("remote-message-1", "Hello world"),
                preview: "Reviewed remote message.".to_owned(),
                status: SessionStatus::Idle,
                session_mutation_stamp: Some(10),
            },
        )
        .expect("same-revision sibling delta should not be suppressed by hydration replay cache");
    let sibling_payload = delta_receiver
        .try_recv()
        .expect("same-revision sibling delta should publish");
    let sibling_delta: DeltaEvent =
        serde_json::from_str(&sibling_payload).expect("sibling delta should decode");
    match sibling_delta {
        DeltaEvent::MessageUpdated { message_id, .. } => {
            assert_eq!(message_id, "remote-message-1");
        }
        _ => panic!("unexpected sibling delta variant"),
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(record.session.messages_loaded);
    assert_eq!(record.session.message_count, 1);
    assert_eq!(record.session.session_mutation_stamp, Some(10));
    assert!(matches!(
        &record.session.messages[0],
        Message::Text { text, .. } if text == "Hello world"
    ));
    assert_eq!(record.session.preview, "Reviewed remote message.");
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&3));
    drop(inner);

    join_test_server(server);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_text_delta_exact_replay_is_skipped_for_loaded_proxy_session() {
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

    let mut full_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    full_remote_session.messages = vec![remote_text_message("remote-message-1", "Hello")];
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = 1;
    full_remote_session.session_mutation_stamp = Some(10);

    let mut delta_receiver = state.subscribe_delta_events();
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: full_remote_session.id.clone(),
                session: full_remote_session,
            },
        )
        .expect("remote full session create delta should apply");
    let _ = delta_receiver.try_recv();

    let text_delta = || DeltaEvent::TextDelta {
        revision: 3,
        session_id: "remote-session-1".to_owned(),
        message_id: "remote-message-1".to_owned(),
        message_index: 0,
        message_count: 1,
        delta: " world".to_owned(),
        preview: Some("Hello world".to_owned()),
        session_mutation_stamp: Some(11),
    };

    state
        .apply_remote_delta_event(&remote.id, text_delta())
        .expect("first text delta should apply");
    let first_payload = delta_receiver
        .try_recv()
        .expect("first text delta should publish");
    let first_delta: DeltaEvent =
        serde_json::from_str(&first_payload).expect("first delta should decode");
    assert!(matches!(first_delta, DeltaEvent::TextDelta { .. }));

    state
        .apply_remote_delta_event(&remote.id, text_delta())
        .expect("exact text delta replay should be skipped");
    assert!(
        delta_receiver.try_recv().is_err(),
        "exact replay should not publish a duplicate text delta"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(matches!(
        &record.session.messages[0],
        Message::Text { text, .. } if text == "Hello world"
    ));
    assert_eq!(record.session.preview, "Hello world");
    assert_eq!(record.session.session_mutation_stamp, Some(11));
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&3));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_delta_replay_cache_clears_with_remote_revision_watermark() {
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

    let mut full_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    full_remote_session.messages = vec![remote_text_message("remote-message-1", "Hello")];
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = 1;
    full_remote_session.session_mutation_stamp = Some(10);

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: full_remote_session.id.clone(),
                session: full_remote_session,
            },
        )
        .expect("remote full session create delta should apply");

    let text_delta = || DeltaEvent::TextDelta {
        revision: 3,
        session_id: "remote-session-1".to_owned(),
        message_id: "remote-message-1".to_owned(),
        message_index: 0,
        message_count: 1,
        delta: " world".to_owned(),
        preview: Some("Hello world".to_owned()),
        session_mutation_stamp: Some(11),
    };

    state
        .apply_remote_delta_event(&remote.id, text_delta())
        .expect("first text delta should apply and seed replay cache");
    state.clear_remote_applied_revision(&remote.id);
    state
        .apply_remote_delta_event(&remote.id, text_delta())
        .expect("same key should be evaluated again after continuity reset");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(matches!(
        &record.session.messages[0],
        Message::Text { text, .. } if text == "Hello world world"
    ));
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&3));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn targeted_remote_hydration_rejects_message_count_length_mismatch() {
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

    let mut full_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    full_remote_session.messages = vec![remote_text_message("remote-message-1", "Hello world")];
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = 2;
    full_remote_session.session_mutation_stamp = Some(10);

    let mut summary_session = full_remote_session.clone();
    make_remote_session_summary_only(&mut summary_session, 1);
    summary_session.session_mutation_stamp = Some(9);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote summary session create delta should apply");

    let (port, _requests, server) = spawn_remote_session_response_server(SessionResponse {
        revision: 3,
        session: full_remote_session,
        server_instance_id: "remote-instance".to_owned(),
    });
    insert_test_remote_connection(&state, &remote, port);

    let err = state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::TextDelta {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "remote-message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                delta: " world".to_owned(),
                preview: Some("Hello world".to_owned()),
                session_mutation_stamp: Some(10),
            },
        )
        .expect_err("targeted hydration should reject inconsistent full transcript");
    assert!(
        err.to_string()
            .contains("did not match loaded transcript length"),
        "unexpected error: {err:#}"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(!record.session.messages_loaded);
    assert_eq!(record.session.message_count, 1);
    assert!(record.session.messages.is_empty());
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&2));
    drop(inner);

    join_test_server(server);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_delta_repair_rejects_newer_targeted_session_revision() {
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

    let mut full_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    full_remote_session.messages = vec![
        remote_text_message("remote-message-1", "Existing remote transcript."),
        remote_text_message("remote-message-2", "Newer transcript from future revision."),
    ];
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = 2;

    let mut summary_session = full_remote_session.clone();
    make_remote_session_summary_only(&mut summary_session, 1);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote summary session create delta should apply");

    let (port, requests, server) = spawn_remote_session_response_server(SessionResponse {
        revision: 5,
        session: full_remote_session,
        server_instance_id: "remote-instance".to_owned(),
    });
    insert_test_remote_connection(&state, &remote, port);

    let error = state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageCreated {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "remote-message-2".to_owned(),
                message_index: 1,
                message_count: 2,
                message: remote_text_message(
                    "remote-message-2",
                    "Newer transcript from future revision.",
                ),
                preview: "Newer transcript from future revision.".to_owned(),
                status: SessionStatus::Idle,
                session_mutation_stamp: None,
            },
        )
        .expect_err("newer targeted session response should reject narrow repair");
    assert!(
        format!("{error:#}").contains("newer than targeted repair revision 3"),
        "unexpected error: {error:#}"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(!record.session.messages_loaded);
    assert!(record.session.messages.is_empty());
    assert_eq!(record.session.message_count, 1);
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&2));
    drop(inner);

    let request_lines = requests.lock().expect("requests mutex poisoned").clone();
    assert!(
        request_lines
            .iter()
            .any(|line| line.starts_with("GET /api/sessions/remote-session-1 ")),
        "expected targeted remote session fetch, saw {request_lines:?}"
    );
    join_test_server(server);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn stale_remote_delta_skips_before_targeted_hydration_fetch() {
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

    let mut summary_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    make_remote_session_summary_only(&mut summary_session, 1);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 5,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote summary session create delta should apply");

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageCreated {
                revision: 4,
                session_id: "remote-session-1".to_owned(),
                message_id: "remote-message-2".to_owned(),
                message_index: 1,
                message_count: 2,
                message: remote_text_message(
                    "remote-message-2",
                    "Stale delta should not trigger a remote fetch.",
                ),
                preview: "Stale delta should not trigger a remote fetch.".to_owned(),
                status: SessionStatus::Idle,
                session_mutation_stamp: None,
            },
        )
        .expect("stale unloaded-proxy delta should be skipped before hydration");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(!record.session.messages_loaded);
    assert!(record.session.messages.is_empty());
    assert_eq!(record.session.message_count, 1);
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&5));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

fn apply_remote_created_text_message_at(
    state: &AppState,
    remote_id: &str,
    revision: u64,
    message_id: &str,
    message_index: usize,
    message_count: u32,
    text: &str,
) {
    state
        .apply_remote_delta_event(
            remote_id,
            DeltaEvent::MessageCreated {
                revision,
                session_id: "remote-session-1".to_owned(),
                message_id: message_id.to_owned(),
                message_index,
                message_count,
                message: remote_text_message(message_id, text),
                preview: text.to_owned(),
                status: SessionStatus::Active,
                session_mutation_stamp: None,
            },
        )
        .expect("remote message create delta should apply");
}

fn apply_remote_created_text_message(
    state: &AppState,
    remote_id: &str,
    revision: u64,
    message_id: &str,
    text: &str,
) {
    apply_remote_created_text_message_at(state, remote_id, revision, message_id, 0, 1, text);
}

#[test]
fn remote_summary_state_snapshot_preserves_existing_proxy_transcript() {
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
    let local_session_id = seed_remote_proxy_session_for_delta_test(&state, &remote);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageCreated {
                revision: 2,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                message: remote_text_message("message-1", "Hydrated remote transcript."),
                preview: "Hydrated remote transcript.".to_owned(),
                status: SessionStatus::Active,
                session_mutation_stamp: Some(42),
            },
        )
        .expect("remote message create delta should apply");

    let mut remote_state = state.snapshot();
    remote_state.revision = 3;
    let mut remote_session = remote_state
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .cloned()
        .expect("local proxy should be present in snapshot");
    remote_session.id = "remote-session-1".to_owned();
    remote_session.preview = "Summary-only remote update.".to_owned();
    remote_session.messages.clear();
    remote_session.messages_loaded = false;
    remote_session.session_mutation_stamp = Some(42);
    remote_state.sessions = vec![remote_session];

    state
        .apply_remote_state_snapshot(&remote.id, remote_state)
        .expect("summary remote snapshot should apply");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .find_session_index(&local_session_id)
        .and_then(|index| inner.sessions.get(index))
        .expect("local proxy should remain");
    assert_eq!(record.session.preview, "Summary-only remote update.");
    assert!(record.session.messages_loaded);
    assert_eq!(record.session.messages.len(), 1);
    assert_eq!(record.session.messages[0].id(), "message-1");
    assert_eq!(record.message_positions.get("message-1").copied(), Some(0));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_message_created_delta_replaces_and_reorders_existing_message() {
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
    let local_session_id = seed_remote_proxy_session_for_delta_test(&state, &remote);
    apply_remote_created_text_message(&state, &remote.id, 2, "message-1", "First remote message.");
    apply_remote_created_text_message_at(&state, &remote.id, 3, "message-2", 1, 2, "Second draft.");
    let mut delta_receiver = state.subscribe_delta_events();

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageCreated {
                revision: 4,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-2".to_owned(),
                message_index: 0,
                message_count: 2,
                message: remote_text_message("message-2", "Second final."),
                preview: "Second final.".to_owned(),
                status: SessionStatus::Idle,
                session_mutation_stamp: None,
            },
        )
        .expect("remote message create replay should replace and reorder by id");

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    assert_eq!(session.preview, "Second final.");
    assert_eq!(session.status, SessionStatus::Idle);
    let message_ids: Vec<_> = session
        .messages
        .iter()
        .map(|message| message.id().to_owned())
        .collect();
    assert_eq!(message_ids, vec!["message-2", "message-1"]);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Second final."
    ));
    assert!(matches!(
        session.messages.get(1),
        Some(Message::Text { text, .. }) if text == "First remote message."
    ));

    let delta: DeltaEvent = serde_json::from_str(
        &delta_receiver
            .try_recv()
            .expect("remote message create replay should publish a localized delta"),
    )
    .expect("message create delta should decode");
    match delta {
        DeltaEvent::MessageCreated {
            revision,
            session_id,
            message_id,
            message_index,
            message_count,
            message,
            preview,
            status,
            session_mutation_stamp,
        } => {
            assert_eq!(revision, snapshot.revision);
            assert_eq!(session_id, local_session_id);
            assert_eq!(message_id, "message-2");
            assert_eq!(message_index, 0);
            assert_eq!(message_count, 2);
            assert!(matches!(
                message,
                Message::Text { text, .. } if text == "Second final."
            ));
            assert_eq!(preview, "Second final.");
            assert_eq!(status, SessionStatus::Idle);
            assert_eq!(session_mutation_stamp, session.session_mutation_stamp);
        }
        _ => panic!("expected localized MessageCreated delta"),
    }
    assert!(
        delta_receiver.try_recv().is_err(),
        "remote message create replay should publish exactly one delta"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.should_skip_remote_applied_delta_revision(&remote.id, 4));
    assert!(inner.should_skip_remote_applied_delta_revision(&remote.id, 3));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_message_created_delta_rejects_gap_without_advancing_revision() {
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
    let local_session_id = seed_remote_proxy_session_for_delta_test(&state, &remote);
    let initial_revision = state.snapshot().revision;
    let mut delta_receiver = state.subscribe_delta_events();

    let error = state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageCreated {
                revision: 2,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-1".to_owned(),
                message_index: 1,
                message_count: 1,
                message: remote_text_message("message-1", "Gap message."),
                preview: "Gap message.".to_owned(),
                status: SessionStatus::Active,
                session_mutation_stamp: None,
            },
        )
        .expect_err("gap remote MessageCreated should request resync");
    assert!(
        error.to_string().contains("leaves a gap"),
        "unexpected error: {error:#}"
    );
    assert!(
        delta_receiver.try_recv().is_err(),
        "gap MessageCreated should not publish a local delta"
    );

    let snapshot = state.snapshot();
    assert_eq!(snapshot.revision, initial_revision);
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    assert!(session.messages.is_empty());

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.should_skip_remote_applied_delta_revision(&remote.id, 2));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_message_created_delta_rejects_payload_id_mismatch_without_advancing_revision() {
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
    let local_session_id = seed_remote_proxy_session_for_delta_test(&state, &remote);
    let initial_revision = state.snapshot().revision;
    let mut delta_receiver = state.subscribe_delta_events();

    let error = state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageCreated {
                revision: 2,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                message: remote_text_message("different-message", "Wrong message."),
                preview: "Wrong message.".to_owned(),
                status: SessionStatus::Active,
                session_mutation_stamp: None,
            },
        )
        .expect_err("id-mismatched remote MessageCreated should request resync");
    assert!(
        error.to_string().contains("payload id"),
        "unexpected error: {error:#}"
    );
    assert!(
        delta_receiver.try_recv().is_err(),
        "id-mismatched MessageCreated should not publish a local delta"
    );

    let snapshot = state.snapshot();
    assert_eq!(snapshot.revision, initial_revision);
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    assert!(session.messages.is_empty());

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.should_skip_remote_applied_delta_revision(&remote.id, 2));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_message_created_delta_rejects_existing_message_out_of_bounds_without_advancing_revision()
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
    let local_session_id = seed_remote_proxy_session_for_delta_test(&state, &remote);
    apply_remote_created_text_message(&state, &remote.id, 2, "message-1", "First message.");
    apply_remote_created_text_message_at(
        &state,
        &remote.id,
        3,
        "message-2",
        1,
        2,
        "Second message.",
    );
    let initial_revision = state.snapshot().revision;
    let mut delta_receiver = state.subscribe_delta_events();

    let error = state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageCreated {
                revision: 4,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-2".to_owned(),
                message_index: 2,
                message_count: 2,
                message: remote_text_message("message-2", "Second final."),
                preview: "Second final.".to_owned(),
                status: SessionStatus::Idle,
                session_mutation_stamp: None,
            },
        )
        .expect_err("out-of-bounds existing remote MessageCreated should request resync");
    assert!(
        error
            .to_string()
            .contains("out of bounds for existing message"),
        "unexpected error: {error:#}"
    );
    assert!(
        delta_receiver.try_recv().is_err(),
        "out-of-bounds existing MessageCreated should not publish a local delta"
    );

    let snapshot = state.snapshot();
    assert_eq!(snapshot.revision, initial_revision);
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    let message_ids: Vec<_> = session
        .messages
        .iter()
        .map(|message| message.id().to_owned())
        .collect();
    assert_eq!(message_ids, vec!["message-1", "message-2"]);
    assert!(matches!(
        session.messages.get(1),
        Some(Message::Text { text, .. }) if text == "Second message."
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.should_skip_remote_applied_revision(&remote.id, 4));
    assert!(inner.should_skip_remote_applied_revision(&remote.id, 3));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_command_update_missing_target_rejects_gap_without_advancing_revision() {
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
    let local_session_id = seed_remote_proxy_session_for_delta_test(&state, &remote);
    let initial_revision = state.snapshot().revision;
    let mut delta_receiver = state.subscribe_delta_events();

    let error = state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::CommandUpdate {
                revision: 2,
                session_id: "remote-session-1".to_owned(),
                message_id: "command-1".to_owned(),
                message_index: 1,
                message_count: 1,
                command: "cargo check".to_owned(),
                command_language: Some("bash".to_owned()),
                output: String::new(),
                output_language: Some("text".to_owned()),
                status: CommandStatus::Running,
                preview: "cargo check".to_owned(),
                session_mutation_stamp: None,
            },
        )
        .expect_err("gap remote CommandUpdate should request resync");
    assert!(
        error
            .to_string()
            .contains("remote CommandUpdate index `1` leaves a gap"),
        "unexpected error: {error:#}"
    );
    assert!(
        delta_receiver.try_recv().is_err(),
        "gap CommandUpdate should not publish a local delta"
    );

    let snapshot = state.snapshot();
    assert_eq!(snapshot.revision, initial_revision);
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    assert!(session.messages.is_empty());

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.should_skip_remote_applied_delta_revision(&remote.id, 2));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_parallel_agents_update_missing_target_rejects_gap_without_advancing_revision() {
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
    let local_session_id = seed_remote_proxy_session_for_delta_test(&state, &remote);
    let initial_revision = state.snapshot().revision;
    let mut delta_receiver = state.subscribe_delta_events();

    let error = state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::ParallelAgentsUpdate {
                revision: 2,
                session_id: "remote-session-1".to_owned(),
                message_id: "parallel-1".to_owned(),
                message_index: 1,
                message_count: 1,
                agents: vec![ParallelAgentProgress {
                    detail: Some("Collecting context".to_owned()),
                    id: "reviewer".to_owned(),
                    status: ParallelAgentStatus::Running,
                    title: "Reviewer".to_owned(),
                }],
                preview: "Running reviewer".to_owned(),
                session_mutation_stamp: None,
            },
        )
        .expect_err("gap remote ParallelAgentsUpdate should request resync");
    assert!(
        error
            .to_string()
            .contains("remote ParallelAgentsUpdate index `1` leaves a gap"),
        "unexpected error: {error:#}"
    );
    assert!(
        delta_receiver.try_recv().is_err(),
        "gap ParallelAgentsUpdate should not publish a local delta"
    );

    let snapshot = state.snapshot();
    assert_eq!(snapshot.revision, initial_revision);
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    assert!(session.messages.is_empty());

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.should_skip_remote_applied_delta_revision(&remote.id, 2));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that a remote MessageUpdated delta is an in-place replacement:
// the local proxy keeps the existing transcript position, republishes a
// localized MessageUpdated delta, and advances the remote applied
// revision only after the replacement commits.
// Guards against regressing back to MessageCreated-style insertion.
#[test]
fn remote_message_updated_delta_replaces_existing_message_and_publishes_local_delta() {
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
    let local_session_id = seed_remote_proxy_session_for_delta_test(&state, &remote);
    apply_remote_created_text_message(&state, &remote.id, 2, "message-1", "Draft remote message.");
    let mut delta_receiver = state.subscribe_delta_events();

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageUpdated {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                message: remote_text_message("message-1", "Final remote message."),
                preview: "Final remote message.".to_owned(),
                status: SessionStatus::Idle,
                session_mutation_stamp: None,
            },
        )
        .expect("remote message update delta should apply");

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    assert_eq!(session.preview, "Final remote message.");
    assert_eq!(session.status, SessionStatus::Idle);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Final remote message."
    ));

    let delta: DeltaEvent = serde_json::from_str(
        &delta_receiver
            .try_recv()
            .expect("remote message update should publish a localized delta"),
    )
    .expect("message update delta should decode");
    match delta {
        DeltaEvent::MessageUpdated {
            revision,
            session_id,
            message_id,
            message_index,
            message_count,
            message,
            preview,
            status,
            session_mutation_stamp,
        } => {
            assert_eq!(revision, snapshot.revision);
            assert_eq!(session_id, local_session_id);
            assert_eq!(message_id, "message-1");
            assert_eq!(message_index, 0);
            assert_eq!(message_count, 1);
            assert!(matches!(
                message,
                Message::Text { text, .. } if text == "Final remote message."
            ));
            assert_eq!(preview, "Final remote message.");
            assert_eq!(status, SessionStatus::Idle);
            assert_eq!(session_mutation_stamp, session.session_mutation_stamp);
        }
        _ => panic!("expected localized MessageUpdated delta"),
    }
    assert!(
        delta_receiver.try_recv().is_err(),
        "remote message update should publish exactly one delta"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.should_skip_remote_applied_delta_revision(&remote.id, 3));
    assert!(inner.should_skip_remote_applied_delta_revision(&remote.id, 2));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_message_updated_delta_uses_message_id_when_remote_index_is_stale() {
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
    let local_session_id = seed_remote_proxy_session_for_delta_test(&state, &remote);
    apply_remote_created_text_message(&state, &remote.id, 2, "message-1", "First message.");
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageCreated {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-2".to_owned(),
                message_index: 1,
                message_count: 2,
                message: remote_text_message("message-2", "Second draft."),
                preview: "Second draft.".to_owned(),
                status: SessionStatus::Active,
                session_mutation_stamp: None,
            },
        )
        .expect("second remote message create delta should apply");
    let mut delta_receiver = state.subscribe_delta_events();

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageUpdated {
                revision: 4,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-2".to_owned(),
                message_index: 0,
                message_count: 2,
                message: remote_text_message("message-2", "Second final."),
                preview: "Second final.".to_owned(),
                status: SessionStatus::Idle,
                session_mutation_stamp: None,
            },
        )
        .expect("remote message update with stale index should apply by id");

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    let message_ids: Vec<_> = session
        .messages
        .iter()
        .map(|message| message.id().to_owned())
        .collect();
    assert_eq!(message_ids, vec!["message-1", "message-2"]);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "First message."
    ));
    assert!(matches!(
        session.messages.get(1),
        Some(Message::Text { text, .. }) if text == "Second final."
    ));

    let delta: DeltaEvent = serde_json::from_str(
        &delta_receiver
            .try_recv()
            .expect("remote stale-index update should publish a localized delta"),
    )
    .expect("message update delta should decode");
    match delta {
        DeltaEvent::MessageUpdated {
            revision,
            session_id,
            message_id,
            message_index,
            message_count,
            message,
            preview,
            status,
            session_mutation_stamp,
        } => {
            assert_eq!(revision, snapshot.revision);
            assert_eq!(session_id, local_session_id);
            assert_eq!(message_id, "message-2");
            assert_eq!(message_index, 1);
            assert_eq!(message_count, 2);
            assert!(matches!(
                message,
                Message::Text { text, .. } if text == "Second final."
            ));
            assert_eq!(preview, "Second final.");
            assert_eq!(status, SessionStatus::Idle);
            assert_eq!(session_mutation_stamp, session.session_mutation_stamp);
        }
        _ => panic!("expected localized MessageUpdated delta"),
    }
    assert!(
        delta_receiver.try_recv().is_err(),
        "remote stale-index update should publish exactly one delta"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.should_skip_remote_applied_delta_revision(&remote.id, 4));
    assert!(inner.should_skip_remote_applied_delta_revision(&remote.id, 3));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that a remote MessageUpdated targeting an unknown local message is
// treated as a sync gap. It must not synthesize MessageCreated, publish a
// partial local delta, or advance the remote applied revision.
// Guards against masking missed transcript state.
#[test]
fn remote_message_updated_delta_missing_target_errors_without_creating_or_advancing_revision() {
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
    let local_session_id = seed_remote_proxy_session_for_delta_test(&state, &remote);
    let initial_revision = state.snapshot().revision;
    let mut delta_receiver = state.subscribe_delta_events();

    let error = state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageUpdated {
                revision: 5,
                session_id: "remote-session-1".to_owned(),
                message_id: "missing-message".to_owned(),
                message_index: 0,
                message_count: 1,
                message: remote_text_message("missing-message", "Remote drift."),
                preview: "Remote drift.".to_owned(),
                status: SessionStatus::Active,
                session_mutation_stamp: None,
            },
        )
        .expect_err("missing-target remote MessageUpdated should request resync");
    assert!(
        error
            .to_string()
            .contains("remote MessageUpdated for unknown message `missing-message`"),
        "unexpected error: {error:#}"
    );
    assert!(
        delta_receiver.try_recv().is_err(),
        "missing-target MessageUpdated should not publish a local delta"
    );

    let snapshot = state.snapshot();
    assert_eq!(snapshot.revision, initial_revision);
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    assert!(session.messages.is_empty());

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.should_skip_remote_applied_delta_revision(&remote.id, 5));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that stale remote MessageUpdated deltas are ignored before any
// local transcript mutation or republish.
// Guards against delayed SSE replay rolling back a newer local proxy.
#[test]
fn stale_remote_message_updated_delta_is_ignored() {
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
    let local_session_id = seed_remote_proxy_session_for_delta_test(&state, &remote);
    apply_remote_created_text_message(&state, &remote.id, 4, "message-1", "Current text.");
    let revision_after_create = state.snapshot().revision;
    let mut delta_receiver = state.subscribe_delta_events();

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageUpdated {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                message: remote_text_message("message-1", "Stale text."),
                preview: "Stale text.".to_owned(),
                status: SessionStatus::Active,
                session_mutation_stamp: None,
            },
        )
        .expect("stale remote message update should be ignored");
    assert!(
        delta_receiver.try_recv().is_err(),
        "stale MessageUpdated should not publish a local delta"
    );

    let snapshot = state.snapshot();
    assert_eq!(snapshot.revision, revision_after_create);
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Current text."
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.should_skip_remote_applied_delta_revision(&remote.id, 3));
    assert!(!inner.should_skip_remote_applied_delta_revision(&remote.id, 4));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn stale_remote_message_updated_delta_with_mismatched_payload_id_is_ignored() {
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
    let local_session_id = seed_remote_proxy_session_for_delta_test(&state, &remote);
    apply_remote_created_text_message(&state, &remote.id, 4, "message-1", "Current text.");
    let revision_after_create = state.snapshot().revision;
    let mut delta_receiver = state.subscribe_delta_events();

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageUpdated {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                message: remote_text_message("different-message", "Stale wrong text."),
                preview: "Stale wrong text.".to_owned(),
                status: SessionStatus::Active,
                session_mutation_stamp: None,
            },
        )
        .expect("stale id-mismatched remote message update should be ignored");
    assert!(
        delta_receiver.try_recv().is_err(),
        "stale id-mismatched MessageUpdated should not publish a local delta"
    );

    let snapshot = state.snapshot();
    assert_eq!(snapshot.revision, revision_after_create);
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Current text."
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.should_skip_remote_applied_delta_revision(&remote.id, 3));
    assert!(!inner.should_skip_remote_applied_delta_revision(&remote.id, 4));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that a malformed MessageUpdated payload is rejected before state
// mutation: the event id and embedded message id must match.
// Guards against localizing the wrong remote message.
#[test]
fn remote_message_updated_delta_rejects_payload_id_mismatch() {
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
    let local_session_id = seed_remote_proxy_session_for_delta_test(&state, &remote);
    apply_remote_created_text_message(&state, &remote.id, 2, "message-1", "Current text.");
    let revision_after_create = state.snapshot().revision;
    let mut delta_receiver = state.subscribe_delta_events();

    let error = state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageUpdated {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                message: remote_text_message("different-message", "Wrong text."),
                preview: "Wrong text.".to_owned(),
                status: SessionStatus::Active,
                session_mutation_stamp: None,
            },
        )
        .expect_err("id-mismatched remote MessageUpdated should fail");
    assert!(
        error
            .to_string()
            .contains("did not match event id `message-1`"),
        "unexpected error: {error:#}"
    );
    assert!(
        delta_receiver.try_recv().is_err(),
        "id-mismatched MessageUpdated should not publish a local delta"
    );

    let snapshot = state.snapshot();
    assert_eq!(snapshot.revision, revision_after_create);
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Current text."
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.should_skip_remote_applied_delta_revision(&remote.id, 3));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that two deltas sharing the same remote revision both apply in
// order when they affect different fields; applied-revision dedupe
// recognizes only strictly-older revisions as stale.
// Guards against the dedupe window being too tight (drops valid
// sibling deltas) or too loose (replays already-applied ones).
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
                message_count: 1,
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
                session_mutation_stamp: None,
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
                message_count: 2,
                command: "echo ok".to_owned(),
                command_language: Some("bash".to_owned()),
                output: "ok".to_owned(),
                output_language: Some("text".to_owned()),
                status: CommandStatus::Success,
                preview: "echo ok".to_owned(),
                session_mutation_stamp: None,
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

// Pins the end-to-end create-orchestrator flow for remote projects: the
// request is rewritten with the remote's own project id and forwarded
// as POST /api/orchestrators, and the response is localized (new local
// orchestrator id, template_snapshot project_id rewritten to local).
// Guards against the UI seeing raw remote ids or sending local ids that
// the remote cannot resolve.
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

// Pins that create_remote_orchestrator_proxy localizes the response
// orchestrator, registers the remote_orchestrator_id, and writes the
// returned revision into the applied-revision watermark so that later
// delta/snapshot replays at the same revision are skipped.
// Guards against creating a remote orchestrator but leaving the local
// watermark behind, which would cause the same state to re-apply.
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

// Pins that create_remote_orchestrator_proxy returns BAD_GATEWAY and
// leaves next_session_number, orchestrator instances, session records,
// the applied-revision watermark, and the persisted state file all
// unchanged when the localization step fails.
// Guards against partial writes on failed proxy creation that would
// surface as orphaned sessions or a poisoned watermark.
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

// Pins that a stale create-orchestrator response (revision below the
// already-applied remote revision) still materializes the launched
// orchestrator locally rather than being dropped as a stale snapshot.
// Guards against newly-launched remote orchestrators disappearing from
// the UI when an unrelated delta has bumped the revision in the meantime.
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

// Pins that when a remote replies 404 to POST /api/orchestrators with
// an inline template body, the error is translated to a BAD_GATEWAY
// "must be upgraded" message and only the expected health + create
// requests are made (no extra diagnostic probe loop).
// Guards against silent failure when a remote lacks inline-template
// support and against accidentally hammering it with retries.
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

// Pins that a pre-cached supports_inline_orchestrator_templates=false
// still returns the upgrade-required error on a 404, without issuing
// a second post-404 capability probe, while still performing the
// normal pre-request availability check.
// Guards against the capability cache causing either misleading
// success or an extra round-trip on repeated failures.
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

// Pins that a snapshot whose orchestrator references a remote project
// with no corresponding local project mapping applies without error
// but leaves orchestrator_instances and sessions empty.
// Guards against assigning an empty-string local project id to
// orchestrators or sessions when the remote/local project pairing is
// missing.
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

// Pins that pause / resume / stop on a mirrored orchestrator each
// issue POST /api/orchestrators/{remote_id}/{action} to the remote (with
// a preceding health check), apply the returned state snapshot locally,
// and update the UI-visible status accordingly.
// Guards against lifecycle actions being applied only locally (which
// would diverge from the remote) or silently swallowing the proxy error.
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

// Pins that when a later snapshot introduces a new orchestrator whose
// localization fails, existing mirrored orchestrators for that remote
// survive intact rather than being cleared in the rollback.
// Guards against an "all or nothing" rollback that wipes healthy
// mirrored orchestrators on a single bad delta.
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

// Pins that sessions referenced by a surviving mirrored orchestrator
// are not pruned by session retention logic, even when the incoming
// snapshot drops those session ids because its orchestrator fails to
// localize.
// Guards against the retention pass removing proxy sessions still in
// active use by a mirrored orchestrator.
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

// Pins that sync_remote_state_for_target, given a focused target,
// updates the single focused session even when orchestrator
// localization fails, without creating proxy records for other
// sessions in the payload and without writing any orchestrator entry
// with this remote's id to persisted state.
// Guards against a focused resync accidentally doing a full sync's
// work when its orchestrator leg fails.
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

// Pins that sync_remote_state_for_target is a no-op when the payload's
// revision is older than the applied-revision watermark: the target
// session's preview stays at the newer value already mirrored locally.
// Guards against a stale focused fetch from an in-flight request
// clobbering a fresher update.
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

// Pins that a snapshot whose session list omits a previously mirrored
// remote session drops that local proxy record, while leaving remote
// sessions still present and purely local sessions alone.
// Guards against the retention pass being too aggressive (wiping
// unrelated sessions) or too lenient (leaving zombies behind).
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

// Pins that a marked empty-state SSE "state" event triggers one full
// /api/state resync per revision, deduplicates repeated sends of the
// same revision, and still resyncs when the revision bumps.
// Guards against a chatty remote forcing the local backend into a
// tight resync loop over identical fallback payloads.
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

// Pins that the fallback-resync watermark is keyed per remote id, is
// monotonic (older revisions for the same remote stay skipped), treats
// a different remote id as independent, and is cleared by
// clear_remote_sse_fallback_resync.
// Guards against cross-remote contamination or a sticky watermark
// that blocks resync after a reconnect.
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

// Pins StateInner::{note,should_skip}_remote_applied_revision and the
// delta variant: the snapshot watermark skips <= revisions once noted,
// the delta watermark skips only strictly-older revisions, and a lower
// note() call is a no-op.
// Guards against the dedupe predicates letting already-applied state
// replay or blocking fresh state.
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

// Pins that decode_remote_json strips control characters, replaces
// CRLF with spaces, and truncates raw non-JSON error bodies past the
// MAX_REMOTE_ERROR_BODY_CHARS limit before surfacing them.
// Guards against a hostile/broken remote injecting log-splitting or
// control sequences, or flooding logs with a huge error body.
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

// Pins that decode_remote_json applies the same sanitization + cap to
// the "error" field of a structured JSON error body, stripping control
// chars and appending "..." when truncated.
// Guards against attackers smuggling log-splitting sequences via the
// JSON "error" field instead of the raw body.
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

// Pins that decode_remote_json rejects response bodies exceeding
// MAX_REMOTE_ERROR_BODY_BYTES with the fixed message "remote error
// response too large" rather than reading the full body.
// Guards against memory exhaustion from a remote replying with a
// multi-megabyte error body.
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

// Pins that applied-revision tracking on StateInner is keyed per
// remote id, stays monotonic (a lower note() call does not lower the
// watermark), and that clear_remote_applied_revision resets it without
// touching other remotes.
// Guards against cross-remote dedupe contamination and against a
// stuck watermark after event-stream teardown.
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

// Pins that an SSE "state" event carrying an ordinary (non-fallback)
// empty snapshot is applied directly: remote proxy sessions go away,
// local-only sessions stay.
// Guards against mistaking a genuine empty-state resync for a
// fallback marker and skipping its session retention work.
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

// Pins that when a remote responds 200 OK to /api/terminal/run/stream
// with a non-SSE content-type (e.g. text/html), the local proxy emits
// a BAD_GATEWAY "unexpected content type" error event and does not
// fall back to the non-stream /api/terminal/run JSON route.
// Guards against silently accepting malformed stream responses or
// double-executing the command by falling back after success.
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

// Pins that a remote SSE response to /api/terminal/run/stream is
// forwarded frame-for-frame with output events ahead of a single
// complete event and no error event, and that the non-stream JSON
// fallback is not invoked.
// Guards against reordering, duplicated complete frames, or the
// fallback firing alongside a successful stream.
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

// Pins that 404 and 405 on the stream route both trigger the JSON
// fallback via POST /api/terminal/run (exercising assert_remote_terminal_stream_fallback_for_status).
// Guards against only one of those statuses being treated as
// "remote lacks streaming" and the other propagating as an error.
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

// Pins parse_terminal_sse_frame's handling of default `message` event
// names, `:` comment lines, CRLF line endings, multi-line `data:`
// concatenation via `\n`, and the mixed-delimiter detection in
// find_sse_frame_delimiter.
// Guards against subtle SSE-parser regressions that would drop comment
// lines or mis-join multi-line data fields.
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

// Pins that a remote `event: error` SSE frame carrying its own status
// (503 here) is normalized to a local BAD_GATEWAY with a message
// including "remote terminal stream error (503)".
// Guards against the remote dictating client-visible status codes
// (e.g. pretending to be 5xx/4xx from our server).
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

// Pins that a remote error frame with status 429 is specifically kept
// as TOO_MANY_REQUESTS (not downgraded to BAD_GATEWAY) and that
// annotate_remote_terminal_429 adds a "remote <name>:" prefix so the
// UI can attribute the throttling to the remote.
// Guards against losing the 429 signal and against unattributed
// throttling messages.
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

// Pins that handle_remote_terminal_sse_frame truncates an oversized
// `output` stdout chunk to TERMINAL_OUTPUT_MAX_BYTES, marks
// RemoteTerminalForwardState::output_truncated, and then propagates
// that flag onto the terminal response when the `complete` frame
// arrives without it.
// Guards against streaming a remote's oversized chunk straight to the
// client and against missing the truncation flag at completion.
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

// Pins that cap_terminal_response_output maintains independent
// per-stream budgets: when stdout is 500 KiB and stderr 100 KiB (each
// within its own TERMINAL_OUTPUT_MAX_BYTES budget), both survive
// unchanged and the function returns false (no truncation).
// Guards against a shared counter that would truncate stderr just
// because stdout was large.
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

// Pins that when both stdout and stderr exceed
// TERMINAL_OUTPUT_MAX_BYTES, cap_terminal_response_output truncates
// each independently to the budget (preserving the original bytes up
// to the cap) and returns true.
// Guards against only one stream being truncated or the function
// failing to report truncation when both are capped.
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

// Pins that a full-budget stdout frame followed by a small stderr
// frame still forwards both: stdout is allowed through at exactly its
// per-stream budget without tripping output_truncated, and the later
// stderr event is delivered rather than silently dropped by a shared
// byte counter.
// Guards against a regression to a single shared forward counter
// across stdout and stderr.
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

// Pins that forward_remote_terminal_stream_reader accepts a completion
// frame whose JSON encoding balloons well past
// 2 * TERMINAL_OUTPUT_MAX_BYTES (newline-heavy stdout produces \n
// escapes that roughly double the byte count) but stays within
// TERMINAL_REMOTE_SSE_PENDING_MAX_BYTES.
// Guards against the pending-frame cap becoming so tight that real,
// legal completion payloads are rejected.
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

// Pins that InterruptibleRemoteStreamReader::read correctly drains a
// single buffered chunk across multiple small destination buffers
// without losing or duplicating bytes (offset tracking + buffered
// clear).
// Guards against future callers with smaller scratch buffers silently
// dropping/repeating bytes through the partial-drain path.
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

// Pins that when the chunk channel is idle and cancellation flips,
// forward_remote_terminal_stream_reader returns BAD_GATEWAY with
// "terminal stream client disconnected" from the adapter-level
// recv_timeout poll.
// Guards against a cancellation signal being swallowed because the
// recv_timeout loop is not periodically re-checking it.
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

// Pins the production spawn path: when the worker thread is parked
// inside a blocking source read and cancellation flips,
// forward_remote_terminal_stream_reader still returns the disconnect
// error, and the worker thread is confirmed to have entered the real
// read_remote_stream_response path.
// Guards against a regression where the spawn adapter silently swallows
// cancellation while the worker sits inside reqwest's body read.
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

// Pins that forward_remote_terminal_stream_reader_capped rejects an
// unterminated frame once it exceeds the configured pending cap,
// returning a BAD_GATEWAY "remote terminal stream frame exceeded the
// allowed size" error rather than buffering forever.
// Guards against unbounded memory growth from a hostile or broken
// remote that never emits a frame delimiter.
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

// Pins that TERMINAL_REMOTE_SSE_PENDING_MAX_BYTES is at least
// 2 * TERMINAL_OUTPUT_MAX_BYTES + 4 KiB, i.e. large enough to fit a
// worst-case newline-heavy completion frame plus SSE envelope.
// Guards against accidentally shrinking the cap below what the
// acceptance test above needs, which would reintroduce valid-frame
// rejection.
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

// Pins that annotate_remote_terminal_429 prefixes "remote <name>: " to
// 429 messages (so the UI attributes throttling to the remote) but
// leaves other statuses like INTERNAL_SERVER_ERROR untouched.
// Guards against mis-annotating non-throttling errors or losing the
// annotation on legitimate throttling.
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

// Pins that /api/terminal/run forwards a command with maximum-length
// multibyte (combining-character-safe) content to the remote's
// /api/terminal/run and returns the remote's response, i.e. the
// proxy's character-length validation does not count bytes.
// Guards against rejecting legitimate unicode commands at the proxy
// layer due to byte-vs-char confusion.
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
