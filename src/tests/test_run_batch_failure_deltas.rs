// Regression child of test_run_registration.rs, using its real observation
// fixture. Owns the failure-publication contract of both test-run observation
// writers: an evidence batch whose stopped-worker synchronous save fails, and
// the single-row writer. Every changed row publishes its failed tracking at a
// revision its own commit allocated, and no revision is reused. Does not own
// the batch's fences or durable saves (test_run_lifecycle_batch.rs) or delta
// allocation itself (sse_broadcast.rs). New file, not split from another.
use super::*;

/// A second live row in the same exact lifecycle as `row`.
fn sibling_row(row: &TestRunCommandObservation, suffix: &str) -> TestRunCommandObservation {
    let mut sibling = row.clone();
    sibling.execution_id = format!("{}-{suffix}", row.execution_id);
    sibling.summary.run_id = sibling.execution_id.clone();
    sibling.tool_identity = sha256_hex(suffix.as_bytes());
    sibling
}

/// Two changed rows, a stopped persistence worker, a disconnected sender and a
/// state path that cannot be written: every synchronous save attempt fails.
fn failing_stopped_worker_batch(label: &str) -> (RunFixture, [String; 2], EngramNamedRootReadResponse, EngramAuthorityStoreKey, EngramControlWorkBinding) {
    let (mut fixture, id, store, binding, _) = evidence_fixture(label);
    let second = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let second = sibling_row(&inner.test_run_observations[&id], "second");
        let key = second.execution_id.clone();
        inner.test_run_observations.insert(key.clone(), second);
        key
    };
    let read: EngramNamedRootReadResponse =
        serde_json::from_value(completed_lifecycle(&fixture, &store, &binding)).unwrap();
    // The fixture's sender has no receiver, so every dispatch falls back to a
    // synchronous save; a stopped worker makes later delta revisions save too.
    let (sender, receiver) = mpsc::channel();
    drop(receiver);
    fixture.state.persist_tx = sender;
    fixture.state.persist_worker_alive.store(false, Ordering::Release);
    let invalid = fixture.root.join("state-is-a-directory");
    fs::create_dir(&invalid).unwrap();
    fixture.state.persistence_path = Arc::new(invalid);
    (fixture, [id, second], read, store, binding)
}

/// Every TestRunChanged delta published since `deltas` subscribed.
fn changed_runs(deltas: &mut broadcast::Receiver<String>) -> Vec<(u64, TestRunSummary)> {
    let mut runs = Vec::new();
    while let Ok(text) = deltas.try_recv() {
        let event: Value = serde_json::from_str(&text).unwrap();
        if event["type"] == "testRunChanged" {
            let revision = event["revision"].as_u64().unwrap();
            runs.push((revision, serde_json::from_value(event["run"].clone()).unwrap()));
        }
    }
    runs
}

#[test]
fn every_row_of_a_failed_stopped_worker_batch_publishes_its_failure_at_its_own_revision() {
    let (fixture, ids, read, store, binding) = failing_stopped_worker_batch("batch-failure-every-row");
    let mut deltas = fixture.state.subscribe_delta_events();
    let before = fixture.state.inner.lock().unwrap().revision;
    fixture.state.revalidate_test_run_lifecycle(&store, &binding, &read);
    let published = changed_runs(&mut deltas);
    let inner = fixture.state.inner.lock().unwrap();
    for id in &ids {
        assert!(inner.test_runs.observations_failed.contains(id));
        assert!(!inner.test_runs.observations_durable.contains(id));
    }
    assert_eq!(
        published.iter().map(|(revision, _)| *revision).collect::<Vec<_>>(),
        [before + 1, before + 2],
        "each changed row publishes at the distinct revision its failed save allocated"
    );
    let mut published_ids: Vec<_> = published.iter().map(|(_, run)| run.run_id.clone()).collect();
    published_ids.sort();
    let mut expected_ids = ids.to_vec();
    expected_ids.sort();
    assert_eq!(published_ids, expected_ids);
    for (_, run) in &published {
        assert_eq!(run.registration.tracking_durability.as_deref(), Some("failed"));
    }
    assert_eq!(inner.revision, before + 2);
}

#[test]
fn a_batch_whose_revision_cannot_advance_publishes_no_reused_revision() {
    let (fixture, ids, read, store, binding) = failing_stopped_worker_batch("batch-failure-exhausted");
    fixture.state.inner.lock().unwrap().revision = u64::MAX;
    let mut deltas = fixture.state.subscribe_delta_events();
    fixture.state.revalidate_test_run_lifecycle(&store, &binding, &read);
    let published = changed_runs(&mut deltas);
    assert!(
        published.is_empty(),
        "no row may publish at a revision that was not allocated for it: {:?}",
        published.iter().map(|(revision, run)| (*revision, run.run_id.clone())).collect::<Vec<_>>()
    );
    let inner = fixture.state.inner.lock().unwrap();
    assert_eq!(inner.revision, u64::MAX);
    for id in &ids {
        assert!(inner.test_runs.observations_failed.contains(id), "the failed save is still tracked");
    }
}

/// Persists one row through the single-row writer and returns what it published.
fn persist_single_row(fixture: &RunFixture, id: &str) -> Vec<(u64, TestRunSummary)> {
    let row = fixture.state.inner.lock().unwrap().test_run_observations[id].clone();
    let mut deltas = fixture.state.subscribe_delta_events();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        fixture.state.persist_test_command_observation_locked(&mut inner, row);
    }
    changed_runs(&mut deltas)
}

#[test]
fn the_single_row_writer_with_an_exhausted_revision_publishes_no_reused_revision() {
    let (fixture, [id, _], _, _, _) = failing_stopped_worker_batch("single-row-exhausted");
    fixture.state.inner.lock().unwrap().revision = u64::MAX;
    let published = persist_single_row(&fixture, &id);
    assert!(
        published.is_empty(),
        "the single-row writer may not publish at a revision it did not allocate: {:?}",
        published.iter().map(|(revision, run)| (*revision, run.run_id.clone())).collect::<Vec<_>>()
    );
    let inner = fixture.state.inner.lock().unwrap();
    assert_eq!(inner.revision, u64::MAX);
    assert!(inner.test_runs.observations_failed.contains(&id), "the failed save is still tracked");
    assert!(!inner.test_runs.observations_durable.contains(&id));
}

#[test]
fn the_single_row_writer_still_publishes_a_failed_save_at_its_allocated_revision() {
    let (fixture, [id, _], _, _, _) = failing_stopped_worker_batch("single-row-failed-save");
    let before = fixture.state.inner.lock().unwrap().revision;
    let published = persist_single_row(&fixture, &id);
    assert_eq!(
        published.iter().map(|(revision, run)| (*revision, run.run_id.clone())).collect::<Vec<_>>(),
        [(before + 1, id.clone())],
        "a save that fails after allocation still publishes its failure at that revision"
    );
    assert_eq!(published[0].1.registration.tracking_durability.as_deref(), Some("failed"));
    let inner = fixture.state.inner.lock().unwrap();
    assert_eq!(inner.revision, before + 1);
    assert!(inner.test_runs.observations_failed.contains(&id));
    assert!(!inner.test_runs.observations_durable.contains(&id));
}
