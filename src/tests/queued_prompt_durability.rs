// Owns the tests of the opt-in durability prerequisite of a queued prompt: no
// dispatch entry point promotes a held entry before the persistence
// acknowledgement of exactly its content, a held head blocks the queue behind
// it, a failed acknowledgement leaves it held with its reason, a late or
// mismatched acknowledgement releases nothing, a Resume of a paused queue
// whose head is held is refused and keeps the pause, an entry read back from the
// durable row after a restart is eligible, and prompts without a hold behave
// as before. Does not own the persist fence's other targets (persist_fence.rs
// tests) or any producer of held prompts. New module beside
// src/queued_prompt_durability.rs.
use super::delegation_support::boot_on_delegation_root;
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// A persist writer that writes nothing and acknowledges every fence at once,
/// except a queued prompt's durability fence: that one is handed to the test,
/// which decides how it resolves, standing for a writer still holding it.
struct HoldingWriter {
    tx: mpsc::Sender<PersistRequest>,
    held: mpsc::Receiver<(String, Box<PersistFence>)>,
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl HoldingWriter {
    fn attach(state: &mut AppState) -> Self {
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
                match &fence.target {
                    PersistFenceTarget::QueuedPromptDurability { prompt_id, .. } => {
                        let _ = held_tx.send((prompt_id.clone(), fence));
                    }
                    _ => fence.finish(Ok(())),
                }
            }
        });
        Self { tx, held, stop, thread: Mutex::new(Some(thread)) }
    }

    fn take(&self, what: &str) -> (String, Box<PersistFence>) {
        phase_sync::receive(&self.held, what)
    }
}

impl Drop for HoldingWriter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.tx.send(PersistRequest::Delta);
        if let Some(thread) = self.thread.lock().unwrap().take() {
            let _ = thread.join();
        }
    }
}

struct Fixture {
    state: AppState,
    session: String,
    prompts: mpsc::Receiver<ClaudeRuntimeCommand>,
    _root: TestTempRoot,
}

impl Fixture {
    /// An idle Claude session whose runtime hands every prompt to `prompts`.
    fn new(state: AppState, label: &str) -> Self {
        let root = TestTempRoot::create(&format!("queued-prompt-durability-{label}"));
        let (session, prompts) = {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let record = inner.create_session(
                Agent::Claude,
                Some(format!("Durability {label}")),
                root.path().to_string_lossy().into_owned(),
                None,
                None,
            );
            let session = record.session.id.clone();
            let index = inner.find_session_index(&session).unwrap();
            let (runtime, prompts) = test_claude_runtime_handle(&format!("durability-{label}"));
            let record = inner.session_mut_by_index(index).unwrap();
            record.runtime = SessionRuntime::Claude(runtime);
            record.session.status = SessionStatus::Idle;
            state.commit_locked(&mut inner).unwrap();
            (session, prompts)
        };
        Self { state, session, prompts, _root: root }
    }

    fn pending_prompt(&self, inner: &mut StateInner, text: &str) -> PendingPrompt {
        PendingPrompt {
            engram_interrupted: false,
            is_engram_retained: false,
            attachments: Vec::new(),
            id: inner.next_message_id(),
            timestamp: stamp_now(),
            text: text.to_owned(),
            expanded_text: None,
            source: None,
        }
    }

