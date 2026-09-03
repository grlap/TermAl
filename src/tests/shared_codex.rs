//! Shared Codex thread setup, request, timeout, and runtime lifecycle tests.
//!
//! Owns the shared app-server setup handshake and runtime failure/recovery
//! seams after a thread is bound. Deliberately does not own event routing,
//! text reconciliation, or retry/error-notification coverage; those were
//! split from this original module into `shared_codex_events.rs`.

use super::*;

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
        service_tier: Some(CODEX_FAST_SERVICE_TIER.to_owned()),
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
    let delivered = input_rx
        .recv_timeout(Duration::from_secs(5))
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
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, _process) = test_shared_codex_runtime("codex-thread-setup-detach");

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let (input_tx, _dummy_input_rx) = mpsc::channel::<CodexRuntimeCommand>();
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
fn answer_pending_codex_thread_setups(pending_requests: &CodexPendingRequestMap, thread_id: &str) {
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
fn shared_codex_prompt_dispatch_clears_stale_command_state_before_turn_started_notification() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-prompt-dispatch-clears-stale-state");

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

    state
        .upsert_command_message(
            &session_id,
            "old-command-message",
            "Web search: previous turn",
            "previous turn",
            CommandStatus::Success,
        )
        .unwrap();

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                recorder: SessionRecorderState {
                    command_messages: HashMap::from([(
                        "webSearch".to_owned(),
                        "old-command-message".to_owned(),
                    )]),
                    parallel_agents_messages: HashMap::new(),
                    streaming_text_message_id: Some("stale-stream".to_owned()),
                },
                thread_id: Some("conversation-123".to_owned()),
                turn_id: Some("turn-old".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = Vec::new();
    // The session already has a thread_id so the fast path is taken and
    // input_tx is unused, but the parameter is still required.
    let (dummy_input_tx, _dummy_input_rx) = mpsc::channel::<CodexRuntimeCommand>();
    handle_shared_codex_prompt_command(
        &mut writer,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &test_missing_shared_codex_home(),
        &runtime.sessions,
        &runtime.thread_sessions,
        &dummy_input_tx,
        None,
        &session_id,
        CodexPromptCommand {
            active_turn_generation: 0,
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "check the repo".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .unwrap();
    let response_sender = {
        let mut pending = pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned");
        let request_id = pending
            .keys()
            .next()
            .cloned()
            .expect("fire-and-forget turn/start request should be pending after dispatch");
        pending
            .remove(&request_id)
            .expect("pending turn/start response sender should exist")
    };
    response_sender
        .send(Ok(json!({
            "turn": {
                "id": "turn-new"
            }
        })))
        .expect("turn/start waiter should accept the scripted response");

    let item_started = json!({
        "method": "item/started",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "type": "webSearch",
                "query": "serde_json value",
                "action": {
                    "type": "search",
                    "queries": ["serde_json value"]
                }
            }
        }
    });
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-new"
            }
        }
    });
    let item_completed = json!({
        "method": "item/completed",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "type": "webSearch",
                "query": "serde_json value",
                "action": {
                    "type": "search",
                    "queries": ["serde_json value"]
                }
            }
        }
    });

    handle_shared_codex_app_server_message(
        &item_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &item_completed,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    let command_messages = session
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Command {
                command,
                output,
                status,
                ..
            } => Some((command.clone(), output.clone(), *status)),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        command_messages,
        vec![
            (
                "Web search: previous turn".to_owned(),
                "previous turn".to_owned(),
                CommandStatus::Success,
            ),
            (
                "Web search: serde_json value".to_owned(),
                "serde_json value".to_owned(),
                CommandStatus::Success,
            ),
        ]
    );
}

// Pins the turn/start race: if turn/started lands while the turn/start
// JSON-RPC request is still being written, the fast notification must NOT
// reintroduce pending_turn_start_request_id once handle_shared_codex_start_turn
// returns — its post-write state merge wins.
// Guards against the notification path re-setting pending state that the
// writer path has already cleared.
#[test]
fn shared_codex_turn_started_notification_does_not_restore_pending_state() {
    struct RaceWriter<F: FnMut()> {
        buffer: Vec<u8>,
        injected: bool,
        on_turn_start_written: F,
    }

    impl<F: FnMut()> std::io::Write for RaceWriter<F> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.buffer.extend_from_slice(buf);
            if !self.injected && self.buffer.ends_with(b"\n") {
                let line = std::str::from_utf8(&self.buffer)
                    .expect("turn/start payload should stay valid UTF-8");
                if line.contains("\"method\":\"turn/start\"") {
                    self.injected = true;
                    (self.on_turn_start_written)();
                }
            }
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-turn-start-race");

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

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let callback_state = state.clone();
    let callback_pending_requests = pending_requests.clone();
    let callback_sessions = runtime.sessions.clone();
    let callback_thread_sessions = runtime.thread_sessions.clone();
    let callback_runtime_id = runtime.runtime_id.clone();
    let (callback_input_tx, _callback_input_rx) = mpsc::channel::<CodexRuntimeCommand>();
    let mut writer = RaceWriter {
        buffer: Vec::new(),
        injected: false,
        on_turn_start_written: move || {
            handle_shared_codex_app_server_message(
                &json!({
                    "method": "turn/started",
                    "params": {
                        "threadId": "conversation-123",
                        "turn": {
                            "id": "turn-fast"
                        }
                    }
                }),
                &callback_state,
                &callback_runtime_id,
                &callback_pending_requests,
                &callback_sessions,
                &callback_thread_sessions,
                &callback_input_tx,
            )
            .expect("turn/started callback should be handled");
        },
    };

    handle_shared_codex_start_turn(
        &mut writer,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &runtime.thread_sessions,
        None,
        &session_id,
        "conversation-123",
        None,
        CodexPromptCommand {
            active_turn_generation: 0,
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "inspect race handling".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: Some(CODEX_FAST_SERVICE_TIER.to_owned()),
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .unwrap();

    let written = String::from_utf8(writer.buffer.clone()).expect("Codex request should be UTF-8");
    assert!(
        written.contains("\"serviceTier\":\"priority\""),
        "turn/start should include the session-scoped Fast service tier\n{written}"
    );

    {
        let sessions = runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        let session_state = sessions
            .get(&session_id)
            .expect("shared Codex session state should exist");
        assert_eq!(session_state.turn_id.as_deref(), Some("turn-fast"));
        assert!(session_state.turn_started);
        assert_eq!(session_state.pending_turn_start_request_id, None);
    }

    let (_request_id, sender) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));
    sender
        .send(Ok(json!({
            "turn": {
                "id": "turn-fast"
            }
        })))
        .unwrap();
}

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

    let (_request_id, sender) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));
    sender
        .send(Ok(json!({ "turn": { "id": "turn-standard" } })))
        .expect("turn/start response should send");
}

// Pins that shared Codex thread setup includes TermAl's parent-scoped
// delegation MCP bridge in the app-server `thread/start` config. This is the
// hook that makes `/review-changes` available inside Codex sessions.
fn shared_codex_setup_request_for_mcp_test(
    codex_home: &FsPath,
    resume_thread_id: Option<&str>,
) -> Value {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _runtime_input_rx, _process) =
        test_shared_codex_runtime("shared-codex-seeded-mcp-config");
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = Vec::new();
    let (input_tx, _input_rx) = mpsc::channel::<CodexRuntimeCommand>();

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

    retire_pending_codex_thread_setups(&pending_requests);
    String::from_utf8(writer)
        .expect("Codex request should be UTF-8")
        .lines()
        .find_map(|line| serde_json::from_str::<Value>(line).ok())
        .expect("Codex thread setup should be valid JSON-RPC")
}

#[test]
fn shared_codex_thread_setup_merges_seeded_user_mcp_servers_with_termal_precedence() {
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
"#,
    )
    .expect("seeded Codex config should write");

    for (expected_method, resume_thread_id) in [
        ("thread/start", None),
        ("thread/resume", Some("thread-existing")),
    ] {
        let request = shared_codex_setup_request_for_mcp_test(root.path(), resume_thread_id);
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
    }
}

#[test]
fn shared_codex_thread_setup_falls_back_when_seeded_config_is_missing_or_malformed() {
    let root = TestTempRoot::create("termal-shared-codex-invalid-mcp");
    let missing_home = root.path().join("missing-home");
    let missing_request = shared_codex_setup_request_for_mcp_test(&missing_home, None);
    let missing_servers = missing_request
        .pointer("/params/config/mcp_servers")
        .and_then(Value::as_object)
        .expect("fallback config should contain mcp_servers");
    assert_eq!(missing_servers.len(), 1);
    assert!(missing_servers.contains_key(TERMAL_DELEGATION_MCP_SERVER_NAME));

    let malformed_home = root.path().join("malformed-home");
    fs::create_dir_all(&malformed_home).expect("malformed Codex home should be created");
    fs::write(
        malformed_home.join("config.toml"),
        "[mcp_servers.user-tools",
    )
    .expect("malformed Codex config should write");
    let malformed_request =
        shared_codex_setup_request_for_mcp_test(&malformed_home, Some("thread-existing"));
    let malformed_servers = malformed_request
        .pointer("/params/config/mcp_servers")
        .and_then(Value::as_object)
        .expect("fallback config should contain mcp_servers");
    assert_eq!(malformed_servers.len(), 1);
    assert!(malformed_servers.contains_key(TERMAL_DELEGATION_MCP_SERVER_NAME));
}

