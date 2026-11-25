use super::*;

const OWNED_REPORT_ARG: &str = "<owned Dart reporter>";

fn dart_run(
    fixture: &RunFixture,
    planned: &[&str],
    observed: &[&str],
    report: Option<&[u8]>,
    exit: i64,
) -> RecordRun {
    dart_run_for_session(fixture, SESSION, planned, observed, report, exit)
}

fn dart_run_for_session(
    fixture: &RunFixture,
    session: &str,
    planned: &[&str],
    observed: &[&str],
    report: Option<&[u8]>,
    exit: i64,
) -> RecordRun {
    let run_id = format!("test-{}", Uuid::new_v4());
    let directory = fixture.runs_dir().join(&run_id);
    fs::create_dir_all(&directory).unwrap();
    let owned = format!(
        "json:{}",
        test_run_display_path(
            &fs::canonicalize(&directory)
                .unwrap()
                .join(ENGRAM_LAUNCHER_DART_REPORT)
        )
    );
    let observed = observed
        .iter()
        .map(|arg| {
            if *arg == OWNED_REPORT_ARG {
                owned.as_str()
            } else {
                *arg
            }
        })
        .collect::<Vec<_>>();
    let mut request = commit_request(&fixture.root);
    request["full"] = json!(false);
    request["owner"] = json!(session);
    let started = test_run_timestamp();
    request["started"] = json!(started);
    request["stages"] = json!([{"name":"focused", "command":planned[0], "args":&planned[1..]}]);
    let mut results = commit_results();
    // Exit 0 and a console count must not turn a failing or incomplete
    // reporter into a passed verification.
    let state = if exit == 0 { "passed" } else { "failed" };
    results["state"] = json!(state);
    results["exitCode"] = json!(exit);
    results["started"] = json!(started);
    let ended = test_run_timestamp();
    results["ended"] = json!(ended);
    results["input"]["eligible"] = json!(exit == 0);
    let mut observed_stage = stage("focused", state, exit, &observed, 0);
    observed_stage["started"] = json!(started);
    observed_stage["ended"] = json!(ended);
    observed_stage["cwd"] = json!(test_run_display_path(&fixture.root));
    observed_stage["tests"] = json!({"runner":"cargo-libtest","passed":123,"failed":0,"ignored":0});
    if let Some(bytes) = report {
        observed_stage["dartReportDigest"] = json!(file_content_hash(bytes));
    }
    results["stages"] = json!([observed_stage]);
    let mut run = record_run(fixture, &run_id, request, results);
    initiated_by(&mut run, session);
    run.registration.started_at = Some(started.clone());
    run.registration.registered_at = started;
    run.registration.preset = TestRunPreset::Focused;
    run.registration.stage_names = vec!["focused".to_owned()];
    if let Some(bytes) = report {
        fs::write(run.run_dir.join(ENGRAM_LAUNCHER_DART_REPORT), bytes).unwrap();
    }
    run
}

