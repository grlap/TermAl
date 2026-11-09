// Regression child of test_run_registration.rs, using its real observation
// fixture. Owns batched evidence durability and shared lifecycle-response
// witnesses; does not measure elapsed lock time or mint verification credit.
use super::*;

#[path = "test_run_indexed_selection_cost.rs"]
mod indexed_selection_cost;

fn second_observation(row: &TestRunCommandObservation, suffix: &str) -> TestRunCommandObservation {
    let mut second = row.clone();
    second.execution_id = format!("{}-{suffix}", row.execution_id);
    second.summary.run_id = second.execution_id.clone();
    second.tool_identity = sha256_hex(suffix.as_bytes());
    second
}

fn queued_persistence(fixture: &mut RunFixture) -> mpsc::Receiver<PersistRequest> {
    fixture.state.shutdown_persist_blocking();
    let (sender, receiver) = mpsc::channel();
    fixture.state.persist_tx = sender;
    // This fixture observes the real dispatch queue without a worker. Keeping
    // it alive exercises normal delta allocation, rather than shutdown I/O.
    fixture
        .state
        .persist_worker_alive
        .store(true, Ordering::Release);
    receiver
}

fn assert_one_save_and_exact_fences(
    queued: &[PersistRequest],
    expected: &BTreeMap<String, TestRunCommandObservation>,
    compacted: Option<&TestRunCompacted>,
) {
    assert_eq!(
        queued
            .iter()
            .filter(|request| matches!(request, PersistRequest::Delta))
            .count(),
        1
    );
    let mut observations = BTreeMap::new();
    let mut compactions = Vec::new();
    for request in queued {
        if let PersistRequest::Fence(fence) = request {
            match &fence.target {
                PersistFenceTarget::TestRunObservation(row) => {
                    assert!(
                        observations
                            .insert(row.execution_id.clone(), row.as_ref().clone())
                            .is_none()
                    );
                }
                PersistFenceTarget::TestRunCompaction(row) => {
                    compactions.push(row.as_ref().clone())
                }
                _ => panic!("unexpected evidence persistence target"),
            }
        }
    }
    assert_eq!(
        &observations, expected,
        "fences retain every exact final row"
    );
    assert_eq!(
        compactions,
        compacted.into_iter().cloned().collect::<Vec<_>>()
    );
    assert_eq!(
        queued.len(),
        1 + expected.len() + usize::from(compacted.is_some())
    );
}

