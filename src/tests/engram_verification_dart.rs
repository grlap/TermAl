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

fn commit_dart_worktree_fixture(main: &FsPath) {
    run_git_test_command(
        main,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "--allow-empty",
            "-m",
            "test fixture",
        ],
    );
}

fn dart_named_root(main: &FsPath, linked: &FsPath, session: &str) -> EngramWorkSourceRoot {
    let binding = binding();
    EngramWorkSourceRoot {
        store: store(),
        work_id: binding.work_id,
        short_ref: "dart-receipt".to_owned(),
        claim_id: binding.claim_id,
        claim_fence: binding.claim_fence,
        root: fs::canonicalize(linked)
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        common_dir_key: engram_exact_path_key(&fs::canonicalize(main.join(".git")).unwrap()),
        named_by_session: session.to_owned(),
        named_at: test_run_timestamp(),
        generation: 3,
    }
}

#[test]
fn focused_dart_receipt_settlement_respects_original_worktree_and_shell_boundaries() {
    let tags = "smoke && io";
    for (late_description, requested_tags, next_command, should_attach) in [
        (false, tags, "", true),
        (true, tags, "", true),
        (false, tags, " && echo other", false),
        (false, "different", "", false),
    ] {
        let mut fixture = RunFixture::new("verification-focused-dart-settle");
        let (session, _) = engram_session(&fixture, false);
        let main = fixture.root.clone();
        let linked = main.with_file_name("verification-dart-linked");
        commit_dart_worktree_fixture(&main);
        run_git_test_command(
            &main,
            &["worktree", "add", "--detach", linked.to_str().unwrap()],
        );
        let named = dart_named_root(&main, &linked, &session);
        let (runtime, _runtime_rx) = test_codex_runtime_handle("dart-launch-runtime");
        {
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
            inner.sessions[index].engram.work_binding = Some(binding());
            inner.engram_work_source_roots.push(named.clone());
        }
        // The provider's configured workdir stays at main. The invocation and
        // its launcher artifacts belong to the separate linked worktree.
        fixture.root = linked;
        let sdk = main
            .join("sdk tools")
            .join(if cfg!(windows) { "dart.exe" } else { "dart" });
        let sdk = test_run_display_path(&sdk);
        let command = format!(
            r#"bash -lc "node scripts/test-launcher.mjs focused --input commit -- '{sdk}' test --tags '{tags}'{next_command}""#,
        );
        assert!(engram_check_command(&command).is_none());
        let provenance = EngramObservationProvenance::Ambient;
        let key = "dart-launch";
        let worktree = test_run_display_path(&fixture.root);
        fixture.state.note_engram_command_started(
            &session,
            &provenance,
            key,
            (!late_description).then_some(&command),
            Some(&worktree),
        );
        let mut run = dart_run_for_session(
            &fixture,
            &session,
            &[&sdk, "test", "--tags", requested_tags],
            &[
                &sdk,
                "test",
                "--tags",
                requested_tags,
                "--file-reporter",
                OWNED_REPORT_ARG,
            ],
            Some(include_bytes!("fixtures/declared-dart/dart-pass.jsonl")),
            0,
        );
        run.registration.named_root.as_mut().unwrap().workspace_id = named.root.clone();
        register(&fixture, &[&run]);
        if late_description {
            let successor_root = fs::canonicalize(&main)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let mut inner = fixture.state.inner.lock().unwrap();
            inner.engram_work_source_roots[0].root = successor_root;
            inner.engram_work_source_roots[0].generation = 4;
        }
        fixture.state.note_engram_command_finished(
            &session,
            &provenance,
            key,
            &command,
            &format!("RUN {}", test_run_display_path(&run.run_dir)),
            Some(EngramCommandExit::Code(0)),
        );

        let execution = {
            let inner = fixture.state.inner.lock().unwrap();
            assert!(
                inner
                    .test_run_observations
                    .values()
                    .all(|row| row.evidence.is_empty())
            );
            inner
                .test_run_observations
                .values()
                .find(|row| engram_verification_record_row_matches(row, &run.registration))
                .map(|row| {
                    assert_eq!(row.source_root.as_ref(), Some(&named));
                    assert_eq!(row.summary.worktree, worktree);
                    row.execution_id.clone()
                })
        };
        assert_eq!(
            execution.is_some(),
            should_attach,
            "{command}, recorded tags {requested_tags}"
        );
        let frozen = ready(&run.registration);
        let receipt: EngramVerificationRecordReceipt = serde_json::from_value(json!({
            "operation": PRODUCER,
            "producer": PRODUCER,
            "verifications": [{ "stage": "focused", "record_id": record_id(0) }],
            "run_outcome": "passed",
        }))
        .unwrap();
        let settlement = fixture.state.complete_engram_verification_record(
            &run.registration,
            &frozen.body,
            &receipt,
        );
        if let Some(execution) = execution {
            assert_eq!(settlement, EngramVerificationRecordSettlement::Settled);
            assert_eq!(row_records(&fixture, &execution), [Some(record_id(0))]);
            assert!(matches!(
                current(&fixture, &run).verification_record,
                Some(TestRunVerificationAck::Settled { .. }),
            ));
        } else {
            assert_eq!(settlement, EngramVerificationRecordSettlement::NoLaunchRow);
            assert!(matches!(
                current(&fixture, &run).verification_record,
                Some(TestRunVerificationAck::Received { .. }),
            ));
        }
    }
}

