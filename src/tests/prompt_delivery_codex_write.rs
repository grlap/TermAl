// Owns the tests of when a receipted prompt promoted on a Codex session
// counts as written to Codex: only the turn/start request carries the prompt,
// so a turn that fails before that write (a failed thread registration or
// runtime-config save) leaves the receipt rejected with its retry entry,
// never ambiguous, and a receipt settled before the write keeps the writer
// from writing it; a failed turn/start write stays ambiguous. Does not own
// the receipt's other paths (src/tests/prompt_delivery.rs) or the Codex
// protocol itself (src/tests/shared_codex*.rs). New module beside
// src/tests/prompt_delivery.rs, whose delivery writer it uses.
use super::*;
use super::prompt_delivery::{DeliveryWriter, HeldFence};

/// A receipted prompt promoted through the real queue on an idle Codex
/// session whose shared runtime is a test channel: the promotion's prompt
/// command, as the runtime's writer thread receives it.
struct PromotedCodexReceipt {
    state: AppState,
    session: String,
    runtime: SharedCodexRuntime,
    id: String,
    command: CodexPromptCommand,
    writer: Option<DeliveryWriter>,
    _write_ahead: Box<PersistFence>,
    _process: Arc<SharedChild>,
}

impl PromotedCodexReceipt {
    fn new(label: &str) -> Self {
        let mut state = test_app_state();
        let writer = DeliveryWriter::attach(&mut state);
        let session = test_session_id(&state, Agent::Codex);
        let (runtime, input_rx, process) = test_shared_codex_runtime(&format!("receipted-codex-{label}"));
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                runtime_id: runtime.runtime_id.clone(),
                input_tx: runtime.input_tx.clone(),
                process: process.clone(),
                shared_session: Some(SharedCodexSessionHandle {
                    runtime: runtime.clone(),
                    session_id: session.clone(),
                }),
            });
            record.session.status = SessionStatus::Idle;
        }
        let id = {
            let mut inner = state.inner.lock().unwrap();
            let prompt = PendingPrompt {
                engram_interrupted: false,
                is_engram_retained: false,
                attachments: Vec::new(),
                id: inner.next_message_id(),
                timestamp: stamp_now(),
                text: format!("a receipted Codex prompt ({label})"),
                expanded_text: None,
                source: None,
            };
            let id = prompt.id.clone();
            let index = inner.find_session_index(&session).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            queue_receipted_orchestrator_prompt_on_record(record, prompt, Vec::new());
            state.commit_locked(&mut inner).unwrap();
            id
        };
        assert!(state.request_queued_prompt_durability(&session, &id));
        writer.take(HeldFence::Durability).finish(Ok(()));
        let write_ahead = writer.take(HeldFence::WriteAhead);
        let nonce = Self::receipt_of(&state, &session, &id).write_ahead;
        // The acknowledgement promotes the head and hands its command to the
        // Codex runtime's writer thread.
        state.settle_prompt_delivery_write_ahead(&session, &id, &nonce, Ok(()));
        let command = match recv_within_guard(&input_rx, "the promoted prompt command") {
            Ok(CodexRuntimeCommand::Prompt { command, .. }) => command,
            Ok(_) => panic!("the runtime received another command first"),
            Err(error) => panic!("the promoted prompt command did not arrive: {error:?}"),
        };
        let delivery = Self::receipt_of(&state, &session, &id);
        assert_eq!(delivery.disposition, PromptDeliveryDisposition::HandingOff);
        assert_eq!(delivery.turn_generation, Some(command.active_turn_generation), "promoted");
        Self {
            state,
            session,
            runtime,
            id,
            command,
            writer: Some(writer),
            _write_ahead: write_ahead,
            _process: process,
        }
    }

    fn receipt_of(state: &AppState, session: &str, id: &str) -> PromptDelivery {
        let inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(session).unwrap();
        inner.sessions[index].prompt_deliveries[id].clone()
    }

    fn receipt(&self) -> PromptDelivery {
        Self::receipt_of(&self.state, &self.session, &self.id)
    }

    /// From here every save fails: the writer is gone, so commits fall back
    /// to a synchronous save into a directory. Returns that directory and the
    /// path to restore.
    fn fail_saves(&mut self, label: &str) -> (PathBuf, Arc<PathBuf>) {
        drop(self.writer.take());
        let failing = test_temp_dir().join(format!("termal-receipted-codex-{label}-{}", Uuid::new_v4()));
        fs::create_dir_all(&failing).expect("the failing persistence directory exists");
        let original = std::mem::replace(&mut self.state.persistence_path, Arc::new(failing.clone()));
        (failing, original)
    }

    /// Saves work again, under a fresh writer: the rejected prompt re-queues
    /// under a new id with a fresh durability prerequisite.
    fn assert_requeues(mut self, failing: PathBuf, original: Arc<PathBuf>) {
        self.state.persistence_path = original;
        let writer = DeliveryWriter::attach(&mut self.state);
        let new_id = self
            .state
            .requeue_rejected_prompt_delivery(&self.session, &self.id)
            .expect("the rejected prompt re-queues");
        assert_ne!(new_id, self.id);
        drop(writer.take(HeldFence::Durability));
        drop(writer);
        self.finish();
        // State clones taken while saves failed hold this path: it goes
        // only after all of them.
        remove_test_directory(failing);
    }

    /// Ends the test only after every thread it started (a reply waiter, a
    /// fence's settlement thread) has released the state.
    fn finish(self) {
        let Self { state, writer, _write_ahead, runtime, _process, .. } = self;
        drop(writer);
        drop(_write_ahead);
        drop(runtime);
        drop(_process);
        release_state(state);
    }
}

