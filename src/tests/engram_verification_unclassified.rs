// Terminal generic-run refusal through the actual sender, registration
// persistence, launcher summary and original requester feedback owners.
use super::*;

#[path = "engram_verification_unclassified_recovery.rs"]
mod recovery;

fn wrapper_run(fixture: &RunFixture, session: &str, label: &str) -> RecordRun {
    let mut request = commit_request(&fixture.root);
    request["full"] = json!(false);
    request["stages"] = json!([{
        "name":"focused", "kind":"other", "command":"pwsh",
        "args":["-NoProfile", "-File", "scripts/test-rust.ps1", "--test",
            "diagnostics_cli", "--", "--test-threads=2", "private-argument-sentinel"]
    }]);
    let mut results = commit_results();
    results["state"] = json!("passed");
    results["exitCode"] = json!(0);
    let mut finished = stage(
        "focused",
        "passed",
        0,
        &[
            "pwsh",
            "-NoProfile",
            "-File",
            "scripts/test-rust.ps1",
            "--test",
            "diagnostics_cli",
            "--",
            "--test-threads=2",
            "private-argument-sentinel",
        ],
        1,
    );
    finished["tests"] = json!({"passed":33,"failed":0});
    results["stages"] = json!([finished]);
    let mut run = record_run(fixture, label, request, results);
    initiated_by(&mut run, session);
    run.registration.stage_names = vec!["focused".into()];
    run
}

fn refusal(fixture: &RunFixture, run: &RecordRun) -> String {
    match current(fixture, run).verification_record {
        Some(TestRunVerificationAck::LocalUnclassified { detail }) => detail,
        other => panic!("expected a final local refusal, got {other:?}"),
    }
}

fn focused_record_run(
    fixture: &RunFixture,
    run_id: &str,
    planned: &[&str],
    finished: Value,
) -> RecordRun {
    let mut request = commit_request(&fixture.root);
    request["full"] = json!(false);
    request["detached"] = json!(false);
    request["stages"] = json!([{
        "name": "focused", "command": planned[0], "args": &planned[1..]
    }]);
    let mut results = commit_results();
    results["state"] = finished["state"].clone();
    results["exitCode"] = json!(if finished["code"].as_i64() == Some(0) {
        0
    } else {
        1
    });
    results["stages"] = json!([finished]);
    let mut run = record_run(fixture, run_id, request, results);
    run.registration.preset = TestRunPreset::Focused;
    run.registration.stage_names = vec!["focused".to_owned()];
    run
}

#[test]
fn recognized_focused_cargo_test_records_crash_failure_and_pass() {
    let fixture = RunFixture::new("focused-cargo-outcomes");
    let argv = ["cargo", "test", "--bin", "termal"];
    let cases = [
        (
            "test-focused-cargo-crash",
            "failed",
            101,
            None,
            EngramVerificationRecordCheckOutcome::Failed,
        ),
        (
            "test-focused-cargo-failure",
            "failed",
            101,
            Some(json!({"passed": 2, "failed": 1})),
            EngramVerificationRecordCheckOutcome::Failed,
        ),
        (
            "test-focused-cargo-pass",
            "passed",
            0,
            Some(json!({"passed": 3, "failed": 0})),
            EngramVerificationRecordCheckOutcome::Passed,
        ),
    ];
    for (run_id, state, code, counts, expected) in cases {
        let mut finished = stage("focused", state, code, &argv, 1);
        // Cargo reports the child crash with a failing exit and no libtest totals.
        let has_counts = counts.is_some();
        if let Some(counts) = counts {
            finished["tests"] = counts;
        }
        let run = focused_record_run(&fixture, run_id, &argv, finished);

        let frozen = ready(&run.registration);
        let [check] = frozen.body.checks.as_slice() else {
            panic!("{run_id}: expected one verification check");
        };
        assert_eq!(
            check.check_kind,
            EngramVerificationRecordCheckKind::Test,
            "{run_id}"
        );
        assert_eq!(check.outcome, expected, "{run_id}");
        assert_eq!(check.counts.is_some(), has_counts, "{run_id}");
    }
}

fn failed_focused_powershell_run(fixture: &RunFixture, planned: &[&str]) -> RecordRun {
    let mut observed = planned.to_vec();
    observed[0] = r"C:\Program Files\PowerShell\7\pwsh.exe";
    let mut finished = stage("focused", "failed", 1, &observed, 1);
    finished["signal"] = Value::Null;
    focused_record_run(fixture, "test-focused-powershell-crash", planned, finished)
}

