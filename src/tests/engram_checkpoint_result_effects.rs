// Actual SQLite cuts around original receipt accounting. Uses the mediated
// command fixture; no launcher result or provider verification is fabricated.
use super::*;

const VERIFICATION: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
const CHECKPOINT: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

fn fixture(label: &str) -> (CheckedTurn, EngramCheckpointOperation, String) {
    let (turn, original) = lost_checkpoint(label, vec![ScriptedEngramControlResponse::Reply(Ok(json!({
        "decision": "checkpointed", "receipt": {
            "grant_id": CHECK_GRANT, "cursor": 3, "confirmed_cursor": 3,
            "checkpoint": CHECKPOINT, "verification_evidence": [VERIFICATION]
        }
    })))]);
    let observation = attach_batch(&turn.state, &turn.session_id, &turn.root, &original);
    (turn, original, observation)
}

fn attach_batch(state: &AppState, session_id: &str, _root: &FsPath, operation: &EngramCheckpointOperation) -> String {
    let EngramControlRequest::TurnCheckpoint { report, .. } = &operation.original.request else { unreachable!() };
    assert_eq!(report.verification_evidence.len(), 1);
    let EngramObservationReference::ObservationId { observation_id } =
        &report.verification_evidence[0].producer_observation;
    let producer = report.observations.iter().find(|producer|
        &producer.observation_id == observation_id).unwrap();
    let source = producer.source_basis.as_ref().unwrap();
    let observation = {
        let mut inner = state.inner.lock().unwrap();
        let row = inner.test_run_observations.values().find(|row|
            row.evidence.iter().any(|item| &item.producer_observation_id == observation_id)).unwrap();
        let observation = row.execution_id.clone();
        assert!(row.evidence.iter().all(|item| item.verification_record_id.is_none()));
        let index = inner.find_session_index(session_id).unwrap();
        let pending = EngramPendingBatch {
            checkpoint_grant_id: CHECK_GRANT.into(), producer_observation_id: observation_id.clone(),
            measurement: EngramBindMeasurement {
                workspace_id: source.workspace_id.clone(),
                source_revision: source.source_revision.clone(), measured_at: producer.observed_at.clone().unwrap(),
            },
            targets: vec![EngramBindTarget {
                work_id: "receipt-member".into(), short_ref: "fixture-member".into(),
                claim_id: "receipt-member-claim".into(), claim_fence: 1, root_generation: 1,
            }],
            pended_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        };
        assert!(checkpoint_pending_batch_matches_original(&pending, operation));
        inner.session_mut_by_index(index).unwrap().engram.pending_batches.push(pending);
        state.commit_locked(&mut inner).unwrap();
        observation
    };
    observation
}

#[test]
fn administrative_removal_retains_exact_batch_inputs_and_owed_custody() {
    let (turn, original, observation) = fixture("batch-administrative-removal");
    let (project, pending) = turn.record(|record|
        (record.session.project_id.clone().unwrap(), record.engram.pending_batches.clone()));
    assert_eq!(pending.len(), 1);
    assert!(!pending[0].targets.is_empty());
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    let rows = raw_session_rows(&connection);
    drop(connection);
    assert!(turn.state.kill_session(&turn.session_id).is_err());
    assert!(turn.state.delete_project(&project).is_err());
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    assert_eq!(raw_session_rows(&connection), rows, "refusal changes no stored row");
    drop(connection);
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    assert!(loaded.find_project(&project).is_some());
    let record = &loaded.sessions[loaded.find_session_index(&turn.session_id).unwrap()];
    assert_eq!(record.engram.pending_batches, pending);
    assert_eq!(serde_json::to_value(&record.engram.checkpoint_operation.as_ref().unwrap().original).unwrap(),
        serde_json::to_value(&original.original).unwrap());
    assert!(turn.state.recover_original_checkpoint_result(&original.original.id).unwrap());
    assert_effects(&turn.state.inner.lock().unwrap(), &turn.session_id, &observation, true);
    // The original receipt is settled, but its member bind still has its own
    // existing owner and must survive administrative removal until settled.
    assert!(turn.state.kill_session(&turn.session_id).is_err());
    assert!(turn.state.delete_project(&project).is_err());
    assert!(!turn.state.recover_original_checkpoint_result(&original.original.id).unwrap());
    assert_eq!(checkpoints(&turn).len(), 2);
    turn.state.engram_owed_binds_tick(chrono::Utc::now() + chrono::Duration::minutes(31), Duration::ZERO);
    assert!(turn.record(|record| record.engram.owed_binds.is_empty()),
        "the existing time-cap settlement clears its own bind duty");
    turn.state.delete_project(&project).unwrap();
    turn.state.kill_session(&turn.session_id).unwrap();
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    assert!(loaded.find_project(&project).is_none());
    assert!(loaded.find_session_index(&turn.session_id).is_none());
}

#[test]
fn project_deletion_rechecks_batch_custody_at_the_final_mutation_cut() {
    let (turn, original) = lost_checkpoint("batch-delete-final-cut", vec![]);
    let project = turn.record(|record| record.session.project_id.clone().unwrap());
    let (state, session, root, operation) =
        (turn.state.clone(), turn.session_id.clone(), turn.root.clone(), original.clone());
    let reached = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = reached.clone();
    TEST_ENGRAM_SETTINGS_MUTATION_BOUNDARY.with(|slot| {
        *slot.borrow_mut() = Some(("before_delete_commit", Box::new(move || {
            attach_batch(&state, &session, &root, &operation);
            observed.store(true, std::sync::atomic::Ordering::Release);
        })));
    });
    let result = turn.state.delete_project(&project);
    assert!(reached.load(std::sync::atomic::Ordering::Acquire));
    assert!(result.is_err(), "a precheck cannot authorize losing newly retained inputs");
    assert_eq!(checkpoints(&turn).len(), 1);
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    assert!(loaded.find_project(&project).is_some());
    let record = &loaded.sessions[loaded.find_session_index(&turn.session_id).unwrap()];
    assert_eq!(record.session.project_id.as_deref(), Some(project.as_str()));
    assert_eq!(record.engram.pending_batches.len(), 1);
    assert!(pending_checkpoint_batch_owner(&loaded, &turn.session_id, &record.engram.pending_batches[0]).is_some());
    assert_eq!(serde_json::to_value(&record.engram.checkpoint_operation.as_ref().unwrap().original).unwrap(),
        serde_json::to_value(&original.original).unwrap());
}

