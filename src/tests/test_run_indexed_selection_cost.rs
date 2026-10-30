//! Test-only child of src/tests/test_run_lifecycle_batch.rs, using its
//! evidence fixtures. Owns the receipt-settlement and check-association
//! candidate witnesses: each selection visits only its index bucket, so
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
    let (fixture, id, store, binding, report) =
        evidence_fixture(&format!("indexed-settlement-{unrelated}-{distractors}"));
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        // A distractor carries the receipt's producer under another binding:
        // the producer bucket admits it, the original binding test refuses it.
        add_history(&mut inner, &id, unrelated, distractors, |row| {
            row.evidence[0].binding.work_id = "another-work".to_owned();
        });
    }
    let (candidates, maintenance) = indexed_candidates(|| {
        fixture.state.settle_test_run_verification_receipt(&report, &exact_receipt(), &store, &binding)
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

fn association_candidates(unrelated: usize, distractors: usize) -> usize {
    let (mut fixture, id, store, binding, _) =
        evidence_fixture(&format!("indexed-association-{unrelated}-{distractors}"));
    let checks = [reported_check(0, "indexed-key")];
    let index = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let session = inner.test_run_observations[&id].summary.owner_session_id.clone().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram.work_binding = Some(binding.clone());
        let project_id = inner.sessions[index].session.project_id.clone().unwrap();
        let project = inner.projects.iter_mut().find(|project| project.id == project_id).unwrap();
        project.engram = Some(EngramProjectSettings {
            enabled: true,
            authority_store_key: Some(store.clone()),
            ..EngramProjectSettings::default()
        });
        let check = &checks[0].check;
        inner.test_run_observations.update(&id, |row| {
            row.grant_identity = Some(sha256_hex(check.grant_id.as_bytes()));
            row.tool_identity = sha256_hex(check.key.as_bytes());
            row.command_fingerprint = EngramHost::check_fingerprint(&check.command);
            row.summary.started_at = Some(check.started_at.clone());
            row.summary.worktree = test_run_display_path(&check.target.root);
        });
        // A distractor shares owner, grant and tool, so the check bucket
        // admits it; its other start refuses it, and multiplicity stays one.
        add_history(&mut inner, &id, unrelated, distractors, |row| {
            row.summary.started_at = Some("2026-09-23T00:00:00.000Z".to_owned());
        });
        index
    };
    let _queued = queued_persistence(&mut fixture);
    let (candidates, maintenance) = indexed_candidates(|| {
        let mut inner = fixture.state.inner.lock().unwrap();
        fixture.state.associate_reported_test_checks_locked(&mut inner, index, &checks);
    });
    // A new association changes indexed buckets: exactly its one row is reindexed.
    assert_eq!(maintenance, 1, "association reindexes only the associated row");
    let inner = fixture.state.inner.lock().unwrap();
    assert_eq!(inner.test_run_observations[&id].evidence.len(), 2, "the exact row gains the check's association");
    for (key, row) in inner.test_run_observations.iter().filter(|(key, _)| key.as_str() != id) {
        assert_eq!(row.evidence.len(), 1, "{key} gains no association");
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

/// Check association visits only the owner/grant/tool bucket: unrelated
/// history adds none, same-bucket distractors are the bucket and are refused.
#[test]
fn check_association_visits_only_the_owner_grant_tool_bucket() {
    assert_eq!(association_candidates(16, 0), 1);
    assert_eq!(association_candidates(256, 0), 1, "unrelated history adds no association candidate");
    assert_eq!(association_candidates(16, 8), 9, "same-bucket distractors are measured as the bucket");
}
