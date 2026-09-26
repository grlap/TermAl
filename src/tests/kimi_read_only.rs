//! Tests the read-only gate for Kimi delegation children (kimi_read_only.rs):
//! the decision for each permission request, the observations it rests on,
//! the fenced answer through `handle_acp_message`, the after-the-fact
//! `rawInput` check, the mode guard, and admission of Kimi reviewers.
//!
//! Owns these tests only. New module; the gate is new in `kimi_read_only.rs`.
//! Fixtures follow the real Kimi Code 2.0.2 ordering from the ACP captures:
//! streamed argument JSON, then the permission request, then the answer, then
//! the `rawInput` report, then the terminal status.

use super::*;

const WORKDIR: &str = "/work/repo";

fn text_content(text: &str) -> Value {
    json!([{ "type": "content", "content": { "type": "text", "text": text } }])
}

fn tool_call(id: &str, title: &str) -> Value {
    json!({ "sessionUpdate": "tool_call", "toolCallId": id, "title": title,
        "kind": "execute", "status": "pending", "content": text_content("") })
}

fn streamed(id: &str, args_text: &str) -> Value {
    json!({ "sessionUpdate": "tool_call_update", "toolCallId": id,
        "status": "in_progress", "content": text_content(args_text) })
}

fn raw_input(id: &str, raw: Value) -> Value {
    json!({ "sessionUpdate": "tool_call_update", "toolCallId": id, "status": "in_progress",
        "title": "Running: …", "kind": "execute", "rawInput": raw })
}

fn options() -> Value {
    json!([
        { "optionId": "approve_once", "name": "Approve once", "kind": "allow_once" },
        { "optionId": "approve_always", "name": "Approve for this session", "kind": "allow_always" },
        { "optionId": "reject", "name": "Reject", "kind": "reject_once" }
    ])
}

fn request_params(id: &str, title: &str, summary: &str) -> Value {
    json!({ "sessionId": "session_x", "options": options(),
        "toolCall": { "toolCallId": id, "title": title, "content": text_content(summary) } })
}

fn observed(title: &str, args_text: &str) -> KimiToolCallObservation {
    KimiToolCallObservation {
        title: Some(title.to_owned()),
        streamed_args: Some(args_text.to_owned()),
        oversized: false,
        approved_args: None,
    }
}

fn decide(params: &Value, observation: Option<&KimiToolCallObservation>) -> KimiReadOnlyDecision {
    kimi_read_only_permission_decision(params, observation, WORKDIR, &|_| true)
}

fn bash_summary(command: &str) -> String {
    // Kimi cuts its summary to about 50 characters and ends it with an ellipsis.
    if command.chars().count() > 50 {
        format!(
            "Requesting approval to Running: {}\u{2026}",
            command.chars().take(40).collect::<String>()
        )
    } else {
        format!("Requesting approval to Running: {command}")
    }
}

fn bash_decision(args: Value) -> KimiReadOnlyDecision {
    let command = args["command"].as_str().unwrap_or_default().to_owned();
    decide(
        &request_params("b", "Bash", &bash_summary(&command)),
        Some(&observed("Bash", &args.to_string())),
    )
}

fn is_reject(decision: &KimiReadOnlyDecision) -> bool {
    matches!(decision, KimiReadOnlyDecision::Reject { .. })
}

#[test]
fn read_only_bash_is_allowed_from_its_complete_streamed_arguments() {
    for command in [
        "git status --short",
        "git diff -- src/main.rs",
        "ls src | grep kimi",
        "sed -n '1,40p' src/kimi.rs",
        "git log --oneline -20 -- docs/features/kimi-cli-integration.md src/kimi_read_only.rs",
    ] {
        let args = json!({ "command": command });
        assert_eq!(
            bash_decision(args.clone()),
            KimiReadOnlyDecision::Allow { args },
            "{command}"
        );
    }
}

