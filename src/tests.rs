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
    streaming_text_delta_start: Option<usize>,
    streaming_text_active: bool,
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
        if !self.streaming_text_active {
            self.streaming_text_delta_start = Some(self.text_deltas.len());
            self.streaming_text_active = true;
        }
        self.text_deltas.push(delta.to_owned());
        Ok(())
    }

    fn replace_streaming_text(&mut self, text: &str) -> Result<()> {
        if let Some(start) = self.streaming_text_delta_start {
            self.text_deltas.truncate(start);
            self.text_deltas.push(text.to_owned());
            self.streaming_text_active = true;
            return Ok(());
        }

        self.texts.push(text.to_owned());
        Ok(())
    }

    fn finish_streaming_text(&mut self) -> Result<()> {
        self.streaming_text_delta_start = None;
        self.streaming_text_active = false;
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
        orchestrator_templates_path: Arc::new(
            std::env::temp_dir().join(format!("termal-orchestrators-test-{}.json", Uuid::new_v4())),
        ),
        orchestrator_templates_lock: Arc::new(Mutex::new(())),
        state_events: broadcast::channel(16).0,
        delta_events: broadcast::channel(16).0,
        shared_codex_runtime: Arc::new(Mutex::new(None)),
        remote_registry: test_remote_registry(),
        stopping_orchestrator_ids: Arc::new(Mutex::new(HashSet::new())),
        stopping_orchestrator_session_ids: Arc::new(Mutex::new(HashMap::new())),
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

fn run_git_test_command_output(repo_root: &FsPath, args: &[&str]) -> String {
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

    String::from_utf8_lossy(&output.stdout).trim().to_owned()
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

struct TestKillChildProcessFailureGuard;

impl Drop for TestKillChildProcessFailureGuard {
    fn drop(&mut self) {
        set_test_kill_child_process_failure(None, None);
    }
}

fn force_test_kill_child_process_failure(
    process: &Arc<SharedChild>,
    label: &str,
) -> TestKillChildProcessFailureGuard {
    set_test_kill_child_process_failure(Some(label), Some(process));
    TestKillChildProcessFailureGuard
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
fn claude_streamed_text_replaces_divergent_final_text() {
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
                    "text": "Draft answer."
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
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "text",
                        "text": "Final answer."
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
        Some(Message::Text { text, .. }) if text == "Final answer."
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

async fn request_response(app: &Router, request: Request<Body>) -> axum::response::Response {
    app.clone()
        .oneshot(request)
        .await
        .expect("request should complete")
}
async fn next_sse_event<S>(stream: &mut std::pin::Pin<Box<S>>) -> String
where
    S: futures_core::Stream<Item = Result<axum::body::Bytes, axum::Error>>,
{
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut event = String::new();
        loop {
            let chunk = std::future::poll_fn(|cx| stream.as_mut().poll_next(cx))
                .await
                .expect("SSE chunk should arrive")
                .expect("SSE chunk should stream cleanly");
            event.push_str(
                std::str::from_utf8(chunk.as_ref()).expect("SSE chunk should be valid UTF-8"),
            );
            if event.contains("\n\n") || event.contains("\r\n\r\n") {
                return event;
            }
        }
    })
    .await
    .expect("SSE event should arrive before timeout")
}
fn parse_sse_event(raw: &str) -> (String, String) {
    let mut event_name = None;
    let mut data_lines = Vec::new();
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(value) = line.strip_prefix("event: ") {
            event_name = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("data: ") {
            data_lines.push(value.to_owned());
        }
    }
    (
        event_name.expect("SSE event should include a name"),
        data_lines.join("\n"),
    )
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

    drop(response);
    drop(created);
    drop(state);
    let _ = fs::remove_dir_all(&root);
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
fn killing_session_persists_removal_even_when_shared_codex_interrupt_fails() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let shared_runtime = SharedCodexRuntime {
        runtime_id: "runtime-1".to_owned(),
        input_tx: input_tx.clone(),
        process: process.clone(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
    };
    drop(input_rx);

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("test session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: "runtime-1".to_owned(),
            input_tx,
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: shared_runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    shared_runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-1".to_owned()),
                turn_id: Some("turn-1".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    shared_runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("thread-1".to_owned(), session_id.clone());

    let killed = state.kill_session(&session_id).unwrap();
    assert!(
        killed
            .sessions
            .iter()
            .all(|session| session.id != session_id)
    );

    let reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    assert!(
        reloaded_inner
            .sessions
            .iter()
            .all(|record| record.session.id != session_id)
    );
    assert!(
        wait_for_shared_child_exit_timeout(
            &process,
            Duration::from_millis(50),
            "shared Codex runtime"
        )
        .unwrap()
        .is_none()
    );
    assert!(
        !shared_runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned")
            .contains_key(&session_id)
    );
    assert!(
        !shared_runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .contains_key("thread-1")
    );

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn kill_session_route_returns_ok_when_shared_codex_interrupt_fails() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let shared_runtime = SharedCodexRuntime {
        runtime_id: "runtime-route".to_owned(),
        input_tx: input_tx.clone(),
        process: process.clone(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
    };
    drop(input_rx);

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("test session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: "runtime-route".to_owned(),
            input_tx,
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: shared_runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    shared_runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-route".to_owned()),
                turn_id: Some("turn-route".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    shared_runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("thread-route".to_owned(), session_id.clone());

    let app = app_router(state.clone());
    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/kill"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        response
            .sessions
            .iter()
            .all(|session| session.id != session_id)
    );
    assert!(
        wait_for_shared_child_exit_timeout(
            &process,
            Duration::from_millis(50),
            "shared Codex runtime"
        )
        .unwrap()
        .is_none()
    );

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn killing_shared_codex_session_does_not_reset_other_shared_sessions_when_interrupt_fails() {
    let state = test_app_state();
    let first_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Two".to_owned()),
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
    let second_session_id = created.session_id;
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let shared_runtime = SharedCodexRuntime {
        runtime_id: "runtime-shared".to_owned(),
        input_tx: input_tx.clone(),
        process: process.clone(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
    };
    drop(input_rx);

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        for session_id in [&first_session_id, &second_session_id] {
            let index = inner
                .find_session_index(session_id)
                .expect("test session should exist");
            inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                runtime_id: "runtime-shared".to_owned(),
                input_tx: input_tx.clone(),
                process: process.clone(),
                shared_session: Some(SharedCodexSessionHandle {
                    runtime: shared_runtime.clone(),
                    session_id: session_id.to_string(),
                }),
            });
            inner.sessions[index].session.status = SessionStatus::Active;
        }
    }

    shared_runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .extend([
            (
                first_session_id.clone(),
                SharedCodexSessionState {
                    thread_id: Some("thread-a".to_owned()),
                    turn_id: Some("turn-a".to_owned()),
                    ..SharedCodexSessionState::default()
                },
            ),
            (
                second_session_id.clone(),
                SharedCodexSessionState {
                    thread_id: Some("thread-b".to_owned()),
                    turn_id: Some("turn-b".to_owned()),
                    ..SharedCodexSessionState::default()
                },
            ),
        ]);
    shared_runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .extend([
            ("thread-a".to_owned(), first_session_id.clone()),
            ("thread-b".to_owned(), second_session_id.clone()),
        ]);

    let killed = state.kill_session(&first_session_id).unwrap();
    assert!(
        killed
            .sessions
            .iter()
            .all(|session| session.id != first_session_id)
    );
    assert!(
        killed
            .sessions
            .iter()
            .any(|session| session.id == second_session_id)
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let second_record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == second_session_id)
        .expect("second session should still exist");
    assert!(matches!(second_record.runtime, SessionRuntime::Codex(_)));
    assert_eq!(second_record.session.status, SessionStatus::Active);
    drop(inner);

    let shared_sessions = shared_runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned");
    assert!(!shared_sessions.contains_key(&first_session_id));
    assert!(shared_sessions.contains_key(&second_session_id));
    drop(shared_sessions);
    let thread_sessions = shared_runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned");
    assert!(!thread_sessions.contains_key("thread-a"));
    assert_eq!(
        thread_sessions.get("thread-b").map(String::as_str),
        Some(second_session_id.as_str())
    );
    drop(thread_sessions);
    assert!(
        wait_for_shared_child_exit_timeout(
            &process,
            Duration::from_millis(50),
            "shared Codex runtime"
        )
        .unwrap()
        .is_none()
    );

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn killing_local_codex_session_prevents_rediscovery_after_restart() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&session_id, "thread-killed".to_owned())
        .unwrap();

    let killed = state.kill_session(&session_id).unwrap();
    assert!(
        killed
            .sessions
            .iter()
            .all(|session| session.id != session_id)
    );

    let mut reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    assert!(
        reloaded_inner
            .ignored_discovered_codex_thread_ids
            .contains("thread-killed")
    );

    reloaded_inner.import_discovered_codex_threads(
        "/tmp",
        vec![DiscoveredCodexThread {
            approval_policy: Some(CodexApprovalPolicy::Never),
            archived: false,
            cwd: "/tmp".to_owned(),
            id: "thread-killed".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            title: "Killed thread".to_owned(),
        }],
    );

    assert!(
        reloaded_inner
            .sessions
            .iter()
            .all(|record| record.external_session_id.as_deref() != Some("thread-killed"))
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn setting_non_codex_external_session_id_does_not_clear_ignored_codex_thread() {
    let state = test_app_state();
    let killed_codex_session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&killed_codex_session_id, "thread-shared".to_owned())
        .unwrap();
    state.kill_session(&killed_codex_session_id).unwrap();

    let cursor_session_id = test_session_id(&state, Agent::Cursor);
    state
        .set_external_session_id(&cursor_session_id, "thread-shared".to_owned())
        .unwrap();

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .ignored_discovered_codex_thread_ids
            .contains("thread-shared")
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn import_discovered_codex_threads_prunes_stale_ignored_thread_ids() {
    let mut inner = StateInner::new();
    inner
        .ignored_discovered_codex_thread_ids
        .extend(["thread-live".to_owned(), "thread-stale".to_owned()]);

    inner.import_discovered_codex_threads(
        "/tmp/termal",
        vec![DiscoveredCodexThread {
            approval_policy: Some(CodexApprovalPolicy::Never),
            archived: false,
            cwd: "/tmp/termal".to_owned(),
            id: "thread-live".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            title: "Still around".to_owned(),
        }],
    );

    assert!(
        inner
            .ignored_discovered_codex_thread_ids
            .contains("thread-live")
    );
    assert!(
        !inner
            .ignored_discovered_codex_thread_ids
            .contains("thread-stale")
    );
    assert!(
        inner
            .sessions
            .iter()
            .all(|record| record.external_session_id.as_deref() != Some("thread-live"))
    );
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
        orchestrator_templates_path: state.orchestrator_templates_path.clone(),
        orchestrator_templates_lock: state.orchestrator_templates_lock.clone(),
        state_events: broadcast::channel(16).0,
        delta_events: broadcast::channel(16).0,
        shared_codex_runtime: Arc::new(Mutex::new(None)),
        remote_registry: test_remote_registry(),
        stopping_orchestrator_ids: Arc::new(Mutex::new(HashSet::new())),
        stopping_orchestrator_session_ids: Arc::new(Mutex::new(HashMap::new())),
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
fn discover_codex_threads_from_home_requires_optional_columns() {
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

    let err = discover_codex_threads_from_home(&codex_home, &[PathBuf::from("/tmp/project")])
        .expect_err("legacy threads schema should fail");
    let err_text = format!("{err:#}");
    assert!(
        err_text.contains("no such column"),
        "unexpected discovery error: {err_text}"
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

#[cfg(windows)]
#[test]
fn import_discovered_codex_threads_normalizes_legacy_local_verbatim_paths() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-discovered-verbatim-path-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let legacy_root = format!(r"\\?\{normalized_root}");

    let mut inner = StateInner::new();
    let project = inner.create_project(
        Some("TermAl".to_owned()),
        normalized_root.clone(),
        default_local_remote_id(),
    );
    inner.import_discovered_codex_threads(
        &normalized_root,
        vec![DiscoveredCodexThread {
            approval_policy: Some(CodexApprovalPolicy::Never),
            archived: false,
            cwd: legacy_root,
            id: "thread-legacy".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            reasoning_effort: None,
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            title: "Legacy thread".to_owned(),
        }],
    );

    assert_eq!(inner.projects.len(), 1);
    assert_eq!(inner.projects[0].root_path, normalized_root);
    let record = inner
        .sessions
        .iter()
        .find(|entry| entry.external_session_id.as_deref() == Some("thread-legacy"))
        .expect("legacy discovered thread should be imported");
    assert_eq!(record.session.workdir, normalized_root);
    assert_eq!(
        record.session.project_id.as_deref(),
        Some(project.id.as_str())
    );

    let _ = fs::remove_dir_all(project_root);
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
async fn create_session_route_returns_created_response() {
    let state = test_app_state();
    let initial_session_count = state.snapshot().sessions.len();
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "name": "Route Created Session",
        "workdir": "/tmp"
    }))
    .expect("create session route body should serialize");
    let (status, response): (StatusCode, CreateSessionResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/sessions")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(response.state.sessions.len(), initial_session_count + 1);
    let created_session = response
        .state
        .sessions
        .iter()
        .find(|session| session.id == response.session_id)
        .expect("created session should be present");
    assert_eq!(created_session.name, "Route Created Session");
    let expected_workdir = resolve_session_workdir("/tmp").expect("route workdir should normalize");
    assert_eq!(created_session.workdir, expected_workdir);
    assert_eq!(created_session.agent, Agent::Codex);
    let _ = fs::remove_file(state.persistence_path.as_path());
}
#[tokio::test]
async fn update_session_settings_route_updates_session_name() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "name": "Route Updated Session"
    }))
    .expect("settings route body should serialize");
    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/settings"))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.name, "Route Updated Session");
    let _ = fs::remove_file(state.persistence_path.as_path());
}
#[tokio::test]
async fn send_message_route_accepts_and_queues_prompt_for_busy_session() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
    }
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "text": "Queued route prompt",
        "expandedText": "Expanded queued route prompt"
    }))
    .expect("message route body should serialize");
    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/messages"))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("queued session should be present");
    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(session.pending_prompts.len(), 1);
    assert_eq!(session.pending_prompts[0].text, "Queued route prompt");
    assert_eq!(
        session.pending_prompts[0].expanded_text.as_deref(),
        Some("Expanded queued route prompt")
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}
#[tokio::test]
async fn submit_approval_route_updates_claude_session_and_delivers_runtime_response() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, input_rx) = test_claude_runtime_handle("claude-approval-route");
    let message_id = "approval-route-1".to_owned();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    }
    state
        .push_message(
            &session_id,
            Message::Approval {
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Claude needs approval".to_owned(),
                command: "Edit src/main.rs".to_owned(),
                command_language: None,
                detail: "Need to update the route tests.".to_owned(),
                decision: ApprovalDecision::Pending,
            },
        )
        .expect("approval message should be recorded");
    state
        .register_claude_pending_approval(
            &session_id,
            message_id.clone(),
            ClaudePendingApproval {
                permission_mode_for_session: Some("acceptEdits".to_owned()),
                request_id: "claude-route-request".to_owned(),
                tool_input: json!({
                    "path": "src/main.rs"
                }),
            },
        )
        .expect("pending Claude approval should be registered");
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "decision": "acceptedForSession"
    }))
    .expect("approval route body should serialize");
    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/approvals/{message_id}"))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated Claude session should be present");
    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(
        session.preview,
        approval_preview_text("Claude", ApprovalDecision::AcceptedForSession)
    );
    assert!(session.messages.iter().any(|message| matches!(
        message,
        Message::Approval { id, decision, .. }
            if id == &message_id && *decision == ApprovalDecision::AcceptedForSession
    )));
    match input_rx.recv_timeout(Duration::from_millis(50)) {
        Ok(ClaudeRuntimeCommand::SetPermissionMode(mode)) => {
            assert_eq!(mode, "acceptEdits");
        }
        Ok(_) => panic!("expected Claude permission-mode update"),
        Err(err) => panic!("Claude permission-mode update should arrive: {err}"),
    }
    match input_rx.recv_timeout(Duration::from_millis(50)) {
        Ok(ClaudeRuntimeCommand::PermissionResponse(ClaudePermissionDecision::Allow {
            request_id,
            updated_input,
        })) => {
            assert_eq!(request_id, "claude-route-request");
            assert_eq!(updated_input, json!({ "path": "src/main.rs" }));
        }
        Ok(_) => panic!("expected Claude permission response"),
        Err(err) => panic!("Claude permission response should arrive: {err}"),
    }
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert!(record.pending_claude_approvals.is_empty());
    drop(inner);
    let _ = fs::remove_file(state.persistence_path.as_path());
}
#[tokio::test]
async fn state_events_route_streams_initial_state_and_live_deltas() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());
    let response = request_response(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/events")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .expect("SSE route should set a content type");
    assert!(content_type.starts_with("text/event-stream"));
    let mut body = Box::pin(response.into_body().into_data_stream());
    let initial_event = next_sse_event(&mut body).await;
    let (initial_name, initial_data) = parse_sse_event(&initial_event);
    assert_eq!(initial_name, "state");
    let initial_state: StateResponse =
        serde_json::from_str(&initial_data).expect("initial SSE payload should parse");
    assert!(
        initial_state
            .sessions
            .iter()
            .any(|session| session.id == session_id)
    );
    let message_id = state.allocate_message_id();
    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Live delta".to_owned(),
                expanded_text: None,
            },
        )
        .expect("delta message should be recorded");
    let delta_event = next_sse_event(&mut body).await;
    let (delta_name, delta_data) = parse_sse_event(&delta_event);
    assert_eq!(delta_name, "delta");
    let delta: Value = serde_json::from_str(&delta_data).expect("delta SSE payload should parse");
    assert_eq!(delta["type"], "messageCreated");
    assert_eq!(delta["sessionId"], session_id);
    assert_eq!(delta["messageId"], message_id);
    assert_eq!(delta["message"]["type"], "text");
    assert_eq!(delta["message"]["text"], "Live delta");
    let _ = fs::remove_file(state.persistence_path.as_path());
}
#[tokio::test]
async fn state_events_route_streams_orchestrator_creation_state_and_live_orchestrator_deltas() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-events-route-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("events project root should exist");
    let project_id = create_test_project(&state, &project_root, "Events Orchestrator Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let app = app_router(state.clone());
    let response = request_response(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/events")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = Box::pin(response.into_body().into_data_stream());

    let initial_event = next_sse_event(&mut body).await;
    let (initial_name, initial_data) = parse_sse_event(&initial_event);
    assert_eq!(initial_name, "state");
    let initial_state: StateResponse =
        serde_json::from_str(&initial_data).expect("initial SSE payload should parse");
    assert!(initial_state.orchestrators.is_empty());

    let created = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
        })
        .expect("orchestrator instance should be created");
    let instance_id = created.orchestrator.id.clone();
    let created_session_ids = created
        .orchestrator
        .session_instances
        .iter()
        .map(|instance| instance.session_id.clone())
        .collect::<Vec<_>>();

    let created_event = next_sse_event(&mut body).await;
    let (created_name, created_data) = parse_sse_event(&created_event);
    assert_eq!(created_name, "state");
    let created_state: StateResponse =
        serde_json::from_str(&created_data).expect("create SSE payload should parse");
    let created_orchestrator = created_state
        .orchestrators
        .iter()
        .find(|instance| instance.id == instance_id)
        .expect("create SSE state should include the orchestrator instance");
    assert_eq!(
        created_orchestrator.status,
        OrchestratorInstanceStatus::Running
    );
    for session_id in &created_session_ids {
        assert!(
            created_state
                .sessions
                .iter()
                .any(|session| session.id == *session_id),
            "create SSE state should include orchestrator session {session_id}"
        );
    }

    state
        .pause_orchestrator_instance(&instance_id)
        .expect("pause route should update orchestrator state");

    let delta_event = next_sse_event(&mut body).await;
    let (delta_name, delta_data) = parse_sse_event(&delta_event);
    assert_eq!(delta_name, "delta");
    let delta: Value = serde_json::from_str(&delta_data).expect("delta SSE payload should parse");
    assert_eq!(delta["type"], "orchestratorsUpdated");
    assert!(
        delta["orchestrators"]
            .as_array()
            .is_some_and(|instances| instances.iter().any(|instance| {
                instance["id"] == Value::String(instance_id.clone())
                    && instance["status"] == Value::String("paused".to_owned())
            }))
    );

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
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
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-agent-final-suffix");

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
fn shared_codex_agent_message_final_event_replaces_divergent_streamed_text() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-agent-final-replace");
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
                "delta": "Hello from stream",
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
                "message": "Different final answer.",
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
    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Hello from stream"
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
    assert_eq!(session.preview, "Different final answer.");
    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Different final answer."
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

    assert_eq!(
        recorder.text_deltas,
        vec!["Hello".to_owned(), " from REPL.".to_owned()]
    );
    assert!(recorder.texts.is_empty());
    assert!(repl_state.turn_completed);
    assert!(repl_state.current_turn_id.is_none());
}