#[test]
fn shared_codex_thread_start_includes_delegation_mcp_config() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
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
    let (input_tx, _input_rx) = mpsc::channel::<CodexRuntimeCommand>();

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
        .find_map(|line| serde_json::from_str::<Value>(line).ok())
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
#[test]
fn shared_codex_thread_setup_handoff_failure_rolls_back_registration() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _runtime_input_rx, process) =
        test_shared_codex_runtime("shared-codex-thread-setup-handoff-failure");

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
    drop(input_rx);

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
            approval_policy: CodexApprovalPolicy::Never,
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

    let (_request_id, sender) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));
    sender
        .send(Ok(json!({
            "thread": {
                "id": "conversation-orphan"
            }
        })))
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let (runtime_cleared, external_session_id, status, preview) = {
            let inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&session_id)
                .expect("Codex session should exist");
            let record = &inner.sessions[index];
            (
                matches!(record.runtime, SessionRuntime::None),
                record.external_session_id.clone(),
                record.session.status,
                record.session.preview.clone(),
            )
        };
        let (shared_thread_id, has_thread_mapping) = {
            let sessions = runtime
                .sessions
                .lock()
                .expect("shared Codex session mutex poisoned");
            let thread_id = sessions
                .get(&session_id)
                .and_then(|session| session.thread_id.clone());
            drop(sessions);
            let thread_sessions = runtime
                .thread_sessions
                .lock()
                .expect("shared Codex thread mutex poisoned");
            (
                thread_id,
                thread_sessions.contains_key("conversation-orphan"),
            )
        };

        if runtime_cleared
            && external_session_id.is_none()
            && shared_thread_id.is_none()
            && !has_thread_mapping
        {
            assert_eq!(status, SessionStatus::Error);
            assert!(preview.contains("failed to queue shared Codex turn/start after thread setup"));
            break;
        }

        assert!(
            std::time::Instant::now() < deadline,
            "failed StartTurnAfterSetup handoff should roll back provisional thread registration"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

// Pins that a persistence failure during post-thread-setup state commit
// leaves the shared Codex runtime handle intact on the session, marks the
// session Error with a generic "Failed to save session state" preview, and
// blocks the StartTurnAfterSetup command from being queued.
// Guards against persistence IO errors tearing down a healthy runtime or
// leaking a thread mapping the caller cannot recover from.
#[test]
fn shared_codex_thread_setup_persist_failure_does_not_tear_down_runtime() {
    let mut state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _runtime_input_rx, process) =
        test_shared_codex_runtime("shared-codex-thread-setup-persist-failure");
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-shared-codex-thread-setup-persist-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

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
    state.shutdown_persist_blocking();
    state.persistence_path = Arc::new(failing_persistence_path.clone());

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
            approval_policy: CodexApprovalPolicy::Never,
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

    let (_request_id, sender) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));
    sender
        .send(Ok(json!({
            "thread": {
                "id": "conversation-persist-failure"
            }
        })))
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        match input_rx.try_recv() {
            Ok(_) => panic!("failed thread registration should not queue StartTurnAfterSetup"),
            Err(mpsc::TryRecvError::Disconnected) => {
                panic!("test input channel should remain open")
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }

        let failed = {
            let snapshot = state.full_snapshot();
            snapshot
                .sessions
                .iter()
                .find(|session| session.id == session_id)
                .is_some_and(|session| {
                    session.status == SessionStatus::Error
                        && session.preview.contains("Failed to save session state")
                })
        };
        if failed {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "failed thread registration should mark the session error"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        assert!(matches!(
            &inner.sessions[index].runtime,
            SessionRuntime::Codex(handle) if handle.runtime_id == runtime.runtime_id
        ));
    }
    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.status, SessionStatus::Error);
    assert!(
        session.preview.contains("Failed to save session state"),
        "persistence-failure preview should use generic message, got: {}",
        session.preview,
    );
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. })
            if text.contains("Turn failed: Failed to save session state")
    ));
    assert!(
        !runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .contains_key("conversation-persist-failure"),
        "failed thread registration should not publish a shared thread mapping"
    );

    // The app-server already wrote this thread to disk, and the persist that would
    // have made the record claim it just failed — so nothing owns it. It must be
    // disowned, exactly as every sibling failure branch does. Without this the next
    // discovery scan imports it as a phantom unlinked top-level session: the very
    // leak this change exists to close, reintroduced through the one branch that
    // forgot.
    //
    // Suppression persists too, so on a genuinely full disk it would fail as well —
    // but it updates the in-memory ignore set first, which is what we can observe
    // here and what holds for transient/permission failures.
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert!(
            inner
                .ignored_discovered_codex_thread_ids
                .contains("conversation-persist-failure"),
            "a thread whose registration failed to persist must be disowned, or discovery \
             re-imports it as a phantom top-level session"
        );
    }

    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Pins that handle_shared_codex_start_turn with a runtime_id that no
// longer matches the session's current runtime is a no-op: no bytes
// written, no pending request registered, no session entry inserted, and
// active_codex_* config fields stay None.
// Guards against stale StartTurnAfterSetup handoffs writing a different
// runtime's config onto the current session.
#[test]
fn shared_codex_stale_start_turn_handoff_skips_runtime_config_persistence() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (other_runtime, _other_input_rx) = test_codex_runtime_handle("other-runtime");
    let sessions = SharedCodexSessions::new();
    let thread_sessions: SharedCodexThreadMap = Arc::new(Mutex::new(HashMap::new()));
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = Vec::new();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(other_runtime);
        inner.sessions[index].active_codex_approval_policy = None;
        inner.sessions[index].active_codex_reasoning_effort = None;
        inner.sessions[index].active_codex_sandbox_mode = None;
    }

    handle_shared_codex_start_turn(
        &mut writer,
        &pending_requests,
        &state,
        "stale-runtime",
        &sessions,
        &thread_sessions,
        None,
        &session_id,
        "conversation-stale",
        None,
        CodexPromptCommand {
            active_turn_generation: 0,
            approval_policy: CodexApprovalPolicy::OnRequest,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "stale handoff".to_owned(),
            reasoning_effort: CodexReasoningEffort::XHigh,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::DangerFullAccess,
        },
    )
    .unwrap();

    assert!(writer.is_empty());
    assert!(
        pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned")
            .is_empty()
    );
    assert!(
        sessions
            .lock()
            .expect("shared Codex session mutex poisoned")
            .is_empty()
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(&session_id)
        .expect("Codex session should exist");
    assert_eq!(inner.sessions[index].active_codex_approval_policy, None);
    assert_eq!(inner.sessions[index].active_codex_reasoning_effort, None);
    assert_eq!(inner.sessions[index].active_codex_sandbox_mode, None);
}

// A `StartTurnAfterSetup` hand-off is enqueued by a WAITER thread, and the waiter
// clears the setup slot BEFORE it sends (`complete_shared_codex_thread_setup` returns,
// THEN `input_tx.send`). The writer is free to run in that gap — so a detach plus a
// fresh prompt can land in between, and the fresh prompt claims a NEW setup. The
// hand-off then arrives at a session that re-armed underneath it.
//
// The runtime-id check at the top of `handle_shared_codex_start_turn` does NOT catch
// this. Every session on the shared app-server carries the SAME `runtime_id` — it is
// cloned straight off `SharedCodexRuntime` in `spawn_codex_runtime` — so detach and
// re-attach yield the same id and the check returns `Applied`. It is a PROCESS check,
// not an ATTACHMENT check.
//
// This was previously "impossible": the code asserted no setup could be in flight here,
// on the theory that prompt handling and turn start are serialized on the writer thread.
// They are — but the hand-off is enqueued by a WAITER, and serializing the writer says
// nothing about what a waiter puts on the queue. The assert was reachable. In debug it
// panicked while HOLDING the shared-session mutex, poisoning it for every other Codex
// session on the shared runtime; in release it destroyed the prompt the user had just
// typed and started a stale turn on the detached attachment's thread.
#[test]
fn stale_start_turn_handoff_leaves_the_setup_that_re_armed_the_session_alone() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime_handle, _runtime_input_rx) = test_codex_runtime_handle("shared-app-server");
    let sessions = SharedCodexSessions::new();
    let thread_sessions: SharedCodexThreadMap = Arc::new(Mutex::new(HashMap::new()));
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = Vec::new();

    // Attached to the shared app-server, so the runtime-id check passes — exactly as it
    // does after a real detach + re-attach, because the id belongs to the process.
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime_handle);
        state.commit_locked(&mut inner).unwrap();
    }

    // The session re-armed: a NEW setup is in flight with the user's freshly-typed
    // prompt parked on it.
    {
        let mut guard = sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        let session_state = guard.entry(session_id.clone()).or_default();
        session_state.pending_thread_setup = Some(PendingCodexThreadSetup {
            request_id: "setup-b".to_owned(),
            command: CodexPromptCommand {
                active_turn_generation: 0,
                approval_policy: CodexApprovalPolicy::Never,
                attachments: Vec::new(),
                cwd: "/tmp".to_owned(),
                model: "gpt-5.4".to_owned(),
                prompt: "the prompt the user just typed".to_owned(),
                reasoning_effort: CodexReasoningEffort::Medium,
                service_tier: None,
                resume_thread_id: None,
                sandbox_mode: CodexSandboxMode::WorkspaceWrite,
            },
        });
    }

    // The stale hand-off, carrying the DETACHED attachment's thread and prompt.
    handle_shared_codex_start_turn(
        &mut writer,
        &pending_requests,
        &state,
        "shared-app-server",
        &sessions,
        &thread_sessions,
        None,
        &session_id,
        "thread-from-the-detached-attachment",
        None,
        CodexPromptCommand {
            active_turn_generation: 0,
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "stale prompt from before the detach".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .expect("a stale hand-off must be abandoned quietly, not fail the shared writer thread");

    let guard = sessions
        .lock()
        .expect("shared Codex session mutex poisoned");
    let session_state = guard
        .get(&session_id)
        .expect("the re-armed session state should still exist");

    let parked = session_state
        .pending_thread_setup
        .as_ref()
        .expect("the setup that re-armed the session must survive a stale hand-off");
    assert_eq!(parked.request_id, "setup-b");
    assert_eq!(
        parked.command.prompt, "the prompt the user just typed",
        "the stale hand-off destroyed the prompt parked on the session's CURRENT setup"
    );
    assert_eq!(
        session_state.thread_id, None,
        "the stale hand-off must not bind the detached attachment's thread onto the \
         re-armed session — its own setup will bind the right one"
    );
    assert!(
        session_state.pending_turn_start_request_id.is_none(),
        "a stale hand-off must not start a turn"
    );
    assert!(
        writer.is_empty(),
        "a stale hand-off must not write turn/start to the shared app-server"
    );
    assert!(
        pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned")
            .is_empty(),
        "a stale hand-off must not register a pending request"
    );
}

// Pins that set_external_session_id_if_runtime_matches surfaces Err when
// the commit fails rather than silently collapsing into the stale-session
// "skip" branch that returns Ok(()).
// Guards against persistence failures being masked as routine stale-runtime
// misses and thus going undetected by callers.
#[test]
fn set_external_session_id_if_runtime_matches_reports_persist_failure() {
    let mut state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx) = test_codex_runtime_handle("persist-thread-id-runtime");
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-codex-thread-id-persist-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }
    state.shutdown_persist_blocking();
    state.persistence_path = Arc::new(failing_persistence_path.clone());

    let result = state.set_external_session_id_if_runtime_matches(
        &session_id,
        &RuntimeToken::Codex("persist-thread-id-runtime".to_owned()),
        "conversation-123".to_owned(),
    );

    assert!(
        result.is_err(),
        "commit failures should not collapse into stale-session misses"
    );
    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Pins that record_codex_runtime_config_if_runtime_matches propagates