#[test]
fn bash_that_writes_or_escapes_the_checker_is_rejected() {
    for command in [
        "git commit -m x",
        "echo hi > file.txt",
        "rm -rf src",
        "git status; touch x",
        "cat $(echo x)",
        "sed -i 's/a/b/' src/main.rs",
    ] {
        assert!(is_reject(&bash_decision(json!({ "command": command }))), "{command}");
    }
}

#[test]
fn bash_options_the_gate_does_not_judge_are_rejected() {
    for args in [
        json!({ "command": "git status", "shell": "pwsh" }),
        json!({ "command": "git status", "run_in_background": true, "description": "x" }),
        json!({ "command": "git status", "disable_timeout": true }),
        json!({ "command": "git status", "cwd": "/somewhere/else" }),
    ] {
        assert!(is_reject(&bash_decision(args.clone())), "{args}");
    }
    for args in [
        json!({ "command": "git status", "run_in_background": false, "timeout": 30 }),
        json!({ "command": "git status", "cwd": "/work/repo/", "description": "status" }),
    ] {
        assert!(!is_reject(&bash_decision(args.clone())), "{args}");
    }
}

#[test]
fn a_request_without_complete_observed_arguments_is_rejected() {
    let params = request_params("b", "Bash", &bash_summary("git status"));
    // A subagent's request: nothing was streamed for it in this session.
    assert!(is_reject(&decide(&params, None)));
    // A prefix of the argument JSON is never taken as the arguments.
    assert!(is_reject(&decide(
        &params,
        Some(&observed("Bash", "{\"command\": \"git sta"))
    )));
    // The request must be for the tool call that was observed.
    assert!(is_reject(&decide(
        &params,
        Some(&observed("Write", "{\"command\": \"git status\"}"))
    )));
}

#[test]
fn repeated_argument_keys_are_rejected_never_resolved() {
    // serde_json keeps the last of two equal keys; Kimi might keep the first.
    let params = request_params("b", "Bash", &bash_summary("git status"));
    for text in [
        r#"{"command":"rm -rf .","command":"git status"}"#,
        r#"{"command":"git status","description":"a","description":"b"}"#,
        r#"{"command":"git status","extra":{"x":1,"x":2}}"#,
    ] {
        assert!(is_reject(&decide(&params, Some(&observed("Bash", text)))), "{text}");
    }
    assert_eq!(
        kimi_parse_arguments(r#"{"a":{"b":[1,{"c":true}]}}"#),
        Some(json!({ "a": { "b": [1, { "c": true }] } }))
    );
    assert_eq!(kimi_parse_arguments("[1,2]"), None, "only an object is arguments");
}

#[test]
fn the_after_the_fact_check_compares_values_not_text() {
    let mut observations = KimiReadOnlyObservations::default();
    observations.observe(&tool_call("c1", "Bash"));
    // The model's text: spaces and its own key order.
    let approved = kimi_parse_arguments(r#"{ "command" : "git status",  "timeout": 30 }"#)
        .expect("valid arguments");
    observations.record_approval("c1", approved);
    // Kimi's re-serialization: compact, another key order.
    assert_eq!(
        observations.observe(&raw_input("c1", json!({ "timeout": 30, "command": "git status" }))),
        None
    );
    assert!(observations
        .observe(&raw_input("c1", json!({ "timeout": 30, "command": "git stash" })))
        .is_some());
}

#[test]
fn cwd_is_compared_as_a_path_key() {
    let same = if cfg!(windows) {
        r"\WORK\Repo\"
    } else {
        "/work/repo/"
    };
    assert!(!is_reject(&bash_decision(json!({ "command": "git status", "cwd": same }))));
    assert!(is_reject(&bash_decision(json!({ "command": "git status", "cwd": "/work/repo/sub" }))));
    assert!(is_reject(&bash_decision(json!({ "command": "git status", "cwd": 7 }))));
}

#[test]
fn only_the_streamed_text_shape_counts_and_oversized_streams_are_rejected() {
    let mut observations = KimiReadOnlyObservations::default();
    observations.observe(&tool_call("c1", "Bash"));
    let odd_shapes = [
        json!([{ "type": "diff", "content": { "type": "text", "text": "{\"command\":\"x\"}" } }]),
        json!([{ "type": "content", "content": { "type": "image", "text": "{\"command\":\"x\"}" } }]),
        json!([
            { "type": "content", "content": { "type": "text", "text": "{\"command\":\"x\"}" } },
            { "type": "content", "content": { "type": "text", "text": "{}" } }
        ]),
    ];
    for content in odd_shapes {
        observations.observe(&json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c1",
            "status": "in_progress", "content": content }));
    }
    assert_eq!(observations.get("c1").unwrap().streamed_args, None);

    let huge = format!(
        "{{\"command\": \"git status {}\"}}",
        "x".repeat(KIMI_READ_ONLY_STREAMED_ARGS_MAX_BYTES)
    );
    observations.observe(&streamed("c1", &huge));
    // A later, small text does not undo the mark.
    observations.observe(&streamed("c1", "{\"command\": \"git status\"}"));
    let observation = observations.get("c1").unwrap();
    assert!(observation.oversized);
    assert!(is_reject(&decide(
        &request_params("c1", "Bash", &bash_summary("git status")),
        Some(observation)
    )));
}

