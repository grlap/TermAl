//! Shared Codex thread setup, parking, and request-configuration tests.
//!
//! Owns the app-server setup handshake before a thread is bound, including
//! parked prompts, setup-slot release, and Engram shell-environment overlays.

use super::*;

pub(super) fn answer_engram_config_for_test(
    pending: &CodexPendingRequestMap,
    response: std::result::Result<Value, CodexResponseError>,
) {
    let mut requests = pending.lock().unwrap();
    assert_eq!(requests.len(), 1, "only config/read is outstanding");
    let (_, sender) = requests.drain().next().unwrap();
    sender.send(response).unwrap();
}

pub(super) fn finish_engram_config_for_test(
    state: &AppState,
    runtime: &SharedCodexRuntime,
    pending: &CodexPendingRequestMap,
    input_tx: &Sender<CodexRuntimeCommand>,
    input_rx: &mpsc::Receiver<CodexRuntimeCommand>,
    writer: &mut Vec<u8>,
    response: Value,
) {
    answer_engram_config_for_test(pending, Ok(response));
    let command = recv_within_guard(input_rx, "config/read should queue thread/start").unwrap();
    run_engram_config_continuation_for_test(state, runtime, pending, input_tx, writer, command);
}

pub(super) fn run_engram_config_continuation_for_test(
    state: &AppState,
    runtime: &SharedCodexRuntime,
    pending: &CodexPendingRequestMap,
    input_tx: &Sender<CodexRuntimeCommand>,
    writer: &mut Vec<u8>,
    command: CodexRuntimeCommand,
) {
    let CodexRuntimeCommand::StartThreadAfterConfig {
        session_id,
        request_id,
        params,
    } = command
    else {
        panic!("expected resolved config continuation");
    };
    handle_shared_codex_start_thread_after_config(
        writer,
        pending,
        state,
        &runtime.runtime_id,
        &runtime.sessions,
        &runtime.thread_sessions,
        input_tx,
        None,
        &session_id,
        request_id,
        params,
    )
    .unwrap();
}