#[test]
fn repl_codex_streamed_agent_message_replaces_divergent_completed_text() {
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
            "delta": "Hello from stream"
        }
    });
    let completed = json!({
        "method": "item/completed",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "id": "item-1",
                "type": "agentMessage",
                "text": "Different final answer."
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
    assert_eq!(
        recorder.text_deltas,
        vec!["Different final answer.".to_owned()]
    );
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
fn clear_runtime_commits_revision_when_it_resets_state() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, _input_rx) = test_claude_runtime_handle("clear-runtime");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].runtime_reset_required = true;
        state.commit_locked(&mut inner).unwrap();
    }

    let baseline = state.snapshot().revision;
    state.clear_runtime(&session_id).unwrap();

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).unwrap();
        let record = &inner.sessions[index];
        assert!(matches!(record.runtime, SessionRuntime::None));
        assert!(!record.runtime_reset_required);
    }
    assert_eq!(state.snapshot().revision, baseline + 1);

    let stable_revision = state.snapshot().revision;
    state.clear_runtime(&session_id).unwrap();
    assert_eq!(state.snapshot().revision, stable_revision);
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
    assert!(!shared_sessions.contains_key("session-a"));
    assert!(!shared_sessions.contains_key("session-b"));
    assert!(shared_sessions.is_empty());
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
    assert!(session.external_session_id.is_none());
    assert!(session.codex_thread_state.is_none());
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
fn stop_session_detaches_shared_codex_session_when_interrupt_fails() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = SharedCodexRuntime {
        runtime_id: "shared-codex-stop-fail".to_owned(),
        input_tx,
        process: process.clone(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
    };
    drop(input_rx);

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-stop-fail".to_owned()),
                turn_id: Some("turn-stop-fail".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("thread-stop-fail".to_owned(), session_id.clone());

    let handle = CodexRuntimeHandle {
        runtime_id: runtime.runtime_id.clone(),
        input_tx: runtime.input_tx.clone(),
        process: process.clone(),
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
        set_record_external_session_id(
            &mut inner.sessions[index],
            Some("thread-stop-fail".to_owned()),
        );
    }

    let snapshot = state.stop_session(&session_id).unwrap();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("stopped session should remain present");
    assert_eq!(session.status, SessionStatus::Idle);
    assert!(session.external_session_id.is_none());
    assert!(session.codex_thread_state.is_none());
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
    assert!(record.external_session_id.is_none());
    assert!(record.session.external_session_id.is_none());
    assert!(record.session.codex_thread_state.is_none());
    drop(inner);

    let reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    let reloaded = reloaded_inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should persist");
    assert!(reloaded.external_session_id.is_none());
    assert!(reloaded.session.external_session_id.is_none());
    assert!(reloaded.session.codex_thread_state.is_none());

    assert!(
        !runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned")
            .contains_key(&session_id)
    );
    assert!(
        !runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .contains_key("thread-stop-fail")
    );

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn stop_session_dispatches_queued_prompt_after_shared_codex_interrupt_failure() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = SharedCodexRuntime {
        runtime_id: "shared-codex-stop-fail-queued".to_owned(),
        input_tx,
        process: process.clone(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
    };

    {
        let mut shared_runtime = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        *shared_runtime = Some(runtime.clone());
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-stop-fail-queued".to_owned()),
                turn_id: Some("turn-stop-fail-queued".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("thread-stop-fail-queued".to_owned(), session_id.clone());

    let handle = CodexRuntimeHandle {
        runtime_id: runtime.runtime_id.clone(),
        input_tx: runtime.input_tx.clone(),
        process: process.clone(),
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
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
        set_record_external_session_id(
            &mut inner.sessions[index],
            Some("thread-stop-fail-queued".to_owned()),
        );
        inner.sessions[index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::User,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-shared-stop-fail".to_owned(),
                    timestamp: stamp_now(),
                    text: "queued prompt after failed interrupt".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[index]);
    }

    let queued_session_id = session_id.clone();
    let command_thread = std::thread::spawn(move || {
        let interrupt = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Codex interrupt command should arrive");
        match interrupt {
            CodexRuntimeCommand::InterruptTurn {
                thread_id,
                turn_id,
                response_tx,
            } => {
                assert_eq!(thread_id, "thread-stop-fail-queued");
                assert_eq!(turn_id, "turn-stop-fail-queued");
                let _ = response_tx.send(Err("interrupt failed".to_owned()));
            }
            _ => panic!("expected Codex turn interrupt command"),
        }

        let prompt = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("queued Codex prompt should be dispatched");
        match prompt {
            CodexRuntimeCommand::Prompt {
                session_id,
                command,
            } => {
                assert_eq!(session_id, queued_session_id);
                assert_eq!(command.prompt, "queued prompt after failed interrupt");
                assert!(command.resume_thread_id.is_none());
            }
            _ => panic!("expected queued Codex prompt dispatch"),
        }
    });

    let snapshot = state.stop_session(&session_id).unwrap();
    command_thread
        .join()
        .expect("shared Codex command thread should join cleanly");

    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should remain present");
    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(session.preview, "queued prompt after failed interrupt");
    assert!(session.external_session_id.is_none());
    assert!(session.codex_thread_state.is_none());
    assert!(session.pending_prompts.is_empty());
    assert!(session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn stopped by user."
    )));
    assert!(session.messages.iter().any(|message| matches!(
        message,
        Message::Text {
            author: Author::You,
            text,
            ..
        } if text == "queued prompt after failed interrupt"
    )));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert_eq!(record.session.status, SessionStatus::Active);
    assert!(matches!(record.runtime, SessionRuntime::Codex(_)));
    assert!(!record.runtime_stop_in_progress);
    assert!(record.queued_prompts.is_empty());
    assert!(record.external_session_id.is_none());
    assert!(record.session.external_session_id.is_none());
    assert!(record.session.codex_thread_state.is_none());
    drop(inner);

    let reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    let reloaded = reloaded_inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should persist");
    assert!(reloaded.external_session_id.is_none());
    assert!(reloaded.session.external_session_id.is_none());
    assert!(reloaded.session.codex_thread_state.is_none());
    assert!(
        !runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .contains_key("thread-stop-fail-queued")
    );

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn stop_session_returns_an_error_when_a_dedicated_runtime_refuses_to_stop() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-fail".to_owned(),
        input_tx,
        process: process.clone(),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::User,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-stop-follow-up".to_owned(),
                    timestamp: stamp_now(),
                    text: "queued prompt".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[index]);
    }

    let baseline_revision = state.snapshot().revision;
    let mut state_events = state.subscribe_events();
    let _failure_guard = force_test_kill_child_process_failure(&process, "Claude");
    let error = match state.stop_session(&session_id) {
        Ok(_) => panic!("failed dedicated runtime kills should not be treated as clean stops"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to stop session `"));
    assert_eq!(state.snapshot().revision, baseline_revision);
    assert!(matches!(
        state_events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert_eq!(record.session.status, SessionStatus::Active);
    assert_eq!(record.session.preview, "Ready for a prompt.");
    assert!(matches!(record.runtime, SessionRuntime::Claude(_)));
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(record.session.pending_prompts.len(), 1);
    drop(inner);

    assert!(input_rx.recv_timeout(Duration::from_millis(100)).is_err());

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn stop_session_keeps_the_previous_state_visible_until_shutdown_completes() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-concurrent-read".to_owned(),
        input_tx,
        process: process.clone(),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
    }

    let stop_state = state.clone();
    let stop_session_id = session_id.clone();
    let stop_handle = std::thread::spawn(move || stop_state.stop_session(&stop_session_id));

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        {
            let inner = state.inner.lock().expect("state mutex poisoned");
            let record = inner
                .sessions
                .iter()
                .find(|record| record.session.id == session_id)
                .expect("Claude session should exist");
            if record.runtime_stop_in_progress {
                assert_eq!(record.session.status, SessionStatus::Active);
                assert_eq!(record.session.preview, "Streaming reply...");
                break;
            }
        }

        if std::time::Instant::now() >= deadline {
            panic!("stop_session did not enter the shutdown window in time");
        }

        std::thread::sleep(Duration::from_millis(5));
    }

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should still be visible while stopping");
    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(session.preview, "Streaming reply...");

    let stopped_snapshot = stop_handle
        .join()
        .expect("stop_session thread should join cleanly")
        .expect("stop_session should succeed");
    let stopped_session = stopped_snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("stopped session should remain present");
    assert_eq!(stopped_session.status, SessionStatus::Idle);
    assert_eq!(stopped_session.preview, "Turn stopped by user.");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert!(!record.runtime_stop_in_progress);
    assert!(matches!(record.runtime, SessionRuntime::None));
    drop(inner);

    assert!(input_rx.recv_timeout(Duration::from_millis(100)).is_err());

    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn stop_session_returns_conflict_when_already_stopping() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-conflict".to_owned(),
        input_tx,
        process,
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
    }

    let stop_state = state.clone();
    let stop_session_id = session_id.clone();
    let stop_handle = std::thread::spawn(move || stop_state.stop_session(&stop_session_id));

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let stop_in_progress = {
            let inner = state.inner.lock().expect("state mutex poisoned");
            inner
                .sessions
                .iter()
                .find(|record| record.session.id == session_id)
                .expect("Claude session should exist")
                .runtime_stop_in_progress
        };
        if stop_in_progress {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!("stop_session did not enter the shutdown window in time");
        }

        std::thread::sleep(Duration::from_millis(5));
    }

    let error = match state.stop_session(&session_id) {
        Ok(_) => panic!("a second stop should conflict while shutdown is in flight"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, "session is already stopping");

    let stopped_snapshot = stop_handle
        .join()
        .expect("stop_session thread should join cleanly")
        .expect("initial stop_session should succeed");
    let stopped_session = stopped_snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("stopped session should remain present");
    assert_eq!(stopped_session.status, SessionStatus::Idle);
    assert_eq!(stopped_session.preview, "Turn stopped by user.");

    assert!(input_rx.recv_timeout(Duration::from_millis(100)).is_err());

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn runtime_turn_callbacks_are_suppressed_while_stop_is_in_progress() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, input_rx) = test_claude_runtime_handle("claude-stop-callback-guard");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
        inner.sessions[index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::User,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-stop-callback-guard".to_owned(),
                    timestamp: stamp_now(),
                    text: "queued prompt".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[index]);
        inner.sessions[index].runtime_stop_in_progress = true;
    }

    let baseline_revision = state.snapshot().revision;
    let mut state_events = state.subscribe_events();

    state
        .fail_turn_if_runtime_matches(&session_id, &runtime_token, "reader failure")
        .expect("fail_turn_if_runtime_matches should succeed");
    state
        .note_turn_retry_if_runtime_matches(&session_id, &runtime_token, "Retrying Claude...")
        .expect("note_turn_retry_if_runtime_matches should succeed");
    state
        .mark_turn_error_if_runtime_matches(&session_id, &runtime_token, "runtime error")
        .expect("mark_turn_error_if_runtime_matches should succeed");
    state
        .finish_turn_ok_if_runtime_matches(&session_id, &runtime_token)
        .expect("finish_turn_ok_if_runtime_matches should succeed");

    assert_eq!(state.snapshot().revision, baseline_revision);
    assert!(matches!(
        state_events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert_eq!(record.session.status, SessionStatus::Active);
    assert_eq!(record.session.preview, "Streaming reply...");
    assert!(record.session.messages.is_empty());
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(record.session.pending_prompts.len(), 1);
    assert!(matches!(record.runtime, SessionRuntime::Claude(_)));
    assert!(record.runtime_stop_in_progress);
    assert_eq!(
        record.deferred_stop_callbacks,
        vec![
            DeferredStopCallback::TurnFailed("reader failure".to_owned()),
            DeferredStopCallback::TurnError("runtime error".to_owned()),
            DeferredStopCallback::TurnCompleted,
        ]
    );
    drop(inner);

    assert!(input_rx.recv_timeout(Duration::from_millis(100)).is_err());

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn codex_thread_state_updates_are_suppressed_while_stop_is_in_progress() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx) = test_codex_runtime_handle("codex-stop-thread-state-guard");
    let runtime_token = RuntimeToken::Codex(runtime.runtime_id.clone());
    state
        .set_external_session_id(&session_id, "thread-stop-guard".to_owned())
        .expect("Codex session should accept external thread ids");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
        inner.sessions[index].runtime_stop_in_progress = true;
    }

    let baseline_revision = state.snapshot().revision;
    let mut state_events = state.subscribe_events();

    state
        .set_codex_thread_state_if_runtime_matches(
            &session_id,
            &runtime_token,
            CodexThreadState::Archived,
        )
        .expect("set_codex_thread_state_if_runtime_matches should succeed");

    assert_eq!(state.snapshot().revision, baseline_revision);
    assert!(matches!(
        state_events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert_eq!(record.session.status, SessionStatus::Active);
    assert_eq!(record.session.preview, "Streaming reply...");
    assert_eq!(
        record.session.codex_thread_state,
        Some(CodexThreadState::Active)
    );
    assert!(matches!(record.runtime, SessionRuntime::Codex(_)));
    assert!(record.runtime_stop_in_progress);
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn runtime_exit_is_suppressed_while_stop_is_in_progress() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, _input_rx) = test_claude_runtime_handle("claude-stop-exit-guard");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
        inner.sessions[index].runtime_stop_in_progress = true;
    }

    let baseline_revision = state.snapshot().revision;
    let mut state_events = state.subscribe_events();

    state
        .handle_runtime_exit_if_matches(&session_id, &runtime_token, Some("runtime exited"))
        .expect("handle_runtime_exit_if_matches should succeed");

    assert_eq!(state.snapshot().revision, baseline_revision);
    assert!(matches!(
        state_events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert_eq!(record.session.status, SessionStatus::Active);
    assert_eq!(record.session.preview, "Streaming reply...");
    assert!(record.session.messages.is_empty());
    assert!(matches!(record.runtime, SessionRuntime::Claude(_)));
    assert!(record.runtime_stop_in_progress);
    assert_eq!(
        record.deferred_stop_callbacks,
        vec![DeferredStopCallback::RuntimeExited(Some(
            "runtime exited".to_owned()
        ))]
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn successful_stop_discards_deferred_callbacks() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-discard-deferred".to_owned(),
        input_tx,
        process: process.clone(),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
        inner.sessions[index].deferred_stop_callbacks = vec![DeferredStopCallback::TurnCompleted];
    }

    let snapshot = state.stop_session(&session_id).unwrap();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("stopped session should remain present");
    assert_eq!(session.status, SessionStatus::Idle);
    assert_eq!(session.preview, "Turn stopped by user.");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert_eq!(record.session.preview, "Turn stopped by user.");
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.runtime_stop_in_progress);
    assert!(record.deferred_stop_callbacks.is_empty());
    drop(inner);

    assert!(input_rx.recv_timeout(Duration::from_millis(100)).is_err());
    process.wait().unwrap();

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn failed_dedicated_stop_replays_deferred_turn_completion() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-replay".to_owned(),
        input_tx,
        process: process.clone(),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
    }

    // Pre-stage a deferred completion callback. In production this would be stored by
    // `finish_turn_ok_if_runtime_matches` arriving during the shutdown window; here we set it
    // directly because the forced kill failure completes synchronously with no observable window.
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].deferred_stop_callbacks = vec![DeferredStopCallback::TurnCompleted];
    }

    let _failure_guard = force_test_kill_child_process_failure(&process, "Claude");
    let error = match state.stop_session(&session_id) {
        Ok(_) => panic!("failed dedicated runtime kills should not be treated as clean stops"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to stop session `"));

    // The deferred callback should have been replayed: session should now be Idle with the
    // runtime detached, just as if `finish_turn_ok_if_runtime_matches` had run normally.
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert!(!record.runtime_stop_in_progress);
    assert!(record.deferred_stop_callbacks.is_empty());
    drop(inner);

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn failed_dedicated_stop_replays_deferred_runtime_exit() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-exit-replay".to_owned(),
        input_tx,
        process: process.clone(),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
    }

    // Pre-stage a deferred exit callback with an error message.
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].deferred_stop_callbacks = vec![DeferredStopCallback::RuntimeExited(
            Some("process crashed".to_owned()),
        )];
    }

    let _failure_guard = force_test_kill_child_process_failure(&process, "Claude");
    let error = match state.stop_session(&session_id) {
        Ok(_) => panic!("failed dedicated runtime kills should not be treated as clean stops"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);

    // The replayed exit callback should have transitioned the session to Error.
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert_eq!(record.session.status, SessionStatus::Error);
    assert!(record.session.preview.contains("process crashed"));
    assert!(!record.runtime_stop_in_progress);
    assert!(record.deferred_stop_callbacks.is_empty());
    assert!(matches!(record.runtime, SessionRuntime::None));
    drop(inner);

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn failed_dedicated_stop_replays_multiple_deferred_callbacks_in_order() {
    let expected_state = test_app_state();
    let expected_session_id = test_session_id(&expected_state, Agent::Claude);
    let expected_process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (expected_input_tx, _expected_input_rx) = mpsc::channel();
    let expected_runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-replay-order-expected".to_owned(),
        input_tx: expected_input_tx,
        process: expected_process.clone(),
    };
    let expected_token = RuntimeToken::Claude(expected_runtime.runtime_id.clone());

    let initial_message_count = {
        let mut inner = expected_state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&expected_session_id)
            .expect("Claude session should exist");
        let message_id = inner.next_message_id();
        inner.sessions[index].runtime = SessionRuntime::Claude(expected_runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Partial reply".to_owned();
        inner.sessions[index].session.messages.push(Message::Text {
            attachments: Vec::new(),
            id: message_id,
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: "Partial reply".to_owned(),
            expanded_text: None,
        });
        inner.sessions[index].session.messages.len()
    };

    expected_state
        .finish_turn_ok_if_runtime_matches(&expected_session_id, &expected_token)
        .expect("finish_turn_ok_if_runtime_matches should succeed");
    expected_state
        .handle_runtime_exit_if_matches(&expected_session_id, &expected_token, None)
        .expect("handle_runtime_exit_if_matches should succeed");

    let (expected_status, expected_preview, expected_message_count, expected_message_text) = {
        let inner = expected_state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == expected_session_id)
            .expect("Claude session should exist");
        let expected_message_text = match record.session.messages.last() {
            Some(Message::Text { text, .. }) => text.clone(),
            _ => panic!("Claude session should end with a text message"),
        };
        (
            record.session.status,
            record.session.preview.clone(),
            record.session.messages.len(),
            expected_message_text,
        )
    };
    assert_eq!(expected_status, SessionStatus::Idle);
    assert_eq!(expected_preview, "Partial reply");
    assert_eq!(expected_message_count, initial_message_count);
    assert_eq!(expected_message_text, "Partial reply");

    expected_process.kill().unwrap();
    expected_process.wait().unwrap();
    let _ = fs::remove_file(expected_state.persistence_path.as_path());

    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-replay-order".to_owned(),
        input_tx,
        process: process.clone(),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        let message_id = inner.next_message_id();
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Partial reply".to_owned();
        inner.sessions[index].session.messages.push(Message::Text {
            attachments: Vec::new(),
            id: message_id,
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: "Partial reply".to_owned(),
            expanded_text: None,
        });
        inner.sessions[index].deferred_stop_callbacks = vec![
            DeferredStopCallback::TurnCompleted,
            DeferredStopCallback::RuntimeExited(None),
        ];
    }

    let _failure_guard = force_test_kill_child_process_failure(&process, "Claude");
    let error = match state.stop_session(&session_id) {
        Ok(_) => panic!("failed dedicated runtime kills should not be treated as clean stops"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert_eq!(record.session.status, expected_status);
    assert_eq!(record.session.preview, expected_preview);
    assert_eq!(record.session.messages.len(), expected_message_count);
    assert!(matches!(
        record.session.messages.last(),
        Some(Message::Text { text, .. }) if text == &expected_message_text
    ));
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.runtime_stop_in_progress);
    assert!(record.deferred_stop_callbacks.is_empty());
    drop(inner);

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn failed_dedicated_stop_replays_runtime_exit_last_even_when_it_arrives_first() {
    let expected_state = test_app_state();
    let expected_session_id = test_session_id(&expected_state, Agent::Claude);
    let expected_process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (expected_input_tx, _expected_input_rx) = mpsc::channel();
    let expected_runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-replay-reversed-expected".to_owned(),
        input_tx: expected_input_tx,
        process: expected_process.clone(),
    };
    let expected_token = RuntimeToken::Claude(expected_runtime.runtime_id.clone());

    let initial_message_count = {
        let mut inner = expected_state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&expected_session_id)
            .expect("Claude session should exist");
        let message_id = inner.next_message_id();
        inner.sessions[index].runtime = SessionRuntime::Claude(expected_runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Partial reply".to_owned();
        inner.sessions[index].session.messages.push(Message::Text {
            attachments: Vec::new(),
            id: message_id,
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: "Partial reply".to_owned(),
            expanded_text: None,
        });
        inner.sessions[index].session.messages.len()
    };

    expected_state
        .finish_turn_ok_if_runtime_matches(&expected_session_id, &expected_token)
        .expect("finish_turn_ok_if_runtime_matches should succeed");
    expected_state
        .handle_runtime_exit_if_matches(&expected_session_id, &expected_token, None)
        .expect("handle_runtime_exit_if_matches should succeed");

    let (expected_status, expected_preview, expected_message_count, expected_message_text) = {
        let inner = expected_state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == expected_session_id)
            .expect("Claude session should exist");
        let expected_message_text = match record.session.messages.last() {
            Some(Message::Text { text, .. }) => text.clone(),
            _ => panic!("Claude session should end with a text message"),
        };
        (
            record.session.status,
            record.session.preview.clone(),
            record.session.messages.len(),
            expected_message_text,
        )
    };
    assert_eq!(expected_status, SessionStatus::Idle);
    assert_eq!(expected_preview, "Partial reply");
    assert_eq!(expected_message_count, initial_message_count);
    assert_eq!(expected_message_text, "Partial reply");

    expected_process.kill().unwrap();
    expected_process.wait().unwrap();
    let _ = fs::remove_file(expected_state.persistence_path.as_path());

    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-replay-reversed".to_owned(),
        input_tx,
        process: process.clone(),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        let message_id = inner.next_message_id();
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Partial reply".to_owned();
        inner.sessions[index].session.messages.push(Message::Text {
            attachments: Vec::new(),
            id: message_id,
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: "Partial reply".to_owned(),
            expanded_text: None,
        });
        inner.sessions[index].deferred_stop_callbacks = vec![
            DeferredStopCallback::RuntimeExited(None),
            DeferredStopCallback::TurnCompleted,
        ];
    }

    let _failure_guard = force_test_kill_child_process_failure(&process, "Claude");
    let error = match state.stop_session(&session_id) {
        Ok(_) => panic!("failed dedicated runtime kills should not be treated as clean stops"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert_eq!(record.session.status, expected_status);
    assert_eq!(record.session.preview, expected_preview);
    assert_eq!(record.session.messages.len(), expected_message_count);
    assert!(matches!(
        record.session.messages.last(),
        Some(Message::Text { text, .. }) if text == &expected_message_text
    ));
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.runtime_stop_in_progress);
    assert!(record.deferred_stop_callbacks.is_empty());
    drop(inner);

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
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
fn push_git_repo_updates_tracking_branch() {
    let root = std::env::temp_dir().join(format!("termal-git-push-{}", Uuid::new_v4()));
    let remote_root = root.join("remote.git");
    let repo_root = root.join("local");
    let remote_root_string = remote_root.to_string_lossy().into_owned();
    let repo_root_string = repo_root.to_string_lossy().into_owned();

    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&remote_root).unwrap();

    run_git_test_command(&remote_root, &["init", "--bare"]);
    run_git_test_command(
        &root,
        &[
            "clone",
            remote_root_string.as_str(),
            repo_root_string.as_str(),
        ],
    );
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);

    fs::write(repo_root.join("README.md"), "# Init\n").unwrap();
    run_git_test_command(&repo_root, &["add", "README.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);
    run_git_test_command(&repo_root, &["push", "-u", "origin", "HEAD"]);

    fs::write(repo_root.join("README.md"), "# Updated\n").unwrap();
    run_git_test_command(&repo_root, &["commit", "-am", "update"]);

    let response = push_git_repo(&repo_root).unwrap();
    let local_head = run_git_test_command_output(&repo_root, &["rev-parse", "HEAD"]);
    let remote_head = run_git_test_command_output(&remote_root, &["rev-parse", "HEAD"]);

    assert_eq!(local_head, remote_head);
    assert_eq!(response.status.ahead, 0);
    assert_eq!(response.status.behind, 0);
    assert!(response.summary.starts_with("Pushed "));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sync_git_repo_pulls_remote_changes() {
    let root = std::env::temp_dir().join(format!("termal-git-sync-{}", Uuid::new_v4()));
    let remote_root = root.join("remote.git");
    let repo_root = root.join("local");
    let peer_root = root.join("peer");
    let remote_root_string = remote_root.to_string_lossy().into_owned();
    let repo_root_string = repo_root.to_string_lossy().into_owned();
    let peer_root_string = peer_root.to_string_lossy().into_owned();

    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&remote_root).unwrap();

    run_git_test_command(&remote_root, &["init", "--bare"]);
    run_git_test_command(
        &root,
        &[
            "clone",
            remote_root_string.as_str(),
            repo_root_string.as_str(),
        ],
    );
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);

    fs::write(repo_root.join("README.md"), "# Init\n").unwrap();
    run_git_test_command(&repo_root, &["add", "README.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);
    run_git_test_command(&repo_root, &["push", "-u", "origin", "HEAD"]);

    run_git_test_command(
        &root,
        &[
            "clone",
            remote_root_string.as_str(),
            peer_root_string.as_str(),
        ],
    );
    run_git_test_command(&peer_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&peer_root, &["config", "user.name", "TermAl"]);
    fs::write(peer_root.join("README.md"), "# Peer\n").unwrap();
    run_git_test_command(&peer_root, &["commit", "-am", "peer update"]);
    run_git_test_command(&peer_root, &["push"]);

    let response = sync_git_repo(&repo_root).unwrap();
    let local_head = run_git_test_command_output(&repo_root, &["rev-parse", "HEAD"]);
    let remote_head = run_git_test_command_output(&remote_root, &["rev-parse", "HEAD"]);

    assert_eq!(
        fs::read_to_string(repo_root.join("README.md"))
            .unwrap()
            .replace("\r\n", "\n"),
        "# Peer\n",
    );
    assert_eq!(local_head, remote_head);
    assert_eq!(response.status.ahead, 0);
    assert_eq!(response.status.behind, 0);
    assert!(response.summary.starts_with("Synced "));

    fs::remove_dir_all(root).unwrap();
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
async fn api_router_sets_local_cors_headers() {
    let response = app_router(test_app_state())
        .oneshot(
            Request::builder()
                .uri("/api/health")
                .header(axum::http::header::ORIGIN, "http://127.0.0.1:8787")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("request should complete");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN),
        Some(&HeaderValue::from_static("http://127.0.0.1:8787")),
    );
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

#[tokio::test]
async fn read_file_returns_not_found_for_missing_project_file() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-project-file-missing-{}", Uuid::new_v4()));
    let missing_file = root.join("missing.rs");

    fs::create_dir_all(&root).unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Project Files".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();

    let error = match read_file(
        State(state),
        Query(FileQuery {
            path: missing_file.to_string_lossy().into_owned(),
            project_id: Some(project.project_id),
            session_id: None,
        }),
    )
    .await
    {
        Ok(_) => panic!("missing file read should fail"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert!(error.message.contains("file not found"));

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn read_directory_returns_not_found_for_missing_project_path() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!(
        "termal-project-directory-missing-{}",
        Uuid::new_v4()
    ));
    let missing_dir = root.join("missing");

    fs::create_dir_all(&root).unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Project Files".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();

    let error = match read_directory(
        State(state),
        Query(FileQuery {
            path: missing_dir.to_string_lossy().into_owned(),
            project_id: Some(project.project_id),
            session_id: None,
        }),
    )
    .await
    {
        Ok(_) => panic!("missing directory read should fail"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert!(error.message.contains("path not found"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn read_instruction_document_returns_not_found_for_missing_file() {
    let workdir =
        std::env::temp_dir().join(format!("termal-instruction-missing-{}", Uuid::new_v4()));
    let missing_file = workdir.join("AGENTS.md");

    fs::create_dir_all(&workdir).unwrap();

    let error = read_instruction_document(&missing_file, &workdir)
        .expect_err("missing instruction file should fail");

    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert!(error.message.contains("instruction file not found"));

    fs::remove_dir_all(workdir).unwrap();
}

#[tokio::test]
async fn read_file_rejects_content_over_size_limit() {
    let state = test_app_state();
    let root =
        std::env::temp_dir().join(format!("termal-project-file-read-limit-{}", Uuid::new_v4()));
    let oversized_file = root.join("big.txt");

    fs::create_dir_all(&root).unwrap();
    fs::write(&oversized_file, "a".repeat(MAX_FILE_CONTENT_BYTES + 1)).unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Project Files".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();

    let error = match read_file(
        State(state),
        Query(FileQuery {
            path: oversized_file.to_string_lossy().into_owned(),
            project_id: Some(project.project_id),
            session_id: None,
        }),
    )
    .await
    {
        Ok(_) => panic!("oversized read should fail"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("read limit"));

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn write_file_rejects_content_over_size_limit() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!(
        "termal-project-file-write-limit-{}",
        Uuid::new_v4()
    ));
    let output_file = root.join("generated").join("output.rs");
    let oversized_content = "b".repeat(MAX_FILE_CONTENT_BYTES + 1);

    fs::create_dir_all(&root).unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Project Files".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();

    let error = match write_file(
        State(state),
        Json(WriteFileRequest {
            path: output_file.to_string_lossy().into_owned(),
            content: oversized_content,
            project_id: Some(project.project_id),
            session_id: None,
        }),
    )
    .await
    {
        Ok(_) => panic!("oversized write should fail"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("write limit"));
    assert!(!output_file.exists());

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

fn persisted_state_load_error_after_mutation<F>(inner: StateInner, mutate: F) -> String
where
    F: FnOnce(&mut Value),
{
    let path =
        std::env::temp_dir().join(format!("termal-state-load-error-{}.json", Uuid::new_v4()));
    persist_state(&path, &inner).expect("persisted state should be written");

    let mut encoded: Value = serde_json::from_slice(&fs::read(&path).unwrap())
        .expect("persisted state should deserialize");
    mutate(&mut encoded);
    fs::write(&path, serde_json::to_vec(&encoded).unwrap()).expect("persisted state should update");

    let err = match load_state(&path) {
        Ok(_) => panic!("mutated persisted state should fail to load"),
        Err(err) => err,
    };
    let _ = fs::remove_file(path);
    format!("{err:#}")
}

#[cfg(windows)]
#[test]
fn persisted_state_normalizes_legacy_local_verbatim_paths() {
    let project_root =
        std::env::temp_dir().join(format!("termal-legacy-verbatim-path-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let legacy_root = format!(r"\\?\{normalized_root}");
    let path = std::env::temp_dir().join(format!(
        "termal-legacy-verbatim-state-{}.json",
        Uuid::new_v4()
    ));

    let mut inner = StateInner::new();
    let project = inner.create_project(None, normalized_root.clone(), default_local_remote_id());
    inner.create_session(
        Agent::Claude,
        Some("Claude".to_owned()),
        normalized_root.clone(),
        Some(project.id),
        None,
    );
    persist_state(&path, &inner).expect("persisted state should be written");

    let mut encoded: Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).expect("state should deserialize");
    encoded["projects"][0]["rootPath"] = Value::String(legacy_root.clone());
    encoded["sessions"][0]["session"]["workdir"] = Value::String(legacy_root);
    fs::write(&path, serde_json::to_vec(&encoded).unwrap()).expect("persisted state should update");

    let loaded = load_state(&path)
        .expect("persisted state should load")
        .expect("persisted state should exist");
    assert_eq!(loaded.projects[0].root_path, normalized_root);
    assert_eq!(loaded.sessions[0].session.workdir, normalized_root);

    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(project_root);
}

#[cfg(windows)]
#[test]
fn persisted_state_normalizes_legacy_workspace_layout_paths() {
    let project_root =
        std::env::temp_dir().join(format!("termal-layout-verbatim-path-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let legacy_root = format!(r"\\?\{normalized_root}");
    let normalized_file = format!(r"{normalized_root}\src\main.rs");
    let legacy_file = format!(r"\\?\{normalized_file}");
    let path = std::env::temp_dir().join(format!(
        "termal-layout-verbatim-state-{}.json",
        Uuid::new_v4()
    ));

    let mut inner = StateInner::new();
    inner.workspace_layouts.insert(
        "workspace-1".to_owned(),
        WorkspaceLayoutDocument {
            id: "workspace-1".to_owned(),
            revision: 1,
            updated_at: "2026-04-01 12:00:00".to_owned(),
            control_panel_side: WorkspaceControlPanelSide::Left,
            theme_id: None,
            style_id: None,
            font_size_px: None,
            editor_font_size_px: None,
            density_percent: None,
            workspace: json!({
                "root": {
                    "type": "pane",
                    "paneId": "pane-a"
                },
                "panes": [{
                    "id": "pane-a",
                    "tabs": [
                        {
                            "id": "tab-files",
                            "kind": "filesystem",
                            "rootPath": legacy_root,
                            "originSessionId": serde_json::Value::Null
                        },
                        {
                            "id": "tab-git",
                            "kind": "gitStatus",
                            "workdir": format!(r"\\?\{normalized_root}"),
                            "originSessionId": serde_json::Value::Null
                        },
                        {
                            "id": "tab-debug",
                            "kind": "instructionDebugger",
                            "workdir": format!(r"\\?\{normalized_root}"),
                            "originSessionId": serde_json::Value::Null
                        },
                        {
                            "id": "tab-source",
                            "kind": "source",
                            "path": legacy_file,
                            "originSessionId": serde_json::Value::Null
                        },
                        {
                            "id": "tab-diff",
                            "kind": "diffPreview",
                            "changeType": "edit",
                            "diff": "-before\n+after",
                            "diffMessageId": "message-1",
                            "filePath": format!(r"\\?\{normalized_file}"),
                            "originSessionId": serde_json::Value::Null,
                            "summary": "Updated file"
                        }
                    ],
                    "activeTabId": "tab-files",
                    "activeSessionId": serde_json::Value::Null,
                    "viewMode": "filesystem",
                    "lastSessionViewMode": "session",
                    "sourcePath": format!(r"\\?\{normalized_file}")
                }],
                "activePaneId": "pane-a"
            }),
        },
    );
    persist_state(&path, &inner).expect("persisted state should be written");

    let loaded = load_state(&path)
        .expect("persisted state should load")
        .expect("persisted state should exist");
    let layout = loaded
        .workspace_layouts
        .get("workspace-1")
        .expect("workspace layout should load");
    assert_eq!(
        layout
            .workspace
            .pointer("/panes/0/sourcePath")
            .and_then(Value::as_str),
        Some(normalized_file.as_str())
    );
    assert_eq!(
        layout
            .workspace
            .pointer("/panes/0/tabs/0/rootPath")
            .and_then(Value::as_str),
        Some(normalized_root.as_str())
    );
    assert_eq!(
        layout
            .workspace
            .pointer("/panes/0/tabs/1/workdir")
            .and_then(Value::as_str),
        Some(normalized_root.as_str())
    );
    assert_eq!(
        layout
            .workspace
            .pointer("/panes/0/tabs/2/workdir")
            .and_then(Value::as_str),
        Some(normalized_root.as_str())
    );
    assert_eq!(
        layout
            .workspace
            .pointer("/panes/0/tabs/3/path")
            .and_then(Value::as_str),
        Some(normalized_file.as_str())
    );
    assert_eq!(
        layout
            .workspace
            .pointer("/panes/0/tabs/4/filePath")
            .and_then(Value::as_str),
        Some(normalized_file.as_str())
    );

    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(project_root);
}

#[test]
fn app_state_bootstrap_normalizes_legacy_local_verbatim_workdir() {
    let project_root =
        std::env::temp_dir().join(format!("termal-bootstrap-verbatim-path-{}", Uuid::new_v4()));
    let state_root =
        std::env::temp_dir().join(format!("termal-bootstrap-state-root-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&state_root).expect("state root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let legacy_root = format!(r"\\?\{normalized_root}");
    let persistence_path = state_root.join("sessions.json");
    let orchestrator_templates_path = state_root.join("orchestrators.json");

    let state = AppState::new_with_paths(
        legacy_root,
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");

    assert_eq!(state.default_workdir, normalized_root);
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.projects.len(), 1);
    assert_eq!(inner.projects[0].root_path, normalized_root);
    let bootstrapped_sessions = inner
        .sessions
        .iter()
        .filter(|record| matches!(record.session.name.as_str(), "Codex Live" | "Claude Live"))
        .collect::<Vec<_>>();
    assert_eq!(bootstrapped_sessions.len(), 2);
    assert!(
        bootstrapped_sessions
            .iter()
            .all(|record| record.session.workdir == normalized_root)
    );
    drop(inner);
    drop(state);

    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(project_root);
}

#[cfg(not(windows))]
#[test]
fn persisted_state_preserves_significant_local_path_spaces() {
    let project_root =
        std::env::temp_dir().join(format!("termal-significant-path-space-{} ", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let path = std::env::temp_dir().join(format!(
        "termal-significant-path-space-state-{}.json",
        Uuid::new_v4()
    ));

    assert!(normalized_root.ends_with(' '));

    let mut inner = StateInner::new();
    let project = inner.create_project(None, normalized_root.clone(), default_local_remote_id());
    inner.create_session(
        Agent::Claude,
        Some("Claude".to_owned()),
        normalized_root.clone(),
        Some(project.id),
        None,
    );
    persist_state(&path, &inner).expect("persisted state should be written");

    let loaded = load_state(&path)
        .expect("persisted state should load")
        .expect("persisted state should exist");
    assert_eq!(loaded.projects[0].root_path, normalized_root);
    assert_eq!(loaded.sessions[0].session.workdir, normalized_root);

    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(project_root);
}

#[test]
fn persisted_state_requires_projects() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Codex,
        Some("Migrated".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        encoded
            .as_object_mut()
            .expect("persisted state should be an object")
            .remove("projects");
    });

    assert!(
        err_text.contains("missing field `projects`"),
        "unexpected load_state error: {err_text}"
    );
}

#[test]
fn persisted_state_requires_next_project_number() {
    let inner = StateInner::new();

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        encoded
            .as_object_mut()
            .expect("persisted state should be an object")
            .remove("nextProjectNumber");
    });

    assert!(
        err_text.contains("missing field `nextProjectNumber`"),
        "unexpected load_state error: {err_text}"
    );
}

#[test]
fn persisted_state_requires_project_remote_id() {
    let mut inner = StateInner::new();
    inner.create_project(None, "/tmp".to_owned(), default_local_remote_id());

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        encoded["projects"]
            .as_array_mut()
            .expect("persisted projects should be an array")[0]
            .as_object_mut()
            .expect("persisted project should be an object")
            .remove("remoteId");
    });

    assert!(
        err_text.contains("missing field `remoteId`"),
        "unexpected load_state error: {err_text}"
    );
}

#[test]
fn persisted_state_requires_valid_remotes() {
    let inner = StateInner::new();

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        encoded["preferences"]["remotes"] = json!([
            {
                "id": "local",
                "name": "Local",
                "transport": "local",
                "enabled": true
            },
            {
                "id": "ssh-1",
                "name": "pop-os",
                "transport": "ssh",
                "enabled": true,
                "host": "pop-os.local",
                "port": 22,
                "user": "greg"
            },
            {
                "id": "ssh-1",
                "name": "backup",
                "transport": "ssh",
                "enabled": true,
                "host": "backup.local",
                "port": 22,
                "user": "greg"
            }
        ]);
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("duplicate remote id `ssh-1`"),
        "unexpected load_state error: {err_text}"
    );
}

#[test]
fn persisted_state_requires_cursor_mode() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Cursor,
        Some("Cursor".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        let session = encoded["sessions"]
            .as_array_mut()
            .expect("persisted sessions should be an array")[0]["session"]
            .as_object_mut()
            .expect("persisted session should be an object");
        session.remove("cursorMode");
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("missing cursorMode"),
        "unexpected load_state error: {err_text}"
    );
}

#[test]
fn persisted_state_requires_claude_settings() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Claude,
        Some("Claude".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        let session = encoded["sessions"]
            .as_array_mut()
            .expect("persisted sessions should be an array")[0]["session"]
            .as_object_mut()
            .expect("persisted session should be an object");
        session.remove("claudeApprovalMode");
        session.remove("claudeEffort");
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("missing claudeApprovalMode"),
        "unexpected load_state error: {err_text}"
    );
}

#[test]
fn persisted_state_requires_gemini_approval_mode() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Gemini,
        Some("Gemini".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        let session = encoded["sessions"]
            .as_array_mut()
            .expect("persisted sessions should be an array")[0]["session"]
            .as_object_mut()
            .expect("persisted session should be an object");
        session.remove("geminiApprovalMode");
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("missing geminiApprovalMode"),
        "unexpected load_state error: {err_text}"
    );
}

#[test]
fn persisted_state_requires_codex_prompt_fields() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Codex,
        Some("Codex".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        let session = encoded["sessions"]
            .as_array_mut()
            .expect("persisted sessions should be an array")[0]["session"]
            .as_object_mut()
            .expect("persisted session should be an object");
        session.remove("approvalPolicy");
        session.remove("reasoningEffort");
        session.remove("sandboxMode");
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("missing approvalPolicy"),
        "unexpected load_state error: {err_text}"
    );
}

#[test]
fn persisted_state_requires_codex_thread_state_for_live_threads() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Codex,
        Some("Codex".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        let entry = encoded["sessions"]
            .as_array_mut()
            .expect("persisted sessions should be an array")[0]
            .as_object_mut()
            .expect("persisted session record should be an object");
        entry.insert(
            "externalSessionId".to_owned(),
            Value::String("thread-live".to_owned()),
        );
        let session = entry["session"]
            .as_object_mut()
            .expect("persisted session should be an object");
        session.insert(
            "externalSessionId".to_owned(),
            Value::String("thread-live".to_owned()),
        );
        session.remove("codexThreadState");
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("missing codexThreadState"),
        "unexpected load_state error: {err_text}"
    );
}

#[test]
fn persisted_state_requires_queued_prompt_source() {
    let path = std::env::temp_dir().join(format!(
        "termal-queued-prompt-source-required-{}",
        Uuid::new_v4()
    ));
    let mut inner = StateInner::new();
    let record = inner.create_session(
        Agent::Codex,
        Some("Queued".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let session_id = record.session.id.clone();
    let index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    queue_prompt_on_record(
        &mut inner.sessions[index],
        PendingPrompt {
            attachments: Vec::new(),
            id: "queued-prompt-1".to_owned(),
            timestamp: stamp_now(),
            text: "queued prompt".to_owned(),
            expanded_text: None,
        },
        Vec::new(),
    );
    persist_state(&path, &inner).unwrap();

    let mut encoded: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let sessions = encoded["sessions"]
        .as_array_mut()
        .expect("persisted sessions should be an array");
    let queued_prompts = sessions[0]["queuedPrompts"]
        .as_array_mut()
        .expect("persisted queued prompts should be an array");
    queued_prompts[0]
        .as_object_mut()
        .expect("queued prompt should be an object")
        .remove("source");
    fs::write(&path, serde_json::to_vec(&encoded).unwrap()).unwrap();

    let err = match load_state(&path) {
        Ok(_) => panic!("persisted state without queued prompt source should fail"),
        Err(err) => err,
    };
    let err_text = format!("{err:#}");
    assert!(
        err_text.contains("missing field `source`"),
        "unexpected load_state error: {err_text}"
    );

    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn create_orchestrator_instance_route_uses_template_project_when_request_project_id_is_empty()
{
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-route-empty-project-id-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("route fallback project root should exist");
    let project_id = create_test_project(&state, &project_root, "Route Fallback Project");
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(project_id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;
    let template_id = template.id.clone();
    let template_session_count = template.sessions.len();

    let app = app_router(state);
    let (status, response): (StatusCode, CreateOrchestratorInstanceResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/orchestrators")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "templateId": template_id,
                    "projectId": "",
                }))
                .expect("request body should serialize"),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(response.orchestrator.template_id, template.id);
    assert_eq!(response.orchestrator.project_id, project_id);
    assert_eq!(
        response
            .orchestrator
            .template_snapshot
            .project_id
            .as_deref(),
        Some(response.orchestrator.project_id.as_str())
    );
    assert_eq!(
        response.orchestrator.session_instances.len(),
        template_session_count
    );
    let _ = fs::remove_dir_all(project_root);
}

#[tokio::test]
async fn orchestrator_lifecycle_routes_update_state_and_stop_active_sessions() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-lifecycle-route-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("route project root should exist");
    let project_id = create_test_project(&state, &project_root, "Route Orchestrator Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let (runtime, _input_rx) = test_claude_runtime_handle("route-orchestrator-stop");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        inner.sessions[planner_index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[planner_index].session.status = SessionStatus::Active;
        inner.sessions[planner_index].active_turn_start_message_count =
            Some(inner.sessions[planner_index].session.messages.len());
        inner.sessions[planner_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-orchestrator-stop-follow-up".to_owned(),
                    timestamp: stamp_now(),
                    text: "queued orchestrator follow-up".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[planner_index]);
    }

    let app = app_router(state.clone());
    let (pause_status, pause_response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/orchestrators/{instance_id}/pause"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(pause_status, StatusCode::OK);
    let paused = pause_response
        .orchestrators
        .iter()
        .find(|instance| instance.id == instance_id)
        .expect("paused orchestrator should be present");
    assert_eq!(paused.status, OrchestratorInstanceStatus::Paused);

    let (resume_status, resume_response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/orchestrators/{instance_id}/resume"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resume_status, StatusCode::OK);
    let resumed = resume_response
        .orchestrators
        .iter()
        .find(|instance| instance.id == instance_id)
        .expect("resumed orchestrator should be present");
    assert_eq!(resumed.status, OrchestratorInstanceStatus::Running);

    let (stop_status, stop_response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/orchestrators/{instance_id}/stop"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(stop_status, StatusCode::OK);
    let stopped = stop_response
        .orchestrators
        .iter()
        .find(|instance| instance.id == instance_id)
        .expect("stopped orchestrator should be present");
    assert_eq!(stopped.status, OrchestratorInstanceStatus::Stopped);
    assert!(stopped.pending_transitions.is_empty());
    assert!(stopped.completed_at.is_some());

    let planner_session = stop_response
        .sessions
        .iter()
        .find(|session| session.id == planner_session_id)
        .expect("planner session should still be present");
    assert_eq!(planner_session.status, SessionStatus::Idle);
    assert!(planner_session.pending_prompts.is_empty());

    let inner = state.inner.lock().expect("state mutex poisoned");
    let planner_record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == planner_session_id)
        .expect("planner session should still exist");
    assert_eq!(planner_record.session.status, SessionStatus::Idle);
    assert!(matches!(planner_record.runtime, SessionRuntime::None));
    assert!(planner_record.queued_prompts.is_empty());
    assert!(planner_record.session.pending_prompts.is_empty());
    drop(inner);
    assert!(planner_session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn stopped by user."
    )));

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}
#[tokio::test]
async fn orchestrator_stop_route_preserves_running_state_when_a_child_stop_fails() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-failure-route-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("route project root should exist");
    let project_id = create_test_project(&state, &project_root, "Route Orchestrator Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let reviewer_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "reviewer")
        .expect("reviewer session should be mapped")
        .session_id
        .clone();
    let failing_process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (planner_input_tx, _planner_input_rx) = mpsc::channel();
    let planner_runtime = ClaudeRuntimeHandle {
        runtime_id: "route-orchestrator-stop-fail".to_owned(),
        input_tx: planner_input_tx,
        process: failing_process.clone(),
    };
    let (reviewer_runtime, _reviewer_input_rx) =
        test_claude_runtime_handle("route-orchestrator-stop-reviewer");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        for (session_id, runtime) in [
            (
                planner_session_id.clone(),
                SessionRuntime::Claude(planner_runtime),
            ),
            (
                reviewer_session_id.clone(),
                SessionRuntime::Claude(reviewer_runtime),
            ),
        ] {
            let index = inner
                .find_session_index(&session_id)
                .expect("orchestrator session should exist");
            inner.sessions[index].runtime = runtime;
            inner.sessions[index].session.status = SessionStatus::Active;
            inner.sessions[index].active_turn_start_message_count =
                Some(inner.sessions[index].session.messages.len());
        }
    }

    let app = app_router(state.clone());
    let failure_guard = force_test_kill_child_process_failure(&failing_process, "Claude");
    let response = request_response(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/orchestrators/{instance_id}/stop"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body should read");
    let error: Value = serde_json::from_slice(&body).expect("error response should parse");
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|message| message.contains("failed to stop session `"))
    );
    drop(failure_guard);

    let snapshot = state.snapshot();
    let instance = snapshot
        .orchestrators
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("orchestrator should still be present");
    assert_eq!(instance.status, OrchestratorInstanceStatus::Running);
    assert!(instance.completed_at.is_none());

    let planner_session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == planner_session_id)
        .expect("planner session should still be present");
    assert_eq!(planner_session.status, SessionStatus::Active);

    let reviewer_session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == reviewer_session_id)
        .expect("reviewer session should still be present");
    assert_eq!(reviewer_session.status, SessionStatus::Idle);
    assert!(reviewer_session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn stopped by user."
    )));

    let reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    let reloaded_instance = reloaded_inner
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("persisted orchestrator should still exist");
    assert_eq!(
        reloaded_instance.status,
        OrchestratorInstanceStatus::Running
    );
    assert!(reloaded_instance.completed_at.is_none());
    let reloaded_reviewer = reloaded_inner
        .sessions
        .iter()
        .find(|record| record.session.id == reviewer_session_id)
        .expect("persisted reviewer session should exist");
    assert_eq!(reloaded_reviewer.session.status, SessionStatus::Idle);

    let _ = failing_process.kill();
    let _ = failing_process.wait();
    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn aborted_stop_cleanup_preserves_child_work_when_child_stop_persist_fails() {
    let mut state = test_app_state();
    let original_persistence_path = state.persistence_path.clone();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-cleanup-{}",
        Uuid::new_v4()
    ));
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-cleanup-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");
    let project_id = create_test_project(&state, &project_root, "Persist Failure Cleanup");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (builder_runtime, _builder_input_rx) =
        test_claude_runtime_handle("orchestrator-stop-persist-failure-builder");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let instance_index = inner
            .orchestrator_instances
            .iter()
            .position(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        inner.sessions[builder_index].runtime = SessionRuntime::Claude(builder_runtime);
        inner.sessions[builder_index].session.status = SessionStatus::Active;
        inner.sessions[builder_index].active_turn_start_message_count =
            Some(inner.sessions[builder_index].session.messages.len());
        inner.sessions[builder_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-persist-failure-cleanup-builder".to_owned(),
                    timestamp: stamp_now(),
                    text: "builder work that should survive aborted cleanup".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[builder_index]);
        let completion_revision = inner.revision.saturating_add(1);
        inner.orchestrator_instances[instance_index]
            .pending_transitions
            .push(PendingTransition {
                id: format!("pending-transition-{}", Uuid::new_v4()),
                transition_id: "planner-to-builder".to_owned(),
                source_session_id: planner_session_id.clone(),
                destination_session_id: builder_session_id.clone(),
                completion_revision,
                rendered_prompt: "builder pending work should survive aborted cleanup".to_owned(),
                created_at: stamp_orchestrator_template_now(),
            });
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .begin_orchestrator_stop(&instance_id)
        .expect("stop should be marked in progress");
    state.persistence_path = Arc::new(failing_persistence_path.clone());

    let error = state
        .stop_session_with_options(
            &builder_session_id,
            StopSessionOptions {
                dispatch_queued_prompts_on_success: false,
                orchestrator_stop_instance_id: Some(instance_id.clone()),
            },
        )
        .err()
        .expect("persist failures should abort child stop persistence");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to persist session state"));

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let instance = inner
            .orchestrator_instances
            .iter()
            .find(|candidate| candidate.id == instance_id)
            .expect("orchestrator should still exist");
        assert!(instance.stopped_session_ids_during_stop.is_empty());
    }
    assert!(
        state
            .stopping_orchestrator_session_ids
            .lock()
            .expect("orchestrator stop session mutex poisoned")
            .get(&instance_id)
            .is_some_and(|session_ids| session_ids.is_empty())
    );

    state.persistence_path = original_persistence_path.clone();
    state
        .prune_pending_transitions_for_stopped_orchestrator_sessions(&instance_id)
        .expect("aborted cleanup should preserve work for uncommitted child stops");

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let instance = inner
            .orchestrator_instances
            .iter()
            .find(|candidate| candidate.id == instance_id)
            .expect("orchestrator should still exist");
        assert_eq!(instance.status, OrchestratorInstanceStatus::Running);
        assert!(!instance.stop_in_progress);
        assert!(instance.active_session_ids_during_stop.is_none());
        assert!(instance.stopped_session_ids_during_stop.is_empty());
        assert_eq!(instance.pending_transitions.len(), 1);
        assert_eq!(
            instance.pending_transitions[0].destination_session_id,
            builder_session_id
        );
        let builder = inner
            .sessions
            .iter()
            .find(|record| record.session.id == builder_session_id)
            .expect("builder session should still exist");
        assert_eq!(builder.queued_prompts.len(), 1);
        assert_eq!(builder.session.pending_prompts.len(), 1);
    }

    let reloaded_inner = load_state(original_persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    let reloaded_instance = reloaded_inner
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("persisted orchestrator should still exist");
    assert_eq!(
        reloaded_instance.status,
        OrchestratorInstanceStatus::Running
    );
    assert!(!reloaded_instance.stop_in_progress);
    assert!(reloaded_instance.active_session_ids_during_stop.is_none());
    assert!(reloaded_instance.stopped_session_ids_during_stop.is_empty());
    assert_eq!(reloaded_instance.pending_transitions.len(), 1);
    assert_eq!(
        reloaded_instance.pending_transitions[0].destination_session_id,
        builder_session_id
    );
    let reloaded_builder = reloaded_inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("persisted builder session should still exist");
    assert_eq!(reloaded_builder.queued_prompts.len(), 1);
    assert_eq!(reloaded_builder.session.pending_prompts.len(), 1);

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(original_persistence_path.as_path());
    let _ = fs::remove_dir_all(failing_persistence_path);
}

