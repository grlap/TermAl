//! Nonzero-watermark SQLite cuts retain exact passive mirrors and ledger units.
use super::*;

struct CheckpointDeltaWriter {
    _rx: mpsc::Receiver<PersistRequest>,
    inner: Arc<StateMutex<StateInner>>,
    path: PathBuf,
    cache: SqlitePersistConnectionCache,
    batch: PersistFenceBatch,
    watermark: u64,
}

impl CheckpointDeltaWriter {
    fn attach(state: &mut AppState) -> Self {
        state.shutdown_persist_blocking();
        let (tx, rx) = mpsc::channel();
        state.persist_tx = tx;
        let mut writer = Self {
            _rx: rx,
            inner: state.inner.clone(),
            path: state.persistence_path.as_ref().clone(),
            cache: SqlitePersistConnectionCache::new(),
            batch: PersistFenceBatch::default(),
            watermark: 0,
        };
        writer.write();
        assert!(writer.watermark > 0);
        writer
    }

    fn write(&mut self) {
        let delta = collect_persist_delta_from_shared_state(&self.inner, self.watermark);
        persist_delta_with_fences(&mut self.cache, &self.path, &delta, &mut self.batch).unwrap();
        self.watermark = delta.watermark;
    }
}

fn checkpoint_sql_cut(path: &FsPath) -> (PersistMetadataUnits, Vec<(String, String)>, u64) {
    let connection = rusqlite::Connection::open(path).unwrap();
    let rows = connection
        .prepare("SELECT id,value_json FROM sessions ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    (
        read_metadata_units(&connection).unwrap(),
        rows,
        metadata_unit_generation(&connection).unwrap(),
    )
}

fn assert_checkpoint_row_matches_ledger(turn: &OrdinaryRoot) {
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    let row: String = connection
        .query_row(
            "SELECT value_json FROM sessions WHERE id=?1",
            [&turn.session],
            |row| row.get(0),
        )
        .unwrap();
    let stored: Value = serde_json::from_str(&row).unwrap();
    let inner = turn.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&turn.session).unwrap()];
    assert_eq!(
        stored["engramCheckpointOperation"],
        record.engram.historical_control["engramCheckpointOperation"]
    );
    let metadata = read_metadata_for_fence(&connection, &["engramSessionObligations"])
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&metadata.engram_session_obligations).unwrap(),
        serde_json::to_value(&inner.engram_session_obligations).unwrap()
    );
    validate_sqlite_checkpoint_mirrors(&connection, &inner.engram_session_obligations).unwrap();
}

fn enqueue_selected_row(inner: &mut StateInner, session: &str) {
    let id = inner.next_message_id();
    queue_prompt_on_record(
        inner.session_mut(session).unwrap(),
        PendingPrompt {
            id,
            timestamp: stamp_now(),
            text: "Selected ordinary persistence image".into(),
            expanded_text: None,
            source: None,
            attachments: Vec::new(),
            engram_interrupted: false,
            is_engram_retained: false,
        },
        Vec::new(),
    );
}

fn selected_row_fence(turn: &OrdinaryRoot) -> PersistFenceTarget {
    let inner = turn.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&turn.session).unwrap()];
    PersistFenceTarget::OrdinarySessionRow {
        session_id: turn.session.clone(),
        content: ordinary_session_row_persisted_content(&PersistedSessionRecord::from_record(
            record,
        )),
    }
}

