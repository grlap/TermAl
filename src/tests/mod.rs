/*
Backend regression tests
Coverage is organized around production seams rather than tiny private helpers:
  - HTTP router behavior
  - state mutation and persistence
  - runtime protocol normalization
  - remote/orchestrator integration helpers
The local fixtures below keep tests close to real wiring so include!-based
refactors still exercise the same cross-file behavior the app depends on.
*/

use super::*;
use axum::body::{Body, to_bytes};
use axum::http::Request;
use std::io::Read as _;
use tower::util::ServiceExt;

mod acp_gemini;
mod agent_commands;
mod agent_readiness;
mod claude;
mod codex_discovery;
mod codex_threads;
mod cursor;
mod git;
mod http_routes;
mod instruction_search;
mod json_rpc;
mod persist;
mod remote;
mod review;
mod runtime_rpc;
mod session_lifecycle;
mod session_settings;
mod shared_codex;
mod terminal;
mod workspace;

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
    finish_streaming_text_calls: usize,
    reset_turn_state_calls: usize,
}

#[test]
fn format_runtime_stderr_prefix_includes_timestamp_and_label() {
    assert_eq!(
        format_runtime_stderr_prefix("codex", "12:59:03"),
        "codex stderr [12:59:03]>"
    );
    assert_eq!(
        format_runtime_stderr_prefix("gemini", "12:59:04"),
        "gemini stderr [12:59:04]>"
    );
    assert_eq!(
        format_runtime_stderr_prefix("claude", "12:59:05"),
        "claude stderr [12:59:05]>"
    );
}

#[test]
fn stamp_now_includes_seconds() {
    let timestamp = stamp_now();
    let parts: Vec<&str> = timestamp.split(':').collect();

    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0].len(), 2);
    assert_eq!(parts[1].len(), 2);
    assert_eq!(parts[2].len(), 2);
    assert!(
        parts
            .iter()
            .all(|part| part.chars().all(|ch| ch.is_ascii_digit()))
    );
}


#[test]
fn truncate_child_stdout_log_line_appends_ellipsis_only_when_needed() {
    assert_eq!(truncate_child_stdout_log_line("abcdef", 4), "abcd...");
    assert_eq!(truncate_child_stdout_log_line("abc", 4), "abc");
}

#[test]
fn shared_codex_bad_json_streak_failure_detail_trips_at_threshold() {
    assert_eq!(
        shared_codex_bad_json_streak_failure_detail(
            SHARED_CODEX_MAX_CONSECUTIVE_BAD_JSON_LINES - 1,
            "warn"
        ),
        None
    );

    let detail = shared_codex_bad_json_streak_failure_detail(
        SHARED_CODEX_MAX_CONSECUTIVE_BAD_JSON_LINES,
        &"x".repeat(SHARED_CODEX_STDOUT_LOG_PREVIEW_MAX_CHARS + 5),
    )
    .expect("threshold should produce a failure detail");
    assert!(detail.contains(&format!(
        "{} consecutive non-JSON stdout lines",
        SHARED_CODEX_MAX_CONSECUTIVE_BAD_JSON_LINES
    )));
    // The user-facing detail must NOT include the raw child stdout preview.
    assert!(
        !detail.contains("xxx"),
        "raw child stdout content should not appear in user-facing failure detail"
    );
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
        self.finish_streaming_text_calls += 1;
        Ok(())
    }

    fn reset_turn_state(&mut self) -> Result<()> {
        self.reset_turn_state_calls += 1;
        self.finish_streaming_text()
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

#[test]
fn shared_codex_app_server_event_matches_active_turn_covers_turn_id_and_turnless_events() {
    assert!(!shared_codex_app_server_event_matches_active_turn(
        None, false, None
    ));
    assert!(shared_codex_app_server_event_matches_active_turn(
        Some("turn-1"),
        false,
        Some("turn-1")
    ));
    assert!(!shared_codex_app_server_event_matches_active_turn(
        Some("turn-1"),
        false,
        Some("turn-2")
    ));
    assert!(shared_codex_app_server_event_matches_active_turn(
        Some("turn-1"),
        true,
        None
    ));
    assert!(!shared_codex_app_server_event_matches_active_turn(
        Some("turn-1"),
        false,
        None
    ));
}

#[test]
fn clear_shared_codex_turn_recorder_state_resets_all_fields() {
    let mut recorder_state = SessionRecorderState {
        command_messages: HashMap::from([("cmd-1".to_owned(), "Running".to_owned())]),
        parallel_agents_messages: HashMap::from([("parallel-1".to_owned(), "Working".to_owned())]),
        streaming_text_message_id: Some("message-1".to_owned()),
    };

    clear_shared_codex_turn_recorder_state(&mut recorder_state);

    assert!(recorder_state.command_messages.is_empty());
    assert!(recorder_state.parallel_agents_messages.is_empty());
    assert_eq!(recorder_state.streaming_text_message_id, None);
}

#[test]
fn clear_shared_codex_turn_session_state_resets_turn_local_fields_and_preserves_thread_id() {
    let mut session_state = SharedCodexSessionState {
        pending_turn_start_request_id: Some("turn-start-1".to_owned()),
        recorder: SessionRecorderState {
            command_messages: HashMap::from([("cmd-1".to_owned(), "Running".to_owned())]),
            parallel_agents_messages: HashMap::from([(
                "parallel-1".to_owned(),
                "Working".to_owned(),
            )]),
            streaming_text_message_id: Some("message-1".to_owned()),
        },
        thread_id: Some("thread-1".to_owned()),
        turn_id: Some("turn-1".to_owned()),
        completed_turn_id: Some("turn-0".to_owned()),
        turn_started: true,
        turn_state: CodexTurnState {
            current_agent_message_id: Some("assistant-1".to_owned()),
            streamed_agent_message_text_by_item_id: HashMap::from([(
                "item-1".to_owned(),
                "hello".to_owned(),
            )]),
            streamed_agent_message_item_ids: HashSet::from(["item-1".to_owned()]),
            pending_subagent_results: vec![PendingSubagentResult {
                title: "Worker".to_owned(),
                summary: "Done".to_owned(),
                conversation_id: Some("conversation-1".to_owned()),
                turn_id: Some("turn-1".to_owned()),
            }],
            assistant_output_started: true,
            first_visible_assistant_message_id: Some("visible-1".to_owned()),
        },
    };

    clear_shared_codex_turn_session_state(&mut session_state);

    assert_eq!(session_state.pending_turn_start_request_id, None);
    assert_eq!(session_state.thread_id.as_deref(), Some("thread-1"));
    assert_eq!(session_state.turn_id, None);
    assert_eq!(session_state.completed_turn_id, None);
    assert!(!session_state.turn_started);
    assert_eq!(session_state.turn_state.current_agent_message_id, None);
    assert!(
        session_state
            .turn_state
            .streamed_agent_message_text_by_item_id
            .is_empty()
    );
    assert!(
        session_state
            .turn_state
            .streamed_agent_message_item_ids
            .is_empty()
    );
    assert!(session_state.turn_state.pending_subagent_results.is_empty());
    assert!(!session_state.turn_state.assistant_output_started);
    assert_eq!(
        session_state.turn_state.first_visible_assistant_message_id,
        None
    );
    assert!(session_state.recorder.command_messages.is_empty());
    assert!(session_state.recorder.parallel_agents_messages.is_empty());
    assert_eq!(session_state.recorder.streaming_text_message_id, None);
}


fn accept_test_connection_with_timeout(
    listener: &std::net::TcpListener,
    label: &str,
    timeout: std::time::Duration,
) -> std::net::TcpStream {
    listener
        .set_nonblocking(true)
        .expect("test listener should support nonblocking mode");
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .expect("accepted test socket should support blocking mode");
                return stream;
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "{label} timed out waiting for a connection"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(err) => panic!("{label} failed to accept a connection: {err}"),
        }
    }
}

fn accept_test_connection(listener: &std::net::TcpListener, label: &str) -> std::net::TcpStream {
    accept_test_connection_with_timeout(listener, label, std::time::Duration::from_secs(2))
}

fn join_test_server(server: std::thread::JoinHandle<()>) {
    if let Err(panic) = server.join() {
        std::panic::resume_unwind(panic);
    }
}

struct TestHttpRequest {
    request_line: String,
    body: String,
}

fn read_test_http_request(stream: &mut std::net::TcpStream) -> TestHttpRequest {
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
    let request_line = headers
        .lines()
        .next()
        .expect("request line should exist")
        .to_owned();
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
    let body =
        String::from_utf8_lossy(&buffer[body_start..body_start + content_length]).to_string();

    TestHttpRequest { request_line, body }
}

fn write_test_http_response(
    stream: &mut std::net::TcpStream,
    status: StatusCode,
    content_type: &str,
    body: &str,
) {
    stream
        .write_all(
            format!(
                "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("OK"),
                content_type,
                body.as_bytes().len(),
                body
            )
            .as_bytes(),
        )
        .expect("test response should write");
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
        review_documents_lock: Arc::new(Mutex::new(())),
        state_events: broadcast::channel(16).0,
        delta_events: broadcast::channel(16).0,
        file_events: broadcast::channel(16).0,
        file_events_revision: Arc::new(AtomicU64::new(0)),
        persist_tx: mpsc::channel().0,
        state_broadcast_tx: mpsc::channel().0,
        shared_codex_runtime: Arc::new(Mutex::new(None)),
        agent_readiness_cache: Arc::new(RwLock::new(fresh_agent_readiness_cache("/tmp"))),
        agent_readiness_refresh_lock: Arc::new(Mutex::new(())),
        remote_registry: test_remote_registry(),
        remote_sse_fallback_resynced_revision: Arc::new(Mutex::new(HashMap::new())),
        terminal_local_command_semaphore: Arc::new(tokio::sync::Semaphore::new(
            TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT,
        )),
        terminal_remote_command_semaphore: Arc::new(tokio::sync::Semaphore::new(
            TERMINAL_REMOTE_COMMAND_CONCURRENCY_LIMIT,
        )),
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

static TEST_HOME_ENV_MUTEX: std::sync::LazyLock<std::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

#[cfg(windows)]
const TEST_HOME_ENV_KEY: &str = "USERPROFILE";
#[cfg(not(windows))]
const TEST_HOME_ENV_KEY: &str = "HOME";

struct ScopedEnvVar {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

impl ScopedEnvVar {
    fn set_path(key: &'static str, value: &FsPath) -> Self {
        let original = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value.as_os_str());
        }
        Self { key, original }
    }

    fn set(key: &'static str, value: &str) -> Self {
        let original = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, original }
    }
}

