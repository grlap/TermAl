//! Test-only child of src/tests/test_run_lifecycle_lock.rs, using the history
//! fixtures of src/tests/idle_state_lock.rs. Owns the derived-index oracle for
//! the retained test-run history stores and the run entry positions: after
//! every mutation kind, whole-store replacement and load, each index equals a
//! full rebuild, and every bucket equals the full scan's matches in map key
//! order. Also owns the persistence stamp witnesses of the store helpers.
//! No live-store inputs, latency thresholds or credit.
use super::*;

fn lifecycle_key(association: &TestRunEvidenceAssociation) -> TestRunLifecycleKey {
    TestRunLifecycleKey::new(
        &association.store,
        &association.binding.root_execution_id,
        &association.binding.work_id,
        &association.binding.run_id,
        &association.binding.claim_id,
    )
}

/// The previous full scan: the map keys whose rows match, in key order.
fn scan<'a, V: 'a>(
    rows: impl IntoIterator<Item = (&'a String, &'a V)>,
    matches: impl Fn(&V) -> bool,
) -> Vec<&'a String> {
    rows.into_iter().filter(|(_, row)| matches(row)).map(|(key, _)| key).collect()
}

/// Every bucket a row can name, queried through the index, equals the full
/// scan for that bucket: same keys, same multiplicity, same order.
fn assert_buckets_equal_full_scan(inner: &StateInner, label: &str) {
    let observations = &inner.test_run_observations;
    for row in observations.values() {
        for association in &row.evidence {
            let key = lifecycle_key(association);
            assert_eq!(
                observations.index().lifecycle(&key).collect::<Vec<_>>(),
                scan(observations, |row| row.evidence.iter().any(|other| lifecycle_key(other) == key)),
                "observation lifecycle bucket at {label}"
            );
            let producer = &association.producer_observation_id;
            assert_eq!(
                observations.index().producer(producer).collect::<Vec<_>>(),
                scan(observations, |row| row.evidence.iter()
                    .any(|other| &other.producer_observation_id == producer)),
                "observation producer bucket at {label}"
            );
        }
        let bucket = (row.summary.owner_session_id.clone(), row.grant_identity.clone(), row.tool_identity.clone());
        assert_eq!(
            observations.index().check(&bucket).collect::<Vec<_>>(),
            scan(observations, |row| row.summary.owner_session_id == bucket.0
                && row.grant_identity == bucket.1 && row.tool_identity == bucket.2),
            "observation check bucket at {label}"
        );
        let run = &row.summary.run_id;
        assert_eq!(
            observations.index().summary_run(run).collect::<Vec<_>>(),
            scan(observations, |row| &row.summary.run_id == run),
            "observation summary run bucket at {label}"
        );
        if let Some(artifact) = &row.artifact {
            assert_eq!(
                observations.index().artifact_run(&artifact.run_id).collect::<Vec<_>>(),
                scan(observations, |row| row.artifact.as_ref()
                    .is_some_and(|other| other.run_id == artifact.run_id)),
                "observation artifact run bucket at {label}"
            );
        }
    }
    let compacted = &inner.test_run_compacted;
    for row in compacted.values() {
        for observation in &row.observations {
            for association in &observation.evidence {
                let key = lifecycle_key(association);
                assert_eq!(
                    compacted.index().lifecycle(&key).collect::<Vec<_>>(),
                    scan(compacted, |row| row.observations.iter()
                        .any(|observation| observation.evidence.iter().any(|other| lifecycle_key(other) == key))),
                    "compacted lifecycle bucket at {label}"
                );
            }
            let execution = &observation.execution_id;
            assert_eq!(
                compacted.index().execution(execution).collect::<Vec<_>>(),
                scan(compacted, |row| row.observations.iter().any(|other| &other.execution_id == execution)),
                "compacted execution bucket at {label}"
            );
        }
        let run = &row.summary.run_id;
        assert_eq!(
            compacted.index().summary_run(run).collect::<Vec<_>>(),
            scan(compacted, |row| &row.summary.run_id == run),
            "compacted summary run bucket at {label}"
        );
    }
    let registrations = &inner.test_run_registrations;
    for row in registrations.values() {
        assert_eq!(
            registrations.index().run(&row.run_id).collect::<Vec<_>>(),
            scan(registrations, |other| other.run_id == row.run_id),
            "registration run bucket at {label}"
        );
    }
    assert_eq!(observations.index().summary_run("absent-run").count(), 0, "absent bucket at {label}");
    assert_eq!(registrations.index().run("absent-run").count(), 0, "absent bucket at {label}");
    for (position, entry) in inner.test_runs.entries.iter().enumerate() {
        let found: Vec<usize> = inner.test_runs.entries.with_run_id(&entry.summary.run_id)
            .map(|found| inner.test_runs.entries.iter().position(|other| std::ptr::eq(other, found)).unwrap())
            .collect();
        let expected: Vec<usize> = inner.test_runs.entries.iter().enumerate()
            .filter(|(_, other)| other.summary.run_id == entry.summary.run_id)
            .map(|(position, _)| position)
            .collect();
        assert_eq!(found, expected, "entry {position} positions at {label}");
    }
}