/// Drops `state` and waits for its temp root's removal receipt, which comes
/// only once the last clone of the state is gone.
fn release_state(state: AppState) {
    let cleaned = state
        .test_temp_root
        .as_ref()
        .expect("the test state owns its temp root")
        .observe_cleanup();
    drop(state);
    phase_sync::receive(&cleaned, "every clone of the test state is released")
        .expect("the test state's temp root is removed");
}

/// Answers a written turn/start with a refusal, so its reply waiter ends now.
fn refuse_turn_start(pending_requests: &CodexPendingRequestMap) {
    let (_request_id, reply) = take_pending_codex_request(pending_requests);
    reply
        .send(Err(CodexResponseError::JsonRpc("refused at the end of the test".to_owned())))
        .unwrap();
}

fn assert_rejected_with_retry_entry(delivery: &PromptDelivery, what: &str) {
    assert_eq!(
        delivery.disposition,
        PromptDeliveryDisposition::Rejected,
        "{what}: the prompt was not written to Codex: {:?}",
        delivery.reason
    );
    assert!(delivery.entry.is_some(), "{what}: the retry entry stays");
}

/// A runtime-config save that fails before turn/start fails the turn without
/// writing the prompt: the receipt is rejected, not ambiguous, and re-queues.
#[test]
fn a_codex_runtime_config_save_failure_before_turn_start_rejects_the_receipted_prompt() {
    let mut promoted = PromotedCodexReceipt::new("config-save");
    let (failing, original) = promoted.fail_saves("config-save");
    let mut wire = Vec::new();
    handle_shared_codex_start_turn(
        &mut wire,
        &Arc::new(Mutex::new(HashMap::new())),
        &promoted.state,
        &promoted.runtime.runtime_id,
        &promoted.runtime.sessions,
        &promoted.runtime.thread_sessions,
        None,
        &promoted.session,
        "thread-receipted",
        None,
        promoted.command.clone(),
    )
    .expect("a failed save fails the turn, not the runtime");
    assert!(!String::from_utf8_lossy(&wire).contains("turn/start"), "turn/start was never written");
    assert_rejected_with_retry_entry(&promoted.receipt(), "runtime-config save");
    promoted.assert_requeues(failing, original);
}

