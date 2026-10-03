// Owns the tests of the SessionStart compact hook: the control-channel
// callback Claude Code sends while it compacts, which the host answers with
// the session's Engram work context so the first continuation after the
// compaction carries it. Frame shapes follow a live Claude Code 2.1.288
// capture: the callback arrives during status "compacting", before the
// compact_boundary frame, and compaction waits for its answer.
//
// Does not own: the next-prompt context nudge itself (engram_compaction.rs),
// other control requests (claude.rs), or the replay rules of the callback and
// of SessionStart hook frames (claude_frame_router.rs). New file.

use super::engram_host_adapter::real_engram_control_fixture_path;
use super::phase_sync::{DEADLOCK_GUARD, WorkContextGate};
use super::*;

const RUNTIME_ID: &str = "compact-hook-runtime";

/// One Claude runtime's stdout reader and stdin writer on a real session.
struct HookReader {
    state: AppState,
    session_id: String,
    context: ClaudeRuntimeContext,
    frames: ClaudeReaderFrames,
    recorder: SessionRecorder,
    /// What the runtime's stdin would receive.
    commands: mpsc::Receiver<ClaudeRuntimeCommand>,
}

impl HookReader {
    fn on_session(state: &AppState, session_id: &str) -> Self {
        Self::with_answer_budget(state, session_id, None)
    }

    /// A reader whose hook answers within `answer_budget` instead of the
    /// production budget.
    fn with_answer_budget(
        state: &AppState,
        session_id: &str,
        answer_budget: Option<Duration>,
    ) -> Self {
        let (runtime, commands) = test_claude_runtime_handle(RUNTIME_ID);
        let token = RuntimeToken::Claude(runtime.runtime_id.clone());
        // Decided before initialize, as the spawn decides it.
        let compact_hook = compact_hook_for(state, session_id).map(|mut hook| {
            if let Some(answer_budget) = answer_budget {
                hook.answer_budget = answer_budget;
            }
            Arc::new(hook)
        });
        let input_tx = runtime.input_tx.clone();
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .expect("the session exists");
            let record = &mut inner.sessions[index];
            record.runtime = SessionRuntime::Claude(runtime);
            record.session.status = SessionStatus::Active;
            record.active_turn_generation = 1;
        }
        let context = ClaudeRuntimeContext::new(
            state.clone(),
            session_id.to_owned(),
            token,
            new_claude_turn_ownership(),
            Arc::new(Mutex::new(None)),
            "/tmp".to_owned(),
        )
        .with_compact_hook(compact_hook);
        let frames = ClaudeReaderFrames::new(&context, input_tx, None);
        let recorder = SessionRecorder::new(state.clone(), session_id.to_owned());
        Self {
            state: state.clone(),
            session_id: session_id.to_owned(),
            context,
            frames,
            recorder,
            commands,
        }
    }

    fn feed(&mut self, frame: Value) -> ClaudeFrameApplied {
        apply_claude_frame(&self.context, &mut self.frames, &mut self.recorder, &frame).next
    }

    /// The next command the runtime's writer receives, not yet written. The
    /// answer may come from a worker; the guard bounds a broken fixture, not
    /// the behaviour.
    fn next_command(&mut self) -> Option<ClaudeRuntimeCommand> {
        self.commands.recv_timeout(DEADLOCK_GUARD).ok()
    }

    /// Writes `command` as the writer thread writes it; returns what reached
    /// stdin.
    fn write(&mut self, command: ClaudeRuntimeCommand) -> Vec<Value> {
        let mut wire = Vec::new();
        apply_claude_writer_command(&self.context, &mut wire, command);
        String::from_utf8(wire)
            .expect("UTF-8")
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("NDJSON"))
            .collect()
    }

    /// The next command the runtime's stdin receives, written as the writer
    /// thread writes it.
    fn next_written(&mut self) -> Option<Vec<Value>> {
        let command = self.next_command()?;
        Some(self.write(command))
    }

    /// The control response the runtime's stdin receives for `request_id`.
    fn answer_to(&mut self, request_id: &str) -> Option<Value> {
        loop {
            let written = self.next_written()?;
            if let Some(answer) = written.into_iter().find(|frame| {
                frame["type"] == "control_response"
                    && frame.pointer("/response/request_id") == Some(&json!(request_id))
            }) {
                return Some(answer);
            }
        }
    }

    /// Every command queued so far, written; none is waited for.
    fn drain_written(&mut self) -> Vec<Value> {
        let mut written = Vec::new();
        while let Ok(command) = self.commands.try_recv() {
            let mut wire = Vec::new();
            apply_claude_writer_command(&self.context, &mut wire, command);
            written.extend(
                String::from_utf8(wire)
                    .expect("UTF-8")
                    .lines()
                    .map(|line| serde_json::from_str::<Value>(line).expect("NDJSON")),
            );
        }
        written
    }

    fn engram<T>(&self, read: impl FnOnce(&EngramSessionState) -> T) -> T {
        let inner = self.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&self.session_id)
            .expect("the session exists");
        read(&inner.sessions[index].engram)
    }

    /// Waits until the context read the callback started has finished.
    fn wait_for_context_read(&self) {
        let guard = phase_sync::PollGuard::new();
        while self.engram(|engram| engram.context_nudge_in_progress) {
            guard.wait("the compact hook's context read finishes");
        }
    }

    fn callback_id(&self) -> String {
        self.context
            .compact_hook
            .as_ref()
            .expect("this runtime registered the compact hook")
            .callback_id
            .clone()
    }
}

