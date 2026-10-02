//! Public body sequences describe transcript content independently of global
//! revisions and metadata stamps. Exercise production mutations and projections.
use super::remote::remote_text_message as text_message;
use super::*;

fn next_delta(receiver: &mut broadcast::Receiver<String>) -> Value {
    serde_json::from_str(
        &receiver
            .try_recv()
            .expect("body delta should publish synchronously"),
    )
    .unwrap()
}

fn sequence(state: &AppState, id: &str) -> u64 {
    state
        .get_session_tail(id, 20)
        .unwrap()
        .session
        .body_seq
        .unwrap()
}

fn card_snapshot() -> TestRunCardSnapshot {
    TestRunCardSnapshot {
        run_id: "sequence-test".to_owned(),
        worktree: "fixture".to_owned(),
        run_dir: "fixture".to_owned(),
        preset: TestRunPreset::Focused,
        detached: Some(false),
        state: TestRunState::Running,
        unknown_reason: None,
        interrupted: false,
        current_stage: None,
        started_at: None,
        ended_at: None,
        exit_code: None,
        stages: Vec::new(),
        stages_omitted: 0,
        error: None,
        error_truncated: false,
        failure: None,
        command: None,
        command_truncated: false,
    }
}

#[test]
fn body_sequence_tracks_every_local_body_delta_and_read_to_next_delta() {
    let state = test_app_state();
    let id = test_session_id(&state, Agent::Codex);
    assert_eq!(sequence(&state, &id), 0);
    let mut receiver = state.subscribe_delta_events();
    state
        .push_message(&id, text_message("text", "first"))
        .unwrap();
    let mut expected = 1;
    let first = next_delta(&mut receiver);
    assert_eq!(first["sessionSeq"], expected);
    assert_eq!(first["bodySeqEpoch"], state.server_instance_id);
    assert_eq!(first["type"], "messageCreated");
    state.append_text_delta(&id, "text", " second").unwrap();
    state
        .replace_text_message(&id, "text", "replacement")
        .unwrap();
    state
        .upsert_command_message(&id, "command", "echo ok", "", CommandStatus::Running)
        .unwrap();
    state
        .upsert_command_message(&id, "command", "echo ok", "ok", CommandStatus::Success)
        .unwrap();
    state
        .upsert_parallel_agents_message(&id, "parallel", Vec::new())
        .unwrap();
    state
        .upsert_parallel_agents_message(
            &id,
            "parallel",
            vec![ParallelAgentProgress {
                id: "child".to_owned(),
                title: "child".to_owned(),
                detail: None,
                source: ParallelAgentSource::Tool,
                status: ParallelAgentStatus::Running,
            }],
        )
        .unwrap();
    for kind in [
        "textDelta",
        "textReplace",
        "messageCreated",
        "commandUpdate",
        "messageCreated",
        "parallelAgentsUpdate",
    ] {
        expected += 1;
        let event = next_delta(&mut receiver);
        assert_eq!(event["type"], kind);
        assert_eq!(event["sessionSeq"], expected);
        assert_eq!(event["bodySeqEpoch"], state.server_instance_id);
    }
    // Interaction/lifecycle updates use the shared MessageUpdated producer.
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        if let Message::Text { text, .. } = &mut record.session.messages[0] {
            *text = "updated".to_owned();
        }
        let updates = message_updated_delta_parts_for_indices(record, vec![0]);
        let revision = state.commit_persisted_delta_locked(&mut inner).unwrap();
        state.publish_message_updated_delta_parts(&inner, revision, updates);
    }
    expected += 1;
    let updated = next_delta(&mut receiver);
    assert_eq!(updated["type"], "messageUpdated");
    assert_eq!(updated["sessionSeq"], expected);
    assert_eq!(updated["bodySeqEpoch"], state.server_instance_id);
    {
        let mut inner = state.inner.lock().unwrap();
        let mut card = card_snapshot();
        let created = create_test_run_card_locked(&mut inner, &id, card.clone(), None).unwrap();
        let revision = state.commit_persisted_delta_locked(&mut inner).unwrap();
        state.publish_delta_locked(
            &inner,
            created.into_event(revision, &state.server_instance_id),
        );
        card.state = TestRunState::Passed;
        let updated =
            update_test_run_card_locked(&mut inner, &card.run_id.clone(), card, None).unwrap();
        let revision = state.commit_persisted_delta_locked(&mut inner).unwrap();
        state.publish_delta_locked(
            &inner,
            updated.into_event(revision, &state.server_instance_id),
        );
    }
    for kind in ["messageCreated", "testRunCardUpdated"] {
        expected += 1;
        let event = next_delta(&mut receiver);
        assert_eq!(event["type"], kind);
        assert_eq!(event["sessionSeq"], expected);
        assert_eq!(event["bodySeqEpoch"], state.server_instance_id);
    }
    let page = state
        .get_session_history(&id, None, None, None, Some(0), false, 20)
        .unwrap();
    assert_eq!(page.body_seq, Some(expected));
    assert_eq!(
        page.body_seq_epoch.as_deref(),
        Some(state.server_instance_id.as_str())
    );
    assert_eq!(sequence(&state, &id), expected);
    assert_eq!(
        state.get_session(&id).unwrap().session.body_seq,
        Some(expected)
    );
    assert_eq!(
        state
            .snapshot()
            .sessions
            .iter()
            .find(|s| s.id == id)
            .unwrap()
            .body_seq,
        Some(expected)
    );
    state.append_text_delta(&id, "text", " after read").unwrap();
    assert_eq!(next_delta(&mut receiver)["sessionSeq"], expected + 1);
}

