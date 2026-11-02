//! Test-only child of src/tests/test_run_lifecycle_batch.rs, using its
//! native evidence fixtures. Owns producer-association candidate witnesses:
//! each selection visits only its index bucket, so
//! unrelated retained history adds no candidate, while same-bucket
//! distractors are measured separately and still refused by the original
//! predicates. No latency thresholds, live-store inputs or credit.
use super::*;

/// Adds `unrelated` rows outside every bucket the fixture row names (another
/// owner, tool and producer), and `distractors` rows that share the queried
/// bucket but that `distract` makes fail the original predicate.
fn add_history(
    inner: &mut StateInner,
    id: &str,
    unrelated: usize,
    distractors: usize,
    distract: impl Fn(&mut TestRunCommandObservation),
) {
    let original = inner.test_run_observations[id].clone();
    for ordinal in 0..unrelated {
        let mut row = second_observation(&original, &format!("unrelated-{ordinal:04}"));
        row.summary.owner_session_id = Some(format!("another-owner-{ordinal}"));
        for association in &mut row.evidence {
            association.producer_observation_id = sha256_hex(format!("another-producer-{ordinal}").as_bytes());
        }
        inner.test_run_observations.insert(row.execution_id.clone(), row);
    }
    for ordinal in 0..distractors {
        let mut row = original.clone();
        row.execution_id = format!("{}-distractor-{ordinal:04}", original.execution_id);
        row.summary.run_id = row.execution_id.clone();
        distract(&mut row);
        inner.test_run_observations.insert(row.execution_id.clone(), row);
    }
}

/// Index candidates the action visits, and index maintenance operations it
/// performs.
fn indexed_candidates(action: impl FnOnce()) -> (usize, usize) {
    TEST_TEST_RUN_INDEXED_CANDIDATES.with(|count| count.set(0));
    TEST_HISTORY_INDEX_MAINTENANCE.with(|count| count.set(0));
    action();
    (
        TEST_TEST_RUN_INDEXED_CANDIDATES.with(|count| count.get()),
        TEST_HISTORY_INDEX_MAINTENANCE.with(|count| count.get()),
    )
}

fn settlement_candidates(unrelated: usize, distractors: usize) -> usize {
    let (fixture, _, store, binding, native) =
        native_evidence_fixture(&format!("indexed-settlement-{unrelated}-{distractors}"));
    record_native_evidence(&fixture, &native);
    let id = native.execution_id.clone();
    let producer = engram_verification_record_producers(&native.registration.run_id, &native.body)
        .into_iter().next().unwrap().1;
    let record_id = native.receipt.verifications[0].record_id.clone();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        // A selector-only pending-record seam, derived from an actual durable
        // native association. No evidence is minted by this cost witness.
        inner.test_run_observations.get_mut(&id).unwrap().evidence[0].verification_record_id = None;
        // A distractor carries the receipt's producer under another binding:
        // the producer bucket admits it, the original binding test refuses it.
        add_history(&mut inner, &id, unrelated, distractors, |row| {
            row.evidence[0].binding.work_id = "another-work".to_owned();
        });
    }
    let (candidates, maintenance) = indexed_candidates(|| {
        let mut inner = fixture.state.inner.lock().unwrap();
        let changed = AppState::settle_test_run_producer_records_locked(&mut inner,
            HashMap::from([(producer.observation_id.as_str(), vec![(&producer, &record_id)])]),
            &store, &binding);
        assert_eq!(changed.len(), 1);
    });
    assert_eq!(maintenance, 0, "settlement fills results without index maintenance");
    let inner = fixture.state.inner.lock().unwrap();
    assert!(inner.test_run_observations[&id].evidence[0].verification_record_id.is_some(), "the exact row settles");
    for (key, row) in inner.test_run_observations.iter().filter(|(key, _)| key.as_str() != id) {
        assert!(
            row.evidence.iter().all(|association| association.verification_record_id.is_none()),
            "{key} stays unsettled"
        );
    }
    candidates
}

/// Receipt settlement visits only the rows carrying the receipt's producer:
/// unrelated history adds none, same-producer distractors are the bucket.
#[test]
fn receipt_settlement_visits_only_the_producer_bucket() {
    assert_eq!(settlement_candidates(16, 0), 1);
    assert_eq!(settlement_candidates(256, 0), 1, "unrelated history adds no settlement candidate");
    assert_eq!(settlement_candidates(16, 8), 9, "same-producer distractors are measured as the bucket");
}
