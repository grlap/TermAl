// Owns the tests of the durable delivery receipt of a receipted prompt: the
// write-ahead hand-off record acknowledged before the runtime is called, the
// runtimes' acknowledgements and failures (Claude, Codex), the return to
// pending after a failed write-ahead, the restart semantics before and after
// the write-ahead, after acceptance and after rejection, the re-queue of a
// rejected prompt under a new prompt id and prerequisite, and the explicit
// Resume of a paused queue's receipted head (its one-shot continuation, and
// every Stop, pause, Cancel or edit that ends it), and the settlement of a
// head's receipt when a removal, a discarded deferred hand-off or a direct
// hand-off ends its attempt unsent. Does not own the
// durability hold itself (queued_prompt_durability.rs tests). New module
// beside src/prompt_delivery.rs.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// Which kind of fence the writer handed to the test.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum HeldFence {
    Durability,
    WriteAhead,
}

/// A persist writer that writes nothing and acknowledges every fence at
/// once, except a receipted prompt's durability and write-ahead fences: those
/// are handed to the test, which decides how they resolve.
pub(super) struct DeliveryWriter {
    tx: mpsc::Sender<PersistRequest>,
    held: mpsc::Receiver<(HeldFence, Box<PersistFence>)>,
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl DeliveryWriter {
    pub(super) fn attach(state: &mut AppState) -> Self {
        state.shutdown_persist_blocking();
        let (tx, rx) = mpsc::channel::<PersistRequest>();
        state.persist_tx = tx.clone();
        let (held_tx, held) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let writer_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            while let Ok(request) = rx.recv() {
                if writer_stop.load(Ordering::SeqCst) {
                    break;
                }
                let PersistRequest::Fence(fence) = request else {
                    continue;
                };
                let kind = match &fence.target {
                    PersistFenceTarget::QueuedPromptDurability { .. } => HeldFence::Durability,
                    PersistFenceTarget::PromptDeliveryWriteAhead { .. } => HeldFence::WriteAhead,
                    _ => {
                        fence.finish(Ok(()));
                        continue;
                    }
                };
                let _ = held_tx.send((kind, fence));
            }
        });
        Self { tx, held, stop, thread: Mutex::new(Some(thread)) }
    }

    pub(super) fn take(&self, expected: HeldFence) -> Box<PersistFence> {
        let (kind, fence) = phase_sync::receive(&self.held, "the next held fence");
        assert_eq!(kind, expected, "the writer received another fence first");
        fence
    }
}

impl Drop for DeliveryWriter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.tx.send(PersistRequest::Delta);
        if let Some(thread) = self.thread.lock().unwrap().take() {
            let _ = thread.join();
        }
    }
}

/// An idle Claude session with a test runtime, and its receipted prompt.
struct Fixture {
    state: AppState,
    session: String,
    token: RuntimeToken,
    runtime_input: mpsc::Sender<ClaudeRuntimeCommand>,
    prompts: mpsc::Receiver<ClaudeRuntimeCommand>,
    _root: TestTempRoot,
}

impl Fixture {
    fn new(state: AppState, label: &str) -> Self {
        let root = TestTempRoot::create(&format!("prompt-delivery-{label}"));
        let session = {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let record = inner.create_session(
                Agent::Claude,
                Some(format!("Delivery {label}")),
                root.path().to_string_lossy().into_owned(),
                None,
                None,
            );
            let session = record.session.id.clone();
            state.commit_locked(&mut inner).unwrap();
            session
        };
        let (token, runtime_input, prompts) = install_test_claude_runtime(&state, &session, label);
        Self { state, session, token, runtime_input, prompts, _root: root }
    }

    /// Enqueues a receipted prompt and commits it; returns its id.
    fn enqueue_receipted(&self, text: &str) -> String {
        let mut inner = self.state.inner.lock().expect("state mutex poisoned");
        let prompt = PendingPrompt {
            engram_interrupted: false,
            is_engram_retained: false,
            attachments: Vec::new(),
            id: inner.next_message_id(),
            timestamp: stamp_now(),
            text: text.to_owned(),
            expanded_text: None,
            source: None,
        };
        let id = prompt.id.clone();
        let index = inner.find_session_index(&self.session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        queue_receipted_orchestrator_prompt_on_record(record, prompt, Vec::new());
        self.state.commit_locked(&mut inner).unwrap();
        id
    }

    fn delivery(&self, prompt_id: &str) -> Option<PromptDelivery> {
        let inner = self.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&self.session).unwrap();
        inner.sessions[index].prompt_deliveries.get(prompt_id).cloned()
    }

    fn view(&self) -> Vec<PromptDeliveryView> {
        let inner = self.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&self.session).unwrap();
        inner.sessions[index].session.prompt_deliveries.clone()
    }

    fn queue_ids(&self) -> Vec<String> {
        let inner = self.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&self.session).unwrap();
        inner.sessions[index]
            .queued_prompts
            .iter()
            .map(|queued| queued.pending_prompt.id.clone())
            .collect()
    }

    /// Waits until the hold's durability acknowledgement has settled (on its
    /// own thread): only then is the entry eligible for a Resume.
    fn wait_for_hold_acknowledged(&self, prompt_id: &str) {
        let settled = phase_sync::PollGuard::new();
        loop {
            let acknowledged = {
                let inner = self.state.inner.lock().expect("state mutex poisoned");
                let index = inner.find_session_index(&self.session).unwrap();
                inner.sessions[index]
                    .queued_prompt_durability_holds
                    .get(prompt_id)
                    .is_some_and(|hold| hold.acknowledged)
            };
            if acknowledged {
                return;
            }
            settled.wait("the hold's durability acknowledgement");
        }
    }

    fn set_paused(&self, paused: bool) {
        let mut inner = self.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&self.session).unwrap();
        inner.session_mut_by_index(index).unwrap().set_auto_dispatch_blocked(paused);
        self.state.commit_locked(&mut inner).unwrap();
    }

    fn wait_for(&self, prompt_id: &str, what: &str, done: impl Fn(&PromptDelivery) -> bool) {
        let published = phase_sync::PollGuard::new();
        while !self.delivery(prompt_id).as_ref().is_some_and(&done) {
            published.wait(what);
        }
    }

    /// The prompt the runtime received.
    fn received_prompt(&self, what: &str) -> ClaudePromptCommand {
        match recv_within_guard(&self.prompts, what) {
            Ok(ClaudeRuntimeCommand::Prompt(command)) => command,
            Ok(_) => panic!("{what}: got another runtime command"),
            Err(error) => panic!("{what}: {error:?}"),
        }
    }

    fn delivered_prompts(&self) -> usize {
        let mut count = 0;
        while let Ok(command) = self.prompts.try_recv() {
            if matches!(command, ClaudeRuntimeCommand::Prompt(_)) {
                count += 1;
            }
        }
        count
    }

    /// The runtime's reader and writer context, as the spawned runtime has.
    fn claude_context(&self) -> ClaudeRuntimeContext {
        ClaudeRuntimeContext::new(
            self.state.clone(),
            self.session.clone(),
            self.token.clone(),
            new_claude_turn_ownership(),
            Arc::new(Mutex::new(None)),
            "/tmp".to_owned(),
        )
    }
}

/// Gives the session an idle Claude test runtime; returns its token, its
/// input sender and what reaches it.
fn install_test_claude_runtime(
    state: &AppState,
    session: &str,
    label: &str,
) -> (
    RuntimeToken,
    mpsc::Sender<ClaudeRuntimeCommand>,
    mpsc::Receiver<ClaudeRuntimeCommand>,
) {
    let (runtime, prompts) = test_claude_runtime_handle(&format!("delivery-{label}"));
    let token = RuntimeToken::Claude(runtime.runtime_id.clone());
    let input = runtime.input_tx.clone();
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(session).unwrap();
    let record = inner.session_mut_by_index(index).unwrap();
    record.runtime = SessionRuntime::Claude(runtime);
    record.session.status = SessionStatus::Idle;
    (token, input, prompts)
}

/// A receipted prompt goes from pending through its held durability
/// acknowledgement to a write-ahead hand-off record; the runtime is called
/// only once that record is acknowledged durable.
fn hand_off_through_held_fences(fixture: &Fixture, writer: &DeliveryWriter, prompt: &str) -> String {
    let id = fixture.enqueue_receipted(prompt);
    assert_eq!(fixture.delivery(&id).unwrap().disposition, PromptDeliveryDisposition::Pending);
    assert!(fixture.state.request_queued_prompt_durability(&fixture.session, &id));
    writer.take(HeldFence::Durability).finish(Ok(()));
    let write_ahead = writer.take(HeldFence::WriteAhead);
    let delivery = fixture.delivery(&id).unwrap();
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::HandingOff);
    assert_eq!(delivery.attempt, 1);
    assert_eq!(delivery.turn_generation, None, "not promoted before the write-ahead");
    assert_eq!(fixture.delivered_prompts(), 0, "no runtime call before the write-ahead");
    assert_eq!(fixture.queue_ids(), vec![id.clone()]);
    write_ahead.finish(Ok(()));
    id
}

#[test]
fn the_write_ahead_record_is_durable_before_the_runtime_is_called() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "write-ahead");
    let id = hand_off_through_held_fences(&fixture, &writer, "the receipted prompt");
    let command = fixture.received_prompt("the acknowledged write-ahead releases the hand-off");
    assert_eq!(command.text, "the receipted prompt");
    let delivery = fixture.delivery(&id).unwrap();
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::HandingOff);
    assert_eq!(delivery.turn_generation, Some(command.turn_generation));
    assert_eq!(delivery.attempt_key.as_deref(), Some(command.replay_generation.as_str()));
    assert!(delivery.message_id.is_some());
    assert!(fixture.queue_ids().is_empty());
    let view = fixture.view();
    assert_eq!(view.len(), 1, "the receipt is on the wire");
    assert_eq!(view[0].prompt_id, id);
    assert_eq!(view[0].disposition, PromptDeliveryDisposition::HandingOff);
}

/// The ruling on a failed write-ahead: nothing reached the runtime, so the
/// prompt returns to pending by a durable write that supersedes the hand-off
/// record; a late acknowledgement of that write-ahead delivers nothing, and
/// the row stays pending across a restart.
#[test]
fn a_failed_write_ahead_returns_the_prompt_to_pending_and_a_late_acknowledgement_delivers_nothing()
{
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "failed-write-ahead");
    let id = fixture.enqueue_receipted("never sent by a lost write-ahead");
    assert!(fixture.state.request_queued_prompt_durability(&fixture.session, &id));
    writer.take(HeldFence::Durability).finish(Ok(()));
    let write_ahead = writer.take(HeldFence::WriteAhead);
    let old_nonce = fixture.delivery(&id).unwrap().write_ahead;
    write_ahead.finish(Err(PersistFenceError::Deadline));
    fixture.wait_for(&id, "the return to pending", |delivery| {
        delivery.disposition == PromptDeliveryDisposition::Pending
    });
    let pending = fixture.delivery(&id).unwrap();
    assert_ne!(pending.write_ahead, old_nonce, "the restore supersedes the hand-off record");
    assert!(pending.reason.as_deref().is_some_and(|reason| reason.contains("not sent")));

    // A late acknowledgement of the superseded write-ahead.
    fixture
        .state
        .settle_prompt_delivery_write_ahead(&fixture.session, &id, &old_nonce, Ok(()));
    assert_eq!(fixture.delivered_prompts(), 0, "a late acknowledgement delivers nothing");
    assert_eq!(fixture.delivery(&id).unwrap().disposition, PromptDeliveryDisposition::Pending);
    assert_eq!(fixture.queue_ids(), vec![id.clone()]);

    // The row as a restart reads it back.
    let reloaded = {
        let inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        PersistedSessionRecord::from_record(&inner.sessions[index])
            .into_record()
            .expect("the row loads")
    };
    assert_eq!(
        reloaded.prompt_deliveries[&id].disposition,
        PromptDeliveryDisposition::Pending,
        "pending across a restart"
    );
    assert_eq!(reloaded.queued_prompts.front().unwrap().pending_prompt.id, id);

    // The next ordinary dispatch starts a new attempt.
    assert!(fixture.state.dispatch_next_queued_turn(&fixture.session, false).unwrap().is_none());
    let _second = writer.take(HeldFence::WriteAhead);
    assert_eq!(fixture.delivery(&id).unwrap().attempt, 2);
}