/// An in-flight Codex thread setup carrying a parked prompt.
///
/// A setup always owns the prompt that opened it, so tests cannot construct one
/// without a command — which is the point of collapsing the two into one value.
pub(super) fn test_pending_codex_thread_setup(request_id: &str) -> PendingCodexThreadSetup {
    PendingCodexThreadSetup {
        request_id: request_id.to_owned(),
        command: CodexPromptCommand {
            active_turn_generation: 0,
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "parked prompt".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    }
}

pub(super) fn create_test_engram_codex_session(state: &AppState, suffix: &str) -> String {
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join(suffix);
    fs::create_dir_all(&root).expect("Engram test project root should exist");
    fs::write(root.join(".engram-project"), "fixture-ready\n")
        .expect("Engram test repository should be declared");
    let project_id = create_test_project(state, &root, "Shared Codex Engram project");
    let session_id = create_test_project_session(state, Agent::Codex, &project_id, &root);
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    inner
        .projects
        .iter_mut()
        .find(|project| project.id == project_id)
        .expect("Engram test project should exist")
        .engram = Some(EngramProjectSettings {
        acceptance_evaluation: None,
        enabled: true,
        turn_gated_control: false,
        binary_path: Some("engram".to_owned()),
        home: Some("test-engram-home".to_owned()),
        work_authority_grant: None,
        authority_store_key: None,
        deadline_ms: None,
    });
    inner.engram_declared_project_ids.insert(project_id.clone());
    inner
        .engram_declaration_checked_project_ids
        .insert(project_id);
    session_id
}

// Pins the duplicate-Codex-thread leak at its source: a prompt that arrives
// while a `thread/start` is still in flight must NOT start a second thread.
//
// `thread_id` is only populated once the setup response lands, so testing it
// alone made every command arriving in that window take the slow path and fire
// another `thread/start` — and the app-server writes a thread to disk for each
// one. Only one could ever be bound to the session; the rest leaked as phantom
// top-level "Ready to continue this Codex thread" sessions. Worse, a setup whose
// response never arrived (app-server timeout) left behind a thread whose id
// TermAl never learned, so it could not even be suppressed. A single delegation
// was observed minting eight threads this way, six of them unaccounted for.
//
// The later command must be parked on the in-flight setup instead, so the newest
// prompt still wins — matching the previous behaviour, where superseded waiters
// simply dropped their turns — while the app-server only ever creates one thread.
#[test]
fn prompt_during_codex_thread_setup_does_not_start_a_second_thread() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("codex-thread-setup-single-thread");

    // Bind the runtime to the session. Without this the setup waiter bails out at
    // `RuntimeMismatch` and never reaches the handoff — which is exactly the part
    // this test exists to pin.
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let (input_tx, input_rx) = mpsc::channel::<CodexRuntimeCommand>();
    let mut writer = Vec::new();

    let prompt_command = |prompt: &str| CodexPromptCommand {
        active_turn_generation: 0,
        approval_policy: CodexApprovalPolicy::Never,
        attachments: Vec::new(),
        cwd: "/tmp".to_owned(),
        model: "gpt-5.4".to_owned(),
        prompt: prompt.to_owned(),
        reasoning_effort: CodexReasoningEffort::Medium,
        service_tier: Some("priority".to_owned()),
        resume_thread_id: None,
        sandbox_mode: CodexSandboxMode::WorkspaceWrite,
    };

    // No thread yet: fires `thread/start` and parks the command on that setup.
    handle_shared_codex_prompt_command(
        &mut writer,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &test_missing_shared_codex_home(),
        &runtime.sessions,
        &runtime.thread_sessions,
        &input_tx,
        None,
        &session_id,
        prompt_command("first prompt"),
    )
    .unwrap();

    // The setup response is deliberately never delivered, so the request is still
    // in flight — exactly the window that used to mint extra threads.
    handle_shared_codex_prompt_command(
        &mut writer,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &test_missing_shared_codex_home(),
        &runtime.sessions,
        &runtime.thread_sessions,
        &input_tx,
        None,
        &session_id,
        prompt_command("second prompt"),
    )
    .unwrap();

    let written = String::from_utf8(writer).expect("writer output should be utf-8");
    let thread_starts = written.matches("thread/start").count();
    assert_eq!(
        thread_starts, 1,
        "a prompt arriving during thread setup must not mint a second Codex thread \
         (wrote {thread_starts} thread/start requests)"
    );
    assert!(
        written.contains("\"serviceTier\":\"priority\""),
        "thread/start should include the session-scoped Fast service tier\n{written}"
    );

    let parked = runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .get(&session_id)
        .and_then(|session_state| {
            session_state
                .pending_thread_setup
                .as_ref()
                .map(|setup| setup.command.prompt.clone())
        });
    assert_eq!(
        parked.as_deref(),
        Some("second prompt"),
        "the newest prompt should be the one the in-flight setup runs"
    );

    // Answer the setup. The waiter must bind the thread and hand back the prompt
    // that was PARKED on it, not the one that opened it.
    answer_pending_codex_thread_setups(&pending_requests, "thread-only-one");

    // The handoff is the half of this change that can fail silently: if the waiter
    // falls back to the command that opened the setup, the session answers the
    // OLDER prompt and nothing errors. Pin it end to end.
    let delivered = recv_within_guard(
        &input_rx,
        "thread setup should hand the parked prompt back as StartTurnAfterSetup",
    )
    .expect("thread setup should hand the parked prompt back as StartTurnAfterSetup");
    match delivered {
        CodexRuntimeCommand::StartTurnAfterSetup {
            session_id: delivered_session_id,
            thread_id,
            command,
        } => {
            assert_eq!(delivered_session_id, session_id);
            assert_eq!(
                thread_id, "thread-only-one",
                "the turn must run on the single thread the setup created"
            );
            assert_eq!(
                command.prompt, "second prompt",
                "the prompt parked during setup must be the one that runs — falling \
                 back to the command that opened the setup silently answers the wrong prompt"
            );
        }
        _ => panic!("expected StartTurnAfterSetup after thread setup completed"),
    }

    // Exactly one turn. A setup that delivered the parked prompt AND the one that
    // opened it would still satisfy the assertion above while running two turns.
    //
    // `try_recv` would be a false pass here: the second handoff comes from the same
    // async waiter and could land a moment later, so an immediate "nothing there"
    // proves nothing. Wait for one, and require the wait to TIME OUT.
    assert!(
        matches!(
            input_rx.recv_timeout(Duration::from_millis(500)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "the setup must hand back exactly one turn, not the parked prompt and the opener"
    );
}

// Pins the invariant the whole parking rule rests on: a stop/detach removes the
// entire shared-session entry, and with it any in-flight setup.
//
// This is WHY parking never has to compare thread identities. A prompt could only
// arrive wanting a different thread than the in-flight setup if something changed
// the session's thread identity — and everything that does (stop, kill, runtime
// teardown) goes through `interrupt_and_detach`, which calls `detach()`
// UNCONDITIONALLY, even when the interrupt itself fails. So there is never a stale
// setup left to park on.
//
// Earlier revisions compared `resume_thread_id` here and superseded on a mismatch.
// That machinery was guarding an unreachable state, and it is what produced a
// redundant `thread/resume` and made the superseded waiter permanently suppress the
// session's own LIVE thread. If this invariant ever breaks, parking becomes unsafe —
// so it gets its own test rather than living only in a comment.
#[test]
fn detach_removes_the_in_flight_thread_setup_so_the_next_prompt_starts_fresh() {
    let state = test_app_state();
    let session_id = create_test_engram_codex_session(&state, "codex-thread-setup-detach");
    let (runtime, _input_rx, process) = test_shared_codex_runtime("codex-thread-setup-detach");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
    }

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let (input_tx, input_rx) = mpsc::channel::<CodexRuntimeCommand>();
    let mut writer = Vec::new();

    let prompt_command = |prompt: &str, resume: Option<&str>| CodexPromptCommand {
        active_turn_generation: 0,
        approval_policy: CodexApprovalPolicy::AutoApprove,
        attachments: Vec::new(),
        cwd: "/tmp".to_owned(),
        model: "gpt-5.4".to_owned(),
        prompt: prompt.to_owned(),
        reasoning_effort: CodexReasoningEffort::Medium,
        service_tier: None,
        resume_thread_id: resume.map(str::to_owned),
        sandbox_mode: CodexSandboxMode::WorkspaceWrite,
    };

    // A `thread/resume` is in flight, with a prompt parked on it.
    handle_shared_codex_prompt_command(
        &mut writer,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &test_missing_shared_codex_home(),
        &runtime.sessions,
        &runtime.thread_sessions,
        &input_tx,
        None,
        &session_id,
        prompt_command("before stop", Some("thread-old")),
    )
    .unwrap();
    assert!(
        runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned")
            .get(&session_id)
            .and_then(|session_state| session_state.pending_thread_setup.as_ref())
            .is_some(),
        "a setup should be in flight before the detach"
    );

    // What a stop does — including the interrupt-FAILURE path, which still detaches.
    SharedCodexSessionHandle {
        runtime: runtime.clone(),
        session_id: session_id.clone(),
    }
    .detach();

    assert!(
        runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned")
            .get(&session_id)
            .is_none(),
        "detach must remove the whole shared-session entry: the parking rule assumes \
         no setup can survive a stop, which is precisely why it never compares \
         thread identities"
    );

    answer_pending_codex_thread_setups(&pending_requests, "thread-old");

    // So the next prompt starts a FRESH thread instead of parking on — and inheriting
    // the thread identity of — the setup the stop invalidated.
    handle_shared_codex_prompt_command(
        &mut writer,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &test_missing_shared_codex_home(),
        &runtime.sessions,
        &runtime.thread_sessions,
        &input_tx,
        None,
        &session_id,
        prompt_command("after stop", None),
    )
    .unwrap();

    finish_engram_config_for_test(
        &state,
        &runtime,
        &pending_requests,
        &input_tx,
        &input_rx,
        &mut writer,
        json!({"config": {}}),
    );
    let written = String::from_utf8(writer).expect("writer output should be utf-8");
    assert_eq!(
        written.matches("thread/resume").count(),
        1,
        "the first prompt resumed the pre-existing thread"
    );
    let resume_request = written
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|request| request["method"] == "thread/resume")
        .expect("the resume request should be valid JSON-RPC");
    assert_eq!(
        resume_request.pointer("/params/excludeTurns"),
        Some(&Value::Bool(true)),
        "resuming a long thread must request metadata only so its full turn history \
         cannot exceed the shared app-server stdout line cap"
    );
    assert_eq!(
        resume_request.pointer("/params/approvalPolicy"),
        Some(&json!("on-request")),
        "TermAl AutoApprove must keep native Codex approval requests enabled on resume"
    );
    assert_eq!(
        resume_request.pointer("/params/serviceTier"),
        Some(&Value::Null),
        "Standard must explicitly clear a service tier inherited by the resumed thread"
    );
    let resume_set = resume_request
        .pointer("/params/config/shell_environment_policy/set")
        .and_then(Value::as_object)
        .expect("thread/resume should carry a shell environment policy");
    assert_eq!(
        resume_set,
        &expected_codex_shell_environment_set(&resume_request["params"]["config"], &session_id),
        "thread/resume must carry exactly the seven owned identity values"
    );
    assert_eq!(
        written.matches("thread/start").count(),
        1,
        "after a detach there is no setup to park on, so the next prompt starts a FRESH thread"
    );
    let start_request = written
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|request| request["method"] == "thread/start")
        .expect("the start request should be valid JSON-RPC");
    assert_eq!(
        start_request.pointer("/params/serviceTier"),
        Some(&Value::Null),
        "Standard thread/start must serialize the service tier explicitly"
    );
    let start_set = start_request
        .pointer("/params/config/shell_environment_policy/set")
        .and_then(Value::as_object)
        .expect("thread/start should carry a shell environment policy");
    assert_eq!(
        start_set,
        &expected_codex_shell_environment_set(&start_request["params"]["config"], &session_id),
        "thread/start must carry exactly the seven owned identity values"
    );

    retire_pending_codex_thread_setups(&pending_requests);
}

