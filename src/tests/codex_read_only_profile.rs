// Owns the tests of what a read-only Codex child relies on: the read-only
// sandbox it is given, the app-server profile that sandbox selects (a
// separate app-server on Windows, whose PATH holds no PowerShell 7), and the
// routing of sessions, thread requests and runtime exits to the app-server
// of their profile. Does not own delegation lifecycle or release behaviour
// (delegations.rs, codex_delegation_release.rs), nor the live probe that the
// sandbox refuses a write (recorded on the tracker item). New file.
use super::*;

fn delegation_child_sandbox(write_policy: DelegationWritePolicy) -> SessionRecord {
    let mut inner = StateInner::new();
    let mut record = inner.create_session(
        Agent::Codex,
        Some("Codex child".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    configure_delegation_child_prompt_settings(
        &mut record,
        DelegationMode::Reviewer,
        &write_policy,
    );
    record
}

// A read-only Codex child is given Codex's read-only sandbox on every
// platform, and a writable child keeps workspace-write. Before the fix a
// Windows read-only child ran danger-full-access, so nothing enforced it.
#[test]
fn a_read_only_codex_child_gets_the_read_only_sandbox_and_its_app_server_profile() {
    let read_only = delegation_child_sandbox(DelegationWritePolicy::ReadOnly);
    assert_eq!(read_only.codex_sandbox_mode, CodexSandboxMode::ReadOnly);
    assert_eq!(
        read_only.session.sandbox_mode,
        Some(CodexSandboxMode::ReadOnly)
    );
    assert_eq!(read_only.codex_approval_policy, CodexApprovalPolicy::Never);
    let expected_profile = if cfg!(windows) {
        SharedCodexProfile::ReadOnlySandbox
    } else {
        SharedCodexProfile::Default
    };
    assert_eq!(read_only.shared_codex_profile(), expected_profile);

    let writable = delegation_child_sandbox(DelegationWritePolicy::SharedWorktree {
        owned_paths: vec!["src".to_owned()],
    });
    assert_eq!(
        writable.codex_sandbox_mode,
        CodexSandboxMode::WorkspaceWrite
    );
    assert_eq!(writable.shared_codex_profile(), SharedCodexProfile::Default);

    // Every other sandbox mode stays on the default app-server.
    for mode in [
        CodexSandboxMode::WorkspaceWrite,
        CodexSandboxMode::DangerFullAccess,
    ] {
        assert_eq!(
            SharedCodexProfile::for_sandbox_mode(mode),
            SharedCodexProfile::Default
        );
    }
    // The two profiles keep their Codex threads in separate homes.
    assert_ne!(
        SharedCodexProfile::Default.codex_home_scope(),
        SharedCodexProfile::ReadOnlySandbox.codex_home_scope()
    );
}

// The read-only app-server's PATH leaves out every directory that holds a
// PowerShell 7, including a Store app-execution alias, and keeps the rest in
// order.
#[test]
fn the_read_only_app_server_path_holds_no_powershell_7() {
    let temp = TestTempRoot::create("codex-read-only-path");
    let root = temp.path();
    let store_alias = root.join("WindowsApps");
    let msi_install = root.join("PowerShell").join("7");
    let git = root.join("Git").join("cmd");
    let system = root.join("WindowsPowerShell").join("v1.0");
    for dir in [&store_alias, &msi_install, &git, &system] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::write(store_alias.join("pwsh.exe"), b"").unwrap();
    fs::write(msi_install.join("pwsh.exe"), b"").unwrap();
    fs::write(system.join("powershell.exe"), b"").unwrap();
    let path =
        std::env::join_paths([&store_alias, &git, &msi_install, &system]).expect("joinable path");

    let filtered = path_without_pwsh(&path, directory_holds_pwsh).expect("rebuilt path");
    let kept = std::env::split_paths(&filtered).collect::<Vec<_>>();
    assert_eq!(kept, vec![git.clone(), system.clone()]);
}

// Sessions, per-thread requests and runtime exits go to the app-server of
// their profile, and one profile's exit leaves the other's app-server alone.
#[test]
fn each_profile_keeps_its_own_app_server() {
    let state = test_app_state();
    let (default_runtime, default_rx, _) = test_shared_codex_runtime("default-app-server");
    let (read_only_runtime, read_only_rx, _) = test_shared_codex_runtime("read-only-app-server");
    *state.shared_codex_runtime.lock().unwrap() = Some(default_runtime);
    *state.shared_codex_read_only_runtime.lock().unwrap() = Some(read_only_runtime);

    let default_handle = spawn_codex_runtime(
        state.clone(),
        "session-default".to_owned(),
        "/tmp".to_owned(),
        SharedCodexProfile::Default,
    )
    .unwrap();
    let read_only_handle = spawn_codex_runtime(
        state.clone(),
        "session-read-only".to_owned(),
        "/tmp".to_owned(),
        SharedCodexProfile::ReadOnlySandbox,
    )
    .unwrap();
    assert_eq!(default_handle.runtime_id, "default-app-server");
    assert_eq!(read_only_handle.runtime_id, "read-only-app-server");
    assert_eq!(
        state.shared_codex_profile_holding("read-only-app-server"),
        Some(SharedCodexProfile::ReadOnlySandbox)
    );

    // A thread request for the read-only profile reaches only its app-server.
    let request = std::thread::spawn({
        let state = state.clone();
        move || {
            state.perform_codex_json_rpc_request_for(
                SharedCodexProfile::ReadOnlySandbox,
                "thread/list",
                json!({}),
                Duration::from_secs(5),
            )
        }
    });
    match read_only_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the read-only app-server receives the request")
    {
        CodexRuntimeCommand::JsonRpcRequest {
            method,
            response_tx,
            ..
        } => {
            assert_eq!(method, "thread/list");
            response_tx.send(Ok(json!({"data": []}))).unwrap();
        }
        _ => panic!("expected a JSON-RPC request"),
    }
    request.join().unwrap().unwrap();
    assert!(
        default_rx.try_recv().is_err(),
        "the default app-server got nothing"
    );

    // The read-only app-server's exit clears only its own slot.
    state
        .handle_shared_codex_runtime_exit("read-only-app-server", None)
        .unwrap();
    assert!(
        state
            .shared_codex_read_only_runtime
            .lock()
            .unwrap()
            .is_none()
    );
    assert_eq!(
        state
            .shared_codex_runtime
            .lock()
            .unwrap()
            .as_ref()
            .map(|runtime| runtime.runtime_id.clone()),
        Some("default-app-server".to_owned())
    );
}

pub(super) fn codex_session_update(sandbox_mode: CodexSandboxMode) -> UpdateSessionSettingsRequest {
    UpdateSessionSettingsRequest {
        kimi_effort: None,
        opencode_approval_mode: None,
        kimi_approval_mode: None,
        kimi_mode: None,
        name: None,
        model: None,
        sandbox_mode: Some(sandbox_mode),
        approval_policy: None,
        reasoning_effort: None,
        cursor_mode: None,
        claude_approval_mode: None,
        claude_effort: None,
        gemini_approval_mode: None,
        opencode_effort: None,
        opencode_mode: None,
        codex_fast_mode: None,
    }
}

// A thread stays with the app-server whose Codex home holds it, whatever its
// sandbox mode later says: the profile is fixed when a session without a
// thread attaches, in both directions, and a thread from before profiles
// existed stays on the default app-server.
#[test]
fn a_threads_profile_follows_its_home_not_its_current_sandbox() {
    let read_only_profile = SharedCodexProfile::for_sandbox_mode(CodexSandboxMode::ReadOnly);

    // Writable, attached, thread created; then switched to read-only.
    let mut writable = delegation_child_sandbox(DelegationWritePolicy::SharedWorktree {
        owned_paths: vec!["src".to_owned()],
    });
    writable.attach_to_shared_codex_profile(writable.shared_codex_profile());
    set_record_external_session_id(&mut writable, Some("thread-writable".to_owned()));
    writable.codex_sandbox_mode = CodexSandboxMode::ReadOnly;
    assert_eq!(writable.shared_codex_profile(), SharedCodexProfile::Default);

    // Read-only, attached, thread created; then switched to writable.
    let mut read_only = delegation_child_sandbox(DelegationWritePolicy::ReadOnly);
    read_only.attach_to_shared_codex_profile(read_only.shared_codex_profile());
    set_record_external_session_id(&mut read_only, Some("thread-read-only".to_owned()));
    read_only.codex_sandbox_mode = CodexSandboxMode::WorkspaceWrite;
    assert_eq!(read_only.shared_codex_profile(), read_only_profile);
    // Attaching again does not move an existing thread.
    read_only.attach_to_shared_codex_profile(SharedCodexProfile::Default);
    assert_eq!(read_only.shared_codex_profile(), read_only_profile);

    // A thread recorded before profiles existed lives in the default home.
    let mut legacy = delegation_child_sandbox(DelegationWritePolicy::ReadOnly);
    set_record_external_session_id(&mut legacy, Some("thread-legacy".to_owned()));
    legacy.codex_thread_profile = None;
    assert_eq!(legacy.shared_codex_profile(), SharedCodexProfile::Default);

    // The pinned profile survives persistence.
    let reloaded = PersistedSessionRecord::from_record(&read_only)
        .into_record()
        .expect("record reloads");
    assert_eq!(
        reloaded.codex_thread_profile,
        read_only.codex_thread_profile
    );
    assert_eq!(reloaded.shared_codex_profile(), read_only_profile);
}

// On Windows, an existing thread on the default app-server cannot take the
// read-only sandbox; the change is refused before anything is changed. A
// session without a thread may switch, and elsewhere the switch is allowed.
#[test]
fn switching_an_existing_default_thread_to_read_only_is_refused_on_windows() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session_id).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.codex_sandbox_mode = CodexSandboxMode::WorkspaceWrite;
        set_record_external_session_id(record, Some("thread-on-default".to_owned()));
        record.codex_thread_profile = Some(SharedCodexProfile::Default);
    }
    let result = state.update_session_settings(
        &session_id,
        codex_session_update(CodexSandboxMode::ReadOnly),
    );
    let sandbox_after = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session_id).unwrap()].codex_sandbox_mode
    };
    if cfg!(windows) {
        let error = result.err().expect("the switch is refused on Windows");
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(
            error.message.contains("new Codex session"),
            "{}",
            error.message
        );
        assert_eq!(sandbox_after, CodexSandboxMode::WorkspaceWrite);
    } else {
        result.expect("the switch is allowed off Windows");
        assert_eq!(sandbox_after, CodexSandboxMode::ReadOnly);
    }

    // Without a thread, the switch is allowed everywhere.
    let fresh = test_session_id(&state, Agent::Codex);
    state
        .update_session_settings(&fresh, codex_session_update(CodexSandboxMode::ReadOnly))
        .expect("a session without a thread may switch");
}