#[test]
fn lifecycle_batch_shares_one_response_across_live_and_compacted_exact_fences() {
    let (mut fixture, _, store, binding, native) =
        native_evidence_fixture("lifecycle-batch-shared");
    record_native_evidence(&fixture, &native);
    let id = native.execution_id;
    let (second_id, compact_id, foreign_id, generation_id) = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let original = inner.test_run_observations[&id].clone();
        let second = second_observation(&original, "second");
        let second_id = second.execution_id.clone();
        inner
            .test_run_observations
            .insert(second_id.clone(), second);
        let proof = second_observation(&original, "compacted");
        let compact_id = proof.execution_id.clone();
        inner.test_run_compacted.insert(
            compact_id.clone(),
            TestRunCompacted {
                compacted: true,
                request_identity: None,
                summary: test_run_minimal_summary(proof.summary.clone()),
                registration: None,
                observations: vec![test_run_minimal_observation(proof)],
                request_stamp: None,
                results_stamp: None,
                reason: "resolved fixture".to_owned(),
                artifact_conflict: false,
                artifact_unavailable: true,
            },
        );
        inner
            .test_runs
            .compactions_durable
            .insert(compact_id.clone());
        let mut foreign = second_observation(&original, "foreign");
        foreign.evidence[0].store.project_id = "another-authority".to_owned();
        let foreign_id = foreign.execution_id.clone();
        inner
            .test_run_observations
            .insert(foreign_id.clone(), foreign);
        let mut generation = second_observation(&original, "generation");
        generation.evidence[0].source_basis.source_root_generation = Some(2);
        let generation_id = generation.execution_id.clone();
        inner
            .test_run_observations
            .insert(generation_id.clone(), generation);
        (second_id, compact_id, foreign_id, generation_id)
    };
    let read: EngramNamedRootReadResponse =
        serde_json::from_value(completed_lifecycle(&fixture, &store, &binding)).unwrap();
    let receiver = queued_persistence(&mut fixture);
    let mut deltas = fixture.state.subscribe_delta_events();
    let before = fixture.state.inner.lock().unwrap().revision;
    fixture
        .state
        .revalidate_test_run_lifecycle(&store, &binding, &read);
    let queued: Vec<_> = receiver.try_iter().collect();
    let inner = fixture.state.inner.lock().unwrap();
    let first = &inner.test_run_observations[&id];
    let second = &inner.test_run_observations[&second_id];
    let compacted = &inner.test_run_compacted[&compact_id];
    let shared = first.evidence[0].lifecycle_read.as_ref().unwrap();
    assert!(Arc::ptr_eq(
        shared,
        second.evidence[0].lifecycle_read.as_ref().unwrap()
    ));
    assert!(Arc::ptr_eq(
        shared,
        compacted.observations[0].evidence[0]
            .lifecycle_read
            .as_ref()
            .unwrap()
    ));
    for excluded in [&foreign_id, &generation_id] {
        assert!(
            inner.test_run_observations[excluded].evidence[0]
                .lifecycle_read
                .is_none()
        );
        assert!(
            !inner.test_runs.lifecycle_revalidated.contains(&(
                excluded.clone(),
                inner.test_run_observations[excluded].evidence[0]
                    .producer_observation_id
                    .clone(),
            ))
        );
    }
    assert_eq!(
        inner
            .test_runs
            .observation_persistence
            .0
            .lock()
            .unwrap()
            .len(),
        2
    );
    assert!(
        inner
            .test_runs
            .compactions_revalidating
            .contains(&compact_id)
    );
    let expected = BTreeMap::from([
        (id.clone(), first.clone()),
        (second_id.clone(), second.clone()),
    ]);
    assert_one_save_and_exact_fences(&queued, &expected, Some(compacted));
    let mut revisions = Vec::new();
    while let Ok(text) = deltas.try_recv() {
        revisions.push(
            serde_json::from_str::<Value>(&text).unwrap()["revision"]
                .as_u64()
                .unwrap(),
        );
    }
    assert_eq!(
        revisions,
        [before + 1, before + 2, before + 3],
        "each row delta has its own global revision"
    );
    assert_eq!(inner.revision, before + 3);
    // Persisted JSON remains a response object, never Arc bookkeeping.
    let encoded = serde_json::to_value(first).unwrap();
    assert_eq!(
        encoded["evidence"][0]["lifecycleRead"],
        serde_json::to_value(&read).unwrap()
    );
    let decoded: TestRunCommandObservation = serde_json::from_value(encoded).unwrap();
    assert_eq!(&decoded, first);
}

#[test]
fn lifecycle_batch_synchronous_save_survives_reload_without_reusing_live_markers() {
    let (fixture, run_id, store, binding, native) =
        native_evidence_fixture("lifecycle-batch-reload");
    record_native_evidence(&fixture, &native);
    let id = native.execution_id;
    let read: EngramNamedRootReadResponse =
        serde_json::from_value(completed_lifecycle(&fixture, &store, &binding)).unwrap();
    fixture
        .state
        .revalidate_test_run_lifecycle(&store, &binding, &read);
    assert_eq!(
        fixture
            .summary(&run_id)
            .registration
            .restart_would_void_evidence,
        Some(false)
    );
    let restored = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        restored.test_run_observations[&id].evidence[0]
            .lifecycle_read
            .as_deref(),
        Some(&read)
    );
    assert!(restored.test_runs.lifecycle_revalidated.is_empty());
    *fixture.state.inner.lock().unwrap() = restored;
    assert_eq!(
        fixture
            .summary(&run_id)
            .registration
            .restart_would_void_evidence,
        None
    );
    fixture
        .state
        .revalidate_test_run_lifecycle(&store, &binding, &read);
    {
        let inner = fixture.state.inner.lock().unwrap();
        let row = &inner.test_run_observations[&id];
        assert_eq!(row.evidence[0].lifecycle_read.as_deref(), Some(&read));
        assert!(
            inner
                .test_runs
                .lifecycle_revalidated
                .contains(&(id.clone(), row.evidence[0].producer_observation_id.clone(),))
        );
        assert_eq!(test_run_evidence_impact(&*inner, row), Some(false));
    }
    fixture.refresh(&[]);
    assert_eq!(
        fixture
            .summary(&run_id)
            .registration
            .restart_would_void_evidence,
        Some(false)
    );
}

