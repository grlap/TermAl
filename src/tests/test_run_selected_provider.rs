// Synthetic provider frames exercise the production facade with a selector-
// bearing command and real registration/settlement owners. These fixture facts
// are neither an executed remote test nor verification credit.
use super::*;

const PROVIDER_THREAD: &str = "selected-provider-thread";
const PROVIDER_TURN: &str = "selected-provider-turn";

fn linked_provider_dispatch(
    state: AppState,
    runner: &str,
    provenance: &EngramObservationProvenance,
) -> impl Fn(&str, Value) {
    let dispatch = provider_router(state, runner, provenance);
    move |method, item| {
        dispatch(
            method,
            json!({
                "threadId": PROVIDER_THREAD, "turnId": PROVIDER_TURN, "item": item,
            }),
        );
    }
}

fn provider_router(
    state: AppState,
    runner: &str,
    provenance: &EngramObservationProvenance,
) -> impl Fn(&str, Value) {
    let EngramObservationProvenance::Runtime {
        token: RuntimeToken::Codex(runtime),
        turn_generation,
    } = provenance
    else {
        panic!("expected a Codex runtime for the linked provider fixture");
    };
    let runtime = runtime.clone();
    let sessions = SharedCodexSessions::new();
    sessions.lock().unwrap().insert(
        runner.to_owned(),
        SharedCodexSessionState {
            thread_id: Some(PROVIDER_THREAD.to_owned()),
            turn_id: Some(PROVIDER_TURN.to_owned()),
            turn_started: true,
            active_turn_generation: Some(*turn_generation),
            ..SharedCodexSessionState::default()
        },
    );
    let threads = Arc::new(Mutex::new(HashMap::from([(
        PROVIDER_THREAD.to_owned(),
        runner.to_owned(),
    )])));
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let (input, commands) = mpsc::channel();
    move |method, params| {
        let _keep_commands = &commands;
        let message = json!({ "method": method, "params": params });
        handle_shared_codex_app_server_message(
            &message, &state, &runtime, &pending, &sessions, &threads, &input,
        )
        .unwrap();
    }
}

