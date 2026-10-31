// Exercises the production delta writer after a nonzero saved watermark.
// No synchronous full-save or watermark-zero acknowledgement fallback is used.
use super::*;

#[test]
fn project_containment_checkpoint_review_cut_sparse_capture_keeps_ledger_eager() {
    for cold_cache in [false, true] {
        let (mut state, project, session, transport) = checkpoint_with_unacknowledged_send();
        {
            let mut inner = state.inner.lock().unwrap();
            inner.ignored_discovered_codex_thread_ids.insert("retained-unrelated".into());
            state.commit_locked(&mut inner).unwrap();
        }
        let mut writer = CheckpointDeltaWriter::attach(&mut state);
        if cold_cache { writer.cache.invalidate(); }
        let target = {
            let mut inner = state.inner.lock().unwrap();
            let original = inner.engram_session_obligations.checkpoint_operations[0].clone();
            let ScriptedEngramControlResponse::Reply(Ok(response)) = checkpoint_reply("retained-grant")
            else { panic!("checkpoint reply") };
            let (ledger, _, operation) = prospective_checkpoint_result(
                &inner, &session, &original, Some(&response),
                Some(&EngramCheckpointAttemptResult::Accepted),
            ).unwrap();
            inner.engram_session_obligations = ledger;
            inner.session_mut(&session).unwrap().engram.checkpoint_operation = Some(operation);
            state.commit_locked(&mut inner).unwrap();
            let full = PersistedState::metadata_from_inner(&inner);
            let sparse = PersistedState::metadata_delta_from_inner(&inner);
            assert!(sparse.metadata_collection_is_omitted("ignoredDiscoveredCodexThreadIds"));
            assert!(sparse.metadata_collection_is_omitted("projects"));
            let full_units = project_metadata_units(&full).unwrap();
            let expected = full_units.iter().filter(|((field, _), _)| field == "engramSessionObligations")
                .map(|(key, value)| (key.clone(), value.clone())).collect::<PersistMetadataUnits>();
            assert_eq!(expected.len(), 4, "the complete descriptor and member field is eager");
            for image in [full, sparse.clone(), sparse.metadata_only()] {
                assert!(!image.metadata_collection_is_omitted("engramSessionObligations"));
                let actual = project_metadata_units(&image).unwrap().into_iter()
                    .filter(|((field, _), _)| field == "engramSessionObligations")
                    .collect::<PersistMetadataUnits>();
                assert_eq!(actual, expected);
            }
            PersistFenceTarget::EngramProjectContainment(Box::new(
                EngramProjectContainmentFence::capture(&inner, &project, &[session.clone()]).unwrap(),
            ))
        };
        let clock = state.engram_budget_clock();
        let (fence, waiter) = PersistFence::new_with_clock(
            target, clock.now() + Duration::from_secs(20), clock,
        );
        writer.batch.accept(PersistRequest::Fence(Box::new(fence)));
        writer.write();
        waiter.wait().unwrap();
        assert_checkpoint_row_matches_ledger(&state, &session);
        let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
        assert!(loaded.ignored_discovered_codex_thread_ids.contains("retained-unrelated"));
        assert!(matches!(loaded.engram_session_obligations.checkpoint_operations[0].progress,
            EngramExistingCheckpointProgress::Accepted { .. }));
        assert!(transport.requests().is_empty());
    }
}