#[test]
fn project_containment_checkpoint_review_cut_sparse_capture_keeps_ledger_eager() {
    for cold_cache in [false, true] {
        let (mut turn, mut operation) = passive_checkpoint_turn("checkpoint-sparse-cut");
        {
            let mut inner = turn.state.inner.lock().unwrap();
            inner
                .ignored_discovered_codex_thread_ids
                .insert("retained-unrelated".into());
            turn.state.commit_locked(&mut inner).unwrap();
        }
        let mut writer = CheckpointDeltaWriter::attach(&mut turn.state);
        if cold_cache {
            writer.cache.invalidate();
        }
        accepted_history(&mut operation);
        {
            let mut inner = turn.state.inner.lock().unwrap();
            install_historical_checkpoint(&mut inner, &turn.session, &operation);
            enqueue_selected_row(&mut inner, &turn.session);
            turn.state.commit_locked(&mut inner).unwrap();
            let full = PersistedState::metadata_from_inner(&inner);
            let sparse = PersistedState::metadata_delta_from_inner(&inner);
            assert!(sparse.metadata_collection_is_omitted("ignoredDiscoveredCodexThreadIds"));
            assert!(sparse.metadata_collection_is_omitted("projects"));
            let expected = project_metadata_units(&full)
                .unwrap()
                .into_iter()
                .filter(|((field, _), _)| field == "engramSessionObligations")
                .collect::<PersistMetadataUnits>();
            assert_eq!(expected.len(), 4);
            for image in [full, sparse.clone(), sparse.metadata_only()] {
                assert!(!image.metadata_collection_is_omitted("engramSessionObligations"));
                assert_eq!(
                    project_metadata_units(&image)
                        .unwrap()
                        .into_iter()
                        .filter(|((field, _), _)| field == "engramSessionObligations")
                        .collect::<PersistMetadataUnits>(),
                    expected
                );
            }
        }
        let target = selected_row_fence(&turn);
        let connection = rusqlite::Connection::open(&writer.path).unwrap();
        assert!(!target.is_already_durable(&connection).unwrap());
        drop(connection);
        let clock = turn.state.engram_budget_clock();
        let (fence, waiter) =
            PersistFence::new_with_clock(target, clock.now() + Duration::from_secs(20), clock);
        writer.batch.accept(PersistRequest::Fence(Box::new(fence)));
        writer.write();
        waiter.wait().unwrap();
        assert_checkpoint_row_matches_ledger(&turn);
        let loaded = load_state(&writer.path).unwrap().unwrap();
        assert!(
            loaded
                .ignored_discovered_codex_thread_ids
                .contains("retained-unrelated")
        );
        assert_passive_checkpoint(&turn, &operation);
    }
}

#[test]
fn project_containment_checkpoint_review_cut_index_decodes_descriptor_and_member_changes() {
    let field = "engramSessionObligations".to_owned();
    let empty = metadata_value_to_units(
        json!({"engramSessionObligations":EngramSessionObligationLedger::empty()}),
    )
    .unwrap();
    assert_eq!(empty.len(), 4);
    assert_eq!(
        empty.get(&(field.clone(), String::new())),
        Some(&("object".into(), "3".into()))
    );
    assert!(
        checkpoint_index_from_units(&empty)
            .unwrap()
            .unwrap()
            .is_empty()
    );
    for member in ["/schemaVersion", "/entries", "/checkpointOperations"] {
        let mut missing = empty.clone();
        missing.remove(&(field.clone(), member.into()));
        assert!(
            checkpoint_index_from_units(&missing).is_err(),
            "missing {member} must not default"
        );
    }
    let (turn, mut operation) = passive_checkpoint_turn("checkpoint-index-members");
    let previous = metadata_value_to_units(json!({"engramSessionObligations":
        turn.state.inner.lock().unwrap().engram_session_obligations}))
    .unwrap();
    accepted_history(&mut operation);
    let desired = metadata_value_to_units(json!({"engramSessionObligations":
        EngramSessionObligationLedger { schema_version: 1, entries: Vec::new(), checkpoint_operations: vec![operation.clone()] }})).unwrap();
    assert_eq!(
        previous.get(&(field.clone(), String::new())),
        desired.get(&(field.clone(), String::new()))
    );
    assert_ne!(
        previous.get(&(field.clone(), "/checkpointOperations".into())),
        desired.get(&(field, "/checkpointOperations".into()))
    );
    let (index, owners) = checkpoint_write_cut_index(&previous, &desired, None).unwrap();
    assert_eq!(owners, BTreeSet::from([turn.session.clone()]));
    assert_ne!(
        index.unwrap().get(&operation.original.id),
        checkpoint_index_from_units(&previous)
            .unwrap()
            .unwrap()
            .get(&operation.original.id)
    );
    turn.assert_no_control();
}

