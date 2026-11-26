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

fn mark_external_dart_run(run: &mut RecordRun, script: &FsPath, clean: Option<bool>) {
    let path = run.run_dir.join("request.json");
    let mut request: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    request["detached"] = json!(false);
    request["externalLauncher"] = json!({
        "script": test_run_display_path(script), "commit": "a".repeat(40), "clean": clean,
    });
    fs::write(path, request.to_string()).unwrap();
    run.registration.external_launcher = engram_external_launcher_request(&request).unwrap();
    assert!(run.registration.external_launcher.is_some());
    run.registration.request_identity = test_run_registration_identity(&request).unwrap();
    run.registration.locator_identity = test_run_locator_identity(&run.run_dir);
}

#[test]
fn external_dart_producer_requires_the_original_launch_and_terminal() {
    for (style, should_send) in [
        ("immediate", true),
        ("late", true),
        ("command then cwd", true),
        ("cwd then command", true),
        ("wrong cwd", false),
        ("wrong original cwd", false),
        ("wrong description key", false),
        ("wrong project root", false),
        ("different launcher script", false),
        ("summary only", false),
        ("started only", false),
        ("rewritten terminal", false),
        ("rewritten before resend", false),
        ("rewritten with held receipt", false),
        ("terminal changes during send", true),
        ("reporter changes during send", true),
        ("extra result stage", false),
        ("changed request", false),
        ("changed stage argv", false),
        ("changed reporter", false),
        ("dirty launcher", false),
        ("unknown launcher cleanliness", false),
    ] {
        let fixture = RunFixture::new("external-dart-custody");
        let (session, transport) = engram_session(&fixture, false);
        let (runtime, _rx) = test_codex_runtime_handle("external-dart-runtime");
        let named = dart_named_root(&fixture.root, &fixture.root, &session);
        {
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
            inner.sessions[index].engram.work_binding = Some(binding());
            inner.engram_work_source_roots.push(named.clone());
        }
        let launcher = fixture
            .root
            .with_file_name("launcher package")
            .join("scripts/test-launcher.mjs");
        fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        fs::write(&launcher, "").unwrap();
        let root = test_run_display_path(&fixture.root);
        let other = fixture.root.join("nested cwd");
        fs::create_dir(&other).unwrap();
        let wrong_cwd = test_run_display_path(&other);
        let selected = if style == "wrong project root" {
            let other_project = fixture.root.with_file_name("other project");
            fs::create_dir(&other_project).unwrap();
            run_git_test_command(&other_project, &["init"]);
            test_run_display_path(&other_project)
        } else {
            root.clone()
        };
        let command = format!(
            r#"node "{}" focused --input commit --project-root "{selected}" -- dart test"#,
            test_run_display_path(&launcher)
        );
        let provenance = EngramObservationProvenance::Ambient;
        let key = "external-dart-launch";
        let commandless = matches!(
            style,
            "command then cwd" | "cwd then command" | "wrong original cwd"
        );
        fixture.state.note_engram_command_started(
            &session,
            &provenance,
            key,
            (!commandless).then_some(command.as_str()),
            match style {
                "late" | "command then cwd" | "cwd then command" | "wrong description key" => None,
                "wrong cwd" | "wrong original cwd" => Some(wrong_cwd.as_str()),
                _ => Some(root.as_str()),
            },
        );
        if style == "late" {
            fixture.state.note_engram_command_started(
                &session,
                &provenance,
                key,
                Some(&command),
                Some(&root),
            );
        }
        let describe = |description_key, line, cwd| {
            fixture.state.note_engram_command_described(
                &session,
                &provenance,
                description_key,
                line,
                cwd,
            );
        };
        match style {
            "command then cwd" => {
                describe(key, Some(command.as_str()), None);
                describe(key, None, Some(root.as_str()));
            }
            "cwd then command" => {
                describe(key, None, Some(root.as_str()));
                describe(key, Some(command.as_str()), None);
            }
            "wrong original cwd" => describe(key, Some(command.as_str()), Some(root.as_str())),
            "wrong description key" => describe("different invocation", None, Some(root.as_str())),
            _ => {}
        }
        let mut run = dart_run_for_session(
            &fixture,
            &session,
            &["dart", "test"],
            &["dart", "test", "--file-reporter", OWNED_REPORT_ARG],
            Some(include_bytes!("fixtures/declared-dart/dart-pass.jsonl")),
            0,
        );
        run.registration.named_root.as_mut().unwrap().workspace_id = named.root.clone();
        let registered_launcher = if style == "different launcher script" {
            let other_launcher = fixture
                .root
                .with_file_name("other launcher")
                .join("scripts/test-launcher.mjs");
            fs::create_dir_all(other_launcher.parent().unwrap()).unwrap();
            fs::write(&other_launcher, "").unwrap();
            other_launcher
        } else {
            launcher
        };
        let clean = match style {
            "dirty launcher" => Some(false),
            "unknown launcher cleanliness" => None,
            _ => Some(true),
        };
        mark_external_dart_run(&mut run, &registered_launcher, clean);
        register(&fixture, &[&run]);
        fixture.state.engram_verification_records_tick();
        assert!(transport.sent_records().is_empty());
        assert_eq!(
            engram_verification_record_progress(&run.run_dir),
            Some(EngramVerificationRecordProgress::Pending {
                reason: EngramVerificationRecordPending::ExternalLaunchCustodyMissing,
            })
        );
        let results_path = run.run_dir.join("results.json");
        let terminal = fs::read(&results_path).unwrap();
        let write_results = |edit: fn(&mut Value)| {
            let mut results: Value = serde_json::from_slice(&terminal).unwrap();
            edit(&mut results);
            fs::write(&results_path, results.to_string()).unwrap();
        };
        if style == "extra result stage" {
            write_results(|results| {
                let mut extra = stage("extra", "passed", 0, &["cargo", "check"], 1);
                extra["started"] = results["stages"][0]["started"].clone();
                extra["ended"] = results["stages"][0]["ended"].clone();
                results["stages"].as_array_mut().unwrap().push(extra);
            });
        }
        if style == "changed stage argv" {
            write_results(|results| {
                results["stages"][0]["command"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!("--run-skipped"));
            });
        }
        let directory = test_run_display_path(&run.run_dir);
        let output = match style {
            "summary only" => format!("results: {directory}/results.json"),
            "started only" => format!("STARTED {directory}"),
            _ => format!("RUN {directory}\nresults: {directory}/results.json"),
        };
        fixture.state.note_engram_command_finished(
            &session,
            &provenance,
            key,
            &command,
            &output,
            Some(EngramCommandExit::Code(0)),
        );
        let response = json!({"operation": PRODUCER, "producer": PRODUCER,
            "verifications": [{ "stage": "focused", "record_id": record_id(0) }], "run_outcome": "passed"});
        if style == "extra result stage" {
            assert!(matches!(
                fixture.state.prepare_engram_verification_record(&current(&fixture, &run)),
                EngramVerificationRecordPrepared::Pending(EngramVerificationRecordPending::BodyNotBuilt { detail })
                    if detail.contains("exactly its registered focused Dart stage")
            ));
        }
        if style == "changed reporter" {
            fs::write(
                run.run_dir.join(ENGRAM_LAUNCHER_DART_REPORT),
                include_bytes!("fixtures/declared-dart/dart-fail.jsonl"),
            )
            .unwrap();
            let EngramVerificationRecordPrepared::Ready(frozen) = fixture
                .state
                .prepare_engram_verification_record(&current(&fixture, &run))
            else {
                panic!("the original terminal is still readable after the reporter changed");
            };
            assert_eq!(
                frozen.body.checks[0].outcome,
                EngramVerificationRecordCheckOutcome::Indeterminate
            );
            assert!(transport.sent_records().is_empty());
            continue;
        }
        if style.starts_with("rewritten") {
            assert!(
                fixture
                    .state
                    .inner
                    .lock()
                    .unwrap()
                    .test_run_observations
                    .values()
                    .any(|row| row
                        .artifact
                        .as_ref()
                        .is_some_and(|artifact| artifact.run_id == run.registration.run_id
                            && artifact.terminal_digest.is_some()))
            );
            if style != "rewritten terminal" {
                let EngramVerificationRecordPrepared::Ready(frozen) = fixture
                    .state
                    .prepare_engram_verification_record(&current(&fixture, &run))
                else {
                    panic!("the original terminal prepares a passing body");
                };
                if style == "rewritten with held receipt" {
                    fixture.state.note_engram_verification_record_ack(
                        &run.registration,
                        TestRunVerificationAck::Received {
                            receipt: serde_json::from_value(response.clone()).unwrap(),
                            body_digest: engram_verification_record_body_digest(&frozen.body),
                        },
                    );
                    assert!(matches!(
                        fixture
                            .state
                            .prepare_engram_verification_record(&current(&fixture, &run)),
                        EngramVerificationRecordPrepared::ReceiptHeld { .. }
                    ));
                }
            }
            // Body equality alone cannot detect a change to an unrelated
            // result field. The original terminal digest still must match.
            write_results(|results| results["replacementFixtureField"] = json!(true));
            assert!(
                matches!(
                    fixture
                        .state
                        .prepare_engram_verification_record(&current(&fixture, &run)),
                    EngramVerificationRecordPrepared::Pending(
                        EngramVerificationRecordPending::BodyMismatch
                    )
                ),
                "{style}"
            );
        }
        if style == "changed request" {
            let path = run.run_dir.join("request.json");
            let mut request: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            request["externalLauncher"]["commit"] = json!("b".repeat(40));
            fs::write(path, request.to_string()).unwrap();
        }
        let changed_during_send = match style {
            "terminal changes during send" => {
                let mut results: Value = serde_json::from_slice(&terminal).unwrap();
                results["replacementFixtureField"] = json!(true);
                Some((results_path.clone(), serde_json::to_vec(&results).unwrap()))
            }
            "reporter changes during send" => Some((
                run.run_dir.join(ENGRAM_LAUNCHER_DART_REPORT),
                include_bytes!("fixtures/declared-dart/dart-fail.jsonl").to_vec(),
            )),
            _ => None,
        };
        let restore = changed_during_send.map(|(path, changed)| {
            let original = fs::read(&path).unwrap();
            let during_send = path.clone();
            *transport.observe.lock().unwrap() = Some(Box::new(move |request| {
                if matches!(request, EngramControlRequest::VerificationRecord { .. }) {
                    fs::write(&during_send, &changed).unwrap();
                }
            }));
            (path, original)
        });
        transport.script(Ok(work_supported(&session)));
        transport.script(Ok(response));
        fixture.state.engram_verification_records_tick();
        assert_eq!(
            transport.sent_records().len(),
            usize::from(should_send),
            "{style}"
        );
        if let Some((path, original)) = restore {
            assert!(
                matches!(
                    current(&fixture, &run).verification_record,
                    Some(TestRunVerificationAck::Received { .. })
                ),
                "{style}"
            );
            assert!(
                fixture
                    .state
                    .inner
                    .lock()
                    .unwrap()
                    .test_run_observations
                    .values()
                    .all(|row| row.evidence.is_empty()),
                "{style}"
            );
            fs::write(path, original).unwrap();
            fixture.state.engram_verification_records_tick();
            assert_eq!(transport.sent_records().len(), 1, "{style}");
        }
        if should_send {
            let records = transport.sent_records();
            let refs = records[0]["checks"][0]["refs"].as_array().unwrap();
            assert!(refs.contains(&json!(format!("launcher_commit:{}", "a".repeat(40)))));
            assert!(refs.contains(&json!("launcher_clean:true")));
            assert!(matches!(
                current(&fixture, &run).verification_record,
                Some(TestRunVerificationAck::Settled { .. })
            ));
        } else if style == "rewritten with held receipt" {
            assert!(matches!(
                current(&fixture, &run).verification_record,
                Some(TestRunVerificationAck::Received { .. })
            ));
        } else if style == "changed stage argv" {
            assert!(matches!(
                current(&fixture, &run).verification_record,
                Some(TestRunVerificationAck::LocalUnclassified { detail })
                    if detail.contains("focused")
            ));
        } else {
            if clean != Some(true) {
                assert_eq!(
                    engram_verification_record_progress(&run.run_dir),
                    Some(EngramVerificationRecordProgress::Pending {
                        reason:
                            EngramVerificationRecordPending::ExternalLauncherProvenanceUnverified,
                    }),
                    "{style}"
                );
            }
            assert!(
                !matches!(
                    current(&fixture, &run).verification_record,
                    Some(
                        TestRunVerificationAck::Received { .. }
                            | TestRunVerificationAck::Settled { .. }
                    )
                ),
                "{style}"
            );
            assert_eq!(
                engram_verification_record_frozen(&run.run_dir, &run.registration.run_id).is_some(),
                style == "rewritten before resend",
                "{style}"
            );
        }
    }
}