/// The hook callback a live Claude Code 2.1.288 sent during /compact, with
/// its callback id, event and source set to the ones under test.
fn hook_callback(request_id: &str, callback_id: &str, event: &str, source: &str) -> Value {
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {
            "subtype": "hook_callback",
            "callback_id": callback_id,
            "input": {
                "session_id": "b63a8bdc-b7e4-4585-a5d9-3e493b2acaa2",
                "transcript_path": "C:\\Users\\fixture\\.claude\\projects\\fixture\\session.jsonl",
                "cwd": "C:\\fixture",
                "prompt_id": "d57820e6-52e1-49bc-9724-af495cacb343",
                "hook_event_name": event,
                "source": source,
                "model": "claude-haiku-4-5-20251001"
            },
            "tool_use_id": "76c80a5c-b7cf-4ef2-9bfa-a97abb994b09"
        }
    })
}

fn compact_hook_callback(request_id: &str, callback_id: &str) -> Value {
    hook_callback(request_id, callback_id, "SessionStart", "compact")
}

fn compacting_status() -> Value {
    json!({"type": "system", "subtype": "status", "status": "compacting"})
}

fn compact_boundary() -> Value {
    json!({"type": "system", "subtype": "compact_boundary",
        "compact_metadata": {"trigger": "manual"}})
}

/// The initialize request this session's runtime writes.
/// The hook this session's runtime registers, decided as a spawn caller
/// decides it: from the Engram MCP configuration resolved under the state
/// lock, by the same function the spawn calls.
fn compact_hook_for(state: &AppState, session_id: &str) -> Option<ClaudeCompactHook> {
    let engram_mcp = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        engram_mcp_runtime_config_for_session_locked(&inner, session_id)
    };
    claude_compact_hook_for_runtime(engram_mcp.as_ref().map(|config| &config.stdio), RUNTIME_ID)
}

fn initialize_request(state: &AppState, session_id: &str) -> Value {
    let hook = compact_hook_for(state, session_id);
    let mut initialize = Vec::new();
    write_claude_initialize(&mut initialize, state, session_id, hook.as_ref())
        .expect("initialize serializes");
    serde_json::from_slice(initialize.trim_ascii_end()).expect("initialize is JSON")
}