pub(in crate::tests) async fn selected_provider_completion<F, Fut>(
    state: AppState,
    mode: MailboxTestMode,
    linked_worktree: bool,
    select: F,
) -> TestRunRegistration
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = String>,
{
    let root = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("selected-provider");
    fs::create_dir_all(&root).unwrap();
    run_git_test_command(&root, &["init", "--quiet"]);
    let project_id = create_test_project(&state, &root, "Selected provider");
    let fixture = RunFixture {
        state,
        root,
        project_id,
    };
    let runner = running_owner(&fixture, "selected-provider-runtime");
    let command_root = if linked_worktree {
        create_provider_worktree(&fixture.root, "selected-provider-linked")
    } else {
        fixture.root.clone()
    };
    let provenance = {
        let mut inner = fixture.state.inner.lock().unwrap();
        inner
            .projects
            .iter_mut()
            .find(|p| p.id == fixture.project_id)
            .unwrap()
            .engram = Some(EngramProjectSettings::default());
        let index = inner.find_session_index(&runner).unwrap();
        let record = &mut inner.sessions[index];
        EngramObservationProvenance::Runtime {
            token: record.runtime.runtime_token().unwrap(),
            turn_generation: record.active_turn_generation,
        }
    };
    // The caller sends through the real authenticated relay and reads the
    // recipient's actual handle. Encoding a local message would lack membership.
    let selector = select(runner.clone()).await;
    let canonical = MailboxRelayLocator::decode(&selector, true)
        .unwrap()
        .unwrap()
        .id;
    let command = match mode {
        MailboxTestMode::Focused => format!(
            "node scripts/test-launcher.mjs focused --input commit --request-message-id {selector} -- cargo test -j 8 --bin termal -- tests::test_runs::test_run_registration::native_completion:: --test-threads=2"
        ),
        MailboxTestMode::Full => format!(
            "node scripts/test-launcher.mjs full --input commit --request-message-id {selector}"
        ),
    };
    let command_root = fs::canonicalize(&command_root).unwrap();
    let cwd = test_run_display_path(&command_root);
    let key = "selected-provider-tool";
    let dispatch = linked_worktree
        .then(|| linked_provider_dispatch(fixture.state.clone(), &runner, &provenance));
    if let Some(dispatch) = &dispatch {
        dispatch(
            "item/started",
            json!({"type": "commandExecution", "id": key, "command": command,
                "cwd": cwd, "status": "inProgress"}),
        );
    } else {
        fixture.state.engram_host().observe(
            &runner,
            &provenance,
            EngramRecorderObservation::CommandStarted {
                key,
                ran: Some(&command),
                cwd: Some(&cwd),
            },
        );
    }
    let observed = {
        let inner = fixture.state.inner.lock().unwrap();
        let rows: Vec<_> = inner
            .test_run_observations
            .values()
            .filter(|row| row.tool_identity == sha256_hex(key.as_bytes()))
            .cloned()
            .collect();
        assert_eq!(
            rows.len(),
            1,
            "the real selector-bearing provider start must create one observation, even with Engram disabled"
        );
        assert!(rows[0].runtime_identity.is_some());
        if linked_worktree {
            let session = &inner.sessions[inner.find_session_index(&runner).unwrap()];
            assert_eq!(
                fs::canonicalize(&session.session.workdir).unwrap(),
                fs::canonicalize(&fixture.root).unwrap(),
            );
            assert_ne!(
                test_run_path_key(&session.session.workdir),
                test_run_path_key(&cwd)
            );
            assert!(rows[0].work_binding.is_none());
            assert!(rows[0].source_root.is_none());
            assert!(session.engram.active_turn_checks.is_empty());
        }
        rows[0].execution_id.clone()
    };

    let id = format!("test-{}", Uuid::new_v4());
    let mut request = full_request(&command_root, &test_run_timestamp());
    request["owner"] = json!(runner);
    request["creatorPid"] = json!(std::process::id());
    request["detached"] = json!(false);
    request["input"] = json!("commit");
    request["requestMessageId"] = json!(selector);
    request["full"] = json!(mode == MailboxTestMode::Full);
    if mode == MailboxTestMode::Focused {
        request["stages"] = json!([{ "name": "focused" }]);
    }
    let runs_dir = if linked_worktree {
        let git_dir = run_git_test_command_output(
            &normalize_user_facing_path(&command_root),
            &["rev-parse", "--absolute-git-dir"],
        );
        PathBuf::from(git_dir).join("review-runs")
    } else {
        fixture.runs_dir()
    };
    let directory = write_run(&runs_dir, &id, request, Some(running_results(Some(717))));
    let (status, ack): (StatusCode, Value) = request_json(
        &app_router(fixture.state.clone()),
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{runner}/test-runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "runId": id, "runDirectory": test_run_display_path(&directory),
                    "requestMessageId": selector,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{ack}");
    assert_eq!(ack["registered"], true);
    {
        let inner = fixture.state.inner.lock().unwrap();
        let registration = &inner.test_run_registrations[&id];
        let origin = registration.native_origin.as_ref().unwrap();
        assert!(origin.project_scope.is_none());
        let causal = test_run_causal_request(registration).unwrap();
        assert_eq!(causal.request_message_id, selector);
        assert_eq!(causal.canonical_message_id, canonical);
        assert!(registration.native_completion.is_none());
    }
    let locator = format!("RUN {}", test_run_display_path(&directory));
    if !linked_worktree {
        fixture.state.engram_host().observe(
            &runner,
            &provenance,
            EngramRecorderObservation::CommandFinished {
                key,
                command: &command,
                reported_command: Some(&command),
                output: &locator,
                exit: Some(EngramCommandExit::NotFinished),
                runtime_output_cut: false,
            },
        );
        assert_eq!(
            fixture.state.inner.lock().unwrap().test_run_observations[&observed]
                .artifact
                .as_ref()
                .unwrap()
                .run_id,
            id
        );
    }

    let source = json!({
        "commit": "a".repeat(40), "tree": "b".repeat(40),
        "submodules": [], "changes": [],
    });
    let mut results = passed_results();
    results["runId"] = json!(id);
    results["ended"] = json!(test_run_timestamp());
    results["sourceIdBefore"] = source.clone();
    results["sourceIdAfter"] = source.clone();
    if mode == MailboxTestMode::Focused {
        results["stages"] = json!([{
            "name": "focused", "state": "passed", "code": 0,
            "tests": {"passed": 1, "failed": 0, "ignored": 0},
        }]);
    }
    fs::write(directory.join("results.json"), results.to_string()).unwrap();
    if let Some(dispatch) = &dispatch {
        dispatch(
            "item/completed",
            json!({"type": "commandExecution", "id": key, "command": command,
                "cwd": cwd, "status": "completed", "exitCode": 0, "aggregatedOutput": locator}),
        );
        let inner = fixture.state.inner.lock().unwrap();
        let row = &inner.test_run_observations[&observed];
        let artifact = row.artifact.as_ref().unwrap_or_else(|| {
            panic!(
                "the original provider completion must attach the linked launcher artifact (observed worktree {:?})",
                row.summary.worktree
            )
        });
        assert_eq!(
            test_run_path_key(&row.summary.worktree),
            test_run_path_key(&cwd)
        );
        assert_eq!(artifact.run_id, id);
        assert_eq!(
            artifact.request_identity.as_ref(),
            Some(&inner.test_run_registrations[&id].request_identity),
        );
    } else {
        fixture.state.engram_host().observe(
            &runner,
            &provenance,
            EngramRecorderObservation::CommandFinished {
                key,
                command: &command,
                reported_command: Some(&command),
                output: &locator,
                exit: Some(EngramCommandExit::Code(0)),
                runtime_output_cut: false,
            },
        );
    }
    fixture.refresh(&[]);
    let registration = fixture.state.inner.lock().unwrap().test_run_registrations[&id].clone();
    let retained = registration
        .native_completion
        .as_ref()
        .expect("the original observed execution must retain its native completion");
    assert_eq!(retained.execution_id, observed);
    assert_eq!(retained.completion.run_id, id);
    assert_eq!(
        retained.completion.result.verdict,
        native_test_completion::Verdict::Pass
    );
    assert_eq!(
        retained.completion.mode,
        match mode {
            MailboxTestMode::Focused => native_test_completion::Mode::Focused,
            MailboxTestMode::Full => native_test_completion::Mode::Full,
        }
    );
    let native_test_completion::Input::SourceId(input) = &retained.completion.input else {
        panic!("native completion must retain SourceId endpoints");
    };
    assert_eq!(
        serde_json::to_value(&input.source_id_before).unwrap(),
        source
    );
    assert_eq!(input.source_id_before, input.source_id_after);
    let durable = fixture
        .state
        .read_durable_native_test_run(&registration)
        .unwrap()
        .unwrap();
    assert_eq!(durable.completion.as_ref(), Some(&retained.completion));
    assert!(
        registration.verification_record.is_none(),
        "producer facts do not mint local credit"
    );
    if let Some(dispatch) = &dispatch {
        let original = fixture.state.inner.lock().unwrap().test_run_observations[&observed].clone();
        dispatch(
            "item/completed",
            json!({"type": "commandExecution", "id": key, "command": command,
                "cwd": cwd, "status": "completed", "exitCode": 1}),
        );
        assert_eq!(
            fixture.state.inner.lock().unwrap().test_run_observations[&observed],
            original,
            "duplicate provider completion must leave the original association and outcome intact",
        );
    }
    registration
}

fn create_provider_worktree(root: &FsPath, name: &str) -> PathBuf {
    run_git_test_command(
        root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "fixture root",
        ],
    );
    let linked = root.with_file_name(name);
    run_git_test_command(
        root,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );
    linked
}