// persistence errors as Err instead of folding them into the stale-session
// skip path — the sibling helper to set_external_session_id_if_runtime_matches.
// Guards against runtime-config persistence errors being lost to callers.
#[test]
fn record_codex_runtime_config_if_runtime_matches_reports_persist_failure() {
    let mut state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx) = test_codex_runtime_handle("persist-runtime-config-runtime");
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-codex-runtime-config-persist-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }
    state.shutdown_persist_blocking();
    state.persistence_path = Arc::new(failing_persistence_path.clone());

    let result = state.record_codex_runtime_config_if_runtime_matches(
        &session_id,
        &RuntimeToken::Codex("persist-runtime-config-runtime".to_owned()),
        0,
        CodexSandboxMode::WorkspaceWrite,
        CodexApprovalPolicy::Never,
        CodexReasoningEffort::Medium,
    );

    assert!(
        result.is_err(),
        "persistence failures should remain fatal to the caller"
    );
    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Pins that a persistence failure during handle_shared_codex_start_turn's
// runtime-config commit keeps the shared runtime attached to the session
// (SessionRuntime::Codex stays), flips session to Error, and records a
// "Turn failed: Failed to save session state" transcript message.
// Guards against one session's persistence IO killing the shared process.
#[test]
fn shared_codex_start_turn_persist_failure_does_not_tear_down_runtime() {
    let mut state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _runtime_input_rx, process) =
        test_shared_codex_runtime("shared-codex-start-turn-persist-failure");
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-shared-codex-start-turn-persist-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

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
    state.shutdown_persist_blocking();
    state.persistence_path = Arc::new(failing_persistence_path.clone());

    handle_shared_codex_start_turn(
        &mut Vec::new(),
        &Arc::new(Mutex::new(HashMap::new())),
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &runtime.thread_sessions,
        None,
        &session_id,
        "conversation-123",
        None,
        CodexPromptCommand {
            active_turn_generation: 0,
            approval_policy: CodexApprovalPolicy::Never,
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

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(&session_id)
        .expect("Codex session should exist");
    assert!(matches!(
        &inner.sessions[index].runtime,
        SessionRuntime::Codex(handle) if handle.runtime_id == runtime.runtime_id
    ));
    drop(inner);

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.status, SessionStatus::Error);
    assert!(
        session.preview.contains("Failed to save session state"),
        "persistence-failure preview should use generic message, got: {}",
        session.preview,
    );
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. })
            if text.contains("Turn failed: Failed to save session state")
    ));

    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Pins the shared_codex_event_matches_visible_turn predicate: it accepts
// events for the active turn_id, accepts events whose turn_id matches the
// completed_turn_id ONLY when there is no active turn, and rejects
// orphan/mismatched turn ids.
// Guards against the grace-period branch firing while a new turn is live,
// or orphan events being accepted with no turn context at all.
#[test]
fn shared_codex_event_matches_visible_turn_handles_active_and_completed_turns() {
    assert!(shared_codex_event_matches_visible_turn(
        Some("turn-active"),
        None,
        Some("turn-active"),
    ));
    assert!(shared_codex_event_matches_visible_turn(
        None,
        Some("turn-completed"),
        Some("turn-completed"),
    ));
    assert!(!shared_codex_event_matches_visible_turn(
        None,
        Some("turn-completed"),
        Some("turn-other"),
    ));
    assert!(!shared_codex_event_matches_visible_turn(
        None,
        Some("turn-completed"),
        None,
    ));
    // No active or completed turn — event with a turn ID is rejected.
    assert!(!shared_codex_event_matches_visible_turn(
        None,
        None,
        Some("turn-orphan"),
    ));
    // Active turn differs from event but completed turn matches — the
    // completed branch is only entered when current_turn_id is None.
    assert!(!shared_codex_event_matches_visible_turn(
        Some("turn-active"),
        Some("turn-completed"),
        Some("turn-completed"),
    ));
}

// Pins the app-server error classifier: only the exact "session
// `<id>` not found" shape (possibly wrapped in context) counts as a stale
// session; "session ... message ... not found", "anchor message not
// found", and unrelated errors are fatal.
// Guards against over-broad stale-session matching that would swallow
// genuine persist/runtime failures as routine stale-session skips.
#[test]
fn shared_codex_app_server_error_classifier_only_ignores_missing_sessions() {
    assert!(shared_codex_app_server_error_is_stale_session(&anyhow!(
        "session `session-1` not found"
    )));
    assert!(shared_codex_app_server_error_is_stale_session(
        &anyhow!("session `session-1` not found").context("wrapped")
    ));
    assert!(!shared_codex_app_server_error_is_stale_session(&anyhow!(
        "session `session-1` message `message-1` not found"
    )));
    assert!(!shared_codex_app_server_error_is_stale_session(
        &anyhow!("session `session-1` message `message-1` not found").context("wrapped")
    ));
    assert!(!shared_codex_app_server_error_is_stale_session(&anyhow!(
        "session `session-1` anchor message `message-1` not found"
    )));
    assert!(!shared_codex_app_server_error_is_stale_session(&anyhow!(
        "failed to persist Codex notice"
    )));
}

// Pins that a server-initiated JSON-RPC request whose thread_id maps to
// no known session is auto-rejected with a -32001 "Session unavailable"
// error, routed back through the CodexRuntimeCommand::JsonRpcResponse
// writer-loop path.
// Guards against Codex hanging on requests the runtime cannot deliver.
#[test]
fn shared_codex_undeliverable_server_request_returns_json_rpc_error() {
    let state = test_app_state();
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let sessions = SharedCodexSessions::new();
    let thread_sessions: SharedCodexThreadMap = Arc::new(Mutex::new(HashMap::new()));
    let (input_tx, input_rx) = mpsc::channel::<CodexRuntimeCommand>();

    handle_shared_codex_app_server_message(
        &json!({
            "jsonrpc": "2.0",
            "id": "request-missing-session",
            "method": "session/request_permission",
            "params": {
                "threadId": "missing-thread"
            }
        }),
        &state,
        "shared-codex-missing-session",
        &pending_requests,
        &sessions,
        &thread_sessions,
        &input_tx,
    )
    .unwrap();

    match input_rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        CodexRuntimeCommand::JsonRpcResponse { response } => {
            assert_eq!(
                codex_json_rpc_response_message(&response),
                json!({
                    "jsonrpc": "2.0",
                    "id": "request-missing-session",
                    "error": {
                        "code": -32001,
                        "message": "Session unavailable; request could not be delivered."
                    }
                })
            );
        }
        _ => panic!("expected JSON-RPC rejection"),
    }
}

// Pins that a request with an explicit but unknown thread id is rejected even
// if its turn id matches an active session. The turn-id fallback is only safe
// for messages that truly lack thread identity.
#[test]
fn shared_codex_server_request_with_unknown_thread_id_does_not_fallback_to_turn_id() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) =
        test_shared_codex_runtime("shared-codex-wrong-thread-no-turn-fallback");

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

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                turn_id: Some("turn-live".to_owned()),
                turn_started: true,
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    handle_shared_codex_app_server_message(
        &json!({
            "method": "item/completed",
            "params": {
                "threadId": "wrong-thread",
                "turnId": "turn-live",
                "item": {
                    "id": "msg-wrong-thread",
                    "type": "agentMessage",
                    "text": "Wrong-thread final answer."
                }
            }
        }),
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.input_tx,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &json!({
            "jsonrpc": "2.0",
            "id": "request-wrong-thread",
            "method": "session/request_permission",
            "params": {
                "threadId": "wrong-thread",
                "turnId": "turn-live"
            }
        }),
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.input_tx,
    )
    .unwrap();

    match input_rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        CodexRuntimeCommand::JsonRpcResponse { response } => {
            assert_eq!(
                codex_json_rpc_response_message(&response),
                json!({
                    "jsonrpc": "2.0",
                    "id": "request-wrong-thread",
                    "error": {
                        "code": -32001,
                        "message": "Session unavailable; request could not be delivered."
                    }
                })
            );
        }
        _ => panic!("expected JSON-RPC rejection"),
    }

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());
}

// Pins that a request routed by the completed-turn grace window is still
// answered with an error when there is no active turn. It must not be
// silently dropped, because Codex waits for JSON-RPC request responses.
#[test]
fn shared_codex_server_request_for_completed_turn_returns_json_rpc_error() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) =
        test_shared_codex_runtime("shared-codex-completed-turn-request");

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
        inner.sessions[index].session.status = SessionStatus::Idle;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id,
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                completed_turn_id: Some("turn-completed".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    handle_shared_codex_app_server_message(
        &json!({
            "jsonrpc": "2.0",
            "id": "request-completed-turn",
            "method": "session/request_permission",
            "params": {
                "turnId": "turn-completed"
            }
        }),
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.input_tx,
    )
    .unwrap();

    match input_rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        CodexRuntimeCommand::JsonRpcResponse { response } => {
            assert_eq!(
                codex_json_rpc_response_message(&response),
                json!({
                    "jsonrpc": "2.0",
                    "id": "request-completed-turn",
                    "error": {
                        "code": -32001,
                        "message": "Session unavailable; request could not be delivered."
                    }
                })
            );
        }
        _ => panic!("expected JSON-RPC rejection"),
    }
}

// Pins that a server-initiated JSON-RPC request with no thread id is
// rejected instead of being logged-and-dropped. Newer Codex app-server
// builds can emit global requests such as auth-token refresh; leaving
// those unanswered stalls the shared app-server and makes Codex turns
// look permanently active.
#[test]
fn shared_codex_server_request_missing_thread_id_returns_json_rpc_error() {
    let state = test_app_state();
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let sessions = SharedCodexSessions::new();
    let thread_sessions: SharedCodexThreadMap = Arc::new(Mutex::new(HashMap::new()));
    let (input_tx, input_rx) = mpsc::channel::<CodexRuntimeCommand>();

    handle_shared_codex_app_server_message(
        &json!({
            "jsonrpc": "2.0",
            "id": "request-without-thread",
            "method": "account/chatgptAuthTokens/refresh",
            "params": {}
        }),
        &state,
        "shared-codex-missing-thread",
        &pending_requests,
        &sessions,
        &thread_sessions,
        &input_tx,
    )
    .unwrap();

    match input_rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        CodexRuntimeCommand::JsonRpcResponse { response } => {
            assert_eq!(
                codex_json_rpc_response_message(&response),
                json!({
                    "jsonrpc": "2.0",
                    "id": "request-without-thread",
                    "error": {
                        "code": -32001,
                        "message": "Session unavailable; request could not be delivered."
                    }
                })
            );
        }
        _ => panic!("expected JSON-RPC rejection"),
    }
}

