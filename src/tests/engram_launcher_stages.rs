// Owns the tests of which launcher stages are test stages
// (src/engram_launcher_stages.rs): the rule by a request record's `kind`, or
// by TermAl's two names in a record without kinds; a foreground full gate
// credited by its own request record, found from its summary's `results:`
// line; and a carried run judged by the same rule, for Engram's nine-stage
// gate and TermAl's own five-stage one. Does not own the rest of a carried
// run's judgment (src/tests/engram_carried_checks.rs) or the other runners'
// evidence (src/tests/engram_turn_checks.rs). New module, a child of the
// check tests like its siblings.
use super::*;

/// Engram's full gate as its launcher writes it: nine stages, each with a
/// `kind`, five of them `test`.
const ENGRAM_STAGES: [(&str, Option<&str>); 9] = [
    ("fmt", Some("lint")),
    ("check", Some("build")),
    ("clippy", Some("lint")),
    ("rust", Some("test")),
    ("freeze", Some("test")),
    ("mcp", Some("test")),
    ("control", Some("test")),
    ("parity", Some("test")),
    ("docs", Some("other")),
];

/// TermAl's own full gate: five stages with no `kind`.
const TERMAL_STAGES: [(&str, Option<&str>); 5] = [
    ("cargo-check", None),
    ("typescript", None),
    ("fingerprint-tests", None),
    ("rust-tests", None),
    ("vitest", None),
];

/// A request record listing `stages`, each with its `kind` where it has one.
fn request(run_id: &str, stages: &[(&str, Option<&str>)]) -> Value {
    json!({
        "runId": run_id,
        "stages": stages
            .iter()
            .map(|(name, kind)| match kind {
                Some(kind) => json!({ "name": name, "kind": kind }),
                None => json!({ "name": name }),
            })
            .collect::<Vec<_>>(),
        "full": true,
        "started": "2026-10-01T00:00:00.000Z",
        "expectedFingerprint": "f".repeat(64),
    })
}

/// A run directory `run_id` under `runs` whose request lists `stages` and
/// whose terminal record says `state`, every stage passed but `failed_stage`.
fn write_run(
    runs: &FsPath,
    run_id: &str,
    stages: &[(&str, Option<&str>)],
    state: &str,
    failed_stage: Option<&str>,
) -> PathBuf {
    let directory = runs.join(run_id);
    fs::create_dir_all(&directory).expect("the run directory should be created");
    fs::write(
        directory.join("request.json"),
        serde_json::to_vec(&request(run_id, stages)).expect("the request serializes"),
    )
    .expect("the request should write");
    let fingerprint = "f".repeat(64);
    let passed = state == "passed";
    fs::write(
        directory.join("results.json"),
        serde_json::to_vec(&json!({
            "runId": run_id,
            "state": state,
            "stages": stages
                .iter()
                .map(|(name, _)| {
                    let stage_passed = Some(*name) != failed_stage;
                    json!({
                        "name": name,
                        "state": if stage_passed { "passed" } else { "failed" },
                        "code": if stage_passed { 0 } else { 1 },
                    })
                })
                .collect::<Vec<_>>(),
            "expectedFingerprint": fingerprint,
            "before": fingerprint,
            "after": fingerprint,
            "exitCode": if passed { 0 } else { 1 },
            "ended": "2026-10-01T00:05:00.000Z",
        }))
        .expect("the results serialize"),
    )
    .expect("the results should write");
    directory
}

/// A launcher summary as it prints: its verdict, its `results:` line naming
/// `directory`'s record, and each stage's line.
fn summary(
    directory: &FsPath,
    stages: &[(&str, Option<&str>)],
    failed_stage: Option<&str>,
) -> String {
    let run_id = directory.file_name().unwrap().to_string_lossy();
    let verdict = if failed_stage.is_some() {
        "FAIL"
    } else {
        "PASS"
    };
    let mut lines = vec![
        format!(
            "{verdict} {run_id} exit={}",
            u8::from(failed_stage.is_some())
        ),
        format!("results: {}", directory.join("results.json").display()),
    ];
    lines.extend(stages.iter().map(|(name, _)| {
        let state = if Some(*name) == failed_stage {
            "failed exit=1"
        } else {
            "passed exit=0"
        };
        format!(
            "{name}: {state} log={}",
            directory.join(format!("{name}.log")).display()
        )
    }));
    lines.join("\n")
}

#[test]
fn a_stage_is_a_test_stage_by_its_requests_kind_or_else_by_name() {
    assert_eq!(
        engram_launcher_test_stages(&request("test-1", &ENGRAM_STAGES)),
        ["rust", "freeze", "mcp", "control", "parity"]
    );
    assert_eq!(
        engram_launcher_test_stages(&request("test-1", &TERMAL_STAGES)),
        ["rust-tests", "vitest"]
    );
    // Once any stage gives a kind, kinds alone decide: a stage named
    // `rust-tests` but given no kind, or another kind, is no test stage.
    assert!(
        engram_launcher_test_stages(&request(
            "test-1",
            &[
                ("check", Some("build")),
                ("rust-tests", None),
                ("vitest", Some("lint"))
            ]
        ))
        .is_empty()
    );
    // A record that gives kinds but marks no stage `test`, and one without
    // stages, have none.
    assert!(
        engram_launcher_test_stages(&request(
            "test-1",
            &[("fmt", Some("lint")), ("check", Some("build"))]
        ))
        .is_empty()
    );
    assert!(engram_launcher_test_stages(&json!({ "runId": "test-1" })).is_empty());
    // Engram's names without kinds are not TermAl's names.
    assert!(
        engram_launcher_test_stages(&request("test-1", &[("rust", None), ("freeze", None)]))
            .is_empty()
    );
}