// Install real readiness fixture paths and store identity before opening the
// turn. The original is then frozen from this actual initial configuration.
fn settings_ready_checked_turn(turn: &CheckedTurn) {
    fs::write(turn.root.join(".engram-project"), "fixture-ready\n").unwrap();
    let store = materialize_fixture_engram_store(&turn.root, &turn.root);
    let mut settings = real_fixture_engram_settings(&turn.root);
    settings.authority_store_key = Some(store);
    let mut inner = turn.state.inner.lock().unwrap();
    let project = inner.sessions[inner.find_session_index(&turn.session_id).unwrap()]
        .session.project_id.clone().unwrap();
    inner.projects.iter_mut().find(|candidate| candidate.id == project).unwrap().engram = Some(settings);
    turn.state.commit_locked(&mut inner).unwrap();
    drop(inner);
    turn.transport.named_roots.lock().unwrap().as_mut().unwrap().project_id = Some("fixture-ready".into());
}

#[test]
fn same_store_settings_reset_preserves_exact_batches_and_independently_owed_binds() {
    let turn = CheckedTurn::start_with_opening_and_setup("batch-settings-reset", true, None,
        vec![ScriptedEngramControlResponse::Reply(Err(EngramTransportError::transport("reply lost"))),
            ScriptedEngramControlResponse::Reply(Ok(json!({
                "decision": "checkpointed", "receipt": {
                    "grant_id": CHECK_GRANT, "cursor": 3, "confirmed_cursor": 3,
                    "checkpoint": CHECKPOINT, "verification_evidence": [VERIFICATION]
                }
            })))], 1, false, Some(settings_ready_checked_turn));
    turn.run("reset-original-check", SIZE_TEST, EngramCommandExit::Code(0), || {});
    turn.finish();
    let original = turn.record(|record| record.engram.checkpoint_operation.clone().unwrap());
    let observation = attach_batch(&turn.state, &turn.session_id, &turn.root, &original);
    let (project, pending) = turn.record(|record|
        (record.session.project_id.clone().unwrap(), record.engram.pending_batches.clone()));
    let mut settings = turn.state.inner.lock().unwrap().find_project(&project).unwrap().engram.clone().unwrap();
    let original_settings = settings.clone();
    let alias = turn.root.join("same-store-engram-fixture");
    fs::copy(settings.binary_path.as_ref().unwrap(), &alias).unwrap();
    // Executable replacement on the same authority store goes through the
    // normal reset. Restore the exact original configuration by that same
    // API before recovering its result; no private recovery image is changed.
    settings.binary_path = Some(alias.to_string_lossy().into_owned());
    let _readiness = stage_test_engram_readiness_without_launch(&[&turn.root]);
    turn.state.update_project_engram_settings(&project, settings.clone()).unwrap();
    assert_eq!(turn.record(|record| record.engram.pending_batches.clone()), pending);
    turn.state.update_project_engram_settings(&project, original_settings).unwrap();
    assert_eq!(turn.record(|record| record.engram.pending_batches.clone()), pending);
    turn.state.shutdown_persist_blocking();
    turn.transport.work_bindings.lock().unwrap().push_back(Ok(original.original.work_binding.clone()));
    let fresh = fresh_host_with_transport(&turn.state, turn.root.join("reset-batch.sqlite"),
        turn.root.clone(), turn.transport.clone());
    {
        let inner = fresh.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&turn.session_id).unwrap()];
        assert_eq!(record.engram.pending_batches, pending);
        assert_eq!(serde_json::to_value(&record.engram.checkpoint_operation.as_ref().unwrap().original).unwrap(),
            serde_json::to_value(&original.original).unwrap());
        assert!(pending_checkpoint_batch_owner(&inner, &turn.session_id, &pending[0]).is_some());
    }
    assert!(fresh.recover_original_checkpoint_result(&original.original.id).unwrap());
    assert_effects(&fresh.inner.lock().unwrap(), &turn.session_id, &observation, true);
    let owed = {
        let inner = fresh.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&turn.session_id).unwrap()].engram.owed_binds.clone()
    };
    assert_eq!(owed.len(), 1);
    fresh.update_project_engram_settings(&project, settings).unwrap();
    {
        let inner = fresh.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&turn.session_id).unwrap()];
        assert!(record.engram.pending_batches.is_empty());
        assert_eq!(record.engram.owed_binds, owed, "settled original does not own this independent bind's lifetime");
    }
    assert!(!fresh.recover_original_checkpoint_result(&original.original.id).unwrap());
    assert_eq!(checkpoints(&turn).len(), 2, "one exact replay, no reset checkpoint or duplicate conversion");
    fresh.shutdown_persist_blocking();
    let loaded = load_state(fresh.persistence_path.as_path()).unwrap().unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&turn.session_id).unwrap()];
    assert_eq!(record.engram.owed_binds.len(), 1);
    assert_eq!(record.engram.owed_binds[0].target, pending[0].targets[0]);
    assert_eq!(record.engram.owed_binds[0].measurement, pending[0].measurement);
}

fn after_confirmations(remaining: usize, action: Box<dyn FnOnce()>) {
    TEST_CONTAINMENT_BEFORE_ACK_RETURN.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            if remaining == 1 { action() } else { after_confirmations(remaining - 1, action) }
        }));
    });
}

fn assert_effects(inner: &StateInner, session: &str, observation: &str, settled: bool) {
    let row = &inner.test_run_observations[observation];
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    let operation = record.engram.checkpoint_operation.as_ref().unwrap();
    if settled {
        assert_eq!(row.evidence.len(), 1);
        assert_eq!(row.evidence[0].verification_record_id.as_deref(), Some(VERIFICATION));
        assert_eq!(row.evidence[0].checkpoint_id.as_deref(), Some(CHECKPOINT));
        assert_eq!(row.evidence[0].confirmed_cursor, Some(3));
        assert!(record.engram.pending_batches.is_empty());
        assert_eq!(record.engram.owed_binds.len(), 1);
        assert_eq!(record.engram.owed_binds[0].original, VERIFICATION);
        assert_eq!(record.engram.owed_binds[0].target.work_id, "receipt-member");
        assert!(engram_parse_time(&record.engram.owed_binds[0].first_owed_at).is_some());
        assert!(!matches!(engram_owed_bind_step(&record.engram.owed_binds[0], chrono::Utc::now()),
            EngramOwedBindStep::GiveUp(_)));
        assert_eq!(operation.protection.phase, EngramSessionObligationPhase::Confirmed);
        assert!(matches!(operation.progress, EngramExistingCheckpointProgress::Accepted { .. }));
    } else {
        assert!(row.evidence.iter().all(|item| item.verification_record_id.is_none()));
        assert!(record.engram.owed_binds.is_empty());
        assert_ne!(operation.protection.phase, EngramSessionObligationPhase::Confirmed);
        assert!(engram_session_obligation_locked(inner, session).is_some());
    }
}

