//! Execution observations on the end-of-turn checkpoint (Engram
//! w-108a13d58018, tm-winf): a mediated turn reports one observation on the
//! checkpoint that closes its grant, with `source_changed` decided by content
//! (the workspace's content revision before the prompt reached the
//! runtime against the one at the close; the turn's file-change tracking
//! decides only when the closing revision is missing), in the source root
//! named for its claim when there is one, the outcome of the transition that closed it,
//! the intent fingerprint the grant was issued for, and the closing
//! fingerprint with its canonical worktree root as the source basis. A turn
//! that changed source under a grant that mediates no local mutation reports
//! nothing, and the first report built for a grant is repeated by every
//! later checkpoint of that grant.
//!
//! Owns the turn-observation tests. Does not own checkpoint idempotency,
//! intent-key or recovery tests, which stay in
//! `src/tests/engram_host_adapter.rs`. New module beside the adapter tests,
//! created instead of growing them.

use super::super::run_git_test_command;
use super::*;

#[path = "engram_source_observations.rs"]
mod source_observations;

#[test]
fn a_change_between_named_root_turns_is_recorded_before_the_next_provider_handoff() {
    let label = "between-named-turns";
    let (claimed, worktree, first_runtime) = named_root_turn(label, true);
    let canonical = fs::canonicalize(&worktree).expect("the named worktree canonicalizes");
    let first_basis = claimed.record(|record| {
        record.engram.active_turn_start_basis.clone().expect("first opening basis")
    });
    assert_eq!(PathBuf::from(&first_basis.workspace_id), canonical);
    assert_eq!(first_basis.source_root_generation, Some(1));
    assert_eq!(first_basis.source_root_state, Some(EngramSourceRootState::Named));

    fs::write(worktree.join("README.md"), "changed during the first turn\n")
        .expect("first source change writes");
    let first = finish_claimed_turn(&claimed, &first_runtime);
    assert_eq!(first["source_changed"], true);
    let baseline = first["source_basis"].clone();
    assert_eq!(baseline["source_root_generation"], 1);

    // The first checkpoint has finished. This real write is attributed to
    // neither turn by a watcher; its endpoints disclose only a content change.
    fs::write(worktree.join("README.md"), "changed between the named turns\n")
        .expect("the inter-turn source change writes");
    let current = content_revision_of(&worktree);
    assert_ne!(baseline["source_revision"], current);
    let next_grant = format!("turn-observation-{label}-next-grant");
    claimed.transport.responses.lock().unwrap().extend([
        grant_reply(&next_grant),
        begin_reply(&next_grant),
        checkpoint_reply(&next_grant),
    ]);
    // ClaimedRoot scripts one binding read for its original single turn.
    // The second admission must read the same still-held claim, not the
    // scripted transport's empty-queue absence default.
    let binding = test_control_work_binding(&format!("turn-observation-{label}"), 1);
    claimed.transport.work_bindings.lock().unwrap().push_back(Ok(Some(binding.clone())));
    let second_request_start = claimed.transport.requests().len();
    deliver_turn_dispatch(&claimed.state, claimed.dispatch())
        .expect("the next admitted turn reaches provider handoff");
    claimed.record(|record| {
        assert_eq!(record.engram.work_binding.as_ref(), Some(&binding));
        assert_eq!(record.engram.active_grant_id.as_deref(), Some(next_grant.as_str()));
    });
    let _ = received_prompt(&claimed);
    let at_handoff = claimed.transport.requests().len();
    let saved = load_state(claimed.state.persistence_path.as_path()).unwrap().unwrap();
    let durable_intent = saved.engram_source_sightings.iter()
        .flat_map(|owner| &owner.observations)
        .find(|intent| intent.grant_id == next_grant).expect("the actual durable gap intent exists at handoff");
    assert!(matches!(&durable_intent.phase, EngramSourceObservationPhase::Recorded { request, receipt }
        if request["occurrence"]["source_change"]["sighting"]["source_basis"]["source_revision"] == current
            && receipt["accounting"]["kind"] == "source_change"));
    assert!(durable_intent.baseline.is_some());
    let second_basis = claimed.record(|record| {
        assert!(record.active_turn_file_changes.is_empty(), "no watcher attribution");
        record.engram.active_turn_start_basis.clone().expect("second opening basis")
    });
    assert_eq!(PathBuf::from(&second_basis.workspace_id), canonical);
    assert_eq!(second_basis.source_root_generation, Some(1));
    assert_eq!(second_basis.source_root_state, Some(EngramSourceRootState::Named));
    assert_eq!(second_basis.source_revision, current);
    claimed.state
        .finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token())
        .expect("the unchanged second turn closes");
    let requests = claimed.transport.requests();
    let checkpoints: Vec<_> = requests.iter()
        .filter(|request| request.request["operation"] == "turn_checkpoint")
        .collect();
    assert_eq!(checkpoints.len(), 2);
    let second = &checkpoints[1].request["observations"][0];
    assert_eq!(second["source_changed"], false);
    assert_eq!(second["effect"], "observe");
    assert_eq!(second["source_basis"]["source_revision"], current);
    assert_eq!(content_revision_of(&worktree), current, "the second turn made no edit");

    let boundary = (second_request_start..requests.len())
        .find(|index| requests[*index].request["operation"] == "execution_observe")
        .expect("the real inter-turn change is recorded separately from the quiet turn");
    let request = &requests[boundary].request;
    assert_eq!(request["occurrence"]["kind"], "inter_turn_change");
    assert_eq!(request["causality"]["kind"], "unknown");
    assert_eq!(request["policy_basis"]["mode"], "account_if_eligible");
    let change = &request["occurrence"]["source_change"];
    assert_eq!(change["detection"], "content_comparison");
    assert_eq!(change["baseline"]["source_revision"], baseline["source_revision"]);
    assert_eq!(change["sighting"]["source_basis"]["source_revision"], current);
    let begin = (second_request_start..boundary)
        .find(|index| requests[*index].request["operation"] == "turn_begin"
            && requests[*index].request["grant_id"] == next_grant)
        .expect("the observation follows the accepted begin");
    assert!(begin < boundary && boundary < at_handoff,
        "the boundary observation precedes the provider handoff");
}

#[test]
fn a_worktree_root_over_engrams_basis_bound_leaves_the_report_without_a_basis() {
    // Engram refuses a basis field over 512 bytes, and with it the whole
    // report; such a root is reported without a basis instead.
    let workdir = FsPath::new("C:/w");
    let at_bound = format!("C:/{}", "w".repeat(ENGRAM_SOURCE_BASIS_MAX_BYTES - 3));
    assert_eq!(
        engram_bounded_source_basis(workdir, FsPath::new(&at_bound), "revision".to_owned()),
        Some(EngramExecutionSourceBasis {
            source_root_generation: None,
            source_root_state: None,
            workspace_id: at_bound.clone(),
            source_revision: "revision".to_owned(),
        })
    );
    let over_bound = format!("{at_bound}w");
    assert_eq!(
        engram_bounded_source_basis(workdir, FsPath::new(&over_bound), "revision".to_owned()),
        None
    );
}

/// Whether the delegation child may mutate its workspace, which is what
/// decides whether its grant mediates `mutate_local`.
#[derive(Clone, Copy)]
enum ChildWorkspace {
    ReadOnly,
    IsolatedWorktree,
}

/// Whether the child's control session is bound to claimed work, which is
/// what lets its checkpoints carry evidence at all.
#[derive(Clone, Copy)]
enum ControlBinding {
    Claimed,
    Unbound,
}

/// How the scripted control plane answers the turn's checkpoints.
#[derive(Clone, Copy)]
enum CheckpointScript {
    /// The first checkpoint is accepted.
    Accept,
    /// The first checkpoint times out after Engram may have accepted it; the
    /// retry is accepted.
    DeadlineThenAccept,
    /// Engram answers the first checkpoint by rejecting its payload; the
    /// retry is accepted.
    RejectThenAccept,
    /// The first checkpoint's call is lost; the next prompt's rebind finds
    /// the grant open, closes it and binds afresh for a new grant.
    LostThenClosedByRebind,
    /// The first checkpoint's call is lost; Engram refuses a rebind's
    /// recovery checkpoint with an error; the next rebind's recovery
    /// checkpoint is accepted and the session binds afresh.
    LostThenRecoveryRefusedWithError,
    /// As [`Self::LostThenRecoveryRefusedWithError`], with the refusal given
    /// as a refuse result rather than an error.
    LostThenRecoveryRefusedWithAnswer,
}

impl CheckpointScript {
    fn loses_the_first_close(self) -> bool {
        matches!(
            self,
            Self::LostThenClosedByRebind
                | Self::LostThenRecoveryRefusedWithError
                | Self::LostThenRecoveryRefusedWithAnswer
        )
    }
}

/// The content revision of `root`, as the observation reports it.
fn content_revision_of(root: &FsPath) -> String {
    content_revision(root)
        .expect("the content revision should be taken")
        .1
}