/// The SessionStart compact callback id the host registered in this
/// session's initialize request, if any.
fn registered_compact_callback_id(state: &AppState, session_id: &str) -> Option<String> {
    initialize_request(state, session_id)
        .pointer("/request/hooks/SessionStart")
        .and_then(Value::as_array)
        .and_then(|matchers| {
            matchers
                .iter()
                .find(|matcher| matcher["matcher"] == "compact")
        })
        .and_then(|matcher| matcher.pointer("/hookCallbackIds/0"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// A Claude session in a project whose Engram fixture answers `engram work
/// next` in `mode`, and the project root.
fn claude_session_in(mode: &str, engram_enabled: bool) -> (AppState, String, PathBuf) {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root")
        .path()
        .join("compact-hook");
    fs::create_dir_all(&root).expect("create project");
    fs::write(root.join(".engram-project"), format!("{mode}\n")).expect("declaration");
    let project_id = create_test_project(&state, &root, "Compact hook");
    if engram_enabled {
        set_project_engram(&state, &project_id, &root, true);
    }
    let session_id = create_test_project_session(&state, Agent::Claude, &project_id, &root);
    (state, session_id, root)
}

fn set_project_engram(state: &AppState, project_id: &str, root: &FsPath, enabled: bool) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let project = inner
        .projects
        .iter_mut()
        .find(|project| project.id == project_id)
        .expect("project");
    project.engram = Some(EngramProjectSettings {
        acceptance_evaluation: None,
        enabled,
        turn_gated_control: false,
        binary_path: Some(
            real_engram_control_fixture_path()
                .to_string_lossy()
                .into_owned(),
        ),
        home: Some(root.to_string_lossy().into_owned()),
        work_authority_grant: None,
        authority_store_key: None,
        deadline_ms: Some(250),
    });
    // The repository declares the project, as the declaration refresh a
    // session runs before its runtime starts records it.
    inner
        .engram_declared_project_ids
        .insert(project_id.to_owned());
    inner
        .engram_declaration_checked_project_ids
        .insert(project_id.to_owned());
}

fn project_of(state: &AppState, session_id: &str) -> String {
    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(session_id).expect("session");
    inner.sessions[index]
        .session
        .project_id
        .clone()
        .expect("a project session")
}

fn engram_claude_session() -> (AppState, String) {
    let (state, session_id, _) = claude_session_in("fixture-ready", true);
    (state, session_id)
}

fn work_context_reads(root: &FsPath) -> usize {
    fs::read_to_string(root.join("work-context-reads"))
        .map(|reads| reads.lines().count())
        .unwrap_or(0)
}

/// The additional context of a hook answer, if it carries one.
fn additional_context(answer: &Value) -> Option<&str> {
    answer
        .pointer("/response/response/hookSpecificOutput/additionalContext")
        .and_then(Value::as_str)
}

fn assert_empty_hook_answer(answer: &Value) {
    assert_eq!(answer.pointer("/response/subtype"), Some(&json!("success")));
    assert_eq!(answer.pointer("/response/response"), Some(&json!({})));
}

// Criterion 1's witnesses: they failed on the unchanged code.

#[test]
fn a_session_start_compact_hook_callback_gets_an_answer() {
    // Compaction waits for the callback's answer. A callback the host leaves
    // unanswered holds the compaction until Claude Code's own timeout,
    // whichever callback id it names.
    let (state, session_id) = engram_claude_session();
    let mut reader = HookReader::on_session(&state, &session_id);
    reader.feed(compact_hook_callback(
        "compact-hook-request-1",
        "a-session-start-compact-callback",
    ));
    assert!(
        reader.answer_to("compact-hook-request-1").is_some(),
        "the compact hook callback must be answered"
    );
}

#[test]
fn the_compact_hook_answer_carries_the_engram_work_context() {
    // The answer is what the first continuation after the compaction sees:
    // Claude Code attaches its additionalContext to that continuation.
    let (state, session_id) = engram_claude_session();
    let callback_id = registered_compact_callback_id(&state, &session_id)
        .expect("an Engram session's initialize registers a SessionStart compact hook");
    let mut reader = HookReader::on_session(&state, &session_id);
    reader.feed(compact_hook_callback(
        "compact-hook-request-2",
        &callback_id,
    ));
    let answer = reader
        .answer_to("compact-hook-request-2")
        .expect("the compact hook callback must be answered");
    let context = additional_context(&answer).expect("the answer carries additionalContext");
    assert!(
        context.contains("engram-work-context"),
        "the first continuation carries the Engram work context: {context}"
    );
    assert_eq!(
        answer.pointer("/response/response/hookSpecificOutput/hookEventName"),
        Some(&json!("SessionStart"))
    );
}

// Criterion 3: the initialize snapshot.

#[test]
fn the_spawn_paths_hook_decision_takes_no_state_lock() {
    // Both callers spawn a Claude runtime while they hold the state lock, so
    // the hook decision made inside the spawn must not take it. Run that
    // decision, exactly as the spawn makes it, on another thread while this
    // thread holds the lock: a decision that took the lock would block
    // there, and the test fails at the guard instead of hanging.
    let (state, session_id) = engram_claude_session();
    let guard = state.inner.lock().expect("state mutex poisoned");
    let engram_mcp = engram_mcp_runtime_config_for_session_locked(&guard, &session_id)
        .expect("the session is Engram-configured");
    let (decided_tx, decided_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let hook = claude_compact_hook_for_runtime(Some(&engram_mcp.stdio), RUNTIME_ID);
        let _ = decided_tx.send(hook.map(|hook| hook.callback_id));
    });
    let decided = decided_rx.recv_timeout(DEADLOCK_GUARD);
    drop(guard);
    assert_eq!(
        decided.expect("the spawn's hook decision must not wait for the state lock"),
        Some(format!("termal-session-start-compact-{RUNTIME_ID}"))
    );
}

#[test]
fn an_engram_session_registers_exactly_one_session_start_compact_hook() {
    let (state, session_id) = engram_claude_session();
    let hooks = initialize_request(&state, &session_id)["request"]["hooks"].clone();
    assert_eq!(
        hooks,
        json!({"SessionStart": [{
            "matcher": "compact",
            "hookCallbackIds": [format!("termal-session-start-compact-{RUNTIME_ID}")],
            "timeout": CLAUDE_COMPACT_HOOK_REGISTERED_TIMEOUT_SECS,
        }]})
    );
    assert!(
        Duration::from_secs(CLAUDE_COMPACT_HOOK_REGISTERED_TIMEOUT_SECS)
            > CLAUDE_COMPACT_HOOK_ANSWER_BUDGET,
        "the host answers before Claude Code would cancel"
    );
}

#[test]
fn a_session_without_engram_registers_no_hook() {
    let state = test_app_state();
    let plain = test_session_id(&state, Agent::Claude);
    assert_eq!(
        initialize_request(&state, &plain)["request"]["hooks"],
        json!({})
    );
    let (state, disabled, _) = claude_session_in("fixture-ready", false);
    assert_eq!(
        initialize_request(&state, &disabled)["request"]["hooks"],
        json!({})
    );
}

#[test]
fn engram_enabled_after_initialize_keeps_the_next_prompt_fallback() {
    // The registration is the runtime's initialize snapshot: enabling Engram
    // later adds no hook to it, and the next prompt carries the context.
    let (state, session_id, root) = claude_session_in("fixture-ready", false);
    let mut reader = HookReader::on_session(&state, &session_id);
    assert!(reader.context.compact_hook.is_none());
    set_project_engram(&state, &project_of(&state, &session_id), &root, true);
    reader.feed(compacting_status());
    reader.feed(compact_hook_callback(
        "late-enable",
        &format!("termal-session-start-compact-{RUNTIME_ID}"),
    ));
    let answer = reader.answer_to("late-enable").expect("answered");
    assert_empty_hook_answer(&answer);
    reader.feed(compact_boundary());
    assert!(
        reader.engram(|engram| engram.context_nudge_pending && engram.context_refresh_needed),
        "the boundary asks the next prompt to carry the context"
    );
}

// Criterion 4: the responder.

#[test]
fn a_delivered_answer_leaves_no_second_block_for_the_next_prompt() {
    let (state, session_id) = engram_claude_session();
    let mut reader = HookReader::on_session(&state, &session_id);
    let callback_id = reader.callback_id();
    reader.feed(compacting_status());
    reader.feed(compact_hook_callback("delivered", &callback_id));
    let answer = reader.answer_to("delivered").expect("answered");
    assert!(additional_context(&answer).is_some());
    // Claude Code echoes the answer, then ends the compaction with its
    // boundary: neither asks for the context again.
    assert_eq!(
        reader.feed(json!({"type": "control_response", "response": {
            "subtype": "success", "request_id": "delivered", "response": {}}})),
        ClaudeFrameApplied::Continue
    );
    reader.feed(compact_boundary());
    assert!(
        reader.engram(|engram| engram.pending_context_nudge.is_none()
            && !engram.context_nudge_pending
            && !engram.context_refresh_needed)
    );
    assert!(reader.drain_written().is_empty(), "answered once");
}

#[test]
fn a_page_fetched_before_the_compaction_is_never_its_answer() {
    let (state, session_id, root) = claude_session_in("fixture-ready", true);
    let mut reader = HookReader::on_session(&state, &session_id);
    assert_eq!(
        state.prepare_engram_context_nudge_off_lock(&session_id),
        EngramContextNudgePreparation::Ready
    );
    let earlier = reader.engram(|engram| engram.context_nudge_generation);
    assert!(reader.engram(|engram| engram.pending_context_nudge.is_some()));
    assert_eq!(work_context_reads(&root), 1);
    let callback_id = reader.callback_id();
    reader.feed(compact_hook_callback("fresh", &callback_id));
    reader.answer_to("fresh").expect("answered");
    assert_eq!(work_context_reads(&root), 2, "the compaction reads afresh");
    assert!(reader.engram(|engram| engram.context_nudge_generation) > earlier);
}

#[test]
fn a_duplicate_callback_is_answered_once_and_the_reader_stays_live() {
    let (state, session_id, root) = claude_session_in("fixture-work-next-gated", true);
    let mut gate = WorkContextGate::new(&root);
    let mut reader = HookReader::on_session(&state, &session_id);
    let callback_id = reader.callback_id();
    reader.feed(compact_hook_callback("held", &callback_id));
    gate.wait();
    // While the context is read, the reader keeps reading and the writer
    // keeps writing: another callback is answered at once.
    reader.feed(compact_hook_callback("held", &callback_id));
    reader.feed(hook_callback(
        "other",
        "someone-else",
        "SessionStart",
        "compact",
    ));
    assert_empty_hook_answer(&reader.answer_to("other").expect("answered at once"));
    gate.release();
    let answer = reader.answer_to("held").expect("answered");
    assert!(additional_context(&answer).is_some());
    reader.wait_for_context_read();
    assert!(
        reader.drain_written().is_empty(),
        "the duplicate is not answered"
    );
}

// Criterion 5: delivery, fallback and fences.

#[test]
fn a_failed_context_read_answers_empty_and_keeps_the_next_prompt_fallback() {
    let (state, session_id, root) = claude_session_in("fixture-ready", true);
    {
        let project_id = project_of(&state, &session_id);
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let project = inner
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
            .expect("project");
        project.engram.as_mut().expect("settings").binary_path =
            Some(root.join("missing-engram").to_string_lossy().into_owned());
    }
    let mut reader = HookReader::on_session(&state, &session_id);
    let callback_id = reader.callback_id();
    reader.feed(compact_hook_callback("failing", &callback_id));
    assert_empty_hook_answer(&reader.answer_to("failing").expect("answered"));
    assert!(reader.engram(|engram| engram.context_nudge_pending));
}

#[test]
fn a_cancelled_callback_gets_no_answer_and_keeps_its_page_for_the_next_prompt() {
    let (state, session_id, root) = claude_session_in("fixture-work-next-gated", true);
    let mut gate = WorkContextGate::new(&root);
    let mut reader = HookReader::on_session(&state, &session_id);
    let callback_id = reader.callback_id();
    reader.feed(compact_hook_callback("abandoned", &callback_id));
    gate.wait();
    reader.feed(json!({"type": "control_cancel_request", "request_id": "abandoned"}));
    gate.release();
    reader.wait_for_context_read();
    // A probe answered after the read: nothing for the abandoned request
    // comes before it, and nothing can come after, since its single answer
    // was taken by the cancellation.
    reader.feed(hook_callback(
        "probe",
        "someone-else",
        "SessionStart",
        "startup",
    ));
    let written = std::iter::from_fn(|| reader.next_written())
        .flatten()
        .take_while(|frame| frame.pointer("/response/request_id") != Some(&json!("probe")))
        .collect::<Vec<_>>();
    assert!(written.is_empty(), "{written:?}");
    assert!(
        reader.engram(|engram| engram.pending_context_nudge.is_some()),
        "the undelivered page stays for the next prompt"
    );
}

#[test]
fn a_replaced_runtime_gets_no_late_answer_and_nothing_is_delivered() {
    let (state, session_id, root) = claude_session_in("fixture-work-next-gated", true);
    let mut gate = WorkContextGate::new(&root);
    let mut reader = HookReader::on_session(&state, &session_id);
    let callback_id = reader.callback_id();
    reader.feed(compact_hook_callback("stale", &callback_id));
    gate.wait();
    let (replacement, _replacement_commands) = test_claude_runtime_handle("replacement-runtime");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).expect("session");
        inner.sessions[index].runtime = SessionRuntime::Claude(replacement);
    }
    gate.release();
    reader.wait_for_context_read();
    reader.feed(hook_callback(
        "probe",
        "someone-else",
        "SessionStart",
        "startup",
    ));
    let written = std::iter::from_fn(|| reader.next_written())
        .flatten()
        .take_while(|frame| frame.pointer("/response/request_id") != Some(&json!("probe")))
        .collect::<Vec<_>>();
    assert!(written.is_empty(), "{written:?}");
    assert!(reader.engram(|engram| engram.pending_context_nudge.is_some()));
}

