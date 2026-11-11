//! Archive ownership across SQLite reload, exercised through release and follow-up.
use super::delegation_support::{
    boot_on_delegation_root, finish_delegation_child_with_assistant_text,
};
use super::home_fixture::BootState;
use super::*;

#[test]
fn terminal_release_archive_survives_reload_and_allows_followup() {
    let (original, parent, delegation, child) = archived_child(true);
    let reloaded = reload(&original);
    let wire = Wire::install(&reloaded, "terminal-release-reloaded");

    let result = reloaded.followup_delegation(&parent, &delegation, "Continue.".to_owned());
    if result.is_ok() {
        wire.expect("thread/unarchive");
        wire.expect("prompt");
    }
    let commands = wire.finish();
    assert!(
        result.is_ok(),
        "TermAl's terminal archive must remain resumable after SQLite reload: {:?}",
        result.err().map(|error| error.message)
    );
    assert_eq!(commands, ["thread/unarchive", "prompt"]);
    let inner = reloaded.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&child).unwrap()];
    assert!(!record_has_archived_codex_thread(record));
    assert_eq!(
        inner.delegations[inner.find_delegation_index(&delegation).unwrap()].status,
        DelegationStatus::Running
    );
}

#[test]
fn manual_archive_survives_reload_and_still_refuses_followup() {
    let (original, parent, delegation, child) = archived_child(false);
    assert_refused_after_reload(&original, &parent, &delegation, &child);
}

#[test]
fn manual_archive_after_automatic_restore_does_not_inherit_release_ownership() {
    let (original, parent, delegation, child) = archived_child(true);
    let wire = Wire::install(&original, "manual-cycle");
    original.unarchive_codex_thread(&child).unwrap();
    wire.expect("thread/unarchive");
    original.archive_codex_thread(&child).unwrap();
    wire.expect("thread/archive");
    assert_eq!(wire.finish(), ["thread/unarchive", "thread/archive"]);
    assert_refused_after_reload(&original, &parent, &delegation, &child);
}

#[test]
fn replacing_the_thread_does_not_transfer_terminal_archive_ownership() {
    let (original, parent, delegation, child) = archived_child(true);
    original
        .set_external_session_id(&child, "replacement-thread".to_owned())
        .unwrap();
    assert_refused_after_reload(&original, &parent, &delegation, &child);
}

#[test]
fn same_thread_id_in_another_home_does_not_inherit_terminal_archive_ownership() {
    let (original, parent, delegation, child) = archived_child(true);
    {
        let mut inner = original.inner.lock().unwrap();
        let index = inner.find_session_index(&child).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.codex_thread_profile = Some(match record.codex_thread_profile {
            Some(SharedCodexProfile::ReadOnlySandbox) => SharedCodexProfile::Default,
            _ => SharedCodexProfile::ReadOnlySandbox,
        });
        original.commit_locked(&mut inner).unwrap();
    }
    assert_refused_after_reload(&original, &parent, &delegation, &child);
}

#[test]
fn legacy_archive_without_provenance_still_requires_manual_unarchive() {
    let (original, parent, delegation, child) = archived_child(true);
    // Emulate the older durable row, which has no archive-origin metadata.
    let connection = rusqlite::Connection::open(original.persistence_path.as_path()).unwrap();
    assert_eq!(connection.execute(
        "UPDATE sessions SET value_json=json_remove(value_json,'$.codexTerminalArchive') WHERE id=?1",
        [&child],
    ).unwrap(), 1);
    drop(connection);
    assert_refused_after_reload(&original, &parent, &delegation, &child);
}

fn assert_refused_after_reload(original: &AppState, parent: &str, delegation: &str, child: &str) {
    let reloaded = reload(original);
    let wire = Wire::install(&reloaded, "manual-archive-reloaded");

    let result = reloaded.followup_delegation(parent, delegation, "Continue.".to_owned());
    if result.is_ok() {
        wire.expect("thread/unarchive");
        wire.expect("prompt");
    }
    let commands = wire.finish();
    let error = result
        .err()
        .expect("manual Archive must still require Unarchive");
    assert_eq!(error.status, StatusCode::CONFLICT, "{}", error.message);
    assert!(
        error
            .message
            .contains("archived manually; unarchive it before sending another prompt")
    );
    assert!(
        commands.is_empty(),
        "a refusal must send no RPC or prompt: {commands:?}"
    );
    let inner = reloaded.inner.lock().unwrap();
    assert!(record_has_archived_codex_thread(
        &inner.sessions[inner.find_session_index(child).unwrap()]
    ));
    assert_eq!(
        inner.delegations[inner.find_delegation_index(delegation).unwrap()].status,
        DelegationStatus::Completed
    );
}