impl Drop for ScopedEnvVar {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = self.original.as_ref() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
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

fn create_test_remote_project(
    state: &AppState,
    remote: &RemoteConfig,
    root_path: &str,
    name: &str,
    remote_project_id: &str,
) -> String {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    if inner.find_remote(&remote.id).is_none() {
        inner.preferences.remotes.push(remote.clone());
    }
    let project = inner.create_project(
        Some(name.to_owned()),
        root_path.to_owned(),
        remote.id.clone(),
    );
    let index = inner
        .projects
        .iter()
        .position(|candidate| candidate.id == project.id)
        .expect("remote project should exist");
    inner.projects[index].remote_project_id = Some(remote_project_id.to_owned());
    state.commit_locked(&mut inner).unwrap();
    project.id
}

fn insert_test_remote_connection(state: &AppState, remote: &RemoteConfig, forwarded_port: u16) {
    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(true),
                event_bridge_shutdown: AtomicBool::new(false),
                supports_inline_orchestrator_templates: Mutex::new(None),
            }),
        );
}

fn sample_remote_orchestrator_state(
    remote_project_id: &str,
    root_path: &str,
    revision: u64,
    status: OrchestratorInstanceStatus,
) -> StateResponse {
    let draft = sample_orchestrator_template_draft();
    let template = OrchestratorTemplate {
        id: "remote-template-1".to_owned(),
        name: draft.name.clone(),
        description: draft.description.clone(),
        project_id: Some(remote_project_id.to_owned()),
        sessions: draft.sessions.clone(),
        transitions: draft.transitions.clone(),
        created_at: "2026-04-03 10:00:00".to_owned(),
        updated_at: "2026-04-03 10:00:00".to_owned(),
    };
    let remote_session_ids_by_template_session_id = draft
        .sessions
        .iter()
        .enumerate()
        .map(|(index, session)| (session.id.clone(), format!("remote-session-{}", index + 1)))
        .collect::<HashMap<_, _>>();
    let sessions = draft
        .sessions
        .iter()
        .map(|template_session| {
            let agent = template_session.agent;
            let mut session = Session {
                id: remote_session_ids_by_template_session_id[&template_session.id].clone(),
                name: template_session.name.clone(),
                emoji: agent.avatar().to_owned(),
                agent,
                workdir: root_path.to_owned(),
                project_id: Some(remote_project_id.to_owned()),
                model: template_session
                    .model
                    .clone()
                    .unwrap_or_else(|| agent.default_model().to_owned()),
                model_options: Vec::new(),
                approval_policy: None,
                reasoning_effort: None,
                sandbox_mode: None,
                cursor_mode: agent
                    .supports_cursor_mode()
                    .then_some(default_cursor_mode()),
                claude_effort: agent
                    .supports_claude_approval_mode()
                    .then_some(ClaudeEffortLevel::Default),
                claude_approval_mode: agent
                    .supports_claude_approval_mode()
                    .then_some(default_claude_approval_mode()),
                gemini_approval_mode: agent
                    .supports_gemini_approval_mode()
                    .then_some(default_gemini_approval_mode()),
                external_session_id: None,
                agent_commands_revision: 0,
                codex_thread_state: None,
                status: SessionStatus::Idle,
                preview: format!("Remote {} ready.", template_session.name),
                messages: Vec::new(),
                pending_prompts: Vec::new(),
            };
            if session.agent.supports_codex_prompt_settings() {
                session.approval_policy = Some(default_codex_approval_policy());
                session.reasoning_effort = Some(default_codex_reasoning_effort());
                session.sandbox_mode = Some(default_codex_sandbox_mode());
            }
            session
        })
        .collect::<Vec<_>>();
    let pending_transitions = if status == OrchestratorInstanceStatus::Stopped {
        Vec::new()
    } else {
        let transition = draft
            .transitions
            .first()
            .expect("sample draft should include a transition");
        vec![PendingTransition {
            id: "remote-pending-1".to_owned(),
            transition_id: transition.id.clone(),
            source_session_id: remote_session_ids_by_template_session_id
                [&transition.from_session_id]
                .clone(),
            destination_session_id: remote_session_ids_by_template_session_id
                [&transition.to_session_id]
                .clone(),
            completion_revision: 7,
            rendered_prompt: "Review the implementation.".to_owned(),
            created_at: "2026-04-03 10:05:00".to_owned(),
        }]
    };
    StateResponse {
        revision,
        codex: CodexState::default(),
        agent_readiness: Vec::new(),
        preferences: AppPreferences::default(),
        projects: Vec::new(),
        workspaces: Vec::new(),
        orchestrators: vec![OrchestratorInstance {
            id: "remote-orchestrator-1".to_owned(),
            remote_id: None,
            remote_orchestrator_id: None,
            template_id: template.id.clone(),
            project_id: remote_project_id.to_owned(),
            template_snapshot: template,
            status,
            session_instances: draft
                .sessions
                .iter()
                .map(|template_session| OrchestratorSessionInstance {
                    template_session_id: template_session.id.clone(),
                    session_id: remote_session_ids_by_template_session_id[&template_session.id]
                        .clone(),
                    last_completion_revision: None,
                    last_delivered_completion_revision: None,
                })
                .collect(),
            pending_transitions,
            created_at: "2026-04-03 10:00:00".to_owned(),
            error_message: None,
            completed_at: (status == OrchestratorInstanceStatus::Stopped)
                .then_some("2026-04-03 10:15:00".to_owned()),
            stop_in_progress: false,
            active_session_ids_during_stop: None,
            stopped_session_ids_during_stop: Vec::new(),
        }],
        sessions,
    }
}