    /// Enqueues a held orchestrator prompt and commits it; returns its id.
    fn enqueue_held(&self, text: &str) -> String {
        let mut inner = self.state.inner.lock().expect("state mutex poisoned");
        let prompt = self.pending_prompt(&mut inner, text);
        let id = prompt.id.clone();
        let index = inner.find_session_index(&self.session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        queue_durable_orchestrator_prompt_on_record(record, prompt, Vec::new());
        self.state.commit_locked(&mut inner).unwrap();
        id
    }

    fn enqueue_user(&self, text: &str) -> String {
        let mut inner = self.state.inner.lock().expect("state mutex poisoned");
        let prompt = self.pending_prompt(&mut inner, text);
        let id = prompt.id.clone();
        let index = inner.find_session_index(&self.session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        queue_prompt_on_record(record, prompt, Vec::new());
        self.state.commit_locked(&mut inner).unwrap();
        id
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

    fn hold(&self, prompt_id: &str) -> Option<QueuedPromptDurabilityHold> {
        let inner = self.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&self.session).unwrap();
        inner.sessions[index]
            .queued_prompt_durability_holds
            .get(prompt_id)
            .cloned()
    }

    /// Every way the test can ask the host to promote the queue head: the
    /// automatic dispatch that turn ends, wait resumes and mailbox wakes use,
    /// its Resume/bypass variant, and the boot recovery of workflow prompts.
    fn drive_every_entry_point(&self) -> usize {
        let mut promoted = 0;
        for allow_blocked in [false, true] {
            if let Ok(Some(dispatch)) =
                self.state.dispatch_next_queued_turn(&self.session, allow_blocked)
            {
                promoted += 1;
                let _ = deliver_turn_dispatch(&self.state, dispatch);
            }
        }
        self.state.dispatch_orphaned_workflow_prompts();
        promoted
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
}

#[test]
fn a_held_prompt_is_promoted_by_no_entry_point_until_its_exact_acknowledgement() {
    let mut state = test_app_state();
    let writer = HoldingWriter::attach(&mut state);
    let fixture = Fixture::new(state, "barrier");
    let held = fixture.enqueue_held("resume after the gate");
    let behind = fixture.enqueue_user("a user prompt behind the held head");
    assert!(fixture.state.request_queued_prompt_durability(&fixture.session, &held));
    let (fenced, fence) = writer.take("the held prompt's durability fence");
    assert_eq!(fenced, held);

    assert_eq!(fixture.drive_every_entry_point(), 0, "nothing promotes a held head");
    assert_eq!(fixture.delivered_prompts(), 0, "nothing reached the runtime");
    assert_eq!(
        fixture.queue_ids(),
        vec![held.clone(), behind.clone()],
        "the held head blocks the queue behind it"
    );

    // The exact acknowledgement releases it, and the release dispatches it.
    fence.finish(Ok(()));
    match recv_within_guard(&fixture.prompts, "the released prompt reaches the runtime") {
        Ok(ClaudeRuntimeCommand::Prompt(command)) => {
            assert_eq!(command.text, "resume after the gate")
        }
        Ok(_) => panic!("expected the released prompt, got another runtime command"),
        Err(error) => panic!("expected the released prompt, got {error:?}"),
    }
    assert_eq!(fixture.hold(&held), None);
    assert_eq!(fixture.queue_ids(), vec![behind], "one promotion, the rest still queued");
}

#[test]
fn a_failed_acknowledgement_leaves_the_prompt_held_with_its_reason() {
    let mut state = test_app_state();
    let writer = HoldingWriter::attach(&mut state);
    let fixture = Fixture::new(state, "failed-ack");
    let held = fixture.enqueue_held("resume after a failed write");
    assert!(fixture.state.request_queued_prompt_durability(&fixture.session, &held));
    let (_, fence) = writer.take("the durability fence");
    fence.finish(Err(PersistFenceError::Deadline));
    // The settlement runs off the writer thread and publishes only to state.
    let settled = phase_sync::PollGuard::new();
    let hold = loop {
        let hold = fixture.hold(&held).expect("a failed acknowledgement keeps the hold");
        if hold.failure.is_some() {
            break hold;
        }
        settled.wait("the failed acknowledgement's settlement");
    };
    assert!(
        hold.failure.as_deref().is_some_and(|reason| reason.contains("Deadline")),
        "the reason is recorded: {hold:?}"
    );
    assert_eq!(fixture.drive_every_entry_point(), 0);
    assert_eq!(fixture.delivered_prompts(), 0);
    assert_eq!(fixture.queue_ids(), vec![held]);
}

#[test]
fn a_late_acknowledgement_after_removal_or_for_a_replaced_entry_releases_nothing() {
    let mut state = test_app_state();
    let writer = HoldingWriter::attach(&mut state);
    let fixture = Fixture::new(state, "stale-ack");

    // Removed (as Stop clears workflow prompts) before its acknowledgement.
    let removed = fixture.enqueue_held("removed before its acknowledgement");
    assert!(fixture.state.request_queued_prompt_durability(&fixture.session, &removed));
    let (_, removed_fence) = writer.take("the removed prompt's fence");
    let removed_nonce = fixture.hold(&removed).unwrap().nonce;
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        clear_queued_prompts_by_source(record, QueuedPromptSource::Orchestrator);
        fixture.state.commit_locked(&mut inner).unwrap();
    }

    // Replaced: the same prompt id held again for new content.
    let replaced = fixture.enqueue_held("first content");
    assert!(fixture.state.request_queued_prompt_durability(&fixture.session, &replaced));
    let (_, old_fence) = writer.take("the first content's fence");
    let old_nonce = fixture.hold(&replaced).unwrap().nonce;
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        let mut prompt = record.queued_prompts.front().unwrap().pending_prompt.clone();
        prompt.text = "replacement content".to_owned();
        clear_queued_prompts_by_source(record, QueuedPromptSource::Orchestrator);
        queue_durable_orchestrator_prompt_on_record(record, prompt, Vec::new());
        fixture.state.commit_locked(&mut inner).unwrap();
    }

    // Each fence would settle on its own thread; settle both outcomes here,
    // synchronously, with the nonces those fences carried, so every
    // assertion below runs after both settlements.
    fixture.state.settle_queued_prompt_durability(
        &fixture.session,
        &removed,
        &removed_nonce,
        Ok(()),
    );
    fixture
        .state
        .settle_queued_prompt_durability(&fixture.session, &replaced, &old_nonce, Ok(()));
    assert_eq!(fixture.hold(&removed), None, "the removed entry's hold is dropped");
    assert_eq!(fixture.delivered_prompts(), 0, "a stale acknowledgement releases nothing");
    assert_eq!(fixture.queue_ids(), vec![replaced.clone()]);
    let replacement = fixture.hold(&replaced).expect("the replacement still waits for its own");
    assert_ne!(replacement.nonce, old_nonce);
    assert_eq!(fixture.drive_every_entry_point(), 0);
    // Resolved only now, the old fences' own settlements are stale too.
    drop((removed_fence, old_fence));
}

/// Boot recovery of workflow prompts starts only sessions with no runtime, so
/// it is driven on an ACP session whose next runtime is a test override.
#[test]
fn boot_orphan_dispatch_skips_a_held_workflow_prompt_until_its_acknowledgement() {
    let mut state = test_app_state();
    let writer = HoldingWriter::attach(&mut state);
    let root = TestTempRoot::create("queued-prompt-durability-orphan");
    let (session, held) = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Cursor,
            Some("Durability orphan".to_owned()),
            root.path().to_string_lossy().into_owned(),
            None,
            None,
        );
        let session = record.session.id.clone();
        let prompt = PendingPrompt {
            engram_interrupted: false,
            is_engram_retained: false,
            attachments: Vec::new(),
            id: inner.next_message_id(),
            timestamp: stamp_now(),
            text: "resume the workflow after boot".to_owned(),
            expanded_text: None,
            source: None,
        };
        let held = prompt.id.clone();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.session.status = SessionStatus::Idle;
        record.runtime = SessionRuntime::None;
        queue_durable_orchestrator_prompt_on_record(record, prompt, Vec::new());
        state.commit_locked(&mut inner).unwrap();
        (session, held)
    };
    let (runtime, prompts) = test_acp_runtime_handle(AcpAgent::Cursor, "durability-orphan");
    state.install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
    assert!(state.request_queued_prompt_durability(&session, &held));
    let (_, fence) = writer.take("the workflow prompt's durability fence");

    state.dispatch_orphaned_workflow_prompts();
    assert!(prompts.try_recv().is_err(), "boot recovery promotes no held prompt");
    {
        let inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        assert_eq!(inner.sessions[index].queued_prompts.len(), 1);
        assert!(matches!(inner.sessions[index].runtime, SessionRuntime::None));
    }

    fence.finish(Ok(()));
    assert!(matches!(
        recv_within_guard(&prompts, "the acknowledged workflow prompt starts"),
        Ok(AcpRuntimeCommand::Prompt(command))
            if command.prompt.contains("resume the workflow after boot")
    ));
}

