// Test-only child of src/tests/test_run_compaction.rs.
// Owns the compaction work a test-run rescan does on unchanged history, the
// eligibility changes made outside a rescan that must still be compacted by
// the next pass, and a pass's own index changes, which select again. Does not
// own compaction proofs, identity or persistence
// (the parent module), nor the idle compaction step's own gating
// (src/tests/idle_state_lock.rs).
use super::*;

/// Ends one safe native command observation in `session` under `key`.
fn finish_safe(fixture: &RunFixture, session: &str, key: &str) {
    fixture.state.note_engram_command_started(
        session,
        &EngramObservationProvenance::Ambient,
        key,
        Some("cargo test"),
        Some(&fixture.root.to_string_lossy()),
    );
    fixture.state.observe_test_command_end(
        session,
        &EngramObservationProvenance::Ambient,
        key,
        Some(EngramCommandExit::Code(0)),
    );
}

fn compacted_ids(fixture: &RunFixture) -> Vec<String> {
    let inner = fixture.state.inner.lock().unwrap();
    let mut ids: Vec<_> = inner
        .test_run_compacted
        .iter()
        .filter(|(_, row)| row.compacted)
        .map(|(id, _)| id.clone())
        .collect();
    ids.sort();
    ids
}

/// The compaction population the newest rescan built under its commit lock.
fn last_rescan_compaction_population(fixture: &RunFixture) -> usize {
    let (traces, _) = fixture.state.test_run_phase_traces.recent_rescans(1);
    traces[0]
        .rescan
        .as_ref()
        .expect("a rescan trace records its work")
        .compaction_population
}

#[test]
fn an_unchanged_rescan_builds_no_compaction_population() {
    let fixture = RunFixture::new("rescan-compaction-unchanged");
    let session = running_owner(&fixture, "rescan-compaction-unchanged-runtime");
    fill_safe(&fixture, &session, TEST_RUN_TERMINAL_RUNS_PER_PROJECT + 1);
    fixture.refresh(&[]);
    assert_eq!(
        compacted_ids(&fixture).len(),
        1,
        "the history over the per-project limit is compacted first"
    );
    // Nothing changes between these passes: no artifact, observation, wait,
    // registration or card.
    fixture.refresh(&[]);
    let before = compacted_ids(&fixture);
    fixture.refresh(&[]);
    assert_eq!(compacted_ids(&fixture), before);
    assert_eq!(
        last_rescan_compaction_population(&fixture),
        0,
        "an unchanged rescan must not rebuild every terminal summary to select nothing"
    );
}

#[test]
fn a_live_card_removed_with_its_session_is_compacted_by_the_same_rescan() {
    let fixture = RunFixture::new("rescan-compaction-gone-card");
    let session = running_owner(&fixture, "rescan-compaction-gone-card-runtime");
    fill_safe(&fixture, &session, TEST_RUN_TERMINAL_RUNS_PER_PROJECT);
    let carded = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let carded = inner
            .test_run_observations
            .values()
            .find(|row| row.summary.owner_session_id.as_deref() == Some(session.as_str()))
            .unwrap()
            .execution_id
            .clone();
        // A card that is not terminal, in a session that no longer exists.
        inner.test_run_cards.insert(
            carded.clone(),
            TestRunCardRef {
                session_id: "session-deleted".to_owned(),
                message_id: "message-deleted".to_owned(),
                terminal: false,
                failure_detail_version: None,
            },
        );
        carded
    };
    finish_safe(&fixture, &session, "rescan-compaction-over-limit");
    assert!(
        compacted_ids(&fixture).is_empty(),
        "a run with a live card is not eligible, so the history stays within the limit"
    );
    // The index is unchanged; only the card's session is gone.
    fixture.refresh(&[]);
    assert!(
        !fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_run_cards
            .contains_key(&carded),
        "the rescan drops the card whose session is gone"
    );
    assert_eq!(
        compacted_ids(&fixture).len(),
        1,
        "the dropped card makes the history exceed the limit by one in that pass"
    );
}

#[test]
fn a_stale_observation_fence_removed_by_the_step_is_compacted_by_the_next_rescan() {
    let fixture = RunFixture::new("rescan-compaction-stale-fence");
    let session = running_owner(&fixture, "rescan-compaction-stale-fence-runtime");
    fill_safe(&fixture, &session, TEST_RUN_TERMINAL_RUNS_PER_PROJECT);
    {
        let inner = fixture.state.inner.lock().unwrap();
        let current = inner
            .test_run_observations
            .values()
            .find(|row| row.summary.owner_session_id.as_deref() == Some(session.as_str()))
            .unwrap()
            .clone();
        // An older queued image of the row: the row has since been saved
        // synchronously, so this fence no longer matches it.
        let mut stale = current.clone();
        stale.summary.exit_code = Some(99);
        let (fence, waiter) = PersistFence::new(
            PersistFenceTarget::TestRunObservation(Box::new(stale.clone())),
            std::time::Instant::now() + Duration::from_secs(5),
        );
        // A dropped fence resolves, as when its writer is gone.
        drop(fence);
        inner
            .test_runs
            .observation_persistence
            .0
            .lock()
            .unwrap()
            .insert(current.execution_id.clone(), (stale, waiter));
    }
    finish_safe(&fixture, &session, "rescan-compaction-over-limit");
    fixture.refresh(&[]);
    assert!(
        compacted_ids(&fixture).is_empty(),
        "a run with a pending fence is not eligible"
    );
    fixture.state.step_test_command_observation_persistence();
    assert!(
        fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_runs
            .observation_persistence
            .0
            .lock()
            .unwrap()
            .is_empty(),
        "the step removes the resolved stale fence"
    );
    // Nothing in the index changes on this pass.
    fixture.refresh(&[]);
    assert_eq!(
        compacted_ids(&fixture).len(),
        1,
        "the removed fence makes the history exceed the limit by one"
    );
}