#[test]
fn external_dart_acp_completion_uses_original_command_only_when_omitted() {
    for scenario in [
        "omitted",
        "same pushd directory",
        "different pushd directory",
        "matching pushd without original cwd",
        "different pushd without original cwd",
        "matching pushd described at end",
        "different pushd described at end",
        "matching cwd without command",
        "different cwd without command",
        "matching cwd with command",
        "different cwd with command",
        "matching shell echo",
        "different shell echo",
        "different reported command",
        "different request argv",
        "repeated end",
    ] {
        let fixture = RunFixture::new("external-dart-acp-completion");
        let (session, transport) = engram_session(&fixture, false);
        let (runtime, _rx) = test_acp_runtime_handle(AcpAgent::Cursor, "external-dart-acp-runtime");
        let named = dart_named_root(&fixture.root, &fixture.root, &session);
        {
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].runtime = SessionRuntime::Acp(runtime);
            inner.sessions[index].engram.work_binding = Some(binding());
            inner.engram_work_source_roots.push(named.clone());
        }
        let launcher = fixture
            .root
            .with_file_name("launcher package")
            .join("scripts/test-launcher.mjs");
        fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        fs::write(&launcher, "").unwrap();
        let cwd = test_run_display_path(&fixture.root);
        let other_directory = fixture.root.join("other directory");
        fs::create_dir(&other_directory).unwrap();
        let described_at_end = scenario.ends_with("described at end");
        let omitted_cwd = described_at_end || scenario.ends_with("without original cwd");
        if omitted_cwd {
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].session.workdir = test_run_display_path(&other_directory);
        }
        let command = format!(
            r#"node "{}" focused --input commit --project-root "{cwd}" -- dart test"#,
            test_run_display_path(&launcher),
        );
        let shell_echo = scenario.ends_with("shell echo");
        let command = if shell_echo {
            format!(r#"{command} --plain-name "group\\case""#)
        } else {
            command
        };
        let one_call = scenario.ends_with("pushd directory") || omitted_cwd;
        let started = if one_call {
            format!(r#"pushd "{cwd}" && {command}"#)
        } else if shell_echo {
            format!("bash -lc '{command}'")
        } else {
            command.clone()
        };
        let key = "external-acp-execution";
        let mut recorder = SessionRecorder::new(fixture.state.clone(), session.clone());
        let mut turn = AcpTurnState::default();
        let (input_tx, _input_rx) = std::sync::mpsc::channel();
        let mut apply = |update: Value| {
            handle_acp_session_update(
                &update,
                &fixture.state,
                &session,
                &input_tx,
                &mut turn,
                &mut recorder,
                AcpAgent::Cursor,
            )
            .unwrap();
        };
        apply(json!({"sessionUpdate":"tool_call", "toolCallId":key, "title":"Run test"}));
        let mut start_description = json!({"command":started});
        if one_call && !omitted_cwd {
            start_description["cwd"] = json!("other directory");
        }
        if !described_at_end {
            apply(json!({"sessionUpdate":"tool_call_update", "toolCallId":key,
                "status":"in_progress", "rawInput":start_description}));
        }
        if !one_call {
            apply(json!({"sessionUpdate":"tool_call_update", "toolCallId":key,
                "status":"in_progress", "rawInput":{"cwd":"."}}));
        }

        let different_argv = scenario == "different request argv";
        let planned: &[&str] = if shell_echo {
            &["dart", "test", "--plain-name", r"group\case"]
        } else if different_argv {
            &["dart", "test", "--run-skipped"]
        } else {
            &["dart", "test"]
        };
        let observed: Vec<_> = planned
            .iter()
            .copied()
            .chain(["--file-reporter", OWNED_REPORT_ARG])
            .collect();
        let mut run = dart_run_for_session(
            &fixture,
            &session,
            planned,
            &observed,
            Some(include_bytes!("fixtures/declared-dart/dart-pass.jsonl")),
            0,
        );
        run.registration.named_root.as_mut().unwrap().workspace_id = named.root;
        mark_external_dart_run(&mut run, &launcher, Some(true));
        register(&fixture, &[&run]);

        let directory = test_run_display_path(&run.run_dir);
        let mut end = json!({"sessionUpdate":"tool_call_update", "toolCallId":key,
        "status":"completed", "title":"Run test", "rawOutput":{
            "exitCode":0, "stdout":format!("RUN {directory}\nresults: {directory}/results.json")
        }});
        if one_call {
            let directory = if scenario == "different pushd directory" {
                test_run_display_path(&other_directory)
            } else {
                cwd.clone()
            };
            let end_cwd = if omitted_cwd && scenario.starts_with("different") {
                cwd.clone()
            } else {
                test_run_display_path(&other_directory)
            };
            end["rawInput"] = json!({
                "command":format!(r#"pushd "{directory}" && {command}"#),
                "cwd":end_cwd,
            });
        } else if scenario.contains(" cwd ") {
            let end_cwd = if scenario.starts_with("different") {
                test_run_display_path(&other_directory)
            } else {
                cwd.clone()
            };
            end["rawInput"] = json!({"cwd":end_cwd});
            if scenario.ends_with("with command") {
                end["rawInput"]["command"] = json!(command);
            }
        } else if shell_echo {
            let echo = if scenario == "different shell echo" {
                format!("pwsh -Command {command}")
            } else {
                started.clone()
            };
            let provenance = EngramObservationProvenance::Ambient;
            let parsed_start = engram_observed_dart_launcher(&started, &provenance).unwrap();
            let parsed_end = engram_observed_dart_launcher(&echo, &provenance).unwrap();
            assert_eq!(
                EngramHost::check_fingerprint(&parsed_start),
                EngramHost::check_fingerprint(&parsed_end)
            );
            assert_eq!(
                parsed_start.argv == parsed_end.argv,
                scenario == "matching shell echo"
            );
            end["rawInput"] = json!({"command":echo});
        } else if scenario == "different reported command" {
            end["rawInput"] = json!({"command":format!("{command} --run-skipped")});
        } else if scenario == "repeated end" {
            let mut first = end.clone();
            first["rawOutput"]["stdout"] = json!("completed without a locator");
            apply(first);
        }
        apply(end);
        transport.script(Ok(work_supported(&session)));
        transport.script(Ok(json!({"operation": PRODUCER, "producer": PRODUCER,
            "verifications": [{ "stage": "focused", "record_id": record_id(0) }],
            "run_outcome": "passed"})));
        fixture.state.engram_verification_records_tick();
        let should_settle = matches!(scenario, "omitted" | "same pushd directory")
            || scenario.starts_with("matching");
        assert_eq!(
            transport.sent_records().len(),
            usize::from(should_settle),
            "{scenario}"
        );
        assert_eq!(
            matches!(
                current(&fixture, &run).verification_record,
                Some(TestRunVerificationAck::Settled { .. })
            ),
            should_settle,
            "{scenario}"
        );
    }
}

#[test]
fn external_dart_codex_typed_and_legacy_routes_only_attach_the_first_end() {
    use crate::codex_events::handle_shared_codex_app_server_message;

    for (legacy_first, threadless) in [(false, false), (true, false), (false, true), (true, true)] {
        for (start_has_id, end_has_id, first_has_locator, cwd_matches) in [
            (true, true, false, true),
            (true, true, true, true),
            (true, true, true, false),
            (false, false, true, true),
            (true, false, true, true),
        ] {
            let fixture = RunFixture::new("external-dart-codex-completion");
            let (session, _) = engram_session(&fixture, false);
            let (runtime, _rx, process) = test_shared_codex_runtime("external-dart-codex-runtime");
            let named = dart_named_root(&fixture.root, &fixture.root, &session);
            let thread = "external-codex-thread";
            let turn_id = "external-codex-turn";
            install_single_wire_codex_fixture(&fixture.state, Some(runtime.clone()));
            {
                let mut inner = fixture.state.inner.lock().unwrap();
                let index = inner.find_session_index(&session).unwrap();
                let record = &mut inner.sessions[index];
                record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                    runtime_id: runtime.runtime_id.clone(),
                    input_tx: runtime.input_tx.clone(),
                    process,
                    shared_session: Some(SharedCodexSessionHandle {
                        runtime: runtime.clone(),
                        session_id: session.clone(),
                    }),
                });
                record.session.status = SessionStatus::Active;
                record.active_turn_generation = 1;
                record.engram.work_binding = Some(binding());
                inner.engram_work_source_roots.push(named.clone());
            }
            runtime.sessions.lock().unwrap().insert(
                session.clone(),
                SharedCodexSessionState {
                    active_turn_generation: Some(1),
                    thread_id: Some(thread.to_owned()),
                    turn_id: Some(turn_id.to_owned()),
                    turn_started: true,
                    ..SharedCodexSessionState::default()
                },
            );
            runtime
                .thread_sessions
                .lock()
                .unwrap()
                .insert(thread.to_owned(), session.clone());
            let launcher = fixture
                .root
                .with_file_name("launcher package")
                .join("scripts/test-launcher.mjs");
            fs::create_dir_all(launcher.parent().unwrap()).unwrap();
            fs::write(&launcher, "").unwrap();
            let cwd = test_run_display_path(&fixture.root);
            let command = format!(
                r#"node "{}" focused --input commit --project-root "{cwd}" -- dart test"#,
                test_run_display_path(&launcher),
            );
            // Use the display fallback as a real ID to prove an ID-less END cannot
            // consume even a START that happens to have that key.
            let key = &command;
            let pending = Arc::new(Mutex::new(HashMap::new()));
            let dispatch = |frame: Value| {
                handle_shared_codex_app_server_message(
                    &frame,
                    &fixture.state,
                    &runtime.runtime_id,
                    &pending,
                    &runtime.sessions,
                    &runtime.thread_sessions,
                    &runtime.input_tx,
                )
                .unwrap();
            };
            let mut start = json!({"type":"commandExecution", "command":command, "cwd":cwd});
            if start_has_id {
                start["id"] = json!(key);
            }
            dispatch(json!({"method":"item/started", "params":{
                "threadId":thread, "turnId":turn_id, "item":start
            }}));

            let mut run = dart_run_for_session(
                &fixture,
                &session,
                &["dart", "test"],
                &["dart", "test", "--file-reporter", OWNED_REPORT_ARG],
                Some(include_bytes!("fixtures/declared-dart/dart-pass.jsonl")),
                0,
            );
            run.registration.named_root.as_mut().unwrap().workspace_id = named.root;
            mark_external_dart_run(&mut run, &launcher, Some(true));
            register(&fixture, &[&run]);

            let directory = test_run_display_path(&run.run_dir);
            let locator = format!("RUN {directory}\nresults: {directory}/results.json");
            let other_directory = fixture.root.join("other directory");
            fs::create_dir(&other_directory).unwrap();
            let other_cwd = test_run_display_path(&other_directory);
            let terminal = |legacy, has_id, output: &str, reported_cwd: &str| {
                let mut item = json!({
                    "type": if legacy { "CommandExecution" } else { "commandExecution" },
                    "command": command, "status": "completed", "exitCode": 0,
                    "aggregatedOutput": output, "cwd": reported_cwd,
                });
                if has_id {
                    item["id"] = json!(key);
                }
                let mut frame = if legacy {
                    json!({"method":"codex/event/item_completed", "params":{"msg":{"turn_id":turn_id, "item":item}}})
                } else {
                    json!({"method":"item/completed", "params":{"turnId":turn_id, "item":item}})
                };
                if !threadless {
                    if legacy {
                        frame["params"]["msg"]["thread_id"] = json!(thread);
                    } else {
                        frame["params"]["threadId"] = json!(thread);
                    }
                }
                dispatch(frame);
            };
            let custody = || {
                let inner = fixture.state.inner.lock().unwrap();
                let mut rows = inner.test_run_observations.values().filter(|row| {
                    row.summary.owner_session_id.as_deref() == Some(session.as_str())
                });
                let row = rows.next();
                assert!(rows.next().is_none());
                row.map(|row| {
                    let ended = row.external_dart_launch.as_ref().unwrap().provider_ended;
                    let attached = row.artifact.as_ref().is_some_and(|artifact| {
                        artifact.run_id == run.registration.run_id
                            && artifact.terminal_digest.is_some()
                    });
                    (ended, attached)
                })
            };
            assert_eq!(custody(), start_has_id.then_some((false, false)));
            terminal(
                legacy_first,
                end_has_id,
                if first_has_locator {
                    &locator
                } else {
                    "finished"
                },
                if cwd_matches { &cwd } else { &other_cwd },
            );
            let should_attach = end_has_id && first_has_locator && cwd_matches;
            assert_eq!(
                custody(),
                start_has_id.then_some((end_has_id, should_attach)),
                "first end legacy={legacy_first} threadless={threadless}",
            );
            terminal(!legacy_first, true, &locator, &cwd);
            assert_eq!(
                custody(),
                start_has_id.then_some((true, should_attach || !end_has_id)),
                "second end legacy={} threadless={threadless}",
                !legacy_first,
            );
        }
    }
}

