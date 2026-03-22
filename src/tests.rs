use super::*;
use axum::body::{Body, to_bytes};
use axum::http::Request;
use std::io::Read as _;
use tower::util::ServiceExt;

#[derive(Default)]
struct TestRecorder {
    approvals: Vec<(String, String, String)>,
    codex_approvals: Vec<(String, String, String, CodexPendingApproval)>,
    codex_user_input_requests: Vec<(
        String,
        String,
        Vec<UserInputQuestion>,
        CodexPendingUserInput,
    )>,
    codex_mcp_elicitation_requests: Vec<(
        String,
        String,
        McpElicitationRequestPayload,
        CodexPendingMcpElicitation,
    )>,
    codex_app_requests: Vec<(String, String, String, Value, CodexPendingAppRequest)>,
    commands: Vec<(String, String, CommandStatus)>,
    diffs: Vec<(String, String, String, ChangeType)>,
    parallel_agents: Vec<Vec<ParallelAgentProgress>>,
    subagent_results: Vec<(String, String)>,
    thinking: Vec<(String, Vec<String>)>,
    texts: Vec<String>,
    text_deltas: Vec<String>,
}

impl TurnRecorder for TestRecorder {
    fn note_external_session(&mut self, _session_id: &str) -> Result<()> {
        Ok(())
    }

    fn push_text(&mut self, text: &str) -> Result<()> {
        self.texts.push(text.to_owned());
        Ok(())
    }

    fn push_subagent_result(
        &mut self,
        title: &str,
        summary: &str,
        _conversation_id: Option<&str>,
        _turn_id: Option<&str>,
    ) -> Result<()> {
        self.subagent_results
            .push((title.to_owned(), summary.to_owned()));
        self.texts.push(format!("{title}\n{summary}"));
        Ok(())
    }

    fn push_approval(&mut self, title: &str, command: &str, detail: &str) -> Result<()> {
        self.approvals
            .push((title.to_owned(), command.to_owned(), detail.to_owned()));
        Ok(())
    }

    fn push_thinking(&mut self, title: &str, lines: Vec<String>) -> Result<()> {
        self.thinking.push((title.to_owned(), lines));
        Ok(())
    }

    fn push_diff(
        &mut self,
        file_path: &str,
        summary: &str,
        diff: &str,
        change_type: ChangeType,
    ) -> Result<()> {
        self.diffs.push((
            file_path.to_owned(),
            summary.to_owned(),
            diff.to_owned(),
            change_type,
        ));
        Ok(())
    }

    fn text_delta(&mut self, delta: &str) -> Result<()> {
        self.text_deltas.push(delta.to_owned());
        Ok(())
    }

    fn finish_streaming_text(&mut self) -> Result<()> {
        Ok(())
    }

    fn command_started(&mut self, _key: &str, command: &str) -> Result<()> {
        self.commands
            .push((command.to_owned(), String::new(), CommandStatus::Running));
        Ok(())
    }

    fn command_completed(
        &mut self,
        _key: &str,
        command: &str,
        output: &str,
        status: CommandStatus,
    ) -> Result<()> {
        self.commands
            .push((command.to_owned(), output.to_owned(), status));
        Ok(())
    }

    fn upsert_parallel_agents(
        &mut self,
        _key: &str,
        agents: &[ParallelAgentProgress],
    ) -> Result<()> {
        self.parallel_agents.push(agents.to_vec());
        Ok(())
    }

    fn error(&mut self, _detail: &str) -> Result<()> {
        Ok(())
    }
}

impl CodexTurnRecorder for TestRecorder {
    fn push_codex_approval(
        &mut self,
        title: &str,
        command: &str,
        detail: &str,
        approval: CodexPendingApproval,
    ) -> Result<()> {
        self.approvals
            .push((title.to_owned(), command.to_owned(), detail.to_owned()));
        self.codex_approvals.push((
            title.to_owned(),
            command.to_owned(),
            detail.to_owned(),
            approval,
        ));
        Ok(())
    }

    fn push_codex_user_input_request(
        &mut self,
        title: &str,
        detail: &str,
        questions: Vec<UserInputQuestion>,
        request: CodexPendingUserInput,
    ) -> Result<()> {
        self.codex_user_input_requests.push((
            title.to_owned(),
            detail.to_owned(),
            questions,
            request,
        ));
        Ok(())
    }

    fn push_codex_mcp_elicitation_request(
        &mut self,
        title: &str,
        detail: &str,
        request: McpElicitationRequestPayload,
        pending: CodexPendingMcpElicitation,
    ) -> Result<()> {
        self.codex_mcp_elicitation_requests.push((
            title.to_owned(),
            detail.to_owned(),
            request,
            pending,
        ));
        Ok(())
    }

    fn push_codex_app_request(
        &mut self,
        title: &str,
        detail: &str,
        method: &str,
        params: Value,
        pending: CodexPendingAppRequest,
    ) -> Result<()> {
        self.codex_app_requests.push((
            title.to_owned(),
            detail.to_owned(),
            method.to_owned(),
            params,
            pending,
        ));
        Ok(())
    }
}

fn test_app_state() -> AppState {
    let persistence_path =
        std::env::temp_dir().join(format!("termal-test-{}.json", Uuid::new_v4()));

    AppState {
        default_workdir: "/tmp".to_owned(),
        persistence_path: Arc::new(persistence_path),
        state_events: broadcast::channel(16).0,
        delta_events: broadcast::channel(16).0,
        shared_codex_runtime: Arc::new(Mutex::new(None)),
        remote_registry: test_remote_registry(),
        inner: Arc::new(Mutex::new(StateInner::new())),
    }
}

fn test_remote_registry() -> Arc<RemoteRegistry> {
    Arc::new(
        std::thread::spawn(RemoteRegistry::new)
            .join()
            .expect("remote registry init thread panicked")
            .expect("remote registry should initialize"),
    )
}

fn write_test_codex_threads_db(
    codex_home: &FsPath,
    rows: &[(
        &str,
        &str,
        &str,
        &str,
        &str,
        i64,
        Option<&str>,
        Option<&str>,
        i64,
    )],
) {
    fs::create_dir_all(codex_home).expect("test Codex home should be created");
    let connection =
        rusqlite::Connection::open(codex_home.join("state_5.sqlite")).expect("db should open");
    connection
        .execute_batch(
            "create table threads (
                id text primary key,
                cwd text not null,
                title text not null,
                sandbox_policy text not null,
                approval_mode text not null,
                archived integer not null,
                model text,
                reasoning_effort text,
                updated_at integer not null
            );",
        )
        .expect("threads table should be created");

    for (
        id,
        cwd,
        title,
        sandbox_policy,
        approval_mode,
        archived,
        model,
        reasoning_effort,
        updated_at,
    ) in rows
    {
        connection
            .execute(
                "insert into threads (
                    id, cwd, title, sandbox_policy, approval_mode, archived, model, reasoning_effort, updated_at
                ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    id,
                    cwd,
                    title,
                    sandbox_policy,
                    approval_mode,
                    archived,
                    model,
                    reasoning_effort,
                    updated_at
                ],
            )
            .expect("thread row should insert");
    }
}

fn test_session_id(state: &AppState, agent: Agent) -> String {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner.create_session(
        agent,
        Some("Test".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let session_id = record.session.id.clone();
    state.commit_locked(&mut inner).unwrap();
    session_id
}

fn create_test_project(state: &AppState, root_path: &FsPath, name: &str) -> String {
    state
        .create_project(CreateProjectRequest {
            name: Some(name.to_owned()),
            root_path: root_path.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap()
        .project_id
}

fn create_test_project_session(
    state: &AppState,
    agent: Agent,
    project_id: &str,
    workdir: &FsPath,
) -> String {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner.create_session(
        agent,
        Some("Test".to_owned()),
        workdir.to_string_lossy().into_owned(),
        Some(project_id.to_owned()),
        None,
    );
    let session_id = record.session.id.clone();
    state.commit_locked(&mut inner).unwrap();
    session_id
}

fn run_git_test_command(repo_root: &FsPath, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("failed to run git {:?}: {err}", args));

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        panic!(
            "git {:?} failed with status {}.\nstdout: {}\nstderr: {}",
            args, output.status, stdout, stderr
        );
    }
}

fn test_exit_success_child() -> Child {
    if cfg!(windows) {
        Command::new("cmd").args(["/C", "exit 0"]).spawn().unwrap()
    } else {
        Command::new("sh").arg("-c").arg("exit 0").spawn().unwrap()
    }
}

fn test_sleep_child() -> Child {
    if cfg!(windows) {
        Command::new("cmd")
            .args(["/C", "ping -n 6 127.0.0.1 >NUL"])
            .spawn()
            .unwrap()
    } else {
        Command::new("sh").arg("-c").arg("sleep 5").spawn().unwrap()
    }
}

fn test_codex_runtime_handle(
    runtime_id: &str,
) -> (CodexRuntimeHandle, mpsc::Receiver<CodexRuntimeCommand>) {
    let child = test_exit_success_child();
    let (input_tx, input_rx) = mpsc::channel();

    (
        CodexRuntimeHandle {
            runtime_id: runtime_id.to_owned(),
            input_tx,
            process: Arc::new(SharedChild::new(child).unwrap()),
            shared_session: None,
        },
        input_rx,
    )
}

fn test_claude_runtime_handle(
    runtime_id: &str,
) -> (ClaudeRuntimeHandle, mpsc::Receiver<ClaudeRuntimeCommand>) {
    let child = test_exit_success_child();
    let (input_tx, input_rx) = mpsc::channel();

    (
        ClaudeRuntimeHandle {
            runtime_id: runtime_id.to_owned(),
            input_tx,
            process: Arc::new(SharedChild::new(child).unwrap()),
        },
        input_rx,
    )
}

fn test_acp_runtime_handle(
    agent: AcpAgent,
    runtime_id: &str,
) -> (AcpRuntimeHandle, mpsc::Receiver<AcpRuntimeCommand>) {
    let child = test_exit_success_child();
    let (input_tx, input_rx) = mpsc::channel();

    (
        AcpRuntimeHandle {
            agent,
            runtime_id: runtime_id.to_owned(),
            input_tx,
            process: Arc::new(SharedChild::new(child).unwrap()),
        },
        input_rx,
    )
}

#[derive(Clone, Default)]
struct SharedBufferWriter {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl SharedBufferWriter {
    fn contents(&self) -> String {
        String::from_utf8(
            self.buffer
                .lock()
                .expect("shared writer mutex poisoned")
                .clone(),
        )
        .expect("shared writer buffer should stay UTF-8")
    }
}

impl std::io::Write for SharedBufferWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buffer
            .lock()
            .expect("shared writer mutex poisoned")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn cursor_permission_request(request_id: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "method": "session/request_permission",
        "params": {
            "toolName": "edit_file",
            "description": "Edit src/main.rs",
            "options": [
                { "optionId": "allow-once" },
                { "optionId": "allow-always" },
                { "optionId": "reject-once" }
            ]
        }
    })
}

#[test]
fn claude_task_tool_use_updates_parallel_agent_progress() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "task-1",
                        "name": "Task",
                        "input": {
                            "description": "Rust code review",
                            "subagent_type": "general-purpose"
                        }
                    },
                    {
                        "type": "tool_use",
                        "id": "task-2",
                        "name": "Task",
                        "input": {
                            "description": "Architecture code review",
                            "subagent_type": "general-purpose"
                        }
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let latest = recorder
        .parallel_agents
        .last()
        .expect("parallel agents update should be recorded");
    assert_eq!(latest.len(), 2);
    assert_eq!(latest[0].title, "Rust code review");
    assert_eq!(latest[0].detail.as_deref(), Some("Initializing..."));
    assert_eq!(latest[0].status, ParallelAgentStatus::Initializing);
    assert_eq!(latest[1].title, "Architecture code review");
    assert_eq!(latest[1].status, ParallelAgentStatus::Initializing);
}

#[test]
fn claude_task_tool_result_updates_parallel_agents_and_records_subagent_result() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "task-1",
                        "name": "Task",
                        "input": {
                            "description": "Rust code review",
                            "subagent_type": "general-purpose"
                        }
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let detail = "Reviewer found a batching bug in location smoothing.\nRead src/state.rs for the stale preview path.";
    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "task-1",
                        "content": detail
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let latest = recorder
        .parallel_agents
        .last()
        .expect("completed parallel agent update should be recorded");
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].title, "Rust code review");
    assert_eq!(latest[0].status, ParallelAgentStatus::Completed);
    assert_eq!(
        latest[0].detail.as_deref(),
        Some("Reviewer found a batching bug in location smoothing.")
    );
    assert_eq!(
        recorder.subagent_results,
        vec![("Rust code review".to_owned(), detail.to_owned())]
    );
}

#[test]
fn claude_task_tool_error_records_full_failure_detail() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "task-1",
                        "name": "Task",
                        "input": {
                            "description": "Rust code review",
                            "subagent_type": "general-purpose"
                        }
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let detail = "Reviewer failed to parse the diff.\nStack trace line 1\nStack trace line 2";
    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "task-1",
                        "is_error": true,
                        "content": detail
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let latest = recorder
        .parallel_agents
        .last()
        .expect("errored parallel agent update should be recorded");
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].title, "Rust code review");
    assert_eq!(latest[0].status, ParallelAgentStatus::Error);
    assert_eq!(
        latest[0].detail.as_deref(),
        Some("Reviewer failed to parse the diff.")
    );
    assert_eq!(
        recorder.subagent_results,
        vec![("Rust code review".to_owned(), detail.to_owned())]
    );
}

#[test]
fn claude_task_tool_error_without_detail_records_fallback_failure_message() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "task-1",
                        "name": "Task",
                        "input": {
                            "description": "Rust code review",
                            "subagent_type": "general-purpose"
                        }
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "task-1",
                        "is_error": true,
                        "content": ""
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let latest = recorder
        .parallel_agents
        .last()
        .expect("errored parallel agent update should be recorded");
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].status, ParallelAgentStatus::Error);
    assert_eq!(latest[0].detail.as_deref(), Some("Task failed."));
    assert_eq!(
        recorder.subagent_results,
        vec![("Rust code review".to_owned(), "Task failed.".to_owned())]
    );
}

#[test]
fn claude_streamed_text_appends_missing_final_suffix_after_message_stop() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn_state = ClaudeTurnState::default();
    let mut external_session_id = None;

    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "Hello"
                }
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "message_stop"
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "text",
                        "text": "Hello there."
                    }
                ]
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "result",
            "is_error": false
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Claude session should exist");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Hello there."
    ));
}

#[test]
fn claude_streamed_text_skips_duplicate_final_text_after_message_stop() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn_state = ClaudeTurnState::default();
    let mut external_session_id = None;

    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "Hello there."
                }
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "message_stop"
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "text",
                        "text": "Hello there."
                    }
                ]
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "result",
            "is_error": false
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Claude session should exist");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Hello there."
    ));
}

#[test]
fn claude_tool_use_after_streamed_text_starts_followup_in_new_message() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn_state = ClaudeTurnState::default();
    let mut external_session_id = None;

    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "Hello"
                }
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "message_stop"
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "bash-1",
                        "name": "Bash",
                        "input": {
                            "command": "pwd"
                        }
                    }
                ]
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "World"
                }
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "result",
            "is_error": false
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Claude session should exist");

    assert_eq!(session.messages.len(), 3);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Hello"
    ));
    assert!(matches!(
        session.messages.get(1),
        Some(Message::Command {
            command,
            output,
            status,
            ..
        }) if command == "pwd" && output.is_empty() && *status == CommandStatus::Running
    ));
    assert!(matches!(
        session.messages.get(2),
        Some(Message::Text { text, .. }) if text == "World"
    ));
}

#[test]
fn acp_json_rpc_request_without_timeout_waits_for_late_response() {
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let (result_tx, result_rx) = mpsc::channel();
    let request_pending_requests = pending_requests.clone();

    std::thread::spawn(move || {
        let mut writer = Vec::new();
        let result = send_acp_json_rpc_request_without_timeout(
            &mut writer,
            &request_pending_requests,
            "session/prompt",
            json!({
                "sessionId": "cursor-session-1",
                "prompt": [],
            }),
            AcpAgent::Cursor,
        )
        .expect("prompt request should resolve once a response arrives");
        result_tx
            .send((
                String::from_utf8(writer).expect("request payload should be UTF-8"),
                result,
            ))
            .unwrap();
    });

    std::thread::sleep(Duration::from_millis(50));

    let (request_id, sender) = {
        let mut locked = pending_requests
            .lock()
            .expect("ACP pending requests mutex poisoned");
        assert_eq!(locked.len(), 1);
        let request_id = locked
            .keys()
            .next()
            .cloned()
            .expect("request id should exist");
        let sender = locked
            .remove(&request_id)
            .expect("request sender should still be pending");
        (request_id, sender)
    };

    sender.send(Ok(json!({ "ok": true }))).unwrap();

    let (written, result) = result_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("late ACP response should unblock the prompt request");
    assert!(written.contains("\"method\":\"session/prompt\""));
    assert!(written.contains(&format!("\"id\":\"{request_id}\"")));
    assert_eq!(result, json!({ "ok": true }));
    assert!(
        pending_requests
            .lock()
            .expect("ACP pending requests mutex poisoned")
            .is_empty()
    );
}

#[test]
fn acp_prompt_command_keeps_writer_loop_responsive_while_waiting_for_response() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Prompt Loop".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Ask),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: Some("cursor-session-1".to_owned()),
        is_loading_history: false,
    }));
    let writer = SharedBufferWriter::default();
    let thread_writer = writer.clone();
    let thread_pending_requests = pending_requests.clone();
    let thread_runtime_state = runtime_state.clone();
    let thread_state = state.clone();
    let thread_session_id = created.session_id.clone();
    let runtime_token = RuntimeToken::Acp("cursor-runtime-1".to_owned());
    let (input_tx, input_rx) = mpsc::channel();

    let writer_thread = std::thread::spawn(move || {
        let mut stdin = thread_writer;
        while let Ok(command) = input_rx.recv_timeout(Duration::from_millis(250)) {
            match command {
                AcpRuntimeCommand::Prompt(prompt) => handle_acp_prompt_command(
                    &mut stdin,
                    &thread_pending_requests,
                    &thread_state,
                    &thread_session_id,
                    &thread_runtime_state,
                    &runtime_token,
                    AcpAgent::Cursor,
                    prompt,
                )
                .unwrap(),
                AcpRuntimeCommand::JsonRpcMessage(message) => {
                    write_acp_json_rpc_message(&mut stdin, &message, AcpAgent::Cursor).unwrap();
                }
                AcpRuntimeCommand::RefreshSessionConfig { .. } => {
                    panic!("unexpected config refresh in prompt loop test");
                }
            }
        }
    });

    input_tx
        .send(AcpRuntimeCommand::Prompt(AcpPromptCommand {
            cwd: "/tmp".to_owned(),
            cursor_mode: Some(CursorMode::Ask),
            model: "auto".to_owned(),
            prompt: "review-local".to_owned(),
            resume_session_id: Some("cursor-session-1".to_owned()),
        }))
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        if pending_requests
            .lock()
            .expect("ACP pending requests mutex poisoned")
            .len()
            == 1
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "prompt request should stay pending while waiting for a response"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    input_tx
        .send(AcpRuntimeCommand::JsonRpcMessage(json!({
            "id": "approval-1",
            "result": {
                "outcome": {
                    "outcome": "selected",
                    "optionId": "allow-once",
                }
            }
        })))
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let written = writer.contents();
        if written.contains("\"method\":\"session/prompt\"")
            && written.contains("\"id\":\"approval-1\"")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "writer loop should remain able to write approval responses while prompt is pending"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let sender = {
        let mut locked = pending_requests
            .lock()
            .expect("ACP pending requests mutex poisoned");
        let request_id = locked
            .keys()
            .next()
            .cloned()
            .expect("prompt request id should exist");
        locked
            .remove(&request_id)
            .expect("prompt request sender should still be pending")
    };
    sender.send(Ok(json!({ "ok": true }))).unwrap();

    drop(input_tx);
    writer_thread.join().unwrap();
}

#[test]
fn fail_pending_acp_requests_releases_waiters() {
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let (tx, rx) = mpsc::channel::<std::result::Result<Value, String>>();

    pending_requests
        .lock()
        .expect("ACP pending requests mutex poisoned")
        .insert("req-1".to_owned(), tx);

    fail_pending_acp_requests(&pending_requests, "Cursor ACP runtime exited.");

    assert!(
        pending_requests
            .lock()
            .expect("ACP pending requests mutex poisoned")
            .is_empty()
    );
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        Err("Cursor ACP runtime exited.".to_owned())
    );
}

fn test_shared_codex_runtime(
    runtime_id: &str,
) -> (
    SharedCodexRuntime,
    mpsc::Receiver<CodexRuntimeCommand>,
    Arc<SharedChild>,
) {
    let child = test_exit_success_child();
    let process = Arc::new(SharedChild::new(child).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = SharedCodexRuntime {
        runtime_id: runtime_id.to_owned(),
        input_tx,
        process: process.clone(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
    };
    (runtime, input_rx, process)
}

async fn request_json<T: for<'de> Deserialize<'de>>(
    app: &Router,
    request: Request<Body>,
) -> (StatusCode, T) {
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("request should complete");
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body should read");
    let parsed = serde_json::from_slice(&body).expect("response body should be valid JSON");
    (status, parsed)
}

#[test]
fn wait_for_shared_child_exit_timeout_returns_status_for_completed_process() {
    let child = test_exit_success_child();
    let process = Arc::new(SharedChild::new(child).unwrap());

    let status = wait_for_shared_child_exit_timeout(&process, Duration::from_secs(1), "test child")
        .unwrap()
        .expect("completed process should return a status");

    assert!(status.success());
}

#[test]
fn wait_for_shared_child_exit_timeout_returns_none_for_running_process() {
    let child = test_sleep_child();
    let process = Arc::new(SharedChild::new(child).unwrap());

    let status =
        wait_for_shared_child_exit_timeout(&process, Duration::from_millis(10), "test child")
            .unwrap();

    assert!(status.is_none());
    process.kill().unwrap();
    process.wait().unwrap();
}

#[test]
fn shutdown_repl_codex_process_forces_running_process_after_timeout() {
    let child = test_sleep_child();
    let process = Arc::new(SharedChild::new(child).unwrap());

    let (status, forced_shutdown) = shutdown_repl_codex_process(&process).unwrap();

    assert!(forced_shutdown);
    assert!(!status.success());
}

#[test]
fn reads_claude_agent_commands_from_markdown_files() {
    let root = std::env::temp_dir().join(format!("termal-agent-commands-{}", Uuid::new_v4()));
    let commands_dir = root.join(".claude").join("commands");

    fs::create_dir_all(commands_dir.join("nested")).unwrap();
    fs::write(
        commands_dir.join("review-local.md"),
        "Review local changes.

## Step 1
Inspect diffs.
",
    )
    .unwrap();
    fs::write(
        commands_dir.join("fix-bug.md"),
        "
Fix a bug from docs/bugs.md by number.

$ARGUMENTS
",
    )
    .unwrap();
    fs::write(commands_dir.join("notes.txt"), "ignore").unwrap();
    fs::write(commands_dir.join("nested").join("ignored.md"), "ignore").unwrap();

    let commands = read_claude_agent_commands(&root).unwrap();

    assert_eq!(
        commands,
        vec![
            AgentCommand {
                kind: AgentCommandKind::PromptTemplate,
                name: "fix-bug".to_owned(),
                description: "Fix a bug from docs/bugs.md by number.".to_owned(),
                content: "
Fix a bug from docs/bugs.md by number.

$ARGUMENTS
"
                .to_owned(),
                source: ".claude/commands/fix-bug.md".to_owned(),
                argument_hint: None,
            },
            AgentCommand {
                kind: AgentCommandKind::PromptTemplate,
                name: "review-local".to_owned(),
                description: "Review local changes.".to_owned(),
                content: "Review local changes.

## Step 1
Inspect diffs.
"
                .to_owned(),
                source: ".claude/commands/review-local.md".to_owned(),
                argument_hint: None,
            },
        ]
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn returns_empty_agent_commands_when_commands_directory_is_missing() {
    let root =
        std::env::temp_dir().join(format!("termal-agent-commands-missing-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();

    let commands = read_claude_agent_commands(&root).unwrap();
    assert!(commands.is_empty());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn returns_agent_commands_for_non_claude_sessions() {
    let root = std::env::temp_dir().join(format!("termal-agent-commands-codex-{}", Uuid::new_v4()));
    let commands_dir = root.join(".claude").join("commands");
    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("review-local.md"),
        "Review local changes.

Use the active agent's tools.
",
    )
    .unwrap();

    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Session".to_owned()),
            workdir: Some(root.to_string_lossy().into_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let response = state.list_agent_commands(&created.session_id).unwrap();
    assert_eq!(response.commands.len(), 1);
    assert_eq!(response.commands[0].name, "review-local");
    assert_eq!(response.commands[0].description, "Review local changes.");
    assert_eq!(response.commands[0].kind, AgentCommandKind::PromptTemplate);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn extracts_claude_native_agent_commands_from_initialize_response() {
    let message = json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "response": {
                "commands": [
                    {
                        "name": "review",
                        "description": "Review the current changes. (bundled)",
                        "argumentHint": ""
                    },
                    {
                        "name": "review-local",
                        "description": "Review local changes. (project)",
                        "argumentHint": "[scope]"
                    }
                ]
            }
        }
    });

    assert_eq!(
        claude_agent_commands(&message),
        Some(vec![
            AgentCommand {
                kind: AgentCommandKind::NativeSlash,
                name: "review".to_owned(),
                description: "Review the current changes.".to_owned(),
                content: "/review".to_owned(),
                source: "Claude bundled command".to_owned(),
                argument_hint: None,
            },
            AgentCommand {
                kind: AgentCommandKind::NativeSlash,
                name: "review-local".to_owned(),
                description: "Review local changes.".to_owned(),
                content: "/review-local".to_owned(),
                source: "Claude project command".to_owned(),
                argument_hint: Some("[scope]".to_owned()),
            },
        ])
    );
}

#[test]
fn extracts_claude_native_agent_commands_filters_empty_names_and_normalizes_user_suffix() {
    let message = json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "response": {
                "commands": [
                    {
                        "name": "   ",
                        "description": "Should be filtered."
                    },
                    {
                        "name": "release-notes",
                        "description": "Draft release notes. (user)"
                    }
                ]
            }
        }
    });

    assert_eq!(
        claude_agent_commands(&message),
        Some(vec![AgentCommand {
            kind: AgentCommandKind::NativeSlash,
            name: "release-notes".to_owned(),
            description: "Draft release notes.".to_owned(),
            content: "/release-notes".to_owned(),
            source: "Claude user command".to_owned(),
            argument_hint: None,
        }])
    );
}

#[test]
fn extracts_claude_native_agent_commands_returns_none_for_empty_command_list() {
    let message = json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "response": {
                "commands": []
            }
        }
    });

    assert_eq!(claude_agent_commands(&message), None);
}