#[test]
fn project_containment_checkpoint_review_cut_index_decodes_descriptor_and_member_changes() {
    let field = "engramSessionObligations".to_owned();
    let empty = metadata_value_to_units(json!({
        "engramSessionObligations": EngramSessionObligationLedger::empty(),
    })).unwrap();
    assert_eq!(empty.len(), 4);
    assert_eq!(empty.get(&(field.clone(), String::new())), Some(&("object".into(), "3".into())));
    assert!(checkpoint_index_from_units(&empty).unwrap().unwrap().is_empty());
    for member in ["/schemaVersion", "/entries", "/checkpointOperations"] {
        let mut missing = empty.clone();
        missing.remove(&(field.clone(), member.into()));
        assert!(checkpoint_index_from_units(&missing).is_err(), "missing {member} must not default");
    }
    let (state, _project, session, transport) = checkpoint_with_unacknowledged_send();
    let inner = state.inner.lock().unwrap();
    let original = inner.engram_session_obligations.checkpoint_operations[0].clone();
    let ScriptedEngramControlResponse::Reply(Ok(response)) = checkpoint_reply("retained-grant")
    else { panic!("checkpoint reply") };
    let next = prospective_checkpoint_result(&inner, &session, &original, Some(&response),
        Some(&EngramCheckpointAttemptResult::Accepted)).unwrap().0;
    let previous = metadata_value_to_units(json!({
        "engramSessionObligations": inner.engram_session_obligations,
    })).unwrap();
    let desired = metadata_value_to_units(json!({"engramSessionObligations": next})).unwrap();
    assert_eq!(previous.get(&(field.clone(), String::new())), desired.get(&(field.clone(), String::new())),
        "member changes do not change the root count descriptor");
    assert_ne!(previous.get(&(field.clone(), "/checkpointOperations".into())),
        desired.get(&(field, "/checkpointOperations".into())));
    let (index, owners) = checkpoint_write_cut_index(&previous, &desired, None).unwrap();
    assert_eq!(owners, BTreeSet::from([session]));
    assert_ne!(index.unwrap().get(&original.original.id),
        checkpoint_index_from_units(&previous).unwrap().unwrap().get(&original.original.id));
    assert!(transport.requests().is_empty());
}

fn checkpoint_sql_cut(path: &FsPath) -> (PersistMetadataUnits, Vec<(String, String)>, u64) {
    let connection = rusqlite::Connection::open(path).unwrap();
    let mut statement = connection.prepare("SELECT id,value_json FROM sessions ORDER BY id").unwrap();
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap()
        .collect::<rusqlite::Result<Vec<_>>>().unwrap();
    (read_metadata_units(&connection).unwrap(), rows, metadata_unit_generation(&connection).unwrap())
}