#[tokio::test]
async fn foreign_provider_cwd_refusal_survives_compaction_and_reload() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = RunFixture::new("selected-provider-refusal");
    let runner = running_owner(&fixture, "selected-provider-refusal-runtime");
    run_git_test_command(&fixture.root, &["init", "--quiet"]);
    let foreign = fixture.root.with_file_name("selected-provider-foreign");
    fs::create_dir_all(&foreign).unwrap();
    run_git_test_command(&foreign, &["init", "--quiet"]);
    let provenance = {
        let inner = fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&runner).unwrap()];
        EngramObservationProvenance::Runtime {
            token: record.runtime.runtime_token().unwrap(),
            turn_generation: record.active_turn_generation,
        }
    };
    let dispatch = linked_provider_dispatch(fixture.state.clone(), &runner, &provenance);
    let id = "foreign-test";
    let cwd = test_run_display_path(&fs::canonicalize(&foreign).unwrap());
    dispatch(
        "item/started",
        json!({"type": "commandExecution", "id": id,
        "command": "cargo test", "cwd": cwd, "status": "inProgress"}),
    );
    let refused = fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_observations
        .values()
        .find(|row| row.tool_identity == sha256_hex(id.as_bytes()))
        .unwrap()
        .clone();
    assert_eq!(refused.summary.state, TestRunState::Unknown);
    dispatch(
        "item/completed",
        json!({"type": "commandExecution", "id": id,
        "command": "cargo test", "cwd": cwd, "status": "completed", "exitCode": 0}),
    );
    let execution = {
        let inner = fixture.state.inner.lock().unwrap();
        let row = inner
            .test_run_observations
            .values()
            .find(|row| row.tool_identity == sha256_hex(id.as_bytes()))
            .unwrap();
        assert!(row.artifact.is_none() && row.evidence.is_empty());
        assert!(row.summary.worktree.is_empty());
        assert!(row.final_unbound_proof.is_none());
        assert_eq!(row, &refused);
        row.execution_id.clone()
    };
    let live = fixture.summary(&execution);
    assert_eq!(live.state, TestRunState::Unknown);
    assert!(live.exit_code.is_none());
    let explanation = live.error.expect("foreign cwd has a bounded reason");
    assert!(explanation.contains("outside the session repository"));

    super::compaction::fill_unbound_history(&fixture, TEST_RUN_TERMINAL_RUNS_PER_PROJECT + 1);
    fixture.refresh(&[]);
    for reload in [false, true] {
        if reload {
            *fixture.state.inner.lock().unwrap() =
                load_state(fixture.state.persistence_path.as_path())
                    .unwrap()
                    .unwrap();
        }
        let app = app_router(fixture.state.clone());
        let (status, detail): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .uri(format!("/api/test-runs/{execution}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(detail["source"], "retained");
        assert_eq!(detail["run"]["error"], explanation);
        assert_eq!(detail["run"]["state"], "unknown");
        let (status, history): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .uri("/api/test-runs")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let listed = history["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["runId"] == execution)
            .expect("compacted foreign provider observation remains in history");
        assert_eq!(listed["historyCompacted"], true);
        assert_eq!(listed["error"], explanation);
        assert_eq!(listed["state"], "unknown");
    }
}

#[tokio::test]
async fn provider_execution_lifecycle_respects_original_placement_and_terminal_identity() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    // These are real router deliveries. An exact late END after any seal
    // must leave the entire original observation unchanged.
    for (case, expected_state, expected_refusal) in [
        ("numeric exit", TestRunState::Passed, None),
        ("one-call directory", TestRunState::Passed, None),
        ("same start replay", TestRunState::Passed, None),
        ("unrelated completion", TestRunState::Passed, None),
        (
            "conflicting start",
            TestRunState::Unknown,
            Some(TestRunExecutionRefusal::ConflictingStart),
        ),
        (
            "different command at end",
            TestRunState::Unknown,
            Some(TestRunExecutionRefusal::CompletionMismatch),
        ),
        (
            "missing item",
            TestRunState::Unknown,
            Some(TestRunExecutionRefusal::ItemIdentityUnavailable),
        ),
        (
            "declined",
            TestRunState::Unknown,
            Some(TestRunExecutionRefusal::Declined),
        ),
        (
            "declined start",
            TestRunState::Unknown,
            Some(TestRunExecutionRefusal::Declined),
        ),
        (
            "missing exit",
            TestRunState::Unknown,
            Some(TestRunExecutionRefusal::MissingExit),
        ),
        (
            "turn closes first",
            TestRunState::Unknown,
            Some(TestRunExecutionRefusal::MissingExit),
        ),
        (
            "owned error",
            TestRunState::Unknown,
            Some(TestRunExecutionRefusal::MissingExit),
        ),
        (
            "unusable Git layout",
            TestRunState::Unknown,
            Some(TestRunExecutionRefusal::OutsideRepository),
        ),
    ] {
        let fixture = RunFixture::new("provider-lifecycle");
        let runner = running_owner(&fixture, "provider-lifecycle-runtime");
        run_git_test_command(&fixture.root, &["init", "--quiet"]);
        let linked = create_provider_worktree(&fixture.root, "lifecycle-linked");
        let root_cwd = test_run_display_path(&fs::canonicalize(&fixture.root).unwrap());
        let linked_cwd = test_run_display_path(&fs::canonicalize(&linked).unwrap());
        if case == "unusable Git layout" {
            let admin = run_git_test_command_output(&linked, &["rev-parse", "--absolute-git-dir"]);
            fs::remove_file(PathBuf::from(admin).join("HEAD")).unwrap();
        }
        let (command, cwd) = if case == "one-call directory" {
            (format!("pushd \"{linked_cwd}\" && cargo test"), &root_cwd)
        } else {
            ("cargo test".to_owned(), &linked_cwd)
        };
        let provenance = {
            let inner = fixture.state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&runner).unwrap()];
            EngramObservationProvenance::Runtime {
                token: record.runtime.runtime_token().unwrap(),
                turn_generation: record.active_turn_generation,
            }
        };
        let route = provider_router(fixture.state.clone(), &runner, &provenance);
        let deliver = |method, item: Value| {
            route(
                method,
                json!({
                    "threadId": PROVIDER_THREAD, "turnId": PROVIDER_TURN, "item": item,
                }),
            )
        };
        let mut started = json!({
            "type": "commandExecution", "id": "lifecycle-command",
            "command": command, "cwd": cwd, "status": "inProgress",
        });
        let complete = json!({
            "type": "commandExecution", "id": "lifecycle-command",
            "command": command, "cwd": cwd, "status": "completed", "exitCode": 0,
        });
        if case == "missing item" {
            started.as_object_mut().unwrap().remove("id");
        } else if case == "declined start" {
            started["status"] = json!("declined");
        }
        deliver("item/started", started.clone());
        let original = {
            let inner = fixture.state.inner.lock().unwrap();
            assert_eq!(inner.test_run_observations.len(), 1, "{case}");
            inner.test_run_observations.values().next().unwrap().clone()
        };
        let read = || {
            fixture.state.inner.lock().unwrap().test_run_observations[&original.execution_id]
                .clone()
        };
        if expected_refusal.is_none() {
            assert_eq!(original.summary.worktree, linked_cwd, "{case}");
            assert_eq!(original.summary.state, TestRunState::Running, "{case}");
        }
        match case {
            "same start replay" => {
                deliver("item/started", started);
                assert_eq!(read(), original);
                deliver("item/completed", complete.clone());
            }
            "conflicting start" => {
                started["cwd"] = json!(root_cwd);
                deliver("item/started", started);
            }
            "unrelated completion" => {
                for (thread, turn) in [
                    ("unrelated-thread", PROVIDER_TURN),
                    (PROVIDER_THREAD, "unrelated-turn"),
                ] {
                    route(
                        "item/completed",
                        json!({
                            "threadId": thread, "turnId": turn, "item": complete,
                        }),
                    );
                    assert_eq!(read(), original);
                }
                let mut different_item = complete.clone();
                different_item["id"] = json!("different-command-item");
                deliver("item/completed", different_item);
                assert_eq!(read(), original);
                deliver("item/completed", complete.clone());
            }
            "different command at end" => {
                let mut changed = complete.clone();
                changed["command"] = json!("cargo test --workspace");
                deliver("item/completed", changed);
            }
            "missing item" => {
                // The recorder's transcript key is identical. This cannot
                // substitute for the missing actual native item identity.
                deliver("item/started", started);
                let inner = fixture.state.inner.lock().unwrap();
                assert_eq!(inner.test_run_observations.len(), 2);
                assert!(inner.test_run_observations.values().all(|row| {
                    row.runtime_identity.is_some()
                        && row.artifact.is_none()
                        && row.final_unbound_proof.is_none()
                        && row.summary.ended_at.is_some()
                }));
            }
            "declined" | "missing exit" => {
                let mut unknown = complete.clone();
                unknown["status"] = json!(if case == "declined" {
                    "declined"
                } else {
                    "failed"
                });
                unknown.as_object_mut().unwrap().remove("exitCode");
                deliver("item/completed", unknown);
            }
            "turn closes first" => route(
                "turn/completed",
                json!({
                    "threadId": PROVIDER_THREAD,
                    "turn": {"id": PROVIDER_TURN, "status": "completed"},
                }),
            ),
            "owned error" => route(
                "error",
                json!({
                    "threadId": PROVIDER_THREAD, "turnId": PROVIDER_TURN,
                    "willRetry": false, "error": {"message": "fixture owned terminal error"},
                }),
            ),
            _ => deliver("item/completed", complete.clone()),
        }
        let terminal = read();
        let context = terminal.execution_context.as_ref().unwrap();
        assert_eq!(terminal.summary.state, expected_state, "{case}");
        assert_eq!(context.refusal(), expected_refusal, "{case}");
        assert!(terminal.summary.ended_at.is_some(), "{case}");
        assert!(
            terminal.artifact.is_none() && terminal.evidence.is_empty(),
            "{case}"
        );
        let mut duplicate = complete;
        duplicate["exitCode"] = json!(9);
        deliver("item/completed", duplicate);
        assert_eq!(read(), terminal, "{case}");
    }
}