#[test]
fn returns_cached_claude_native_commands_alongside_template_fallbacks() {
    let root = std::env::temp_dir().join(format!(
        "termal-agent-commands-claude-native-{}",
        Uuid::new_v4()
    ));
    let commands_dir = root.join(".claude").join("commands");
    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("review-local.md"),
        "Review local changes from the filesystem template.",
    )
    .unwrap();
    fs::write(
        commands_dir.join("fix-bug.md"),
        "Fix a bug from docs/bugs.md by number.\n\n$ARGUMENTS\n",
    )
    .unwrap();

    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude Session".to_owned()),
            workdir: Some(root.to_string_lossy().into_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    state
        .sync_session_agent_commands(
            &created.session_id,
            vec![
                AgentCommand {
                    kind: AgentCommandKind::NativeSlash,
                    name: "review".to_owned(),
                    description: "Review the current changes.".to_owned(),
                    content: "/review".to_owned(),
                    source: "Claude bundled command".to_owned(),
                    argument_hint: None,
                },
                AgentCommand {
                    kind: AgentCommandKind::NativeSlash,
                    name: "review-local".to_owned(),
                    description: "Review local changes.".to_owned(),
                    content: "/review-local".to_owned(),
                    source: "Claude project command".to_owned(),
                    argument_hint: Some("[scope]".to_owned()),
                },
            ],
        )
        .unwrap();

    let response = state.list_agent_commands(&created.session_id).unwrap();

    assert_eq!(
        response
            .commands
            .iter()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>(),
        vec!["fix-bug", "review", "review-local"]
    );
    assert_eq!(response.commands[0].kind, AgentCommandKind::PromptTemplate);
    assert_eq!(response.commands[1].kind, AgentCommandKind::NativeSlash);
    assert_eq!(response.commands[2].kind, AgentCommandKind::NativeSlash);
    assert_eq!(
        response.commands[2].argument_hint.as_deref(),
        Some("[scope]")
    );
    assert_eq!(response.commands[2].source, "Claude project command");

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sync_session_agent_commands_bumps_visible_session_command_revision() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude Session".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let starting_revision = created.state.revision;
    let starting_session_revision = created
        .state
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("created Claude session should exist")
        .agent_commands_revision;

    state
        .sync_session_agent_commands(
            &created.session_id,
            vec![AgentCommand {
                kind: AgentCommandKind::NativeSlash,
                name: "review".to_owned(),
                description: "Review the current changes.".to_owned(),
                content: "/review".to_owned(),
                source: "Claude bundled command".to_owned(),
                argument_hint: None,
            }],
        )
        .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Claude session should exist");
    assert!(snapshot.revision > starting_revision);
    assert_eq!(
        session.agent_commands_revision,
        starting_session_revision.saturating_add(1)
    );
}

#[test]
fn returns_not_found_for_missing_agent_command_session() {
    let state = test_app_state();
    let error = state.list_agent_commands("missing-session").unwrap_err();

    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert_eq!(error.message, "session not found");
}

#[test]
fn instruction_search_returns_all_roots_for_a_phrase() {
    let root = std::env::temp_dir().join(format!("termal-instruction-search-{}", Uuid::new_v4()));
    let docs_dir = root.join("docs");
    fs::create_dir_all(&docs_dir).unwrap();
    fs::write(
        root.join("AGENTS.md"),
        "See docs/backend.md for service rules.\n",
    )
    .unwrap();
    fs::write(
        root.join("CLAUDE.md"),
        "Use docs/backend.md for implementation guidance.\n",
    )
    .unwrap();
    fs::write(
        docs_dir.join("backend.md"),
        "# Backend\n\nPrefer dependency injection when module boundaries shift.\n",
    )
    .unwrap();

    let response = search_instruction_phrase(&root, "dependency injection").unwrap();

    assert_eq!(response.matches.len(), 1);
    let matched = &response.matches[0];
    assert!(
        matched.path.ends_with("docs\\backend.md") || matched.path.ends_with("docs/backend.md")
    );
    assert_eq!(
        matched.text,
        "Prefer dependency injection when module boundaries shift."
    );
    assert_eq!(matched.root_paths.len(), 2);
    assert_eq!(
        matched
            .root_paths
            .iter()
            .map(|root_path| root_path.root_path.clone())
            .collect::<Vec<_>>(),
        vec![
            normalize_path_best_effort(&root.join("AGENTS.md"))
                .to_string_lossy()
                .into_owned(),
            normalize_path_best_effort(&root.join("CLAUDE.md"))
                .to_string_lossy()
                .into_owned(),
        ]
    );
    assert!(
        matched
            .root_paths
            .iter()
            .all(|root_path| root_path.steps.len() == 1
                && root_path.steps[0].to_path == matched.path)
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn instruction_search_expands_directory_discovery_edges() {
    let root = std::env::temp_dir().join(format!(
        "termal-instruction-directory-search-{}",
        Uuid::new_v4()
    ));
    let reviewers_dir = root.join(".claude").join("reviewers");
    let commands_dir = root.join(".claude").join("commands");
    fs::create_dir_all(&reviewers_dir).unwrap();
    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("review-local.md"),
        "Discover reviewers in .claude/reviewers before running checks.\n",
    )
    .unwrap();
    fs::write(
        reviewers_dir.join("rust.md"),
        "Prefer dependency injection at unstable ownership boundaries.\n",
    )
    .unwrap();

    let response = search_instruction_phrase(&root, "dependency injection").unwrap();

    assert_eq!(response.matches.len(), 1);
    let matched = &response.matches[0];
    assert!(
        matched.path.ends_with(".claude\\reviewers\\rust.md")
            || matched.path.ends_with(".claude/reviewers/rust.md")
    );
    assert_eq!(matched.root_paths.len(), 1);
    let root_path = &matched.root_paths[0];
    assert_eq!(
        root_path.root_path,
        normalize_path_best_effort(&commands_dir.join("review-local.md"))
            .to_string_lossy()
            .into_owned()
    );
    assert_eq!(root_path.steps.len(), 1);
    assert_eq!(
        root_path.steps[0].relation,
        InstructionRelation::DirectoryDiscovery
    );
    assert_eq!(
        root_path.steps[0].to_path,
        normalize_path_best_effort(&reviewers_dir.join("rust.md"))
            .to_string_lossy()
            .into_owned()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn instruction_search_stops_at_generic_referenced_docs() {
    let root = std::env::temp_dir().join(format!(
        "termal-instruction-generic-docs-{}",
        Uuid::new_v4()
    ));
    let docs_dir = root.join("docs");
    let features_dir = docs_dir.join("features");
    fs::create_dir_all(&features_dir).unwrap();
    fs::write(
        root.join("AGENTS.md"),
        "See README.md for additional context.\n",
    )
    .unwrap();
    fs::write(
        root.join("README.md"),
        "- [docs/bugs.md](docs/bugs.md) - implementation backlog\n",
    )
    .unwrap();
    fs::write(
        docs_dir.join("bugs.md"),
        "- [Instruction Debugger](./features/instruction-debugger.md)\n",
    )
    .unwrap();
    fs::write(
        features_dir.join("instruction-debugger.md"),
        "Prefer dependency injection when debugging instruction graphs.\n",
    )
    .unwrap();

    let response = search_instruction_phrase(&root, "dependency injection").unwrap();

    assert!(response.matches.is_empty());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn instruction_search_walks_instructionish_docs_transitively() {
    let root =
        std::env::temp_dir().join(format!("termal-instruction-transitive-{}", Uuid::new_v4()));
    let instructions_dir = root.join("docs").join("instructions");
    fs::create_dir_all(&instructions_dir).unwrap();
    fs::write(
        root.join("AGENTS.md"),
        "Use docs/instructions/backend.md for service rules.\n",
    )
    .unwrap();
    fs::write(
        instructions_dir.join("backend.md"),
        "See shared.md for composition guidance.\n",
    )
    .unwrap();
    fs::write(
        instructions_dir.join("shared.md"),
        "Prefer dependency injection when module boundaries shift.\n",
    )
    .unwrap();

    let response = search_instruction_phrase(&root, "dependency injection").unwrap();

    assert_eq!(response.matches.len(), 1);
    let matched = &response.matches[0];
    assert!(
        matched.path.ends_with("docs\\instructions\\shared.md")
            || matched.path.ends_with("docs/instructions/shared.md")
    );
    assert_eq!(matched.root_paths.len(), 1);
    let root_path = &matched.root_paths[0];
    assert_eq!(
        root_path.root_path,
        normalize_path_best_effort(&root.join("AGENTS.md"))
            .to_string_lossy()
            .into_owned()
    );
    assert_eq!(root_path.steps.len(), 2);
    assert_eq!(
        root_path.steps[0].to_path,
        normalize_path_best_effort(&instructions_dir.join("backend.md"))
            .to_string_lossy()
            .into_owned()
    );
    assert_eq!(
        root_path.steps[1].to_path,
        normalize_path_best_effort(&instructions_dir.join("shared.md"))
            .to_string_lossy()
            .into_owned()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn instruction_search_ignores_internal_termal_roots_for_claude_reviewers() {
    let root = std::env::temp_dir().join(format!(
        "termal-instruction-realtime-search-{}",
        Uuid::new_v4()
    ));
    let commands_dir = root.join(".claude").join("commands");
    let reviewers_dir = root.join(".claude").join("reviewers");
    let docs_features_dir = root.join("docs").join("features");
    let internal_skill_dir = root
        .join(".termal")
        .join("codex-home")
        .join("session-1")
        .join("skills")
        .join(".system")
        .join("skill-creator");
    fs::create_dir_all(&commands_dir).unwrap();
    fs::create_dir_all(&reviewers_dir).unwrap();
    fs::create_dir_all(&docs_features_dir).unwrap();
    fs::create_dir_all(&internal_skill_dir).unwrap();

    fs::write(
        commands_dir.join("review-local.md"),
        "Run `find .claude/reviewers -name \"*.md\" 2>/dev/null` via Bash to find all available reviewer lens files.\n",
    )
    .unwrap();
    fs::write(
        commands_dir.join("fix-bug.md"),
        "Read `docs/bugs.md` and find the matching bug entry.\n",
    )
    .unwrap();
    fs::write(
        reviewers_dir.join("react-typescript.md"),
        "5. **SSE / real-time handling**:\n",
    )
    .unwrap();
    fs::write(
        root.join("README.md"),
        "- [`docs/bugs.md`](docs/bugs.md) - implementation backlog\n",
    )
    .unwrap();
    fs::write(
        root.join("docs").join("bugs.md"),
        "- [Instruction Debugger](./features/instruction-debugger.md)\n",
    )
    .unwrap();
    fs::write(
        docs_features_dir.join("instruction-debugger.md"),
        "- a reviewer file was discovered from `.claude/reviewers/`\n",
    )
    .unwrap();
    fs::write(internal_skill_dir.join("SKILL.md"), "- README.md\n").unwrap();

    let response = search_instruction_phrase(&root, "real-time handling").unwrap();

    assert_eq!(response.matches.len(), 1);
    let matched = &response.matches[0];
    assert!(
        matched
            .path
            .ends_with(".claude\\reviewers\\react-typescript.md")
            || matched
                .path
                .ends_with(".claude/reviewers/react-typescript.md")
    );
    assert_eq!(matched.root_paths.len(), 1);
    let root_path = &matched.root_paths[0];
    assert_eq!(
        root_path.root_path,
        normalize_path_best_effort(&commands_dir.join("review-local.md"))
            .to_string_lossy()
            .into_owned()
    );
    assert_eq!(root_path.steps.len(), 1);
    assert_eq!(
        root_path.steps[0].relation,
        InstructionRelation::DirectoryDiscovery
    );
    assert_eq!(
        root_path.steps[0].to_path,
        normalize_path_best_effort(&reviewers_dir.join("react-typescript.md"))
            .to_string_lossy()
            .into_owned()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn instruction_search_returns_not_found_for_missing_session() {
    let state = test_app_state();
    let error = state
        .search_instructions("missing-session", "dependency injection")
        .unwrap_err();

    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert_eq!(error.message, "session not found");
}

#[test]
fn creates_claude_sessions_with_default_ask_mode() {
    let mut inner = StateInner::new();

    let record = inner.create_session(Agent::Claude, None, "/tmp".to_owned(), None, None);

    assert_eq!(
        record.session.claude_approval_mode,
        Some(ClaudeApprovalMode::Ask)
    );
    assert_eq!(
        record.session.claude_effort,
        Some(ClaudeEffortLevel::Default)
    );
    assert_eq!(record.session.approval_policy, None);
    assert_eq!(record.session.sandbox_mode, None);
}

#[test]
fn creates_claude_sessions_with_requested_plan_mode() {
    let state = test_app_state();

    let response = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Plan Claude".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: Some(ClaudeApprovalMode::Plan),
            claude_effort: Some(ClaudeEffortLevel::High),
            gemini_approval_mode: None,
        })
        .unwrap();
    let session = response
        .state
        .sessions
        .iter()
        .find(|session| session.id == response.session_id)
        .expect("created session should be present");

    assert_eq!(session.claude_approval_mode, Some(ClaudeApprovalMode::Plan));
    assert_eq!(session.claude_effort, Some(ClaudeEffortLevel::High));
}

#[test]
fn hidden_claude_spares_are_filtered_from_snapshots_and_persistence() {
    let state = test_app_state();
    let workdir = resolve_session_workdir("/tmp").expect("test workdir should resolve");
    let hidden_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .ensure_hidden_claude_spare(
                workdir,
                None,
                Agent::Claude.default_model().to_owned(),
                ClaudeApprovalMode::Ask,
                ClaudeEffortLevel::Default,
            )
            .expect("hidden Claude spare should be created")
    };

    let snapshot = state.snapshot();
    assert!(
        snapshot
            .sessions
            .iter()
            .all(|session| session.id != hidden_session_id)
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .sessions
            .iter()
            .any(|record| record.hidden && record.session.id == hidden_session_id)
    );
    let persisted = PersistedState::from_inner(&inner);
    assert!(
        persisted
            .sessions
            .iter()
            .all(|record| record.session.id != hidden_session_id)
    );
}

#[test]
fn create_session_promotes_matching_hidden_claude_spare_and_replenishes_pool() {
    let state = test_app_state();
    let workdir = resolve_session_workdir("/tmp").expect("test workdir should resolve");
    let hidden_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .ensure_hidden_claude_spare(
                workdir.clone(),
                None,
                Agent::Claude.default_model().to_owned(),
                ClaudeApprovalMode::Ask,
                ClaudeEffortLevel::Default,
            )
            .expect("hidden Claude spare should be created")
    };

    let response = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Visible Claude".to_owned()),
            workdir: Some(workdir.clone()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    assert_eq!(response.session_id, hidden_session_id);
    let session = response
        .state
        .sessions
        .iter()
        .find(|session| session.id == hidden_session_id)
        .expect("promoted hidden session should be visible");
    assert_eq!(session.name, "Visible Claude");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let promoted = inner
        .sessions
        .iter()
        .find(|record| record.session.id == hidden_session_id)
        .expect("promoted session record should exist");
    assert!(!promoted.hidden);

    let hidden_spares = inner
        .sessions
        .iter()
        .filter(|record| {
            record.hidden
                && record.session.agent == Agent::Claude
                && record.session.workdir == workdir
                && record.session.project_id.is_none()
        })
        .collect::<Vec<_>>();
    assert_eq!(hidden_spares.len(), 1);
    assert_ne!(hidden_spares[0].session.id, hidden_session_id);
}

#[test]
fn create_session_promotes_matching_non_default_hidden_claude_spare() {
    let state = test_app_state();
    let workdir = resolve_session_workdir("/tmp").expect("test workdir should resolve");
    let hidden_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .ensure_hidden_claude_spare(
                workdir.clone(),
                None,
                "claude-custom".to_owned(),
                ClaudeApprovalMode::Plan,
                ClaudeEffortLevel::High,
            )
            .expect("hidden Claude spare should be created")
    };

    let response = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Plan Claude".to_owned()),
            workdir: Some(workdir.clone()),
            project_id: None,
            model: Some("claude-custom".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: Some(ClaudeApprovalMode::Plan),
            claude_effort: Some(ClaudeEffortLevel::High),
            gemini_approval_mode: None,
        })
        .unwrap();

    assert_eq!(response.session_id, hidden_session_id);
    let inner = state.inner.lock().expect("state mutex poisoned");
    let hidden_spares = inner
        .sessions
        .iter()
        .filter(|record| {
            record.hidden
                && record.session.agent == Agent::Claude
                && record.session.workdir == workdir
                && record.session.model == "claude-custom"
                && record.session.claude_approval_mode == Some(ClaudeApprovalMode::Plan)
                && record.session.claude_effort == Some(ClaudeEffortLevel::High)
        })
        .collect::<Vec<_>>();
    assert_eq!(hidden_spares.len(), 1);
    assert_ne!(hidden_spares[0].session.id, hidden_session_id);
}

#[test]
fn killing_last_visible_claude_session_reaps_hidden_spare_for_context() {
    let state = test_app_state();
    let workdir = resolve_session_workdir("/tmp").expect("test workdir should resolve");
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude Visible".to_owned()),
            workdir: Some(workdir.clone()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert!(inner.sessions.iter().any(|record| {
            record.hidden
                && record.session.agent == Agent::Claude
                && record.session.workdir == workdir
        }));
    }

    let killed = state.kill_session(&created.session_id).unwrap();
    assert!(
        killed
            .sessions
            .iter()
            .all(|session| session.id != created.session_id)
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.sessions.iter().all(|record| {
        !(record.session.agent == Agent::Claude && record.session.workdir == workdir)
    }));
}

#[test]
fn killing_one_visible_claude_session_keeps_hidden_spares_when_another_visible_session_remains() {
    let state = test_app_state();
    let workdir = resolve_session_workdir("/tmp").expect("test workdir should resolve");
    let first = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude A".to_owned()),
            workdir: Some(workdir.clone()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let second = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude B".to_owned()),
            workdir: Some(workdir.clone()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    state.kill_session(&first.session_id).unwrap();

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .sessions
            .iter()
            .any(|record| !record.hidden && record.session.id == second.session_id)
    );
    assert!(inner.sessions.iter().any(|record| {
        record.hidden
            && record.session.agent == Agent::Claude
            && record.session.workdir == workdir
            && record.session.project_id.is_none()
    }));
}

#[test]
fn persists_app_settings_and_applies_them_to_new_sessions() {
    let state = test_app_state();

    let updated = state
        .update_app_settings(UpdateAppSettingsRequest {
            default_codex_reasoning_effort: Some(CodexReasoningEffort::High),
            default_claude_effort: Some(ClaudeEffortLevel::Max),
            remotes: None,
        })
        .unwrap();

    assert_eq!(
        updated.preferences.default_codex_reasoning_effort,
        CodexReasoningEffort::High
    );
    assert_eq!(
        updated.preferences.default_claude_effort,
        ClaudeEffortLevel::Max
    );
    assert_eq!(updated.preferences.remotes, default_remote_configs());

    let reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    assert_eq!(
        reloaded_inner.preferences.default_codex_reasoning_effort,
        CodexReasoningEffort::High
    );
    assert_eq!(
        reloaded_inner.preferences.default_claude_effort,
        ClaudeEffortLevel::Max
    );
    assert_eq!(reloaded_inner.preferences.remotes, default_remote_configs());

    let reloaded_state = AppState {
        default_workdir: "/tmp".to_owned(),
        persistence_path: state.persistence_path.clone(),
        state_events: broadcast::channel(16).0,
        delta_events: broadcast::channel(16).0,
        shared_codex_runtime: Arc::new(Mutex::new(None)),
        remote_registry: test_remote_registry(),
        inner: Arc::new(Mutex::new(reloaded_inner)),
    };

    let codex_created = reloaded_state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Persisted Codex".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let codex_session = codex_created
        .state
        .sessions
        .iter()
        .find(|session| session.id == codex_created.session_id)
        .expect("created Codex session should be present");
    assert_eq!(
        codex_session.reasoning_effort,
        Some(CodexReasoningEffort::High)
    );

    let claude_created = reloaded_state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Persisted Claude".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let claude_session = claude_created
        .state
        .sessions
        .iter()
        .find(|session| session.id == claude_created.session_id)
        .expect("created Claude session should be present");
    assert_eq!(claude_session.claude_effort, Some(ClaudeEffortLevel::Max));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn creates_codex_sessions_with_requested_prompt_defaults() {
    let state = test_app_state();

    let response = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Custom Codex".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5-mini".to_owned()),
            approval_policy: Some(CodexApprovalPolicy::OnRequest),
            reasoning_effort: Some(CodexReasoningEffort::High),
            sandbox_mode: Some(CodexSandboxMode::ReadOnly),
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let session = response
        .state
        .sessions
        .iter()
        .find(|session| session.id == response.session_id)
        .expect("created session should be present");

    assert_eq!(
        session.approval_policy,
        Some(CodexApprovalPolicy::OnRequest)
    );
    assert_eq!(session.model, "gpt-5-mini");
    assert_eq!(session.reasoning_effort, Some(CodexReasoningEffort::High));
    assert_eq!(session.sandbox_mode, Some(CodexSandboxMode::ReadOnly));
    assert_eq!(session.claude_approval_mode, None);

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .find_session_index(&response.session_id)
        .map(|index| &inner.sessions[index]);
    let record = record.expect("session record should exist");
    assert_eq!(record.codex_approval_policy, CodexApprovalPolicy::OnRequest);
    assert_eq!(record.codex_reasoning_effort, CodexReasoningEffort::High);
    assert_eq!(record.codex_sandbox_mode, CodexSandboxMode::ReadOnly);
}

#[test]
fn updates_cursor_session_model_settings() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Model".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Agent),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: Some("gpt-5.3-codex".to_owned()),
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Cursor session should be present");
    assert_eq!(session.model, "gpt-5.3-codex");
}

#[test]
fn updates_codex_session_model_settings_without_restarting_runtime() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Model".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: Some("gpt-5-mini".to_owned()),
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Codex session should be present");
    assert_eq!(session.model, "gpt-5-mini");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Codex session should exist");
    assert!(!record.runtime_reset_required);
}

#[test]
fn updates_codex_reasoning_effort_without_restarting_runtime() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Effort".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5".to_owned()),
            approval_policy: None,
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: None,
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: Some(CodexReasoningEffort::High),
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Codex session should be present");
    assert_eq!(session.reasoning_effort, Some(CodexReasoningEffort::High));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Codex session should exist");
    assert_eq!(record.codex_reasoning_effort, CodexReasoningEffort::High);
    assert!(!record.runtime_reset_required);
}

#[test]
fn normalizes_codex_reasoning_effort_when_switching_models() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Model Caps".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5".to_owned()),
            approval_policy: None,
            reasoning_effort: Some(CodexReasoningEffort::Minimal),
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    state
        .sync_session_model_options(
            &created.session_id,
            None,
            vec![
                SessionModelOption {
                    label: "GPT-5".to_owned(),
                    value: "gpt-5".to_owned(),
                    description: Some("Frontier agentic coding model.".to_owned()),
                    badges: Vec::new(),
                    supported_claude_effort_levels: Vec::new(),
                    default_reasoning_effort: Some(CodexReasoningEffort::Medium),
                    supported_reasoning_efforts: vec![
                        CodexReasoningEffort::Minimal,
                        CodexReasoningEffort::Low,
                        CodexReasoningEffort::Medium,
                        CodexReasoningEffort::High,
                    ],
                },
                SessionModelOption {
                    label: "GPT-5 Codex Mini".to_owned(),
                    value: "gpt-5-codex-mini".to_owned(),
                    description: Some(
                        "Optimized for codex. Cheaper, faster, but less capable.".to_owned(),
                    ),
                    badges: Vec::new(),
                    supported_claude_effort_levels: Vec::new(),
                    default_reasoning_effort: Some(CodexReasoningEffort::Medium),
                    supported_reasoning_efforts: vec![
                        CodexReasoningEffort::Medium,
                        CodexReasoningEffort::High,
                    ],
                },
            ],
        )
        .unwrap();

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: Some("gpt-5-codex-mini".to_owned()),
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Codex session should be present");
    assert_eq!(session.model, "gpt-5-codex-mini");
    assert_eq!(session.reasoning_effort, Some(CodexReasoningEffort::Medium));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Codex session should exist");
    assert_eq!(record.codex_reasoning_effort, CodexReasoningEffort::Medium);
}