#[test]
fn claude_accepts_the_prompt_with_its_command_lifecycle_started() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "claude-started");
    let id = hand_off_through_held_fences(&fixture, &writer, "accepted by Claude");
    let command = fixture.received_prompt("the hand-off");
    let uuid = command.replay_generation.clone();
    let context = fixture.claude_context();
    let mut wire = Vec::new();
    assert!(apply_claude_writer_command(
        &context,
        &mut wire,
        ClaudeRuntimeCommand::Prompt(command)
    ));
    assert_eq!(fixture.delivery(&id).unwrap().disposition, PromptDeliveryDisposition::HandingOff);
    let mut frames = ClaudeReaderFrames::new(&context, fixture.runtime_input.clone(), None);
    let mut recorder = SessionRecorder::new(fixture.state.clone(), fixture.session.clone());
    let _ = apply_claude_frame(
        &context,
        &mut frames,
        &mut recorder,
        &json!({"type": "command_lifecycle", "command_uuid": uuid, "state": "started"}),
    );
    let delivery = fixture.delivery(&id).unwrap();
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Accepted);
    assert!(delivery.entry.is_none());
}

struct BrokenStdin;

impl std::io::Write for BrokenStdin {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "stdin closed"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_claude_stdin_write_error_leaves_the_delivery_ambiguous() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "claude-write-error");
    let id = hand_off_through_held_fences(&fixture, &writer, "half written");
    let command = fixture.received_prompt("the hand-off");
    let context = fixture.claude_context();
    assert!(!apply_claude_writer_command(
        &context,
        &mut BrokenStdin,
        ClaudeRuntimeCommand::Prompt(command)
    ));
    let delivery = fixture.delivery(&id).unwrap();
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Ambiguous);
    assert!(delivery.reason.as_deref().is_some_and(|reason| reason.contains("stdin")));
}

/// A Codex session whose receipted delivery is handing off under
/// `generation`, as promotion leaves it; returns the session, its shared
/// runtime and the prompt id.
fn codex_handing_off(state: &AppState, generation: u64) -> (String, SharedCodexRuntime, String) {
    let session_id = test_session_id(state, Agent::Codex);
    let (runtime, _input_rx, process) = test_shared_codex_runtime("prompt-delivery-codex");
    let prompt_id = "message-codex-receipted".to_owned();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
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
        record.active_turn_generation = generation;
        record.prompt_deliveries.insert(
            prompt_id.clone(),
            PromptDelivery {
                federation_operation: None,
                federation_handoff_started: false,
                disposition: PromptDeliveryDisposition::HandingOff,
                attempt: 1,
                write_ahead: "write-ahead-1".to_owned(),
                sequence: 1,
                message_id: Some(prompt_id.clone()),
                turn_generation: Some(generation),
                attempt_key: Some(format!("{session_id}:{generation}:1")),
                reason: None,
                entry: None,
                write_ahead_acknowledged: true,
                resume: None,
                runtime_write: PromptRuntimeWrite::Untracked,
            },
        );
    }
    runtime
        .sessions
        .lock()
        .unwrap()
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-receipted".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .unwrap()
        .insert("thread-receipted".to_owned(), session_id.clone());
    (session_id, runtime, prompt_id)
}

fn codex_prompt(generation: u64) -> CodexPromptCommand {
    CodexPromptCommand {
        active_turn_generation: generation,
        approval_policy: CodexApprovalPolicy::Never,
        attachments: Vec::new(),
        cwd: "/tmp".to_owned(),
        model: "gpt-5.4".to_owned(),
        prompt: "a receipted Codex prompt".to_owned(),
        reasoning_effort: CodexReasoningEffort::Medium,
        service_tier: None,
        resume_thread_id: None,
        sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        delivery_receipt: None,
    }
}

fn codex_delivery(state: &AppState, session_id: &str, prompt_id: &str) -> PromptDelivery {
    let inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(session_id).unwrap();
    inner.sessions[index].prompt_deliveries[prompt_id].clone()
}

/// Starts the turn on the shared runtime and answers its `turn/start` with
/// `reply`; waits until the delivery leaves handing off.
fn answer_codex_turn_start(
    reply: std::result::Result<Value, CodexResponseError>,
) -> (AppState, String, String) {
    let state = test_app_state();
    let generation = 7;
    let (session_id, runtime, prompt_id) = codex_handing_off(&state, generation);
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut wire = Vec::new();
    handle_shared_codex_start_turn(
        &mut wire,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &runtime.thread_sessions,
        None,
        &session_id,
        "thread-receipted",
        None,
        codex_prompt(generation),
    )
    .expect("turn/start is written");
    let (_request_id, sender) = take_pending_codex_request(&pending_requests);
    sender.send(reply).unwrap();
    let published = phase_sync::PollGuard::new();
    while codex_delivery(&state, &session_id, &prompt_id).disposition
        == PromptDeliveryDisposition::HandingOff
    {
        published.wait("the turn/start reply's delivery outcome");
    }
    (state, session_id, prompt_id)
}

#[test]
fn a_codex_turn_start_response_accepts_the_prompt() {
    let (state, session_id, prompt_id) =
        answer_codex_turn_start(Ok(json!({"turn": {"id": "turn-receipted"}})));
    assert_eq!(
        codex_delivery(&state, &session_id, &prompt_id).disposition,
        PromptDeliveryDisposition::Accepted
    );
}

#[test]
fn a_codex_turn_start_timeout_leaves_the_delivery_ambiguous() {
    let (state, session_id, prompt_id) = answer_codex_turn_start(Err(
        CodexResponseError::Timeout("timed out waiting for turn/start".to_owned()),
    ));
    let delivery = codex_delivery(&state, &session_id, &prompt_id);
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Ambiguous);
    assert!(delivery.reason.as_deref().is_some_and(|reason| reason.contains("Timeout")));
}

#[test]
fn a_codex_refused_turn_rejects_the_prompt() {
    let (state, session_id, prompt_id) = answer_codex_turn_start(Err(
        CodexResponseError::JsonRpc("the thread is archived".to_owned()),
    ));
    assert_eq!(
        codex_delivery(&state, &session_id, &prompt_id).disposition,
        PromptDeliveryDisposition::Rejected
    );
}

/// Codex's notifications can overtake the `turn/start` response. A turn that
/// starts and then fails before that response arrives was received: its
/// `turn/started` accepts the receipted prompt, the failure settles nothing
/// more, and the acceptance survives a reload while the turn stays failed.
#[test]
fn a_codex_turn_that_starts_and_fails_before_its_turn_start_response_accepts_the_prompt() {
    let state = test_app_state();
    let generation = 7;
    let (session_id, runtime, prompt_id) = codex_handing_off(&state, generation);
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
        "thread-receipted",
        None,
        codex_prompt(generation),
    )
    .expect("turn/start is written");
    // The reply stays unsent until the end: the notifications overtake it.
    let (_request_id, reply) = take_pending_codex_request(&pending_requests);
    for message in [
        json!({
            "method": "turn/started",
            "params": { "threadId": "thread-receipted", "turn": { "id": "turn-receipted" } }
        }),
        json!({
            "method": "turn/completed",
            "params": {
                "threadId": "thread-receipted",
                "turn": { "id": "turn-receipted", "error": { "message": "the model failed" } }
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
        .expect("the notification is handled");
    }

    let delivery = codex_delivery(&state, &session_id, &prompt_id);
    assert_eq!(
        delivery.disposition,
        PromptDeliveryDisposition::Accepted,
        "the started turn received the prompt: {:?}",
        delivery.reason
    );
    let reloaded = {
        let inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session_id).unwrap();
        assert_ne!(inner.sessions[index].session.status, SessionStatus::Active, "the turn failed");
        PersistedSessionRecord::from_record(&inner.sessions[index])
            .into_record()
            .expect("the row loads")
    };
    assert_eq!(
        reloaded.prompt_deliveries[&prompt_id].disposition,
        PromptDeliveryDisposition::Accepted
    );
    drop(reply);
}

/// An ACP session whose receipted delivery is handing off under
/// `generation` on the runtime `runtime_id`; returns the session and its
/// prompt id.
fn acp_handing_off(state: &AppState, runtime_id: &str, generation: u64) -> (String, String) {
    let session_id = test_session_id(state, Agent::Cursor);
    let (runtime, _input) = test_acp_runtime_handle(AcpAgent::Cursor, runtime_id);
    let prompt_id = "message-acp-receipted".to_owned();
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&session_id).unwrap();
    let record = inner.session_mut_by_index(index).unwrap();
    record.runtime = SessionRuntime::Acp(runtime);
    record.session.status = SessionStatus::Active;
    record.active_turn_generation = generation;
    record.prompt_deliveries.insert(
        prompt_id.clone(),
        PromptDelivery {
            federation_operation: None,
            federation_handoff_started: false,
            disposition: PromptDeliveryDisposition::HandingOff,
            attempt: 1,
            write_ahead: "write-ahead-1".to_owned(),
            sequence: 1,
            message_id: Some(prompt_id.clone()),
            turn_generation: Some(generation),
            attempt_key: Some(format!("{session_id}:{generation}:1")),
            reason: None,
            entry: None,
            write_ahead_acknowledged: true,
            resume: None,
            runtime_write: PromptRuntimeWrite::Untracked,
        },
    );
    (session_id, prompt_id)
}

fn acp_prompt_on(
    state: &AppState,
    session_id: &str,
    runtime_id: &str,
    runtime_state: AcpRuntimeState,
) -> Result<()> {
    handle_acp_prompt_command(
        &mut BrokenStdin,
        &Arc::new(Mutex::new(HashMap::new())),
        state,
        session_id,
        &Arc::new(Mutex::new(runtime_state)),
        &Arc::new((Mutex::new(false), Condvar::new())),
        &RuntimeToken::Acp(runtime_id.to_owned()),
        None,
        AcpAgent::Cursor,
        AcpPromptCommand {
            cwd: "/tmp".to_owned(),
            cursor_mode: None,
            model: "auto".to_owned(),
            opencode_effort: None,
            opencode_mode: None,
            prompt: "a receipted ACP prompt".to_owned(),
            resume_session_id: None,
        },
    )
}

fn acp_delivery(state: &AppState, session_id: &str, prompt_id: &str) -> PromptDelivery {
    let inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(session_id).unwrap();
    inner.sessions[index].prompt_deliveries[prompt_id].clone()
}

/// An ACP runtime that fails while preparing its session, before
/// session/prompt is written, never received the prompt: rejected.
#[test]
fn an_acp_failure_before_session_prompt_rejects_the_delivery() {
    let state = test_app_state();
    let (session_id, prompt_id) = acp_handing_off(&state, "acp-unready", 4);
    assert!(acp_prompt_on(&state, &session_id, "acp-unready", AcpRuntimeState::default()).is_err());
    let delivery = acp_delivery(&state, &session_id, &prompt_id);
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Rejected);
    assert!(delivery.reason.as_deref().is_some_and(|reason| reason.contains("before the prompt")));
}

/// A failed session/prompt write may have reached the runtime: ambiguous.
#[test]
fn a_failed_acp_session_prompt_write_leaves_the_delivery_ambiguous() {
    let state = test_app_state();
    let (session_id, prompt_id) = acp_handing_off(&state, "acp-ready", 4);
    let ready = AcpRuntimeState {
        current_session_id: Some("external-acp-receipted".to_owned()),
        ..Default::default()
    };
    assert!(acp_prompt_on(&state, &session_id, "acp-ready", ready).is_err());
    let delivery = acp_delivery(&state, &session_id, &prompt_id);
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Ambiguous);
    assert!(delivery.reason.as_deref().is_some_and(|reason| reason.contains("session/prompt")));
}

/// A host with a real persist writer and a receipted Claude session, which a
/// restart reloads from the same store. Every state it boots shares its temp
/// root, so the root goes only with the last owner of any of them.
struct DurableHost {
    project_root: PathBuf,
    persistence_path: PathBuf,
    templates_path: PathBuf,
    root: Arc<TestTempRoot>,
}

impl DurableHost {
    fn new() -> Self {
        let (temp_root, project_root, persistence_path, templates_path) =
            super::delegation_support::temp_delegation_state_paths();
        Self { project_root, persistence_path, templates_path, root: Arc::new(temp_root) }
    }

    fn boot(&self) -> AppState {
        let mut state = AppState::new_with_paths(
            self.project_root.to_string_lossy().into_owned(),
            self.persistence_path.clone(),
            self.templates_path.clone(),
        )
        .expect("state should boot");
        state.test_temp_root = Some(self.root.clone());
        state
    }

    /// Ends the test only once every thread that cloned a booted state (a
    /// fence's settlement thread, for one) has released it: the root's
    /// removal receipt comes only after its last owner is gone.
    fn finish(self, fixture: Fixture) {
        let cleaned = self.root.observe_cleanup();
        drop(fixture);
        drop(self);
        phase_sync::receive(&cleaned, "every owner of the host's states releases them")
            .expect("the host's temp root is removed");
    }