#[test]
fn a_prompt_without_a_hold_dispatches_exactly_as_before() {
    let mut state = test_app_state();
    let _writer = HoldingWriter::attach(&mut state);
    let fixture = Fixture::new(state, "unheld");
    fixture.enqueue_user("an ordinary prompt");
    assert_eq!(fixture.drive_every_entry_point(), 1);
    assert_eq!(fixture.delivered_prompts(), 1);
    assert!(fixture.queue_ids().is_empty());
}

#[test]
fn a_held_entry_read_back_after_a_restart_is_eligible() {
    let (temp_root, project_root, persistence_path, templates_path) =
        super::delegation_support::temp_delegation_state_paths();
    let session;
    let held;
    {
        let state = boot_on_delegation_root(&temp_root, ||
            AppState::new_with_paths(
                project_root.to_string_lossy().into_owned(),
                persistence_path.clone(),
                templates_path.clone(),
            )
            .expect("state should boot")
        );
        let fixture = Fixture::new(state, "restart");
        held = fixture.enqueue_held("held across a restart");
        session = fixture.session.clone();
        // Durable, never acknowledged in this process.
        fixture.state.shutdown_persist_blocking();
    }
    let restarted = boot_on_delegation_root(&temp_root, ||
        AppState::new_with_paths(
            project_root.to_string_lossy().into_owned(),
            persistence_path,
            templates_path,
        )
        .expect("state should reload")
    );
    let prompts = {
        let mut inner = restarted.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        assert_eq!(record.queued_prompts.front().unwrap().pending_prompt.id, held);
        let head = record.queued_prompts.front().unwrap();
        assert!(!queued_prompt_held_for_durability(record, head), "read back, it is durable");
        assert!(
            queued_prompt_kept_standalone(record, head),
            "it stays standalone after the restart"
        );
        let (runtime, prompts) = test_claude_runtime_handle("durability-restarted");
        record.runtime = SessionRuntime::Claude(runtime);
        record.session.status = SessionStatus::Idle;
        prompts
    };
    let dispatch = restarted
        .dispatch_next_queued_turn(&session, false)
        .unwrap()
        .expect("the durable entry is eligible");
    let _ = deliver_turn_dispatch(&restarted, dispatch);
    assert!(matches!(
        recv_within_guard(&prompts, "the durable entry reaches the runtime"),
        Ok(ClaudeRuntimeCommand::Prompt(_))
    ));
    restarted.shutdown_persist_blocking();
}

/// With the production writer, the fence is acknowledged only by reading the
/// exact held entry back from the durable session row.
#[test]
fn the_production_writer_acknowledges_the_exact_held_entry_from_the_durable_row() {
    let (temp_root, project_root, persistence_path, templates_path) =
        super::delegation_support::temp_delegation_state_paths();
    let state = boot_on_delegation_root(&temp_root, ||
        AppState::new_with_paths(
            project_root.to_string_lossy().into_owned(),
            persistence_path,
            templates_path,
        )
        .expect("state should boot")
    );
    let fixture = Fixture::new(state, "production");
    let held = fixture.enqueue_held("released by the real writer");
    assert!(fixture.state.request_queued_prompt_durability(&fixture.session, &held));
    match recv_within_guard(&fixture.prompts, "the real writer's acknowledgement releases it") {
        Ok(ClaudeRuntimeCommand::Prompt(command)) => {
            assert_eq!(command.text, "released by the real writer")
        }
        Ok(_) => panic!("expected the released prompt, got another runtime command"),
        Err(error) => panic!("expected the released prompt, got {error:?}"),
    }
    assert_eq!(fixture.hold(&held), None);
    fixture.state.shutdown_persist_blocking();
}