#[test]
fn external_dart_registration_and_original_launch_select_the_same_target_claim() {
    use super::super::test_run_registration::named_root::name_root;

    for (late_description, name_after_start, successor, should_send) in [
        (false, false, false, true),
        (true, false, false, true),
        (false, false, true, true),
        (true, false, true, true),
        (true, true, true, false),
    ] {
        let fixture = RunFixture::new("external-dart-target-claim");
        let (session, transport) = engram_session(&fixture, false);
        commit_dart_worktree_fixture(&fixture.root);
        let active_root = fixture.root.with_file_name("active worktree");
        run_git_test_command(
            &fixture.root,
            &["worktree", "add", "--detach", active_root.to_str().unwrap()],
        );
        let target = binding();
        let active = EngramControlWorkBinding {
            work_id: "active-work".to_owned(),
            claim_id: "active-claim".to_owned(),
            run_id: "active-run".to_owned(),
            root_execution_id: "active-execution".to_owned(),
            ..target.clone()
        };
        name_root(
            &fixture,
            &store(),
            &active,
            &session,
            &fs::canonicalize(active_root).unwrap(),
            2,
        );
        let naming_session = if successor {
            create_test_project_session(
                &fixture.state,
                Agent::Codex,
                &fixture.project_id,
                &fixture.root,
            )
        } else {
            session.clone()
        };
        let name_target = || {
            name_root(
                &fixture,
                &store(),
                &target,
                &naming_session,
                &fs::canonicalize(&fixture.root).unwrap(),
                3,
            )
        };
        let original_name = if name_after_start {
            None
        } else {
            Some(name_target())
        };
        let (runtime, _rx) = test_codex_runtime_handle("external-dart-target-claim-runtime");
        {
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
            inner.sessions[index].engram.work_binding = Some(active.clone());
        }
        let launcher = fixture
            .root
            .with_file_name("launcher package")
            .join("scripts/test-launcher.mjs");
        fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        fs::write(&launcher, "").unwrap();
        let root = test_run_display_path(&fixture.root);
        let command = format!(
            r#"node "{}" focused --input commit --project-root "{root}" -- dart test"#,
            test_run_display_path(&launcher)
        );
        let key = "external-dart-target-claim";
        let provenance = EngramObservationProvenance::Ambient;
        fixture.state.note_engram_command_started(
            &session,
            &provenance,
            key,
            (!late_description).then_some(command.as_str()),
            (!late_description).then_some(root.as_str()),
        );
        let named = original_name.unwrap_or_else(name_target);
        if late_description {
            fixture.state.note_engram_command_described(
                &session,
                &provenance,
                key,
                Some(&command),
                Some(&root),
            );
        }
        let mut run = dart_run_for_session(
            &fixture,
            &session,
            &["dart", "test"],
            &["dart", "test", "--file-reporter", OWNED_REPORT_ARG],
            Some(include_bytes!("fixtures/declared-dart/dart-pass.jsonl")),
            0,
        );
        mark_external_dart_run(&mut run, &launcher, Some(true));
        let path = run.run_dir.join("results.json");
        let mut results: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        fs::write(
            &path,
            json!({"runId": run.registration.run_id, "state":"running"}).to_string(),
        )
        .unwrap();
        fixture
            .state
            .register_test_run(
                &session,
                RegisterTestRunRequest {
                    run_id: run.registration.run_id.clone(),
                    run_directory: test_run_display_path(&run.run_dir),
                },
            )
            .unwrap();
        let registered = current(&fixture, &run);
        assert_eq!(registered.work_binding.as_ref(), Some(&target));
        assert_eq!(
            registered.named_root.as_ref().unwrap().workspace_id,
            named.root
        );
        assert_eq!(registered.authority_store.as_ref(), Some(&store()));
        let completed = test_run_timestamp();
        results["ended"] = json!(completed);
        results["stages"][0]["started"] = json!(registered.registered_at);
        results["stages"][0]["ended"] = json!(completed);
        fs::write(&path, results.to_string()).unwrap();
        fixture.state.note_engram_command_finished(
            &session,
            &provenance,
            key,
            &command,
            &format!("RUN {}", test_run_display_path(&run.run_dir)),
            Some(EngramCommandExit::Code(0)),
        );
        fixture
            .state
            .install_test_engram_transport(transport.clone());
        transport.script(Ok(work_supported(&session)));
        transport.script(Ok(json!({"operation": PRODUCER, "producer": PRODUCER,
            "verifications": [{ "stage": "focused", "record_id": record_id(0) }],
            "run_outcome": "passed"})));
        fixture.state.engram_verification_records_tick();
        assert_eq!(transport.sent_records().len(), usize::from(should_send));
        if should_send {
            assert!(matches!(
                current(&fixture, &run).verification_record,
                Some(TestRunVerificationAck::Settled { .. })
            ));
        } else {
            assert_eq!(
                engram_verification_record_progress(&run.run_dir),
                Some(EngramVerificationRecordProgress::Pending {
                    reason: EngramVerificationRecordPending::ExternalLaunchCustodyMissing,
                })
            );
        }
    }
}