#[test]
fn a_foreground_full_gate_is_credited_by_its_own_request_records_test_stages() {
    let temp = TestTempRoot::create("termal-engram-launcher-stages");
    let check = engram_check_command("node scripts/test-launcher.mjs full").expect("a test");
    let showed = |output: &str| {
        engram_check_showed_passing_tests(
            &check,
            &engram_check_result_lines(&check.program, output),
            &engram_launcher_output_test_stages(output),
        )
    };

    // Engram's nine-stage gate, every stage passed: credited by its kinds.
    let engram = write_run(temp.path(), "test-engram", &ENGRAM_STAGES, "passed", None);
    assert!(showed(&summary(&engram, &ENGRAM_STAGES, None)));
    // TermAl's own gate, whose record gives no kinds: credited as before.
    let termal = write_run(temp.path(), "test-termal", &TERMAL_STAGES, "passed", None);
    assert!(showed(&summary(&termal, &TERMAL_STAGES, None)));
    // A launcher that names no run directory gets TermAl's names.
    assert!(showed("PASS test-1 exit=0\nrust-tests: passed exit=0"));
    assert!(!showed("PASS test-1 exit=0\nrust: passed exit=0"));

    // Engram's stage lines beside a record that marks no stage `test`.
    let untested = write_run(
        temp.path(),
        "test-untested",
        &[("fmt", Some("lint")), ("rust", Some("build"))],
        "passed",
        None,
    );
    assert!(!showed(&summary(&untested, &ENGRAM_STAGES, None)));
    // A `results:` line whose record is gone, is another run's, or is not
    // an absolute path shows nothing, whatever its stage lines say.
    let gone = temp.path().join("test-gone");
    assert!(!showed(&summary(&gone, &ENGRAM_STAGES, None)));
    let elsewhere = write_run(
        temp.path(),
        "test-elsewhere",
        &ENGRAM_STAGES,
        "passed",
        None,
    );
    fs::write(
        elsewhere.join("request.json"),
        serde_json::to_vec(&request("test-other", &ENGRAM_STAGES)).unwrap(),
    )
    .unwrap();
    assert!(!showed(&summary(&elsewhere, &ENGRAM_STAGES, None)));
    assert!(!showed(
        "PASS test-1 exit=0\nresults: review-runs/test-1/results.json\nrust-tests: passed exit=0"
    ));
    // Only a test stage's pass is the evidence: Engram's lint and build
    // stages passing with every test stage failed show nothing.
    assert!(!showed(
        &summary(&engram, &ENGRAM_STAGES, None)
            .lines()
            .filter(|line| !["rust", "freeze", "mcp", "control", "parity"]
                .iter()
                .any(|stage| line.starts_with(&format!("{stage}:"))))
            .collect::<Vec<_>>()
            .join("\n")
    ));
}

#[test]
fn a_carried_full_gate_is_judged_by_the_same_rule() {
    let temp = TestTempRoot::create("termal-engram-launcher-stages-carried");
    let read = |directory: &FsPath| {
        let digest = engram_terminal_record_digest(directory).expect("a terminal record");
        engram_read_carried_run(directory, &digest)
    };

    // Engram's nine-stage gate, every stage passed: a passed test check.
    let engram = write_run(temp.path(), "test-engram", &ENGRAM_STAGES, "passed", None);
    let verdict = read(&engram).expect("Engram's passed gate is credited");
    assert!(verdict.passed);
    assert_eq!(verdict.stages.len(), 9);
    // Failed at a test stage, or before any (at clippy): recorded failed, so
    // it stays the newest check.
    for failed_stage in ["rust", "clippy"] {
        let failed = write_run(
            temp.path(),
            &format!("test-engram-{failed_stage}"),
            &ENGRAM_STAGES,
            "failed",
            Some(failed_stage),
        );
        let verdict = read(&failed).expect("a failed gate is recorded");
        assert!(!verdict.passed, "failed at {failed_stage}");
        assert!(
            verdict
                .stage_lines
                .contains(&format!("{failed_stage}: failed exit=1")),
            "{:?}",
            verdict.stage_lines
        );
    }
    // TermAl's own gate, as before.
    let termal = write_run(temp.path(), "test-termal", &TERMAL_STAGES, "passed", None);
    assert!(
        read(&termal)
            .expect("TermAl's passed gate is credited")
            .passed
    );
    // A record that gives kinds but marks no stage `test` is not credited.
    let untested = write_run(
        temp.path(),
        "test-untested",
        &[("fmt", Some("lint")), ("check", Some("build"))],
        "passed",
        None,
    );
    assert_eq!(read(&untested), Err(ENGRAM_CARRIED_RUN_NO_TEST_STAGE));
}

#[path = "engram_focused_launcher_credit.rs"]
mod focused_credit;
