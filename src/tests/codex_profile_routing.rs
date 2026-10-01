//! Two-slot witnesses for Codex thread actions, delegation release and dispatch.
//! Each wire reports its profile; a request on the wrong wire fails before reply.

use super::delegation_support::finish_delegation_child_with_assistant_text;
use super::*;

struct TwoProfiles {
    state: AppState,
    commands: mpsc::Receiver<(SharedCodexProfile, CodexRuntimeCommand)>,
    _owners: Vec<phase_sync::ParkedProcess>,
}

impl TwoProfiles {
    fn new() -> Self {
        let state = test_app_state();
        let (tx, commands) = mpsc::channel();
        let mut owners = Vec::new();
        for (profile, id) in [
            (SharedCodexProfile::Default, "two-profile-default"),
            (SharedCodexProfile::ReadOnlySandbox, "two-profile-read-only"),
        ] {
            let owner = phase_sync::ParkedProcess::spawn();
            let (runtime, rx, _) =
                test_shared_codex_runtime_with_process(id, owner.process.clone());
            owners.push(owner);
            *state.shared_codex_runtime_slot(profile).lock().unwrap() = Some(runtime);
            let tx = tx.clone();
            std::thread::spawn(move || {
                while let Ok(command) = rx.recv() {
                    if tx.send((profile, command)).is_err() {
                        break;
                    }
                }
            });
        }
        Self {
            state,
            commands,
            _owners: owners,
        }
    }

    fn rpc<T: Send + 'static>(
        &self,
        profile: SharedCodexProfile,
        method: &str,
        thread: &str,
        reply: Value,
        action: impl FnOnce(AppState) -> T + Send + 'static,
    ) -> T {
        let state = self.state.clone();
        let action = std::thread::spawn(move || action(state));
        let (observed_profile, command) = phase_sync::receive(&self.commands, method);
        let CodexRuntimeCommand::JsonRpcRequest {
            method: observed_method,
            params,
            response_tx,
            ..
        } = command
        else {
            panic!("expected {method} on {profile:?}");
        };
        // Always release the caller before asserting, including a wrong-wire receipt.
        response_tx.send(Ok(reply)).unwrap();
        let result = action.join().expect("thread action should complete");
        assert_eq!(
            observed_profile, profile,
            "{method} reached the wrong app-server"
        );
        assert_eq!(observed_method, method);
        assert_eq!(params["threadId"], thread);
        assert!(
            self.commands.try_recv().is_err(),
            "one action must issue one request"
        );
        result
    }

    fn prompt(&self, profile: SharedCodexProfile, session_id: &str) {
        let (observed_profile, command) = phase_sync::receive(&self.commands, "profile prompt");
        assert_eq!(observed_profile, profile);
        assert!(
            matches!(command, CodexRuntimeCommand::Prompt { session_id: id, .. } if id == session_id)
        );
        assert!(self.commands.try_recv().is_err());
    }
}

#[test]
fn two_profiles_empty_read_only_slot_never_uses_the_default_wire() {
    let state = test_app_state();
    let (runtime, _, _) = test_shared_codex_runtime("only-default");
    *state
        .shared_codex_runtime_slot(SharedCodexProfile::Default)
        .lock()
        .unwrap() = Some(runtime);
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
        "only-default"
    );
}