#[test]
fn a_summary_that_does_not_show_the_command_is_rejected() {
    let observation = observed("Bash", &json!({ "command": "git status" }).to_string());
    for summary in [
        "Requesting approval to Running: rm -rf src",
        "Requesting approval to Running: git stash\u{2026}",
        "Requesting approval to Running: \u{2026}",
        "git status",
    ] {
        assert!(
            is_reject(&decide(&request_params("b", "Bash", summary), Some(&observation))),
            "{summary}"
        );
    }
}

#[test]
fn writes_and_other_tools_that_ask_are_rejected() {
    for title in ["Write", "Edit", "CronCreate", "Agent", "mcp__other__tool"] {
        let observation = observed(title, "{\"path\":\"x\",\"content\":\"y\"}");
        assert!(
            is_reject(&decide(
                &request_params("t", title, &format!("Requesting approval to Writing x")),
                Some(&observation)
            )),
            "{title}"
        );
    }
}

fn submit_args() -> Value {
    json!({ "schemaVersion": DELEGATION_REVIEW_RESULT_SCHEMA_VERSION, "status": "completed",
        "summary": "ok", "findings": [], "commandsRun": [], "filesInspected": [],
        "notes": [], "suggestedTrackerUpdates": [] })
}

fn mcp_params(title: &str) -> Value {
    request_params("m", title, &format!("Requesting approval to Approve {title}"))
}

#[test]
fn only_the_exact_termal_result_tool_with_a_valid_request_is_allowed() {
    let args = submit_args();
    let observation = observed(TERMAL_SUBMIT_REVIEW_RESULT_QUALIFIED_TOOL_NAME, &args.to_string());
    assert_eq!(
        decide(
            &mcp_params(TERMAL_SUBMIT_REVIEW_RESULT_QUALIFIED_TOOL_NAME),
            Some(&observation)
        ),
        KimiReadOnlyDecision::Allow { args: args.clone() }
    );
    // Without the delegation's authority it is refused.
    assert!(is_reject(&kimi_read_only_permission_decision(
        &mcp_params(TERMAL_SUBMIT_REVIEW_RESULT_QUALIFIED_TOOL_NAME),
        Some(&observation),
        WORKDIR,
        &|_| false,
    )));
    // A wrong schema version is not a valid request.
    let mut wrong = submit_args();
    wrong["schemaVersion"] = json!(99);
    assert!(is_reject(&decide(
        &mcp_params(TERMAL_SUBMIT_REVIEW_RESULT_QUALIFIED_TOOL_NAME),
        Some(&observed(TERMAL_SUBMIT_REVIEW_RESULT_QUALIFIED_TOOL_NAME, &wrong.to_string()))
    )));
    // Foreign aliases, the bare name and the evaluator tool are refused.
    for title in [
        "mcp__termal-delegation-x__termal_submit_review_result",
        "mcp__termal_delegation__termal_submit_review_result",
        TERMAL_SUBMIT_REVIEW_RESULT_TOOL_NAME,
        TERMAL_SUBMIT_ACCEPTANCE_EVALUATION_QUALIFIED_TOOL_NAME,
    ] {
        assert!(
            is_reject(&decide(&mcp_params(title), Some(&observed(title, &args.to_string())))),
            "{title}"
        );
    }
}

