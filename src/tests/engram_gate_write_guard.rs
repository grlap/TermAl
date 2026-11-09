// Real Claude permission frames obey the ordinary policy independently of
// carried artifacts. Permission responses are not tool execution: Ask still
// pauses, explicit rejection still denies, and acceptance keeps request
// identity/input and the session-mode contract without gating verification.
use super::*;

/// A Claude runtime reading frames for one session, through the production
/// frame application, whose permission answers can be read back.
struct Requester {
    context: ClaudeRuntimeContext,
    frames: ClaudeReaderFrames,
    recorder: SessionRecorder,
    commands: mpsc::Receiver<ClaudeRuntimeCommand>,
}

impl Requester {
    /// A runtime installed on `session_id`, working in the session's
    /// directory, answering by `mode`.
    fn on(state: &AppState, session_id: &str, mode: ClaudeApprovalMode) -> Self {
        let (runtime, commands) = test_claude_runtime_handle(&format!("gate-guard-{session_id}"));
        let token = RuntimeToken::Claude(runtime.runtime_id.clone());
        let input_tx = runtime.input_tx.clone();
        let cwd = {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .expect("the session exists");
            let record = &mut inner.sessions[index];
            record.runtime = SessionRuntime::Claude(runtime);
            record.session.claude_approval_mode = Some(mode);
            record.session.workdir.clone()
        };
        let context = ClaudeRuntimeContext::new(
            state.clone(),
            session_id.to_owned(),
            token,
            new_claude_turn_ownership(),
            Arc::new(Mutex::new(None)),
            cwd,
        );
        let frames = ClaudeReaderFrames::new(&context, input_tx, None);
        let recorder = SessionRecorder::new(state.clone(), session_id.to_owned());
        Self {
            context,
            frames,
            recorder,
            commands,
        }
    }

    /// Feeds the permission request `request_id` for `tool` with `input`,
    /// a subagent's when `parent` names its tool use.
    fn request(&mut self, request_id: &str, tool: &str, input: Value, parent: Option<&str>) {
        let mut frame = json!({"type": "control_request", "request_id": request_id,
            "request": {"subtype": "can_use_tool", "tool_name": tool, "input": input}});
        if let Some(parent) = parent {
            frame["parent_tool_use_id"] = json!(parent);
        }
        self.feed(&frame);
    }

    /// Feeds `frame` through the production frame application.
    fn feed(&mut self, frame: &Value) {
        let outcome =
            apply_claude_frame(&self.context, &mut self.frames, &mut self.recorder, frame);
        assert_eq!(outcome.next, ClaudeFrameApplied::Continue, "{frame}");
    }

    /// The one permission answer the runtime received since the last read.
    fn answer(&self) -> ClaudePermissionDecision {
        let (mut answers, _) = self.received();
        assert_eq!(answers.len(), 1, "one answer, not {}", answers.len());
        answers.remove(0)
    }

    /// The permission answers and the permission-mode changes the runtime
    /// received since the last read.
    fn received(&self) -> (Vec<ClaudePermissionDecision>, Vec<String>) {
        let (mut answers, mut modes) = (Vec::new(), Vec::new());
        for command in self.commands.try_iter() {
            match command {
                ClaudeRuntimeCommand::PermissionResponse(decision) => answers.push(decision),
                ClaudeRuntimeCommand::SetPermissionMode(mode) => modes.push(mode),
                _ => {}
            }
        }
        (answers, modes)
    }
}

/// Asserts `decision` allows the request: the ordinary policy answered.
fn assert_allowed(decision: &ClaudePermissionDecision, why: &str) {
    match decision {
        ClaudePermissionDecision::Allow { .. } => {}
        ClaudePermissionDecision::Deny { message, .. } => panic!("{why}: denied: {message}"),
    }
}

/// Inspect the real response's correlation, not a gate-shaped denial.
fn assert_allowed_request(decision: &ClaudePermissionDecision, expected_id: &str) {
    match decision {
        ClaudePermissionDecision::Allow { request_id, .. } => assert_eq!(request_id, expected_id),
        ClaudePermissionDecision::Deny { message, .. } => {
            panic!("{expected_id}: ordinary policy unexpectedly denied: {message}")
        }
    }
}

/// The four file-writing tools' requests for `target`.
fn file_writes(target: &FsPath) -> Vec<(&'static str, &'static str, Value)> {
    let notebook = target.with_extension("ipynb");
    vec![
        (
            "write",
            "Write",
            json!({"file_path": target, "content": "changed\n"}),
        ),
        (
            "edit",
            "Edit",
            json!({"file_path": target, "old_string": "checked", "new_string": "changed"}),
        ),
        (
            "multi-edit",
            "MultiEdit",
            json!({"file_path": target, "edits": [
                {"old_string": "checked", "new_string": "changed"}]}),
        ),
        (
            "notebook",
            "NotebookEdit",
            json!({"notebook_path": notebook, "new_source": "changed"}),
        ),
    ]
}