    /// Stops `fixture`'s host (its writer flushes), boots it again, and
    /// gives the session a fresh test runtime.
    fn restart(&self, fixture: Fixture) -> Fixture {
        let Fixture { state, session, _root, .. } = fixture;
        state.shutdown_persist_blocking();
        drop(state);
        let state = self.boot();
        let (token, runtime_input, prompts) =
            install_test_claude_runtime(&state, &session, "restarted");
        Fixture { state, session, token, runtime_input, prompts, _root }
    }
}

#[test]
fn restart_before_the_write_ahead_commit_keeps_the_prompt_pending_and_sends_it_once() {
    let host = DurableHost::new();
    let fixture = Fixture::new(host.boot(), "restart-before");
    let id = fixture.enqueue_receipted("pending across the restart");
    // Paused, so the boot recovery of workflow prompts, which tests run
    // inside the restart, does not begin the hand-off before this test gives
    // the restarted session its test runtime.
    fixture.set_paused(true);
    let fixture = host.restart(fixture);
    let delivery = fixture.delivery(&id).expect("the receipt reloads");
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Pending);
    assert_eq!(fixture.queue_ids(), vec![id.clone()]);

    // Read back, its content is durable: the next dispatch writes ahead, and
    // the real writer's acknowledgement hands it off exactly once.
    fixture.set_paused(false);
    assert!(fixture.state.dispatch_next_queued_turn(&fixture.session, false).unwrap().is_none());
    assert_eq!(fixture.received_prompt("the restarted hand-off").text, "pending across the restart");
    fixture.wait_for(&id, "the promotion", |delivery| delivery.turn_generation.is_some());
    assert_eq!(fixture.delivered_prompts(), 0, "sent once");
    fixture.state.shutdown_persist_blocking();
    host.finish(fixture);
}

#[test]
fn restart_after_the_write_ahead_commit_is_ambiguous_and_never_replayed() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "restart-after-write-ahead");
    let id = fixture.enqueue_receipted("written ahead, then the host went down");
    assert!(fixture.state.request_queued_prompt_durability(&fixture.session, &id));
    writer.take(HeldFence::Durability).finish(Ok(()));
    // The write-ahead record is committed and its acknowledgement is still in
    // flight: the host goes down now. From the row alone, whether the hand-off
    // began cannot be told.
    let in_flight = writer.take(HeldFence::WriteAhead);
    let reloaded = {
        let inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        PersistedSessionRecord::from_record(&inner.sessions[index])
            .into_record()
            .expect("the row loads")
    };
    let delivery = &reloaded.prompt_deliveries[&id];
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Ambiguous);
    assert!(delivery.reason.as_deref().is_some_and(|reason| reason.contains("restarted")));
    assert!(
        !reloaded
            .queued_prompts
            .iter()
            .any(|queued| queued.pending_prompt.id == id),
        "its entry never runs again"
    );
    assert!(
        reloaded.session.prompt_deliveries.iter().any(|view| view.prompt_id == id
            && view.disposition == PromptDeliveryDisposition::Ambiguous),
        "the wire says so"
    );
    drop(in_flight);
}

/// A write-ahead acknowledged while the entry cannot be promoted (the
/// session went busy) sent nothing: the prompt returns to pending, durably,
/// so a restart does not drop it as ambiguous.
#[test]
fn an_acknowledged_write_ahead_that_promotes_nothing_returns_to_pending() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "busy-at-write-ahead");
    let id = fixture.enqueue_receipted("overtaken while written ahead");
    assert!(fixture.state.request_queued_prompt_durability(&fixture.session, &id));
    writer.take(HeldFence::Durability).finish(Ok(()));
    let write_ahead = writer.take(HeldFence::WriteAhead);
    let nonce = fixture.delivery(&id).unwrap().write_ahead;
    {
        // Another turn starts on the session before the acknowledgement.
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        inner.session_mut_by_index(index).unwrap().session.status = SessionStatus::Active;
    }
    // Settled here, so its dispatch attempt has returned before the asserts.
    fixture
        .state
        .settle_prompt_delivery_write_ahead(&fixture.session, &id, &nonce, Ok(()));
    let delivery = fixture.delivery(&id).unwrap();
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Pending);
    assert_ne!(delivery.write_ahead, nonce, "a superseding write");
    assert_eq!(fixture.delivered_prompts(), 0);
    assert_eq!(fixture.queue_ids(), vec![id.clone()]);
    let reloaded = {
        let inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        PersistedSessionRecord::from_record(&inner.sessions[index])
            .into_record()
            .expect("the row loads")
    };
    assert_eq!(reloaded.prompt_deliveries[&id].disposition, PromptDeliveryDisposition::Pending);
    assert_eq!(reloaded.queued_prompts.front().unwrap().pending_prompt.id, id);
    drop(write_ahead);
}

/// A runtime event that names its runtime but not its turn settles only the
/// turn that runtime runs now: a turn's end accepts, a runtime's exit leaves
/// the delivery unknown, and an earlier runtime's event settles nothing.
#[test]
fn turn_end_events_settle_only_their_own_runtime_turn() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "turn-end");
    let id = hand_off_through_held_fences(&fixture, &writer, "ended by its runtime");
    fixture.received_prompt("the hand-off");
    let earlier_runtime = RuntimeToken::Claude("an-earlier-runtime".to_owned());
    let _ = fixture
        .state
        .handle_runtime_exit_if_matches(&fixture.session, &earlier_runtime, Some("gone"));
    let _ = fixture
        .state
        .finish_turn_ok_if_runtime_matches(&fixture.session, &earlier_runtime);
    assert_eq!(
        fixture.delivery(&id).unwrap().disposition,
        PromptDeliveryDisposition::HandingOff,
        "an earlier runtime's events settle nothing"
    );
    let _ = fixture
        .state
        .handle_runtime_exit_if_matches(&fixture.session, &fixture.token, Some("crashed"));
    let delivery = fixture.delivery(&id).unwrap();
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Ambiguous);
    assert!(delivery.reason.as_deref().is_some_and(|reason| reason.contains("exited")));
}

#[test]
fn a_finished_turn_accepts_a_prompt_not_yet_acknowledged() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "turn-finished");
    let id = hand_off_through_held_fences(&fixture, &writer, "finished by its runtime");
    fixture.received_prompt("the hand-off");
    let _ = fixture
        .state
        .finish_turn_ok_if_runtime_matches(&fixture.session, &fixture.token);
    assert_eq!(fixture.delivery(&id).unwrap().disposition, PromptDeliveryDisposition::Accepted);
}

/// Makes a receipted head durable on a paused queue: its hold is acknowledged
/// and, paused, nothing begins its write-ahead by itself.
fn durable_paused_head(fixture: &Fixture, writer: &DeliveryWriter, text: &str) -> String {
    let id = fixture.enqueue_receipted(text);
    fixture.set_paused(true);
    assert!(fixture.state.request_queued_prompt_durability(&fixture.session, &id));
    writer.take(HeldFence::Durability).finish(Ok(()));
    fixture.wait_for_hold_acknowledged(&id);
    let delivery = fixture.delivery(&id).unwrap();
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Pending);
    assert_eq!(delivery.attempt, 0, "a paused queue begins no write-ahead by itself");
    assert_eq!(fixture.delivered_prompts(), 0);
    id
}

fn queue_paused(fixture: &Fixture) -> bool {
    let inner = fixture.state.inner.lock().unwrap();
    let index = inner.find_session_index(&fixture.session).unwrap();
    inner.sessions[index].orchestrator_auto_dispatch_blocked
}

fn resume(fixture: &Fixture) -> std::result::Result<Option<TurnDispatchDeliveryOutcome>, ApiError> {
    fixture.state.resume_local_session_queue(&fixture.session, None)
}

fn assert_preparing(fixture: &Fixture) {
    assert!(
        matches!(resume(fixture), Ok(Some(TurnDispatchDeliveryOutcome::Scheduled))),
        "the Resume is accepted as preparing the hand-off"
    );
}

/// Enqueues an ordinary prompt behind the head; returns its id.
fn enqueue_successor(fixture: &Fixture, text: &str) -> String {
    let mut inner = fixture.state.inner.lock().unwrap();
    let prompt = PendingPrompt {
        engram_interrupted: false,
        is_engram_retained: false,
        attachments: Vec::new(),
        id: inner.next_message_id(),
        timestamp: stamp_now(),
        text: text.to_owned(),
        expanded_text: None,
        source: None,
    };
    let id = prompt.id.clone();
    let index = inner.find_session_index(&fixture.session).unwrap();
    queue_prompt_on_record(inner.session_mut_by_index(index).unwrap(), prompt, Vec::new());
    fixture.state.commit_locked(&mut inner).unwrap();
    id
}

/// A durable receipted head of a paused queue: Resume begins its write-ahead
/// and reports it as being prepared, the pause staying; a repeated Resume is
/// the same operation; the acknowledgement sends the head exactly once.
#[test]
fn resume_of_a_paused_queue_prepares_then_sends_its_receipted_head_once() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "paused-resume");
    let id = durable_paused_head(&fixture, &writer, "resumed explicitly");

    assert_preparing(&fixture);
    let write_ahead = writer.take(HeldFence::WriteAhead);
    let delivery = fixture.delivery(&id).unwrap();
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::HandingOff);
    assert_eq!(delivery.attempt, 1);
    assert!(delivery.resume.is_some(), "the attempt carries this Resume");
    assert!(queue_paused(&fixture), "the pause stays until the turn starts");
    assert_eq!(fixture.delivered_prompts(), 0, "nothing before the write-ahead");

    assert_preparing(&fixture);
    assert_eq!(fixture.delivery(&id).unwrap().attempt, 1, "the same attempt");
    assert!(writer.held.try_recv().is_err(), "no second write-ahead");

    write_ahead.finish(Ok(()));
    assert_eq!(fixture.received_prompt("the resumed head").text, "resumed explicitly");
    fixture.wait_for(&id, "the promotion", |delivery| delivery.turn_generation.is_some());
    assert_eq!(fixture.delivered_prompts(), 0, "sent once");
}

/// A Resume while the head's initial durability is still pending keeps the
/// landed refusal and remembers nothing: that acknowledgement sends nothing,
/// and only a fresh Resume begins the hand-off.
#[test]
fn resume_before_the_head_is_saved_is_refused_and_keeps_nothing() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "resume-during-hold");
    let id = fixture.enqueue_receipted("resumed before it was saved");
    fixture.set_paused(true);
    assert!(fixture.state.request_queued_prompt_durability(&fixture.session, &id));
    let durability = writer.take(HeldFence::Durability);

    let refused = match resume(&fixture) {
        Err(error) => error,
        Ok(_) => panic!("a Resume of an unsaved head is refused"),
    };
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert!(refused.message.contains("still being saved"), "{}", refused.message);

    durability.finish(Ok(()));
    fixture.wait_for_hold_acknowledged(&id);
    let delivery = fixture.delivery(&id).unwrap();
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Pending);
    assert_eq!(delivery.attempt, 0, "the refused Resume began nothing");
    assert!(writer.held.try_recv().is_err());
    assert_eq!(fixture.delivered_prompts(), 0, "no later bypass");
    assert!(queue_paused(&fixture));

    assert_preparing(&fixture);
    writer.take(HeldFence::WriteAhead).finish(Ok(()));
    assert_eq!(fixture.received_prompt("the head, resumed again").text, "resumed before it was saved");
    assert_eq!(fixture.delivered_prompts(), 0, "sent once");
}

/// A Stop between the Resume and the write-ahead's acknowledgement ends
/// that Resume: the acknowledgement sends nothing past the pause, and the
/// head returns to pending for a fresh Resume.
#[test]
fn a_stop_after_the_resume_ends_it() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "resume-then-stop");
    let id = durable_paused_head(&fixture, &writer, "stopped before its hand-off");
    assert_preparing(&fixture);
    let _write_ahead = writer.take(HeldFence::WriteAhead);
    let nonce = fixture.delivery(&id).unwrap().write_ahead;
    {
        // What a Stop does to the stop generation.
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.runtime_stop_generation = record.runtime_stop_generation.wrapping_add(1).max(1);
    }
    // Settled here, so its dispatch attempt has returned before the asserts.
    fixture
        .state
        .settle_prompt_delivery_write_ahead(&fixture.session, &id, &nonce, Ok(()));
    assert_eq!(fixture.delivered_prompts(), 0, "nothing past the pause after the Stop");
    let delivery = fixture.delivery(&id).unwrap();
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Pending);
    assert!(delivery.resume.is_none());
    assert!(queue_paused(&fixture));
    assert_eq!(fixture.queue_ids(), vec![id]);
}