// Pins the initialize handshake both Codex paths share. The
// `capabilities.experimentalApi` opt-in is what authorizes
// `thread/resume.excludeTurns`; shipping the resume param without the
// capability bricked every Codex session on 2026-07-23 with
// "excludeTurns requires experimentalApi capability" — including the
// implementer session that would have fixed it. One helper, one pin, no
// drift between the shared app-server and REPL handshakes.
#[test]
fn codex_initialize_params_declare_the_experimental_api_capability() {
    let params = codex_initialize_params();

    assert_eq!(
        params.pointer("/clientInfo/name"),
        Some(&Value::String("termal".to_owned()))
    );
    assert_eq!(
        params.pointer("/capabilities/experimentalApi"),
        Some(&Value::Bool(true)),
        "thread/resume.excludeTurns is rejected by the app-server unless the \
         initialize handshake opts into the experimental API"
    );
}

// Pins the `{setup in flight, thread bound}` window — the one the decision
// ordering exists for, and the one that broke.
//
// `thread/started` can arrive before the `thread/start` response. It does not just
// bind the thread in the shared map, it also PERSISTS `external_session_id`
// (`codex_events.rs`), and prompts take `resume_thread_id` from that record
// (`turn_dispatch.rs`). So the next prompt arrives asking to resume `T1` while the
// setup that is *creating* `T1` recorded `resume_thread_id: None`.
//
// Comparing those two raw values calls it a different target. The prompt then
// supersedes the setup: a redundant `thread/resume` for a thread already being
// created, and — far worse — the superseded waiter disowns `T1` as an orphan and
// adds the session's own LIVE thread to the persisted never-rediscover set. That
// is the phantom-session leak inverted: instead of importing threads that are
// dead, we permanently hide one that is alive.
//
// It must park.
#[test]
fn prompt_resuming_the_thread_its_own_setup_just_started_parks_instead_of_superseding() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, _process) =
        test_shared_codex_runtime("codex-thread-setup-early-started");

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let (input_tx, _dummy_input_rx) = mpsc::channel::<CodexRuntimeCommand>();
    let mut writer = Vec::new();

    let prompt_command = |prompt: &str, resume: Option<&str>| CodexPromptCommand {
        active_turn_generation: 0,
        approval_policy: CodexApprovalPolicy::Never,
        attachments: Vec::new(),
        cwd: "/tmp".to_owned(),
        model: "gpt-5.4".to_owned(),
        prompt: prompt.to_owned(),
        reasoning_effort: CodexReasoningEffort::Medium,
        service_tier: None,
        resume_thread_id: resume.map(str::to_owned),
        sandbox_mode: CodexSandboxMode::WorkspaceWrite,
    };

    // A fresh `thread/start` (no resume target) claims the setup.
    handle_shared_codex_prompt_command(
        &mut writer,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &test_missing_shared_codex_home(),
        &runtime.sessions,
        &runtime.thread_sessions,
        &input_tx,
        None,
        &session_id,
        prompt_command("opening prompt", None),
    )
    .unwrap();

    // `thread/started` lands before the response: the thread is bound while the
    // setup is still pending.
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .get_mut(&session_id)
        .expect("session should be registered")
        .thread_id = Some("thread-early".to_owned());

    // It also persisted `external_session_id`, so the next prompt asks to RESUME
    // the very thread the in-flight setup is creating.
    handle_shared_codex_prompt_command(
        &mut writer,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &test_missing_shared_codex_home(),
        &runtime.sessions,
        &runtime.thread_sessions,
        &input_tx,
        None,
        &session_id,
        prompt_command("next prompt", Some("thread-early")),
    )
    .unwrap();

    let written = String::from_utf8(writer).expect("writer output should be utf-8");
    assert_eq!(
        written.matches("thread/start").count(),
        1,
        "only the opening prompt starts a thread"
    );
    assert_eq!(
        written.matches("thread/resume").count(),
        0,
        "a prompt resuming the thread its OWN in-flight setup is creating must park, \
         not supersede: superseding fires a redundant thread/resume and makes the \
         orphaned waiter permanently suppress the session's own LIVE thread"
    );

    // Parked on the ORIGINAL setup, which still targets no thread.
    let setup = runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .get(&session_id)
        .map(|session_state| {
            let setup = session_state
                .pending_thread_setup
                .as_ref()
                .expect("the original setup should still be in flight");
            setup.command.prompt.clone()
        });
    assert_eq!(setup.as_deref(), Some("next prompt"));

    retire_pending_codex_thread_setups(&pending_requests);
}