#[test]
fn focused_dart_receipt_settles_onto_its_observed_foreground_launch() {
    let command = "node scripts/test-launcher.mjs focused --input commit -- dart test";
    assert!(engram_check_command(command).is_none());
    for late_description in [false, true] {
        let fixture = RunFixture::new("verification-focused-dart-settle");
        let (session, _) = engram_session(&fixture, false);
        let (runtime, _runtime_rx) = test_codex_runtime_handle("dart-launch-runtime");
        {
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
        }
        bound_by_naming(&fixture, &session);
        let provenance = EngramObservationProvenance::Ambient;
        let key = "dart-launch";
        let worktree = test_run_display_path(&fixture.root);
        fixture.state.note_engram_command_started(
            &session,
            &provenance,
            key,
            (!late_description).then_some(command),
            Some(&worktree),
        );
        let run = dart_run_for_session(
            &fixture,
            &session,
            &["dart", "test"],
            &["dart", "test", "--file-reporter", OWNED_REPORT_ARG],
            Some(include_bytes!("fixtures/declared-dart/dart-pass.jsonl")),
            0,
        );
        register(&fixture, &[&run]);
        fixture.state.note_engram_command_finished(
            &session,
            &provenance,
            key,
            command,
            &format!("RUN {}", test_run_display_path(&run.run_dir)),
            Some(EngramCommandExit::Code(0)),
        );

        let execution = {
            let inner = fixture.state.inner.lock().unwrap();
            assert_eq!(inner.test_run_observations.len(), 1);
            let row = inner.test_run_observations.values().next().unwrap();
            assert!(engram_verification_record_row_matches(
                row,
                &run.registration
            ));
            assert!(
                row.evidence.is_empty(),
                "the runtime exit cannot grant credit"
            );
            row.execution_id.clone()
        };
        let frozen = ready(&run.registration);
        let receipt: EngramVerificationRecordReceipt = serde_json::from_value(json!({
            "operation": PRODUCER,
            "producer": PRODUCER,
            "verifications": [{ "stage": "focused", "record_id": record_id(0) }],
            "run_outcome": "passed",
        }))
        .unwrap();
        assert_eq!(
            fixture.state.complete_engram_verification_record(
                &run.registration,
                &frozen.body,
                &receipt,
            ),
            EngramVerificationRecordSettlement::Settled
        );
        assert_eq!(row_records(&fixture, &execution), [Some(record_id(0))]);
        assert!(matches!(
            current(&fixture, &run).verification_record,
            Some(TestRunVerificationAck::Settled { .. }),
        ));
    }
}

#[cfg(unix)]
#[test]
fn focused_dart_does_not_wait_for_a_replacement_fifo() {
    use std::os::unix::ffi::OsStrExt;

    let fixture = RunFixture::new("verification-focused-dart-fifo");
    let run = dart_run(
        &fixture,
        &["dart", "test"],
        &["dart", "test", "--file-reporter", OWNED_REPORT_ARG],
        Some(include_bytes!("fixtures/declared-dart/dart-pass.jsonl")),
        0,
    );
    let path = run.run_dir.join(ENGRAM_LAUNCHER_DART_REPORT);
    fs::remove_file(&path).unwrap();
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the NUL-terminated path remains live for the call.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || send.send(ready(&run.registration).body).unwrap());
    let body = receive
        .recv_timeout(Duration::from_secs(5))
        .expect("verification must return without a writer opening the FIFO");
    assert_eq!(
        body.checks[0].outcome,
        EngramVerificationRecordCheckOutcome::Indeterminate
    );
    assert!(body.checks[0].counts.is_none());
}

#[test]
fn focused_flutter_and_dart_verify_the_existing_reporter_verdicts() {
    use EngramVerificationRecordCheckOutcome as Check;
    use EngramVerificationRecordRunOutcome as Run;
    for (runner, resolved, artifacts) in [
        (
            "flutter",
            if cfg!(windows) {
                r"C:\SDK tools\flutter.bat"
            } else {
                "/sdk/bin/flutter"
            },
            [
                &include_bytes!("fixtures/declared-dart/flutter-pass.jsonl")[..],
                &include_bytes!("fixtures/declared-dart/flutter-fail.jsonl")[..],
                &include_bytes!("fixtures/declared-dart/flutter-skipped.jsonl")[..],
                &include_bytes!("fixtures/declared-dart/flutter-truncated.jsonl")[..],
            ],
        ),
        (
            "dart",
            if cfg!(windows) {
                r"C:\SDK tools\dart.exe"
            } else {
                "/sdk/bin/dart"
            },
            [
                &include_bytes!("fixtures/declared-dart/dart-pass.jsonl")[..],
                &include_bytes!("fixtures/declared-dart/dart-fail.jsonl")[..],
                &include_bytes!("fixtures/declared-dart/dart-skipped.jsonl")[..],
                &include_bytes!("fixtures/declared-dart/dart-truncated.jsonl")[..],
            ],
        ),
    ] {
        for (bytes, expected, run_expected) in [
            (artifacts[0], Check::Passed, Run::Passed),
            (artifacts[1], Check::Failed, Run::Failed),
            (artifacts[2], Check::Indeterminate, Run::Indeterminate),
            (artifacts[3], Check::Indeterminate, Run::Indeterminate),
        ] {
            let fixture = RunFixture::new("verification-focused-dart-artifact");
            let mut planned = vec![runner, "test"];
            if runner == "flutter" {
                planned.extend(["--no-pub", "--exclude-tags", "supabase-local"]);
            }
            planned.extend([
                "test/integration",
                "-t",
                "integration",
                "--run-skipped",
                "--concurrency=1",
                "--plain-name",
                "name fragment",
            ]);
            let mut observed = planned.clone();
            observed[0] = resolved;
            observed.extend(["--file-reporter", OWNED_REPORT_ARG]);
            let run = dart_run(&fixture, &planned, &observed, Some(bytes), 0);
            let record = ready(&run.registration);
            let [check] = record.body.checks.as_slice() else {
                panic!("one focused stage")
            };
            assert_eq!(check.check_kind, EngramVerificationRecordCheckKind::Test);
            assert_eq!(check.outcome, expected, "{runner}");
            assert_eq!(record.body.run.outcome, run_expected, "{runner}");
            assert!(!check.command.contains("--file-reporter"));
            assert!(!check.command.contains(ENGRAM_LAUNCHER_DART_REPORT));
            assert!(
                check
                    .refs
                    .contains(&format!("artifact_digest:{}", file_content_hash(bytes)))
            );
            assert_ne!(
                check
                    .counts
                    .as_ref()
                    .and_then(|counts| counts.get("passed")),
                Some(&123)
            );
        }
    }
}