#[test]
fn external_dart_registration_refuses_a_sibling_worktree_admin_locator() {
    let fixture = RunFixture::new("external-dart-sibling");
    let (session, transport) = engram_session(&fixture, false);
    commit_dart_worktree_fixture(&fixture.root);
    let sibling = fixture.root.with_file_name("sibling worktree");
    run_git_test_command(
        &fixture.root,
        &["worktree", "add", "--detach", sibling.to_str().unwrap()],
    );
    let mut run = dart_run_for_session(
        &fixture,
        &session,
        &["dart", "test"],
        &["dart", "test", "--file-reporter", OWNED_REPORT_ARG],
        Some(include_bytes!("fixtures/declared-dart/dart-pass.jsonl")),
        0,
    );
    let launcher = fixture
        .root
        .with_file_name("launcher package")
        .join("scripts/test-launcher.mjs");
    mark_external_dart_run(&mut run, &launcher, Some(true));
    let request_path = run.run_dir.join("request.json");
    let original = fs::read(&request_path).unwrap();
    let request: Value = serde_json::from_slice(&original).unwrap();
    let mut selected = request.clone();
    selected["requestMessageId"] = json!("mailbox-message-81710daa-a221-4111-921b-9d11f4bfd3b6");
    let selected_command = format!(
        r#"node "{}" focused --input commit --project-root "{}" --request-message-id {} -- dart test"#,
        test_run_display_path(&launcher),
        test_run_display_path(&fixture.root),
        selected["requestMessageId"].as_str().unwrap(),
    );
    assert!(
        engram_observed_dart_launcher(&selected_command, &EngramObservationProvenance::Ambient,)
            .is_none()
    );
    let mut unmeasured = request;
    unmeasured["externalLauncher"]["commit"] = Value::Null;
    for (invalid, reason) in [
        (
            selected,
            "--request-message-id cannot be combined with --project-root",
        ),
        (unmeasured, "external_launcher_provenance_unverified"),
    ] {
        fs::write(&request_path, invalid.to_string()).unwrap();
        let error = fixture
            .state
            .register_test_run(
                &session,
                RegisterTestRunRequest {
                    run_id: run.registration.run_id.clone(),
                    run_directory: test_run_display_path(&run.run_dir),
                },
            )
            .err()
            .expect("unsupported external request must be refused");
        assert!(error.message.contains(reason), "{error:?}");
    }
    fs::write(request_path, original).unwrap();
    let directory = engram_external_launcher_run_parent(&sibling)
        .unwrap()
        .join(&run.registration.run_id);
    fs::create_dir_all(&directory).unwrap();
    fs::copy(
        run.run_dir.join("request.json"),
        directory.join("request.json"),
    )
    .unwrap();
    fs::copy(
        run.run_dir.join(ENGRAM_LAUNCHER_DART_REPORT),
        directory.join(ENGRAM_LAUNCHER_DART_REPORT),
    )
    .unwrap();
    let mut results: Value =
        serde_json::from_slice(&fs::read(run.run_dir.join("results.json")).unwrap()).unwrap();
    results["stages"][0]["command"][3] = json!(format!(
        "json:{}",
        test_run_display_path(&directory.join(ENGRAM_LAUNCHER_DART_REPORT))
    ));
    fs::write(directory.join("results.json"), results.to_string()).unwrap();
    let error = fixture
        .state
        .register_test_run(
            &session,
            RegisterTestRunRequest {
                run_id: run.registration.run_id.clone(),
                run_directory: test_run_display_path(&directory),
            },
        )
        .err()
        .expect("the sibling run directory must be refused");
    assert!(
        error
            .message
            .contains("selected worktree's Git admin directory"),
        "{error:?}"
    );
    fixture.state.engram_verification_records_tick();
    assert!(transport.sent_records().is_empty());
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