#[test]
fn rejects_unsupported_codex_reasoning_effort_for_selected_model() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Invalid Effort".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5-codex-mini".to_owned()),
            approval_policy: None,
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    state
        .sync_session_model_options(
            &created.session_id,
            None,
            vec![SessionModelOption {
                label: "GPT-5 Codex Mini".to_owned(),
                value: "gpt-5-codex-mini".to_owned(),
                description: Some(
                    "Optimized for codex. Cheaper, faster, but less capable.".to_owned(),
                ),
                badges: Vec::new(),
                supported_claude_effort_levels: Vec::new(),
                default_reasoning_effort: Some(CodexReasoningEffort::Medium),
                supported_reasoning_efforts: vec![
                    CodexReasoningEffort::Medium,
                    CodexReasoningEffort::High,
                ],
            }],
        )
        .unwrap();

    let error = match state.update_session_settings(
        &created.session_id,
        UpdateSessionSettingsRequest {
            name: None,
            model: None,
            sandbox_mode: None,
            approval_policy: None,
            reasoning_effort: Some(CodexReasoningEffort::Low),
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        },
    ) {
        Ok(_) => panic!("unsupported Codex effort should be rejected"),
        Err(error) => error,
    };

    assert!(
        error
            .message
            .contains("does not support `low` reasoning effort; choose medium or high")
    );
}

#[test]
fn updates_claude_session_model_settings_without_restarting_runtime() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude Model".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("sonnet".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: Some(ClaudeApprovalMode::Ask),
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let child = test_exit_success_child();
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-model-update".to_owned(),
        input_tx,
        process: Arc::new(SharedChild::new(child).unwrap()),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&created.session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    }

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: Some("opus".to_owned()),
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Claude session should be present");
    assert_eq!(session.model, "opus");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Claude session should exist");
    assert!(!record.runtime_reset_required);

    let command = input_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("Claude model update should arrive");
    match command {
        ClaudeRuntimeCommand::SetModel(model) => assert_eq!(model, "opus"),
        _ => panic!("expected Claude model update command"),
    }
}

#[test]
fn updates_claude_effort_and_marks_runtime_for_restart() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude Effort".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("sonnet".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: Some(ClaudeApprovalMode::Ask),
            claude_effort: Some(ClaudeEffortLevel::Default),
            gemini_approval_mode: None,
        })
        .unwrap();

    let child = test_exit_success_child();
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-effort-update".to_owned(),
        input_tx,
        process: Arc::new(SharedChild::new(child).unwrap()),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&created.session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    }

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: None,
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: Some(ClaudeEffortLevel::High),
                gemini_approval_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Claude session should be present");
    assert_eq!(session.claude_effort, Some(ClaudeEffortLevel::High));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Claude session should exist");
    assert!(record.runtime_reset_required);

    match input_rx.recv_timeout(Duration::from_millis(100)) {
        Err(mpsc::RecvTimeoutError::Timeout) => {}
        Ok(_) => panic!("Claude effort changes should not send a live runtime command"),
        Err(err) => panic!("unexpected channel error: {err}"),
    }
}

#[test]
fn syncs_claude_model_options_into_session_state() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude Refresh".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let model_options = vec![
        SessionModelOption::plain("Default (recommended)", "default"),
        SessionModelOption::plain("Sonnet", "sonnet"),
    ];

    state
        .sync_session_model_options(&created.session_id, None, model_options.clone())
        .expect("Claude model options should sync");

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("synced Claude session should be present");

    assert_eq!(session.model_options, model_options);
}

#[test]
fn refreshes_codex_model_options_from_runtime() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Refresh".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5.4".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let child = test_exit_success_child();
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = CodexRuntimeHandle {
        runtime_id: "codex-model-refresh".to_owned(),
        input_tx,
        process: Arc::new(SharedChild::new(child).unwrap()),
        shared_session: None,
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&created.session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }

    std::thread::spawn(move || {
        let command = input_rx
            .recv()
            .expect("Codex refresh command should arrive");
        match command {
            CodexRuntimeCommand::RefreshModelList { response_tx } => {
                let _ = response_tx.send(Ok(vec![
                    SessionModelOption::plain("gpt-5.4", "gpt-5.4"),
                    SessionModelOption::plain("gpt-5.3-codex", "gpt-5.3-codex"),
                ]));
            }
            _ => panic!("expected Codex model refresh command"),
        }
    });

    let refreshed = state
        .refresh_session_model_options(&created.session_id)
        .expect("Codex model refresh should succeed");
    let session = refreshed
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("refreshed Codex session should be present");

    assert_eq!(
        session.model_options,
        vec![
            SessionModelOption::plain("gpt-5.4", "gpt-5.4"),
            SessionModelOption::plain("gpt-5.3-codex", "gpt-5.3-codex"),
        ]
    );
}

#[test]
fn fork_codex_thread_creates_a_new_local_session() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Review".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5.4".to_owned()),
            approval_policy: Some(CodexApprovalPolicy::Never),
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    state
        .set_external_session_id(&created.session_id, "thread-origin".to_owned())
        .unwrap();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&created.session_id)
            .expect("source Codex session should exist");
        inner.sessions[index].session.model_options =
            vec![SessionModelOption::plain("gpt-5.4", "gpt-5.4")];
    }

    let (runtime, input_rx, _process) = test_shared_codex_runtime("shared-codex-fork");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);

    std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Codex fork command should arrive");
        match command {
            CodexRuntimeCommand::JsonRpcRequest {
                method,
                params,
                response_tx,
                ..
            } => {
                assert_eq!(method, "thread/fork");
                assert_eq!(params["threadId"], "thread-origin");
                let _ = response_tx.send(Ok(json!({
                    "thread": {
                        "id": "thread-forked",
                        "name": "Forked Review",
                        "preview": "Forked preview",
                        "turns": [
                            {
                                "id": "turn-1",
                                "status": "completed",
                                "items": [
                                    {
                                        "id": "item-user-1",
                                        "type": "userMessage",
                                        "content": [
                                            {
                                                "type": "text",
                                                "text": "Review src/state.rs"
                                            },
                                            {
                                                "type": "mention",
                                                "name": "docs/bugs.md",
                                                "path": "docs/bugs.md"
                                            }
                                        ]
                                    },
                                    {
                                        "id": "item-reasoning-1",
                                        "type": "reasoning",
                                        "summary": ["Inspect session state."],
                                        "content": ["Watch archive transitions."]
                                    },
                                    {
                                        "id": "item-agent-1",
                                        "type": "agentMessage",
                                        "text": "I found the bug."
                                    },
                                    {
                                        "id": "item-command-1",
                                        "type": "commandExecution",
                                        "command": "git diff --stat",
                                        "commandActions": [],
                                        "cwd": "/tmp/forked",
                                        "status": "completed",
                                        "aggregatedOutput": "1 file changed",
                                        "exitCode": 0
                                    },
                                    {
                                        "id": "item-file-1",
                                        "type": "fileChange",
                                        "status": "completed",
                                        "changes": [
                                            {
                                                "path": "src/state.rs",
                                                "diff": "@@ -1 +1 @@\n-old\n+new",
                                                "kind": {
                                                    "type": "modify"
                                                }
                                            }
                                        ]
                                    }
                                ]
                            }
                        ]
                    },
                    "model": "gpt-5.5",
                    "approvalPolicy": "on-request",
                    "sandbox": {
                        "type": "workspaceWrite"
                    },
                    "reasoningEffort": "high",
                    "cwd": "/tmp/forked",
                })));
            }
            _ => panic!("expected shared Codex JSON-RPC request"),
        }
    });

    let forked = state.fork_codex_thread(&created.session_id).unwrap();
    assert_ne!(forked.session_id, created.session_id);

    let forked_session = forked
        .state
        .sessions
        .iter()
        .find(|session| session.id == forked.session_id)
        .expect("forked session should be present");
    assert_eq!(forked_session.name, "Forked Review Fork");
    assert_eq!(forked_session.model, "gpt-5.5");
    assert_eq!(
        forked_session.approval_policy,
        Some(CodexApprovalPolicy::OnRequest)
    );
    assert_eq!(
        forked_session.reasoning_effort,
        Some(CodexReasoningEffort::High)
    );
    assert_eq!(
        forked_session.sandbox_mode,
        Some(CodexSandboxMode::WorkspaceWrite)
    );
    assert_eq!(
        forked_session.external_session_id.as_deref(),
        Some("thread-forked")
    );
    assert_eq!(
        forked_session.codex_thread_state,
        Some(CodexThreadState::Active)
    );
    assert_eq!(forked_session.workdir, "/tmp/forked");
    assert_eq!(
        forked_session.model_options,
        vec![SessionModelOption::plain("gpt-5.4", "gpt-5.4")]
    );
    assert!(matches!(
        forked_session.messages.first(),
        Some(Message::Text { author: Author::You, text, .. })
            if text.contains("Review src/state.rs")
                && text.contains("Mention: docs/bugs.md (docs/bugs.md)")
    ));
    assert!(matches!(
        forked_session.messages.get(1),
        Some(Message::Thinking { title, lines, .. })
            if title == "Codex reasoning"
                && lines == &vec![
                    "Inspect session state.".to_owned(),
                    "Watch archive transitions.".to_owned(),
                ]
    ));
    assert!(matches!(
        forked_session.messages.get(2),
        Some(Message::Text { author: Author::Assistant, text, .. }) if text == "I found the bug."
    ));
    assert!(matches!(
        forked_session.messages.get(3),
        Some(Message::Command {
            command,
            output,
            status,
            ..
        }) if command == "git diff --stat"
            && output == "1 file changed"
            && *status == CommandStatus::Success
    ));
    assert!(matches!(
        forked_session.messages.get(4),
        Some(Message::Diff {
            file_path,
            summary,
            diff,
            change_type,
            ..
        }) if file_path == "src/state.rs"
            && summary == "Updated state.rs"
            && diff.contains("+new")
            && *change_type == ChangeType::Edit
    ));
    assert!(!forked_session.messages.iter().any(
        |message| matches!(message, Message::Markdown { title, .. } if title == "Forked Codex thread")
    ));
}

#[test]
fn fork_codex_thread_falls_back_to_note_when_history_is_unavailable() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Review".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5.4".to_owned()),
            approval_policy: Some(CodexApprovalPolicy::Never),
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    state
        .set_external_session_id(&created.session_id, "thread-origin".to_owned())
        .unwrap();

    let (runtime, input_rx, _process) = test_shared_codex_runtime("shared-codex-fork-fallback");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);

    std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Codex fork command should arrive");
        match command {
            CodexRuntimeCommand::JsonRpcRequest {
                method,
                params,
                response_tx,
                ..
            } => {
                assert_eq!(method, "thread/fork");
                assert_eq!(params["threadId"], "thread-origin");
                let _ = response_tx.send(Ok(json!({
                    "thread": {
                        "id": "thread-forked",
                        "name": "Forked Review",
                        "preview": "Forked preview"
                    },
                    "model": "gpt-5.5",
                    "approvalPolicy": "on-request",
                    "sandbox": {
                        "type": "workspaceWrite"
                    },
                    "reasoningEffort": "high",
                    "cwd": "/tmp/forked"
                })));
            }
            _ => panic!("expected shared Codex JSON-RPC request"),
        }
    });

    let forked = state.fork_codex_thread(&created.session_id).unwrap();
    let forked_session = forked
        .state
        .sessions
        .iter()
        .find(|session| session.id == forked.session_id)
        .expect("forked session should be present");
    assert!(matches!(
        forked_session.messages.last(),
        Some(Message::Markdown { title, markdown, .. })
            if title == "Forked Codex thread"
                && markdown.contains("Codex did not return the earlier thread history")
    ));
}

#[test]
fn codex_thread_actions_require_a_live_idle_thread() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);

    let missing_thread_error = match state.archive_codex_thread(&session_id) {
        Ok(_) => panic!("archive should fail without a live Codex thread"),
        Err(err) => err,
    };
    assert!(
        missing_thread_error
            .message
            .contains("only available after the session has started a thread")
    );

    state
        .set_external_session_id(&session_id, "thread-live".to_owned())
        .unwrap();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    let busy_error = match state.compact_codex_thread(&session_id) {
        Ok(_) => panic!("compact should fail while the session is active"),
        Err(err) => err,
    };
    assert!(
        busy_error
            .message
            .contains("wait for the current Codex turn to finish")
    );

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].session.status = SessionStatus::Idle;
        let queued_message_id = inner.next_message_id();
        queue_prompt_on_record(
            &mut inner.sessions[index],
            PendingPrompt {
                attachments: Vec::new(),
                id: queued_message_id,
                timestamp: stamp_now(),
                text: "queued prompt".to_owned(),
                expanded_text: None,
            },
            Vec::new(),
        );
    }

    let queued_error = match state.archive_codex_thread(&session_id) {
        Ok(_) => panic!("archive should fail while prompts are queued"),
        Err(err) => err,
    };
    assert!(
        queued_error
            .message
            .contains("wait for queued Codex prompts to finish")
    );
}

#[test]
fn codex_archive_and_unarchive_actions_update_thread_state_and_block_dispatch() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&session_id, "thread-live".to_owned())
        .unwrap();

    let initial_session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == session_id)
        .expect("Codex session should exist");
    assert_eq!(
        initial_session.codex_thread_state,
        Some(CodexThreadState::Active)
    );

    let (runtime, input_rx, _process) = test_shared_codex_runtime("shared-codex-archive");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);

    std::thread::spawn(move || {
        for expected_method in ["thread/archive", "thread/unarchive"] {
            let command = input_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("Codex thread action should arrive");
            match command {
                CodexRuntimeCommand::JsonRpcRequest {
                    method,
                    params,
                    response_tx,
                    ..
                } => {
                    assert_eq!(method, expected_method);
                    assert_eq!(params["threadId"], "thread-live");
                    let _ = response_tx.send(Ok(json!({})));
                }
                _ => panic!("expected shared Codex JSON-RPC request"),
            }
        }
    });

    let archived = state.archive_codex_thread(&session_id).unwrap();
    let archived_session = archived
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated Codex session should be present");
    assert_eq!(
        archived_session.codex_thread_state,
        Some(CodexThreadState::Archived)
    );
    assert!(matches!(
        archived_session.messages.last(),
        Some(Message::Markdown { title, .. }) if title == "Archived Codex thread"
    ));

    let archived_error = match state.dispatch_turn(
        &session_id,
        SendMessageRequest {
            text: "resume the review".to_owned(),
            expanded_text: None,
            attachments: Vec::new(),
        },
    ) {
        Ok(_) => panic!("archived Codex thread should reject new prompts"),
        Err(err) => err,
    };
    assert_eq!(archived_error.status, StatusCode::CONFLICT);
    assert!(
        archived_error
            .message
            .contains("current Codex thread is archived")
    );

    let restored = state.unarchive_codex_thread(&session_id).unwrap();
    let restored_session = restored
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated Codex session should be present");
    assert_eq!(
        restored_session.codex_thread_state,
        Some(CodexThreadState::Active)
    );
    assert!(matches!(
        restored_session.messages.last(),
        Some(Message::Markdown { title, .. }) if title == "Restored Codex thread"
    ));
}

#[test]
fn shared_codex_archive_notifications_update_thread_state() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&session_id, "conversation-123".to_owned())
        .unwrap();
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-thread-state");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let archived = json!({
        "method": "thread/archived",
        "params": {
            "threadId": "conversation-123"
        }
    });
    let unarchived = json!({
        "method": "thread/unarchived",
        "params": {
            "threadId": "conversation-123"
        }
    });

    handle_shared_codex_app_server_message(
        &archived,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let archived_session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(
        archived_session.codex_thread_state,
        Some(CodexThreadState::Archived)
    );

    handle_shared_codex_app_server_message(
        &unarchived,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let restored_session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(
        restored_session.codex_thread_state,
        Some(CodexThreadState::Active)
    );
}

#[test]
fn shared_codex_model_rerouted_notification_records_notice() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&session_id, "conversation-reroute".to_owned())
        .unwrap();
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-reroute");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-reroute".to_owned()),
                turn_id: Some("turn-reroute".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-reroute".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let rerouted = json!({
        "method": "model/rerouted",
        "params": {
            "threadId": "conversation-reroute",
            "turnId": "turn-reroute",
            "fromModel": "gpt-5.4",
            "toModel": "gpt-5.4-mini",
            "reason": "highRiskCyberActivity"
        }
    });

    handle_shared_codex_app_server_message(
        &rerouted,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. })
            if text == "Codex rerouted this turn from `gpt-5.4` to `gpt-5.4-mini` because it detected high-risk cyber activity."
    ));
}

#[test]
fn shared_codex_compaction_notice_inserts_before_visible_assistant_output() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&session_id, "conversation-compact".to_owned())
        .unwrap();
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-compact");

    let assistant_message_id = state.allocate_message_id();
    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: assistant_message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Existing assistant output".to_owned(),
                expanded_text: None,
            },
        )
        .unwrap();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-compact".to_owned()),
                turn_id: Some("turn-compact".to_owned()),
                turn_state: CodexTurnState {
                    assistant_output_started: true,
                    first_visible_assistant_message_id: Some(assistant_message_id.clone()),
                    ..CodexTurnState::default()
                },
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-compact".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let compacted = json!({
        "method": "thread/compacted",
        "params": {
            "threadId": "conversation-compact",
            "turnId": "turn-compact"
        }
    });

    handle_shared_codex_app_server_message(
        &compacted,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    let compact_notice_index = session
        .messages
        .iter()
        .position(|message| {
            matches!(
                message,
                Message::Text { text, .. }
                    if text == "Codex compacted the thread context for this turn."
            )
        })
        .expect("compaction notice should be present");
    let assistant_index = session
        .messages
        .iter()
        .position(|message| {
            matches!(
                message,
                Message::Text { id, text, .. }
                    if id == &assistant_message_id && text == "Existing assistant output"
            )
        })
        .expect("assistant output should remain present");
    assert!(compact_notice_index < assistant_index);
}

#[test]
fn shared_codex_global_notices_update_codex_state() {
    let state = test_app_state();
    let (runtime, _input_rx, _process) = test_shared_codex_runtime("shared-codex-global-notices");
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));

    let config_warning = json!({
        "method": "configWarning",
        "params": {
            "message": "Codex is using fallback sandbox defaults.",
            "code": "sandbox_fallback"
        }
    });
    let deprecation_notice = json!({
        "method": "deprecationNotice",
        "params": {
            "title": "Legacy model alias",
            "detail": "`gpt-4` will be removed soon.",
            "code": "legacy_model_alias"
        }
    });

    handle_shared_codex_app_server_message(
        &config_warning,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &config_warning,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &deprecation_notice,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let codex = state.snapshot().codex;
    assert_eq!(codex.notices.len(), 2);
    assert!(matches!(
        codex.notices.first(),
        Some(CodexNotice {
            kind: CodexNoticeKind::DeprecationNotice,
            level: CodexNoticeLevel::Info,
            title,
            detail,
            code,
            ..
        }) if title == "Legacy model alias"
            && detail == "`gpt-4` will be removed soon."
            && code.as_deref() == Some("legacy_model_alias")
    ));
    assert!(matches!(
        codex.notices.get(1),
        Some(CodexNotice {
            kind: CodexNoticeKind::ConfigWarning,
            level: CodexNoticeLevel::Warning,
            title,
            detail,
            code,
            ..
        }) if title == "Config warning"
            && detail == "Codex is using fallback sandbox defaults."
            && code.as_deref() == Some("sandbox_fallback")
    ));
}

#[test]
fn shared_codex_threadless_runtime_notice_is_recorded() {
    let state = test_app_state();
    let (runtime, _input_rx, _process) = test_shared_codex_runtime("shared-codex-runtime-notice");
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_notice = json!({
        "method": "authRequired",
        "params": {
            "message": "Sign in again before continuing.",
            "code": "auth_required",
            "level": "warning"
        }
    });

    handle_shared_codex_app_server_message(
        &runtime_notice,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let codex = state.snapshot().codex;
    assert!(matches!(
        codex.notices.first(),
        Some(CodexNotice {
            kind: CodexNoticeKind::RuntimeNotice,
            level: CodexNoticeLevel::Warning,
            title,
            detail,
            code,
            ..
        }) if title == "Codex notice: authRequired"
            && detail == "Sign in again before continuing."
            && code.as_deref() == Some("auth_required")
    ));
}

#[test]
fn discover_codex_threads_from_home_reads_latest_database() {
    let codex_home = std::env::temp_dir().join(format!("termal-codex-home-{}", Uuid::new_v4()));
    fs::write(codex_home.join("state.db"), b"").unwrap_or_default();
    write_test_codex_threads_db(
        &codex_home,
        &[(
            "thread-1",
            "/tmp/project",
            "Review local repo",
            r#"{"type":"danger-full-access"}"#,
            "on-request",
            1,
            Some("gpt-5-codex"),
            Some("high"),
            10,
        )],
    );

    let threads = discover_codex_threads_from_home(&codex_home, &[PathBuf::from("/tmp/project")])
        .expect("threads should load");

    assert_eq!(
        threads,
        vec![DiscoveredCodexThread {
            approval_policy: Some(CodexApprovalPolicy::OnRequest),
            archived: true,
            cwd: "/tmp/project".to_owned(),
            id: "thread-1".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            reasoning_effort: Some(CodexReasoningEffort::High),
            sandbox_mode: Some(CodexSandboxMode::DangerFullAccess),
            title: "Review local repo".to_owned(),
        }]
    );

    let _ = fs::remove_dir_all(&codex_home);
}

#[test]
fn discover_codex_threads_from_home_supports_legacy_schema_without_optional_columns() {
    let codex_home =
        std::env::temp_dir().join(format!("termal-codex-home-legacy-{}", Uuid::new_v4()));
    fs::create_dir_all(&codex_home).expect("test Codex home should be created");
    let connection =
        rusqlite::Connection::open(codex_home.join("state_5.sqlite")).expect("db should open");
    connection
        .execute_batch(
            "create table threads (
                id text primary key,
                cwd text not null,
                title text not null,
                sandbox_policy text,
                approval_mode text,
                archived integer not null,
                updated_at integer not null
            );",
        )
        .expect("legacy threads table should be created");
    connection
        .execute(
            "insert into threads (
                id, cwd, title, sandbox_policy, approval_mode, archived, updated_at
            ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                "thread-legacy",
                "/tmp/project",
                "Legacy thread",
                r#"{"type":"workspace-write"}"#,
                "on-request",
                0,
                10,
            ],
        )
        .expect("legacy thread row should insert");

    let threads = discover_codex_threads_from_home(&codex_home, &[PathBuf::from("/tmp/project")])
        .expect("legacy threads should load");

    assert_eq!(
        threads,
        vec![DiscoveredCodexThread {
            approval_policy: Some(CodexApprovalPolicy::OnRequest),
            archived: false,
            cwd: "/tmp/project".to_owned(),
            id: "thread-legacy".to_owned(),
            model: None,
            reasoning_effort: None,
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            title: "Legacy thread".to_owned(),
        }]
    );

    let _ = fs::remove_dir_all(&codex_home);
}

#[test]
fn resolve_codex_threads_database_path_skips_unrelated_entries() {
    let codex_home =
        std::env::temp_dir().join(format!("termal-codex-home-scan-{}", Uuid::new_v4()));
    fs::create_dir_all(&codex_home).expect("test Codex home should be created");
    fs::write(codex_home.join("state_9.sqlite"), b"sqlite").expect("valid state db should exist");
    fs::write(codex_home.join("state_preview.sqlite"), b"broken")
        .expect("unrelated sqlite file should be created");

    let path = resolve_codex_threads_database_path(&codex_home)
        .expect("database discovery should skip unrelated entries");

    assert_eq!(
        path.file_name().and_then(|value| value.to_str()),
        Some("state_9.sqlite")
    );

    let _ = fs::remove_dir_all(&codex_home);
}