#[test]
fn exit_plan_mode_rejects_the_plan_and_leaves_plan_mode() {
    let options = json!([
        { "optionId": "plan_approve", "name": "Approve", "kind": "allow_once" },
        { "optionId": "plan_revise", "name": "Revise", "kind": "reject_once" },
        { "optionId": "plan_reject_and_exit", "name": "Reject and Exit", "kind": "reject_once" }
    ]);
    let mut params = request_params("p", "ExitPlanMode", "Plan saved to: x");
    params["options"] = options.clone();
    let decision = decide(&params, None);
    assert_eq!(decision, KimiReadOnlyDecision::PlanRejectAndExit);
    assert_eq!(
        kimi_read_only_option_id(options.as_array().unwrap(), &decision).as_deref(),
        Some("plan_reject_and_exit")
    );
    // Without that option the plan is refused, and plan mode stays.
    assert!(is_reject(&decide(&request_params("p", "ExitPlanMode", "x"), None)));
}

#[test]
fn an_approval_selects_allow_once_and_never_allow_always() {
    let decision = KimiReadOnlyDecision::Allow { args: json!({}) };
    assert_eq!(
        kimi_read_only_option_id(options().as_array().unwrap(), &decision).as_deref(),
        Some("approve_once")
    );
    let only_always = json!([{ "optionId": "always", "kind": "allow_always" }]);
    assert_eq!(kimi_read_only_option_id(only_always.as_array().unwrap(), &decision), None);
}

#[test]
fn observations_follow_kimis_real_ordering_and_catch_a_changed_raw_input() {
    let mut observations = KimiReadOnlyObservations::default();
    assert_eq!(observations.observe(&tool_call("c1", "Bash")), None);
    for chunk in ["{", "{\"command\": \"git", "{\"command\": \"git status\"}"] {
        assert_eq!(observations.observe(&streamed("c1", chunk)), None);
    }
    let observation = observations.get("c1").expect("observed");
    assert_eq!(observation.title.as_deref(), Some("Bash"));
    assert_eq!(
        observation.streamed_args.as_deref(),
        Some("{\"command\": \"git status\"}")
    );

    observations.record_approval("c1", json!({ "command": "git status" }));
    assert_eq!(
        observations.observe(&raw_input("c1", json!({ "command": "git status" }))),
        None,
        "the reported input is the approved one"
    );
    assert_eq!(
        observations.observe(&raw_input("c1", json!({ "command": "git status; rm -rf ." }))),
        Some(KimiApprovedInputMismatch { title: "Bash".to_owned() })
    );
    let done = json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed" });
    observations.observe(&done);
    assert!(observations.get("c1").is_none(), "a finished call is forgotten");
}

#[test]
fn observations_stay_bounded() {
    let mut observations = KimiReadOnlyObservations::default();
    for index in 0..(KIMI_READ_ONLY_OBSERVED_CALLS_MAX + 10) {
        observations.observe(&tool_call(&format!("c{index}"), "Bash"));
    }
    assert_eq!(observations.calls.len(), KIMI_READ_ONLY_OBSERVED_CALLS_MAX);
    assert!(observations.get("c0").is_none());
}

#[test]
fn only_default_and_plan_keep_the_gate() {
    let current = json!({ "sessionUpdate": "current_mode_update", "currentModeId": "yolo" });
    assert_eq!(kimi_reported_mode(&current).as_deref(), Some("yolo"));
    let config = json!({ "sessionUpdate": "config_option_update", "configOptions": [
        { "id": "mode", "currentValue": "auto", "options": [] }
    ] });
    assert_eq!(kimi_reported_mode(&config).as_deref(), Some("auto"));
    assert!(kimi_mode_keeps_permission_gate("default"));
    assert!(kimi_mode_keeps_permission_gate("plan"));
    assert!(!kimi_mode_keeps_permission_gate("auto"));
    assert!(!kimi_mode_keeps_permission_gate("yolo"));
}