// Pins the release path. If the setup request never reaches the app-server, the
// slot claimed for it must be released — otherwise the session is wedged in
// `{setup in flight}` forever and EVERY later prompt parks behind a setup that can
// never complete. This is the worst failure mode the parking rule can produce, so
// it gets its own test rather than riding on the happy path.
#[test]
fn failed_thread_setup_write_releases_the_setup_slot() {
    struct FailingWriter;
    impl std::io::Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "codex stdin closed",
            ))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, _process) =
        test_shared_codex_runtime("codex-thread-setup-write-failure");

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let (input_tx, _dummy_input_rx) = mpsc::channel::<CodexRuntimeCommand>();
    let mut writer = FailingWriter;

    let result = handle_shared_codex_prompt_command(
        &mut writer,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &test_missing_shared_codex_home(),
        &runtime.sessions,
        &runtime.thread_sessions,
        &input_tx,
        None,
        &session_id,
        CodexPromptCommand {
            active_turn_generation: 0,
            approval_policy: CodexApprovalPolicy::AutoApprove,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "doomed prompt".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    );
    assert!(
        result.is_err(),
        "a failed stdin write should surface as an error"
    );

    let pending_setup = runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .get(&session_id)
        .and_then(|session_state| {
            session_state
                .pending_thread_setup
                .as_ref()
                .map(|setup| setup.request_id.clone())
        });
    assert_eq!(
        pending_setup, None,
        "a setup whose request never went out must release its slot, or the session \
         parks every later prompt behind a setup that can never complete"
    );
}

// The app-server erroring or timing out on `thread/start` is the failure that was
// actually observed in the wild, so pin that it releases the setup slot: the
// session must be free to start a fresh setup afterwards rather than parking every
// later prompt behind a setup that can never complete. The sibling test at the
// bottom of this file only covers the NotCurrent branch (a stale waiter must not
// retire a newer setup); this covers the current one.
#[test]
fn thread_setup_response_error_releases_the_setup_slot() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, _process) = test_shared_codex_runtime("codex-thread-setup-error");

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                pending_thread_setup: Some(test_pending_codex_thread_setup("doomed-setup")),
                ..SharedCodexSessionState::default()
            },
        );

    handle_shared_codex_thread_setup_response_error_if_current(
        &runtime.sessions,
        &state,
        &runtime.runtime_id,
        &session_id,
        "doomed-setup",
        Duration::from_secs(180),
        CodexResponseError::Timeout("codex app-server did not respond".to_owned()),
    );

    let pending_setup = runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .get(&session_id)
        .and_then(|session_state| {
            session_state
                .pending_thread_setup
                .as_ref()
                .map(|setup| setup.request_id.clone())
        });
    assert_eq!(
        pending_setup, None,
        "an app-server error/timeout must release the setup slot AND drop the prompt \
         parked on it, or the session parks every later prompt behind a setup that can \
         never complete — this is the failure that was actually observed in the wild"
    );
}