// Pins that while handle_shared_codex_prompt_command blocks waiting on a
// turn/start JSON-RPC response, the writer loop still accepts and
// forwards other CodexRuntimeCommand::JsonRpcResponse items (e.g. an
// approval reply) to the shared stdin.
// Guards against turn/start dispatch starving the writer loop and stalling
// concurrent approval/response traffic on the shared Codex process.
#[test]
fn shared_codex_prompt_command_keeps_writer_loop_responsive_while_turn_start_is_pending() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _runtime_input_rx, process) =
        test_shared_codex_runtime("shared-codex-prompt-writer-responsive");

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

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let writer = SharedBufferWriter::default();
    let thread_writer = writer.clone();
    let thread_pending_requests = pending_requests.clone();
    let thread_state = state.clone();
    let thread_runtime = runtime.clone();
    let (input_tx, input_rx) = mpsc::channel();
    let thread_input_tx = input_tx.clone();

    let writer_thread = std::thread::spawn(move || {
        let mut stdin = thread_writer;
        let runtime_token = RuntimeToken::Codex(thread_runtime.runtime_id.clone());
        while let Ok(command) = input_rx.recv_timeout(Duration::from_millis(250)) {
            match command {
                CodexRuntimeCommand::Prompt {
                    session_id,
                    command,
                } => {
                    let active_turn_generation = command.active_turn_generation;
                    handle_shared_codex_prompt_command_result(
                        &thread_state,
                        &session_id,
                        &runtime_token,
                        active_turn_generation,
                        handle_shared_codex_prompt_command(
                            &mut stdin,
                            &thread_pending_requests,
                            &thread_state,
                            &thread_runtime.runtime_id,
                            &test_missing_shared_codex_home(),
                            &thread_runtime.sessions,
                            &thread_runtime.thread_sessions,
                            &thread_input_tx,
                            None,
                            &session_id,
                            command,
                        ),
                    )
                    .unwrap();
                }
                CodexRuntimeCommand::StartTurnAfterSetup {
                    session_id,
                    thread_id,
                    command,
                } => {
                    let active_turn_generation = command.active_turn_generation;
                    handle_shared_codex_prompt_command_result(
                        &thread_state,
                        &session_id,
                        &runtime_token,
                        active_turn_generation,
                        handle_shared_codex_start_turn(
                            &mut stdin,
                            &thread_pending_requests,
                            &thread_state,
                            &thread_runtime.runtime_id,
                            &thread_runtime.sessions,
                            &thread_runtime.thread_sessions,
                            None,
                            &session_id,
                            &thread_id,
                            None,
                            command,
                        ),
                    )
                    .unwrap();
                }
                CodexRuntimeCommand::JsonRpcResponse { response } => {
                    write_codex_json_rpc_message(
                        &mut stdin,
                        &codex_json_rpc_response_message(&response),
                    )
                    .unwrap();
                }
                _ => panic!("unexpected shared Codex runtime command"),
            }
        }
    });

    input_tx
        .send(CodexRuntimeCommand::Prompt {
            session_id: session_id.clone(),
            command: CodexPromptCommand {
                active_turn_generation: 0,
                approval_policy: CodexApprovalPolicy::Never,
                attachments: Vec::new(),
                cwd: "/tmp".to_owned(),
                model: "gpt-5.4".to_owned(),
                prompt: "check the repo".to_owned(),
                reasoning_effort: CodexReasoningEffort::Medium,
                service_tier: None,
                resume_thread_id: None,
                sandbox_mode: CodexSandboxMode::WorkspaceWrite,
            },
        })
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let written = writer.contents();
        let pending_count = pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned")
            .len();
        if written.contains("\"method\":\"turn/start\"") && pending_count == 1 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "turn/start request should stay pending while the writer loop remains active"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    input_tx
        .send(CodexRuntimeCommand::JsonRpcResponse {
            response: CodexJsonRpcResponseCommand {
                request_id: json!("approval-1"),
                payload: CodexJsonRpcResponsePayload::Result(json!({
                    "outcome": "approved",
                })),
            },
        })
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let written = writer.contents();
        if written.contains("\"id\":\"approval-1\"") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "writer loop should still write JSON-RPC responses while turn/start is pending"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let (_request_id, sender) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));
    sender
        .send(Ok(json!({
            "turn": {
                "id": "turn-1"
            }
        })))
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let (turn_id, pending_turn_start) = {
            let sessions = runtime
                .sessions
                .lock()
                .expect("shared Codex session mutex poisoned");
            let session_state = sessions
                .get(&session_id)
                .expect("shared Codex session state should exist");
            (
                session_state.turn_id.clone(),
                session_state.pending_turn_start_request_id.clone(),
            )
        };
        if turn_id.as_deref() == Some("turn-1") && pending_turn_start.is_some() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "turn/start waiter should record the turn id and retain its watchdog marker until turn/started arrives"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    drop(input_tx);
    writer_thread
        .join()
        .expect("shared Codex writer thread should join cleanly");
}

// Pins that a CodexResponseError::JsonRpc from turn/start is recorded as
// a session-scoped turn failure (status Error, preview set to the error
// message, "Turn failed: ..." transcript message), while the shared
// runtime handle stays attached to the session record.
// Guards against one turn's JSON-RPC rejection tearing down the entire
// shared Codex process for all co-hosted sessions.
#[test]
fn shared_codex_prompt_json_rpc_errors_fail_the_turn_without_tearing_down_runtime() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-prompt-jsonrpc-error");
    let runtime_token = RuntimeToken::Codex(runtime.runtime_id.clone());

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
        inner.sessions[index].session.preview = "Waiting for Codex".to_owned();
    }

    handle_shared_codex_prompt_command_result(
        &state,
        &session_id,
        &runtime_token,
        0,
        Err(anyhow::Error::new(CodexResponseError::JsonRpc(
            "turn/start rejected the request".to_owned(),
        ))),
    )
    .expect("JSON-RPC prompt errors should be recorded as turn failures");

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.status, SessionStatus::Error);
    assert_eq!(session.preview, "turn/start rejected the request");
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. })
            if text == "Turn failed: turn/start rejected the request"
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert!(matches!(record.runtime, SessionRuntime::Codex(_)));
}

// A successful JSON-RPC response without the matching turn/started event must
// not leave the durable turn Active forever. The watchdog is request-scoped,
// clears the stale attachment, and records one terminal failure.
#[test]
fn shared_codex_turn_started_watchdog_terminalizes_its_current_request() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) =
        test_shared_codex_runtime("shared-codex-turn-started-watchdog");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "LIVE TURN".to_owned();
    }
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                pending_turn_start_request_id: Some("turn-start-watchdog".to_owned()),
                active_turn_generation: Some(0),
                thread_id: Some("thread-watchdog".to_owned()),
                turn_id: Some("turn-watchdog".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("thread-watchdog".to_owned(), session_id.clone());

    let interrupt = std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("watchdog should interrupt the accepted turn before terminalizing it");
        match command {
            CodexRuntimeCommand::InterruptTurn {
                response_tx,
                thread_id,
                turn_id,
            } => {
                assert_eq!(thread_id, "thread-watchdog");
                assert_eq!(turn_id, "turn-watchdog");
                response_tx
                    .send(Ok(()))
                    .expect("watchdog interrupt acknowledgement should be received");
            }
            _ => panic!("watchdog should issue an exact turn interrupt"),
        }
    });

    assert!(handle_shared_codex_turn_started_watchdog_expiry(
        &state,
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.runtime_id,
        &session_id,
        "turn-start-watchdog",
        SHARED_CODEX_TURN_STARTED_EVENT_TIMEOUT,
    ));
    interrupt
        .join()
        .expect("watchdog interrupt responder should finish");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert_eq!(record.session.status, SessionStatus::Error);
    assert!(record.session.preview.contains("did not emit turn/started"));
    assert!(matches!(record.runtime, SessionRuntime::None));
    drop(inner);
    assert!(
        !runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .contains_key("thread-watchdog")
    );

    let _ = process.kill();
    let _ = process.wait();
}

// A response without a turn id cannot be interrupted exactly. The app-server
// may still be executing tools after TermAl terminalizes the durable turn, so
// fail closed by retiring the shared runtime and every sibling attachment.
#[test]
fn shared_codex_turn_started_watchdog_missing_turn_id_retires_shared_runtime() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let sibling_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) =
        test_shared_codex_runtime("shared-codex-watchdog-missing-turn-id");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime.clone());
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        for (id, generation, preview) in [
            (&session_id, 1, "MISSING TURN ID"),
            (&sibling_id, 7, "SIBLING TURN"),
        ] {
            let index = inner
                .find_session_index(id)
                .expect("Codex session should exist");
            inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                runtime_id: runtime.runtime_id.clone(),
                input_tx: runtime.input_tx.clone(),
                process: process.clone(),
                shared_session: Some(SharedCodexSessionHandle {
                    runtime: runtime.clone(),
                    session_id: id.clone(),
                }),
            });
            inner.sessions[index].session.status = SessionStatus::Active;
            inner.sessions[index].session.preview = preview.to_owned();
            inner.sessions[index].active_turn_generation = generation;
        }
    }
    {
        let mut sessions = runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        sessions.insert(
            session_id.clone(),
            SharedCodexSessionState {
                pending_turn_start_request_id: Some("turn-start-without-id".to_owned()),
                active_turn_generation: Some(1),
                thread_id: Some("thread-without-turn-id".to_owned()),
                turn_id: None,
                ..SharedCodexSessionState::default()
            },
        );
        sessions.insert(
            sibling_id.clone(),
            SharedCodexSessionState {
                active_turn_generation: Some(7),
                thread_id: Some("thread-sibling".to_owned()),
                turn_id: Some("turn-sibling".to_owned()),
                turn_started: true,
                ..SharedCodexSessionState::default()
            },
        );
    }
    {
        let mut thread_sessions = runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned");
        thread_sessions.insert("thread-without-turn-id".to_owned(), session_id.clone());
        thread_sessions.insert("thread-sibling".to_owned(), sibling_id.clone());
    }

    assert!(handle_shared_codex_turn_started_watchdog_expiry(
        &state,
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.runtime_id,
        &session_id,
        "turn-start-without-id",
        SHARED_CODEX_TURN_STARTED_EVENT_TIMEOUT,
    ));
    match input_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("runtime retirement should request graceful shutdown")
    {
        CodexRuntimeCommand::JsonRpcNotification { method } => {
            assert_eq!(method, "shutdown");
        }
        _ => panic!("missing turn id must retire without an inexact turn interrupt"),
    }
    assert!(
        state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned")
            .is_none()
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let failed = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("affected Codex session should remain");
    assert_eq!(failed.session.status, SessionStatus::Error);
    assert!(failed.session.messages.iter().any(|message| {
        matches!(message, Message::Text { text, .. }
            if text.contains("accepted shared Codex turn has no turn id")
                && text.contains("retiring the shared runtime"))
    }));
    assert!(matches!(failed.runtime, SessionRuntime::None));
    let sibling = inner
        .sessions
        .iter()
        .find(|record| record.session.id == sibling_id)
        .expect("sibling Codex session should remain");
    assert_eq!(sibling.session.status, SessionStatus::Error);
    assert_eq!(sibling.active_turn_generation, 7);
    assert!(matches!(sibling.runtime, SessionRuntime::None));
    drop(inner);
    assert!(
        wait_for_shared_child_exit_timeout(
            &process,
            Duration::from_secs(1),
            "shared Codex runtime without a turn id",
        )
        .expect("shared Codex process status should be readable")
        .is_some()
    );
}