fn kimi_reviewer_record(
    parent_session_id: String,
    child_session_id: String,
    agent: Agent,
) -> DelegationRecord {
    DelegationRecord {
        id: "delegation-kimi-read-only".to_owned(),
        parent_session_id,
        child_session_id,
        mode: DelegationMode::Reviewer,
        status: DelegationStatus::Running,
        title: "Kimi /review-code".to_owned(),
        prompt: "/review-code".to_owned(),
        cwd: WORKDIR.to_owned(),
        agent,
        model: Some("kimi-code/k3".to_owned()),
        write_policy: DelegationWritePolicy::ReadOnly,
        created_at: stamp_now(),
        started_at: Some(stamp_now()),
        completed_at: None,
        result: None,
        submitted_review_result: None,
        post_submission_transport_error: None,
        review_result_recovery_probe_attempt: None,
        review_result_recovery_error: None,
        review_result_schema_version: None,
        queued_followup_prompt_id: None,
        review_result_submission_attempt: 1,
        acceptance_evaluation: None,
    }
}

#[test]
fn a_read_only_kimi_child_is_told_what_the_gate_allows() {
    let kimi = build_delegation_prompt(&kimi_reviewer_record(
        "session-parent".to_owned(),
        "session-child".to_owned(),
        Agent::Kimi,
    ));
    assert!(kimi.contains("TERMAL_STRUCTURED_REVIEW_RESULT_V1"));
    assert!(kimi.contains("TermAl read-only gate for Kimi"));
    assert!(kimi.contains("Do not use Agent or AgentSwarm"));
    let codex = build_delegation_prompt(&kimi_reviewer_record(
        "session-parent".to_owned(),
        "session-child".to_owned(),
        Agent::Codex,
    ));
    assert!(!codex.contains("TermAl read-only gate for Kimi"));
}

/// One Kimi session driven through `handle_acp_message`, with the runtime
/// state the reader and writer share and the reader's turn state.
struct KimiHarness {
    state: AppState,
    id: String,
    runtime: AcpRuntimeHandle,
    rx: mpsc::Receiver<AcpRuntimeCommand>,
    runtime_state: Arc<Mutex<AcpRuntimeState>>,
    turn_state: AcpTurnState,
}

impl KimiHarness {
    /// A Kimi child of a running read-only reviewer delegation.
    fn read_only_child() -> Self {
        let state = test_app_state();
        let (id, runtime, rx) = kimi_read_only_child(&state);
        Self::new(state, id, runtime, rx)
    }

    fn new(
        state: AppState,
        id: String,
        runtime: AcpRuntimeHandle,
        rx: mpsc::Receiver<AcpRuntimeCommand>,
    ) -> Self {
        Self {
            state,
            id,
            runtime,
            rx,
            runtime_state: Arc::new(Mutex::new(AcpRuntimeState::default())),
            turn_state: AcpTurnState::default(),
        }
    }

    fn send(&mut self, message: Value) {
        handle_acp_message(
            &message,
            &self.state,
            &self.id,
            &RuntimeToken::Acp("kimi-read-only-runtime".to_owned()),
            &Arc::new(Mutex::new(HashMap::new())),
            &self.runtime_state,
            &self.runtime.input_tx,
            &mut self.turn_state,
            &mut SessionRecorder::new(self.state.clone(), self.id.clone()),
            AcpAgent::Kimi,
        )
        .expect("the message should be handled");
    }

    /// Streams a call's complete arguments, then its permission request.
    fn request(&mut self, id: &str, title: &str, args: &str, summary: &str) {
        self.send(update(tool_call(id, title)));
        self.send(update(streamed(id, args)));
        self.send(permission_message(request_params(id, title, summary)));
    }

