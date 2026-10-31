// Test-only child of src/tests/test_runs.rs. Owns the witnesses that a run's
// detail carries the launcher's committed-input record exactly as
// results.json records it: each outcome, a refused run, a staged-lane run
// without the record, and malformed records reading as none. Does not own
// the rest of the run detail (the parent's tests) or the launcher's own
// classification (scripts/test-launcher-commit-input.test.mjs). New module.
use super::*;

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const MOVED: &str = "89abcdef0123456789abcdef0123456789abcdef";

fn detail_of(fixture: &RunFixture, run_id: &str, results: Value) -> TestRunDetail {
    let run_dir = write_run(
        &fixture.runs_dir(),
        run_id,
        full_request(&fixture.root, "2026-10-30T10:00:00.000Z"),
        Some(results),
    );
    test_run_detail(&run_dir).expect("readable results give a detail")
}

fn with_input(input: Value) -> Value {
    let mut results = passed_results();
    results["input"] = input;
    results
}

#[test]
fn each_committed_input_outcome_reads_as_recorded() {
    let fixture = RunFixture::new("committed-input-outcomes");
    let clean = detail_of(
        &fixture,
        "test-committed-clean",
        with_input(json!({
            "kind": "commit", "commit": COMMIT, "cleanBefore": true,
            "after": { "commit": COMMIT, "clean": true, "paths": [] },
            "outcome": "clean", "eligible": true,
            "laterField": { "ignored": true },
        })),
    );
    assert_eq!(
        clean.committed_input,
        Some(TestRunCommittedInput {
            kind: TestRunCommittedInputKind::Commit,
            commit: Some(COMMIT.to_owned()),
            clean_before: true,
            dirty_paths: Vec::new(),
            skipped_paths: Vec::new(),
            filters: Vec::new(),
            gitlinks: Vec::new(),
            after: Some(TestRunCommittedInputAfter {
                commit: COMMIT.to_owned(),
                clean: true,
                paths: Vec::new(),
                skipped_paths: Vec::new(),
                filters: Vec::new(),
                gitlinks: Vec::new(),
            }),
            outcome: Some(TestRunCommittedInputOutcome::Clean),
            eligible: Some(true),
        })
    );

    let debris = detail_of(
        &fixture,
        "test-committed-debris",
        with_input(json!({
            "kind": "commit", "commit": COMMIT, "cleanBefore": true,
            "after": { "commit": COMMIT, "clean": false,
                       "paths": [{ "status": "??", "path": "target/new.txt" }] },
            "outcome": "debris_pending_disposition", "eligible": false, "disposition": null,
        })),
    )
    .committed_input
    .expect("a debris record reads");
    assert_eq!(debris.outcome, Some(TestRunCommittedInputOutcome::DebrisPendingDisposition));
    assert_eq!(debris.eligible, Some(false));
    let after = debris.after.expect("the after state is kept");
    assert!(!after.clean);
    assert_eq!(after.paths[0].path, "target/new.txt");
    assert_eq!(after.paths[0].status, "??");

    let changed = detail_of(
        &fixture,
        "test-committed-changed",
        with_input(json!({
            "kind": "commit", "commit": COMMIT, "cleanBefore": true,
            "after": { "commit": MOVED, "clean": false,
                       "paths": [{ "status": "R ", "path": "b.rs", "origin": "a.rs" }] },
            "outcome": "input_changed", "eligible": false,
        })),
    )
    .committed_input
    .expect("an input-changed record reads");
    assert_eq!(changed.outcome, Some(TestRunCommittedInputOutcome::InputChanged));
    let after = changed.after.expect("the after state is kept");
    assert_eq!(after.commit, MOVED);
    assert_eq!(after.paths[0].origin.as_deref(), Some("a.rs"));

    let refused = detail_of(
        &fixture,
        "test-committed-refused",
        with_input(json!({
            "kind": "commit", "cleanBefore": false,
            "dirtyPaths": [{ "status": " M", "path": "src/main.rs" }],
        })),
    )
    .committed_input
    .expect("a refused record reads");
    assert!(!refused.clean_before);
    assert_eq!(refused.commit, None);
    assert_eq!(refused.dirty_paths.len(), 1);
    assert_eq!(refused.outcome, None);
    assert_eq!(refused.eligible, None);

    // A Git filter configured before the run refuses it: the launcher
    // records the filters beside the dirty paths.
    let filtered_before = detail_of(
        &fixture,
        "test-committed-filtered-before",
        with_input(json!({
            "kind": "commit", "cleanBefore": false, "dirtyPaths": [],
            "filters": ["filter.fixture.clean"],
        })),
    )
    .committed_input
    .expect("a record refused for a configured filter reads");
    assert!(!filtered_before.clean_before);
    assert_eq!(filtered_before.filters, vec!["filter.fixture.clean".to_owned()]);
    assert_eq!(
        serde_json::to_value(&filtered_before).unwrap()["filters"],
        json!(["filter.fixture.clean"]),
        "kept in the wire form"
    );

    // A submodule gitlink refuses the run unread: the launcher records the
    // gitlinks beside the dirty paths, and one added during the run changes
    // the input.
    let gitlinked_before = detail_of(
        &fixture,
        "test-committed-gitlinked-before",
        with_input(json!({
            "kind": "commit", "cleanBefore": false, "dirtyPaths": [],
            "gitlinks": ["sub"],
        })),
    )
    .committed_input
    .expect("a record refused for a gitlink reads");
    assert!(!gitlinked_before.clean_before);
    assert_eq!(gitlinked_before.gitlinks, vec!["sub".to_owned()]);
    assert_eq!(
        serde_json::to_value(&gitlinked_before).unwrap()["gitlinks"],
        json!(["sub"]),
        "kept in the wire form"
    );
    let gitlinked_after = detail_of(
        &fixture,
        "test-committed-gitlinked-after",
        with_input(json!({
            "kind": "commit", "commit": COMMIT, "cleanBefore": true,
            "after": { "commit": COMMIT, "clean": false, "paths": [],
                       "gitlinks": ["sub"] },
            "outcome": "input_changed", "eligible": false,
        })),
    )
    .committed_input
    .expect("an input-changed record for a gitlink reads");
    assert_eq!(
        gitlinked_after.after.expect("the after state is kept").gitlinks,
        vec!["sub".to_owned()]
    );

    // A tracked path the index tells Git to skip refuses the run, and one
    // that appears during the run changes the input.
    let skipped_before = detail_of(
        &fixture,
        "test-committed-skipped-before",
        with_input(json!({
            "kind": "commit", "cleanBefore": false, "dirtyPaths": [],
            "skippedPaths": [{ "tag": "S", "path": "src/main.rs" }],
        })),
    )
    .committed_input
    .expect("a record refused for a skipped path reads");
    assert!(!skipped_before.clean_before);
    assert_eq!(
        skipped_before.skipped_paths,
        vec![TestRunCommittedInputSkippedPath {
            tag: "S".to_owned(),
            path: "src/main.rs".to_owned(),
        }]
    );
    let skipped_after = detail_of(
        &fixture,
        "test-committed-skipped-after",
        with_input(json!({
            "kind": "commit", "commit": COMMIT, "cleanBefore": true,
            "after": { "commit": COMMIT, "clean": false, "paths": [],
                       "skippedPaths": [{ "tag": "h", "path": "src/main.rs" }] },
            "outcome": "input_changed", "eligible": false,
        })),
    )
    .committed_input
    .expect("an input-changed record for a skipped path reads");
    assert_eq!(skipped_after.outcome, Some(TestRunCommittedInputOutcome::InputChanged));
    assert_eq!(skipped_after.after.expect("the after state is kept").skipped_paths.len(), 1);

    // A Git filter configured during the run stops the status being read:
    // the launcher records it after the run, as an input change.
    let filtered_after = detail_of(
        &fixture,
        "test-committed-filtered-after",
        with_input(json!({
            "kind": "commit", "commit": COMMIT, "cleanBefore": true,
            "after": { "commit": COMMIT, "clean": false, "paths": [],
                       "filters": ["filter.fixture.clean"] },
            "outcome": "input_changed", "eligible": false,
        })),
    )
    .committed_input
    .expect("an input-changed record for a filter reads");
    assert_eq!(filtered_after.outcome, Some(TestRunCommittedInputOutcome::InputChanged));
    assert_eq!(
        filtered_after.after.expect("the after state is kept").filters,
        vec!["filter.fixture.clean".to_owned()]
    );

    let pending = detail_of(
        &fixture,
        "test-committed-pending",
        with_input(json!({ "kind": "commit", "commit": COMMIT, "cleanBefore": true })),
    )
    .committed_input
    .expect("a record written before the run reads");
    assert_eq!((pending.after, pending.outcome, pending.eligible), (None, None, None));

    // The wire form names it beside its siblings, in camelCase.
    let wire = serde_json::to_value(&clean).unwrap();
    assert_eq!(wire["committedInput"]["cleanBefore"], true);
    assert_eq!(wire["committedInput"]["outcome"], "clean");
    assert_eq!(wire["committedInput"]["after"]["commit"], COMMIT);
}

