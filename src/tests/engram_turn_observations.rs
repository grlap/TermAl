//! Execution observations on the end-of-turn checkpoint (Engram
//! w-108a13d58018, tm-winf): a mediated turn reports one observation on the
//! checkpoint that closes its grant, with `source_changed` decided by content
//! (the workspace's review-freeze fingerprint before the prompt reached the
//! runtime against the one at the close, the turn's file-change tracking
//! being only a lower bound), the outcome of the transition that closed it,
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

/// The review-freeze fingerprint of `root`, as the observation reports it.
fn freeze_fingerprint(root: &FsPath) -> String {
    review_freeze_fingerprint(root)
        .expect("the worktree should freeze")
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
    /// Records a source change the workspace watcher would have observed
    /// during the running turn.
    fn change_source(&self, path: &str) {
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
        freeze_fingerprint(&turn.child_workdir),
        "the revision is the review-freeze fingerprint of the full working content"
    );
    fs::write(turn.child_workdir.join("notes.txt"), "an untracked edit\n")
        .expect("untracked file should write");
    assert_ne!(
        reported_revision,
        freeze_fingerprint(&turn.child_workdir),
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
        freeze_fingerprint(&turn.child_workdir),
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
        freeze_fingerprint(&turn.child_workdir),
        "the basis is the unchanged content's fingerprint"
    );
    assert!(
        observation.get("observed_at").is_some(),
        "a basis comes with its time"
    );
}

#[test]
fn a_turn_without_a_begin_basis_under_a_mutation_grant_reports_a_change() {
    // The comparison cannot clear a turn whose begin-time capture failed.
    // Under a grant that mediates local mutation the conservative answer is
    // a change, with the closing basis a later check can match.
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
        observation["source_changed"], true,
        "an unclearable turn counts as changed under a mutation grant"
    );
    assert_eq!(observation["effect"], "mutate_local");
    assert_eq!(
        observation["source_basis"]["source_revision"]
            .as_str()
            .expect("source revision"),
        freeze_fingerprint(&turn.child_workdir),
        "the closing basis is still reported for a later check to match"
    );
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
        freeze_fingerprint(&turn.child_workdir).as_str()
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