    fn status(&self) -> SessionStatus {
        let inner = self.state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&self.id).unwrap()]
            .session
            .status
    }

    fn cancelled(&self) -> bool {
        self.rx
            .try_iter()
            .any(|command| matches!(command, AcpRuntimeCommand::Cancel))
    }
}

/// A Kimi child of a running read-only reviewer delegation, with a live ACP
/// runtime whose input the test reads.
fn kimi_read_only_child(
    state: &AppState,
) -> (String, AcpRuntimeHandle, mpsc::Receiver<AcpRuntimeCommand>) {
    let parent_session_id = test_session_id(state, Agent::Claude);
    let (runtime, rx) = test_acp_runtime_handle(AcpAgent::Kimi, "kimi-read-only-runtime");
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let child = inner.create_session(
        Agent::Kimi,
        Some("Kimi reviewer".to_owned()),
        WORKDIR.to_owned(),
        None,
        Some("kimi-code/k3".to_owned()),
    );
    let child_session_id = child.session.id.clone();
    let child_index = inner
        .find_session_index(&child_session_id)
        .expect("child should be indexed");
    inner.sessions[child_index].session.parent_delegation_id =
        Some("delegation-kimi-read-only".to_owned());
    inner.sessions[child_index].session.status = SessionStatus::Active;
    inner.sessions[child_index].runtime = SessionRuntime::Acp(runtime.clone());
    inner
        .delegations
        .push(kimi_reviewer_record(parent_session_id, child_session_id.clone(), Agent::Kimi));
    let delegation_index = inner.delegations.len() - 1;
    inner.mark_delegation_mutated(delegation_index);
    inner.sync_running_read_only_delegation_index(delegation_index);
    state.commit_locked(&mut inner).unwrap();
    drop(inner);
    (child_session_id, runtime, rx)
}

fn update(update: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": "session/update",
        "params": { "sessionId": "session_x", "update": update } })
}

fn permission_message(params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": 7, "method": "session/request_permission", "params": params })
}

fn next_permission_outcome(rx: &mpsc::Receiver<AcpRuntimeCommand>) -> Value {
    loop {
        match rx.try_recv().expect("a permission answer should have been sent") {
            AcpRuntimeCommand::JsonRpcMessage(message) => return message["result"]["outcome"].clone(),
            _ => continue,
        }
    }
}

#[test]
fn a_read_only_kimi_child_is_answered_by_the_host_in_the_real_message_order() {
    let mut kimi = KimiHarness::read_only_child();

    // Read-only bash: streamed, requested, approved once.
    kimi.request("c1", "Bash", "{\"command\": \"git status\"}",
        "Requesting approval to Running: git status");
    assert_eq!(
        next_permission_outcome(&kimi.rx),
        json!({ "outcome": "selected", "optionId": "approve_once" })
    );
    // Kimi's report after the answer matches what was approved.
    kimi.send(update(raw_input("c1", json!({ "command": "git status" }))));
    assert!(kimi.rx.try_recv().is_err(), "a matching report changes nothing");

    // A write: rejected once, never shown as a manual card.
    kimi.request("c2", "Write", "{\"path\": \"x.txt\", \"content\": \"x\"}",
        "Requesting approval to Writing x.txt");
    assert_eq!(
        next_permission_outcome(&kimi.rx),
        json!({ "outcome": "selected", "optionId": "reject" })
    );
    let inner = kimi.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&kimi.id).unwrap()];
    assert!(
        record.pending_acp_approvals.is_empty(),
        "no manual approval card for a read-only child"
    );
}

#[test]
fn a_request_for_a_call_that_was_not_observed_is_rejected() {
    let mut kimi = KimiHarness::read_only_child();
    kimi.send(update(tool_call("c1", "Bash")));
    kimi.send(update(streamed("c1", "{\"command\": \"git status\"}")));
    // The request names another call, for which nothing was streamed.
    kimi.send(permission_message(request_params(
        "c2",
        "Bash",
        "Requesting approval to Running: git status",
    )));
    assert_eq!(
        next_permission_outcome(&kimi.rx),
        json!({ "outcome": "selected", "optionId": "reject" })
    );
}