#[test]
fn two_profiles_route_child_thread_actions_and_terminal_release() {
    let fixture = TwoProfiles::new();
    let state = &fixture.state;
    let parent = test_session_id(state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent,
            CreateDelegationRequest {
                prompt: "Inspect the tree".to_owned(),
                title: None,
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Explorer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .unwrap();
    let child = created.delegation.child_session_id;
    fixture.prompt(
        SharedCodexProfile::for_sandbox_mode(CodexSandboxMode::ReadOnly),
        &child,
    );
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&child).unwrap();
        let record = &mut inner.sessions[index];
        record.session.status = SessionStatus::Idle;
        // Pin explicitly so the two-home witness also runs on non-Windows hosts.
        record.codex_thread_profile = Some(SharedCodexProfile::ReadOnlySandbox);
        record.runtime = SessionRuntime::Codex(
            spawn_codex_runtime(
                state.clone(),
                child.clone(),
                record.session.workdir.clone(),
                SharedCodexProfile::ReadOnlySandbox,
            )
            .unwrap(),
        );
        set_record_external_session_id(record, Some("read-only-thread".to_owned()));
    }
    let fork_child = child.clone();
    let fork = fixture.rpc(
        SharedCodexProfile::ReadOnlySandbox,
        "thread/fork",
        "read-only-thread",
        json!({"thread":{"id":"read-only-fork","turns":[]}}),
        move |state| state.fork_codex_thread(&fork_child).unwrap(),
    );
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fork.session_id).unwrap()];
        assert_eq!(
            record.codex_thread_profile,
            Some(SharedCodexProfile::ReadOnlySandbox)
        );
        assert_eq!(record.codex_sandbox_mode, CodexSandboxMode::ReadOnly);
    }
    // The fork is no longer a read-only delegated child, so its settings may change.
    // Its existing thread must still use the source's home after that change.
    state
        .update_session_settings(
            &fork.session_id,
            super::codex_read_only_profile::codex_session_update(CodexSandboxMode::WorkspaceWrite),
        )
        .unwrap();
    // Drive an action on the fork itself: the pin must affect routing, not only storage.
    for (id, thread) in [
        (child.clone(), "read-only-thread"),
        (fork.session_id, "read-only-fork"),
    ] {
        let archived = id.clone();
        fixture.rpc(
            SharedCodexProfile::ReadOnlySandbox,
            "thread/archive",
            thread,
            json!({}),
            move |state| state.archive_codex_thread(&archived).unwrap(),
        );
        fixture.rpc(
            SharedCodexProfile::ReadOnlySandbox,
            "thread/unarchive",
            thread,
            json!({}),
            move |state| state.unarchive_codex_thread(&id).unwrap(),
        );
    }
    state
        .set_external_session_id(&parent, "default-thread".to_owned())
        .unwrap();
    let fork_parent = parent.clone();
    let default_fork = fixture.rpc(
        SharedCodexProfile::Default,
        "thread/fork",
        "default-thread",
        json!({"thread":{"id":"default-fork","turns":[]}}),
        move |state| state.fork_codex_thread(&fork_parent).unwrap(),
    );
    {
        let inner = state.inner.lock().unwrap();
        assert_eq!(
            inner.sessions[inner.find_session_index(&default_fork.session_id).unwrap()]
                .codex_thread_profile,
            Some(SharedCodexProfile::Default),
        );
    }
    let archive_parent = parent.clone();
    fixture.rpc(
        SharedCodexProfile::Default,
        "thread/archive",
        "default-thread",
        json!({}),
        move |state| state.archive_codex_thread(&archive_parent).unwrap(),
    );
    fixture.rpc(
        SharedCodexProfile::Default,
        "thread/unarchive",
        "default-thread",
        json!({}),
        move |state| state.unarchive_codex_thread(&parent).unwrap(),
    );

    finish_delegation_child_with_assistant_text(
        state,
        &child,
        "## Result\nStatus: completed\n\nSummary:\nFinished.",
    );
    state.refresh_delegation_for_child_session(&child).unwrap();
    let release_child = child.clone();
    let outcome = fixture.rpc(
        SharedCodexProfile::ReadOnlySandbox,
        "thread/archive",
        "read-only-thread",
        json!({}),
        move |state| {
            state
                .wait_for_codex_child_release(&release_child)
                .unwrap()
                .unwrap()
        },
    );
    assert_eq!(outcome, CodexReleaseOutcome::Archived);
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&child).unwrap()];
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert_eq!(
        record.session.codex_thread_state,
        Some(CodexThreadState::Archived)
    );
    assert_eq!(
        inner.delegations[inner.find_delegation_index(&created.delegation.id).unwrap()].status,
        DelegationStatus::Completed
    );
    assert!(
        state
            .running_shared_codex_runtime(SharedCodexProfile::Default)
            .is_some()
    );
    assert!(
        state
            .running_shared_codex_runtime(SharedCodexProfile::ReadOnlySandbox)
            .is_some()
    );
}