/// A delegation child whose first mediated turn is running against a scripted
/// control plane that accepts one checkpoint.
struct RunningMediatedTurn {
    state: AppState,
    child_id: String,
    child_workdir: PathBuf,
    grant_id: String,
    runtime_token: RuntimeToken,
    transport: Arc<ScriptedEngramControlTransport>,
    _runtime_rx: std::sync::mpsc::Receiver<CodexRuntimeCommand>,
}

fn init_git_repository(root: &FsPath) {
    run_git_test_command(root, &["init", "--quiet"]);
    run_git_test_command(root, &["config", "user.email", "termal-tests@example.com"]);
    run_git_test_command(root, &["config", "user.name", "TermAl tests"]);
    fs::write(root.join("README.md"), "observed\n").expect("fixture file should write");
    run_git_test_command(root, &["add", "README.md"]);
    run_git_test_command(root, &["commit", "--quiet", "-m", "observed"]);
}

fn start_mediated_turn(
    label: &str,
    workspace: ChildWorkspace,
    binding: ControlBinding,
) -> RunningMediatedTurn {
    start_mediated_turn_with_script(label, workspace, binding, CheckpointScript::Accept)
}

fn start_mediated_turn_with_wait_deadline(
    label: &str,
    workspace: ChildWorkspace,
    binding: ControlBinding,
) -> RunningMediatedTurn {
    start_mediated_turn_with_script(
        label,
        workspace,
        binding,
        CheckpointScript::DeadlineThenAccept,
    )
}

fn start_mediated_turn_with_script(
    label: &str,
    workspace: ChildWorkspace,
    binding: ControlBinding,
    script: CheckpointScript,
) -> RunningMediatedTurn {
    let (state, runtime_rx) =
        test_app_state_with_delegation_codex_runtime(&format!("engram-turn-observation-{label}"));
    select_scripted_engram_budget_clock_before_enable(&state);
    let temp_root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .to_path_buf();
    let root = temp_root.join(format!("engram-turn-observation-{label}-project"));
    fs::create_dir_all(&root).expect("project root should exist");
    let write_policy = match workspace {
        ChildWorkspace::ReadOnly => DelegationWritePolicy::ReadOnly,
        ChildWorkspace::IsolatedWorktree => {
            // A worktree needs a repository with a commit to branch from.
            init_git_repository(&root);
            DelegationWritePolicy::IsolatedWorktree {
                owned_paths: vec!["src".to_owned()],
                worktree_path: Some(
                    temp_root
                        .join(format!("engram-turn-observation-{label}-worktree"))
                        .to_string_lossy()
                        .into_owned(),
                ),
            }
        }
    };
    let project_id = create_test_project(&state, &root, "Engram turn observation");
    let parent_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    enable_test_project_engram(&state, &project_id, &root);
    if matches!(workspace, ChildWorkspace::IsolatedWorktree) {
        // The Engram project marker written above must be tracked before a
        // worktree can be materialized from this repository.
        run_git_test_command(&root, &["add", "-A"]);
        run_git_test_command(&root, &["commit", "--quiet", "-m", "declare engram"]);
    }
    let grant_id = format!("turn-observation-{label}-grant");
    let mut responses = vec![
        bind_reply(&format!("turn-observation-parent-{label}")),
        bind_reply(&format!("turn-observation-child-{label}")),
        grant_reply(&grant_id),
        begin_reply(&grant_id),
    ];
    match script {
        CheckpointScript::Accept => {}
        CheckpointScript::DeadlineThenAccept => {
            responses.push(ScriptedEngramControlResponse::Reply(Err(
                EngramTransportError::deadline(
                    "checkpoint timed out after Engram may have accepted it",
                ),
            )));
        }
        CheckpointScript::RejectThenAccept => {
            responses.push(remote_error_reply("control_projection_invalid"));
        }
        CheckpointScript::LostThenClosedByRebind
        | CheckpointScript::LostThenRecoveryRefusedWithError
        | CheckpointScript::LostThenRecoveryRefusedWithAnswer => {
            let open_grant = || {
                ScriptedEngramControlResponse::Reply(Ok(json!({
                    "phase": "turn_open",
                    "open_grant_id": grant_id
                })))
            };
            responses.push(ScriptedEngramControlResponse::Reply(Err(
                EngramTransportError::transport("checkpoint call lost"),
            )));
            responses.push(open_grant());
            match script {
                CheckpointScript::LostThenRecoveryRefusedWithError => {
                    responses.push(remote_error_reply("control_projection_invalid"));
                    responses.push(open_grant());
                }
                CheckpointScript::LostThenRecoveryRefusedWithAnswer => {
                    responses.push(checkpoint_refusal_reply("observation_scope_mismatch"));
                    responses.push(open_grant());
                }
                _ => {}
            }
        }
    }
    responses.push(checkpoint_reply(&grant_id));
    if script.loses_the_first_close() {
        responses.push(rebind_reply(&format!(
            "turn-observation-child-{label}-rebound"
        )));
    }
    if matches!(script, CheckpointScript::LostThenClosedByRebind) {
        responses.push(grant_reply(&format!("{grant_id}-next")));
        responses.push(begin_reply(&format!("{grant_id}-next")));
    }
    let transport = match binding {
        // The parent binds without work; the child's bind, and every rebind
        // of it, read the claim.
        ControlBinding::Claimed => {
            let work_binding = test_control_work_binding(&format!("turn-observation-{label}"), 1);
            ScriptedEngramControlTransport::new_with_work_bindings(
                responses,
                [
                    Ok(None),
                    Ok(Some(work_binding.clone())),
                    Ok(Some(work_binding.clone())),
                    Ok(Some(work_binding)),
                ],
            )
        }
        ControlBinding::Unbound => ScriptedEngramControlTransport::new(responses),
    };
    if matches!(binding, ControlBinding::Claimed) {
        let work_binding = test_control_work_binding(&format!("turn-observation-{label}"), 1);
        let mut inner = state.inner.lock().unwrap();
        inner
            .projects
            .iter_mut()
            .find(|p| p.id == project_id)
            .unwrap()
            .engram
            .as_mut()
            .unwrap()
            .authority_store_key = Some(EngramAuthorityStoreKey {
            database_path: root.join("engram.db"),
            project_id: "github.com/example/source-root".to_owned(),
        });
        drop(inner);
        transport.register_named_root_run(&work_binding);
        // Lost-checkpoint tests own their status replies, including open grants.
        transport
            .named_roots
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .scripted_status = true;
    }
    install_control_only_transport(&state, transport.clone());
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: format!("Observe the {label} turn."),
                title: Some(format!("Engram turn observation {label}")),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(write_policy),
            },
        )
        .expect("delegation should begin");
    assert!(matches!(
        receive(&runtime_rx, "runtime should receive the prompt"),
        CodexRuntimeCommand::Prompt { .. }
    ));
    let child_id = created.delegation.child_session_id;
    let (runtime_token, child_workdir) = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == child_id)
            .expect("child should exist");
        (
            record
                .runtime
                .runtime_token()
                .expect("child runtime should be active"),
            PathBuf::from(&record.session.workdir),
        )
    };
    RunningMediatedTurn {
        state,
        child_id,
        child_workdir,
        grant_id,
        runtime_token,
        transport,
        _runtime_rx: runtime_rx,
    }
}

impl RunningMediatedTurn {
    /// Changes source during the running turn: writes `path` in the child's
    /// workdir, so its content revision moves, and records the event the
    /// workspace watcher would have raised for it.
    fn change_source(&self, path: &str) {
        let file = self.child_workdir.join(path);
        fs::create_dir_all(file.parent().expect("a source path has a parent"))
            .expect("source directory should exist");
        fs::write(&file, format!("changed during turn {}\n", self.grant_id))
            .expect("source edit should write");
        self.record_watcher_event(path);
    }

