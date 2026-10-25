// Regression child of test_run_registration.rs, using its real observation
// fixture. Owns batched evidence durability and shared lifecycle-response
// witnesses; does not measure elapsed lock time or mint verification credit.
use super::*;

fn second_observation(row: &TestRunCommandObservation, suffix: &str) -> TestRunCommandObservation {
    let mut second = row.clone();
    second.execution_id = format!("{}-{suffix}", row.execution_id);
    second.summary.run_id = second.execution_id.clone();
    second.tool_identity = sha256_hex(suffix.as_bytes());
    second
}

fn queued_persistence(fixture: &mut RunFixture) -> mpsc::Receiver<PersistRequest> {
    let (sender, receiver) = mpsc::channel();
    fixture.state.persist_tx = sender;
    // This fixture observes the real dispatch queue without a worker. Keeping
    // it alive exercises normal delta allocation, rather than shutdown I/O.
    fixture.state.persist_worker_alive.store(true, Ordering::Release);
    receiver
}

fn assert_one_save_and_exact_fences(
    queued: &[PersistRequest],
    expected: &BTreeMap<String, TestRunCommandObservation>,
    compacted: Option<&TestRunCompacted>,
) {
    assert_eq!(queued.iter().filter(|request| matches!(request, PersistRequest::Delta)).count(), 1);
    let mut observations = BTreeMap::new();
    let mut compactions = Vec::new();
    for request in queued {
        if let PersistRequest::Fence(fence) = request {
            match &fence.target {
                PersistFenceTarget::TestRunObservation(row) => {
                    assert!(observations.insert(row.execution_id.clone(), row.as_ref().clone()).is_none());
                }
                PersistFenceTarget::TestRunCompaction(row) => compactions.push(row.as_ref().clone()),
                _ => panic!("unexpected evidence persistence target"),
            }
        }
    }
    assert_eq!(&observations, expected, "fences retain every exact final row");
    assert_eq!(compactions, compacted.into_iter().cloned().collect::<Vec<_>>());
    assert_eq!(queued.len(), 1 + expected.len() + usize::from(compacted.is_some()));
}

#[test]
fn lifecycle_batch_shares_one_response_across_live_and_compacted_exact_fences() {
    let (mut fixture, id, store, binding, _) = evidence_fixture("lifecycle-batch-shared");
    let (second_id, compact_id, foreign_id, generation_id) = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let original = inner.test_run_observations[&id].clone();
        let second = second_observation(&original, "second");
        let second_id = second.execution_id.clone();
        inner.test_run_observations.insert(second_id.clone(), second);
        let proof = second_observation(&original, "compacted");
        let compact_id = proof.execution_id.clone();
        inner.test_run_compacted.insert(compact_id.clone(), TestRunCompacted {
            compacted: true, request_identity: None, summary: test_run_minimal_summary(proof.summary.clone()),
            registration: None, observations: vec![test_run_minimal_observation(proof)],
            request_stamp: None, results_stamp: None, reason: "resolved fixture".to_owned(),
            artifact_conflict: false, artifact_unavailable: true,
        });
        inner.test_runs.compactions_durable.insert(compact_id.clone());
        let mut foreign = second_observation(&original, "foreign");
        foreign.evidence[0].store.project_id = "another-authority".to_owned();
        let foreign_id = foreign.execution_id.clone();
        inner.test_run_observations.insert(foreign_id.clone(), foreign);
        let mut generation = second_observation(&original, "generation");
        generation.evidence[0].source_basis.source_root_generation = Some(2);
        let generation_id = generation.execution_id.clone();
        inner.test_run_observations.insert(generation_id.clone(), generation);
        (second_id, compact_id, foreign_id, generation_id)
    };
    let read: EngramNamedRootReadResponse =
        serde_json::from_value(completed_lifecycle(&fixture, &store, &binding)).unwrap();
    let receiver = queued_persistence(&mut fixture);
    let mut deltas = fixture.state.subscribe_delta_events();
    let before = fixture.state.inner.lock().unwrap().revision;
    fixture.state.revalidate_test_run_lifecycle(&store, &binding, &read);
    let queued: Vec<_> = receiver.try_iter().collect();
    let inner = fixture.state.inner.lock().unwrap();
    let first = &inner.test_run_observations[&id];
    let second = &inner.test_run_observations[&second_id];
    let compacted = &inner.test_run_compacted[&compact_id];
    let shared = first.evidence[0].lifecycle_read.as_ref().unwrap();
    assert!(Arc::ptr_eq(shared, second.evidence[0].lifecycle_read.as_ref().unwrap()));
    assert!(Arc::ptr_eq(shared, compacted.observations[0].evidence[0].lifecycle_read.as_ref().unwrap()));
    for excluded in [&foreign_id, &generation_id] {
        assert!(inner.test_run_observations[excluded].evidence[0].lifecycle_read.is_none());
        assert!(!inner.test_runs.lifecycle_revalidated.contains(&(
            excluded.clone(), inner.test_run_observations[excluded].evidence[0].producer_observation_id.clone(),
        )));
    }
    assert_eq!(inner.test_runs.observation_persistence.0.lock().unwrap().len(), 2);
    assert!(inner.test_runs.compactions_revalidating.contains(&compact_id));
    let expected = BTreeMap::from([(id.clone(), first.clone()), (second_id.clone(), second.clone())]);
    assert_one_save_and_exact_fences(&queued, &expected, Some(compacted));
    let mut revisions = Vec::new();
    while let Ok(text) = deltas.try_recv() {
        revisions.push(serde_json::from_str::<Value>(&text).unwrap()["revision"].as_u64().unwrap());
    }
    assert_eq!(revisions, [before + 1, before + 2, before + 3], "each row delta has its own global revision");
    assert_eq!(inner.revision, before + 3);
    // Persisted JSON remains a response object, never Arc bookkeeping.
    let encoded = serde_json::to_value(first).unwrap();
    assert_eq!(encoded["evidence"][0]["lifecycleRead"], serde_json::to_value(&read).unwrap());
    let decoded: TestRunCommandObservation = serde_json::from_value(encoded).unwrap();
    assert_eq!(&decoded, first);
}