fn checkpoint_collection_cut_survives_deferral(keep_earlier_row: bool, cold_cache: bool) {
    let (mut state, project, session, transport) = checkpoint_with_unacknowledged_send();
    let root = state.inner.lock().unwrap().sessions[0].session.workdir.clone();
    let other = create_test_project_session(&state, Agent::Codex, &project, FsPath::new(&root));
    let mut writer = CheckpointDeltaWriter::attach(&mut state);
    let before = checkpoint_sql_cut(&writer.path);
    if cold_cache { writer.cache.invalidate(); }
    let delta = {
        let mut inner = state.inner.lock().unwrap();
        inner.session_mut(&session).unwrap().session.preview = "first selected image".into();
        inner.session_mut(&other).unwrap().session.preview = "other selected image".into();
        state.commit_locked(&mut inner).unwrap();
        let first_plan = inner.collect_persist_delta_plan(writer.watermark);
        let first = if keep_earlier_row {
            // Defer a sibling so the normal bounded collector needs a retry,
            // while retaining the first checkpoint owner's row snapshot.
            inner.session_mut(&other).unwrap().session.preview = "sibling changed".into();
            inner.materialize_persist_delta(first_plan)
        } else {
            inner.session_mut(&session).unwrap().session.preview = "owner changed".into();
            inner.materialize_persist_delta(first_plan)
        };
        assert!(!first.deferred_session_ids.is_empty());
        let original = inner.engram_session_obligations.checkpoint_operations[0].clone();
        let ScriptedEngramControlResponse::Reply(Ok(response)) = checkpoint_reply("retained-grant")
        else { panic!("checkpoint reply") };
        // Use the real result projection at the producer-reply boundary.
        let (ledger, _, operation) = prospective_checkpoint_result(
            &inner, &session, &original, Some(&response),
            Some(&EngramCheckpointAttemptResult::Accepted),
        ).unwrap();
        assert!(matches!(operation.progress, EngramExistingCheckpointProgress::Accepted { .. }));
        ledger.validate().unwrap();
        inner.engram_session_obligations = ledger;
        inner.session_mut(&session).unwrap().engram.checkpoint_operation = Some(operation);
        state.commit_locked(&mut inner).unwrap();
        let retry_plan = inner.collect_persist_delta_plan(first.watermark);
        // A second real mutation after retry selection defers the same row.
        inner.session_mut(&session).unwrap().session.preview = "owner changed after retry".into();
        state.commit_locked(&mut inner).unwrap();
        let retry = inner.materialize_persist_delta(retry_plan);
        assert!(retry.deferred_session_ids.contains(&session));
        merge_persist_delta_passes(first, retry)
    };
    let target = {
        let inner = state.inner.lock().unwrap();
        PersistFenceTarget::EngramProjectContainment(Box::new(
            EngramProjectContainmentFence::capture(&inner, &project, &[session.clone()]).unwrap(),
        ))
    };
    let clock = state.engram_budget_clock();
    let (fence, waiter) = PersistFence::new_with_clock(
        target.clone(), clock.now() + Duration::from_secs(20), clock,
    );
    writer.batch.accept(PersistRequest::Fence(Box::new(fence)));
    let write = persist_delta_with_fences(&mut writer.cache, &writer.path, &delta, &mut writer.batch);
    assert!(write.as_ref().unwrap_err().to_string().contains("incoherent checkpoint persistence cut"));
    // The refused cut is a collection race the next tick repairs, so the
    // fence stays pending for that retry instead of failing with it.
    assert!(waiter.completion.poll_at(waiter.now()).is_none(), "the refused cut keeps its fence pending");
    assert_eq!(checkpoint_sql_cut(&writer.path), before, "units, rows and generation roll back together");
    assert_eq!(delta.metadata.metadata_capture.applied.load(std::sync::atomic::Ordering::Acquire), 0);
    assert_eq!(delta.metadata.metadata_capture.known.load(std::sync::atomic::Ordering::Acquire),
        delta.metadata.metadata_capture.predecessor, "rejected writes cannot adopt a predecessor");
    assert!(writer.cache.metadata_units.is_none());
    let loaded = load_state(state.persistence_path.as_path());
    assert!(loaded.is_ok(),
        "a deferred checkpoint row must never commit incompatible ledger metadata; write={write:?}, reload={:?}",
        loaded.as_ref().err());
    // Retry through the existing collector with the old watermark, not the
    // rejected immutable cut. The next image must contain the accepted reply.
    writer.write();
    assert!(matches!(waiter.wait(), Ok(())), "the coherent retry resolves the retained fence");
    assert_checkpoint_row_matches_ledger(&state, &session);
    let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    let operation = loaded.sessions[loaded.find_session_index(&session).unwrap()]
        .engram.checkpoint_operation.as_ref().unwrap();
    assert!(matches!(operation.progress, EngramExistingCheckpointProgress::Accepted { .. }));
    assert!(matches!(operation.attempts.last().unwrap().result, Some(EngramCheckpointAttemptResult::Accepted)));
    assert_eq!(serde_json::to_value(operation).unwrap(),
        serde_json::to_value(&loaded.engram_session_obligations.checkpoint_operations[0]).unwrap());
    let connection = rusqlite::Connection::open(&writer.path).unwrap();
    assert!(target.is_already_durable(&connection).unwrap());
    assert!(transport.requests().is_empty());
}

#[test]
fn project_containment_checkpoint_review_cut_twice_deferred_reload_is_coherent() {
    for cold_cache in [false, true] { checkpoint_collection_cut_survives_deferral(false, cold_cache); }
}

