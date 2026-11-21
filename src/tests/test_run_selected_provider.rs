// Synthetic provider frames exercise the production facade with a selector-
// bearing command and real registration/settlement owners. These fixture facts
// are neither an executed remote test nor verification credit.
use super::*;

pub(in crate::tests) async fn selected_provider_completion<F, Fut>(
    state: AppState,
    mode: MailboxTestMode,
    select: F,
) where
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
    let cwd = test_run_display_path(&fs::canonicalize(&fixture.root).unwrap());
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
        rows[0].execution_id.clone()
    };

    let id = format!("test-{}", Uuid::new_v4());
    let mut request = full_request(
        &fs::canonicalize(&fixture.root).unwrap(),
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
    let directory = write_run(
        &fixture.runs_dir(),
        &id,
        request,
        Some(running_results(Some(717))),
    );
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
            output: &locator,
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
}

const PLACEMENT_LAUNCH: &str =
    "node scripts/test-launcher.mjs focused --input commit -- cargo test";

fn create_linked_checkout(root: &FsPath, name: &str) -> PathBuf {
    run_git_test_command(
        root,
        &[
            "-c",
            "user.name=Provider fixture",
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
    add_linked_checkout(root, name)
}

fn add_linked_checkout(root: &FsPath, name: &str) -> PathBuf {
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
    normalize_user_facing_path(&fs::canonicalize(linked).unwrap())
}

fn replace_linked_checkout(root: &FsPath, linked: &FsPath) {
    let original_admin = run_git_test_command_output(linked, &["rev-parse", "--absolute-git-dir"]);
    let moved = linked.with_file_name("provider-placement-moved");
    run_git_test_command(
        root,
        &[
            "worktree",
            "move",
            linked.to_str().unwrap(),
            moved.to_str().unwrap(),
        ],
    );
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
    assert_ne!(
        run_git_test_command_output(linked, &["rev-parse", "--absolute-git-dir"]),
        original_admin
    );
}

fn placement_provider_dispatch(fixture: &RunFixture, runner: &str) -> impl Fn(&str, Value) {
    const THREAD: &str = "placement-thread";
    const TURN: &str = "placement-turn";
    let (runtime, generation) = {
        let inner = fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(runner).unwrap()];
        let RuntimeToken::Codex(runtime) = record.runtime.runtime_token().unwrap() else {
            panic!("expected a Codex runtime");
        };
        (runtime, record.active_turn_generation)
    };
    let sessions = SharedCodexSessions::new();
    sessions.lock().unwrap().insert(
        runner.to_owned(),
        SharedCodexSessionState {
            thread_id: Some(THREAD.to_owned()),
            turn_id: Some(TURN.to_owned()),
            turn_started: true,
            active_turn_generation: Some(generation),
            ..SharedCodexSessionState::default()
        },
    );
    let threads = Arc::new(Mutex::new(HashMap::from([(
        THREAD.to_owned(),
        runner.to_owned(),
    )])));
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let (input, commands) = mpsc::channel();
    let state = fixture.state.clone();
    move |method, item| {
        let _keep_commands = &commands;
        let message = json!({
            "method": method,
            "params": {"threadId": THREAD, "turnId": TURN, "item": item},
        });
        handle_shared_codex_app_server_message(
            &message, &state, &runtime, &pending, &sessions, &threads, &input,
        )
        .unwrap();
    }
}

fn provider_placement_row(fixture: &RunFixture, key: &str) -> TestRunCommandObservation {
    let inner = fixture.state.inner.lock().unwrap();
    let tool = sha256_hex(key.as_bytes());
    let mut rows = inner
        .test_run_observations
        .values()
        .filter(|row| row.tool_identity == tool);
    let row = rows.next().expect("provider observed the original start");
    assert!(rows.next().is_none());
    row.clone()
}

#[tokio::test]
async fn linked_provider_cwd_associates_original_registered_native_completion() {
    exercise_linked_provider_completion(false).await;
}

/// A provider may report its cwd relative to the session workdir; a sibling
/// linked worktree is then reached through a parent step.
#[tokio::test]
async fn linked_provider_relative_cwd_associates_original_registered_native_completion() {
    exercise_linked_provider_completion(true).await;
}

#[cfg(unix)]
#[tokio::test]
async fn linked_provider_end_refuses_redirected_review_runs() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = RunFixture::new("linked-provider-redirected-runs");
    let linked = create_linked_checkout(&fixture.root, "provider-original-linked");
    let sibling = add_linked_checkout(&fixture.root, "provider-sibling-linked");
    let runner = running_owner(&fixture, "redirected-placement-runtime");
    let host = native_origin::bind_native_host(&fixture);
    let dispatch = placement_provider_dispatch(&fixture, &runner);
    let cwd = test_run_display_path(&linked);
    let key = "redirected-placement-tool";
    dispatch(
        "item/started",
        json!({"type":"commandExecution", "id":key, "command":PLACEMENT_LAUNCH,
            "cwd":cwd, "status":"inProgress"}),
    );
    let original = provider_placement_row(&fixture, key);
    assert_eq!(original.summary.state, TestRunState::Running);
    assert!(original.linked_placement);

    let original_runs = linked_review_runs_directory(&linked);
    let sibling_runs = linked_review_runs_directory(&sibling);
    fs::create_dir_all(&sibling_runs).unwrap();
    std::os::unix::fs::symlink(&sibling_runs, &original_runs).unwrap();
    assert_eq!(
        fs::canonicalize(&original_runs).unwrap(),
        fs::canonicalize(&sibling_runs).unwrap()
    );
    // The worktree's own Git layout still validates; only its run parent is
    // redirected.
    delegation_git_boundary::ProviderLinkedWorktree::resolve(&fixture.root, &linked).unwrap();

    let (id, directory, _) = register_linked_provider_artifact(&fixture, &runner, &linked).await;
    let registered = fixture.state.inner.lock().unwrap().test_run_registrations[&id].clone();
    assert_eq!(registered.native_origin.as_ref().unwrap().host, host);
    assert_eq!(
        FsPath::new(&registered.run_directory),
        fs::canonicalize(sibling_runs.join(&id)).unwrap()
    );
    dispatch(
        "item/completed",
        json!({"type":"commandExecution", "id":key, "command":PLACEMENT_LAUNCH, "cwd":cwd,
            "status":"completed", "exitCode":0,
            "aggregatedOutput":format!("RUN {}", test_run_display_path(&directory))}),
    );
    let observed = provider_placement_row(&fixture, key);
    assert_eq!(observed.execution_id, original.execution_id);
    assert!(observed.linked_placement);
    assert_eq!(observed.summary.state, TestRunState::Passed);
    fixture.refresh(&[]);
    let settled = fixture.state.inner.lock().unwrap().test_run_registrations[&id].clone();
    let durable = fixture
        .state
        .read_durable_native_test_run(&settled)
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            observed.artifact.is_some(),
            observed.native_completion_refusal.is_some(),
            durable.completion.is_some(),
        ),
        (false, true, false),
        "a redirected review-runs child must refuse association and durable completion"
    );
    let refusal = observed.native_completion_refusal.as_ref().unwrap();
    assert_eq!(
        refusal.code,
        TestRunNativeCompletionRefusalCode::ArtifactRootUnavailable
    );
    assert_eq!(
        refusal
            .observed
            .get(&TestRunNativeCompletionFact::ArtifactRootAvailable),
        Some(&false),
        "{refusal:?}"
    );
    assert!(settled.verification_record.is_none());
    observed_terminal::assert_no_credit(&fixture);
}

