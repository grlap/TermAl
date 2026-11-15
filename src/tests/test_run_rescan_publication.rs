// Test-only child of src/tests/test_runs.rs.
// Owns what an unchanged test-run rescan publishes: no new state revision and
// no new test-run history projection, including when one indexed run's
// results cannot be parsed. Does not own the deltas a changed scan publishes
// (src/tests/test_runs.rs) or compaction selection
// (src/tests/test_run_compaction.rs).
use super::*;

fn revision_and_projection_version(fixture: &RunFixture) -> (u64, u64) {
    let inner = fixture.state.inner.lock().unwrap();
    (
        inner.revision,
        inner
            .test_runs
            .transport_version
            .load(std::sync::atomic::Ordering::Relaxed),
    )
}

/// Three passed runs, indexed by two rescans.
fn indexed_history(label: &str) -> RunFixture {
    let fixture = RunFixture::new(label);
    for ordinal in 0..3 {
        write_run(
            &fixture.runs_dir(),
            &format!("test-published-{ordinal}"),
            full_request(&fixture.root, "2026-10-25T00:00:00Z"),
            Some(passed_results()),
        );
    }
    fixture
}

/// Runs one more rescan over unchanged disk and state, and asserts that it
/// neither commits a revision nor publishes a new test-run projection.
fn assert_unchanged_rescan_publishes_nothing(fixture: &RunFixture) {
    let before = revision_and_projection_version(fixture);
    fixture.refresh(&[]);
    let after = revision_and_projection_version(fixture);
    assert_eq!(
        after.0, before.0,
        "an unchanged rescan must not commit a new state revision"
    );
    assert_eq!(
        after.1, before.1,
        "an unchanged rescan must not publish a new test-run projection"
    );
}

#[test]
fn an_unchanged_rescan_neither_advances_the_revision_nor_republishes() {
    let fixture = indexed_history("rescan-publication-unchanged");
    fixture.refresh(&[]);
    fixture.refresh(&[]);
    assert_eq!(fixture.summaries().len(), 3, "the runs are indexed");
    assert_unchanged_rescan_publishes_nothing(&fixture);
}

#[test]
fn an_unchanged_unreadable_result_neither_advances_the_revision_nor_republishes() {
    let fixture = indexed_history("rescan-publication-unreadable");
    // A results file that exists but never parses, as a crashed writer can
    // leave it: the same bytes on every pass.
    let unreadable = write_run(
        &fixture.runs_dir(),
        "test-unreadable-results",
        full_request(&fixture.root, "2026-10-25T00:00:00Z"),
        None,
    );
    fs::write(unreadable.join("results.json"), vec![0u8; 220]).unwrap();
    fixture.refresh(&[]);
    fixture.refresh(&[]);
    assert!(
        fixture
            .summaries()
            .iter()
            .any(|run| run.run_id == "test-unreadable-results"),
        "the run with unreadable results is indexed"
    );
    assert_unchanged_rescan_publishes_nothing(&fixture);
}

/// The parsed disk entry the index holds for `run_id`.
fn indexed_disk(fixture: &RunFixture, run_id: &str) -> Arc<TestRunDisk> {
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_runs
        .find(run_id)
        .unwrap_or_else(|| panic!("{run_id} should be indexed"))
        .disk
        .clone()
}

#[test]
fn unreadable_results_stay_surfaced_and_a_changed_file_is_read_again() {
    let fixture = indexed_history("rescan-publication-unreadable-changed");
    let unreadable = write_run(
        &fixture.runs_dir(),
        "test-unreadable-results",
        full_request(&fixture.root, "2026-10-25T00:00:00Z"),
        None,
    );
    fs::write(unreadable.join("results.json"), vec![0u8; 220]).unwrap();
    fixture.refresh(&[]);
    fixture.refresh(&[]);
    let run = fixture.summary("test-unreadable-results");
    assert_eq!(run.state, TestRunState::Unknown);
    assert_eq!(
        run.unknown_reason,
        Some(TestRunUnknownReason::ResultsUnreadable),
        "the unreadable result is reported, not hidden"
    );
    let kept = indexed_disk(&fixture, "test-unreadable-results");
    fixture.refresh(&[]);
    assert!(
        Arc::ptr_eq(&kept, &indexed_disk(&fixture, "test-unreadable-results")),
        "unchanged unparseable results keep their entry"
    );
    // The launcher replaces the file: the stamp changes and it is read again.
    let mut results = passed_results();
    results["runId"] = json!("test-unreadable-results");
    fs::write(unreadable.join("results.json"), results.to_string()).unwrap();
    fixture.refresh(&[]);
    assert_eq!(
        fixture.summary("test-unreadable-results").state,
        TestRunState::Passed
    );
}

#[test]
fn a_good_result_that_turns_unparseable_keeps_its_one_rescan_grace() {
    let fixture = indexed_history("rescan-publication-grace");
    fixture.refresh(&[]);
    fixture.refresh(&[]);
    assert_eq!(
        fixture.summary("test-published-0").state,
        TestRunState::Passed
    );
    let directory = fixture.runs_dir().join("test-published-0");
    fs::write(directory.join("results.json"), vec![0u8; 220]).unwrap();
    // The last good results stand for one rescan.
    fixture.refresh(&[]);
    assert_eq!(
        fixture.summary("test-published-0").state,
        TestRunState::Passed
    );
    // The grace copy is read again on the next pass, and then expires.
    fixture.refresh(&[]);
    let run = fixture.summary("test-published-0");
    assert_eq!(run.state, TestRunState::Unknown);
    assert_eq!(
        run.unknown_reason,
        Some(TestRunUnknownReason::ResultsUnreadable)
    );
    assert_unchanged_rescan_publishes_nothing(&fixture);
}

#[test]
fn a_results_read_that_fails_outright_is_read_again_on_every_pass() {
    let fixture = indexed_history("rescan-publication-read-error");
    let failing = write_run(
        &fixture.runs_dir(),
        "test-results-read-error",
        full_request(&fixture.root, "2026-10-25T00:00:00Z"),
        None,
    );
    // A directory where results.json belongs cannot be read as a file on any
    // platform: an I/O failure, unlike content that does not parse, may not
    // repeat, so it is never kept.
    fs::create_dir(failing.join("results.json")).unwrap();
    fixture.refresh(&[]);
    fixture.refresh(&[]);
    assert_eq!(
        fixture.summary("test-results-read-error").unknown_reason,
        Some(TestRunUnknownReason::ResultsUnreadable)
    );
    let earlier = indexed_disk(&fixture, "test-results-read-error");
    fixture.refresh(&[]);
    assert!(
        !Arc::ptr_eq(&earlier, &indexed_disk(&fixture, "test-results-read-error")),
        "a failed read is read again on the next pass"
    );
}
