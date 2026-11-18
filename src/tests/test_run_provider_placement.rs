// Negative placement controls use real provider starts and durable public
// observations. Only disposable repositories below the fixture root are used.
use super::selected_provider::{PROVIDER_THREAD, PROVIDER_TURN, codex_provider_dispatch};
use super::*;

#[path = "test_run_provider_placement_compaction.rs"]
mod refusal_history;

#[test]
fn a_conflicting_codex_item_start_cannot_lend_its_original_launcher_stream() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for changed in [
        Some("cargo test --bin termal"),
        Some("node scripts/test-launcher.mjs full --input commit"),
        None,
    ] {
        let fixture = RunFixture::new("provider-conflicting-start");
        let session = running_owner(&fixture, "conflicting-start-runtime");
        let provenance = {
            let mut inner = fixture.state.inner.lock().unwrap();
            inner
                .projects
                .iter_mut()
                .find(|project| project.id == fixture.project_id)
                .unwrap()
                .engram = Some(EngramProjectSettings::default());
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            EngramObservationProvenance::Runtime {
                token: record.runtime.runtime_token().unwrap(),
                turn_generation: record.active_turn_generation,
            }
        };
        let command =
            "node scripts/test-launcher.mjs focused --input commit -- cargo test --bin termal";
        let cwd = test_run_display_path(&fs::canonicalize(&fixture.root).unwrap());
        let dispatch = codex_provider_dispatch(
            fixture.state.clone(),
            session.clone(),
            provenance,
            PROVIDER_TURN,
        );
        let mut start = json!({"method":"item/started","params":{
            "threadId":PROVIDER_THREAD,"turnId":PROVIDER_TURN,
            "item":{"type":"commandExecution","id":"original","command":command,
                "cwd":cwd,"status":"inProgress"}}});
        dispatch(start.clone());
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
        assert!(!original.summary.worktree.is_empty());
        let id = format!("test-{}", Uuid::new_v4());
        let mut request = full_request(&fixture.root, &test_run_timestamp());
        request["owner"] = json!(session);
        request["creatorPid"] = json!(std::process::id());
        request["detached"] = json!(false);
        request["input"] = json!("commit");
        request["full"] = json!(false);
        request["stages"] = json!([{"name":"focused"}]);
        let directory = write_run(
            &fixture.runs_dir(),
            &id,
            request,
            Some(running_results(Some(717))),
        );
        fixture
            .state
            .register_test_run(&session, registration_request(&id, &directory))
            .unwrap();
        if let Some(changed) = changed {
            start["params"]["item"]["command"] = json!(changed);
        } else {
            start["params"]["item"]
                .as_object_mut()
                .unwrap()
                .remove("command");
        }
        dispatch(start);
        dispatch(
            json!({"method":"item/commandExecution/outputDelta","params":{
            "threadId":PROVIDER_THREAD,"turnId":PROVIDER_TURN,"itemId":"original",
            "delta":format!("RUN {}\n", test_run_display_path(&directory))}}),
        );
        let inner = fixture.state.inner.lock().unwrap();
        assert_eq!(inner.test_run_observations.len(), 1);
        assert_eq!(
            inner.test_run_observations[&original.execution_id], original,
            "a reused item with changed or absent command cannot modify the original execution"
        );
        assert!(
            inner.test_run_registrations[&id]
                .native_completion
                .is_none()
        );
        assert!(
            inner.test_run_registrations[&id]
                .verification_record
                .is_none()
        );
    }
}

#[test]
fn codex_stream_custody_refuses_a_repeated_start_with_changed_cwd() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for change_cwd in [false, true] {
        let fixture = RunFixture::new("stream-cwd");
        let [original_root, other_root] =
            linked_stream_worktrees(&fixture, ["stream-a", "stream-b"]);
        let session = running_owner(&fixture, "stream-cwd-runtime");
        let origin = stream_turn(&fixture, &session, 1);
        let dispatch = codex_provider_dispatch(
            fixture.state.clone(),
            session.clone(),
            origin,
            PROVIDER_TURN,
        );
        let start = stream_start(PROVIDER_TURN, &original_root);
        dispatch(start.clone());
        let original = stream_observation(&fixture, 1);
        assert_eq!(
            test_run_path_key(&original.summary.worktree),
            test_run_path_key(&test_run_display_path(&original_root))
        );
        assert!(original.artifact.is_none());
        let expected = register_stream_run(&fixture, &session, &original_root);

        dispatch(stream_delta(PROVIDER_TURN, "RU"));
        let mut repeated = start;
        if change_cwd {
            repeated["params"]["item"]["cwd"] = json!(test_run_display_path(&other_root));
        }
        dispatch(repeated);
        assert_eq!(stream_observation(&fixture, 1), original);
        dispatch(stream_delta(
            PROVIDER_TURN,
            &format!("N {}\n", expected.run_directory),
        ));

        let observed = stream_observation(&fixture, 1);
        if change_cwd {
            assert_eq!(
                observed, original,
                "a conflicting explicit cwd must invalidate the pending split RUN"
            );
        } else {
            let mut expected_row = original;
            expected_row.artifact = Some(expected);
            assert_eq!(
                observed, expected_row,
                "an unchanged cwd must preserve the split RUN and attach its exact artifact"
            );
        }
    }
}