fn linked_provider_fixture(
    label: &str,
) -> (RunFixture, String, PathBuf, EngramObservationProvenance) {
    let fixture = RunFixture::new(label);
    let runner = running_owner(&fixture, label);
    let linked = fs::canonicalize(create_provider_worktree(&fixture.root, label)).unwrap();
    let provenance = {
        let inner = fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&runner).unwrap()];
        EngramObservationProvenance::Runtime {
            token: record.runtime.runtime_token().unwrap(),
            turn_generation: record.active_turn_generation,
        }
    };
    (fixture, runner, linked, provenance)
}

#[tokio::test]
async fn linked_detached_refresh_refuses_replaced_run_directory() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (fixture, runner, linked, provenance) = linked_provider_fixture("detached-refresh");
    super::native_origin::bind_native_host(&fixture);
    let route = linked_provider_dispatch(fixture.state.clone(), &runner, &provenance);
    let cwd = test_run_display_path(&linked);
    let command = "node scripts/test-launcher.mjs full --input commit --detach";
    let item = "detached-refresh-item";
    route(
        "item/started",
        json!({"type":"commandExecution", "id":item,
        "command":command, "cwd":cwd, "status":"inProgress"}),
    );
    let execution = {
        let inner = fixture.state.inner.lock().unwrap();
        assert_eq!(
            inner
                .test_run_observations
                .index()
                .linked_artifact_executions()
                .count(),
            0
        );
        assert!(test_run_linked_artifact_sources(&inner).is_empty());
        inner
            .test_run_observations
            .values()
            .next()
            .unwrap()
            .execution_id
            .clone()
    };
    let run = format!("test-{}", Uuid::new_v4());
    let admin = run_git_test_command_output(
        &normalize_user_facing_path(&linked),
        &["rev-parse", "--absolute-git-dir"],
    );
    let mut request = full_request(&linked, &test_run_timestamp());
    request["owner"] = json!(runner);
    request["input"] = json!("commit");
    request["detached"] = json!(true);
    request["creatorPid"] = json!(std::process::id());
    let directory = write_run(
        &PathBuf::from(admin).join("review-runs"),
        &run,
        request,
        Some(running_results(Some(717))),
    );
    fixture
        .state
        .register_test_run(&runner, registration_request(&run, &directory))
        .unwrap();
    route(
        "item/completed",
        json!({"type":"commandExecution", "id":item,
        "command":command, "cwd":cwd, "status":"completed", "exitCode":0,
        "aggregatedOutput":format!("STARTED {}", test_run_display_path(&directory))}),
    );
    let observation =
        || fixture.state.inner.lock().unwrap().test_run_observations[&execution].clone();
    let registration = || fixture.state.inner.lock().unwrap().test_run_registrations[&run].clone();
    let launched = observation();
    let context = launched.execution_context.as_ref().unwrap();
    assert_eq!(context.phase, TestRunExecutionPhase::LauncherReported);
    assert!(context.artifact_directory_identity.is_some());
    assert_eq!(launched.artifact.as_ref().unwrap().run_id, run);
    assert!(
        launched
            .artifact
            .as_ref()
            .unwrap()
            .terminal_digest
            .is_none()
    );
    assert!(registration().native_origin.is_some());
    assert!(registration().native_completion.is_none());
    assert_eq!(
        test_run_linked_artifact_sources(&fixture.state.inner.lock().unwrap()).len(),
        1
    );

    let original = directory.with_file_name("retired-original");
    fs::rename(&directory, &original).unwrap();
    fs::create_dir(&directory).unwrap();
    fs::copy(
        original.join("request.json"),
        directory.join("request.json"),
    )
    .unwrap();
    let mut terminal = passed_results();
    terminal["runId"] = json!(run);
    terminal["ended"] = json!(test_run_timestamp());
    let results = terminal.to_string();
    fs::write(directory.join("results.json"), &results).unwrap();
    fixture.refresh(&[]);
    assert_eq!(observation(), launched);
    assert!(registration().native_completion.is_none());

    fs::rename(&directory, directory.with_file_name("rejected-replacement")).unwrap();
    fs::rename(&original, &directory).unwrap();
    terminal["ended"] = json!(1);
    fs::write(directory.join("results.json"), terminal.to_string()).unwrap();
    fixture.refresh(&[]);
    assert_eq!(
        observation(),
        launched,
        "malformed terminal must not advance the phase"
    );
    assert!(registration().native_completion.is_none());

    fs::write(directory.join("results.json"), &results).unwrap();
    fixture.refresh(&[]);
    let finished = observation();
    assert!(matches!(
        finished.execution_context.unwrap().phase,
        TestRunExecutionPhase::Terminal {
            outcome: TestRunExecutionOutcome::Exited(0),
            ..
        }
    ));
    assert_eq!(finished.summary.state, TestRunState::Passed);
    assert_eq!(
        registration().native_completion.unwrap().execution_id,
        execution
    );
    assert!(registration().verification_record.is_none());
}