#[test]
fn body_sequence_metadata_and_other_sessions_do_not_advance_it() {
    let state = test_app_state();
    let id = test_session_id(&state, Agent::Codex);
    state
        .push_message(&id, text_message("text", "body"))
        .unwrap();
    let before = sequence(&state, &id);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.session.name = "renamed".to_owned();
        record.session.status = SessionStatus::Active;
        record.set_auto_dispatch_blocked(true);
        state.commit_locked(&mut inner).unwrap();
    }
    let other = test_session_id(&state, Agent::Codex);
    state
        .push_message(&other, text_message("other", "other body"))
        .unwrap();
    assert_eq!(sequence(&state, &id), before);
}

#[test]
fn body_sequence_rollback_leaves_a_gap_before_the_next_delta() {
    let state = test_app_state();
    let id = test_session_id(&state, Agent::Codex);
    state
        .push_message(&id, text_message("text", "before"))
        .unwrap();
    let before = sequence(&state, &id);
    let mut receiver = state.subscribe_delta_events();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        replace_session_messages_on_record(
            inner.session_mut_by_index(index).unwrap(),
            vec![text_message("text", "rolled back")],
            None,
        );
        state.commit_locked(&mut inner).unwrap();
    }
    let page = state
        .get_session_history(&id, None, None, None, Some(0), false, 20)
        .unwrap();
    assert_eq!(page.body_seq, Some(before + 1));
    assert_eq!(
        page.body_seq_epoch.as_deref(),
        Some(state.server_instance_id.as_str())
    );
    assert!(matches!(&page.messages[0], Message::Text { text, .. } if text == "rolled back"));
    assert!(
        receiver.try_recv().is_err(),
        "bulk replacement has no narrow body delta"
    );
    state.append_text_delta(&id, "text", " next").unwrap();
    assert_eq!(next_delta(&mut receiver)["sessionSeq"], before + 2);
}

#[test]
fn body_sequence_same_revision_batch_is_contiguous_in_publication_order() {
    let state = test_app_state();
    let id = test_session_id(&state, Agent::Codex);
    state
        .push_message(&id, text_message("existing", "before"))
        .unwrap();
    let before = sequence(&state, &id);
    let mut receiver = state.subscribe_delta_events();
    let revision = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        let first = push_message_on_record(record, text_message("one", "one"));
        let second = push_message_on_record(record, text_message("two", "two"));
        let creates = message_created_delta_parts_for_indices(record, vec![first, second]);
        if let Message::Text { text, .. } = &mut record.session.messages[0] {
            *text = "after".to_owned();
        }
        let updates = message_updated_delta_parts_for_indices(record, vec![0]);
        let revision = state.commit_locked(&mut inner).unwrap();
        state.publish_message_created_delta_parts(&inner, revision, creates);
        state.publish_message_updated_delta_parts(&inner, revision, updates);
        revision
    };
    for offset in 1..=3 {
        let event = next_delta(&mut receiver);
        assert_eq!(event["revision"], revision);
        assert_eq!(event["sessionSeq"], before + offset);
    }
    assert_eq!(sequence(&state, &id), before + 3);
}