    /// Records only a watcher event for `path`, as the watcher raises one for
    /// any write under the session's workdir, whoever made it and wherever
    /// it lands (a nested worktree, an ignored path), with no content change
    /// in the files the turn's revision covers.
    fn record_watcher_event(&self, path: &str) {
        let mut inner = self.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&self.child_id)
            .expect("child should exist");
        inner
            .session_mut_by_index(index)
            .expect("session index should be valid")
            .active_turn_file_changes
            .insert(path.to_owned(), WorkspaceFileChangeKind::Modified);
    }

    /// The begin-time basis the record holds for the running turn.
    fn start_basis(&self) -> Option<EngramExecutionSourceBasis> {
        let inner = self.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&self.child_id)
            .expect("child should exist");
        inner.sessions[index].engram.active_turn_start_basis.clone()
    }

    /// Forgets the begin-time basis, as a capture that failed at the turn's
    /// start would have left it.
    fn forget_start_basis(&self) {
        let mut inner = self.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&self.child_id)
            .expect("child should exist");
        inner
            .session_mut_by_index(index)
            .expect("session index should be valid")
            .engram
            .active_turn_start_basis = None;
    }

    /// Lets the bind backoff a failed control call armed elapse, as time
    /// would.
    fn let_bind_retry_elapse(&self) {
        let mut inner = self.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&self.child_id)
            .expect("child should exist");
        inner
            .session_mut_by_index(index)
            .expect("session index should be valid")
            .engram
            .next_bind_retry_at = None;
    }

    fn checkpoints(&self) -> Vec<Value> {
        self.transport
            .requests()
            .into_iter()
            .filter(|request| request.request["operation"] == "turn_checkpoint")
            .map(|request| request.request)
            .collect()
    }

    /// The single checkpoint that closed the turn.
    fn checkpoint(&self) -> Value {
        let checkpoints = self.checkpoints();
        assert_eq!(checkpoints.len(), 1, "one checkpoint closes the turn");
        checkpoints.into_iter().next().expect("checkpoint")
    }

    /// The single observation the closing checkpoint reports.
    fn observation(&self) -> (Value, Value) {
        let checkpoint = self.checkpoint();
        let observations = checkpoint["observations"]
            .as_array()
            .expect("the turn-end checkpoint carries observations")
            .clone();
        assert_eq!(observations.len(), 1, "one observation per mediated turn");
        (
            checkpoint,
            observations.into_iter().next().expect("observation"),
        )
    }

    fn evaluated_intent_fingerprint(&self) -> String {
        self.transport
            .requests()
            .into_iter()
            .find(|request| request.request["operation"] == "turn_evaluate")
            .expect("the turn was evaluated")
            .request["intent_fingerprint"]
            .as_str()
            .expect("intent fingerprint")
            .to_owned()
    }
}

#[test]
fn a_completed_turn_that_changed_source_reports_a_mutating_observation_with_its_basis() {
    // The rule Engram applies to a work-bound observation with
    // source_changed=true is the whole point: the checkpoint must say the
    // turn mutated source, under which intent, and on which content.
    let turn = start_mediated_turn(
        "mutating",
        ChildWorkspace::IsolatedWorktree,
        ControlBinding::Claimed,
    );
    turn.change_source("src/lib.rs");

    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.child_id, &turn.runtime_token)
        .expect("turn should complete");

    let (checkpoint, observation) = turn.observation();
    assert_eq!(checkpoint["next_intent"], "wait");
    assert_eq!(observation["outcome"], "succeeded");
    assert_eq!(observation["source_changed"], true);
    assert_eq!(observation["effect"], "mutate_local");
    assert_eq!(
        observation["action_fingerprint"],
        turn.evaluated_intent_fingerprint(),
        "the observation names the intent the grant was issued for"
    );
    assert_eq!(
        observation["observation_id"].as_str().map(str::len),
        Some(64),
        "a canonical object id"
    );
    assert_eq!(
        PathBuf::from(
            observation["source_basis"]["workspace_id"]
                .as_str()
                .expect("workspace id")
        ),
        fs::canonicalize(&turn.child_workdir).expect("the worktree should canonicalize"),
        "the workspace is the canonical root of the child's own worktree"
    );
    let reported_revision = observation["source_basis"]["source_revision"]
        .as_str()
        .expect("source revision")
        .to_owned();
    assert_eq!(
        reported_revision,
        content_revision_of(&turn.child_workdir),
        "the revision is the content revision of the files present"
    );
    fs::write(turn.child_workdir.join("notes.txt"), "an untracked edit\n")
        .expect("untracked file should write");
    assert_ne!(
        reported_revision,
        content_revision_of(&turn.child_workdir),
        "an uncommitted, untracked edit changes the revision"
    );
    let observed_at = observation["observed_at"]
        .as_str()
        .expect("a basis comes with its time");
    chrono::DateTime::parse_from_rfc3339(observed_at).expect("observed_at should be RFC 3339");
    assert!(
        checkpoint["idempotency_key"]
            .as_str()
            .expect("idempotency key")
            .starts_with(&format!(
                "termal-checkpoint:{}:{}:wait:observations:",
                turn.child_id, turn.grant_id
            )),
        "the key folds the observation"
    );
}

/// The toolchain label every check a `ClaimedRoot` starts gets. Taking a real
/// one runs the host's rustup in the check's worktree on a detached thread,
/// which can outlive the test and keep its temporary root from being removed.
const CLAIMED_ROOT_TOOLCHAIN: &str =
    "rustc 1.90.0 (fixture 2025-09-14); cargo 1.90.0 (fixture 2025-07-30)";

/// A root session, bound to claimed work, in a Git repository of its own,
/// behind a scripted control plane answering `responses`.
struct ClaimedRoot {
    state: AppState,
    session_id: String,
    root: PathBuf,
    transport: Arc<ScriptedEngramControlTransport>,
    runtime_rx: std::sync::mpsc::Receiver<CodexRuntimeCommand>,
    /// Told "settling" when teardown starts waiting for capture workers.
    teardown_events: Mutex<Option<std::sync::mpsc::Sender<&'static str>>>,
}

impl Drop for ClaimedRoot {
    fn drop(&mut self) {
        self.settle_capture_workers();
    }
}

impl ClaimedRoot {
    fn new_scripted(label: &str, responses: Vec<ScriptedEngramControlResponse>) -> Self {
        let fixture = Self::new(label, responses);
        fixture
            .state
            .install_test_engram_budget_clock(EngramBudgetClock::scripted());
        fixture
    }

    fn new(label: &str, responses: Vec<ScriptedEngramControlResponse>) -> Self {
        // Checks started on this test thread settle their toolchain capture
        // inline, so no probe outlives the fixture.
        TEST_ENGRAM_TOOLCHAIN_LABEL.with(|fixture| {
            *fixture.borrow_mut() = Some(Some(CLAIMED_ROOT_TOOLCHAIN.to_owned()));
        });
        let (state, runtime_rx) = test_app_state_with_delegation_codex_runtime(&format!(
            "engram-turn-observation-{label}"
        ));
        let root = state
            .test_temp_root
            .as_ref()
            .expect("test root should exist")
            .path()
            .join(format!("engram-turn-observation-{label}-project"));
        fs::create_dir_all(&root).expect("project root should exist");
        init_git_repository(&root);
        let project_id = create_test_project(&state, &root, "Engram root turn observation");
        let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
        enable_test_project_engram(&state, &project_id, &root);
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&session_id)
                .expect("root should exist");
            // These tests are about the checkpoint, not the context reader.
            inner.sessions[index].engram.context_nudge_pending = false;
        }
        let transport = ScriptedEngramControlTransport::new_with_work_bindings(
            responses,
            [Ok(Some(test_control_work_binding(
                &format!("turn-observation-{label}"),
                1,
            )))],
        );
        install_control_only_transport(&state, transport.clone());
        Self {
            state,
            session_id,
            root,
            transport,
            runtime_rx,
            teardown_events: Mutex::new(None),
        }
    }

    /// Drops the runtime's receiving end, so later sends to the runtime fail,
    /// while the fixture itself stays owned until its teardown.
    fn close_runtime_channel(&mut self) {
        let (_closed, receiver) = std::sync::mpsc::channel();
        drop(std::mem::replace(&mut self.runtime_rx, receiver));
    }

    /// Waits until no capture worker of this session runs. A basis snapshot
    /// or toolchain probe still running in the fixture's worktree keeps the
    /// temporary root from being removed when the state drops. A hung worker
    /// fails the test by the watchdog, never by a longer wait.
    fn settle_capture_workers(&self) {
        let workers = {
            let Ok(inner) = self.state.inner.lock() else {
                return;
            };
            inner
                .find_session_index(&self.session_id)
                .map(|index| inner.sessions[index].engram.capture_workers.clone())
        };
        let Some(workers) = workers else {
            return;
        };
        let running = || workers.load(std::sync::atomic::Ordering::SeqCst);
        if running() == 0 {
            return;
        }
        if let Ok(events) = self.teardown_events.lock()
            && let Some(events) = events.as_ref()
        {
            let _ = events.send("settling");
        }
        let started = std::time::Instant::now();
        while running() > 0 {
            if started.elapsed() >= DEADLOCK_GUARD {
                let detail = format!(
                    "ClaimedRoot teardown: {} capture workers still running after {:?}",
                    running(),
                    started.elapsed()
                );
                if std::thread::panicking() {
                    eprintln!("{detail}");
                    return;
                }
                panic!("{detail}");
            }
            // Back off repeated observations of the worker count.
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn dispatch(&self) -> TurnDispatch {
        match self
            .state
            .dispatch_turn(
                &self.session_id,
                SendMessageRequest {
                    text: "Change the root workspace.".to_owned(),
                    expanded_text: None,
                    attachments: Vec::new(),
                    source_session_id: None,
                    source_mailbox: None,
                },
            )
            .expect("root should reach admission")
        {
            DispatchTurnResult::Dispatched(dispatch)
            | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
            DispatchTurnResult::Queued => panic!("an idle root should dispatch"),
        }
    }

    fn runtime_token(&self) -> RuntimeToken {
        let inner = self.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&self.session_id)
            .expect("root should exist");
        inner.sessions[index]
            .runtime
            .runtime_token()
            .expect("root runtime should be active")
    }

    fn record<T>(&self, read: impl FnOnce(&mut SessionRecord) -> T) -> T {
        let mut inner = self.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&self.session_id)
            .expect("root should exist");
        read(
            inner
                .session_mut_by_index(index)
                .expect("session index should be valid"),
        )
    }
}