#[test]
fn discover_codex_threads_from_sources_skips_repl_home_and_uses_shared_runtime_home() {
    let root = std::env::temp_dir().join(format!("termal-codex-discovery-{}", Uuid::new_v4()));
    let source_home = root.join(".codex");
    let termal_root = root.join(".termal").join("codex-home");
    let shared_home = termal_root.join("shared-app-server");
    let repl_home = termal_root.join("repl");

    write_test_codex_threads_db(
        &shared_home,
        &[(
            "thread-shared",
            "/tmp/project-shared",
            "Shared runtime thread",
            r#"{"type":"workspace-write"}"#,
            "on-request",
            0,
            Some("gpt-5-codex"),
            Some("medium"),
            30,
        )],
    );
    write_test_codex_threads_db(
        &repl_home,
        &[(
            "thread-repl",
            "/tmp/project-repl",
            "REPL thread",
            r#"{"type":"read-only"}"#,
            "never",
            0,
            Some("gpt-5-mini"),
            Some("low"),
            20,
        )],
    );
    write_test_codex_threads_db(
        &source_home,
        &[
            (
                "thread-shared",
                "/tmp/project-source",
                "Older source copy",
                r#"{"type":"danger-full-access"}"#,
                "never",
                1,
                Some("gpt-5"),
                Some("high"),
                10,
            ),
            (
                "thread-source",
                "/tmp/project-source-only",
                "Source-only thread",
                r#"{"type":"workspace-write"}"#,
                "on-failure",
                0,
                Some("gpt-5-codex"),
                Some("minimal"),
                5,
            ),
        ],
    );

    let threads = discover_codex_threads_from_sources(
        Some(&source_home),
        &termal_root,
        &[
            PathBuf::from("/tmp/project-shared"),
            PathBuf::from("/tmp/project-source-only"),
        ],
    )
    .expect("threads should load");

    assert_eq!(
        threads
            .iter()
            .map(|thread| thread.id.as_str())
            .collect::<Vec<_>>(),
        vec!["thread-shared", "thread-source"]
    );
    assert!(matches!(
        threads.first(),
        Some(DiscoveredCodexThread {
            title,
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            ..
        }) if title == "Shared runtime thread"
    ));
    assert!(threads.iter().all(|thread| thread.id != "thread-repl"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn discover_codex_threads_from_home_filters_scopes_before_limiting_results() {
    let codex_home =
        std::env::temp_dir().join(format!("termal-codex-home-large-{}", Uuid::new_v4()));
    fs::create_dir_all(&codex_home).expect("test Codex home should be created");
    let connection =
        rusqlite::Connection::open(codex_home.join("state_5.sqlite")).expect("db should open");
    connection
        .execute_batch(
            "create table threads (
                id text primary key,
                cwd text not null,
                title text not null,
                sandbox_policy text not null,
                approval_mode text not null,
                archived integer not null,
                model text,
                reasoning_effort text,
                updated_at integer not null
            );",
        )
        .expect("threads table should be created");

    for index in 0..101 {
        connection
            .execute(
                "insert into threads (
                    id, cwd, title, sandbox_policy, approval_mode, archived, model, reasoning_effort, updated_at
                ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    format!("thread-other-{index}"),
                    "/tmp/out-of-scope",
                    format!("Out-of-scope thread {index}"),
                    r#"{"type":"workspace-write"}"#,
                    "never",
                    0,
                    "gpt-5-codex",
                    "low",
                    1_000 - index,
                ],
            )
            .expect("thread row should insert");
    }
    connection
        .execute(
            "insert into threads (
                id, cwd, title, sandbox_policy, approval_mode, archived, model, reasoning_effort, updated_at
            ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                "thread-target",
                "/tmp/termal",
                "Older in-scope thread",
                r#"{"type":"danger-full-access"}"#,
                "on-request",
                0,
                "gpt-5-codex",
                "medium",
                1,
            ],
        )
        .expect("target row should insert");

    let threads = discover_codex_threads_from_home(&codex_home, &[PathBuf::from("/tmp/termal")])
        .expect("threads should load");

    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].id, "thread-target");

    let _ = fs::remove_dir_all(&codex_home);
}

#[test]
fn discover_codex_threads_from_home_limits_in_scope_results_per_home() {
    let codex_home =
        std::env::temp_dir().join(format!("termal-codex-home-limited-{}", Uuid::new_v4()));
    fs::create_dir_all(&codex_home).expect("test Codex home should be created");
    let connection =
        rusqlite::Connection::open(codex_home.join("state_7.sqlite")).expect("db should open");
    connection
        .execute_batch(
            "create table threads (
                id text primary key,
                cwd text not null,
                title text not null,
                sandbox_policy text not null,
                approval_mode text not null,
                archived integer not null,
                model text,
                reasoning_effort text,
                updated_at integer not null
            );",
        )
        .expect("threads table should be created");

    for index in 0..(MAX_DISCOVERED_CODEX_THREADS_PER_HOME + 25) {
        connection
            .execute(
                "insert into threads (
                    id, cwd, title, sandbox_policy, approval_mode, archived, model, reasoning_effort, updated_at
                ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    format!("thread-in-scope-{index}"),
                    "/tmp/termal/subdir",
                    format!("In-scope thread {index}"),
                    r#"{"type":"workspace-write"}"#,
                    "on-request",
                    0,
                    "gpt-5-codex",
                    "medium",
                    10_000 - index as i64,
                ],
            )
            .expect("thread row should insert");
    }

    let threads = discover_codex_threads_from_home(&codex_home, &[PathBuf::from("/tmp/termal")])
        .expect("threads should load");
    let last_expected_id = format!(
        "thread-in-scope-{}",
        MAX_DISCOVERED_CODEX_THREADS_PER_HOME - 1
    );

    assert_eq!(threads.len(), MAX_DISCOVERED_CODEX_THREADS_PER_HOME);
    assert_eq!(
        threads.first().map(|thread| thread.id.as_str()),
        Some("thread-in-scope-0")
    );
    assert_eq!(
        threads.last().map(|thread| thread.id.as_str()),
        Some(last_expected_id.as_str()),
    );

    let _ = fs::remove_dir_all(&codex_home);
}

#[test]
fn import_discovered_codex_threads_adds_project_scoped_sessions_without_duplicates() {
    let mut inner = StateInner::new();
    let project = inner.create_project(
        Some("TermAl".to_owned()),
        "/tmp/termal".to_owned(),
        default_local_remote_id(),
    );
    inner.create_session(
        Agent::Codex,
        Some("Codex Live".to_owned()),
        "/tmp/termal".to_owned(),
        Some(project.id.clone()),
        None,
    );

    let discovered = vec![
        DiscoveredCodexThread {
            approval_policy: Some(CodexApprovalPolicy::Never),
            archived: true,
            cwd: "/tmp/termal".to_owned(),
            id: "thread-local".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            reasoning_effort: Some(CodexReasoningEffort::Low),
            sandbox_mode: Some(CodexSandboxMode::DangerFullAccess),
            title: "Read bugs".to_owned(),
        },
        DiscoveredCodexThread {
            approval_policy: Some(CodexApprovalPolicy::Never),
            archived: false,
            cwd: "/tmp/elsewhere".to_owned(),
            id: "thread-other".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            reasoning_effort: None,
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            title: "Ignore me".to_owned(),
        },
    ];

    inner.import_discovered_codex_threads("/tmp/termal", discovered.clone());
    inner.import_discovered_codex_threads("/tmp/termal", discovered);

    let discovered_session = inner
        .sessions
        .iter()
        .find(|record| record.external_session_id.as_deref() == Some("thread-local"))
        .expect("project-scoped discovered thread should be imported");
    assert_eq!(discovered_session.session.agent, Agent::Codex);
    assert_eq!(discovered_session.session.workdir, "/tmp/termal");
    assert_eq!(
        discovered_session.session.project_id.as_deref(),
        Some(project.id.as_str())
    );
    assert_eq!(discovered_session.session.model, "gpt-5-codex");
    assert_eq!(
        discovered_session.session.codex_thread_state,
        Some(CodexThreadState::Archived)
    );
    assert_eq!(
        discovered_session.session.preview,
        "Archived Codex thread ready to reopen."
    );
    assert_eq!(
        discovered_session.session.reasoning_effort,
        Some(CodexReasoningEffort::Low)
    );
    assert_eq!(
        discovered_session.session.sandbox_mode,
        Some(CodexSandboxMode::DangerFullAccess)
    );
    assert_eq!(
        discovered_session.session.approval_policy,
        Some(CodexApprovalPolicy::Never)
    );
    assert_eq!(
        inner
            .sessions
            .iter()
            .filter(|record| record.external_session_id.as_deref() == Some("thread-local"))
            .count(),
        1
    );
    assert!(
        inner
            .sessions
            .iter()
            .all(|record| record.external_session_id.as_deref() != Some("thread-other"))
    );
}

#[test]
fn import_discovered_codex_threads_preserves_existing_prompt_settings() {
    let mut inner = StateInner::new();
    let project = inner.create_project(
        Some("TermAl".to_owned()),
        "/tmp/termal".to_owned(),
        default_local_remote_id(),
    );
    let mut record = inner.create_session(
        Agent::Codex,
        Some("Codex Live".to_owned()),
        "/tmp/termal".to_owned(),
        Some(project.id.clone()),
        Some("gpt-5-mini".to_owned()),
    );
    record.codex_sandbox_mode = CodexSandboxMode::ReadOnly;
    record.session.sandbox_mode = Some(CodexSandboxMode::ReadOnly);
    record.codex_approval_policy = CodexApprovalPolicy::OnFailure;
    record.session.approval_policy = Some(CodexApprovalPolicy::OnFailure);
    record.codex_reasoning_effort = CodexReasoningEffort::Minimal;
    record.session.reasoning_effort = Some(CodexReasoningEffort::Minimal);
    set_record_external_session_id(&mut record, Some("thread-existing".to_owned()));
    if let Some(slot) = inner
        .find_session_index(&record.session.id)
        .and_then(|index| inner.sessions.get_mut(index))
    {
        *slot = record;
    }

    inner.import_discovered_codex_threads(
        "/tmp/termal",
        vec![DiscoveredCodexThread {
            approval_policy: Some(CodexApprovalPolicy::Never),
            archived: true,
            cwd: "/tmp/termal".to_owned(),
            id: "thread-existing".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            reasoning_effort: Some(CodexReasoningEffort::High),
            sandbox_mode: Some(CodexSandboxMode::DangerFullAccess),
            title: "Existing thread".to_owned(),
        }],
    );

    let record = inner
        .sessions
        .iter()
        .find(|entry| entry.external_session_id.as_deref() == Some("thread-existing"))
        .expect("existing discovered thread should still be present");
    assert_eq!(record.session.model, "gpt-5-mini");
    assert_eq!(
        record.session.sandbox_mode,
        Some(CodexSandboxMode::ReadOnly)
    );
    assert_eq!(
        record.session.approval_policy,
        Some(CodexApprovalPolicy::OnFailure)
    );
    assert_eq!(
        record.session.reasoning_effort,
        Some(CodexReasoningEffort::Minimal)
    );
    assert_eq!(
        record.session.codex_thread_state,
        Some(CodexThreadState::Archived)
    );
}

#[tokio::test]
async fn codex_thread_action_routes_update_session_state() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&session_id, "thread-live".to_owned())
        .unwrap();
    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "stale local message".to_owned(),
                expanded_text: None,
            },
        )
        .unwrap();
    let (runtime, input_rx, _process) = test_shared_codex_runtime("shared-codex-route-actions");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);

    std::thread::spawn(move || {
        for expected_method in ["thread/archive", "thread/unarchive", "thread/rollback"] {
            let command = input_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("Codex thread action should arrive");
            match command {
                CodexRuntimeCommand::JsonRpcRequest {
                    method,
                    params,
                    response_tx,
                    ..
                } => {
                    assert_eq!(method, expected_method);
                    assert_eq!(params["threadId"], "thread-live");
                    if method == "thread/rollback" {
                        assert_eq!(params["numTurns"], 2);
                        let _ = response_tx.send(Ok(json!({
                            "thread": {
                                "preview": "Rolled back preview",
                                "turns": [
                                    {
                                        "id": "turn-rollback",
                                        "status": "completed",
                                        "items": [
                                            {
                                                "id": "rollback-user",
                                                "type": "userMessage",
                                                "content": [
                                                    {
                                                        "type": "text",
                                                        "text": "Current diff state"
                                                    }
                                                ]
                                            },
                                            {
                                                "id": "rollback-agent",
                                                "type": "agentMessage",
                                                "text": "Rollback synced."
                                            }
                                        ]
                                    }
                                ]
                            }
                        })));
                        continue;
                    }
                    let _ = response_tx.send(Ok(json!({})));
                }
                _ => panic!("expected shared Codex JSON-RPC request"),
            }
        }
    });

    let app = app_router(state);
    let (archive_status, archive_response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/codex/thread/archive"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(archive_status, StatusCode::OK);
    let archived_session = archive_response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(
        archived_session.codex_thread_state,
        Some(CodexThreadState::Archived)
    );

    let (unarchive_status, unarchive_response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/codex/thread/unarchive"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(unarchive_status, StatusCode::OK);
    let restored_session = unarchive_response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(
        restored_session.codex_thread_state,
        Some(CodexThreadState::Active)
    );

    let (rollback_status, rollback_response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/codex/thread/rollback"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"numTurns":2}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(rollback_status, StatusCode::OK);
    let rollback_session = rollback_response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(matches!(
        rollback_session.messages.first(),
        Some(Message::Text { author: Author::You, text, .. }) if text == "Current diff state"
    ));
    assert!(matches!(
        rollback_session.messages.get(1),
        Some(Message::Text { author: Author::Assistant, text, .. }) if text == "Rollback synced."
    ));
    assert!(!rollback_session.messages.iter().any(
        |message| matches!(message, Message::Markdown { title, .. }
            if title == "Archived Codex thread"
                || title == "Restored Codex thread"
                || title == "Rolled back Codex thread")
    ));
    assert!(!rollback_session.messages.iter().any(
        |message| matches!(message, Message::Text { text, .. } if text == "stale local message")
    ));
}

#[tokio::test]
async fn codex_thread_rollback_route_falls_back_when_history_is_unavailable() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&session_id, "thread-live".to_owned())
        .unwrap();
    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "local history".to_owned(),
                expanded_text: None,
            },
        )
        .unwrap();
    let (runtime, input_rx, _process) =
        test_shared_codex_runtime("shared-codex-route-rollback-fallback");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);

    std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Codex rollback command should arrive");
        match command {
            CodexRuntimeCommand::JsonRpcRequest {
                method,
                params,
                response_tx,
                ..
            } => {
                assert_eq!(method, "thread/rollback");
                assert_eq!(params["threadId"], "thread-live");
                assert_eq!(params["numTurns"], 1);
                let _ = response_tx.send(Ok(json!({
                    "thread": {
                        "preview": "Fallback preview"
                    }
                })));
            }
            _ => panic!("expected shared Codex JSON-RPC request"),
        }
    });

    let app = app_router(state);
    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/codex/thread/rollback"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"numTurns":1}"#))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "local history"
    ));
    assert!(matches!(
        session.messages.last(),
        Some(Message::Markdown { title, markdown, .. })
            if title == "Rolled back Codex thread"
                && markdown.contains("Codex did not return the updated thread history")
    ));
}

#[tokio::test]
async fn codex_thread_fork_route_returns_created_response() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Route Review".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5.4".to_owned()),
            approval_policy: Some(CodexApprovalPolicy::Never),
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    state
        .set_external_session_id(&created.session_id, "thread-origin".to_owned())
        .unwrap();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&created.session_id)
            .expect("source Codex session should exist");
        inner.sessions[index].session.model_options =
            vec![SessionModelOption::plain("gpt-5.4", "gpt-5.4")];
    }

    let (runtime, input_rx, _process) = test_shared_codex_runtime("shared-codex-route-fork");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);

    std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Codex fork command should arrive");
        match command {
            CodexRuntimeCommand::JsonRpcRequest {
                method,
                params,
                response_tx,
                ..
            } => {
                assert_eq!(method, "thread/fork");
                assert_eq!(params["threadId"], "thread-origin");
                let _ = response_tx.send(Ok(json!({
                    "thread": {
                        "id": "thread-forked",
                        "name": "Forked Review",
                        "preview": "Forked preview",
                        "turns": [
                            {
                                "id": "turn-forked",
                                "status": "completed",
                                "items": [
                                    {
                                        "id": "fork-user",
                                        "type": "userMessage",
                                        "content": [
                                            {
                                                "type": "text",
                                                "text": "Fork context"
                                            }
                                        ]
                                    },
                                    {
                                        "id": "fork-agent",
                                        "type": "agentMessage",
                                        "text": "Ready to continue."
                                    }
                                ]
                            }
                        ]
                    },
                    "model": "gpt-5.5",
                    "approvalPolicy": "on-request",
                    "sandbox": {
                        "type": "workspaceWrite"
                    },
                    "reasoningEffort": "high",
                    "cwd": "/tmp/forked",
                })));
            }
            _ => panic!("expected shared Codex JSON-RPC request"),
        }
    });

    let app = app_router(state);
    let (status, response): (StatusCode, CreateSessionResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{}/codex/thread/fork",
                created.session_id
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    let forked_session = response
        .state
        .sessions
        .iter()
        .find(|session| session.id == response.session_id)
        .expect("forked session should be present");
    assert_eq!(
        forked_session.codex_thread_state,
        Some(CodexThreadState::Active)
    );
    assert_eq!(
        forked_session.external_session_id.as_deref(),
        Some("thread-forked")
    );
    assert!(matches!(
        forked_session.messages.first(),
        Some(Message::Text { author: Author::You, text, .. }) if text == "Fork context"
    ));
    assert!(matches!(
        forked_session.messages.get(1),
        Some(Message::Text { author: Author::Assistant, text, .. }) if text == "Ready to continue."
    ));
}

#[test]
fn shared_codex_task_complete_event_buffers_subagent_result_until_final_agent_message() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-task-complete");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-sub-1"
            }
        }
    });
    let task_complete = json!({
        "method": "codex/event/task_complete",
        "params": {
            "conversationId": "conversation-123",
            "msg": {
                "last_agent_message": "Reviewer found a real bug.",
                "turn_id": "turn-sub-1",
                "type": "task_complete"
            }
        }
    });
    let final_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-sub-1",
            "msg": {
                "message": "Final shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &task_complete,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &final_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.preview, "Final shared Codex answer.");
    assert_eq!(session.messages.len(), 2);
    assert!(matches!(
        session.messages.first(),
        Some(Message::SubagentResult {
            title,
            summary,
            conversation_id,
            turn_id,
            ..
        }) if title == "Subagent completed"
            && summary == "Reviewer found a real bug."
            && conversation_id.as_deref() == Some("conversation-123")
            && turn_id.as_deref() == Some("turn-sub-1")
    ));
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Final shared Codex answer."
    ));
}

#[test]
fn shared_codex_agent_message_event_without_turn_id_uses_active_turn() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-agent-final-no-turn-id");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-no-id"
            }
        }
    });
    let final_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "msg": {
                "message": "Final shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &final_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.preview, "Final shared Codex answer.");
    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Final shared Codex answer."
    ));
}

#[test]
fn shared_codex_agent_message_event_ignores_stale_turn_id_from_params_id() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-agent-final-stale-params-id");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started_previous = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stale"
            }
        }
    });
    let turn_started_current = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-current"
            }
        }
    });
    let stale_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-stale",
            "msg": {
                "message": "Stale shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });
    let current_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-current",
            "msg": {
                "message": "Current shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started_previous,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_started_current,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &stale_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &current_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.preview, "Current shared Codex answer.");
    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Current shared Codex answer."
    ));
}

#[test]
fn shared_codex_task_complete_event_stays_in_current_turn_after_prior_assistant_message() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: "assistant-previous".to_owned(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Previous shared Codex answer.".to_owned(),
                expanded_text: None,
            },
        )
        .unwrap();
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-task-complete-order");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-sub-2"
            }
        }
    });
    let task_complete = json!({
        "method": "codex/event/task_complete",
        "params": {
            "conversationId": "conversation-123",
            "msg": {
                "last_agent_message": "Reviewer found a real bug.",
                "turn_id": "turn-sub-2",
                "type": "task_complete"
            }
        }
    });
    let final_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-sub-2",
            "msg": {
                "message": "Current shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &task_complete,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(matches!(
        session.messages.as_slice(),
        [Message::Text { text, .. }] if text == "Previous shared Codex answer."
    ));

    handle_shared_codex_app_server_message(
        &final_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.messages.len(), 3);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Previous shared Codex answer."
    ));
    assert!(matches!(
        session.messages.get(1),
        Some(Message::SubagentResult {
            title,
            summary,
            conversation_id,
            turn_id,
            ..
        }) if title == "Subagent completed"
            && summary == "Reviewer found a real bug."
            && conversation_id.as_deref() == Some("conversation-123")
            && turn_id.as_deref() == Some("turn-sub-2")
    ));
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Current shared Codex answer."
    ));
}

#[test]
fn shared_codex_task_complete_event_without_active_turn_is_ignored() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-task-complete-no-active-turn");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Idle;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let task_complete = json!({
        "method": "codex/event/task_complete",
        "params": {
            "conversationId": "conversation-123",
            "msg": {
                "last_agent_message": "Stale hidden summary.",
                "type": "task_complete"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &task_complete,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert!(session.messages.is_empty());
}

#[test]
fn shared_codex_task_complete_event_after_streaming_output_inserts_before_answer() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-task-complete-late");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-sub-3"
            }
        }
    });
    let delta_message = json!({
        "method": "codex/event/agent_message_content_delta",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-sub-3",
            "msg": {
                "delta": "Current shared Codex answer.",
                "item_id": "msg-123",
                "thread_id": "conversation-123",
                "turn_id": "turn-sub-3",
                "type": "agent_message_content_delta"
            }
        }
    });
    let task_complete = json!({
        "method": "codex/event/task_complete",
        "params": {
            "conversationId": "conversation-123",
            "msg": {
                "last_agent_message": "Reviewer found a real bug.",
                "turn_id": "turn-sub-3",
                "type": "task_complete"
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-sub-3",
                "error": null
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &delta_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &task_complete,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_completed,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.preview, "Current shared Codex answer.");
    assert_eq!(session.messages.len(), 2);
    assert!(matches!(
        session.messages.first(),
        Some(Message::SubagentResult {
            title,
            summary,
            conversation_id,
            turn_id,
            ..
        }) if title == "Subagent completed"
            && summary == "Reviewer found a real bug."
            && conversation_id.as_deref() == Some("conversation-123")
            && turn_id.as_deref() == Some("turn-sub-3")
    ));
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Current shared Codex answer."
    ));
}

#[test]
fn shared_codex_task_complete_event_ignores_stale_summary_from_previous_turn() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-task-complete-stale");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started_previous = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stale"
            }
        }
    });
    let turn_started_current = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-current"
            }
        }
    });
    let stale_task_complete = json!({
        "method": "codex/event/task_complete",
        "params": {
            "conversationId": "conversation-123",
            "msg": {
                "last_agent_message": "Stale hidden summary.",
                "turn_id": "turn-stale",
                "type": "task_complete"
            }
        }
    });
    let final_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-current",
            "msg": {
                "message": "Current shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started_previous,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_started_current,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &stale_task_complete,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &final_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.preview, "Current shared Codex answer.");
    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Current shared Codex answer."
    ));
}

#[test]
fn shared_codex_task_complete_event_drops_buffered_summary_on_failed_turn() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-task-complete-error");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-sub-4"
            }
        }
    });
    let task_complete = json!({
        "method": "codex/event/task_complete",
        "params": {
            "conversationId": "conversation-123",
            "msg": {
                "last_agent_message": "Reviewer found a real bug.",
                "turn_id": "turn-sub-4",
                "type": "task_complete"
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-sub-4",
                "error": {
                    "message": "stream failed"
                }
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &task_complete,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_completed,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.status, SessionStatus::Error);
    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Turn failed: stream failed"
    ));
}

#[test]
fn shared_codex_turn_completed_flushes_buffered_subagent_results_after_output_started() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-turn-completed-flushes-buffer");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                turn_id: Some("turn-sub-5".to_owned()),
                turn_state: CodexTurnState {
                    assistant_output_started: true,
                    pending_subagent_results: vec![PendingSubagentResult {
                        title: "Subagent completed".to_owned(),
                        summary: "Buffered reviewer summary.".to_owned(),
                        conversation_id: Some("conversation-123".to_owned()),
                        turn_id: Some("turn-sub-5".to_owned()),
                    }],
                    ..CodexTurnState::default()
                },
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-sub-5",
                "error": null
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_completed,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::SubagentResult {
            title,
            summary,
            conversation_id,
            turn_id,
            ..
        }) if title == "Subagent completed"
            && summary == "Buffered reviewer summary."
            && conversation_id.as_deref() == Some("conversation-123")
            && turn_id.as_deref() == Some("turn-sub-5")
    ));
}

#[test]
fn shared_codex_item_completed_event_records_agent_message() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-item-completed");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-123"
            }
        }
    });
    let message = json!({
        "method": "codex/event/item_completed",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-123",
            "msg": {
                "item": {
                    "content": [
                        {
                            "text": "Hello.",
                            "type": "Text"
                        }
                    ],
                    "id": "msg-123",
                    "phase": "final_answer",
                    "type": "AgentMessage"
                },
                "thread_id": "conversation-123",
                "turn_id": "turn-123",
                "type": "item_completed"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Hello."
    ));
}

#[test]
fn shared_codex_item_completed_event_ignores_stale_turn_id_from_params_id() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-item-completed-stale-params-id");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started_previous = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stale"
            }
        }
    });
    let turn_started_current = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-current"
            }
        }
    });
    let stale_message = json!({
        "method": "codex/event/item_completed",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-stale",
            "msg": {
                "item": {
                    "content": [
                        {
                            "text": "Stale shared Codex answer.",
                            "type": "Text"
                        }
                    ],
                    "id": "msg-stale",
                    "phase": "final_answer",
                    "type": "AgentMessage"
                },
                "thread_id": "conversation-123",
                "type": "item_completed"
            }
        }
    });
    let current_message = json!({
        "method": "codex/event/item_completed",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-current",
            "msg": {
                "item": {
                    "content": [
                        {
                            "text": "Current shared Codex answer.",
                            "type": "Text"
                        }
                    ],
                    "id": "msg-current",
                    "phase": "final_answer",
                    "type": "AgentMessage"
                },
                "thread_id": "conversation-123",
                "type": "item_completed"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started_previous,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_started_current,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &stale_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &current_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.preview, "Current shared Codex answer.");
    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Current shared Codex answer."
    ));
}