#[test]
fn shared_codex_turn_started_watchdog_extends_while_stdout_is_active() {
    let (cancel_tx, cancel_rx) = mpsc::channel();
    let mut probes = 0usize;
    let timed_out = wait_for_shared_codex_turn_started_or_timeout(
        &cancel_rx,
        SharedCodexTurnStartedWatchdogConfig {
            silence_limit: Duration::from_millis(1),
            max_wait_while_active: Duration::from_secs(30),
            poll_slice: Duration::from_millis(1),
        },
        || {
            probes += 1;
            if probes == 2 {
                cancel_tx
                    .send(())
                    .expect("test cancellation should reach the watchdog");
            }
            Some(Duration::ZERO)
        },
    );

    assert!(!timed_out, "recent stdout must extend the silence budget");
    assert!(
        probes >= 2,
        "the watchdog should survive its first silence-budget poll"
    );
}

#[test]
fn shared_codex_turn_started_watchdog_is_armed_by_the_real_turn_start_waiter() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) =
        test_shared_codex_runtime("shared-codex-wired-turn-started-watchdog");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].active_turn_generation = 9;
    }
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-wired-watchdog".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("thread-wired-watchdog".to_owned(), session_id.clone());

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    handle_shared_codex_start_turn(
        &mut Vec::new(),
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &runtime.thread_sessions,
        None,
        &session_id,
        "thread-wired-watchdog",
        Some(SharedCodexTurnStartedWatchdogConfig::fixed(
            Duration::from_millis(25),
        )),
        CodexPromptCommand {
            active_turn_generation: 9,
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "arm the real watchdog".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .expect("turn/start should be written");
    let (_request_id, response_tx) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));
    let interrupt = std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("wired watchdog should interrupt the accepted turn");
        match command {
            CodexRuntimeCommand::InterruptTurn { response_tx, .. } => response_tx
                .send(Ok(()))
                .expect("watchdog interrupt acknowledgement should send"),
            _ => panic!("wired watchdog should issue an exact turn interrupt"),
        }
    });
    response_tx
        .send(Ok(json!({ "turn": { "id": "turn-wired-watchdog" } })))
        .expect("turn/start response should send");

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let terminalized = {
            let inner = state.inner.lock().expect("state mutex poisoned");
            let record = inner
                .sessions
                .iter()
                .find(|record| record.session.id == session_id)
                .expect("Codex session should remain");
            record.session.status == SessionStatus::Error
                && matches!(record.runtime, SessionRuntime::None)
        };
        if terminalized {
            break;
        }
        assert!(Instant::now() < deadline, "wired watchdog should expire");
        std::thread::sleep(Duration::from_millis(5));
    }
    interrupt
        .join()
        .expect("watchdog interrupt responder should finish");
    let _ = process.kill();
    let _ = process.wait();
}

#[test]
fn shared_codex_turn_started_event_cancels_the_real_waiter_watchdog() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) =
        test_shared_codex_runtime("shared-codex-cancelled-turn-started-watchdog");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].active_turn_generation = 10;
    }
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-cancelled-watchdog".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("thread-cancelled-watchdog".to_owned(), session_id.clone());

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    handle_shared_codex_start_turn(
        &mut Vec::new(),
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &runtime.thread_sessions,
        None,
        &session_id,
        "thread-cancelled-watchdog",
        Some(SharedCodexTurnStartedWatchdogConfig::fixed(
            Duration::from_secs(30),
        )),
        CodexPromptCommand {
            active_turn_generation: 10,
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "cancel the real watchdog".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .expect("turn/start should be written");
    let (_request_id, response_tx) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));
    response_tx
        .send(Ok(json!({ "turn": { "id": "turn-cancelled-watchdog" } })))
        .expect("turn/start response should send");
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let armed = runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned")
            .get(&session_id)
            .is_some_and(|session| session.turn_started_watchdog_cancel_tx.is_some());
        if armed {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "watchdog should arm after response"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    handle_shared_codex_app_server_message(
        &json!({
            "method": "turn/started",
            "params": {
                "threadId": "thread-cancelled-watchdog",
                "turn": { "id": "turn-cancelled-watchdog" }
            }
        }),
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.input_tx,
    )
    .expect("turn/started should cancel the watchdog");
    assert!(matches!(
        input_rx.recv_timeout(Duration::from_millis(200)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should remain");
    assert_eq!(record.session.status, SessionStatus::Active);
    assert!(
        record
            .runtime
            .matches_runtime_token(&RuntimeToken::Codex(runtime.runtime_id.clone()))
    );
    drop(inner);
    let _ = process.kill();
    let _ = process.wait();
}

#[test]
fn idless_turn_started_before_response_reconciles_without_arming_watchdog() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) =
        test_shared_codex_runtime("shared-codex-idless-turn-started-before-response");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].active_turn_generation = 11;
    }
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-idless-before-response".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert(
            "thread-idless-before-response".to_owned(),
            session_id.clone(),
        );

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    handle_shared_codex_start_turn(
        &mut Vec::new(),
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &runtime.thread_sessions,
        None,
        &session_id,
        "thread-idless-before-response",
        Some(SharedCodexTurnStartedWatchdogConfig::fixed(
            Duration::from_secs(30),
        )),
        CodexPromptCommand {
            active_turn_generation: 11,
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "accept an id-less early start".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .expect("turn/start should be written");
    let (request_id, response_tx) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));

    handle_shared_codex_app_server_message(
        &json!({
            "method": "turn/started",
            "params": {
                "threadId": "thread-idless-before-response",
                "turn": {}
            }
        }),
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.input_tx,
    )
    .expect("id-less turn/started should be retained until the response arrives");
    {
        let sessions = runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        let session = sessions
            .get(&session_id)
            .expect("shared Codex session should remain");
        assert_eq!(
            session.pending_turn_start_request_id.as_deref(),
            Some(request_id.as_str())
        );
        assert!(session.turn_started_before_response);
        assert!(session.turn_started);
        assert!(session.turn_id.is_none());
        assert!(session.turn_started_watchdog_cancel_tx.is_none());
    }

    response_tx
        .send(Ok(json!({
            "turn": { "id": "turn-idless-before-response" }
        })))
        .expect("turn/start response should send");
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let reconciled = {
            let sessions = runtime
                .sessions
                .lock()
                .expect("shared Codex session mutex poisoned");
            sessions.get(&session_id).is_some_and(|session| {
                session.pending_turn_start_request_id.is_none()
                    && !session.turn_started_before_response
                    && session.turn_started
                    && session.turn_id.as_deref() == Some("turn-idless-before-response")
                    && session.turn_started_watchdog_cancel_tx.is_none()
            })
        };
        if reconciled {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "response waiter should reconcile the early notification"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(matches!(
        input_rx.recv_timeout(Duration::from_millis(200)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should remain");
    assert_eq!(record.session.status, SessionStatus::Active);
    drop(inner);
    let _ = process.kill();
    let _ = process.wait();
}

#[test]
fn idless_turn_completed_before_response_finishes_the_armed_turn() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) =
        test_shared_codex_runtime("shared-codex-idless-completed-before-response");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].active_turn_generation = 12;
    }
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-idless-completed-before-response".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert(
            "thread-idless-completed-before-response".to_owned(),
            session_id.clone(),
        );

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    handle_shared_codex_start_turn(
        &mut Vec::new(),
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &runtime.thread_sessions,
        None,
        &session_id,
        "thread-idless-completed-before-response",
        Some(SharedCodexTurnStartedWatchdogConfig::fixed(
            Duration::from_secs(30),
        )),
        CodexPromptCommand {
            active_turn_generation: 12,
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "finish before the response waiter runs".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .expect("turn/start should be written");
    let (_request_id, response_tx) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));

    for message in [
        json!({
            "method": "turn/started",
            "params": {
                "threadId": "thread-idless-completed-before-response",
                "turn": {}
            }
        }),
        json!({
            "method": "turn/completed",
            "params": {
                "threadId": "thread-idless-completed-before-response",
                "turn": {}
            }
        }),
    ] {
        handle_shared_codex_app_server_message(
            &message,
            &state,
            &runtime.runtime_id,
            &pending_requests,
            &runtime.sessions,
            &runtime.thread_sessions,
            &runtime.input_tx,
        )
        .expect("the early id-less lifecycle event should be accepted");
    }
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == session_id)
            .expect("Codex session should remain");
        assert_eq!(record.session.status, SessionStatus::Idle);
    }
    {
        let sessions = runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        let session = sessions
            .get(&session_id)
            .expect("shared Codex session should remain registered");
        assert!(session.pending_turn_start_request_id.is_none());
        assert!(!session.turn_started_before_response);
        assert!(!session.turn_started);
        assert!(session.turn_started_watchdog_cancel_tx.is_none());
    }

    response_tx
        .send(Ok(json!({
            "turn": { "id": "turn-too-late-for-completed" }
        })))
        .expect("the late response should still release its waiter");
    std::thread::sleep(Duration::from_millis(25));
    assert!(matches!(
        input_rx.recv_timeout(Duration::from_millis(200)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should remain");
    assert_eq!(record.session.status, SessionStatus::Idle);
    drop(inner);
    let _ = process.kill();
    let _ = process.wait();
}

// If the accepted turn cannot be interrupted, reporting Error is not enough:
// the shared process may still be executing tools. The watchdog must retire
// that exact runtime before it terminalizes the durable turn.
#[test]
fn shared_codex_turn_started_watchdog_retires_runtime_when_interrupt_fails() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) =
        test_shared_codex_runtime("shared-codex-turn-started-watchdog-interrupt-failure");

    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime.clone());
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "LIVE TURN".to_owned();
    }
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                pending_turn_start_request_id: Some(
                    "turn-start-watchdog-interrupt-failure".to_owned(),
                ),
                active_turn_generation: Some(0),
                thread_id: Some("thread-watchdog-interrupt-failure".to_owned()),
                turn_id: Some("turn-watchdog-interrupt-failure".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert(
            "thread-watchdog-interrupt-failure".to_owned(),
            session_id.clone(),
        );

    let interrupt = std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("watchdog should attempt to interrupt the accepted turn");
        match command {
            CodexRuntimeCommand::InterruptTurn { response_tx, .. } => {
                response_tx
                    .send(Err("scripted interrupt refusal".to_owned()))
                    .expect("watchdog interrupt refusal should be received");
            }
            _ => panic!("watchdog should issue an exact turn interrupt"),
        }
    });

    assert!(handle_shared_codex_turn_started_watchdog_expiry(
        &state,
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.runtime_id,
        &session_id,
        "turn-start-watchdog-interrupt-failure",
        SHARED_CODEX_TURN_STARTED_EVENT_TIMEOUT,
    ));
    interrupt
        .join()
        .expect("watchdog interrupt responder should finish");

    assert!(
        state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned")
            .is_none(),
        "a failed interrupt must retire the shared runtime slot"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert_eq!(record.session.status, SessionStatus::Error);
    assert!(record.session.messages.iter().any(|message| {
        matches!(message, Message::Text { text, .. } if text.contains("interrupt failed"))
    }));
    assert!(matches!(record.runtime, SessionRuntime::None));
    drop(inner);

    let _ = process.kill();
    let _ = process.wait();
}