#[test]
fn failed_focused_powershell_test_rust_ordinary_stays_unclassified() {
    let fixture = RunFixture::new("focused-powershell-test-rust-ordinary");
    let run = failed_focused_powershell_run(
        &fixture,
        &[
            "pwsh",
            "-NoProfile",
            "-File",
            "scripts/test-rust.ps1",
            "--phase",
            "ordinary",
        ],
    );

    assert_eq!(
        pending_of(&run.registration),
        EngramVerificationRecordPending::UnclassifiedStages {
            stages: vec!["focused".to_owned()]
        }
    );
}

#[test]
fn failed_focused_powershell_inline_command_stays_unclassified() {
    let fixture = RunFixture::new("focused-powershell-inline");
    let run = failed_focused_powershell_run(
        &fixture,
        &["pwsh", "-NoProfile", "-Command", "cargo test --bin termal"],
    );

    assert_eq!(
        pending_of(&run.registration),
        EngramVerificationRecordPending::UnclassifiedStages {
            stages: vec!["focused".to_owned()]
        }
    );
}

#[test]
fn completed_unindexed_wrapper_is_finally_refused_and_tells_only_its_requester() {
    let mut fixture = RunFixture::new("unclassified-final-refusal");
    synchronous_persistence(&mut fixture);
    let (session, transport) = engram_session(&fixture, true);
    let (other, other_transport) = engram_session(&fixture, true);
    let run = wrapper_run(&fixture, &session, "test-unclassified-final");
    register(&fixture, &[&run]);
    assert!(
        fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_runs
            .entries
            .with_run_id(&run.registration.run_id)
            .next()
            .is_none(),
        "registration delivery does not require a disk-index entry"
    );
    let other_before = session_engram(&fixture, &other, |engram| {
        engram.pending_source_root_line.clone()
    });
    let mut cursor = EngramVerificationRecordCursor::default();
    fixture.state.engram_verification_records_tick_from(
        Duration::from_secs(1),
        &|| false,
        &mut cursor,
    );
    let detail = refusal(&fixture, &run);
    assert!(detail.starts_with("unclassified_stages:"));
    assert!(detail.contains("focused"));
    assert!(detail.contains("declared-command native route"));
    assert!(detail.len() <= 1024);
    assert!(!detail.contains("private-argument-sentinel"));
    assert_eq!(
        engram_verification_record_progress(&run.run_dir),
        Some(EngramVerificationRecordProgress::Rejected {
            detail: detail.clone()
        })
    );
    let notice = session_engram(&fixture, &session, |engram| {
        engram.pending_source_root_line.clone()
    })
    .unwrap();
    assert!(notice.contains(&run.registration.run_id));
    assert!(notice.contains(&detail));
    assert_eq!(
        session_engram(&fixture, &other, |engram| engram
            .pending_source_root_line
            .clone()),
        other_before
    );
    assert!(
        !run.run_dir
            .join(ENGRAM_VERIFICATION_RECORD_BODY_FILE)
            .exists()
    );
    assert!(
        transport.sent_operations().is_empty(),
        "no route lookup or producer send"
    );
    assert!(other_transport.sent_operations().is_empty());
    let loaded = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        loaded.test_run_registrations[&run.registration.run_id].verification_record,
        Some(TestRunVerificationAck::LocalUnclassified {
            detail: detail.clone()
        })
    );
    assert_eq!(
        fixture.state.engram_verification_records_tick_from(
            Duration::from_secs(1),
            &|| false,
            &mut cursor
        ),
        0,
        "terminal refusal is not retried"
    );
    assert!(transport.sent_operations().is_empty());
    let rendered = super::refusals::summary(&run.run_dir);
    assert!(rendered.contains("verification: rejected:"), "{rendered}");
    assert!(rendered.contains(&detail), "{rendered}");
}

#[test]
fn registration_delivery_then_terminal_results_refuses_without_a_new_run() {
    let mut fixture = RunFixture::new("unclassified-late-delivery");
    synchronous_persistence(&mut fixture);
    let (session, transport) = engram_session(&fixture, true);
    let run = wrapper_run(&fixture, &session, "test-unclassified-late");
    // Before registration publication there is no owner to settle.
    fixture.state.engram_verification_records_tick();
    register(&fixture, &[&run]);
    let result_path = run.run_dir.join("results.json");
    let finished = fs::read(&result_path).unwrap();
    let mut running: Value = serde_json::from_slice(&finished).unwrap();
    running["state"] = json!("running");
    running["exitCode"] = Value::Null;
    running["ended"] = Value::Null;
    fs::write(&result_path, serde_json::to_vec_pretty(&running).unwrap()).unwrap();
    fixture.state.engram_verification_records_tick();
    assert!(current(&fixture, &run).verification_record.is_none());
    fs::write(result_path, finished).unwrap();
    fixture.state.engram_verification_records_tick();
    assert!(refusal(&fixture, &run).starts_with("unclassified_stages:"));
    assert!(transport.sent_operations().is_empty());
}