/// A pause after the Resume ends it even though the queue reads paused
/// again: the Resume crossed one pause, not a later one.
#[test]
fn a_later_pause_ends_the_resume_even_when_the_queue_reads_paused_again() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "resume-then-repause");
    let id = durable_paused_head(&fixture, &writer, "paused again before its hand-off");
    assert_preparing(&fixture);
    let _write_ahead = writer.take(HeldFence::WriteAhead);
    let nonce = fixture.delivery(&id).unwrap().write_ahead;
    fixture.set_paused(false);
    fixture.set_paused(true);
    fixture
        .state
        .settle_prompt_delivery_write_ahead(&fixture.session, &id, &nonce, Ok(()));
    assert_eq!(fixture.delivered_prompts(), 0, "the later pause is not crossed");
    assert_eq!(fixture.delivery(&id).unwrap().disposition, PromptDeliveryDisposition::Pending);
    assert!(queue_paused(&fixture));
}

/// What the settlement does under its lock on the acknowledgement of a
/// Resume's attempt: the attempt is acknowledged and the Resume, still
/// holding, names the head its drain is bound to. The release drain then runs
/// after that lock is released.
fn acknowledged_resume(
    fixture: &Fixture,
    writer: &DeliveryWriter,
    text: &str,
) -> (String, EngramQueuedAdmissionOwner, Box<PersistFence>) {
    let id = durable_paused_head(fixture, writer, text);
    assert_preparing(fixture);
    let write_ahead = writer.take(HeldFence::WriteAhead);
    let owner = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        assert!(prompt_delivery_resume_holds(record, &id));
        let delivery = record.prompt_deliveries.get_mut(&id).unwrap();
        delivery.write_ahead_acknowledged = true;
        delivery.resume.as_ref().unwrap().owner(&id)
    };
    (id, owner, write_ahead)
}

/// The old unbound-drain interleaving: a Cancel exposes a successor between
/// the acknowledgement's check and the release drain. That drain is bound to
/// the cancelled head, so it starts nothing, and the successor stays paused.
#[test]
fn a_cancel_between_the_acknowledgement_and_the_promotion_starts_no_successor() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "resume-then-cancel");
    let (id, owner, _write_ahead) =
        acknowledged_resume(&fixture, &writer, "cancelled before its promotion");
    let successor = enqueue_successor(&fixture, "the successor behind it");
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        clear_queued_prompts_by_source(record, QueuedPromptSource::Orchestrator);
        fixture.state.commit_locked(&mut inner).unwrap();
    }
    assert_eq!(fixture.queue_ids(), vec![successor.clone()]);
    fixture.state.dispatch_after_write_ahead(&fixture.session, &id, Some(owner));
    assert_eq!(fixture.delivered_prompts(), 0, "no successor past the pause");
    assert_eq!(fixture.queue_ids(), vec![successor]);
    assert!(queue_paused(&fixture));
}

/// The same window with the head edited in place (same prompt id): the
/// Resume was for the content it saw, so nothing is sent.
#[test]
fn a_content_change_between_the_acknowledgement_and_the_promotion_sends_nothing() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "resume-then-edit");
    let (id, owner, _write_ahead) =
        acknowledged_resume(&fixture, &writer, "edited before its promotion");
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.queued_prompts.front_mut().unwrap().pending_prompt.text = "something else".to_owned();
    }
    fixture.state.dispatch_after_write_ahead(&fixture.session, &id, Some(owner));
    assert_eq!(fixture.delivered_prompts(), 0, "not the content the Resume saw");
    assert_eq!(fixture.queue_ids(), vec![id]);
    assert!(queue_paused(&fixture));
}

/// The same window with a pause and its lifting and a new pause: the queue
/// reads paused again, but the Resume crossed an earlier pause.
#[test]
fn a_pause_between_the_acknowledgement_and_the_promotion_sends_nothing() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "resume-then-ack-then-repause");
    let (id, owner, _write_ahead) =
        acknowledged_resume(&fixture, &writer, "paused again before its promotion");
    fixture.set_paused(false);
    fixture.set_paused(true);
    fixture.state.dispatch_after_write_ahead(&fixture.session, &id, Some(owner));
    assert_eq!(fixture.delivered_prompts(), 0, "the later pause is not crossed");
    assert_eq!(fixture.queue_ids(), vec![id]);
}

/// A failed write-ahead after a Resume ends that Resume and leaves an
/// actionable paused head; a fresh Resume begins a new attempt that sends it.
#[test]
fn a_failed_write_ahead_after_a_resume_leaves_a_paused_head_for_a_fresh_resume() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "resume-then-failed-write-ahead");
    let id = durable_paused_head(&fixture, &writer, "its first record was not saved");
    assert_preparing(&fixture);
    writer.take(HeldFence::WriteAhead).finish(Err(PersistFenceError::Deadline));
    fixture.wait_for(&id, "the return to pending", |delivery| {
        delivery.disposition == PromptDeliveryDisposition::Pending && delivery.attempt == 1
    });
    let delivery = fixture.delivery(&id).unwrap();
    assert!(delivery.resume.is_none());
    assert!(delivery.reason.as_deref().is_some_and(|reason| reason.contains("not acknowledged")));
    assert!(queue_paused(&fixture));
    assert_eq!(fixture.delivered_prompts(), 0);

    assert_preparing(&fixture);
    assert_eq!(fixture.delivery(&id).unwrap().attempt, 2);
    writer.take(HeldFence::WriteAhead).finish(Ok(()));
    assert_eq!(fixture.received_prompt("the second attempt").text, "its first record was not saved");
    assert_eq!(fixture.delivered_prompts(), 0, "sent once");
}

/// A drain allowed past the pause that is not an explicit Resume (a manual
/// send's, a retry's) begins no write-ahead for a paused queue's receipted
/// head and carries none across the pause.
#[test]
fn a_drain_that_is_not_an_explicit_resume_begins_no_hand_off_on_a_paused_queue() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "paused-internal-drain");
    let id = durable_paused_head(&fixture, &writer, "paused, drained internally");
    assert!(fixture
        .state
        .dispatch_next_queued_turn(&fixture.session, true)
        .unwrap()
        .is_none());
    let delivery = fixture.delivery(&id).unwrap();
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Pending);
    assert_eq!(delivery.attempt, 0);
    assert!(writer.held.try_recv().is_err(), "no write-ahead");
    assert_eq!(fixture.delivered_prompts(), 0);
}

/// Claude's command lifecycle `queued` for the attempt's command uuid proves
/// receipt by itself, whatever the turn's attribution does.
#[test]
fn claude_accepts_the_prompt_with_its_command_lifecycle_queued_by_uuid() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "claude-queued");
    let id = hand_off_through_held_fences(&fixture, &writer, "queued by Claude");
    let command = fixture.received_prompt("the hand-off");
    let uuid = command.replay_generation.clone();
    let context = fixture.claude_context();
    let mut frames = ClaudeReaderFrames::new(&context, fixture.runtime_input.clone(), None);
    let mut recorder = SessionRecorder::new(fixture.state.clone(), fixture.session.clone());
    // Another command's lifecycle settles nothing.
    let _ = apply_claude_frame(
        &context,
        &mut frames,
        &mut recorder,
        &json!({"type": "command_lifecycle", "command_uuid": "another-command", "state": "queued"}),
    );
    assert_eq!(fixture.delivery(&id).unwrap().disposition, PromptDeliveryDisposition::HandingOff);
    let _ = apply_claude_frame(
        &context,
        &mut frames,
        &mut recorder,
        &json!({"type": "command_lifecycle", "command_uuid": uuid, "state": "queued"}),
    );
    assert_eq!(fixture.delivery(&id).unwrap().disposition, PromptDeliveryDisposition::Accepted);
}

/// An error `result` naming the attempt's command uuid, with no lifecycle
/// frame or echo before it, proves Claude received the prompt as a successful
/// one does: the delivery is accepted while the turn ends in its error.
#[test]
fn claude_accepts_the_prompt_its_correlated_error_result_names() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "claude-error-result");
    let id = hand_off_through_held_fences(&fixture, &writer, "ended in an error");
    let command = fixture.received_prompt("the hand-off");
    let uuid = command.replay_generation.clone();
    let context = fixture.claude_context();
    let mut wire = Vec::new();
    assert!(apply_claude_writer_command(
        &context,
        &mut wire,
        ClaudeRuntimeCommand::Prompt(command)
    ));
    let mut frames = ClaudeReaderFrames::new(&context, fixture.runtime_input.clone(), None);
    let mut recorder = SessionRecorder::new(fixture.state.clone(), fixture.session.clone());
    assert_eq!(fixture.delivery(&id).unwrap().disposition, PromptDeliveryDisposition::HandingOff);
    let _ = apply_claude_frame(
        &context,
        &mut frames,
        &mut recorder,
        &json!({"type": "result", "subtype": "error_during_execution", "is_error": true,
            "result": "the tool failed", "user_message_uuid": uuid}),
    );
    let delivery = fixture.delivery(&id).unwrap();
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Accepted);
    assert!(delivery.entry.is_none());
}

/// Claude took up the prompt inside a turn no prompt owned (a background
/// notice opened it first): the attribution is mixed, but the delivery is
/// accepted.
#[test]
fn claude_accepts_a_prompt_its_unowned_turn_took_up() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "claude-joined");
    let id = hand_off_through_held_fences(&fixture, &writer, "taken up mid-turn");
    let command = fixture.received_prompt("the hand-off");
    let uuid = command.replay_generation.clone();
    let context = fixture.claude_context();
    let mut frames = ClaudeReaderFrames::new(&context, fixture.runtime_input.clone(), None);
    let mut recorder = SessionRecorder::new(fixture.state.clone(), fixture.session.clone());
    for frame in [
        json!({"type": "system", "subtype": "init", "session_id": "joined",
            "capabilities": ["interrupt_receipt_v1", "msg_lifecycle_v1"]}),
        json!({"type": "user", "message": {"role": "user", "content":
            "<task-notification>\n<task-id>b1</task-id>\n<status>completed</status>\n</task-notification>"},
            "isReplay": true}),
    ] {
        let _ = apply_claude_frame(&context, &mut frames, &mut recorder, &frame);
    }
    let mut wire = Vec::new();
    assert!(apply_claude_writer_command(
        &context,
        &mut wire,
        ClaudeRuntimeCommand::Prompt(command)
    ));
    let _ = apply_claude_frame(
        &context,
        &mut frames,
        &mut recorder,
        &json!({"type": "command_lifecycle", "command_uuid": uuid, "state": "started"}),
    );
    assert_eq!(fixture.delivery(&id).unwrap().disposition, PromptDeliveryDisposition::Accepted);
}

/// Drives a receipted prompt through the real writer to its hand-off and
/// returns its id, its hold's nonce and the turn it was promoted under.
fn hand_off_durably(fixture: &Fixture, text: &str) -> (String, String, u64) {
    let id = fixture.enqueue_receipted(text);
    let nonce = {
        let inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        inner.sessions[index].queued_prompt_durability_holds[&id].nonce.clone()
    };
    assert!(fixture.state.request_queued_prompt_durability(&fixture.session, &id));
    let command = fixture.received_prompt("the durable hand-off");
    fixture.wait_for(&id, "the promotion", |delivery| delivery.turn_generation.is_some());
    (id, nonce, command.turn_generation)
}

#[test]
fn restart_after_acceptance_reads_delivered_and_never_replays() {
    let host = DurableHost::new();
    let fixture = Fixture::new(host.boot(), "restart-accepted");
    let (id, _, generation) = hand_off_durably(&fixture, "accepted before the restart");
    fixture.state.note_prompt_delivery_outcome(
        &fixture.session,
        Some(generation),
        PromptDeliveryOutcome::Accepted,
    );
    let fixture = host.restart(fixture);
    assert_eq!(
        fixture.delivery(&id).expect("the receipt reloads").disposition,
        PromptDeliveryDisposition::Accepted
    );
    assert!(fixture.queue_ids().is_empty());
    assert!(fixture.state.dispatch_next_queued_turn(&fixture.session, false).unwrap().is_none());
    assert_eq!(fixture.delivered_prompts(), 0, "never replayed");
    fixture.state.shutdown_persist_blocking();
    host.finish(fixture);
}