#[test]
fn a_stale_or_stopping_runtime_is_answered_cancelled() {
    let mut kimi = KimiHarness::read_only_child();
    {
        let mut inner = kimi.state.inner.lock().unwrap();
        let index = inner.find_session_index(&kimi.id).unwrap();
        inner.sessions[index].runtime_stop_in_progress = true;
    }
    kimi.request("c1", "Bash", "{\"command\": \"git status\"}",
        "Requesting approval to Running: git status");
    assert_eq!(next_permission_outcome(&kimi.rx), json!({ "outcome": "cancelled" }));
}

#[test]
fn a_changed_raw_input_after_an_approval_stops_the_turn() {
    let mut kimi = KimiHarness::read_only_child();
    kimi.request("c1", "Bash", "{\"command\": \"git status\"}",
        "Requesting approval to Running: git status");
    let _ = next_permission_outcome(&kimi.rx);

    kimi.send(update(raw_input("c1", json!({ "command": "git status; touch x" }))));

    assert!(kimi.cancelled(), "the prompt is cancelled");
    assert_eq!(kimi.status(), SessionStatus::Error);
}

#[test]
fn a_mode_that_stops_asking_stops_a_read_only_turn_only_inside_the_prompt() {
    let mut kimi = KimiHarness::read_only_child();

    // During session setup, before TermAl's own Default-mode ACK, a mode Kimi
    // kept from an earlier session is about to be replaced: no violation.
    kimi.send(update(json!({ "sessionUpdate": "current_mode_update", "currentModeId": "yolo" })));
    assert!(!kimi.cancelled(), "setup is not a violation");
    assert_eq!(kimi.status(), SessionStatus::Active);

    // After the ACK, for the rest of the prompt, the gate is armed.
    set_kimi_mode_gate_armed(&kimi.runtime_state, true);
    kimi.send(update(json!({ "sessionUpdate": "current_mode_update", "currentModeId": "plan" })));
    assert!(!kimi.cancelled(), "plan mode keeps the gate");
    kimi.send(update(json!({ "sessionUpdate": "current_mode_update", "currentModeId": "yolo" })));
    assert!(kimi.cancelled());
    assert_eq!(kimi.status(), SessionStatus::Error);
}

#[test]
fn a_kimi_session_that_is_not_read_only_keeps_manual_approvals() {
    let state = test_app_state();
    let id = test_session_id(&state, Agent::Kimi);
    let (runtime, rx) = test_acp_runtime_handle(AcpAgent::Kimi, "kimi-read-only-runtime");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Acp(runtime.clone());
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    let mut kimi = KimiHarness::new(state, id, runtime, rx);
    kimi.request("c1", "Bash", "{\"command\": \"git status\"}",
        "Requesting approval to Running: git status");
    assert!(kimi.rx.try_recv().is_err(), "the host answers nothing");
    let inner = kimi.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&kimi.id).unwrap()];
    assert_eq!(record.pending_acp_approvals.len(), 1, "a manual card is queued");
}

#[test]
fn the_freeze_tool_is_allowed_with_a_valid_request_and_a_mismatched_summary_is_not() {
    let title = TERMAL_REVIEW_FREEZE_QUALIFIED_TOOL_NAME;
    let args = json!({ "manifestPath": "review/manifest.json",
        "expectedFingerprint": "a".repeat(64) });
    let observation = observed(title, &args.to_string());
    assert_eq!(
        decide(&mcp_params(title), Some(&observation)),
        KimiReadOnlyDecision::Allow { args: args.clone() }
    );
    let other_summary = request_params(
        "m",
        title,
        &format!("Requesting approval to Approve {TERMAL_SUBMIT_REVIEW_RESULT_QUALIFIED_TOOL_NAME}"),
    );
    assert!(is_reject(&decide(&other_summary, Some(&observation))));
    let bad = json!({ "manifestPath": "", "expectedFingerprint": "x" });
    assert!(is_reject(&decide(
        &mcp_params(title),
        Some(&observed(title, &bad.to_string()))
    )));
}