#[test]
fn a_root_session_on_claimed_work_reports_the_source_change_its_turn_made() {
    // A root session edits its own workspace, so its grant mediates local
    // mutation and a turn that changed source reports it, rather than being
    // withheld as a read-only child's would be.
    let grant_id = "turn-observation-root-grant";
    let claimed = ClaimedRoot::new_scripted(
        "root",
        vec![
            bind_reply("turn-observation-root-token"),
            grant_reply(grant_id),
            begin_reply(grant_id),
            checkpoint_reply(grant_id),
        ],
    );
    prepare_confirmed_claimed_opening(&claimed, "root");
    let (state, session_id, root, transport) = (
        &claimed.state,
        &claimed.session_id,
        &claimed.root,
        &claimed.transport,
    );
    deliver_turn_dispatch(state, claimed.dispatch())
        .expect("the begun root turn should reach the runtime");
    assert!(matches!(
        receive(
            &claimed.runtime_rx,
            "runtime should receive the root prompt"
        ),
        CodexRuntimeCommand::Prompt { .. }
    ));
    let runtime_token = claimed.runtime_token();
    // Content only, with no watcher event: the fingerprints decide.
    fs::write(root.join("README.md"), "changed by the root turn\n")
        .expect("tracked file should change");

    state
        .finish_turn_ok_if_runtime_matches(session_id, &runtime_token)
        .expect("root turn should complete");

    let requests = transport.requests();
    let bind = requests
        .iter()
        .find(|request| request.request["operation"] == "session_bind")
        .expect("the root binds");
    assert_eq!(
        bind.request["mediated_effects"],
        json!(["observe", "communicate", "mutate_local"])
    );
    let evaluation = requests
        .iter()
        .find(|request| request.request["operation"] == "turn_evaluate")
        .expect("the root turn is evaluated");
    assert_eq!(
        evaluation.request["requested_effects"],
        bind.request["mediated_effects"]
    );
    let checkpoint = requests
        .iter()
        .find(|request| request.request["operation"] == "turn_checkpoint")
        .expect("the root turn is checkpointed")
        .request
        .clone();
    let observations = checkpoint["observations"]
        .as_array()
        .expect("the root turn reports its observation");
    assert_eq!(observations.len(), 1);
    let observation = &observations[0];
    assert_eq!(observation["outcome"], "succeeded");
    assert_eq!(observation["source_changed"], true);
    assert_eq!(observation["effect"], "mutate_local");
    assert_eq!(
        observation["action_fingerprint"],
        evaluation.request["intent_fingerprint"]
    );
    assert_eq!(
        observation["source_basis"]["source_revision"]
            .as_str()
            .expect("source revision"),
        content_revision_of(root),
        "the basis is the root workspace's changed content"
    );
}

#[test]
fn a_completed_turn_that_changed_nothing_reports_an_observing_observation() {
    let turn = start_mediated_turn(
        "observing",
        ChildWorkspace::ReadOnly,
        ControlBinding::Claimed,
    );

    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.child_id, &turn.runtime_token)
        .expect("turn should complete");

    let (_, observation) = turn.observation();
    assert_eq!(observation["outcome"], "succeeded");
    assert_eq!(observation["source_changed"], false);
    assert_eq!(observation["effect"], "observe");
    assert_eq!(
        observation["action_fingerprint"],
        turn.evaluated_intent_fingerprint()
    );
    assert_eq!(
        observation.get("source_basis").is_some(),
        observation.get("observed_at").is_some(),
        "a basis and its time come together or not at all"
    );
}

#[test]
fn a_completed_turn_that_changed_nothing_in_a_worktree_reports_its_unchanged_basis() {
    // The negative case of the content decision: both bases exist and are
    // equal. Anything TermAl itself wrote into the worktree between the two
    // captures would surface here as a change the turn never made.
    let turn = start_mediated_turn(
        "unchanged",
        ChildWorkspace::IsolatedWorktree,
        ControlBinding::Claimed,
    );
    assert_eq!(
        turn.start_basis()
            .expect("the turn began with a basis")
            .source_revision,
        content_revision_of(&turn.child_workdir),
        "the begin-time basis is the worktree's fingerprint, so the comparison is real"
    );

    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.child_id, &turn.runtime_token)
        .expect("turn should complete");

    let (_, observation) = turn.observation();
    assert_eq!(observation["outcome"], "succeeded");
    assert_eq!(
        observation["source_changed"], false,
        "equal fingerprints are no change"
    );
    assert_eq!(observation["effect"], "observe");
    assert_eq!(
        PathBuf::from(
            observation["source_basis"]["workspace_id"]
                .as_str()
                .expect("an unchanged worktree still has a basis")
        ),
        fs::canonicalize(&turn.child_workdir).expect("the worktree should canonicalize")
    );
    assert_eq!(
        observation["source_basis"]["source_revision"]
            .as_str()
            .expect("source revision"),
        content_revision_of(&turn.child_workdir),
        "the basis is the unchanged content's fingerprint"
    );
    assert!(
        observation.get("observed_at").is_some(),
        "a basis comes with its time"
    );
}

#[test]
fn a_watcher_event_without_a_content_change_is_no_source_change() {
    // tm-97wp: the watcher credits a turn with any write under its session's
    // workdir, such as another session's edit in a nested worktree. When
    // both bases exist and are equal, the content decides: no change, and no
    // obligation opened for a turn that changed nothing.
    let turn = start_mediated_turn(
        "watcher-only",
        ChildWorkspace::IsolatedWorktree,
        ControlBinding::Claimed,
    );
    assert!(
        turn.start_basis().is_some(),
        "the turn began with a basis, so a failed begin capture is not mistaken for the rule"
    );
    turn.record_watcher_event(".worktrees/peer/scripts/test-launcher.test.mjs");

    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.child_id, &turn.runtime_token)
        .expect("turn should complete");

    let (_, observation) = turn.observation();
    assert_eq!(
        observation["source_changed"], false,
        "equal fingerprints are no change, whatever the watcher saw"
    );
    assert_eq!(observation["effect"], "observe");
    assert_eq!(
        observation["source_basis"]["source_revision"]
            .as_str()
            .expect("source revision"),
        content_revision_of(&turn.child_workdir),
        "the basis is the unchanged content's fingerprint"
    );
}

#[test]
fn a_turn_without_a_closing_basis_is_judged_by_its_watcher_event() {
    // With no closing basis there is nothing to compare, so the watcher's
    // hint decides. An untracked nested repository makes the closing
    // capture fail closed while the begin-time basis stands.
    let turn = start_mediated_turn(
        "no-closing-basis",
        ChildWorkspace::IsolatedWorktree,
        ControlBinding::Claimed,
    );
    assert!(turn.start_basis().is_some(), "the turn began with a basis");
    let nested = turn.child_workdir.join("nested");
    fs::create_dir_all(&nested).expect("nested directory should exist");
    init_git_repository(&nested);
    turn.record_watcher_event("nested/README.md");

    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.child_id, &turn.runtime_token)
        .expect("turn should complete");

    let (_, observation) = turn.observation();
    assert!(
        observation.get("source_basis").is_none(),
        "the closing capture refuses the nested repository"
    );
    assert_eq!(
        observation["source_changed"], true,
        "without a closing basis the watcher's hint decides"
    );
    assert_eq!(observation["effect"], "mutate_local");
}

#[test]
fn a_turn_without_a_begin_basis_withholds_comparison_without_inventing_a_change() {
    // Missing comparison evidence is not evidence of a source mutation.
    let turn = start_mediated_turn(
        "no-begin-basis",
        ChildWorkspace::IsolatedWorktree,
        ControlBinding::Claimed,
    );
    assert!(turn.start_basis().is_some(), "the turn began with a basis");
    turn.forget_start_basis();

    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.child_id, &turn.runtime_token)
        .expect("turn should complete");

    let (_, observation) = turn.observation();
    assert_eq!(observation["outcome"], "succeeded");
    assert_eq!(
        observation["source_changed"], false,
        "an unavailable opening basis does not invent a source change"
    );
    assert_eq!(observation["effect"], "observe");
    assert!(observation.get("source_basis").is_none());
}

