// Synthetic provider frames exercise the production facade with a selector-
// bearing command and real registration/settlement owners. These fixture facts
// are neither an executed remote test nor verification credit.
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::tests) enum SelectedProviderWorktree {
    Session,
    LinkedStart,
    LinkedArtifact,
    SessionStream,
    LinkedStream,
}

pub(super) const PROVIDER_THREAD: &str = "selected-provider-thread";
pub(super) const PROVIDER_TURN: &str = "selected-provider-turn";

// Use the ordinary native event router with disposable session maps; this
// supplies provider frames, never a replacement observation or completion.
pub(super) fn codex_provider_dispatch(
    state: AppState,
    runner: String,
    provenance: EngramObservationProvenance,
    turn_id: &str,
) -> impl Fn(Value) {
    codex_provider_dispatch_for_turn(state, runner, provenance, Some(turn_id))
}

pub(super) fn codex_provider_dispatch_for_turn(
    state: AppState,
    runner: String,
    provenance: EngramObservationProvenance,
    turn_id: Option<&str>,
) -> impl Fn(Value) {
    codex_provider_dispatch_with_handshake(state, runner, provenance, turn_id, false)
}

pub(super) fn codex_provider_dispatch_with_armed_idless_turn(
    state: AppState,
    runner: String,
    provenance: EngramObservationProvenance,
) -> impl Fn(Value) {
    codex_provider_dispatch_with_handshake(state, runner, provenance, None, true)
}

fn codex_provider_dispatch_with_handshake(
    state: AppState,
    runner: String,
    provenance: EngramObservationProvenance,
    turn_id: Option<&str>,
    response_already_armed: bool,
) -> impl Fn(Value) {
    let EngramObservationProvenance::Runtime {
        token: RuntimeToken::Codex(runtime),
        turn_generation,
    } = provenance
    else {
        panic!("Codex provider fixture")
    };
    let sessions = SharedCodexSessions::new();
    let (cancel_watchdog, watchdog) = mpsc::channel();
    sessions.lock().unwrap().insert(
        runner.clone(),
        SharedCodexSessionState {
            thread_id: Some(PROVIDER_THREAD.to_owned()),
            turn_id: turn_id.map(str::to_owned),
            turn_started: turn_id.is_some(),
            pending_turn_start_request_id: turn_id.is_none().then(|| "open-turn".to_owned()),
            turn_started_watchdog_cancel_tx: response_already_armed.then_some(cancel_watchdog),
            active_turn_generation: Some(turn_generation),
            ..SharedCodexSessionState::default()
        },
    );
    let threads = Arc::new(Mutex::new(HashMap::from([(
        PROVIDER_THREAD.to_owned(),
        runner,
    )])));
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let (input, commands) = mpsc::channel();
    move |message: Value| {
        let _keep_channels = (&commands, &watchdog);
        handle_shared_codex_app_server_message(
            &message, &state, &runtime, &pending, &sessions, &threads, &input,
        )
        .unwrap();
    }
}