/// A thread registration save that fails after thread/start drops the
/// prompt parked on the setup before any turn/start: the receipt is
/// rejected, not ambiguous, and re-queues.
#[test]
fn a_codex_thread_registration_save_failure_rejects_the_receipted_prompt() {
    let mut promoted = PromotedCodexReceipt::new("registration-save");
    // Saves fail from before the prompt command, as in
    // shared_codex_thread_setup_persist_failure_does_not_tear_down_runtime.
    let (failing, original) = promoted.fail_saves("registration-save");
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let (input_tx, input_rx) = mpsc::channel::<CodexRuntimeCommand>();
    let mut wire = Vec::new();
    handle_shared_codex_prompt_command(
        &mut wire,
        &pending_requests,
        &promoted.state,
        &promoted.runtime.runtime_id,
        &test_missing_shared_codex_home(),
        &promoted.runtime.sessions,
        &promoted.runtime.thread_sessions,
        &input_tx,
        None,
        &promoted.session,
        promoted.command.clone(),
    )
    .expect("thread/start is written");
    assert!(String::from_utf8_lossy(&wire).contains("thread/start"), "the setup request is written");
    assert_eq!(pending_requests.lock().unwrap().len(), 1, "only the setup request is pending");
    let (_request_id, reply) = take_pending_codex_request(&pending_requests);
    reply
        .send(Ok(json!({ "thread": { "id": "conversation-registration-save" } })))
        .unwrap();
    // The setup's waiter takes the reply and ends the setup, whichever way.
    let setup_ended = phase_sync::PollGuard::new();
    while promoted
        .runtime
        .sessions
        .lock()
        .unwrap()
        .get(&promoted.session)
        .is_some_and(|session| session.pending_thread_setup.is_some())
    {
        setup_ended.wait("the setup's waiter ends the setup");
    }
    let settled = phase_sync::PollGuard::new();
    while promoted.receipt().disposition == PromptDeliveryDisposition::HandingOff {
        settled.wait("the failed registration settles the receipt");
    }
    assert!(
        !matches!(input_rx.try_recv(), Ok(CodexRuntimeCommand::StartTurnAfterSetup { .. })),
        "a failed registration starts no turn"
    );
    assert!(!String::from_utf8_lossy(&wire).contains("turn/start"), "turn/start was never written");
    assert_rejected_with_retry_entry(&promoted.receipt(), "thread registration save");
    promoted.assert_requeues(failing, original);
}

/// A receipt already settled before the writer reaches turn/start (here
/// rejected) is never written afterwards: rejected means not sent, so a
/// producer that re-queues it cannot send it twice.
#[test]
fn a_receipt_settled_before_the_write_keeps_codex_from_writing_turn_start() {
    let promoted = PromotedCodexReceipt::new("settled-before-write");
    promoted.state.note_prompt_delivery_outcome(
        &promoted.session,
        Some(promoted.command.active_turn_generation),
        PromptDeliveryOutcome::Rejected("settled before the write".to_owned()),
    );
    let mut wire = Vec::new();
    let _ = handle_shared_codex_start_turn(
        &mut wire,
        &Arc::new(Mutex::new(HashMap::new())),
        &promoted.state,
        &promoted.runtime.runtime_id,
        &promoted.runtime.sessions,
        &promoted.runtime.thread_sessions,
        None,
        &promoted.session,
        "thread-receipted",
        None,
        promoted.command.clone(),
    );
    assert!(
        !String::from_utf8_lossy(&wire).contains("turn/start"),
        "a rejected attempt is never written"
    );
    assert_eq!(promoted.receipt().disposition, PromptDeliveryDisposition::Rejected);
    promoted.finish();
}

/// The two orders of a failure racing the writer. Failure first: the turn
/// fails while the prompt is still unwritten, so the receipt is rejected and
/// the writer, arriving later, writes nothing. Writer first: once turn/start
/// is written, a later failure leaves the receipt ambiguous.
#[test]
fn a_failure_before_the_write_rejects_and_denies_the_writer_and_one_after_it_is_ambiguous() {
    // Failure wins.
    let promoted = PromotedCodexReceipt::new("failure-wins");
    let token = RuntimeToken::Codex(promoted.runtime.runtime_id.clone());
    promoted
        .state
        .fail_turn_if_runtime_matches(&promoted.session, &token, "the turn failed before the write")
        .expect("the failure settles");
    assert_rejected_with_retry_entry(&promoted.receipt(), "failure before the write");
    let mut wire = Vec::new();
    let _ = handle_shared_codex_start_turn(
        &mut wire,
        &Arc::new(Mutex::new(HashMap::new())),
        &promoted.state,
        &promoted.runtime.runtime_id,
        &promoted.runtime.sessions,
        &promoted.runtime.thread_sessions,
        None,
        &promoted.session,
        "thread-receipted",
        None,
        promoted.command.clone(),
    );
    assert!(!String::from_utf8_lossy(&wire).contains("turn/start"), "the writer is denied");
    assert_eq!(promoted.receipt().disposition, PromptDeliveryDisposition::Rejected);
    promoted.finish();

    // Writer wins.
    let promoted = PromotedCodexReceipt::new("writer-wins");
    let token = RuntimeToken::Codex(promoted.runtime.runtime_id.clone());
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut wire = Vec::new();
    handle_shared_codex_start_turn(
        &mut wire,
        &pending_requests,
        &promoted.state,
        &promoted.runtime.runtime_id,
        &promoted.runtime.sessions,
        &promoted.runtime.thread_sessions,
        None,
        &promoted.session,
        "thread-receipted",
        None,
        promoted.command.clone(),
    )
    .expect("turn/start is written");
    assert!(String::from_utf8_lossy(&wire).contains("turn/start"));
    promoted
        .state
        .fail_turn_if_runtime_matches(&promoted.session, &token, "the turn failed after the write")
        .expect("the failure settles");
    assert_eq!(promoted.receipt().disposition, PromptDeliveryDisposition::Ambiguous);
    refuse_turn_start(&pending_requests);
    promoted.finish();
}

