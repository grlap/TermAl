//! Test-only child of src/tests/idle_state_lock.rs, using its history fixtures.
//! Owns the lifecycle-revalidation lock witnesses: the changed-row run lookup
//! equals the full projection's first match from single identities, and the
//! revalidating hold builds no full-history projection whatever the size of
//! unrelated history. No live-store inputs, latency thresholds or credit.
use super::*;

#[path = "test_run_history_index.rs"]
mod history_index;

// The previous changed-row lookup: first match by execution id, then by the
// artifact's run id and run directory, over the full public list.
fn reference_changed_row_summary(inner: &StateInner, id: &str) -> TestRunSummary {
    let runs = test_run_summaries_locked(inner);
    let row = &inner.test_run_observations[id];
    runs.iter()
        .find(|run| run.run_id == id)
        .or_else(|| {
            row.artifact.as_ref().and_then(|artifact| {
                runs.iter().find(|run| {
                    run.run_id == artifact.run_id
                        && test_run_path_key(&run.run_dir) == test_run_path_key(&artifact.run_directory)
                })
            })
        })
        .cloned()
        .unwrap_or_else(|| test_run_observation_summary(inner, row))
}

fn assert_identity_projection_equals_full(inner: &StateInner, label: &str) {
    let full = test_run_summaries_locked(inner);
    let ids: std::collections::BTreeSet<String> = full.iter().map(|run| run.run_id.clone())
        .chain(inner.test_run_observations.keys().cloned())
        .chain(inner.test_run_observations.values()
            .filter_map(|row| row.artifact.as_ref().map(|artifact| artifact.run_id.clone())))
        .chain(inner.test_run_compacted.keys().cloned())
        .chain(inner.test_run_registrations.keys().cloned())
        .collect();
    for id in &ids {
        let expected: Vec<_> = full.iter().filter(|run| &run.run_id == id).cloned().collect();
        assert_eq!(test_run_identity_summaries_locked(inner, id), expected, "identity {id} at {label}");
    }
    for (id, row) in &inner.test_run_observations {
        assert_eq!(
            test_run_changed_row_summary_locked(inner, id, row),
            reference_changed_row_summary(inner, id),
            "changed row {id} at {label}"
        );
    }
}

#[test]
fn lifecycle_lock_changed_row_lookup_equals_the_full_projection_first_match() {
    let fixture = registered_history(8);
    let mut inner = fixture.state.inner.lock().unwrap();
    for change in 0..8 {
        match change {
            // Registered fallback: the entry is gone, the registration stays.
            1 => inner.test_runs.entries.retain(|entry| entry.summary.run_id != "test-history-0000"),
            // Retained, uncompacted artifact with no entry or registration.
            2 => {
                let entry = inner.test_runs.find("test-history-0001").unwrap();
                let retained = TestRunCompacted {
                    compacted: false,
                    request_identity: entry.disk.request_identity.clone(),
                    request_stamp: entry.disk.request_stamp,
                    results_stamp: entry.disk.results_stamp,
                    summary: entry.summary.clone(),
                    registration: None,
                    observations: Vec::new(),
                    reason: "fixture retained artifact".to_owned(),
                    artifact_conflict: false,
                    artifact_unavailable: false,
                };
                inner.test_runs.entries.retain(|entry| entry.summary.run_id != "test-history-0001");
                inner.test_run_registrations.remove("test-history-0001");
                inner.test_run_compacted.insert("test-history-0001".to_owned(), retained);
            }
            // Duplicate entries for one run id.
            3 => {
                let duplicate = inner.test_runs.find("test-history-0002").unwrap().clone();
                inner.test_runs.entries.push(duplicate);
            }
            // An observation its artifact no longer merges, and one with none.
            4 => {
                inner.test_run_observations.get_mut("observed-history-0003").unwrap()
                    .artifact.as_mut().unwrap().run_directory.push_str("-different");
                inner.test_run_observations.get_mut("observed-history-0004").unwrap().artifact = None;
            }
            // An installed compacted summary with no entry or registration,
            // still awaiting revalidation, so it stays in the public list.
            5 => {
                let mut summary = inner.test_runs.find("test-history-0005").unwrap().summary.clone();
                summary.run_id = "compacted-history".to_owned();
                inner.test_runs.compactions_durable.insert("compacted-history".to_owned());
                inner.test_runs.compactions_revalidating.insert("compacted-history".to_owned());
                inner.test_run_compacted.insert("compacted-history".to_owned(), TestRunCompacted {
                    compacted: true,
                    request_identity: None,
                    request_stamp: None,
                    results_stamp: None,
                    summary,
                    registration: None,
                    observations: Vec::new(),
                    reason: "fixture compacted artifact".to_owned(),
                    artifact_conflict: false,
                    artifact_unavailable: true,
                });
            }
            // An unmerged observation whose summary run id is not its key.
            6 => {
                let mut row = inner.test_run_observations.get_mut("observed-history-0006").unwrap();
                row.artifact = None;
                row.summary.run_id = "test-history-0007".to_owned();
            }
            // Retained rows keyed apart from the run id their fallback carries.
            7 => {
                let registration = inner.test_run_registrations.remove("test-history-0004").unwrap();
                inner.test_run_registrations.insert("re-keyed-registration".to_owned(), registration);
                let retained = inner.test_run_compacted.remove("test-history-0001").unwrap();
                inner.test_run_compacted.insert("re-keyed-retained".to_owned(), retained);
            }
            _ => {}
        }
        assert_identity_projection_equals_full(&inner, &format!("change {change}"));
    }
    let full = test_run_summaries_locked(&inner);
    for exercised in ["test-history-0000", "test-history-0001", "observed-history-0003",
        "observed-history-0004", "compacted-history"]
    {
        assert!(full.iter().any(|run| run.run_id == exercised), "{exercised} is in the public list");
    }
    assert_eq!(full.iter().filter(|run| run.run_id == "test-history-0002").count(), 2);
    assert_eq!(full.iter().filter(|run| run.run_id == "test-history-0007").count(), 2);
    // The re-keyed registration still yields its own run id's fallback.
    assert_eq!(full.iter().filter(|run| run.run_id == "test-history-0004").count(), 2);
    assert!(!full.iter().any(|run| run.run_id.starts_with("re-keyed")));
}