#[test]
fn a_terminal_update_that_carries_raw_input_still_forgets_the_call() {
    let mut observations = KimiReadOnlyObservations::default();
    observations.observe(&tool_call("c1", "Bash"));
    observations.record_approval("c1", json!({ "command": "git status" }));
    let finished = json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c1",
        "status": "completed", "rawInput": { "command": "git status" } });
    assert_eq!(observations.observe(&finished), None);
    assert!(observations.get("c1").is_none());
}

#[test]
fn eviction_spares_an_approved_call_whose_report_is_pending() {
    let mut observations = KimiReadOnlyObservations::default();
    observations.observe(&tool_call("approved", "Bash"));
    observations.record_approval("approved", json!({ "command": "git status" }));
    for index in 0..(KIMI_READ_ONLY_OBSERVED_CALLS_MAX + 5) {
        observations.observe(&tool_call(&format!("read{index}"), "Read"));
    }
    assert!(observations.get("approved").is_some());
    assert!(observations
        .observe(&raw_input("approved", json!({ "command": "rm -rf ." })))
        .is_some());
}

#[test]
fn only_windows_folds_separators_and_case_in_the_cwd_key() {
    if cfg!(windows) {
        assert_eq!(kimi_path_key(r"C:\Work\Repo\"), kimi_path_key("c:/work/repo"));
    } else {
        assert_ne!(kimi_path_key(r"/work\repo"), kimi_path_key("/work/repo"));
        assert_ne!(kimi_path_key("/Work/Repo"), kimi_path_key("/work/repo"));
        assert_eq!(kimi_path_key("/work/repo/"), kimi_path_key("/work/repo"));
    }
}

#[test]
fn the_creation_path_admits_a_kimi_reviewer_and_a_read_only_explorer() {
    // The production create path, stopped just before the child's runtime
    // starts, so no Kimi process is launched.
    for mode in [DelegationMode::Reviewer, DelegationMode::Explorer] {
        let state = test_app_state();
        let parent_session_id = test_session_id(&state, Agent::Claude);
        let response = state
            .create_read_only_delegation(
                &parent_session_id,
                CreateDelegationRequest {
                    prompt: format!("Review. {TEST_CANCEL_DELEGATION_BEFORE_START_PROMPT}"),
                    title: Some("Kimi read-only".to_owned()),
                    cwd: None,
                    agent: Some(Agent::Kimi),
                    model: None,
                    mode: Some(mode),
                    write_policy: Some(DelegationWritePolicy::ReadOnly),
                },
            )
            .unwrap_or_else(|err| panic!("{mode:?} should be admitted: {}", err.message));
        assert_eq!(response.delegation.agent, Agent::Kimi);
        assert_eq!(response.delegation.write_policy, DelegationWritePolicy::ReadOnly);
        let inner = state.inner.lock().unwrap();
        let child = inner
            .sessions
            .iter()
            .find(|record| record.session.id == response.delegation.child_session_id)
            .expect("the child session exists");
        assert_eq!(child.session.agent, Agent::Kimi);
        assert_eq!(
            child.session.parent_delegation_id.as_deref(),
            Some(response.delegation.id.as_str())
        );
        drop(inner);
        let _ = fs::remove_file(state.persistence_path.as_path());
    }
}

#[test]
fn kimi_may_review_and_evaluators_stay_claude_or_codex() {
    assert!(Agent::Kimi.supports_structured_review_results());
    assert!(!Agent::Kimi.supports_acceptance_evaluations());
    assert!(Agent::Codex.supports_acceptance_evaluations());
    assert!(Agent::Claude.supports_acceptance_evaluations());
    for agent in [Agent::Cursor, Agent::Gemini, Agent::OpenCode] {
        assert!(!agent.supports_structured_review_results());
    }
}