/// A Claude session of `turn`'s project working in `directory`.
fn claude_session_in(turn: &EOCheckedTurn, directory: &FsPath) -> String {
    let project_id = turn.record(|record| record.session.project_id.clone().unwrap());
    create_test_project_session(&turn.state, Agent::Claude, &project_id, directory)
}

#[test]
fn another_sessions_file_permissions_are_ordinary_and_do_not_execute_writes() {
    let (turn, worktree) = ordinary_carried_turn("guard-other-session");
    let elsewhere = sibling_worktree(&turn, "guard-other-session-elsewhere");
    let writer = claude_session_in(&turn, &elsewhere);
    ordinary_launch_gate(&turn, &worktree, false);
    let mut requester = Requester::on(&turn.state, &writer, ClaudeApprovalMode::AutoApprove);
    let target = worktree.join("README.md");
    let before = fs::read(&target).expect("the tracked file exists");
    for (id, tool, input) in file_writes(&target) {
        requester.request(id, tool, input, None);
        assert_allowed_request(&requester.answer(), id);
    }
    assert_eq!(fs::read(&target).unwrap(), before, "nothing was written");
    assert!(
        !target.with_extension("ipynb").exists(),
        "no notebook was created"
    );
    assert_eq!(
        turn.record(|record| record.engram.carried_checks[0].fence.clone()),
        None,
        "a permission response is not a tool write"
    );
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_artifact_custody(&turn, &run, true);
}

#[test]
fn the_artifact_holders_own_permission_uses_ordinary_policy() {
    let (turn, worktree) = ordinary_carried_turn("guard-holder");
    ordinary_launch_gate(&turn, &worktree, false);
    let mut requester = Requester::on(
        &turn.state,
        &turn.session_id,
        ClaudeApprovalMode::AutoApprove,
    );
    let target = worktree.join("README.md");
    let before = fs::read(&target).unwrap();
    requester.request(
        "holder-edit",
        "Edit",
        file_writes(&target)[1].2.clone(),
        None,
    );
    assert_allowed_request(&requester.answer(), "holder-edit");
    assert_eq!(
        turn.record(|record| record.engram.carried_checks[0].fence.clone()),
        None,
        "a permission response is not a tool write"
    );
    assert_eq!(fs::read(&target).unwrap(), before);
    assert_ordinary_turn_has_no_native_requests(&turn);
}

#[test]
fn a_subagents_file_permission_is_ordinary_and_preserves_artifact_custody() {
    let (turn, worktree) = ordinary_carried_turn("guard-subagent");
    let elsewhere = sibling_worktree(&turn, "guard-subagent-elsewhere");
    let writer = claude_session_in(&turn, &elsewhere);
    ordinary_launch_gate(&turn, &worktree, false);
    let mut requester = Requester::on(&turn.state, &writer, ClaudeApprovalMode::AutoApprove);
    let target = worktree.join("README.md");
    let before = fs::read(&target).unwrap();
    requester.request(
        "subagent-write",
        "Write",
        file_writes(&target)[0].2.clone(),
        Some("toolu-task"),
    );
    assert_allowed_request(&requester.answer(), "subagent-write");
    assert_eq!(
        turn.record(|record| record.engram.carried_checks[0].fence.clone()),
        None,
        "a permission response is not a tool write"
    );
    assert_eq!(fs::read(&target).unwrap(), before);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_artifact_custody(&turn, &run, true);
}