// The request id is the watchdog generation. Once turn/started cleared it (or
// a newer request replaced it), the old timer must not touch the live runtime.
#[test]
fn shared_codex_turn_started_watchdog_ignores_a_completed_or_superseded_request() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-turn-started-watchdog-stale");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                pending_turn_start_request_id: Some("newer-turn-start".to_owned()),
                active_turn_generation: Some(0),
                thread_id: Some("thread-current".to_owned()),
                turn_id: Some("turn-current".to_owned()),
                turn_started: true,
                ..SharedCodexSessionState::default()
            },
        );

    assert!(!handle_shared_codex_turn_started_watchdog_expiry(
        &state,
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.runtime_id,
        &session_id,
        "old-turn-start",
        SHARED_CODEX_TURN_STARTED_EVENT_TIMEOUT,
    ));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert_eq!(record.session.status, SessionStatus::Active);
    assert!(
        record
            .runtime
            .matches_runtime_token(&RuntimeToken::Codex(runtime.runtime_id.clone()))
    );
    drop(inner);

    let _ = process.kill();
    let _ = process.wait();
}

// A shared app-server runtime id identifies the process, not a particular
// logical turn. A watchdog that armed for generation N must not terminalize a
// successor turn N+1 merely because both use the same shared process.
#[test]
fn shared_codex_turn_started_watchdog_ignores_a_successor_on_the_same_runtime() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) =
        test_shared_codex_runtime("shared-codex-turn-started-watchdog-successor");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "SUCCESSOR TURN".to_owned();
        inner.sessions[index].active_turn_generation = 2;
    }
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                pending_turn_start_request_id: Some("old-turn-start".to_owned()),
                active_turn_generation: Some(1),
                thread_id: Some("thread-old".to_owned()),
                turn_id: Some("turn-old".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );

    assert!(!handle_shared_codex_turn_started_watchdog_expiry(
        &state,
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.runtime_id,
        &session_id,
        "old-turn-start",
        SHARED_CODEX_TURN_STARTED_EVENT_TIMEOUT,
    ));
    assert!(matches!(
        input_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert_eq!(record.session.status, SessionStatus::Active);
    assert_eq!(record.session.preview, "SUCCESSOR TURN");
    assert_eq!(record.active_turn_generation, 2);
    assert!(
        record
            .runtime
            .matches_runtime_token(&RuntimeToken::Codex(runtime.runtime_id.clone()))
    );
    drop(inner);

    let _ = process.kill();
    let _ = process.wait();
}

#[test]
fn watchdog_routing_cleanup_preserves_a_successor_turn_generation() {
    let sessions = SharedCodexSessions::new();
    sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            "session-successor".to_owned(),
            SharedCodexSessionState {
                thread_id: Some("thread-shared".to_owned()),
                active_turn_generation: Some(12),
                turn_id: Some("turn-successor".to_owned()),
                turn_started: true,
                ..SharedCodexSessionState::default()
            },
        );
    let thread_sessions: SharedCodexThreadMap = Arc::new(Mutex::new(HashMap::from([(
        "thread-shared".to_owned(),
        "session-successor".to_owned(),
    )])));

    forget_shared_codex_thread_if_generation_matches(
        &sessions,
        &thread_sessions,
        "session-successor",
        "thread-shared",
        11,
    );

    let sessions = sessions.lock().expect("shared Codex mutex poisoned");
    let successor = sessions
        .get("session-successor")
        .expect("successor attachment should remain");
    assert_eq!(successor.active_turn_generation, Some(12));
    assert_eq!(successor.thread_id.as_deref(), Some("thread-shared"));
    assert_eq!(successor.turn_id.as_deref(), Some("turn-successor"));
    drop(sessions);
    assert_eq!(
        thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .get("thread-shared")
            .map(String::as_str),
        Some("session-successor")
    );
}

// Pins that a startup timeout against a SILENT app-server is treated as a
// shared-transport failure, not as one bad turn. If the server said nothing for
// the entire wait window it is wedge-shaped, and keeping the shared runtime
// attached would route every later Codex session into the same stuck process —
// nothing else would ever retire it (reader EOF and the `wait()` thread only
// fire when the process actually dies; the stdin watchdog only covers blocked
// writes). The busy-server counterpart is
// `shared_codex_startup_timeout_on_active_server_fails_only_that_turn`.
#[test]
fn shared_codex_startup_timeout_on_silent_server_tears_down_runtime() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let idle_session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-startup-timeout");
    let runtime_token = RuntimeToken::Codex(runtime.runtime_id.clone());

    {
        let mut shared_runtime = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        *shared_runtime = Some(runtime.clone());
    }

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Waiting for Codex".to_owned();

        let idle_index = inner
            .find_session_index(&idle_session_id)
            .expect("idle Codex session should exist");
        inner.sessions[idle_index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: idle_session_id.clone(),
            }),
        });
        inner.sessions[idle_index].session.status = SessionStatus::Idle;
        inner.sessions[idle_index].session.preview = "Idle Codex tab".to_owned();
    }

    // `test_shared_codex_runtime` stamps stdout activity at construction, so a
    // zero-length wait window means the server cannot have spoken DURING the
    // wait — the silence condition — without back-dating an `Instant` (which
    // would underflow on a freshly booted machine).
    handle_shared_codex_startup_response_error(
        &state,
        &runtime.runtime_id,
        &session_id,
        0,
        Duration::ZERO,
        CodexResponseError::Timeout(
            "timed out waiting for Codex app-server response to `turn/start`".to_owned(),
        ),
    );

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.status, SessionStatus::Error);
    assert!(
        session
            .preview
            .contains("failed to communicate with shared Codex app-server")
    );
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. })
            if text.contains("Turn failed: failed to communicate with shared Codex app-server")
    ));

    let idle_session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == idle_session_id)
        .expect("idle session should remain present");
    assert_eq!(idle_session.status, SessionStatus::Idle);
    assert_eq!(idle_session.preview, "Idle Codex tab");

    assert!(
        state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned")
            .is_none()
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert!(matches!(record.runtime, SessionRuntime::None));
    let idle_record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == idle_session_id)
        .expect("idle Codex session should exist");
    assert!(matches!(idle_record.runtime, SessionRuntime::None));

    drop(inner);
    drop(runtime_token);
}

/// Builds a pending request entry the liveness-scaled waiter can be pointed
/// at, plus the sender a test uses to play the app-server's side.
fn test_pending_codex_response(
    request_id: &str,
) -> (
    CodexPendingRequestMap,
    PendingCodexJsonRpcRequest,
    mpsc::Sender<std::result::Result<Value, CodexResponseError>>,
) {
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let (tx, rx) = mpsc::channel();
    pending_requests
        .lock()
        .expect("Codex pending requests mutex poisoned")
        .insert(request_id.to_owned(), tx.clone());
    (
        pending_requests,
        PendingCodexJsonRpcRequest {
            request_id: request_id.to_owned(),
            response_rx: rx,
        },
        tx,
    )
}

// Pins the extension rule of the liveness-scaled waiter: a response that
// lands AFTER the silence budget still completes as long as the app-server
// kept emitting stdout — under the old flat wait this exact timing failed the
// turn. This reproduces the large-resume-behind-a-larger-sibling incident in
// miniature.
#[test]
fn shared_codex_patient_wait_outlasts_silence_budget_while_server_is_active() {
    let state = test_app_state();
    let (runtime, _input_rx, _process) = test_shared_codex_runtime("shared-codex-patient-wait");
    {
        let mut shared_runtime = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        *shared_runtime = Some(runtime.clone());
    }
    let (pending_requests, pending, tx) = test_pending_codex_response("patient-wait");

    // The app-server side: stdout stays chatty the whole time, and the
    // response arrives well after the 100ms silence budget.
    let stamper_activity = runtime.stdout_activity.clone();
    let server = std::thread::spawn(move || {
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(10));
            *stamper_activity
                .lock()
                .expect("shared Codex stdout activity mutex poisoned") = std::time::Instant::now();
        }
        let _ = tx.send(Ok(json!({"thread": {"id": "thread-patient"}})));
    });

    let result = wait_for_shared_codex_response_while_server_active(
        &pending_requests,
        pending,
        "thread/resume",
        &state,
        &runtime.runtime_id,
        Duration::from_millis(100),
        Duration::from_secs(5),
        Duration::from_millis(20),
    );
    server.join().expect("server thread should finish");

    assert_eq!(
        result.expect("late response from an active server should succeed"),
        json!({"thread": {"id": "thread-patient"}})
    );
}