// The manual fixture closes its writer cache before invoking the ACK hook.
// Copy that actual file into another host's disposable path; no row or ledger
// is transplanted into a newer state, and no volatile receipt pool is copied.
pub(super) fn fresh_host_at_cut(state: &AppState, path: PathBuf, workdir: PathBuf) -> (AppState, Arc<ScriptedEngramControlTransport>) {
    let transport = ScriptedEngramControlTransport::new(vec![]);
    let host = fresh_host_with_transport(state, path, workdir, transport.clone());
    (host, transport)
}

fn fresh_host_with_transport(state: &AppState, path: PathBuf, workdir: PathBuf,
    transport: Arc<ScriptedEngramControlTransport>) -> AppState {
    fs::copy(state.persistence_path.as_path(), &path).unwrap();
    // Match the mediated fixture's scripted budget from construction, as
    // the reporting restart fixture does. Actual SQLite and source reads
    // remain real; machine load cannot spend a synthetic short RPC budget.
    let host = AppState::new_with_paths_engram_transport_and_clock_for_test(
        workdir.to_string_lossy().into_owned(), path,
        state.orchestrator_templates_path.as_path().to_path_buf(), transport.clone(),
        EngramBudgetClock::scripted(),
    ).unwrap();
    host
}

// The hook runs after the real scripted wire request, before ordinary local
// accounting. It never supplies a fabricated private completion or ledger.
struct OrdinaryReceiptCut {
    scripted: Arc<ScriptedEngramControlTransport>,
    after: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl EngramControlTransport for OrdinaryReceiptCut {
    fn read_observation_policy(&self, connection: &EngramConnectionConfig,
        store: &EngramAuthorityStoreKey, timeout: Duration, workdir: &FsPath,
    ) -> Result<EngramObservationPolicyBasis, EngramTransportError> {
        self.scripted.read_observation_policy(connection, store, timeout, workdir)
    }

    fn read_work_binding(&self, connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>, timeout: Duration,
    ) -> Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.scripted.read_work_binding(connection, preference, timeout)
    }

    fn read_held_claims(&self, connection: &EngramConnectionConfig, timeout: Duration,
    ) -> Result<EngramHeldClaims, EngramTransportError> {
        self.scripted.read_held_claims(connection, timeout)
    }

    fn request(&self, connection: &EngramConnectionConfig, request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        let response = self.scripted.request(connection, request, timeout);
        if matches!(request, EngramControlRequest::TurnCheckpoint { .. })
            && let Some(after) = self.after.lock().unwrap().take() { after(); }
        response
    }

    fn shutdown_session(&self, session: &str) { self.scripted.shutdown_session(session); }
}

fn ordinary_turn(label: &str) -> CheckedTurn {
    let turn = CheckedTurn::start_with(label, true, None, vec![
        ScriptedEngramControlResponse::Reply(Ok(json!({
            "decision": "checkpointed", "receipt": {
                "grant_id": CHECK_GRANT, "cursor": 3, "confirmed_cursor": 3,
                "checkpoint": CHECKPOINT, "verification_evidence": [VERIFICATION]
            }
        }))),
    ]);
    turn.run("ordinary-check", SIZE_TEST, EngramCommandExit::Code(0), || {});
    turn
}

fn finish_ordinary_guarded(turn: &CheckedTurn) -> Result<()> {
    let (token, generation) = turn.record(|record|
        (record.runtime.runtime_token().unwrap(), record.active_turn_generation));
    turn.state.finish_turn_ok_if_runtime_and_generation_match(&turn.session_id, &token, generation)
}

#[test]
fn ordinary_refused_batch_disposition_shares_publication_and_requires_the_current_owner() {
    for boundary in ["current", "failed-publication", "successor"] {
        let turn = CheckedTurn::start_with(&format!("refused-batch-{boundary}"), true, None,
            vec![checkpoint_refusal_reply("stale_fence")]);
        turn.run("refused-batch-check", SIZE_TEST, EngramCommandExit::Code(0), || {});
        let (state, session, root) = (turn.state.clone(), turn.session_id.clone(), turn.root.clone());
        let captured = Arc::new(Mutex::new(None));
        let observed = captured.clone();
        turn.state.install_control_test_transport(Arc::new(OrdinaryReceiptCut {
            scripted: turn.transport.clone(), after: Mutex::new(Some(Box::new(move || {
                let operation = {
                    let inner = state.inner.lock().unwrap();
                    inner.sessions[inner.find_session_index(&session).unwrap()]
                        .engram.checkpoint_operation.clone().unwrap()
                };
                attach_batch(&state, &session, &root, &operation);
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&session).unwrap();
                let record = inner.session_mut_by_index(index).unwrap();
                let mut successor = record.engram.pending_batches[0].clone();
                successor.checkpoint_grant_id = "successor-grant".into();
                successor.targets[0].work_id = "successor-member".into();
                record.engram.pending_batches.push(successor);
                if boundary == "successor" {
                    record.active_turn_generation += 1;
                    record.engram.dispatch_generation += 1;
                    record.engram.active_grant_id = Some("successor-grant".into());
                }
                let pending = record.engram.pending_batches.clone();
                let generation = record.engram.dispatch_generation;
                state.commit_locked(&mut inner).unwrap();
                *observed.lock().unwrap() = Some((pending, generation));
                if boundary == "failed-publication" {
                    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
                    connection.execute_batch("CREATE TRIGGER fail_refused_publication BEFORE UPDATE ON state_metadata_units
                        WHEN NEW.field_name='engramSessionObligations'
                        BEGIN SELECT RAISE(ABORT,'fixture Refused publication failure'); END;").unwrap();
                }
            }))),
        }));
        let result = finish_ordinary_guarded(&turn);
        if boundary == "failed-publication" { assert!(result.is_err()); } else { result.unwrap(); }
        let (pending, generation) = captured.lock().unwrap().take().unwrap();
        let expected = if boundary == "current" { pending[1..].to_vec() } else { pending };
        turn.record(|record| {
            assert_eq!(record.engram.pending_batches, expected, "{boundary}");
            assert!(record.engram.owed_binds.is_empty());
            assert_eq!(record.engram.dispatch_generation, generation);
            let notice = record.engram.pending_source_root_line.as_deref().unwrap_or_default();
            assert_eq!(notice.contains("Engram refused the checkpoint that carried the batch gate"), boundary == "current");
            if boundary == "successor" {
                assert_eq!(record.engram.active_grant_id.as_deref(), Some("successor-grant"));
            }
        });
        let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
        let raw: String = connection.query_row("SELECT value_json FROM sessions WHERE id=?1",
            [&turn.session_id], |row| row.get(0)).unwrap();
        let row: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(row["engramPendingBatches"], serde_json::to_value(&expected).unwrap());
        let operation: EngramCheckpointOperation = serde_json::from_value(row["engramCheckpointOperation"].clone()).unwrap();
        if boundary == "failed-publication" {
            assert!(operation.attempts.last().unwrap().result.is_none(), "the failed cut stores neither Refused nor its batch disposition");
            connection.execute_batch("DROP TRIGGER fail_refused_publication").unwrap();
        } else {
            assert!(matches!(operation.attempts.last().unwrap().result,
                Some(EngramCheckpointAttemptResult::Refused { .. })));
        }
        assert_eq!(checkpoints(&turn).len(), 1);
        assert!(turn._runtime_rx.try_recv().is_err());
    }
}

