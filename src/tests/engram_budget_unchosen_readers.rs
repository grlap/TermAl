//! No-use clock witnesses for ordinary queue and provider retry owners.
//! Removed control transport-failure/boot-recovery readers are retired.
use super::*;

fn plain_session() -> (AppState, String) {
    let (state, _, session, _) = super::super::work_visualizer::fixture();
    assert_eq!(
        super::snapshots(&state),
        0,
        "the fixture reads no budget clock"
    );
    (state, session)
}

#[test]
fn an_empty_queue_drain_without_engram_takes_no_budget_clock() {
    let (state, session) = plain_session();
    let started = state
        .start_next_queued_turn_inner_off_lock(&session, false, false, None)
        .expect("the drain succeeds");
    assert!(started.is_none(), "an empty queue starts nothing");
    assert_eq!(
        super::snapshots(&state),
        0,
        "an empty drain has no clock work"
    );
}

#[test]
fn a_capacity_ack_without_a_retry_record_takes_no_budget_clock() {
    let (state, session) = plain_session();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let before =
            serde_json::to_value(PersistedSessionRecord::from_record(&inner.sessions[index]))
                .unwrap();
        state.request_provider_capacity_ack_locked(&mut inner, index);
        assert!(inner.sessions[index].provider_capacity_retry.is_none());
        assert_eq!(
            serde_json::to_value(PersistedSessionRecord::from_record(&inner.sessions[index]))
                .unwrap(),
            before
        );
    }
    assert_eq!(super::snapshots(&state), 0);
}

#[test]
fn a_capacity_tick_without_scheduled_retries_takes_no_budget_clock() {
    let (state, session) = plain_session();
    state.provider_capacity_retry_tick(chrono::Utc::now());
    assert_eq!(super::snapshots(&state), 0);
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(record.provider_capacity_retry.is_none());
    assert!(record.queued_prompts.is_empty());
}

#[test]
fn a_stale_ordinary_queue_owner_neither_reads_a_clock_nor_consumes_its_successor() {
    let label = "budget-stale-ordinary-owner";
    let (state, receiver) =
        crate::tests::delegation_support::test_app_state_with_delegation_codex_runtime(label);
    state.select_test_scripted_engram_budget_clock();
    let root = state.test_temp_root.as_ref().unwrap().path().join(label);
    fs::create_dir_all(&root).unwrap();
    let project = create_test_project(&state, &root, "Ordinary stale queue owner");
    let session = create_test_project_session(&state, Agent::Codex, &project, &root);
    fs::write(root.join(".engram-project"), format!("{project}\n")).unwrap();
    let transport = ScriptedEngramControlTransport::new([]);
    state.install_control_test_transport(transport.clone());
    {
        let mut inner = state.inner.lock().unwrap();
        inner
            .projects
            .iter_mut()
            .find(|entry| entry.id == project)
            .unwrap()
            .engram = Some(EngramProjectSettings {
            enabled: true,
            execution_observability: false,
            home: Some(root.to_string_lossy().into_owned()),
            binary_path: Some(
                root.join("missing-engram.exe")
                    .to_string_lossy()
                    .into_owned(),
            ),
            ..EngramProjectSettings::default()
        });
        inner.engram_declared_project_ids.insert(project.clone());
        inner.engram_declaration_checked_project_ids.insert(project);
        let record = inner.session_mut(&session).unwrap();
        record.engram.context_nudge_pending = false;
        record.set_auto_dispatch_blocked(true);
        state.commit_locked(&mut inner).unwrap();
    }
    let mut ids = Vec::new();
    let mut owner = None;
    for text in ["old head", "independent successor"] {
        let mut inner = state.inner.lock().unwrap();
        let id = inner.next_message_id();
        queue_prompt_on_record_with_source(
            inner.session_mut(&session).unwrap(),
            PendingPrompt {
                engram_interrupted: false,
                is_engram_retained: false,
                attachments: Vec::new(),
                id: id.clone(),
                timestamp: stamp_now(),
                text: text.into(),
                expanded_text: None,
                source: None,
            },
            Vec::new(),
            QueuedPromptSource::User,
        );
        state.commit_locked(&mut inner).unwrap();
        if ids.is_empty() {
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            owner = QueuedTurnOwner::capture(record);
        }
        ids.push(id);
    }
    state.cancel_queued_prompt(&session, &ids[0]).unwrap();
    let before = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap()
    };
    let baseline = super::snapshots(&state);
    let result = state
        .resume_local_session_queue_inner(
            &session,
            Some(owner.expect("the original ordinary head has an owner")),
            None,
        )
        .unwrap();
    assert!(result.is_none(), "the stale owner authorizes no Resume");
    assert_eq!(super::snapshots(&state), baseline);
    let after = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(record.queued_prompts[0].pending_prompt.id, ids[1]);
        assert!(record.orchestrator_auto_dispatch_blocked);
        serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap()
    };
    assert_eq!(after, before);
    while let Ok(command) = receiver.try_recv() {
        assert!(
            !matches!(command, CodexRuntimeCommand::Prompt { .. }),
            "unexpected provider handoff"
        );
    }
    assert!(
        transport.requests().is_empty(),
        "ordinary queue service must not call control"
    );
    state.shutdown_persist_blocking();
}