// Pins that a server silent past the budget still fails on the old schedule:
// the waiter must NOT extend for a server that stopped talking, or a wedged
// process would hold sessions on "working" until the hard cap. The stamp is
// only ever the construction-time one here, so silence and elapsed cross the
// budget together.
#[test]
fn shared_codex_patient_wait_gives_up_on_silent_server_at_silence_budget() {
    let state = test_app_state();
    let (runtime, _input_rx, _process) = test_shared_codex_runtime("shared-codex-silent-wait");
    {
        let mut shared_runtime = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        *shared_runtime = Some(runtime.clone());
    }
    let (pending_requests, pending, _tx) = test_pending_codex_response("silent-wait");

    let started = std::time::Instant::now();
    let result = wait_for_shared_codex_response_while_server_active(
        &pending_requests,
        pending,
        "thread/resume",
        &state,
        &runtime.runtime_id,
        Duration::from_millis(80),
        Duration::from_secs(30),
        Duration::from_millis(20),
    );
    let took = started.elapsed();

    assert!(matches!(
        result,
        Err(CodexResponseError::Timeout(detail))
            if detail.contains("timed out waiting for Codex app-server response to `thread/resume`")
    ));
    // Far below the 30s active-cap: the silent budget governed the give-up.
    assert!(took < Duration::from_secs(5), "took {took:?}");
    // The abandoned entry must not linger — a late response has nobody to
    // reach and unknown ids are dropped by the reader.
    assert!(
        pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned")
            .is_empty()
    );
}

// Pins the hard cap: stdout activity alone cannot extend a wait forever — a
// lost request against a chatty server fails once `max_wait_while_active`
// elapses (scoped downstream, since the server is demonstrably alive).
#[test]
fn shared_codex_patient_wait_hits_hard_cap_despite_active_server() {
    let state = test_app_state();
    let (runtime, _input_rx, _process) = test_shared_codex_runtime("shared-codex-capped-wait");
    {
        let mut shared_runtime = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        *shared_runtime = Some(runtime.clone());
    }
    let (pending_requests, pending, _tx) = test_pending_codex_response("capped-wait");

    let stamper_activity = runtime.stdout_activity.clone();
    let stamper_done = Arc::new(Mutex::new(false));
    let stamper_stop = stamper_done.clone();
    let stamper = std::thread::spawn(move || {
        while !*stamper_stop.lock().expect("stamper stop mutex poisoned") {
            std::thread::sleep(Duration::from_millis(10));
            *stamper_activity
                .lock()
                .expect("shared Codex stdout activity mutex poisoned") = std::time::Instant::now();
        }
    });

    let started = std::time::Instant::now();
    let result = wait_for_shared_codex_response_while_server_active(
        &pending_requests,
        pending,
        "turn/start",
        &state,
        &runtime.runtime_id,
        Duration::from_secs(30),
        Duration::from_millis(200),
        Duration::from_millis(20),
    );
    let took = started.elapsed();
    *stamper_done.lock().expect("stamper stop mutex poisoned") = true;
    stamper.join().expect("stamper thread should finish");

    assert!(matches!(
        result,
        Err(CodexResponseError::Timeout(detail))
            if detail.contains("timed out waiting for Codex app-server response to `turn/start`")
    ));
    // The cap is a floor on the give-up (an active server is never failed
    // early) and the assertion ceiling is generous for scheduler noise.
    assert!(took >= Duration::from_millis(200), "took {took:?}");
    assert!(took < Duration::from_secs(10), "took {took:?}");
}

// Pins the degraded path: when the shared slot no longer holds this runtime,
// the probe reports no liveness and the wait falls back to the silent budget
// instead of extending on a runtime that is already gone.
#[test]
fn shared_codex_patient_wait_without_registered_runtime_uses_silence_budget() {
    let state = test_app_state();
    let (runtime, _input_rx, _process) =
        test_shared_codex_runtime("shared-codex-unregistered-wait");
    // Deliberately NOT placed into `state.shared_codex_runtime`.
    let (pending_requests, pending, _tx) = test_pending_codex_response("unregistered-wait");

    let started = std::time::Instant::now();
    let result = wait_for_shared_codex_response_while_server_active(
        &pending_requests,
        pending,
        "thread/resume",
        &state,
        &runtime.runtime_id,
        Duration::from_millis(80),
        Duration::from_secs(30),
        Duration::from_millis(20),
    );
    let took = started.elapsed();

    assert!(matches!(result, Err(CodexResponseError::Timeout(_))));
    assert!(took < Duration::from_secs(5), "took {took:?}");
}

// Pins the busy-server side of the timeout split: when the app-server emitted
// stdout during the wait window (here: the activity stamp is fresh and the
// window is 120s), a response timeout fails ONLY the requesting session's
// turn. The shared runtime survives and a sibling session's in-flight turn is
// untouched. The incident this pins against: one slow `thread/resume` (a
// ~39MB rollout, contending with a ~170MB thread mid-turn) killed every
// session on the shared server, and the replacement server then had to
// re-parse the same rollouts — re-creating the very contention that caused
// the timeout.
#[test]
fn shared_codex_startup_timeout_on_active_server_fails_only_that_turn() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let busy_session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-busy-timeout");

    {
        let mut shared_runtime = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        *shared_runtime = Some(runtime.clone());
    }

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Waiting for Codex".to_owned();

        let busy_index = inner
            .find_session_index(&busy_session_id)
            .expect("busy Codex session should exist");
        inner.sessions[busy_index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: busy_session_id.clone(),
            }),
        });
        inner.sessions[busy_index].session.status = SessionStatus::Active;
        inner.sessions[busy_index].session.preview = "Streaming a sibling turn".to_owned();
    }

    handle_shared_codex_startup_response_error(
        &state,
        &runtime.runtime_id,
        &session_id,
        0,
        Duration::from_secs(120),
        CodexResponseError::Timeout(
            "timed out waiting for Codex app-server response to `turn/start`".to_owned(),
        ),
    );

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.status, SessionStatus::Error);
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. })
            if text.contains(
                "Turn failed: timed out waiting for Codex app-server response to `turn/start`"
            ) && text.contains("only this turn was abandoned")
    ));

    let busy_session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == busy_session_id)
        .expect("busy session should remain present");
    assert_eq!(busy_session.status, SessionStatus::Active);
    assert_eq!(busy_session.preview, "Streaming a sibling turn");

    assert!(
        state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned")
            .as_ref()
            .is_some_and(|shared_runtime| shared_runtime.runtime_id == runtime.runtime_id)
    );

    // Scoped failure must not detach anyone from the surviving runtime.
    let inner = state.inner.lock().expect("state mutex poisoned");
    for id in [&session_id, &busy_session_id] {
        let record = inner
            .sessions
            .iter()
            .find(|record| &record.session.id == id)
            .expect("session record should exist");
        assert!(matches!(record.runtime, SessionRuntime::Codex(_)));
    }
    drop(inner);
}

// Pins that an old thread-setup waiter cannot retire the shared app-server
// after its session has already stopped or rebound away from that runtime.
#[test]
fn shared_codex_stale_thread_setup_timeout_does_not_clear_shared_runtime() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, _process) =
        test_shared_codex_runtime("shared-codex-stale-thread-setup-timeout");

    {
        let mut shared_runtime = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        *shared_runtime = Some(runtime.clone());
    }

    handle_shared_codex_thread_setup_response_error_if_current(
        &runtime.sessions,
        &state,
        &runtime.runtime_id,
        &session_id,
        "old-thread-setup-request",
        Duration::from_secs(180),
        CodexResponseError::Timeout(
            "timed out waiting for Codex app-server response to `thread/start`".to_owned(),
        ),
    );

    assert!(
        state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned")
            .as_ref()
            .is_some_and(|shared_runtime| shared_runtime.runtime_id == runtime.runtime_id)
    );

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should remain present");
    assert_eq!(session.status, SessionStatus::Idle);
    assert!(session.messages.is_empty());
}

// Pins that a stale thread-setup waiter cannot fail a newer setup attempt
// on the same shared app-server runtime after the session has restarted.
#[test]
fn shared_codex_stale_thread_setup_timeout_ignores_newer_same_runtime_request() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-stale-thread-setup-same-runtime");

    {
        let mut shared_runtime = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        *shared_runtime = Some(runtime.clone());
    }

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
        inner.sessions[index].session.preview = "New thread setup is active".to_owned();
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                pending_thread_setup: Some(test_pending_codex_thread_setup(
                    "new-thread-setup-request",
                )),
                ..SharedCodexSessionState::default()
            },
        );

    handle_shared_codex_thread_setup_response_error_if_current(
        &runtime.sessions,
        &state,
        &runtime.runtime_id,
        &session_id,
        "old-thread-setup-request",
        Duration::from_secs(180),
        CodexResponseError::Timeout(
            "timed out waiting for Codex app-server response to `thread/start`".to_owned(),
        ),
    );

    assert!(
        state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned")
            .as_ref()
            .is_some_and(|shared_runtime| shared_runtime.runtime_id == runtime.runtime_id)
    );

    let pending_request_id = runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .get(&session_id)
        .and_then(|session| {
            session
                .pending_thread_setup
                .as_ref()
                .map(|setup| setup.request_id.clone())
        });
    assert_eq!(
        pending_request_id.as_deref(),
        Some("new-thread-setup-request")
    );

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should remain present");
    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(session.preview, "New thread setup is active");
    assert!(session.messages.is_empty());
}