#[test]
fn a_context_read_past_its_budget_answers_empty_and_keeps_its_page_for_the_next_prompt() {
    // The budget runs out while the read is held at its gate, so the empty
    // answer is certain without waiting out any real time: a zero budget
    // and a read that cannot have finished.
    let (state, session_id, root) = claude_session_in("fixture-work-next-gated", true);
    let mut gate = WorkContextGate::new(&root);
    let mut reader = HookReader::with_answer_budget(&state, &session_id, Some(Duration::ZERO));
    let callback_id = reader.callback_id();
    reader.feed(compact_hook_callback("late", &callback_id));
    gate.wait();
    assert_empty_hook_answer(
        &reader
            .answer_to("late")
            .expect("answered within its budget"),
    );
    gate.release();
    reader.wait_for_context_read();
    assert!(
        reader.engram(|engram| engram.pending_context_nudge.is_some()),
        "the read finished after the answer: its page waits for the next prompt"
    );
    reader.feed(hook_callback(
        "probe",
        "someone-else",
        "SessionStart",
        "startup",
    ));
    let written = std::iter::from_fn(|| reader.next_written())
        .flatten()
        .take_while(|frame| frame.pointer("/response/request_id") != Some(&json!("probe")))
        .collect::<Vec<_>>();
    assert!(written.is_empty(), "answered once: {written:?}");
}