#[test]
fn a_staged_lane_run_has_no_committed_input() {
    let fixture = RunFixture::new("committed-input-staged");
    let mut results = passed_results();
    results["expectedFingerprint"] = json!("f".repeat(64));
    results["before"] = json!("f".repeat(64));
    results["after"] = json!("f".repeat(64));
    let detail = detail_of(&fixture, "test-staged-lane", results);
    assert_eq!(detail.committed_input, None);
    assert_eq!(detail.after.as_deref(), Some("f".repeat(64).as_str()));
    let wire = serde_json::to_value(&detail).unwrap();
    assert_eq!(wire["committedInput"], Value::Null);
}

#[test]
fn a_malformed_committed_input_reads_as_none() {
    let fixture = RunFixture::new("committed-input-malformed");
    let cases = [
        json!("commit"),
        json!(null),
        json!({ "kind": "tree", "commit": COMMIT, "cleanBefore": true }),
        json!({ "kind": "commit", "commit": COMMIT }),
        json!({ "kind": "commit", "commit": "not-a-commit", "cleanBefore": true }),
        json!({ "kind": "commit", "commit": COMMIT.to_uppercase(), "cleanBefore": true }),
        json!({ "kind": "commit", "cleanBefore": true }),
        json!({ "kind": "commit", "commit": COMMIT, "cleanBefore": "yes" }),
        json!({ "kind": "commit", "commit": COMMIT, "cleanBefore": true,
                "outcome": "someday", "after": { "commit": COMMIT, "clean": true } }),
        json!({ "kind": "commit", "commit": COMMIT, "cleanBefore": true, "outcome": "clean" }),
        json!({ "kind": "commit", "commit": COMMIT, "cleanBefore": true,
                "after": { "commit": COMMIT, "clean": false,
                           "paths": [{ "status": "??", "path": "x" }] },
                "outcome": "debris_pending_disposition", "eligible": true }),
        json!({ "kind": "commit", "commit": COMMIT, "cleanBefore": true,
                "after": { "commit": MOVED, "clean": true, "paths": [] },
                "outcome": "clean", "eligible": true }),
        json!({ "kind": "commit", "cleanBefore": false,
                "after": { "commit": COMMIT, "clean": true, "paths": [] }, "outcome": "clean" }),
        json!({ "kind": "commit", "commit": COMMIT, "cleanBefore": true,
                "after": { "commit": COMMIT, "clean": true, "paths": "none" } }),
        // A skipped tracked path is never clean, and never only debris.
        json!({ "kind": "commit", "commit": COMMIT, "cleanBefore": true,
                "after": { "commit": COMMIT, "clean": true, "paths": [],
                           "skippedPaths": [{ "tag": "S", "path": "x" }] },
                "outcome": "clean", "eligible": true }),
        json!({ "kind": "commit", "commit": COMMIT, "cleanBefore": true,
                "after": { "commit": COMMIT, "clean": false,
                           "paths": [{ "status": "??", "path": "y" }],
                           "skippedPaths": [{ "tag": "S", "path": "x" }] },
                "outcome": "debris_pending_disposition", "eligible": false }),
        json!({ "kind": "commit", "commit": COMMIT, "cleanBefore": true,
                "skippedPaths": [{ "tag": "S", "path": "x" }] }),
        // A configured filter is never clean.
        json!({ "kind": "commit", "commit": COMMIT, "cleanBefore": true,
                "filters": ["filter.fixture.clean"] }),
        json!({ "kind": "commit", "commit": COMMIT, "cleanBefore": true,
                "after": { "commit": COMMIT, "clean": true, "paths": [],
                           "filters": ["filter.fixture.clean"] },
                "outcome": "clean", "eligible": true }),
        // A submodule gitlink is never clean, and never only debris.
        json!({ "kind": "commit", "commit": COMMIT, "cleanBefore": true,
                "gitlinks": ["sub"] }),
        json!({ "kind": "commit", "commit": COMMIT, "cleanBefore": true,
                "after": { "commit": COMMIT, "clean": true, "paths": [],
                           "gitlinks": ["sub"] },
                "outcome": "clean", "eligible": true }),
        json!({ "kind": "commit", "commit": COMMIT, "cleanBefore": true,
                "after": { "commit": COMMIT, "clean": false,
                           "paths": [{ "status": "??", "path": "y" }],
                           "gitlinks": ["sub"] },
                "outcome": "debris_pending_disposition", "eligible": false }),
    ];
    for (index, input) in cases.into_iter().enumerate() {
        let detail = detail_of(
            &fixture,
            &format!("test-malformed-{index}"),
            with_input(input.clone()),
        );
        assert_eq!(detail.committed_input, None, "{input}");
        assert_eq!(detail.stages.len(), 2, "the rest of the detail still reads: {input}");
    }
}