#[test]
fn project_containment_checkpoint_review_cut_earlier_row_and_later_ledger_reload_is_coherent() {
    for cold_cache in [false, true] { checkpoint_collection_cut_survives_deferral(true, cold_cache); }
}

#[test]
fn project_containment_checkpoint_review_cut_full_writer_rejects_mixed_snapshot() {
    let (state, _project, session, transport) = checkpoint_with_unacknowledged_send();
    let before = checkpoint_sql_cut(state.persistence_path.as_path());
    let mut snapshot = PersistedState::from_inner(&state.inner.lock().unwrap());
    let original = snapshot.engram_session_obligations.checkpoint_operations[0].clone();
    let ScriptedEngramControlResponse::Reply(Ok(response)) = checkpoint_reply("retained-grant")
    else { panic!("checkpoint reply") };
    snapshot.engram_session_obligations = prospective_checkpoint_result(
        &state.inner.lock().unwrap(), &session, &original, Some(&response),
        Some(&EngramCheckpointAttemptResult::Accepted),
    ).unwrap().0;
    let error = persist_persisted_state_to_sqlite(state.persistence_path.as_path(), &snapshot).unwrap_err();
    assert!(error.to_string().contains("incoherent checkpoint persistence cut"));
    assert_eq!(checkpoint_sql_cut(state.persistence_path.as_path()), before);
    assert_eq!(snapshot.metadata_capture.applied.load(std::sync::atomic::Ordering::Acquire), 0);
    load_state(state.persistence_path.as_path()).unwrap().unwrap();
    persist_locked_state_to_sqlite(state.persistence_path.as_path(), &state.inner.lock().unwrap()).unwrap();
    assert_checkpoint_row_matches_ledger(&state, &session);
    assert!(transport.requests().is_empty());
}

#[test]
fn project_containment_checkpoint_review_cut_work_ignores_unrelated_rows() {
    for unrelated in [0, 1000] {
        let (mut state, _project, session, transport) = checkpoint_with_unacknowledged_send();
        let mut writer = CheckpointDeltaWriter::attach(&mut state);
        // Populate real persisted session rows without N fixture commits. They
        // carry no checkpoint mirror and are not selected by this delta.
        let mut historical = {
            let inner = state.inner.lock().unwrap();
            PersistedSessionRecord::from_record(&inner.sessions[inner.find_session_index(&session).unwrap()])
        };
        historical.engram_checkpoint_operation = None;
        let mut connection = rusqlite::Connection::open(&writer.path).unwrap();
        let transaction = connection.transaction().unwrap();
        for ordinal in 0..unrelated {
            historical.session.id = format!("unrelated-history-{ordinal}");
            transaction.execute("INSERT INTO sessions(id,value_json) VALUES(?1,?2)",
                rusqlite::params![historical.session.id, serde_json::to_string(&historical).unwrap()]).unwrap();
        }
        transaction.commit().unwrap();
        {
            let mut inner = state.inner.lock().unwrap();
            inner.session_mut(&session).unwrap().session.preview = "only selected row".into();
            state.commit_locked(&mut inner).unwrap();
        }
        TEST_CHECKPOINT_WRITE_CUT_WORK.with(|work| work.set((0, 0)));
        writer.write();
        assert_eq!(TEST_CHECKPOINT_WRITE_CUT_WORK.with(|work| work.get()), (0, 1),
            "warm unchanged ledger rebuilds no index and probes only the changed row at population {unrelated}");
        assert_checkpoint_row_matches_ledger(&state, &session);
        let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
        assert_eq!(loaded.sessions.len(), unrelated + 1);
        assert!(transport.requests().is_empty());
    }
}