#[test]
fn disabling_engram_mid_callback_answers_empty() {
    let (state, session_id, root) = claude_session_in("fixture-work-next-gated", true);
    let mut gate = WorkContextGate::new(&root);
    let mut reader = HookReader::on_session(&state, &session_id);
    let callback_id = reader.callback_id();
    reader.feed(compact_hook_callback("disabled", &callback_id));
    gate.wait();
    set_project_engram(&state, &project_of(&state, &session_id), &root, false);
    gate.release();
    assert_empty_hook_answer(&reader.answer_to("disabled").expect("answered"));
    assert!(reader.engram(|engram| engram.pending_context_nudge.is_none()));
}

#[test]
fn other_callbacks_get_an_empty_hook_answer_never_a_permission_payload() {
    let (state, session_id) = engram_claude_session();
    let mut reader = HookReader::on_session(&state, &session_id);
    let callback_id = reader.callback_id();
    for (request_id, callback, event, source) in [
        ("unregistered", "someone-else", "SessionStart", "compact"),
        ("startup", callback_id.as_str(), "SessionStart", "startup"),
        ("other-event", callback_id.as_str(), "PreCompact", "compact"),
    ] {
        reader.feed(hook_callback(request_id, callback, event, source));
        let answer = reader.answer_to(request_id).expect("answered");
        assert_empty_hook_answer(&answer);
        assert!(answer.pointer("/response/response/behavior").is_none());
    }
    assert!(
        reader.engram(|engram| !engram.context_refresh_needed),
        "none of them is the compaction's"
    );
}