fn checkpoint_collection_cut_survives_deferral(keep_earlier_row: bool, cold_cache: bool) {
    let (mut turn, mut operation) = passive_checkpoint_turn("checkpoint-deferred-cut");
    let other = create_test_project_session(&turn.state, Agent::Codex, &turn.project, &turn.root);
    let mut writer = CheckpointDeltaWriter::attach(&mut turn.state);
    let before = checkpoint_sql_cut(&writer.path);
    if cold_cache {
        writer.cache.invalidate();
    }
    let delta = {
        let mut inner = turn.state.inner.lock().unwrap();
        inner.session_mut(&turn.session).unwrap().session.preview = "first selected image".into();
        inner.session_mut(&other).unwrap().session.preview = "other selected image".into();
        turn.state.commit_locked(&mut inner).unwrap();
        let first_plan = inner.collect_persist_delta_plan(writer.watermark);
        let first = if keep_earlier_row {
            inner.session_mut(&other).unwrap().session.preview = "sibling changed".into();
            inner.materialize_persist_delta(first_plan)
        } else {
            inner.session_mut(&turn.session).unwrap().session.preview = "owner changed".into();
            inner.materialize_persist_delta(first_plan)
        };
        assert!(!first.deferred_session_ids.is_empty());
        // A validated imported newer historical image, not a producer reply.
        accepted_history(&mut operation);
        install_historical_checkpoint(&mut inner, &turn.session, &operation);
        enqueue_selected_row(&mut inner, &turn.session);
        turn.state.commit_locked(&mut inner).unwrap();
        let retry_plan = inner.collect_persist_delta_plan(first.watermark);
        inner.session_mut(&turn.session).unwrap().session.preview =
            "owner changed after retry".into();
        turn.state.commit_locked(&mut inner).unwrap();
        let retry = inner.materialize_persist_delta(retry_plan);
        assert!(retry.deferred_session_ids.contains(&turn.session));
        merge_persist_delta_passes(first, retry)
    };
    let target = selected_row_fence(&turn);
    let clock = turn.state.engram_budget_clock();
    let (fence, waiter) =
        PersistFence::new_with_clock(target.clone(), clock.now() + Duration::from_secs(20), clock);
    writer.batch.accept(PersistRequest::Fence(Box::new(fence)));
    let write =
        persist_delta_with_fences(&mut writer.cache, &writer.path, &delta, &mut writer.batch);
    assert!(
        write
            .unwrap_err()
            .to_string()
            .contains("incoherent checkpoint persistence cut")
    );
    assert!(waiter.completion.poll_at(waiter.now()).is_none());
    assert_eq!(
        checkpoint_sql_cut(&writer.path),
        before,
        "units, rows and generation roll back together"
    );
    assert_eq!(
        delta
            .metadata
            .metadata_capture
            .applied
            .load(std::sync::atomic::Ordering::Acquire),
        0
    );
    assert_eq!(
        delta
            .metadata
            .metadata_capture
            .known
            .load(std::sync::atomic::Ordering::Acquire),
        delta.metadata.metadata_capture.predecessor
    );
    assert!(writer.cache.metadata_units.is_none());
    load_state(&writer.path).unwrap().unwrap();
    writer.write();
    waiter.wait().unwrap();
    assert_checkpoint_row_matches_ledger(&turn);
    let connection = rusqlite::Connection::open(&writer.path).unwrap();
    assert!(target.is_already_durable(&connection).unwrap());
    assert_passive_checkpoint(&turn, &operation);
}

#[test]
fn project_containment_checkpoint_review_cut_twice_deferred_reload_is_coherent() {
    for cold_cache in [false, true] {
        checkpoint_collection_cut_survives_deferral(false, cold_cache);
    }
}

#[test]
fn project_containment_checkpoint_review_cut_earlier_row_and_later_ledger_reload_is_coherent() {
    for cold_cache in [false, true] {
        checkpoint_collection_cut_survives_deferral(true, cold_cache);
    }
}

#[test]
fn project_containment_checkpoint_review_cut_full_writer_rejects_mixed_snapshot() {
    let (turn, mut operation) = passive_checkpoint_turn("checkpoint-full-cut");
    let before = checkpoint_sql_cut(turn.state.persistence_path.as_path());
    let mut snapshot = PersistedState::from_inner(&turn.state.inner.lock().unwrap());
    accepted_history(&mut operation);
    snapshot.engram_session_obligations.checkpoint_operations = vec![operation];
    let error = persist_persisted_state_to_sqlite(turn.state.persistence_path.as_path(), &snapshot)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("incoherent checkpoint persistence cut")
    );
    assert_eq!(
        checkpoint_sql_cut(turn.state.persistence_path.as_path()),
        before
    );
    assert_eq!(
        snapshot
            .metadata_capture
            .applied
            .load(std::sync::atomic::Ordering::Acquire),
        0
    );
    load_state(turn.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    persist_locked_state_to_sqlite(
        turn.state.persistence_path.as_path(),
        &turn.state.inner.lock().unwrap(),
    )
    .unwrap();
    assert_checkpoint_row_matches_ledger(&turn);
    turn.assert_no_control();
}

