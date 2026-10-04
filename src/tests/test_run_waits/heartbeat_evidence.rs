//! Explicit observation-time, cache and post-liveness reread witnesses.
//! Exercises the shared index followed by actual registration/settlement;
//! does not start a producer or change the queue's admission ownership.

use super::*;

fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-10-03T12:00:00Z").unwrap().with_timezone(&chrono::Utc)
}

fn beat(at: chrono::DateTime<chrono::Utc>, pid: Option<u32>) -> Value {
    let mut value = json!({ "state": "running", "heartbeat": {
        "at": at.to_rfc3339(), "everyMs": 10_000
    }, "stages": [{ "name": "rust-tests", "state": "running" }] });
    if let Some(pid) = pid {
        value["pid"] = json!(pid);
    }
    value
}

fn write_results(directory: &FsPath, mut results: Value) {
    results["runId"] = json!("test-evidence");
    fs::write(directory.join("results.json"), results.to_string()).unwrap();
}

fn scan(fixture: &WaitFixture, at: chrono::DateTime<chrono::Utc>, alive: &TestRunLiveness) {
    fixture.state.refresh_test_runs_at_with(at, alive, &|inner, event| {
        fixture.state.publish_delta_locked(inner, event.clone())
    });
    fixture.state.refresh_test_run_waits();
}

fn summary(fixture: &WaitFixture) -> TestRunSummary {
    fixture.state.test_run_summaries(None).into_iter()
        .find(|entry| entry.run_id == "test-evidence").unwrap()
}

fn expect_pending(fixture: &WaitFixture) {
    assert_eq!(fixture.pending_waits().len(), 1);
    assert!(fixture.queued_prompts().is_empty(), "uncertainty must not settle with recovery advice");
}

#[test]
fn heartbeat_parser_and_exact_freshness_boundaries_are_bounded() {
    let at = now();
    let valid = TestRunHeartbeat::parse(&json!({ "at": at.to_rfc3339(), "everyMs": 10_000 })).unwrap();
    for (offset, fresh) in [(-10_001, None), (-10_000, Some(true)), (0, Some(true)),
        (30_000, Some(true)), (30_001, Some(false))] {
        assert_eq!(valid.fresh_at(at + chrono::TimeDelta::milliseconds(offset)), fresh, "{offset}");
    }
    for interval in [json!(999), json!(600_001), json!(-1), json!(10_000.5), json!("10000"), json!(u64::MAX)] {
        assert!(TestRunHeartbeat::parse(&json!({ "at": at.to_rfc3339(), "everyMs": interval })).is_none());
    }
    for timestamp in [json!(null), json!(7), json!("not RFC3339"), json!("x".repeat(TEST_RUN_TEXT_MAX_BYTES + 1))] {
        assert!(TestRunHeartbeat::parse(&json!({ "at": timestamp, "everyMs": 10_000 })).is_none());
    }
    for interval in [1000, 600_000] {
        assert!(TestRunHeartbeat::parse(&json!({ "at": at.to_rfc3339(), "everyMs": interval })).is_some());
    }
    let extremes = TestRunHeartbeat { at: chrono::DateTime::<chrono::Utc>::MIN_UTC, every_ms: 600_000 };
    assert_eq!(extremes.fresh_at(chrono::DateTime::<chrono::Utc>::MAX_UTC), Some(false));
    let extremes = TestRunHeartbeat { at: chrono::DateTime::<chrono::Utc>::MAX_UTC, every_ms: 600_000 };
    assert_eq!(extremes.fresh_at(chrono::DateTime::<chrono::Utc>::MIN_UTC), None);
}

#[test]
fn cached_heartbeat_expiry_is_uncertainty_then_renewal_and_real_completion() {
    let fixture = WaitFixture::new("cached-heartbeat");
    let directory = fixture.write("test-evidence", Some(beat(now(), None)));
    scan(&fixture, now(), &|_, _| false);
    fixture.busy();
    assert!(!fixture.register(&["test-evidence"], "all").unwrap().resume_prompt_queued);
    let version = summary(&fixture).detail_version;
    expect_pending(&fixture);
    scan(&fixture, now() + chrono::TimeDelta::milliseconds(30_000), &|_, _| false);
    assert_eq!(summary(&fixture).state, TestRunState::Running);
    scan(&fixture, now() + chrono::TimeDelta::milliseconds(30_001), &|_, _| false);
    assert_eq!(summary(&fixture).state, TestRunState::Unknown);
    assert_eq!(summary(&fixture).unknown_reason, Some(TestRunUnknownReason::HeartbeatStale));
    assert_eq!(summary(&fixture).detail_version, version, "expiry must not require a file change");
    expect_pending(&fixture);
    let inner = fixture.state.inner.lock().unwrap();
    let entry = inner.test_runs.entries.iter().find(|entry| entry.summary.run_id == "test-evidence").unwrap();
    assert_eq!(test_run_card_snapshot(entry, None).unwrap().unknown_reason,
        Some(TestRunCardUnknownReason::HeartbeatStale), "card shares index uncertainty");
    drop(inner);
    let previous_size = fs::metadata(directory.join("results.json")).unwrap().len();
    let mut renewed = beat(now() + chrono::TimeDelta::seconds(31), None);
    // An ignored fixture field changes the length regardless of timestamp resolution.
    renewed["renewal"] = json!(true);
    write_results(&directory, renewed);
    assert_ne!(fs::metadata(directory.join("results.json")).unwrap().len(), previous_size,
        "renewal must invalidate the parse cache even when the file timestamp is unchanged");
    scan(&fixture, now() + chrono::TimeDelta::seconds(31), &|_, _| false);
    assert_eq!(summary(&fixture).state, TestRunState::Running);
    expect_pending(&fixture);
    write_results(&directory, passed());
    scan(&fixture, now() + chrono::TimeDelta::seconds(32), &|_, _| false);
    assert!(fixture.pending_waits().is_empty());
    assert_eq!(fixture.queued_prompts().len(), 1);
    assert!(fixture.queued_prompts()[0].contains("`test-evidence`: PASS"));
    scan(&fixture, now() + chrono::TimeDelta::seconds(33), &|_, _| false);
    assert_eq!(fixture.queued_prompts().len(), 1);
}