#[test]
fn original_unknown_then_refused_keeps_exact_batch_inputs_through_reload() {
    let (turn, original) = lost_checkpoint("batch-unknown-refused", vec![checkpoint_refusal_reply("stale_fence")]);
    attach_batch(&turn.state, &turn.session_id, &turn.root, &original);
    let expected = turn.record(|record| record.engram.pending_batches.clone());
    assert!(!turn.state.recover_original_checkpoint_result(&original.original.id).unwrap());
    {
        let mut inner = turn.state.inner.lock().unwrap();
        let index = inner.find_session_index(&turn.session_id).unwrap();
        assert!(!AppState::drain_engram_refused_batches(&mut inner, index, &turn.session_id,
            CHECK_GRANT, Some("stale_fence")), "a later refusal cannot dispose earlier uncertain inputs");
    }
    turn.state.shutdown_persist_blocking();
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&turn.session_id).unwrap()];
    assert_eq!(record.engram.pending_batches, expected);
    assert!(record.engram.owed_binds.is_empty());
    let operation = record.engram.checkpoint_operation.as_ref().unwrap();
    assert_eq!(serde_json::to_value(&operation.original).unwrap(), serde_json::to_value(&original.original).unwrap());
    assert!(matches!(operation.attempts[0].result, Some(EngramCheckpointAttemptResult::Unknown { .. })));
    assert!(matches!(operation.attempts.last().unwrap().result, Some(EngramCheckpointAttemptResult::Refused { .. })));
    assert!(engram_session_obligation_locked(&loaded, &turn.session_id).is_some());
    assert!(!loaded.engram_confirmed_containment_ids.contains(&original.original.id));
    let sent = checkpoints(&turn);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0], sent[1]);
    assert!(turn._runtime_rx.try_recv().is_err());
}

#[test]
fn ordinary_checkpoint_first_accepted_cut_keeps_actual_effects_across_new_host() {
    let turn = ordinary_turn("ordinary-effects-cut");
    let state = turn.state.clone();
    let session = turn.session_id.clone();
    let root = turn.root.clone();
    let observation = Arc::new(Mutex::new(None));
    let captured = observation.clone();
    let cut_seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = cut_seen.clone();
    turn.state.install_control_test_transport(Arc::new(OrdinaryReceiptCut {
        scripted: turn.transport.clone(), after: Mutex::new(Some(Box::new(move || {
            let operation = {
                let inner = state.inner.lock().unwrap();
                inner.sessions[inner.find_session_index(&session).unwrap()]
                    .engram.checkpoint_operation.clone().unwrap()
            };
            let id = attach_batch(&state, &session, &root, &operation);
            *captured.lock().unwrap() = Some(id.clone());
            // Pre-send fences have completed. The next fence is the first
            // Accepted commit's ACK; inspect its closed actual SQLite file.
            after_confirmations(1, Box::new(move || {
                assert!(!state.inner.lock().unwrap().engram_confirmed_containment_ids
                    .contains(&operation.original.id));
                let (fresh, transport) = fresh_host_at_cut(&state, root.join("ordinary-cut.sqlite"), root.clone());
                {
                    let inner = fresh.inner.lock().unwrap();
                    assert_effects(&inner, &session, &id, true);
                    let canonical = inner.engram_session_obligations.checkpoint_operations.iter()
                        .find(|entry| entry.original.id == operation.original.id).unwrap();
                    assert_eq!(serde_json::to_value(&canonical.original).unwrap(),
                        serde_json::to_value(&operation.original).unwrap());
                    assert!(inner.engram_pending_checkpoint_receipts.entries.is_empty());
                }
                assert!(!fresh.recover_original_checkpoint_result(&operation.original.id).unwrap());
                assert!(!fresh.account_pending_checkpoint_receipt(&session).unwrap());
                assert!(!transport.requests().iter().any(|request| matches!(request.request["operation"].as_str(),
                    Some("turn_checkpoint" | "session_bind" | "turn_evaluate" | "turn_begin"))));
                fresh.shutdown_persist_blocking();
                observed.store(true, std::sync::atomic::Ordering::Release);
            }));
        }))),
    }));
    let request = turn.finish();
    assert!(cut_seen.load(std::sync::atomic::Ordering::Acquire));
    let id = observation.lock().unwrap().clone().unwrap();
    assert_effects(&turn.state.inner.lock().unwrap(), &turn.session_id, &id, true);
    assert!(!turn.state.account_pending_checkpoint_receipt(&turn.session_id).unwrap());
    assert_eq!(checkpoints(&turn), vec![request]);
    assert!(turn._runtime_rx.try_recv().is_err());
}