#[test]
fn focused_detached_dart_cannot_send_a_passing_report_as_test_credit() {
    let fixture = RunFixture::new("verification-focused-dart-detached");
    let mut run = dart_run(
        &fixture,
        &["dart", "test"],
        &["dart", "test", "--file-reporter", OWNED_REPORT_ARG],
        Some(include_bytes!("fixtures/declared-dart/dart-pass.jsonl")),
        0,
    );
    let request_path = run.run_dir.join("request.json");
    let mut request: Value = serde_json::from_slice(&fs::read(&request_path).unwrap()).unwrap();
    request["detached"] = json!(true);
    request["notifyTo"] = json!("coordinator");
    fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();
    run.registration.request_identity = test_run_registration_identity(&request).unwrap();
    run.registration.detached = Some(true);
    assert_eq!(
        pending_of(&run.registration),
        EngramVerificationRecordPending::UnclassifiedStages {
            stages: vec!["focused".to_owned()],
        }
    );
}

#[test]
fn focused_dart_does_not_credit_positional_paths() {
    let fixture = RunFixture::new("verification-focused-dart-positional");
    for selector in [
        "test/widgets",
        "test/widget_test.dart",
        "../other-suite",
        "file:///outside/suite_test.dart",
    ] {
        let run = dart_run(
            &fixture,
            &["dart", "test", selector],
            &[
                "dart",
                "test",
                selector,
                "--file-reporter",
                OWNED_REPORT_ARG,
            ],
            Some(include_bytes!("fixtures/declared-dart/dart-pass.jsonl")),
            0,
        );
        assert_eq!(
            pending_of(&run.registration),
            EngramVerificationRecordPending::UnclassifiedStages {
                stages: vec!["focused".to_owned()],
            },
            "{selector}"
        );
        let line = format!(
            "node scripts/test-launcher.mjs focused --input commit -- dart test {selector}"
        );
        let notice = engram_unsupported_focused_line(&line).expect("a supported-form diagnostic");
        assert!(
            notice.contains("omit --file-reporter and positional paths"),
            "{notice}"
        );
    }
}

#[cfg(unix)]
#[test]
fn focused_dart_rejects_distinct_worktrees_with_the_same_display_key() {
    let mut fixture = RunFixture::new("verification-focused-dart-root-collision");
    let main = fixture.root.clone();
    commit_dart_worktree_fixture(&main);
    let named_path = main.with_file_name("dart-named").join("checkout");
    let other = main.with_file_name("dart-named\\checkout");
    for path in [&named_path, &other] {
        run_git_test_command(
            &main,
            &["worktree", "add", "--detach", path.to_str().unwrap()],
        );
    }
    let named = dart_named_root(&main, &named_path, SESSION);
    assert_eq!(
        engram_exact_path_key(&named_path),
        engram_exact_path_key(&other)
    );
    let command = engram_observed_dart_launcher(
        "node scripts/test-launcher.mjs focused --input commit -- dart test",
        &EngramObservationProvenance::Ambient,
    )
    .unwrap();
    let target = engram_dart_observation_worktree(
        &command,
        &main,
        Some(&[Some(other.to_str().unwrap().to_owned())]),
        Some(&named),
    );
    assert!(target.is_none());

    fixture.root = other;
    let mut run = dart_run(
        &fixture,
        &["dart", "test"],
        &["dart", "test", "--file-reporter", OWNED_REPORT_ARG],
        Some(include_bytes!("fixtures/declared-dart/dart-pass.jsonl")),
        0,
    );
    run.registration.named_root.as_mut().unwrap().workspace_id = named.root;
    assert_eq!(
        pending_of(&run.registration),
        EngramVerificationRecordPending::UnclassifiedStages {
            stages: vec!["focused".to_owned()],
        }
    );
}