#[test]
fn restart_after_rejection_requeues_only_under_a_new_prompt_id_and_prerequisite() {
    let host = DurableHost::new();
    let fixture = Fixture::new(host.boot(), "restart-rejected");
    let (id, old_nonce, generation) = hand_off_durably(&fixture, "rejected before the restart");
    assert!(
        fixture.delivery(&id).unwrap().entry.is_some(),
        "a promoted entry is kept for a re-queue"
    );
    fixture.state.note_prompt_delivery_outcome(
        &fixture.session,
        Some(generation),
        PromptDeliveryOutcome::Rejected("the runtime was gone".to_owned()),
    );
    let mut restarted = host.restart(fixture);
    assert_eq!(
        restarted.delivery(&id).expect("the receipt reloads").disposition,
        PromptDeliveryDisposition::Rejected
    );
    assert!(restarted.queue_ids().is_empty(), "a rejected prompt is not replayed by itself");

    // Re-queued, it is a new prompt id with a new hold.
    let writer = DeliveryWriter::attach(&mut restarted.state);
    let new_id = restarted
        .state
        .requeue_rejected_prompt_delivery(&restarted.session, &id)
        .expect("the rejected prompt re-queues");
    assert_ne!(new_id, id);
    let durability = writer.take(HeldFence::Durability);
    let new_hold = {
        let inner = restarted.state.inner.lock().unwrap();
        let index = inner.find_session_index(&restarted.session).unwrap();
        inner.sessions[index].queued_prompt_durability_holds[&new_id].clone()
    };
    assert!(!new_hold.acknowledged);
    assert_ne!(new_hold.nonce, old_nonce, "a fresh nonce");
    // The old prompt's acknowledgement cannot release the new one.
    restarted.state.settle_queued_prompt_durability(
        &restarted.session,
        &new_id,
        &old_nonce,
        Ok(()),
    );
    {
        let inner = restarted.state.inner.lock().unwrap();
        let index = inner.find_session_index(&restarted.session).unwrap();
        assert!(
            !inner.sessions[index].queued_prompt_durability_holds[&new_id].acknowledged,
            "still held for its own acknowledgement"
        );
    }
    assert_eq!(restarted.delivered_prompts(), 0);
    assert_eq!(restarted.delivery(&new_id).unwrap().disposition, PromptDeliveryDisposition::Pending);
    assert!(
        restarted
            .state
            .requeue_rejected_prompt_delivery(&restarted.session, &id)
            .is_err(),
        "a rejected prompt is re-queued once"
    );
    // The unanswered fence settles on its own thread, which holds the state;
    // the host's finish waits for it.
    drop(durability);
    drop(writer);
    host.finish(restarted);
}

#[test]
fn a_prompt_without_a_receipt_dispatches_exactly_as_before() {
    let mut state = test_app_state();
    let _writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "unreceipted");
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let prompt = PendingPrompt {
            engram_interrupted: false,
            is_engram_retained: false,
            attachments: Vec::new(),
            id: inner.next_message_id(),
            timestamp: stamp_now(),
            text: "ordinary".to_owned(),
            expanded_text: None,
            source: None,
        };
        let index = inner.find_session_index(&fixture.session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        queue_prompt_on_record(record, prompt, Vec::new());
        fixture.state.commit_locked(&mut inner).unwrap();
    }
    let dispatch = fixture
        .state
        .dispatch_next_queued_turn(&fixture.session, false)
        .unwrap()
        .expect("an ordinary prompt promotes at once");
    let _ = deliver_turn_dispatch(&fixture.state, dispatch);
    assert_eq!(fixture.received_prompt("the ordinary prompt").text, "ordinary");
    assert!(fixture.view().is_empty(), "no receipt for an ordinary prompt");
}

/// Another drain that reaches the promotion gate after the acknowledgement
/// and before the release drain completes the Resume's one authorized
/// promotion, of the original head and once: the release drain then starts
/// nothing, its cleanup does not return the promoted attempt to pending, and
/// the head's successor stays queued.
#[test]
fn another_drain_after_the_acknowledgement_completes_the_authorized_promotion_once() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "resume-other-drain");
    let (id, owner, _write_ahead) =
        acknowledged_resume(&fixture, &writer, "promoted by another drain");
    let nonce = fixture.delivery(&id).unwrap().write_ahead;
    let successor = enqueue_successor(&fixture, "the successor behind it");

    let dispatch = fixture
        .state
        .dispatch_next_queued_turn(&fixture.session, true)
        .unwrap()
        .expect("the authorized head is promoted");
    let _ = deliver_turn_dispatch(&fixture.state, dispatch);
    assert_eq!(fixture.received_prompt("the authorized head").text, "promoted by another drain");
    let delivery = fixture.delivery(&id).unwrap();
    assert!(delivery.turn_generation.is_some());
    assert!(delivery.resume.is_none(), "consumed by that promotion");

    fixture.state.dispatch_after_write_ahead(&fixture.session, &id, Some(owner));
    fixture
        .state
        .return_unpromoted_write_ahead_to_pending(&fixture.session, &id, &nonce);
    assert_eq!(fixture.delivered_prompts(), 0, "sent once");
    let delivery = fixture.delivery(&id).unwrap();
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::HandingOff);
    assert!(delivery.turn_generation.is_some(), "the promoted attempt is not reset");
    assert_eq!(fixture.queue_ids(), vec![successor]);
}

/// The same window after a pause and its lifting and a new pause: the other
/// drain finds the Resume ended and promotes nothing past the new pause.
#[test]
fn another_drain_after_a_later_pause_promotes_nothing() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "resume-other-drain-repaused");
    let (id, _owner, _write_ahead) =
        acknowledged_resume(&fixture, &writer, "paused again before another drain");
    fixture.set_paused(false);
    fixture.set_paused(true);
    assert!(fixture
        .state
        .dispatch_next_queued_turn(&fixture.session, true)
        .unwrap()
        .is_none());
    assert_eq!(fixture.delivered_prompts(), 0, "the later pause is not crossed");
    assert_eq!(fixture.queue_ids(), vec![id]);
}

/// A manual send in that window starts its own prompt ahead of the paused
/// queue, as any manual send does; it inherits no Resume and no receipt. The
/// receipted head, never sent, returns to pending with its Resume ended.
#[test]
fn a_manual_send_after_the_acknowledgement_inherits_nothing() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "resume-manual-send");
    let (id, owner, _write_ahead) =
        acknowledged_resume(&fixture, &writer, "overtaken by a manual send");
    let nonce = fixture.delivery(&id).unwrap().write_ahead;
    let sent = fixture.state.dispatch_turn(
        &fixture.session,
        SendMessageRequest {
            text: "a manual send".to_owned(),
            expanded_text: None,
            attachments: Vec::new(),
            source_session_id: None,
            source_mailbox: None,
        },
    );
    match sent {
        Ok(DispatchTurnResult::Dispatched(dispatch))
        | Ok(DispatchTurnResult::DispatchedAfterQueue(dispatch)) => {
            let _ = deliver_turn_dispatch(&fixture.state, dispatch);
        }
        Ok(_) => panic!("the manual send starts its own prompt"),
        Err(error) => panic!("the manual send failed: {}", error.message),
    }
    assert_eq!(fixture.received_prompt("the manual send").text, "a manual send");

    fixture.state.dispatch_after_write_ahead(&fixture.session, &id, Some(owner));
    fixture
        .state
        .return_unpromoted_write_ahead_to_pending(&fixture.session, &id, &nonce);
    assert_eq!(fixture.delivered_prompts(), 0, "the receipted head is not sent");
    let delivery = fixture.delivery(&id).unwrap();
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Pending);
    assert!(delivery.resume.is_none());
    let inner = fixture.state.inner.lock().unwrap();
    let index = inner.find_session_index(&fixture.session).unwrap();
    assert_eq!(inner.sessions[index].prompt_deliveries.len(), 1, "the manual prompt keeps no receipt");
}

/// A Stop after the acknowledgement, as a Stop advances the stop generation.
fn revoke_by_stop(fixture: &Fixture, _id: &str) {
    let mut inner = fixture.state.inner.lock().unwrap();
    let index = inner.find_session_index(&fixture.session).unwrap();
    let record = inner.session_mut_by_index(index).unwrap();
    record.runtime_stop_generation = record.runtime_stop_generation.wrapping_add(1).max(1);
}

/// A pause, its lifting and a new pause: the queue reads paused again.
fn revoke_by_later_pause(fixture: &Fixture, _id: &str) {
    fixture.set_paused(false);
    fixture.set_paused(true);
}

/// The head replaced in place under the same prompt id.
fn revoke_by_replacement(fixture: &Fixture, id: &str) {
    let mut inner = fixture.state.inner.lock().unwrap();
    let index = inner.find_session_index(&fixture.session).unwrap();
    let record = inner.session_mut_by_index(index).unwrap();
    let head = record.queued_prompts.front_mut().unwrap();
    assert_eq!(head.pending_prompt.id, id);
    head.pending_prompt.text = "a replacement".to_owned();
}

/// Another drain after the acknowledgement and `revoke` finds the Resume
/// ended: it promotes nothing past the pause.
fn another_drain_after(label: &str, revoke: fn(&Fixture, &str)) {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, label);
    let (id, _owner, _write_ahead) = acknowledged_resume(&fixture, &writer, "revoked before another drain");
    revoke(&fixture, &id);
    assert!(fixture
        .state
        .dispatch_next_queued_turn(&fixture.session, true)
        .unwrap()
        .is_none());
    assert_eq!(fixture.delivered_prompts(), 0, "the ended Resume authorizes nothing");
    assert_eq!(fixture.queue_ids(), vec![id]);
}

#[test]
fn another_drain_after_a_stop_promotes_nothing() {
    another_drain_after("resume-other-drain-stopped", revoke_by_stop);
}

#[test]
fn another_drain_after_a_replacement_promotes_nothing() {
    another_drain_after("resume-other-drain-replaced", revoke_by_replacement);
}

/// A manual send after the acknowledgement and `revoke` starts only its own
/// prompt; neither it nor the release drain sends the receipted head, which
/// returns to pending with its Resume ended.
fn manual_send_after(label: &str, revoke: fn(&Fixture, &str)) {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, label);
    let (id, owner, _write_ahead) = acknowledged_resume(&fixture, &writer, "revoked before a manual send");
    let nonce = fixture.delivery(&id).unwrap().write_ahead;
    revoke(&fixture, &id);
    let sent = fixture.state.dispatch_turn(
        &fixture.session,
        SendMessageRequest {
            text: "a manual send".to_owned(),
            expanded_text: None,
            attachments: Vec::new(),
            source_session_id: None,
            source_mailbox: None,
        },
    );
    match sent {
        Ok(DispatchTurnResult::Dispatched(dispatch))
        | Ok(DispatchTurnResult::DispatchedAfterQueue(dispatch)) => {
            let _ = deliver_turn_dispatch(&fixture.state, dispatch);
        }
        Ok(_) => panic!("the manual send starts its own prompt"),
        Err(error) => panic!("the manual send failed: {}", error.message),
    }
    assert_eq!(fixture.received_prompt("the manual send").text, "a manual send");
    fixture.state.dispatch_after_write_ahead(&fixture.session, &id, Some(owner));
    fixture
        .state
        .return_unpromoted_write_ahead_to_pending(&fixture.session, &id, &nonce);
    assert_eq!(fixture.delivered_prompts(), 0, "the receipted head is not sent");
    let delivery = fixture.delivery(&id).unwrap();
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Pending);
    assert!(delivery.resume.is_none());
}

#[test]
fn a_manual_send_after_a_stop_inherits_nothing() {
    manual_send_after("resume-manual-send-stopped", revoke_by_stop);
}

#[test]
fn a_manual_send_after_a_later_pause_inherits_nothing() {
    manual_send_after("resume-manual-send-repaused", revoke_by_later_pause);
}

#[test]
fn a_manual_send_after_a_replacement_inherits_nothing() {
    manual_send_after("resume-manual-send-replaced", revoke_by_replacement);
}

/// What a Resume accepts, under its first lock, for a durable paused
/// receipted head.
fn accepted_identity(fixture: &Fixture) -> ReceiptedResumeIdentity {
    let inner = fixture.state.inner.lock().unwrap();
    let index = inner.find_session_index(&fixture.session).unwrap();
    receipted_resume_identity(&inner.sessions[index], None).expect("a receipted head")
}

/// The Resume's write-ahead step runs after the Engram release, under a
/// fresh lock. A Stop, a later pause or a replacement of the head since the
/// Resume accepted it makes that step begin nothing and refuse with why: no
/// write-ahead, the head pending, the queue paused.
#[test]
fn a_change_between_the_resume_and_its_write_ahead_begins_nothing() {
    let revocations: [(&str, fn(&Fixture, &str)); 3] = [
        ("stop", revoke_by_stop),
        ("pause", revoke_by_later_pause),
        ("replacement", revoke_by_replacement),
    ];
    for (label, revoke) in revocations {
        let mut state = test_app_state();
        let writer = DeliveryWriter::attach(&mut state);
        let fixture = Fixture::new(state, &format!("resume-release-gap-{label}"));
        let id = durable_paused_head(&fixture, &writer, "changed after the Resume accepted it");
        let accepted = accepted_identity(&fixture);
        revoke(&fixture, &id);
        match fixture.state.resume_receipted_head(&fixture.session, None, &accepted) {
            ReceiptedResume::Refused(message) => {
                assert_eq!(message, RESUME_QUEUE_CHANGED_REFUSAL, "{label}")
            }
            ReceiptedResume::Preparing | ReceiptedResume::NotReceipted => {
                panic!("{label}: a Resume whose head changed begins nothing")
            }
        }
        let delivery = fixture.delivery(&id).unwrap();
        assert_eq!(delivery.disposition, PromptDeliveryDisposition::Pending, "{label}");
        assert_eq!(delivery.attempt, 0, "{label}: no write-ahead");
        assert!(writer.held.try_recv().is_err(), "{label}: no write-ahead fence");
        assert_eq!(fixture.delivered_prompts(), 0, "{label}");
        assert!(queue_paused(&fixture), "{label}: the pause stays");
    }
}