#[test]
fn a_malformed_callback_gets_an_error_answer() {
    let (state, session_id) = engram_claude_session();
    let mut reader = HookReader::on_session(&state, &session_id);
    reader.feed(json!({"type": "control_request", "request_id": "malformed",
        "request": {"subtype": "hook_callback"}}));
    let answer = reader.answer_to("malformed").expect("answered");
    assert_eq!(answer.pointer("/response/subtype"), Some(&json!("error")));
}

#[test]
fn a_callback_request_has_one_lifecycle() {
    let hook = ClaudeCompactHook::new("owner-runtime");
    assert!(hook.begin("written"));
    assert!(
        !hook.begin("written"),
        "a duplicate does not start a second"
    );
    assert!(hook.queue("written"));
    assert_eq!(hook.take_for_write("written"), Some(true));
    assert_eq!(hook.take_for_write("written"), None, "it is taken once");
    assert!(hook.finish_write("written"));
    assert!(!hook.finish_write("written"), "it is finished once");
    assert!(!hook.begin("written"), "a settled request is not reopened");

    assert!(hook.begin("cancelled-while-read"));
    hook.settle_unanswered("cancelled-while-read");
    assert!(!hook.queue("cancelled-while-read"));
    assert!(!hook.begin("cancelled-while-read"));

    assert!(hook.begin("cancelled-while-queued"));
    assert!(hook.queue("cancelled-while-queued"));
    hook.settle_unanswered("cancelled-while-queued");
    assert_eq!(hook.take_for_write("cancelled-while-queued"), None);

    assert!(hook.begin("expired-while-queued"));
    assert!(hook.queue("expired-while-queued"));
    hook.expire("expired-while-queued");
    assert_eq!(
        hook.take_for_write("expired-while-queued"),
        Some(false),
        "taken, but late"
    );
    assert!(hook.finish_write("expired-while-queued"));

    for revoke in ["cancelled-while-written", "expired-while-written"] {
        assert!(hook.begin(revoke));
        assert!(hook.queue(revoke));
        assert_eq!(hook.take_for_write(revoke), Some(true));
        if revoke.starts_with("cancelled") {
            hook.settle_unanswered(revoke);
        } else {
            hook.expire(revoke);
        }
        assert!(
            !hook.finish_write(revoke),
            "{revoke}: its write delivers nothing"
        );
        assert!(!hook.begin(revoke));
    }

    // Old settled ids give way, bounded.
    for index in 0..CLAUDE_COMPACT_HOOK_SETTLED_LIMIT {
        let id = format!("settled-{index}");
        assert!(hook.begin(&id));
        hook.settle_unanswered(&id);
    }
    assert!(hook.requests().states.len() <= CLAUDE_COMPACT_HOOK_SETTLED_LIMIT);
}

/// The answer the worker queued for `request_id`, taken from the writer's
/// queue without writing it.
fn queued_answer(reader: &mut HookReader, request_id: &str) -> ClaudeRuntimeCommand {
    let command = reader.next_command().expect("an answer is queued");
    assert!(matches!(
        &command,
        ClaudeRuntimeCommand::HookResponse(response) if response.request_id == request_id
    ));
    command
}