/// The read-back acknowledges only the committed row's own hold nonce and
/// exact entry: another nonce, other content or another prompt never match.
#[test]
fn the_durable_row_read_back_matches_only_the_exact_hold_and_entry() {
    let (temp_root, project_root, persistence_path, templates_path) =
        super::delegation_support::temp_delegation_state_paths();
    let state = boot_on_delegation_root(&temp_root, ||
        AppState::new_with_paths(
            project_root.to_string_lossy().into_owned(),
            persistence_path.clone(),
            templates_path,
        )
        .expect("state should boot")
    );
    let fixture = Fixture::new(state, "read-back");
    let held = fixture.enqueue_held("only this exact entry");
    let hold = fixture.hold(&held).unwrap();
    fixture.state.shutdown_persist_blocking();
    let connection = rusqlite::Connection::open(&persistence_path).unwrap();
    let target = |prompt_id: &str, nonce: &str, content: &Value| {
        PersistFenceTarget::QueuedPromptDurability {
            session_id: fixture.session.clone(),
            prompt_id: prompt_id.to_owned(),
            content: queued_prompt_durability_fence_content(nonce, content),
        }
    };
    assert!(target(&held, &hold.nonce, &hold.content).is_already_durable(&connection).unwrap());
    assert!(
        !target(&held, "another-nonce", &hold.content).is_already_durable(&connection).unwrap(),
        "a stale hold's nonce never matches"
    );
    let mut changed = hold.content.clone();
    changed["pending_prompt"]["text"] = json!("other content");
    assert_ne!(changed, hold.content, "the entry carries its text");
    assert!(
        !target(&held, &hold.nonce, &changed).is_already_durable(&connection).unwrap(),
        "other content never matches"
    );
    assert!(
        !target("another-prompt", &hold.nonce, &hold.content).is_already_durable(&connection).unwrap(),
        "no entry, no acknowledgement"
    );
}

/// An ordinary send to an idle session whose queue head is held is queued
/// behind it and reported as queued; the head's acknowledgement then
/// dispatches the queue in order.
#[test]
fn a_direct_send_behind_a_held_head_is_queued_not_an_error() {
    let mut state = test_app_state();
    let writer = HoldingWriter::attach(&mut state);
    let fixture = Fixture::new(state, "direct-send");
    let held = fixture.enqueue_held("the held head");
    assert!(fixture.state.request_queued_prompt_durability(&fixture.session, &held));
    let (_, fence) = writer.take("the held head's fence");
    let result = fixture.state.dispatch_turn(
        &fixture.session,
        SendMessageRequest {
            text: "a user prompt sent while the head is held".to_owned(),
            expanded_text: None,
            attachments: Vec::new(),
            source_session_id: None,
            source_mailbox: None,
        },
    );
    assert!(
        matches!(result, Ok(DispatchTurnResult::Queued)),
        "a send behind a held head is queued"
    );
    assert_eq!(fixture.delivered_prompts(), 0);
    let queue = fixture.queue_ids();
    assert_eq!(queue.len(), 2);
    assert_eq!(queue[0], held, "FIFO: the held head stays first");

    fence.finish(Ok(()));
    match recv_within_guard(&fixture.prompts, "the acknowledged head is dispatched first") {
        Ok(ClaudeRuntimeCommand::Prompt(command)) => assert_eq!(command.text, "the held head"),
        Ok(_) => panic!("expected the held head, got another runtime command"),
        Err(error) => panic!("expected the held head, got {error:?}"),
    }
    assert_eq!(fixture.queue_ids(), vec![queue[1].clone()]);
}

/// An entry whose content changes in place under the same prompt id is not
/// released by the acknowledgement of its earlier content: it is held again
/// for its current content, and only that new acknowledgement releases it.
#[test]
fn an_entry_changed_in_place_is_held_again_for_its_new_content() {
    let mut state = test_app_state();
    let writer = HoldingWriter::attach(&mut state);
    let fixture = Fixture::new(state, "changed-in-place");
    let held = fixture.enqueue_held("content as first held");
    assert!(fixture.state.request_queued_prompt_durability(&fixture.session, &held));
    let (_, first) = writer.take("the first content's fence");
    let first_nonce = fixture.hold(&held).unwrap().nonce;
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.queued_prompts.front_mut().unwrap().pending_prompt.text =
            "content changed in place".to_owned();
        fixture.state.commit_locked(&mut inner).unwrap();
    }

    // Its acknowledgement, success or failure, releases nothing and asks
    // for an acknowledgement of the current content instead.
    first.finish(Ok(()));
    let (refenced, second) = writer.take("the changed content's fence");
    assert_eq!(refenced, held);
    let hold = fixture.hold(&held).expect("the changed entry stays held");
    assert_ne!(hold.nonce, first_nonce);
    assert_eq!(hold.content["pending_prompt"]["text"], json!("content changed in place"));
    assert_eq!(fixture.drive_every_entry_point(), 0);
    assert_eq!(fixture.delivered_prompts(), 0);

    second.finish(Ok(()));
    match recv_within_guard(&fixture.prompts, "the current content's acknowledgement releases it") {
        Ok(ClaudeRuntimeCommand::Prompt(command)) => {
            assert_eq!(command.text, "content changed in place")
        }
        Ok(_) => panic!("expected the released prompt, got another runtime command"),
        Err(error) => panic!("expected the released prompt, got {error:?}"),
    }
}

