//! Real queue promotion must refuse an isolated own-row write, retaining its head.
use super::*;

struct QueueFixture {
    _store: FenceFixture,
    state: AppState,
    bad: String,
    good: String,
    bad_prompt: String,
    bad_generation: u64,
    bad_input: mpsc::Receiver<ClaudeRuntimeCommand>,
    good_input: mpsc::Receiver<ClaudeRuntimeCommand>,
}

fn ordinary_queue_fixture() -> QueueFixture {
    queue_fixture(Some(QueuedPromptSource::User))
}

fn queue_fixture(source: Option<QueuedPromptSource>) -> QueueFixture {
    let mut store = FenceFixture::new();
    let (bad, good, bad_prompt, bad_input, good_input) = {
        let mut inner = store.inner.lock().unwrap();
        let cwd = store.root.path().to_string_lossy().into_owned();
        let bad = inner.create_session(Agent::Claude, None, cwd.clone(), None, None).session.id;
        let good = inner.create_session(Agent::Claude, None, cwd, None, None).session.id;
        let (bad_runtime, bad_input) = test_claude_runtime_handle("rejected-promotion");
        let (good_runtime, good_input) = test_claude_runtime_handle("saved-promotion");
        let record = inner.session_mut(&bad).unwrap();
        for id in ["body-0", "body-1", "body-2"] {
            push_message_on_record(record, message(id));
        }
        record.runtime = SessionRuntime::Claude(bad_runtime);
        record.session.status = SessionStatus::Idle;
        record.session.prompt_history = vec!["earlier prompt".into()];
        record.prompt_history_mutation_stamp = record.mutation_stamp;
        record.set_auto_dispatch_blocked(true);
        let bad_prompt = format!("ack-head-{bad}");
        if let Some(source) = source {
            queue_ordinary_prompt(record, &bad_prompt);
            record.queued_prompts[0].source = source;
        }
        let record = inner.session_mut(&good).unwrap();
        record.runtime = SessionRuntime::Claude(good_runtime);
        record.session.status = SessionStatus::Idle;
        record.set_auto_dispatch_blocked(true);
        if let Some(source) = source {
            queue_ordinary_prompt(record, &format!("ack-head-{good}"));
            record.queued_prompts[0].source = source;
        }
        (bad, good, bad_prompt, bad_input, good_input)
    };
    // The original head and all three cold-prefix identities are already durable.
    store.write(&store.collect()).unwrap();
    {
        let mut inner = store.inner.lock().unwrap();
        let record = inner.session_mut(&bad).unwrap();
        record.session.messages.remove(0);
        record.session.messages.remove(0);
        record.message_start_index = 2;
        record.session.messages_loaded = false;
        record.session.messages[0] = message("body-0");
        record.message_positions = build_message_positions(&record.session.messages);
        inner.preferences.default_codex_model = "saved-sibling-preference".into();
    }
    let mut state = store.app_state();
    state.inner = Arc::clone(&store.inner);
    state.persist_worker_alive.store(false, Ordering::Release);
    state.select_test_scripted_engram_budget_clock();
    let bad_generation = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&bad).unwrap()].active_turn_generation
    };
    QueueFixture { _store: store, state, bad, good, bad_prompt, bad_generation, bad_input, good_input }
}

fn stored_ids(fixture: &QueueFixture, session: &str) -> Vec<String> {
    let connection = fixture._store.cache.connection.as_ref().unwrap();
    let mut statement = connection.prepare(
        "SELECT message_id FROM messages WHERE session_id=?1 ORDER BY position",
    ).unwrap();
    let rows = statement.query_map([session], |row| row.get::<_, String>(0)).unwrap();
    rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
}

fn stored_session(fixture: &QueueFixture, session: &str) -> String {
    fixture._store.cache.connection.as_ref().unwrap().query_row(
        "SELECT value_json FROM sessions WHERE id=?1", [session], |row| row.get(0),
    ).unwrap()
}

fn assert_original_head_retained(fixture: &QueueFixture) {
    let inner = fixture.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&fixture.bad).unwrap()];
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(record.session.queue_paused);
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert_eq!(record.active_turn_generation, fixture.bad_generation);
    assert_eq!(record.session.messages.len(), 1, "no promoted prompt can survive rollback");
    assert_eq!(record.session.prompt_history, ["earlier prompt"]);
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(record.queued_prompts[0].pending_prompt.id, fixture.bad_prompt);
    assert_eq!(record.queued_prompts[0].pending_prompt.text, "keep the original queued intent");
    assert!(record.prompt_deliveries.is_empty(), "ordinary prompts need no forged receipt");
    assert!(matches!(fixture.bad_input.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "a rejected promotion must not reach or tear down the provider channel");
}