#[test]
fn a_cancel_after_the_answer_is_queued_writes_nothing_and_keeps_the_page() {
    let (state, session_id) = engram_claude_session();
    let mut reader = HookReader::on_session(&state, &session_id);
    let callback_id = reader.callback_id();
    reader.feed(compact_hook_callback("queued-then-cancelled", &callback_id));
    let queued = queued_answer(&mut reader, "queued-then-cancelled");
    reader.feed(json!({"type": "control_cancel_request", "request_id": "queued-then-cancelled"}));
    assert!(
        reader.write(queued).is_empty(),
        "a cancelled answer is not written"
    );
    assert!(
        reader.engram(|engram| engram.pending_context_nudge.is_some()),
        "nothing was delivered: the next prompt carries the page"
    );
}

#[test]
fn context_invalidated_after_it_is_queued_is_written_as_an_empty_answer() {
    let (state, session_id) = engram_claude_session();
    let mut reader = HookReader::on_session(&state, &session_id);
    let callback_id = reader.callback_id();
    reader.feed(compact_hook_callback("queued-then-stale", &callback_id));
    let queued = queued_answer(&mut reader, "queued-then-stale");
    {
        // A settings change invalidates the page and moves the generation on.
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).expect("session");
        inner.sessions[index].engram.invalidate_context_nudge();
    }
    let written = reader.write(queued);
    assert_eq!(written.len(), 1);
    assert_empty_hook_answer(&written[0]);
    assert!(
        reader.engram(|engram| engram.context_nudge_pending),
        "the next prompt still reads and carries a context"
    );
}

#[test]
fn an_invalidation_during_the_read_answers_empty_and_consumes_no_newer_page() {
    // The generation moves on while the hook's read is held: the read is
    // superseded, and the hook neither reads again nor answers with what a
    // newer read would fetch; the newer generation stays the next prompt's.
    let (state, session_id, root) = claude_session_in("fixture-work-next-gated", true);
    let mut gate = WorkContextGate::new(&root);
    let mut reader = HookReader::on_session(&state, &session_id);
    let callback_id = reader.callback_id();
    reader.feed(compact_hook_callback("superseded", &callback_id));
    gate.wait();
    let invalidated = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).expect("session");
        let engram = &mut inner.sessions[index].engram;
        engram.invalidate_context_nudge();
        engram.context_nudge_generation
    };
    gate.release();
    assert_empty_hook_answer(&reader.answer_to("superseded").expect("answered"));
    reader.wait_for_context_read();
    assert_eq!(work_context_reads(&root), 1, "the hook does not read again");
    assert!(
        reader.engram(|engram| engram.context_nudge_generation == invalidated
            && engram.pending_context_nudge.is_none()
            && engram.context_nudge_pending)
    );
}

#[test]
fn a_settings_change_during_the_read_answers_empty_and_keeps_the_page() {
    // A settings change that leaves the generation as it is still changes the
    // identity the callback met: its page is not this answer, and it waits,
    // undelivered, for the next prompt.
    let (state, session_id, root) = claude_session_in("fixture-work-next-gated", true);
    let mut gate = WorkContextGate::new(&root);
    let mut reader = HookReader::on_session(&state, &session_id);
    let callback_id = reader.callback_id();
    reader.feed(compact_hook_callback("resettled", &callback_id));
    gate.wait();
    {
        let project_id = project_of(&state, &session_id);
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let project = inner
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
            .expect("project");
        project.engram.as_mut().expect("settings").deadline_ms = Some(500);
    }
    gate.release();
    assert_empty_hook_answer(&reader.answer_to("resettled").expect("answered"));
    reader.wait_for_context_read();
    assert!(
        reader.engram(|engram| engram.pending_context_nudge.is_some()),
        "the page is not delivered"
    );
}

#[test]
fn context_queued_past_its_deadline_is_written_as_an_empty_answer() {
    // The answer budget runs from the callback to the write: context that
    // waited in the writer's queue past it is not written and not delivered.
    let (state, session_id) = engram_claude_session();
    let mut reader = HookReader::on_session(&state, &session_id);
    let callback_id = reader.callback_id();
    reader.feed(compact_hook_callback("queued-too-long", &callback_id));
    let mut queued = queued_answer(&mut reader, "queued-too-long");
    let ClaudeRuntimeCommand::HookResponse(response) = &mut queued else {
        unreachable!("queued_answer checked the command");
    };
    assert!(matches!(response.answer, ClaudeHookAnswer::Context { .. }));
    // A deadline at this instant has passed by the write.
    response.deadline = Some(std::time::Instant::now());
    let written = reader.write(queued);
    assert_eq!(written.len(), 1);
    assert_empty_hook_answer(&written[0]);
    assert!(
        reader.engram(|engram| engram.pending_context_nudge.is_some()),
        "nothing was delivered: the next prompt carries the page"
    );
}