#[test]
fn shared_codex_item_completed_event_concatenates_multipart_agent_message() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-item-completed-multipart");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-123"
            }
        }
    });
    let message = json!({
        "method": "codex/event/item_completed",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-123",
            "msg": {
                "item": {
                    "content": [
                        {
                            "text": "Hello",
                            "type": "Text"
                        },
                        {
                            "metadata": {
                                "ignored": true
                            },
                            "type": "Reasoning"
                        },
                        {
                            "text": ", world.",
                            "type": "Text"
                        }
                    ],
                    "id": "msg-123",
                    "phase": "final_answer",
                    "type": "AgentMessage"
                },
                "thread_id": "conversation-123",
                "turn_id": "turn-123",
                "type": "item_completed"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Hello, world."
    ));
}

#[test]
fn shared_codex_agent_message_content_delta_event_ignores_stale_turn_id_from_params_id() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-agent-delta-stale-params-id");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started_previous = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stale"
            }
        }
    });
    let turn_started_current = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-current"
            }
        }
    });
    let stale_delta_message = json!({
        "method": "codex/event/agent_message_content_delta",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-stale",
            "msg": {
                "delta": "Stale shared Codex answer.",
                "item_id": "msg-stale",
                "thread_id": "conversation-123",
                "type": "agent_message_content_delta"
            }
        }
    });
    let current_delta_message = json!({
        "method": "codex/event/agent_message_content_delta",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-current",
            "msg": {
                "delta": "Current shared Codex answer.",
                "item_id": "msg-current",
                "thread_id": "conversation-123",
                "type": "agent_message_content_delta"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started_previous,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_started_current,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &stale_delta_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &current_delta_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.preview, "Current shared Codex answer.");
    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Current shared Codex answer."
    ));
}

#[test]
fn shared_codex_agent_message_final_event_appends_missing_suffix_after_streamed_delta() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-agent-final-suffix");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-123"
            }
        }
    });
    let delta_message = json!({
        "method": "codex/event/agent_message_content_delta",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-123",
            "msg": {
                "delta": "Hello",
                "item_id": "msg-123",
                "thread_id": "conversation-123",
                "turn_id": "turn-123",
                "type": "agent_message_content_delta"
            }
        }
    });
    let final_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-123",
            "msg": {
                "message": "Hello there.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &delta_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &final_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Hello there."
    ));
}

#[test]
fn shared_codex_agent_message_content_delta_streams_without_duplicate_final_message() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-agent-delta");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-123"
            }
        }
    });
    let app_server_delta_message = json!({
        "method": "item/agentMessage/delta",
        "params": {
            "delta": "Hello.",
            "itemId": "msg-123"
        }
    });
    let delta_message = json!({
        "method": "codex/event/agent_message_content_delta",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-123",
            "msg": {
                "delta": "Hello.",
                "item_id": "msg-123",
                "thread_id": "conversation-123",
                "turn_id": "turn-123",
                "type": "agent_message_content_delta"
            }
        }
    });
    let final_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-123",
            "msg": {
                "message": "Hello.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &app_server_delta_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &delta_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &final_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Hello."
    ));
}

#[test]
fn shared_codex_agent_message_event_after_turn_completed_is_ignored() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-agent-final-after-turn-completed");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-finished"
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-finished",
                "error": null
            }
        }
    });
    let late_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-finished",
            "msg": {
                "message": "Late shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_completed,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &late_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert!(session.messages.is_empty());
}

#[test]
fn codex_app_server_command_approval_request_records_pending_approval() {
    let mut recorder = TestRecorder::default();
    let message = json!({
        "id": "req-1",
        "params": {
            "command": "cargo test",
            "cwd": "/tmp/project",
            "reason": "Need to verify the fix."
        }
    });

    handle_codex_app_server_request(
        "item/commandExecution/requestApproval",
        &message,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(recorder.codex_approvals.len(), 1);
    assert!(matches!(
        recorder.codex_approvals.first(),
        Some((title, command, detail, approval))
            if title == "Codex needs approval"
                && command == "cargo test"
                && detail == "Codex requested approval to execute this command in /tmp/project. Reason: Need to verify the fix."
                && matches!(approval.kind, CodexApprovalKind::CommandExecution)
                && approval.request_id == json!("req-1")
    ));
}

#[test]
fn codex_app_server_file_change_approval_request_records_pending_approval() {
    let mut recorder = TestRecorder::default();
    let message = json!({
        "id": "req-2",
        "params": {
            "reason": "Need to update generated files."
        }
    });

    handle_codex_app_server_request("item/fileChange/requestApproval", &message, &mut recorder)
        .unwrap();

    assert_eq!(recorder.codex_approvals.len(), 1);
    assert!(matches!(
        recorder.codex_approvals.first(),
        Some((title, command, detail, approval))
            if title == "Codex needs approval"
                && command == "Apply file changes"
                && detail == "Codex requested approval to apply file changes. Reason: Need to update generated files."
                && matches!(approval.kind, CodexApprovalKind::FileChange)
                && approval.request_id == json!("req-2")
    ));
}

#[test]
fn codex_app_server_permissions_approval_request_records_pending_approval() {
    let mut recorder = TestRecorder::default();
    let requested_permissions = json!({
        "fileSystem": {
            "read": ["/repo/docs"],
            "write": ["/repo/src"]
        },
        "network": {
            "enabled": true
        },
        "macos": {
            "preferences": "system",
            "automations": {
                "bundle_ids": ["com.apple.Terminal"]
            }
        }
    });
    let message = json!({
        "id": "req-3",
        "params": {
            "permissions": requested_permissions,
            "reason": "Need access to update build scripts."
        }
    });

    handle_codex_app_server_request("item/permissions/requestApproval", &message, &mut recorder)
        .unwrap();

    assert_eq!(recorder.codex_approvals.len(), 1);
    let (title, command, detail, approval) = recorder
        .codex_approvals
        .first()
        .expect("Codex permissions approval should be recorded");
    assert_eq!(title, "Codex needs approval");
    assert_eq!(command, "Grant additional permissions");
    assert_eq!(
        detail,
        "Codex requested approval to grant additional permissions: read access to `/repo/docs`, write access to `/repo/src`, network access, macOS preferences access (system), macOS automation access for `com.apple.Terminal`. Reason: Need access to update build scripts."
    );
    match &approval.kind {
        CodexApprovalKind::Permissions {
            requested_permissions,
        } => {
            assert_eq!(
                requested_permissions,
                &json!({
                    "fileSystem": {
                        "read": ["/repo/docs"],
                        "write": ["/repo/src"]
                    },
                    "network": {
                        "enabled": true
                    },
                    "macos": {
                        "preferences": "system",
                        "automations": {
                            "bundle_ids": ["com.apple.Terminal"]
                        }
                    }
                })
            );
        }
        _ => panic!("expected Codex permissions approval"),
    }
    assert_eq!(approval.request_id, json!("req-3"));
}

#[test]
fn codex_app_server_user_input_request_records_pending_request() {
    let mut recorder = TestRecorder::default();
    let message = json!({
        "id": "req-input-1",
        "params": {
            "questions": [
                {
                    "header": "Environment",
                    "id": "environment",
                    "question": "Which environment should I use?",
                    "options": [
                        {
                            "label": "Production",
                            "description": "Use the production cluster."
                        },
                        {
                            "label": "Staging",
                            "description": "Use the staging environment."
                        }
                    ]
                },
                {
                    "header": "API token",
                    "id": "apiToken",
                    "question": "Paste the temporary token.",
                    "isSecret": true
                }
            ]
        }
    });

    handle_codex_app_server_request("item/tool/requestUserInput", &message, &mut recorder).unwrap();

    assert_eq!(recorder.codex_user_input_requests.len(), 1);
    let (title, detail, questions, request) = recorder
        .codex_user_input_requests
        .first()
        .expect("Codex user input request should be recorded");
    assert_eq!(title, "Codex needs input");
    assert_eq!(detail, "Codex requested additional input for 2 questions.");
    assert_eq!(questions.len(), 2);
    assert_eq!(questions[0].header, "Environment");
    assert_eq!(questions[1].id, "apiToken");
    assert!(questions[1].is_secret);
    assert_eq!(request.request_id, json!("req-input-1"));
    assert_eq!(request.questions, questions.clone());
}

#[test]
fn codex_app_server_mcp_elicitation_request_records_pending_request() {
    let mut recorder = TestRecorder::default();
    let message = json!({
        "id": "req-elicit-1",
        "params": {
            "threadId": "thread-1",
            "turnId": "turn-1",
            "serverName": "deployment-helper",
            "mode": "form",
            "message": "Confirm the deployment settings.",
            "requestedSchema": {
                "type": "object",
                "properties": {
                    "environment": {
                        "type": "string",
                        "title": "Environment",
                        "oneOf": [
                            { "const": "production", "title": "Production" },
                            { "const": "staging", "title": "Staging" }
                        ]
                    },
                    "replicas": {
                        "type": "integer",
                        "title": "Replicas"
                    }
                },
                "required": ["environment", "replicas"]
            }
        }
    });

    handle_codex_app_server_request("mcpServer/elicitation/request", &message, &mut recorder)
        .unwrap();

    assert_eq!(recorder.codex_mcp_elicitation_requests.len(), 1);
    let (title, detail, request, pending) = recorder
        .codex_mcp_elicitation_requests
        .first()
        .expect("MCP elicitation request should be recorded");
    assert_eq!(title, "Codex needs MCP input");
    assert_eq!(
        detail,
        "MCP server deployment-helper requested additional structured input. Confirm the deployment settings."
    );
    assert_eq!(request.server_name, "deployment-helper");
    assert_eq!(request.thread_id, "thread-1");
    assert_eq!(request.turn_id.as_deref(), Some("turn-1"));
    assert!(matches!(
        request.mode,
        McpElicitationRequestMode::Form { .. }
    ));
    assert_eq!(pending.request_id, json!("req-elicit-1"));
    assert_eq!(pending.request, *request);
}

#[test]
fn codex_app_server_generic_request_records_pending_request() {
    let mut recorder = TestRecorder::default();
    let message = json!({
        "id": "req-tool-1",
        "params": {
            "toolName": "search_workspace",
            "arguments": {
                "pattern": "Codex"
            }
        }
    });

    handle_codex_app_server_request("item/tool/call", &message, &mut recorder).unwrap();

    assert_eq!(recorder.codex_app_requests.len(), 1);
    let (title, detail, method, params, pending) = recorder
        .codex_app_requests
        .first()
        .expect("generic Codex app request should be recorded");
    assert_eq!(title, "Codex needs a tool result");
    assert_eq!(
        detail,
        "Codex requested a result for `search_workspace`. Review the request payload and submit the JSON result to continue."
    );
    assert_eq!(method, "item/tool/call");
    assert_eq!(
        params,
        &json!({
            "toolName": "search_workspace",
            "arguments": {
                "pattern": "Codex"
            }
        })
    );
    assert_eq!(pending.request_id, json!("req-tool-1"));
}

#[test]
fn repl_codex_task_complete_event_buffers_subagent_result_until_final_message() {
    let mut recorder = TestRecorder::default();
    let mut repl_state = ReplCodexSessionState::default();
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-sub-1"
            }
        }
    });
    let task_complete = json!({
        "method": "codex/event/task_complete",
        "params": {
            "conversationId": "conversation-123",
            "msg": {
                "last_agent_message": "Reviewer found a real bug.",
                "turn_id": "turn-sub-1",
                "type": "task_complete"
            }
        }
    });
    let final_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-sub-1",
            "msg": {
                "message": "Final REPL Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_repl_codex_app_server_notification(
        "turn/started",
        &turn_started,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "codex/event/task_complete",
        &task_complete,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();

    assert!(recorder.subagent_results.is_empty());
    assert!(recorder.texts.is_empty());

    handle_repl_codex_app_server_notification(
        "codex/event/agent_message",
        &final_message,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(
        recorder.subagent_results,
        vec![(
            "Subagent completed".to_owned(),
            "Reviewer found a real bug.".to_owned(),
        )]
    );
    assert_eq!(
        recorder.texts,
        vec![
            "Subagent completed\nReviewer found a real bug.".to_owned(),
            "Final REPL Codex answer.".to_owned(),
        ]
    );
}

#[test]
fn repl_codex_streamed_agent_message_appends_missing_completed_suffix() {
    let mut recorder = TestRecorder::default();
    let mut repl_state = ReplCodexSessionState::default();
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stream-1"
            }
        }
    });
    let delta = json!({
        "method": "item/agentMessage/delta",
        "params": {
            "threadId": "conversation-123",
            "itemId": "item-1",
            "delta": "Hello"
        }
    });
    let completed = json!({
        "method": "item/completed",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "id": "item-1",
                "type": "agentMessage",
                "text": "Hello from REPL."
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stream-1",
                "error": null
            }
        }
    });

    handle_repl_codex_app_server_notification(
        "turn/started",
        &turn_started,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "item/agentMessage/delta",
        &delta,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "item/completed",
        &completed,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "turn/completed",
        &turn_completed,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(recorder.text_deltas, vec!["Hello".to_owned(), " from REPL.".to_owned()]);
    assert!(recorder.texts.is_empty());
    assert!(repl_state.turn_completed);
    assert!(repl_state.current_turn_id.is_none());
}

#[test]
fn repl_codex_streamed_agent_message_skips_duplicate_completed_text() {
    let mut recorder = TestRecorder::default();
    let mut repl_state = ReplCodexSessionState::default();
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stream-1"
            }
        }
    });
    let delta = json!({
        "method": "item/agentMessage/delta",
        "params": {
            "threadId": "conversation-123",
            "itemId": "item-1",
            "delta": "Hello from REPL."
        }
    });
    let completed = json!({
        "method": "item/completed",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "id": "item-1",
                "type": "agentMessage",
                "text": "Hello from REPL."
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stream-1",
                "error": null
            }
        }
    });

    handle_repl_codex_app_server_notification(
        "turn/started",
        &turn_started,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "item/agentMessage/delta",
        &delta,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "item/completed",
        &completed,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "turn/completed",
        &turn_completed,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(recorder.text_deltas, vec!["Hello from REPL.".to_owned()]);
    assert!(recorder.texts.is_empty());
    assert!(repl_state.turn_completed);
    assert!(repl_state.current_turn_id.is_none());
}

#[test]
fn codex_app_server_web_search_item_records_command_lifecycle() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let mut recorder = TestRecorder::default();
    let mut turn_state = CodexTurnState::default();
    let item = json!({
        "id": "web-1",
        "type": "webSearch",
        "query": "rust anyhow",
        "action": {
            "type": "search",
            "queries": ["rust anyhow", "serde_json value"]
        }
    });

    handle_codex_app_server_item_started(&item, &mut recorder).unwrap();
    handle_codex_app_server_item_completed(
        &item,
        &state,
        &session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(
        recorder.commands,
        vec![
            (
                "Web search: rust anyhow".to_owned(),
                String::new(),
                CommandStatus::Running,
            ),
            (
                "Web search: rust anyhow".to_owned(),
                "rust anyhow\nserde_json value".to_owned(),
                CommandStatus::Success,
            ),
        ]
    );
}

#[test]
fn codex_app_server_file_change_item_records_create_and_edit_diffs() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let mut recorder = TestRecorder::default();
    let mut turn_state = CodexTurnState::default();
    let item = json!({
        "type": "fileChange",
        "status": "completed",
        "changes": [
            {
                "path": "src/new.rs",
                "diff": "+fn main() {}\n",
                "kind": {
                    "type": "add"
                }
            },
            {
                "path": "src/lib.rs",
                "diff": "@@ -1 +1 @@\n-old\n+new\n",
                "kind": {
                    "type": "edit"
                }
            }
        ]
    });

    handle_codex_app_server_item_completed(
        &item,
        &state,
        &session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(
        recorder.diffs,
        vec![
            (
                "src/new.rs".to_owned(),
                "Created new.rs".to_owned(),
                "+fn main() {}\n".to_owned(),
                ChangeType::Create,
            ),
            (
                "src/lib.rs".to_owned(),
                "Updated lib.rs".to_owned(),
                "@@ -1 +1 @@\n-old\n+new\n".to_owned(),
                ChangeType::Edit,
            ),
        ]
    );
}

#[test]
fn codex_delta_suffix_deduplicates_cumulative_and_overlapping_chunks() {
    let mut text = String::new();

    assert_eq!(
        next_codex_delta_suffix(&mut text, "Try"),
        Some("Try".to_owned())
    );
    assert_eq!(text, "Try");

    assert_eq!(
        next_codex_delta_suffix(&mut text, "Try these"),
        Some(" these".to_owned())
    );
    assert_eq!(text, "Try these");

    assert_eq!(next_codex_delta_suffix(&mut text, "Try these"), None);
    assert_eq!(text, "Try these");

    assert_eq!(
        next_codex_delta_suffix(&mut text, " these plain"),
        Some(" plain".to_owned())
    );
    assert_eq!(text, "Try these plain");

    assert_eq!(next_codex_delta_suffix(&mut text, " plain"), None);
    assert_eq!(text, "Try these plain");
}

#[test]
fn codex_delta_suffix_handles_multibyte_utf8_characters() {
    let mut text = String::new();

    // Smart quote ' is 3 bytes (U+2018: E2 80 98)
    assert_eq!(
        next_codex_delta_suffix(&mut text, "I\u{2018}m"),
        Some("I\u{2018}m".to_owned())
    );
    assert_eq!(text, "I\u{2018}m");

    // Overlapping chunk that shares the multi-byte char boundary
    assert_eq!(
        next_codex_delta_suffix(&mut text, "\u{2018}m here"),
        Some(" here".to_owned())
    );
    assert_eq!(text, "I\u{2018}m here");
}

#[test]
fn shared_codex_agent_message_event_uses_conversation_id_for_session_routing() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-agent-final");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-123"
            }
        }
    });
    let message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-123",
            "msg": {
                "message": "Final shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Final shared Codex answer."
    ));
}

#[test]
fn subagent_results_append_after_existing_assistant_text() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);

    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: "assistant-1".to_owned(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Final answer".to_owned(),
                expanded_text: None,
            },
        )
        .unwrap();
    state
        .push_message(
            &session_id,
            Message::SubagentResult {
                id: "subagent-1".to_owned(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Subagent completed".to_owned(),
                summary: "Hidden thinking".to_owned(),
                conversation_id: None,
                turn_id: None,
            },
        )
        .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should be present");

    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Final answer"
    ));
    assert!(matches!(
        session.messages.last(),
        Some(Message::SubagentResult { .. })
    ));
}

#[test]
fn reuses_shared_codex_runtime_across_sessions() {
    let state = test_app_state();
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime.clone());

    let first = spawn_codex_runtime(state.clone(), "session-a".to_owned(), "/tmp".to_owned())
        .expect("first Codex handle should attach");
    let second = spawn_codex_runtime(state.clone(), "session-b".to_owned(), "/tmp".to_owned())
        .expect("second Codex handle should attach");

    assert_eq!(first.runtime_id, "shared-codex");
    assert_eq!(second.runtime_id, "shared-codex");
    assert!(Arc::ptr_eq(&first.process, &process));
    assert!(Arc::ptr_eq(&second.process, &process));
    let shared_sessions = runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned");
    assert!(shared_sessions.contains_key("session-a"));
    assert!(shared_sessions.contains_key("session-b"));
}

#[test]
fn stops_shared_codex_sessions_via_turn_interrupt() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) = test_shared_codex_runtime("shared-codex-stop");
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-123".to_owned()),
                turn_id: Some("turn-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("thread-123".to_owned(), session_id.clone());

    let handle = CodexRuntimeHandle {
        runtime_id: runtime.runtime_id.clone(),
        input_tx: runtime.input_tx.clone(),
        process,
        shared_session: Some(SharedCodexSessionHandle {
            runtime: runtime.clone(),
            session_id: session_id.clone(),
        }),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(handle);
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Codex interrupt command should arrive");
        match command {
            CodexRuntimeCommand::InterruptTurn {
                thread_id,
                turn_id,
                response_tx,
            } => {
                assert_eq!(thread_id, "thread-123");
                assert_eq!(turn_id, "turn-123");
                let _ = response_tx.send(Ok(()));
            }
            _ => panic!("expected Codex turn interrupt command"),
        }
    });

    let snapshot = state.stop_session(&session_id).unwrap();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("stopped session should remain present");
    assert_eq!(session.status, SessionStatus::Idle);
    assert!(session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn stopped by user."
    )));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert!(matches!(record.runtime, SessionRuntime::None));
    drop(inner);

    let shared_sessions = runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned");
    assert!(!shared_sessions.contains_key(&session_id));
    drop(shared_sessions);
    assert!(
        !runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .contains_key("thread-123")
    );
}

#[test]
fn syncs_cursor_model_options_from_acp_config() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor ACP".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Agent),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let model_options = vec![
        SessionModelOption::plain("Auto", "auto"),
        SessionModelOption::plain("GPT-5.3 Codex", "gpt-5.3-codex"),
    ];
    state
        .sync_session_model_options(
            &created.session_id,
            Some("gpt-5.3-codex".to_owned()),
            model_options.clone(),
        )
        .unwrap();

    let inner = state.inner.lock().expect("state mutex poisoned");
    let session = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .map(|record| &record.session)
        .expect("Cursor session should exist");
    assert_eq!(session.model, "gpt-5.3-codex");
    assert_eq!(session.model_options, model_options);
}

#[test]
fn cursor_agent_mode_auto_approves_acp_permission_requests() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Agent".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Agent),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let (input_tx, input_rx) = mpsc::channel();
    let mut recorder = SessionRecorder::new(state.clone(), created.session_id.clone());

    handle_acp_request(
        &cursor_permission_request("cursor-agent-approval"),
        &state,
        &created.session_id,
        &input_tx,
        &mut recorder,
        AcpAgent::Cursor,
    )
    .unwrap();

    match input_rx
        .recv_timeout(Duration::from_millis(100))
        .expect("Cursor agent mode should auto-respond")
    {
        AcpRuntimeCommand::JsonRpcMessage(message) => {
            assert_eq!(message.get("id"), Some(&json!("cursor-agent-approval")));
            assert_eq!(
                message.pointer("/result/outcome/optionId"),
                Some(&json!("allow-once"))
            );
        }
        _ => panic!("expected automatic Cursor approval response"),
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Cursor session should exist");
    assert!(record.pending_acp_approvals.is_empty());
    assert!(record.session.messages.is_empty());
    assert_eq!(record.session.status, SessionStatus::Idle);
}

#[test]
fn cursor_ask_mode_queues_acp_permission_requests() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Ask".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Ask),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let (input_tx, input_rx) = mpsc::channel();
    let mut recorder = SessionRecorder::new(state.clone(), created.session_id.clone());

    handle_acp_request(
        &cursor_permission_request("cursor-ask-approval"),
        &state,
        &created.session_id,
        &input_tx,
        &mut recorder,
        AcpAgent::Cursor,
    )
    .unwrap();

    assert!(matches!(
        input_rx.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Cursor session should exist");
    assert_eq!(record.pending_acp_approvals.len(), 1);
    assert!(matches!(
        record.session.messages.first(),
        Some(Message::Approval {
            title,
            command,
            decision,
            ..
        }) if title == "Cursor needs approval"
            && command == "Edit src/main.rs"
            && *decision == ApprovalDecision::Pending
    ));
    assert_eq!(record.session.status, SessionStatus::Approval);
}

#[test]
fn cursor_plan_mode_rejects_acp_permission_requests() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Plan".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Plan),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let (input_tx, input_rx) = mpsc::channel();
    let mut recorder = SessionRecorder::new(state.clone(), created.session_id.clone());

    handle_acp_request(
        &cursor_permission_request("cursor-plan-approval"),
        &state,
        &created.session_id,
        &input_tx,
        &mut recorder,
        AcpAgent::Cursor,
    )
    .unwrap();

    match input_rx
        .recv_timeout(Duration::from_millis(100))
        .expect("Cursor plan mode should auto-reject")
    {
        AcpRuntimeCommand::JsonRpcMessage(message) => {
            assert_eq!(message.get("id"), Some(&json!("cursor-plan-approval")));
            assert_eq!(
                message.pointer("/result/outcome/optionId"),
                Some(&json!("reject-once"))
            );
        }
        _ => panic!("expected automatic Cursor rejection response"),
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Cursor session should exist");
    assert!(record.pending_acp_approvals.is_empty());
    assert!(record.session.messages.is_empty());
    assert_eq!(record.session.status, SessionStatus::Idle);
}

#[test]
fn syncs_cursor_mode_from_acp_config_updates() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Config Sync".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Agent),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let mut recorder = SessionRecorder::new(state.clone(), created.session_id.clone());
    let mut turn_state = AcpTurnState::default();

    handle_acp_session_update(
        &json!({
            "sessionUpdate": "config_update",
            "configOptions": [
                {
                    "id": "model",
                    "currentValue": "auto",
                    "options": [{ "value": "auto", "name": "Auto" }]
                },
                {
                    "id": "mode",
                    "currentValue": "ask",
                    "options": [
                        { "value": "agent" },
                        { "value": "ask" },
                        { "value": "plan" }
                    ]
                }
            ]
        }),
        &state,
        &created.session_id,
        &mut turn_state,
        &mut recorder,
        AcpAgent::Cursor,
    )
    .unwrap();

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Cursor session should exist");
    assert_eq!(record.session.cursor_mode, Some(CursorMode::Ask));
}