#[tokio::test]
async fn delayed_provider_description_refuses_replacement_linked_checkout() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = RunFixture::new("delayed-linked-provider-placement");
    let linked = create_linked_checkout(&fixture.root, "delayed-provider-linked");
    let host = native_origin::bind_native_host(&fixture);
    let runner = owner(&fixture);
    let (runtime, _runtime_rx) = test_acp_runtime_handle(AcpAgent::Cursor, "delayed-placement");
    let input = runtime.input_tx.clone();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&runner).unwrap();
        let record = &mut inner.sessions[index];
        record.session.agent = Agent::Cursor;
        record.runtime = SessionRuntime::Acp(runtime);
    }
    let mut recorder = SessionRecorder::new(fixture.state.clone(), runner.clone());
    let mut turn = AcpTurnState::default();
    let mut dispatch = |update| {
        handle_acp_session_update(
            &update,
            &fixture.state,
            &runner,
            &input,
            &mut turn,
            &mut recorder,
            AcpAgent::Cursor,
        )
        .unwrap();
    };
    let key = "delayed-placement-tool";
    let cwd = test_run_display_path(&linked);
    dispatch(json!({
        "sessionUpdate":"tool_call", "toolCallId":key, "title":"Run check",
        "status":"pending", "rawInput":{"cwd":cwd},
    }));
    let (original_id, original_runtime, original_started_at) = {
        let inner = fixture.state.inner.lock().unwrap();
        assert!(inner.test_run_observations.is_empty());
        let pending = inner
            .test_runs
            .unclassified_commands
            .values()
            .next()
            .unwrap();
        assert_eq!(pending.directories, Some(vec![Some(cwd)]));
        assert!(pending.origin.validated_unbound);
        (
            pending.origin.execution_id(),
            pending.origin.runtime_identity.clone(),
            pending.started_at.clone(),
        )
    };
    replace_linked_checkout(&fixture.root, &linked);
    let (id, directory, _) = register_linked_provider_artifact(&fixture, &runner, &linked).await;

    dispatch(json!({
        "sessionUpdate":"tool_call_update", "toolCallId":key, "status":"completed",
        "rawInput":{"command":PLACEMENT_LAUNCH},
        "rawOutput":{"exitCode":0, "stdout":format!("RUN {}", test_run_display_path(&directory))},
    }));

    let observed = provider_placement_row(&fixture, key);
    assert_eq!(observed.execution_id, original_id);
    assert_eq!(observed.runtime_identity, original_runtime);
    assert_eq!(
        observed.summary.started_at.as_deref(),
        Some(original_started_at.as_str())
    );
    assert_eq!(observed.summary.state, TestRunState::Passed);
    assert!(observed.summary.worktree.is_empty());
    assert!(!observed.linked_placement);
    assert!(observed.artifact.is_none());
    assert_eq!(
        observed.native_completion_refusal.as_ref().unwrap().code,
        TestRunNativeCompletionRefusalCode::ProviderPlacementUnconfirmed
    );
    fixture.refresh(&[]);
    let registration = fixture.state.inner.lock().unwrap().test_run_registrations[&id].clone();
    assert_eq!(registration.native_origin.as_ref().unwrap().host, host);
    assert!(registration.native_completion.is_none());
    assert!(registration.verification_record.is_none());
    let durable = fixture
        .state
        .read_durable_native_test_run(&registration)
        .unwrap()
        .unwrap();
    assert!(durable.completion.is_none());
    observed_terminal::assert_no_credit(&fixture);
}