// Pins the runtime-exit queued-dispatch ordering: for a dying shared Codex
// runtime, the AppState shared-runtime slot is cleared before the queued turn
// dispatcher attempts recovery work.
#[test]
fn shared_codex_runtime_exit_clears_shared_slot_before_queued_dispatch_attempt() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-runtime-exit-clear-before-dispatch");
    let runtime_token = RuntimeToken::Codex(runtime.runtime_id.clone());

    {
        let mut shared_runtime = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        *shared_runtime = Some(runtime.clone());
    }

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        let queued_prompt_id = inner.next_message_id();
        let record = &mut inner.sessions[index];
        record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        record.session.status = SessionStatus::Active;
        record.session.preview = "Waiting for Codex".to_owned();
        record.remote_id = Some("remote-proxy-to-block-dispatch-spawn".to_owned());
        record.remote_session_id = Some("remote-session".to_owned());
        record.queued_prompts.push_back(QueuedPromptRecord {
            source: QueuedPromptSource::User,
            attachments: Vec::new(),
            pending_prompt: PendingPrompt {
                attachments: Vec::new(),
                id: queued_prompt_id,
                timestamp: stamp_now(),
                text: "queued recovery prompt".to_owned(),
                expanded_text: None,
                source: None,
            },
        });
    }

    let error = state
        .handle_runtime_exit_if_matches(
            &session_id,
            &runtime_token,
            Some("shared app-server exited"),
        )
        .expect_err("remote proxy queued dispatch should fail after slot clear");
    assert!(
        format!("{error:#}").contains("remote proxy sessions must dispatch"),
        "unexpected runtime-exit error: {error:#}"
    );
    assert!(
        state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned")
            .is_none(),
        "shared runtime slot should be cleared before queued dispatch can fail"
    );
}

// Pins that an item/agentMessage/delta arriving while turn_started=false
// is held (no transcript write) and then takes effect once turn/started
// flips the flag for the matching turn_id.
// Guards against streamed text appearing before Codex has acknowledged the
// turn, which would ride ahead of turn-setup notices.
#[test]
fn shared_codex_app_server_agent_message_delta_waits_for_turn_started() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-app-server-agent-delta-turn-started");

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

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                turn_id: Some("turn-current".to_owned()),
                turn_started: false,
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let delta = json!({
        "method": "item/agentMessage/delta",
        "params": {
            "threadId": "conversation-123",
            "itemId": "msg-1",
            "delta": "Hello"
        }
    });
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-current"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &delta,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &delta,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.preview, "Hello");
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Hello"
    ));
}

// Pins that a server-initiated request (item/tool/requestUserInput) is
// rejected while turn_started=false, and only records a
// Message::UserInputRequest once turn/started has fired for the matching turn_id.
// Guards against user-input prompts rendering before the turn is actually
// live, while still answering Codex's JSON-RPC request instead of dropping it.
#[test]
fn shared_codex_app_server_request_waits_for_turn_started() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) =
        test_shared_codex_runtime("shared-codex-app-server-request-turn-started");

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

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                turn_id: Some("turn-current".to_owned()),
                turn_started: false,
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let request = json!({
        "id": "req-1",
        "method": "item/tool/requestUserInput",
        "params": {
            "threadId": "conversation-123",
            "turnId": "turn-current",
            "questions": [
                {
                    "header": "Scope",
                    "id": "scope",
                    "question": "What should Codex review?"
                }
            ]
        }
    });
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-current"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &request,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.input_tx,
    )
    .unwrap();

    match input_rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        CodexRuntimeCommand::JsonRpcResponse { response } => {
            assert_eq!(
                codex_json_rpc_response_message(&response),
                json!({
                    "jsonrpc": "2.0",
                    "id": "req-1",
                    "error": {
                        "code": -32001,
                        "message": "Session unavailable; request could not be delivered."
                    }
                })
            );
        }
        _ => panic!("expected JSON-RPC rejection"),
    }

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &request,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(matches!(
        session.messages.last(),
        Some(Message::UserInputRequest { title, detail, state, .. })
            if title == "Codex needs input"
                && detail == "Codex requested additional input for \"Scope\"."
                && *state == InteractionRequestState::Pending
    ));
}

fn set_active_codex_approval_policy(
    state: &AppState,
    session_id: &str,
    active_policy: CodexApprovalPolicy,
    configured_policy: CodexApprovalPolicy,
) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(session_id)
        .expect("Codex session should exist");
    inner.sessions[index].active_codex_approval_policy = Some(active_policy);
    inner.sessions[index].codex_approval_policy = configured_policy;
    inner.sessions[index].session.approval_policy = Some(configured_policy);
}

fn install_read_only_codex_delegation(state: &AppState, child_session_id: &str) {
    let parent_session_id = test_session_id(state, Agent::Codex);
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let delegation_id = inner.next_delegation_id();
    let child_index = inner
        .find_session_index(child_session_id)
        .expect("Codex child session should exist");
    inner.sessions[child_index].session.parent_delegation_id = Some(delegation_id.clone());
    inner.delegations.push(DelegationRecord {
        id: delegation_id,
        parent_session_id,
        child_session_id: child_session_id.to_owned(),
        mode: DelegationMode::Reviewer,
        status: DelegationStatus::Running,
        title: "Read-only Codex reviewer".to_owned(),
        prompt: "Review without writing.".to_owned(),
        cwd: "/tmp".to_owned(),
        agent: Agent::Codex,
        model: None,
        write_policy: DelegationWritePolicy::ReadOnly,
        created_at: stamp_now(),
        started_at: Some(stamp_now()),
        completed_at: None,
        result: None,
        submitted_review_result: None,
        post_submission_transport_error: None,
        review_result_recovery_probe_attempt: None,
        review_result_recovery_error: None,
        review_result_schema_version: None,
        review_result_required: false,
        review_result_submission_attempt: 1,
        result_parser_version: 0,
    });
}

fn codex_auto_approval_requests() -> Vec<(&'static str, Value, Value, Value)> {
    vec![
        (
            "item/commandExecution/requestApproval",
            json!({
                "id": "command-approval",
                "params": { "command": "cargo test", "cwd": "/tmp" }
            }),
            json!("command-approval"),
            json!({ "decision": "accept" }),
        ),
        (
            "item/fileChange/requestApproval",
            json!({
                "id": "file-approval",
                "params": { "reason": "Apply the requested fix" }
            }),
            json!("file-approval"),
            json!({ "decision": "accept" }),
        ),
        (
            "item/permissions/requestApproval",
            json!({
                "id": "permissions-approval",
                "params": {
                    "permissions": {
                        "network": { "enabled": true }
                    }
                }
            }),
            json!("permissions-approval"),
            json!({
                "permissions": {
                    "network": { "enabled": true }
                },
                "scope": "turn"
            }),
        ),
    ]
}

#[test]
fn codex_auto_approve_maps_to_native_on_request() {
    assert_eq!(
        CodexApprovalPolicy::AutoApprove.as_cli_value(),
        "on-request"
    );
    assert_eq!(
        serde_json::to_value(CodexApprovalPolicy::AutoApprove).unwrap(),
        json!("auto-approve")
    );
    assert_eq!(
        codex_approval_policy_from_json_value(&json!("auto-approve")),
        Some(CodexApprovalPolicy::AutoApprove)
    );
}

#[test]
fn codex_auto_approve_accepts_all_approval_kinds_without_pending_cards() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    // The active turn owns the decision even if the mutable next-turn setting
    // has already changed.
    set_active_codex_approval_policy(
        &state,
        &session_id,
        CodexApprovalPolicy::AutoApprove,
        CodexApprovalPolicy::OnRequest,
    );
    let (input_tx, input_rx) = mpsc::channel();

    for (method, request, expected_request_id, expected_result) in codex_auto_approval_requests() {
        assert!(
            try_auto_respond_codex_approval_request(
                method,
                &request,
                &state,
                &session_id,
                &input_tx,
            )
            .expect("AutoApprove request should be classified")
        );
        assert!(matches!(
            input_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            CodexRuntimeCommand::JsonRpcResponse {
                response: CodexJsonRpcResponseCommand {
                    request_id,
                    payload: CodexJsonRpcResponsePayload::Result(result),
                }
            } if request_id == expected_request_id && result == expected_result
        ));
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = &inner.sessions[inner
        .find_session_index(&session_id)
        .expect("Codex session should exist")];
    assert!(record.session.messages.is_empty());
    assert!(record.pending_codex_approvals.is_empty());
}

#[test]
fn codex_auto_approve_declines_all_approval_kinds_for_read_only_delegations() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    install_read_only_codex_delegation(&state, &session_id);
    set_active_codex_approval_policy(
        &state,
        &session_id,
        CodexApprovalPolicy::AutoApprove,
        CodexApprovalPolicy::AutoApprove,
    );
    let (input_tx, input_rx) = mpsc::channel();

    for (method, request, expected_request_id, accepted_result) in codex_auto_approval_requests() {
        assert!(
            try_auto_respond_codex_approval_request(
                method,
                &request,
                &state,
                &session_id,
                &input_tx,
            )
            .expect("read-only AutoApprove request should be classified")
        );
        let expected_result = if method == "item/permissions/requestApproval" {
            json!({ "permissions": {}, "scope": "turn" })
        } else {
            json!({ "decision": "decline" })
        };
        assert_ne!(expected_result, accepted_result);
        assert!(matches!(
            input_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            CodexRuntimeCommand::JsonRpcResponse {
                response: CodexJsonRpcResponseCommand {
                    request_id,
                    payload: CodexJsonRpcResponsePayload::Result(result),
                }
            } if request_id == expected_request_id && result == expected_result
        ));
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = &inner.sessions[inner
        .find_session_index(&session_id)
        .expect("Codex session should exist")];
    assert!(record.session.messages.is_empty());
    assert!(record.pending_codex_approvals.is_empty());
}

#[test]
fn codex_auto_approve_leaves_non_approval_requests_and_next_turn_settings_alone() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    set_active_codex_approval_policy(
        &state,
        &session_id,
        CodexApprovalPolicy::OnRequest,
        CodexApprovalPolicy::AutoApprove,
    );
    let (input_tx, input_rx) = mpsc::channel();
    let command_request = &codex_auto_approval_requests()[0];
    assert!(
        !try_auto_respond_codex_approval_request(
            command_request.0,
            &command_request.1,
            &state,
            &session_id,
            &input_tx,
        )
        .expect("current turn should remain interactive")
    );

    for (method, request) in [
        (
            "item/tool/requestUserInput",
            json!({ "id": "input", "params": { "questions": [] } }),
        ),
        (
            "mcpServer/elicitation/request",
            json!({ "id": "elicitation", "params": {} }),
        ),
        ("item/tool/call", json!({ "id": "generic", "params": {} })),
    ] {
        assert!(
            !try_auto_respond_codex_approval_request(
                method,
                &request,
                &state,
                &session_id,
                &input_tx,
            )
            .expect("non-approval request should remain interactive")
        );
    }
    assert!(input_rx.try_recv().is_err());
}
