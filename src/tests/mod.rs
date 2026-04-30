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
mod codex_protocol;
mod codex_threads;
mod cursor;
mod file_changes;
mod git;
mod http_routes;
mod instruction_search;
mod json_rpc;
mod orchestrator;
pub use orchestrator::{
    sample_deadlocked_orchestrator_template_draft, sample_orchestrator_template_draft,
};
mod persist;
mod project_digest;
mod projects;
mod remote;
mod review;
mod runtime_rpc;
mod session_lifecycle;
mod session_settings;
mod session_stop;
mod session_stop_runtime;
mod shared_codex;
mod telegram;
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
        server_instance_id: Uuid::new_v4().to_string(),
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
        // Tests skip the real persist worker; the test constructor owns no
        // background thread to join, so the handle stays `None` and
        // `shutdown_persist_blocking` is a no-op.
        persist_thread_handle: Arc::new(Mutex::new(None)),
        // Test constructors don't spawn the persist worker — keep
        // `alive=true` so the production-shaped fallback (when
        // `persist_tx.send` succeeds, async path; otherwise sync) drives
        // tests just like before. Production tests that exercise the
        // shutdown path explicitly flip this to `false`.
        persist_worker_alive: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        shutdown_signal_tx: Arc::new(tokio::sync::watch::channel(false).0),
        state_broadcast_tx: mpsc::channel().0,
        shared_codex_runtime: Arc::new(Mutex::new(None)),
        agent_readiness_cache: Arc::new(RwLock::new(fresh_agent_readiness_cache("/tmp"))),
        agent_readiness_refresh_lock: Arc::new(Mutex::new(())),
        remote_registry: test_remote_registry(),
        remote_sse_fallback_resynced_revision: Arc::new(Mutex::new(HashMap::new())),
        remote_delta_replay_cache: Arc::new(Mutex::new(RemoteDeltaReplayCache::default())),
        remote_delta_hydrations_in_flight: Arc::new(Mutex::new(HashSet::new())),
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

    /// Unsets `key` for the scope of the returned guard, saving any
    /// current value so it is restored on drop. Used by tests that assert
    /// "function X returns None when this env var is not set" to isolate
    /// from sibling tests that may have set the same var in the process
    /// env (or from the developer's own shell env). Must be called while
    /// holding `TEST_HOME_ENV_MUTEX` when the var affects any path read
    /// via `HOME` / `USERPROFILE` — otherwise the remove races against
    /// other env-mutating tests.
    fn remove(key: &'static str) -> Self {
        let original = std::env::var_os(key);
        unsafe {
            std::env::remove_var(key);
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
                messages_loaded: true,
                message_count: 0,
                pending_prompts: Vec::new(),
                session_mutation_stamp: None,
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
        // Tests simulate a remote snapshot; a stable stub id is fine
        // since remote-state sync doesn't consume the field today.
        server_instance_id: "remote-test-instance".to_owned(),
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
        capabilities: Some(AcpCapabilities {
            supports_session_load: Some(true),
        }),
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

    let codex = state.full_snapshot().codex;
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

// Tests that Codex global notices are capped in most-recent-first order.
#[test]
fn codex_notice_cap_retains_most_recent_notices() {
    let state = test_app_state();

    for index in 0..7 {
        state
            .note_codex_notice(CodexNotice {
                kind: CodexNoticeKind::RuntimeNotice,
                level: CodexNoticeLevel::Info,
                title: format!("Runtime notice {index}"),
                detail: format!("Runtime detail {index}"),
                timestamp: format!("2026-04-26T00:00:0{index}Z"),
                code: Some(format!("runtime-notice-{index}")),
            })
            .expect("notice should be recorded");
    }

    let notices = state.full_snapshot().codex.notices;
    assert_eq!(CODEX_NOTICE_CAP, 5);
    assert_eq!(notices.len(), CODEX_NOTICE_CAP);
    assert_eq!(
        notices
            .iter()
            .map(|notice| notice.title.as_str())
            .collect::<Vec<_>>(),
        vec![
            "Runtime notice 6",
            "Runtime notice 5",
            "Runtime notice 4",
            "Runtime notice 3",
            "Runtime notice 2",
        ]
    );

    state
        .note_codex_notice(CodexNotice {
            kind: CodexNoticeKind::RuntimeNotice,
            level: CodexNoticeLevel::Info,
            title: "Runtime notice 4".to_string(),
            detail: "Runtime detail 4".to_string(),
            timestamp: "2026-04-26T00:00:09Z".to_string(),
            code: Some("runtime-notice-4".to_string()),
        })
        .expect("duplicate notice should be promoted");

    let notices = state.full_snapshot().codex.notices;
    assert_eq!(notices.len(), CODEX_NOTICE_CAP);
    assert_eq!(
        notices
            .iter()
            .map(|notice| notice.title.as_str())
            .collect::<Vec<_>>(),
        vec![
            "Runtime notice 4",
            "Runtime notice 6",
            "Runtime notice 5",
            "Runtime notice 3",
            "Runtime notice 2",
        ]
    );
    assert_eq!(notices[0].timestamp, "2026-04-26T00:00:09Z");
}

// Tests that shared Codex rate-limit updates use a narrow delta rather
// than publishing a full state snapshot with every transcript attached.
#[test]
fn shared_codex_rate_limits_publish_codex_delta_without_full_state_snapshot() {
    let state = test_app_state();
    let mut state_events = state.subscribe_events();
    let mut delta_events = state.subscribe_delta_events();
    let (runtime, _input_rx, _process) = test_shared_codex_runtime("shared-codex-rate-limits");
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));

    let rate_limit_update = json!({
        "method": "account/rateLimits/updated",
        "params": {
            "rateLimits": {
                "planType": "pro",
                "primary": {
                    "resetsAt": 12345,
                    "usedPercent": 42,
                    "windowDurationMins": 300
                }
            }
        }
    });

    handle_shared_codex_app_server_message(
        &rate_limit_update,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    assert!(matches!(
        state_events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    let payload = delta_events
        .try_recv()
        .expect("Codex rate-limit update should publish a delta");
    let delta: DeltaEvent = serde_json::from_str(&payload).expect("delta should decode");
    match delta {
        DeltaEvent::CodexUpdated { revision, codex } => {
            assert_eq!(revision, state.full_snapshot().revision);
            assert_eq!(
                codex
                    .rate_limits
                    .as_ref()
                    .and_then(|rate_limits| rate_limits.plan_type.as_deref()),
                Some("pro")
            );
            assert_eq!(
                codex
                    .rate_limits
                    .as_ref()
                    .and_then(|rate_limits| rate_limits.primary.as_ref())
                    .and_then(|primary| primary.used_percent),
                Some(42)
            );
        }
        _ => panic!("expected CodexUpdated delta"),
    }
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

    let codex = state.full_snapshot().codex;
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

    let snapshot = state.full_snapshot();
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

    let snapshot = state.full_snapshot();
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

    let baseline = state.full_snapshot().revision;
    state.clear_runtime(&session_id).unwrap();

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).unwrap();
        let record = &inner.sessions[index];
        assert!(matches!(record.runtime, SessionRuntime::None));
        assert!(!record.runtime_reset_required);
    }
    assert_eq!(state.full_snapshot().revision, baseline + 1);

    let stable_revision = state.full_snapshot().revision;
    state.clear_runtime(&session_id).unwrap();
    assert_eq!(state.full_snapshot().revision, stable_revision);
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

    // Out-of-bounds misses must NOT advance `last_mutation_stamp`.
    // Advancing the counter without a matching record would break the
    // invariant "stamp implies an actual mutation" — the global
    // watermark gap would grow by one per miss and the persist thread's
    // watermark math would still work by luck alone. Pin the reorder
    // fix that guards against the leak.
    let counter_before_miss = inner.last_mutation_stamp;
    let oob_index = inner.sessions.len() + 100;

    assert!(inner.session_mut_by_index(oob_index).is_none());
    assert_eq!(
        inner.last_mutation_stamp, counter_before_miss,
        "session_mut_by_index miss must not burn a mutation stamp"
    );

    assert!(inner.stamp_session_at_index(oob_index).is_none());
    assert_eq!(
        inner.last_mutation_stamp, counter_before_miss,
        "stamp_session_at_index miss must not burn a mutation stamp"
    );

    assert!(inner.session_mut("nonexistent-session").is_none());
    assert_eq!(
        inner.last_mutation_stamp, counter_before_miss,
        "session_mut miss must not burn a mutation stamp (by-id variant \
         has always short-circuited via find_session_index)"
    );

    // Valid mutations must still stamp the record after all three misses.
    let post_miss_stamped = inner
        .stamp_session_at_index(index)
        .expect("post-miss stamp_session_at_index should still succeed");
    assert_eq!(post_miss_stamped, counter_before_miss + 1);
    assert_eq!(inner.sessions[index].mutation_stamp, post_miss_stamped);
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

// Tests that health route reports inline orchestrator template compatibility
// and emits a non-empty `serverInstanceId` that clients use for
// restart-detection (pairs with the new state-revision server-instance
// mismatch branch).
#[tokio::test]
async fn health_route_reports_inline_orchestrator_template_support() {
    let state = test_app_state();
    let expected_server_instance_id = state.server_instance_id.clone();
    let (status, response): (StatusCode, Value) = request_json(
        &app_router(state),
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
            "serverInstanceId": expected_server_instance_id,
        })
    );
    // Defensive: the instance id must not be empty, otherwise the
    // client's restart-detection treats it as "unknown" and the
    // whole mechanism becomes a no-op.
    let server_instance_id = response
        .get("serverInstanceId")
        .and_then(Value::as_str)
        .expect("serverInstanceId should be a string");
    assert!(
        !server_instance_id.is_empty(),
        "serverInstanceId must be non-empty so the client can use it \
         for restart detection"
    );
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