/// A held prompt has its own entry: a peer prompt is neither merged into it
/// nor merges it into an earlier peer batch.
#[test]
fn a_held_prompt_is_never_part_of_a_peer_message_batch() {
    let fixture = Fixture::new(test_app_state(), "peer-batch");
    let peer = |inner: &mut StateInner, text: &str| PendingPrompt {
        source: Some(MessageSource::peer("session-peer".to_owned(), "Peer".to_owned())),
        ..fixture.pending_prompt(inner, text)
    };
    let mut inner = fixture.state.inner.lock().unwrap();
    let before = peer(&mut inner, "a peer batch already queued");
    let held = peer(&mut inner, "a held peer prompt");
    let after = peer(&mut inner, "a later peer prompt");
    let held_id = held.id.clone();
    let index = inner.find_session_index(&fixture.session).unwrap();
    let record = inner.session_mut_by_index(index).unwrap();
    queue_orchestrator_prompt_on_record(record, before, Vec::new());
    queue_durable_orchestrator_prompt_on_record(record, held, Vec::new());
    queue_orchestrator_prompt_on_record(record, after, Vec::new());
    assert_eq!(record.queued_prompts.len(), 3, "three separate entries");
    let entry = &record.queued_prompts[1];
    assert_eq!(entry.pending_prompt.id, held_id);
    assert_eq!(entry.pending_prompt.text, "a held peer prompt");
    assert_eq!(
        record.queued_prompt_durability_holds[&held_id].content,
        queued_prompt_durability_content(entry),
        "the hold guards exactly its own entry"
    );
}

/// Acknowledged but not yet dispatched (the queue is paused), the entry still
/// takes no peer prompt into a batch: its content stays what was proven.
#[test]
fn an_acknowledged_entry_still_queued_stays_out_of_peer_batches() {
    let fixture = Fixture::new(test_app_state(), "acknowledged-standalone");
    let mut inner = fixture.state.inner.lock().unwrap();
    let source = MessageSource::peer("session-peer".to_owned(), "Peer".to_owned());
    let held = PendingPrompt {
        source: Some(source.clone()),
        ..fixture.pending_prompt(&mut inner, "acknowledged while the queue is paused")
    };
    let later = PendingPrompt {
        source: Some(source),
        ..fixture.pending_prompt(&mut inner, "a later peer prompt")
    };
    let held_id = held.id.clone();
    let index = inner.find_session_index(&fixture.session).unwrap();
    let record = inner.session_mut_by_index(index).unwrap();
    record.set_auto_dispatch_blocked(true);
    let nonce = queue_durable_orchestrator_prompt_on_record(record, held, Vec::new());
    fixture.state.commit_locked(&mut inner).unwrap();
    drop(inner);

    fixture
        .state
        .settle_queued_prompt_durability(&fixture.session, &held_id, &nonce, Ok(()));
    let hold = fixture.hold(&held_id).expect("the acknowledged hold stays as a marker");
    assert!(hold.acknowledged);
    assert_eq!(fixture.delivered_prompts(), 0, "the paused queue dispatched nothing");

    let mut inner = fixture.state.inner.lock().unwrap();
    let index = inner.find_session_index(&fixture.session).unwrap();
    let record = inner.session_mut_by_index(index).unwrap();
    let head = record.queued_prompts.front().unwrap();
    assert!(!queued_prompt_held_for_durability(record, head), "acknowledged, it is eligible");
    queue_orchestrator_prompt_on_record(record, later, Vec::new());
    assert_eq!(record.queued_prompts.len(), 2, "the later peer prompt is its own entry");
    assert_eq!(
        record.queued_prompts[0].pending_prompt.text,
        "acknowledged while the queue is paused"
    );
}

/// A manual send to a paused queue, and user prioritization, never overtake a
/// held head, as they never overtake an Engram-retained one.
#[test]
fn a_paused_queue_with_a_held_head_is_not_overtaken_by_a_manual_send() {
    let mut state = test_app_state();
    let writer = HoldingWriter::attach(&mut state);
    let fixture = Fixture::new(state, "paused-manual-send");
    let held = fixture.enqueue_held("held at the head of a paused queue");
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        inner.session_mut_by_index(index).unwrap().set_auto_dispatch_blocked(true);
        fixture.state.commit_locked(&mut inner).unwrap();
    }
    assert!(fixture.state.request_queued_prompt_durability(&fixture.session, &held));
    let (_, fence) = writer.take("the held head's fence");

    let result = fixture.state.dispatch_turn(
        &fixture.session,
        SendMessageRequest {
            text: "a manual send to the paused queue".to_owned(),
            expanded_text: None,
            attachments: Vec::new(),
            source_session_id: None,
            source_mailbox: None,
        },
    );
    assert!(matches!(result, Ok(DispatchTurnResult::Queued)), "it queues behind the held head");
    assert_eq!(fixture.delivered_prompts(), 0);
    let queue = fixture.queue_ids();
    assert_eq!(queue.len(), 2);
    assert_eq!(queue[0], held);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        prioritize_user_queued_prompts(inner.session_mut_by_index(index).unwrap());
    }
    assert_eq!(fixture.queue_ids(), queue, "user prioritization keeps the held head first");
    drop(fence);
}