// Every early return between claiming the setup slot and putting the request on the
// wire must release the slot, or the session wedges in `{setup in flight}` and EVERY
// later prompt parks behind a setup that will never fire — the worst failure mode the
// parking rule can produce.
//
// `PendingCodexThreadSetupGuard` is what makes that true for early returns nobody has
// written yet, so pin the guard itself. This replaces a test that claimed to cover the
// MCP-config failure arm and covered nothing: it forced the failure with an env var
// (`TERMAL_DELEGATION_MCP_EXE`) that NO production code reads, so the build always
// succeeded, the test always took its `else` branch, and it asserted the slot was
// still held — the opposite of its own name. It could not fail. Deleting the abort it
// supposedly guarded left the suite green.
#[test]
fn thread_setup_guard_releases_the_slot_unless_the_request_reached_the_wire() {
    let (runtime, _input_rx, _process) = test_shared_codex_runtime("codex-thread-setup-guard");
    let session_id = "session-guarded".to_owned();

    let claim = |request_id: &str| {
        let mut sessions = runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        let session_state = sessions.entry(session_id.clone()).or_default();
        session_state.pending_thread_setup = Some(test_pending_codex_thread_setup(request_id));
    };
    let parked_setup = || {
        runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned")
            .get(&session_id)
            .and_then(|session_state| {
                session_state
                    .pending_thread_setup
                    .as_ref()
                    .map(|setup| setup.request_id.clone())
            })
    };

    // Armed: the request never made it out, so dropping the guard must release the
    // slot AND drop the prompt parked on it.
    claim("setup-abandoned");
    {
        let _guard =
            PendingCodexThreadSetupGuard::new(&runtime.sessions, &session_id, "setup-abandoned");
    }
    assert_eq!(
        parked_setup(),
        None,
        "a setup abandoned before its request reached the wire must release the slot, \
         or the session parks every later prompt behind a setup that can never fire"
    );

    // Disarmed: the request IS on the wire and the waiter owns the slot. Releasing it
    // here would abort a genuinely live setup.
    claim("setup-in-flight");
    {
        let guard =
            PendingCodexThreadSetupGuard::new(&runtime.sessions, &session_id, "setup-in-flight");
        guard.disarm();
    }
    assert_eq!(
        parked_setup(),
        Some("setup-in-flight".to_owned()),
        "a setup whose request is in flight is owned by its waiter; the guard must not \
         release it"
    );

    // A detach (or a newer setup) can replace the slot while an older guard is still
    // alive. Dropping that stale guard must not disturb whatever holds the slot now.
    claim("setup-current");
    {
        let _stale =
            PendingCodexThreadSetupGuard::new(&runtime.sessions, &session_id, "setup-superseded");
    }
    assert_eq!(
        parked_setup(),
        Some("setup-current".to_owned()),
        "a guard for a setup that is no longer current must leave the live setup alone"
    );
}

/// Answers every outstanding thread-setup request with `thread_id`.
///
/// Also serves as cleanup: an unanswered setup leaves its waiter blocked for the
/// full `SHARED_CODEX_THREAD_SETUP_TIMEOUT`, and a thread parked for three minutes
/// outlives the test and perturbs the rest of the suite.
pub(super) fn answer_pending_codex_thread_setups(
    pending_requests: &CodexPendingRequestMap,
    thread_id: &str,
) {
    let request_ids = {
        let pending = pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned");
        pending.keys().cloned().collect::<Vec<_>>()
    };
    for request_id in request_ids {
        let sender = pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned")
            .remove(&request_id);
        if let Some(sender) = sender {
            let _ = sender.send(Ok(json!({ "thread": { "id": thread_id } })));
        }
    }
}

/// Retires outstanding setups whose thread id the test does not assert on.
fn retire_pending_codex_thread_setups(pending_requests: &CodexPendingRequestMap) {
    answer_pending_codex_thread_setups(pending_requests, "thread-retired");
}

