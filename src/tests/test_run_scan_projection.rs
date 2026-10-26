//! Deterministic scan preparation, exact-cut and publication ownership witnesses.
use super::*;

fn indexed_run(label: &str) -> RunFixture {
    let fixture = RunFixture::new(label);
    write_run(
        &fixture.runs_dir(),
        "test-projection",
        full_request(&fixture.root, "2026-10-26T01:00:00Z"),
        Some(passed_results()),
    );
    fixture.refresh(&[]);
    fixture
}

#[test]
fn unchanged_scan_builds_projection_without_holding_state() {
    let fixture = indexed_run("projection-off-lock");
    let state = fixture.state.clone();
    let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let result = observed.clone();
    TEST_RUN_PROJECTION_STARTED.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            result.store(
                state.inner.is_not_held_by_current_thread_for_test(),
                std::sync::atomic::Ordering::SeqCst,
            );
        }));
    });
    fixture.refresh(&[]);
    assert!(
        observed.load(std::sync::atomic::Ordering::SeqCst),
        "unchanged-run projection must be prepared outside StateInner"
    );
}

#[test]
fn unchanged_scan_builds_summary_without_holding_state() {
    let fixture = indexed_run("summary-off-lock");
    let state = fixture.state.clone();
    let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let result = observed.clone();
    TEST_RUN_SUMMARY_STARTED.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            result.store(
                state.inner.is_not_held_by_current_thread_for_test(),
                std::sync::atomic::Ordering::SeqCst,
            );
        }));
    });
    fixture.refresh(&[]);
    assert!(
        observed.load(std::sync::atomic::Ordering::SeqCst),
        "unchanged-run summary must be prepared outside StateInner"
    );
}

#[test]
fn scan_commit_rejects_prepared_session_authority_after_an_unstamped_change() {
    let fixture = RunFixture::new("projection-exact-cut");
    let owner = create_test_project_session(
        &fixture.state,
        Agent::Codex,
        &fixture.project_id,
        &fixture.root,
    );
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&owner).unwrap();
        inner.sessions[index].session.name = "Scan owner".to_owned();
    }
    let mut request = full_request(&fixture.root, "2026-10-26T01:00:00Z");
    request["owner"] = json!("Scan owner");
    write_run(
        &fixture.runs_dir(),
        "test-projection",
        request,
        Some(passed_results()),
    );
    fixture.refresh(&[]);
    assert_eq!(
        fixture.summary("test-projection").owner_session_id,
        Some(owner.clone())
    );
    let state = fixture.state.clone();
    TEST_RUN_PROJECTION_PREPARED.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            assert!(state.inner.is_not_held_by_current_thread_for_test());
            let mut inner = state.inner.lock().unwrap();
            let revision = inner.revision;
            let index = inner.find_session_index(&owner).unwrap();
            // Deliberately bypass stamps/revision: equality must be the fence.
            inner.sessions[index].session.name = "Renamed during preparation".to_owned();
            assert_eq!(inner.revision, revision);
        }));
    });
    let mut receiver = fixture.state.subscribe_delta_events();
    fixture.refresh(&[]);
    assert_eq!(fixture.summary("test-projection").owner_session_id, None);
    let changes = drain_test_run_deltas(&mut receiver);
    assert!(changes.iter().any(
        |event| matches!(event, DeltaEvent::TestRunChanged { run, .. }
        if run.run_id == "test-projection" && run.owner_session_id.is_none())
    ));
}

#[test]
fn scan_keeps_revision_allocation_and_publication_locked_after_off_lock_preparation() {
    let fixture = indexed_run("projection-delta-order");
    let directory = fixture.runs_dir().join("test-projection");
    let mut changed = passed_results();
    changed["runId"] = json!("test-projection");
    changed["error"] = json!("Changed detail");
    fs::write(directory.join("results.json"), changed.to_string()).unwrap();
    let state = fixture.state.clone();
    let intervening = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let observed = intervening.clone();
    TEST_RUN_PROJECTION_PREPARED.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            assert!(state.inner.is_not_held_by_current_thread_for_test());
            let mut inner = state.inner.lock().unwrap();
            let revision = state.commit_delta_locked(&mut inner).unwrap();
            state.publish_delta_locked(
                &inner,
                DeltaEvent::TestRunRemoved {
                    revision,
                    run_id: "test-intervening".to_owned(),
                },
            );
            observed.store(revision, std::sync::atomic::Ordering::SeqCst);
        }));
    });
    let mut receiver = fixture.state.subscribe_delta_events();
    fixture
        .state
        .refresh_test_runs_with(&|_, _| false, &|inner, event| {
            assert!(!fixture.state.inner.is_not_held_by_current_thread_for_test());
            if let DeltaEvent::TestRunChanged { revision, .. } = event {
                assert_eq!(*revision, inner.revision);
                assert!(*revision > intervening.load(std::sync::atomic::Ordering::SeqCst));
            }
            fixture.state.publish_delta_locked(inner, event.clone());
        });
    let events = drain_test_run_deltas(&mut receiver);
    assert!(
        matches!(events.first(), Some(DeltaEvent::TestRunRemoved { run_id, .. }) if run_id == "test-intervening")
    );
    assert!(events.iter().skip(1).any(|event| matches!(event, DeltaEvent::TestRunChanged { run, .. } if run.run_id == "test-projection")));
}