#[test]
fn ordinary_checkpoint_failed_first_accepted_cut_keeps_custody_until_local_retry() {
    let turn = ordinary_turn("ordinary-effects-failed-cut");
    let state = turn.state.clone();
    let session = turn.session_id.clone();
    let root = turn.root.clone();
    let observation = Arc::new(Mutex::new(None));
    let captured = observation.clone();
    turn.state.install_control_test_transport(Arc::new(OrdinaryReceiptCut {
        scripted: turn.transport.clone(), after: Mutex::new(Some(Box::new(move || {
            let operation = {
                let inner = state.inner.lock().unwrap();
                inner.sessions[inner.find_session_index(&session).unwrap()]
                    .engram.checkpoint_operation.clone().unwrap()
            };
            *captured.lock().unwrap() = Some(attach_batch(&state, &session, &root, &operation));
            let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
            connection.execute_batch("CREATE TRIGGER fail_ordinary_receipt BEFORE UPDATE ON state_metadata_units
                WHEN NEW.field_name='engramSessionObligations'
                BEGIN SELECT RAISE(ABORT,'fixture ordinary Accepted failure'); END;").unwrap();
        }))),
    }));
    finish_ordinary_guarded(&turn).expect_err("the injected transaction failure also rejects terminal persistence");
    let request = checkpoints(&turn).into_iter().next().unwrap();
    let id = observation.lock().unwrap().clone().unwrap();
    {
        let inner = turn.state.inner.lock().unwrap();
        assert!(inner.engram_pending_checkpoint_receipts.entries.values().any(|entry| entry.accepted));
        let operation = inner.sessions[inner.find_session_index(&turn.session_id).unwrap()]
            .engram.checkpoint_operation.as_ref().unwrap();
        assert!(!inner.engram_confirmed_containment_ids.contains(&operation.original.id));
    }
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    connection.execute_batch("DROP TRIGGER fail_ordinary_receipt").unwrap();
    drop(connection);
    let (fresh, transport) = fresh_host_at_cut(&turn.state,
        turn.root.join("ordinary-failed-cut.sqlite"), turn.root.clone());
    assert_effects(&fresh.inner.lock().unwrap(), &turn.session_id, &id, false);
    assert!(!transport.requests().iter().any(|request| matches!(request.request["operation"].as_str(),
        Some("turn_checkpoint" | "session_bind" | "turn_evaluate" | "turn_begin"))));
    fresh.shutdown_persist_blocking();
    let operation = turn.record(|record| record.engram.checkpoint_operation.clone().unwrap());
    assert!(turn.state.recover_original_checkpoint_result(&operation.original.id).unwrap());
    assert!(!turn.state.recover_original_checkpoint_result(&operation.original.id).unwrap());
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    assert_effects(&loaded, &turn.session_id, &id, true);
    assert_eq!(checkpoints(&turn), vec![request]);
    assert!(turn._runtime_rx.try_recv().is_err());
}

#[test]
fn ordinary_checkpoint_nonowning_receipt_settles_only_original_evidence_and_keeps_successor() {
    let turn = ordinary_turn("ordinary-effects-successor");
    let state = turn.state.clone();
    let session = turn.session_id.clone();
    let root = turn.root.clone();
    let captured = Arc::new(Mutex::new(None));
    let observed = captured.clone();
    turn.state.install_control_test_transport(Arc::new(OrdinaryReceiptCut {
        scripted: turn.transport.clone(), after: Mutex::new(Some(Box::new(move || {
            let operation = {
                let inner = state.inner.lock().unwrap();
                inner.sessions[inner.find_session_index(&session).unwrap()]
                    .engram.checkpoint_operation.clone().unwrap()
            };
            let id = attach_batch(&state, &session, &root, &operation);
            queue_test_engram_prompt(&state, &session, "successor during ordinary receipt",
                QueuedPromptSource::User, None);
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            record.active_turn_generation += 1;
            record.engram.dispatch_generation += 1;
            let generation = record.engram.dispatch_generation;
            let queue = record.queued_prompts.iter().map(|head|
                (head.pending_prompt.id.clone(), containment_prompt_fingerprint(head))).collect::<Vec<_>>();
            let pending = serde_json::to_value(&record.engram.pending_batches).unwrap();
            state.commit_locked(&mut inner).unwrap();
            *observed.lock().unwrap() = Some((id, operation, generation, queue, pending));
        }))),
    }));
    finish_ordinary_guarded(&turn).unwrap();
    let request = checkpoints(&turn).into_iter().next().unwrap();
    let (id, original, generation, queue, pending) = captured.lock().unwrap().take().unwrap();
    let check = |inner: &StateInner| {
        let row = &inner.test_run_observations[&id];
        assert_eq!(row.evidence[0].verification_record_id.as_deref(), Some(VERIFICATION));
        assert_eq!(row.evidence[0].checkpoint_id.as_deref(), Some(CHECKPOINT));
        let record = &inner.sessions[inner.find_session_index(&turn.session_id).unwrap()];
        assert!(record.engram.owed_binds.is_empty());
        assert_eq!(record.engram.dispatch_generation, generation);
        assert_eq!(record.queued_prompts.iter().map(|head|
            (head.pending_prompt.id.clone(), containment_prompt_fingerprint(head))).collect::<Vec<_>>(), queue);
        let canonical = inner.engram_session_obligations.checkpoint_operations.iter()
            .find(|entry| entry.original.id == original.original.id).unwrap();
        assert_eq!(serde_json::to_value(&canonical.original).unwrap(), serde_json::to_value(&original.original).unwrap());
        assert!(matches!(canonical.progress, EngramExistingCheckpointProgress::Accepted { .. }));
        assert_ne!(canonical.protection.phase, EngramSessionObligationPhase::Confirmed);
        assert!(engram_obligation_requires_exclusion(inner, &canonical.protection));
    };
    {
        let inner = turn.state.inner.lock().unwrap();
        check(&inner);
        let record = &inner.sessions[inner.find_session_index(&turn.session_id).unwrap()];
        assert_eq!(serde_json::to_value(&record.engram.pending_batches).unwrap(), pending);
    }
    // The non-owning Accepted cut retains the exact original receipt input.
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    let raw: String = connection.query_row("SELECT value_json FROM sessions WHERE id=?1",
        [&turn.session_id], |row| row.get(0)).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&raw).unwrap()["engramPendingBatches"], pending);
    drop(connection);
    let (fresh, transport) = fresh_host_at_cut(&turn.state,
        turn.root.join("ordinary-nonowner-cut.sqlite"), turn.root.clone());
    check(&fresh.inner.lock().unwrap());
    {
        let inner = fresh.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&turn.session_id).unwrap()];
        assert_eq!(serde_json::to_value(&record.engram.pending_batches).unwrap(), pending);
        assert!(matches!(record.runtime, SessionRuntime::None));
        assert!(inner.engram_pending_checkpoint_receipts.entries.is_empty());
    }
    // A new cold accounting owner may settle the historical input; it does
    // not resurrect the former runtime or consume the ordinary successor.
    assert!(fresh.recover_original_checkpoint_result(&original.original.id).unwrap());
    {
        let inner = fresh.inner.lock().unwrap();
        assert_effects(&inner, &turn.session_id, &id, true);
        let record = &inner.sessions[inner.find_session_index(&turn.session_id).unwrap()];
        assert_eq!(record.queued_prompts.iter().map(|head|
            (head.pending_prompt.id.clone(), containment_prompt_fingerprint(head))).collect::<Vec<_>>(), queue);
    }
    assert!(!fresh.recover_original_checkpoint_result(&original.original.id).unwrap());
    fresh.shutdown_persist_blocking();
    assert_effects(&load_state(fresh.persistence_path.as_path()).unwrap().unwrap(), &turn.session_id, &id, true);
    assert!(!transport.requests().iter().any(|request| matches!(request.request["operation"].as_str(),
        Some("turn_checkpoint" | "session_bind" | "turn_evaluate" | "turn_begin"))));
    assert_eq!(checkpoints(&turn), vec![request]);
    assert!(turn._runtime_rx.try_recv().is_err());
}