#[test]
fn a_wait_released_by_a_stopped_session_is_compacted_by_the_next_rescan() {
    let fixture = RunFixture::new("rescan-compaction-stopped-wait");
    let session = running_owner(&fixture, "rescan-compaction-stopped-wait-runtime");
    fill_safe(&fixture, &session, TEST_RUN_TERMINAL_RUNS_PER_PROJECT);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let waited = inner
            .test_run_observations
            .values()
            .find(|row| row.summary.owner_session_id.as_deref() == Some(session.as_str()))
            .unwrap()
            .execution_id
            .clone();
        let mut wait = test_run_wait_record("rescan-compaction-wait", "session-waiting");
        wait.run_ids = vec![waited];
        inner.test_run_waits = vec![wait].into();
    }
    finish_safe(&fixture, &session, "rescan-compaction-over-limit");
    fixture.refresh(&[]);
    fixture.refresh(&[]);
    assert!(
        compacted_ids(&fixture).is_empty(),
        "a waited run is not eligible, so the history stays within the limit"
    );
    let stopped = {
        let mut inner = fixture.state.inner.lock().unwrap();
        consume_test_run_waits_for_stopped_session_locked(&mut inner, "session-waiting")
    };
    assert_eq!(stopped.len(), 1);
    fixture.refresh(&[]);
    assert_eq!(
        compacted_ids(&fixture).len(),
        1,
        "the released wait makes the history exceed the limit by one"
    );
}

#[test]
fn a_wait_settled_by_the_wait_refresh_is_compacted_by_the_next_rescan() {
    let fixture = RunFixture::new("rescan-compaction-settled-wait");
    let session = running_owner(&fixture, "rescan-compaction-settled-wait-runtime");
    fill_safe(&fixture, &session, TEST_RUN_TERMINAL_RUNS_PER_PROJECT);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let waited = inner
            .test_run_observations
            .values()
            .find(|row| row.summary.owner_session_id.as_deref() == Some(session.as_str()))
            .unwrap()
            .execution_id
            .clone();
        // The waiting session does not exist, so the refresh consumes it.
        let mut wait = test_run_wait_record("rescan-compaction-wait", "session-removed");
        wait.run_ids = vec![waited];
        inner.test_run_waits = vec![wait].into();
    }
    finish_safe(&fixture, &session, "rescan-compaction-over-limit");
    fixture.refresh(&[]);
    fixture.refresh(&[]);
    assert!(compacted_ids(&fixture).is_empty());
    let _refresh = fixture.state.refresh_test_run_waits();
    assert!(
        fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_run_waits
            .is_empty(),
        "the wait refresh settles the removed session's wait"
    );
    fixture.refresh(&[]);
    assert_eq!(compacted_ids(&fixture).len(), 1);
}

#[test]
fn a_rescan_that_changes_the_index_selects_again() {
    let fixture = RunFixture::new("rescan-compaction-index-change");
    let session = running_owner(&fixture, "rescan-compaction-index-change-runtime");
    fill_safe(&fixture, &session, 3);
    let directory = write_run(
        &fixture.runs_dir(),
        "test-arriving",
        full_request(&fixture.root, &test_run_timestamp()),
        None,
    );
    fixture.refresh(&[]);
    fixture.refresh(&[]);
    assert_eq!(
        last_rescan_compaction_population(&fixture),
        0,
        "a run still waiting for its results changes nothing"
    );
    let mut results = passed_results();
    results["runId"] = json!("test-arriving");
    fs::write(directory.join("results.json"), results.to_string()).unwrap();
    fixture.refresh(&[]);
    assert!(
        fixture
            .summaries()
            .iter()
            .any(|run| run.run_id == "test-arriving"),
        "the arrived results index the run"
    );
    assert!(
        last_rescan_compaction_population(&fixture) > 3,
        "a pass that changed the index selects over the whole history"
    );
}

#[test]
fn a_cleared_registration_failure_is_compacted_by_the_next_rescan() {
    let fixture = RunFixture::new("rescan-compaction-registration");
    let session = running_owner(&fixture, "rescan-compaction-registration-runtime");
    let (run_id, _, directory) = registered_alias(&fixture, &session, "rescan-registered");
    // An earlier failed registration attempt of the same run.
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_runs
        .registration_failures
        .insert(run_id.clone());
    fill_safe(&fixture, &session, TEST_RUN_TERMINAL_RUNS_PER_PROJECT);
    fixture.refresh(&[]);
    fixture.refresh(&[]);
    assert!(
        compacted_ids(&fixture).is_empty(),
        "a run with a failed registration is not eligible"
    );
    fixture
        .state
        .register_test_run(&session, registration_request(&run_id, &directory))
        .unwrap();
    assert!(
        !fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_runs
            .registration_failures
            .contains(&run_id),
        "the successful registration clears the failure"
    );
    fixture.refresh(&[]);
    assert_eq!(
        compacted_ids(&fixture).len(),
        1,
        "the registered run makes the history exceed the limit by one"
    );
}