#[test]
fn renewed_heartbeat_on_reread_defers_death_and_expiry_rechecks_terminal_race() {
    let fixture = WaitFixture::new("renewed-reread");
    let directory = fixture.write("test-evidence", Some(beat(now() - chrono::TimeDelta::days(1), Some(42))));
    let probes = std::rc::Rc::new(std::cell::Cell::new(0));
    let observed_probes = probes.clone();
    let renewed_directory = directory.clone();
    scan(&fixture, now(), &move |pid, _| {
        assert_eq!(pid, 42);
        observed_probes.set(observed_probes.get() + 1);
        write_results(&renewed_directory, beat(now(), Some(42)));
        false
    });
    assert_eq!(probes.get(), 1, "reread heartbeat vetoes premature disappearance");
    assert_eq!(summary(&fixture).state, TestRunState::Running);
    fixture.busy();
    fixture.register(&["test-evidence"], "all").unwrap();
    expect_pending(&fixture);
    scan(&fixture, now() + chrono::TimeDelta::milliseconds(30_001), &move |pid, _| {
        assert_eq!(pid, 42);
        write_results(&directory, passed());
        false
    });
    assert_eq!(summary(&fixture).state, TestRunState::Passed,
        "expiry must reread, not reuse an old disappearance confirmation");
    assert!(fixture.queued_prompts()[0].contains("`test-evidence`: PASS"));
    assert!(!fixture.queued_prompts()[0].contains("recover"));
}

#[test]
fn responsible_pid_handoff_and_post_check_failure_keep_actual_verdict() {
    let fixture = WaitFixture::new("heartbeat-pid-handoff");
    let directory = fixture.write("test-evidence", Some(beat(now() - chrono::TimeDelta::days(1), Some(100))));
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let observed_pids = seen.clone();
    let handoff_directory = directory.clone();
    scan(&fixture, now(), &move |pid, _| {
        observed_pids.borrow_mut().push(pid);
        if pid == 100 {
            write_results(&handoff_directory, beat(now() - chrono::TimeDelta::days(1), Some(200)));
            false
        } else {
            assert_eq!(pid, 200);
            true
        }
    });
    assert_eq!(*seen.borrow(), vec![100, 200]);
    assert_eq!(summary(&fixture).state, TestRunState::Running);
    fixture.busy();
    fixture.register(&["test-evidence"], "all").unwrap();
    expect_pending(&fixture);
    scan(&fixture, now(), &move |pid, _| {
        assert_eq!(pid, 200);
        write_results(&directory, failed("worker terminal failure"));
        false
    });
    assert_eq!(summary(&fixture).state, TestRunState::Failed);
    assert!(fixture.queued_prompts()[0].contains("worker terminal failure"));
    assert!(!fixture.queued_prompts()[0].contains("recover"));
}

#[test]
fn unreadable_post_check_evidence_cannot_settle_and_next_terminal_read_wins() {
    let fixture = WaitFixture::new("heartbeat-reread-fails");
    let directory = fixture.write("test-evidence", Some(beat(now() - chrono::TimeDelta::days(1), Some(42))));
    let unreadable_directory = directory.clone();
    scan(&fixture, now(), &move |_, _| {
        fs::write(unreadable_directory.join("results.json"), "{ not json").unwrap();
        false
    });
    assert_eq!(summary(&fixture).unknown_reason, Some(TestRunUnknownReason::ResultsUnreadable));
    fixture.busy();
    fixture.register(&["test-evidence"], "all").unwrap();
    expect_pending(&fixture);
    scan(&fixture, now(), &|_, _| false);
    expect_pending(&fixture);
    write_results(&directory, passed());
    scan(&fixture, now(), &|_, _| false);
    assert_eq!(summary(&fixture).state, TestRunState::Passed);
    assert!(fixture.pending_waits().is_empty());
    assert_eq!(fixture.queued_prompts().len(), 1);
}

#[test]
fn future_tolerance_is_checked_in_the_real_index_and_wait_path() {
    for (offset, state, reason) in [(10_000, TestRunState::Running, None),
        (10_001, TestRunState::Unknown, Some(TestRunUnknownReason::NoPid))] {
        let fixture = WaitFixture::new("heartbeat-future-boundary");
        fixture.write("test-evidence", Some(beat(now() + chrono::TimeDelta::milliseconds(offset), None)));
        scan(&fixture, now(), &|_, _| false);
        assert_eq!(summary(&fixture).state, state);
        assert_eq!(summary(&fixture).unknown_reason, reason);
        fixture.busy();
        fixture.register(&["test-evidence"], "all").unwrap();
        expect_pending(&fixture);
    }
}