#[test]
fn relative_new_and_linked_targets_use_ordinary_permission_policy() {
    // Relative to the requesting tool's directory, which is the worktree.
    let (turn, worktree) = ordinary_carried_turn("guard-relative");
    let inside = claude_session_in(&turn, &worktree);
    ordinary_launch_gate(&turn, &worktree, false);
    let mut requester = Requester::on(&turn.state, &inside, ClaudeApprovalMode::AutoApprove);
    requester.request(
        "relative-edit",
        "Edit",
        json!({"file_path": "README.md", "old_string": "checked", "new_string": "changed"}),
        None,
    );
    assert_allowed_request(&requester.answer(), "relative-edit");
    // A file not created yet, under a directory not created yet.
    requester.request(
        "new-file",
        "Write",
        json!({"file_path": worktree.join("src").join("new").join("file.rs"), "content": "x"}),
        None,
    );
    assert_allowed_request(&requester.answer(), "new-file");

    // Through a link to the worktree from outside it.
    let (turn, worktree) = ordinary_carried_turn("guard-linked");
    let elsewhere = sibling_worktree(&turn, "guard-linked-elsewhere");
    let link = elsewhere.join("gate-link");
    link_directory(&link, &worktree);
    let writer = claude_session_in(&turn, &elsewhere);
    ordinary_launch_gate(&turn, &worktree, false);
    let mut requester = Requester::on(&turn.state, &writer, ClaudeApprovalMode::AutoApprove);
    requester.request(
        "linked-write",
        "Write",
        json!({"file_path": link.join("README.md"), "content": "changed\n"}),
        None,
    );
    assert_allowed_request(&requester.answer(), "linked-write");
    assert_eq!(
        turn.record(|record| record.engram.carried_checks[0].fence.clone()),
        None,
        "a permission response is not a tool write"
    );
}

#[test]
fn ask_acceptance_and_rejection_are_not_overridden_by_a_carried_artifact() {
    let (turn, worktree) = ordinary_carried_turn("guard-delayed");
    let elsewhere = sibling_worktree(&turn, "guard-delayed-elsewhere");
    let writer = claude_session_in(&turn, &elsewhere);
    let mut requester = Requester::on(&turn.state, &writer, ClaudeApprovalMode::Ask);
    let target = worktree.join("README.md");
    // Ask waits for a real user decision, before and after an artifact starts.
    requester.request(
        "delayed-edit",
        "Edit",
        file_writes(&target)[1].2.clone(),
        None,
    );
    let (answers, modes) = requester.received();
    assert!(
        answers.is_empty() && modes.is_empty(),
        "Ask has not authorized anything"
    );
    let message_id = {
        let inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&writer).unwrap();
        inner.sessions[index]
            .pending_claude_approvals
            .keys()
            .next()
            .cloned()
            .expect("the request waits for the user")
    };
    ordinary_launch_gate(&turn, &worktree, false);
    turn.state
        .update_approval(&writer, &message_id, ApprovalDecision::Accepted)
        .expect("the approval is delivered");
    let (answers, modes) = requester.received();
    assert_eq!(answers.len(), 1);
    assert!(
        modes.is_empty(),
        "one acceptance does not change the session mode"
    );
    assert_allowed_request(&answers[0], "delayed-edit");
    assert_eq!(
        turn.record(|record| record.engram.carried_checks[0].fence.clone()),
        None,
        "a permission response is not a tool write"
    );
    requester.request(
        "rejected-edit",
        "Edit",
        file_writes(&target)[1].2.clone(),
        None,
    );
    let (answers, modes) = requester.received();
    assert!(answers.is_empty() && modes.is_empty());
    let rejected_id = {
        let inner = turn.state.inner.lock().unwrap();
        let index = inner.find_session_index(&writer).unwrap();
        inner.sessions[index]
            .pending_claude_approvals
            .keys()
            .next()
            .unwrap()
            .clone()
    };
    turn.state
        .update_approval(&writer, &rejected_id, ApprovalDecision::Rejected)
        .unwrap();
    match requester.answer() {
        ClaudePermissionDecision::Deny {
            request_id,
            message,
        } => {
            assert_eq!(request_id, "rejected-edit");
            assert_eq!(message, "User rejected this action in TermAl.");
        }
        ClaudePermissionDecision::Allow { .. } => panic!("explicit rejection must deny"),
    }
    assert_ordinary_turn_has_no_native_requests(&turn);
}

#[test]
fn accepted_for_session_delivers_its_offered_mode_despite_a_carried_artifact() {
    let (turn, worktree) = ordinary_carried_turn("guard-delayed-session");
    let elsewhere = sibling_worktree(&turn, "guard-delayed-session-elsewhere");
    let writer = claude_session_in(&turn, &elsewhere);
    let mut requester = Requester::on(&turn.state, &writer, ClaudeApprovalMode::Ask);
    let request = json!({"subtype": "can_use_tool", "tool_name": "Edit",
        "input": file_writes(&worktree.join("README.md"))[1].2.clone(),
        "permission_suggestions": [
            {"type": "setMode", "mode": "acceptEdits", "destination": "session"}]});
    requester.feed(
        &json!({"type": "control_request", "request_id": "session-edit",
        "request": request}),
    );
    let (answers, modes) = requester.received();
    assert!(
        answers.is_empty() && modes.is_empty(),
        "Ask waits for the session decision"
    );
    let message_id = {
        let inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&writer).unwrap();
        let (message_id, pending) = inner.sessions[index]
            .pending_claude_approvals
            .iter()
            .next()
            .expect("the request waits for the user");
        assert_eq!(
            pending.permission_mode_for_session.as_deref(),
            Some("acceptEdits"),
            "the request offers a mode for the session"
        );
        message_id.clone()
    };
    ordinary_launch_gate(&turn, &worktree, false);
    turn.state
        .update_approval(&writer, &message_id, ApprovalDecision::AcceptedForSession)
        .expect("the approval is delivered");
    let (answers, modes) = requester.received();
    assert_eq!(answers.len(), 1, "one answer");
    assert_allowed_request(&answers[0], "session-edit");
    assert_eq!(
        modes,
        vec!["acceptEdits".to_owned()],
        "AcceptedForSession delivers the offered ordinary session mode"
    );
    assert_ordinary_turn_has_no_native_requests(&turn);
}