#[test]
fn aborted_stop_resume_does_not_redispatch_child_after_child_stop_persist_fails() {
    let mut state = test_app_state();
    let original_persistence_path = state.persistence_path.clone();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-resume-{}",
        Uuid::new_v4()
    ));
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-resume-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");
    let project_id = create_test_project(&state, &project_root, "Persist Failure Resume");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (builder_runtime, _builder_input_rx) =
        test_claude_runtime_handle("orchestrator-stop-persist-failure-resume-builder");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let instance_index = inner
            .orchestrator_instances
            .iter()
            .position(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        inner.sessions[builder_index].runtime = SessionRuntime::Claude(builder_runtime);
        inner.sessions[builder_index].session.status = SessionStatus::Active;
        inner.sessions[builder_index].active_turn_start_message_count =
            Some(inner.sessions[builder_index].session.messages.len());
        let completion_revision = inner.revision.saturating_add(1);
        inner.orchestrator_instances[instance_index]
            .pending_transitions
            .push(PendingTransition {
                id: format!("pending-transition-{}", Uuid::new_v4()),
                transition_id: "planner-to-builder".to_owned(),
                source_session_id: planner_session_id.clone(),
                destination_session_id: builder_session_id.clone(),
                completion_revision,
                rendered_prompt: "builder pending work should remain pending after resume"
                    .to_owned(),
                created_at: stamp_orchestrator_template_now(),
            });
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .begin_orchestrator_stop(&instance_id)
        .expect("stop should be marked in progress");
    state.persistence_path = Arc::new(failing_persistence_path.clone());

    let error = state
        .stop_session_with_options(
            &builder_session_id,
            StopSessionOptions {
                dispatch_queued_prompts_on_success: false,
                orchestrator_stop_instance_id: Some(instance_id.clone()),
            },
        )
        .err()
        .expect("persist failures should abort child stop persistence");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to persist session state"));

    state.persistence_path = original_persistence_path.clone();
    state
        .prune_pending_transitions_for_stopped_orchestrator_sessions(&instance_id)
        .expect("aborted cleanup should preserve pending child work");
    state.finish_orchestrator_stop(&instance_id);
    state
        .resume_pending_orchestrator_transitions()
        .expect("aborted stop resume should succeed without redispatching the blocked child");

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let instance = inner
            .orchestrator_instances
            .iter()
            .find(|candidate| candidate.id == instance_id)
            .expect("orchestrator should still exist");
        assert_eq!(instance.status, OrchestratorInstanceStatus::Running);
        assert_eq!(instance.pending_transitions.len(), 1);
        assert_eq!(
            instance.pending_transitions[0].destination_session_id,
            builder_session_id
        );
        let builder = inner
            .sessions
            .iter()
            .find(|record| record.session.id == builder_session_id)
            .expect("builder session should still exist");
        assert_eq!(builder.session.status, SessionStatus::Idle);
        assert!(matches!(builder.runtime, SessionRuntime::None));
        assert!(builder.orchestrator_auto_dispatch_blocked);
        assert!(builder.queued_prompts.is_empty());
        assert!(builder.session.pending_prompts.is_empty());
    }

    let reloaded_inner = load_state(original_persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    let reloaded_instance = reloaded_inner
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("persisted orchestrator should still exist");
    assert_eq!(
        reloaded_instance.status,
        OrchestratorInstanceStatus::Running
    );
    assert_eq!(reloaded_instance.pending_transitions.len(), 1);
    assert_eq!(
        reloaded_instance.pending_transitions[0].destination_session_id,
        builder_session_id
    );
    let reloaded_builder = reloaded_inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("persisted builder session should still exist");
    assert!(reloaded_builder.orchestrator_auto_dispatch_blocked);

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(original_persistence_path.as_path());
    let _ = fs::remove_dir_all(failing_persistence_path);
}