fn run_git_test_command(repo_root: &FsPath, args: &[&str]) {
    let output = git_command()
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
    let output = git_command()
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

fn init_git_document_test_repo(repo_root: &FsPath) {
    run_git_test_command(repo_root, &["init"]);
    run_git_test_command(repo_root, &["config", "core.autocrlf", "false"]);
    run_git_test_command(repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(repo_root, &["config", "user.name", "TermAl"]);
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

#[test]
fn shared_codex_watched_writer_clears_activity_after_successful_write() {
    let activity: SharedCodexStdinActivityState = Arc::new(Mutex::new(None));
    let mut writer = SharedCodexWatchedWriter::new(SharedBufferWriter::default(), activity.clone());

    write_codex_json_rpc_message(&mut writer, &json_rpc_notification_message("initialized"))
        .expect("tracked shared Codex writer should write successfully");

    assert!(
        activity
            .lock()
            .expect("shared Codex stdin activity mutex poisoned")
            .is_none()
    );
}

fn take_pending_acp_request(
    pending_requests: &AcpPendingRequestMap,
    timeout: Duration,
) -> (
    String,
    std::sync::mpsc::Sender<std::result::Result<Value, AcpResponseError>>,
) {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(request) = {
            let mut locked = pending_requests
                .lock()
                .expect("ACP pending requests mutex poisoned");
            let request_id = locked.keys().next().cloned();
            request_id.and_then(|request_id| {
                locked
                    .remove(&request_id)
                    .map(|sender| (request_id, sender))
            })
        } {
            return request;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "ACP request should arrive before timeout"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn take_pending_codex_request(
    pending_requests: &CodexPendingRequestMap,
    timeout: Duration,
) -> (
    String,
    std::sync::mpsc::Sender<std::result::Result<Value, CodexResponseError>>,
) {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(request) = {
            let mut locked = pending_requests
                .lock()
                .expect("Codex pending requests mutex poisoned");
            let request_id = locked.keys().next().cloned();
            request_id.and_then(|request_id| {
                locked
                    .remove(&request_id)
                    .map(|sender| (request_id, sender))
            })
        } {
            return request;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Codex request should arrive before timeout"
        );
        std::thread::sleep(Duration::from_millis(10));
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
        sessions: SharedCodexSessions::new(),
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

async fn collect_sse_events(response: axum::response::Response) -> Vec<(String, String)> {
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("SSE response body should drain");
    let raw = std::str::from_utf8(&body).expect("SSE response should be UTF-8");
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    normalized
        .split("\n\n")
        .filter(|frame| frame.lines().any(|line| line.starts_with("event: ")))
        .map(parse_sse_event)
        .collect()
}

// Tests that wait for shared child exit timeout returns status for completed process.
#[test]
fn wait_for_shared_child_exit_timeout_returns_status_for_completed_process() {
    let child = test_exit_success_child();
    let process = Arc::new(SharedChild::new(child).unwrap());

    let status = wait_for_shared_child_exit_timeout(&process, Duration::from_secs(1), "test child")
        .unwrap()
        .expect("completed process should return a status");

    assert!(status.success());
}

// Tests that wait for shared child exit timeout returns none for running process.
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
fn wait_for_terminal_command_status_returns_status_without_cancellation_delay() {
    let child = test_exit_success_child();
    let process = Arc::new(SharedChild::new(child).unwrap());
    let cancellation = AtomicBool::new(false);

    let (status, cancelled) =
        wait_for_terminal_command_status(&process, None, Some(&cancellation)).unwrap();

    assert!(!cancelled);
    assert!(
        status
            .expect("completed process should return a status")
            .success(),
        "completed process should be successful"
    );
}

#[test]
fn wait_for_terminal_command_status_observes_mid_wait_cancellation() {
    let child = test_sleep_child();
    let process = Arc::new(SharedChild::new(child).unwrap());
    let cancellation = Arc::new(AtomicBool::new(false));
    let waiter_process = process.clone();
    let waiter_cancellation = cancellation.clone();
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<
        Result<(Option<std::process::ExitStatus>, bool), ApiError>,
    >(1);
    let waiter = std::thread::spawn(move || {
        let result =
            wait_for_terminal_command_status(&waiter_process, None, Some(&waiter_cancellation));
        let _ = done_tx.send(result);
    });

    std::thread::sleep(Duration::from_millis(20));
    cancellation.store(true, Ordering::SeqCst);

    let (status, cancelled) = done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("waiter should observe cancellation")
        .expect("waiter should not error");
    assert!(status.is_none());
    assert!(cancelled);

    process.kill().unwrap();
    process.wait().unwrap();
    waiter.join().unwrap();
}

#[test]
fn terminal_command_sse_stream_drop_cancels_before_first_poll() {
    let cancellation = Arc::new(AtomicBool::new(false));
    let (_event_tx, event_rx) = tokio::sync::mpsc::channel::<TerminalCommandStreamEvent>(
        TERMINAL_STREAM_EVENT_QUEUE_CAPACITY,
    );
    let stream = TerminalCommandSseStream {
        event_rx,
        _cancel_on_drop: TerminalStreamCancelGuard {
            cancellation: cancellation.clone(),
        },
    };

    assert!(!cancellation.load(Ordering::SeqCst));
    drop(stream);
    assert!(cancellation.load(Ordering::SeqCst));
}

#[tokio::test]
async fn spawn_terminal_stream_worker_reports_panics_as_error_events() {
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TerminalCommandStreamEvent>(
        TERMINAL_STREAM_EVENT_QUEUE_CAPACITY,
    );

    spawn_terminal_stream_worker(event_tx, async {
        panic!("synthetic terminal stream worker panic");
    });

    let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
        .await
        .expect("panic recovery should emit an event")
        .expect("panic recovery should keep the event channel open for the error");
    match event {
        TerminalCommandStreamEvent::Error { error, status } => {
            assert_eq!(error, "terminal stream task panicked");
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR.as_u16());
        }
        _ => panic!("expected terminal stream worker panic to emit an error event"),
    }

    let channel_closed = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
        .await
        .expect("panic recovery sender should be dropped after reporting the error");
    assert!(
        channel_closed.is_none(),
        "panic recovery should report exactly one terminal stream event"
    );
}

// Tests that shutdown REPL Codex process forces running process after timeout.
#[test]
fn shutdown_repl_codex_process_forces_running_process_after_timeout() {
    let child = test_sleep_child();
    let process = Arc::new(SharedChild::new(child).unwrap());

    let (status, forced_shutdown) = shutdown_repl_codex_process(&process).unwrap();

    assert!(forced_shutdown);
    assert!(!status.success());
}





// Tests that Gemini falls back from a rejected session/load to a new ACP session.
#[test]
fn gemini_invalid_session_load_falls_back_to_session_new() {
    // Hold the home-env mutex and set a dummy GEMINI_API_KEY so
    // validate_agent_session_setup passes on machines without real Gemini
    // credentials.  The API key is never used for a real network call — the
    // ACP runtime is driven by SharedBufferWriter throughout the test.
    let _env_lock = TEST_HOME_ENV_MUTEX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _api_key = ScopedEnvVar::set("GEMINI_API_KEY", "test-key-not-real");

    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Gemini),
            name: Some("Gemini Resume".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gemini-pro".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: Some(default_gemini_approval_mode()),
        })
        .expect("Gemini session should be created");
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: None,
        is_loading_history: false,
        supports_session_load: Some(true),
    }));
    let writer = SharedBufferWriter::default();
    let thread_writer = writer.clone();
    let thread_pending_requests = pending_requests.clone();
    let thread_state = state.clone();
    let thread_runtime_state = runtime_state.clone();
    let thread_session_id = created.session_id.clone();
    let handle = std::thread::spawn(move || {
        let mut stdin = thread_writer;
        ensure_acp_session_ready(
            &mut stdin,
            &thread_pending_requests,
            &thread_state,
            &thread_session_id,
            &thread_runtime_state,
            AcpAgent::Gemini,
            &AcpPromptCommand {
                cwd: "/tmp".to_owned(),
                cursor_mode: None,
                model: "gemini-pro".to_owned(),
                prompt: "Resume the prior session".to_owned(),
                resume_session_id: Some("gemini-session-stale".to_owned()),
            },
        )
    });

    let (_load_request_id, load_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    load_sender
        .send(Err(AcpResponseError::JsonRpc(AcpJsonRpcError {
            code: Some(-32602),
            message: "Invalid session identifier".to_owned(),
            data: Some(json!({
                "reason": "invalidSessionIdentifier",
            })),
        })))
        .expect("session/load response should send");

    let (_new_request_id, new_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    new_sender
        .send(Ok(json!({
            "sessionId": "gemini-session-new",
            "configOptions": [
                {
                    "id": "model",
                    "currentValue": "gemini-pro",
                    "options": [
                        {
                            "value": "gemini-pro",
                            "name": "Gemini Pro"
                        }
                    ]
                }
            ]
        })))
        .expect("session/new response should send");

    let external_session_id = handle
        .join()
        .expect("Gemini ACP worker should finish")
        .expect("Gemini fallback should recover with a new session");
    assert_eq!(external_session_id, "gemini-session-new");
    let written = writer.contents();
    let load_index = written
        .find("\"method\":\"session/load\"")
        .expect("session/load request should be written");
    let new_index = written
        .find("\"method\":\"session/new\"")
        .expect("session/new request should be written");
    assert!(
        load_index < new_index,
        "session/load should happen before session/new\n{written}"
    );
    let session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Gemini session should be present");
    assert_eq!(
        session.external_session_id.as_deref(),
        Some("gemini-session-new")
    );
    assert_eq!(
        runtime_state
            .lock()
            .expect("ACP runtime state mutex poisoned")
            .current_session_id
            .as_deref(),
        Some("gemini-session-new")
    );
}

// Tests that shared Codex global notices update Codex state.
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
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &config_warning,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &deprecation_notice,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
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

// Tests that shared Codex threadless runtime notice is recorded.
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
        &mpsc::channel::<CodexRuntimeCommand>().0,
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


// Tests that Codex app server command approval request records pending approval.
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

// Tests that Codex app server file change approval request records pending approval.
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

// Tests that Codex app server permissions approval request records pending approval.
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

// Tests that Codex app server user input request records pending request.
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

// Tests that Codex app server MCP elicitation request records pending request.
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

// Tests that Codex app server generic request records pending request.
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

// Tests that REPL Codex task complete event buffers subagent result until final message.
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

// Tests that REPL Codex streamed agent message appends missing completed suffix.
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

// Tests that REPL Codex streamed agent message replaces divergent completed text.
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
// Tests that REPL Codex streamed agent message skips duplicate completed text.
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

// Tests that REPL Codex final agent messages still land after turn completion.
#[test]
fn repl_codex_agent_message_after_turn_completed_is_recorded() {
    let mut recorder = TestRecorder::default();
    let mut repl_state = ReplCodexSessionState::default();
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
                "message": "Late REPL answer.",
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
        "turn/completed",
        &turn_completed,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "codex/event/agent_message",
        &late_message,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(recorder.texts, vec!["Late REPL answer.".to_owned()]);
    assert!(repl_state.turn_completed);
    assert!(repl_state.current_turn_id.is_none());
}

// Tests that REPL Codex app-server agentMessage completions still land after turn completion.
#[test]
fn repl_codex_app_server_agent_message_completed_after_turn_completed_is_recorded() {
    let mut recorder = TestRecorder::default();
    let mut repl_state = ReplCodexSessionState::default();
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
    let late_item = json!({
        "method": "item/completed",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "id": "item-final",
                "type": "agentMessage",
                "text": "Late REPL item answer."
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
        "turn/completed",
        &turn_completed,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "item/completed",
        &late_item,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(recorder.texts, vec!["Late REPL item answer.".to_owned()]);
    assert!(repl_state.turn_completed);
    assert!(repl_state.current_turn_id.is_none());
}

// Tests that Codex app server web search item records command lifecycle.
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

// Tests that Codex app server file change item records create and edit diffs.
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

// Tests that Codex delta suffix deduplicates cumulative and overlapping chunks.
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

// Tests that Codex delta suffix handles multibyte UTF-8 characters.
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

// Tests that shared Codex agent message event uses conversation ID for session routing.
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
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
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

// Tests that subagent results append after existing assistant text.
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

// Tests that clear runtime commits revision when it resets state.
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

// Tests that reuses shared Codex runtime across sessions.
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

// Tests that stops shared Codex sessions via turn interrupt.
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

// Tests that stop session detaches shared Codex session when interrupt fails.
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
        sessions: SharedCodexSessions::new(),
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

// Tests that stop session dispatches queued prompt after shared Codex interrupt failure.
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
        sessions: SharedCodexSessions::new(),
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

// Tests that stop session returns an error when a dedicated runtime refuses to stop.
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

// Tests that stop session keeps the previous state visible until shutdown completes.
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

// Tests that stop session returns conflict when already stopping.
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

// Tests that runtime turn callbacks are suppressed while stop is in progress.
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
fn fail_turn_if_runtime_matches_publishes_error_state_when_persist_fails() {
    let mut state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, _input_rx) = test_claude_runtime_handle("claude-fail-turn-persist-fallback");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-fail-turn-persist-fallback-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
    }

    state.persistence_path = Arc::new(failing_persistence_path.clone());
    state.persist_tx = mpsc::channel().0;

    let baseline_revision = state.snapshot().revision;
    let mut state_events = state.subscribe_events();

    state
        .fail_turn_if_runtime_matches(&session_id, &runtime_token, "persist fallback failure")
        .expect("fail_turn_if_runtime_matches should publish even when persistence fails");

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(snapshot.revision, baseline_revision + 1);
    assert_eq!(session.status, SessionStatus::Error);
    assert_eq!(session.preview, "persist fallback failure");
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Turn failed: persist fallback failure"
    ));

    let published: StateResponse = serde_json::from_str(
        &state_events
            .try_recv()
            .expect("fail_turn_if_runtime_matches should publish a state snapshot"),
    )
    .expect("published state snapshot should decode");
    let published_session = published
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("published session should be present");
    assert_eq!(published.revision, snapshot.revision);
    assert_eq!(published_session.status, SessionStatus::Error);
    assert_eq!(published_session.preview, "persist fallback failure");
    assert!(matches!(
        published_session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Turn failed: persist fallback failure"
    ));

    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Tests that Codex thread state updates are suppressed while stop is in progress.
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

// Tests that shared Codex runtime exit clears state and kills the helper process.
#[test]
fn shared_codex_runtime_exit_clears_state_and_kills_process() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = SharedCodexRuntime {
        runtime_id: "shared-codex-timeout".to_owned(),
        input_tx,
        process: process.clone(),
        sessions: SharedCodexSessions::new(),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
    };
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
        let mut shared_runtime = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        *shared_runtime = Some(runtime.clone());
    }

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(handle);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
    }

    state
        .handle_shared_codex_runtime_exit(
            "shared-codex-timeout",
            Some("failed to communicate with shared Codex app-server"),
        )
        .expect("shared Codex runtime exit should succeed");

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Codex session should remain present");
    assert_eq!(session.status, SessionStatus::Error);
    assert!(
        session
            .preview
            .contains("failed to communicate with shared Codex app-server")
    );

    assert!(
        state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned")
            .is_none()
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert!(matches!(record.runtime, SessionRuntime::None));
    drop(inner);

    let _ = process.kill();
    let _ = wait_for_shared_child_exit_timeout(
        &process,
        Duration::from_secs(3),
        "shared Codex runtime",
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn shared_codex_stdin_watchdog_times_out_stalled_writer_and_clears_runtime() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = SharedCodexRuntime {
        runtime_id: "shared-codex-stdin-watchdog".to_owned(),
        input_tx,
        process: process.clone(),
        sessions: SharedCodexSessions::new(),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
    };
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
        let mut shared_runtime = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        *shared_runtime = Some(runtime.clone());
    }

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(handle);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
    }

    let activity: SharedCodexStdinActivityState =
        Arc::new(Mutex::new(Some(SharedCodexStdinActivity {
            operation: "flush",
            started_at: std::time::Instant::now() - Duration::from_millis(50),
            timed_out: false,
        })));
    let (_stop_tx, stop_rx) = mpsc::channel();
    spawn_shared_codex_stdin_watchdog(
        &state,
        &runtime.runtime_id,
        process.clone(),
        &activity,
        stop_rx,
        Duration::from_millis(10),
        Duration::from_millis(5),
    )
    .expect("shared Codex stdin watchdog should spawn");

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let cleared = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned")
            .is_none();
        if cleared {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "shared Codex stdin watchdog should tear down the stalled runtime"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Codex session should remain present");
    assert_eq!(session.status, SessionStatus::Error);
    assert!(
        session.preview.contains("Agent communication timed out"),
        "watchdog timeout should use generic message, got: {}",
        session.preview,
    );

    let _ = process.kill();
    let _ = wait_for_shared_child_exit_timeout(
        &process,
        Duration::from_secs(3),
        "shared Codex runtime",
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that runtime exit is suppressed while stop is in progress.
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

// Tests that successful stop discards deferred callbacks.
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

// Tests that failed dedicated stop replays deferred turn completion.
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

// Tests that failed dedicated stop replays deferred runtime exit.
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

// Tests that failed dedicated stop replays multiple deferred callbacks in order.
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

// Tests that failed dedicated stop replays runtime exit last even when it arrives first.
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


// Tests that canonicalizes session model updates from live model labels.
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

// Tests that revisions increase for visible state changes.
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
    assert_eq!(created.revision, 1);

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

    let renamed = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: Some("Revision Test Renamed".to_owned()),
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
    assert_eq!(renamed.revision, 3);
}

// Tests that renames sessions via settings updates.
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

// Tests that the monotonic mutation counter advances exactly once per
// `next_mutation_stamp` call and never decreases. The persist thread's
// delta logic depends on stamps being strictly monotonic within a
// process lifetime; a regression that skipped, repeated, or rolled back
// a stamp would either re-write sessions needlessly or silently skip
// writes.
#[test]
fn state_inner_next_mutation_stamp_is_strictly_monotonic() {
    let mut inner = StateInner::new();
    assert_eq!(inner.last_mutation_stamp, 0);

    let first = inner.next_mutation_stamp();
    let second = inner.next_mutation_stamp();
    let third = inner.next_mutation_stamp();

    assert_eq!(first, 1);
    assert_eq!(second, 2);
    assert_eq!(third, 3);
    assert_eq!(inner.last_mutation_stamp, 3);
}

// Tests that `session_mut` / `session_mut_by_index` / `stamp_session_at_index`
// all bump the monotonic counter and stamp the targeted session record,
// so any routing of mutations through these helpers causes the
// persist thread to see the session as dirty on the next tick.
#[test]
fn state_inner_session_mut_helpers_stamp_the_record() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let initial_stamp = inner
        .find_session_index(&session_id)
        .map(|index| inner.sessions[index].mutation_stamp)
        .expect("session should exist");
    let initial_counter = inner.last_mutation_stamp;

    {
        let record = inner
            .session_mut(&session_id)
            .expect("session_mut should find the session");
        assert!(record.mutation_stamp > initial_stamp);
        assert_eq!(record.mutation_stamp, initial_counter + 1);
    }
    let after_session_mut_counter = inner.last_mutation_stamp;
    assert_eq!(after_session_mut_counter, initial_counter + 1);

    let index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    {
        let record = inner
            .session_mut_by_index(index)
            .expect("session_mut_by_index should succeed");
        assert_eq!(record.mutation_stamp, after_session_mut_counter + 1);
    }
    let after_indexed_counter = inner.last_mutation_stamp;
    assert_eq!(after_indexed_counter, after_session_mut_counter + 1);

    let stamped = inner
        .stamp_session_at_index(index)
        .expect("stamp_session_at_index should succeed");
    assert_eq!(stamped, after_indexed_counter + 1);
    assert_eq!(inner.sessions[index].mutation_stamp, stamped);
    assert_eq!(inner.last_mutation_stamp, stamped);
}