#[test]
fn a_delayed_approval_preserves_relative_input_after_the_session_moves() {
    // Ordinary approval preserves the runtime's original request/input.
    // Moving the session's displayed directory does not rewrite that input.
    let (turn, worktree) = ordinary_carried_turn("guard-delayed-relative");
    let elsewhere = sibling_worktree(&turn, "guard-delayed-relative-elsewhere");
    let writer = claude_session_in(&turn, &worktree);
    let mut requester = Requester::on(&turn.state, &writer, ClaudeApprovalMode::Ask);
    requester.request(
        "delayed-relative",
        "Edit",
        json!({"file_path": "README.md", "old_string": "checked", "new_string": "changed"}),
        None,
    );
    let message_id = {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&writer).unwrap();
        let record = &mut inner.sessions[index];
        // The session's directory now names another worktree.
        record.session.workdir = elsewhere.to_string_lossy().into_owned();
        record
            .pending_claude_approvals
            .keys()
            .next()
            .cloned()
            .expect("the request waits for the user")
    };
    ordinary_launch_gate(&turn, &worktree, false);
    turn.state
        .update_approval(&writer, &message_id, ApprovalDecision::Accepted)
        .expect("the approval is delivered");
    match requester.answer() {
        ClaudePermissionDecision::Allow {
            request_id,
            updated_input,
        } => {
            assert_eq!(request_id, "delayed-relative");
            assert_eq!(
                updated_input,
                json!({
                "file_path": "README.md", "old_string": "checked", "new_string": "changed"})
            );
        }
        ClaudePermissionDecision::Deny { message, .. } => panic!("{message}"),
    }
    assert_eq!(requester.context.cwd, worktree.to_string_lossy());
    assert_ordinary_turn_has_no_native_requests(&turn);
}

#[test]
fn ordinary_permissions_cover_nested_beside_scratch_ignored_and_tracked_targets() {
    let (turn, worktree) = ordinary_carried_turn("guard-elsewhere-in-tree");
    // A plain directory beside the worktree whose name shares its prefix.
    let beside = worktree.with_file_name("wt-beside");
    fs::create_dir_all(&beside).expect("the directory beside exists");
    let writer = claude_session_in(&turn, &worktree);
    ordinary_launch_gate(&turn, &worktree, false);
    // A repository of its own nested in the gate's worktree, made after the
    // launch: one there before it leaves the launch snapshot without a basis.
    let nested = worktree.join("nested");
    fs::create_dir_all(&nested).expect("the nested repository's directory exists");
    run_git_test_command(&nested, &["init", "--quiet"]);
    let mut requester = Requester::on(&turn.state, &writer, ClaudeApprovalMode::AutoApprove);
    for (id, target, why) in [
        (
            "nested",
            nested.join("x.rs"),
            "a nested worktree is its own",
        ),
        (
            "beside",
            beside.join("x.rs"),
            "a directory sharing the root's prefix is outside it",
        ),
        (
            "scratch",
            worktree.join(".tmp").join("x.txt"),
            "the worktree's .tmp/ is scratch",
        ),
        (
            "ignored",
            worktree.join("target").join("x.txt"),
            "the watcher ignores target/",
        ),
    ] {
        requester.request(
            id,
            "Write",
            json!({"file_path": target, "content": "x"}),
            None,
        );
        assert_allowed(&requester.answer(), why);
    }
    // The tracked-file control obeys the same ordinary policy.
    requester.request(
        "tracked",
        "Edit",
        file_writes(&worktree.join("README.md"))[1].2.clone(),
        None,
    );
    assert_allowed_request(&requester.answer(), "tracked");
    assert_ordinary_turn_has_no_native_requests(&turn);
}