#[test]
fn aborted_stop_restart_does_not_redispatch_child_after_child_stop_persist_fails() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-restart-{}",
        Uuid::new_v4()
    ));
    let state_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-restart-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&state_root).expect("state root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let persistence_path = state_root.join("sessions.json");
    let orchestrator_templates_path = state_root.join("orchestrators.json");
    let failing_persistence_path = state_root.join("persist-failure");
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    let mut state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    let project_id = create_test_project(&state, &project_root, "Persist Failure Restart");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (builder_runtime, _builder_input_rx) =
        test_claude_runtime_handle("orchestrator-stop-persist-failure-restart-builder");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let instance_index = inner
            .orchestrator_instances
            .iter()
            .position(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        inner.sessions[builder_index].runtime = SessionRuntime::Claude(builder_runtime);
        inner.sessions[builder_index].session.status = SessionStatus::Active;
        inner.sessions[builder_index].active_turn_start_message_count =
            Some(inner.sessions[builder_index].session.messages.len());
        let completion_revision = inner.revision.saturating_add(1);
        inner.orchestrator_instances[instance_index]
            .pending_transitions
            .push(PendingTransition {
                id: format!("pending-transition-{}", Uuid::new_v4()),
                transition_id: "planner-to-builder".to_owned(),
                source_session_id: planner_session_id.clone(),
                destination_session_id: builder_session_id.clone(),
                completion_revision,
                rendered_prompt: "builder pending work should remain pending after restart"
                    .to_owned(),
                created_at: stamp_orchestrator_template_now(),
            });
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .begin_orchestrator_stop(&instance_id)
        .expect("stop should be marked in progress");
    state.persistence_path = Arc::new(failing_persistence_path.clone());

    let error = state
        .stop_session_with_options(
            &builder_session_id,
            StopSessionOptions {
                dispatch_queued_prompts_on_success: false,
                orchestrator_stop_instance_id: Some(instance_id.clone()),
            },
        )
        .err()
        .expect("persist failures should abort child stop persistence");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to persist session state"));

    state.persistence_path = Arc::new(persistence_path.clone());
    state
        .prune_pending_transitions_for_stopped_orchestrator_sessions(&instance_id)
        .expect("aborted cleanup should preserve pending child work");
    state.finish_orchestrator_stop(&instance_id);
    drop(state);

    let restarted = AppState::new_with_paths(
        normalized_root,
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should restart");

    {
        let inner = restarted.inner.lock().expect("state mutex poisoned");
        let instance = inner
            .orchestrator_instances
            .iter()
            .find(|candidate| candidate.id == instance_id)
            .expect("orchestrator should still exist after restart");
        assert_eq!(instance.status, OrchestratorInstanceStatus::Running);
        assert_eq!(instance.pending_transitions.len(), 1);
        assert_eq!(
            instance.pending_transitions[0].destination_session_id,
            builder_session_id
        );
        let builder = inner
            .sessions
            .iter()
            .find(|record| record.session.id == builder_session_id)
            .expect("builder session should still exist after restart");
        assert_eq!(builder.session.status, SessionStatus::Idle);
        assert!(matches!(builder.runtime, SessionRuntime::None));
        assert!(builder.orchestrator_auto_dispatch_blocked);
        assert!(builder.queued_prompts.is_empty());
        assert!(builder.session.pending_prompts.is_empty());
    }

    drop(restarted);

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn aborted_stop_restart_does_not_dispatch_orphaned_child_queue_after_child_stop_persist_fails() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-restart-queued-{}",
        Uuid::new_v4()
    ));
    let state_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-restart-queued-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&state_root).expect("state root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let persistence_path = state_root.join("sessions.json");
    let orchestrator_templates_path = state_root.join("orchestrators.json");
    let failing_persistence_path = state_root.join("persist-failure");
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    let mut state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    let project_id = create_test_project(&state, &project_root, "Persist Failure Restart Queue");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (builder_runtime, _builder_input_rx) =
        test_claude_runtime_handle("orchestrator-stop-persist-failure-restart-queued-builder");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        inner.sessions[builder_index].runtime = SessionRuntime::Claude(builder_runtime);
        inner.sessions[builder_index].session.status = SessionStatus::Active;
        inner.sessions[builder_index].active_turn_start_message_count =
            Some(inner.sessions[builder_index].session.messages.len());
        inner.sessions[builder_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-persist-failure-restart-builder".to_owned(),
                    timestamp: stamp_now(),
                    text: "builder queued work should remain parked after restart".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[builder_index]);
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .begin_orchestrator_stop(&instance_id)
        .expect("stop should be marked in progress");
    state.persistence_path = Arc::new(failing_persistence_path.clone());

    let error = state
        .stop_session_with_options(
            &builder_session_id,
            StopSessionOptions {
                dispatch_queued_prompts_on_success: false,
                orchestrator_stop_instance_id: Some(instance_id.clone()),
            },
        )
        .err()
        .expect("persist failures should abort child stop persistence");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to persist session state"));

    state.persistence_path = Arc::new(persistence_path.clone());
    state
        .prune_pending_transitions_for_stopped_orchestrator_sessions(&instance_id)
        .expect("aborted cleanup should preserve queued child work");
    state.finish_orchestrator_stop(&instance_id);
    drop(state);

    let restarted = AppState::new_with_paths(
        normalized_root,
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should restart");

    {
        let inner = restarted.inner.lock().expect("state mutex poisoned");
        let instance = inner
            .orchestrator_instances
            .iter()
            .find(|candidate| candidate.id == instance_id)
            .expect("orchestrator should still exist after restart");
        assert_eq!(instance.status, OrchestratorInstanceStatus::Running);
        assert!(instance.pending_transitions.is_empty());
        let builder = inner
            .sessions
            .iter()
            .find(|record| record.session.id == builder_session_id)
            .expect("builder session should still exist after restart");
        assert_eq!(builder.session.status, SessionStatus::Idle);
        assert!(matches!(builder.runtime, SessionRuntime::None));
        assert!(builder.orchestrator_auto_dispatch_blocked);
        assert_eq!(builder.queued_prompts.len(), 1);
        assert_eq!(builder.session.pending_prompts.len(), 1);
        assert_eq!(
            builder.queued_prompts[0].pending_prompt.text,
            "builder queued work should remain parked after restart"
        );
    }

    drop(restarted);

    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(project_root);
}

#[test]
fn blocked_session_manual_recovery_dispatch_prioritizes_user_prompt_after_restart() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-manual-recovery-{}",
        Uuid::new_v4()
    ));
    let state_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-manual-recovery-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&state_root).expect("state root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let persistence_path = state_root.join("sessions.json");
    let orchestrator_templates_path = state_root.join("orchestrators.json");
    let failing_persistence_path = state_root.join("persist-failure");
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    let mut state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    let project_id = create_test_project(&state, &project_root, "Manual Recovery Ordering");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let reviewer_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "reviewer")
        .expect("reviewer session should be mapped")
        .session_id
        .clone();
    let (reviewer_runtime, _reviewer_input_rx) =
        test_claude_runtime_handle("orchestrator-stop-persist-failure-manual-recovery-reviewer");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let reviewer_index = inner
            .find_session_index(&reviewer_session_id)
            .expect("reviewer session should exist");
        inner.sessions[reviewer_index].runtime = SessionRuntime::Claude(reviewer_runtime);
        inner.sessions[reviewer_index].session.status = SessionStatus::Active;
        inner.sessions[reviewer_index].active_turn_start_message_count =
            Some(inner.sessions[reviewer_index].session.messages.len());
        inner.sessions[reviewer_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-persist-failure-manual-recovery-reviewer".to_owned(),
                    timestamp: stamp_now(),
                    text: "reviewer queued work should stay behind the user prompt".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[reviewer_index]);
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .begin_orchestrator_stop(&instance_id)
        .expect("stop should be marked in progress");
    state.persistence_path = Arc::new(failing_persistence_path.clone());

    let error = state
        .stop_session_with_options(
            &reviewer_session_id,
            StopSessionOptions {
                dispatch_queued_prompts_on_success: false,
                orchestrator_stop_instance_id: Some(instance_id.clone()),
            },
        )
        .err()
        .expect("persist failures should abort child stop persistence");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to persist session state"));

    state.persistence_path = Arc::new(persistence_path.clone());
    state
        .prune_pending_transitions_for_stopped_orchestrator_sessions(&instance_id)
        .expect("aborted cleanup should preserve queued child work");
    state.finish_orchestrator_stop(&instance_id);
    drop(state);

    let restarted = AppState::new_with_paths(
        normalized_root,
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should restart");
    let (wrong_runtime, _wrong_input_rx) = test_codex_runtime_handle(
        "orchestrator-stop-persist-failure-manual-recovery-wrong-runtime",
    );
    let baseline_message_count = {
        let mut inner = restarted.inner.lock().expect("state mutex poisoned");
        let reviewer_index = inner
            .find_session_index(&reviewer_session_id)
            .expect("reviewer session should exist after restart");
        inner.sessions[reviewer_index].runtime = SessionRuntime::Codex(wrong_runtime);
        inner.sessions[reviewer_index].session.messages.len()
    };

    let failed_recovery = restarted
        .dispatch_turn(
            &reviewer_session_id,
            SendMessageRequest {
                text: "this failed recovery should not clear the block".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
            },
        )
        .err()
        .expect("wrong runtime should reject the first manual recovery attempt");
    assert_eq!(failed_recovery.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        failed_recovery
            .message
            .contains("unexpected Codex runtime attached to Claude session")
    );

    {
        let inner = restarted.inner.lock().expect("state mutex poisoned");
        let reviewer = inner
            .sessions
            .iter()
            .find(|record| record.session.id == reviewer_session_id)
            .expect("reviewer session should still exist after failed manual recovery");
        assert_eq!(reviewer.session.status, SessionStatus::Idle);
        assert!(reviewer.orchestrator_auto_dispatch_blocked);
        assert_eq!(reviewer.queued_prompts.len(), 1);
        assert_eq!(reviewer.session.pending_prompts.len(), 1);
        assert_eq!(reviewer.session.messages.len(), baseline_message_count);
        assert!(reviewer.session.messages.iter().all(|message| !matches!(
            message,
            Message::Text { text, author: Author::You, .. }
                if text.contains("this failed recovery should not clear the block")
        )));
    }

    let (restart_reviewer_runtime, _restart_reviewer_input_rx) = test_claude_runtime_handle(
        "orchestrator-stop-persist-failure-manual-recovery-reviewer-restarted",
    );
    {
        let mut inner = restarted.inner.lock().expect("state mutex poisoned");
        let reviewer_index = inner
            .find_session_index(&reviewer_session_id)
            .expect("reviewer session should exist after restart");
        inner.sessions[reviewer_index].runtime = SessionRuntime::Claude(restart_reviewer_runtime);
    }

    let dispatch_result = restarted
        .dispatch_turn(
            &reviewer_session_id,
            SendMessageRequest {
                text: "please continue with a manual recovery prompt".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
            },
        )
        .expect("manual recovery prompt should dispatch");

    match dispatch_result {
        DispatchTurnResult::Dispatched(TurnDispatch::PersistentClaude { command, .. }) => {
            assert!(
                command
                    .text
                    .contains("please continue with a manual recovery prompt")
            );
        }
        DispatchTurnResult::Dispatched(_) => {
            panic!("manual recovery should dispatch on the reviewer Claude runtime")
        }
        DispatchTurnResult::Queued => panic!("manual recovery prompt should dispatch immediately"),
    }

    {
        let inner = restarted.inner.lock().expect("state mutex poisoned");
        let reviewer = inner
            .sessions
            .iter()
            .find(|record| record.session.id == reviewer_session_id)
            .expect("reviewer session should still exist after manual recovery");
        assert_eq!(reviewer.session.status, SessionStatus::Active);
        assert!(!reviewer.orchestrator_auto_dispatch_blocked);
        assert_eq!(reviewer.queued_prompts.len(), 1);
        assert_eq!(reviewer.session.pending_prompts.len(), 1);
        assert_eq!(
            reviewer.queued_prompts[0].pending_prompt.text,
            "reviewer queued work should stay behind the user prompt"
        );
        assert!(matches!(
            reviewer.session.messages.last(),
            Some(Message::Text { text, author: Author::You, .. })
                if text.contains("please continue with a manual recovery prompt")
        ));
    }

    drop(restarted);

    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(project_root);
}