fn assert_history_indexes_consistent(inner: &StateInner, label: &str) {
    assert_eq!(
        inner.test_run_observations.index(),
        &inner.test_run_observations.rebuilt_index(),
        "observation index at {label}"
    );
    assert_eq!(
        inner.test_run_compacted.index(),
        &inner.test_run_compacted.rebuilt_index(),
        "compacted index at {label}"
    );
    assert_eq!(
        inner.test_run_registrations.index(),
        &inner.test_run_registrations.rebuilt_index(),
        "registration index at {label}"
    );
    assert_eq!(
        inner.test_runs.entries.positions(),
        &inner.test_runs.entries.rebuilt_positions(),
        "entry positions at {label}"
    );
    assert_buckets_equal_full_scan(inner, label);
}

fn association(fixture: &RunFixture, work: &str, producer: &str) -> TestRunEvidenceAssociation {
    TestRunEvidenceAssociation {
        store: EngramAuthorityStoreKey {
            database_path: fixture.root.join("tracker.db"),
            project_id: "canonical-project".to_owned(),
        },
        binding: EngramControlWorkBinding {
            root_execution_id: "root-execution-one".to_owned(),
            work_id: work.to_owned(),
            run_id: "run-one".to_owned(),
            work_revision: 1,
            claim_id: "claim-one".to_owned(),
            claim_fence: 1,
        },
        source_basis: EngramExecutionSourceBasis {
            workspace_id: fixture.root.to_string_lossy().into_owned(),
            source_revision: "a".repeat(64),
            source_root_generation: Some(1),
            source_root_state: None,
        },
        producer_observation_id: producer.to_owned(),
        reported_outcome: EngramExecutionOutcome::Succeeded,
        action_fingerprint: "c".repeat(64),
        verification_record_id: None,
        checkpoint_id: None,
        confirmed_cursor: None,
        lifecycle_read: None,
    }
}

fn retained(summary: TestRunSummary, observations: Vec<TestRunCommandObservation>) -> TestRunCompacted {
    TestRunCompacted {
        compacted: true,
        request_identity: None,
        request_stamp: None,
        results_stamp: None,
        summary,
        registration: None,
        observations,
        reason: "fixture compacted artifact".to_owned(),
        artifact_conflict: false,
        artifact_unavailable: true,
    }
}