// The profile guard refuses only a real change: re-sending the current
// sandbox mode (the UI sends it with every settings change) is accepted, even
// for a thread whose recorded mode is read-only on the default app-server. A
// refused request leaves the whole record as it was, its name included.
#[test]
fn an_unchanged_sandbox_mode_is_accepted_and_a_refusal_changes_nothing() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session_id).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        // A thread from before profiles existed, recorded as read-only.
        record.codex_sandbox_mode = CodexSandboxMode::ReadOnly;
        record.session.sandbox_mode = Some(CodexSandboxMode::ReadOnly);
        set_record_external_session_id(record, Some("thread-legacy".to_owned()));
        record.codex_thread_profile = None;
    }
    let mut unchanged = codex_session_update(CodexSandboxMode::ReadOnly);
    unchanged.name = Some("Renamed".to_owned());
    state
        .update_session_settings(&session_id, unchanged)
        .expect("re-sending the current sandbox mode is accepted");

    // A real crossing change on a default-app-server thread is refused, and
    // the rename that came with it is not applied.
    let fresh = test_session_id(&state, Agent::Codex);
    let name_before = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&fresh).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.codex_sandbox_mode = CodexSandboxMode::WorkspaceWrite;
        record.session.sandbox_mode = Some(CodexSandboxMode::WorkspaceWrite);
        set_record_external_session_id(record, Some("thread-on-default".to_owned()));
        record.codex_thread_profile = Some(SharedCodexProfile::Default);
        record.session.name.clone()
    };
    let mut crossing = codex_session_update(CodexSandboxMode::ReadOnly);
    crossing.name = Some("Must not stick".to_owned());
    let result = state.update_session_settings(&fresh, crossing);
    let name_after = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&fresh).unwrap()]
            .session
            .name
            .clone()
    };
    if cfg!(windows) {
        assert!(result.is_err());
        assert_eq!(
            name_after, name_before,
            "a refusal must not rename the session"
        );
    } else {
        result.expect("off Windows the switch is allowed");
    }
}