#[test]
fn native_receipt_batch_preserves_the_first_record_and_saves_only_changed_rows() {
    let (mut fixture, _, _, _, native) = native_evidence_fixture("lifecycle-batch-receipt");
    record_native_evidence(&fixture, &native);
    let id = native.execution_id.clone();
    let second_id = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let original = inner.test_run_observations[&id].clone();
        let mut second = second_observation(&original, "second");
        second.evidence[0].verification_record_id = None;
        let second_id = second.execution_id.clone();
        inner
            .test_run_observations
            .insert(second_id.clone(), second);
        // An earlier recorded ID for this producer cannot be replaced by a
        // later different ID, even while another matching row can be filled.
        inner.test_run_observations.get_mut(&id).unwrap().evidence[0].verification_record_id =
            Some("a".repeat(64));
        second_id
    };
    engram_verification_record_receipt_check(&native.body, &native.receipt).unwrap();
    let receiver = queued_persistence(&mut fixture);
    fixture.state.settle_engram_verification_record(
        &native.registration,
        &native.body,
        &native.receipt,
    );
    let queued: Vec<_> = receiver.try_iter().collect();
    let inner = fixture.state.inner.lock().unwrap();
    let first = &inner.test_run_observations[&id];
    let second = &inner.test_run_observations[&second_id];
    assert_eq!(
        first.evidence[0].verification_record_id,
        Some("a".repeat(64))
    );
    assert_eq!(
        second.evidence[0].verification_record_id,
        Some("e".repeat(64))
    );
    for row in [first, second] {
        assert!(row.evidence[0].checkpoint_id.is_none());
        assert!(row.evidence[0].confirmed_cursor.is_none());
    }
    assert!(!inner.test_runs.observations_durable.contains(&second_id));
    assert_one_save_and_exact_fences(
        &queued,
        &BTreeMap::from([(second_id, second.clone())]),
        None,
    );
}

#[test]
fn lifecycle_batch_failed_persistence_exposes_failure_without_losing_response() {
    let (mut fixture, _, store, binding, native) =
        native_evidence_fixture("lifecycle-batch-failure");
    record_native_evidence(&fixture, &native);
    let id = native.execution_id;
    let second_id = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let second = second_observation(&inner.test_run_observations[&id], "second");
        let key = second.execution_id.clone();
        inner.test_run_observations.insert(key.clone(), second);
        key
    };
    let invalid = fixture.root.join("state-is-a-directory");
    fs::create_dir(&invalid).unwrap();
    fixture.state.shutdown_persist_blocking();
    fixture.state.persistence_path = Arc::new(invalid);
    let read: EngramNamedRootReadResponse =
        serde_json::from_value(completed_lifecycle(&fixture, &store, &binding)).unwrap();
    fixture
        .state
        .revalidate_test_run_lifecycle(&store, &binding, &read);
    let inner = fixture.state.inner.lock().unwrap();
    for key in [&id, &second_id] {
        assert!(inner.test_runs.observations_failed.contains(key));
        assert!(!inner.test_runs.observations_durable.contains(key));
        assert_eq!(
            inner.test_run_observations[key].evidence[0]
                .lifecycle_read
                .as_deref(),
            Some(&read)
        );
        assert_eq!(
            test_run_public_observation(&inner, &inner.test_run_observations[key])
                .registration
                .restart_would_void_evidence,
            None
        );
    }
}