#[tokio::test]
async fn unaddressed_linked_completion_cannot_retire_reused_ordinary_writer() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (fixture, runner, linked, original) = linked_provider_fixture("provider-carried-reuse");
    let item = "reused-provider-item";
    let command = "cargo test --test source_file_size -- --nocapture";
    let root_cwd = test_run_display_path(&fs::canonicalize(&fixture.root).unwrap());
    fixture.state.engram_host().observe(
        &runner,
        &original,
        EngramRecorderObservation::CommandStarted {
            key: item,
            ran: Some(command),
            cwd: Some(&root_cwd),
        },
    );
    let provenance = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&runner).unwrap();
        let record = &mut inner.sessions[index];
        let runtime = record.runtime.runtime_token().unwrap();
        // Model the earlier turn being closed with its ordinary writer still
        // running. The test drives the real key-only END handoff below.
        engram_retire_command_writer(&mut record.engram, item);
        engram_note_running_codex_test(
            &mut record.engram,
            EngramRunningCodexTest {
                key: item.to_owned(),
                runtime: runtime.clone(),
                root_key: engram_path_key(&fixture.root),
            },
        );
        record.active_turn_generation += 1;
        EngramObservationProvenance::Runtime {
            token: runtime,
            turn_generation: record.active_turn_generation,
        }
    };
    let running = || {
        let inner = fixture.state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&runner).unwrap()]
            .engram
            .running_codex_tests
            .iter()
            .any(|writer| writer.key == item)
    };
    assert!(running());
    let route = provider_router(fixture.state.clone(), &runner, &provenance);
    let cwd = test_run_display_path(&linked);
    route(
        "item/started",
        json!({"threadId":PROVIDER_THREAD,"turnId":PROVIDER_TURN,
        "item":{"type":"commandExecution","id":item,"command":command,"cwd":cwd,"status":"inProgress"}}),
    );
    let rows = || {
        fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_run_observations
            .values()
            .cloned()
            .collect::<Vec<_>>()
    };
    let originals = rows();
    assert_eq!(originals.len(), 2);
    assert!(
        originals
            .iter()
            .any(|row| row.summary.worktree == cwd && row.managed_provider_execution())
    );
    let mut completed = json!({"type":"commandExecution","id":item,
        "command":command,"cwd":cwd,"status":"completed","exitCode":0});
    route(
        "item/completed",
        json!({"threadId":PROVIDER_THREAD,"item":completed}),
    );
    completed["type"] = json!("CommandExecution");
    route(
        "codex/event/item_completed",
        json!({"conversationId":PROVIDER_THREAD,
        "msg":{"item":completed}}),
    );
    assert!(running());
    assert_eq!(rows(), originals);

    completed["type"] = json!("commandExecution");
    route(
        "item/completed",
        json!({"threadId":PROVIDER_THREAD,
        "turnId":PROVIDER_TURN,"item":completed}),
    );
    assert!(running());
    let current_rows = rows();
    let linked = current_rows
        .iter()
        .find(|row| row.managed_provider_execution())
        .unwrap();
    assert_eq!(linked.summary.state, TestRunState::Passed);
    let ordinary = current_rows
        .iter()
        .find(|row| !row.managed_provider_execution())
        .unwrap();
    assert_eq!(
        ordinary,
        originals
            .iter()
            .find(|row| !row.managed_provider_execution())
            .unwrap()
    );
    completed["cwd"] = json!(root_cwd);
    route(
        "item/completed",
        json!({"threadId":PROVIDER_THREAD,
        "turnId":"original-ordinary-turn","item":completed}),
    );
    assert!(
        !running(),
        "the explicitly addressed ordinary END still retires its writer"
    );
}