#[test]
fn focused_dart_never_adopts_a_later_or_replaced_reporter() {
    use EngramVerificationRecordCheckOutcome::Indeterminate;
    let planned = ["dart", "test"];
    let observed = ["dart", "test", "--file-reporter", OWNED_REPORT_ARG];
    let passing = include_bytes!("fixtures/declared-dart/dart-pass.jsonl");
    let failing = include_bytes!("fixtures/declared-dart/dart-fail.jsonl");
    for original in [None, Some(failing.as_slice())] {
        let fixture = RunFixture::new("verification-focused-dart-replaced");
        let run = dart_run(&fixture, &planned, &observed, original, 0);
        if original.is_none() {
            let body = ready(&run.registration).body;
            assert_eq!(body.checks[0].outcome, Indeterminate);
        }
        fs::write(run.run_dir.join(ENGRAM_LAUNCHER_DART_REPORT), passing).unwrap();
        let body = ready(&run.registration).body;
        assert_eq!(body.checks[0].outcome, Indeterminate);
        assert!(body.checks[0].counts.is_none());
    }
}

#[test]
fn focused_dart_nonzero_exit_fails_with_or_without_a_passing_report() {
    use EngramVerificationRecordCheckOutcome as Check;
    use EngramVerificationRecordRunOutcome as Run;
    let planned = ["dart", "test"];
    let observed = ["dart", "test", "--file-reporter", OWNED_REPORT_ARG];
    let passing = include_bytes!("fixtures/declared-dart/dart-pass.jsonl");
    for report in [Some(passing.as_slice()), None] {
        let fixture = RunFixture::new("verification-focused-dart-nonzero");
        let run = dart_run(&fixture, &planned, &observed, report, 1);
        let body = ready(&run.registration).body;
        assert_eq!(body.run.outcome, Run::Failed);
        assert_eq!(body.checks[0].outcome, Check::Failed);
        assert!(body.checks[0].refs.contains(&"exit:1".to_owned()));
        assert!(body.checks[0].counts.is_none());
    }
}