#[test]
fn lifecycle_batch_synchronous_save_survives_reload_without_reusing_live_markers() {
    let (fixture, id, store, binding, report) = evidence_fixture("lifecycle-batch-reload");
    fixture.state.settle_test_run_verification_receipt(&report, &exact_receipt(), &store, &binding);
    let read: EngramNamedRootReadResponse =
        serde_json::from_value(completed_lifecycle(&fixture, &store, &binding)).unwrap();
    fixture.state.revalidate_test_run_lifecycle(&store, &binding, &read);
    assert_eq!(fixture.summary(&id).registration.restart_would_void_evidence, Some(false));
    let restored = load_state(fixture.state.persistence_path.as_path()).unwrap().unwrap();
    assert_eq!(restored.test_run_observations[&id].evidence[0].lifecycle_read.as_deref(), Some(&read));
    assert!(restored.test_runs.lifecycle_revalidated.is_empty());
    *fixture.state.inner.lock().unwrap() = restored;
    assert_eq!(fixture.summary(&id).registration.restart_would_void_evidence, None);
    fixture.state.revalidate_test_run_lifecycle(&store, &binding, &read);
    assert_eq!(fixture.summary(&id).registration.restart_would_void_evidence, Some(false));
}

#[test]
fn receipt_batch_preserves_ordered_references_and_first_producer_with_one_save() {
    let (mut fixture, id, store, binding, mut report) = evidence_fixture("lifecycle-batch-receipt");
    let second_id = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let original = inner.test_run_observations[&id].clone();
        let mut second = second_observation(&original, "second");
        second.evidence[0].producer_observation_id = "1".repeat(64);
        let second_id = second.execution_id.clone();
        inner.test_run_observations.insert(second_id.clone(), second);
        // An earlier exact receipt for the later repeated reference must win.
        inner.test_run_observations.get_mut(&id).unwrap().evidence[0].verification_record_id = Some("a".repeat(64));
        second_id
    };
    let mut second_producer = report.observations[0].clone();
    second_producer.observation_id = "1".repeat(64);
    report.observations.push(second_producer);
    let mut shadow_producer = report.observations[0].clone();
    shadow_producer.action_fingerprint = "9".repeat(64);
    report.observations.push(shadow_producer);
    let mut second_input = report.verification_evidence[0].clone();
    second_input.producer_observation = EngramObservationReference::ObservationId { observation_id: "1".repeat(64) };
    report.verification_evidence.push(second_input);
    report.verification_evidence.push(report.verification_evidence[0].clone());
    let mut receipt = exact_receipt();
    receipt.verification_evidence = vec!["e".repeat(64), "f".repeat(64), "a".repeat(64)];
    let receiver = queued_persistence(&mut fixture);
    fixture.state.settle_test_run_verification_receipt(&report, &receipt, &store, &binding);
    let queued: Vec<_> = receiver.try_iter().collect();
    let inner = fixture.state.inner.lock().unwrap();
    let first = &inner.test_run_observations[&id];
    let second = &inner.test_run_observations[&second_id];
    assert_eq!(first.evidence[0].verification_record_id, Some("a".repeat(64)));
    assert_eq!(second.evidence[0].verification_record_id, Some("f".repeat(64)));
    for row in [first, second] {
        assert_eq!(row.evidence[0].checkpoint_id, receipt.checkpoint);
        assert_eq!(row.evidence[0].confirmed_cursor, Some(receipt._confirmed_cursor));
        assert!(!inner.test_runs.observations_durable.contains(&row.execution_id));
    }
    assert_one_save_and_exact_fences(&queued, &BTreeMap::from([
        (id, first.clone()), (second_id, second.clone()),
    ]), None);
}