// Pins that handle_shared_codex_prompt_command clears stale
// command_messages/streaming_text keys at dispatch time, so even if the
// next turn's item/started notification arrives BEFORE turn/started, the
// recorder still creates a fresh Message::Command.
// Guards against pre-turn-started notifications mutating the previous
// turn's command entry through leftover recorder keys.
#[test]
fn shared_codex_standard_turn_start_serializes_explicit_null_service_tier() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-standard-turn-start");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = Vec::new();
    handle_shared_codex_start_turn(
        &mut writer,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &runtime.thread_sessions,
        None,
        &session_id,
        "conversation-standard",
        None,
        CodexPromptCommand {
            active_turn_generation: 0,
            approval_policy: CodexApprovalPolicy::AutoApprove,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "clear the sticky tier".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .expect("Standard turn should start");

    let request: Value = serde_json::from_slice(&writer).expect("turn/start should be JSON-RPC");
    assert_eq!(
        request.pointer("/params/serviceTier"),
        Some(&Value::Null),
        "Standard must explicitly clear a service tier inherited by the thread"
    );
    assert_eq!(
        request.pointer("/params/approvalPolicy"),
        Some(&json!("on-request")),
        "TermAl AutoApprove must keep native Codex approval requests enabled per turn"
    );

    let (_request_id, sender) = take_pending_codex_request(&pending_requests);
    sender
        .send(Ok(json!({ "turn": { "id": "turn-standard" } })))
        .expect("turn/start response should send");
}

// Pins that shared Codex thread setup includes TermAl's parent-scoped
// delegation MCP bridge in the app-server `thread/start` config. This is the
// hook that makes `/review-changes` available inside Codex sessions.
pub(super) fn shared_codex_setup_request_for_mcp_test(
    codex_home: &FsPath,
    resume_thread_id: Option<&str>,
    engram_enabled: bool,
) -> Value {
    let state = test_app_state();
    let session_id = if engram_enabled {
        create_test_engram_codex_session(&state, "shared-codex-seeded-mcp-config")
    } else {
        test_session_id(&state, Agent::Codex)
    };
    let (runtime, _runtime_input_rx, process) =
        test_shared_codex_runtime("shared-codex-seeded-mcp-config");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
    }
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = Vec::new();
    let (input_tx, input_rx) = mpsc::channel::<CodexRuntimeCommand>();

    handle_shared_codex_prompt_command(
        &mut writer,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        codex_home,
        &runtime.sessions,
        &runtime.thread_sessions,
        &input_tx,
        None,
        &session_id,
        CodexPromptCommand {
            active_turn_generation: 0,
            approval_policy: CodexApprovalPolicy::AutoApprove,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "start with seeded MCP servers".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: None,
            resume_thread_id: resume_thread_id.map(str::to_owned),
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .expect("seeded MCP config must not prevent Codex thread setup");

    if engram_enabled && resume_thread_id.is_none() {
        finish_engram_config_for_test(
            &state,
            &runtime,
            &pending_requests,
            &input_tx,
            &input_rx,
            &mut writer,
            json!({"config": {}}),
        );
    }
    retire_pending_codex_thread_setups(&pending_requests);
    String::from_utf8(writer)
        .expect("Codex request should be UTF-8")
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|message| message["method"] != "config/read")
        .expect("Codex thread setup should be valid JSON-RPC")
}

#[test]
fn shared_codex_thread_setup_merges_seeded_user_config_with_termal_precedence() {
    let root = TestTempRoot::create("termal-shared-codex-user-mcp");
    fs::write(
        root.path().join("config.toml"),
        r#"
[mcp_servers.user-tools]
command = "user-mcp"
args = ["--from-config"]

[mcp_servers.user-tools.env]
USER_TOKEN = "keep-me"

[mcp_servers.termal-delegation]
command = "untrusted-collision"
args = ["--wrong-parent"]

[shell_environment_policy]
inherit = "none"
ignore_default_excludes = false
exclude = ["DROP_ME"]
include_only = ["KEEP_ONLY"]
future_policy = { mode = "preserve-me" }

[shell_environment_policy.set]
KEEP_ME = "present"
TERMAL_SESSION_ID = "stale-termal-session"
TERMAL_BASE_URL = "http://stale.invalid"
TERMAL_CLI = "stale-termal"
termal_session_id = "case-colliding-termal-session"
ENGRAM_HOME = "stale-home"
ENGRAM_ACTOR_ID = "stale-actor"
ENGRAM_ACTOR_CONTEXT = "stale-context"
ENGRAM_SESSION_ID = "stale-session"
engram_home = "case-colliding-home"
"#,
    )
    .expect("seeded Codex config should write");

    for (expected_method, resume_thread_id) in [
        ("thread/start", None),
        ("thread/resume", Some("thread-existing")),
    ] {
        let request = shared_codex_setup_request_for_mcp_test(root.path(), resume_thread_id, true);
        assert_eq!(request["method"], expected_method);
        assert_eq!(
            request.pointer("/params/config/mcp_servers/user-tools/command"),
            Some(&json!("user-mcp")),
            "user-configured MCP servers must survive the thread-level config override"
        );
        assert_eq!(
            request.pointer("/params/config/mcp_servers/user-tools/env/USER_TOKEN"),
            Some(&json!("keep-me")),
            "nested user MCP settings must survive without reshaping"
        );
        assert_ne!(
            request.pointer("/params/config/mcp_servers/termal-delegation/command"),
            Some(&json!("untrusted-collision")),
            "TermAl must replace a colliding user definition for its owned server name"
        );
        assert!(
            request
                .pointer("/params/config/mcp_servers/termal-delegation/args")
                .and_then(Value::as_array)
                .is_some_and(|args| args.iter().any(|arg| arg == "delegation-mcp")),
            "the winning TermAl descriptor must launch the delegation bridge"
        );
        assert_eq!(
            request.pointer("/params/config/shell_environment_policy/inherit"),
            Some(&json!("none")),
            "the seeded inheritance policy must survive the thread-level override"
        );
        assert_eq!(
            request.pointer("/params/config/shell_environment_policy/exclude"),
            Some(&json!(["DROP_ME"])),
            "the seeded exclusion policy must survive the thread-level override"
        );
        assert_eq!(
            request.pointer("/params/config/shell_environment_policy/include_only"),
            Some(&json!([
                "KEEP_ONLY",
                "TERMAL_SESSION_ID",
                "TERMAL_BASE_URL",
                "TERMAL_CLI",
                "ENGRAM_HOME",
                "ENGRAM_ACTOR_ID",
                "ENGRAM_ACTOR_CONTEXT",
                "ENGRAM_SESSION_ID",
            ])),
            "an active seeded allowlist must retain its entries and admit the owned identity"
        );
        assert_eq!(
            request.pointer("/params/config/shell_environment_policy/ignore_default_excludes"),
            Some(&json!(false)),
            "the seeded default-exclusion setting must survive the thread-level override"
        );
        assert_eq!(
            request.pointer("/params/config/shell_environment_policy/future_policy"),
            Some(&json!({ "mode": "preserve-me" })),
            "unknown seeded policy keys must survive without reshaping"
        );
        assert_eq!(
            request.pointer("/params/config/shell_environment_policy/set/KEEP_ME"),
            Some(&json!("present")),
            "unrelated seeded environment entries must survive the Engram overlay"
        );
        let termal_server = request
            .pointer("/params/config/mcp_servers/termal-delegation")
            .expect("TermAl delegation server should be present");
        let termal_args = termal_server["args"]
            .as_array()
            .expect("TermAl delegation args should be an array");
        let parent_index = termal_args
            .iter()
            .position(|value| value == "--parent-session-id")
            .expect("parent-session-id argument should be present");
        let base_url_index = termal_args
            .iter()
            .position(|value| value == "--base-url")
            .expect("base-url argument should be present");
        assert_eq!(
            request.pointer("/params/config/shell_environment_policy/set/TERMAL_SESSION_ID"),
            termal_args.get(parent_index + 1),
        );
        assert_eq!(
            request.pointer("/params/config/shell_environment_policy/set/TERMAL_BASE_URL"),
            termal_args.get(base_url_index + 1),
        );
        assert_eq!(
            request.pointer("/params/config/shell_environment_policy/set/TERMAL_CLI"),
            termal_server.get("command"),
        );
        #[cfg(windows)]
        assert!(
            request
                .pointer("/params/config/shell_environment_policy/set/termal_session_id")
                .is_none(),
            "Windows must remove case-insensitive TermAl aliases before inserting canonical names"
        );
        #[cfg(not(windows))]
        assert_eq!(
            request.pointer("/params/config/shell_environment_policy/set/termal_session_id"),
            Some(&json!("case-colliding-termal-session")),
        );
        #[cfg(windows)]
        assert!(
            request
                .pointer("/params/config/shell_environment_policy/set/engram_home")
                .is_none(),
            "Windows must remove case-insensitive aliases before inserting the canonical name"
        );
        #[cfg(not(windows))]
        assert_eq!(
            request.pointer("/params/config/shell_environment_policy/set/engram_home"),
            Some(&json!("case-colliding-home")),
            "Unix environment names are case-sensitive, so the lowercase entry is unrelated"
        );
        for name in ENGRAM_AGENT_PROCESS_ENV_NAMES {
            assert_eq!(
                request.pointer(&format!(
                    "/params/config/shell_environment_policy/set/{name}"
                )),
                request.pointer(&format!("/params/config/mcp_servers/engram/env/{name}")),
                "{expected_method} must overlay `{name}` with the MCP child's exact value"
            );
        }
    }

    let ineligible_request = shared_codex_setup_request_for_mcp_test(root.path(), None, false);
    assert!(
        ineligible_request
            .pointer("/params/config/mcp_servers/engram")
            .is_none(),
        "an ineligible thread must not receive the Engram MCP server"
    );
    for name in TERMAL_AGENT_PROCESS_ENV_NAMES {
        assert!(
            ineligible_request
                .pointer(&format!(
                    "/params/config/shell_environment_policy/set/{name}"
                ))
                .is_some(),
            "every Codex thread must receive TermAl identity `{name}`"
        );
    }
}

#[test]
fn codex_engram_shell_env_collision_matching_follows_platform_semantics() {
    let seeded = json!({
        "ENGRAM_HOME": "canonical-stale",
        "engram_home": "mixed-case-stale",
        "KEEP_ME": "unrelated",
    });

    let mut case_sensitive = seeded
        .as_object()
        .expect("seeded set should be an object")
        .clone();
    remove_owned_agent_shell_env_collisions(
        &mut case_sensitive,
        &ENGRAM_AGENT_PROCESS_ENV_NAMES,
        false,
    );
    assert!(!case_sensitive.contains_key("ENGRAM_HOME"));
    assert_eq!(
        case_sensitive.get("engram_home"),
        Some(&json!("mixed-case-stale"))
    );
    assert_eq!(case_sensitive.get("KEEP_ME"), Some(&json!("unrelated")));

    let mut case_insensitive = seeded
        .as_object()
        .expect("seeded set should be an object")
        .clone();
    remove_owned_agent_shell_env_collisions(
        &mut case_insensitive,
        &ENGRAM_AGENT_PROCESS_ENV_NAMES,
        true,
    );
    assert!(!case_insensitive.contains_key("ENGRAM_HOME"));
    assert!(!case_insensitive.contains_key("engram_home"));
    assert_eq!(case_insensitive.get("KEEP_ME"), Some(&json!("unrelated")));
}

#[test]
fn codex_engram_shell_env_include_only_admits_exact_canonical_names() {
    let mut config = json!({
        "shell_environment_policy": {
            "include_only": ["engram_home"],
        },
    });
    let engram_env = BTreeMap::from([
        (ENGRAM_HOME_ENV.to_owned(), "test-home".to_owned()),
        (ENGRAM_ACTOR_ID_ENV.to_owned(), "dev/codex".to_owned()),
        (
            ENGRAM_ACTOR_CONTEXT_ENV.to_owned(),
            "agent=codex;model=test;reasoning=high".to_owned(),
        ),
        (ENGRAM_SESSION_ID_ENV.to_owned(), "test-session".to_owned()),
    ]);

    merge_owned_agent_shell_env_into_codex_config(
        &mut config,
        &engram_env,
        &ENGRAM_AGENT_PROCESS_ENV_NAMES,
        &ENGRAM_REQUIRED_AGENT_PROCESS_ENV_NAMES,
        "Engram MCP descriptor",
    )
    .expect("Engram shell identity should merge into the allowlist");

    assert_eq!(
        config.pointer("/shell_environment_policy/include_only"),
        Some(&json!([
            "engram_home",
            "ENGRAM_HOME",
            "ENGRAM_ACTOR_ID",
            "ENGRAM_ACTOR_CONTEXT",
            "ENGRAM_SESSION_ID",
        ])),
        "a differently-cased allowlist entry must not suppress the exact canonical name"
    );
}

#[test]
fn shared_codex_thread_setup_falls_back_when_seeded_config_is_missing_or_malformed() {
    let root = TestTempRoot::create("termal-shared-codex-invalid-mcp");
    let missing_home = root.path().join("missing-home");
    let missing_request = shared_codex_setup_request_for_mcp_test(&missing_home, None, true);
    let missing_servers = missing_request
        .pointer("/params/config/mcp_servers")
        .and_then(Value::as_object)
        .expect("fallback config should contain mcp_servers");
    assert_eq!(missing_servers.len(), 2);
    assert!(missing_servers.contains_key(TERMAL_DELEGATION_MCP_SERVER_NAME));
    assert!(missing_servers.contains_key(ENGRAM_MCP_SERVER_NAME));
    let missing_policy = missing_request
        .pointer("/params/config/shell_environment_policy")
        .and_then(Value::as_object)
        .expect("eligible fallback config should contain a shell policy");
    assert_eq!(missing_policy.len(), 1);
    let missing_set = missing_policy
        .get("set")
        .and_then(Value::as_object)
        .expect("fallback policy set should be an object");
    assert_eq!(
        missing_set.len(),
        TERMAL_AGENT_PROCESS_ENV_NAMES.len() + ENGRAM_AGENT_PROCESS_ENV_NAMES.len(),
        "a missing seeded policy must contain exactly the TermAl and Engram owned values"
    );
    for name in TERMAL_AGENT_PROCESS_ENV_NAMES {
        assert!(missing_set.contains_key(name));
    }
    for name in ENGRAM_AGENT_PROCESS_ENV_NAMES {
        assert!(
            missing_set.contains_key(name),
            "fallback policy must contain the exact owned key `{name}`"
        );
    }
    assert!(
        missing_policy.get("include_only").is_none(),
        "an absent allowlist must stay absent instead of enabling allowlist mode"
    );

    let malformed_home = root.path().join("malformed-home");
    fs::create_dir_all(&malformed_home).expect("malformed Codex home should be created");
    fs::write(
        malformed_home.join("config.toml"),
        "[mcp_servers.user-tools",
    )
    .expect("malformed Codex config should write");
    let malformed_request =
        shared_codex_setup_request_for_mcp_test(&malformed_home, Some("thread-existing"), true);
    let malformed_servers = malformed_request
        .pointer("/params/config/mcp_servers")
        .and_then(Value::as_object)
        .expect("fallback config should contain mcp_servers");
    assert_eq!(malformed_servers.len(), 2);
    assert!(malformed_servers.contains_key(TERMAL_DELEGATION_MCP_SERVER_NAME));
    assert!(malformed_servers.contains_key(ENGRAM_MCP_SERVER_NAME));
    let malformed_policy = malformed_request
        .pointer("/params/config/shell_environment_policy")
        .and_then(Value::as_object)
        .expect("eligible malformed fallback should contain a shell policy");
    assert_eq!(malformed_policy.len(), 1);
    let malformed_set = malformed_policy
        .get("set")
        .and_then(Value::as_object)
        .expect("malformed fallback policy set should be an object");
    assert_eq!(
        malformed_set.len(),
        TERMAL_AGENT_PROCESS_ENV_NAMES.len() + ENGRAM_AGENT_PROCESS_ENV_NAMES.len(),
        "a malformed seeded config must contain exactly the TermAl and Engram owned values"
    );
    for name in TERMAL_AGENT_PROCESS_ENV_NAMES {
        assert!(malformed_set.contains_key(name));
    }
    for name in ENGRAM_AGENT_PROCESS_ENV_NAMES {
        assert!(
            malformed_set.contains_key(name),
            "malformed fallback policy must contain the exact owned key `{name}`"
        );
    }
    assert!(
        malformed_policy.get("include_only").is_none(),
        "a malformed fallback must not enable allowlist mode"
    );
}

#[test]
fn shared_codex_thread_start_includes_delegation_mcp_config() {
    let state = test_app_state();
    let session_id =
        create_test_engram_codex_session(&state, "shared-codex-thread-start-mcp-config");
    let (runtime, _runtime_input_rx, process) =
        test_shared_codex_runtime("shared-codex-thread-start-mcp-config");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = Vec::new();
    let (input_tx, input_rx) = mpsc::channel::<CodexRuntimeCommand>();

    handle_shared_codex_prompt_command(
        &mut writer,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &test_missing_shared_codex_home(),
        &runtime.sessions,
        &runtime.thread_sessions,
        &input_tx,
        None,
        &session_id,
        CodexPromptCommand {
            active_turn_generation: 0,
            approval_policy: CodexApprovalPolicy::AutoApprove,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "start the turn".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .unwrap();

    finish_engram_config_for_test(
        &state,
        &runtime,
        &pending_requests,
        &input_tx,
        &input_rx,
        &mut writer,
        json!({"config": {}}),
    );
    let written = String::from_utf8(writer).expect("Codex request should be UTF-8");
    assert!(
        written.contains("\"method\":\"thread/start\""),
        "thread/start request should be written\n{written}"
    );
    assert!(
        written.contains("\"mcp_servers\"")
            && written.contains("\"termal-delegation\"")
            && written.contains("\"delegation-mcp\"")
            && written.contains("\"--parent-session-id\"")
            && written.contains(&format!("\"{}\"", session_id)),
        "thread/start should include the parent-scoped TermAl delegation MCP bridge\n{written}"
    );
    let start_request = written
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|message| message["method"] == "thread/start")
        .expect("thread/start should be valid JSON-RPC");
    assert_eq!(
        start_request.pointer("/params/serviceTier"),
        Some(&Value::Null),
        "Standard must not omit the explicit service-tier clear"
    );
    assert_eq!(
        start_request.pointer("/params/approvalPolicy"),
        Some(&json!("on-request")),
        "TermAl AutoApprove must keep native Codex approval requests enabled at thread start"
    );
    let shell_env = start_request
        .pointer("/params/config/shell_environment_policy/set")
        .and_then(Value::as_object)
        .expect("the real thread/start request must carry shell identity");
    assert_eq!(
        shell_env,
        &expected_codex_shell_environment_set(&start_request["params"]["config"], &session_id),
        "thread/start must carry exactly the seven owned identity values"
    );

    // This test owns only the setup request shape. Retire its waiter instead
    // of adding a scheduler-sensitive hand-off assertion already covered by
    // the dedicated setup lifecycle tests in this module.
    retire_pending_codex_thread_setups(&pending_requests);
}

// Pins that if the StartTurnAfterSetup channel hand-off fails (input_rx
// dropped), the provisional thread registration is rolled back: runtime
// cleared, external_session_id cleared, shared thread_id cleared, and the
// thread_sessions map no longer contains the conversation id.
// Guards against orphaned thread mappings lingering after a failed handoff.