/// Inserts, replacements, row updates (closure and guard), removals,
/// retains, clears and entry-vector changes each leave every index equal to
/// a full rebuild and every bucket equal to the full scan, including rows
/// keyed apart from their own identities and buckets shared by many rows.
#[test]
fn history_index_equals_a_full_rebuild_after_every_mutation_kind() {
    let fixture = registered_history(6);
    let mut inner = fixture.state.inner.lock().unwrap();
    assert_history_indexes_consistent(&inner, "fixture");
    let template = inner.test_run_observations["observed-history-0000"].clone();
    let first = association(&fixture, "work-one", "producer-one");
    let second = association(&fixture, "work-two", "producer-two");

    // A row keyed apart from its execution id, with one lifecycle twice and
    // a producer shared with another row.
    let mut extra = template.clone();
    extra.evidence = vec![first.clone(), first.clone(), second.clone()];
    inner.test_run_observations.insert("re-keyed-observation".to_owned(), extra);
    inner.test_run_observations.update("observed-history-0001", |row| row.evidence.push(second.clone()));
    assert_history_indexes_consistent(&inner, "insert apart from identity");

    // A replacement under an existing key moves every bucket.
    let mut replaced = template.clone();
    replaced.tool_identity = "replaced-tool".to_owned();
    replaced.grant_identity = None;
    replaced.artifact = None;
    replaced.summary.run_id = "replaced-run".to_owned();
    replaced.evidence = vec![first.clone()];
    let previous = inner.test_run_observations.insert("observed-history-0002".to_owned(), replaced);
    assert!(previous.is_some());
    assert_history_indexes_consistent(&inner, "replacement");

    // A closure update moves one row between buckets; a missing key changes nothing.
    let moved = inner.test_run_observations.update("observed-history-0003", |row| {
        row.grant_identity = Some("moved-grant".to_owned());
        row.summary.owner_session_id = None;
        row.artifact.as_mut().unwrap().run_id = "moved-artifact-run".to_owned();
        row.evidence.push(first.clone());
        row.execution_id.clone()
    });
    assert_eq!(moved.as_deref(), Some("observed-history-0003"));
    assert!(inner.test_run_observations.update("missing-row", |_| ()).is_none());
    assert_history_indexes_consistent(&inner, "closure update");

    // The test-only guard reindexes the row's final fields on drop.
    {
        let mut row = inner.test_run_observations.get_mut("observed-history-0004").unwrap();
        row.evidence = vec![second.clone()];
        row.summary.run_id = "guarded-run".to_owned();
    }
    assert_history_indexes_consistent(&inner, "guard");
    inner.test_run_observations.for_each_mut(|row| row.evidence.retain(|other| other.binding.work_id != "work-two"));
    assert_history_indexes_consistent(&inner, "every row");
    inner.test_run_observations.remove("re-keyed-observation");
    assert!(inner.test_run_observations.remove("re-keyed-observation").is_none());
    inner.test_run_observations.retain(|key, _| key != "observed-history-0005");
    assert_history_indexes_consistent(&inner, "removal");

    // Compacted proofs: summary run apart from the key, shared executions.
    let summary = inner.test_runs.find("test-history-0000").unwrap().summary.clone();
    let mut proof = template.clone();
    proof.evidence = vec![first.clone()];
    inner.test_run_compacted.insert("compacted-one".to_owned(), retained(summary.clone(), vec![proof.clone()]));
    inner.test_run_compacted.insert("compacted-two".to_owned(), retained(summary.clone(), vec![proof.clone(), template.clone()]));
    assert_history_indexes_consistent(&inner, "compacted insert");
    inner.test_run_compacted.update("compacted-two", |row| {
        row.observations.retain(|observation| !observation.evidence.is_empty());
        row.summary.run_id = "compacted-run".to_owned();
    });
    inner.test_run_compacted.remove("compacted-one");
    assert_history_indexes_consistent(&inner, "compacted update and removal");

    // Registrations keyed apart from the run id they carry.
    let registration = inner.test_run_registrations.remove("test-history-0001").unwrap();
    inner.test_run_registrations.insert("re-keyed-registration".to_owned(), registration.clone());
    inner.test_run_registrations.insert("second-registration".to_owned(), registration);
    assert_history_indexes_consistent(&inner, "registration re-key");
    inner.test_run_registrations.retain(|key, _| key != "second-registration");
    assert_history_indexes_consistent(&inner, "registration retain");

    // The entry vector: duplicates, removal shifts, retain, in-place change.
    let duplicate = inner.test_runs.find("test-history-0002").unwrap().clone();
    inner.test_runs.entries.push(duplicate);
    assert_history_indexes_consistent(&inner, "duplicate entry");
    inner.test_runs.entries.remove(1);
    assert_history_indexes_consistent(&inner, "entry removal");
    inner.test_runs.entries.update(0, |entry| entry.summary.run_id = "test-history-0002".to_owned());
    inner.test_runs.entries.retain(|entry| entry.summary.run_id != "test-history-0003");
    assert_history_indexes_consistent(&inner, "entry change and retain");

    inner.test_run_observations.clear();
    inner.test_run_compacted.clear();
    inner.test_run_registrations.clear();
    inner.test_runs.entries.clear();
    assert_history_indexes_consistent(&inner, "clear");
    assert_eq!(inner.test_run_observations.index(), &Default::default());
}