#[test]
fn lifecycle_batch_failed_persistence_exposes_failure_without_losing_response() {
    let (mut fixture, id, store, binding, report) = evidence_fixture("lifecycle-batch-failure");
    fixture.state.settle_test_run_verification_receipt(&report, &exact_receipt(), &store, &binding);
    let second_id = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let second = second_observation(&inner.test_run_observations[&id], "second");
        let key = second.execution_id.clone();
        inner.test_run_observations.insert(key.clone(), second);
        key
    };
    let invalid = fixture.root.join("state-is-a-directory");
    fs::create_dir(&invalid).unwrap();
    fixture.state.persistence_path = Arc::new(invalid);
    let read: EngramNamedRootReadResponse =
        serde_json::from_value(completed_lifecycle(&fixture, &store, &binding)).unwrap();
    fixture.state.revalidate_test_run_lifecycle(&store, &binding, &read);
    let inner = fixture.state.inner.lock().unwrap();
    for key in [&id, &second_id] {
        assert!(inner.test_runs.observations_failed.contains(key));
        assert!(!inner.test_runs.observations_durable.contains(key));
        assert_eq!(inner.test_run_observations[key].evidence[0].lifecycle_read.as_deref(), Some(&read));
        assert_eq!(test_run_public_observation(&inner, &inner.test_run_observations[key]).registration.restart_would_void_evidence, None);
    }
}

#[test]
fn lifecycle_batch_wrong_authority_or_generation_has_no_dispatch_or_revision() {
    let (mut fixture, id, store, binding, _) = evidence_fixture("lifecycle-batch-noop");
    let mut wrong_store = store.clone();
    wrong_store.project_id = "wrong-authority".to_owned();
    let read: EngramNamedRootReadResponse =
        serde_json::from_value(completed_lifecycle(&fixture, &store, &binding)).unwrap();
    let receiver = queued_persistence(&mut fixture);
    let before = fixture.state.inner.lock().unwrap().revision;
    fixture.state.revalidate_test_run_lifecycle(&wrong_store, &binding, &read);
    let mut other_generation = completed_lifecycle(&fixture, &store, &binding);
    other_generation["latest_event"]["generation"] = json!(2);
    let read = serde_json::from_value(other_generation).unwrap();
    fixture.state.revalidate_test_run_lifecycle(&store, &binding, &read);
    assert!(receiver.try_recv().is_err());
    let inner = fixture.state.inner.lock().unwrap();
    assert_eq!(inner.revision, before);
    assert!(inner.test_run_observations[&id].evidence[0].lifecycle_read.is_none());
}