// A populated history whose first `associated` rows carry an association to
// one completed lifecycle, so a revalidation changes exactly those rows.
fn associated_history(count: usize, associated: usize) -> (
    RunFixture, EngramAuthorityStoreKey, EngramControlWorkBinding, EngramNamedRootReadResponse,
    mpsc::Receiver<PersistRequest>,
) {
    let mut fixture = populated_history(count);
    // Production-style queued persistence and broadcast, as the idle witnesses.
    let (persist_tx, persist_rx) = mpsc::channel();
    fixture.state.persist_tx = persist_tx;
    fixture.state.persist_worker_alive.store(true, Ordering::Release);
    fixture.state.state_broadcast_mailbox = Some(Arc::new(StateBroadcastMailbox::default()));
    let store = EngramAuthorityStoreKey {
        database_path: fixture.root.join("tracker.db"),
        project_id: "canonical-project".to_owned(),
    };
    let binding = EngramControlWorkBinding {
        root_execution_id: "root-execution-one".to_owned(),
        work_id: "work-one".to_owned(),
        run_id: "run-one".to_owned(),
        work_revision: 1,
        claim_id: "claim-one".to_owned(),
        claim_fence: 1,
    };
    let mut inner = fixture.state.inner.lock().unwrap();
    for ordinal in 0..associated {
        let mut row = inner.test_run_observations.get_mut(&format!("observed-history-{ordinal:04}")).unwrap();
        row.work_binding = Some(binding.clone());
        row.evidence.push(TestRunEvidenceAssociation {
            store: store.clone(),
            binding: binding.clone(),
            source_basis: EngramExecutionSourceBasis {
                workspace_id: fixture.root.to_string_lossy().into_owned(),
                source_revision: "a".repeat(64),
                source_root_generation: Some(1),
                source_root_state: None,
            },
            producer_observation_id: "b".repeat(64),
            reported_outcome: EngramExecutionOutcome::Succeeded,
            action_fingerprint: "c".repeat(64),
            verification_record_id: Some("e".repeat(64)),
            checkpoint_id: Some("d".repeat(64)),
            confirmed_cursor: Some(3),
            lifecycle_read: None,
        });
    }
    drop(inner);
    let read = serde_json::from_value(json!({
        "project_id": store.project_id, "work_id": binding.work_id, "root_execution_id": binding.root_execution_id,
        "run_id": binding.run_id, "claim_id": binding.claim_id, "run": {"state": "completed", "generation": 1},
        "named_root": {"state": "none"},
        "latest_event": {"event": "f".repeat(64), "position": {"feed": {"kind": "run_execution", "id": binding.run_id}, "position": 2},
            "generation": 1, "kind": "ended", "workspace_id": fixture.root.to_string_lossy(), "named_at": "2026-10-14T01:00:00Z"},
        "read_cut": {"feed": {"kind": "run_execution", "id": binding.run_id}, "position": 3},
    })).unwrap();
    (fixture, store, binding, read, persist_rx)
}