/// A receipted command whose receipt row is gone (here removed) when the
/// writer reaches turn/start is not mistaken for an ordinary prompt: it
/// fails closed and writes nothing. A genuinely non-receipted prompt is
/// written as before.
#[test]
fn a_receipted_command_whose_receipt_is_gone_is_denied_and_an_ordinary_prompt_is_written() {
    let promoted = PromotedCodexReceipt::new("receipt-gone");
    {
        let mut inner = promoted.state.inner.lock().unwrap();
        let index = inner.find_session_index(&promoted.session).unwrap();
        inner.session_mut_by_index(index).unwrap().prompt_deliveries.remove(&promoted.id);
    }
    let mut wire = Vec::new();
    let _ = handle_shared_codex_start_turn(
        &mut wire,
        &Arc::new(Mutex::new(HashMap::new())),
        &promoted.state,
        &promoted.runtime.runtime_id,
        &promoted.runtime.sessions,
        &promoted.runtime.thread_sessions,
        None,
        &promoted.session,
        "thread-receipted",
        None,
        promoted.command.clone(),
    );
    assert!(
        !String::from_utf8_lossy(&wire).contains("turn/start"),
        "a receipted command fails closed without its receipt"
    );
    let mut command = promoted.command.clone();
    promoted.finish();

    // An ordinary prompt on a Codex session, never receipted.
    let state = test_app_state();
    let session = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) = test_shared_codex_runtime("ordinary-codex-prompt");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session.clone(),
            }),
        });
        record.session.status = SessionStatus::Active;
        record.active_turn_generation = 3;
    }
    command.active_turn_generation = 3;
    // Never receipted: it carries no attempt.
    command.delivery_receipt = None;
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
        &session,
        "thread-ordinary",
        None,
        command,
    )
    .expect("an ordinary prompt is written");
    assert!(String::from_utf8_lossy(&wire).contains("turn/start"), "written as before");
    refuse_turn_start(&pending_requests);
    drop((runtime, input_rx));
    release_state(state);
}

/// Once turn/start is written, a reply that never arrives leaves the
/// receipted prompt ambiguous.
#[test]
fn a_turn_start_reply_timeout_after_the_write_leaves_the_receipted_prompt_ambiguous() {
    let promoted = PromotedCodexReceipt::new("reply-timeout");
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut wire = Vec::new();
    handle_shared_codex_start_turn(
        &mut wire,
        &pending_requests,
        &promoted.state,
        &promoted.runtime.runtime_id,
        &promoted.runtime.sessions,
        &promoted.runtime.thread_sessions,
        None,
        &promoted.session,
        "thread-receipted",
        None,
        promoted.command.clone(),
    )
    .expect("turn/start is written");
    let (_request_id, reply) = take_pending_codex_request(&pending_requests);
    reply
        .send(Err(CodexResponseError::Timeout("no turn/start reply".to_owned())))
        .unwrap();
    let settled = phase_sync::PollGuard::new();
    while promoted.receipt().disposition == PromptDeliveryDisposition::HandingOff {
        settled.wait("the timed-out reply settles the receipt");
    }
    assert_eq!(promoted.receipt().disposition, PromptDeliveryDisposition::Ambiguous);
    promoted.finish();
}

/// Once the turn/start write has begun, part of the prompt may have reached
/// Codex: a failed write leaves the receipt ambiguous.
#[test]
fn a_failed_turn_start_write_leaves_the_receipted_prompt_ambiguous() {
    struct FailingWriter;
    impl std::io::Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "codex stdin closed"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let promoted = PromotedCodexReceipt::new("write-failure");
    let result = handle_shared_codex_start_turn(
        &mut FailingWriter,
        &Arc::new(Mutex::new(HashMap::new())),
        &promoted.state,
        &promoted.runtime.runtime_id,
        &promoted.runtime.sessions,
        &promoted.runtime.thread_sessions,
        None,
        &promoted.session,
        "thread-receipted",
        None,
        promoted.command.clone(),
    );
    assert!(result.is_err(), "the failed write reaches the writer loop");
    assert_eq!(promoted.receipt().disposition, PromptDeliveryDisposition::Ambiguous);
    promoted.finish();
}