// A session without a thread may cross between the read-only sandbox and
// another mode only while no turn runs: a running turn may be creating its
// first thread on the current app-server.
#[test]
fn a_thread_less_session_cannot_cross_profiles_while_a_turn_runs() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let set_status = |status: SessionStatus| {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session_id).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.codex_sandbox_mode = CodexSandboxMode::WorkspaceWrite;
        record.session.sandbox_mode = Some(CodexSandboxMode::WorkspaceWrite);
        record.session.status = status;
    };

    set_status(SessionStatus::Active);
    let busy = state.update_session_settings(
        &session_id,
        codex_session_update(CodexSandboxMode::ReadOnly),
    );
    if cfg!(windows) {
        let error = busy.err().expect("a crossing switch waits for the turn");
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(
            error.message.contains("current Codex turn"),
            "{}",
            error.message
        );
    } else {
        busy.expect("off Windows no profile is crossed");
    }

    set_status(SessionStatus::Idle);
    state
        .update_session_settings(
            &session_id,
            codex_session_update(CodexSandboxMode::ReadOnly),
        )
        .expect("an idle session without a thread may switch");
}

// Startup discovery never imports a thread from the read-only home: the
// default app-server could not resume it.
#[test]
fn discovery_skips_the_read_only_codex_home() {
    let temp = TestTempRoot::create("codex-read-only-discovery");
    let root = temp.path();
    for scope in [
        SharedCodexProfile::Default.codex_home_scope(),
        SharedCodexProfile::ReadOnlySandbox.codex_home_scope(),
        "repl",
        "other-scope",
    ] {
        fs::create_dir_all(root.join(scope)).unwrap();
    }
    let homes = discover_codex_home_candidates(None, root);
    let names = homes
        .iter()
        .filter_map(|home| home.file_name().and_then(|name| name.to_str()))
        .collect::<Vec<_>>();
    assert!(names.contains(&SharedCodexProfile::Default.codex_home_scope()));
    assert!(names.contains(&"other-scope"));
    assert!(!names.contains(&SharedCodexProfile::ReadOnlySandbox.codex_home_scope()));
    assert!(!names.contains(&"repl"));
}
