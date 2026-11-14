// Owns the operator-run checks of carried background gates
// (src/engram_carried_checks.rs) against a real Engram binary and a
// disposable store: a background full gate launched in one turn and
// retained as exact launch/artifact custody without automatic verification;
// a holder's write preserves its refusal cause and likewise earns no credit. Does not own the
// scripted checks of the same behaviour (src/tests/engram_carried_checks.rs)
// or the root recovery checks of its parent module, whose fixture it uses, or
// the batch gate's live check (its child module, engram_batch_gate_live.rs).
// New module, a child of the live root recovery module so the launcher's
// `live` mode runs it.
use super::*;

const LIVE_GATE: &str = "node scripts/test-launcher.mjs full";

/// A real Engram claim for the fixture's session, a named linked worktree of
/// the fixture's repository, and the paths the checks need.
struct LiveCarriedGate {
    fixture: LiveRootFixture,
    project_file: PathBuf,
    home: PathBuf,
    worktree: PathBuf,
    work_ref: String,
}

/// Runs the real Engram binary against the fixture's disposable store only,
/// as the fixture's session and actor, and returns its JSON reply.
fn live_engram_work(gate: &LiveCarriedGate, args: &[&str]) -> Value {
    let (actor_id, actor_context) = {
        let inner = gate
            .fixture
            .state
            .inner
            .lock()
            .expect("state mutex poisoned");
        let index = inner
            .find_session_index(&gate.fixture.session_id)
            .expect("the live root exists");
        engram_runtime_actor_identity(
            &inner.preferences.engram.developer_name,
            &inner.sessions[index],
        )
    };
    let mut command = Command::new(live_engram_binary());
    command
        .arg("--project-file")
        .arg(&gate.project_file)
        .arg("--home")
        .arg(&gate.home)
        .args([
            "work",
            "--actor-id",
            &actor_id,
            "--session-id",
            &gate.fixture.session_id,
        ]);
    if let Some(actor_context) = actor_context.as_deref() {
        command.args(["--actor-context", actor_context]);
    }
    let output = command
        .args(args)
        .output()
        .expect("real Engram should launch");
    assert!(
        output.status.success(),
        "engram work {args:?} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "engram work {args:?} printed no JSON ({error}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

/// The fixture's project made a Git repository with a linked worktree at
/// `.worktrees/wt`, and a real claim on a new item for its session.
fn live_carried_gate(suffix: &str) -> LiveCarriedGate {
    let fixture = live_root_fixture(suffix, []);
    let base = fixture
        .state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .to_path_buf();
    let root = base.join(format!("live-root-project-{suffix}"));
    let home = base.join(format!("live-root-home-{suffix}"));
    run_git_test_command(&root, &["init", "--quiet"]);
    run_git_test_command(&root, &["config", "user.email", "termal-tests@example.com"]);
    run_git_test_command(&root, &["config", "user.name", "TermAl tests"]);
    fs::write(root.join("README.md"), "carried\n").expect("fixture file should write");
    fs::write(root.join(".gitignore"), ".worktrees/\n").expect("ignore file should write");
    run_git_test_command(
        &root,
        &["add", "README.md", ".gitignore", ".engram-project"],
    );
    run_git_test_command(&root, &["commit", "--quiet", "-m", "carried"]);
    let worktree = root.join(".worktrees").join("wt");
    run_git_test_command(
        &root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "wt",
            worktree.to_str().expect("a UTF-8 worktree path"),
        ],
    );
    let mut gate = LiveCarriedGate {
        project_file: root.join(".engram-project"),
        home,
        worktree,
        work_ref: String::new(),
        fixture,
    };
    let project_id = live_record(&gate, |record| record.session.project_id.clone().unwrap());
    {
        let mut inner = gate
            .fixture
            .state
            .inner
            .lock()
            .expect("state mutex poisoned");
        inner.preferences.engram.binary_path = live_engram_binary().to_string_lossy().into_owned();
        inner.preferences.engram.home = gate.home.to_string_lossy().into_owned();
        // The prompt fixture preinstalls settings without learning a store.
        // Real enablement must run readiness before this fixture names a root.
        inner
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
            .expect("carried gate project exists")
            .engram = None;
    }
    gate.fixture
        .state
        .patch_project_engram_settings(
            &project_id,
            UpdateProjectEngramSettingsRequest {
                enabled: true,
                execution_observability: Some(true),
                acceptance_evaluation: None,
                binary_path: None,
                home: None,
                deadline_ms: None,
            },
        )
        .expect("real readiness identifies the disposable store");
    let database = {
        let inner = gate.fixture.state.inner.lock().unwrap();
        inner
            .find_project(&project_id)
            .unwrap()
            .engram
            .as_ref()
            .unwrap()
            .authority_store_key
            .as_ref()
            .expect("real readiness learned the disposable store identity")
            .database_path
            .clone()
    };
    assert!(
        fs::canonicalize(database)
            .unwrap()
            .starts_with(fs::canonicalize(&gate.home).unwrap())
    );
    let context_read = phase_sync::PollGuard::new();
    while live_record(&gate, |record| record.engram.context_nudge_in_progress) {
        context_read.wait("the context read started by real enablement finishes");
    }
    let added = live_engram_work(&gate, &["add", "Carried gate live check", "--json"]);
    let work_ref = added["work"]["short_ref"]
        .as_str()
        .or_else(|| added["short_ref"].as_str())
        .unwrap_or_else(|| panic!("the item has a short ref: {added:#}"))
        .to_owned();
    live_engram_work(&gate, &["claim", &work_ref, "--json"]);
    gate.work_ref = work_ref;
    gate
}

/// Names the actual held claim through the ordinary work-session service.
fn name_live_root(gate: &LiveCarriedGate) {
    gate.fixture
        .state
        .name_engram_source_root(
            &gate.fixture.session_id,
            EngramSourceRootRequest {
                work: gate.work_ref.clone(),
                path: Some(Some(gate.worktree.to_string_lossy().into_owned())),
            },
        )
        .expect("real named_root_bind acknowledges the worktree");
}

/// Reads the live root's session record.
fn live_record<T>(gate: &LiveCarriedGate, read: impl FnOnce(&SessionRecord) -> T) -> T {
    let inner = gate
        .fixture
        .state
        .inner
        .lock()
        .expect("state mutex poisoned");
    let index = inner
        .find_session_index(&gate.fixture.session_id)
        .expect("the live root exists");
    read(&inner.sessions[index])
}

/// Starts a turn of the live root and delivers its prompt.
fn begin_live_turn(gate: &LiveCarriedGate, prompt: &str) {
    let dispatch = dispatch_live_root(&gate.fixture.state, &gate.fixture.session_id, prompt, None);
    deliver_turn_dispatch(&gate.fixture.state, dispatch).expect("the live turn should be admitted");
    // A bound turn's prompt may lead with the host's lines about where its
    // work is measured; the agent's prompt ends it.
    match gate
        .fixture
        .receiver
        .try_recv()
        .expect("the admitted prompt reaches the simulated provider")
    {
        CodexRuntimeCommand::Prompt {
            session_id,
            command,
        } => {
            assert_eq!(session_id, gate.fixture.session_id);
            assert!(command.prompt.ends_with(prompt), "{}", command.prompt);
        }
        _ => panic!("the live root should get a provider prompt"),
    }
    assert!(gate.fixture.receiver.try_recv().is_err());
}

/// Launches the gate in the worktree in the background, as a runtime that
/// reports the command's directory does.
fn launch_live_gate(gate: &LiveCarriedGate) {
    let cwd = fs::canonicalize(&gate.worktree)
        .expect("the worktree canonicalizes")
        .to_string_lossy()
        .into_owned();
    let mut recorder =
        SessionRecorder::new(gate.fixture.state.clone(), gate.fixture.session_id.clone());
    recorder
        .command_started_in("gate", LIVE_GATE, Some(LIVE_GATE), Some(&cwd))
        .expect("the launch should record");
    let start = live_record(gate, |record| {
        record
            .engram
            .active_turn_checks
            .first()
            .map(|check| check.start_basis.clone())
    });
    if let Some(start) = start {
        start.wait_until(std::time::Instant::now() + DEADLOCK_GUARD);
    }
    recorder
        .command_completed_with_exit(
            "gate",
            LIVE_GATE,
            "Command running in background",
            CommandStatus::Success,
            EngramCommandExit::NotFinished,
        )
        .expect("the launch result should record");
    let carried = live_record(gate, |record| record.engram.carried_checks.len());
    assert_eq!(carried, 1, "the live gate is carried past its launch");
}

/// Writes the gate's passed run for the worktree, started after its launch.
fn finish_live_run(gate: &LiveCarriedGate) {
    let runs = PathBuf::from(
        run_git_test_command_output(
            &gate.worktree,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                "review-runs",
            ],
        )
        .trim(),
    );
    let directory = runs.join("test-live-carried");
    fs::create_dir_all(&directory).expect("the run directory should be created");
    let root = fs::canonicalize(&gate.worktree).expect("the worktree canonicalizes");
    let stages = [
        "cargo-check",
        "typescript",
        "fingerprint-tests",
        "rust-tests",
        "vitest",
    ];
    let source_id = launcher_source_id_fixture();
    let now = || chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let started = now();
    fs::write(
        directory.join("request.json"),
        serde_json::to_vec(&json!({
            "runId": "test-live-carried",
            "root": root.to_string_lossy(),
            "stages": stages.iter().map(|name| json!({ "name": name })).collect::<Vec<_>>(),
            "full": true,
            "started": started,
            "sourceId": source_id,
        }))
        .unwrap(),
    )
    .expect("the request should write");
    fs::write(
        directory.join("results.json"),
        serde_json::to_vec(&json!({
            "runId": "test-live-carried",
            "state": "passed",
            "started": started,
            "stages": stages
                .iter()
                .map(|name| json!({ "name": name, "state": "passed", "code": 0 }))
                .collect::<Vec<_>>(),
            "sourceIdBefore": source_id,
            "sourceIdAfter": source_id,
            "exitCode": 0,
            "ended": now(),
        }))
        .unwrap(),
    )
    .expect("the results should write");
}

/// The test verifications the real store holds for the claimed item, with
/// their typed results.
fn live_verifications(gate: &LiveCarriedGate) -> Vec<Value> {
    let shown = live_engram_work(gate, &["show", &gate.work_ref, "--notes", "--json"]);
    let mut found = Vec::new();
    let mut stack = vec![shown];
    while let Some(value) = stack.pop() {
        match value {
            Value::Object(map) => {
                if let Some(verification) = map.get("verification")
                    && verification.is_object()
                {
                    found.push(verification.clone());
                }
                stack.extend(map.into_iter().map(|(_, value)| value));
            }
            Value::Array(values) => stack.extend(values),
            _ => {}
        }
    }
    found
}

fn finish_carried_turn(gate: &LiveCarriedGate) {
    finish_live_root(&gate.fixture.state, &gate.fixture.session_id);
    gate.fixture
        .state
        .start_runtime_observation_worker(&gate.fixture.session_id);
    let guard = phase_sync::PollGuard::new();
    loop {
        let done = live_record(gate, |record| {
            record.engram.unadmitted_closing_jobs.is_empty()
                && !record.engram.unadmitted_worker_running
                && record.engram.unadmitted_reports.iter().all(|report| {
                    report.envelopes.iter().all(|envelope| {
                        matches!(
                            envelope.delivery,
                            EngramRuntimeObservationDelivery::Recorded { .. }
                        )
                    })
                })
        });
        if done {
            break;
        }
        guard.wait("real carried-launch ordinary report and worker finish before process cleanup");
    }
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn live_carried_background_gate_keeps_artifact_custody_without_automatic_native_credit() {
    let gate = live_carried_gate("carried-custody");
    name_live_root(&gate);
    begin_live_turn(&gate, "Launch the gate.");
    launch_live_gate(&gate);
    finish_carried_turn(&gate);
    assert!(live_verifications(&gate).is_empty());
    finish_live_run(&gate);
    gate.fixture.state.poll_engram_carried_runs();
    let run = live_record(&gate, |record| {
        assert!(record.engram.carried_checks.is_empty());
        record.engram.carried_consumed_runs.last().cloned().unwrap()
    });
    let digest = engram_terminal_record_digest(&run).unwrap();
    assert!(
        engram_read_carried_run(&run, &digest).unwrap().passed,
        "actual artifact reader"
    );
    begin_live_turn(&gate, "Read the gate's summary.");
    finish_carried_turn(&gate);
    gate.fixture.state.poll_engram_carried_runs();
    assert!(
        live_verifications(&gate).is_empty(),
        "a launch artifact is not registered native verification"
    );
    live_record(&gate, |record| {
        assert!(record.engram.carried_checks.is_empty());
        assert!(record.engram.carried_consumed_runs.contains(&run));
    });
    assert_no_live_turn_gate(&gate.fixture.transport);
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn live_carried_gate_fenced_by_a_write_keeps_its_cause_and_no_native_verification() {
    let gate = live_carried_gate("carried-refused");
    name_live_root(&gate);
    begin_live_turn(&gate, "Launch the gate.");
    launch_live_gate(&gate);
    let cwd = fs::canonicalize(&gate.worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    SessionRecorder::new(gate.fixture.state.clone(), gate.fixture.session_id.clone())
        .command_started_in("build", "cargo build", Some("cargo build"), Some(&cwd))
        .unwrap();
    finish_live_run(&gate);
    gate.fixture.state.poll_engram_carried_runs();
    let cause = live_record(&gate, |record| {
        record.engram.carried_checks[0].fence.clone().unwrap()
    });
    assert!(cause.contains("cargo"), "{cause}");
    finish_carried_turn(&gate);
    assert!(live_verifications(&gate).is_empty());
    live_record(&gate, |record| {
        assert_eq!(record.engram.carried_checks.len(), 1);
        assert_eq!(
            record.engram.carried_checks[0].fence.as_deref(),
            Some(cause.as_str())
        );
        assert!(record.engram.carried_consumed_runs.is_empty());
    });
    assert_no_live_turn_gate(&gate.fixture.transport);
}