fn proxy_fixture(body_seq: Option<u64>) -> (AppState, RemoteConfig, Session, String) {
    let state = test_app_state();
    let remote = super::remote_delta_replay::local_replay_test_remote();
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote",
        "remote-project-1",
    );
    let mut session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .remove(0);
    session.messages = vec![text_message("text", "seed")];
    session.message_count = 1;
    session.messages_loaded = true;
    session.session_mutation_stamp = Some(10);
    session.body_seq = body_seq;
    session.body_seq_epoch = body_seq.map(|_| "upstream-instance".to_owned());
    let mut inner = state.inner.lock().unwrap();
    let id = upsert_remote_proxy_session_record(&mut inner, &remote.id, &session, None);
    state.commit_locked(&mut inner).unwrap();
    drop(inner);
    (state, remote, session, id)
}

fn remote_replace(session: &Session, body_seq: Option<u64>, text: &str) -> DeltaEvent {
    DeltaEvent::TextReplace {
        revision: 100,
        session_id: session.id.clone(),
        message_id: "text".to_owned(),
        message_index: 0,
        message_count: 1,
        text: text.to_owned(),
        preview: None,
        session_mutation_stamp: Some(11),
        session_seq: body_seq,
        body_seq_epoch: session.body_seq_epoch.clone(),
    }
}

#[test]
fn body_sequence_proxy_preserves_upstream_values_and_rejects_a_gap() {
    let (state, remote, session, id) = proxy_fixture(Some(40));
    let mut receiver = state.subscribe_delta_events();
    for value in [41, 42] {
        state
            .apply_remote_delta_event(
                &remote.id,
                remote_replace(&session, Some(value), "same body"),
            )
            .unwrap();
        assert_eq!(next_delta(&mut receiver)["sessionSeq"], value);
        assert_eq!(sequence(&state, &id), value);
    }
    assert!(
        state
            .apply_remote_delta_event(&remote.id, remote_replace(&session, Some(44), "gap"))
            .is_err()
    );
    assert_eq!(sequence(&state, &id), 42);
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
    assert!(
        matches!(&record.session.messages[0], Message::Text { text, .. } if text == "same body")
    );
}

#[test]
fn body_sequence_proxy_summary_cannot_certify_unapplied_bodies() {
    let (state, remote, mut session, id) = proxy_fixture(Some(40));
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&id).unwrap();
    session.body_seq = Some(45); // Same metadata stamp and count, newer body head.
    apply_remote_session_summary_to_record(
        &mut inner.sessions[index],
        &remote.id,
        None,
        &test_state_session_summary_from_session(&session),
    );
    let record = &inner.sessions[index];
    assert_eq!(
        AppState::wire_session_summary_from_record(&state.server_instance_id, record).body_seq,
        Some(45)
    );
    assert_eq!(
        AppState::wire_session_from_record(&state.server_instance_id, record).body_seq,
        None
    );
    assert!(record.session.messages.is_empty());
    apply_remote_session_to_record(&mut inner.sessions[index], &remote.id, None, &session);
    assert_eq!(
        AppState::wire_session_from_record(&state.server_instance_id, &inner.sessions[index])
            .body_seq,
        Some(45)
    );
}

#[test]
fn body_sequence_older_peer_omits_sequence_on_reads_and_deltas() {
    let (state, remote, session, id) = proxy_fixture(None);
    let mut receiver = state.subscribe_delta_events();
    state
        .apply_remote_delta_event(&remote.id, remote_replace(&session, None, "legacy"))
        .unwrap();
    assert!(next_delta(&mut receiver).get("sessionSeq").is_none());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
    assert!(
        serde_json::to_value(AppState::wire_session_from_record(
            &state.server_instance_id,
            record
        ))
        .unwrap()
        .get("bodySeq")
        .is_none()
    );
    assert!(
        serde_json::to_value(AppState::wire_session_summary_from_record(
            &state.server_instance_id,
            record
        ))
        .unwrap()
        .get("bodySeq")
        .is_none()
    );
}

#[test]
fn body_sequence_is_reinitialized_when_local_state_is_loaded() {
    let state = test_app_state();
    let id = test_session_id(&state, Agent::Codex);
    state
        .push_message(&id, text_message("text", "persisted body"))
        .unwrap();
    assert_eq!(sequence(&state, &id), 1);
    let inner = state.inner.lock().unwrap();
    let loaded = PersistedState::from_inner(&inner).into_inner().unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&id).unwrap()];
    let restarted = test_app_state();
    let read = AppState::wire_session_from_record(&restarted.server_instance_id, record);
    assert_eq!(read.body_seq, Some(0));
    assert_eq!(
        read.body_seq_epoch.as_deref(),
        Some(restarted.server_instance_id.as_str())
    );
    assert_ne!(
        read.body_seq_epoch.as_deref(),
        Some(state.server_instance_id.as_str())
    );
    assert!(
        matches!(&record.session.messages[0], Message::Text { text, .. } if text == "persisted body")
    );
}