#[test]
fn project_containment_checkpoint_review_cut_work_ignores_unrelated_rows() {
    for unrelated in [0, 1000] {
        let (mut turn, _) = passive_checkpoint_turn("checkpoint-unrelated-rows");
        let mut writer = CheckpointDeltaWriter::attach(&mut turn.state);
        let baseline_count = load_state(&writer.path).unwrap().unwrap().sessions.len();
        let mut historical = turn.record(PersistedSessionRecord::from_record);
        historical
            .historical_control
            .remove("engramCheckpointOperation");
        let mut connection = rusqlite::Connection::open(&writer.path).unwrap();
        let transaction = connection.transaction().unwrap();
        for ordinal in 0..unrelated {
            historical.session.id = format!("unrelated-history-{ordinal}");
            transaction
                .execute(
                    "INSERT INTO sessions(id,value_json) VALUES(?1,?2)",
                    rusqlite::params![
                        historical.session.id,
                        serde_json::to_string(&historical).unwrap()
                    ],
                )
                .unwrap();
        }
        transaction.commit().unwrap();
        turn.edit(|record| record.session.preview = "only selected row".into());
        TEST_CHECKPOINT_WRITE_CUT_WORK.with(|work| work.set((0, 0)));
        writer.write();
        assert_eq!(
            TEST_CHECKPOINT_WRITE_CUT_WORK.with(|work| work.get()),
            (0, 1),
            "a warm unchanged ledger probes only its changed row at population {unrelated}"
        );
        assert_checkpoint_row_matches_ledger(&turn);
        assert_eq!(
            load_state(&writer.path).unwrap().unwrap().sessions.len(),
            unrelated + baseline_count
        );
        turn.assert_no_control();
    }
}

#[test]
fn project_containment_checkpoint_review_cut_sync_writer_invalidates_cached_index_generation() {
    let (mut turn, mut operation) = passive_checkpoint_turn("checkpoint-cache-generation");
    let mut writer = CheckpointDeltaWriter::attach(&mut turn.state);
    let previous_index = writer
        .cache
        .metadata_units
        .as_ref()
        .unwrap()
        .checkpoint_index
        .clone()
        .unwrap();
    let original_index = previous_index.as_ref().clone();
    accepted_history(&mut operation);
    {
        let mut inner = turn.state.inner.lock().unwrap();
        install_historical_checkpoint(&mut inner, &turn.session, &operation);
        turn.state.commit_locked(&mut inner).unwrap();
        persist_locked_state_to_sqlite(turn.state.persistence_path.as_path(), &inner).unwrap();
    }
    assert!(
        checkpoint_sql_cut(&writer.path).2
            > writer.cache.metadata_units.as_ref().unwrap().generation
    );
    TEST_CHECKPOINT_WRITE_CUT_WORK.with(|work| work.set((0, 0)));
    writer.write();
    assert_eq!(
        TEST_CHECKPOINT_WRITE_CUT_WORK.with(|work| work.get()),
        (1, 1)
    );
    assert_eq!(
        previous_index.as_ref(),
        &original_index,
        "shared predecessor indices stay immutable"
    );
    assert_checkpoint_row_matches_ledger(&turn);
    assert_passive_checkpoint(&turn, &operation);
}

#[test]
fn project_containment_checkpoint_watermark_mailbox_coalescing_survives_reload() {
    let (mut turn, original) = passive_checkpoint_turn("checkpoint-mailbox-watermark");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let mut writer = CheckpointDeltaWriter::attach(&mut turn.state);
    for sequence in [7, 9] {
        assert!(matches!(
            turn.state
                .dispatch_turn(
                    &turn.session,
                    SendMessageRequest {
                        text: format!("Mailbox wake {sequence}"),
                        expanded_text: None,
                        attachments: Vec::new(),
                        source_session_id: Some(turn.session.clone()),
                        source_mailbox: Some(MailboxMessageSource {
                            mailbox_id: "watermark-mailbox".into(),
                            message_id: format!("wake-{sequence}"),
                            sequence,
                            unread_count: sequence,
                        }),
                    }
                )
                .unwrap(),
            DispatchTurnResult::Queued
        ));
        writer.write();
    }
    let loaded = load_state(&writer.path).unwrap().unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&turn.session).unwrap()];
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(
        record.queued_prompts[0].pending_prompt.text,
        "Mailbox wake 9"
    );
    assert_eq!(
        record.queued_prompts[0]
            .pending_prompt
            .source
            .as_ref()
            .unwrap()
            .mailbox
            .as_ref()
            .unwrap()
            .sequence,
        9
    );
    assert_checkpoint_row_matches_ledger(&turn);
    assert_passive_checkpoint(&turn, &original);
    turn.assert_no_prompt();
}