fn archived_child(automatic: bool) -> (AppState, String, String, String) {
    let state = test_app_state();
    let wire = Wire::install(&state, "archive-before-reload");
    let parent = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent,
            CreateDelegationRequest {
                prompt: "Inspect this fixture.".to_owned(),
                title: None,
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
    wire.expect("prompt");
    if automatic {
        state
            .set_external_session_id(&child, "restart-thread".to_owned())
            .unwrap();
    }
    finish_delegation_child_with_assistant_text(
        &state,
        &child,
        "## Result\nStatus: completed\n\nSummary:\nFinished.",
    );
    state.refresh_delegation_for_child_session(&child).unwrap();
    if automatic {
        assert_eq!(
            state.wait_for_codex_child_release(&child).unwrap(),
            Some(CodexReleaseOutcome::Archived)
        );
    } else {
        // No thread existed at terminal release. This archive is performed by
        // the real manual action, never by a fabricated release barrier.
        state
            .set_external_session_id(&child, "restart-thread".to_owned())
            .unwrap();
        state.archive_codex_thread(&child).unwrap();
    }
    wire.expect("thread/archive");
    assert_eq!(wire.finish(), ["prompt", "thread/archive"]);
    let inner = state.inner.lock().unwrap();
    assert!(record_has_archived_codex_thread(
        &inner.sessions[inner.find_session_index(&child).unwrap()]
    ));
    // The test constructor has no async writer: its production commit fallback
    // writes SQLite. Confirm that the next host will load the archived thread.
    drop(inner);
    let disk = load_state(&state.persistence_path).unwrap().unwrap();
    assert!(record_has_archived_codex_thread(
        &disk.sessions[disk.find_session_index(&child).unwrap()]
    ));
    (state, parent, delegation, child)
}

fn reload(original: &AppState) -> BootState {
    let root = original.test_temp_root.as_ref().unwrap().clone();
    let mut reloaded = boot_on_delegation_root(&root, || {
        AppState::new_before_listen_with_paths(
            original.default_workdir.clone(),
            original.persistence_path.as_ref().clone(),
            original.orchestrator_templates_path.as_ref().clone(),
        )
        .expect("SQLite host reload")
    });
    // No external server may be launched by a missing fixture response.
    reloaded.agent_runtime_spawning_enabled = false;
    let reloaded = BootState::new(reloaded);
    // Pre-listen boot deliberately fences delegation routes until the actual
    // SQLite inventory has been loaded and published by its startup owner.
    reloaded
        .load_delegation_inventory_after_listen()
        .expect("delegation inventory startup must complete before follow-up");
    reloaded
}

/// Scripted app-server wire; stop and join even when an assertion unwinds.
struct Wire {
    runtime: SharedCodexRuntime,
    observed: mpsc::Receiver<String>,
    worker: Option<std::thread::JoinHandle<Vec<String>>>,
}

impl Wire {
    fn install(state: &AppState, id: &str) -> Self {
        let (runtime, input, _) = test_shared_codex_runtime(id);
        install_single_wire_codex_fixture(state, Some(runtime.clone()));
        let (observed_tx, observed) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut commands = Vec::new();
            while let Ok(command) = input.recv() {
                let method = match command {
                    CodexRuntimeCommand::JsonRpcRequest {
                        method,
                        params,
                        response_tx,
                        ..
                    } => {
                        assert_eq!(params["threadId"], "restart-thread");
                        let _ = response_tx.send(Ok(json!({})));
                        method
                    }
                    CodexRuntimeCommand::Prompt { .. } => "prompt".to_owned(),
                    CodexRuntimeCommand::JsonRpcNotification { method }
                        if method == "fixture/stop" =>
                    {
                        break;
                    }
                    _ => panic!("unexpected fixture command"),
                };
                commands.push(method.clone());
                let _ = observed_tx.send(method);
            }
            commands
        });
        Self {
            runtime,
            observed,
            worker: Some(worker),
        }
    }

    fn expect(&self, method: &str) {
        assert_eq!(
            phase_sync::receive(&self.observed, "archive/follow-up wire event"),
            method
        );
    }

    fn finish(mut self) -> Vec<String> {
        self.stop();
        self.worker
            .take()
            .unwrap()
            .join()
            .expect("fixture wire must finish")
    }

    fn stop(&self) {
        let _ = self
            .runtime
            .input_tx
            .send(CodexRuntimeCommand::JsonRpcNotification {
                method: "fixture/stop".to_owned(),
            });
    }
}

impl Drop for Wire {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            self.stop();
            let _ = worker.join();
        }
    }
}