#[tokio::test]
async fn idless_provider_command_without_process_cwd_stays_refused() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (fixture, runner, _, provenance) = linked_provider_fixture("missing-provider-item");
    let route = linked_provider_dispatch(fixture.state.clone(), &runner, &provenance);
    route(
        "item/started",
        json!({"type":"commandExecution",
        "command":"cargo test", "status":"inProgress"}),
    );
    let read = || {
        let inner = fixture.state.inner.lock().unwrap();
        assert_eq!(inner.test_run_observations.len(), 1);
        inner.test_run_observations.values().next().unwrap().clone()
    };
    let refusal = read();
    assert_eq!(
        refusal.execution_context.as_ref().unwrap().refusal(),
        Some(TestRunExecutionRefusal::ItemIdentityUnavailable)
    );
    assert_eq!(refusal.summary.state, TestRunState::Unknown);
    assert!(refusal.summary.ended_at.is_some());
    route(
        "item/completed",
        json!({"type":"commandExecution",
        "command":"cargo test", "status":"completed","exitCode":0}),
    );
    assert_eq!(read(), refusal);
    assert!(refusal.artifact.is_none() && refusal.final_unbound_proof.is_none());
}

#[tokio::test]
async fn one_call_linked_start_without_process_cwd_uses_original_directory() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (fixture, runner, linked, provenance) = linked_provider_fixture("one-call-cwd");
    let route = linked_provider_dispatch(fixture.state.clone(), &runner, &provenance);
    let cwd = test_run_display_path(&linked);
    let command = format!("pushd \"{cwd}\" && cargo test");
    route(
        "item/started",
        json!({"type":"commandExecution","id":"one-call",
        "command":command,"status":"inProgress"}),
    );
    let read = || {
        let inner = fixture.state.inner.lock().unwrap();
        assert_eq!(inner.test_run_observations.len(), 1);
        inner.test_run_observations.values().next().unwrap().clone()
    };
    let before = read();
    assert_eq!(before.summary.worktree, cwd);
    assert!(matches!(
        before.execution_context.as_ref().unwrap().placement,
        TestRunExecutionPlacement::Linked(_)
    ));
    assert!(
        before
            .execution_context
            .as_ref()
            .unwrap()
            .reported_cwd
            .is_none()
    );
    route(
        "item/completed",
        json!({"type":"commandExecution","id":"one-call",
        "command":command,"status":"completed","exitCode":0}),
    );
    let after = read();
    assert_eq!(after.execution_id, before.execution_id);
    assert_eq!(after.summary.worktree, cwd);
    assert_eq!(after.summary.state, TestRunState::Passed);
}