async fn exercise_linked_provider_completion(relative_cwd: bool) {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = RunFixture::new("linked-provider-placement");
    let runner = running_owner(&fixture, "placement-runtime");
    let linked = create_linked_checkout(&fixture.root, "provider-placement-linked");
    let host = native_origin::bind_native_host(&fixture);
    let dispatch = placement_provider_dispatch(&fixture, &runner);
    let cwd = if relative_cwd {
        // The linked checkout is the session root's sibling.
        format!("../{}", linked.file_name().unwrap().to_str().unwrap())
    } else {
        test_run_display_path(&linked)
    };
    let key = "linked-placement-tool";
    dispatch(
        "item/started",
        json!({"type":"commandExecution", "id":key, "command":PLACEMENT_LAUNCH,
            "cwd":cwd, "status":"inProgress"}),
    );
    let original = provider_placement_row(&fixture, key);
    assert_eq!(original.summary.state, TestRunState::Running);
    assert!(original.runtime_identity.is_some());
    assert!(original.artifact.is_none());
    assert!(original.work_binding.is_none());
    assert!(original.source_root.is_none());
    {
        let inner = fixture.state.inner.lock().unwrap();
        let session = &inner.sessions[inner.find_session_index(&runner).unwrap()];
        assert_eq!(
            fs::canonicalize(&session.session.workdir).unwrap(),
            fs::canonicalize(&fixture.root).unwrap()
        );
        assert_ne!(
            test_run_path_key(&session.session.workdir),
            test_run_path_key(&cwd)
        );
        assert!(session.engram.active_turn_checks.is_empty());
        assert!(
            inner
                .projects
                .iter()
                .any(|project| project.id == fixture.project_id && project.engram.is_none())
        );
    }
    assert!(original.linked_placement);

    let (id, directory, source) =
        register_linked_provider_artifact(&fixture, &runner, &linked).await;
    let registration = fixture.state.inner.lock().unwrap().test_run_registrations[&id].clone();
    assert_eq!(registration.native_origin.as_ref().unwrap().host, host);
    assert!(registration.native_completion.is_none());

    dispatch(
        "item/completed",
        json!({"type":"commandExecution", "id":key, "command":PLACEMENT_LAUNCH, "cwd":cwd,
            "status":"completed", "exitCode":0,
            "aggregatedOutput":format!("RUN {}", test_run_display_path(&directory))}),
    );
    let observed = provider_placement_row(&fixture, key);
    assert_eq!(observed.execution_id, original.execution_id);
    assert_eq!(observed.runtime_identity, original.runtime_identity);
    assert_eq!(observed.summary.state, TestRunState::Passed);
    assert!(observed.linked_placement);
    let artifact = observed.artifact.as_ref().unwrap_or_else(|| {
        panic!(
            "the original provider END must attach the launcher artifact (worktree {:?})",
            observed.summary.worktree
        )
    });
    // The observation records the resolved linked worktree, never the
    // provider's raw cwd, which may be relative to the session workdir.
    assert_eq!(
        test_run_path_key(&original.summary.worktree),
        test_run_path_key(&test_run_display_path(&linked))
    );
    assert_eq!(artifact.run_id, id);
    assert_eq!(
        artifact.request_identity.as_ref(),
        Some(&registration.request_identity)
    );
    fixture.refresh(&[]);
    let settled = fixture.state.inner.lock().unwrap().test_run_registrations[&id].clone();
    let completion = settled
        .native_completion
        .as_ref()
        .expect("native producer settled");
    assert_eq!(completion.execution_id, original.execution_id);
    assert_eq!(completion.completion.run_id, id);
    assert_eq!(
        completion.completion.result.verdict,
        native_test_completion::Verdict::Pass
    );
    let native_test_completion::Input::SourceId(input) = &completion.completion.input else {
        panic!("original native result must retain SourceId");
    };
    assert_eq!(
        serde_json::to_value(&input.source_id_before).unwrap(),
        source
    );
    assert_eq!(input.source_id_before, input.source_id_after);
    let durable = fixture
        .state
        .read_durable_native_test_run(&settled)
        .unwrap()
        .unwrap();
    assert_eq!(durable.completion.as_ref(), Some(&completion.completion));
    assert!(settled.verification_record.is_none());
    observed_terminal::assert_no_credit(&fixture);
}

