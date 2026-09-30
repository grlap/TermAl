// Owns the operator-run checks of carried background gates
// (src/engram_carried_checks.rs) against a real Engram binary and a
// disposable store: a background full gate launched in one turn and
// credited as a passed test verification on the holder's next checkpoint,
// which the real store accepts and reads back; and a gate the holder's own
// write refused, which leaves no verification in the store. Does not own the
// scripted checks of the same behaviour (src/tests/engram_carried_checks.rs)
// or the root recovery checks of its parent module, whose fixture it uses.
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

/// Names the worktree as the source root of the claim the session is bound
/// to, as the host records a name, and makes it the running turn's root.
fn name_live_root(gate: &LiveCarriedGate) {
    let project_id = live_record(gate, |record| {
        record
            .session
            .project_id
            .clone()
            .expect("the live root has a project")
    });
    let root_path = {
        let inner = gate
            .fixture
            .state
            .inner
            .lock()
            .expect("state mutex poisoned");
        inner
            .find_project(&project_id)
            .expect("the project exists")
            .root_path
            .clone()
    };
    let (root, common_dir_key) =
        validate_engram_source_root(&gate.worktree.to_string_lossy(), &root_path, &root_path)
            .expect("the linked worktree can be named");
    let mut inner = gate
        .fixture
        .state
        .inner
        .lock()
        .expect("state mutex poisoned");
    let index = inner
        .find_session_index(&gate.fixture.session_id)
        .expect("the live root exists");
    let binding = inner.sessions[index]
        .engram
        .work_binding
        .clone()
        .expect("the live turn is bound to the real claim");
    let store = EngramAuthorityStoreKey {
        database_path: gate.home.join("engram.db"),
        project_id: "termal-live-carried".to_owned(),
    };
    inner
        .projects
        .iter_mut()
        .find(|project| project.id == project_id)
        .expect("the project exists")
        .engram
        .as_mut()
        .expect("the project uses Engram")
        .authority_store_key = Some(store.clone());
    let entry = EngramWorkSourceRoot {
        store,
        work_id: binding.work_id.clone(),
        short_ref: gate.work_ref.clone(),
        claim_id: binding.claim_id.clone(),
        claim_fence: binding.claim_fence,
        root,
        common_dir_key,
        named_by_session: gate.fixture.session_id.clone(),
        named_at: "2026-09-30T00:00:00.000Z".to_owned(),
        generation: 1,
    };
    inner.sessions[index].engram.active_turn_source_root =
        Some(EngramTurnSourceRoot::from_entry(&entry));
    inner.engram_work_source_roots = vec![entry];
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
    let fingerprint = "f".repeat(64);
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
            "expectedFingerprint": fingerprint,
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
            "expectedFingerprint": fingerprint,
            "before": fingerprint,
            "after": fingerprint,
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

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn live_carried_background_gate_is_credited_by_the_real_store_on_the_next_checkpoint() {
    let gate = live_carried_gate("carried-credited");
    begin_live_turn(&gate, "Launch the gate.");
    name_live_root(&gate);
    launch_live_gate(&gate);
    finish_live_root(&gate.fixture.state, &gate.fixture.session_id);
    assert!(
        live_verifications(&gate).is_empty(),
        "nothing is credited while the run has not ended"
    );

    finish_live_run(&gate);
    begin_live_turn(&gate, "Read the gate's summary.");
    finish_live_root(&gate.fixture.state, &gate.fixture.session_id);

    let checkpoints = gate.fixture.transport.requests_for("turn_checkpoint");
    let settling = checkpoints.last().expect("the second turn is checkpointed");
    assert_eq!(
        settling["verification_evidence"][0]["check_kind"], "test",
        "{settling:#}"
    );
    assert!(
        settling["verification_evidence"][0]["refs"]
            .as_array()
            .is_some_and(|refs| refs.iter().any(|reference| reference
                .as_str()
                .is_some_and(|reference| reference.starts_with("launched-under-grant:")))),
        "{settling:#}"
    );
    let verifications = live_verifications(&gate);
    eprintln!("live carried gate: the real store's verifications: {verifications:#?}");
    assert!(
        verifications
            .iter()
            .any(|verification| verification["check_kind"] == "test"
                && verification["result"] == "passed"
                && verification["producer_outcome"] == "succeeded"),
        "the real store holds a passed test verification: {verifications:#?}"
    );
    assert!(live_record(&gate, |record| record
        .engram
        .carried_checks
        .is_empty()));
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn live_carried_gate_refused_by_a_write_leaves_no_verification_in_the_real_store() {
    let gate = live_carried_gate("carried-refused");
    begin_live_turn(&gate, "Launch the gate.");
    name_live_root(&gate);
    launch_live_gate(&gate);
    let cwd = fs::canonicalize(&gate.worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    SessionRecorder::new(gate.fixture.state.clone(), gate.fixture.session_id.clone())
        .command_started_in("build", "cargo build", Some("cargo build"), Some(&cwd))
        .expect("the start should record");
    finish_live_run(&gate);
    finish_live_root(&gate.fixture.state, &gate.fixture.session_id);

    assert!(
        live_verifications(&gate).is_empty(),
        "a refused gate leaves no verification in the real store"
    );
    assert!(live_record(&gate, |record| record
        .engram
        .carried_checks
        .is_empty()));
    let line = live_record(&gate, |record| {
        record.engram.pending_source_root_line.clone()
    })
    .unwrap_or_default();
    assert!(
        line.contains("earned no credit") && line.contains("ran a `cargo` command there"),
        "{line}"
    );
}