#[test]
fn ordinary_checkpoint_effects_do_not_release_an_adopted_independent_runtime_duty() {
    let turn = ordinary_turn("ordinary-effects-adopted-duty");
    let state = turn.state.clone();
    let session = turn.session_id.clone();
    let root = turn.root.clone();
    let captured = Arc::new(Mutex::new(None));
    let observed = captured.clone();
    turn.state.install_control_test_transport(Arc::new(OrdinaryReceiptCut {
        scripted: turn.transport.clone(), after: Mutex::new(Some(Box::new(move || {
            let operation = {
                let inner = state.inner.lock().unwrap();
                inner.sessions[inner.find_session_index(&session).unwrap()]
                    .engram.checkpoint_operation.clone().unwrap()
            };
            let id = attach_batch(&state, &session, &root, &operation);
            let mut inner = state.inner.lock().unwrap();
            let prepared = prepare_engram_containment_entry_locked(&inner, &session,
                &operation.protection.project_id, &operation.protection.authority, true)
                .unwrap().unwrap();
            let enclosing = prepared.obligation;
            assert_eq!(enclosing.id, operation.original.id);
            assert_eq!(enclosing.kind, EngramSessionObligationKind::CheckpointContainment);
            assert_ne!(enclosing.phase, EngramSessionObligationPhase::Confirmed);
            inner.engram_session_obligations = inner.engram_session_obligations.clone()
                .with_entry(enclosing.clone()).unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.session_mut_by_index(index).unwrap().engram.dispatch_generation = enclosing.owner_generation;
            state.commit_locked(&mut inner).unwrap();
            *observed.lock().unwrap() = Some((id, operation, enclosing));
        }))),
    }));
    let request = turn.finish();
    let (id, original, enclosing) = captured.lock().unwrap().take().unwrap();
    let check = |inner: &StateInner| {
        inner.engram_session_obligations.validate().unwrap();
        let row = &inner.test_run_observations[&id];
        assert_eq!(row.evidence[0].verification_record_id.as_deref(), Some(VERIFICATION));
        assert_eq!(row.evidence[0].checkpoint_id.as_deref(), Some(CHECKPOINT));
        let record = &inner.sessions[inner.find_session_index(&turn.session_id).unwrap()];
        assert!(record.engram.pending_batches.is_empty());
        assert_eq!(record.engram.owed_binds.len(), 1);
        assert_eq!(record.engram.owed_binds[0].original, VERIFICATION);
        let held = inner.engram_session_obligations.entries.iter()
            .find(|entry| entry.id == enclosing.id).unwrap();
        assert_eq!(held.frozen_request, enclosing.frozen_request);
        assert_eq!(held.phase, enclosing.phase);
        assert!(!inner.engram_confirmed_containment_ids.contains(&held.id));
        assert!(engram_obligation_requires_exclusion(inner, held));
        let canonical = inner.engram_session_obligations.checkpoint_operations.iter()
            .find(|entry| entry.original.id == original.original.id).unwrap();
        assert!(matches!(canonical.progress, EngramExistingCheckpointProgress::Accepted { .. }));
        assert_eq!(serde_json::to_value(&canonical.original).unwrap(), serde_json::to_value(&original.original).unwrap());
    };
    check(&turn.state.inner.lock().unwrap());
    assert!(!turn.state.recover_original_checkpoint_result(&original.original.id).unwrap(),
        "a proved component is held while the independent enclosure is unsettled");
    let unchanged = || {
        let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
        (metadata_unit_generation(&connection).unwrap(), raw_session_rows(&connection),
            turn.state.inner.lock().unwrap().revision)
    };
    let before = unchanged();
    for _ in 0..3 {
        assert!(!turn.state.recover_original_checkpoint_result(&original.original.id).unwrap());
        assert_eq!(unchanged(), before, "re-entry proves the image without rewriting it");
    }
    let due = turn.state.engram_retry_slots.checkpoint_due.lock().unwrap()
        .get(&original.original.id).unwrap().next_at;
    assert!(due >= turn.state.engram_budget_clock().wall_now() + chrono::Duration::seconds(590));
    turn.state.engram_checkpoint_result_retry_tick(due);
    assert_eq!(unchanged(), before, "a due held tick also performs no unchanged commits");
    assert!(require_no_engram_session_obligation(&turn.state.inner.lock().unwrap(), &turn.session_id).is_err());
    // A missing actual effect image and a failed writer cannot be waived by
    // the in-memory Accepted/Confirmed fields or an earlier held proof.
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    connection.execute("UPDATE sessions SET value_json=json_set(value_json,'$.engramOwedBinds',json('[]')) WHERE id=?1",
        [&turn.session_id]).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_held_proof BEFORE UPDATE ON sessions
        BEGIN SELECT RAISE(ABORT,'fixture failed held image repair'); END;").unwrap();
    drop(connection);
    assert!(turn.state.recover_original_checkpoint_result(&original.original.id).is_err());
    let failed_due = turn.state.engram_retry_slots.checkpoint_due.lock().unwrap()
        .get(&original.original.id).unwrap().next_at;
    assert!(failed_due < turn.state.engram_budget_clock().wall_now() + chrono::Duration::seconds(5),
        "a failed proof returns to the fast class");
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    connection.execute_batch("DROP TRIGGER fail_held_proof").unwrap();
    drop(connection);
    assert!(!turn.state.recover_original_checkpoint_result(&original.original.id).unwrap());
    check(&load_state(turn.state.persistence_path.as_path()).unwrap().unwrap());
    let (fresh, transport) = fresh_host_at_cut(&turn.state,
        turn.root.join("ordinary-adopted-cut.sqlite"), turn.root.clone());
    check(&fresh.inner.lock().unwrap());
    assert!(fresh.engram_retry_slots.checkpoint_due.lock().unwrap().is_empty(),
        "held writer proof is process-local, not restored from fields");
    assert!(!fresh.recover_original_checkpoint_result(&original.original.id).unwrap());
    let held_after_boot = fresh.engram_budget_clock().wall_now() + chrono::Duration::seconds(590);
    assert!(fresh.engram_retry_slots.checkpoint_due.lock().unwrap()
        .get(&original.original.id).unwrap().next_at >= held_after_boot);
    check(&fresh.inner.lock().unwrap());
    assert!(!transport.requests().iter().any(|request| matches!(request.request["operation"].as_str(),
        Some("turn_checkpoint" | "session_bind" | "turn_evaluate" | "turn_begin"))));
    fresh.shutdown_persist_blocking();
    assert_eq!(checkpoints(&turn), vec![request]);
    assert!(turn._runtime_rx.try_recv().is_err());
}