#[test]
fn syncs_cursor_mode_from_mode_updates() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Mode Sync".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Ask),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let mut recorder = SessionRecorder::new(state.clone(), created.session_id.clone());
    let mut turn_state = AcpTurnState::default();

    handle_acp_session_update(
        &json!({
            "sessionUpdate": "mode_update",
            "mode": "plan"
        }),
        &state,
        &created.session_id,
        &mut turn_state,
        &mut recorder,
        AcpAgent::Cursor,
    )
    .unwrap();

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Cursor session should exist");
    assert_eq!(record.session.cursor_mode, Some(CursorMode::Plan));
}

#[test]
fn borrowed_session_recorder_uses_shared_message_and_request_logic() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let questions = vec![UserInputQuestion {
        header: "Scope".to_owned(),
        id: "scope".to_owned(),
        is_other: false,
        is_secret: false,
        options: None,
        question: "What should Codex review?".to_owned(),
    }];
    let mut recorder_state = SessionRecorderState::default();
    let mut recorder = BorrowedSessionRecorder::new(&state, &session_id, &mut recorder_state);

    recorder.push_text("Initial text").unwrap();
    recorder.text_delta("streamed text").unwrap();
    recorder.finish_streaming_text().unwrap();
    recorder.command_started("cmd-1", "pwd").unwrap();
    recorder
        .command_completed("cmd-1", "pwd", "/tmp", CommandStatus::Success)
        .unwrap();
    recorder
        .push_codex_user_input_request(
            "Need input",
            "Choose the review scope.",
            questions.clone(),
            CodexPendingUserInput {
                questions: questions.clone(),
                request_id: json!("request-1"),
            },
        )
        .unwrap();

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");

    assert!(record.session.messages.iter().any(|message| {
        matches!(message, Message::Text { text, .. } if text == "Initial text")
    }));
    assert!(record.session.messages.iter().any(|message| {
        matches!(message, Message::Text { text, .. } if text == "streamed text")
    }));
    assert!(record.session.messages.iter().any(|message| {
        matches!(
            message,
            Message::Command {
                command,
                output,
                status,
                ..
            } if command == "pwd" && output == "/tmp" && *status == CommandStatus::Success
        )
    }));
    assert!(record.session.messages.iter().any(|message| {
        matches!(
            message,
            Message::UserInputRequest {
                title,
                detail,
                questions: message_questions,
                state,
                ..
            } if title == "Need input"
                && detail == "Choose the review scope."
                && message_questions == &questions
                && *state == InteractionRequestState::Pending
        )
    }));
    assert_eq!(record.pending_codex_user_inputs.len(), 1);
}

#[test]
fn updates_live_cursor_mode_on_active_acp_sessions() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Live Mode".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Agent),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let (runtime, input_rx) = test_acp_runtime_handle(AcpAgent::Cursor, "cursor-live-mode");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&created.session_id)
            .expect("Cursor session should exist");
        inner.sessions[index].runtime = SessionRuntime::Acp(runtime);
        inner.sessions[index].external_session_id = Some("cursor-session-1".to_owned());
    }

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: None,
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: Some(CursorMode::Ask),
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Cursor session should be present");
    assert_eq!(session.cursor_mode, Some(CursorMode::Ask));

    match input_rx
        .recv_timeout(Duration::from_millis(100))
        .expect("Cursor mode change should be forwarded to the live ACP session")
    {
        AcpRuntimeCommand::JsonRpcMessage(message) => {
            assert_eq!(
                message.get("method").and_then(Value::as_str),
                Some("session/set_config_option")
            );
            assert_eq!(
                message.pointer("/params/sessionId"),
                Some(&json!("cursor-session-1"))
            );
            assert_eq!(message.pointer("/params/optionId"), Some(&json!("mode")));
            assert_eq!(message.pointer("/params/value"), Some(&json!("ask")));
        }
        _ => panic!("expected live Cursor mode update request"),
    }
}

#[test]
fn matches_acp_model_options_by_name_or_label() {
    let config = json!({
        "configOptions": [
            {
                "id": "model",
                "options": [
                    {
                        "value": "auto",
                        "name": "Auto"
                    },
                    {
                        "value": "gpt-5.3-codex-high-fast",
                        "label": "GPT-5.3 Codex High Fast"
                    }
                ]
            }
        ]
    });

    assert_eq!(
        matching_acp_config_option_value(&config, "model", "Auto"),
        Some("auto".to_owned())
    );
    assert_eq!(
        matching_acp_config_option_value(&config, "model", "GPT-5.3 Codex High Fast"),
        Some("gpt-5.3-codex-high-fast".to_owned())
    );
}

#[test]
fn canonicalizes_session_model_updates_from_live_model_labels() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Canonical".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5.4".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    state
        .sync_session_model_options(
            &created.session_id,
            None,
            vec![SessionModelOption::plain("GPT-5.4", "gpt-5.4")],
        )
        .unwrap();

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: Some("GPT-5.4".to_owned()),
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Codex session should be present");
    assert_eq!(session.model, "gpt-5.4");
}

#[test]
fn revisions_increase_for_visible_state_changes() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Revision Test".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    assert_eq!(created.state.revision, 1);

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: None,
                sandbox_mode: Some(CodexSandboxMode::ReadOnly),
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            },
        )
        .unwrap();
    assert_eq!(updated.revision, 2);
}

#[test]
fn renames_sessions_via_settings_updates() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Old Name".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: Some("New Name".to_owned()),
                model: None,
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            },
        )
        .unwrap();

    let renamed = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("renamed session should be present");
    assert_eq!(renamed.name, "New Name");
}

#[test]
fn persists_remote_settings() {
    let state = test_app_state();

    let updated = state
        .update_app_settings(UpdateAppSettingsRequest {
            default_codex_reasoning_effort: None,
            default_claude_effort: None,
            remotes: Some(vec![
                RemoteConfig::local(),
                RemoteConfig {
                    id: "ssh-lab".to_owned(),
                    name: "SSH Lab".to_owned(),
                    transport: RemoteTransport::Ssh,
                    enabled: true,
                    host: Some("example.com".to_owned()),
                    port: Some(2222),
                    user: Some("alice".to_owned()),
                },
            ]),
        })
        .unwrap();

    assert_eq!(updated.preferences.remotes.len(), 2);
    assert_eq!(updated.preferences.remotes[1].id, "ssh-lab");
    assert_eq!(
        updated.preferences.remotes[1].transport,
        RemoteTransport::Ssh
    );

    let reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    assert_eq!(
        reloaded_inner.preferences.remotes,
        updated.preferences.remotes
    );
}

#[test]
fn rejects_remote_settings_with_unsafe_remote_id() {
    let state = test_app_state();

    let error = match state.update_app_settings(UpdateAppSettingsRequest {
        default_codex_reasoning_effort: None,
        default_claude_effort: None,
        remotes: Some(vec![
            RemoteConfig::local(),
            RemoteConfig {
                id: "ssh/lab".to_owned(),
                name: "SSH Lab".to_owned(),
                transport: RemoteTransport::Ssh,
                enabled: true,
                host: Some("example.com".to_owned()),
                port: Some(22),
                user: Some("alice".to_owned()),
            },
        ]),
    }) {
        Ok(_) => panic!("unsafe remote id should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error.message,
        "remote id `ssh/lab` contains unsupported characters"
    );
}

#[test]
fn rejects_remote_settings_with_invalid_ssh_host() {
    let state = test_app_state();

    let error = match state.update_app_settings(UpdateAppSettingsRequest {
        default_codex_reasoning_effort: None,
        default_claude_effort: None,
        remotes: Some(vec![
            RemoteConfig::local(),
            RemoteConfig {
                id: "ssh-lab".to_owned(),
                name: "SSH Lab".to_owned(),
                transport: RemoteTransport::Ssh,
                enabled: true,
                host: Some("-oProxyCommand=touch/tmp/pwned".to_owned()),
                port: Some(22),
                user: Some("alice".to_owned()),
            },
        ]),
    }) {
        Ok(_) => panic!("host injection should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, "remote `SSH Lab` has an invalid SSH host");
}

#[test]
fn rejects_remote_settings_with_invalid_ssh_user() {
    let state = test_app_state();

    let error = match state.update_app_settings(UpdateAppSettingsRequest {
        default_codex_reasoning_effort: None,
        default_claude_effort: None,
        remotes: Some(vec![
            RemoteConfig::local(),
            RemoteConfig {
                id: "ssh-lab".to_owned(),
                name: "SSH Lab".to_owned(),
                transport: RemoteTransport::Ssh,
                enabled: true,
                host: Some("example.com".to_owned()),
                port: Some(22),
                user: Some("alice@example.com".to_owned()),
            },
        ]),
    }) {
        Ok(_) => panic!("invalid SSH user should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, "remote `SSH Lab` has an invalid SSH user");
}

#[test]
fn remote_connection_issue_message_hides_transport_details() {
    assert_eq!(
        remote_connection_issue_message("SSH Lab"),
        "Could not connect to remote \"SSH Lab\" over SSH. Check the host, network, and SSH settings, then try again."
    );
}

#[test]
fn local_ssh_start_issue_message_hides_transport_details() {
    assert_eq!(
        local_ssh_start_issue_message("SSH Lab"),
        "Could not start the local SSH client for remote \"SSH Lab\". Verify OpenSSH is installed and available on PATH, then try again."
    );
}

#[test]
fn remote_ssh_command_args_insert_double_dash_before_target() {
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(2222),
        user: Some("alice".to_owned()),
    };

    let args = remote_ssh_command_args(&remote, 47001, RemoteProcessMode::ManagedServer)
        .expect("SSH args should build");

    let separator_index = args
        .iter()
        .position(|arg| arg == "--")
        .expect("SSH args should include `--` before the target");
    assert_eq!(args[separator_index + 1], "alice@example.com");
    assert_eq!(&args[separator_index + 2..], ["termal", "server"]);
}

#[test]
fn removing_remote_stops_event_bridge_worker_and_resets_started_guard() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let connection = Arc::new(RemoteConnection {
        config: Mutex::new(remote.clone()),
        forwarded_port: 47001,
        process: Mutex::new(None),
        event_bridge_started: AtomicBool::new(false),
        event_bridge_shutdown: AtomicBool::new(false),
    });
    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(remote.id.clone(), connection.clone());

    connection.start_event_bridge(state.remote_registry.client.client().clone(), state.clone());
    assert!(connection.event_bridge_started.load(Ordering::SeqCst));

    state.remote_registry.reconcile(&[RemoteConfig::local()]);

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        if !connection.event_bridge_started.load(Ordering::SeqCst) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "event bridge worker should stop after the remote is removed"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    assert!(
        !state
            .remote_registry
            .connections
            .lock()
            .expect("remote registry mutex poisoned")
            .contains_key(&remote.id)
    );

    connection.start_event_bridge(state.remote_registry.client.client().clone(), state.clone());
    assert!(connection.event_bridge_started.load(Ordering::SeqCst));

    connection.stop_event_bridge();

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        if !connection.event_bridge_started.load(Ordering::SeqCst) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "event bridge started guard should reset after shutdown"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn remote_snapshot_sync_removes_missing_proxy_sessions() {
    let state = test_app_state();
    let (kept_local_session_id, removed_local_session_id, local_session_id) = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let kept = inner.create_session(Agent::Codex, None, "/tmp".to_owned(), None, None);
        let removed = inner.create_session(Agent::Codex, None, "/tmp".to_owned(), None, None);
        let local = inner.create_session(Agent::Claude, None, "/tmp".to_owned(), None, None);

        let kept_index = inner
            .find_session_index(&kept.session.id)
            .expect("kept session should exist");
        inner.sessions[kept_index].remote_id = Some("ssh-lab".to_owned());
        inner.sessions[kept_index].remote_session_id = Some("remote-session-keep".to_owned());

        let removed_index = inner
            .find_session_index(&removed.session.id)
            .expect("removed session should exist");
        inner.sessions[removed_index].remote_id = Some("ssh-lab".to_owned());
        inner.sessions[removed_index].remote_session_id = Some("remote-session-gone".to_owned());

        (kept.session.id, removed.session.id, local.session.id)
    };

    let mut remote_state = state.snapshot();
    let mut remote_session = remote_state
        .sessions
        .iter()
        .find(|session| session.id == kept_local_session_id)
        .cloned()
        .expect("kept session should be present in the snapshot");
    remote_session.id = "remote-session-keep".to_owned();
    remote_session.preview = "Remote session still exists.".to_owned();
    remote_state.sessions = vec![remote_session];

    state
        .apply_remote_state_snapshot("ssh-lab", remote_state)
        .expect("remote snapshot should apply");

    let snapshot = state.snapshot();
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.id == kept_local_session_id)
    );
    assert!(
        !snapshot
            .sessions
            .iter()
            .any(|session| session.id == removed_local_session_id)
    );
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.id == local_session_id)
    );
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.id == kept_local_session_id)
            .expect("kept session should remain")
            .preview,
        "Remote session still exists."
    );
}

#[test]
fn remote_review_put_sends_scope_via_query_params() {
    let captured = Arc::new(Mutex::new(None::<(String, String)>));
    let captured_for_server = captured.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("connection should arrive");
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                let bytes_read = stream.read(&mut chunk).expect("request should read");
                assert!(bytes_read > 0, "request closed before headers completed");
                buffer.extend_from_slice(&chunk[..bytes_read]);
                if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end;
                }
            };
            let headers = String::from_utf8_lossy(&buffer[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then_some(value.trim())
                        .and_then(|value| value.parse::<usize>().ok())
                })
                .unwrap_or(0);
            let body_start = header_end + 4;
            while buffer.len() < body_start + content_length {
                let bytes_read = stream.read(&mut chunk).expect("request body should read");
                if bytes_read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..bytes_read]);
            }

            let request_head = String::from_utf8_lossy(&buffer[..body_start]).to_string();
            let request_line = request_head
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();
            let body = String::from_utf8_lossy(&buffer[body_start..body_start + content_length])
                .to_string();

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 11\r\n\r\n{\"ok\":true}",
                    )
                    .expect("health response should write");
                continue;
            }

            if request_line.starts_with("PUT /api/reviews/change-set-1?") {
                *captured_for_server.lock().expect("capture mutex poisoned") =
                    Some((request_line.clone(), body));
                let response = serde_json::to_string(&ReviewDocumentResponse {
                    review_file_path: "/remote/.termal/reviews/change-set-1.json".to_owned(),
                    review: ReviewDocument {
                        version: 1,
                        change_set_id: "change-set-1".to_owned(),
                        revision: 0,
                        origin: None,
                        files: Vec::new(),
                        threads: Vec::new(),
                    },
                })
                .expect("review response should encode");
                let response_bytes = response.as_bytes();
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            response_bytes.len(),
                            response
                        )
                        .as_bytes(),
                    )
                    .expect("review response should write");
                continue;
            }

            panic!("unexpected request: {request_line}");
        }
    });

    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(false),
                event_bridge_shutdown: AtomicBool::new(false),
            }),
        );

    let response: ReviewDocumentResponse = state
        .remote_put_json_with_query_scope(
            &RemoteScope {
                remote,
                remote_project_id: None,
                remote_session_id: Some("remote-session-1".to_owned()),
            },
            "/api/reviews/change-set-1",
            Vec::new(),
            json!({
                "version": 1,
                "changeSetId": "change-set-1",
                "revision": 0,
                "threads": [],
            }),
        )
        .expect("remote review PUT should succeed");

    assert_eq!(
        response.review_file_path,
        "/remote/.termal/reviews/change-set-1.json"
    );
    let (request_line, body) = captured
        .lock()
        .expect("capture mutex poisoned")
        .clone()
        .expect("captured request should exist");
    assert!(request_line.contains("sessionId=remote-session-1"));
    assert!(!request_line.contains("projectId="));
    let parsed_body: Value = serde_json::from_str(&body).expect("review body should decode");
    assert_eq!(parsed_body.get("sessionId"), None);
    assert_eq!(parsed_body.get("projectId"), None);

    server.join().expect("test server should finish");
}

#[test]
fn normalize_git_repo_relative_path_rejects_parent_traversal_components() {
    let error = normalize_git_repo_relative_path("../../etc/passwd")
        .expect_err("parent traversal should be rejected");

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error.message,
        "git file path cannot contain parent-directory traversal"
    );
}

#[test]
fn rejects_projects_with_unknown_remote() {
    let state = test_app_state();

    let error = match state.create_project(CreateProjectRequest {
        name: Some("Remote Project".to_owned()),
        root_path: "/tmp".to_owned(),
        remote_id: "missing-remote".to_owned(),
    }) {
        Ok(_) => panic!("project creation should reject unknown remotes"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("unknown remote"));
}

#[test]
#[ignore = "requires a reachable SSH remote"]
fn creates_sessions_for_remote_projects_over_ssh() {
    let state = test_app_state();

    state
        .update_app_settings(UpdateAppSettingsRequest {
            default_codex_reasoning_effort: None,
            default_claude_effort: None,
            remotes: Some(vec![
                RemoteConfig::local(),
                RemoteConfig {
                    id: "ssh-lab".to_owned(),
                    name: "SSH Lab".to_owned(),
                    transport: RemoteTransport::Ssh,
                    enabled: true,
                    host: Some("example.com".to_owned()),
                    port: Some(22),
                    user: Some("alice".to_owned()),
                },
            ]),
        })
        .unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Remote Project".to_owned()),
            root_path: "/workspace/demo".to_owned(),
            remote_id: "ssh-lab".to_owned(),
        })
        .unwrap();

    let stored_project = project
        .state
        .projects
        .iter()
        .find(|entry| entry.id == project.project_id)
        .expect("created project should be present");
    assert_eq!(stored_project.remote_id, "ssh-lab");

    let error = match state.create_session(CreateSessionRequest {
        agent: Some(Agent::Codex),
        name: Some("Remote Session".to_owned()),
        workdir: None,
        project_id: Some(project.project_id),
        model: None,
        approval_policy: None,
        reasoning_effort: None,
        sandbox_mode: None,
        cursor_mode: None,
        claude_approval_mode: None,
        claude_effort: None,
        gemini_approval_mode: None,
    }) {
        Ok(_) => {
            panic!(
                "remote session creation should require a reachable SSH remote in this integration test"
            )
        }
        Err(error) => error,
    };

    assert!(matches!(
        error.status,
        StatusCode::BAD_GATEWAY | StatusCode::BAD_REQUEST
    ));
    assert!(!error.message.trim().is_empty());
}

#[test]
fn creates_projects_and_assigns_sessions_to_them() {
    let state = test_app_state();
    let expected_root = resolve_project_root_path("/tmp").unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: None,
            root_path: "/tmp".to_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    assert_eq!(project.state.projects.len(), 1);

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Project Session".to_owned()),
            workdir: None,
            project_id: Some(project.project_id.clone()),
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let session = created
        .state
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("created session should be present");

    assert_eq!(
        session.project_id.as_deref(),
        Some(project.project_id.as_str())
    );
    assert_eq!(session.workdir, expected_root);
}

#[test]
fn rejects_session_workdirs_outside_the_selected_project() {
    let state = test_app_state();
    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Project".to_owned()),
            root_path: "/tmp".to_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();

    let result = state.create_session(CreateSessionRequest {
        agent: Some(Agent::Codex),
        name: Some("Out of Bounds".to_owned()),
        workdir: Some("/Users".to_owned()),
        project_id: Some(project.project_id),
        model: None,
        approval_policy: None,
        reasoning_effort: None,
        sandbox_mode: None,
        cursor_mode: None,
        claude_approval_mode: None,
        claude_effort: None,
        gemini_approval_mode: None,
    });

    let error = match result {
        Ok(_) => panic!("session workdir outside project should fail"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("must stay inside project"));
}

#[test]
fn rejects_empty_project_roots() {
    let state = test_app_state();

    let result = state.create_project(CreateProjectRequest {
        name: None,
        root_path: "   ".to_owned(),
        remote_id: default_local_remote_id(),
    });
    let error = match result {
        Ok(_) => panic!("empty project path should fail"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, "project root path cannot be empty");
}

#[test]
fn resolves_requested_paths_inside_the_session_project_root() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-project-scope-{}", Uuid::new_v4()));
    let inside_dir = root.join("src");
    let inside_file = inside_dir.join("main.rs");
    let outside_root =
        std::env::temp_dir().join(format!("termal-project-scope-outside-{}", Uuid::new_v4()));
    let outside_file = outside_root.join("main.rs");

    fs::create_dir_all(&inside_dir).unwrap();
    fs::create_dir_all(&outside_root).unwrap();
    fs::write(&inside_file, "fn main() {}\n").unwrap();
    fs::write(&outside_file, "fn main() {}\n").unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Scoped Project".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Scoped Session".to_owned()),
            workdir: Some(inside_dir.to_string_lossy().into_owned()),
            project_id: Some(project.project_id),
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let resolved = resolve_session_scoped_requested_path(
        &state,
        &created.session_id,
        &inside_file.to_string_lossy(),
        ScopedPathMode::ExistingFile,
    )
    .unwrap();
    assert_eq!(
        resolved,
        normalize_user_facing_path(&fs::canonicalize(&inside_file).unwrap())
    );

    let error = resolve_session_scoped_requested_path(
        &state,
        &created.session_id,
        &outside_file.to_string_lossy(),
        ScopedPathMode::ExistingFile,
    )
    .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("must stay inside project"));

    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(outside_root).unwrap();
}

#[test]
fn allows_new_file_paths_inside_the_session_project_root() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-project-write-scope-{}", Uuid::new_v4()));
    let inside_dir = root.join("src");
    let new_file = root.join("generated").join("output.rs");
    let outside_root =
        std::env::temp_dir().join(format!("termal-project-write-outside-{}", Uuid::new_v4()));
    let outside_file = outside_root.join("escape.rs");

    fs::create_dir_all(&inside_dir).unwrap();
    fs::create_dir_all(&outside_root).unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Writable Project".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Writable Session".to_owned()),
            workdir: Some(inside_dir.to_string_lossy().into_owned()),
            project_id: Some(project.project_id),
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let resolved = resolve_session_scoped_requested_path(
        &state,
        &created.session_id,
        &new_file.to_string_lossy(),
        ScopedPathMode::AllowMissingLeaf,
    )
    .unwrap();
    assert_eq!(resolved, new_file);

    let error = resolve_session_scoped_requested_path(
        &state,
        &created.session_id,
        &outside_file.to_string_lossy(),
        ScopedPathMode::AllowMissingLeaf,
    )
    .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("must stay inside project"));

    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(outside_root).unwrap();
}

#[test]
fn resolves_project_scoped_paths_without_a_session() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-project-scope-only-{}", Uuid::new_v4()));
    let inside_dir = root.join("src");
    let inside_file = inside_dir.join("main.rs");
    let outside_root = std::env::temp_dir().join(format!(
        "termal-project-scope-only-outside-{}",
        Uuid::new_v4()
    ));
    let outside_file = outside_root.join("escape.rs");

    fs::create_dir_all(&inside_dir).unwrap();
    fs::create_dir_all(&outside_root).unwrap();
    fs::write(&inside_file, "fn main() {}\n").unwrap();
    fs::write(&outside_file, "fn main() {}\n").unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Scope Only Project".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();

    let resolved = resolve_project_scoped_requested_path(
        &state,
        None,
        Some(&project.project_id),
        &inside_file.to_string_lossy(),
        ScopedPathMode::ExistingFile,
    )
    .unwrap();
    assert_eq!(
        resolved,
        normalize_user_facing_path(&fs::canonicalize(&inside_file).unwrap())
    );

    let error = resolve_project_scoped_requested_path(
        &state,
        None,
        Some(&project.project_id),
        &outside_file.to_string_lossy(),
        ScopedPathMode::ExistingFile,
    )
    .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("must stay inside project"));

    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(outside_root).unwrap();
}