// Tests that `record_removed_session` collects non-empty session ids for
// the persist thread to drain, and ignores empty ids so a misuse does
// not generate useless `DELETE WHERE id = ''` statements.
#[test]
fn state_inner_record_removed_session_accumulates_ids() {
    let mut inner = StateInner::new();
    assert!(inner.removed_session_ids.is_empty());

    inner.record_removed_session("session-1".to_owned());
    inner.record_removed_session("session-2".to_owned());
    inner.record_removed_session(String::new());
    inner.record_removed_session("session-3".to_owned());

    assert_eq!(
        inner.removed_session_ids,
        vec![
            "session-1".to_owned(),
            "session-2".to_owned(),
            "session-3".to_owned(),
        ],
    );
}
// Tests that normalize Git repo relative path rejects parent traversal components.
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

// Tests that normalize Git repo relative path rejects rooted paths.
#[test]
fn normalize_git_repo_relative_path_rejects_rooted_paths() {
    for path in ["/etc/passwd.md", r"\etc\passwd.md"] {
        let error =
            normalize_git_repo_relative_path(path).expect_err("rooted paths should be rejected");

        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            error.message,
            "git file actions require repository-relative paths"
        );
    }

    assert_eq!(
        normalize_git_repo_relative_path("./foo.md").unwrap(),
        "./foo.md"
    );
}

// Tests that normalize Git repo relative path rejects drive-prefixed Windows paths.
#[cfg(windows)]
#[test]
fn normalize_git_repo_relative_path_rejects_windows_prefix_paths() {
    for path in [
        r"C:\Windows\System32\drivers\etc\hosts",
        r"C:foo.md",
        r"\\server\share\file.md",
        r"\\?\C:\Windows\System32\drivers\etc\hosts",
        r"\\.\COM1",
    ] {
        let error = normalize_git_repo_relative_path(path)
            .expect_err("drive-prefixed paths should be rejected");

        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            error.message,
            "git file actions require repository-relative paths"
        );
    }
}

// Tests that rejects projects with unknown remote.
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

// Tests that creates sessions for remote projects over SSH.
#[test]
#[ignore = "requires a reachable SSH remote"]
fn creates_sessions_for_remote_projects_over_ssh() {
    let state = test_app_state();

    state
        .update_app_settings(UpdateAppSettingsRequest {
            default_codex_reasoning_effort: None,
            default_claude_approval_mode: None,
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

// Tests that creates projects and assigns sessions to them.
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
        .session
        .as_ref()
        .expect("created session should be returned");

    assert_eq!(
        session.project_id.as_deref(),
        Some(project.project_id.as_str())
    );
    assert_eq!(session.workdir, expected_root);
}

// Tests that deleting a project keeps its sessions valid and visible globally.
#[test]
fn deletes_projects_and_unassigns_existing_sessions() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-delete-project-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Delete Project".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
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
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .unwrap()
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project.project_id.clone()),
            template: None,
        })
        .unwrap()
        .orchestrator;

    let deleted = state.delete_project(&project.project_id).unwrap();

    assert!(
        deleted
            .projects
            .iter()
            .all(|entry| entry.id != project.project_id)
    );
    let session = deleted
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("created session should remain visible");
    assert_eq!(session.project_id, None);
    let deleted_orchestrator = deleted
        .orchestrators
        .iter()
        .find(|instance| instance.id == orchestrator.id)
        .expect("created orchestrator should remain visible");
    assert_eq!(deleted_orchestrator.project_id, "");

    fs::remove_dir_all(root).unwrap();
}

// Tests that rejects session workdirs outside the selected project.
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

// Tests that rejects empty project roots.
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

// Tests that resolves requested paths inside the session project root.
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

// Tests that allows new file paths inside the session project root.
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

// Tests that resolves project scoped paths without a session.
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


// Tests that project scoped paths require a session or project identifier.
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

// Tests that read directory accepts project ID without session.
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

// Tests that API router sets local CORS headers.
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

// Tests that health route reports inline orchestrator template compatibility.
#[tokio::test]
async fn health_route_reports_inline_orchestrator_template_support() {
    let (status, response): (StatusCode, Value) = request_json(
        &app_router(test_app_state()),
        Request::builder()
            .method("GET")
            .uri("/api/health")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        response,
        json!({
            "ok": true,
            "supportsInlineOrchestratorTemplates": true,
        })
    );
}