pub(in crate::tests) async fn selected_provider_completion<F, Fut>(
    state: AppState,
    mode: MailboxTestMode,
    placement: SelectedProviderWorktree,
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
    let command_root = if matches!(
        placement,
        SelectedProviderWorktree::Session | SelectedProviderWorktree::SessionStream
    ) {
        fixture.root.clone()
    } else {
        run_git_test_command(
            &fixture.root,
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
        let linked = fixture.root.with_file_name("selected-provider-linked");
        run_git_test_command(
            &fixture.root,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                linked.to_str().unwrap(),
                "HEAD",
            ],
        );
        assert_ne!(
            fs::canonicalize(&fixture.root).unwrap(),
            fs::canonicalize(&linked).unwrap(),
        );
        assert_eq!(
            fs::canonicalize(test_runs_git_common_dir(&fixture.root).unwrap()).unwrap(),
            fs::canonicalize(test_runs_git_common_dir(&linked).unwrap()).unwrap(),
            "the explicit command cwd is another worktree of the session's repository",
        );
        linked
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
    let cwd = test_run_display_path(&fs::canonicalize(&command_root).unwrap());
    let key = "selected-provider-tool";
    fixture.state.engram_host().observe(
        &runner,
        &provenance,
        EngramRecorderObservation::CommandStarted {
            key,
            ran: Some(&command),
            cwd: Some(&cwd),
        },
    );
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
        if !matches!(
            placement,
            SelectedProviderWorktree::Session | SelectedProviderWorktree::SessionStream
        ) {
            assert!(rows[0].work_binding.is_none());
            assert!(rows[0].source_root.is_none());
            assert!(rows[0].grant_identity.is_none());
            assert_ne!(
                test_run_path_key(
                    &inner.sessions[inner.find_session_index(&runner).unwrap()]
                        .session
                        .workdir
                ),
                test_run_path_key(&cwd),
            );
        }
        if placement == SelectedProviderWorktree::LinkedStart {
            assert_eq!(
                test_run_path_key(&rows[0].summary.worktree),
                test_run_path_key(&cwd),
                "provider visibility retains the actual linked worktree without granting local credit",
            );
        }
        rows[0].execution_id.clone()
    };

    let id = format!("test-{}", Uuid::new_v4());
    let mut request = full_request(
        &fs::canonicalize(&command_root).unwrap(),
        &test_run_timestamp(),
    );
    request["owner"] = json!(runner);
    request["creatorPid"] = json!(std::process::id());
    request["detached"] = json!(false);
    request["input"] = json!("commit");
    request["requestMessageId"] = json!(selector);
    request["full"] = json!(mode == MailboxTestMode::Full);
    if mode == MailboxTestMode::Focused {
        request["stages"] = json!([{ "name": "focused" }]);
    }
    let runs_dir = if placement == SelectedProviderWorktree::Session {
        fixture.runs_dir()
    } else {
        EngramHost::run_artifact_directory(&command_root).unwrap()
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
    let streaming = matches!(
        placement,
        SelectedProviderWorktree::SessionStream | SelectedProviderWorktree::LinkedStream
    );
    if streaming {
        let EngramObservationProvenance::Runtime {
            token,
            turn_generation,
        } = &provenance
        else {
            panic!("runtime-owned provider fixture");
        };
        let start = CodexLauncherStart {
            native_turn_id: PROVIDER_TURN,
            key,
            line: &command,
            cwd: Some(&cwd),
        };
        assert!(
            fixture
                .state
                .capture_codex_launcher_output(&runner, token, Some(turn_generation + 1), start,)
                .is_none()
        );
        assert!(
            fixture
                .state
                .capture_codex_launcher_output(
                    &runner,
                    token,
                    Some(*turn_generation),
                    CodexLauncherStart {
                        key: "another-item",
                        ..start
                    },
                )
                .is_none()
        );
        let mut invalid = fixture
            .state
            .capture_codex_launcher_output(&runner, token, Some(*turn_generation), start)
            .expect("original unbound launcher stream");
        fixture.state.observe_codex_launcher_output(
            &runner,
            token,
            Some(*turn_generation),
            &mut invalid,
            &"x".repeat(256 * 1024 + 1),
        );
        assert!(
            fixture.state.inner.lock().unwrap().test_run_observations[&observed]
                .artifact
                .is_none()
        );
        let mut output = fixture
            .state
            .capture_codex_launcher_output(&runner, token, Some(*turn_generation), start)
            .unwrap();
        // Rejected ingress must not contribute even a partial line to the
        // original stream. A later genuine split RUN still has to associate.
        let before = fixture.state.inner.lock().unwrap().test_run_observations[&observed].clone();
        fixture.state.observe_codex_launcher_output(
            &runner,
            token,
            Some(turn_generation + 1),
            &mut output,
            "stale prefix ",
        );
        fixture.state.observe_codex_launcher_output(
            &runner,
            &RuntimeToken::Codex("another-runtime".to_owned()),
            Some(*turn_generation),
            &mut output,
            "foreign prefix ",
        );
        assert_eq!(
            fixture.state.inner.lock().unwrap().test_run_observations[&observed],
            before
        );
        if placement == SelectedProviderWorktree::LinkedStream {
            // The linked case also traverses the real native Codex event
            // router. Duplicate starts must preserve the original buffer as
            // the same observation gains its artifact.
            let dispatch = codex_provider_dispatch(
                fixture.state.clone(),
                runner.clone(),
                provenance.clone(),
                PROVIDER_TURN,
            );
            let start = json!({"method":"item/started","params":{
                "threadId":PROVIDER_THREAD,"turnId":PROVIDER_TURN,
                "item":{"type":"commandExecution","id":key,"command":command,
                    "cwd":cwd,"status":"inProgress"}}});
            dispatch(start.clone());
            let mut delta = json!({"method":"item/commandExecution/outputDelta","params":{
                "threadId":PROVIDER_THREAD,"turnId":PROVIDER_TURN,"itemId":key,"delta":"RU"}});
            for (field, wrong) in [
                ("threadId", "another-thread"),
                ("turnId", "another-turn"),
                ("itemId", "another-item"),
            ] {
                let mut foreign = delta.clone();
                foreign["params"][field] = json!(wrong);
                foreign["params"]["delta"] = json!("foreign prefix ");
                dispatch(foreign);
            }
            dispatch(delta.clone());
            dispatch(start.clone());
            assert!(
                fixture.state.inner.lock().unwrap().test_run_observations[&observed]
                    .artifact
                    .is_none()
            );
            delta["params"]["delta"] = json!(format!("{}\n", &locator[2..]));
            dispatch(delta.clone());
            let original_artifact = fixture.state.inner.lock().unwrap().test_run_observations
                [&observed]
                .artifact
                .clone();
            assert!(original_artifact.is_some());
            dispatch(start);
            delta["params"]["delta"] = json!(format!("{locator}\n"));
            dispatch(delta);
            assert_eq!(
                fixture.state.inner.lock().unwrap().test_run_observations[&observed].artifact,
                original_artifact
            );
        } else {
            fixture.state.observe_codex_launcher_output(
                &runner,
                token,
                Some(*turn_generation),
                &mut output,
                "RU",
            );
            assert!(
                fixture.state.inner.lock().unwrap().test_run_observations[&observed]
                    .artifact
                    .is_none()
            );
            fixture.state.observe_codex_launcher_output(
                &runner,
                token,
                Some(*turn_generation),
                &mut output,
                &format!("{}\n", &locator[2..]),
            );
        }
        let inner = fixture.state.inner.lock().unwrap();
        assert!(
            inner.sessions[inner.find_session_index(&runner).unwrap()]
                .engram
                .active_turn_checks
                .is_empty(),
            "streaming visibility cannot manufacture a mediated check"
        );
        let row = &inner.test_run_observations[&observed];
        assert!(
            row.work_binding.is_none() && row.source_root.is_none() && row.grant_identity.is_none()
        );
    }
    fixture.state.engram_host().observe(
        &runner,
        &provenance,
        EngramRecorderObservation::CommandFinished {
            key,
            command: &command,
            reported_command: Some(&command),
            output: if streaming { "" } else { &locator },
            exit: Some(EngramCommandExit::NotFinished),
            runtime_output_cut: false,
        },
    );
    {
        let inner = fixture.state.inner.lock().unwrap();
        let row = &inner.test_run_observations[&observed];
        let artifact = row.artifact.as_ref().expect(
            "the original RUN locator must attach the linked worktree's exact request artifact",
        );
        assert_eq!(artifact.run_id, id);
        assert_eq!(
            artifact.run_directory,
            test_run_display_path(&fs::canonicalize(&directory).unwrap())
        );
        assert_eq!(
            artifact.request_identity,
            Some(inner.test_run_registrations[&id].request_identity.clone())
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
    fixture.state.engram_host().observe(
        &runner,
        &provenance,
        EngramRecorderObservation::CommandFinished {
            key,
            command: &command,
            reported_command: Some(&command),
            output: if streaming { "" } else { &locator },
            exit: Some(EngramCommandExit::Code(0)),
            runtime_output_cut: false,
        },
    );
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
    registration
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