/// A hold whose acknowledgement failed is dropped when its entry is removed,
/// even though no fence outcome is left to settle it.
#[test]
fn a_failed_hold_is_dropped_with_its_removed_entry() {
    let mut state = test_app_state();
    let writer = HoldingWriter::attach(&mut state);
    let fixture = Fixture::new(state, "failed-then-removed");
    let held = fixture.enqueue_held("failed, then stopped");
    let nonce = fixture.hold(&held).unwrap().nonce;
    assert!(fixture.state.request_queued_prompt_durability(&fixture.session, &held));
    let (_, fence) = writer.take("the fence");
    fixture.state.settle_queued_prompt_durability(
        &fixture.session,
        &held,
        &nonce,
        Err(PersistFenceError::Deadline),
    );
    assert!(fixture.hold(&held).unwrap().failure.is_some());
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        clear_queued_prompts_by_source(record, QueuedPromptSource::Orchestrator);
        fixture.state.commit_locked(&mut inner).unwrap();
    }
    assert_eq!(fixture.hold(&held), None, "no orphan hold is left behind");
    drop(fence);
}

/// An acknowledgement that arrives while the session is mid-turn starts no
/// second turn: the acknowledged entry waits for the turn's own completion.
#[test]
fn an_acknowledgement_during_a_busy_turn_starts_no_second_turn() {
    let mut state = test_app_state();
    let writer = HoldingWriter::attach(&mut state);
    let fixture = Fixture::new(state, "busy");
    let held = fixture.enqueue_held("queued behind a running turn");
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        inner.session_mut_by_index(index).unwrap().session.status = SessionStatus::Active;
        fixture.state.commit_locked(&mut inner).unwrap();
    }
    assert!(fixture.state.request_queued_prompt_durability(&fixture.session, &held));
    let (_, fence) = writer.take("the fence");
    let nonce = fixture.hold(&held).unwrap().nonce;
    // The fence's own settlement would run on a spawned thread, and its
    // release attempt could then race the status change below. Settle the
    // acknowledgement here instead: the settlement, including its release
    // attempt, has returned before the next line runs.
    fixture
        .state
        .settle_queued_prompt_durability(&fixture.session, &held, &nonce, Ok(()));
    assert!(fixture.hold(&held).is_some_and(|hold| hold.acknowledged));
    assert_eq!(fixture.delivered_prompts(), 0, "the release attempt started no second turn");
    assert_eq!(fixture.drive_every_entry_point(), 0, "no drain starts a second turn");
    assert_eq!(fixture.delivered_prompts(), 0);
    assert_eq!(fixture.queue_ids(), vec![held.clone()]);

    // The turn ends: the ordinary drain promotes the acknowledged entry.
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        inner.session_mut_by_index(index).unwrap().session.status = SessionStatus::Idle;
        fixture.state.commit_locked(&mut inner).unwrap();
    }
    assert_eq!(fixture.drive_every_entry_point(), 1);
    assert_eq!(fixture.delivered_prompts(), 1);
    // Resolved only now, the fence's own settlement finds an acknowledged
    // hold, or none, and settles nothing.
    drop(fence);
}

/// A promotion whose commit fails puts the entry back with its acknowledged
/// marker, so it stays standalone for the retry.
#[test]
fn a_rolled_back_promotion_keeps_the_acknowledged_marker() {
    let fixture = Fixture::new(test_app_state(), "rollback");
    let mut inner = fixture.state.inner.lock().unwrap();
    let source = MessageSource::peer("session-peer".to_owned(), "Peer".to_owned());
    let held = PendingPrompt {
        source: Some(source.clone()),
        ..fixture.pending_prompt(&mut inner, "promoted, then rolled back")
    };
    let later = PendingPrompt {
        source: Some(source),
        ..fixture.pending_prompt(&mut inner, "a peer prompt after the rollback")
    };
    let held_id = held.id.clone();
    let index = inner.find_session_index(&fixture.session).unwrap();
    let record = inner.session_mut_by_index(index).unwrap();
    queue_durable_orchestrator_prompt_on_record(record, held, Vec::new());
    record
        .queued_prompt_durability_holds
        .get_mut(&held_id)
        .unwrap()
        .acknowledged = true;

    // What a promotion does before its commit, then the failed commit's undo.
    let snapshot = record.capture_queue_promotion_snapshot();
    let promoted = record.queued_prompts.pop_front().unwrap();
    sync_pending_prompts(record);
    assert!(!record.queued_prompt_durability_holds.contains_key(&held_id));
    record.restore_queue_promotion(snapshot, promoted, "no-started-message");

    let hold = &record.queued_prompt_durability_holds[&held_id];
    assert!(hold.acknowledged, "the marker comes back with its entry");
    queue_orchestrator_prompt_on_record(record, later, Vec::new());
    assert_eq!(record.queued_prompts.len(), 2, "still standalone after the rollback");
    assert_eq!(record.queued_prompts[0].pending_prompt.text, "promoted, then rolled back");
}