#[test]
fn blocked_session_manual_recovery_preserves_user_prompt_fifo_after_plain_stop_persist_failure() {
    let mut state = test_app_state();
    let original_persistence_path = state.persistence_path.clone();
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-stop-persist-failure-user-queue-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Blocked FIFO".to_owned()),
            workdir: Some(state.default_workdir.clone()),
            project_id: None,
            model: Some("claude-sonnet-4-5".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("session should be created");
    let session_id = created.session_id.clone();
    let (initial_runtime, _initial_input_rx) =
        test_claude_runtime_handle("plain-stop-persist-failure-initial-runtime");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(initial_runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].active_turn_start_message_count =
            Some(inner.sessions[index].session.messages.len());
        inner.sessions[index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::User,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-older-user-prompt".to_owned(),
                    timestamp: stamp_now(),
                    text: "older queued user prompt".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[index]);
        state.commit_locked(&mut inner).unwrap();
    }

    state.persistence_path = Arc::new(failing_persistence_path.clone());
    let stop_error = state
        .stop_session_with_options(&session_id, StopSessionOptions::default())
        .err()
        .expect("persist failures should abort plain stop persistence");
    assert_eq!(stop_error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        stop_error
            .message
            .contains("failed to persist session state")
    );

    state.persistence_path = original_persistence_path.clone();
    let (recovery_runtime, _recovery_input_rx) =
        test_claude_runtime_handle("plain-stop-persist-failure-recovery-runtime");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist after stop failure");
        inner.sessions[index].runtime = SessionRuntime::Claude(recovery_runtime);
        assert!(inner.sessions[index].orchestrator_auto_dispatch_blocked);
        assert_eq!(inner.sessions[index].queued_prompts.len(), 1);
        assert_eq!(
            inner.sessions[index].queued_prompts[0].source,
            QueuedPromptSource::User
        );
    }

    let dispatch_result = state
        .dispatch_turn(
            &session_id,
            SendMessageRequest {
                text: "new recovery prompt should stay behind old queued user work".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
            },
        )
        .expect("manual recovery should dispatch the oldest queued user prompt");

    match dispatch_result {
        DispatchTurnResult::Dispatched(TurnDispatch::PersistentClaude { command, .. }) => {
            assert_eq!(command.text, "older queued user prompt");
        }
        DispatchTurnResult::Dispatched(_) => {
            panic!("plain blocked FIFO recovery should dispatch on the Claude runtime")
        }
        DispatchTurnResult::Queued => {
            panic!("plain blocked FIFO recovery should dispatch immediately")
        }
    }

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == session_id)
            .expect("session should still exist after recovery dispatch");
        assert_eq!(record.session.status, SessionStatus::Active);
        assert!(!record.orchestrator_auto_dispatch_blocked);
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(record.session.pending_prompts.len(), 1);
        assert_eq!(
            record.queued_prompts[0].pending_prompt.text,
            "new recovery prompt should stay behind old queued user work"
        );
        assert_eq!(record.queued_prompts[0].source, QueuedPromptSource::User);
    }

    let _ = fs::remove_file(original_persistence_path.as_path());
    let _ = fs::remove_dir_all(failing_persistence_path);
}