/// Runtime stdin that runs `interrupt` while the writer's first write is under
/// way, as a cancel or the deadline could land while a blocked write waits.
struct InterruptedStdin<F: FnMut()> {
    interrupt: Option<F>,
    bytes: Vec<u8>,
}

impl<F: FnMut()> Write for InterruptedStdin<F> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(mut interrupt) = self.interrupt.take() {
            interrupt();
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Writes `command` as the writer thread writes it, with `interrupt` landing
/// during the write; returns what reached stdin.
fn write_interrupted(
    reader: &HookReader,
    command: ClaudeRuntimeCommand,
    interrupt: impl FnMut(),
) -> Vec<Value> {
    let mut stdin = InterruptedStdin {
        interrupt: Some(interrupt),
        bytes: Vec::new(),
    };
    assert!(apply_claude_writer_command(
        &reader.context,
        &mut stdin,
        command
    ));
    String::from_utf8(stdin.bytes)
        .expect("UTF-8")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("NDJSON"))
        .collect()
}

#[test]
fn a_cancel_or_the_deadline_during_the_write_delivers_nothing() {
    // The write was already under way, so its context reaches stdin, but
    // Claude Code abandoned the request or the budget ended before the write
    // did: the page is not delivered, and the next prompt carries it.
    for (request_id, cancel) in [("cancelled-in-write", true), ("expired-in-write", false)] {
        let (state, session_id) = engram_claude_session();
        let mut reader = HookReader::on_session(&state, &session_id);
        let callback_id = reader.callback_id();
        reader.feed(compact_hook_callback(request_id, &callback_id));
        let queued = queued_answer(&mut reader, request_id);
        let context = reader.context.clone();
        let written = write_interrupted(&reader, queued, || {
            if cancel {
                cancel_claude_hook_callback(&context, request_id);
            } else {
                context
                    .compact_hook
                    .as_ref()
                    .expect("registered")
                    .expire(request_id);
            }
        });
        assert_eq!(written.len(), 1);
        assert!(additional_context(&written[0]).is_some(), "{request_id}");
        assert!(
            reader.engram(|engram| engram.pending_context_nudge.is_some()),
            "{request_id}: nothing was delivered, the next prompt carries the page"
        );
    }
}

#[test]
fn a_compaction_after_one_that_ended_without_its_boundary_still_asks_for_context() {
    // The first compaction's callback is answered, but its boundary never
    // comes and no compacting status follows: the next compaction's callback
    // asks for a fresh context itself and is answered with it.
    let (state, session_id) = engram_claude_session();
    let mut reader = HookReader::on_session(&state, &session_id);
    let callback_id = reader.callback_id();
    reader.feed(compacting_status());
    reader.feed(compact_hook_callback("abandoned-compaction", &callback_id));
    let first = reader.answer_to("abandoned-compaction").expect("answered");
    assert!(additional_context(&first).is_some());
    reader.feed(compact_hook_callback("next-compaction", &callback_id));
    let next = reader.answer_to("next-compaction").expect("answered");
    assert!(
        additional_context(&next).is_some(),
        "the next compaction's continuation carries the context: {next}"
    );
}

#[test]
fn a_duplicate_after_the_answer_or_a_cancellation_is_not_answered_again() {
    let (state, session_id, root) = claude_session_in("fixture-work-next-gated", true);
    let mut gate = WorkContextGate::new(&root);
    let mut reader = HookReader::on_session(&state, &session_id);
    let callback_id = reader.callback_id();
    // Cancelled while its context is read.
    reader.feed(compact_hook_callback("cancelled", &callback_id));
    gate.wait();
    reader.feed(json!({"type": "control_cancel_request", "request_id": "cancelled"}));
    gate.release();
    reader.wait_for_context_read();
    // Answered and written.
    reader.feed(compact_hook_callback("answered", &callback_id));
    reader.answer_to("answered").expect("answered");
    // Both repeated: neither starts a read or an answer.
    reader.feed(compact_hook_callback("cancelled", &callback_id));
    reader.feed(compact_hook_callback("answered", &callback_id));
    reader.feed(hook_callback(
        "probe",
        "someone-else",
        "SessionStart",
        "startup",
    ));
    let written = std::iter::from_fn(|| reader.next_written())
        .flatten()
        .take_while(|frame| frame.pointer("/response/request_id") != Some(&json!("probe")))
        .collect::<Vec<_>>();
    assert!(written.is_empty(), "{written:?}");
}
