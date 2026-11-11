//! Direct Node Vitest runs retain their actual argv through Test recording.
use super::*;

fn vitest_run(fixture: &RunFixture, outcome: &str, argv: &[&str]) -> RecordRun {
    vitest_stage_run(fixture, outcome, argv, "focused", None)
}

fn vitest_stage_run(
    fixture: &RunFixture,
    outcome: &str,
    argv: &[&str],
    name: &str,
    scope: Option<Value>,
) -> RecordRun {
    let mut request = commit_request(&fixture.root);
    request["full"] = json!(false);
    request["stages"] = json!([{"name":name, "command":argv[0], "args":&argv[1..]}]);
    if let Some(scope) = scope {
        request["scope"] = scope;
    }
    let code = if outcome == "passed" { 0 } else { 1 };
    let mut results = commit_results();
    results["state"] = json!(outcome);
    results["exitCode"] = json!(code);
    results["stages"] = json!([stage(name, outcome, code, argv, 0)]);
    let mut run = record_run(fixture, "test-direct-vitest", request, results);
    run.registration.preset = TestRunPreset::Focused;
    run.registration.stage_names = vec![name.to_owned()];
    run
}

#[test]
fn committed_vitest_category_keeps_its_unclassified_no_credit_policy() {
    let fixture = RunFixture::new("verification-vitest-category");
    let argv = [
        r"C:\Program Files\nodejs\node.exe",
        "node_modules/vitest/vitest.mjs",
        "run",
        "--project",
        "component",
    ];
    let run = vitest_stage_run(
        &fixture,
        "passed",
        &argv,
        "ui-component",
        Some(json!({"kind":"category", "category":"ui-component", "project":"component"})),
    );
    assert_eq!(
        pending_of(&run.registration),
        EngramVerificationRecordPending::UnclassifiedStages {
            stages: vec!["ui-component".to_owned()]
        }
    );
    let argv: Vec<String> = argv.into_iter().map(str::to_owned).collect();
    assert_eq!(
        engram_verification_record_check_kind(
            EngramVerificationRecordMode::Focused,
            "ui-component",
            &argv,
            &["ui-component".to_owned()]
        ),
        None,
        "neither real category argv nor a client label changes category credit"
    );
}