#[test]
fn ordinary_resume_sql_conflict_rolls_back_and_same_head_dispatches_once_after_correction() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = ordinary_queue_fixture();
    let error = match fixture.state.resume_session_queue(&fixture.bad) {
        Err(error) => error,
        Ok(_) => panic!("a rejected promotion must fail before provider dispatch"),
    };
    assert!(error.message.contains("UNIQUE constraint failed: messages.session_id, messages.message_id"));
    assert!(error.message.contains(&fixture.bad));
    assert_original_head_retained(&fixture);
    // A different session can promote and really enqueue while the bad row remains rejected.
    fixture.state.resume_session_queue(&fixture.good).unwrap();
    assert!(matches!(recv_within_guard(&fixture.good_input, "healthy sibling promotion"),
        Ok(ClaudeRuntimeCommand::Prompt(_))));
    assert!(fixture.good_input.try_recv().is_err());
    assert_original_head_retained(&fixture);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let record = inner.session_mut(&fixture.bad).unwrap();
        record.session.messages[0] = message("body-2");
        record.message_positions = build_message_positions(&record.session.messages);
    }
    fixture.state.resume_session_queue(&fixture.bad).unwrap();
    assert!(matches!(recv_within_guard(&fixture.bad_input, "corrected original promotion"),
        Ok(ClaudeRuntimeCommand::Prompt(_))));
    assert!(fixture.bad_input.try_recv().is_err(), "no duplicate provider command");
    // Inspect the durable rows separately from boot's recovery projection.
    assert_eq!(stored_ids(&fixture, &fixture.bad),
        ["body-0", "body-1", "body-2", fixture.bad_prompt.as_str()]);
    let loaded = load_state(fixture.state.persistence_path.as_path()).unwrap().unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&fixture.bad).unwrap()];
    assert!(record.queued_prompts.is_empty());
    assert_eq!(record.session.prompt_history, ["earlier prompt", "keep the original queued intent"]);
    assert_eq!(record.session.messages.iter().map(Message::id).collect::<Vec<_>>(),
        ["body-0", "body-1", "body-2", fixture.bad_prompt.as_str(), "message-1"]);
    assert!(matches!(record.session.messages.last(), Some(Message::Text {
        author: Author::Assistant, text, source: None, ..
    }) if text == "TermAl restarted before this turn finished. The last response may be incomplete. Send another prompt to continue."));
    assert_eq!(record.session.status, SessionStatus::Error);
    assert_eq!(loaded.preferences.default_codex_model, "saved-sibling-preference");
}

#[test]
fn direct_queued_promotion_sql_conflict_uses_the_existing_rollback_owner() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = ordinary_queue_fixture();
    let error = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.bad).unwrap();
        match fixture.state.start_and_commit_queued_dispatch_locked(
            &mut inner, index, None, &fixture.bad_prompt,
        ) {
            Err(error) => error,
            Ok(_) => panic!("a direct promotion cannot succeed after its session was rejected"),
        }
    };
    assert!(format!("{error:#}").contains("UNIQUE constraint failed"));
    assert_original_head_retained(&fixture);
}

