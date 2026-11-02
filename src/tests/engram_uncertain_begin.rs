//! Ordinary queue ownership and cancellation beside passive control history.
use super::*;

/// Real local runtime/dispatch owners, with automatic observations off. The
/// empty protocol recorder detects accidental calls and implements no gate.
pub(super) struct OrdinaryRoot {
    pub(super) state: AppState,
    pub(super) session: String,
    pub(super) project: String,
    pub(super) root: PathBuf,
    pub(super) receiver: mpsc::Receiver<CodexRuntimeCommand>,
    pub(super) transport: Arc<ScriptedEngramControlTransport>,
}

impl OrdinaryRoot {
    pub(super) fn new(label: &str) -> Self {
        let (state, receiver) = test_app_state_with_delegation_codex_runtime(label);
        state.select_test_scripted_engram_budget_clock();
        let root = state.test_temp_root.as_ref().unwrap().path().join(label);
        fs::create_dir_all(&root).unwrap();
        let project = create_test_project(&state, &root, "Ordinary queue");
        let session = create_test_project_session(&state, Agent::Codex, &project, &root);
        fs::write(root.join(".engram-project"), format!("{project}\n")).unwrap();
        {
            let mut inner = state.inner.lock().unwrap();
            inner.projects.iter_mut().find(|entry| entry.id == project).unwrap().engram =
                Some(EngramProjectSettings {
                    enabled: true, execution_observability: false,
                    home: Some(root.to_string_lossy().into_owned()),
                    binary_path: Some(root.join("engram-fixture").to_string_lossy().into_owned()),
                    ..EngramProjectSettings::default()
                });
            inner.engram_declared_project_ids.insert(project.clone());
            inner.engram_declaration_checked_project_ids.insert(project.clone());
            inner.session_mut(&session).unwrap().engram.context_nudge_pending = false;
            state.commit_locked(&mut inner).unwrap();
        }
        let transport = ScriptedEngramControlTransport::new([]);
        state.install_control_test_transport(transport.clone());
        Self { state, session, project, root, receiver, transport }
    }

    pub(super) fn record<T>(&self, read: impl FnOnce(&SessionRecord) -> T) -> T {
        let inner = self.state.inner.lock().unwrap();
        read(&inner.sessions[inner.find_session_index(&self.session).unwrap()])
    }

    pub(super) fn edit<T>(&self, edit: impl FnOnce(&mut SessionRecord) -> T) -> T {
        let mut inner = self.state.inner.lock().unwrap();
        let value = edit(inner.session_mut(&self.session).unwrap());
        self.state.commit_locked(&mut inner).unwrap();
        value
    }

    pub(super) fn prepare(&self, text: &str) -> TurnDispatch {
        match self.state.dispatch_turn(&self.session, SendMessageRequest {
            text: text.into(), expanded_text: None, attachments: Vec::new(),
            source_session_id: None, source_mailbox: None,
        }).unwrap() {
            DispatchTurnResult::Dispatched(dispatch)
            | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
            DispatchTurnResult::Queued => panic!("ordinary idle session must dispatch"),
        }
    }

    pub(super) fn enqueue(&self, text: &str, source: QueuedPromptSource) -> String {
        let mut inner = self.state.inner.lock().unwrap();
        let id = inner.next_message_id();
        queue_prompt_on_record_with_source(inner.session_mut(&self.session).unwrap(), PendingPrompt {
            engram_interrupted: false, is_engram_retained: false, attachments: Vec::new(),
            id: id.clone(), timestamp: stamp_now(), text: text.into(), expanded_text: None, source: None,
        }, Vec::new(), source);
        self.state.commit_locked(&mut inner).unwrap();
        id
    }

    pub(super) fn prompt(&self) -> CodexPromptCommand {
        loop {
            if let CodexRuntimeCommand::Prompt { session_id, command } =
                crate::tests::phase_sync::receive(&self.receiver, "ordinary provider prompt")
            {
                assert_eq!(session_id, self.session);
                return command;
            }
        }
    }

    pub(super) fn enqueue_durable(&self, text: &str, receipted: bool) -> String {
        let mut inner = self.state.inner.lock().unwrap();
        let id = inner.next_message_id();
        let prompt = PendingPrompt {
            engram_interrupted: false, is_engram_retained: false, attachments: Vec::new(),
            id: id.clone(), timestamp: stamp_now(), text: text.into(), expanded_text: None, source: None,
        };
        let record = inner.session_mut(&self.session).unwrap();
        if receipted { queue_receipted_orchestrator_prompt_on_record(record, prompt, Vec::new()); }
        else { queue_durable_orchestrator_prompt_on_record(record, prompt, Vec::new()); }
        self.state.commit_locked(&mut inner).unwrap();
        id
    }

    pub(super) fn assert_no_prompt(&self) {
        while let Ok(command) = self.receiver.try_recv() {
            assert!(!matches!(command, CodexRuntimeCommand::Prompt { .. }), "unexpected provider handoff");
        }
    }

