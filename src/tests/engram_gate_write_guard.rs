// Owns the end-to-end regressions for the host's guard on Claude file writes
// into a worktree whose credited full gate has not settled: a Write, Edit,
// MultiEdit or NotebookEdit request whose target lies there is denied at the
// permission decision, before anything is written, whoever sends it (another
// session, the gate's own holder, a subagent), through a relative or linked
// path too; a delayed approval, for the session too, is checked again before
// it is allowed; the denied request fences nothing, so the gate keeps its
// credit; a target the watcher would not route to the gate's worktree, and a
// gate that can no longer be credited, are left to the ordinary policy, while
// a run read as ended stays protected, its denial naming what releases it
// (its holder's next checkpoint, or a refocus on its launch claim, or its
// expiry); and with no gate there the ordinary policy answers. Does not own the watcher, edit and
// command fences (the parent module), or Bash commands, which the guard does
// not parse. New module, a child of src/tests/engram_carried_checks.rs,
// whose gate fixtures it uses.
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

/// Asserts `decision` denies `request_id` because a credited full gate runs
/// in the target's worktree, and the denial says so.
fn assert_denied_for_gate(decision: &ClaudePermissionDecision, request_id: &str) {
    match decision {
        ClaudePermissionDecision::Deny {
            request_id: denied,
            message,
        } => {
            assert_eq!(denied, request_id);
            assert!(
                message.contains("full gate"),
                "the denial names the gate it protects: {message}"
            );
        }
        ClaudePermissionDecision::Allow { .. } => {
            panic!("{request_id}: the write is denied, not allowed")
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
fn claude_session_in(turn: &CheckedTurn, directory: &FsPath) -> String {
    let project_id = turn.record(|record| record.session.project_id.clone().unwrap());
    create_test_project_session(&turn.state, Agent::Claude, &project_id, directory)
}

#[test]
fn another_sessions_file_writes_into_a_running_gate_are_denied_and_it_keeps_its_credit() {
    let (turn, worktree) = named_turn("guard-other-session");
    let elsewhere = sibling_worktree(&turn, "guard-other-session-elsewhere");
    let writer = claude_session_in(&turn, &elsewhere);
    launch_gate(&turn, &worktree, false);
    let mut requester = Requester::on(&turn.state, &writer, ClaudeApprovalMode::AutoApprove);
    let target = worktree.join("README.md");
    let before = fs::read(&target).expect("the tracked file exists");
    for (id, tool, input) in file_writes(&target) {
        requester.request(id, tool, input, None);
        assert_denied_for_gate(&requester.answer(), id);
    }
    assert_eq!(fs::read(&target).unwrap(), before, "nothing was written");
    assert!(
        !target.with_extension("ipynb").exists(),
        "no notebook was created"
    );
    assert_eq!(
        carried_fence(&turn),
        None,
        "a denied request fences nothing"
    );
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();
    assert_credited(&turn, &checkpoint, &run, "succeeded");
}

#[test]
fn the_gate_holders_own_file_write_into_its_running_gate_is_denied() {
    let (turn, worktree) = named_turn("guard-holder");
    launch_gate(&turn, &worktree, false);
    let mut requester = Requester::on(
        &turn.state,
        &turn.session_id,
        ClaudeApprovalMode::AutoApprove,
    );
    let target = worktree.join("README.md");
    requester.request(
        "holder-edit",
        "Edit",
        file_writes(&target)[1].2.clone(),
        None,
    );
    assert_denied_for_gate(&requester.answer(), "holder-edit");
    assert_eq!(
        carried_fence(&turn),
        None,
        "a denied request fences nothing"
    );
}

#[test]
fn a_subagents_file_write_into_a_running_gate_is_denied_and_it_keeps_its_credit() {
    let (turn, worktree) = named_turn("guard-subagent");
    let elsewhere = sibling_worktree(&turn, "guard-subagent-elsewhere");
    let writer = claude_session_in(&turn, &elsewhere);
    launch_gate(&turn, &worktree, false);
    let mut requester = Requester::on(&turn.state, &writer, ClaudeApprovalMode::AutoApprove);
    let target = worktree.join("README.md");
    requester.request(
        "subagent-write",
        "Write",
        file_writes(&target)[0].2.clone(),
        Some("toolu-task"),
    );
    assert_denied_for_gate(&requester.answer(), "subagent-write");
    assert_eq!(
        carried_fence(&turn),
        None,
        "a denied request fences nothing"
    );
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();
    assert_credited(&turn, &checkpoint, &run, "succeeded");
}

#[test]
fn a_relative_or_linked_target_inside_a_running_gate_is_denied() {
    // Relative to the requesting tool's directory, which is the worktree.
    let (turn, worktree) = named_turn("guard-relative");
    let inside = claude_session_in(&turn, &worktree);
    launch_gate(&turn, &worktree, false);
    let mut requester = Requester::on(&turn.state, &inside, ClaudeApprovalMode::AutoApprove);
    requester.request(
        "relative-edit",
        "Edit",
        json!({"file_path": "README.md", "old_string": "checked", "new_string": "changed"}),
        None,
    );
    assert_denied_for_gate(&requester.answer(), "relative-edit");
    // A file not created yet, under a directory not created yet.
    requester.request(
        "new-file",
        "Write",
        json!({"file_path": worktree.join("src").join("new").join("file.rs"), "content": "x"}),
        None,
    );
    assert_denied_for_gate(&requester.answer(), "new-file");

    // Through a link to the worktree from outside it.
    let (turn, worktree) = named_turn("guard-linked");
    let elsewhere = sibling_worktree(&turn, "guard-linked-elsewhere");
    let link = elsewhere.join("gate-link");
    link_directory(&link, &worktree);
    let writer = claude_session_in(&turn, &elsewhere);
    launch_gate(&turn, &worktree, false);
    let mut requester = Requester::on(&turn.state, &writer, ClaudeApprovalMode::AutoApprove);
    requester.request(
        "linked-write",
        "Write",
        json!({"file_path": link.join("README.md"), "content": "changed\n"}),
        None,
    );
    assert_denied_for_gate(&requester.answer(), "linked-write");
    assert_eq!(
        carried_fence(&turn),
        None,
        "a denied request fences nothing"
    );
}

#[test]
fn an_approval_given_after_the_gate_started_is_checked_again_and_denied() {
    let (turn, worktree) = named_turn("guard-delayed");
    let elsewhere = sibling_worktree(&turn, "guard-delayed-elsewhere");
    let writer = claude_session_in(&turn, &elsewhere);
    let mut requester = Requester::on(&turn.state, &writer, ClaudeApprovalMode::Ask);
    let target = worktree.join("README.md");
    // Asked before the gate starts: no gate protects the worktree yet, so the
    // request waits for the user, as every Ask request does.
    requester.request(
        "delayed-edit",
        "Edit",
        file_writes(&target)[1].2.clone(),
        None,
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
    launch_gate(&turn, &worktree, false);
    turn.state
        .update_approval(&writer, &message_id, ApprovalDecision::Accepted)
        .expect("the approval is delivered");
    assert_denied_for_gate(&requester.answer(), "delayed-edit");
    assert_eq!(
        carried_fence(&turn),
        None,
        "a denied request fences nothing"
    );
}

#[test]
fn an_approval_for_the_session_after_the_gate_started_is_denied_and_changes_no_mode() {
    let (turn, worktree) = named_turn("guard-delayed-session");
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
    launch_gate(&turn, &worktree, false);
    turn.state
        .update_approval(&writer, &message_id, ApprovalDecision::AcceptedForSession)
        .expect("the approval is delivered");
    let (answers, modes) = requester.received();
    assert_eq!(answers.len(), 1, "one answer");
    assert_denied_for_gate(&answers[0], "session-edit");
    assert!(
        modes.is_empty(),
        "a denied approval changes no mode: {modes:?}"
    );
}

#[test]
fn a_delayed_approval_resolves_a_relative_target_where_the_request_arrived() {
    // The later check resolves a relative target against the runtime's own
    // directory, kept with the request, not against the session's directory
    // read at approval time, which may name another place by then.
    let (turn, worktree) = named_turn("guard-delayed-relative");
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
    launch_gate(&turn, &worktree, false);
    turn.state
        .update_approval(&writer, &message_id, ApprovalDecision::Accepted)
        .expect("the approval is delivered");
    assert_denied_for_gate(&requester.answer(), "delayed-relative");
}

#[test]
fn a_write_the_watcher_would_not_route_to_the_gates_worktree_is_not_denied() {
    let (turn, worktree) = named_turn("guard-elsewhere-in-tree");
    // A plain directory beside the worktree whose name shares its prefix.
    let beside = worktree.with_file_name("wt-beside");
    fs::create_dir_all(&beside).expect("the directory beside exists");
    let writer = claude_session_in(&turn, &worktree);
    launch_gate(&turn, &worktree, false);
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
    // A tracked file of the gate's worktree, from the same session, still is.
    requester.request(
        "tracked",
        "Edit",
        file_writes(&worktree.join("README.md"))[1].2.clone(),
        None,
    );
    assert_denied_for_gate(&requester.answer(), "tracked");
}

#[test]
fn a_gate_that_can_no_longer_be_credited_protects_nothing_and_an_ended_one_still_does() {
    let expired = (chrono::Utc::now()
        - chrono::Duration::seconds(ENGRAM_CARRIED_CHECK_TTL_SECONDS + 60))
    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let cases: Vec<(&str, Box<dyn Fn(&mut EngramCarriedCheck)>, bool)> = vec![
        (
            "fenced",
            Box::new(|carried| carried.fence = Some("an earlier write".to_owned())),
            false,
        ),
        (
            "by-report",
            Box::new(|carried| carried.ends_by_report = true),
            false,
        ),
        (
            "expired",
            Box::new(move |carried| carried.check.started_at = expired.clone()),
            false,
        ),
        (
            "launcher-gone",
            Box::new(|carried| carried.launcher_gone = true),
            false,
        ),
        (
            "ambiguous",
            Box::new(|carried| carried.ambiguous = true),
            false,
        ),
        (
            "conflict",
            Box::new(|carried| {
                carried.terminal_digest = Some("a".repeat(64));
                carried.terminal_conflict = true;
            }),
            false,
        ),
        // Read as ended, not settled: a change the watcher reports now could
        // still have landed while it ran.
        (
            "ended",
            Box::new(|carried| carried.terminal_digest = Some("a".repeat(64))),
            true,
        ),
    ];
    for (label, change, protected) in cases {
        let (turn, worktree) = named_turn(&format!("guard-state-{label}"));
        let elsewhere = sibling_worktree(&turn, &format!("guard-state-{label}-elsewhere"));
        let writer = claude_session_in(&turn, &elsewhere);
        launch_gate(&turn, &worktree, false);
        turn.record_mut(|record| change(&mut record.engram.carried_checks[0]));
        let mut requester = Requester::on(&turn.state, &writer, ClaudeApprovalMode::AutoApprove);
        requester.request(
            label,
            "Edit",
            file_writes(&worktree.join("README.md"))[1].2.clone(),
            None,
        );
        if protected {
            assert_denied_for_gate(&requester.answer(), label);
        } else {
            assert_allowed(&requester.answer(), label);
        }
    }
}

#[test]
fn an_ended_gate_whose_holder_moved_its_focus_names_the_release_it_waits_for() {
    // Read as ended and not settled: only a checkpoint on its launch claim
    // settles it, so the denial says when the protection ends.
    let (turn, worktree) = named_turn("guard-ended-release");
    let elsewhere = sibling_worktree(&turn, "guard-ended-release-elsewhere");
    let writer = claude_session_in(&turn, &elsewhere);
    launch_gate(&turn, &worktree, false);
    turn.record_mut(|record| {
        record.engram.carried_checks[0].terminal_digest = Some("a".repeat(64));
    });
    let mut requester = Requester::on(&turn.state, &writer, ClaudeApprovalMode::AutoApprove);
    let edit = file_writes(&worktree.join("README.md"))[1].2.clone();
    let denial = |decision: ClaudePermissionDecision, id: &str| {
        assert_denied_for_gate(&decision, id);
        match decision {
            ClaudePermissionDecision::Deny { message, .. } => message,
            ClaudePermissionDecision::Allow { .. } => unreachable!(),
        }
    };

    // Its holder is still focused on the launch claim: its next checkpoint
    // settles it.
    requester.request("still-focused", "Edit", edit.clone(), None);
    let message = denial(requester.answer(), "still-focused");
    assert!(
        message.contains(&turn.session_id)
            && message.contains("next checkpoint")
            && message.contains("expires at "),
        "the denial names the holder, its next checkpoint and the expiry: {message}"
    );

    // Its holder's focus moved to another claim: its next checkpoint settles
    // nothing, and the denial says what does.
    turn.record_mut(|record| {
        let binding = record
            .engram
            .work_binding
            .as_mut()
            .expect("the holder holds a claim");
        binding.claim_id = "another-claim".to_owned();
    });
    requester.request("moved-focus", "Edit", edit, None);
    let message = denial(requester.answer(), "moved-focus");
    assert!(
        message.contains(&turn.session_id)
            && message.contains("not focused on the claim that launched it")
            && message.contains("expires at ")
            && !message.contains("next checkpoint"),
        "the denial names the refocus or the expiry as the release: {message}"
    );
    assert_eq!(
        carried_fence(&turn),
        None,
        "a denied request fences nothing"
    );
}

#[test]
fn with_no_gate_running_the_ordinary_policy_answers() {
    let (turn, worktree) = named_turn("guard-no-gate");
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
        "auto-approve allows a write no gate protects"
    );
}