/// A receipted head promoted for a retained admission (handing off, its turn
/// named, still queued) that is held before its hand-off.
fn promoted_retained_receipt(fixture: &Fixture, writer: &DeliveryWriter) -> (String, u64) {
    let id = durable_paused_head(fixture, writer, "promoted, then held before its hand-off");
    let mut inner = fixture.state.inner.lock().unwrap();
    let index = inner.find_session_index(&fixture.session).unwrap();
    let record = inner.session_mut_by_index(index).unwrap();
    let turn_generation = record.active_turn_generation.wrapping_add(1).max(1);
    let delivery = record.prompt_deliveries.get_mut(&id).unwrap();
    delivery.disposition = PromptDeliveryDisposition::HandingOff;
    delivery.attempt = 1;
    delivery.write_ahead_acknowledged = true;
    delivery.turn_generation = Some(turn_generation);
    fixture.state.commit_locked(&mut inner).unwrap();
    (id, turn_generation)
}

/// A dispatch held before its hand-off and dropped (a park for a retry, a
/// hold for recovery or unknown persistence) sent nothing: the receipt of the
/// head it promoted, still queued, returns to pending through the ordinary
/// still-queued outcome, so a fresh Resume can send it.
#[test]
fn a_dispatch_held_before_its_hand_off_returns_the_receipt_to_pending() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "held-before-hand-off");
    let (id, turn_generation) = promoted_retained_receipt(&fixture, &writer);
    let nonce = fixture.delivery(&id).unwrap().write_ahead;
    note_held_before_hand_off(&fixture.state, &fixture.session, turn_generation, None);
    let delivery = fixture.delivery(&id).unwrap();
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Pending);
    assert_eq!(delivery.turn_generation, None);
    assert_ne!(delivery.write_ahead, nonce, "a superseding write");
    assert!(delivery.resume.is_none());
    assert_eq!(fixture.queue_ids(), vec![id]);

    assert_preparing(&fixture);
    writer.take(HeldFence::WriteAhead).finish(Ok(()));
    assert_eq!(
        fixture.received_prompt("the head after a fresh Resume").text,
        "promoted, then held before its hand-off"
    );
    assert_eq!(fixture.delivered_prompts(), 0, "sent once");
}

/// A Stop of a retained admission that never handed its head off returns
/// that head's receipt to pending in the Stop's own write.
#[test]
fn a_stop_of_a_retained_admission_returns_the_receipt_to_pending() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "stop-retained-admission");
    let (id, _turn_generation) = promoted_retained_receipt(&fixture, &writer);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        inner.session_mut_by_index(index).unwrap().engram.admission_in_progress =
            Some(Arc::new(AtomicBool::new(true)));
    }
    assert!(fixture.state.stop_waiting_engram_admission(&fixture.session).unwrap());
    let delivery = fixture.delivery(&id).unwrap();
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Pending);
    assert_eq!(delivery.turn_generation, None);
    assert!(delivery.resume.is_none());
    assert!(queue_paused(&fixture));
    assert_eq!(fixture.delivered_prompts(), 0);
}

/// Runs `change` on the session's record under the state lock and commits.
fn with_record<T>(fixture: &Fixture, change: impl FnOnce(&mut SessionRecord) -> T) -> T {
    let mut inner = fixture.state.inner.lock().unwrap();
    let index = inner.find_session_index(&fixture.session).unwrap();
    let out = change(inner.session_mut_by_index(index).unwrap());
    fixture.state.commit_locked(&mut inner).unwrap();
    out
}

/// The receipt as a restart reads it back from the session's saved row.
fn reloaded_delivery(fixture: &Fixture, prompt_id: &str) -> PromptDelivery {
    let inner = fixture.state.inner.lock().unwrap();
    let index = inner.find_session_index(&fixture.session).unwrap();
    PersistedSessionRecord::from_record(&inner.sessions[index])
        .into_record()
        .expect("the row loads")
        .prompt_deliveries[prompt_id]
        .clone()
}

/// A deferred hand-off of `turn_generation` to the fixture's runtime, as a
/// Stop in progress defers one (`api.rs`).
fn deferred_hand_off(fixture: &Fixture, turn_generation: u64, text: &str) -> DeferredEngramHandoff {
    DeferredEngramHandoff(Arc::new(Mutex::new(Some(TurnDispatch::PersistentClaude {
        federation_handoff: None,
        active_turn_generation: turn_generation,
        opening_prompt: None,
        command: ClaudePromptCommand {
            attachments: Vec::new(),
            replay_generation: Uuid::new_v4().to_string(),
            text: text.to_owned(),
            turn_generation,
        },
        engram_dispatch_generation: None,
        mailbox_notification: None,
        runtime_token: fixture.token.clone(),
        sender: fixture.runtime_input.clone(),
        session_id: fixture.session.clone(),
    }))))
}

fn defer(fixture: &Fixture, turn_generation: u64, carrier: &DeferredEngramHandoff) {
    with_record(fixture, |record| {
        record.deferred_stop_callbacks.push(DeferredStopCallback::EngramHandoff {
            active_turn_generation: turn_generation,
            dispatch: carrier.clone(),
        })
    });
}

fn remove_by_cancel(fixture: &Fixture, id: &str) {
    fixture.state.cancel_queued_prompt(&fixture.session, id).unwrap();
}

fn remove_by_source(fixture: &Fixture, _id: &str) {
    with_record(fixture, |record| {
        clear_queued_prompts_by_source(record, QueuedPromptSource::Orchestrator)
    });
}

fn remove_without_admission_owner(fixture: &Fixture, _id: &str) {
    with_record(fixture, |record| {
        clear_queued_prompts_by_source_except_admission_owner(
            record,
            QueuedPromptSource::Orchestrator,
            None,
        )
    });
}

fn remove_as_retired_promotion(fixture: &Fixture, _id: &str) {
    with_record(fixture, |record| {
        let head = record.queued_prompts.front_mut().unwrap();
        head.promoted_message_index = Some(0);
        let fingerprint = engram_turn_intent_fingerprint(
            &head.pending_prompt.text,
            head.pending_prompt.expanded_text.as_deref(),
            &head.attachments,
            head.pending_prompt.source.as_ref(),
            head.source,
        );
        retire_promoted_engram_head(record, &fingerprint);
    });
}

fn remove_by_child_detach(fixture: &Fixture, _id: &str) {
    let mut inner = fixture.state.inner.lock().unwrap();
    detach_delegation_child_runtime_locked(&mut inner, &fixture.session, None);
    fixture.state.commit_locked(&mut inner).unwrap();
}

/// Retirement wins: a head promoted for its hand-off and removed from the
/// queue before it was handed off was never sent, so every removal settles
/// its receipt as rejected in the removal's own write, and that terminal
/// receipt is what a restart reads back (never an ambiguous hand-off).
#[test]
fn a_removed_head_promoted_for_its_hand_off_is_rejected_durably_and_never_sent() {
    let removals: [(&str, fn(&Fixture, &str)); 5] = [
        ("cancel", remove_by_cancel),
        ("clear-by-source", remove_by_source),
        ("clear-without-owner", remove_without_admission_owner),
        ("retire-promoted", remove_as_retired_promotion),
        ("child-detach", remove_by_child_detach),
    ];
    for (label, remove) in removals {
        let mut state = test_app_state();
        let writer = DeliveryWriter::attach(&mut state);
        let fixture = Fixture::new(state, &format!("retired-{label}"));
        let (id, turn_generation) = promoted_retained_receipt(&fixture, &writer);
        remove(&fixture, &id);
        assert!(!fixture.queue_ids().contains(&id), "{label}: removed");
        let delivery = fixture.delivery(&id).unwrap();
        assert_eq!(delivery.disposition, PromptDeliveryDisposition::Rejected, "{label}");
        assert_eq!(delivery.turn_generation, Some(turn_generation), "{label}");
        assert_eq!(
            reloaded_delivery(&fixture, &id).disposition,
            PromptDeliveryDisposition::Rejected,
            "{label}: the terminal receipt survives a restart"
        );
        assert_eq!(fixture.delivered_prompts(), 0, "{label}: never sent");
    }
}

/// Hand-off wins: a head the hand-off already took out of the queue keeps
/// its handing-off receipt through every later removal or discard; nothing
/// infers a rejection for a prompt that may have reached the runtime.
#[test]
fn a_head_already_handed_off_keeps_its_receipt_through_every_removal() {
    let host = DurableHost::new();
    let fixture = Fixture::new(host.boot(), "hand-off-wins");
    let (id, _, turn_generation) = hand_off_durably(&fixture, "handed off before the removals");
    assert!(fixture.queue_ids().is_empty());
    assert!(fixture.state.cancel_queued_prompt(&fixture.session, &id).is_err());
    remove_by_source(&fixture, &id);
    remove_without_admission_owner(&fixture, &id);
    // A carrier the hand-off emptied belongs to that hand-off.
    let emptied = DeferredEngramHandoff(Arc::new(Mutex::new(None)));
    defer(&fixture, turn_generation, &emptied);
    assert!(!with_record(&fixture, discard_deferred_stop_callbacks));
    let delivery = fixture.delivery(&id).unwrap();
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::HandingOff);
    assert_eq!(delivery.turn_generation, Some(turn_generation));
    fixture.state.shutdown_persist_blocking();
    host.finish(fixture);
}

/// A deferred hand-off keeps its head's receipt handing off while it is
/// live; discarding it takes the dispatch from its carrier, so it settles the
/// receipt exactly once (pending, its head still queued) and the runtime
/// never receives it.
#[test]
fn a_deferred_hand_off_stays_live_until_its_discard_settles_it_once() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "deferred-discard");
    let (id, turn_generation) = promoted_retained_receipt(&fixture, &writer);
    let carrier = deferred_hand_off(&fixture, turn_generation, "deferred, then discarded");
    defer(&fixture, turn_generation, &carrier);
    assert_eq!(fixture.delivery(&id).unwrap().disposition, PromptDeliveryDisposition::HandingOff);

    assert!(with_record(&fixture, discard_deferred_stop_callbacks));
    assert!(carrier.0.lock().unwrap().is_none(), "the discard took the dispatch");
    let delivery = fixture.delivery(&id).unwrap();
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Pending);
    assert_eq!(delivery.turn_generation, None);
    assert_eq!(fixture.queue_ids(), vec![id.clone()]);

    // A second discard of the same carrier settles nothing more.
    defer(&fixture, turn_generation, &carrier);
    assert!(!with_record(&fixture, discard_deferred_stop_callbacks));
    assert_eq!(fixture.delivery(&id).unwrap().write_ahead, delivery.write_ahead, "settled once");
    assert_eq!(fixture.delivered_prompts(), 0, "never sent");
}

/// A direct hand-off of a prepared dispatch (a deferred one replayed, or a
/// parked one released) that never reached the runtime, superseded or
/// rejected before its channel send, settles its receipt as the ordinary
/// delivery does; one that reached the runtime or was deferred again keeps
/// it.
#[test]
fn a_direct_hand_off_that_did_not_reach_the_runtime_settles_its_receipt() {
    let outcomes: [(&str, std::result::Result<HandoffPreparedTurnDispatchOutcome, ApiError>); 2] = [
        ("superseded", Ok(HandoffPreparedTurnDispatchOutcome::Superseded)),
        ("rejected", Err(ApiError::internal("failed to queue prompt for Claude session"))),
    ];
    for (label, outcome) in outcomes {
        let mut state = test_app_state();
        let writer = DeliveryWriter::attach(&mut state);
        let fixture = Fixture::new(state, &format!("direct-hand-off-{label}"));
        let (id, turn_generation) = promoted_retained_receipt(&fixture, &writer);
        fixture
            .state
            .note_direct_hand_off_outcome(&fixture.session, turn_generation, &outcome);
        let delivery = fixture.delivery(&id).unwrap();
        assert_eq!(delivery.disposition, PromptDeliveryDisposition::Pending, "{label}");
        assert_eq!(delivery.turn_generation, None, "{label}");
        assert!(delivery.resume.is_none(), "{label}");
    }
    for (label, outcome) in [
        ("delivered", Ok(HandoffPreparedTurnDispatchOutcome::Delivered)),
        ("scheduled", Ok(HandoffPreparedTurnDispatchOutcome::Scheduled)),
    ] {
        let mut state = test_app_state();
        let writer = DeliveryWriter::attach(&mut state);
        let fixture = Fixture::new(state, &format!("direct-hand-off-{label}"));
        let (id, turn_generation) = promoted_retained_receipt(&fixture, &writer);
        fixture
            .state
            .note_direct_hand_off_outcome(&fixture.session, turn_generation, &outcome);
        assert_eq!(
            fixture.delivery(&id).unwrap().disposition,
            PromptDeliveryDisposition::HandingOff,
            "{label}"
        );
    }
}