#[test]
fn codex_stream_custody_uses_the_current_execution_when_an_older_turn_reused_the_item() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = RunFixture::new("stream-turn");
    let [worktree] = linked_stream_worktrees(&fixture, ["stream-a"]);
    let session = running_owner(&fixture, "stream-turn-runtime");
    let prior_origin = stream_turn(&fixture, &session, 1);
    let start = stream_start(PROVIDER_TURN, &worktree);
    let prior_dispatch = codex_provider_dispatch(
        fixture.state.clone(),
        session.clone(),
        prior_origin,
        PROVIDER_TURN,
    );
    prior_dispatch(start.clone());
    let mut finished = start;
    finished["method"] = json!("item/completed");
    finished["params"]["item"]["status"] = json!("completed");
    finished["params"]["item"]["exitCode"] = json!(0);
    finished["params"]["item"]["aggregatedOutput"] = json!("");
    prior_dispatch(finished);
    let prior = stream_observation(&fixture, 1);
    assert!(prior.summary.ended_at.is_some() && prior.artifact.is_none());

    let next_turn = "stream-successor-turn";
    let current_origin = stream_turn(&fixture, &session, 2);
    let dispatch = codex_provider_dispatch(
        fixture.state.clone(),
        session.clone(),
        current_origin,
        next_turn,
    );
    dispatch(stream_start(next_turn, &worktree));
    let current = stream_observation(&fixture, 2);
    assert_ne!(current.execution_id, prior.execution_id);
    assert_eq!(current.runtime_identity, prior.runtime_identity);
    assert_eq!(current.tool_identity, prior.tool_identity);
    assert_eq!(current.command_fingerprint, prior.command_fingerprint);
    assert!(current.artifact.is_none() && current.summary.ended_at.is_none());
    assert_eq!(stream_observation(&fixture, 1), prior);
    let artifact = register_stream_run(&fixture, &session, &worktree);
    dispatch(stream_delta(next_turn, "RUN "));
    dispatch(stream_delta(
        next_turn,
        &format!("{}\n", artifact.run_directory),
    ));

    assert_eq!(stream_observation(&fixture, 1), prior);
    let mut expected = current;
    expected.artifact = Some(artifact);
    assert_eq!(
        stream_observation(&fixture, 2),
        expected,
        "the retained prior turn must not prevent the current execution attaching its RUN"
    );
}

const STREAM_ITEM: &str = "stream-custody-item";
const STREAM_LAUNCHER: &str =
    "node scripts/test-launcher.mjs focused --input commit -- cargo test --bin termal";

fn stream_start(turn: &str, cwd: &FsPath) -> Value {
    json!({"method":"item/started","params":{
        "threadId":PROVIDER_THREAD,"turnId":turn,
        "item":{"type":"commandExecution","id":STREAM_ITEM,"command":STREAM_LAUNCHER,
            "cwd":test_run_display_path(cwd),"status":"inProgress"}}})
}

fn stream_delta(turn: &str, delta: &str) -> Value {
    json!({"method":"item/commandExecution/outputDelta","params":{
        "threadId":PROVIDER_THREAD,"turnId":turn,"itemId":STREAM_ITEM,"delta":delta}})
}

fn stream_turn(
    fixture: &RunFixture,
    session: &str,
    generation: u64,
) -> EngramObservationProvenance {
    let mut inner = fixture.state.inner.lock().unwrap();
    inner
        .projects
        .iter_mut()
        .find(|project| project.id == fixture.project_id)
        .unwrap()
        .engram = Some(EngramProjectSettings::default());
    let index = inner.find_session_index(session).unwrap();
    let record = &mut inner.sessions[index];
    record.active_turn_generation = generation;
    EngramObservationProvenance::Runtime {
        token: record.runtime.runtime_token().unwrap(),
        turn_generation: generation,
    }
}

fn stream_observation(fixture: &RunFixture, generation: u64) -> TestRunCommandObservation {
    let inner = fixture.state.inner.lock().unwrap();
    let mut rows = inner
        .test_run_observations
        .values()
        .filter(|row| row.turn_generation == generation);
    let row = rows.next().unwrap().clone();
    assert!(rows.next().is_none(), "exactly one start per turn");
    assert!(
        row.work_binding.is_none() && row.source_root.is_none() && row.grant_identity.is_none()
    );
    assert!(row.placement_refusal.is_none() && row.evidence.is_empty());
    row
}

fn linked_stream_worktrees<const N: usize>(fixture: &RunFixture, names: [&str; N]) -> [PathBuf; N] {
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
    names.map(|name| {
        let path = fixture.root.with_file_name(name);
        run_git_test_command(
            &fixture.root,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                path.to_str().unwrap(),
                "HEAD",
            ],
        );
        fs::canonicalize(path).unwrap()
    })
}