#[test]
fn project_containment_checkpoint_review_cut_sync_writer_invalidates_cached_index_generation() {
    let (mut state, _project, session, transport) = checkpoint_with_unacknowledged_send();
    let mut writer = CheckpointDeltaWriter::attach(&mut state);
    let previous_index = writer.cache.metadata_units.as_ref().unwrap().checkpoint_index.clone().unwrap();
    let original_index = previous_index.as_ref().clone();
    {
        let mut inner = state.inner.lock().unwrap();
        let original = inner.engram_session_obligations.checkpoint_operations[0].clone();
        let ScriptedEngramControlResponse::Reply(Ok(response)) = checkpoint_reply("retained-grant")
        else { panic!("checkpoint reply") };
        let (ledger, _, operation) = prospective_checkpoint_result(
            &inner, &session, &original, Some(&response),
            Some(&EngramCheckpointAttemptResult::Accepted),
        ).unwrap();
        inner.engram_session_obligations = ledger;
        inner.session_mut(&session).unwrap().engram.checkpoint_operation = Some(operation);
        state.commit_locked(&mut inner).unwrap();
        // A different real writer commits and adopts the next source generation
        // while the background writer still caches the retained predecessor.
        persist_locked_state_to_sqlite(state.persistence_path.as_path(), &inner).unwrap();
    }
    assert!(checkpoint_sql_cut(&writer.path).2 > writer.cache.metadata_units.as_ref().unwrap().generation);
    TEST_CHECKPOINT_WRITE_CUT_WORK.with(|work| work.set((0, 0)));
    writer.write();
    assert_eq!(TEST_CHECKPOINT_WRITE_CUT_WORK.with(|work| work.get()), (1, 1),
        "derive the mismatched cache generation from actual SQL predecessor units");
    assert_eq!(previous_index.as_ref(), &original_index, "never mutate the shared predecessor index");
    assert_checkpoint_row_matches_ledger(&state, &session);
    let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    assert!(matches!(loaded.sessions[loaded.find_session_index(&session).unwrap()]
        .engram.checkpoint_operation.as_ref().unwrap().progress,
        EngramExistingCheckpointProgress::Accepted { .. }));
    assert!(transport.requests().is_empty());
}

struct CheckpointDeltaWriter {
    rx: mpsc::Receiver<PersistRequest>,
    inner: Arc<StateMutex<StateInner>>,
    path: PathBuf,
    cache: SqlitePersistConnectionCache,
    batch: PersistFenceBatch,
    watermark: u64,
}