#[test]
fn parses_quoted_git_status_paths() {
    assert_eq!(
        parse_git_status_paths(r#""folder/file with spaces.txt""#),
        (None, "folder/file with spaces.txt".to_owned())
    );
    assert_eq!(
        parse_git_status_paths(r#""caf\303\251.txt""#),
        (None, "caf\u{00e9}.txt".to_owned())
    );
    assert_eq!(
        parse_git_status_paths(r#""old name.txt" -> "new name.txt""#),
        (Some("old name.txt".to_owned()), "new name.txt".to_owned(),)
    );
}

#[test]
fn git_status_file_actions_support_paths_with_spaces() {
    let repo_root = std::env::temp_dir().join(format!("termal-git-status-{}", Uuid::new_v4()));
    let nested_dir = repo_root.join("folder");
    let tracked_file = repo_root.join("README.md");
    let spaced_file = nested_dir.join("file with spaces.txt");

    fs::create_dir_all(&nested_dir).unwrap();
    fs::write(&tracked_file, "# Test\n").unwrap();

    run_git_test_command(&repo_root, &["init"]);
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);
    run_git_test_command(&repo_root, &["add", "README.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);

    fs::write(&spaced_file, "hello\n").unwrap();

    let status = load_git_status_for_path(&repo_root).unwrap();
    let file = status
        .files
        .iter()
        .find(|entry| entry.path == "folder/file with spaces.txt")
        .expect("status should include the untracked file");

    assert_eq!(file.index_status.as_deref(), Some("?"));
    assert_eq!(file.worktree_status.as_deref(), Some("?"));

    let pathspecs = collect_git_pathspecs(&file.path, None);
    run_git_pathspec_command(
        &repo_root,
        &["add", "-A"],
        &pathspecs,
        "failed to stage git changes",
    )
    .unwrap();

    let staged_status = load_git_status_for_path(&repo_root).unwrap();
    let staged_file = staged_status
        .files
        .iter()
        .find(|entry| entry.path == "folder/file with spaces.txt")
        .expect("status should include the staged file");

    assert_eq!(staged_file.index_status.as_deref(), Some("A"));
    assert_eq!(staged_file.worktree_status, None);

    run_git_pathspec_command(
        &repo_root,
        &["restore", "--staged"],
        &pathspecs,
        "failed to unstage git changes",
    )
    .unwrap();

    let unstaged_status = load_git_status_for_path(&repo_root).unwrap();
    let unstaged_file = unstaged_status
        .files
        .iter()
        .find(|entry| entry.path == "folder/file with spaces.txt")
        .expect("status should include the unstaged file");

    assert_eq!(unstaged_file.index_status.as_deref(), Some("?"));
    assert_eq!(unstaged_file.worktree_status.as_deref(), Some("?"));

    fs::remove_dir_all(repo_root).unwrap();
}

#[test]
fn project_scoped_paths_require_a_session_or_project_identifier() {
    let state = test_app_state();
    let error = resolve_project_scoped_requested_path(
        &state,
        None,
        None,
        "/tmp",
        ScopedPathMode::ExistingPath,
    )
    .unwrap_err();

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, "sessionId or projectId is required");
}

#[tokio::test]
async fn read_directory_accepts_project_id_without_session() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-project-fs-read-{}", Uuid::new_v4()));
    let src_dir = root.join("src");
    let file_path = src_dir.join("main.rs");

    fs::create_dir_all(&src_dir).unwrap();
    fs::write(
        &file_path,
        "fn main() {}
",
    )
    .unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Project Files".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();

    let Json(response) = read_directory(
        State(state),
        Query(FileQuery {
            path: root.to_string_lossy().into_owned(),
            project_id: Some(project.project_id),
            session_id: None,
        }),
    )
    .await
    .unwrap();

    assert_eq!(
        response.path,
        normalize_user_facing_path(&fs::canonicalize(&root).unwrap()).to_string_lossy()
    );
    assert_eq!(response.entries.len(), 1);
    assert_eq!(response.entries[0].name, "src");

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn read_and_write_file_accept_project_id_without_session() {
    let state = test_app_state();
    let root =
        std::env::temp_dir().join(format!("termal-project-file-read-write-{}", Uuid::new_v4()));
    let existing_file = root.join("src").join("main.rs");
    let new_file = root.join("generated").join("output.rs");

    fs::create_dir_all(existing_file.parent().unwrap()).unwrap();
    fs::write(
        &existing_file,
        "fn main() {}
",
    )
    .unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Project Files".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();

    let Json(read_response) = read_file(
        State(state.clone()),
        Query(FileQuery {
            path: existing_file.to_string_lossy().into_owned(),
            project_id: Some(project.project_id.clone()),
            session_id: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        read_response.content,
        "fn main() {}
"
    );

    let Json(write_response) = write_file(
        State(state),
        Json(WriteFileRequest {
            path: new_file.to_string_lossy().into_owned(),
            content: "pub fn generated() {}
"
            .to_owned(),
            project_id: Some(project.project_id),
            session_id: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(write_response.path, new_file.to_string_lossy());
    assert_eq!(
        fs::read_to_string(&new_file).unwrap(),
        "pub fn generated() {}
"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn project_digest_surfaces_pending_approval_actions() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-project-digest-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();

    let project_id = create_test_project(&state, &root, "Digest Project");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);

    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Implemented the requested fix.".to_owned(),
                expanded_text: None,
            },
        )
        .unwrap();

    let approval_message_id = state.allocate_message_id();
    state
        .push_message(
            &session_id,
            Message::Approval {
                id: approval_message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Approve command".to_owned(),
                command: "cargo test".to_owned(),
                command_language: Some(shell_language().to_owned()),
                detail: "Approval required.".to_owned(),
                decision: ApprovalDecision::Pending,
            },
        )
        .unwrap();
    state
        .register_codex_pending_approval(
            &session_id,
            approval_message_id.clone(),
            CodexPendingApproval {
                kind: CodexApprovalKind::CommandExecution,
                request_id: json!("req-project-digest"),
            },
        )
        .unwrap();

    let digest = state.project_digest(&project_id).unwrap();
    let action_ids = digest
        .proposed_actions
        .iter()
        .map(|action| action.id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        digest.primary_session_id.as_deref(),
        Some(session_id.as_str())
    );
    assert_eq!(digest.current_status, "Waiting on your decision.");
    assert_eq!(digest.done_summary, "Implemented the requested fix.");
    assert_eq!(digest.source_message_ids[0], approval_message_id);
    assert_eq!(action_ids, vec!["approve", "reject", "review-in-termal"]);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn project_digest_prefers_review_actions_for_dirty_idle_project() {
    let state = test_app_state();
    let repo_root = std::env::temp_dir().join(format!("termal-project-review-{}", Uuid::new_v4()));
    fs::create_dir_all(repo_root.join("src")).unwrap();
    fs::write(
        repo_root.join("src/lib.rs"),
        "pub fn value() -> u32 { 1 }\n",
    )
    .unwrap();

    run_git_test_command(&repo_root, &["init"]);
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);
    run_git_test_command(&repo_root, &["add", "."]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);

    fs::write(
        repo_root.join("src/lib.rs"),
        "pub fn value() -> u32 { 2 }\n",
    )
    .unwrap();

    let project_id = create_test_project(&state, &repo_root, "Review Project");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &repo_root);

    let digest = state.project_digest(&project_id).unwrap();
    let action_ids = digest
        .proposed_actions
        .iter()
        .map(|action| action.id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        digest.primary_session_id.as_deref(),
        Some(session_id.as_str())
    );
    assert_eq!(digest.current_status, "Changes are ready for review.");
    assert!(digest.done_summary.contains("1 changed file"));
    assert_eq!(
        action_ids,
        vec!["review-in-termal", "ask-agent-to-commit", "keep-iterating"]
    );

    fs::remove_dir_all(repo_root).unwrap();
}

#[test]
fn project_action_approve_routes_to_the_live_project_approval() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-project-approve-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();

    let project_id = create_test_project(&state, &root, "Approval Project");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    let (runtime, input_rx) = test_codex_runtime_handle("project-approve");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }

    let approval_message_id = state.allocate_message_id();
    state
        .push_message(
            &session_id,
            Message::Approval {
                id: approval_message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Approve command".to_owned(),
                command: "cargo test".to_owned(),
                command_language: Some(shell_language().to_owned()),
                detail: "Approval required.".to_owned(),
                decision: ApprovalDecision::Pending,
            },
        )
        .unwrap();
    state
        .register_codex_pending_approval(
            &session_id,
            approval_message_id.clone(),
            CodexPendingApproval {
                kind: CodexApprovalKind::CommandExecution,
                request_id: json!("req-project-approve"),
            },
        )
        .unwrap();

    let digest = state
        .execute_project_action(&project_id, "approve")
        .unwrap();

    match input_rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        CodexRuntimeCommand::JsonRpcResponse { response } => {
            assert_eq!(response.request_id, json!("req-project-approve"));
            assert_eq!(response.result, json!({ "decision": "accept" }));
        }
        _ => panic!("expected approval response"),
    }

    assert_eq!(digest.current_status, "Agent is working.");
    assert!(
        !digest
            .proposed_actions
            .iter()
            .any(|action| action.id == "approve")
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn project_action_keep_iterating_dispatches_a_follow_up_prompt() {
    let state = test_app_state();
    let repo_root = std::env::temp_dir().join(format!("termal-project-iterate-{}", Uuid::new_v4()));
    fs::create_dir_all(repo_root.join("src")).unwrap();
    fs::write(
        repo_root.join("src/lib.rs"),
        "pub fn value() -> u32 { 1 }\n",
    )
    .unwrap();

    run_git_test_command(&repo_root, &["init"]);
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);
    run_git_test_command(&repo_root, &["add", "."]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);

    fs::write(
        repo_root.join("src/lib.rs"),
        "pub fn value() -> u32 { 2 }\n",
    )
    .unwrap();

    let project_id = create_test_project(&state, &repo_root, "Iterate Project");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &repo_root);
    let (runtime, input_rx) = test_codex_runtime_handle("project-iterate");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }

    let digest = state
        .execute_project_action(&project_id, "keep-iterating")
        .unwrap();

    match input_rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        CodexRuntimeCommand::Prompt {
            session_id: runtime_session_id,
            command,
        } => {
            assert_eq!(runtime_session_id, session_id);
            assert_eq!(
                command.prompt,
                ProjectActionId::KeepIterating.prompt().unwrap()
            );
        }
        _ => panic!("expected prompt dispatch"),
    }

    assert_eq!(digest.current_status, "Agent is working.");
    assert_eq!(
        digest
            .proposed_actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>(),
        vec!["stop", "review-in-termal"]
    );

    fs::remove_dir_all(repo_root).unwrap();
}

#[test]
fn telegram_command_parser_supports_suffixes_and_aliases() {
    let parsed =
        parse_telegram_command("/commit@termal_bot   now please").expect("command should parse");
    assert_eq!(
        parsed.command,
        TelegramIncomingCommand::Action(ProjectActionId::AskAgentToCommit)
    );
    assert_eq!(parsed.args, "now please");

    let parsed = parse_telegram_command("/status").expect("status should parse");
    assert_eq!(parsed.command, TelegramIncomingCommand::Status);
}

#[test]
fn telegram_command_parser_rejects_unknown_slash_commands() {
    assert!(parse_telegram_command("/unknown").is_none());
}

#[test]
fn telegram_digest_renderer_includes_actions_and_public_link() {
    let digest = ProjectDigestResponse {
        project_id: "project-1".to_owned(),
        headline: "termal".to_owned(),
        done_summary: "Updated the digest API.".to_owned(),
        current_status: "Changes are ready for review.".to_owned(),
        primary_session_id: Some("session-1".to_owned()),
        proposed_actions: vec![
            ProjectActionId::ReviewInTermal.into_digest_action(),
            ProjectActionId::AskAgentToCommit.into_digest_action(),
        ],
        deep_link: Some("/?projectId=project-1&sessionId=session-1".to_owned()),
        source_message_ids: vec!["message-1".to_owned()],
    };

    let rendered = render_telegram_digest(&digest, Some("https://termal.local"));
    assert!(rendered.contains("Project: termal"));
    assert!(rendered.contains("Next: Review in TermAl, Ask Agent to Commit"));
    assert!(
        rendered.contains("Open: https://termal.local/?projectId=project-1&sessionId=session-1")
    );

    let keyboard = build_telegram_digest_keyboard(&digest).expect("keyboard should exist");
    assert_eq!(keyboard.inline_keyboard.len(), 1);
    assert_eq!(
        keyboard.inline_keyboard[0][0].callback_data,
        "review-in-termal"
    );
    assert_eq!(
        keyboard.inline_keyboard[0][1].callback_data,
        "ask-agent-to-commit"
    );
}

#[test]
fn persisted_state_without_projects_migrates_cleanly() {
    let path = std::env::temp_dir().join(format!("termal-project-migrate-{}.json", Uuid::new_v4()));
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Codex,
        Some("Migrated".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    persist_state(&path, &inner).unwrap();

    let mut encoded: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let object = encoded
        .as_object_mut()
        .expect("persisted state should be an object");
    object.remove("projects");
    object.remove("nextProjectNumber");
    fs::write(&path, serde_json::to_vec(&encoded).unwrap()).unwrap();

    let loaded = load_state(&path).unwrap().expect("state should load");
    assert_eq!(loaded.projects.len(), 1);
    assert_eq!(loaded.projects[0].root_path, "/tmp");
    assert_eq!(loaded.projects[0].remote_id, default_local_remote_id());
    assert_eq!(loaded.preferences.remotes, default_remote_configs());
    assert_eq!(
        loaded.sessions[0].session.project_id.as_deref(),
        Some(loaded.projects[0].id.as_str())
    );

    let _ = fs::remove_file(path);
}

#[test]
fn delta_events_include_monotonic_revisions() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let mut delta_events = state.subscribe_delta_events();

    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: "message-1".to_owned(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Hi".to_owned(),
                expanded_text: None,
            },
        )
        .unwrap();
    let baseline = state.snapshot().revision;

    let created_payload = delta_events
        .try_recv()
        .expect("message-created delta payload should exist");
    let created_event: Value =
        serde_json::from_str(&created_payload).expect("delta should be valid json");
    assert_eq!(created_event["type"], "messageCreated");
    assert_eq!(created_event["messageIndex"], json!(0));

    state
        .append_text_delta(&session_id, "message-1", " there")
        .unwrap();

    let payload = delta_events.try_recv().expect("delta payload should exist");
    let event: Value = serde_json::from_str(&payload).expect("delta should be valid json");

    assert_eq!(event["type"], "textDelta");
    assert_eq!(event["revision"], json!(baseline + 1));
    assert_eq!(event["messageIndex"], json!(0));
    assert_eq!(state.snapshot().revision, baseline + 1);
}

#[test]
fn parallel_agent_updates_publish_targeted_deltas() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let mut delta_events = state.subscribe_delta_events();

    state
        .upsert_parallel_agents_message(
            &session_id,
            "parallel-1",
            vec![ParallelAgentProgress {
                id: "reviewer".to_owned(),
                title: "Reviewer".to_owned(),
                status: ParallelAgentStatus::Initializing,
                detail: None,
            }],
        )
        .unwrap();
    let baseline = state.snapshot().revision;

    let created_payload = delta_events
        .try_recv()
        .expect("parallel-agent message-created delta payload should exist");
    let created_event: Value =
        serde_json::from_str(&created_payload).expect("delta should be valid json");
    assert_eq!(created_event["type"], "messageCreated");
    assert_eq!(created_event["messageIndex"], json!(0));

    state
        .upsert_parallel_agents_message(
            &session_id,
            "parallel-1",
            vec![ParallelAgentProgress {
                id: "reviewer".to_owned(),
                title: "Reviewer".to_owned(),
                status: ParallelAgentStatus::Running,
                detail: Some("Checking diffs".to_owned()),
            }],
        )
        .unwrap();

    let payload = delta_events
        .try_recv()
        .expect("parallel-agent delta payload should exist");
    let event: Value = serde_json::from_str(&payload).expect("delta should be valid json");

    assert_eq!(event["type"], "parallelAgentsUpdate");
    assert_eq!(event["revision"], json!(baseline + 1));
    assert_eq!(event["messageIndex"], json!(0));
    assert_eq!(event["agents"][0]["status"], json!("running"));
    assert_eq!(state.snapshot().revision, baseline + 1);
}

#[test]
fn delta_persistence_is_deferred_until_the_next_durable_commit() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);

    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: "message-1".to_owned(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Hi".to_owned(),
                expanded_text: None,
            },
        )
        .unwrap();

    state
        .append_text_delta(&session_id, "message-1", " there")
        .unwrap();

    let persisted_before_commit = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    let persisted_record = persisted_before_commit
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("session should reload");
    assert!(matches!(
        persisted_record.session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Hi"
    ));

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        state.commit_locked(&mut inner).unwrap();
    }

    let persisted_after_commit = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    let persisted_record = persisted_after_commit
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("session should reload");
    assert!(matches!(
        persisted_record.session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Hi there"
    ));
}

#[test]
fn internal_runtime_config_persistence_does_not_advance_revision() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let baseline = state.snapshot().revision;

    state
        .record_codex_runtime_config(
            &session_id,
            CodexSandboxMode::ReadOnly,
            CodexApprovalPolicy::OnRequest,
            CodexReasoningEffort::High,
        )
        .unwrap();

    assert_eq!(state.snapshot().revision, baseline);

    let reloaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    let record = reloaded
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("session should reload");

    assert_eq!(
        record.active_codex_sandbox_mode,
        Some(CodexSandboxMode::ReadOnly)
    );
    assert_eq!(
        record.active_codex_approval_policy,
        Some(CodexApprovalPolicy::OnRequest)
    );
    assert_eq!(
        record.active_codex_reasoning_effort,
        Some(CodexReasoningEffort::High)
    );
    assert_eq!(reloaded.revision, baseline);
}

#[test]
fn builds_codex_turn_input_with_text_and_image_attachments() {
    let attachments = vec![PromptImageAttachment {
        data: "aGVsbG8=".to_owned(),
        metadata: MessageImageAttachment {
            byte_size: 5,
            file_name: "paste.png".to_owned(),
            media_type: "image/png".to_owned(),
        },
    }];

    let input = codex_user_input_items("Inspect this screenshot.", &attachments);

    assert_eq!(
        input,
        vec![
            json!({
                "type": "text",
                "text": "Inspect this screenshot.",
            }),
            json!({
                "type": "image",
                "url": "data:image/png;base64,aGVsbG8=",
            })
        ]
    );
}

#[test]
fn builds_codex_turn_input_with_images_only() {
    let attachments = vec![PromptImageAttachment {
        data: "d29ybGQ=".to_owned(),
        metadata: MessageImageAttachment {
            byte_size: 5,
            file_name: "paste.jpg".to_owned(),
            media_type: "image/jpeg".to_owned(),
        },
    }];

    let input = codex_user_input_items("", &attachments);

    assert_eq!(
        input,
        vec![json!({
            "type": "image",
            "url": "data:image/jpeg;base64,d29ybGQ=",
        })]
    );
}

#[test]
fn infers_languages_from_paths() {
    assert_eq!(
        infer_language_from_path(FsPath::new("ui/src/App.tsx")),
        Some("typescript")
    );
    assert_eq!(
        infer_language_from_path(FsPath::new("/tmp/Cargo.toml")),
        Some("ini")
    );
    assert_eq!(
        infer_language_from_path(FsPath::new("Dockerfile")),
        Some("dockerfile")
    );
    assert_eq!(
        infer_language_from_path(FsPath::new("lib/main.dart")),
        Some("dart")
    );
}

#[test]
fn infers_command_output_languages_conservatively() {
    assert_eq!(
        infer_command_output_language(r#"/bin/zsh -lc "sed -n '1,120p' ui/src/App.tsx""#),
        Some("typescript")
    );
    assert_eq!(
        infer_command_output_language("git diff -- ui/src/App.tsx"),
        Some("diff")
    );
    assert_eq!(infer_command_output_language("npm test"), None);
}

#[test]
fn stores_command_language_metadata_on_messages() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);

    state
        .upsert_command_message(
            &session_id,
            "message-1",
            r#"/bin/zsh -lc "sed -n '1,120p' ui/src/App.tsx""#,
            "import { memo } from \"react\";",
            CommandStatus::Success,
        )
        .unwrap();

    let inner = state.inner.lock().expect("state mutex poisoned");
    let session = &inner.sessions[0].session;
    match &session.messages[0] {
        Message::Command {
            command_language,
            output_language,
            ..
        } => {
            assert_eq!(command_language.as_deref(), Some("bash"));
            assert_eq!(output_language.as_deref(), Some("typescript"));
        }
        other => panic!("expected command message, found {other:?}"),
    }
}

#[test]
fn queues_follow_up_prompts_while_session_is_busy() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].session.status = SessionStatus::Active;
        state.commit_locked(&mut inner).unwrap();
    }

    let result = state
        .dispatch_turn(
            &session_id,
            SendMessageRequest {
                text: "queue this follow-up".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
            },
        )
        .unwrap();

    assert!(matches!(result, DispatchTurnResult::Queued));

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .unwrap();
    assert_eq!(session.pending_prompts.len(), 1);
    assert_eq!(session.pending_prompts[0].text, "queue this follow-up");
}

#[test]
fn dispatches_codex_turn_with_image_attachments() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx) = test_codex_runtime_handle("codex-attachment-dispatch");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }

    let result = state
        .dispatch_turn(
            &session_id,
            SendMessageRequest {
                text: "Inspect this screenshot.".to_owned(),
                expanded_text: None,
                attachments: vec![SendMessageAttachmentRequest {
                    data: "aGVsbG8=".to_owned(),
                    file_name: Some("paste.png".to_owned()),
                    media_type: "image/png".to_owned(),
                }],
            },
        )
        .unwrap();

    match result {
        DispatchTurnResult::Dispatched(TurnDispatch::PersistentCodex { command, .. }) => {
            assert_eq!(command.prompt, "Inspect this screenshot.");
            assert_eq!(command.attachments.len(), 1);
            assert_eq!(command.attachments[0].data, "aGVsbG8=");
            assert_eq!(command.attachments[0].metadata.file_name, "paste.png");
            assert_eq!(command.attachments[0].metadata.media_type, "image/png");
            assert_eq!(command.attachments[0].metadata.byte_size, 5);
        }
        DispatchTurnResult::Dispatched(_) => panic!("expected Codex dispatch"),
        DispatchTurnResult::Queued => panic!("expected dispatched turn"),
    }

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .unwrap();

    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(session.preview, "Inspect this screenshot.");
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text {
            text,
            attachments,
            ..
        }) if text == "Inspect this screenshot."
            && attachments.len() == 1
            && attachments[0].file_name == "paste.png"
            && attachments[0].media_type == "image/png"
            && attachments[0].byte_size == 5
    ));
}

#[test]
fn dispatches_saved_queued_prompts_before_new_prompt_after_recovery() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);

    let child = test_exit_success_child();
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "recovered-queue-dispatch".to_owned(),
        input_tx,
        process: Arc::new(SharedChild::new(child).unwrap()),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Error;
        queue_prompt_on_record(
            &mut inner.sessions[index],
            PendingPrompt {
                attachments: Vec::new(),
                id: "queued-1".to_owned(),
                timestamp: stamp_now(),
                text: "first".to_owned(),
                expanded_text: None,
            },
            Vec::new(),
        );
        state.commit_locked(&mut inner).unwrap();
    }

    let result = state
        .dispatch_turn(
            &session_id,
            SendMessageRequest {
                text: "second".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
            },
        )
        .unwrap();

    match result {
        DispatchTurnResult::Dispatched(TurnDispatch::PersistentClaude { command, .. }) => {
            assert_eq!(command.text, "first");
        }
        DispatchTurnResult::Dispatched(_) => panic!("expected Claude dispatch"),
        DispatchTurnResult::Queued => panic!("expected dispatched turn"),
    }

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .unwrap();

    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(session.pending_prompts.len(), 1);
    assert_eq!(session.pending_prompts[0].text, "second");
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "first"
    ));
}

#[test]
fn canceling_claude_approval_marks_message_canceled_and_resumes_session() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let message_id = state.allocate_message_id();

    state
        .push_message(
            &session_id,
            Message::Approval {
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Approve edit".to_owned(),
                command: "apply_patch".to_owned(),
                command_language: Some(shell_language().to_owned()),
                detail: "Claude requested permission.".to_owned(),
                decision: ApprovalDecision::Pending,
            },
        )
        .unwrap();
    state
        .register_claude_pending_approval(
            &session_id,
            message_id.clone(),
            ClaudePendingApproval {
                permission_mode_for_session: None,
                request_id: "req-cancel".to_owned(),
                tool_input: json!({ "path": "src/runtime.rs" }),
            },
        )
        .unwrap();

    state
        .clear_claude_pending_approval_by_request(&session_id, "req-cancel")
        .unwrap();

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");

    assert!(record.pending_claude_approvals.is_empty());
    assert_eq!(record.session.status, SessionStatus::Active);
    assert_eq!(
        record.session.preview,
        "Approval canceled. Claude is continuing\u{2026}"
    );
    assert!(matches!(
        record.session.messages.first(),
        Some(Message::Approval { decision, .. }) if *decision == ApprovalDecision::Canceled
    ));
}

#[test]
fn resolving_one_of_multiple_pending_approvals_keeps_session_waiting() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, input_rx) = test_claude_runtime_handle("claude-approval-lifecycle");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    }

    let first_message_id = state.allocate_message_id();
    state
        .push_message(
            &session_id,
            Message::Approval {
                id: first_message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Approve command".to_owned(),
                command: "git status".to_owned(),
                command_language: Some(shell_language().to_owned()),
                detail: "First approval.".to_owned(),
                decision: ApprovalDecision::Pending,
            },
        )
        .unwrap();
    state
        .register_claude_pending_approval(
            &session_id,
            first_message_id.clone(),
            ClaudePendingApproval {
                permission_mode_for_session: None,
                request_id: "req-1".to_owned(),
                tool_input: json!({ "command": "git status" }),
            },
        )
        .unwrap();

    let second_message_id = state.allocate_message_id();
    state
        .push_message(
            &session_id,
            Message::Approval {
                id: second_message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Approve edit".to_owned(),
                command: "apply_patch".to_owned(),
                command_language: Some(shell_language().to_owned()),
                detail: "Second approval.".to_owned(),
                decision: ApprovalDecision::Pending,
            },
        )
        .unwrap();
    state
        .register_claude_pending_approval(
            &session_id,
            second_message_id.clone(),
            ClaudePendingApproval {
                permission_mode_for_session: None,
                request_id: "req-2".to_owned(),
                tool_input: json!({ "path": "src/state.rs" }),
            },
        )
        .unwrap();

    state
        .update_approval(&session_id, &first_message_id, ApprovalDecision::Accepted)
        .unwrap();

    match input_rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        ClaudeRuntimeCommand::PermissionResponse(ClaudePermissionDecision::Allow {
            request_id,
            updated_input,
        }) => {
            assert_eq!(request_id, "req-1");
            assert_eq!(updated_input, json!({ "command": "git status" }));
        }
        _ => panic!("expected Claude approval response"),
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");

    assert_eq!(record.session.status, SessionStatus::Approval);
    assert_eq!(record.session.preview, "Approval pending.");
    assert!(
        !record
            .pending_claude_approvals
            .contains_key(&first_message_id)
    );
    assert!(
        record
            .pending_claude_approvals
            .contains_key(&second_message_id)
    );
    assert!(matches!(
        record.session.messages.first(),
        Some(Message::Approval { decision, .. }) if *decision == ApprovalDecision::Accepted
    ));
    assert!(matches!(
        record.session.messages.get(1),
        Some(Message::Approval { decision, .. }) if *decision == ApprovalDecision::Pending
    ));
}