/// A load rebuilds every index from the persisted rows, and replacing a
/// whole store with an earlier clone (the rollback shape) carries the index
/// that belongs to those rows.
#[test]
fn history_index_is_rebuilt_on_load_and_carried_by_whole_store_replacement() {
    let fixture = registered_history(4);
    let mut inner = fixture.state.inner.lock().unwrap();
    let first = association(&fixture, "work-one", "producer-one");
    inner.test_run_observations.update("observed-history-0000", |row| row.evidence.push(first.clone()));
    let summary = inner.test_runs.find("test-history-0001").unwrap().summary.clone();
    let proof = inner.test_run_observations["observed-history-0000"].clone();
    inner.test_run_compacted.insert("compacted-one".to_owned(), retained(summary, vec![proof]));

    let loaded = PersistedState::from_inner(&inner).into_inner().unwrap();
    assert_history_indexes_consistent(&loaded, "load");
    assert_eq!(
        loaded.test_run_observations.index().lifecycle(&lifecycle_key(&first)).collect::<Vec<_>>(),
        ["observed-history-0000"],
        "the loaded index answers the persisted association"
    );
    assert_eq!(loaded.test_run_compacted.index().lifecycle(&lifecycle_key(&first)).count(), 1);
    assert_eq!(loaded.test_run_registrations.len(), inner.test_run_registrations.len());

    let before = inner.test_run_observations.clone();
    let before_compacted = inner.test_run_compacted.clone();
    inner.test_run_observations.remove("observed-history-0000");
    inner.test_run_compacted.clear();
    assert_eq!(inner.test_run_observations.index().lifecycle(&lifecycle_key(&first)).count(), 0);
    inner.test_run_observations = before;
    inner.test_run_compacted = before_compacted;
    assert_history_indexes_consistent(&inner, "whole-store replacement");
    assert_eq!(inner.test_run_observations.index().lifecycle(&lifecycle_key(&first)).count(), 1);
}