async fn register_linked_provider_artifact(
    fixture: &RunFixture,
    runner: &str,
    linked: &FsPath,
) -> (String, PathBuf, Value) {
    let id = format!("test-{}", Uuid::new_v4());
    let mut request = full_request(linked, &test_run_timestamp());
    request["owner"] = json!(runner);
    request["creatorPid"] = json!(std::process::id());
    request["detached"] = json!(false);
    request["input"] = json!("commit");
    request["full"] = json!(false);
    request["stages"] = json!([{"name":"focused"}]);
    let directory = write_run(
        &linked_review_runs_directory(linked),
        &id,
        request,
        Some(running_results(Some(717))),
    );
    let (status, ack): (StatusCode, Value) = request_json(
        &app_router(fixture.state.clone()),
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{runner}/test-runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"runId":id, "runDirectory":test_run_display_path(&directory)}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{ack}");
    assert_eq!(ack["registered"], true);
    let source = json!({
        "commit": run_git_test_command_output(linked, &["rev-parse", "HEAD"]),
        "tree": run_git_test_command_output(linked, &["rev-parse", "HEAD^{tree}"]),
        "submodules": [], "changes": [],
    });
    let mut results = passed_results();
    results["runId"] = json!(id);
    results["ended"] = json!(test_run_timestamp());
    results["sourceIdBefore"] = source.clone();
    results["sourceIdAfter"] = source.clone();
    results["stages"] = json!([{"name":"focused","state":"passed","code":0,
        "tests":{"passed":1,"failed":0,"ignored":0}}]);
    fs::write(directory.join("results.json"), results.to_string()).unwrap();
    (id, directory, source)
}