impl CheckpointDeltaWriter {
    fn attach(state: &mut AppState) -> Self {
        let (tx, rx) = mpsc::channel();
        state.persist_tx = tx;
        let mut writer = Self {
            rx,
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

    fn serve(mut self) -> (usize, Vec<String>) {
        let mut fences = 0;
        let mut unmatched = Vec::new();
        loop {
            let request = phase_sync::receive(&self.rx, "checkpoint delta writer request");
            if matches!(request, PersistRequest::Shutdown) {
                self.write();
                break;
            }
            let is_fence = matches!(request, PersistRequest::Fence(_));
            self.batch.accept(request);
            self.write();
            if is_fence {
                fences += 1;
                if self.batch.has_pending() {
                    let inner = self.inner.lock().unwrap();
                    unmatched.push(format!(
                        "checkpoint fence did not match SQLite at {:?}",
                        inner
                            .engram_session_obligations
                            .checkpoint_operations
                            .iter()
                            .map(|operation| &operation.progress)
                            .collect::<Vec<_>>()
                    ));
                    drop(inner);
                    // Unblock the caller on a causal RED before asserting.
                    // This does not pretend the unmatched content was durable.
                    self.batch.fail(PersistFenceError::WriteFailed(
                        "fixture observed unmatched watermark write".into(),
                    ));
                }
            }
        }
        (fences, unmatched)
    }
}

fn assert_checkpoint_row_matches_ledger(state: &AppState, session: &str) {
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    let row: String = connection
        .query_row(
            "SELECT value_json FROM sessions WHERE id=?1",
            [session],
            |row| row.get(0),
        )
        .unwrap();
    let stored: Value = serde_json::from_str(&row).unwrap();
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    let expected = serde_json::to_value(&record.engram.checkpoint_operation).unwrap();
    assert_eq!(
        stored.get("engramCheckpointOperation"),
        Some(&expected),
        "watermark writer must persist the exact checkpoint mirror"
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

#[test]
fn project_containment_checkpoint_watermark_publication_send_and_successor_reload() {
    let (mut state, _project, session, _root, receiver) = fixture();
    let transport = ScriptedEngramControlTransport::new([
        checkpoint_reply("retained-grant"),
        checkpoint_reply("successor-grant"),
    ]);
    state.install_control_test_transport(transport.clone());
    let writer = CheckpointDeltaWriter::attach(&mut state);
    let worker = std::thread::spawn(move || writer.serve());
    let first = close(&state, &session);
    let second = if matches!(first, EngramCheckpointOutcome::Succeeded) {
        successor_grant(&state, &session, "successor-grant");
        Some(close(&state, &session))
    } else {
        None
    };
    state.persist_tx.send(PersistRequest::Shutdown).unwrap();
    let (fences, unmatched) = worker.join().unwrap();
    assert!(
        unmatched.is_empty(),
        "{unmatched:?}; first={first:?}; second={second:?}"
    );
    assert!(matches!(first, EngramCheckpointOutcome::Succeeded));
    assert!(matches!(second, Some(EngramCheckpointOutcome::Succeeded)));
    assert!(
        fences >= 6,
        "publication, send intent and accepted receipt on both turns"
    );
    assert_eq!(transport.requests().len(), 2);
    assert_checkpoint_row_matches_ledger(&state, &session);
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        loaded
            .engram_session_obligations
            .checkpoint_operations
            .len(),
        1
    );
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_checkpoint_watermark_send_intent_needs_a_new_session_stamp() {
    let (mut state, _project, session, transport) = checkpoint_with_unacknowledged_send();
    let expected = {
        let mut inner = state.inner.lock().unwrap();
        let record = inner.session_mut(&session).unwrap();
        assert!(record.engram.begin_checkpoint(None));
        let operation = record.engram.checkpoint_operation.clone().unwrap();
        state.commit_locked(&mut inner).unwrap();
        operation
    };
    let writer = CheckpointDeltaWriter::attach(&mut state);
    let worker = std::thread::spawn(move || writer.serve());
    let outcome = state.confirm_existing_checkpoint_send(
        &session,
        &expected,
        state.engram_budget_clock().now() + ENGRAM_CONTROL_SETTLE_TIMEOUT,
    );
    state.persist_tx.send(PersistRequest::Shutdown).unwrap();
    let (fences, unmatched) = worker.join().unwrap();
    assert!(unmatched.is_empty(), "{unmatched:?}; outcome={outcome:?}");
    outcome.unwrap();
    assert_eq!(fences, 2);
    assert_checkpoint_row_matches_ledger(&state, &session);
    assert!(transport.requests().is_empty());
}

#[test]
fn project_containment_checkpoint_watermark_capacity_note_keeps_exact_mirror() {
    let (mut state, _project, session, _transport) = checkpoint_with_unacknowledged_send();
    let expected = state
        .inner
        .lock()
        .unwrap()
        .engram_session_obligations
        .checkpoint_operations[0]
        .clone();
    let mut writer = CheckpointDeltaWriter::attach(&mut state);
    state.note_checkpoint_receipt_capacity(&expected);
    writer.write();
    assert_checkpoint_row_matches_ledger(&state, &session);
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        loaded.engram_session_obligations.checkpoint_operations[0]
            .protection
            .reason_code,
        "checkpoint_receipt_capacity_unavailable"
    );
}

#[test]
fn project_containment_checkpoint_watermark_late_receipt_keeps_replacement_owner() {
    for changed in ["generation", "grant"] {
        let (mut state, _project, session, transport) = checkpoint_with_unacknowledged_send();
        let expected;
        {
            let mut inner = state.inner.lock().unwrap();
            let mut operation = inner.engram_session_obligations.checkpoint_operations[0].clone();
            operation.progress = EngramExistingCheckpointProgress::PossiblySent;
            operation.protection.phase = EngramSessionObligationPhase::Sent;
            operation.validate().unwrap();
            expected = operation.clone();
            inner.engram_session_obligations.checkpoint_operations[0] = operation.clone();
            let record = inner.session_mut(&session).unwrap();
            record.engram.checkpoint_operation = Some(operation);
            if changed == "generation" {
                record.active_turn_generation += 1;
            } else {
                record.engram.active_grant_id = Some("replacement-grant".into());
            }
            state.commit_locked(&mut inner).unwrap();
        }
        let mut writer = CheckpointDeltaWriter::attach(&mut state);
        let ScriptedEngramControlResponse::Reply(Ok(response)) = checkpoint_reply("retained-grant")
        else {
            panic!("checkpoint reply")
        };
        let card = EngramControlCard {
            schema_version: ENGRAM_CONTROL_SCHEMA_VERSION,
            stage: EngramControlStage::Checkpoint,
            causal_failure: None,
            assurance: ENGRAM_CONTROL_ASSURANCE.into(),
            decision: EngramControlCardDecision::Grant,
            dispatch: EngramControlCardDispatch::SentOnGrant,
            refusal_code: None,
            defer_code: None,
            grant_id: Some("retained-grant".into()),
            directives: Vec::new(),
            delivered_range: None,
            latency_ms: EngramControlLatencyCard {
                admission_fence: None,
                preparation_fence: None,
                evaluate: None,
                begin: None,
                checkpoint: Some(0),
                total: 0,
            },
            fail_mode: EngramControlFailMode::Enforced,
            repair_armed: false,
            next_intent: Some(EngramNextIntent::Wait),
            source_root: None,
        };
        // Only the original receipt is accounted. A replacement owner must
        // remain excluded; no new admission or transport is invoked.
        assert!(!state.finish_engram_checkpoint_record_with_operation(
            &session,
            "retained-grant",
            EngramControlCardDecision::Grant,
            card,
            None,
            false,
            Some(&expected),
            Some(response),
            None,
            Some(EngramCheckpointAttemptResult::Accepted),
        ));
        writer.write();
        assert_checkpoint_row_matches_ledger(&state, &session);
        let loaded = load_state(state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        let record = &loaded.sessions[loaded.find_session_index(&session).unwrap()];
        if changed == "grant" {
            assert_eq!(
                record.engram.active_grant_id.as_deref(),
                Some("replacement-grant")
            );
        }
        assert!(transport.requests().is_empty());
    }
}

#[test]
fn project_containment_checkpoint_watermark_mailbox_coalescing_survives_reload() {
    let (mut state, _project, session, _root, receiver) = fixture();
    let transport = ScriptedEngramControlTransport::new([ScriptedEngramControlResponse::Reply(
        Err(EngramTransportError::deadline("lost close reply")),
    )]);
    state.install_control_test_transport(transport.clone());
    assert!(matches!(
        close(&state, &session),
        EngramCheckpointOutcome::Failed(_)
    ));
    let mut writer = CheckpointDeltaWriter::attach(&mut state);
    for sequence in [7, 9] {
        let mut request = prompt();
        request.text = format!("Mailbox wake {sequence}");
        request.source_session_id = Some(session.clone());
        request.source_mailbox = Some(MailboxMessageSource {
            mailbox_id: "watermark-mailbox".into(),
            message_id: format!("wake-{sequence}"),
            sequence,
            unread_count: sequence,
        });
        assert!(
            state
                .queue_ordinary_behind_checkpoint(&session, &request, None)
                .unwrap()
        );
        writer.write();
    }
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&session).unwrap()];
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
    assert_eq!(transport.requests().len(), 1);
    assert!(receiver.try_recv().is_err());
}