#[test]
fn carried_artifact_states_do_not_override_ordinary_permission_policy() {
    let expired = (chrono::Utc::now()
        - chrono::Duration::seconds(ENGRAM_CARRIED_CHECK_TTL_SECONDS + 60))
    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let cases: Vec<(&str, Box<dyn Fn(&mut EngramCarriedCheck)>)> = vec![
        (
            "fenced",
            Box::new(|carried| carried.fence = Some("an earlier write".to_owned())),
        ),
        (
            "by-report",
            Box::new(|carried| carried.ends_by_report = true),
        ),
        (
            "expired",
            Box::new(move |carried| carried.check.started_at = expired.clone()),
        ),
        (
            "launcher-gone",
            Box::new(|carried| carried.launcher_gone = true),
        ),
        ("ambiguous", Box::new(|carried| carried.ambiguous = true)),
        (
            "conflict",
            Box::new(|carried| {
                carried.terminal_digest = Some("a".repeat(64));
                carried.terminal_conflict = true;
            }),
        ),
        (
            "ended",
            Box::new(|carried| carried.terminal_digest = Some("a".repeat(64))),
        ),
    ];
    for (label, change) in cases {
        let (turn, worktree) = ordinary_carried_turn(&format!("ordinary-policy-state-{label}"));
        let elsewhere =
            sibling_worktree(&turn, &format!("ordinary-policy-state-{label}-elsewhere"));
        let writer = claude_session_in(&turn, &elsewhere);
        ordinary_launch_gate(&turn, &worktree, false);
        turn.record_mut(|record| change(&mut record.engram.carried_checks[0]));
        let before = fs::read(worktree.join("README.md")).unwrap();
        let mut requester = Requester::on(&turn.state, &writer, ClaudeApprovalMode::AutoApprove);
        requester.request(
            label,
            "Edit",
            file_writes(&worktree.join("README.md"))[1].2.clone(),
            None,
        );
        assert_allowed_request(&requester.answer(), label);
        assert_eq!(
            fs::read(worktree.join("README.md")).unwrap(),
            before,
            "permission delivery does not execute the tool"
        );
        assert_ordinary_turn_has_no_native_requests(&turn);
    }
}

#[test]
fn an_ended_artifact_and_a_changed_focus_do_not_create_a_permission_release_gate() {
    let (turn, worktree) = ordinary_carried_turn("ordinary-policy-ended-focus");
    let elsewhere = sibling_worktree(&turn, "ordinary-policy-ended-focus-elsewhere");
    let writer = claude_session_in(&turn, &elsewhere);
    ordinary_launch_gate(&turn, &worktree, false);
    let digest = "a".repeat(64);
    turn.record_mut(|record| {
        record.engram.carried_checks[0].terminal_digest = Some(digest.clone())
    });
    let before = fs::read(worktree.join("README.md")).unwrap();
    let mut requester = Requester::on(&turn.state, &writer, ClaudeApprovalMode::AutoApprove);
    let edit = file_writes(&worktree.join("README.md"))[1].2.clone();
    requester.request("still-focused", "Edit", edit.clone(), None);
    assert_allowed_request(&requester.answer(), "still-focused");
    turn.record_mut(|record| {
        record
            .engram
            .work_binding
            .as_mut()
            .expect("the ordinary holder has a claim")
            .claim_id = "another-claim".to_owned();
    });
    requester.request("moved-focus", "Edit", edit, None);
    assert_allowed_request(&requester.answer(), "moved-focus");
    turn.record(|record| {
        let carried = &record.engram.carried_checks[0];
        assert!(carried.fence.is_none());
        assert_eq!(carried.terminal_digest.as_deref(), Some(digest.as_str()));
        assert!(record.engram.carried_consumed_runs.is_empty());
    });
    assert_eq!(fs::read(worktree.join("README.md")).unwrap(), before);
    assert_ordinary_turn_has_no_native_requests(&turn);
}

#[test]
fn with_no_gate_running_the_ordinary_policy_answers() {
    let (turn, worktree) = ordinary_carried_turn("guard-no-gate");
    let elsewhere = sibling_worktree(&turn, "guard-no-gate-elsewhere");
    let writer = claude_session_in(&turn, &elsewhere);
    let mut requester = Requester::on(&turn.state, &writer, ClaudeApprovalMode::AutoApprove);
    requester.request(
        "free-edit",
        "Edit",
        file_writes(&worktree.join("README.md"))[1].2.clone(),
        None,
    );
    assert!(
        matches!(requester.answer(), ClaudePermissionDecision::Allow { .. }),
        "the real ordinary auto-approve policy allows the request"
    );
    assert_ordinary_turn_has_no_native_requests(&turn);
}