    pub(super) fn assert_no_control(&self) {
        assert!(self.transport.requests().is_empty(), "ordinary queue service must not call control");
    }

    pub(super) fn saved(&self) -> Value {
        self.record(|record| serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap())
    }
}

impl Drop for OrdinaryRoot {
    fn drop(&mut self) { self.state.shutdown_persist_blocking(); }
}

#[test]
fn ordinary_stale_runtime_or_turn_cannot_handoff_or_mutate_its_successor() {
    for change in ["turn", "runtime"] {
        let turn = OrdinaryRoot::new("stale-ordinary-dispatch");
        let dispatch = turn.prepare("old prompt");
        turn.edit(|record| {
            if change == "turn" { record.active_turn_generation += 1; }
            else { record.runtime = SessionRuntime::None; }
        });
        let before = turn.saved();
        assert!(matches!(deliver_turn_dispatch(&turn.state, dispatch), TurnDispatchDeliveryOutcome::Superseded));
        assert_eq!(turn.saved(), before, "a stale callback cannot change the newer owner");
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}

#[test]
fn ordinary_handoff_waits_for_the_existing_stop_owner() {
    let turn = OrdinaryRoot::new("ordinary-stop-owner");
    let dispatch = turn.prepare("unsent during Stop");
    turn.edit(|record| {
        record.runtime_stop_in_progress = true;
        record.runtime_stop_generation += 1;
    });
    assert!(matches!(deliver_turn_dispatch(&turn.state, dispatch), TurnDispatchDeliveryOutcome::Scheduled));
    turn.record(|record| {
        assert!(record.runtime_stop_in_progress);
        assert_eq!(record.deferred_stop_callbacks.len(), 1);
        assert_eq!(record.session.status, SessionStatus::Active);
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_queue_source_filter_keeps_other_sources_and_only_the_exact_stop_owned_head() {
    let turn = OrdinaryRoot::new("ordinary-source-filter");
    let protected = turn.enqueue("Stop-owned workflow", QueuedPromptSource::Orchestrator);
    let removed = turn.enqueue("another workflow", QueuedPromptSource::Orchestrator);
    let user = turn.enqueue("ordinary user", QueuedPromptSource::User);
    let mailbox = turn.enqueue("mailbox", QueuedPromptSource::Mailbox);
    turn.edit(|record| {
        record.queued_prompts[0].promoted_message_index = Some(0);
        record.active_turn_start_message_count = Some(0);
        record.runtime_stop_in_progress = true;
        let owner = QueuedTurnOwner::capture_promoted(record).unwrap();
        clear_queued_prompts_by_source_except_delivery_owner(record, QueuedPromptSource::Orchestrator, Some(&owner));
        let ids = record.queued_prompts.iter().map(|entry| entry.pending_prompt.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids, [protected.clone(), user.clone(), mailbox.clone()]);
        assert!(!ids.contains(&removed));
        clear_queued_prompts_by_source(record, QueuedPromptSource::Orchestrator);
        assert_eq!(record.queued_prompts.iter().map(|entry| entry.pending_prompt.id.clone()).collect::<Vec<_>>(), [user, mailbox]);
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_stale_queue_owner_resume_never_consumes_the_successor() {
    let turn = OrdinaryRoot::new("ordinary-stale-resume");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let first = turn.enqueue("cancelled", QueuedPromptSource::User);
    let owner = turn.record(|record| QueuedTurnOwner::capture(record).unwrap());
    let second = turn.enqueue("successor", QueuedPromptSource::User);
    turn.state.cancel_queued_prompt(&turn.session, &first).unwrap();
    let before = turn.saved();
    let outcome = turn.state.resume_local_session_queue(&turn.session, Some(owner)).unwrap();
    assert!(matches!(outcome, None | Some(TurnDispatchDeliveryOutcome::Superseded)));
    assert_eq!(turn.saved(), before);
    turn.record(|record| assert_eq!(record.queued_prompts[0].pending_prompt.id, second));
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_owner_expires_on_content_pause_stop_or_head_change() {
    for change in ["content", "pause", "stop", "head", "generation"] {
        let turn = OrdinaryRoot::new("ordinary-owner-identity");
        turn.enqueue("original", QueuedPromptSource::User);
        let owner = turn.record(|record| QueuedTurnOwner::capture(record).unwrap());
        turn.edit(|record| {
            assert!(owner.matches(record));
            match change {
                "content" => record.queued_prompts[0].pending_prompt.text.push_str(" changed"),
                "pause" => record.set_auto_dispatch_blocked(true),
                "stop" => record.runtime_stop_generation += 1,
                "head" => { record.queued_prompts.pop_front(); },
                _ => record.active_turn_generation += 1,
            }
            assert!(!owner.matches(record), "{change} must invalidate the exact owner");
        });
        turn.assert_no_prompt();
    }
}