// What one revalidation does under the lock: association matches, full
// projections, single-identity lookup candidates examined before filtering,
// the lifecycle index candidates its selection visits, and the index
// maintenance operations its result-only updates perform.
struct RevalidationCost {
    visits: usize,
    projections: usize,
    candidates: usize,
    indexed: usize,
    maintenance: usize,
}

fn revalidation_cost(count: usize, associated: usize) -> RevalidationCost {
    let (fixture, store, binding, read, _queued) = associated_history(count, associated);
    TEST_TEST_RUN_FULL_PROJECTIONS.with(|count| count.set(0));
    TEST_TEST_RUN_IDENTITY_CANDIDATES.with(|count| count.set(0));
    TEST_TEST_RUN_INDEXED_CANDIDATES.with(|count| count.set(0));
    TEST_HISTORY_INDEX_MAINTENANCE.with(|count| count.set(0));
    // A timing witness measures the production path: the test-only contract
    // guard of update_unindexed_fields is skipped for this scope only.
    let (_, visits) = {
        let _timing = SkipContractGuardForTiming::new();
        measure(&format!("lifecycle_revalidation_{count}_{associated}"), || {
            fixture.state.revalidate_test_run_lifecycle(&store, &binding, &read)
        })
    };
    assert!(!contract_guard_skipped(), "the timing scope restores the contract guard");
    let projections = TEST_TEST_RUN_FULL_PROJECTIONS.with(|count| count.get());
    let candidates = TEST_TEST_RUN_IDENTITY_CANDIDATES.with(|count| count.get());
    let indexed = TEST_TEST_RUN_INDEXED_CANDIDATES.with(|count| count.get());
    let maintenance = TEST_HISTORY_INDEX_MAINTENANCE.with(|count| count.get());
    let inner = fixture.state.inner.lock().unwrap();
    for ordinal in 0..associated {
        assert!(
            inner.test_run_observations[&format!("observed-history-{ordinal:04}")].evidence[0]
                .lifecycle_read.is_some(),
            "associated row {ordinal} is revalidated"
        );
    }
    RevalidationCost { visits, projections, candidates, indexed, maintenance }
}

/// Unrelated history adds no projection, no association matching and no
/// selection candidate to the revalidating hold: the lifecycle bucket is the
/// only history it visits.
#[test]
fn lifecycle_lock_revalidation_builds_no_full_projection_or_unrelated_matching() {
    let small = revalidation_cost(16, 1);
    let large = revalidation_cost(256, 1);
    assert_eq!((small.projections, large.projections), (0, 0),
        "the revalidating hold builds no full-history summary projection");
    assert_eq!(small.visits, large.visits,
        "unrelated history adds no association matching: 16 rows {}, 256 rows {}", small.visits, large.visits);
    assert_eq!((small.indexed, large.indexed), (1, 1),
        "selection visits only the lifecycle bucket: 16 rows {}, 256 rows {}", small.indexed, large.indexed);
    assert_eq!((small.maintenance, large.maintenance), (0, 0), "revalidation performs no index maintenance");
}

/// Every retained row changed in one batch: the lookups share one index, so
/// candidates examined stay linear in the batch, never rows times history.
#[test]
fn lifecycle_lock_batch_lookup_stays_linear_when_every_row_changes() {
    for count in [16, 256] {
        let cost = revalidation_cost(count, count);
        assert_eq!(cost.projections, 0, "{count} changed rows build no full projection");
        assert!(cost.candidates <= 8 * count,
            "{count} changed rows examined {} lookup candidates, not linear", cost.candidates);
        assert!(cost.visits <= 4 * count,
            "{count} changed rows made {} association matches, not linear", cost.visits);
        // A hot bucket: every row shares the lifecycle, so the bucket is the history.
        assert_eq!(cost.indexed, count, "{count} rows in one lifecycle bucket are each visited once");
        assert_eq!(cost.maintenance, 0, "{count} revalidated rows need no index remove or re-add");
    }
}
