//! Owns idle history projection cost witnesses; no live-store inputs or latency thresholds.
use super::*;

fn populated_history(count: usize) -> RunFixture {
    let fixture = RunFixture::new("idle-history-projection");
    let session = create_test_project_session(
        &fixture.state,
        Agent::Codex,
        &fixture.project_id,
        &fixture.root,
    );
    fixture.state.note_engram_command_started(
        &session,
        &EngramObservationProvenance::Ambient,
        "seed-observation",
        Some("cargo test"),
        Some(&fixture.root.to_string_lossy()),
    );
    let directory = write_run(
        &fixture.runs_dir(),
        "test-seed",
        full_request(&fixture.root, "2026-10-25T00:00:00Z"),
        Some(passed_results()),
    );
    let disk = TestRunDisk::read(&directory, None).unwrap();
    let mut inner = fixture.state.inner.lock().unwrap();
    let observation = inner.test_run_observations.values().next().unwrap().clone();
    inner.test_run_observations.clear();
    // Build a history population in memory; avoid N fixture commits or files.
    for ordinal in 0..count {
        let mut disk = disk.clone();
        disk.run_id = format!("test-history-{ordinal:04}");
        disk.owner = Some(session.clone());
        disk.run_dir = fixture.runs_dir().join(&disk.run_id);
        let mut summary = test_run_summary(
            &inner,
            &TestRunScanned {
                disk: Arc::new(disk.clone()),
                state: TestRunState::Passed,
                unknown_reason: None,
                project_id: Some(fixture.project_id.clone()),
            },
        );
        summary.registration.restart_would_void_evidence = None;
        inner.test_runs.entries.push(TestRunEntry {
            summary,
            disk: Arc::new(disk.clone()),
            discovered_project_id: Some(fixture.project_id.clone()),
        });
        let mut row = observation.clone();
        row.execution_id = format!("observed-history-{ordinal:04}");
        row.summary.run_id = row.execution_id.clone();
        row.artifact = Some(TestRunObservedArtifact {
            run_id: disk.run_id,
            run_directory: test_run_display_path(&disk.run_dir),
            request_identity: disk.request_identity,
            terminal_digest: None,
        });
        inner
            .test_run_observations
            .insert(row.execution_id.clone(), row);
    }
    drop(inner);
    fixture
}

fn registered_history(count: usize) -> RunFixture {
    let fixture = populated_history(count);
    let mut inner = fixture.state.inner.lock().unwrap();
    for ordinal in 0..count {
        let execution = format!("observed-history-{ordinal:04}");
        let registration = {
            let disk = Arc::make_mut(&mut inner.test_runs.entries[ordinal].disk);
            disk.locator_identity = Some(format!("fixture-locator-{ordinal}"));
            TestRunRegistration {
                run_id: disk.run_id.clone(),
                project_id: Some(fixture.project_id.clone()),
                run_directory: test_run_display_path(&disk.run_dir),
                locator_identity: disk.locator_identity.clone(),
                worktree: disk.worktree.clone(),
                request_identity: disk.request_identity.clone().unwrap(),
                initiating_session_id: disk.owner.clone().unwrap(),
                owning_session_id: disk.owner.clone().unwrap(),
                creator_pid: disk.creator_pid,
                registered_at: "2026-10-25T00:00:00Z".to_owned(),
                started_at: disk.started_at.clone(),
                preset: disk.preset,
                detached: disk.detached,
                stage_names: Vec::new(),
                work_binding: None,
                authority_store: None,
                validated_unbound: true,
                monitoring_interrupted_at: None,
            }
        };
        let row = inner.test_run_observations.get_mut(&execution).unwrap();
        row.runtime_identity = Some("fixture-original-runtime".to_owned());
        row.validated_unbound = true;
        row.summary.state = TestRunState::Passed;
        row.summary.ended_at = Some("2026-10-25T00:01:00Z".to_owned());
        row.summary.exit_code = Some(0);
        row.final_unbound_proof = Some(TestRunFinalUnboundProof {
            version: 1,
            terminal_identity: test_run_final_unbound_identity(row).unwrap(),
        });
        inner.test_runs.observations_durable.insert(execution);
        inner
            .test_runs
            .registered
            .insert(registration.run_id.clone());
        inner
            .test_run_registrations
            .insert(registration.run_id.clone(), registration);
    }
    drop(inner);
    fixture
}