/// Mailbox revalidation before dispatch, and the refresh after a mailbox
/// acknowledgement, neither rewrite nor drop an entry with a durability hold,
/// even one that carries a mailbox source and is already acknowledged.
#[test]
fn mailbox_revalidation_leaves_an_acknowledged_entry_untouched() {
    let fixture = Fixture::new(test_app_state(), "mailbox-revalidation");
    let (held_id, nonce) = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let held = PendingPrompt {
            source: Some(MessageSource::mailbox(
                "session-peer".to_owned(),
                "Peer".to_owned(),
                MailboxMessageSource {
                    mailbox_id: "mailbox-durability".to_owned(),
                    message_id: "mailbox-message-durability".to_owned(),
                    sequence: 1,
                    unread_count: 1,
                },
            )),
            ..fixture.pending_prompt(&mut inner, "a durable prompt with a mailbox source")
        };
        let held_id = held.id.clone();
        let index = inner.find_session_index(&fixture.session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.set_auto_dispatch_blocked(true);
        let nonce = queue_durable_orchestrator_prompt_on_record(record, held, Vec::new());
        fixture.state.commit_locked(&mut inner).unwrap();
        (held_id, nonce)
    };
    fixture
        .state
        .settle_queued_prompt_durability(&fixture.session, &held_id, &nonce, Ok(()));
    let hold = fixture.hold(&held_id).unwrap();
    assert!(hold.acknowledged);

    fixture
        .state
        .revalidate_queued_mailbox_wakeups_before_dispatch(&fixture.session).unwrap();
    let _ = fixture.state.refresh_acknowledged_mailbox_wakeups(
        &fixture.session,
        "mailbox-durability",
        u64::MAX,
    );
    let inner = fixture.state.inner.lock().unwrap();
    let index = inner.find_session_index(&fixture.session).unwrap();
    let record = &inner.sessions[index];
    assert_eq!(record.queued_prompts.len(), 1, "the entry is not dropped");
    assert_eq!(
        queued_prompt_durability_content(&record.queued_prompts[0]),
        hold.content,
        "the entry is exactly what was proven durable"
    );
}

/// Whether the session's queue is paused.
fn queue_paused(fixture: &Fixture) -> bool {
    let inner = fixture.state.inner.lock().unwrap();
    let index = inner.find_session_index(&fixture.session).unwrap();
    inner.sessions[index].orchestrator_auto_dispatch_blocked
}

/// The race: Resume of a paused queue while its head's durability
/// acknowledgement is still pending. The Resume is refused with a conflict
/// that says why, and the pause stays; the acknowledgement then sends
/// nothing past the pause, and the next Resume sends the head exactly once.
#[test]
fn resume_while_the_held_head_is_being_saved_is_refused_and_the_pause_kept() {
    let mut state = test_app_state();
    let writer = HoldingWriter::attach(&mut state);
    let fixture = Fixture::new(state, "resume-during-hold");
    let held = fixture.enqueue_held("held, resumed early");
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        inner.session_mut_by_index(index).unwrap().set_auto_dispatch_blocked(true);
        fixture.state.commit_locked(&mut inner).unwrap();
    }
    assert!(fixture.state.request_queued_prompt_durability(&fixture.session, &held));
    let (_, fence) = writer.take("the held head's durability fence");

    let refused = match fixture.state.resume_local_session_queue(&fixture.session, None) {
        Err(error) => error,
        Ok(_) => panic!("a Resume of a held head must be refused"),
    };
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert!(
        refused.message.contains("still being saved"),
        "the refusal says why: {}",
        refused.message
    );
    assert!(queue_paused(&fixture), "the pause stays");
    assert_eq!(fixture.delivered_prompts(), 0);

    // Settled here, so its release attempt has returned before the asserts.
    let nonce = fixture.hold(&held).unwrap().nonce;
    fixture
        .state
        .settle_queued_prompt_durability(&fixture.session, &held, &nonce, Ok(()));
    assert!(fixture.hold(&held).is_some_and(|hold| hold.acknowledged));
    assert_eq!(fixture.delivered_prompts(), 0, "nothing is sent past the pause");
    assert!(queue_paused(&fixture));
    assert_eq!(fixture.queue_ids(), vec![held.clone()]);

    assert!(matches!(
        fixture.state.resume_local_session_queue(&fixture.session, None),
        Ok(Some(_))
    ));
    match recv_within_guard(&fixture.prompts, "the second Resume sends the saved head") {
        Ok(ClaudeRuntimeCommand::Prompt(command)) => {
            assert_eq!(command.text, "held, resumed early")
        }
        Ok(_) => panic!("expected the resumed head, got another runtime command"),
        Err(error) => panic!("expected the resumed head, got {error:?}"),
    }
    assert_eq!(fixture.delivered_prompts(), 0, "sent once");
    assert!(fixture.queue_ids().is_empty());
    drop(fence);
}

/// A Resume of a paused queue whose head's acknowledgement failed is refused
/// with that failure and what to do, and the pause stays.
#[test]
fn a_resume_refused_for_a_failed_acknowledgement_names_the_failure() {
    let mut state = test_app_state();
    let writer = HoldingWriter::attach(&mut state);
    let fixture = Fixture::new(state, "resume-after-failure");
    let held = fixture.enqueue_held("held, its acknowledgement failed");
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        inner.session_mut_by_index(index).unwrap().set_auto_dispatch_blocked(true);
        fixture.state.commit_locked(&mut inner).unwrap();
    }
    assert!(fixture.state.request_queued_prompt_durability(&fixture.session, &held));
    let (_, fence) = writer.take("the held head's durability fence");
    let nonce = fixture.hold(&held).unwrap().nonce;
    fixture.state.settle_queued_prompt_durability(
        &fixture.session,
        &held,
        &nonce,
        Err(PersistFenceError::Deadline),
    );

    let refused = match fixture.state.resume_local_session_queue(&fixture.session, None) {
        Err(error) => error,
        Ok(_) => panic!("a Resume of a held head must be refused"),
    };
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert!(
        refused.message.contains("could not be confirmed saved")
            && refused.message.contains("Deadline")
            && refused.message.contains("cancel that prompt"),
        "the refusal names the failure and the way out: {}",
        refused.message
    );
    assert!(queue_paused(&fixture), "the pause stays");
    assert_eq!(fixture.delivered_prompts(), 0);
    assert_eq!(fixture.queue_ids(), vec![held]);
    drop(fence);
}