#[test]
fn body_sequence_upstream_restart_changes_issuer_behind_unchanged_proxy() {
    let (state, remote, mut session, id) = proxy_fixture(Some(40));
    let before = state.get_session_tail(&id, 20).unwrap();
    session.body_seq = Some(0);
    session.body_seq_epoch = Some("upstream-restarted".to_owned());
    session.messages = vec![text_message("text", "recovered")];
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        apply_remote_session_summary_to_record(
            &mut inner.sessions[index],
            &remote.id,
            None,
            &test_state_session_summary_from_session(&session),
        );
        let summary = AppState::wire_session_summary_from_record(
            &state.server_instance_id,
            &inner.sessions[index],
        );
        assert_eq!(summary.body_seq, Some(0));
        assert_eq!(summary.body_seq_epoch, session.body_seq_epoch);
        assert!(inner.sessions[index].session.messages.is_empty());
        apply_remote_session_to_record(&mut inner.sessions[index], &remote.id, None, &session);
        state.commit_locked(&mut inner).unwrap();
    }
    let after = state.get_session_tail(&id, 20).unwrap();
    assert_eq!(before.server_instance_id, after.server_instance_id);
    assert_ne!(before.session.body_seq_epoch, after.session.body_seq_epoch);
    assert_eq!(after.session.body_seq, Some(0));
    let mut receiver = state.subscribe_delta_events();
    state
        .apply_remote_delta_event(&remote.id, remote_replace(&session, Some(1), "new turn"))
        .unwrap();
    let event = next_delta(&mut receiver);
    assert_eq!(event["sessionSeq"], 1);
    assert_eq!(event["bodySeqEpoch"], "upstream-restarted");
    let mut retired = session.clone();
    retired.body_seq_epoch = Some("upstream-instance".to_owned());
    assert!(
        state
            .apply_remote_delta_event(&remote.id, remote_replace(&retired, Some(2), "retired"))
            .is_err()
    );
    assert_eq!(sequence(&state, &id), 1);
}

#[test]
fn body_sequence_remote_test_card_update_preserves_sequence_and_replay_identity() {
    let (state, remote, mut session, id) = proxy_fixture(Some(40));
    session.messages = vec![Message::TestRun {
        id: "card".to_owned(),
        timestamp: stamp_now(),
        author: Author::System,
        schema_version: 1,
        run: card_snapshot(),
    }];
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        apply_remote_session_to_record(&mut inner.sessions[index], &remote.id, None, &session);
    }
    let mut receiver = state.subscribe_delta_events();
    let mut run = card_snapshot();
    run.state = TestRunState::Passed;
    let event = DeltaEvent::TestRunCardUpdated {
        revision: 100,
        session_id: session.id.clone(),
        message_id: "card".to_owned(),
        message_index: 0,
        message_count: 1,
        preview: "passed".to_owned(),
        session_mutation_stamp: Some(11),
        session_seq: Some(41),
        body_seq_epoch: session.body_seq_epoch.clone(),
        run,
    };
    state
        .apply_remote_delta_event(&remote.id, event.clone())
        .unwrap();
    let published = next_delta(&mut receiver);
    assert_eq!(published["type"], "testRunCardUpdated");
    assert_eq!(published["sessionSeq"], 41);
    assert_eq!(sequence(&state, &id), 41);
    state.apply_remote_delta_event(&remote.id, event).unwrap();
    assert!(
        receiver.try_recv().is_err(),
        "an exact replay publishes no second delta"
    );
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
    assert!(
        matches!(&record.session.messages[0], Message::TestRun { run, .. } if run.state == TestRunState::Passed)
    );
}