#[test]
fn original_checkpoint_first_accepted_cut_already_contains_evidence_and_owed_batches() {
    let (turn, original, observation) = fixture("receipt-effects-accepted-cut");
    let cut_state = turn.state.clone();
    let session = turn.session_id.clone();
    let id = observation.clone();
    let root = turn.root.clone();
    let operation_id = original.original.id.clone();
    let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed_in_hook = observed.clone();
    after_confirmations(3, Box::new(move || {
        let latch = cut_state.inner.lock().unwrap().engram_confirmed_containment_ids.contains(&operation_id);
        assert!(!latch, "cut is after commit and before the live ACK latch");
        let (fresh, transport) = fresh_host_at_cut(&cut_state, root.join("accepted-cut.sqlite"), root.clone());
        {
            let inner = fresh.inner.lock().unwrap();
            assert_effects(&inner, &session, &id, true);
            assert!(inner.engram_pending_checkpoint_receipts.entries.is_empty());
        }
        assert!(!fresh.recover_original_checkpoint_result(&operation_id).unwrap());
        assert!(!transport.requests().iter().any(|request| matches!(request.request["operation"].as_str(),
            Some("turn_checkpoint" | "session_bind" | "turn_evaluate" | "turn_begin"))),
            "fresh host needs no checkpoint or admission replay");
        fresh.shutdown_persist_blocking();
        observed_in_hook.store(true, std::sync::atomic::Ordering::Release);
    }));
    assert!(turn.state.recover_original_checkpoint_result(&original.original.id).unwrap());
    assert!(observed.load(std::sync::atomic::Ordering::Acquire));
    assert!(!turn.state.recover_original_checkpoint_result(&original.original.id).unwrap());
    assert_eq!(checkpoints(&turn).len(), 2);
    turn.state.shutdown_persist_blocking();
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    assert_effects(&loaded, &turn.session_id, &observation, true);
    assert!(turn._runtime_rx.try_recv().is_err());
}