/// Every store helper that changes rows stamps the map for metadata capture,
/// as the map's own mutable borrow did; reads and index queries do not.
#[test]
fn history_store_mutations_stamp_capture_and_reads_do_not() {
    let fixture = registered_history(3);
    let mut inner = fixture.state.inner.lock().unwrap();
    let template = inner.test_run_observations["observed-history-0000"].clone();
    let stamp = inner.test_run_observations.stamp();
    let _ = inner.test_run_observations.get("observed-history-0000");
    let _ = inner.test_run_observations.index().summary_run("observed-history-0000").count();
    let _ = inner.test_run_observations.clone();
    assert!(inner.test_run_observations.stamp() == stamp, "reads and clones leave the stamp");

    let mut changes: Vec<(&str, Box<dyn Fn(&mut StateInner)>)> = vec![
        ("insert", Box::new(move |inner| {
            inner.test_run_observations.insert("inserted".to_owned(), template.clone());
        })),
        ("update", Box::new(|inner| {
            inner.test_run_observations.update("observed-history-0001", |row| row.validated_unbound = false);
        })),
        ("update_unindexed_fields", Box::new(|inner| {
            inner.test_run_observations.update_unindexed_fields("observed-history-0001", |row| {
                row.validated_unbound = true;
            });
        })),
        ("remove", Box::new(|inner| {
            inner.test_run_observations.remove("inserted");
        })),
        ("retain", Box::new(|inner| inner.test_run_observations.retain(|_, _| true))),
        ("guard", Box::new(|inner| {
            inner.test_run_observations.get_mut("observed-history-0002").unwrap().launch_only = true;
        })),
        ("clear", Box::new(|inner| inner.test_run_observations.clear())),
    ];
    for (label, change) in changes.drain(..) {
        let before = inner.test_run_observations.stamp();
        change(&mut *inner);
        assert!(inner.test_run_observations.stamp() != before, "{label} stamps the observation map");
    }
    let before = inner.test_run_registrations.stamp();
    inner.test_run_registrations.remove("test-history-0000");
    assert!(inner.test_run_registrations.stamp() != before, "registration removal stamps");
    let before = inner.test_run_compacted.stamp();
    inner.test_run_compacted.retain(|_, _| true);
    assert!(inner.test_run_compacted.stamp() != before, "compacted retain stamps");
}

/// Result-only changes through the index-neutral helper perform no index
/// remove or re-add, and the index still equals a full rebuild.
#[test]
fn index_neutral_update_does_no_maintenance_and_keeps_the_index() {
    assert!(!contract_guard_skipped(), "ordinary tests enforce the update_unindexed_fields contract");
    let fixture = registered_history(3);
    let mut inner = fixture.state.inner.lock().unwrap();
    let first = association(&fixture, "work-one", "producer-one");
    inner.test_run_observations.update("observed-history-0000", |row| row.evidence.push(first.clone()));
    TEST_HISTORY_INDEX_MAINTENANCE.with(|count| count.set(0));
    let filled = inner.test_run_observations.update_unindexed_fields("observed-history-0000", |row| {
        let association = row.evidence.last_mut().unwrap();
        association.verification_record_id = Some("a".repeat(64));
        association.checkpoint_id = Some("b".repeat(64));
        association.confirmed_cursor = Some(7);
    });
    assert!(filled.is_some());
    assert!(inner.test_run_observations.update_unindexed_fields("missing-row", |_| ()).is_none());
    assert_eq!(TEST_HISTORY_INDEX_MAINTENANCE.with(|count| count.get()), 0, "no index maintenance");
    assert_history_indexes_consistent(&inner, "index-neutral update");
}

/// The helper's contract is enforced in test builds: changing an indexed
/// field through it panics in every test that reaches the change.
#[test]
#[should_panic(expected = "update_unindexed_fields changed an indexed field")]
fn index_neutral_update_refuses_an_indexed_change() {
    let fixture = registered_history(1);
    let mut inner = fixture.state.inner.lock().unwrap();
    inner.test_run_observations.update_unindexed_fields("observed-history-0000", |row| {
        row.tool_identity = "another-tool".to_owned();
    });
}

/// The timing-only switch is off by default, on only inside its scope, and
/// restored when the scope ends, including when the scope panics.
#[test]
fn contract_guard_skip_is_scoped_and_restored_on_panic() {
    assert!(!contract_guard_skipped());
    {
        let _timing = SkipContractGuardForTiming::new();
        assert!(contract_guard_skipped());
    }
    assert!(!contract_guard_skipped(), "dropping the scope restores the guard");
    let unwound = std::panic::catch_unwind(|| {
        let _timing = SkipContractGuardForTiming::new();
        panic!("timing witness failed");
    });
    assert!(unwound.is_err());
    assert!(!contract_guard_skipped(), "a panicking scope still restores the guard");
}