#[cfg(unix)]
#[test]
fn focused_dart_does_not_credit_renamed_unix_executables() {
    let fixture = RunFixture::new("verification-focused-dart-renamed");
    for name in ["renamed\\dart", "DART"] {
        let runner = fixture.root.join(name);
        let runner = runner.to_str().unwrap();
        let run = dart_run(
            &fixture,
            &[runner, "test"],
            &[runner, "test", "--file-reporter", OWNED_REPORT_ARG],
            Some(include_bytes!("fixtures/declared-dart/dart-pass.jsonl")),
            0,
        );
        assert_eq!(
            pending_of(&run.registration),
            EngramVerificationRecordPending::UnclassifiedStages {
                stages: vec!["focused".to_owned()],
            },
            "{runner}"
        );
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

#[cfg(windows)]
#[test]
fn focused_dart_retries_a_sharing_violation_before_freezing_or_reading_back() {
    use std::os::windows::fs::OpenOptionsExt;

    use EngramVerificationRecordPrepared as Prepared;
    let fixture = RunFixture::new("verification-focused-dart-sharing");
    let bytes = include_bytes!("fixtures/declared-dart/dart-pass.jsonl");
    let mut run = dart_run(
        &fixture,
        &["dart", "test"],
        &["dart", "test", "--file-reporter", OWNED_REPORT_ARG],
        Some(bytes),
        0,
    );
    let report = run.run_dir.join(ENGRAM_LAUNCHER_DART_REPORT);
    let body_path = run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE);
    let hold_report = || {
        fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&report)
            .unwrap()
    };
    let exclusive = hold_report();
    assert!(matches!(
        engram_verification_record_prepare(&run.registration),
        Prepared::Waiting
    ));
    assert!(!body_path.exists());
    drop(exclusive);

    let frozen = ready(&run.registration);
    assert_eq!(
        frozen.body.checks[0].outcome,
        EngramVerificationRecordCheckOutcome::Passed
    );
    let original = fs::read(&body_path).unwrap();
    let receipt: EngramVerificationRecordReceipt = serde_json::from_value(json!({
        "operation": PRODUCER,
        "producer": PRODUCER,
        "verifications": [{ "stage": "focused", "record_id": record_id(0) }],
        "run_outcome": "passed",
    }))
    .unwrap();
    run.registration.verification_record = Some(TestRunVerificationAck::Received {
        body_digest: engram_verification_record_body_digest(&frozen.body),
        receipt: receipt.clone(),
    });
    let exclusive = hold_report();
    assert!(matches!(
        engram_verification_record_prepare(&run.registration),
        Prepared::Waiting
    ));
    drop(exclusive);

    let Prepared::ReceiptHeld {
        frozen: rebuilt,
        receipt: held,
    } = engram_verification_record_prepare(&run.registration)
    else {
        panic!("the receipt can be read back once the report is available again");
    };
    assert_eq!(held, receipt);
    assert_eq!(rebuilt.body, frozen.body);
    assert_eq!(fs::read(&body_path).unwrap(), original);
    assert_eq!(fs::read(&report).unwrap(), bytes.as_slice());
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
    let valid = ["flutter", "test", "--tags", "widgets"];
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
            "--tags",
            "widgets",
            "--file-reporter",
            "json:../other-run/focused.dart.json",
        ],
        vec![
            "flutter",
            "test",
            "--tags",
            "widgets",
            "--file-reporter",
            "json:.tmp/shared.json",
        ],
        vec![
            "flutter",
            "test",
            "--file-reporter",
            OWNED_REPORT_ARG,
            "--tags",
            "widgets",
        ],
        vec![
            "flutter",
            "test",
            "--tags",
            "other_widgets",
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
        "test/widgets",
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