#[test]
fn blocked_session_manual_recovery_prioritizes_existing_user_queue_ahead_of_stale_orchestrator_work()
 {
    let mut state = test_app_state();
    let original_persistence_path = state.persistence_path.clone();
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-stop-persist-failure-mixed-queue-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Blocked Mixed Queue".to_owned()),
            workdir: Some(state.default_workdir.clone()),
            project_id: None,
            model: Some("claude-sonnet-4-5".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("session should be created");
    let session_id = created.session_id.clone();
    let (initial_runtime, _initial_input_rx) =
        test_claude_runtime_handle("mixed-queue-persist-failure-initial-runtime");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(initial_runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].active_turn_start_message_count =
            Some(inner.sessions[index].session.messages.len());
        inner.sessions[index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-stale-orchestrator-prompt".to_owned(),
                    timestamp: stamp_now(),
                    text: "older stale orchestrator prompt".to_owned(),
                    expanded_text: None,
                },
            });
        inner.sessions[index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::User,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-older-user-prompt-mixed".to_owned(),
                    timestamp: stamp_now(),
                    text: "older queued user prompt behind stale orchestrator work".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[index]);
        state.commit_locked(&mut inner).unwrap();
    }

    state.persistence_path = Arc::new(failing_persistence_path.clone());
    let stop_error = state
        .stop_session_with_options(&session_id, StopSessionOptions::default())
        .err()
        .expect("persist failures should abort plain stop persistence");
    assert_eq!(stop_error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        stop_error
            .message
            .contains("failed to persist session state")
    );

    state.persistence_path = original_persistence_path.clone();
    let (recovery_runtime, _recovery_input_rx) =
        test_claude_runtime_handle("mixed-queue-persist-failure-recovery-runtime");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist after stop failure");
        inner.sessions[index].runtime = SessionRuntime::Claude(recovery_runtime);
        assert!(inner.sessions[index].orchestrator_auto_dispatch_blocked);
        assert_eq!(inner.sessions[index].queued_prompts.len(), 2);
        assert_eq!(
            inner.sessions[index].queued_prompts[0].source,
            QueuedPromptSource::Orchestrator
        );
        assert_eq!(
            inner.sessions[index].queued_prompts[1].source,
            QueuedPromptSource::User
        );
    }

    let dispatch_result = state
        .dispatch_turn(
            &session_id,
            SendMessageRequest {
                text: "new manual recovery prompt should not jump ahead of older queued user work"
                    .to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
            },
        )
        .expect("manual recovery should dispatch the older queued user prompt first");

    match dispatch_result {
        DispatchTurnResult::Dispatched(TurnDispatch::PersistentClaude { command, .. }) => {
            assert_eq!(
                command.text,
                "older queued user prompt behind stale orchestrator work"
            );
        }
        DispatchTurnResult::Dispatched(_) => {
            panic!("mixed blocked recovery should dispatch on the Claude runtime")
        }
        DispatchTurnResult::Queued => {
            panic!("mixed blocked recovery should dispatch immediately")
        }
    }

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == session_id)
            .expect("session should still exist after mixed recovery dispatch");
        assert_eq!(record.session.status, SessionStatus::Active);
        assert!(!record.orchestrator_auto_dispatch_blocked);
        assert_eq!(record.queued_prompts.len(), 2);
        assert_eq!(
            record.queued_prompts[0].pending_prompt.text,
            "new manual recovery prompt should not jump ahead of older queued user work"
        );
        assert_eq!(record.queued_prompts[0].source, QueuedPromptSource::User);
        assert_eq!(
            record.queued_prompts[1].pending_prompt.text,
            "older stale orchestrator prompt"
        );
        assert_eq!(
            record.queued_prompts[1].source,
            QueuedPromptSource::Orchestrator
        );
    }

    let _ = fs::remove_file(original_persistence_path.as_path());
    let _ = fs::remove_dir_all(failing_persistence_path);
}