#[test]
fn original_checkpoint_failed_accepted_transaction_keeps_the_old_cut_and_retries_effects_once() {
    let (turn, original, observation) = fixture("receipt-effects-write-failure");
    let path = turn.state.persistence_path.as_path().to_path_buf();
    after_confirmations(2, Box::new(move || {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection.execute_batch("CREATE TRIGGER fail_receipt_effects BEFORE UPDATE ON state_metadata_units
            WHEN NEW.field_name='engramSessionObligations'
            BEGIN SELECT RAISE(ABORT,'fixture first Accepted transaction failure'); END;").unwrap();
    }));
    assert!(turn.state.recover_original_checkpoint_result(&original.original.id).is_err());
    assert_eq!(checkpoints(&turn).len(), 2);
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    connection.execute_batch("DROP TRIGGER fail_receipt_effects").unwrap();
    drop(connection);
    let (fresh, transport) = fresh_host_at_cut(&turn.state, turn.root.join("failed-cut.sqlite"), turn.root.clone());
    {
        let inner = fresh.inner.lock().unwrap();
        assert_effects(&inner, &turn.session_id, &observation, false);
    }
    assert!(!transport.requests().iter().any(|request| matches!(request.request["operation"].as_str(),
        Some("turn_checkpoint" | "session_bind" | "turn_evaluate" | "turn_begin"))));
    fresh.shutdown_persist_blocking();
    assert!(turn.state.recover_original_checkpoint_result(&original.original.id).unwrap());
    assert_eq!(checkpoints(&turn).len(), 2, "the retained Accepted response finishes locally");
    assert!(!turn.state.recover_original_checkpoint_result(&original.original.id).unwrap());
    turn.state.shutdown_persist_blocking();
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    assert_effects(&loaded, &turn.session_id, &observation, true);
    assert!(turn._runtime_rx.try_recv().is_err());
}

#[test]
fn unknown_original_batch_inputs_survive_fresh_hosts_expiry_and_refusal_then_owe_once() {
    let (turn, original, observation) = fixture("pending-original-restart");
    let expected = {
        let mut inner = turn.state.inner.lock().unwrap();
        let index = inner.find_session_index(&turn.session_id).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        let batch = record.engram.pending_batches[0].clone();
        // Identical persisted copies may never mint two owed binds.
        record.engram.pending_batches.push(batch.clone());
        let mut unrelated = batch.clone();
        unrelated.measurement.source_revision.push_str("-unmatched");
        unrelated.targets[0].short_ref = "unmatched-member".into();
        record.engram.pending_batches.push(unrelated);
        turn.state.commit_locked(&mut inner).unwrap();
        vec![batch.clone(), batch]
    };
    let response = ScriptedEngramControlResponse::Reply(Ok(json!({
        "decision": "checkpointed", "receipt": {
            "grant_id": CHECK_GRANT, "cursor": 3, "confirmed_cursor": 3,
            "checkpoint": CHECKPOINT, "verification_evidence": [VERIFICATION]
        }
    })));
    let transport = ScriptedEngramControlTransport::new(vec![response]);
    // The fresh producer model must answer the same readiness and authority
    // probes as CheckedTurn before replaying the saved checkpoint.
    transport.enable_named_roots(&turn.session_id,
        original.original.work_binding.as_ref().expect("original claimed binding"));
    transport.work_bindings.lock().unwrap().extend(
        (0..8).map(|_| Ok(original.original.work_binding.clone())));
    let fresh = fresh_host_with_transport(&turn.state,
        turn.root.join("pending-original-cut.sqlite"), turn.root.clone(), transport.clone());
    let check_pending = |inner: &StateInner| {
        let record = &inner.sessions[inner.find_session_index(&turn.session_id).unwrap()];
        assert_eq!(record.engram.pending_batches, expected);
        assert!(record.engram.owed_binds.is_empty());
        assert!(inner.engram_pending_checkpoint_receipts.entries.is_empty());
        assert!(engram_session_obligation_locked(inner, &turn.session_id).is_some());
    };
    let check_unmatched_notice = |inner: &StateInner| {
        let record = &inner.sessions[inner.find_session_index(&turn.session_id).unwrap()];
        let notice = engram_batch_member_lost_line("unmatched-member",
            "before Engram's answer named the batch gate's record");
        assert!(record.engram.pending_source_root_line.as_deref()
            .is_some_and(|line| line.contains(&notice)));
    };
    check_pending(&fresh.inner.lock().unwrap());
    check_unmatched_notice(&fresh.inner.lock().unwrap());
    assert!(!transport.requests().iter().any(|request| matches!(request.request["operation"].as_str(),
        Some("turn_checkpoint" | "session_bind" | "turn_evaluate" | "turn_begin"))),
        "boot does not replay or admit");
    {
        let mut inner = fresh.inner.lock().unwrap();
        let index = inner.find_session_index(&turn.session_id).unwrap();
        assert!(!AppState::drain_engram_refused_batches(&mut inner, index,
            &turn.session_id, CHECK_GRANT, Some("later_invocation_refused")));
    }
    fresh.engram_owed_binds_tick(chrono::Utc::now() + chrono::Duration::days(2), Duration::ZERO);
    check_pending(&fresh.inner.lock().unwrap());
    check_unmatched_notice(&fresh.inner.lock().unwrap());
    // A second whole-file load keeps exactly the same original fences/times.
    fresh.shutdown_persist_blocking();
    let loaded = load_state(fresh.persistence_path.as_path()).unwrap().unwrap();
    check_pending(&loaded);
    // The first load consumed the unmatched input. Its memory-only notice is
    // still on that host, but cannot be regenerated from the matched inputs.
    let loaded_record = &loaded.sessions[loaded.find_session_index(&turn.session_id).unwrap()];
    assert!(loaded_record.engram.pending_source_root_line.as_deref()
        .is_none_or(|line| !line.contains("unmatched-member")));
    assert!(fresh.recover_original_checkpoint_result(&original.original.id).unwrap());
    assert!(!fresh.recover_original_checkpoint_result(&original.original.id).unwrap());
    assert_effects(&load_state(fresh.persistence_path.as_path()).unwrap().unwrap(),
        &turn.session_id, &observation, true);
    let sent = transport.requests();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].request, serde_json::to_value(&original.original.request).unwrap());
    assert_eq!(checkpoints(&turn).len(), 1, "the old host never replayed");
    assert!(turn._runtime_rx.try_recv().is_err());
}

#[test]
fn original_batch_conversion_rejects_bad_receipts_and_preserves_conflicting_owed_identity() {
    let (turn, original, _) = fixture("pending-original-validation");
    let receipt = |ids: Vec<String>| EngramCheckpointReceipt {
        grant_id: CHECK_GRANT.into(), _cursor: 3, _confirmed_cursor: 3,
        checkpoint: Some(CHECKPOINT.into()), verification_evidence: ids,
    };
    let mut inner = turn.state.inner.lock().unwrap();
    let index = inner.find_session_index(&turn.session_id).unwrap();
    let batch = inner.sessions[index].engram.pending_batches[0].clone();
    let mut successor = batch.clone();
    successor.checkpoint_grant_id = "successor-checkpoint-grant".into();
    successor.targets[0].work_id = "successor-member".into();
    inner.session_mut_by_index(index).unwrap().engram.pending_batches.push(successor.clone());
    let before = serde_json::to_value(&inner.sessions[inner.find_session_index(&turn.session_id).unwrap()]
        .engram.pending_batches).unwrap();
    assert!(AppState::owe_checkpoint_batch_binds_locked(&mut inner, &turn.session_id, &original,
        &receipt(vec![])).is_err());
    assert!(AppState::owe_checkpoint_batch_binds_locked(&mut inner, &turn.session_id, &original,
        &receipt(vec!["not-a-record".into()])).is_err());
    assert_eq!(serde_json::to_value(&inner.sessions[inner.find_session_index(&turn.session_id).unwrap()]
        .engram.pending_batches).unwrap(), before);
    let mut selected = vec![batch.clone()];
    let EngramControlRequest::TurnCheckpoint { report, .. } = &original.original.request else { unreachable!() };
    let mut conflict = engram_owe_batch_binds(&mut selected, report,
        &receipt(vec![VERIFICATION.into()]),
        &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)).owed.pop().unwrap();
    conflict.measurement.source_revision.push_str("-independent");
    inner.session_mut_by_index(index).unwrap().engram.owed_binds.push(conflict.clone());
    assert!(AppState::owe_checkpoint_batch_binds_locked(&mut inner, &turn.session_id, &original,
        &receipt(vec![VERIFICATION.into()])).is_err());
    assert_eq!(inner.sessions[index].engram.pending_batches, vec![batch, successor.clone()]);
    assert_eq!(inner.sessions[index].engram.owed_binds, vec![conflict]);
    inner.session_mut_by_index(index).unwrap().engram.owed_binds.clear();
    assert!(AppState::owe_checkpoint_batch_binds_locked(&mut inner, &turn.session_id, &original,
        &receipt(vec![VERIFICATION.into()])).unwrap());
    assert_eq!(inner.sessions[index].engram.pending_batches, vec![successor],
        "the exact original receipt cannot consume a successor's pending batch");
    assert_eq!(inner.sessions[index].engram.owed_binds.len(), 1);
    assert_eq!(inner.sessions[index].engram.owed_binds[0].original, VERIFICATION);
}