fn populated_unrelated_history(label: &str) -> (RunFixture, String) {
    let fixture = indexed_run(label);
    let owner = create_test_project_session(
        &fixture.state,
        Agent::Codex,
        &fixture.project_id,
        &fixture.root,
    );
    let (runtime, _commands) = test_codex_runtime_handle("projection-scope-runtime");
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&owner).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }
    fixture.state.note_engram_command_started(
        &owner,
        &EngramObservationProvenance::Ambient,
        "scope-provider",
        Some("cargo test"),
        Some(&fixture.root.to_string_lossy()),
    );
    fixture.state.observe_test_command_end(
        &owner,
        &EngramObservationProvenance::Ambient,
        "scope-provider",
        Some(EngramCommandExit::Code(0)),
    );
    let seed;
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let template = inner.test_run_observations.values().next().unwrap().clone();
        seed = template.execution_id.clone();
        assert!(test_run_final_unbound_valid(&template));
        let retained = inner
            .test_run_compacted
            .get("test-projection")
            .unwrap()
            .clone();
        for ordinal in 0..128 {
            let mut row = template.clone();
            row.execution_id = format!("observed-unrelated-{ordinal:03}");
            row.summary.run_id = row.execution_id.clone();
            row.summary.error = Some("unrelated retained payload ".repeat(80));
            row.artifact = None;
            inner
                .test_run_observations
                .insert(row.execution_id.clone(), row);
            let mut row = retained.clone();
            row.summary.run_id = format!("test-unrelated-{ordinal:03}");
            row.summary.run_dir = fixture
                .runs_dir()
                .join(&row.summary.run_id)
                .to_string_lossy()
                .into_owned();
            inner
                .test_run_compacted
                .insert(row.summary.run_id.clone(), row);
            inner
                .test_runs
                .registered
                .insert(format!("unrelated-marker-{ordinal:03}"));
        }
    }
    let mut request = full_request(&fixture.root, "2026-10-26T01:00:00Z");
    request["owner"] = json!(owner);
    write_run(
        &fixture.runs_dir(),
        "test-projection",
        request,
        Some(passed_results()),
    );
    fixture.refresh(&[]);
    (fixture, seed)
}

fn associate_seed(inner: &mut StateInner, seed: &str) {
    let disk = inner
        .test_runs
        .find("test-projection")
        .unwrap()
        .disk
        .clone();
    inner.test_run_observations.get_mut(seed).unwrap().artifact = Some(TestRunObservedArtifact {
        run_id: disk.run_id.clone(),
        run_directory: test_run_display_path(&disk.run_dir),
        request_identity: disk.request_identity.clone(),
        // Match the production artifact observer: detailVersion is display
        // text; the retained terminal witness stores the raw digest.
        terminal_digest: Some(
            disk.results
                .as_ref()
                .unwrap()
                .digest
                .strip_prefix("sha256:")
                .expect("versioned result digest")
                .to_owned(),
        ),
    });
    let entry = inner.test_runs.find("test-projection").unwrap();
    let row = inner.test_run_observations.get(seed).unwrap();
    assert!(test_run_final_unbound_valid(row));
    assert!(
        test_run_observation_matches_run(inner, &entry.summary, Some(&disk), row),
        "fixture must establish an exact positive association before testing cut invalidation"
    );
}