#[test]
fn refused_ordinary_promotion_does_not_lend_its_trace_to_a_different_prompt() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = ordinary_queue_fixture();
    let root = fixture._store.root.path();
    let project = create_test_project(&fixture.state, root, "ordinary observation rollback");
    crate::tests::engram_host_adapter::enable_test_project_engram(&fixture.state, &project, root);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        inner.projects.iter_mut().find(|row| row.id == project).unwrap()
            .engram.as_mut().unwrap().authority_store_key = Some(EngramAuthorityStoreKey {
                database_path: root.join("observation-fixture.db"), project_id: project.clone(),
            });
        let record = inner.session_mut(&fixture.bad).unwrap();
        record.session.project_id = Some(project);
        assert!(record.engram.unadmitted_active_turn.is_none());
    }
    let error = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.bad).unwrap();
        fixture.state.start_and_commit_queued_dispatch_locked(
            &mut inner, index, None, &fixture.bad_prompt,
        ).err().expect("the real SQL conflict must refuse promotion")
    };
    assert!(format!("{error:#}").contains("UNIQUE constraint failed"));
    assert_original_head_retained(&fixture);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.bad).unwrap();
        let record = &mut inner.sessions[index];
        assert!(record.engram.unadmitted_active_turn.is_none(), "unsent trace rolled back");
        // Preserve the old intent separately, then select a genuinely different
        // prompt at the same restored generation after correcting the row.
        let mut replacement = record.queued_prompts.front().unwrap().clone();
        replacement.pending_prompt.id = "different-observed-prompt".into();
        replacement.pending_prompt.text = "a different ordinary task".into();
        record.queued_prompts[0] = replacement;
        record.session.messages[0] = message("body-2");
        record.message_positions = build_message_positions(&record.session.messages);
        let started = fixture.state.start_and_commit_queued_dispatch_locked(
            &mut inner, index, None, "different-observed-prompt",
        ).unwrap();
        assert!(started.is_some());
        let record = &inner.sessions[index];
        let trace = record.engram.unadmitted_active_turn.as_ref().unwrap();
        assert_eq!(trace.notice_message_id, "different-observed-prompt");
        assert_eq!(trace.generation, fixture.bad_generation + 1);
        assert!(trace.commands.is_empty());
    }
}

fn historical_causal_card() -> EngramControlCard {
    let mut cause = EngramCausalFailure::local(
        &EngramTransportError::transport("retained concrete control failure"),
        "turn_evaluate", "synchronous card write",
    );
    cause.publication_pending = true;
    EngramControlCard {
        causal_failure: Some(cause),
        schema_version: ENGRAM_CONTROL_SCHEMA_VERSION,
        stage: EngramControlStage::Dispatch,
        assurance: "turn_gated".into(),
        decision: EngramControlCardDecision::Refuse,
        dispatch: EngramControlCardDispatch::Withheld,
        refusal_code: Some("control_unavailable".into()),
        defer_code: None,
        grant_id: None,
        directives: Vec::new(),
        delivered_range: None,
        latency_ms: EngramControlLatencyCard {
            admission_fence: None, preparation_fence: None,
            evaluate: None, begin: None, checkpoint: None, total: 0,
        },
        fail_mode: EngramControlFailMode::Enforced,
        repair_armed: false,
        next_intent: None,
        source_root: None,
    }
}

#[test]
fn historical_cause_reaches_only_its_own_committed_row_after_partial_synchronous_save() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (store, state, bad, good) = partial_synchronous_fixture(false);
    let (bad_index, good_index) = {
        let mut inner = state.inner.lock().unwrap();
        // This test's saved sibling is local, unlike the proxy-retry
        // fixture's default, so its own row really carries the transcript.
        let record = inner.session_mut(&good).unwrap();
        record.remote_id = None;
        record.remote_session_id = None;
        assert!(record.is_local_session());
        let mut indexes = Vec::new();
        for id in [&bad, &good] {
            let record = inner.session_mut(id).unwrap();
            indexes.push(push_message_on_record(record, Message::EngramControl {
                id: format!("causal-{id}"), timestamp: stamp_now(), author: Author::Assistant,
                card: historical_causal_card(),
            }));
        }
        state.commit_locked_with_persist_outcome(&mut inner).unwrap();
        (indexes[0], indexes[1])
    };
    // In memory the cause stays hidden: no publication callback runs.
    let visible = |id: &str, index| {
        let inner = state.inner.lock().unwrap();
        serde_json::to_value(&inner.sessions[inner.find_session_index(id).unwrap()].session.messages[index])
            .unwrap().get("causalFailure").is_some()
    };
    // The committed row alone carries the cause, and only for its own session.
    let stored_causes = |id: &str| -> Vec<bool> {
        let connection = store.cache.connection.as_ref().unwrap();
        let mut statement = connection.prepare(
            "SELECT value_json FROM messages WHERE session_id=?1 AND message_id=?2",
        ).unwrap();
        statement.query_map(rusqlite::params![id, format!("causal-{id}")], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|json| serde_json::from_str::<Value>(&json.unwrap()).unwrap().get("causalFailure").is_some())
            .collect()
    };
    assert!(!visible(&bad, bad_index));
    assert!(!visible(&good, good_index));
    assert_eq!(stored_causes(&bad), Vec::<bool>::new(), "the rejected own row stores no card");
    assert_eq!(stored_causes(&good), [true], "the saved sibling's committed row carries its cause");
    {
        let mut inner = state.inner.lock().unwrap();
        let record = inner.session_mut(&bad).unwrap();
        record.session.messages[0] = message("body-2");
        record.message_positions = build_message_positions(&record.session.messages);
        state.commit_locked_with_persist_outcome(&mut inner).unwrap();
    }
    assert_eq!(stored_causes(&bad), [true], "the corrected own row now commits its cause");
    assert_eq!(stored_causes(&good), [true]);
    assert!(!visible(&bad, bad_index), "a commit does not recreate a publication callback");
}