fn register_stream_run(
    fixture: &RunFixture,
    session: &str,
    cwd: &FsPath,
) -> TestRunObservedArtifact {
    let id = format!("test-{}", Uuid::new_v4());
    let mut request = full_request(cwd, &test_run_timestamp());
    request["owner"] = json!(session);
    request["creatorPid"] = json!(std::process::id());
    request["detached"] = json!(false);
    request["input"] = json!("commit");
    request["full"] = json!(false);
    request["stages"] = json!([{"name":"focused"}]);
    let directory = write_run(
        &EngramHost::run_artifact_directory(cwd).unwrap(),
        &id,
        request,
        Some(running_results(Some(717))),
    );
    fixture
        .state
        .register_test_run(session, registration_request(&id, &directory))
        .unwrap();
    let inner = fixture.state.inner.lock().unwrap();
    let registration = &inner.test_run_registrations[&id];
    assert!(registration.native_completion.is_none() && registration.verification_record.is_none());
    TestRunObservedArtifact {
        run_id: id,
        run_directory: test_run_display_path(&fs::canonicalize(directory).unwrap()),
        request_identity: Some(registration.request_identity.clone()),
        terminal_digest: None,
    }
}

#[test]
fn foreign_and_copied_worktree_placements_report_the_deciding_check_without_credit() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for case in ["foreign", "copied", "arguments"] {
        let fixture = RunFixture::new("provider-refused-placement");
        let session = running_owner(&fixture, "placement-runtime");
        let outside = fixture.root.with_file_name("provider-outside");
        fs::create_dir_all(&outside).unwrap();
        let expected = if case == "copied" {
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
                    "fixture",
                ],
            );
            let linked = fixture.root.with_file_name("provider-registered-linked");
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
            fs::copy(linked.join(".git"), outside.join(".git")).unwrap();
            TestRunPlacementRefusal::UnregisteredWorktree
        } else if case == "foreign" {
            run_git_test_command(&outside, &["init", "--quiet"]);
            TestRunPlacementRefusal::ForeignRepository
        } else {
            TestRunPlacementRefusal::CommandTargetUnconfirmed
        };
        let provenance = {
            let mut inner = fixture.state.inner.lock().unwrap();
            inner
                .projects
                .iter_mut()
                .find(|project| project.id == fixture.project_id)
                .unwrap()
                .engram = Some(EngramProjectSettings::default());
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            EngramObservationProvenance::Runtime {
                token: record.runtime.runtime_token().unwrap(),
                turn_generation: record.active_turn_generation,
            }
        };
        let command = if case == "arguments" {
            format!(
                "node scripts/test-launcher.mjs focused --input commit -- cargo test --manifest-path {}",
                test_run_display_path(&outside.join("Cargo.toml"))
            )
        } else {
            "node scripts/test-launcher.mjs focused --input commit -- cargo test --bin termal"
                .to_owned()
        };
        let command = command.as_str();
        let cwd = test_run_display_path(
            &fs::canonicalize(if case == "arguments" {
                &fixture.root
            } else {
                &outside
            })
            .unwrap(),
        );
        fixture.state.engram_host().observe(
            &session,
            &provenance,
            EngramRecorderObservation::CommandStarted {
                key: "outside",
                ran: Some(command),
                cwd: Some(&cwd),
            },
        );
        let row = fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_run_observations
            .values()
            .next()
            .unwrap()
            .clone();
        assert_eq!(row.placement_refusal, Some(expected));
        assert!(row.summary.worktree.is_empty());
        assert!(
            row.work_binding.is_none() && row.source_root.is_none() && row.grant_identity.is_none()
        );
        let summary = fixture.summary(&row.execution_id);
        assert_eq!(
            summary.state,
            TestRunState::Running,
            "a placement refusal does not invent an execution exit"
        );
        assert_eq!(summary.error.as_deref(), Some(expected.message()));
        assert!(summary.error.as_ref().unwrap().len() <= TEST_RUN_ERROR_MAX_BYTES);
        fixture.state.engram_host().observe(
            &session,
            &provenance,
            EngramRecorderObservation::CommandFinished {
                key: "outside",
                command,
                reported_command: Some(command),
                output: "",
                exit: Some(EngramCommandExit::Code(0)),
                runtime_output_cut: false,
            },
        );
        let restored = load_state(fixture.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        let row = &restored.test_run_observations[&row.execution_id];
        assert_eq!(row.placement_refusal, Some(expected));
        assert!(row.artifact.is_none());
        assert!(restored.test_run_registrations.is_empty());
        assert_eq!(
            test_run_summaries_locked(&restored)[0].error.as_deref(),
            Some(expected.message()),
            "the deciding placement check remains readable after terminal and reload"
        );
        assert!(
            row.evidence.is_empty(),
            "a refusal never manufactures verification"
        );
    }
}