/// A discard or a direct hand-off outcome of another turn leaves a receipt
/// alone, and one the runtime already accepted is never settled again.
#[test]
fn a_stale_turn_or_an_accepted_receipt_is_left_untouched() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "stale-or-accepted");
    let (id, turn_generation) = promoted_retained_receipt(&fixture, &writer);
    let stale = turn_generation.wrapping_add(7);
    let carrier = deferred_hand_off(&fixture, stale, "a stale turn's hand-off");
    defer(&fixture, stale, &carrier);
    assert!(!with_record(&fixture, discard_deferred_stop_callbacks));
    fixture.state.note_direct_hand_off_outcome(
        &fixture.session,
        stale,
        &Ok(HandoffPreparedTurnDispatchOutcome::Superseded),
    );
    assert_eq!(fixture.delivery(&id).unwrap().disposition, PromptDeliveryDisposition::HandingOff);

    fixture.state.note_prompt_delivery_outcome(
        &fixture.session,
        Some(turn_generation),
        PromptDeliveryOutcome::Accepted,
    );
    let carrier = deferred_hand_off(&fixture, turn_generation, "after the acceptance");
    defer(&fixture, turn_generation, &carrier);
    assert!(!with_record(&fixture, discard_deferred_stop_callbacks));
    fixture.state.note_direct_hand_off_outcome(
        &fixture.session,
        turn_generation,
        &Err(ApiError::internal("failed to queue prompt for Claude session")),
    );
    remove_by_source(&fixture, &id);
    assert_eq!(fixture.delivery(&id).unwrap().disposition, PromptDeliveryDisposition::Accepted);
    assert_eq!(fixture.delivered_prompts(), 0);
}

/// Makes a receipted prompt durable while `session` is busy, so its
/// acknowledgement dispatches nothing: it stays pending, attempt 0.
fn durable_pending_head_while_busy(
    state: &AppState,
    writer: &DeliveryWriter,
    session: &str,
    text: &str,
) -> String {
    let id = {
        let mut inner = state.inner.lock().unwrap();
        let prompt = PendingPrompt {
            engram_interrupted: false,
            is_engram_retained: false,
            attachments: Vec::new(),
            id: inner.next_message_id(),
            timestamp: stamp_now(),
            text: text.to_owned(),
            expanded_text: None,
            source: None,
        };
        let id = prompt.id.clone();
        let index = inner.find_session_index(session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.session.status = SessionStatus::Active;
        queue_receipted_orchestrator_prompt_on_record(record, prompt, Vec::new());
        state.commit_locked(&mut inner).unwrap();
        id
    };
    assert!(state.request_queued_prompt_durability(session, &id));
    writer.take(HeldFence::Durability).finish(Ok(()));
    let settled = phase_sync::PollGuard::new();
    loop {
        let acknowledged = {
            let inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(session).unwrap();
            inner.sessions[index]
                .queued_prompt_durability_holds
                .get(&id)
                .is_some_and(|hold| hold.acknowledged)
        };
        if acknowledged {
            break;
        }
        settled.wait("the hold's durability acknowledgement");
    }
    let inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(session).unwrap();
    let delivery = &inner.sessions[index].prompt_deliveries[&id];
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Pending);
    assert_eq!(delivery.attempt, 0, "a busy session begins no write-ahead");
    id
}

fn receipt_of(state: &AppState, session: &str, prompt_id: &str) -> PromptDelivery {
    let inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(session).unwrap();
    inner.sessions[index].prompt_deliveries[prompt_id].clone()
}

/// A successful Stop whose drain meets a pending receipted successor: the
/// Stop's single commit publishes its own deltas first, at its own revision,
/// with no write-ahead committed inside it; only then does the woken drain
/// begin the successor's write-ahead, and its acknowledgement hands the
/// successor off exactly once, with no further input.
#[test]
fn a_stop_wakes_its_receipted_successor_only_after_its_own_commit() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let session = test_session_id(&state, Agent::Cursor);
    let original_owner = phase_sync::ParkedProcess::spawn();
    let (original_input_tx, _original_input_rx) = mpsc::channel();
    let original = AcpRuntimeHandle {
        agent: AcpAgent::Cursor,
        runtime_id: "receipted-stop-original".to_owned(),
        input_tx: original_input_tx,
        #[cfg(windows)]
        process_tree: None,
        process: original_owner.process.clone(),
        turn_lifecycle: Arc::new((Mutex::new(false), Condvar::new())),
    };
    let successor_owner = phase_sync::ParkedProcess::spawn();
    let (successor_input_tx, successor_input_rx) = mpsc::channel();
    state.install_test_acp_runtime_override(
        AcpAgent::Cursor,
        AcpRuntimeHandle {
            agent: AcpAgent::Cursor,
            runtime_id: "receipted-stop-successor".to_owned(),
            input_tx: successor_input_tx,
            #[cfg(windows)]
            process_tree: None,
            process: successor_owner.process.clone(),
            turn_lifecycle: Arc::new((Mutex::new(false), Condvar::new())),
        },
    );
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Acp(original);
    }
    let id = durable_pending_head_while_busy(&state, &writer, &session, "the receipted successor");

    let mut delta_events = state.subscribe_delta_events();
    state
        .stop_session_with_options(
            &session,
            StopSessionOptions {
                dispatch_queued_prompts_on_success: true,
                pause_automatic_resumes_on_success: false,
                orchestrator_stop_instance_id: None,
            },
        )
        .unwrap();
    // The last commit so far is the write-ahead's; its acknowledgement is
    // still held, so nothing commits after it yet.
    let write_ahead_revision = state.snapshot().revision;
    let mut stop_revisions = Vec::new();
    while let Ok(payload) = delta_events.try_recv() {
        let event: DeltaEvent = serde_json::from_str(&payload).expect("delta should deserialize");
        if let DeltaEvent::MessageCreated {
            revision,
            message: Message::Text { text, .. },
            ..
        } = event
            && text == "Turn stopped by user."
        {
            stop_revisions.push(revision);
        }
    }
    assert_eq!(stop_revisions.len(), 1, "the Stop's message is published once");
    assert!(
        stop_revisions[0] < write_ahead_revision,
        "the Stop committed and published before the write-ahead began: {stop_revisions:?} vs {write_ahead_revision}"
    );

    // The woken drain began the successor's write-ahead after that commit.
    let write_ahead = writer.take(HeldFence::WriteAhead);
    let delivery = receipt_of(&state, &session, &id);
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::HandingOff);
    assert_eq!(delivery.attempt, 1);
    assert_eq!(delivery.turn_generation, None);
    assert!(successor_input_rx.try_recv().is_err(), "nothing before the acknowledgement");
    write_ahead.finish(Ok(()));
    match recv_within_guard(&successor_input_rx, "the successor's hand-off") {
        Ok(AcpRuntimeCommand::Prompt(_)) => {}
        Ok(_) => panic!("the successor runtime got another command first"),
        Err(error) => panic!("the successor was not handed off: {error:?}"),
    }
    let settled = phase_sync::PollGuard::new();
    while receipt_of(&state, &session, &id).turn_generation.is_none() {
        settled.wait("the successor's promotion");
    }
    assert!(
        !successor_input_rx.try_iter().any(|command| matches!(command, AcpRuntimeCommand::Prompt(_))),
        "handed off once"
    );
    drop(original_owner);
    drop(successor_owner);
}

/// A direct send to an idle session whose head is a pending receipted
/// prompt queues behind it; the send's committed enqueue wakes the ordinary
/// drain, which begins the head's write-ahead, and its acknowledgement hands
/// the head off with no further input.
#[test]
fn a_direct_send_behind_a_pending_receipted_head_wakes_its_hand_off() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "direct-send-wake");
    let id = durable_pending_head_while_busy(
        &fixture.state,
        &writer,
        &fixture.session,
        "the receipted head",
    );
    with_record(&fixture, |record| record.session.status = SessionStatus::Idle);
    let result = fixture.state.dispatch_turn(
        &fixture.session,
        SendMessageRequest {
            text: "a direct send behind it".to_owned(),
            expanded_text: None,
            attachments: Vec::new(),
            source_session_id: None,
            source_mailbox: None,
        },
    );
    assert!(matches!(result, Ok(DispatchTurnResult::Queued)), "it queues behind the head");
    writer.take(HeldFence::WriteAhead).finish(Ok(()));
    assert_eq!(fixture.received_prompt("the head's hand-off").text, "the receipted head");
    fixture.wait_for(&id, "the promotion", |delivery| delivery.turn_generation.is_some());
    assert_eq!(fixture.delivered_prompts(), 0, "the direct send waits behind the running head");
}

/// A Stop that retires the runtime a receipted prompt was handed to, before
/// that runtime acknowledged it, settles the attempt as ambiguous in its own
/// write, durably, and nothing replays it; an accepted receipt stays
/// accepted.
#[test]
fn a_stop_after_the_hand_off_settles_the_unacknowledged_attempt_ambiguous() {
    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "stop-after-hand-off");
    let id = hand_off_through_held_fences(&fixture, &writer, "handed off, then stopped");
    let _ = fixture.received_prompt("the hand-off");
    fixture.wait_for(&id, "the promotion", |delivery| delivery.turn_generation.is_some());
    fixture.state.stop_session(&fixture.session).unwrap();
    let delivery = fixture.delivery(&id).unwrap();
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Ambiguous);
    assert_eq!(
        reloaded_delivery(&fixture, &id).disposition,
        PromptDeliveryDisposition::Ambiguous,
        "the ambiguity survives a restart"
    );
    assert!(fixture.queue_ids().is_empty());
    assert!(fixture.state.dispatch_next_queued_turn(&fixture.session, true).unwrap().is_none());
    assert_eq!(fixture.delivered_prompts(), 0, "never replayed");

    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let fixture = Fixture::new(state, "stop-after-acceptance");
    let id = hand_off_through_held_fences(&fixture, &writer, "accepted, then stopped");
    let command = fixture.received_prompt("the hand-off");
    fixture.wait_for(&id, "the promotion", |delivery| delivery.turn_generation.is_some());
    fixture.state.note_prompt_delivery_outcome(
        &fixture.session,
        Some(command.turn_generation),
        PromptDeliveryOutcome::Accepted,
    );
    fixture.state.stop_session(&fixture.session).unwrap();
    assert_eq!(fixture.delivery(&id).unwrap().disposition, PromptDeliveryDisposition::Accepted);
}

/// The retired runtime's unacknowledged attempt is ambiguous even while its
/// retained entry is still queued: queue membership does not prove nothing
/// was sent, so the entry leaves the queue in the same write and no drain,
/// Resume or reload sends it. A deferred hand-off of that attempt, still
/// held by its carrier, is instead proven unsent: it returns to pending.
#[test]
fn a_retired_runtime_leaves_a_queued_unacknowledged_attempt_ambiguous_unless_proven_unsent() {
    for proven_unsent in [false, true] {
        let label = if proven_unsent { "carried" } else { "uncertain" };
        let mut state = test_app_state();
        let writer = DeliveryWriter::attach(&mut state);
        let fixture = Fixture::new(state, &format!("retired-queued-{label}"));
        let (id, turn_generation) = promoted_retained_receipt(&fixture, &writer);
        with_record(&fixture, |record| {
            record.active_turn_generation = turn_generation;
            record.session.status = SessionStatus::Active;
        });
        let carrier = deferred_hand_off(&fixture, turn_generation, "the carried hand-off");
        if proven_unsent {
            defer(&fixture, turn_generation, &carrier);
        }
        // A Stop that keeps the queue's workflow prompts, so the entry leaves
        // the queue only through the settlement this test is about.
        fixture
            .state
            .stop_session_with_options(
                &fixture.session,
                StopSessionOptions {
                    dispatch_queued_prompts_on_success: false,
                    pause_automatic_resumes_on_success: false,
                    orchestrator_stop_instance_id: None,
                },
            )
            .unwrap();
        let delivery = fixture.delivery(&id).unwrap();
        if proven_unsent {
            assert_eq!(delivery.disposition, PromptDeliveryDisposition::Pending, "{label}");
            assert_eq!(fixture.queue_ids(), vec![id.clone()], "{label}");
        } else {
            assert_eq!(delivery.disposition, PromptDeliveryDisposition::Ambiguous, "{label}");
            assert!(!fixture.queue_ids().contains(&id), "{label}: the entry left the queue");
            assert_eq!(
                reloaded_delivery(&fixture, &id).disposition,
                PromptDeliveryDisposition::Ambiguous,
                "{label}"
            );
            assert!(
                fixture.state.resume_local_session_queue(&fixture.session, None).is_ok(),
                "{label}"
            );
            assert!(
                fixture.state.dispatch_next_queued_turn(&fixture.session, true).unwrap().is_none(),
                "{label}"
            );
        }
        assert_eq!(fixture.delivered_prompts(), 0, "{label}: never sent");
    }
}