#[test]
fn narrow_row_commit_keeps_existing_housekeeping_semantics() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (_store, state, _, good) = partial_synchronous_fixture(false);
    let mut inner = state.inner.lock().unwrap();
    state.commit_persisted_session_delta_locked(&mut inner, &good).unwrap();
    assert!(inner.settings_persist_dirty);
    assert!(inner.remote_settings_persist_dirty);
    assert!(inner.remote_delta_persist_dirty);
    state.commit_session_locked(&mut inner, &good).unwrap();
    assert!(!inner.settings_persist_dirty);
    assert!(!inner.remote_settings_persist_dirty);
    assert!(!inner.remote_delta_persist_dirty);
}

fn send_human(
    state: &AppState, session: &str, text: &str, accepted: &mut Option<String>,
) -> Result<(), ApiError> {
    let result = state.dispatch_human_turn_with_acceptance(session, SendMessageRequest {
        text: text.into(), expanded_text: None, attachments: Vec::new(),
        source_session_id: None, source_mailbox: None,
    }, accepted)?;
    match result {
        DispatchTurnResult::Dispatched(dispatch) | DispatchTurnResult::DispatchedAfterQueue(dispatch) => {
            deliver_turn_dispatch(state, dispatch).into_public_result()
        }
        DispatchTurnResult::Queued => Ok(()),
    }
}

/// The session's own-row ACK image is scoped to it and already committed:
/// nothing about its queue, deliveries, holds or choices awaits a save.
fn own_row_image_is_durable(state: &AppState, inner: &StateInner, session: &str) -> bool {
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    PersistFenceTarget::OrdinarySessionRow {
        session_id: session.to_owned(),
        content: ordinary_session_row_persisted_content(&PersistedSessionRecord::from_record(record)),
    }
        .is_already_durable(&connection)
        .unwrap()
}

fn human_selection(state: &AppState, session: &str) -> HumanPromptSelection {
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    assert!(own_row_image_is_durable(state, &inner, session));
    assert!(record.prompt_deliveries.is_empty(), "exercise an unreceipted Human head");
    HumanPromptSelection {
        prompt: projected_pending_prompts(record)[0].clone(),
        session_mutation_stamp: record.mutation_stamp,
    }
}