#[test]
fn focused_dart_binds_explicit_sdk_paths_to_the_effective_worktree() {
    let passing = include_bytes!("fixtures/declared-dart/flutter-pass.jsonl");
    let valid_runner = if cfg!(windows) {
        "flutter.bat"
    } else {
        "flutter"
    };
    for relative in [false, true] {
        let fixture = RunFixture::new("verification-dart-sdk-path-valid");
        let planned_path = if relative {
            "./sdk/../sdk/flutter".to_owned()
        } else {
            test_run_display_path(&fixture.root.join("sdk").join("flutter"))
        };
        let observed_path = test_run_display_path(&fixture.root.join("sdk").join(valid_runner));
        let run = dart_run(
            &fixture,
            &[&planned_path, "test"],
            &[&observed_path, "test", "--file-reporter", OWNED_REPORT_ARG],
            Some(passing),
            0,
        );
        assert_eq!(
            ready(&run.registration).body.checks[0].outcome,
            EngramVerificationRecordCheckOutcome::Passed
        );
    }

    for wrong_cwd in [false, true] {
        let fixture = RunFixture::new("verification-dart-sdk-path-refused");
        let directory = if wrong_cwd { "sdk" } else { "other-sdk" };
        let observed = test_run_display_path(&fixture.root.join(directory).join(valid_runner));
        let argv = [&*observed, "test", "--file-reporter", OWNED_REPORT_ARG];
        let run = dart_run(
            &fixture,
            &["./sdk/flutter", "test"],
            &argv,
            Some(passing),
            0,
        );
        if wrong_cwd {
            let other = fixture.root.join("different");
            fs::create_dir(&other).unwrap();
            let results_path = run.run_dir.join("results.json");
            let mut results: Value =
                serde_json::from_slice(&fs::read(&results_path).unwrap()).unwrap();
            results["stages"][0]["cwd"] = json!(test_run_display_path(&other));
            fs::write(&results_path, serde_json::to_vec(&results).unwrap()).unwrap();
        }
        assert_eq!(
            pending_of(&run.registration),
            EngramVerificationRecordPending::UnclassifiedStages {
                stages: vec!["focused".to_owned()]
            },
        );
    }
}

#[test]
fn focused_dart_rejects_caller_reporters_unsupported_modes_and_changed_stage_argv() {
    let valid = ["flutter", "test", "test/widgets"];
    for argv in [
        vec!["dart", "test", "--watch"],
        vec!["dart", "test", "--unknown"],
        vec!["dart", "test", "--file-reporter", "json:.tmp/user.json"],
        vec!["flutter", "test", "--file-reporter=json:.tmp/user.json"],
        vec!["dart.cmd", "test"],
        vec!["dart-shim", "test"],
        vec!["bash", "-c", "dart test"],
    ] {
        let fixture = RunFixture::new("verification-focused-dart-rejected");
        let run = dart_run(&fixture, &argv, &argv, None, 0);
        assert_eq!(
            pending_of(&run.registration),
            EngramVerificationRecordPending::UnclassifiedStages {
                stages: vec!["focused".to_owned()],
            },
            "{argv:?}"
        );
    }
    for observed in [
        vec!["cargo", "test"],
        valid.to_vec(),
        vec![
            "flutter",
            "test",
            "test/widgets",
            "--file-reporter",
            "json:../other-run/focused.dart.json",
        ],
        vec![
            "flutter",
            "test",
            "test/widgets",
            "--file-reporter",
            "json:.tmp/shared.json",
        ],
        vec![
            "flutter",
            "test",
            "--file-reporter",
            OWNED_REPORT_ARG,
            "test/widgets",
        ],
        vec![
            "flutter",
            "test",
            "test/other_widgets",
            "--file-reporter",
            OWNED_REPORT_ARG,
        ],
    ] {
        let fixture = RunFixture::new("verification-focused-dart-changed-argv");
        let run = dart_run(&fixture, &valid, &observed, None, 0);
        assert_eq!(
            pending_of(&run.registration),
            EngramVerificationRecordPending::UnclassifiedStages {
                stages: vec!["focused".to_owned()],
            },
        );
    }
}

#[test]
fn focused_dart_launcher_notice_names_the_supported_form() {
    let supported =
        "node scripts/test-launcher.mjs focused --input commit -- flutter test --no-pub";
    assert!(engram_unsupported_focused_line(supported).is_none());
    for suffix in [
        "--file-reporter json:.tmp/user.json",
        "--watch",
        "--unknown",
    ] {
        let line = format!("{supported} {suffix}");
        let notice = engram_unsupported_focused_line(&line).unwrap();
        assert!(notice.contains("no Test record will follow"), "{notice}");
        assert!(
            notice.contains(supported.trim_end_matches(" --no-pub")),
            "{notice}"
        );
        assert!(notice.contains("omit --file-reporter"), "{notice}");
    }
}
