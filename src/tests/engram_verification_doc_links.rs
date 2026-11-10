//! Exact focused documentation checks use the existing Lint record route.
use super::*;

fn doc_run(fixture: &RunFixture, outcome: &str, argv: &[&str]) -> RecordRun {
    let mut request = commit_request(&fixture.root);
    request["full"] = json!(false);
    request["stages"] = json!([{"name":"focused", "command":argv[0], "args":&argv[1..]}]);
    let mut results = commit_results();
    results["state"] = json!(outcome);
    results["exitCode"] = json!(if outcome == "passed" { 0 } else { 1 });
    results["stages"] = json!([stage(
        "focused",
        outcome,
        if outcome == "passed" { 0 } else { 1 },
        argv,
        0
    )]);
    let mut run = record_run(fixture, "test-doc-links", request, results);
    run.registration.preset = TestRunPreset::Focused;
    run.registration.stage_names = vec!["focused".to_owned()];
    run
}

#[test]
fn exact_doc_link_check_sends_pass_and_failure_as_lint_without_test_credit() {
    for program in [
        "node",
        "/usr/bin/node",
        "C:\\Program Files\\nodejs\\node.exe",
    ] {
        for outcome in ["passed", "failed"] {
            let fixture = RunFixture::new(&format!("verification-doc-{outcome}"));
            let run = doc_run(&fixture, outcome, &[program, "scripts/check-doc-links.mjs"]);
            let frozen = ready(&run.registration);
            let checks = frozen.record["checks"].as_array().unwrap();
            assert_eq!(checks.len(), 1);
            assert_eq!(checks[0]["check_kind"], "lint");
            assert_eq!(checks[0]["outcome"], outcome);
            if program == "node" {
                assert_eq!(checks[0]["command"], "node scripts/check-doc-links.mjs");
            }
            assert!(checks[0].get("counts").is_none());
            assert!(
                engram_verification_record_producers(&run.registration.run_id, &frozen.body)
                    .is_empty(),
                "Lint creates no Test observation"
            );

            let transport = Arc::new(RecordingTransport::default());
            let adapter = adapter(&transport);
            let own = connection(SESSION, "actor");
            read_work_session(&transport, &adapter, &own, supported());
            transport.script(Ok(json!({"operation":PRODUCER, "producer":PRODUCER,
            "verifications":[{"stage":"focused", "record_id":"a".repeat(32)}],
            "run_outcome":outcome})));
            let step = engram_verification_record_send(frozen, link(&adapter, &own));
            assert!(
                matches!(step, EngramVerificationRecordStep::Received { .. }),
                "{step:?}"
            );
            let sent = transport.sent_records();
            assert_eq!(sent.len(), 1);
            assert_eq!(sent[0]["checks"][0]["check_kind"], "lint");
            assert_eq!(sent[0]["checks"][0]["outcome"], outcome);
        }
    }
}

#[test]
fn doc_link_lint_does_not_admit_other_spellings_arguments_or_stage_labels() {
    for argv in [
        vec!["node", "scripts/check-doc-links-renamed.mjs"],
        vec!["node", "./scripts/check-doc-links.mjs"],
        vec!["node", "scripts/check-doc-links.mjs", "--all"],
        vec!["node", "--trace-warnings", "scripts/check-doc-links.mjs"],
        vec!["node.cmd", "scripts/check-doc-links.mjs"],
        vec!["node.bat", "scripts/check-doc-links.mjs"],
        vec!["node.ps1", "scripts/check-doc-links.mjs"],
        vec!["env", "node", "scripts/check-doc-links.mjs"],
    ] {
        let fixture = RunFixture::new("verification-doc-unclassified");
        let run = doc_run(&fixture, "passed", &argv);
        assert_eq!(
            pending_of(&run.registration),
            EngramVerificationRecordPending::UnclassifiedStages {
                stages: vec!["focused".to_owned()]
            }
        );
        assert!(
            !run.run_dir
                .join(ENGRAM_VERIFICATION_RECORD_BODY_FILE)
                .exists()
        );
    }
    let argv = vec!["node".to_owned(), "scripts/check-doc-links.mjs".to_owned()];
    assert_eq!(
        engram_verification_record_check_kind(
            EngramVerificationRecordMode::Focused,
            "lint",
            &argv,
            &[]
        ),
        None
    );
    assert!(
        EngramHost::recognized_check("node scripts/check-doc-links.mjs").is_none(),
        "registered Lint does not change ordinary Test recognition"
    );
}