#[test]
fn a_report_engram_rejects_is_dropped_so_the_grant_can_close_bare() {
    // Engram answered and refused the payload: repeating it could never
    // close the grant. The next closing attempt goes without the report, as
    // every checkpoint did before turns reported observations.
    let turn = start_mediated_turn_with_script(
        "rejected",
        ChildWorkspace::IsolatedWorktree,
        ControlBinding::Claimed,
        CheckpointScript::RejectThenAccept,
    );
    turn.change_source("src/lib.rs");

    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.child_id, &turn.runtime_token)
        .expect("turn completion should tolerate the rejected checkpoint");
    turn.state
        .kill_session(&turn.child_id)
        .expect("terminal cleanup closes the grant with exit intent");

    let checkpoints = turn.checkpoints();
    assert_eq!(checkpoints.len(), 2);
    assert_eq!(
        checkpoints[0]["observations"].as_array().map(Vec::len),
        Some(1),
        "the first attempt carried the report"
    );
    assert_eq!(checkpoints[1]["next_intent"], "exit");
    assert!(
        checkpoints[1].get("observations").is_none(),
        "the rejected report is not repeated"
    );
    assert_eq!(
        checkpoints[1]["idempotency_key"],
        format!("termal-checkpoint:{}:{}:exit", turn.child_id, turn.grant_id),
        "the bare retry has a bare key"
    );
}

#[test]
fn a_rebind_after_a_lost_closing_checkpoint_closes_the_grant_with_the_cached_report() {
    // A closing checkpoint whose call was lost leaves the grant open and the
    // report on the record. The next prompt's rebind finds the grant open
    // and closes it with that report rather than bare, so the turn's
    // evidence survives the lost call.
    let turn = start_mediated_turn_with_script(
        "rebound",
        ChildWorkspace::IsolatedWorktree,
        ControlBinding::Claimed,
        CheckpointScript::LostThenClosedByRebind,
    );
    turn.change_source("src/lib.rs");
    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.child_id, &turn.runtime_token)
        .expect("turn completion should tolerate the lost checkpoint");
    turn.let_bind_retry_elapse();

    queue_test_engram_prompt(
        &turn.state,
        &turn.child_id,
        "Continue after the lost close.",
        QueuedPromptSource::User,
        None,
    );
    let dispatch = turn
        .state
        .start_next_queued_turn_off_lock(&turn.child_id, false, false)
        .expect("queue inspection should succeed")
        .expect("the queued prompt should rebind and dispatch")
        .dispatch;
    deliver_turn_dispatch(&turn.state, dispatch)
        .expect("the rebound prompt should reach the runtime");
    assert!(matches!(
        receive(
            &turn._runtime_rx,
            "runtime should receive the second prompt"
        ),
        CodexRuntimeCommand::Prompt { .. }
    ));

    let checkpoints = turn.checkpoints();
    assert_eq!(
        checkpoints.len(),
        2,
        "the lost close and the recovery close"
    );
    assert_eq!(checkpoints[0]["next_intent"], "wait");
    assert_eq!(
        checkpoints[0]["observations"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(
        checkpoints[1]["observations"], checkpoints[0]["observations"],
        "the recovery close carries the report the lost close built"
    );
    assert!(
        checkpoints[1]["idempotency_key"]
            .as_str()
            .expect("idempotency key")
            .starts_with(&format!(
                "termal-restart-checkpoint:{}:{}:observations:",
                turn.child_id, turn.grant_id
            )),
        "the recovery key folds the report"
    );
    let inner = turn.state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(&turn.child_id)
        .expect("child should exist");
    assert_eq!(
        inner.sessions[index].engram.active_grant_id.as_deref(),
        Some(format!("{}-next", turn.grant_id).as_str()),
        "the rebound session runs its next turn under a new grant"
    );
}

#[test]
fn a_rebind_whose_recovery_checkpoint_is_refused_with_an_error_closes_bare_next_time() {
    assert_a_refused_recovery_checkpoint_closes_bare_next_time(
        "recovery-refused-error",
        CheckpointScript::LostThenRecoveryRefusedWithError,
        "control_projection_invalid",
    );
}

#[test]
fn a_rebind_whose_recovery_checkpoint_is_refused_with_an_answer_closes_bare_next_time() {
    assert_a_refused_recovery_checkpoint_closes_bare_next_time(
        "recovery-refused-answer",
        CheckpointScript::LostThenRecoveryRefusedWithAnswer,
        "restart_checkpoint_refused",
    );
}

/// The recovery checkpoint of a rebind carries the report a lost close
/// built. When Engram refuses it, in either form, the next rebind must not
/// resend the same refused report, or the grant, and the session's prompts,
/// would stay held until a restart.
fn assert_a_refused_recovery_checkpoint_closes_bare_next_time(
    label: &str,
    script: CheckpointScript,
    rebind_error_code: &str,
) {
    let turn = start_mediated_turn_with_script(
        label,
        ChildWorkspace::IsolatedWorktree,
        ControlBinding::Claimed,
        script,
    );
    turn.change_source("src/lib.rs");
    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.child_id, &turn.runtime_token)
        .expect("turn completion should tolerate the lost checkpoint");

    turn.let_bind_retry_elapse();
    let Err(refused) = turn
        .state
        .ensure_engram_session_bound_off_lock(&turn.child_id)
    else {
        panic!("the refused recovery checkpoint fails the rebind");
    };
    assert_eq!(refused.code.as_deref(), Some(rebind_error_code));
    turn.let_bind_retry_elapse();
    turn.state
        .ensure_engram_session_bound_off_lock(&turn.child_id)
        .expect("the bare recovery checkpoint closes the grant and the session binds afresh");

    let checkpoints = turn.checkpoints();
    assert_eq!(
        checkpoints.len(),
        3,
        "the lost close, the refused recovery and the bare recovery"
    );
    assert_eq!(
        checkpoints[0]["observations"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(
        checkpoints[1]["observations"], checkpoints[0]["observations"],
        "the first recovery carried the lost close's report"
    );
    assert!(
        checkpoints[2].get("observations").is_none(),
        "the refused report is not resent"
    );
    assert_eq!(
        checkpoints[2]["idempotency_key"],
        format!(
            "termal-restart-checkpoint:{}:{}",
            turn.child_id, turn.grant_id
        ),
        "the bare recovery has a bare key"
    );
    let inner = turn.state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(&turn.child_id)
        .expect("child should exist");
    assert_eq!(
        inner.sessions[index].engram.active_grant_id, None,
        "the recovered grant is closed"
    );
}

#[test]
fn a_recorded_mutation_flag_applies_only_to_its_own_grant() {
    let recorded = ("grant-a".to_owned(), false);
    assert!(
        !engram_grant_mediates_mutation(Some(&recorded), "grant-a", true),
        "the flag recorded for this grant decides over the current set"
    );
    assert!(
        engram_grant_mediates_mutation(Some(&recorded), "grant-b", true),
        "a flag recorded for another grant never applies; the current set decides"
    );
    assert!(
        !engram_grant_mediates_mutation(Some(&recorded), "grant-b", false),
        "the current set decides both ways"
    );
    assert!(
        engram_grant_mediates_mutation(None, "grant-a", true),
        "with nothing recorded the current set decides"
    );
}

#[test]
fn a_prepared_evaluate_speaks_only_for_the_intent_it_was_issued_for() {
    let evaluate = |intent: &str, effects: Vec<EngramEffect>| EngramControlRequest::TurnEvaluate {
        routing_token: "token".to_owned(),
        idempotency_key: "key".to_owned(),
        intent_fingerprint: intent.to_owned(),
        purpose: "ordinary".to_owned(),
        requested_effects: effects,
        resource_intents: Vec::new(),
    };
    let old_set = vec![EngramEffect::Observe, EngramEffect::Communicate];
    let new_set = vec![
        EngramEffect::Observe,
        EngramEffect::Communicate,
        EngramEffect::MutateLocal,
    ];
    assert_eq!(
        engram_evaluate_requests_mutation(&evaluate("intent-a", old_set), "intent-a"),
        Some(false)
    );
    assert_eq!(
        engram_evaluate_requests_mutation(&evaluate("intent-a", new_set.clone()), "intent-a"),
        Some(true)
    );
    assert_eq!(
        engram_evaluate_requests_mutation(&evaluate("intent-b", new_set), "intent-a"),
        None,
        "another prompt's evaluate says nothing about this grant"
    );
}

#[test]
fn a_turn_is_judged_by_the_effects_its_own_grant_requested() {
    // Engram checks an observation's effect against the grant's requested
    // effects. A retained evaluate prepared under the older root set and
    // replayed after the upgrade is granted without mutate_local, so a turn
    // that changed source under that grant reports nothing rather than a
    // mutate_local claim the grant never covered.
    let grant_id = "turn-observation-replayed-grant";
    let claimed = ClaimedRoot::new_scripted(
        "replayed-evaluate",
        vec![
            bind_reply("turn-observation-replayed-token"),
            ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline(
                "evaluate reply lost",
            ))),
            grant_reply(grant_id),
            begin_reply(grant_id),
            checkpoint_reply(grant_id),
        ],
    );
    prepare_confirmed_claimed_opening(&claimed, "replayed-evaluate");
    deliver_turn_dispatch(&claimed.state, claimed.dispatch())
        .expect("an unknown admission retains the prompt");
    assert!(claimed.runtime_rx.try_recv().is_err());
    // The retained evaluate is the one prepared before the upgrade, under
    // the older root set.
    claimed.record(|record| {
        let prepared = record
            .queued_prompts
            .front_mut()
            .and_then(|queued| queued.engram_evaluate.as_mut())
            .expect("the unknown evaluate is retained for replay");
        let EngramControlRequest::TurnEvaluate {
            requested_effects, ..
        } = &mut prepared.request
        else {
            panic!("the retained request is an evaluate");
        };
        *requested_effects = vec![EngramEffect::Observe, EngramEffect::Communicate];
    });

    claimed
        .state
        .resume_session_queue(&claimed.session_id)
        .expect("the retained evaluate replays");
    assert!(matches!(
        receive(&claimed.runtime_rx, "the replayed turn reaches the runtime"),
        CodexRuntimeCommand::Prompt { .. }
    ));
    assert_eq!(
        claimed.record(|record| record.engram.active_turn_grant_mutates.clone()),
        Some((grant_id.to_owned(), false)),
        "the grant is recorded, under its own id, with what its replayed evaluate requested"
    );
    let runtime_token = claimed.runtime_token();
    fs::write(
        claimed.root.join("README.md"),
        "changed under the old grant\n",
    )
    .expect("tracked file should change");

    claimed
        .state
        .finish_turn_ok_if_runtime_matches(&claimed.session_id, &runtime_token)
        .expect("the replayed turn should complete");

    let requests = claimed.transport.requests();
    let evaluations = requests
        .iter()
        .filter(|request| request.request["operation"] == "turn_evaluate")
        .collect::<Vec<_>>();
    assert_eq!(
        evaluations.last().expect("the replay is evaluated").request["requested_effects"],
        json!(["observe", "communicate"]),
        "the replay is the retained request, older set and all"
    );
    let checkpoint = requests
        .iter()
        .find(|request| request.request["operation"] == "turn_checkpoint")
        .expect("the replayed turn is checkpointed")
        .request
        .clone();
    assert!(
        checkpoint.get("observations").is_none(),
        "the grant never covered mutation, so nothing is claimed for it"
    );
    assert_eq!(
        checkpoint["idempotency_key"],
        format!("termal-checkpoint:{}:{grant_id}:wait", claimed.session_id)
    );
}