/// A Stop whose own commit fails starts no write-ahead for the receipted
/// successor its promotion refused, and sends nothing.
#[test]
fn a_failed_stop_commit_wakes_no_receipted_successor() {
    let mut state = test_app_state();
    let session = test_session_id(&state, Agent::Cursor);
    let original_owner = phase_sync::ParkedProcess::spawn();
    let (original_input_tx, _original_input_rx) = mpsc::channel();
    let successor_owner = phase_sync::ParkedProcess::spawn();
    let (successor_input_tx, successor_input_rx) = mpsc::channel();
    state.install_test_acp_runtime_override(
        AcpAgent::Cursor,
        AcpRuntimeHandle {
            agent: AcpAgent::Cursor,
            runtime_id: "failed-stop-successor".to_owned(),
            input_tx: successor_input_tx,
            #[cfg(windows)]
            process_tree: None,
            process: successor_owner.process.clone(),
            turn_lifecycle: Arc::new((Mutex::new(false), Condvar::new())),
        },
    );
    let id = {
        let writer = DeliveryWriter::attach(&mut state);
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].runtime = SessionRuntime::Acp(AcpRuntimeHandle {
                agent: AcpAgent::Cursor,
                runtime_id: "failed-stop-original".to_owned(),
                input_tx: original_input_tx,
                #[cfg(windows)]
                process_tree: None,
                process: original_owner.process.clone(),
                turn_lifecycle: Arc::new((Mutex::new(false), Condvar::new())),
            });
        }
        durable_pending_head_while_busy(&state, &writer, &session, "the refused successor")
    };
    // Every commit from here on fails: no writer, and a store path that is a
    // directory.
    let failing = test_temp_dir().join(format!("termal-receipted-stop-failure-{}", Uuid::new_v4()));
    fs::create_dir_all(&failing).unwrap();
    state.shutdown_persist_blocking();
    state.persistence_path = Arc::new(failing.clone());
    let result = state.stop_session_with_options(
        &session,
        StopSessionOptions {
            dispatch_queued_prompts_on_success: true,
            pause_automatic_resumes_on_success: false,
            orchestrator_stop_instance_id: None,
        },
    );
    assert!(result.is_err(), "the Stop's commit fails");
    let delivery = receipt_of(&state, &session, &id);
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Pending);
    assert_eq!(delivery.attempt, 0, "no write-ahead escaped the failed Stop");
    assert!(successor_input_rx.try_recv().is_err(), "nothing sent");
    drop(original_owner);
    drop(successor_owner);
    let _ = fs::remove_dir_all(&failing);
}

/// A runtime exit or a turn failure that arrives while a Stop carries a
/// deferred hand-off of the same attempt is the Stop's to settle: it leaves
/// the attempt handing off, still queued, and the Stop's discard of its
/// carrier then proves it unsent, so it returns to pending, never ambiguous.
#[test]
fn a_runtime_exit_or_failure_during_a_stop_leaves_its_carried_hand_off_unsent() {
    for exited in [true, false] {
        let label = if exited { "exit" } else { "failure" };
        let mut state = test_app_state();
        let writer = DeliveryWriter::attach(&mut state);
        let fixture = Fixture::new(state, &format!("carried-during-{label}"));
        let (id, turn_generation) = promoted_retained_receipt(&fixture, &writer);
        with_record(&fixture, |record| {
            record.active_turn_generation = turn_generation;
            record.session.status = SessionStatus::Active;
            record.runtime_stop_in_progress = true;
        });
        let carrier = deferred_hand_off(&fixture, turn_generation, "carried by the Stop");
        defer(&fixture, turn_generation, &carrier);
        if exited {
            fixture
                .state
                .handle_runtime_exit_if_matches(&fixture.session, &fixture.token, Some("gone"))
                .unwrap();
        } else {
            fixture
                .state
                .fail_turn_if_runtime_matches(&fixture.session, &fixture.token, "failed")
                .unwrap();
        }
        let delivery = fixture.delivery(&id).unwrap();
        assert_eq!(delivery.disposition, PromptDeliveryDisposition::HandingOff, "{label}");
        assert_eq!(fixture.queue_ids(), vec![id.clone()], "{label}");
        // The Stop's own retirement takes its carrier: proven unsent.
        assert!(with_record(&fixture, |record| {
            record.runtime_stop_in_progress = false;
            discard_deferred_stop_callbacks(record)
        }));
        assert_eq!(
            fixture.delivery(&id).unwrap().disposition,
            PromptDeliveryDisposition::Pending,
            "{label}"
        );
        assert_eq!(fixture.queue_ids(), vec![id.clone()], "{label}");
        assert_eq!(fixture.delivered_prompts(), 0, "{label}: never sent");
    }
}

/// A re-queue of a rejected receipt whose commit fails restores the receipt
/// as it was: its retry entry stays, and no tentative prompt, receipt or
/// durability hold is left without its acknowledgement request.
#[test]
fn a_failed_requeue_commit_leaves_the_rejected_receipt_retryable() {
    let host = DurableHost::new();
    let mut fixture = Fixture::new(host.boot(), "requeue-commit-failure");
    let (id, _, generation) = hand_off_durably(&fixture, "rejected, then re-queued");
    fixture.state.note_prompt_delivery_outcome(
        &fixture.session,
        Some(generation),
        PromptDeliveryOutcome::Rejected("the runtime was gone".to_owned()),
    );
    assert!(fixture.delivery(&id).unwrap().entry.is_some());
    let holds_before = with_record(&fixture, |record| {
        record.queued_prompt_durability_holds.keys().cloned().collect::<BTreeSet<_>>()
    });
    // Every commit from here on fails: no writer, and a store path that is a
    // directory.
    let failing =
        test_temp_dir().join(format!("termal-receipted-requeue-failure-{}", Uuid::new_v4()));
    fs::create_dir_all(&failing).unwrap();
    fixture.state.shutdown_persist_blocking();
    fixture.state.persistence_path = Arc::new(failing.clone());
    assert!(fixture.state.requeue_rejected_prompt_delivery(&fixture.session, &id).is_err());
    let delivery = fixture.delivery(&id).unwrap();
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Rejected);
    assert!(delivery.entry.is_some(), "the retry entry stays");
    assert!(fixture.queue_ids().is_empty(), "no tentative prompt");
    let inner = fixture.state.inner.lock().unwrap();
    let index = inner.find_session_index(&fixture.session).unwrap();
    assert_eq!(inner.sessions[index].prompt_deliveries.len(), 1, "no tentative receipt");
    assert_eq!(
        inner.sessions[index]
            .queued_prompt_durability_holds
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        holds_before,
        "no hold without its acknowledgement request"
    );
    drop(inner);
    host.finish(fixture);
    let _ = fs::remove_dir_all(&failing);
}

/// A shared Codex thread setup that fails drops the prompt parked on it:
/// turn/start was never written, so the receipt is rejected (its retry entry
/// kept) before the turn ends on the runtime the session keeps, and a
/// restart reads it back rejected, never ambiguous.
#[test]
fn a_failed_codex_thread_setup_rejects_its_parked_receipted_prompt() {
    for (label, error) in [
        ("refused", CodexResponseError::JsonRpc("thread/start refused".to_owned())),
        ("timed out", CodexResponseError::Timeout("no thread/start reply".to_owned())),
    ] {
        let state = test_app_state();
        let generation = 7;
        let (session_id, runtime, prompt_id) = codex_handing_off(&state, generation);
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session_id).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            let delivery = record.prompt_deliveries.get_mut(&prompt_id).unwrap();
            delivery.entry = Some(QueuedPromptRecord {
                engram_ordinary_handoff: false,
                engram_waiting: false,
                promoted_message_index: None,
                promotion_disposition_known: true,
                engram_bind: None,
                engram_evaluate: None,
                engram_interrupted: false,
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    engram_interrupted: false,
                    is_engram_retained: false,
                    attachments: Vec::new(),
                    id: prompt_id.clone(),
                    timestamp: stamp_now(),
                    text: "a receipted Codex prompt".to_owned(),
                    expanded_text: None,
                    source: None,
                },
            });
        }
        let mut setup = super::shared_codex_thread_setup::test_pending_codex_thread_setup(
            "receipted-setup",
        );
        setup.command.active_turn_generation = generation;
        runtime
            .sessions
            .lock()
            .unwrap()
            .get_mut(&session_id)
            .unwrap()
            .pending_thread_setup = Some(setup);
        handle_shared_codex_thread_setup_response_error_if_current(
            &runtime.sessions,
            &state,
            &runtime.runtime_id,
            &session_id,
            "receipted-setup",
            Duration::from_secs(180),
            error,
        );
        let delivery = codex_delivery(&state, &session_id, &prompt_id);
        assert_eq!(delivery.disposition, PromptDeliveryDisposition::Rejected, "{label}");
        assert!(delivery.entry.is_some(), "{label}: the retry entry stays");
        let reloaded = {
            let inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session_id).unwrap();
            PersistedSessionRecord::from_record(&inner.sessions[index])
                .into_record()
                .expect("the row loads")
                .prompt_deliveries[&prompt_id]
                .disposition
        };
        assert_eq!(reloaded, PromptDeliveryDisposition::Rejected, "{label}");
    }
}

/// A shared Codex thread setup whose `thread/start` request fails to be
/// written never reached Codex, so neither did the prompt parked on it: the
/// receipt is rejected as the setup is released, before the writer's error
/// retires the shared runtime, and that retirement leaves it rejected with
/// its retry entry, so the producer can re-queue it.
#[test]
fn a_codex_thread_setup_whose_request_was_never_written_rejects_its_parked_receipted_prompt() {
    struct FailingWriter;
    impl std::io::Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "codex stdin closed"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut state = test_app_state();
    let writer = DeliveryWriter::attach(&mut state);
    let generation = 7;
    let (session_id, runtime, prompt_id) = codex_handing_off(&state, generation);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session_id).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.prompt_deliveries.get_mut(&prompt_id).unwrap().entry = Some(QueuedPromptRecord {
            engram_ordinary_handoff: false,
            engram_waiting: false,
            promoted_message_index: None,
            promotion_disposition_known: true,
            engram_bind: None,
            engram_evaluate: None,
            engram_interrupted: false,
            source: QueuedPromptSource::Orchestrator,
            attachments: Vec::new(),
            pending_prompt: PendingPrompt {
                engram_interrupted: false,
                is_engram_retained: false,
                attachments: Vec::new(),
                id: prompt_id.clone(),
                timestamp: stamp_now(),
                text: "a receipted Codex prompt".to_owned(),
                expanded_text: None,
                source: None,
            },
        });
    }
    // The session has no thread yet, so the prompt needs a thread setup.
    runtime.sessions.lock().unwrap().get_mut(&session_id).unwrap().thread_id = None;
    runtime.thread_sessions.lock().unwrap().clear();

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let (input_tx, _input_rx) = mpsc::channel::<CodexRuntimeCommand>();
    let result = handle_shared_codex_prompt_command(
        &mut FailingWriter,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &test_missing_shared_codex_home(),
        &runtime.sessions,
        &runtime.thread_sessions,
        &input_tx,
        None,
        &session_id,
        codex_prompt(generation),
    );
    assert!(result.is_err(), "the failed write reaches the writer loop");
    // What the writer loop does with that error: it retires the runtime.
    let _ = state.handle_shared_codex_runtime_exit(&runtime.runtime_id, Some("codex stdin closed"));

    let delivery = codex_delivery(&state, &session_id, &prompt_id);
    assert_eq!(
        delivery.disposition,
        PromptDeliveryDisposition::Rejected,
        "thread/start was never written: {:?}",
        delivery.reason
    );
    assert!(delivery.entry.is_some(), "the retry entry stays");
    let new_id = state
        .requeue_rejected_prompt_delivery(&session_id, &prompt_id)
        .expect("the rejected prompt re-queues");
    assert_ne!(new_id, prompt_id);
    drop(writer);
}