#[test]
fn scan_cut_copies_candidates_instead_of_unrelated_retained_history() {
    let (fixture, _seed) = populated_unrelated_history("projection-copy-scope");
    let inner = fixture.state.inner.lock().unwrap();
    let scanned: Vec<_> = inner
        .test_runs
        .entries
        .iter()
        .map(|entry| TestRunScanned {
            disk: entry.disk.clone(),
            state: entry.summary.state,
            unknown_reason: entry.summary.unknown_reason,
            project_id: entry.discovered_project_id.clone(),
        })
        .collect();
    assert_eq!(scanned.len(), 1);
    assert!(inner.test_run_observations.len() >= 129);
    assert!(inner.test_run_compacted.len() >= 129);
    let cut = TestRunProjectionCut::capture(&inner, TestRunProjectionScope::new(&scanned));
    assert!(
        cut.observations.is_empty(),
        "artifact=None history must not be cloned"
    );
    assert_eq!(
        cut.compacted.len(),
        1,
        "only this scan's retained row is copied"
    );
    assert!(
        cut.index.registered.is_empty(),
        "unrelated durability markers are not copied"
    );
    assert_eq!(cut.sessions.len(), 1);
    let expected: Vec<_> = test_run_project_entries(&inner, &scanned)
        .into_iter()
        .map(|entry| entry.summary)
        .collect();
    drop(inner);
    let prepared: Vec<_> = test_run_project_entries(&cut, &scanned)
        .into_iter()
        .map(|entry| entry.summary)
        .collect();
    assert_eq!(prepared, expected);
}

#[test]
fn unrelated_history_mutation_does_not_force_locked_scan_recomputation() {
    let (fixture, seed) = populated_unrelated_history("projection-unrelated-mutation");
    let state = fixture.state.clone();
    TEST_RUN_PROJECTION_FALLBACKS.with(|count| count.set(0));
    TEST_RUN_PROJECTION_PREPARED.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            let mut inner = state.inner.lock().unwrap();
            inner
                .test_run_observations
                .get_mut(&seed)
                .unwrap()
                .summary
                .error = Some("unrelated mutation".to_owned());
            inner
                .test_run_compacted
                .get_mut("test-unrelated-000")
                .unwrap()
                .reason = "unrelated retained update".to_owned();
            inner.test_runs.observations_failed.insert(seed);
        }));
    });
    fixture.refresh(&[]);
    TEST_RUN_PROJECTION_FALLBACKS.with(|count| assert_eq!(count.get(), 0));
}

#[test]
fn newly_relevant_observation_invalidates_an_absent_candidate_at_commit() {
    let (fixture, seed) = populated_unrelated_history("projection-new-candidate");
    assert!(!fixture
        .summary("test-projection")
        .registration
        .superseded_run_ids
        .contains(&seed));
    let state = fixture.state.clone();
    let expected = seed.clone();
    TEST_RUN_PROJECTION_FALLBACKS.with(|count| count.set(0));
    TEST_RUN_PROJECTION_PREPARED.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            associate_seed(&mut state.inner.lock().unwrap(), &seed)
        }));
    });
    fixture.refresh(&[]);
    TEST_RUN_PROJECTION_FALLBACKS.with(|count| assert_eq!(count.get(), 1));
    assert!(fixture
        .summary("test-projection")
        .registration
        .superseded_run_ids
        .contains(&expected));
}

#[test]
fn scan_commit_fences_unstamped_candidate_durability() {
    let (fixture, seed) = populated_unrelated_history("projection-durability-cut");
    associate_seed(&mut fixture.state.inner.lock().unwrap(), &seed);
    fixture.refresh(&[]);
    assert_eq!(
        fixture
            .summary("test-projection")
            .registration
            .restart_would_void_evidence,
        Some(false)
    );
    let state = fixture.state.clone();
    TEST_RUN_PROJECTION_FALLBACKS.with(|count| count.set(0));
    TEST_RUN_PROJECTION_PREPARED.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            let mut inner = state.inner.lock().unwrap();
            let revision = inner.revision;
            assert!(inner.test_runs.observations_durable.remove(&seed));
            assert_eq!(inner.revision, revision);
        }));
    });
    let mut receiver = fixture.state.subscribe_delta_events();
    fixture.refresh(&[]);
    TEST_RUN_PROJECTION_FALLBACKS.with(|count| assert_eq!(count.get(), 1));
    assert_eq!(
        fixture
            .summary("test-projection")
            .registration
            .restart_would_void_evidence,
        None
    );
    assert!(drain_test_run_deltas(&mut receiver).iter().any(|event| matches!(event,
        DeltaEvent::TestRunChanged { run, .. } if run.run_id == "test-projection" && run.registration.restart_would_void_evidence.is_none())));
}