#[tokio::test]
async fn managed_monitoring_interrupt_replays_exact_durable_disposition() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (fixture, runner, linked, provenance) = linked_provider_fixture("monitoring-replay");
    let route = linked_provider_dispatch(fixture.state.clone(), &runner, &provenance);
    route(
        "item/started",
        json!({"type":"commandExecution","id":"monitoring-replay",
        "command":"cargo test","cwd":test_run_display_path(&linked),"status":"inProgress"}),
    );
    let original = fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_observations
        .values()
        .next()
        .unwrap()
        .clone();
    let id = &original.execution_id;
    let interrupt = |reason: &str| {
        Request::builder()
            .method("POST")
            .uri(format!("/api/test-runs/{id}/interrupt-monitoring"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"ownerSessionId": runner, "startedAt": original.summary.started_at,
            "reason": reason})
                .to_string(),
            ))
            .unwrap()
    };
    let app = app_router(fixture.state.clone());
    let (status, first): (StatusCode, Value) =
        request_json(&app, interrupt("Lost original terminal")).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["state"], "unknown");
    assert_eq!(first["interrupted"], true);
    assert!(first["endedAt"].is_string());
    assert!(first["exitCode"].is_null());
    let sealed = fixture.state.inner.lock().unwrap().test_run_observations[id].clone();
    assert_eq!(
        sealed.execution_context.as_ref().unwrap().refusal(),
        Some(TestRunExecutionRefusal::MonitoringInterrupted)
    );
    let (status, replay): (StatusCode, Value) =
        request_json(&app, interrupt("Lost original terminal")).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay, first);
    let (status, _): (StatusCode, Value) = request_json(&app, interrupt("Different reason")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        fixture.state.inner.lock().unwrap().test_run_observations[id],
        sealed
    );
    let loaded = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(loaded.test_run_observations[id], sealed);
}