fn linked_review_runs_directory(linked: &FsPath) -> PathBuf {
    PathBuf::from(run_git_test_command_output(
        linked,
        &["rev-parse", "--absolute-git-dir"],
    ))
    .join("review-runs")
}

async fn assert_refused_provider_placement(fixture: &RunFixture, cwd: &FsPath) {
    let runner = running_owner(fixture, "refused-placement-runtime");
    let dispatch = placement_provider_dispatch(fixture, &runner);
    let cwd = test_run_display_path(cwd);
    let key = "refused-placement-tool";
    dispatch(
        "item/started",
        json!({"type":"commandExecution", "id":key, "command":PLACEMENT_LAUNCH,
            "cwd":cwd, "status":"inProgress"}),
    );
    let start = provider_placement_row(fixture, key);
    assert!(start.summary.worktree.is_empty());
    let refusal = start
        .native_completion_refusal
        .as_ref()
        .expect("placement is refused at START");
    assert_eq!(
        refusal.code,
        TestRunNativeCompletionRefusalCode::ProviderPlacementUnconfirmed
    );
    dispatch(
        "item/completed",
        json!({"type":"commandExecution", "id":key, "command":PLACEMENT_LAUNCH,
        "cwd":cwd, "status":"completed", "exitCode":0, "aggregatedOutput":format!(
                "RUN {}", test_run_display_path(&fixture.runs_dir().join(format!("test-{}", Uuid::new_v4())))
        )}),
    );
    let end = provider_placement_row(fixture, key);
    assert_eq!(end.execution_id, start.execution_id);
    assert_eq!(end.summary.exit_code, Some(0));
    assert!(end.artifact.is_none());
    assert_eq!(end.native_completion_refusal.as_ref(), Some(refusal));
    let (status, detail): (StatusCode, Value) = request_json(
        &app_router(fixture.state.clone()),
        Request::builder()
            .uri(format!("/api/test-runs/{}", end.execution_id))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(
        detail["detail"]["nativeCompletionRefusal"],
        serde_json::to_value(refusal).unwrap()
    );
    observed_terminal::assert_no_credit(fixture);
}

#[tokio::test]
async fn provider_cwd_outside_session_repository_keeps_start_refusal_after_launcher_end() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = RunFixture::new("outside-provider-placement");
    let outside = fixture.root.with_file_name("provider-placement-no-git");
    fs::create_dir_all(&outside).unwrap();
    assert_refused_provider_placement(&fixture, &outside).await;
}

#[tokio::test]
async fn provider_cwd_in_foreign_linked_checkout_keeps_start_refusal_after_launcher_end() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = RunFixture::new("foreign-provider-placement");
    let foreign = fixture.root.with_file_name("provider-placement-other-repo");
    fs::create_dir_all(&foreign).unwrap();
    run_git_test_command(&foreign, &["init", "--quiet"]);
    let linked = create_linked_checkout(&foreign, "provider-placement-other-linked");
    assert_refused_provider_placement(&fixture, &linked).await;
}

#[cfg(unix)]
#[tokio::test]
async fn linked_provider_refuses_git_pointer_with_link_before_parent_step() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = RunFixture::new("linked-provider-pointer-components");
    let linked = create_linked_checkout(&fixture.root, "provider-pointer-linked");
    let admin = PathBuf::from(run_git_test_command_output(
        &linked,
        &["rev-parse", "--absolute-git-dir"],
    ));
    assert!(
        delegation_git_boundary::ProviderLinkedWorktree::resolve(&fixture.root, &linked).is_ok()
    );
    let alias = admin.with_file_name("fixture-link");
    std::os::unix::fs::symlink(&linked, &alias).unwrap();
    let pointer = alias.join("..").join(admin.file_name().unwrap());
    fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", pointer.display()),
    )
    .unwrap();
    assert_refused_provider_placement(&fixture, &linked).await;
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