#[test]
fn direct_node_vitest_records_test_and_accepts_its_stage_receipt() {
    for script in [
        "ui/node_modules/vitest/vitest.mjs",
        "node_modules/vitest/vitest.mjs",
        r"C:\repo with spaces\ui\node_modules\vitest\vitest.mjs",
    ] {
        for outcome in ["passed", "failed"] {
            let fixture = RunFixture::new("verification-direct-vitest");
            let argv = [
                r"C:\Program Files\nodejs\node.exe",
                script,
                "run",
                "--root",
                "ui",
                "--project=component",
                "src/transcript-activity-slot.test.tsx",
                "src/panels/SessionActivityStrip.test.tsx",
            ];
            let run = vitest_run(&fixture, outcome, &argv);
            let frozen = ready(&run.registration);
            assert_eq!(frozen.body.checks.len(), 1);
            let check = &frozen.body.checks[0];
            assert_eq!(check.check_kind, EngramVerificationRecordCheckKind::Test);
            assert_eq!(
                engram_shell_words(&check.command).unwrap(),
                argv.map(str::to_owned)
            );
            let producers =
                engram_verification_record_producers(&run.registration.run_id, &frozen.body);
            assert_eq!(producers.len(), 1);
            assert_eq!(producers[0].0, "focused");
            assert_eq!(
                producers[0].1.outcome,
                if outcome == "passed" {
                    EngramExecutionOutcome::Succeeded
                } else {
                    EngramExecutionOutcome::Failed
                }
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
            assert_eq!(sent[0]["checks"][0]["check_kind"], "test");
            assert_eq!(sent[0]["checks"][0]["outcome"], outcome);
        }
    }
}

#[test]
fn direct_node_vitest_refuses_nonexecution_and_unknown_forms_despite_test_labels() {
    for args in [
        vec!["ui/node_modules/vitest/vitest.mjs"],
        vec!["ui/node_modules/vitest/vitest.mjs", "watch"],
        vec!["ui/node_modules/vitest/vitest.mjs", "list"],
        vec!["ui/node_modules/vitest/vitest.mjs", "run", "--help"],
        vec!["ui/node_modules/vitest/vitest.mjs", "run", "-h"],
        vec!["ui/node_modules/vitest/vitest.mjs", "run", "--version"],
        vec!["ui/node_modules/vitest/vitest.mjs", "run", "--watch=true"],
        vec![
            "ui/node_modules/vitest/vitest.mjs",
            "run",
            "--passWithNoTests",
        ],
        vec![
            "ui/node_modules/vitest/vitest.mjs",
            "run",
            "--typecheck.only",
        ],
        vec!["ui/node_modules/vitest/vitest.mjs", "run", "--list"],
        vec!["ui/node_modules/vitest/vitest.mjs", "run", "--root"],
        vec!["ui/node_modules/vitest/vitest.mjs", "run", "--root="],
        vec![
            "ui/node_modules/vitest/vitest.mjs",
            "run",
            "--project",
            "--help",
        ],
        vec!["ui/node_modules/vitest/vitest.mjs", "run", "--unknown"],
        vec!["ui/node_modules/fake-vitest/vitest.mjs", "run"],
        vec!["ui/node_modules/vitest/vitest.mjs.backup", "run"],
        vec!["scripts/vitest.mjs", "run"],
        vec!["--eval", "ui/node_modules/vitest/vitest.mjs", "run"],
    ] {
        let fixture = RunFixture::new("verification-direct-vitest-refused");
        let mut argv = vec!["node"];
        argv.extend(args);
        let run = vitest_run(&fixture, "passed", &argv);
        assert_eq!(
            pending_of(&run.registration),
            EngramVerificationRecordPending::UnclassifiedStages {
                stages: vec!["focused".to_owned()]
            },
            "{argv:?}"
        );
        let argv: Vec<String> = argv.into_iter().map(str::to_owned).collect();
        assert_eq!(
            engram_verification_record_check_kind(
                EngramVerificationRecordMode::Focused,
                "focused",
                &argv,
                &["focused".to_owned()]
            ),
            None,
            "a client label cannot rescue {argv:?}"
        );
    }
}

#[test]
fn direct_node_vitest_launcher_feedback_agrees_with_record_classification() {
    let launcher = "node scripts/test-launcher.mjs focused --input commit -- node ui/node_modules/vitest/vitest.mjs run --root ui --project component src/transcript-activity-slot.test.tsx";
    let check = engram_check_command(launcher).expect("the supported runner is recognized");
    assert_eq!(check.kind, EngramVerificationKind::Test);
    assert_eq!(engram_unsupported_focused_line(launcher), None);
    for unsupported in [
        launcher.replace(" run ", " watch "),
        format!("{launcher} --coverage"),
        format!("{launcher} --passWithNoTests"),
    ] {
        assert!(engram_check_command(&unsupported).is_none());
        let notice = engram_unsupported_focused_line(&unsupported).unwrap();
        assert!(
            notice.contains("Unsupported focused test-runner form"),
            "{notice}"
        );
        assert!(!notice.contains("no rerun is needed"), "{notice}");
    }
}

#[test]
fn direct_node_vitest_selection_and_reporting_options_preserve_test_classification() {
    for (option, value) in [
        ("--root", "ui"),
        ("--project", "component"),
        ("--config", "ui/vite.config.ts"),
        ("-c", "ui/vite.config.ts"),
        ("--testNamePattern", "working or stopping"),
        ("-t", "working or stopping"),
        ("--reporter", "json"),
        ("--outputFile", ".tmp/vitest-result.json"),
        ("--maxWorkers", "1"),
        ("--testTimeout", "5000"),
    ] {
        for options in [
            vec![option.to_owned(), value.to_owned()],
            vec![format!("{option}={value}")],
        ] {
            let mut argv: Vec<String> = ["node", "ui/node_modules/vitest/vitest.mjs", "run"]
                .into_iter()
                .map(str::to_owned)
                .collect();
            argv.extend(options);
            argv.push("src/transcript-activity-slot.test.tsx".to_owned());
            assert_eq!(
                engram_verification_record_check_kind(
                    EngramVerificationRecordMode::Focused,
                    "focused",
                    &argv,
                    &[]
                ),
                Some(EngramVerificationRecordCheckKind::Test),
                "{argv:?}"
            );
        }
    }
}