#[test]
fn idle_history_registered_projection_counts_registration_candidates_too() {
    let fixture = registered_history(256);
    let inner = fixture.state.inner.lock().unwrap();
    let (runs, visits) = measure("registered_history_projection", || {
        test_run_full_summaries_locked(&inner)
    });
    assert_eq!(runs.len(), 256);
    assert!(
        runs.iter()
            .all(|run| run.project_id.as_ref() == Some(&fixture.project_id)
                && run.registration.registration_state.as_deref() == Some("registered")
                && run.registration.restart_would_void_evidence == Some(false))
    );
    assert!(
        visits <= 3 * 256,
        "including registration/project attribution, one cut must stay linear: {visits}"
    );
    let projection = TestRunProjection::new(&inner);
    let (scanned, visits) = measure("registered_scanner_projection", || {
        inner
            .test_runs
            .entries
            .iter()
            .map(|entry| {
                test_run_summary_from_observations(
                    &inner,
                    &TestRunScanned {
                        disk: entry.disk.clone(),
                        state: TestRunState::Passed,
                        unknown_reason: None,
                        project_id: Some(fixture.project_id.clone()),
                    },
                    projection
                        .observations(&entry.summary.run_id)
                        .iter()
                        .copied(),
                )
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(scanned, runs);
    assert!(
        visits <= 3 * 256,
        "scanner project attribution must stay indexed: {visits}"
    );
}

fn measure<T>(label: &str, action: impl FnOnce() -> T) -> (T, usize) {
    TEST_TEST_RUN_ASSOCIATION_VISITS.with(|count| count.set(0));
    let started = std::time::Instant::now();
    let value = action();
    let visits = TEST_TEST_RUN_ASSOCIATION_VISITS.with(|count| count.get());
    eprintln!(
        "idle_history_profile phase={label} elapsed_us={} association_visits={visits}",
        started.elapsed().as_micros()
    );
    (value, visits)
}

#[test]
fn idle_history_profile_and_projection_has_no_cartesian_association_scan() {
    let mut fixture = populated_history(1024);
    // Production-style queued persistence/broadcast: do not profile the test
    // fixture's disconnected-worker synchronous SQLite/JSON fallback.
    let (persist_tx, _persist_rx) = mpsc::channel();
    fixture.state.persist_tx = persist_tx;
    fixture.state.state_broadcast_mailbox = Some(Arc::new(StateBroadcastMailbox::default()));
    let (summary, visits) = {
        let mut inner = fixture.state.inner.lock().unwrap();
        measure("commit_housekeeping", || {
            inner.retain_hidden_source_observation_sessions()
        });
        measure("revision_persist_enqueue", || {
            fixture
                .state
                .bump_revision_and_persist_locked(&mut inner)
                .unwrap()
        });
        measure("immutable_snapshot", || {
            fixture.state.immutable_state_snapshot_locked(&inner)
        });
        measure("history_projection", || test_run_summaries_locked(&inner))
    };
    assert_eq!(summary.len(), 1024);
    assert!(
        visits <= 1024 * 2,
        "one exact artifact association per run must not visit the entire history per artifact: {visits}"
    );
}

#[test]
fn idle_history_compaction_tick_does_not_reproject_unchanged_history() {
    let fixture = populated_history(256);
    // A scan has already considered this unchanged history. No persistence
    // completions or newly eligible rows exist on subsequent idle ticks.
    fixture.state.step_test_run_compaction();
    let (_, visits) = measure("unchanged_compaction_tick", || {
        fixture.state.step_test_run_compaction()
    });
    assert_eq!(
        visits, 0,
        "idle compaction must not reconstruct every historical summary"
    );
    fixture.state.inner.lock().unwrap().revision += 1;
    let (_, visits) = measure("changed_revision_compaction_tick", || {
        fixture.state.step_test_run_compaction()
    });
    assert!(
        visits > 0,
        "a new revision must recheck eligibility rather than reuse old safety"
    );
    let (_, visits) = measure("idle_again_compaction_tick", || {
        fixture.state.step_test_run_compaction()
    });
    assert_eq!(visits, 0);
}

// Deliberately unindexed reference projection retained only for small tests.
// It follows the previous public selection/ordering and exact predicates.
fn reference_history(inner: &StateInner) -> Vec<TestRunSummary> {
    let mut runs = inner.test_runs.summaries();
    for run in &mut runs {
        if let Some(entry) = inner.test_runs.find(&run.run_id) {
            run.registration = test_run_registration_view(inner, &entry.disk);
            // Preserve the previous project path too, independent of the new
            // shared identity-only helper used by production attribution.
            run.project_id = (test_run_registration_view(inner, &entry.disk)
                .registration_state
                .as_deref()
                == Some("registered"))
            .then(|| inner.test_run_registrations.get(&entry.disk.run_id))
            .flatten()
            .and_then(|row| row.project_id.clone())
            .filter(|id| inner.projects.iter().any(|project| &project.id == id))
            .or_else(|| entry.discovered_project_id.clone());
        }
    }
    let retained: std::collections::BTreeSet<_> = inner
        .test_run_registrations
        .keys()
        .chain(inner.test_run_compacted.keys())
        .collect();
    runs.extend(retained.into_iter().filter_map(|id| {
        inner
            .test_runs
            .find(id)
            .is_none()
            .then(|| test_run_artifact_fallback(inner, id))
            .flatten()
    }));
    let mut merged = HashSet::new();
    for run in &mut runs {
        let disk = inner
            .test_runs
            .find(&run.run_id)
            .map(|entry| entry.disk.as_ref());
        merged.extend(test_run_enrich_artifact_summary(inner, run, disk));
    }
    runs.extend(
        inner
            .test_run_observations
            .values()
            .filter(|row| !merged.contains(&row.execution_id))
            .map(|row| test_run_observation_summary(inner, row)),
    );
    runs.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| a.run_id.cmp(&b.run_id))
    });
    runs
}

#[test]
fn idle_history_index_is_only_a_candidate_filter_and_proof_changes_are_fresh() {
    let fixture = populated_history(8);
    let mut inner = fixture.state.inner.lock().unwrap();
    for change in 0..6 {
        match change {
            1 => {
                inner
                    .test_run_observations
                    .get_mut("observed-history-0000")
                    .unwrap()
                    .summary
                    .owner_session_id = Some("foreign-owner".to_owned())
            }
            2 => inner
                .test_run_observations
                .get_mut("observed-history-0001")
                .unwrap()
                .artifact
                .as_mut()
                .unwrap()
                .run_directory
                .push_str("-different"),
            3 => {
                inner
                    .test_run_observations
                    .get_mut("observed-history-0002")
                    .unwrap()
                    .artifact
                    .as_mut()
                    .unwrap()
                    .request_identity = Some("different-request".to_owned())
            }
            4 => {
                inner
                    .test_run_observations
                    .get_mut("observed-history-0003")
                    .unwrap()
                    .artifact
                    .as_mut()
                    .unwrap()
                    .terminal_digest = Some("different-terminal".to_owned())
            }
            5 => {
                inner.test_runs.entries.remove(4);
            }
            _ => {}
        }
        let expected = reference_history(&inner);
        let actual = test_run_full_summaries_locked(&inner);
        assert_eq!(
            serde_json::to_value(&actual).unwrap(),
            serde_json::to_value(&expected).unwrap(),
            "fresh proof/candidate change {change}"
        );
        for run in &expected {
            assert_eq!(
                test_run_compaction_summary(&inner, &run.run_id).as_ref(),
                Some(run)
            );
        }
        for id in inner.test_run_observations.keys() {
            assert_eq!(
                test_run_compaction_summary(&inner, id).is_some(),
                expected.iter().any(|run| &run.run_id == id),
                "merged observation {id}"
            );
        }
    }
}

#[test]
fn idle_history_registered_and_retained_fallbacks_equal_the_reference_projection() {
    let fixture = registered_history(8);
    let mut inner = fixture.state.inner.lock().unwrap();
    for change in 0..6 {
        match change {
            1 => inner
                .test_runs
                .entries
                .retain(|entry| entry.summary.run_id != "test-history-0000"),
            2 => {
                // A retained unresolved artifact with no live registration or
                // disk still merges only its exact request-correlated alias.
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
                inner
                    .test_runs
                    .entries
                    .retain(|entry| entry.summary.run_id != "test-history-0001");
                inner.test_run_registrations.remove("test-history-0001");
                inner
                    .test_run_compacted
                    .insert("test-history-0001".to_owned(), retained);
            }
            3 => {
                let duplicate = inner.test_runs.find("test-history-0002").unwrap().clone();
                inner.test_runs.entries.push(duplicate);
            }
            4 => {
                inner
                    .test_runs
                    .observations_durable
                    .remove("observed-history-0003");
            }
            5 => {
                inner
                    .test_run_registrations
                    .get_mut("test-history-0004")
                    .unwrap()
                    .request_identity
                    .push_str("-superseded");
            }
            _ => {}
        }
        let expected = reference_history(&inner);
        let actual = test_run_full_summaries_locked(&inner);
        assert_eq!(actual, expected, "registration/fallback change {change}");
        if change == 0 {
            assert!(
                actual
                    .iter()
                    .all(|run| run.registration.restart_would_void_evidence == Some(false))
            );
        }
        if change >= 1 {
            let fallback = actual
                .iter()
                .find(|run| run.run_id == "test-history-0000")
                .unwrap();
            assert_eq!(
                fallback.registration.registration_state.as_deref(),
                Some("registered")
            );
            assert_eq!(
                fallback.registration.restart_would_void_evidence,
                Some(false)
            );
        }
        if change >= 2 {
            let retained = actual
                .iter()
                .find(|run| run.run_id == "test-history-0001")
                .unwrap();
            assert_eq!(retained.registration.history_compacted, Some(false));
            assert!(
                retained
                    .registration
                    .superseded_run_ids
                    .iter()
                    .any(|id| id == "observed-history-0001")
            );
            assert!(
                !actual
                    .iter()
                    .any(|run| run.run_id == "observed-history-0001")
            );
        }
        for run in &expected {
            assert_eq!(
                test_run_compaction_summary(&inner, &run.run_id).as_ref(),
                Some(run),
                "single-summary selection {} at change{change}",
                run.run_id
            );
        }
        for id in inner.test_run_observations.keys() {
            assert_eq!(
                test_run_compaction_summary(&inner, id).is_some(),
                expected.iter().any(|run| &run.run_id == id),
                "alias selection {id} at change{change}"
            );
        }
    }
}

#[test]
fn idle_history_single_compaction_completion_does_not_project_unrelated_artifacts() {
    let fixture = populated_history(1024);
    let inner = fixture.state.inner.lock().unwrap();
    let (summary, visits) = measure("one_compaction_revalidation", || {
        test_run_compaction_summary(&inner, "test-history-0512")
    });
    assert!(summary.is_some());
    assert_eq!(
        visits, 1,
        "only the selected artifact's candidate is proof-checked"
    );
}

#[test]
fn idle_history_pending_compaction_is_polled_at_an_unchanged_revision_and_fenced_by_sqlite() {
    let mut fixture = populated_history(256);
    let id = "test-history-0128";
    let execution = "observed-history-0128";
    let (tx, rx) = mpsc::channel();
    fixture.state.persist_tx = tx;
    fixture.state.state_broadcast_mailbox = Some(Arc::new(StateBroadcastMailbox::default()));
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let row = inner.test_run_observations.get_mut(execution).unwrap();
        row.runtime_identity = Some("fixture-original-runtime".to_owned());
        row.validated_unbound = true;
        row.summary.state = TestRunState::Passed;
        row.summary.ended_at = Some("2026-10-25T00:01:00Z".to_owned());
        row.summary.exit_code = Some(0);
        row.final_unbound_proof = Some(TestRunFinalUnboundProof {
            version: 1,
            terminal_identity: test_run_final_unbound_identity(row).unwrap(),
        });
        inner
            .test_runs
            .observations_durable
            .insert(execution.to_owned());
        let summary = test_run_compaction_summary(&inner, id).unwrap();
        assert!(test_run_compaction_eligible(&inner, &summary));
        let entry = inner.test_runs.find(id).unwrap();
        let compacted = TestRunCompacted {
            compacted: true,
            request_identity: entry.disk.request_identity.clone(),
            request_stamp: entry.disk.request_stamp,
            results_stamp: entry.disk.results_stamp,
            registration: None,
            observations: vec![test_run_minimal_observation(
                inner.test_run_observations[execution].clone(),
            )],
            artifact_unavailable: false,
            artifact_conflict: false,
            summary: test_run_minimal_summary(summary),
            reason: "fixture exact survivor".to_owned(),
        };
        inner
            .test_run_compacted
            .insert(id.to_owned(), compacted.clone());
        assert!(
            !fixture
                .state
                .persist_test_run_compacted_locked(&mut inner, compacted)
                .unwrap()
        );
        inner.test_runs.compaction_examined_revision = Some(inner.revision);
    }
    let revision = fixture.state.inner.lock().unwrap().revision;
    fixture.state.step_test_run_compaction();
    assert!(
        fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_run_observations
            .contains_key(execution),
        "an enqueued fence is not durable proof"
    );
    assert_eq!(fixture.state.inner.lock().unwrap().revision, revision);
    // Drive the actual worker batch and real disposable SQLite transaction,
    // not a fabricated success or a revision-only acknowledgement.
    let mut batch = PersistFenceBatch::default();
    assert!(!batch.drain(&rx, false));
    assert_eq!(batch.pending.len(), 1);
    let delta = collect_persist_delta_from_shared_state(&fixture.state.inner, 0);
    assert!(batch.pending[0].target.is_in_delta(&delta));
    let mut cache = SqlitePersistConnectionCache::new();
    persist_delta_with_fences(
        &mut cache,
        fixture.state.persistence_path.as_ref(),
        &delta,
        &mut batch,
    )
    .unwrap();
    let (_, visits) = measure("durable_compaction_completion", || {
        fixture.state.step_test_run_compaction()
    });
    let inner = fixture.state.inner.lock().unwrap();
    assert!(inner.test_runs.compactions_durable.contains(id));
    assert!(!inner.test_run_observations.contains_key(execution));
    assert!(inner.test_runs.find(id).is_none());
    assert_eq!(
        inner.test_run_compacted[id].observations[0].execution_id,
        execution
    );
    // One selected candidate before removal; then the required exact-revision
    // snapshot publication and fresh compaction selection each see 255 rows.
    // The bound is linear in retained history and does not constrain wall time.
    assert!(
        visits <= 1 + 2 * 255,
        "one completion, one snapshot and one fresh selection, not Cartesian history: {visits}"
    );
}