#[test]
fn resolving_one_of_multiple_pending_codex_approvals_keeps_session_waiting() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx) = test_codex_runtime_handle("codex-approval-lifecycle");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }

    let first_message_id = state.allocate_message_id();
    state
        .push_message(
            &session_id,
            Message::Approval {
                id: first_message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Approve command".to_owned(),
                command: "cargo test".to_owned(),
                command_language: Some(shell_language().to_owned()),
                detail: "First approval.".to_owned(),
                decision: ApprovalDecision::Pending,
            },
        )
        .unwrap();
    state
        .register_codex_pending_approval(
            &session_id,
            first_message_id.clone(),
            CodexPendingApproval {
                kind: CodexApprovalKind::CommandExecution,
                request_id: json!("req-1"),
            },
        )
        .unwrap();

    let second_message_id = state.allocate_message_id();
    state
        .push_message(
            &session_id,
            Message::Approval {
                id: second_message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Approve edit".to_owned(),
                command: "apply patch".to_owned(),
                command_language: Some(shell_language().to_owned()),
                detail: "Second approval.".to_owned(),
                decision: ApprovalDecision::Pending,
            },
        )
        .unwrap();
    state
        .register_codex_pending_approval(
            &session_id,
            second_message_id.clone(),
            CodexPendingApproval {
                kind: CodexApprovalKind::FileChange,
                request_id: json!("req-2"),
            },
        )
        .unwrap();

    state
        .update_approval(&session_id, &first_message_id, ApprovalDecision::Accepted)
        .unwrap();

    match input_rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        CodexRuntimeCommand::JsonRpcResponse { response } => {
            assert_eq!(response.request_id, json!("req-1"));
            assert_eq!(response.result, json!({ "decision": "accept" }));
        }
        _ => panic!("expected Codex approval response"),
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");

    assert_eq!(record.session.status, SessionStatus::Approval);
    assert_eq!(record.session.preview, "Approval pending.");
    assert!(
        !record
            .pending_codex_approvals
            .contains_key(&first_message_id)
    );
    assert!(
        record
            .pending_codex_approvals
            .contains_key(&second_message_id)
    );
    assert!(matches!(
        record.session.messages.first(),
        Some(Message::Approval { decision, .. }) if *decision == ApprovalDecision::Accepted
    ));
    assert!(matches!(
        record.session.messages.get(1),
        Some(Message::Approval { decision, .. }) if *decision == ApprovalDecision::Pending
    ));
}

#[test]
fn codex_permissions_approval_decisions_map_to_expected_runtime_payloads() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx) = test_codex_runtime_handle("codex-permissions-approval");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }

    let requested_permissions = json!({
        "fileSystem": {
            "read": ["/repo/docs"]
        },
        "network": {
            "enabled": true
        }
    });

    let first_message_id = state.allocate_message_id();
    state
        .push_message(
            &session_id,
            Message::Approval {
                id: first_message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Grant additional permissions".to_owned(),
                command: "Grant additional permissions".to_owned(),
                command_language: Some(shell_language().to_owned()),
                detail: "Approval required.".to_owned(),
                decision: ApprovalDecision::Pending,
            },
        )
        .unwrap();
    state
        .register_codex_pending_approval(
            &session_id,
            first_message_id.clone(),
            CodexPendingApproval {
                kind: CodexApprovalKind::Permissions {
                    requested_permissions: requested_permissions.clone(),
                },
                request_id: json!("req-permissions-session"),
            },
        )
        .unwrap();

    state
        .update_approval(
            &session_id,
            &first_message_id,
            ApprovalDecision::AcceptedForSession,
        )
        .unwrap();

    match input_rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        CodexRuntimeCommand::JsonRpcResponse { response } => {
            assert_eq!(response.request_id, json!("req-permissions-session"));
            assert_eq!(
                response.result,
                json!({
                    "permissions": requested_permissions,
                    "scope": "session"
                })
            );
        }
        _ => panic!("expected Codex approval response"),
    }

    let second_message_id = state.allocate_message_id();
    state
        .push_message(
            &session_id,
            Message::Approval {
                id: second_message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Grant additional permissions".to_owned(),
                command: "Grant additional permissions".to_owned(),
                command_language: Some(shell_language().to_owned()),
                detail: "Approval required.".to_owned(),
                decision: ApprovalDecision::Pending,
            },
        )
        .unwrap();
    state
        .register_codex_pending_approval(
            &session_id,
            second_message_id.clone(),
            CodexPendingApproval {
                kind: CodexApprovalKind::Permissions {
                    requested_permissions: json!({
                        "fileSystem": {
                            "write": ["/repo/src"]
                        }
                    }),
                },
                request_id: json!("req-permissions-rejected"),
            },
        )
        .unwrap();

    state
        .update_approval(&session_id, &second_message_id, ApprovalDecision::Rejected)
        .unwrap();

    match input_rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        CodexRuntimeCommand::JsonRpcResponse { response } => {
            assert_eq!(response.request_id, json!("req-permissions-rejected"));
            assert_eq!(
                response.result,
                json!({
                    "permissions": {},
                    "scope": "turn"
                })
            );
        }
        _ => panic!("expected Codex approval response"),
    }
}

#[tokio::test]
async fn codex_user_input_route_submits_answers_and_redacts_secret_values() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx) = test_codex_runtime_handle("codex-user-input");
    let questions = vec![
        UserInputQuestion {
            header: "Environment".to_owned(),
            id: "environment".to_owned(),
            is_other: false,
            is_secret: false,
            options: Some(vec![
                UserInputQuestionOption {
                    label: "Production".to_owned(),
                    description: "Use the production cluster.".to_owned(),
                },
                UserInputQuestionOption {
                    label: "Staging".to_owned(),
                    description: "Use the staging cluster.".to_owned(),
                },
            ]),
            question: "Which environment should I use?".to_owned(),
        },
        UserInputQuestion {
            header: "API token".to_owned(),
            id: "apiToken".to_owned(),
            is_other: false,
            is_secret: true,
            options: None,
            question: "Paste the temporary token.".to_owned(),
        },
    ];

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }

    let message_id = state.allocate_message_id();
    state
        .push_message(
            &session_id,
            Message::UserInputRequest {
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Codex needs input".to_owned(),
                detail: "Codex requested additional input.".to_owned(),
                questions: questions.clone(),
                state: InteractionRequestState::Pending,
                submitted_answers: None,
            },
        )
        .unwrap();
    state
        .register_codex_pending_user_input(
            &session_id,
            message_id.clone(),
            CodexPendingUserInput {
                questions: questions.clone(),
                request_id: json!("req-input-1"),
            },
        )
        .unwrap();

    let app = app_router(state);
    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{session_id}/user-input/{message_id}"
            ))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{
                    "answers": {
                        "environment": ["Production"],
                        "apiToken": ["secret-123"]
                    }
                }"#,
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);

    match input_rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        CodexRuntimeCommand::JsonRpcResponse { response } => {
            assert_eq!(response.request_id, json!("req-input-1"));
            assert_eq!(
                response.result,
                json!({
                    "answers": {
                        "environment": {
                            "answers": ["Production"]
                        },
                        "apiToken": {
                            "answers": ["secret-123"]
                        }
                    }
                })
            );
        }
        _ => panic!("expected Codex JSON-RPC response"),
    }

    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(session.preview, "Input submitted. Codex is continuing…");
    assert!(matches!(
        session.messages.last(),
        Some(Message::UserInputRequest {
            state,
            submitted_answers: Some(submitted_answers),
            ..
        }) if *state == InteractionRequestState::Submitted
            && submitted_answers.get("environment") == Some(&vec!["Production".to_owned()])
            && submitted_answers.get("apiToken") == Some(&vec!["[secret provided]".to_owned()])
    ));
}

#[tokio::test]
async fn codex_mcp_elicitation_route_submits_structured_content() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx) = test_codex_runtime_handle("runtime-mcp-elicitation");
    let request = McpElicitationRequestPayload {
        thread_id: "thread-1".to_owned(),
        turn_id: Some("turn-1".to_owned()),
        server_name: "deployment-helper".to_owned(),
        mode: McpElicitationRequestMode::Form {
            meta: None,
            message: "Confirm the deployment settings.".to_owned(),
            requested_schema: json!({
                "type": "object",
                "properties": {
                    "environment": {
                        "type": "string",
                        "title": "Environment",
                        "oneOf": [
                            { "const": "production", "title": "Production" },
                            { "const": "staging", "title": "Staging" }
                        ]
                    },
                    "replicas": {
                        "type": "integer",
                        "title": "Replicas"
                    },
                    "notify": {
                        "type": "boolean",
                        "title": "Notify"
                    }
                },
                "required": ["environment", "replicas"]
            }),
        },
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }

    let message_id = state.allocate_message_id();
    state
        .push_message(
            &session_id,
            Message::McpElicitationRequest {
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Codex needs MCP input".to_owned(),
                detail: "MCP server deployment-helper requested additional structured input."
                    .to_owned(),
                request: request.clone(),
                state: InteractionRequestState::Pending,
                submitted_action: None,
                submitted_content: None,
            },
        )
        .unwrap();
    state
        .register_codex_pending_mcp_elicitation(
            &session_id,
            message_id.clone(),
            CodexPendingMcpElicitation {
                request: request.clone(),
                request_id: json!("req-elicit-1"),
            },
        )
        .unwrap();

    let app = app_router(state);
    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{session_id}/mcp-elicitation/{message_id}"
            ))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{
                    "action": "accept",
                    "content": {
                        "environment": "production",
                        "replicas": 3,
                        "notify": true
                    }
                }"#,
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);

    match input_rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        CodexRuntimeCommand::JsonRpcResponse { response } => {
            assert_eq!(response.request_id, json!("req-elicit-1"));
            assert_eq!(
                response.result,
                json!({
                    "action": "accept",
                    "content": {
                        "environment": "production",
                        "replicas": 3,
                        "notify": true
                    }
                })
            );
        }
        _ => panic!("expected Codex JSON-RPC response"),
    }

    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(session.preview, "MCP input submitted. Codex is continuing…");
    assert!(matches!(
        session.messages.last(),
        Some(Message::McpElicitationRequest {
            state,
            submitted_action: Some(McpElicitationAction::Accept),
            submitted_content: Some(submitted_content),
            ..
        }) if *state == InteractionRequestState::Submitted
            && submitted_content == &json!({
                "environment": "production",
                "replicas": 3,
                "notify": true
            })
    ));
}

#[tokio::test]
async fn codex_mcp_elicitation_route_rejects_out_of_range_numbers() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx) = test_codex_runtime_handle("runtime-mcp-elicitation-range");
    let request = McpElicitationRequestPayload {
        thread_id: "thread-1".to_owned(),
        turn_id: Some("turn-1".to_owned()),
        server_name: "deployment-helper".to_owned(),
        mode: McpElicitationRequestMode::Form {
            meta: None,
            message: "Confirm the deployment settings.".to_owned(),
            requested_schema: json!({
                "type": "object",
                "properties": {
                    "replicas": {
                        "type": "integer",
                        "title": "Replicas",
                        "minimum": 2,
                        "maximum": 5
                    }
                },
                "required": ["replicas"]
            }),
        },
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }

    let message_id = state.allocate_message_id();
    state
        .push_message(
            &session_id,
            Message::McpElicitationRequest {
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Codex needs MCP input".to_owned(),
                detail: "MCP server deployment-helper requested additional structured input."
                    .to_owned(),
                request: request.clone(),
                state: InteractionRequestState::Pending,
                submitted_action: None,
                submitted_content: None,
            },
        )
        .unwrap();
    state
        .register_codex_pending_mcp_elicitation(
            &session_id,
            message_id.clone(),
            CodexPendingMcpElicitation {
                request,
                request_id: json!("req-elicit-range"),
            },
        )
        .unwrap();

    let app = app_router(state);
    let (status, response): (StatusCode, ErrorResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{session_id}/mcp-elicitation/{message_id}"
            ))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{
                    "action": "accept",
                    "content": {
                        "replicas": 1
                    }
                }"#,
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(response.error, "field `replicas` must be at least 2");
    assert!(input_rx.recv_timeout(Duration::from_millis(100)).is_err());
}

#[tokio::test]
async fn codex_generic_app_request_route_submits_json_result() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx) = test_codex_runtime_handle("runtime-generic-request");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }

    let message_id = state.allocate_message_id();
    let params = json!({
        "toolName": "search_workspace",
        "arguments": {
            "pattern": "Codex"
        }
    });
    state
        .push_message(
            &session_id,
            Message::CodexAppRequest {
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Codex needs a tool result".to_owned(),
                detail: "Codex requested a result for `search_workspace`.".to_owned(),
                method: "item/tool/call".to_owned(),
                params: params.clone(),
                state: InteractionRequestState::Pending,
                submitted_result: None,
            },
        )
        .unwrap();
    state
        .register_codex_pending_app_request(
            &session_id,
            message_id.clone(),
            CodexPendingAppRequest {
                request_id: json!("req-tool-1"),
            },
        )
        .unwrap();

    let app = app_router(state);
    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{session_id}/codex/requests/{message_id}"
            ))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{
                    "result": {
                        "matches": ["docs/bugs.md", "src/runtime.rs"]
                    }
                }"#,
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);

    match input_rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        CodexRuntimeCommand::JsonRpcResponse { response } => {
            assert_eq!(response.request_id, json!("req-tool-1"));
            assert_eq!(
                response.result,
                json!({
                    "matches": ["docs/bugs.md", "src/runtime.rs"]
                })
            );
        }
        _ => panic!("expected Codex JSON-RPC response"),
    }

    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(
        session.preview,
        "Codex response submitted. Codex is continuing…"
    );
    assert!(matches!(
        session.messages.last(),
        Some(Message::CodexAppRequest {
            state,
            submitted_result: Some(submitted_result),
            ..
        }) if *state == InteractionRequestState::Submitted
            && submitted_result == &json!({
                "matches": ["docs/bugs.md", "src/runtime.rs"]
            })
    ));
}

#[test]
fn validate_codex_app_request_result_rejects_excessive_depth() {
    let mut result = json!("leaf");
    for depth in 0..=CODEX_APP_REQUEST_RESULT_MAX_DEPTH {
        result = json!({
            format!("level-{depth}"): result
        });
    }

    let error = validate_codex_app_request_result(result)
        .expect_err("deep generic app request payload should be rejected");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("levels deep"));
}

#[tokio::test]
async fn codex_generic_app_request_route_rejects_oversized_json_result() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx) = test_codex_runtime_handle("runtime-generic-request-limit");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }

    let message_id = state.allocate_message_id();
    state
        .push_message(
            &session_id,
            Message::CodexAppRequest {
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Codex needs a tool result".to_owned(),
                detail: "Codex requested a result for `search_workspace`.".to_owned(),
                method: "item/tool/call".to_owned(),
                params: json!({
                    "toolName": "search_workspace",
                    "arguments": {
                        "pattern": "Codex"
                    }
                }),
                state: InteractionRequestState::Pending,
                submitted_result: None,
            },
        )
        .unwrap();
    state
        .register_codex_pending_app_request(
            &session_id,
            message_id.clone(),
            CodexPendingAppRequest {
                request_id: json!("req-tool-limit"),
            },
        )
        .unwrap();

    let oversized = "x".repeat(CODEX_APP_REQUEST_RESULT_MAX_BYTES + 1);
    let body = json!({
        "result": {
            "blob": oversized
        }
    });
    let app = app_router(state);
    let (status, response): (StatusCode, ErrorResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{session_id}/codex/requests/{message_id}"
            ))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        response.error,
        "Codex app request result must be at most 64 KB"
    );
    assert!(input_rx.recv_timeout(Duration::from_millis(100)).is_err());
}

#[test]
fn review_documents_round_trip_through_disk() {
    let review_path = std::env::temp_dir()
        .join(format!("termal-review-{}", Uuid::new_v4()))
        .join("change-message-42.json");
    let review = ReviewDocument {
        version: REVIEW_DOCUMENT_VERSION,
        revision: 3,
        change_set_id: "change-message-42".to_owned(),
        origin: Some(ReviewOrigin {
            session_id: "session-3".to_owned(),
            message_id: "message-42".to_owned(),
            agent: "Codex".to_owned(),
            workdir: "/tmp/project".to_owned(),
            created_at: "2026-03-09T18:55:00Z".to_owned(),
        }),
        files: vec![ReviewFileEntry {
            file_path: "docs/bugs.md".to_owned(),
            change_type: ChangeType::Edit,
        }],
        threads: vec![ReviewThread {
            id: "comment-1".to_owned(),
            anchor: ReviewAnchor::Line {
                file_path: "docs/bugs.md".to_owned(),
                hunk_header: "@@ -10,3 +10,8 @@".to_owned(),
                old_line: None,
                new_line: Some(17),
            },
            status: ReviewThreadStatus::Open,
            comments: vec![
                ReviewThreadComment {
                    id: "comment-1".to_owned(),
                    author: ReviewCommentAuthor::User,
                    body: "Mention Codex attachment support explicitly.".to_owned(),
                    created_at: "2026-03-09T19:02:00Z".to_owned(),
                    updated_at: "2026-03-09T19:02:00Z".to_owned(),
                },
                ReviewThreadComment {
                    id: "reply-1".to_owned(),
                    author: ReviewCommentAuthor::Agent,
                    body: "Handled in the follow-up patch.".to_owned(),
                    created_at: "2026-03-09T19:05:00Z".to_owned(),
                    updated_at: "2026-03-09T19:05:00Z".to_owned(),
                },
            ],
        }],
    };

    persist_review_document(&review_path, &review).expect("review should persist");
    let loaded =
        load_review_document(&review_path, "change-message-42").expect("review should load");

    assert_eq!(loaded, review);
}

#[test]
fn missing_review_documents_return_default_state() {
    let review_path = std::env::temp_dir()
        .join(format!("termal-review-{}", Uuid::new_v4()))
        .join("change-message-99.json");

    let loaded = load_review_document(&review_path, "change-message-99")
        .expect("missing review should default");

    assert_eq!(
        loaded,
        ReviewDocument {
            version: REVIEW_DOCUMENT_VERSION,
            revision: 0,
            change_set_id: "change-message-99".to_owned(),
            origin: None,
            files: Vec::new(),
            threads: Vec::new(),
        }
    );
}

#[test]
fn review_documents_reject_invalid_line_anchors() {
    let review = ReviewDocument {
        version: REVIEW_DOCUMENT_VERSION,
        revision: 0,
        change_set_id: "change-message-88".to_owned(),
        origin: None,
        files: Vec::new(),
        threads: vec![ReviewThread {
            id: "comment-1".to_owned(),
            anchor: ReviewAnchor::Line {
                file_path: "docs/bugs.md".to_owned(),
                hunk_header: "@@ -1,1 +1,1 @@".to_owned(),
                old_line: None,
                new_line: None,
            },
            status: ReviewThreadStatus::Open,
            comments: vec![ReviewThreadComment {
                id: "comment-1".to_owned(),
                author: ReviewCommentAuthor::User,
                body: "Need a concrete line target.".to_owned(),
                created_at: "2026-03-09T19:02:00Z".to_owned(),
                updated_at: "2026-03-09T19:02:00Z".to_owned(),
            }],
        }],
    };

    let error = validate_review_document("change-message-88", &review)
        .expect_err("invalid line anchors should be rejected");

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("line review anchors"));
}

#[test]
fn review_document_paths_resolve_inside_the_scoped_project_root() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-review-root-{}", Uuid::new_v4()));
    let nested = root.join("workspace");

    fs::create_dir_all(&nested).unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Review Project".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Review Session".to_owned()),
            workdir: Some(nested.to_string_lossy().into_owned()),
            project_id: Some(project.project_id),
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let review_root = resolve_review_storage_root(&state, Some(&created.session_id), None).unwrap();
    let review_path = resolve_review_document_path(&review_root, "change-message-7").unwrap();

    assert_eq!(
        review_root,
        normalize_user_facing_path(&fs::canonicalize(&root).unwrap())
    );
    assert_eq!(
        review_path,
        normalize_user_facing_path(&fs::canonicalize(&root).unwrap())
            .join(".termal")
            .join("reviews")
            .join("change-message-7.json")
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn review_document_writes_increment_revision() {
    let review_path = std::env::temp_dir()
        .join(format!("termal-review-write-{}", Uuid::new_v4()))
        .join("change-message-55.json");
    let review = ReviewDocument {
        version: REVIEW_DOCUMENT_VERSION,
        revision: 0,
        change_set_id: "change-message-55".to_owned(),
        origin: None,
        files: Vec::new(),
        threads: vec![ReviewThread {
            id: "thread-1".to_owned(),
            anchor: ReviewAnchor::ChangeSet,
            status: ReviewThreadStatus::Open,
            comments: vec![ReviewThreadComment {
                id: "comment-1".to_owned(),
                author: ReviewCommentAuthor::User,
                body: "First pass.".to_owned(),
                created_at: "2026-03-17T10:00:00Z".to_owned(),
                updated_at: "2026-03-17T10:00:00Z".to_owned(),
            }],
        }],
    };

    let persisted = prepare_review_document_for_write(&review_path, "change-message-55", review)
        .expect("review should prepare");
    persist_review_document(&review_path, &persisted).expect("review should persist");

    assert_eq!(persisted.revision, 1);
    let loaded = load_review_document(&review_path, "change-message-55").unwrap();
    assert_eq!(loaded.revision, 1);
}

#[test]
fn review_document_writes_reject_stale_revisions() {
    let review_path = std::env::temp_dir()
        .join(format!("termal-review-stale-{}", Uuid::new_v4()))
        .join("change-message-56.json");
    let review = ReviewDocument {
        version: REVIEW_DOCUMENT_VERSION,
        revision: 0,
        change_set_id: "change-message-56".to_owned(),
        origin: None,
        files: Vec::new(),
        threads: vec![ReviewThread {
            id: "thread-1".to_owned(),
            anchor: ReviewAnchor::ChangeSet,
            status: ReviewThreadStatus::Open,
            comments: vec![ReviewThreadComment {
                id: "comment-1".to_owned(),
                author: ReviewCommentAuthor::User,
                body: "Current comment.".to_owned(),
                created_at: "2026-03-17T10:00:00Z".to_owned(),
                updated_at: "2026-03-17T10:00:00Z".to_owned(),
            }],
        }],
    };
    let persisted =
        prepare_review_document_for_write(&review_path, "change-message-56", review.clone())
            .unwrap();
    persist_review_document(&review_path, &persisted).unwrap();

    let error = prepare_review_document_for_write(&review_path, "change-message-56", review)
        .expect_err("stale review should conflict");

    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("out of date"));
}

#[test]
fn review_summary_counts_only_resolved_threads_as_resolved() {
    let review = ReviewDocument {
        version: REVIEW_DOCUMENT_VERSION,
        revision: 2,
        change_set_id: "change-message-57".to_owned(),
        origin: None,
        files: Vec::new(),
        threads: vec![
            ReviewThread {
                id: "thread-open".to_owned(),
                anchor: ReviewAnchor::ChangeSet,
                status: ReviewThreadStatus::Open,
                comments: vec![ReviewThreadComment {
                    id: "comment-open".to_owned(),
                    author: ReviewCommentAuthor::User,
                    body: "Open".to_owned(),
                    created_at: "2026-03-17T10:00:00Z".to_owned(),
                    updated_at: "2026-03-17T10:00:00Z".to_owned(),
                }],
            },
            ReviewThread {
                id: "thread-resolved".to_owned(),
                anchor: ReviewAnchor::ChangeSet,
                status: ReviewThreadStatus::Resolved,
                comments: vec![ReviewThreadComment {
                    id: "comment-resolved".to_owned(),
                    author: ReviewCommentAuthor::User,
                    body: "Resolved".to_owned(),
                    created_at: "2026-03-17T10:00:00Z".to_owned(),
                    updated_at: "2026-03-17T10:00:00Z".to_owned(),
                }],
            },
            ReviewThread {
                id: "thread-applied".to_owned(),
                anchor: ReviewAnchor::ChangeSet,
                status: ReviewThreadStatus::Applied,
                comments: vec![ReviewThreadComment {
                    id: "comment-applied".to_owned(),
                    author: ReviewCommentAuthor::User,
                    body: "Applied".to_owned(),
                    created_at: "2026-03-17T10:00:00Z".to_owned(),
                    updated_at: "2026-03-17T10:00:00Z".to_owned(),
                }],
            },
            ReviewThread {
                id: "thread-dismissed".to_owned(),
                anchor: ReviewAnchor::ChangeSet,
                status: ReviewThreadStatus::Dismissed,
                comments: vec![ReviewThreadComment {
                    id: "comment-dismissed".to_owned(),
                    author: ReviewCommentAuthor::User,
                    body: "Dismissed".to_owned(),
                    created_at: "2026-03-17T10:00:00Z".to_owned(),
                    updated_at: "2026-03-17T10:00:00Z".to_owned(),
                }],
            },
        ],
    };

    let summary = summarize_review_document(&review);

    assert_eq!(summary.thread_count, 4);
    assert_eq!(summary.open_thread_count, 1);
    assert_eq!(summary.resolved_thread_count, 1);
    assert_eq!(summary.comment_count, 4);
}