#[test]
fn aborted_stop_does_not_relaunch_child_work_completed_during_stop() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-orchestrator-stop-guard-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("stop guard project root should exist");
    let project_id = create_test_project(&state, &project_root, "Stop Guard Project");
    let mut draft = sample_orchestrator_template_draft();
    draft.transitions.push(OrchestratorTemplateTransition {
        id: "planner-to-reviewer-during-stop".to_owned(),
        from_session_id: "planner".to_owned(),
        to_session_id: "reviewer".to_owned(),
        from_anchor: Some("right".to_owned()),
        to_anchor: Some("top".to_owned()),
        trigger: OrchestratorTransitionTrigger::OnCompletion,
        result_mode: OrchestratorTransitionResultMode::LastResponse,
        prompt_template: Some("Review this plan directly:\n\n{{result}}".to_owned()),
    });
    let template = state
        .create_orchestrator_template(draft)
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let reviewer_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "reviewer")
        .expect("reviewer session should be mapped")
        .session_id
        .clone();
    let (planner_runtime, _planner_input_rx) =
        test_claude_runtime_handle("orchestrator-stop-guard-planner");
    let (builder_runtime, _builder_input_rx) =
        test_codex_runtime_handle("orchestrator-stop-guard-builder");

    let planner_token = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        let reviewer_index = inner
            .find_session_index(&reviewer_session_id)
            .expect("reviewer session should exist");

        inner.sessions[planner_index].runtime = SessionRuntime::Claude(planner_runtime);
        inner.sessions[planner_index].session.status = SessionStatus::Active;
        inner.sessions[builder_index].runtime = SessionRuntime::Codex(builder_runtime);
        inner.sessions[builder_index].session.status = SessionStatus::Active;
        inner.sessions[builder_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-builder-orchestrator-stop-follow-up".to_owned(),
                    timestamp: stamp_now(),
                    text: "builder follow-up that should be cleared on aborted stop".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[builder_index]);
        inner.sessions[reviewer_index].session.status = SessionStatus::Active;

        let message_id = inner.next_message_id();
        push_message_on_record(
            &mut inner.sessions[planner_index],
            Message::Text {
                attachments: Vec::new(),
                id: message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Implement the panel dragging changes.".to_owned(),
                expanded_text: None,
            },
        );
        inner.sessions[planner_index].session.preview =
            "Implement the panel dragging changes.".to_owned();
        inner.sessions[planner_index]
            .runtime
            .runtime_token()
            .expect("planner runtime token should exist")
    };

    state
        .begin_orchestrator_stop(&instance_id)
        .expect("stop guard should be acquired");
    state
        .finish_turn_ok_if_runtime_matches(&planner_session_id, &planner_token)
        .expect("planner completion should succeed while stop is in flight");

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let instance = inner
            .orchestrator_instances
            .iter()
            .find(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        assert_eq!(instance.pending_transitions.len(), 2);
        assert!(
            instance
                .pending_transitions
                .iter()
                .any(|pending| { pending.destination_session_id == builder_session_id })
        );
        assert!(
            instance
                .pending_transitions
                .iter()
                .any(|pending| { pending.destination_session_id == reviewer_session_id })
        );
    }

    state
        .stop_session_with_options(
            &builder_session_id,
            StopSessionOptions {
                dispatch_queued_prompts_on_success: false,
                orchestrator_stop_instance_id: None,
            },
        )
        .expect("builder stop should succeed while the orchestrator stop is in flight");
    state.note_stopped_orchestrator_session(&instance_id, &builder_session_id);
    state
        .prune_pending_transitions_for_stopped_orchestrator_sessions(&instance_id)
        .expect("aborted stops should prune pending work for stopped children");

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let instance = inner
            .orchestrator_instances
            .iter()
            .find(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        let planner_instance = instance
            .session_instances
            .iter()
            .find(|candidate| candidate.session_id == planner_session_id)
            .expect("planner instance should exist");
        assert_eq!(instance.pending_transitions.len(), 1);
        assert!(
            instance
                .pending_transitions
                .iter()
                .all(|pending| { pending.destination_session_id == reviewer_session_id })
        );
        assert_ne!(
            planner_instance.last_completion_revision,
            planner_instance.last_delivered_completion_revision
        );
        let builder = inner
            .sessions
            .iter()
            .find(|record| record.session.id == builder_session_id)
            .expect("builder session should exist");
        assert_eq!(builder.session.status, SessionStatus::Idle);
        assert!(matches!(builder.runtime, SessionRuntime::None));
        assert!(builder.queued_prompts.is_empty());
        assert!(builder.session.pending_prompts.is_empty());
    }

    let reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    let reloaded_builder = reloaded_inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("persisted builder session should exist");
    assert!(reloaded_builder.queued_prompts.is_empty());
    assert!(reloaded_builder.session.pending_prompts.is_empty());

    state.finish_orchestrator_stop(&instance_id);
    state
        .resume_pending_orchestrator_transitions()
        .expect("aborted stops should resume completions for unstopped children");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let instance = inner
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("orchestrator should exist");
    let planner_instance = instance
        .session_instances
        .iter()
        .find(|candidate| candidate.session_id == planner_session_id)
        .expect("planner instance should exist");
    assert!(instance.pending_transitions.is_empty());
    assert_eq!(
        planner_instance.last_completion_revision,
        planner_instance.last_delivered_completion_revision
    );
    let builder = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("builder session should exist");
    assert_eq!(builder.session.status, SessionStatus::Idle);
    assert!(matches!(builder.runtime, SessionRuntime::None));
    assert!(builder.queued_prompts.is_empty());
    assert!(builder.session.pending_prompts.is_empty());
    let reviewer = inner
        .sessions
        .iter()
        .find(|record| record.session.id == reviewer_session_id)
        .expect("reviewer session should exist");
    assert_eq!(reviewer.session.status, SessionStatus::Active);
    assert_eq!(reviewer.queued_prompts.len(), 1);
    assert_eq!(reviewer.session.pending_prompts.len(), 1);
    assert_eq!(
        reviewer.queued_prompts[0].source,
        QueuedPromptSource::Orchestrator
    );
    assert!(
        reviewer.session.pending_prompts[0]
            .text
            .contains("Implement the panel dragging changes.")
    );

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn begin_orchestrator_stop_cleans_up_guards_on_missing_and_stopped_errors() {
    let state = test_app_state();
    let missing_instance_id = "missing-orchestrator-instance";
    let error = state
        .begin_orchestrator_stop(missing_instance_id)
        .expect_err("missing orchestrators should not start a stop");
    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert_eq!(error.message, "orchestrator instance not found");
    assert!(
        !state
            .stopping_orchestrator_ids
            .lock()
            .expect("orchestrator stop mutex poisoned")
            .contains(missing_instance_id)
    );
    assert!(
        !state
            .stopping_orchestrator_session_ids
            .lock()
            .expect("orchestrator stop session mutex poisoned")
            .contains_key(missing_instance_id)
    );

    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-begin-errors-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let project_id = create_test_project(&state, &project_root, "Begin Stop Errors Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let instance = inner
            .orchestrator_instances
            .iter_mut()
            .find(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        instance.status = OrchestratorInstanceStatus::Stopped;
        instance.stop_in_progress = false;
    }

    let error = state
        .begin_orchestrator_stop(&instance_id)
        .expect_err("stopped orchestrators should reject stop");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, "orchestrator is already stopped");
    assert!(
        !state
            .stopping_orchestrator_ids
            .lock()
            .expect("orchestrator stop mutex poisoned")
            .contains(&instance_id)
    );
    assert!(
        !state
            .stopping_orchestrator_session_ids
            .lock()
            .expect("orchestrator stop session mutex poisoned")
            .contains_key(&instance_id)
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let instance = inner
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("orchestrator should still exist");
    assert_eq!(instance.status, OrchestratorInstanceStatus::Stopped);
    assert!(!instance.stop_in_progress);
    assert!(instance.active_session_ids_during_stop.is_none());
    drop(inner);

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn begin_orchestrator_stop_rolls_back_stop_in_progress_after_persist_failure() {
    let mut state = test_app_state();
    let original_persistence_path = state.persistence_path.clone();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-begin-persist-failure-{}",
        Uuid::new_v4()
    ));
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-begin-persist-failure-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");
    let project_id = create_test_project(&state, &project_root, "Begin Stop Persist Failure");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    state.persistence_path = Arc::new(failing_persistence_path.clone());

    let error = state
        .begin_orchestrator_stop(&instance_id)
        .expect_err("persistence failures should abort stop initialization");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        error
            .message
            .contains("failed to persist orchestrator stop state")
    );
    assert!(
        !state
            .stopping_orchestrator_ids
            .lock()
            .expect("orchestrator stop mutex poisoned")
            .contains(&instance_id)
    );
    assert!(
        !state
            .stopping_orchestrator_session_ids
            .lock()
            .expect("orchestrator stop session mutex poisoned")
            .contains_key(&instance_id)
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let instance = inner
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("orchestrator should still exist");
    assert_eq!(instance.status, OrchestratorInstanceStatus::Running);
    assert!(!instance.stop_in_progress);
    assert!(instance.active_session_ids_during_stop.is_none());
    drop(inner);

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(original_persistence_path.as_path());
    let _ = fs::remove_dir_all(failing_persistence_path);
}

#[test]
fn load_state_preserves_pending_transitions_when_stop_in_progress_has_no_stopped_children() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-restart-{}",
        Uuid::new_v4()
    ));
    let state_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-restart-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("restart recovery project root should exist");
    fs::create_dir_all(&state_root).expect("restart recovery state root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let persistence_path = state_root.join("sessions.json");
    let orchestrator_templates_path = state_root.join("orchestrators.json");

    let state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    let project_id = create_test_project(&state, &project_root, "Restart Recovery Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let reviewer_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "reviewer")
        .expect("reviewer session should be mapped")
        .session_id
        .clone();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let instance_index = inner
            .orchestrator_instances
            .iter()
            .position(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        let completion_revision = inner.revision.saturating_add(1);
        inner.orchestrator_instances[instance_index].stop_in_progress = true;
        inner.orchestrator_instances[instance_index].active_session_ids_during_stop =
            Some(vec![builder_session_id.clone()]);
        inner.orchestrator_instances[instance_index]
            .pending_transitions
            .push(PendingTransition {
                id: format!("pending-transition-{}", Uuid::new_v4()),
                transition_id: "planner-to-reviewer".to_owned(),
                source_session_id: planner_session_id.clone(),
                destination_session_id: reviewer_session_id.clone(),
                completion_revision,
                rendered_prompt: "reviewer work should survive recovery".to_owned(),
                created_at: stamp_orchestrator_template_now(),
            });
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        inner.sessions[builder_index].session.status = SessionStatus::Active;
        state
            .persist_internal_locked(&inner)
            .expect("stop recovery state should persist");
    }

    drop(state);

    let recovered = load_state(&persistence_path)
        .expect("recovered state should load")
        .expect("recovered state should exist");
    let recovered_instance = recovered
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("recovered orchestrator should exist");
    assert_eq!(
        recovered_instance.status,
        OrchestratorInstanceStatus::Running
    );
    assert!(!recovered_instance.stop_in_progress);
    assert!(recovered_instance.active_session_ids_during_stop.is_none());
    assert!(
        recovered_instance
            .stopped_session_ids_during_stop
            .is_empty()
    );
    assert_eq!(recovered_instance.pending_transitions.len(), 1);
    assert_eq!(
        recovered_instance.pending_transitions[0].destination_session_id,
        reviewer_session_id
    );
    let recovered_builder = recovered
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("persisted builder session should exist after restart");
    assert_eq!(recovered_builder.session.status, SessionStatus::Error);
    let _ = fs::remove_file(persistence_path);
    let _ = fs::remove_file(orchestrator_templates_path);
    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(project_root);
}

#[test]
fn load_state_recovers_completed_stop_when_active_children_finished_during_stop() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-restart-finished-{}",
        Uuid::new_v4()
    ));
    let state_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-restart-finished-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("restart recovery project root should exist");
    fs::create_dir_all(&state_root).expect("restart recovery state root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let persistence_path = state_root.join("sessions.json");
    let orchestrator_templates_path = state_root.join("orchestrators.json");

    let state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    let project_id = create_test_project(&state, &project_root, "Restart Recovery Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (planner_runtime, _planner_input_rx) =
        test_claude_runtime_handle("orchestrator-stop-restart-planner");

    let planner_token = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        inner.sessions[planner_index].runtime = SessionRuntime::Claude(planner_runtime);
        inner.sessions[planner_index].session.status = SessionStatus::Active;

        let message_id = inner.next_message_id();
        push_message_on_record(
            &mut inner.sessions[planner_index],
            Message::Text {
                attachments: Vec::new(),
                id: message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Implement the panel dragging changes.".to_owned(),
                expanded_text: None,
            },
        );
        inner.sessions[planner_index].session.preview =
            "Implement the panel dragging changes.".to_owned();
        inner.sessions[planner_index]
            .runtime
            .runtime_token()
            .expect("planner runtime token should exist")
    };

    state
        .begin_orchestrator_stop(&instance_id)
        .expect("stop should be marked in progress");
    state
        .finish_turn_ok_if_runtime_matches(&planner_session_id, &planner_token)
        .expect("planner completion should persist while stop is in flight");

    let persisted_mid_stop: Value = serde_json::from_slice(
        &fs::read(&persistence_path).expect("mid-stop state file should exist"),
    )
    .expect("mid-stop state should deserialize");
    let persisted_mid_stop_instance = persisted_mid_stop["orchestratorInstances"]
        .as_array()
        .expect("persisted orchestrator instances should be present")
        .iter()
        .find(|candidate| candidate["id"] == instance_id)
        .expect("persisted orchestrator should exist");
    assert_eq!(
        persisted_mid_stop_instance["status"],
        Value::String("running".to_owned())
    );
    assert_eq!(
        persisted_mid_stop_instance["stopInProgress"],
        Value::Bool(true)
    );
    assert_eq!(
        persisted_mid_stop_instance["pendingTransitions"]
            .as_array()
            .expect("pending transitions should be present")
            .len(),
        1
    );
    assert_eq!(
        persisted_mid_stop_instance["activeSessionIdsDuringStop"]
            .as_array()
            .expect("active stop session ids should be present")
            .len(),
        1
    );
    assert_eq!(
        persisted_mid_stop_instance["activeSessionIdsDuringStop"][0],
        Value::String(planner_session_id.clone())
    );

    drop(state);

    let recovered = load_state(&persistence_path)
        .expect("recovered state should load")
        .expect("recovered state should exist");
    let recovered_instance = recovered
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("recovered orchestrator should exist");
    assert_eq!(
        recovered_instance.status,
        OrchestratorInstanceStatus::Stopped
    );
    assert!(!recovered_instance.stop_in_progress);
    assert!(recovered_instance.active_session_ids_during_stop.is_none());
    assert!(
        recovered_instance
            .stopped_session_ids_during_stop
            .is_empty()
    );
    assert!(recovered_instance.pending_transitions.is_empty());
    assert!(recovered_instance.completed_at.is_some());
    let recovered_builder = recovered
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("persisted builder session should exist after restart");
    assert!(recovered_builder.queued_prompts.is_empty());
    assert!(recovered_builder.session.pending_prompts.is_empty());
    let _ = fs::remove_file(persistence_path);
    let _ = fs::remove_file(orchestrator_templates_path);
    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(project_root);
}