/// Enqueues `earlier` then a held head behind it and pauses the queue; then,
/// standing for a Resume whose first check saw `earlier`, a Cancel removes
/// `earlier`, the Resume's drain meets the held head and starts nothing, and
/// the head's acknowledgement settles in the gap (its ordinary drain respects
/// the pause). Returns (earlier, held).
fn resume_raced_by_cancel_and_acknowledgement(fixture: &Fixture) -> (String, String) {
    let earlier = fixture.enqueue_user("cancelled while the Resume drains");
    let held = fixture.enqueue_held("saved while the Resume drained");
    let nonce = fixture.hold(&held).unwrap().nonce;
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.set_auto_dispatch_blocked(true);
        clear_queued_prompts_by_source(record, QueuedPromptSource::User);
        fixture.state.commit_locked(&mut inner).unwrap();
    }
    assert!(
        fixture
            .state
            .dispatch_next_queued_turn(&fixture.session, true)
            .unwrap()
            .is_none(),
        "the Resume's drain meets the held head"
    );
    fixture
        .state
        .settle_queued_prompt_durability(&fixture.session, &held, &nonce, Ok(()));
    assert!(fixture.hold(&held).is_some_and(|hold| hold.acknowledged));
    assert_eq!(fixture.delivered_prompts(), 0, "the acknowledgement keeps the pause");
    (earlier, held)
}

/// A Resume whose drain met the head held, the head being saved before the
/// Resume finished, starts it by exactly one more drain: never a success
/// that started nothing.
#[test]
fn a_head_saved_while_the_resume_drained_is_started_by_its_single_retry() {
    let fixture = Fixture::new(test_app_state(), "resume-retry");
    let (earlier, held) = resume_raced_by_cancel_and_acknowledgement(&fixture);
    assert_eq!(fixture.queue_ids(), vec![held]);

    let dispatch = fixture
        .state
        .finish_resume_drain(&fixture.session, None, Some(&earlier), None)
        .expect("not refused")
        .expect("the retry starts the saved head");
    let _ = deliver_turn_dispatch(&fixture.state, dispatch.dispatch);
    match recv_within_guard(&fixture.prompts, "the retry sends the saved head") {
        Ok(ClaudeRuntimeCommand::Prompt(command)) => {
            assert_eq!(command.text, "saved while the Resume drained")
        }
        Ok(_) => panic!("expected the saved head, got another runtime command"),
        Err(error) => panic!("expected the saved head, got {error:?}"),
    }
    assert_eq!(fixture.delivered_prompts(), 0, "sent once");
    assert!(fixture.queue_ids().is_empty());
}

/// When that single retry also starts nothing, the Resume is refused because
/// the queue changed; a drain that started nothing for a reason that is not
/// a durability hold (the head unchanged) keeps its outcome.
#[test]
fn a_retry_that_starts_nothing_refuses_because_the_queue_changed() {
    let fixture = Fixture::new(test_app_state(), "resume-retry-refused");
    let (earlier, held) = resume_raced_by_cancel_and_acknowledgement(&fixture);
    {
        // The retry cannot start it: the session is busy.
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        inner.session_mut_by_index(index).unwrap().session.status = SessionStatus::Active;
        fixture.state.commit_locked(&mut inner).unwrap();
    }
    assert!(matches!(
        fixture
            .state
            .finish_resume_drain(&fixture.session, None, Some(&held), None),
        Ok(None)
    ));
    let refused = match fixture
        .state
        .finish_resume_drain(&fixture.session, None, Some(&earlier), None)
    {
        Err(error) => error,
        Ok(_) => panic!("a retry that starts nothing must be refused"),
    };
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(refused.message, RESUME_QUEUE_CHANGED_REFUSAL);
    assert!(queue_paused(&fixture), "the pause stays");
    assert_eq!(fixture.delivered_prompts(), 0);
    assert_eq!(fixture.queue_ids(), vec![held]);
}

/// A bound Resume (a held delegation's prompt) whose head was cancelled is
/// superseded as before: a held successor's durability refusal is not its.
#[test]
fn a_bound_resume_whose_head_is_gone_is_not_refused_for_a_held_successor() {
    let fixture = Fixture::new(test_app_state(), "bound-resume-superseded");
    fixture.enqueue_user("the bound head, then cancelled");
    let held = fixture.enqueue_held("a held successor");
    let owner = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.set_auto_dispatch_blocked(true);
        let owner = EngramQueuedAdmissionOwner::capture(record).expect("a head");
        clear_queued_prompts_by_source(record, QueuedPromptSource::User);
        fixture.state.commit_locked(&mut inner).unwrap();
        owner
    };
    assert!(matches!(
        fixture.state.resume_local_session_queue(&fixture.session, Some(owner)),
        Ok(None)
    ));
    assert!(queue_paused(&fixture));
    assert_eq!(fixture.delivered_prompts(), 0);
    assert_eq!(fixture.queue_ids(), vec![held]);
}