#[test]
fn two_profiles_wrong_wire_negative_control_is_rejected() {
    let fixture = TwoProfiles::new();
    let state = &fixture.state;
    let id = test_session_id(state, Agent::Codex);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        let record = &mut inner.sessions[index];
        record.codex_thread_profile = Some(SharedCodexProfile::ReadOnlySandbox);
        set_record_external_session_id(record, Some("negative-control-thread".to_owned()));
    }
    // Deliberately send read-only-home lookups to the default wire. Both wires
    // remain distinguishable by the forwarders' receipts, even with this alias.
    let default = state
        .running_shared_codex_runtime(SharedCodexProfile::Default)
        .unwrap();
    *state
        .shared_codex_runtime_slot(SharedCodexProfile::ReadOnlySandbox)
        .lock()
        .unwrap() = Some(default);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        fixture.rpc(
            SharedCodexProfile::ReadOnlySandbox,
            "thread/archive",
            "negative-control-thread",
            json!({}),
            move |state| state.archive_codex_thread(&id).unwrap(),
        )
    }));
    let panic = result
        .err()
        .expect("the deliberately wrong wire must fail the witness");
    assert!(
        panic
            .downcast_ref::<String>()
            .is_some_and(|message| message.contains("reached the wrong app-server"))
    );
}

#[cfg(windows)]
#[test]
fn two_profiles_thread_less_switch_dispatches_and_respawns_in_the_new_home() {
    for (before, after) in [
        (CodexSandboxMode::WorkspaceWrite, CodexSandboxMode::ReadOnly),
        (CodexSandboxMode::ReadOnly, CodexSandboxMode::WorkspaceWrite),
    ] {
        let fixture = TwoProfiles::new();
        let state = &fixture.state;
        let id = test_session_id(state, Agent::Codex);
        let previous = SharedCodexProfile::for_sandbox_mode(before);
        let next = SharedCodexProfile::for_sandbox_mode(after);
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&id).unwrap();
            let record = &mut inner.sessions[index];
            record.codex_sandbox_mode = before;
            record.session.sandbox_mode = Some(before);
            record.runtime = SessionRuntime::Codex(
                spawn_codex_runtime(
                    state.clone(),
                    id.clone(),
                    record.session.workdir.clone(),
                    previous,
                )
                .unwrap(),
            );
            record.attach_to_shared_codex_profile(previous);
        }
        state
            .update_session_settings(
                &id,
                super::codex_read_only_profile::codex_session_update(after),
            )
            .unwrap();
        dispatch_turn_and_snapshot(
            state,
            &id,
            SendMessageRequest {
                text: "First thread".to_owned(),
                expanded_text: None,
                attachments: vec![],
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .unwrap();
        fixture.prompt(next, &id);
        {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
            assert_eq!(record.codex_thread_profile, Some(next));
            let SessionRuntime::Codex(handle) = &record.runtime else {
                panic!("runtime must attach")
            };
            assert_eq!(
                state.shared_codex_profile_holding(&handle.runtime_id),
                Some(next)
            );
        }
        // Force the ordinary idle detach/respawn path without inventing a thread id.
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&id).unwrap();
            let record = &mut inner.sessions[index];
            record.session.status = SessionStatus::Idle;
            record.runtime_reset_required = true;
        }
        dispatch_turn_and_snapshot(
            state,
            &id,
            SendMessageRequest {
                text: "Attach again".to_owned(),
                expanded_text: None,
                attachments: vec![],
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .unwrap();
        fixture.prompt(next, &id);
        let inner = state.inner.lock().unwrap();
        assert_eq!(
            inner.sessions[inner.find_session_index(&id).unwrap()].codex_thread_profile,
            Some(next)
        );
    }
}