#[test]
fn lifecycle_batch_failed_mixed_save_keeps_compacted_proof_revalidating_and_never_safe() {
    let (mut fixture, _, store, binding, native) =
        native_evidence_fixture("lifecycle-batch-mixed-failure");
    record_native_evidence(&fixture, &native);
    let id = native.execution_id;
    let compact_id = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let proof = second_observation(&inner.test_run_observations[&id], "compacted");
        let compact_id = proof.execution_id.clone();
        inner.test_run_compacted.insert(
            compact_id.clone(),
            TestRunCompacted {
                compacted: true,
                request_identity: None,
                summary: test_run_minimal_summary(proof.summary.clone()),
                registration: None,
                observations: vec![test_run_minimal_observation(proof)],
                request_stamp: None,
                results_stamp: None,
                reason: "resolved fixture".to_owned(),
                artifact_conflict: false,
                artifact_unavailable: true,
            },
        );
        // An earlier save made this compact identity durable.
        inner
            .test_runs
            .compactions_durable
            .insert(compact_id.clone());
        compact_id
    };
    let read: EngramNamedRootReadResponse =
        serde_json::from_value(completed_lifecycle(&fixture, &store, &binding)).unwrap();
    let durable_path = fixture.state.persistence_path.clone();
    let invalid = fixture.root.join("state-is-a-directory");
    fs::create_dir(&invalid).unwrap();
    fixture.state.shutdown_persist_blocking();
    fixture.state.persistence_path = Arc::new(invalid);
    fixture
        .state
        .revalidate_test_run_lifecycle(&store, &binding, &read);
    {
        let inner = fixture.state.inner.lock().unwrap();
        // The shared batch failed: the live row says so, and the new compacted
        // proof stays revalidating instead of inheriting the earlier flag.
        assert!(inner.test_runs.observations_failed.contains(&id));
        assert!(!inner.test_runs.observations_durable.contains(&id));
        assert!(
            inner
                .test_runs
                .compactions_revalidating
                .contains(&compact_id)
        );
        assert!(
            inner.test_runs.compactions_durable.contains(&compact_id),
            "the prior durable flag is kept"
        );
        let compacted = &inner.test_run_compacted[&compact_id];
        assert_eq!(
            compacted.observations[0].evidence[0]
                .lifecycle_read
                .as_deref(),
            Some(&read)
        );
        assert_ne!(
            test_run_compacted_summary(&*inner, compacted)
                .registration
                .restart_would_void_evidence,
            Some(false),
            "an unsaved proof is never reported as durably safe",
        );
    }
    // Contrast: the same read saved durably clears the flag and the proof is
    // then safe, so only the failed save withheld it above.
    fixture.state.persistence_path = durable_path;
    fixture
        .state
        .revalidate_test_run_lifecycle(&store, &binding, &read);
    let inner = fixture.state.inner.lock().unwrap();
    assert!(
        !inner
            .test_runs
            .compactions_revalidating
            .contains(&compact_id)
    );
    assert!(inner.test_runs.compactions_durable.contains(&compact_id));
    assert_eq!(
        test_run_compacted_summary(&*inner, &inner.test_run_compacted[&compact_id])
            .registration
            .restart_would_void_evidence,
        Some(false),
    );
}

#[test]
fn lifecycle_batch_wrong_authority_or_generation_has_no_dispatch_or_revision() {
    let (mut fixture, _, store, binding, native) = native_evidence_fixture("lifecycle-batch-noop");
    record_native_evidence(&fixture, &native);
    let id = native.execution_id;
    let mut wrong_store = store.clone();
    wrong_store.project_id = "wrong-authority".to_owned();
    let read: EngramNamedRootReadResponse =
        serde_json::from_value(completed_lifecycle(&fixture, &store, &binding)).unwrap();
    let receiver = queued_persistence(&mut fixture);
    let before = fixture.state.inner.lock().unwrap().revision;
    fixture
        .state
        .revalidate_test_run_lifecycle(&wrong_store, &binding, &read);
    let mut other_generation = completed_lifecycle(&fixture, &store, &binding);
    other_generation["latest_event"]["generation"] = json!(2);
    let read = serde_json::from_value(other_generation).unwrap();
    fixture
        .state
        .revalidate_test_run_lifecycle(&store, &binding, &read);
    assert!(receiver.try_recv().is_err());
    let inner = fixture.state.inner.lock().unwrap();
    assert_eq!(inner.revision, before);
    assert!(
        inner.test_run_observations[&id].evidence[0]
            .lifecycle_read
            .is_none()
    );
}