#[tokio::test]
async fn linked_artifact_reads_are_bound_to_the_open_run_directory() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = RunFixture::new("provider-reader");
    run_git_test_command(&fixture.root, &["init", "--quiet"]);
    let linked = create_provider_worktree(&fixture.root, "reader-linked");
    let admin = PathBuf::from(run_git_test_command_output(
        &linked,
        &["rev-parse", "--absolute-git-dir"],
    ));
    let layout =
        delegation_git_boundary::ProviderLinkedWorktree::resolve(&fixture.root, &linked).unwrap();
    let run_id = format!("test-{}", Uuid::new_v4());
    let directory = admin.join("review-runs").join(&run_id);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("request.json"), b"original").unwrap();
    let pinned = layout.open_run(&directory, &run_id).unwrap();
    #[cfg(windows)]
    {
        let incoming = directory.join("request.next");
        fs::write(&incoming, b"original").unwrap();
        fs::rename(incoming, directory.join("request.json"))
            .expect("a held directory still allows the launcher to replace a child file");
    }
    let retired = directory.with_file_name("retired-run");
    let renamed = fs::rename(&directory, &retired);
    #[cfg(windows)]
    assert!(
        renamed.is_err(),
        "Windows must pin the open directory against renames"
    );
    #[cfg(unix)]
    {
        // Unix keeps the descriptor pinned even if the public name moves.
        renamed.unwrap();
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("request.json"), b"replacement").unwrap();
        let current = layout.open_run(&directory, &run_id).unwrap();
        assert!(!pinned.same_directory(&current).unwrap());
    }
    assert_eq!(pinned.read("request.json", 32).unwrap().0, b"original");
}

#[test]
fn focused_request_selector_metadata_accepts_canonical_and_bounded_relay_handles() {
    // Selector grammar here establishes command recognition only. Authentic
    // mailbox membership and typed custody remain registration requirements.
    for selector in [
        "mailbox-message-11111111-1111-4111-8111-111111111111".to_owned(),
        "relay-message-QWJjMTIz_-".to_owned(),
        format!("relay-message-{}", "a".repeat(1024)),
    ] {
        let line = format!(
            "node scripts/test-launcher.mjs focused --input commit --request-message-id {selector} -- cargo test --bin termal"
        );
        let check = EngramHost::recognized_check(&line).expect("valid selected test command");
        assert_eq!(check.kind, EngramVerificationKind::Test);
        assert!(check.simple);
    }
}

#[test]
fn focused_request_selector_metadata_refuses_duplicates_missing_and_malformed_values() {
    let canonical = "mailbox-message-11111111-1111-4111-8111-111111111111";
    let options = [
        "--request-message-id".to_owned(),
        "--request-message-id ''".to_owned(),
        "--request-message-id mailbox-message-not-a-uuid".to_owned(),
        "--request-message-id relay-message-".to_owned(),
        "--request-message-id relay-message-abc+def".to_owned(),
        format!("--request-message-id relay-message-{}", "a".repeat(1025)),
        format!("--request-message-id {canonical} --request-message-id {canonical}"),
        format!("--request-message-id {canonical} --request-message-id relay-message-YWJj"),
    ];
    for option in options {
        let line = format!(
            "node scripts/test-launcher.mjs focused --input commit {option} -- cargo test --bin termal"
        );
        assert!(EngramHost::recognized_check(&line).is_none(), "{option}");
    }
}