#[test]
fn a_turn_that_changed_source_under_a_read_only_grant_reports_nothing() {
    // A read-only child's grant mediates observe and communicate only, and
    // Engram refuses source_changed without mutate_local. Rather than
    // misreport the turn as observe-only, the checkpoint reports nothing.
    let turn = start_mediated_turn(
        "read-only-mutation",
        ChildWorkspace::ReadOnly,
        ControlBinding::Claimed,
    );
    turn.change_source("src/lib.rs");

    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.child_id, &turn.runtime_token)
        .expect("turn should complete");

    let checkpoint = turn.checkpoint();
    assert_eq!(checkpoint["next_intent"], "wait");
    assert!(
        checkpoint.get("observations").is_none(),
        "no observation rather than a false one"
    );
    assert_eq!(
        checkpoint["idempotency_key"],
        format!("termal-checkpoint:{}:{}:wait", turn.child_id, turn.grant_id),
        "an empty report leaves the key bare"
    );
}

#[test]
fn an_unbound_session_reports_no_observation() {
    // Engram admits host evidence only through an exact work binding. A
    // session bound without one must not send an observation the evidence
    // gate would reject, which would leave the begun turn open.
    let turn = start_mediated_turn("unbound", ChildWorkspace::ReadOnly, ControlBinding::Unbound);

    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.child_id, &turn.runtime_token)
        .expect("turn should complete");

    let checkpoint = turn.checkpoint();
    assert_eq!(checkpoint["next_intent"], "wait");
    assert!(
        checkpoint.get("observations").is_none(),
        "an unbound session's checkpoint carries no evidence"
    );
    assert_eq!(
        checkpoint["idempotency_key"],
        format!("termal-checkpoint:{}:{}:wait", turn.child_id, turn.grant_id)
    );
}

#[test]
fn a_failed_turn_reports_a_failed_observation() {
    let turn = start_mediated_turn(
        "failed",
        ChildWorkspace::IsolatedWorktree,
        ControlBinding::Claimed,
    );
    turn.change_source("src/main.rs");

    turn.state
        .fail_turn_if_runtime_matches(
            &turn.child_id,
            &turn.runtime_token,
            "the mediated turn failed",
        )
        .expect("failure should terminalize the turn");

    let (checkpoint, observation) = turn.observation();
    assert_eq!(checkpoint["next_intent"], "wait");
    assert_eq!(observation["outcome"], "failed");
    assert_eq!(
        observation["source_changed"], true,
        "a failed turn that touched source still owes a check"
    );
    assert_eq!(observation["effect"], "mutate_local");
}

#[test]
fn a_killed_turn_reports_an_unknown_observation_on_its_exit_checkpoint() {
    let turn = start_mediated_turn(
        "killed",
        ChildWorkspace::IsolatedWorktree,
        ControlBinding::Claimed,
    );
    turn.change_source("src/lib.rs");

    turn.state
        .kill_session(&turn.child_id)
        .expect("killing the running child closes its grant");

    let (checkpoint, observation) = turn.observation();
    assert_eq!(checkpoint["next_intent"], "exit");
    assert_eq!(
        observation["outcome"], "unknown",
        "a turn ended from outside has no known outcome"
    );
    assert_eq!(observation["source_changed"], true);
}

#[test]
fn a_turn_whose_content_changed_without_a_watcher_event_still_reports_the_change() {
    // The file watcher is a debounced hint that can arrive late or skip
    // ignored paths. The begin-time and end-time fingerprints of the worktree
    // decide: content that differs at the close is a source change whatever
    // the watcher delivered.
    let turn = start_mediated_turn(
        "content-only",
        ChildWorkspace::IsolatedWorktree,
        ControlBinding::Claimed,
    );
    fs::write(
        turn.child_workdir.join("target-like.txt"),
        "written during the turn\n",
    )
    .expect("turn edit should write");

    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.child_id, &turn.runtime_token)
        .expect("turn should complete");

    let (_, observation) = turn.observation();
    assert_eq!(
        observation["source_changed"], true,
        "the end fingerprint differs from the begin fingerprint"
    );
    assert_eq!(observation["effect"], "mutate_local");
    assert_eq!(
        observation["source_basis"]["source_revision"],
        content_revision_of(&turn.child_workdir).as_str()
    );
}

#[test]
fn a_retried_exit_checkpoint_repeats_the_report_the_wait_built() {
    // The first closing attempt builds the report; the terminal transition
    // then clears the file-change tracking and the content keeps moving. A
    // retry must repeat the original report verbatim, source change and
    // basis included, so its idempotency key repeats.
    let turn = start_mediated_turn_with_wait_deadline(
        "retried",
        ChildWorkspace::IsolatedWorktree,
        ControlBinding::Claimed,
    );
    turn.change_source("src/lib.rs");

    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.child_id, &turn.runtime_token)
        .expect("turn completion should tolerate the wait checkpoint deadline");
    fs::write(turn.child_workdir.join("after.txt"), "after the turn\n")
        .expect("later edit should write");
    turn.state
        .kill_session(&turn.child_id)
        .expect("terminal cleanup retries the grant with exit intent");

    let checkpoints = turn.checkpoints();
    assert_eq!(checkpoints.len(), 2);
    assert_eq!(checkpoints[0]["next_intent"], "wait");
    assert_eq!(checkpoints[1]["next_intent"], "exit");
    let first = checkpoints[0]["observations"]
        .as_array()
        .expect("the wait checkpoint carries the observation");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0]["source_changed"], true);
    assert_eq!(first[0]["outcome"], "succeeded");
    assert!(first[0].get("observed_at").is_some());
    assert_eq!(
        checkpoints[1]["observations"], checkpoints[0]["observations"],
        "the exit retry repeats the report: same change, basis and time"
    );
}