#[test]
fn stale_unclassified_snapshot_cannot_replace_a_held_receipt_or_another_registration() {
    let mut fixture = RunFixture::new("unclassified-stale-snapshot");
    synchronous_persistence(&mut fixture);
    let (session, transport) = engram_session(&fixture, true);
    let run = wrapper_run(&fixture, &session, "test-unclassified-stale");
    register(&fixture, &[&run]);
    let receipt: EngramVerificationRecordReceipt =
        serde_json::from_value(receipt_for("passed")).unwrap();
    let held = TestRunVerificationAck::Received {
        receipt,
        body_digest: "a".repeat(64),
    };
    fixture
        .state
        .note_engram_verification_record_ack(&run.registration, held.clone());
    assert!(
        !fixture
            .state
            .reject_unclassified_verification_record(&run.registration, &["focused".into()])
    );
    assert_eq!(current(&fixture, &run).verification_record, Some(held));
    let mut changed = run.registration.clone();
    changed.request_identity = "b".repeat(64);
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .insert(changed.run_id.clone(), changed.clone());
    assert!(
        !fixture
            .state
            .reject_unclassified_verification_record(&run.registration, &["focused".into()])
    );
    assert_eq!(current(&fixture, &run), changed);
    assert!(transport.sent_operations().is_empty());
}

#[test]
fn stale_unclassified_result_cannot_refuse_a_now_recognized_command() {
    let mut fixture = RunFixture::new("unclassified-replaced-results");
    synchronous_persistence(&mut fixture);
    let (session, _) = engram_session(&fixture, true);
    let run = wrapper_run(&fixture, &session, "test-unclassified-replaced");
    register(&fixture, &[&run]);
    let result_path = run.run_dir.join("results.json");
    let mut results: Value = serde_json::from_slice(&fs::read(&result_path).unwrap()).unwrap();
    results["stages"][0]["command"] = json!(["cargo", "test"]);
    fs::write(result_path, serde_json::to_vec_pretty(&results).unwrap()).unwrap();
    assert!(
        !fixture
            .state
            .reject_unclassified_verification_record(&run.registration, &["focused".into()])
    );
    assert!(current(&fixture, &run).verification_record.is_none());
}

#[test]
fn an_existing_body_preserves_uncertain_producer_custody() {
    let mut fixture = RunFixture::new("unclassified-existing-body");
    synchronous_persistence(&mut fixture);
    let (session, transport) = engram_session(&fixture, true);
    let run = wrapper_run(&fixture, &session, "test-unclassified-existing-body");
    register(&fixture, &[&run]);
    let path = run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE);
    let retained = b"retained body whose producer outcome is uncertain";
    fs::write(&path, retained).unwrap();
    fixture.state.engram_verification_records_tick();
    assert!(current(&fixture, &run).verification_record.is_none());
    assert_eq!(fs::read(path).unwrap(), retained);
    assert!(!matches!(
        engram_verification_record_progress(&run.run_dir),
        Some(EngramVerificationRecordProgress::Rejected { .. })
    ));
    assert!(transport.sent_operations().is_empty());
}

#[test]
fn unclassified_reason_bounds_and_redacts_stage_text() {
    let mut fixture = RunFixture::new("unclassified-bounded");
    synchronous_persistence(&mut fixture);
    let (session, _) = engram_session(&fixture, true);
    let run = wrapper_run(&fixture, &session, "test-unclassified-bounded");
    let stages = (0..30)
        .map(|i| format!("stage-{i}-{}", "é\n".repeat(100)))
        .collect::<Vec<_>>();
    let detail = unclassified_verification_detail(&stages);
    assert!(detail.len() <= 1024);
    assert!(!detail.chars().any(|ch| matches!(ch, '\r' | '\n' | 'é')));
    assert!(detail.contains("+22 more"));
    // The detail formatter cannot create a disposition without real preparation.
    register(&fixture, &[&run]);
    assert!(
        !fixture
            .state
            .reject_unclassified_verification_record(&run.registration, &stages)
    );
    assert!(current(&fixture, &run).verification_record.is_none());
}