#[tokio::test]
async fn terminal_run_route_rejects_invalid_requests() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-terminal-validation-{}", Uuid::new_v4()));
    let outside_root = std::env::temp_dir().join(format!(
        "termal-terminal-validation-outside-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&outside_root).unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Terminal".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    let app = app_router(state);
    let project_id = project.project_id;
    let root_path = root.to_string_lossy().into_owned();
    let outside_path = outside_root.to_string_lossy().into_owned();

    let (empty_status, empty_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "   ",
                    "projectId": project_id,
                    "workdir": root_path,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(empty_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        empty_response,
        json!({ "error": "terminal command cannot be empty" })
    );

    let (empty_workdir_status, empty_workdir_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo ok",
                    "projectId": project_id,
                    "workdir": "   ",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(empty_workdir_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        empty_workdir_response,
        json!({ "error": "terminal workdir cannot be empty" })
    );

    let oversized_workdir = "a".repeat(TERMINAL_WORKDIR_MAX_CHARS + 1);
    let (oversized_workdir_status, oversized_workdir_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo ok",
                    "projectId": project_id,
                    "workdir": oversized_workdir,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(oversized_workdir_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        oversized_workdir_response,
        json!({
            "error": format!(
                "terminal workdir cannot exceed {TERMINAL_WORKDIR_MAX_CHARS} characters"
            )
        })
    );

    let (nul_workdir_status, nul_workdir_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo ok",
                    "projectId": project_id,
                    "workdir": "/repo\0/bad",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(nul_workdir_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        nul_workdir_response,
        json!({ "error": "terminal workdir cannot contain NUL bytes" })
    );

    let (oversized_status, oversized_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "x".repeat(TERMINAL_COMMAND_MAX_CHARS + 1),
                    "projectId": project_id,
                    "workdir": root_path,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(oversized_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        oversized_response,
        json!({
            "error": format!(
                "terminal command cannot exceed {TERMINAL_COMMAND_MAX_CHARS} characters"
            )
        })
    );

    let (outside_status, outside_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo ok",
                    "projectId": project_id,
                    "workdir": outside_path,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(outside_status, StatusCode::BAD_REQUEST);
    assert!(
        outside_response["error"]
            .as_str()
            .unwrap()
            .contains("must stay inside project")
    );

    let (multibyte_status, multibyte_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "\u{00e9}".repeat(TERMINAL_COMMAND_MAX_CHARS),
                    "projectId": project_id,
                    "workdir": outside_path,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(multibyte_status, StatusCode::BAD_REQUEST);
    let multibyte_error = multibyte_response["error"].as_str().unwrap();
    assert!(
        multibyte_error.contains("must stay inside project"),
        "expected scope validation, got {multibyte_error:?}"
    );
    assert!(!multibyte_error.contains("cannot exceed"));

    // The leading `#` is load-bearing: it makes the 20K-char body a shell
    // comment, proving character-count validation without executing it.
    let valid_multibyte_command = format!("#{}", "\u{00e9}".repeat(TERMINAL_COMMAND_MAX_CHARS - 1));
    let (valid_multibyte_status, valid_multibyte_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": valid_multibyte_command,
                    "projectId": project_id,
                    "workdir": root_path,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(valid_multibyte_status, StatusCode::OK);
    assert_eq!(
        valid_multibyte_response["command"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        TERMINAL_COMMAND_MAX_CHARS
    );

    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(outside_root).unwrap();
}

#[tokio::test]
async fn terminal_run_route_validates_remote_scoped_requests_before_proxying() {
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
    let project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Terminal",
        "remote-project-1",
    );
    let app = app_router(state);

    let (empty_status, empty_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "   ",
                    "projectId": project_id,
                    "workdir": "/remote/repo",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(empty_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        empty_response,
        json!({ "error": "terminal command cannot be empty" })
    );

    let (empty_workdir_status, empty_workdir_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo ok",
                    "projectId": project_id,
                    "workdir": "   ",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(empty_workdir_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        empty_workdir_response,
        json!({ "error": "terminal workdir cannot be empty" })
    );

    let oversized_workdir = "a".repeat(TERMINAL_WORKDIR_MAX_CHARS + 1);
    let (oversized_workdir_status, oversized_workdir_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo ok",
                    "projectId": project_id,
                    "workdir": oversized_workdir,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(oversized_workdir_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        oversized_workdir_response,
        json!({
            "error": format!(
                "terminal workdir cannot exceed {TERMINAL_WORKDIR_MAX_CHARS} characters"
            )
        })
    );

    let (nul_workdir_status, nul_workdir_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo ok",
                    "projectId": project_id,
                    "workdir": "/remote\0/repo",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(nul_workdir_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        nul_workdir_response,
        json!({ "error": "terminal workdir cannot contain NUL bytes" })
    );

    let (oversized_status, oversized_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "x".repeat(TERMINAL_COMMAND_MAX_CHARS + 1),
                    "projectId": project_id,
                    "workdir": "/remote/repo",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(oversized_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        oversized_response,
        json!({
            "error": format!(
                "terminal command cannot exceed {TERMINAL_COMMAND_MAX_CHARS} characters"
            )
        })
    );
}

#[cfg(windows)]
fn terminal_exact_stdout_command(text: &str) -> String {
    format!("[Console]::Out.Write('{}')", text.replace('\'', "''"))
}

#[cfg(not(windows))]
fn terminal_exact_stdout_command(text: &str) -> String {
    format!("printf %s {}", shell_single_quote(text))
}

#[tokio::test]
async fn terminal_run_stream_route_emits_output_before_complete() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-terminal-stream-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let command = terminal_exact_stdout_command("stream-ok");
    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Terminal Stream".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    let app = app_router(state);
    let response = request_response(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run/stream")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": command.clone(),
                    "projectId": project.project_id,
                    "workdir": root.to_string_lossy(),
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let events = collect_sse_events(response).await;
    let output_index = events
        .iter()
        .position(|(event_name, _)| event_name == "output")
        .expect("stream should emit stdout before completion");
    let complete_events = events
        .iter()
        .enumerate()
        .filter(|(_, (event_name, _))| event_name == "complete")
        .collect::<Vec<_>>();
    assert_eq!(complete_events.len(), 1, "events: {events:?}");
    assert!(
        events.iter().all(|(event_name, _)| event_name != "error"),
        "successful stream should not emit an error: {events:?}"
    );
    assert!(
        output_index < complete_events[0].0,
        "stdout should arrive before completion: {events:?}"
    );
    let stdout = events
        .iter()
        .filter(|(event_name, _)| event_name == "output")
        .map(|(_, event_data)| {
            let output: Value =
                serde_json::from_str(event_data).expect("output event should decode");
            assert_eq!(output["stream"], Value::String("stdout".to_owned()));
            output["text"].as_str().unwrap().to_owned()
        })
        .collect::<String>();
    assert_eq!(stdout, "stream-ok");

    let complete_data = &complete_events[0].1.1;
    let complete = serde_json::from_str::<TerminalCommandResponse>(complete_data)
        .expect("complete event should decode");
    assert_eq!(complete.command, command);
    assert_eq!(complete.stdout, "stream-ok");
    assert!(complete.success);

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn terminal_run_stream_route_returns_http_error_for_bad_workdir() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-terminal-stream-bad-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let outside = root
        .parent()
        .expect("temp project root should have a parent")
        .to_string_lossy()
        .into_owned();
    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Terminal Stream Bad Workdir".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    let app = app_router(state);

    let (status, response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run/stream")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo stream-ok",
                    "projectId": project.project_id,
                    "workdir": outside,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("must stay inside project"),
        "unexpected error body: {response}"
    );

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn terminal_run_stream_route_limits_local_and_remote_concurrent_commands() {
    let state = test_app_state();
    let root =
        std::env::temp_dir().join(format!("termal-terminal-stream-limit-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Terminal Stream Limit".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    let remote = RemoteConfig {
        id: "ssh-stream-limit".to_owned(),
        name: "SSH Stream Limit".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let remote_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Stream Limit",
        "remote-stream-limit-project",
    );
    let local_permits = (0..TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT)
        .map(|_| {
            state
                .terminal_local_command_semaphore
                .clone()
                .try_acquire_owned()
                .expect("test should acquire local stream permit")
        })
        .collect::<Vec<_>>();
    let remote_permits = (0..TERMINAL_REMOTE_COMMAND_CONCURRENCY_LIMIT)
        .map(|_| {
            state
                .terminal_remote_command_semaphore
                .clone()
                .try_acquire_owned()
                .expect("test should acquire remote stream permit")
        })
        .collect::<Vec<_>>();
    let app = app_router(state);

    let (local_status, local_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run/stream")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo local",
                    "projectId": project.project_id,
                    "workdir": root.to_string_lossy(),
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(local_status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        local_response["error"]
            .as_str()
            .unwrap()
            .contains("too many local terminal commands"),
        "unexpected local stream 429 body: {local_response}"
    );

    let (remote_status, remote_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run/stream")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo remote",
                    "projectId": remote_project_id,
                    "workdir": "/remote/repo",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(remote_status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        remote_response["error"]
            .as_str()
            .unwrap()
            .contains("too many remote terminal commands"),
        "unexpected remote stream 429 body: {remote_response}"
    );

    drop(local_permits);
    drop(remote_permits);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn terminal_run_stream_route_emits_error_without_complete_when_spawn_fails() {
    let state = test_app_state();
    let root =
        std::env::temp_dir().join(format!("termal-terminal-stream-error-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let not_a_directory = root.join("not-a-directory.txt");
    fs::write(&not_a_directory, "not a directory").unwrap();
    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Terminal Stream Error".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    let app = app_router(state);

    let response = request_response(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run/stream")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": terminal_exact_stdout_command("unreachable"),
                    "projectId": project.project_id,
                    "workdir": not_a_directory.to_string_lossy(),
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let events = collect_sse_events(response).await;
    let error_events = events
        .iter()
        .filter(|(event_name, _)| event_name == "error")
        .collect::<Vec<_>>();
    assert_eq!(error_events.len(), 1, "events: {events:?}");
    assert!(
        events
            .iter()
            .all(|(event_name, _)| event_name != "complete"),
        "spawn failure should not emit complete: {events:?}"
    );
    let payload: Value =
        serde_json::from_str(&error_events[0].1).expect("error event should decode");
    assert_eq!(
        payload["status"],
        Value::from(StatusCode::INTERNAL_SERVER_ERROR.as_u16())
    );
    assert!(
        payload["error"]
            .as_str()
            .unwrap()
            .contains("failed to start terminal command"),
        "unexpected error event payload: {payload}"
    );

    fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn terminal_run_route_limits_concurrent_commands() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-terminal-limit-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Terminal Limit".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    let remote_captured_body = Arc::new(Mutex::new(None::<String>));
    let remote_captured_for_server = remote_captured_body.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let remote_port = listener.local_addr().expect("listener addr").port();
    let remote_command = "echo remote";
    let remote_response_body = serde_json::to_string(&TerminalCommandResponse {
        command: remote_command.to_owned(),
        duration_ms: 7,
        exit_code: Some(0),
        output_truncated: false,
        shell: "sh".to_owned(),
        stderr: String::new(),
        stdout: "remote\n".to_owned(),
        success: true,
        timed_out: false,
        workdir: "/remote/repo".to_owned(),
    })
    .expect("terminal response should encode");
    let remote_server = std::thread::spawn(move || {
        // Loop until the terminal run request is captured rather than
        // hard-coding the number of proxy round-trips (see
        // `terminal_run_route_proxies_valid_remote_multibyte_commands` for
        // the full rationale).
        loop {
            let mut stream = accept_test_connection_with_timeout(
                &listener,
                "terminal limit remote listener",
                std::time::Duration::from_secs(10),
            );
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
            let request_line = headers
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();
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

            if request_line.starts_with("POST /api/terminal/run ") {
                *remote_captured_for_server
                    .lock()
                    .expect("capture mutex poisoned") = Some(body);
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            remote_response_body.len(),
                            remote_response_body
                        )
                        .as_bytes(),
                    )
                    .expect("terminal response should write");
                break;
            }

            panic!("unexpected request: {request_line}");
        }
    });
    let remote = RemoteConfig {
        id: "ssh-terminal-limit".to_owned(),
        name: "SSH Terminal Limit".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let remote_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Terminal Limit",
        "remote-terminal-limit-project",
    );
    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port: remote_port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(true),
                event_bridge_shutdown: AtomicBool::new(false),
                supports_inline_orchestrator_templates: Mutex::new(None),
            }),
        );
    let mut permits = (0..TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT)
        .map(|_| {
            state
                .terminal_local_command_semaphore
                .clone()
                .try_acquire_owned()
                .expect("test should acquire local terminal permit")
        })
        .collect::<Vec<_>>();
    let semaphore_state = state.clone();
    let project_id = project.project_id;
    let root_path = root.to_string_lossy().into_owned();
    let app = app_router(state);

    let (status, response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo ok",
                    "projectId": project_id,
                    "workdir": root_path,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let error_body = response["error"]
        .as_str()
        .expect("429 response should include error string");
    assert!(
        error_body.contains("too many local terminal commands"),
        "unexpected 429 body {error_body:?}"
    );
    // Pin the interpolated limit substring so a future `format!` typo or a
    // silent divergence between the string literal and the constant would
    // be caught instead of silently dropping the count.
    assert!(
        error_body.contains(&format!(
            "limit is {TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT}"
        )),
        "429 body {error_body:?} should interpolate TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT"
    );

    drop(permits.pop());
    let (released_status, released_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo released",
                    "projectId": project_id,
                    "workdir": root_path,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(released_status, StatusCode::OK);
    assert_eq!(
        released_response["command"],
        Value::String("echo released".to_owned())
    );
    assert!(
        released_response["stdout"]
            .as_str()
            .unwrap()
            .contains("released")
    );

    permits.push(
        semaphore_state
            .terminal_local_command_semaphore
            .clone()
            .try_acquire_owned()
            .expect("successful command should release its local terminal permit"),
    );
    let (relimited_status, relimited_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo blocked-again",
                    "projectId": project_id,
                    "workdir": root_path,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(relimited_status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        relimited_response["error"]
            .as_str()
            .unwrap()
            .contains("too many local terminal commands")
    );
    drop(permits);

    let remote_permits = (0..TERMINAL_REMOTE_COMMAND_CONCURRENCY_LIMIT)
        .map(|_| {
            semaphore_state
                .terminal_remote_command_semaphore
                .clone()
                .try_acquire_owned()
                .expect("test should acquire remote terminal permit")
        })
        .collect::<Vec<_>>();
    let (local_status, local_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo local",
                    "projectId": project_id,
                    "workdir": root_path,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(local_status, StatusCode::OK);
    assert!(local_response["stdout"].as_str().unwrap().contains("local"));
    drop(remote_permits);

    let local_permits = (0..TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT)
        .map(|_| {
            semaphore_state
                .terminal_local_command_semaphore
                .clone()
                .try_acquire_owned()
                .expect("test should reacquire local terminal permit")
        })
        .collect::<Vec<_>>();
    let (remote_status, remote_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": remote_command,
                    "projectId": remote_project_id,
                    "workdir": "/remote/repo",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(remote_status, StatusCode::OK);
    assert_eq!(
        remote_response["stdout"],
        Value::String("remote\n".to_owned())
    );
    let remote_captured: Value = serde_json::from_str(
        remote_captured_body
            .lock()
            .expect("capture mutex poisoned")
            .as_ref()
            .expect("remote request should be captured"),
    )
    .expect("remote request body should decode");
    assert_eq!(
        remote_captured["command"],
        Value::String(remote_command.to_owned())
    );
    drop(local_permits);
    join_test_server(remote_server);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn run_terminal_shell_command_runs_trivial_local_command() {
    let root = std::env::temp_dir().join(format!("termal-terminal-runner-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();

    let response =
        run_terminal_shell_command("echo ok", &root).expect("terminal command should run");

    assert_eq!(response.command, "echo ok");
    assert!(!response.timed_out);
    assert!(response.exit_code.is_some());
    assert!(response.stdout.contains("ok"));
    assert!(!response.output_truncated);
    // The production `run_terminal_shell_command` returns
    // `normalize_user_facing_path(workdir)` on the uncanonicalized input,
    // so don't couple this assertion to `canonicalize`: on Windows CI
    // runners where `%TEMP%` is a junction or symlink, canonicalize
    // resolves the link while the response preserves the raw form. Assert
    // the load-bearing properties directly: the response contains our
    // test-dir tag and is not returned in Windows verbatim-prefix form.
    assert!(
        response.workdir.contains("termal-terminal-runner-"),
        "workdir {:?} should contain the test-dir tag",
        response.workdir
    );
    assert!(
        !response.workdir.starts_with(r"\\?\"),
        "workdir {:?} should not be in Windows verbatim-prefix form",
        response.workdir
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn run_terminal_shell_command_timeout_kills_process_tree() {
    // Margin budget for this test, tuned for Windows CI: the shell has 500ms
    // to reach `Start-Process` before the timeout fires, and the grandchild
    // sleeps for 1500ms before touching the marker, giving us a 1000ms
    // margin for PowerShell startup + JIT + Job assignment + ResumeThread +
    // command parse + process creation + child startup. Do NOT shrink these
    // numbers without validating against a cold Windows CI agent (first
    // PowerShell launch, unjitted .NET, AV first-scan), which is the worst
    // case. The `assert_path_absent_throughout` window (2500ms) then
    // continuously asserts the marker stays absent for the rest of the
    // grandchild's scheduled sleep + a safety margin.
    let root = std::env::temp_dir().join(format!("termal-terminal-timeout-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let marker = root.join("orphan-marker.txt");
    let command = terminal_timeout_process_tree_command(&marker);

    let response =
        run_terminal_shell_command_with_timeout(&command, &root, Duration::from_millis(500))
            .expect("timeout command should return a response");

    assert!(response.timed_out);
    assert!(!response.success);
    assert_path_absent_throughout(
        &marker,
        Duration::from_millis(2_500),
        "grandchild process should not survive terminal timeout",
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn run_terminal_shell_command_cleans_up_background_children_after_shell_exit() {
    let root = std::env::temp_dir().join(format!("termal-terminal-background-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let marker = root.join("background-marker.txt");
    let command = terminal_background_process_tree_command(&marker);

    let response = run_terminal_shell_command_with_timeout(&command, &root, Duration::from_secs(3))
        .expect("background command should return a response");

    assert!(!response.timed_out);
    assert!(response.success);
    assert!(
        response.stdout.contains("done"),
        "expected parent shell output, got {:?}",
        response.stdout
    );

    // Windows: the Job Object terminates every process assigned to it when
    // the shell exits (JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE), so the
    // background grandchild must be gone by the time we reach the marker
    // check. Unix: we deliberately skip `killpg` on the clean-exit path to
    // avoid racing with PID reuse (see `TerminalProcessTree::cleanup_after_shell_exit`),
    // so a backgrounded grandchild is allowed to re-parent to init and
    // finish on its own schedule. Assert the Windows guarantee, and simply
    // accept that the marker may exist on Unix.
    #[cfg(windows)]
    assert_path_absent_throughout(
        &marker,
        Duration::from_millis(2_500),
        "background grandchild process should not survive terminal command completion on Windows",
    );

    // Best-effort cleanup: on Unix the backgrounded subshell may still be
    // holding the temp directory open. Retry a few times so we don't flake
    // when the grandchild is slow to finish writing the marker.
    for attempt in 0..10 {
        match fs::remove_dir_all(&root) {
            Ok(()) => break,
            Err(err) if attempt == 9 => panic!("failed to remove temp dir {root:?}: {err}"),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// Polls `path` every 50ms for the entire `timeout` window, asserting that
/// it remains absent on every tick. This is deliberately a continuous
/// assertion rather than a poll-with-early-exit helper: the terminal
/// process-tree tests need to prove that a backgrounded grandchild could
/// not have created the marker during the window, not merely that the
/// marker was absent at some instant before the deadline. The helper runs
/// for the full `timeout` even on a warm machine where the kill landed in
/// microseconds.
///
/// `timeout` MUST be substantially larger than the poll interval so the
/// window observes multiple ticks — otherwise the test would trivially
/// pass on a broken kill. We assert this up front so that any future
/// shrinking of the window (or growing of the internal sleep) fails fast
/// with a clear message instead of silently weakening the test.
fn assert_path_absent_throughout(path: &FsPath, timeout: Duration, message: &str) {
    const POLL_INTERVAL: Duration = Duration::from_millis(50);
    const MIN_POLLS: u32 = 4;
    assert!(
        timeout >= POLL_INTERVAL.saturating_mul(MIN_POLLS),
        "assert_path_absent_throughout timeout {timeout:?} is too small for \
         {MIN_POLLS} polls at {POLL_INTERVAL:?}; widen the window or shorten \
         the poll interval"
    );
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        assert!(!path.exists(), "{message}");
        std::thread::sleep(POLL_INTERVAL);
    }
    assert!(!path.exists(), "{message}");
}

#[cfg(windows)]
fn terminal_timeout_process_tree_command(marker: &FsPath) -> String {
    let marker = marker.to_string_lossy().replace('\'', "''");
    format!(
        "Start-Process -FilePath powershell.exe -ArgumentList @('-NoLogo','-NoProfile','-Command','Start-Sleep -Milliseconds 1500; Set-Content -LiteralPath ''{marker}'' done') -WindowStyle Hidden; Start-Sleep -Seconds 5"
    )
}

#[cfg(not(windows))]
fn terminal_timeout_process_tree_command(marker: &FsPath) -> String {
    format!(
        "(sleep 1.5; touch {}) & sleep 5",
        shell_single_quote(marker.to_string_lossy().as_ref())
    )
}

#[cfg(windows)]
fn terminal_background_process_tree_command(marker: &FsPath) -> String {
    let marker = marker.to_string_lossy().replace('\'', "''");
    format!(
        "Start-Process -FilePath powershell.exe -ArgumentList @('-NoLogo','-NoProfile','-Command','Start-Sleep -Milliseconds 1500; Set-Content -LiteralPath ''{marker}'' done') -WindowStyle Hidden; Write-Output done"
    )
}

#[cfg(not(windows))]
fn terminal_background_process_tree_command(marker: &FsPath) -> String {
    format!(
        "(sleep 1.5; touch {}) & echo done",
        shell_single_quote(marker.to_string_lossy().as_ref())
    )
}

#[cfg(not(windows))]
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

// Tests that read and write file accept project ID without session.
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
    assert_eq!(
        read_response.content_hash.as_deref(),
        Some(file_content_hash(read_response.content.as_bytes()).as_str())
    );
    assert_eq!(
        read_response.size_bytes,
        Some(read_response.content.len() as u64)
    );

    let Json(write_response) = write_file(
        State(state),
        Json(WriteFileRequest {
            path: new_file.to_string_lossy().into_owned(),
            content: "pub fn generated() {}
"
            .to_owned(),
            base_hash: None,
            overwrite: false,
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
    assert_eq!(
        write_response.content_hash.as_deref(),
        Some(file_content_hash(write_response.content.as_bytes()).as_str())
    );

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn write_file_rejects_missing_path_traversal_outside_project() {
    let state = test_app_state();
    let root =
        std::env::temp_dir().join(format!("termal-project-file-traversal-{}", Uuid::new_v4()));
    let outside_root = std::env::temp_dir().join(format!(
        "termal-project-file-traversal-outside-{}",
        Uuid::new_v4()
    ));
    let outside_file = outside_root.join("escape.rs");
    let traversal_file = root
        .join("missing")
        .join("..")
        .join("..")
        .join(
            outside_root
                .file_name()
                .expect("outside root should have a name"),
        )
        .join("escape.rs");

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
            path: traversal_file.to_string_lossy().into_owned(),
            content: "pub fn escape() {}\n".to_owned(),
            base_hash: None,
            overwrite: false,
            project_id: Some(project.project_id),
            session_id: None,
        }),
    )
    .await
    {
        Ok(_) => panic!("traversal write should fail"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(
        error
            .message
            .contains("cannot contain unresolved `.` or `..`")
            || error.message.contains("must stay inside project")
    );
    assert!(!outside_file.exists());
    fs::remove_dir_all(root).unwrap();
    if outside_root.exists() {
        fs::remove_dir_all(outside_root).unwrap();
    }
}

// Tests that write file rejects stale editor base hashes.
#[tokio::test]
async fn write_file_rejects_stale_base_hash() {
    let state = test_app_state();
    let root =
        std::env::temp_dir().join(format!("termal-project-file-stale-base-{}", Uuid::new_v4()));
    let existing_file = root.join("src").join("main.rs");

    fs::create_dir_all(existing_file.parent().unwrap()).unwrap();
    fs::write(&existing_file, "fn main() {}\n").unwrap();

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
    let base_hash = read_response
        .content_hash
        .expect("read response should include a content hash");

    fs::write(&existing_file, "fn main() { println!(\"agent\"); }\n").unwrap();

    let error = match write_file(
        State(state.clone()),
        Json(WriteFileRequest {
            path: existing_file.to_string_lossy().into_owned(),
            content: "fn main() { println!(\"user\"); }\n".to_owned(),
            base_hash: Some(base_hash),
            overwrite: false,
            project_id: Some(project.project_id.clone()),
            session_id: None,
        }),
    )
    .await
    {
        Ok(_) => panic!("stale file write should fail"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("file changed on disk before save"));
    assert_eq!(
        fs::read_to_string(&existing_file).unwrap(),
        "fn main() { println!(\"agent\"); }\n"
    );

    let Json(overwrite_response) = write_file(
        State(state),
        Json(WriteFileRequest {
            path: existing_file.to_string_lossy().into_owned(),
            content: "fn main() { println!(\"user\"); }\n".to_owned(),
            base_hash: Some("sha256:stale".to_owned()),
            overwrite: true,
            project_id: Some(project.project_id),
            session_id: None,
        }),
    )
    .await
    .unwrap();

    assert_eq!(
        overwrite_response.content,
        "fn main() { println!(\"user\"); }\n"
    );
    assert_eq!(
        fs::read_to_string(&existing_file).unwrap(),
        "fn main() { println!(\"user\"); }\n"
    );

    fs::remove_dir_all(root).unwrap();
}

// Tests that watcher changes are summarized for the active local agent turn.
#[test]
fn active_turn_file_changes_are_summarized_on_record() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!(
        "termal-active-turn-file-changes-{}",
        Uuid::new_v4()
    ));
    let changed_file = root.join("src").join("main.rs");
    let ignored_file = std::env::temp_dir().join(format!(
        "termal-active-turn-file-changes-outside-{}.rs",
        Uuid::new_v4()
    ));

    fs::create_dir_all(changed_file.parent().unwrap()).unwrap();
    fs::write(&changed_file, "fn main() {}\n").unwrap();
    fs::write(&ignored_file, "pub fn outside() {}\n").unwrap();

    let session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Files".to_owned()),
            root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = record.session.id.clone();
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].active_turn_start_message_count =
            Some(inner.sessions[index].session.messages.len());
        session_id
    };

    state.record_active_turn_file_changes(&[
        WorkspaceFileChangeEvent {
            path: changed_file.to_string_lossy().into_owned(),
            kind: WorkspaceFileChangeKind::Modified,
            root_path: None,
            session_id: None,
            mtime_ms: None,
            size_bytes: None,
        },
        WorkspaceFileChangeEvent {
            path: ignored_file.to_string_lossy().into_owned(),
            kind: WorkspaceFileChangeKind::Modified,
            root_path: None,
            session_id: None,
            mtime_ms: None,
            size_bytes: None,
        },
    ]);

    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(&session_id).unwrap();
    assert_eq!(
        inner.sessions[index].active_turn_file_changes.len(),
        1,
        "only files under the session workdir should be tracked",
    );

    let message_id = inner.next_message_id();
    assert!(push_active_turn_file_changes_on_record(
        &mut inner.sessions[index],
        message_id,
    ));
    assert!(inner.sessions[index].active_turn_file_changes.is_empty());
    match inner.sessions[index].session.messages.last() {
        Some(Message::FileChanges { title, files, .. }) => {
            assert_eq!(title, "Agent changed 1 file");
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].path, changed_file.to_string_lossy());
            assert_eq!(files[0].kind, WorkspaceFileChangeKind::Modified);
        }
        other => panic!("expected file changes message, got {other:?}"),
    }

    drop(inner);
    fs::remove_dir_all(root).unwrap();
    fs::remove_file(ignored_file).unwrap();
}

#[test]
fn active_turn_file_changes_prefer_session_scoped_watcher_hints() {
    let state = test_app_state();
    let root =
        std::env::temp_dir().join(format!("termal-active-turn-file-scope-{}", Uuid::new_v4()));
    let changed_file = root.join("src").join("main.rs");
    fs::create_dir_all(changed_file.parent().unwrap()).unwrap();
    fs::write(&changed_file, "fn main() {}\n").unwrap();

    let (first_session_id, second_session_id) = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let first = inner.create_session(
            Agent::Codex,
            Some("First".to_owned()),
            root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let first_session_id = first.session.id.clone();
        let second = inner.create_session(
            Agent::Codex,
            Some("Second".to_owned()),
            root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let second_session_id = second.session.id.clone();
        for session_id in [&first_session_id, &second_session_id] {
            let index = inner.find_session_index(session_id).unwrap();
            inner.sessions[index].active_turn_start_message_count =
                Some(inner.sessions[index].session.messages.len());
        }
        (first_session_id, second_session_id)
    };

    state.record_active_turn_file_changes(&[
        WorkspaceFileChangeEvent {
            path: changed_file.to_string_lossy().into_owned(),
            kind: WorkspaceFileChangeKind::Modified,
            root_path: Some(root.to_string_lossy().into_owned()),
            session_id: None,
            mtime_ms: None,
            size_bytes: None,
        },
        WorkspaceFileChangeEvent {
            path: changed_file.to_string_lossy().into_owned(),
            kind: WorkspaceFileChangeKind::Modified,
            root_path: Some(root.to_string_lossy().into_owned()),
            session_id: Some(first_session_id.clone()),
            mtime_ms: None,
            size_bytes: None,
        },
    ]);

    let inner = state.inner.lock().expect("state mutex poisoned");
    let first = inner
        .sessions
        .iter()
        .find(|record| record.session.id == first_session_id)
        .expect("first session should exist");
    let second = inner
        .sessions
        .iter()
        .find(|record| record.session.id == second_session_id)
        .expect("second session should exist");
    assert_eq!(first.active_turn_file_changes.len(), 1);
    assert!(second.active_turn_file_changes.is_empty());
    drop(inner);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn late_turn_file_changes_are_summarized_during_grace_window() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-late-file-change-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let changed_file = root.join("generated.rs");
    fs::write(&changed_file, "fn generated() {}\n").unwrap();

    let session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Files".to_owned()),
            root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = record.session.id.clone();
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].active_turn_start_message_count =
            Some(inner.sessions[index].session.messages.len());
        finish_active_turn_file_change_tracking(&mut inner.sessions[index]);
        session_id
    };

    state.record_active_turn_file_changes(&[WorkspaceFileChangeEvent {
        path: changed_file.to_string_lossy().into_owned(),
        kind: WorkspaceFileChangeKind::Created,
        root_path: Some(root.to_string_lossy().into_owned()),
        session_id: Some(session_id.clone()),
        mtime_ms: None,
        size_bytes: None,
    }]);

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should exist");
    let files = session
        .messages
        .iter()
        .find_map(|message| match message {
            Message::FileChanges { files, .. } => Some(files),
            _ => None,
        })
        .expect("late watcher event should create a file-change summary");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].kind, WorkspaceFileChangeKind::Created);
    assert_eq!(files[0].path, changed_file.to_string_lossy());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn expired_late_turn_file_change_grace_window_does_not_emit_summary() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!(
        "termal-expired-late-file-change-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root).unwrap();
    let changed_file = root.join("generated.rs");
    fs::write(&changed_file, "fn generated() {}\n").unwrap();

    let session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Expired Files".to_owned()),
            root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = record.session.id.clone();
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].active_turn_file_change_grace_deadline =
            Some(std::time::Instant::now() - Duration::from_millis(1));
        session_id
    };

    state.record_active_turn_file_changes(&[WorkspaceFileChangeEvent {
        path: changed_file.to_string_lossy().into_owned(),
        kind: WorkspaceFileChangeKind::Created,
        root_path: Some(root.to_string_lossy().into_owned()),
        session_id: Some(session_id.clone()),
        mtime_ms: None,
        size_bytes: None,
    }]);

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("session should exist");
    assert!(record.active_turn_file_change_grace_deadline.is_none());
    assert!(record.active_turn_file_changes.is_empty());
    assert!(
        record
            .session
            .messages
            .iter()
            .all(|message| !matches!(message, Message::FileChanges { .. }))
    );
    drop(inner);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn idle_finish_active_turn_file_change_tracking_does_not_open_grace_window() {
    let mut inner = StateInner::new();
    let mut record = inner.create_session(
        Agent::Codex,
        Some("Idle Files".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    record.active_turn_file_changes.insert(
        "/tmp/generated.rs".to_owned(),
        WorkspaceFileChangeKind::Created,
    );

    finish_active_turn_file_change_tracking(&mut record);

    assert!(record.active_turn_start_message_count.is_none());
    assert!(record.active_turn_file_change_grace_deadline.is_none());
    assert!(record.active_turn_file_changes.is_empty());
}

#[test]
fn late_turn_file_change_grace_window_emits_only_once() {
    let state = test_app_state();
    let root =
        std::env::temp_dir().join(format!("termal-late-file-change-once-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let first_file = root.join("first.rs");
    let second_file = root.join("second.rs");
    fs::write(&first_file, "fn first() {}\n").unwrap();
    fs::write(&second_file, "fn second() {}\n").unwrap();

    let session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Files".to_owned()),
            root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = record.session.id.clone();
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].active_turn_start_message_count =
            Some(inner.sessions[index].session.messages.len());
        finish_active_turn_file_change_tracking(&mut inner.sessions[index]);
        session_id
    };

    state.record_active_turn_file_changes(&[WorkspaceFileChangeEvent {
        path: first_file.to_string_lossy().into_owned(),
        kind: WorkspaceFileChangeKind::Created,
        root_path: Some(root.to_string_lossy().into_owned()),
        session_id: Some(session_id.clone()),
        mtime_ms: None,
        size_bytes: None,
    }]);
    state.record_active_turn_file_changes(&[WorkspaceFileChangeEvent {
        path: second_file.to_string_lossy().into_owned(),
        kind: WorkspaceFileChangeKind::Created,
        root_path: Some(root.to_string_lossy().into_owned()),
        session_id: Some(session_id.clone()),
        mtime_ms: None,
        size_bytes: None,
    }]);

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should exist");
    let file_change_messages = session
        .messages
        .iter()
        .filter(|message| matches!(message, Message::FileChanges { .. }))
        .count();
    assert_eq!(file_change_messages, 1);
    fs::remove_dir_all(root).unwrap();
}

// Tests that read file returns not found for missing project file.
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

// Tests that read directory returns not found for missing project path.
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


// Tests that read file rejects content over size limit.
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

// Tests that write file rejects content over size limit.
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
            base_hash: None,
            overwrite: false,
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

// Tests that project digest surfaces pending approval actions.
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

// Tests that project digest prefers review actions for dirty idle project.
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

// Tests that project action approve routes to the live project approval.
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
            assert_eq!(
                response.payload,
                CodexJsonRpcResponsePayload::Result(json!({ "decision": "accept" }))
            );
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

// Tests that project action keep iterating dispatches a follow up prompt.
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

// Tests that Telegram command parser supports suffixes and aliases.
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

// Tests that Telegram command parser rejects unknown slash commands.
#[test]
fn telegram_command_parser_rejects_unknown_slash_commands() {
    assert!(parse_telegram_command("/unknown").is_none());
}

// Tests that Telegram digest renderer includes actions and public link.
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

// Tests that create orchestrator instance route uses template project when request project ID is empty.
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

// Tests that orchestrator lifecycle routes update state and stop active sessions.
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
            template: None,
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
// Tests that orchestrator stop route preserves running state when a child stop fails.
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
            template: None,
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

// Tests that aborted stop cleanup preserves child work when child stop persist fails.
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
            template: None,
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
    // Force synchronous persistence so the error propagates instead of
    // being swallowed by the background persist thread.
    state.persist_tx = mpsc::channel().0;

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

// Tests that aborted stop resume does not redispatch child after child stop persist fails.
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
            template: None,
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
    // Force synchronous persistence so the error propagates instead of
    // being swallowed by the background persist thread.
    state.persist_tx = mpsc::channel().0;

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

// Tests that aborted stop restart does not redispatch child after child stop persist fails.
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
            template: None,
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
    // Force synchronous persistence so the error propagates instead of
    // being swallowed by the background persist thread.
    state.persist_tx = mpsc::channel().0;

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

// Tests that aborted stop restart does not dispatch orphaned child queue after child stop persist fails.
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
            template: None,
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
    // Force synchronous persistence so the error propagates instead of
    // being swallowed by the background persist thread.
    state.persist_tx = mpsc::channel().0;

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

// Tests that blocked session manual recovery dispatch prioritizes user prompt after restart.
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
            template: None,
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
    state.persist_tx = mpsc::channel().0;

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

// Tests that blocked session manual recovery preserves user prompt fifo after plain stop persist failure.
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
    state.persist_tx = mpsc::channel().0;
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

// Tests that blocked session manual recovery prioritizes existing user queue ahead of stale orchestrator work.
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
    state.persist_tx = mpsc::channel().0;
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

// Tests that aborted stop does not relaunch child work completed during stop.
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
            template: None,
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

// Tests that begin orchestrator stop cleans up guards on missing and stopped errors.
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
            template: None,
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

// Tests that begin orchestrator stop rolls back stop in progress after persist failure.
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
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    state.persist_tx = mpsc::channel().0;

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

// Tests that load state preserves pending transitions when stop in progress has no stopped children.
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

    let mut state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    // Force synchronous persistence so file reads in this test see the
    // written data immediately.
    state.persist_tx = mpsc::channel().0;
    let project_id = create_test_project(&state, &project_root, "Restart Recovery Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
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

// Tests that load state recovers completed stop when active children finished during stop.
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

    let mut state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    // Force synchronous persistence so file reads in this test see the
    // written data immediately.
    state.persist_tx = mpsc::channel().0;
    let project_id = create_test_project(&state, &project_root, "Restart Recovery Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
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

// Tests that load state prunes only stopped child work when recovering stop in progress.
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

    let mut state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    state.persist_tx = mpsc::channel().0;
    let project_id = create_test_project(&state, &project_root, "Recovery Queue Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
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

// Tests that load state recovers completed stop when all active children were stopped.
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

    let mut state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    state.persist_tx = mpsc::channel().0;
    let project_id = create_test_project(&state, &project_root, "Completed Recovery Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
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

// Tests that orchestrator templates round-trip through draft conversion helpers.
#[test]
fn orchestrator_template_draft_round_trips_through_template_helpers() {
    let draft = sample_orchestrator_template_draft();
    let template = orchestrator_template_from_draft("template-round-trip", draft.clone())
        .expect("sample draft should normalize into a template");
    let round_tripped = orchestrator_template_to_draft(&template);

    assert_eq!(round_tripped, draft);
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
                prompt_template: Some(
                    "Use this plan and implement it:

{{result}}"
                        .to_owned(),
                ),
            },
            OrchestratorTemplateTransition {
                id: "builder-to-reviewer".to_owned(),
                from_session_id: "builder".to_owned(),
                to_session_id: "reviewer".to_owned(),
                from_anchor: Some("right".to_owned()),
                to_anchor: Some("left".to_owned()),
                trigger: OrchestratorTransitionTrigger::OnCompletion,
                result_mode: OrchestratorTransitionResultMode::SummaryAndLastResponse,
                prompt_template: Some(
                    "Review this implementation:

{{result}}"
                        .to_owned(),
                ),
            },
        ],
    }
}

fn sample_deadlocked_orchestrator_template_draft() -> OrchestratorTemplateDraft {
    OrchestratorTemplateDraft {
        name: "Consolidate Deadlock Flow".to_owned(),
        description: "Exercise remote deadlock skipping.".to_owned(),
        project_id: None,
        sessions: vec![
            OrchestratorSessionTemplate {
                id: "source-a".to_owned(),
                name: "Source A".to_owned(),
                agent: Agent::Claude,
                model: Some("claude-sonnet-4-5".to_owned()),
                instructions: "Provide the first source input.".to_owned(),
                auto_approve: false,
                input_mode: OrchestratorSessionInputMode::Queue,
                position: OrchestratorNodePosition { x: 120.0, y: 160.0 },
            },
            OrchestratorSessionTemplate {
                id: "source-b".to_owned(),
                name: "Source B".to_owned(),
                agent: Agent::Claude,
                model: Some("claude-sonnet-4-5".to_owned()),
                instructions: "Provide the second source input.".to_owned(),
                auto_approve: false,
                input_mode: OrchestratorSessionInputMode::Queue,
                position: OrchestratorNodePosition { x: 120.0, y: 460.0 },
            },
            OrchestratorSessionTemplate {
                id: "consolidate-a".to_owned(),
                name: "Consolidate A".to_owned(),
                agent: Agent::Claude,
                model: Some("claude-sonnet-4-5".to_owned()),
                instructions: "Wait on source A and consolidate B.".to_owned(),
                auto_approve: false,
                input_mode: OrchestratorSessionInputMode::Consolidate,
                position: OrchestratorNodePosition { x: 760.0, y: 160.0 },
            },
            OrchestratorSessionTemplate {
                id: "consolidate-b".to_owned(),
                name: "Consolidate B".to_owned(),
                agent: Agent::Claude,
                model: Some("claude-sonnet-4-5".to_owned()),
                instructions: "Wait on source B and consolidate A.".to_owned(),
                auto_approve: false,
                input_mode: OrchestratorSessionInputMode::Consolidate,
                position: OrchestratorNodePosition { x: 760.0, y: 460.0 },
            },
        ],
        transitions: vec![
            OrchestratorTemplateTransition {
                id: "source-a-to-consolidate-a".to_owned(),
                from_session_id: "source-a".to_owned(),
                to_session_id: "consolidate-a".to_owned(),
                from_anchor: Some("right".to_owned()),
                to_anchor: Some("left".to_owned()),
                trigger: OrchestratorTransitionTrigger::OnCompletion,
                result_mode: OrchestratorTransitionResultMode::Summary,
                prompt_template: Some(
                    "Source A summary:

{{result}}"
                        .to_owned(),
                ),
            },
            OrchestratorTemplateTransition {
                id: "consolidate-b-to-consolidate-a".to_owned(),
                from_session_id: "consolidate-b".to_owned(),
                to_session_id: "consolidate-a".to_owned(),
                from_anchor: Some("top".to_owned()),
                to_anchor: Some("bottom".to_owned()),
                trigger: OrchestratorTransitionTrigger::OnCompletion,
                result_mode: OrchestratorTransitionResultMode::Summary,
                prompt_template: Some(
                    "Consolidate B summary:

{{result}}"
                        .to_owned(),
                ),
            },
            OrchestratorTemplateTransition {
                id: "source-b-to-consolidate-b".to_owned(),
                from_session_id: "source-b".to_owned(),
                to_session_id: "consolidate-b".to_owned(),
                from_anchor: Some("right".to_owned()),
                to_anchor: Some("left".to_owned()),
                trigger: OrchestratorTransitionTrigger::OnCompletion,
                result_mode: OrchestratorTransitionResultMode::Summary,
                prompt_template: Some(
                    "Source B summary:

{{result}}"
                        .to_owned(),
                ),
            },
            OrchestratorTemplateTransition {
                id: "consolidate-a-to-consolidate-b".to_owned(),
                from_session_id: "consolidate-a".to_owned(),
                to_session_id: "consolidate-b".to_owned(),
                from_anchor: Some("bottom".to_owned()),
                to_anchor: Some("top".to_owned()),
                trigger: OrchestratorTransitionTrigger::OnCompletion,
                result_mode: OrchestratorTransitionResultMode::Summary,
                prompt_template: Some(
                    "Consolidate A summary:

{{result}}"
                        .to_owned(),
                ),
            },
        ],
    }
}

// Tests that start_turn_on_record rejects remote proxy sessions directly.
#[test]
fn start_turn_on_record_rejects_remote_proxy_sessions() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    inner.sessions[index].remote_id = Some("ssh-lab".to_owned());
    inner.sessions[index].remote_session_id = Some("remote-session-1".to_owned());

    let error = match state.start_turn_on_record(
        &mut inner.sessions[index],
        "message-remote-proxy".to_owned(),
        "Dispatch through the remote backend.".to_owned(),
        Vec::new(),
        None,
    ) {
        Ok(_) => panic!("remote proxy sessions should reject local turn dispatch"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        error.message,
        "remote proxy sessions must dispatch through the remote backend"
    );
    assert!(
        inner.sessions[index]
            .active_turn_start_message_count
            .is_none()
    );
    assert!(inner.sessions[index].session.messages.is_empty());
    assert!(inner.sessions[index].session.pending_prompts.is_empty());

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that failed orchestrator transition dispatch becomes a visible destination error.
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
            template: None,
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

// Tests that failed orchestrator transition dispatch does not block other instances.
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
            template: None,
        })
        .expect("first orchestrator instance should be created")
        .orchestrator;
    let orchestrator_b = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id_b),
            template: None,
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

// Tests that stop session does not schedule orchestrator transitions.
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
            template: None,
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

// Tests that fail turn does not schedule orchestrator transitions.
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
            template: None,
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

// Tests that mark turn error does not schedule orchestrator transitions.
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
            template: None,
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

// Tests that orchestrator transition uses only messages from the current turn.
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
            template: None,
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

// Tests that runtime exit does not schedule orchestrator transitions.
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
            template: None,
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

// Tests that killing a session prunes its orchestrator links.
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
            template: None,
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