#[test]
fn human_intake_sql_conflict_leaves_the_draft_unaccepted_and_sibling_delivers() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = queue_fixture(None);
    let before = stored_session(&fixture, &fixture.bad);
    let text = "preserve the unaccepted Human draft";
    let mut accepted = None;
    let error = send_human(&fixture.state, &fixture.bad, text, &mut accepted)
        .err().expect("an own-row rejection must refuse Human intake");
    assert!(error.message.contains("UNIQUE constraint failed"));
    assert!(error.message.contains(&fixture.bad));
    assert!(accepted.is_none(), "no executable copy of a rejected draft may be accepted");
    assert!(matches!(fixture.bad_input.try_recv(), Err(mpsc::TryRecvError::Empty)));
    assert_eq!(stored_session(&fixture, &fixture.bad), before);
    assert_eq!(stored_ids(&fixture, &fixture.bad), ["body-0", "body-1", "body-2"]);
    {
        let inner = fixture.state.inner.lock().unwrap();
        assert!(own_row_image_is_durable(&fixture.state, &inner, &fixture.bad),
            "a rejected draft leaves no unsaved own-row image");
        let record = &inner.sessions[inner.find_session_index(&fixture.bad).unwrap()];
        assert!(record.queued_prompts.is_empty());
        assert!(record.human_prompt_turn.is_none());
        assert_eq!(record.session.status, SessionStatus::Idle);
        assert_eq!(record.active_turn_generation, fixture.bad_generation);
        assert_eq!(record.session.prompt_history, ["earlier prompt"]);
        assert!(record.orchestrator_auto_dispatch_blocked);
    }
    let mut good_accepted = None;
    send_human(&fixture.state, &fixture.good, "healthy Human sibling", &mut good_accepted).unwrap();
    assert!(good_accepted.is_some());
    assert!(matches!(recv_within_guard(&fixture.good_input, "healthy Human intake"),
        Ok(ClaudeRuntimeCommand::Prompt(command)) if command.text.contains("healthy Human sibling")));
    assert!(matches!(fixture.good_input.try_recv(), Err(mpsc::TryRecvError::Empty)));
    assert_eq!(stored_session(&fixture, &fixture.bad), before);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let record = inner.session_mut(&fixture.bad).unwrap();
        record.session.messages[0] = message("body-2");
        record.message_positions = build_message_positions(&record.session.messages);
    }
    send_human(&fixture.state, &fixture.bad, text, &mut accepted).unwrap();
    let accepted = accepted.expect("the corrected draft is now accepted");
    assert!(matches!(recv_within_guard(&fixture.bad_input, "corrected Human intake"),
        Ok(ClaudeRuntimeCommand::Prompt(command)) if command.text.contains(text)));
    assert!(matches!(fixture.bad_input.try_recv(), Err(mpsc::TryRecvError::Empty)));
    assert_eq!(stored_ids(&fixture, &fixture.bad), ["body-0", "body-1", "body-2", accepted.as_str()]);
    let inner = fixture.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&fixture.bad).unwrap()];
    assert_eq!(record.session.prompt_history, ["earlier prompt", text]);
}

#[test]
fn human_promotion_sql_conflict_keeps_the_exact_head_until_correction() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = queue_fixture(Some(QueuedPromptSource::Human));
    let before = stored_session(&fixture, &fixture.bad);
    let queued_before = {
        let inner = fixture.state.inner.lock().unwrap();
        serde_json::to_value(&inner.sessions[inner.find_session_index(&fixture.bad).unwrap()].queued_prompts).unwrap()
    };
    let error = fixture.state.resume_selected_human_prompt(
        &fixture.bad, human_selection(&fixture.state, &fixture.bad),
    ).err().expect("an own-row rejection must refuse Human promotion");
    assert!(error.message.contains("UNIQUE constraint failed"));
    assert!(error.message.contains(&fixture.bad));
    assert_original_head_retained(&fixture);
    assert_eq!(stored_session(&fixture, &fixture.bad), before);
    assert_eq!(stored_ids(&fixture, &fixture.bad), ["body-0", "body-1", "body-2"]);
    {
        let inner = fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.bad).unwrap()];
        assert_eq!(serde_json::to_value(&record.queued_prompts).unwrap(), queued_before);
        assert!(record.human_prompt_turn.is_none());
    }
    fixture.state.resume_selected_human_prompt(
        &fixture.good, human_selection(&fixture.state, &fixture.good),
    ).unwrap();
    assert!(matches!(recv_within_guard(&fixture.good_input, "healthy Human promotion"),
        Ok(ClaudeRuntimeCommand::Prompt(_))));
    assert!(matches!(fixture.good_input.try_recv(), Err(mpsc::TryRecvError::Empty)));
    assert_original_head_retained(&fixture);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let record = inner.session_mut(&fixture.bad).unwrap();
        record.session.messages[0] = message("body-2");
        record.message_positions = build_message_positions(&record.session.messages);
    }
    fixture.state.resume_selected_human_prompt(
        &fixture.bad, human_selection(&fixture.state, &fixture.bad),
    ).unwrap();
    assert!(matches!(recv_within_guard(&fixture.bad_input, "corrected Human promotion"),
        Ok(ClaudeRuntimeCommand::Prompt(command)) if command.text.contains("keep the original queued intent")));
    assert!(matches!(fixture.bad_input.try_recv(), Err(mpsc::TryRecvError::Empty)));
    assert_eq!(stored_ids(&fixture, &fixture.bad), ["body-0", "body-1", "body-2", fixture.bad_prompt.as_str()]);
    let inner = fixture.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&fixture.bad).unwrap()];
    assert_eq!(record.session.prompt_history, ["earlier prompt", "keep the original queued intent"]);
    assert_eq!(record.human_prompt_turn.as_ref().unwrap().prompt_id, fixture.bad_prompt);
}