#[test]
fn load_state_prunes_only_stopped_child_work_when_recovering_stop_in_progress() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-recovery-queued-{}",
        Uuid::new_v4()
    ));
    let state_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-recovery-queued-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("recovery project root should exist");
    fs::create_dir_all(&state_root).expect("recovery state root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let persistence_path = state_root.join("sessions.json");
    let orchestrator_templates_path = state_root.join("orchestrators.json");

    let state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    let project_id = create_test_project(&state, &project_root, "Recovery Queue Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let reviewer_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "reviewer")
        .expect("reviewer session should be mapped")
        .session_id
        .clone();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let instance_index = inner
            .orchestrator_instances
            .iter()
            .position(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        let completion_revision = inner.revision.saturating_add(1);
        inner.orchestrator_instances[instance_index].stop_in_progress = true;
        inner.orchestrator_instances[instance_index].active_session_ids_during_stop = Some(vec![
            builder_session_id.clone(),
            reviewer_session_id.clone(),
        ]);
        inner.orchestrator_instances[instance_index]
            .stopped_session_ids_during_stop
            .push(builder_session_id.clone());
        inner.orchestrator_instances[instance_index]
            .pending_transitions
            .push(PendingTransition {
                id: format!("pending-transition-{}", Uuid::new_v4()),
                transition_id: "planner-to-builder".to_owned(),
                source_session_id: planner_session_id.clone(),
                destination_session_id: builder_session_id.clone(),
                completion_revision,
                rendered_prompt: "stale stop recovery prompt".to_owned(),
                created_at: stamp_orchestrator_template_now(),
            });
        inner.orchestrator_instances[instance_index]
            .pending_transitions
            .push(PendingTransition {
                id: format!("pending-transition-{}", Uuid::new_v4()),
                transition_id: "planner-to-reviewer".to_owned(),
                source_session_id: planner_session_id.clone(),
                destination_session_id: reviewer_session_id.clone(),
                completion_revision,
                rendered_prompt: "reviewer work should survive recovery".to_owned(),
                created_at: stamp_orchestrator_template_now(),
            });
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        let reviewer_index = inner
            .find_session_index(&reviewer_session_id)
            .expect("reviewer session should exist");
        inner.sessions[reviewer_index].session.status = SessionStatus::Active;
        inner.sessions[builder_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-recovery-orchestrator-prompt".to_owned(),
                    timestamp: stamp_now(),
                    text: "stale queued orchestrator prompt".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[builder_index]);
        state
            .persist_internal_locked(&inner)
            .expect("stop recovery state should persist");
    }

    drop(state);

    let recovered = load_state(&persistence_path)
        .expect("recovered state should load")
        .expect("recovered state should exist");
    let recovered_instance = recovered
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("recovered orchestrator should exist");
    assert_eq!(
        recovered_instance.status,
        OrchestratorInstanceStatus::Running
    );
    assert!(!recovered_instance.stop_in_progress);
    assert!(recovered_instance.active_session_ids_during_stop.is_none());
    assert!(
        recovered_instance
            .stopped_session_ids_during_stop
            .is_empty()
    );
    assert_eq!(recovered_instance.pending_transitions.len(), 1);
    assert_eq!(
        recovered_instance.pending_transitions[0].destination_session_id,
        reviewer_session_id
    );
    let recovered_builder = recovered
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("persisted builder session should exist after restart");
    assert!(recovered_builder.queued_prompts.is_empty());
    assert!(recovered_builder.session.pending_prompts.is_empty());
    let _ = fs::remove_file(persistence_path);
    let _ = fs::remove_file(orchestrator_templates_path);
    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(project_root);
}

#[test]
fn load_state_recovers_completed_stop_when_all_active_children_were_stopped() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-recovery-complete-{}",
        Uuid::new_v4()
    ));
    let state_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-recovery-complete-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("completed recovery project root should exist");
    fs::create_dir_all(&state_root).expect("completed recovery state root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let persistence_path = state_root.join("sessions.json");
    let orchestrator_templates_path = state_root.join("orchestrators.json");

    let state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    let project_id = create_test_project(&state, &project_root, "Completed Recovery Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let reviewer_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "reviewer")
        .expect("reviewer session should be mapped")
        .session_id
        .clone();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let instance_index = inner
            .orchestrator_instances
            .iter()
            .position(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        let completion_revision = inner.revision.saturating_add(1);
        inner.orchestrator_instances[instance_index].stop_in_progress = true;
        inner.orchestrator_instances[instance_index].active_session_ids_during_stop =
            Some(vec![builder_session_id.clone()]);
        inner.orchestrator_instances[instance_index]
            .stopped_session_ids_during_stop
            .push(builder_session_id.clone());
        inner.orchestrator_instances[instance_index]
            .pending_transitions
            .push(PendingTransition {
                id: format!("pending-transition-{}", Uuid::new_v4()),
                transition_id: "builder-to-reviewer".to_owned(),
                source_session_id: builder_session_id.clone(),
                destination_session_id: reviewer_session_id.clone(),
                completion_revision,
                rendered_prompt: "idle reviewer work should be discarded".to_owned(),
                created_at: stamp_orchestrator_template_now(),
            });
        let reviewer_index = inner
            .find_session_index(&reviewer_session_id)
            .expect("reviewer session should exist");
        inner.sessions[reviewer_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-completed-stop-reviewer-prompt".to_owned(),
                    timestamp: stamp_now(),
                    text: "queued reviewer work should be discarded".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[reviewer_index]);
        state
            .persist_internal_locked(&inner)
            .expect("completed stop recovery state should persist");
    }

    drop(state);

    let recovered = load_state(&persistence_path)
        .expect("recovered state should load")
        .expect("recovered state should exist");
    let recovered_instance = recovered
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("recovered orchestrator should exist");
    assert_eq!(
        recovered_instance.status,
        OrchestratorInstanceStatus::Stopped
    );
    assert!(!recovered_instance.stop_in_progress);
    assert!(recovered_instance.active_session_ids_during_stop.is_none());
    assert!(
        recovered_instance
            .stopped_session_ids_during_stop
            .is_empty()
    );
    assert!(recovered_instance.pending_transitions.is_empty());
    assert!(recovered_instance.completed_at.is_some());
    let recovered_reviewer = recovered
        .sessions
        .iter()
        .find(|record| record.session.id == reviewer_session_id)
        .expect("persisted reviewer session should exist after restart");
    assert!(recovered_reviewer.queued_prompts.is_empty());
    assert!(recovered_reviewer.session.pending_prompts.is_empty());
    let _ = fs::remove_file(persistence_path);
    let _ = fs::remove_file(orchestrator_templates_path);
    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(project_root);
}

fn sample_orchestrator_template_draft() -> OrchestratorTemplateDraft {
    OrchestratorTemplateDraft {
        name: "Feature Delivery Flow".to_owned(),
        description: "Coordinate implementation and review.".to_owned(),
        project_id: None,
        sessions: vec![
            OrchestratorSessionTemplate {
                id: "planner".to_owned(),
                name: "Planner".to_owned(),
                agent: Agent::Claude,
                model: Some("claude-sonnet-4-5".to_owned()),
                instructions: "Plan the work and decide the next action.".to_owned(),
                auto_approve: false,
                input_mode: OrchestratorSessionInputMode::Queue,
                position: OrchestratorNodePosition { x: 620.0, y: 120.0 },
            },
            OrchestratorSessionTemplate {
                id: "builder".to_owned(),
                name: "Builder".to_owned(),
                agent: Agent::Codex,
                model: Some("gpt-5".to_owned()),
                instructions: "Implement the requested changes.".to_owned(),
                auto_approve: true,
                input_mode: OrchestratorSessionInputMode::Queue,
                position: OrchestratorNodePosition { x: 180.0, y: 420.0 },
            },
            OrchestratorSessionTemplate {
                id: "reviewer".to_owned(),
                name: "Reviewer".to_owned(),
                agent: Agent::Claude,
                model: Some("claude-sonnet-4-5".to_owned()),
                instructions: "Review the produced changes and summarize issues.".to_owned(),
                auto_approve: false,
                input_mode: OrchestratorSessionInputMode::Queue,
                position: OrchestratorNodePosition { x: 980.0, y: 420.0 },
            },
        ],
        transitions: vec![
            OrchestratorTemplateTransition {
                id: "planner-to-builder".to_owned(),
                from_session_id: "planner".to_owned(),
                to_session_id: "builder".to_owned(),
                from_anchor: Some("bottom".to_owned()),
                to_anchor: Some("top".to_owned()),
                trigger: OrchestratorTransitionTrigger::OnCompletion,
                result_mode: OrchestratorTransitionResultMode::LastResponse,
                prompt_template: Some("Use this plan and implement it:\n\n{{result}}".to_owned()),
            },
            OrchestratorTemplateTransition {
                id: "builder-to-reviewer".to_owned(),
                from_session_id: "builder".to_owned(),
                to_session_id: "reviewer".to_owned(),
                from_anchor: Some("right".to_owned()),
                to_anchor: Some("left".to_owned()),
                trigger: OrchestratorTransitionTrigger::OnCompletion,
                result_mode: OrchestratorTransitionResultMode::SummaryAndLastResponse,
                prompt_template: Some("Review this implementation:\n\n{{result}}".to_owned()),
            },
        ],
    }
}

#[test]
fn failed_orchestrator_transition_dispatch_becomes_a_visible_destination_error() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-transition-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("transition failure project root should exist");
    let project_id = state
        .create_project(CreateProjectRequest {
            name: Some("Transition Failure Project".to_owned()),
            root_path: project_root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .expect("project should be created")
        .project_id;
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (runtime, input_rx) = test_codex_runtime_handle("orchestrator-transition-failure");
    drop(input_rx);

    let completion_revision = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");

        inner.sessions[builder_index].runtime = SessionRuntime::Codex(runtime);
        let message_id = inner.next_message_id();
        push_message_on_record(
            &mut inner.sessions[planner_index],
            Message::Text {
                attachments: Vec::new(),
                id: message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Implement the panel dragging changes.".to_owned(),
                expanded_text: None,
            },
        );
        inner.sessions[planner_index].session.status = SessionStatus::Idle;

        let completion_revision = inner.revision + 1;
        schedule_orchestrator_transitions_for_completed_session(
            &mut inner,
            &HashMap::new(),
            &planner_session_id,
            completion_revision,
        );
        state.commit_locked(&mut inner).unwrap();
        completion_revision
    };

    state
        .resume_pending_orchestrator_transitions()
        .expect("transition handoff should stay durable even if runtime delivery fails");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let planner_instance = inner.orchestrator_instances[0]
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner instance should exist");
    let builder = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("builder session should exist");
    assert_eq!(
        planner_instance.last_delivered_completion_revision,
        Some(completion_revision)
    );
    assert_eq!(builder.session.status, SessionStatus::Error);
    assert!(builder.queued_prompts.is_empty());
    assert!(builder.session.pending_prompts.is_empty());
    assert!(matches!(
        builder.session.messages.first(),
        Some(Message::Text {
            author: Author::You,
            text,
            ..
        }) if text.contains("Implement the panel dragging changes.")
    ));
    assert!(matches!(
        builder.session.messages.last(),
        Some(Message::Text {
            author: Author::Assistant,
            text,
            ..
        }) if text.contains("failed to queue prompt for Codex session")
    ));
    assert!(
        inner.orchestrator_instances[0]
            .pending_transitions
            .is_empty()
    );
}

#[test]
fn failed_orchestrator_transition_dispatch_does_not_block_other_instances() {
    let state = test_app_state();
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;

    let project_root_a =
        std::env::temp_dir().join(format!("termal-orchestrator-multi-a-{}", Uuid::new_v4()));
    let project_root_b =
        std::env::temp_dir().join(format!("termal-orchestrator-multi-b-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root_a).expect("first project root should exist");
    fs::create_dir_all(&project_root_b).expect("second project root should exist");

    let project_id_a = create_test_project(&state, &project_root_a, "Multi A");
    let project_id_b = create_test_project(&state, &project_root_b, "Multi B");

    let orchestrator_a = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id.clone(),
            project_id: Some(project_id_a),
        })
        .expect("first orchestrator instance should be created")
        .orchestrator;
    let orchestrator_b = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id_b),
        })
        .expect("second orchestrator instance should be created")
        .orchestrator;

    let planner_a_session_id = orchestrator_a
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("first planner session should be mapped")
        .session_id
        .clone();
    let builder_a_session_id = orchestrator_a
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("first builder session should be mapped")
        .session_id
        .clone();
    let planner_b_session_id = orchestrator_b
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("second planner session should be mapped")
        .session_id
        .clone();
    let builder_b_session_id = orchestrator_b
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("second builder session should be mapped")
        .session_id
        .clone();
    let (failing_runtime, failing_input_rx) =
        test_codex_runtime_handle("orchestrator-transition-failure-a");
    drop(failing_input_rx);

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_a_index = inner
            .find_session_index(&planner_a_session_id)
            .expect("first planner session should exist");
        let builder_a_index = inner
            .find_session_index(&builder_a_session_id)
            .expect("first builder session should exist");
        let planner_b_index = inner
            .find_session_index(&planner_b_session_id)
            .expect("second planner session should exist");
        let builder_b_index = inner
            .find_session_index(&builder_b_session_id)
            .expect("second builder session should exist");

        inner.sessions[builder_a_index].runtime = SessionRuntime::Codex(failing_runtime);
        inner.sessions[builder_b_index].session.status = SessionStatus::Active;

        let planner_a_message_id = inner.next_message_id();
        push_message_on_record(
            &mut inner.sessions[planner_a_index],
            Message::Text {
                attachments: Vec::new(),
                id: planner_a_message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Implement canvas drop zones.".to_owned(),
                expanded_text: None,
            },
        );
        inner.sessions[planner_a_index].session.status = SessionStatus::Idle;

        let planner_b_message_id = inner.next_message_id();
        push_message_on_record(
            &mut inner.sessions[planner_b_index],
            Message::Text {
                attachments: Vec::new(),
                id: planner_b_message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Audit the orchestration editor UI.".to_owned(),
                expanded_text: None,
            },
        );
        inner.sessions[planner_b_index].session.status = SessionStatus::Idle;

        let completion_revision = inner.revision + 1;
        schedule_orchestrator_transitions_for_completed_session(
            &mut inner,
            &HashMap::new(),
            &planner_a_session_id,
            completion_revision,
        );
        schedule_orchestrator_transitions_for_completed_session(
            &mut inner,
            &HashMap::new(),
            &planner_b_session_id,
            completion_revision,
        );
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .resume_pending_orchestrator_transitions()
        .expect("delivery failure in one instance should not block others");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let builder_a = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_a_session_id)
        .expect("first builder session should exist");
    let builder_b = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_b_session_id)
        .expect("second builder session should exist");

    assert_eq!(builder_a.session.status, SessionStatus::Error);
    assert_eq!(builder_b.session.pending_prompts.len(), 1);
    assert!(
        builder_b.session.pending_prompts[0]
            .text
            .contains("Audit the orchestration editor UI.")
    );
    assert!(
        inner
            .orchestrator_instances
            .iter()
            .all(|instance| instance.pending_transitions.is_empty())
    );
}

#[test]
fn stop_session_does_not_schedule_orchestrator_transitions() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-transition-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("stop project root should exist");
    let project_id = create_test_project(&state, &project_root, "Stop Transition Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "orchestrator-stop-transition".to_owned(),
        input_tx,
        process: Arc::new(SharedChild::new(test_sleep_child()).unwrap()),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        inner.sessions[planner_index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[planner_index].session.status = SessionStatus::Active;
        inner.sessions[planner_index].active_turn_start_message_count =
            Some(inner.sessions[planner_index].session.messages.len());
        inner.sessions[builder_index].session.status = SessionStatus::Active;
    }

    state
        .stop_session(&planner_session_id)
        .expect("stopping the session should succeed");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let planner = inner
        .sessions
        .iter()
        .find(|record| record.session.id == planner_session_id)
        .expect("planner session should exist");
    let builder = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("builder session should exist");

    assert!(planner.session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn stopped by user."
    )));
    assert!(builder.session.pending_prompts.is_empty());
    assert!(
        inner.orchestrator_instances[0]
            .pending_transitions
            .is_empty()
    );
}

#[test]
fn fail_turn_does_not_schedule_orchestrator_transitions() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-orchestrator-fail-turn-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("fail-turn project root should exist");
    let project_id = create_test_project(&state, &project_root, "Fail Turn Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (runtime, _input_rx) = test_claude_runtime_handle("orchestrator-fail-turn");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        inner.sessions[planner_index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[planner_index].session.status = SessionStatus::Active;
        inner.sessions[planner_index].active_turn_start_message_count =
            Some(inner.sessions[planner_index].session.messages.len());
    }

    state
        .fail_turn_if_runtime_matches(
            &planner_session_id,
            &runtime_token,
            "planner turn failed before completion",
        )
        .expect("turn failure should be handled");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let planner = inner
        .sessions
        .iter()
        .find(|record| record.session.id == planner_session_id)
        .expect("planner session should exist");
    let builder = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("builder session should exist");

    assert!(planner.session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn failed: planner turn failed before completion"
    )));
    assert!(builder.session.pending_prompts.is_empty());
    assert!(
        inner.orchestrator_instances[0]
            .pending_transitions
            .is_empty()
    );
}

#[test]
fn mark_turn_error_does_not_schedule_orchestrator_transitions() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-orchestrator-mark-error-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("mark-error project root should exist");
    let project_id = create_test_project(&state, &project_root, "Mark Error Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (runtime, _input_rx) = test_claude_runtime_handle("orchestrator-mark-error");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        inner.sessions[planner_index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[planner_index].session.status = SessionStatus::Active;
        inner.sessions[planner_index].active_turn_start_message_count =
            Some(inner.sessions[planner_index].session.messages.len());
    }

    state
        .mark_turn_error_if_runtime_matches(
            &planner_session_id,
            &runtime_token,
            "planner runtime entered an error state",
        )
        .expect("turn error should be handled");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let planner = inner
        .sessions
        .iter()
        .find(|record| record.session.id == planner_session_id)
        .expect("planner session should exist");
    let builder = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("builder session should exist");

    assert_eq!(planner.session.status, SessionStatus::Error);
    assert_eq!(
        planner.session.preview,
        "planner runtime entered an error state"
    );
    assert!(builder.session.pending_prompts.is_empty());
    assert!(
        inner.orchestrator_instances[0]
            .pending_transitions
            .is_empty()
    );
}

#[test]
fn orchestrator_transition_uses_only_messages_from_the_current_turn() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-current-turn-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("current turn project root should exist");
    let project_id = state
        .create_project(CreateProjectRequest {
            name: Some("Current Turn Project".to_owned()),
            root_path: project_root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .expect("project should be created")
        .project_id;
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");

        inner.sessions[builder_index].session.status = SessionStatus::Active;
        let old_message_id = inner.next_message_id();
        push_message_on_record(
            &mut inner.sessions[planner_index],
            Message::Text {
                attachments: Vec::new(),
                id: old_message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Old plan from yesterday.".to_owned(),
                expanded_text: None,
            },
        );
        let turn_start = inner.sessions[planner_index].session.messages.len();
        inner.sessions[planner_index].active_turn_start_message_count = Some(turn_start);
        let current_prompt_id = inner.next_message_id();
        push_message_on_record(
            &mut inner.sessions[planner_index],
            Message::Text {
                attachments: Vec::new(),
                id: current_prompt_id,
                timestamp: stamp_now(),
                author: Author::You,
                text: "Current task prompt.".to_owned(),
                expanded_text: None,
            },
        );
        inner.sessions[planner_index].session.preview = "Current task prompt.".to_owned();
        inner.sessions[planner_index].session.status = SessionStatus::Idle;

        let completion_revision = inner.revision + 1;
        schedule_orchestrator_transitions_for_completed_session(
            &mut inner,
            &HashMap::new(),
            &planner_session_id,
            completion_revision,
        );
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .resume_pending_orchestrator_transitions()
        .expect("pending transitions should be delivered");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let builder = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("builder session should exist");
    assert_eq!(builder.session.pending_prompts.len(), 1);
    assert!(
        !builder.session.pending_prompts[0]
            .text
            .contains("Old plan from yesterday.")
    );
    assert!(
        builder.session.pending_prompts[0]
            .text
            .contains("Use this plan and implement it:")
    );
}

#[test]
fn runtime_exit_does_not_schedule_orchestrator_transitions() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-runtime-exit-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("runtime exit project root should exist");
    let project_id = state
        .create_project(CreateProjectRequest {
            name: Some("Runtime Exit Project".to_owned()),
            root_path: project_root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .expect("project should be created")
        .project_id;
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (runtime, _input_rx) = test_claude_runtime_handle("orchestrator-runtime-exit");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        inner.sessions[planner_index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[planner_index].session.status = SessionStatus::Active;
        inner.sessions[builder_index].session.status = SessionStatus::Active;
    }

    state
        .handle_runtime_exit_if_matches(
            &planner_session_id,
            &runtime_token,
            Some("planner runtime crashed"),
        )
        .expect("runtime exit should be handled");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let builder = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("builder session should exist");
    let planner = inner
        .sessions
        .iter()
        .find(|record| record.session.id == planner_session_id)
        .expect("planner session should exist");

    assert!(planner.session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn failed: planner runtime crashed"
    )));
    assert!(builder.session.pending_prompts.is_empty());
    assert!(
        inner.orchestrator_instances[0]
            .pending_transitions
            .is_empty()
    );
}

#[test]
fn killing_a_session_prunes_its_orchestrator_links() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-kill-cleanup-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("kill cleanup project root should exist");
    let project_id = state
        .create_project(CreateProjectRequest {
            name: Some("Kill Cleanup Project".to_owned()),
            root_path: project_root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .expect("project should be created")
        .project_id;
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        let message_id = inner.next_message_id();
        push_message_on_record(
            &mut inner.sessions[planner_index],
            Message::Text {
                attachments: Vec::new(),
                id: message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Plan before kill.".to_owned(),
                expanded_text: None,
            },
        );
        inner.sessions[planner_index].session.status = SessionStatus::Idle;
        let completion_revision = inner.revision + 1;
        schedule_orchestrator_transitions_for_completed_session(
            &mut inner,
            &HashMap::new(),
            &planner_session_id,
            completion_revision,
        );
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .kill_session(&planner_session_id)
        .expect("session should be killed");

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.orchestrator_instances.iter().all(|instance| {
        instance
            .session_instances
            .iter()
            .all(|session| session.session_id != planner_session_id)
    }));
    assert!(inner.orchestrator_instances.iter().all(|instance| {
        instance.pending_transitions.iter().all(|pending| {
            pending.source_session_id != planner_session_id
                && pending.destination_session_id != planner_session_id
        })
    }));
}