#[test]
fn a_closer_that_captured_before_another_claimed_repeats_the_cached_report() {
    // Two closers of one grant can both plan a fresh report before either
    // claims the checkpoint. Here a teardown captures its basis and is held
    // there; the turn's own completion then claims first, caches its report
    // and loses the reply. When the teardown claims afterwards it must repeat
    // the cached report, not replace it with its own outcome, basis and time.
    let turn = start_mediated_turn_with_wait_deadline(
        "overlapping",
        ChildWorkspace::IsolatedWorktree,
        ControlBinding::Claimed,
    );
    turn.change_source("src/lib.rs");
    let gate = install_test_engram_turn_report_gate(&turn.state, &turn.child_id);
    let teardown = {
        let state = turn.state.clone();
        let child_id = turn.child_id.clone();
        std::thread::spawn(move || state.kill_session(&child_id))
    };
    gate.wait_until_claimed();

    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.child_id, &turn.runtime_token)
        .expect("turn completion should tolerate the wait checkpoint deadline");
    gate.release();
    teardown
        .join()
        .expect("teardown thread should not panic")
        .expect("teardown retries the grant with exit intent");

    let checkpoints = turn.checkpoints();
    assert_eq!(checkpoints.len(), 2);
    assert_eq!(checkpoints[0]["next_intent"], "wait");
    assert_eq!(checkpoints[1]["next_intent"], "exit");
    let first = checkpoints[0]["observations"]
        .as_array()
        .expect("the completion carries the observation");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0]["outcome"], "succeeded");
    assert_eq!(
        checkpoints[1]["observations"], checkpoints[0]["observations"],
        "the teardown repeats the completion's report rather than its own unknown one"
    );
}

#[test]
fn a_stopped_turn_reports_an_unknown_observation() {
    let turn = start_mediated_turn(
        "stopped",
        ChildWorkspace::IsolatedWorktree,
        ControlBinding::Claimed,
    );
    turn.change_source("src/lib.rs");

    turn.state
        .stop_session(&turn.child_id)
        .expect("stop should close the running turn");

    let (checkpoint, observation) = turn.observation();
    assert_eq!(checkpoint["next_intent"], "wait");
    assert_eq!(observation["outcome"], "unknown");
    assert_eq!(observation["source_changed"], true);
}

#[test]
fn a_runtime_exit_with_an_error_reports_a_failed_observation() {
    let turn = start_mediated_turn(
        "exit-error",
        ChildWorkspace::ReadOnly,
        ControlBinding::Claimed,
    );

    turn.state
        .handle_runtime_exit_if_matches(
            &turn.child_id,
            &turn.runtime_token,
            Some("runtime exited with an error"),
        )
        .expect("the exit should terminalize the turn");

    let (_, observation) = turn.observation();
    assert_eq!(observation["outcome"], "failed");
    assert_eq!(observation["effect"], "observe");
}

#[test]
fn a_silent_runtime_exit_reports_an_unknown_observation() {
    let turn = start_mediated_turn(
        "exit-silent",
        ChildWorkspace::ReadOnly,
        ControlBinding::Claimed,
    );

    turn.state
        .handle_runtime_exit_if_matches(&turn.child_id, &turn.runtime_token, None)
        .expect("the exit should terminalize the turn");

    let (_, observation) = turn.observation();
    assert_eq!(observation["outcome"], "unknown");
}

#[test]
fn a_turn_marked_in_error_reports_a_failed_observation() {
    let turn = start_mediated_turn(
        "marked-error",
        ChildWorkspace::ReadOnly,
        ControlBinding::Claimed,
    );

    turn.state
        .mark_turn_error_if_runtime_matches(
            &turn.child_id,
            &turn.runtime_token,
            "the runtime reported an error",
        )
        .expect("the error should terminalize the turn");

    let (_, observation) = turn.observation();
    assert_eq!(observation["outcome"], "failed");
}

#[test]
fn a_confirmed_failure_through_the_atomic_path_reports_a_failed_observation() {
    // The shared-runtime start-error path terminalizes through the atomic
    // helper with a matching runtime: that is a confirmed failure, not an
    // indeterminate ending.
    let turn = start_mediated_turn(
        "atomic-failure",
        ChildWorkspace::ReadOnly,
        ControlBinding::Claimed,
    );
    let active_turn_generation = {
        let inner = turn.state.inner.lock().expect("state mutex poisoned");
        inner.sessions[inner
            .find_session_index(&turn.child_id)
            .expect("child should exist")]
        .active_turn_generation
    };

    assert!(
        turn.state
            .fail_turn_if_runtime_matches_or_missing(
                &turn.child_id,
                &turn.runtime_token,
                active_turn_generation,
                "turn/start failed",
            )
            .expect("the failure should terminalize the turn"),
        "the matching runtime owns the failure"
    );

    let (_, observation) = turn.observation();
    assert_eq!(observation["outcome"], "failed");
}

/// A linked worktree of the claimed root's repository under `.worktrees/wt`,
/// which the main checkout ignores (its untracked nested repository would
/// otherwise leave main without a content revision).
fn add_claimed_root_worktree(root: &FsPath) -> PathBuf {
    fs::write(root.join(".gitignore"), ".worktrees/\n").expect("ignore file should write");
    run_git_test_command(root, &["add", ".gitignore"]);
    run_git_test_command(root, &["commit", "--quiet", "-m", "ignore worktrees"]);
    let worktree = root.join(".worktrees").join("wt");
    run_git_test_command(
        root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "wt",
            worktree.to_str().expect("a UTF-8 test path"),
        ],
    );
    worktree
}

/// Names `worktree` as the source root of the claimed root's work through the
/// tool's handler, with the store identity and the held claim a real Engram
/// gives: the claim the session is bound to by `ClaimedRoot::new`.
fn name_claimed_root_source_root(
    claimed: &ClaimedRoot,
    label: &str,
    worktree: &FsPath,
) -> EngramSourceRootResponse {
    let binding = test_control_work_binding(&format!("turn-observation-{label}"), 1);
    prepare_claimed_root_naming(claimed, label);
    {
        let mut inner = claimed.state.inner.lock().expect("state mutex poisoned");
        for project in &mut inner.projects {
            if let Some(engram) = project.engram.as_mut() {
                engram.authority_store_key = Some(EngramAuthorityStoreKey {
                    database_path: claimed.root.join("engram.db"),
                    project_id: "github.com/example/source-root".to_owned(),
                });
            }
        }
    }
    claimed.transport.script_held_claims([Ok(EngramHeldClaims {
        items: vec![EngramHeldClaim {
            work_id: binding.work_id.clone(),
            short_ref: format!("w-{label}"),
            claim_id: binding.claim_id.clone(),
            claim_fence: binding.claim_fence,
            focused: true,
            control_binding: Some(binding),
        }],
        omitted: 0,
    })]);
    claimed
        .state
        .name_engram_source_root(
            &claimed.session_id,
            EngramSourceRootRequest {
                work: format!("w-{label}"),
                path: Some(Some(worktree.to_string_lossy().into_owned())),
            },
        )
        .expect("the claim's holder names a registered worktree of its repository")
}

fn prepare_claimed_root_naming(claimed: &ClaimedRoot, label: &str) {
    let binding = test_control_work_binding(&format!("turn-observation-{label}"), 1);
    claimed
        .transport
        .enable_named_roots(&claimed.session_id, &binding);
    claimed.record(|record| {
        if record.engram.routing_token.is_none() {
            record.engram.rebind_required = true;
        }
        record
            .engram
            .routing_token
            .get_or_insert_with(|| "fixture-root-token".to_owned());
        record.engram.work_binding.get_or_insert(binding);
    });
}

/// Positive observation fixtures model verified settings before admission;
/// the actual bind/read/ACK and begin path still establishes the opening.
fn prepare_confirmed_claimed_opening(claimed: &ClaimedRoot, label: &str) {
    let store = EngramAuthorityStoreKey {
        database_path: claimed.root.join("engram.db"),
        project_id: "github.com/example/source-root".to_owned(),
    };
    let mut inner = claimed.state.inner.lock().unwrap();
    let project_id = inner
        .sessions
        .iter()
        .find(|r| r.session.id == claimed.session_id)
        .unwrap()
        .session
        .project_id
        .clone()
        .unwrap();
    inner
        .projects
        .iter_mut()
        .find(|p| p.id == project_id)
        .unwrap()
        .engram
        .as_mut()
        .unwrap()
        .authority_store_key = Some(store);
    drop(inner);
    let binding = test_control_work_binding(&format!("turn-observation-{label}"), 1);
    claimed.transport.register_named_root_run(&binding);
}

/// The prompt text the root's runtime received.
fn received_prompt(claimed: &ClaimedRoot) -> String {
    match receive(
        &claimed.runtime_rx,
        "runtime should receive the root prompt",
    ) {
        CodexRuntimeCommand::Prompt { command, .. } => command.prompt,
        _ => panic!("expected the root's runtime to receive a prompt"),
    }
}