#[test]
fn body_sequence_eviction_preserves_counter_and_deleted_ids_are_not_reused() {
    let state = test_app_state();
    let id = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.session.status = SessionStatus::Idle;
        replace_session_messages_on_record(
            record,
            (0..130)
                .map(|n| text_message(&format!("m-{n}"), "body"))
                .collect(),
            None,
        );
        state.commit_locked(&mut inner).unwrap();
        persist_state(state.persistence_path.as_ref(), &inner).unwrap();
        let watermark = inner.last_mutation_stamp;
        inner.trim_persisted_session_tails(watermark, std::slice::from_ref(&id));
        assert_eq!(inner.sessions[index].message_start_index, 66);
    }
    let read = state
        .get_session_history(&id, None, None, None, Some(40), false, 64)
        .unwrap();
    assert_eq!(read.messages.len(), 64);
    assert_eq!(read.body_seq, Some(1));
    assert_eq!(
        read.body_seq_epoch.as_deref(),
        Some(state.server_instance_id.as_str())
    );
    let mut receiver = state.subscribe_delta_events();
    state.append_text_delta(&id, "m-129", " later").unwrap();
    let delta = next_delta(&mut receiver);
    assert_eq!(delta["sessionSeq"], 2);
    assert_eq!(delta["bodySeqEpoch"], state.server_instance_id);
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&id).unwrap();
    inner.remove_session_at(index);
    let replacement = inner.create_session(Agent::Codex, None, "fixture".to_owned(), None, None);
    assert_ne!(replacement.session.id, id);
    assert_eq!(replacement.wire_body_seq(), Some(0));
}

#[test]
fn body_sequence_incomplete_upstream_pairs_are_legacy_on_reads_and_deltas() {
    for (seq, epoch) in [(Some(41), None), (None, Some("unpaired".to_owned()))] {
        let (state, remote, mut session, id) = proxy_fixture(None);
        session.body_seq = seq;
        session.body_seq_epoch = epoch;
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&id).unwrap();
            apply_remote_session_to_record(&mut inner.sessions[index], &remote.id, None, &session);
            let full = AppState::wire_session_from_record(
                &state.server_instance_id,
                &inner.sessions[index],
            );
            assert!(full.body_seq.is_none() && full.body_seq_epoch.is_none());
            apply_remote_session_summary_to_record(
                &mut inner.sessions[index],
                &remote.id,
                None,
                &test_state_session_summary_from_session(&session),
            );
            let summary = AppState::wire_session_summary_from_record(
                &state.server_instance_id,
                &inner.sessions[index],
            );
            assert!(summary.body_seq.is_none() && summary.body_seq_epoch.is_none());
        }
        let mut receiver = state.subscribe_delta_events();
        state
            .apply_remote_delta_event(
                &remote.id,
                remote_replace(&session, seq, "legacy replacement"),
            )
            .unwrap();
        let delta = next_delta(&mut receiver);
        assert!(delta.get("sessionSeq").is_none() && delta.get("bodySeqEpoch").is_none());
        let tail = state.get_session_tail(&id, 20).unwrap();
        assert!(tail.session.body_seq.is_none() && tail.session.body_seq_epoch.is_none());
        assert!(
            matches!(&tail.session.messages[0], Message::Text { text, .. } if text == "legacy replacement")
        );
    }
}

#[test]
fn body_sequence_certified_proxy_rejects_missing_epoch_until_read_repairs_it() {
    let (state, remote, mut session, id) = proxy_fixture(Some(40));
    session.body_seq_epoch = None;
    assert!(
        state
            .apply_remote_delta_event(&remote.id, remote_replace(&session, Some(41), "unpaired"))
            .is_err()
    );
    assert_eq!(sequence(&state, &id), 40);
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&id).unwrap();
    apply_remote_session_to_record(&mut inner.sessions[index], &remote.id, None, &session);
    let repaired =
        AppState::wire_session_from_record(&state.server_instance_id, &inner.sessions[index]);
    assert!(repaired.body_seq.is_none() && repaired.body_seq_epoch.is_none());
    drop(inner);
    state
        .apply_remote_delta_event(
            &remote.id,
            remote_replace(&session, None, "legacy after repair"),
        )
        .unwrap();
}

#[test]
fn body_sequence_loaded_proxy_summary_omits_an_incomplete_saved_pair() {
    for (seq, epoch) in [(Some(41), None), (None, Some("unpaired".to_owned()))] {
        let (state, _, _, id) = proxy_fixture(None);
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        let record = &mut inner.sessions[index];
        record.session.body_seq = seq;
        record.session.body_seq_epoch = epoch;
        let loaded = PersistedSessionRecord::from_record(record)
            .into_record()
            .unwrap();
        let summary =
            AppState::wire_session_summary_from_record(&state.server_instance_id, &loaded);
        let json = serde_json::to_value(summary).unwrap();
        assert!(json.get("bodySeq").is_none() && json.get("bodySeqEpoch").is_none());
    }
}