#[test]
fn a_turn_on_a_claim_with_a_named_source_root_is_measured_in_that_worktree() {
    // A session rooted in the main checkout that works in a worktree names it
    // once; its turns on that claim are measured there.
    let label = "named-root";
    let grant_id = "turn-observation-named-root-grant";
    let claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind_reply("turn-observation-named-root-token"),
            grant_reply(grant_id),
            begin_reply(grant_id),
            checkpoint_reply(grant_id),
        ],
    );
    let worktree = add_claimed_root_worktree(&claimed.root);
    let named = name_claimed_root_source_root(&claimed, label, &worktree);
    let canonical = fs::canonicalize(&worktree).expect("the worktree should canonicalize");
    assert_eq!(
        named.root.as_deref().map(PathBuf::from),
        Some(canonical.clone())
    );
    assert_eq!(named.generation, 1);
    assert_eq!(named.source_revision, Some(content_revision_of(&worktree)));

    deliver_turn_dispatch(&claimed.state, claimed.dispatch())
        .expect("the begun root turn should reach the runtime");
    let prompt = received_prompt(&claimed);
    assert!(
        prompt.contains(&format!(
            "Engram source basis for w-{label}: its named source root"
        )),
        "the agent is told where its turns are measured: {prompt}"
    );
    let runtime_token = claimed.runtime_token();
    fs::write(
        worktree.join("README.md"),
        "changed in the named worktree\n",
    )
    .expect("worktree edit should write");

    claimed
        .state
        .finish_turn_ok_if_runtime_matches(&claimed.session_id, &runtime_token)
        .expect("root turn should complete");

    let checkpoint = claimed
        .transport
        .requests()
        .into_iter()
        .find(|request| request.request["operation"] == "turn_checkpoint")
        .expect("the root turn is checkpointed")
        .request;
    let observation = &checkpoint["observations"][0];
    assert_eq!(observation["source_changed"], true, "{checkpoint:#}");
    assert_eq!(
        PathBuf::from(
            observation["source_basis"]["workspace_id"]
                .as_str()
                .expect("workspace id")
        ),
        canonical,
        "the basis is the named worktree's, not the main checkout's"
    );
    assert_eq!(
        observation["source_basis"]["source_revision"].as_str(),
        Some(content_revision_of(&worktree).as_str())
    );
}

/// A claimed root in a turn with a linked worktree; `name_first` names it as
/// the work's source root before the turn is dispatched, else during it.
fn named_root_turn(label: &str, name_first: bool) -> (ClaimedRoot, PathBuf, RuntimeToken) {
    let grant_id = format!("turn-observation-{label}-grant");
    let claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind_reply(&format!("turn-observation-{label}-token")),
            grant_reply(&grant_id),
            begin_reply(&grant_id),
            checkpoint_reply(&grant_id),
        ],
    );
    prepare_confirmed_claimed_opening(&claimed, label);
    let worktree = add_claimed_root_worktree(&claimed.root);
    if name_first {
        name_claimed_root_source_root(&claimed, label, &worktree);
    }
    let delivery = deliver_turn_dispatch(&claimed.state, claimed.dispatch());
    assert!(matches!(delivery, TurnDispatchDeliveryOutcome::Delivered),
        "the positive named-root fixture must deliver: {delivery:?}; {}",
        claimed.record(|record| record.session.preview.clone()));
    let _ = received_prompt(&claimed);
    if !name_first {
        name_claimed_root_source_root(&claimed, label, &worktree);
    }
    let runtime_token = claimed.runtime_token();
    (claimed, worktree, runtime_token)
}

/// Completes the claimed root's turn and returns its one observation.
fn finish_claimed_turn(claimed: &ClaimedRoot, runtime_token: &RuntimeToken) -> Value {
    claimed
        .state
        .finish_turn_ok_if_runtime_matches(&claimed.session_id, runtime_token)
        .expect("root turn should complete");
    let checkpoint = claimed
        .transport
        .requests()
        .into_iter()
        .find(|request| request.request["operation"] == "turn_checkpoint")
        .expect("the root turn is checkpointed")
        .request;
    checkpoint["observations"][0].clone()
}

#[test]
fn a_root_named_during_a_turn_takes_effect_at_the_next_one() {
    // The turn running when the name is given keeps the root it began with:
    // its basis is not moved, and the agent is told for the next turn.
    let (claimed, worktree, runtime_token) = named_root_turn("named-mid-turn", false);
    fs::write(worktree.join("README.md"), "changed in the worktree\n").expect("worktree edit");
    let line = claimed
        .record(|record| record.engram.pending_source_root_line.clone())
        .expect("the next prompt carries the new root");
    assert!(line.contains("its named source root"), "{line}");
    let root = engram_source_root_display(
        &fs::canonicalize(&worktree)
            .expect("the worktree canonicalizes")
            .to_string_lossy(),
    );
    // A Codex session reports its directory and wraps its lines, so it is
    // not told the one-call form, whatever the root's path.
    assert!(
        !line.contains("pushd"),
        "no form the agent could not use: {line}"
    );
    assert!(
        line.contains(&format!(
            "its named source root {root}. Tests count for it only when they run there."
        )),
        "{line}"
    );

    let observation = finish_claimed_turn(&claimed, &runtime_token);

    assert_eq!(
        PathBuf::from(
            observation["source_basis"]["workspace_id"]
                .as_str()
                .expect("workspace id")
        ),
        fs::canonicalize(&claimed.root).expect("the root should canonicalize"),
        "this turn is still measured in the workdir it began with"
    );
    assert_eq!(
        observation["source_changed"], false,
        "the worktree's edit is not this turn's change of its workdir"
    );
}

#[test]
fn without_a_closing_basis_a_watcher_event_outside_the_named_root_is_no_change() {
    let (claimed, worktree, runtime_token) = named_root_turn("named-no-closing-outside", true);
    // The closing capture of the root fails: an untracked nested repository.
    let nested = worktree.join("nested");
    fs::create_dir_all(&nested).expect("nested directory");
    init_git_repository(&nested);
    // The watcher saw only a write in the main checkout.
    claimed.record(|record| {
        record.active_turn_file_changes.insert(
            claimed
                .root
                .join("README.md")
                .to_string_lossy()
                .into_owned(),
            WorkspaceFileChangeKind::Modified,
        );
    });

    let observation = finish_claimed_turn(&claimed, &runtime_token);

    assert!(observation.get("source_basis").is_none(), "{observation:#}");
    assert_eq!(
        observation["source_changed"], false,
        "the hint counts only paths inside the turn's root"
    );
}

#[test]
fn without_a_closing_basis_a_watcher_event_inside_the_named_root_is_a_change() {
    let (claimed, worktree, runtime_token) = named_root_turn("named-no-closing-inside", true);
    let nested = worktree.join("nested");
    fs::create_dir_all(&nested).expect("nested directory");
    init_git_repository(&nested);
    claimed.record(|record| {
        record.active_turn_file_changes.insert(
            worktree.join("README.md").to_string_lossy().into_owned(),
            WorkspaceFileChangeKind::Modified,
        );
    });

    let observation = finish_claimed_turn(&claimed, &runtime_token);

    assert!(observation.get("source_basis").is_none(), "{observation:#}");
    assert_eq!(
        observation["source_changed"], true,
        "without a closing basis the hint inside the root decides"
    );
}

#[test]
fn an_edit_in_the_main_checkout_is_no_change_of_a_turn_measured_in_a_named_root() {
    let label = "named-root-main-edit";
    let grant_id = "turn-observation-named-root-main-edit-grant";
    let claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind_reply("turn-observation-named-root-main-edit-token"),
            grant_reply(grant_id),
            begin_reply(grant_id),
            checkpoint_reply(grant_id),
        ],
    );
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_claimed_root_source_root(&claimed, label, &worktree);
    deliver_turn_dispatch(&claimed.state, claimed.dispatch())
        .expect("the begun root turn should reach the runtime");
    let _ = received_prompt(&claimed);
    let runtime_token = claimed.runtime_token();
    // Another session's edit in the shared main checkout, which the watcher
    // attributes to every session rooted there.
    fs::write(claimed.root.join("README.md"), "changed in main\n").expect("main edit");
    claimed.record(|record| {
        record.active_turn_file_changes.insert(
            claimed
                .root
                .join("README.md")
                .to_string_lossy()
                .into_owned(),
            WorkspaceFileChangeKind::Modified,
        );
    });

    claimed
        .state
        .finish_turn_ok_if_runtime_matches(&claimed.session_id, &runtime_token)
        .expect("root turn should complete");

    let checkpoint = claimed
        .transport
        .requests()
        .into_iter()
        .find(|request| request.request["operation"] == "turn_checkpoint")
        .expect("the root turn is checkpointed")
        .request;
    let observation = &checkpoint["observations"][0];
    assert_eq!(
        observation["source_changed"], false,
        "the named worktree did not move: {checkpoint:#}"
    );
    assert_eq!(
        PathBuf::from(
            observation["source_basis"]["workspace_id"]
                .as_str()
                .expect("workspace id")
        ),
        fs::canonicalize(&worktree).expect("the worktree should canonicalize")
    );
}

#[path = "engram_source_root_naming.rs"]
mod source_root_naming;
