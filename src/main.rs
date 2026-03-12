use std::collections::{HashMap, HashSet, VecDeque};
use std::convert::Infallible;
use std::fs;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
use std::net::SocketAddr;
use std::path::{Path as FsPath, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tower_http::services::{ServeDir, ServeFile};
use uuid::Uuid;

const MAX_IMAGE_ATTACHMENT_BYTES: usize = 5 * 1024 * 1024;

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("fatal: {err:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match Mode::parse(args)? {
        Mode::Server => run_server().await,
        Mode::Repl { agent } => run_repl(agent),
    }
}

async fn run_server() -> Result<()> {
    let cwd_path = std::env::current_dir().context("failed to resolve current directory")?;
    let ui_dist_dir = cwd_path.join("ui").join("dist");
    let ui_index_file = ui_dist_dir.join("index.html");
    let cwd = cwd_path
        .to_str()
        .context("current directory is not valid UTF-8")?
        .to_owned();

    let port = std::env::var("TERMAL_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(8787);
    let address = SocketAddr::from(([127, 0, 0, 1], port));

    let state = AppState::new(cwd.clone())?;
    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/file", get(read_file).put(write_file))
        .route("/api/fs", get(read_directory))
        .route("/api/git/status", get(read_git_status))
        .route("/api/state", get(get_state))
        .route("/api/events", get(state_events))
        .route("/api/sessions", post(create_session))
        .route("/api/sessions/{id}/settings", post(update_session_settings))
        .route("/api/sessions/{id}/messages", post(send_message))
        .route(
            "/api/sessions/{id}/queued-prompts/{prompt_id}/cancel",
            post(cancel_queued_prompt),
        )
        .route("/api/sessions/{id}/stop", post(stop_session))
        .route("/api/sessions/{id}/kill", post(kill_session))
        .route(
            "/api/sessions/{id}/approvals/{message_id}",
            post(submit_approval),
        )
        .with_state(state)
        .fallback_service(
            ServeDir::new(ui_dist_dir).not_found_service(ServeFile::new(ui_index_file)),
        );

    let listener = TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind backend to {address}"))?;
    let bound = listener
        .local_addr()
        .context("failed to read local backend address")?;

    println!("TermAl backend");
    println!("listening: http://{bound}");
    println!("default cwd: {cwd}");
    println!("ui proxy target: /api");

    axum::serve(listener, app)
        .await
        .context("backend server failed")
}

fn run_repl(agent: Agent) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to resolve current directory")?;
    let cwd = cwd
        .to_str()
        .context("current directory is not valid UTF-8")?
        .to_owned();

    println!("TermAl {} prototype", agent.name());
    println!("cwd: {cwd}");
    println!("type a prompt and press enter");
    println!("type '/new' to start a fresh session");
    println!("type 'exit' or 'quit' to stop");
    io::stdout().flush().context("failed to flush stdout")?;

    let stdin = io::stdin();
    let mut line = String::new();
    let mut external_session_id: Option<String> = None;

    loop {
        print!("you> ");
        io::stdout().flush().context("failed to flush stdout")?;

        line.clear();
        let bytes_read = stdin
            .read_line(&mut line)
            .context("failed to read prompt from stdin")?;

        if bytes_read == 0 {
            println!();
            break;
        }

        let prompt = line.trim();
        if prompt.is_empty() {
            continue;
        }

        if matches!(prompt, "exit" | "quit") {
            break;
        }

        if prompt == "/new" {
            external_session_id = None;
            println!("session cleared");
            continue;
        }

        let mut printer = ReplPrinter::default();
        let next_session_id = run_turn_blocking(
            TurnConfig {
                codex_approval_policy: Some(default_codex_approval_policy()),
                codex_sandbox_mode: Some(default_codex_sandbox_mode()),
                agent,
                cwd: cwd.clone(),
                prompt: prompt.to_owned(),
                external_session_id: external_session_id.clone(),
            },
            &mut printer,
        )?;
        external_session_id = Some(next_session_id);
    }

    Ok(())
}

enum Mode {
    Server,
    Repl { agent: Agent },
}

impl Mode {
    fn parse(args: Vec<String>) -> Result<Self> {
        match args.first().map(String::as_str) {
            None | Some("server") => Ok(Self::Server),
            Some("repl") | Some("cli") => Ok(Self::Repl {
                agent: Agent::parse(args.into_iter().skip(1))?,
            }),
            _ => Ok(Self::Repl {
                agent: Agent::parse(args.into_iter())?,
            }),
        }
    }
}

#[derive(Clone)]
struct AppState {
    default_workdir: String,
    persistence_path: Arc<PathBuf>,
    state_events: broadcast::Sender<String>,
    delta_events: broadcast::Sender<String>,
    inner: Arc<Mutex<StateInner>>,
}

impl AppState {
    fn new(default_workdir: String) -> Result<Self> {
        let persistence_path = resolve_persistence_path(&default_workdir);
        let inner = load_state(&persistence_path)?.unwrap_or_else(|| {
            let mut inner = StateInner::new();
            inner.create_session(
                Agent::Codex,
                Some("Codex Live".to_owned()),
                default_workdir.clone(),
            );
            inner.create_session(
                Agent::Claude,
                Some("Claude Live".to_owned()),
                default_workdir.clone(),
            );
            inner
        });

        let state = Self {
            default_workdir,
            persistence_path: Arc::new(persistence_path),
            state_events: broadcast::channel(128).0,
            delta_events: broadcast::channel(256).0,
            inner: Arc::new(Mutex::new(inner)),
        };
        {
            let inner = state.inner.lock().expect("state mutex poisoned");
            state.persist_internal_locked(&inner)?;
        }
        Ok(state)
    }

    fn snapshot(&self) -> StateResponse {
        let inner = self.inner.lock().expect("state mutex poisoned");
        Self::snapshot_from_inner(&inner)
    }

    fn create_session(&self, request: CreateSessionRequest) -> Result<CreateSessionResponse> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let mut record = inner.create_session(
            request.agent.unwrap_or(Agent::Codex),
            request.name,
            request
                .workdir
                .unwrap_or_else(|| self.default_workdir.clone()),
        );
        if record.session.agent == Agent::Codex {
            if let Some(sandbox_mode) = request.sandbox_mode {
                record.codex_sandbox_mode = sandbox_mode;
                record.session.sandbox_mode = Some(sandbox_mode);
            }
            if let Some(approval_policy) = request.approval_policy {
                record.codex_approval_policy = approval_policy;
                record.session.approval_policy = Some(approval_policy);
            }
        } else if let Some(claude_approval_mode) = request.claude_approval_mode {
            record.session.claude_approval_mode = Some(claude_approval_mode);
        }
        if let Some(slot) = inner
            .find_session_index(&record.session.id)
            .and_then(|index| inner.sessions.get_mut(index))
        {
            *slot = record.clone();
        }
        self.commit_locked(&mut inner)?;
        Ok(CreateSessionResponse {
            session_id: record.session.id,
            state: Self::snapshot_from_inner(&inner),
        })
    }

    fn commit_locked(&self, inner: &mut StateInner) -> Result<u64> {
        let revision = self.bump_revision_and_persist_locked(inner)?;
        self.publish_state_locked(inner)?;
        Ok(revision)
    }

    // Internal bookkeeping changes should be persisted without advancing the client-visible revision.
    fn persist_internal_locked(&self, inner: &StateInner) -> Result<()> {
        persist_state(self.persistence_path.as_path(), inner)
    }

    // Delta-producing changes advance the revision without publishing a full snapshot; the delta event
    // carries the new revision instead.
    fn commit_delta_locked(&self, inner: &mut StateInner) -> Result<u64> {
        self.bump_revision_and_persist_locked(inner)
    }

    fn bump_revision_and_persist_locked(&self, inner: &mut StateInner) -> Result<u64> {
        inner.revision += 1;
        self.persist_internal_locked(inner)?;
        Ok(inner.revision)
    }

    fn subscribe_events(&self) -> broadcast::Receiver<String> {
        self.state_events.subscribe()
    }

    fn subscribe_delta_events(&self) -> broadcast::Receiver<String> {
        self.delta_events.subscribe()
    }

    fn publish_delta(&self, event: &DeltaEvent) {
        if let Ok(payload) = serde_json::to_string(event) {
            let _ = self.delta_events.send(payload);
        }
    }

    fn publish_state_locked(&self, inner: &StateInner) -> Result<()> {
        let payload = serde_json::to_string(&Self::snapshot_from_inner(inner))
            .context("failed to serialize session snapshot")?;
        let _ = self.state_events.send(payload);
        Ok(())
    }

    fn snapshot_from_inner(inner: &StateInner) -> StateResponse {
        StateResponse {
            revision: inner.revision,
            codex: inner.codex.clone(),
            sessions: inner
                .sessions
                .iter()
                .map(|record| record.session.clone())
                .collect(),
        }
    }

    fn start_turn_on_record(
        &self,
        record: &mut SessionRecord,
        message_id: String,
        prompt: String,
        attachments: Vec<PromptImageAttachment>,
    ) -> std::result::Result<TurnDispatch, ApiError> {
        let message_attachments = attachments
            .iter()
            .map(|attachment| attachment.metadata.clone())
            .collect::<Vec<_>>();

        let dispatch = match record.session.agent {
            Agent::Claude => {
                let handle = match &record.runtime {
                    SessionRuntime::Claude(handle) => handle.clone(),
                    SessionRuntime::Codex(_) => {
                        return Err(ApiError::internal(
                            "unexpected Codex runtime attached to Claude session",
                        ));
                    }
                    SessionRuntime::None => {
                        let handle = spawn_claude_runtime(
                            self.clone(),
                            record.session.id.clone(),
                            record.session.workdir.clone(),
                            record.external_session_id.clone(),
                        )
                        .map_err(|err| {
                            ApiError::internal(format!(
                                "failed to start persistent Claude session: {err:#}"
                            ))
                        })?;
                        record.runtime = SessionRuntime::Claude(handle.clone());
                        handle
                    }
                };

                TurnDispatch::PersistentClaude {
                    command: ClaudePromptCommand {
                        attachments: attachments.clone(),
                        text: prompt.clone(),
                    },
                    sender: handle.input_tx,
                    session_id: record.session.id.clone(),
                }
            }
            Agent::Codex => {
                let handle = match &record.runtime {
                    SessionRuntime::Codex(handle) => handle.clone(),
                    SessionRuntime::Claude(_) => {
                        return Err(ApiError::internal(
                            "unexpected Claude runtime attached to Codex session",
                        ));
                    }
                    SessionRuntime::None => {
                        let handle = spawn_codex_runtime(self.clone(), record.session.id.clone())
                            .map_err(|err| {
                            ApiError::internal(format!(
                                "failed to start persistent Codex session: {err:#}"
                            ))
                        })?;
                        record.runtime = SessionRuntime::Codex(handle.clone());
                        handle
                    }
                };

                TurnDispatch::PersistentCodex {
                    command: CodexPromptCommand {
                        approval_policy: record.codex_approval_policy,
                        attachments,
                        cwd: record.session.workdir.clone(),
                        prompt: prompt.to_owned(),
                        resume_thread_id: record.external_session_id.clone(),
                        sandbox_mode: record.codex_sandbox_mode,
                    },
                    sender: handle.input_tx,
                    session_id: record.session.id.clone(),
                }
            }
        };

        record.session.messages.push(Message::Text {
            attachments: message_attachments.clone(),
            id: message_id,
            timestamp: stamp_now(),
            author: Author::You,
            text: prompt.clone(),
        });
        record.session.status = SessionStatus::Active;
        record.session.preview = prompt_preview_text(&prompt, &message_attachments);

        Ok(dispatch)
    }

    fn dispatch_next_queued_turn(&self, session_id: &str) -> Result<Option<TurnDispatch>> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;

        let queued = inner.sessions[index].queued_prompts.front().cloned();

        let Some(queued) = queued else {
            return Ok(None);
        };

        let dispatch = self
            .start_turn_on_record(
                &mut inner.sessions[index],
                queued.pending_prompt.id.clone(),
                queued.pending_prompt.text.clone(),
                queued.attachments.clone(),
            )
            .map_err(|err| anyhow!("failed to dispatch queued prompt: {}", err.message))?;
        inner.sessions[index].queued_prompts.pop_front();
        sync_pending_prompts(&mut inner.sessions[index]);
        self.commit_locked(&mut inner)?;
        Ok(Some(dispatch))
    }

    fn dispatch_turn(
        &self,
        session_id: &str,
        request: SendMessageRequest,
    ) -> std::result::Result<DispatchTurnResult, ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;

        let prompt = request.text.trim().to_owned();
        let attachments = parse_prompt_image_attachments(&request.attachments)?;
        if prompt.is_empty() && attachments.is_empty() {
            return Err(ApiError::bad_request("prompt cannot be empty"));
        }

        if matches!(
            inner.sessions[index].session.status,
            SessionStatus::Active | SessionStatus::Approval
        ) {
            let message_id = inner.next_message_id();
            queue_prompt_on_record(
                &mut inner.sessions[index],
                PendingPrompt {
                    attachments: attachments
                        .iter()
                        .map(|attachment| attachment.metadata.clone())
                        .collect(),
                    id: message_id,
                    timestamp: stamp_now(),
                    text: prompt,
                },
                attachments,
            );
            self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist session state: {err:#}"))
            })?;
            return Ok(DispatchTurnResult::Queued);
        }

        let message_id = inner.next_message_id();
        let dispatch =
            self.start_turn_on_record(&mut inner.sessions[index], message_id, prompt, attachments)?;

        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;

        Ok(DispatchTurnResult::Dispatched(dispatch))
    }

    fn update_session_settings(
        &self,
        session_id: &str,
        request: UpdateSessionSettingsRequest,
    ) -> std::result::Result<StateResponse, ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = &mut inner.sessions[index];

        match record.session.agent {
            Agent::Codex => {
                if request.claude_approval_mode.is_some() {
                    return Err(ApiError::bad_request(
                        "Claude approval mode can only be changed for Claude sessions",
                    ));
                }

                if let Some(sandbox_mode) = request.sandbox_mode {
                    record.codex_sandbox_mode = sandbox_mode;
                    record.session.sandbox_mode = Some(sandbox_mode);
                }
                if let Some(approval_policy) = request.approval_policy {
                    record.codex_approval_policy = approval_policy;
                    record.session.approval_policy = Some(approval_policy);
                }
            }
            Agent::Claude => {
                if request.sandbox_mode.is_some() || request.approval_policy.is_some() {
                    return Err(ApiError::bad_request(
                        "Claude sessions only support approval mode settings",
                    ));
                }

                if let Some(claude_approval_mode) = request.claude_approval_mode {
                    record.session.claude_approval_mode = Some(claude_approval_mode);
                }
            }
        }

        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;
        Ok(Self::snapshot_from_inner(&inner))
    }

    fn allocate_message_id(&self) -> String {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        inner.next_message_id()
    }

    fn set_external_session_id(&self, session_id: &str, external_session_id: String) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index].external_session_id = Some(external_session_id.clone());
        inner.sessions[index].session.external_session_id = Some(external_session_id);
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    fn note_codex_rate_limits(&self, rate_limits: CodexRateLimits) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if inner.codex.rate_limits.as_ref() == Some(&rate_limits) {
            return Ok(());
        }

        inner.codex.rate_limits = Some(rate_limits);
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    fn record_codex_runtime_config(
        &self,
        session_id: &str,
        sandbox_mode: CodexSandboxMode,
        approval_policy: CodexApprovalPolicy,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index].active_codex_sandbox_mode = Some(sandbox_mode);
        inner.sessions[index].active_codex_approval_policy = Some(approval_policy);
        self.persist_internal_locked(&inner)?;
        Ok(())
    }

    fn claude_approval_mode(&self, session_id: &str) -> Result<ClaudeApprovalMode> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        Ok(inner.sessions[index]
            .session
            .claude_approval_mode
            .unwrap_or_else(default_claude_approval_mode))
    }

    fn set_codex_runtime(&self, session_id: &str, handle: CodexRuntimeHandle) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index].runtime = SessionRuntime::Codex(handle);
        Ok(())
    }

    fn clear_runtime(&self, session_id: &str) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index].runtime = SessionRuntime::None;
        inner.sessions[index].pending_claude_approvals.clear();
        inner.sessions[index].pending_codex_approvals.clear();
        Ok(())
    }

    fn fail_turn_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        error_message: &str,
    ) -> Result<()> {
        let cleaned = error_message.trim();
        let should_dispatch_next = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let message_id = (!cleaned.is_empty()).then(|| inner.next_message_id());
            let record = &mut inner.sessions[index];
            if !record.runtime.matches_runtime_token(token) {
                return Ok(());
            }

            if let Some(message_id) = message_id {
                record.session.messages.push(Message::Text {
                    attachments: Vec::new(),
                    id: message_id,
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: format!("Turn failed: {cleaned}"),
                });
            }

            record.session.status = SessionStatus::Error;
            record.session.preview = make_preview(cleaned);
            self.commit_locked(&mut inner)?;
            true
        };

        if should_dispatch_next {
            if let Some(dispatch) = self.dispatch_next_queued_turn(session_id)? {
                deliver_turn_dispatch(self, dispatch).map_err(|err| {
                    anyhow!("failed to deliver queued turn dispatch: {}", err.message)
                })?;
            }
        }

        Ok(())
    }

    fn note_turn_retry_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        detail: &str,
    ) -> Result<()> {
        let cleaned = detail.trim();
        if cleaned.is_empty() {
            return Ok(());
        }

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;

        let duplicate_last_message = {
            let record = &inner.sessions[index];
            if !record.runtime.matches_runtime_token(token) {
                return Ok(());
            }

            matches!(
                record.session.messages.last(),
                Some(Message::Text {
                    author: Author::Assistant,
                    text,
                    ..
                }) if text.trim() == cleaned
            )
        };

        let message_id = (!duplicate_last_message).then(|| inner.next_message_id());
        let record = &mut inner.sessions[index];

        if let Some(message_id) = message_id {
            record.session.messages.push(Message::Text {
                attachments: Vec::new(),
                id: message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: cleaned.to_owned(),
            });
        }

        if record.session.status != SessionStatus::Approval {
            record.session.status = SessionStatus::Active;
        }
        record.session.preview = make_preview(cleaned);
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    fn mark_turn_error_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        error_message: &str,
    ) -> Result<()> {
        let cleaned = error_message.trim();
        let should_dispatch_next = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let record = &mut inner.sessions[index];
            if !record.runtime.matches_runtime_token(token) {
                return Ok(());
            }

            record.session.status = SessionStatus::Error;
            if !cleaned.is_empty() {
                record.session.preview = make_preview(cleaned);
            }
            self.commit_locked(&mut inner)?;
            true
        };

        if should_dispatch_next {
            if let Some(dispatch) = self.dispatch_next_queued_turn(session_id)? {
                deliver_turn_dispatch(self, dispatch).map_err(|err| {
                    anyhow!("failed to deliver queued turn dispatch: {}", err.message)
                })?;
            }
        }

        Ok(())
    }

    fn finish_turn_ok_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
    ) -> Result<()> {
        let should_dispatch_next = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let record = &mut inner.sessions[index];
            if !record.runtime.matches_runtime_token(token) {
                return Ok(());
            }

            if record.session.status == SessionStatus::Active {
                record.session.status = SessionStatus::Idle;
            }
            if record.session.preview.trim().is_empty() {
                record.session.preview = "Turn completed.".to_owned();
            }
            self.commit_locked(&mut inner)?;
            true
        };

        if should_dispatch_next {
            if let Some(dispatch) = self.dispatch_next_queued_turn(session_id)? {
                deliver_turn_dispatch(self, dispatch).map_err(|err| {
                    anyhow!("failed to deliver queued turn dispatch: {}", err.message)
                })?;
            }
        }

        Ok(())
    }

    fn handle_runtime_exit_if_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        error_message: Option<&str>,
    ) -> Result<()> {
        let cleaned = error_message.map(str::trim).unwrap_or("");
        let should_dispatch_next = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let matches_runtime = inner.sessions[index].runtime.matches_runtime_token(token);
            if !matches_runtime {
                return Ok(());
            }
            let was_busy = matches!(
                inner.sessions[index].session.status,
                SessionStatus::Active | SessionStatus::Approval
            );
            let message_id = (was_busy || !cleaned.is_empty()).then(|| inner.next_message_id());
            let record = &mut inner.sessions[index];
            record.runtime = SessionRuntime::None;
            record.pending_claude_approvals.clear();
            record.pending_codex_approvals.clear();

            if !cleaned.is_empty() || was_busy {
                let detail = if !cleaned.is_empty() {
                    cleaned.to_owned()
                } else {
                    match token {
                        RuntimeToken::Claude(_) => {
                            "Claude session exited before the active turn completed".to_owned()
                        }
                        RuntimeToken::Codex(_) => {
                            "Codex session exited before the active turn completed".to_owned()
                        }
                    }
                };
                if let Some(message_id) = message_id {
                    record.session.messages.push(Message::Text {
                        attachments: Vec::new(),
                        id: message_id,
                        timestamp: stamp_now(),
                        author: Author::Assistant,
                        text: format!("Turn failed: {detail}"),
                    });
                }
                record.session.status = SessionStatus::Error;
                record.session.preview = make_preview(&detail);
            }

            let has_queued_prompts = !record.queued_prompts.is_empty();
            self.commit_locked(&mut inner)?;
            has_queued_prompts
        };

        if should_dispatch_next {
            if let Some(dispatch) = self.dispatch_next_queued_turn(session_id)? {
                deliver_turn_dispatch(self, dispatch).map_err(|err| {
                    anyhow!("failed to deliver queued turn dispatch: {}", err.message)
                })?;
            }
        }

        Ok(())
    }

    fn register_claude_pending_approval(
        &self,
        session_id: &str,
        message_id: String,
        approval: ClaudePendingApproval,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index]
            .pending_claude_approvals
            .insert(message_id, approval);
        Ok(())
    }

    fn register_codex_pending_approval(
        &self,
        session_id: &str,
        message_id: String,
        approval: CodexPendingApproval,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index]
            .pending_codex_approvals
            .insert(message_id, approval);
        Ok(())
    }

    fn clear_claude_pending_approval_by_request(
        &self,
        session_id: &str,
        request_id: &str,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index]
            .pending_claude_approvals
            .retain(|_, approval| approval.request_id != request_id);
        Ok(())
    }

    fn kill_session(&self, session_id: &str) -> std::result::Result<StateResponse, ApiError> {
        let runtime_to_kill = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let record = &mut inner.sessions[index];

            let runtime = match &record.runtime {
                SessionRuntime::Claude(handle) => Some(KillableRuntime::Claude(handle.clone())),
                SessionRuntime::Codex(handle) => Some(KillableRuntime::Codex(handle.clone())),
                SessionRuntime::None => None,
            };

            if runtime.is_some()
                && matches!(
                    record.session.status,
                    SessionStatus::Active | SessionStatus::Approval
                )
            {
                record.session.status = SessionStatus::Idle;
                record.session.preview = "Stopping session…".to_owned();
                self.commit_locked(&mut inner).map_err(|err| {
                    ApiError::internal(format!("failed to persist session state: {err:#}"))
                })?;
            }

            runtime
        };

        if let Some(runtime) = runtime_to_kill {
            runtime
                .kill()
                .map_err(|err| ApiError::internal(format!("failed to kill session: {err:#}")))?;
        }

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        inner.sessions.remove(index);

        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;
        Ok(Self::snapshot_from_inner(&inner))
    }

    fn cancel_queued_prompt(
        &self,
        session_id: &str,
        prompt_id: &str,
    ) -> std::result::Result<StateResponse, ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = &mut inner.sessions[index];
        let original_len = record.queued_prompts.len();
        record
            .queued_prompts
            .retain(|queued| queued.pending_prompt.id != prompt_id);
        if record.queued_prompts.len() == original_len {
            return Err(ApiError::not_found("queued prompt not found"));
        }
        sync_pending_prompts(record);

        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;
        Ok(Self::snapshot_from_inner(&inner))
    }

    fn stop_session(&self, session_id: &str) -> std::result::Result<StateResponse, ApiError> {
        let runtime_to_stop = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let record = &mut inner.sessions[index];

            if !matches!(
                record.session.status,
                SessionStatus::Active | SessionStatus::Approval
            ) {
                return Err(ApiError::conflict("session is not currently running"));
            }

            for message in &mut record.session.messages {
                if let Message::Approval { decision, .. } = message {
                    if *decision == ApprovalDecision::Pending {
                        *decision = ApprovalDecision::Rejected;
                    }
                }
            }

            let runtime = match &record.runtime {
                SessionRuntime::Claude(handle) => KillableRuntime::Claude(handle.clone()),
                SessionRuntime::Codex(handle) => KillableRuntime::Codex(handle.clone()),
                SessionRuntime::None => {
                    return Err(ApiError::conflict("session is not currently running"));
                }
            };

            record.session.status = SessionStatus::Idle;
            record.session.preview = "Stopping turn…".to_owned();
            self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist session state: {err:#}"))
            })?;

            runtime
        };

        runtime_to_stop
            .kill()
            .map_err(|err| ApiError::internal(format!("failed to stop session: {err:#}")))?;
        self.clear_runtime(session_id)
            .map_err(|err| ApiError::internal(format!("failed to clear runtime: {err:#}")))?;
        self.push_message(
            session_id,
            Message::Text {
                attachments: Vec::new(),
                id: self.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Turn stopped by user.".to_owned(),
            },
        )
        .map_err(|err| ApiError::internal(format!("failed to record stop message: {err:#}")))?;

        if let Some(dispatch) = self.dispatch_next_queued_turn(session_id).map_err(|err| {
            ApiError::internal(format!("failed to dispatch queued prompt: {err:#}"))
        })? {
            deliver_turn_dispatch(self, dispatch)?;
        }

        Ok(self.snapshot())
    }

    fn push_message(&self, session_id: &str, message: Message) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let session = &mut inner.sessions[index].session;
        if let Some(preview) = message.preview_text() {
            session.preview = preview;
        }
        if matches!(message, Message::Approval { .. }) {
            session.status = SessionStatus::Approval;
        }
        session.messages.push(message);
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    fn append_text_delta(&self, session_id: &str, message_id: &str, delta: &str) -> Result<()> {
        let (preview, revision) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let session = &mut inner.sessions[index].session;

            let mut updated_text = None;
            for message in &mut session.messages {
                if let Message::Text { id, text, .. } = message {
                    if id == message_id {
                        text.push_str(delta);
                        updated_text = Some(text.clone());
                        break;
                    }
                }
            }

            let preview = if let Some(text) = updated_text {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    let p = make_preview(trimmed);
                    session.preview = p.clone();
                    Some(p)
                } else {
                    None
                }
            } else {
                None
            };
            let revision = self.commit_delta_locked(&mut inner)?;
            (preview, revision)
        };

        self.publish_delta(&DeltaEvent::TextDelta {
            revision,
            session_id: session_id.to_owned(),
            message_id: message_id.to_owned(),
            delta: delta.to_owned(),
            preview,
        });

        Ok(())
    }

    fn upsert_command_message(
        &self,
        session_id: &str,
        message_id: &str,
        command: &str,
        output: &str,
        status: CommandStatus,
    ) -> Result<()> {
        let command_language = Some(shell_language().to_owned());
        let output_language = infer_command_output_language(command).map(str::to_owned);

        let (preview, revision) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let session = &mut inner.sessions[index].session;

            let mut found = false;
            for message in &mut session.messages {
                if let Message::Command {
                    id,
                    command: existing_command,
                    command_language: existing_command_language,
                    output: existing_output,
                    output_language: existing_output_language,
                    status: existing_status,
                    ..
                } = message
                {
                    if id == message_id {
                        *existing_command = command.to_owned();
                        *existing_command_language = command_language.clone();
                        *existing_output = output.to_owned();
                        *existing_output_language = output_language.clone();
                        *existing_status = status;
                        found = true;
                        break;
                    }
                }
            }

            if !found {
                session.messages.push(Message::Command {
                    id: message_id.to_owned(),
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    command: command.to_owned(),
                    command_language: command_language.clone(),
                    output: output.to_owned(),
                    output_language: output_language.clone(),
                    status,
                });
            }

            let preview = match status {
                CommandStatus::Running => make_preview(&format!("Running {command}")),
                CommandStatus::Success => {
                    if output.trim().is_empty() {
                        make_preview(&format!("Completed {command}"))
                    } else {
                        make_preview(output.trim())
                    }
                }
                CommandStatus::Error => {
                    if output.trim().is_empty() {
                        make_preview(&format!("Command failed: {command}"))
                    } else {
                        make_preview(output.trim())
                    }
                }
            };
            session.preview = preview.clone();
            let revision = self.commit_delta_locked(&mut inner)?;
            (preview, revision)
        };

        self.publish_delta(&DeltaEvent::CommandUpdate {
            revision,
            session_id: session_id.to_owned(),
            message_id: message_id.to_owned(),
            command: command.to_owned(),
            command_language,
            output: output.to_owned(),
            output_language,
            status,
            preview,
        });

        Ok(())
    }

    fn update_approval(
        &self,
        session_id: &str,
        message_id: &str,
        decision: ApprovalDecision,
    ) -> std::result::Result<StateResponse, ApiError> {
        let mut claude_runtime_action: Option<(ClaudeRuntimeHandle, ClaudePendingApproval)> = None;
        let mut codex_runtime_action: Option<(CodexRuntimeHandle, CodexPendingApproval)> = None;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = &mut inner.sessions[index];
        let session = &mut record.session;
        if session.status != SessionStatus::Approval {
            return Err(ApiError::conflict(
                "session is not currently awaiting approval",
            ));
        }

        if session.agent == Agent::Claude
            && matches!(
                decision,
                ApprovalDecision::Accepted
                    | ApprovalDecision::AcceptedForSession
                    | ApprovalDecision::Rejected
            )
        {
            let pending = record
                .pending_claude_approvals
                .get(message_id)
                .cloned()
                .ok_or_else(|| ApiError::conflict("approval request is no longer live"))?;
            let handle = match &record.runtime {
                SessionRuntime::Claude(handle) => handle.clone(),
                SessionRuntime::Codex(_) => {
                    return Err(ApiError::conflict(
                        "Claude session is not currently running",
                    ));
                }
                SessionRuntime::None => {
                    return Err(ApiError::conflict(
                        "Claude session is not currently running",
                    ));
                }
            };
            claude_runtime_action = Some((handle, pending));
        } else if session.agent == Agent::Codex
            && matches!(
                decision,
                ApprovalDecision::Accepted
                    | ApprovalDecision::AcceptedForSession
                    | ApprovalDecision::Rejected
            )
        {
            let pending = record
                .pending_codex_approvals
                .get(message_id)
                .cloned()
                .ok_or_else(|| ApiError::conflict("approval request is no longer live"))?;
            let handle = match &record.runtime {
                SessionRuntime::Codex(handle) => handle.clone(),
                SessionRuntime::Claude(_) | SessionRuntime::None => {
                    return Err(ApiError::conflict("Codex session is not currently running"));
                }
            };
            codex_runtime_action = Some((handle, pending));
        }

        drop(inner);

        if let Some((handle, pending)) = claude_runtime_action {
            if decision == ApprovalDecision::AcceptedForSession {
                if let Some(mode) = pending.permission_mode_for_session.clone() {
                    handle
                        .input_tx
                        .send(ClaudeRuntimeCommand::SetPermissionMode(mode))
                        .map_err(|err| {
                            ApiError::internal(format!(
                                "failed to update Claude permission mode: {err}"
                            ))
                        })?;
                }
            }

            let response = match decision {
                ApprovalDecision::Accepted | ApprovalDecision::AcceptedForSession => {
                    ClaudePermissionDecision::Allow {
                        request_id: pending.request_id.clone(),
                        updated_input: pending.tool_input.clone(),
                    }
                }
                ApprovalDecision::Rejected => ClaudePermissionDecision::Deny {
                    request_id: pending.request_id.clone(),
                    message: "User rejected this action in TermAl.".to_owned(),
                },
                ApprovalDecision::Pending => unreachable!("pending decisions are not sent"),
            };

            handle
                .input_tx
                .send(ClaudeRuntimeCommand::PermissionResponse(response))
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to deliver approval response to Claude: {err}"
                    ))
                })?;
        }
        if let Some((handle, pending)) = codex_runtime_action {
            handle
                .input_tx
                .send(CodexRuntimeCommand::ApprovalResponse(
                    CodexApprovalResponseCommand {
                        request_id: pending.request_id.clone(),
                        result: codex_approval_result(&pending.kind, decision),
                    },
                ))
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to deliver approval response to Codex: {err}"
                    ))
                })?;
        }

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = &mut inner.sessions[index];
        let session = &mut record.session;
        if session.status != SessionStatus::Approval && decision == ApprovalDecision::Pending {
            return Err(ApiError::conflict(
                "session is not currently awaiting approval",
            ));
        }
        let mut found = false;
        for message in &mut session.messages {
            if let Message::Approval {
                id,
                decision: current,
                ..
            } = message
            {
                if id == message_id {
                    *current = decision;
                    found = true;
                    break;
                }
            }
        }

        if !found {
            return Err(ApiError::not_found("approval message not found"));
        }

        if session.agent == Agent::Claude {
            if decision != ApprovalDecision::Pending {
                record.pending_claude_approvals.remove(message_id);
            }
            if session.status == SessionStatus::Approval {
                session.status = if decision == ApprovalDecision::Pending {
                    SessionStatus::Approval
                } else {
                    SessionStatus::Active
                };
            }
            if session.status == SessionStatus::Approval || session.status == SessionStatus::Active
            {
                session.preview = match decision {
                    ApprovalDecision::Pending => "Approval pending.".to_owned(),
                    ApprovalDecision::Accepted => {
                        "Approval granted. Claude is continuing…".to_owned()
                    }
                    ApprovalDecision::AcceptedForSession => {
                        "Approval granted for this session. Claude is continuing…".to_owned()
                    }
                    ApprovalDecision::Rejected => {
                        "Approval rejected. Claude is continuing…".to_owned()
                    }
                };
            }
        } else {
            if decision != ApprovalDecision::Pending {
                record.pending_codex_approvals.remove(message_id);
            }
            if session.status == SessionStatus::Approval {
                session.status = if decision == ApprovalDecision::Pending {
                    SessionStatus::Approval
                } else {
                    SessionStatus::Active
                };
            }
            if session.status == SessionStatus::Approval || session.status == SessionStatus::Active
            {
                session.preview = match decision {
                    ApprovalDecision::Pending => "Approval pending.".to_owned(),
                    ApprovalDecision::Accepted => {
                        "Approval granted. Codex is continuing…".to_owned()
                    }
                    ApprovalDecision::AcceptedForSession => {
                        "Approval granted for this session. Codex is continuing…".to_owned()
                    }
                    ApprovalDecision::Rejected => {
                        "Approval rejected. Codex is continuing…".to_owned()
                    }
                };
            }
        }

        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;
        Ok(Self::snapshot_from_inner(&inner))
    }

    fn fail_turn(&self, session_id: &str, error_message: &str) -> Result<()> {
        let cleaned = error_message.trim();
        if !cleaned.is_empty() {
            self.push_message(
                session_id,
                Message::Text {
                    attachments: Vec::new(),
                    id: self.allocate_message_id(),
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: format!("Turn failed: {cleaned}"),
                },
            )?;
        }

        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let session = &mut inner.sessions[index].session;
            session.status = SessionStatus::Error;
            session.preview = make_preview(cleaned);
            self.commit_locked(&mut inner)?;
        }

        if let Some(dispatch) = self.dispatch_next_queued_turn(session_id)? {
            deliver_turn_dispatch(self, dispatch).map_err(|err| {
                anyhow!("failed to deliver queued turn dispatch: {}", err.message)
            })?;
        }
        Ok(())
    }
}

fn codex_approval_result(kind: &CodexApprovalKind, decision: ApprovalDecision) -> Value {
    let decision_value = match kind {
        CodexApprovalKind::CommandExecution => match decision {
            ApprovalDecision::Accepted => json!("accept"),
            ApprovalDecision::AcceptedForSession => json!("acceptForSession"),
            ApprovalDecision::Rejected => json!("decline"),
            ApprovalDecision::Pending => unreachable!("pending approvals are not sent to Codex"),
        },
        CodexApprovalKind::FileChange => match decision {
            ApprovalDecision::Accepted => json!("accept"),
            ApprovalDecision::AcceptedForSession => json!("acceptForSession"),
            ApprovalDecision::Rejected => json!("decline"),
            ApprovalDecision::Pending => unreachable!("pending approvals are not sent to Codex"),
        },
    };

    json!({ "decision": decision_value })
}

struct StateInner {
    codex: CodexState,
    revision: u64,
    next_session_number: usize,
    next_message_number: u64,
    sessions: Vec<SessionRecord>,
}

impl StateInner {
    fn new() -> Self {
        Self {
            codex: CodexState::default(),
            revision: 0,
            next_session_number: 1,
            next_message_number: 1,
            sessions: Vec::new(),
        }
    }

    fn create_session(
        &mut self,
        agent: Agent,
        name: Option<String>,
        workdir: String,
    ) -> SessionRecord {
        let number = self.next_session_number;
        self.next_session_number += 1;

        let record = SessionRecord {
            active_codex_approval_policy: None,
            active_codex_sandbox_mode: None,
            codex_approval_policy: default_codex_approval_policy(),
            codex_sandbox_mode: default_codex_sandbox_mode(),
            external_session_id: None,
            pending_claude_approvals: HashMap::new(),
            pending_codex_approvals: HashMap::new(),
            queued_prompts: VecDeque::new(),
            runtime: SessionRuntime::None,
            session: Session {
                id: format!("session-{number}"),
                name: name.unwrap_or_else(|| format!("{} {}", agent.name(), number)),
                emoji: agent.avatar().to_owned(),
                agent,
                workdir,
                model: agent.model_label().to_owned(),
                approval_policy: None,
                sandbox_mode: None,
                claude_approval_mode: (agent == Agent::Claude)
                    .then_some(default_claude_approval_mode()),
                external_session_id: None,
                status: SessionStatus::Idle,
                preview: "Ready for a prompt.".to_owned(),
                messages: Vec::new(),
                pending_prompts: Vec::new(),
            },
        };

        let mut record = record;
        if record.session.agent == Agent::Codex {
            record.session.approval_policy = Some(record.codex_approval_policy);
            record.session.sandbox_mode = Some(record.codex_sandbox_mode);
        }

        self.sessions.push(record.clone());
        record
    }

    fn next_message_id(&mut self) -> String {
        let id = format!("message-{}", self.next_message_number);
        self.next_message_number += 1;
        id
    }

    fn find_session_index(&self, session_id: &str) -> Option<usize> {
        self.sessions
            .iter()
            .position(|record| record.session.id == session_id)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedState {
    #[serde(default, skip_serializing_if = "CodexState::is_empty")]
    codex: CodexState,
    #[serde(default)]
    revision: u64,
    next_session_number: usize,
    next_message_number: u64,
    sessions: Vec<PersistedSessionRecord>,
}

impl PersistedState {
    fn from_inner(inner: &StateInner) -> Self {
        Self {
            codex: inner.codex.clone(),
            revision: inner.revision,
            next_session_number: inner.next_session_number,
            next_message_number: inner.next_message_number,
            sessions: inner
                .sessions
                .iter()
                .map(PersistedSessionRecord::from_record)
                .collect(),
        }
    }

    fn into_inner(self) -> StateInner {
        StateInner {
            codex: self.codex,
            revision: self.revision,
            next_session_number: self.next_session_number,
            next_message_number: self.next_message_number,
            sessions: self
                .sessions
                .into_iter()
                .map(PersistedSessionRecord::into_record)
                .collect(),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedSessionRecord {
    active_codex_approval_policy: Option<CodexApprovalPolicy>,
    active_codex_sandbox_mode: Option<CodexSandboxMode>,
    codex_approval_policy: CodexApprovalPolicy,
    codex_sandbox_mode: CodexSandboxMode,
    external_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "VecDeque::is_empty")]
    queued_prompts: VecDeque<QueuedPromptRecord>,
    session: Session,
}

impl PersistedSessionRecord {
    fn from_record(record: &SessionRecord) -> Self {
        let mut session = record.session.clone();
        session.pending_prompts.clear();

        Self {
            active_codex_approval_policy: record.active_codex_approval_policy,
            active_codex_sandbox_mode: record.active_codex_sandbox_mode,
            codex_approval_policy: record.codex_approval_policy,
            codex_sandbox_mode: record.codex_sandbox_mode,
            external_session_id: record.external_session_id.clone(),
            queued_prompts: record.queued_prompts.clone(),
            session,
        }
    }

    fn into_record(self) -> SessionRecord {
        let mut session = self.session;
        session.external_session_id = self.external_session_id.clone();
        if session.agent == Agent::Claude {
            session
                .claude_approval_mode
                .get_or_insert_with(default_claude_approval_mode);
        } else {
            session.claude_approval_mode = None;
        }
        session.pending_prompts.clear();

        let mut record = SessionRecord {
            active_codex_approval_policy: self.active_codex_approval_policy,
            active_codex_sandbox_mode: self.active_codex_sandbox_mode,
            codex_approval_policy: self.codex_approval_policy,
            codex_sandbox_mode: self.codex_sandbox_mode,
            external_session_id: self.external_session_id,
            pending_claude_approvals: HashMap::new(),
            pending_codex_approvals: HashMap::new(),
            queued_prompts: self.queued_prompts,
            runtime: SessionRuntime::None,
            session,
        };
        sync_pending_prompts(&mut record);
        record
    }
}

#[derive(Clone)]
struct SessionRecord {
    active_codex_approval_policy: Option<CodexApprovalPolicy>,
    active_codex_sandbox_mode: Option<CodexSandboxMode>,
    codex_approval_policy: CodexApprovalPolicy,
    codex_sandbox_mode: CodexSandboxMode,
    external_session_id: Option<String>,
    pending_claude_approvals: HashMap<String, ClaudePendingApproval>,
    pending_codex_approvals: HashMap<String, CodexPendingApproval>,
    queued_prompts: VecDeque<QueuedPromptRecord>,
    runtime: SessionRuntime,
    session: Session,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct QueuedPromptRecord {
    attachments: Vec<PromptImageAttachment>,
    pending_prompt: PendingPrompt,
}

fn sync_pending_prompts(record: &mut SessionRecord) {
    record.session.pending_prompts = record
        .queued_prompts
        .iter()
        .map(|queued| queued.pending_prompt.clone())
        .collect();
}

fn queue_prompt_on_record(
    record: &mut SessionRecord,
    pending_prompt: PendingPrompt,
    attachments: Vec<PromptImageAttachment>,
) {
    record.queued_prompts.push_back(QueuedPromptRecord {
        attachments,
        pending_prompt,
    });
    sync_pending_prompts(record);
}

#[derive(Clone)]
enum SessionRuntime {
    None,
    Claude(ClaudeRuntimeHandle),
    Codex(CodexRuntimeHandle),
}

#[derive(Clone)]
struct ClaudeRuntimeHandle {
    runtime_id: String,
    input_tx: Sender<ClaudeRuntimeCommand>,
    process: Arc<Mutex<Child>>,
}

impl ClaudeRuntimeHandle {
    fn kill(&self) -> Result<()> {
        kill_child_process(&self.process, "Claude")
    }
}

#[derive(Clone)]
struct CodexRuntimeHandle {
    runtime_id: String,
    input_tx: Sender<CodexRuntimeCommand>,
    process: Arc<Mutex<Child>>,
}

impl CodexRuntimeHandle {
    fn kill(&self) -> Result<()> {
        kill_child_process(&self.process, "Codex")
    }
}

#[derive(Clone)]
enum KillableRuntime {
    Claude(ClaudeRuntimeHandle),
    Codex(CodexRuntimeHandle),
}

impl KillableRuntime {
    fn kill(&self) -> Result<()> {
        match self {
            Self::Claude(handle) => handle.kill(),
            Self::Codex(handle) => handle.kill(),
        }
    }
}

#[derive(Clone)]
enum RuntimeToken {
    Claude(String),
    Codex(String),
}

impl SessionRuntime {
    fn matches_runtime_token(&self, token: &RuntimeToken) -> bool {
        match (self, token) {
            (Self::Claude(handle), RuntimeToken::Claude(runtime_id)) => {
                handle.runtime_id == *runtime_id
            }
            (Self::Codex(handle), RuntimeToken::Codex(runtime_id)) => {
                handle.runtime_id == *runtime_id
            }
            _ => false,
        }
    }
}

fn kill_child_process(process: &Arc<Mutex<Child>>, label: &str) -> Result<()> {
    let mut child = process
        .lock()
        .unwrap_or_else(|_| panic!("{label} process mutex poisoned"));
    match child.try_wait() {
        Ok(Some(_)) => Ok(()),
        Ok(None) => child
            .kill()
            .with_context(|| format!("failed to terminate {label} process")),
        Err(err) => Err(anyhow!("failed to inspect {label} process state: {err}")),
    }
}

struct CodexRolloutStreamer {
    saw_final_answer: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    join: std::thread::JoinHandle<()>,
}

fn spawn_codex_rollout_streamer(
    state: AppState,
    session_id: String,
    rollout_path: PathBuf,
    start_offset: u64,
) -> CodexRolloutStreamer {
    let saw_final_answer = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let thread_saw_final_answer = saw_final_answer.clone();
    let thread_stop = stop.clone();

    let join = std::thread::spawn(move || {
        let file = match fs::File::open(&rollout_path) {
            Ok(file) => file,
            Err(err) => {
                eprintln!(
                    "codex rollout> failed to open `{}`: {err}",
                    rollout_path.display()
                );
                return;
            }
        };

        let mut reader = BufReader::new(file);
        if let Err(err) = reader.seek(SeekFrom::Start(start_offset)) {
            eprintln!(
                "codex rollout> failed to seek `{}`: {err}",
                rollout_path.display()
            );
            return;
        }

        let mut recorder = SessionRecorder::new(state, session_id);
        let mut last_signature: Option<String> = None;
        let mut line = String::new();

        loop {
            line.clear();
            let bytes_read = match reader.read_line(&mut line) {
                Ok(bytes_read) => bytes_read,
                Err(err) => {
                    eprintln!(
                        "codex rollout> failed to read `{}`: {err}",
                        rollout_path.display()
                    );
                    break;
                }
            };

            if bytes_read == 0 {
                if thread_stop.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(60));
                continue;
            }

            let message: Value = match serde_json::from_str(line.trim_end()) {
                Ok(message) => message,
                Err(err) => {
                    eprintln!(
                        "codex rollout> failed to parse line from `{}`: {err}",
                        rollout_path.display()
                    );
                    continue;
                }
            };

            let event = extract_codex_rollout_agent_message(&message);
            let Some((phase, text)) = event else {
                continue;
            };

            let signature = format!("{phase}\n{text}");
            if last_signature.as_deref() == Some(signature.as_str()) {
                continue;
            }
            last_signature = Some(signature);

            if phase == "final_answer" {
                thread_saw_final_answer.store(true, Ordering::SeqCst);
            }

            if let Err(err) = recorder.push_text(&text) {
                eprintln!("codex rollout> failed to push streamed text: {err:#}");
                break;
            }
        }
    });

    CodexRolloutStreamer {
        saw_final_answer,
        stop,
        join,
    }
}

fn locate_codex_rollout_path(codex_home: &FsPath, thread_id: &str) -> Result<Option<PathBuf>> {
    let mut stack = vec![codex_home.join("sessions")];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };

        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };

            if file_type.is_dir() {
                stack.push(path);
                continue;
            }

            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if name.starts_with("rollout-") && name.ends_with(&format!("{thread_id}.jsonl")) {
                return Ok(Some(path));
            }
        }
    }

    Ok(None)
}

fn wait_for_codex_rollout_path(codex_home: &FsPath, thread_id: &str) -> Result<Option<PathBuf>> {
    for _ in 0..20 {
        if let Some(path) = locate_codex_rollout_path(codex_home, thread_id)? {
            return Ok(Some(path));
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    Ok(None)
}

fn resolve_source_codex_home_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CODEX_HOME") {
        return Ok(PathBuf::from(path));
    }

    let home = resolve_home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
    Ok(home.join(".codex"))
}

fn resolve_termal_data_dir(default_workdir: &str) -> PathBuf {
    let base = resolve_home_dir().unwrap_or_else(|| PathBuf::from(default_workdir));
    base.join(".termal")
}

fn resolve_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn resolve_termal_codex_home(default_workdir: &str, scope: &str) -> PathBuf {
    resolve_termal_data_dir(default_workdir)
        .join("codex-home")
        .join(scope)
}

fn prepare_termal_codex_home(default_workdir: &str, scope: &str) -> Result<PathBuf> {
    let target_home = resolve_termal_codex_home(default_workdir, scope);
    fs::create_dir_all(&target_home)
        .with_context(|| format!("failed to create `{}`", target_home.display()))?;
    if let Ok(source_home) = resolve_source_codex_home_dir() {
        seed_termal_codex_home_from(&source_home, &target_home)?;
    }
    Ok(target_home)
}

fn seed_termal_codex_home_from(source_home: &FsPath, target_home: &FsPath) -> Result<()> {
    if !source_home.exists() {
        return Ok(());
    }

    let source_home = fs::canonicalize(source_home).unwrap_or_else(|_| source_home.to_path_buf());
    let target_home = fs::canonicalize(target_home).unwrap_or_else(|_| target_home.to_path_buf());

    if source_home == target_home {
        return Ok(());
    }

    for name in [
        "auth.json",
        "config.toml",
        "models_cache.json",
        ".codex-global-state.json",
    ] {
        sync_codex_home_entry(&source_home.join(name), &target_home.join(name))?;
    }

    for name in ["rules", "memories", "skills"] {
        sync_codex_home_entry(&source_home.join(name), &target_home.join(name))?;
    }

    Ok(())
}

fn sync_codex_home_entry(source: &FsPath, target: &FsPath) -> Result<()> {
    if !source.exists() {
        return Ok(());
    }

    let metadata =
        fs::metadata(source).with_context(|| format!("failed to read `{}`", source.display()))?;

    if metadata.is_dir() {
        sync_codex_home_directory(source, target)
    } else if metadata.is_file() {
        sync_codex_home_file(source, target, &metadata)
    } else {
        Ok(())
    }
}

fn sync_codex_home_directory(source: &FsPath, target: &FsPath) -> Result<()> {
    if target.is_file() {
        fs::remove_file(target)
            .with_context(|| format!("failed to remove `{}`", target.display()))?;
    }

    fs::create_dir_all(target)
        .with_context(|| format!("failed to create `{}`", target.display()))?;

    for entry in
        fs::read_dir(source).with_context(|| format!("failed to read `{}`", source.display()))?
    {
        let entry = entry?;
        sync_codex_home_entry(&entry.path(), &target.join(entry.file_name()))?;
    }

    Ok(())
}

fn sync_codex_home_file(
    source: &FsPath,
    target: &FsPath,
    source_metadata: &fs::Metadata,
) -> Result<()> {
    let should_copy = match fs::metadata(target) {
        Ok(target_metadata) => {
            if target_metadata.is_dir() {
                fs::remove_dir_all(target)
                    .with_context(|| format!("failed to remove `{}`", target.display()))?;
                true
            } else if source_metadata.len() != target_metadata.len() {
                true
            } else {
                match (
                    source_metadata.modified().ok(),
                    target_metadata.modified().ok(),
                ) {
                    (Some(source_modified), Some(target_modified)) => {
                        source_modified > target_modified
                    }
                    _ => false,
                }
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => true,
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read `{}`", target.display()));
        }
    };

    if !should_copy {
        return Ok(());
    }

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }

    fs::copy(source, target).with_context(|| {
        format!(
            "failed to copy `{}` to `{}`",
            source.display(),
            target.display()
        )
    })?;
    fs::set_permissions(target, source_metadata.permissions())
        .with_context(|| format!("failed to update permissions on `{}`", target.display()))?;
    Ok(())
}

#[derive(Clone)]
enum CodexRuntimeCommand {
    Prompt(CodexPromptCommand),
    ApprovalResponse(CodexApprovalResponseCommand),
}

#[derive(Clone)]
struct CodexPromptCommand {
    approval_policy: CodexApprovalPolicy,
    attachments: Vec<PromptImageAttachment>,
    cwd: String,
    prompt: String,
    resume_thread_id: Option<String>,
    sandbox_mode: CodexSandboxMode,
}

#[derive(Clone)]
struct CodexApprovalResponseCommand {
    request_id: Value,
    result: Value,
}

#[derive(Clone)]
enum CodexApprovalKind {
    CommandExecution,
    FileChange,
}

#[derive(Clone)]
struct CodexPendingApproval {
    kind: CodexApprovalKind,
    request_id: Value,
}

#[derive(Clone)]
struct ClaudePromptCommand {
    attachments: Vec<PromptImageAttachment>,
    text: String,
}

#[derive(Clone)]
enum ClaudeRuntimeCommand {
    Prompt(ClaudePromptCommand),
    PermissionResponse(ClaudePermissionDecision),
    SetPermissionMode(String),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PromptImageAttachment {
    data: String,
    metadata: MessageImageAttachment,
}

#[derive(Clone)]
struct ClaudePendingApproval {
    permission_mode_for_session: Option<String>,
    request_id: String,
    tool_input: Value,
}

#[derive(Clone)]
enum ClaudePermissionDecision {
    Allow {
        request_id: String,
        updated_input: Value,
    },
    Deny {
        request_id: String,
        message: String,
    },
}

enum ClaudeControlRequestAction {
    QueueApproval {
        title: String,
        command: String,
        detail: String,
        approval: ClaudePendingApproval,
    },
    Respond(ClaudePermissionDecision),
}

#[derive(Clone)]
struct TurnConfig {
    codex_approval_policy: Option<CodexApprovalPolicy>,
    codex_sandbox_mode: Option<CodexSandboxMode>,
    agent: Agent,
    cwd: String,
    prompt: String,
    external_session_id: Option<String>,
}

enum TurnDispatch {
    PersistentClaude {
        command: ClaudePromptCommand,
        sender: Sender<ClaudeRuntimeCommand>,
        session_id: String,
    },
    PersistentCodex {
        command: CodexPromptCommand,
        sender: Sender<CodexRuntimeCommand>,
        session_id: String,
    },
}

enum DispatchTurnResult {
    Dispatched(TurnDispatch),
    Queued,
}

type CodexPendingRequestMap =
    Arc<Mutex<HashMap<String, Sender<std::result::Result<Value, String>>>>>;

#[derive(Default)]
struct CodexTurnState {
    streamed_agent_message_item_ids: HashSet<String>,
}

fn spawn_codex_runtime(state: AppState, session_id: String) -> Result<CodexRuntimeHandle> {
    let codex_home = prepare_termal_codex_home(&state.default_workdir, &session_id)?;
    let runtime_id = Uuid::new_v4().to_string();
    let mut command = codex_command()?;
    command
        .arg("app-server")
        .env("CODEX_HOME", &codex_home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .context("failed to start Codex app-server")?;
    let stdin = child
        .stdin
        .take()
        .context("failed to capture Codex app-server stdin")?;
    let stdout = child
        .stdout
        .take()
        .context("failed to capture Codex app-server stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("failed to capture Codex app-server stderr")?;
    let process = Arc::new(Mutex::new(child));
    let (input_tx, input_rx) = mpsc::channel::<CodexRuntimeCommand>();
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let current_thread_id = Arc::new(Mutex::new(None::<String>));

    {
        let writer_session_id = session_id.clone();
        let writer_state = state.clone();
        let writer_pending_requests = pending_requests.clone();
        let writer_thread_id = current_thread_id.clone();
        let writer_runtime_token = RuntimeToken::Codex(runtime_id.clone());
        std::thread::spawn(move || {
            let mut stdin = stdin;
            let initialize_result = send_codex_json_rpc_request(
                &mut stdin,
                &writer_pending_requests,
                "initialize",
                json!({
                    "clientInfo": {
                        "name": "termal",
                        "version": env!("CARGO_PKG_VERSION"),
                    }
                }),
                Duration::from_secs(15),
            )
            .and_then(|_| {
                write_codex_json_rpc_message(&mut stdin, &json!({ "method": "initialized" }))
            });

            if let Err(err) = initialize_result {
                let _ = writer_state.handle_runtime_exit_if_matches(
                    &writer_session_id,
                    &writer_runtime_token,
                    Some(&format!(
                        "failed to initialize Codex app-server session: {err:#}"
                    )),
                );
                return;
            }

            while let Ok(command) = input_rx.recv() {
                let command_result = match command {
                    CodexRuntimeCommand::Prompt(prompt) => handle_codex_prompt_command(
                        &mut stdin,
                        &writer_pending_requests,
                        &writer_state,
                        &writer_session_id,
                        &writer_thread_id,
                        prompt,
                    ),
                    CodexRuntimeCommand::ApprovalResponse(response) => {
                        write_codex_json_rpc_message(
                            &mut stdin,
                            &json!({
                                "id": response.request_id,
                                "result": response.result,
                            }),
                        )
                    }
                };

                if let Err(err) = command_result {
                    let _ = writer_state.handle_runtime_exit_if_matches(
                        &writer_session_id,
                        &writer_runtime_token,
                        Some(&format!(
                            "failed to communicate with Codex app-server: {err:#}"
                        )),
                    );
                    break;
                }
            }
        });
    }

    {
        let reader_session_id = session_id.clone();
        let reader_state = state.clone();
        let reader_pending_requests = pending_requests.clone();
        let reader_thread_id = current_thread_id.clone();
        let reader_runtime_token = RuntimeToken::Codex(runtime_id.clone());
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut raw_line = String::new();
            let mut turn_state = CodexTurnState::default();
            let mut recorder =
                SessionRecorder::new(reader_state.clone(), reader_session_id.clone());

            loop {
                raw_line.clear();
                let bytes_read = match reader.read_line(&mut raw_line) {
                    Ok(bytes_read) => bytes_read,
                    Err(err) => {
                        let _ = reader_state.fail_turn_if_runtime_matches(
                            &reader_session_id,
                            &reader_runtime_token,
                            &format!("failed to read stdout from Codex app-server: {err}"),
                        );
                        break;
                    }
                };

                if bytes_read == 0 {
                    break;
                }

                let message: Value = match serde_json::from_str(raw_line.trim_end()) {
                    Ok(message) => message,
                    Err(err) => {
                        let _ = reader_state.fail_turn_if_runtime_matches(
                            &reader_session_id,
                            &reader_runtime_token,
                            &format!("failed to parse Codex app-server JSON line: {err}"),
                        );
                        break;
                    }
                };

                if let Err(err) = handle_codex_app_server_message(
                    &message,
                    &reader_state,
                    &reader_session_id,
                    &reader_runtime_token,
                    &reader_pending_requests,
                    &reader_thread_id,
                    &mut turn_state,
                    &mut recorder,
                ) {
                    let _ = reader_state.fail_turn_if_runtime_matches(
                        &reader_session_id,
                        &reader_runtime_token,
                        &format!("failed to handle Codex app-server event: {err:#}"),
                    );
                    break;
                }
            }

            let _ = recorder.finish_streaming_text();
        });
    }

    {
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                eprintln!("codex stderr> {line}");
            }
        });
    }

    {
        let wait_session_id = session_id.clone();
        let wait_state = state.clone();
        let wait_process = process.clone();
        let wait_runtime_token = RuntimeToken::Codex(runtime_id.clone());
        std::thread::spawn(move || {
            loop {
                let status = {
                    let mut child = wait_process.lock().expect("Codex process mutex poisoned");
                    child.try_wait()
                };

                match status {
                    Ok(Some(status)) if status.success() => {
                        let _ = wait_state.handle_runtime_exit_if_matches(
                            &wait_session_id,
                            &wait_runtime_token,
                            None,
                        );
                        break;
                    }
                    Ok(Some(status)) => {
                        let _ = wait_state.handle_runtime_exit_if_matches(
                            &wait_session_id,
                            &wait_runtime_token,
                            Some(&format!("Codex session exited with status {status}")),
                        );
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                    Err(err) => {
                        let _ = wait_state.handle_runtime_exit_if_matches(
                            &wait_session_id,
                            &wait_runtime_token,
                            Some(&format!("failed waiting for Codex session: {err}")),
                        );
                        break;
                    }
                }
            }
        });
    }

    Ok(CodexRuntimeHandle {
        runtime_id,
        input_tx,
        process,
    })
}

fn handle_codex_prompt_command(
    writer: &mut impl Write,
    pending_requests: &CodexPendingRequestMap,
    state: &AppState,
    session_id: &str,
    current_thread_id: &Arc<Mutex<Option<String>>>,
    command: CodexPromptCommand,
) -> Result<()> {
    let thread_id = {
        let mut slot = current_thread_id
            .lock()
            .expect("Codex thread id mutex poisoned");
        if let Some(thread_id) = slot.clone() {
            thread_id
        } else {
            let result = match command.resume_thread_id.as_deref() {
                Some(thread_id) => send_codex_json_rpc_request(
                    writer,
                    pending_requests,
                    "thread/resume",
                    json!({
                        "threadId": thread_id,
                        "cwd": command.cwd,
                        "sandbox": command.sandbox_mode.as_cli_value(),
                        "approvalPolicy": command.approval_policy.as_cli_value(),
                    }),
                    Duration::from_secs(30),
                )?,
                None => send_codex_json_rpc_request(
                    writer,
                    pending_requests,
                    "thread/start",
                    json!({
                        "cwd": command.cwd,
                        "sandbox": command.sandbox_mode.as_cli_value(),
                        "approvalPolicy": command.approval_policy.as_cli_value(),
                        "personality": "pragmatic",
                    }),
                    Duration::from_secs(30),
                )?,
            };

            let thread_id = result
                .pointer("/thread/id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("Codex app-server did not return a thread id"))?
                .to_owned();
            state.set_external_session_id(session_id, thread_id.clone())?;
            *slot = Some(thread_id.clone());
            thread_id
        }
    };

    state.record_codex_runtime_config(session_id, command.sandbox_mode, command.approval_policy)?;

    send_codex_json_rpc_request(
        writer,
        pending_requests,
        "turn/start",
        json!({
            "threadId": thread_id,
            "cwd": command.cwd,
            "approvalPolicy": command.approval_policy.as_cli_value(),
            "sandboxPolicy": codex_sandbox_policy_value(command.sandbox_mode),
            "input": codex_user_input_items(&command.prompt, &command.attachments),
        }),
        Duration::from_secs(30),
    )?;

    Ok(())
}

fn handle_codex_app_server_message(
    message: &Value,
    state: &AppState,
    session_id: &str,
    runtime_token: &RuntimeToken,
    pending_requests: &CodexPendingRequestMap,
    current_thread_id: &Arc<Mutex<Option<String>>>,
    turn_state: &mut CodexTurnState,
    recorder: &mut SessionRecorder,
) -> Result<()> {
    if let Some(response_id) = message.get("id") {
        if message.get("result").is_some() || message.get("error").is_some() {
            let key = codex_request_id_key(response_id);
            let sender = pending_requests
                .lock()
                .expect("Codex pending requests mutex poisoned")
                .remove(&key);
            if let Some(sender) = sender {
                let response = if let Some(result) = message.get("result") {
                    Ok(result.clone())
                } else {
                    Err(summarize_codex_json_rpc_error(
                        message.get("error").unwrap_or(&Value::Null),
                    ))
                };
                let _ = sender.send(response);
            }
            return Ok(());
        }
    }

    let Some(method) = message.get("method").and_then(Value::as_str) else {
        log_unhandled_codex_event("Codex app-server message missing method", message);
        return Ok(());
    };

    if message.get("id").is_some() {
        return handle_codex_app_server_request(method, message, recorder);
    }

    handle_codex_app_server_notification(
        method,
        message,
        state,
        session_id,
        runtime_token,
        current_thread_id,
        turn_state,
        recorder,
    )
}

fn handle_codex_app_server_request(
    method: &str,
    message: &Value,
    recorder: &mut SessionRecorder,
) -> Result<()> {
    let request_id = message
        .get("id")
        .cloned()
        .ok_or_else(|| anyhow!("Codex app-server request missing id"))?;
    let params = message
        .get("params")
        .ok_or_else(|| anyhow!("Codex app-server request missing params"))?;

    match method {
        "item/commandExecution/requestApproval" => {
            let command = params
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("Command execution");
            let cwd = params.get("cwd").and_then(Value::as_str).unwrap_or("");
            let reason = params.get("reason").and_then(Value::as_str).unwrap_or("");
            let detail = if cwd.is_empty() && reason.is_empty() {
                "Codex requested approval to execute a command.".to_owned()
            } else if reason.is_empty() {
                format!("Codex requested approval to execute this command in {cwd}.")
            } else if cwd.is_empty() {
                format!("Codex requested approval to execute this command. Reason: {reason}")
            } else {
                format!(
                    "Codex requested approval to execute this command in {cwd}. Reason: {reason}"
                )
            };

            recorder.push_codex_approval(
                "Codex needs approval",
                command,
                &detail,
                CodexPendingApproval {
                    kind: CodexApprovalKind::CommandExecution,
                    request_id,
                },
            )?;
        }
        "item/fileChange/requestApproval" => {
            let reason = params.get("reason").and_then(Value::as_str).unwrap_or("");
            let detail = if reason.is_empty() {
                "Codex requested approval to apply file changes.".to_owned()
            } else {
                format!("Codex requested approval to apply file changes. Reason: {reason}")
            };

            recorder.push_codex_approval(
                "Codex needs approval",
                "Apply file changes",
                &detail,
                CodexPendingApproval {
                    kind: CodexApprovalKind::FileChange,
                    request_id,
                },
            )?;
        }
        _ => {
            log_unhandled_codex_event(
                &format!("unhandled Codex app-server request `{method}`"),
                message,
            );
        }
    }

    Ok(())
}

fn handle_codex_app_server_notification(
    method: &str,
    message: &Value,
    state: &AppState,
    session_id: &str,
    runtime_token: &RuntimeToken,
    current_thread_id: &Arc<Mutex<Option<String>>>,
    turn_state: &mut CodexTurnState,
    recorder: &mut SessionRecorder,
) -> Result<()> {
    match method {
        "thread/started" => {
            if let Some(thread_id) = message.pointer("/params/thread/id").and_then(Value::as_str) {
                *current_thread_id
                    .lock()
                    .expect("Codex thread id mutex poisoned") = Some(thread_id.to_owned());
                state.set_external_session_id(session_id, thread_id.to_owned())?;
                recorder.note_external_session(thread_id)?;
            }
        }
        "turn/started" => {
            turn_state.streamed_agent_message_item_ids.clear();
            recorder.finish_streaming_text()?;
        }
        "turn/completed" => {
            recorder.finish_streaming_text()?;
            if let Some(error) = message.pointer("/params/turn/error") {
                if !error.is_null() {
                    state.fail_turn_if_runtime_matches(
                        session_id,
                        runtime_token,
                        &summarize_error(error),
                    )?;
                    return Ok(());
                }
            }
            state.finish_turn_ok_if_runtime_matches(session_id, runtime_token)?;
        }
        "item/started" => {
            if let Some(item) = message.get("params").and_then(|params| params.get("item")) {
                handle_codex_app_server_item_started(item, recorder)?;
            }
        }
        "item/completed" => {
            if let Some(item) = message.get("params").and_then(|params| params.get("item")) {
                handle_codex_app_server_item_completed(item, turn_state, recorder)?;
            }
        }
        "item/agentMessage/delta" => {
            let Some(delta) = message.pointer("/params/delta").and_then(Value::as_str) else {
                return Ok(());
            };
            let Some(item_id) = message.pointer("/params/itemId").and_then(Value::as_str) else {
                return Ok(());
            };
            turn_state
                .streamed_agent_message_item_ids
                .insert(item_id.to_owned());
            recorder.text_delta(delta)?;
        }
        "account/rateLimits/updated" => {
            let Some(rate_limits) = message.pointer("/params/rateLimits") else {
                log_unhandled_codex_event(
                    "Codex rate limit notification missing params.rateLimits",
                    message,
                );
                return Ok(());
            };

            match serde_json::from_value::<CodexRateLimits>(rate_limits.clone()) {
                Ok(rate_limits) => state.note_codex_rate_limits(rate_limits)?,
                Err(err) => {
                    log_unhandled_codex_event(
                        &format!("failed to parse Codex rate limits notification: {err}"),
                        message,
                    );
                }
            }
        }
        "thread/status/changed"
        | "turn/diff/updated"
        | "turn/plan/updated"
        | "item/commandExecution/outputDelta"
        | "item/commandExecution/terminalInteraction"
        | "item/fileChange/outputDelta"
        | "item/plan/delta"
        | "item/reasoning/summaryTextDelta"
        | "item/reasoning/summaryPartAdded"
        | "item/reasoning/textDelta"
        | "thread/tokenUsage/updated"
        | "thread/name/updated"
        | "thread/closed"
        | "thread/archived"
        | "thread/unarchived"
        | "thread/compacted"
        | "thread/realtime/started"
        | "thread/realtime/itemAdded"
        | "thread/realtime/outputAudio/delta"
        | "thread/realtime/error"
        | "thread/realtime/closed" => {}
        "error" => {
            let payload = message.get("params").unwrap_or(message);
            let detail = summarize_error(payload);

            if is_retryable_connectivity_error(payload) {
                state.note_turn_retry_if_runtime_matches(session_id, runtime_token, &detail)?;
            } else {
                state.fail_turn_if_runtime_matches(session_id, runtime_token, &detail)?;
            }
        }
        _ if method.starts_with("codex/event/") => {}
        _ => {
            log_unhandled_codex_event(
                &format!("unhandled Codex app-server notification `{method}`"),
                message,
            );
        }
    }

    Ok(())
}

fn handle_codex_app_server_item_started(
    item: &Value,
    recorder: &mut SessionRecorder,
) -> Result<()> {
    match item.get("type").and_then(Value::as_str) {
        Some("agentMessage") => {
            recorder.finish_streaming_text()?;
        }
        Some("commandExecution") => {
            if let Some(command) = item.get("command").and_then(Value::as_str) {
                let key = item.get("id").and_then(Value::as_str).unwrap_or(command);
                recorder.command_started(key, command)?;
            }
        }
        Some("webSearch") => {
            let key = item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("webSearch");
            let command = describe_codex_app_server_web_search_command(item);
            recorder.command_started(key, &command)?;
        }
        _ => {}
    }

    Ok(())
}

fn handle_codex_app_server_item_completed(
    item: &Value,
    turn_state: &CodexTurnState,
    recorder: &mut SessionRecorder,
) -> Result<()> {
    match item.get("type").and_then(Value::as_str) {
        Some("agentMessage") => {
            let item_id = item.get("id").and_then(Value::as_str).unwrap_or("");
            if !turn_state.streamed_agent_message_item_ids.contains(item_id) {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    recorder.push_text(text)?;
                }
            }
        }
        Some("commandExecution") => {
            if let Some(command) = item.get("command").and_then(Value::as_str) {
                let key = item.get("id").and_then(Value::as_str).unwrap_or(command);
                let output = item
                    .get("aggregatedOutput")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let status = match item.get("status").and_then(Value::as_str) {
                    Some("completed")
                        if item.get("exitCode").and_then(Value::as_i64) == Some(0) =>
                    {
                        CommandStatus::Success
                    }
                    Some("completed") => CommandStatus::Error,
                    Some("failed") | Some("declined") => CommandStatus::Error,
                    _ => CommandStatus::Running,
                };
                recorder.command_completed(key, command, output, status)?;
            }
        }
        Some("fileChange") => {
            if item.get("status").and_then(Value::as_str) != Some("completed") {
                return Ok(());
            }
            let Some(changes) = item.get("changes").and_then(Value::as_array) else {
                return Ok(());
            };
            for change in changes {
                let Some(file_path) = change.get("path").and_then(Value::as_str) else {
                    continue;
                };
                let diff = change.get("diff").and_then(Value::as_str).unwrap_or("");
                if diff.trim().is_empty() {
                    continue;
                }
                let change_type = match change.pointer("/kind/type").and_then(Value::as_str) {
                    Some("add") => ChangeType::Create,
                    _ => ChangeType::Edit,
                };
                let summary = match change_type {
                    ChangeType::Create => format!("Created {}", short_file_name(file_path)),
                    ChangeType::Edit => format!("Updated {}", short_file_name(file_path)),
                };
                recorder.push_diff(file_path, &summary, diff, change_type)?;
            }
        }
        Some("webSearch") => {
            let key = item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("webSearch");
            let command = describe_codex_app_server_web_search_command(item);
            let output = summarize_codex_app_server_web_search_output(item);
            recorder.command_completed(key, &command, &output, CommandStatus::Success)?;
        }
        _ => {}
    }

    Ok(())
}

fn send_codex_json_rpc_request(
    writer: &mut impl Write,
    pending_requests: &CodexPendingRequestMap,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value> {
    let request_id = Uuid::new_v4().to_string();
    let (tx, rx) = mpsc::channel();
    pending_requests
        .lock()
        .expect("Codex pending requests mutex poisoned")
        .insert(request_id.clone(), tx);

    if let Err(err) = write_codex_json_rpc_message(
        writer,
        &json!({
            "id": request_id,
            "method": method,
            "params": params,
        }),
    ) {
        pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned")
            .remove(&request_id);
        return Err(err);
    }

    match rx.recv_timeout(timeout) {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(err)) => Err(anyhow!(err)),
        Err(err) => {
            pending_requests
                .lock()
                .expect("Codex pending requests mutex poisoned")
                .remove(&request_id);
            Err(anyhow!(
                "timed out waiting for Codex app-server response to `{method}`: {err}"
            ))
        }
    }
}

fn write_codex_json_rpc_message(writer: &mut impl Write, message: &Value) -> Result<()> {
    serde_json::to_writer(&mut *writer, message)
        .context("failed to encode Codex app-server message")?;
    writer
        .write_all(b"\n")
        .context("failed to write Codex app-server message delimiter")?;
    writer
        .flush()
        .context("failed to flush Codex app-server stdin")
}

fn codex_request_id_key(id: &Value) -> String {
    id.as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| id.to_string())
}

fn summarize_codex_json_rpc_error(error: &Value) -> String {
    if let Some(message) = error.get("message").and_then(Value::as_str) {
        return message.to_owned();
    }

    summarize_error(error)
}

fn codex_sandbox_policy_value(mode: CodexSandboxMode) -> Value {
    match mode {
        CodexSandboxMode::ReadOnly => json!({
            "type": "readOnly",
        }),
        CodexSandboxMode::WorkspaceWrite => json!({
            "type": "workspaceWrite",
        }),
        CodexSandboxMode::DangerFullAccess => json!({
            "type": "dangerFullAccess",
        }),
    }
}

fn codex_command() -> Result<Command> {
    let exe = resolve_codex_executable()?;

    // On Windows, .cmd/.bat shims (from npm) must be run through cmd.exe.
    #[cfg(windows)]
    {
        if let Some(ext) = exe.extension().and_then(|e| e.to_str()) {
            if ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat") {
                let mut cmd = Command::new("cmd.exe");
                cmd.args(["/C", &exe.to_string_lossy()]);
                return Ok(cmd);
            }
        }
    }

    Ok(Command::new(exe))
}

fn resolve_codex_executable() -> Result<PathBuf> {
    let launcher =
        find_command_on_path("codex").ok_or_else(|| anyhow!("`codex` was not found on PATH"))?;
    Ok(resolve_codex_native_binary(&launcher).unwrap_or(launcher))
}

fn find_command_on_path(command: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;

    #[cfg(windows)]
    let extensions: &[&str] = &[".exe", ".cmd", ".bat", ""];

    #[cfg(not(windows))]
    let extensions: &[&str] = &[""];

    for dir in std::env::split_paths(&path) {
        for ext in extensions {
            let candidate = dir.join(format!("{command}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn resolve_codex_native_binary(launcher: &PathBuf) -> Option<PathBuf> {
    let launcher = fs::canonicalize(launcher)
        .ok()
        .unwrap_or_else(|| launcher.clone());
    let package_root = launcher.parent()?.parent()?;
    let node_modules_dir = package_root.join("node_modules").join("@openai");
    let target_triple = codex_target_triple()?;
    let binary_name = if cfg!(windows) { "codex.exe" } else { "codex" };

    let entries = fs::read_dir(node_modules_dir).ok()?;
    for entry in entries {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let name = name.to_str()?;
        if !name.starts_with("codex-") {
            continue;
        }
        let candidate = entry
            .path()
            .join("vendor")
            .join(target_triple)
            .join("codex")
            .join(binary_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

fn codex_target_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-musl"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        ("windows", "aarch64") => Some("aarch64-pc-windows-msvc"),
        _ => None,
    }
}

fn describe_codex_app_server_web_search_command(item: &Value) -> String {
    let query = item
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());

    match item.pointer("/action/type").and_then(Value::as_str) {
        Some("open_page") => item
            .pointer("/action/url")
            .and_then(Value::as_str)
            .map(|url| format!("Open page: {url}"))
            .unwrap_or_else(|| "Open page".to_owned()),
        Some("find_in_page") => item
            .pointer("/action/pattern")
            .and_then(Value::as_str)
            .map(|pattern| format!("Find in page: {pattern}"))
            .unwrap_or_else(|| "Find in page".to_owned()),
        _ => query
            .map(|value| format!("Web search: {value}"))
            .unwrap_or_else(|| "Web search".to_owned()),
    }
}

fn summarize_codex_app_server_web_search_output(item: &Value) -> String {
    match item.pointer("/action/type").and_then(Value::as_str) {
        Some("search") => {
            let queries = item
                .pointer("/action/queries")
                .and_then(Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if !queries.is_empty() {
                return queries.join("\n");
            }
        }
        Some("open_page") => {
            if let Some(url) = item.pointer("/action/url").and_then(Value::as_str) {
                return format!("Opened {url}");
            }
        }
        Some("find_in_page") => {
            let pattern = item.pointer("/action/pattern").and_then(Value::as_str);
            let url = item.pointer("/action/url").and_then(Value::as_str);
            return match (pattern, url) {
                (Some(pattern), Some(url)) => format!("Searched for `{pattern}` in {url}"),
                (Some(pattern), None) => format!("Searched for `{pattern}`"),
                (None, Some(url)) => format!("Searched within {url}"),
                (None, None) => "Find in page completed".to_owned(),
            };
        }
        _ => {}
    }

    item.get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Web search completed")
        .to_owned()
}

fn spawn_claude_runtime(
    state: AppState,
    session_id: String,
    cwd: String,
    resume_session_id: Option<String>,
) -> Result<ClaudeRuntimeHandle> {
    let runtime_id = Uuid::new_v4().to_string();
    let mut command = Command::new("claude");
    command.current_dir(&cwd).args([
        "-p",
        "--verbose",
        "--output-format",
        "stream-json",
        "--input-format",
        "stream-json",
        "--include-partial-messages",
        "--permission-prompt-tool",
        "stdio",
    ]);
    command.env("CLAUDE_CODE_ENTRYPOINT", "termal");
    if let Some(resume_session_id) = resume_session_id {
        command.args(["--resume", &resume_session_id]);
    }

    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start Claude in `{cwd}`"))?;

    let stdin = child
        .stdin
        .take()
        .context("failed to capture Claude stdin")?;
    let stdout = child
        .stdout
        .take()
        .context("failed to capture Claude stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("failed to capture Claude stderr")?;
    let process = Arc::new(Mutex::new(child));

    let (input_tx, input_rx) = mpsc::channel::<ClaudeRuntimeCommand>();

    {
        let writer_session_id = session_id.clone();
        let writer_state = state.clone();
        let writer_runtime_token = RuntimeToken::Claude(runtime_id.clone());
        std::thread::spawn(move || {
            let mut stdin = stdin;
            if let Err(err) = write_claude_initialize(&mut stdin) {
                let _ = writer_state.handle_runtime_exit_if_matches(
                    &writer_session_id,
                    &writer_runtime_token,
                    Some(&format!("failed to initialize Claude session: {err:#}")),
                );
                return;
            }

            while let Ok(command) = input_rx.recv() {
                let write_result = match command {
                    ClaudeRuntimeCommand::Prompt(prompt) => {
                        write_claude_prompt_message(&mut stdin, &prompt)
                    }
                    ClaudeRuntimeCommand::PermissionResponse(decision) => {
                        write_claude_permission_response(&mut stdin, &decision)
                    }
                    ClaudeRuntimeCommand::SetPermissionMode(mode) => {
                        write_claude_set_permission_mode(&mut stdin, &mode)
                    }
                };

                if let Err(err) = write_result {
                    let _ = writer_state.handle_runtime_exit_if_matches(
                        &writer_session_id,
                        &writer_runtime_token,
                        Some(&format!("failed to write prompt to Claude stdin: {err:#}")),
                    );
                    break;
                }
            }
        });
    }

    {
        let reader_session_id = session_id.clone();
        let reader_state = state.clone();
        let reader_input_tx = input_tx.clone();
        let reader_runtime_token = RuntimeToken::Claude(runtime_id.clone());
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut raw_line = String::new();
            let mut turn_state = ClaudeTurnState::default();
            let mut recorder =
                SessionRecorder::new(reader_state.clone(), reader_session_id.clone());
            let mut resolved_session_id: Option<String> = None;

            loop {
                raw_line.clear();
                let bytes_read = match reader.read_line(&mut raw_line) {
                    Ok(bytes_read) => bytes_read,
                    Err(err) => {
                        let _ = reader_state.fail_turn_if_runtime_matches(
                            &reader_session_id,
                            &reader_runtime_token,
                            &format!("failed to read stdout from Claude: {err}"),
                        );
                        break;
                    }
                };

                if bytes_read == 0 {
                    break;
                }

                let message: Value = match serde_json::from_str(raw_line.trim_end()) {
                    Ok(message) => message,
                    Err(err) => {
                        let _ = reader_state.fail_turn_if_runtime_matches(
                            &reader_session_id,
                            &reader_runtime_token,
                            &format!("failed to parse Claude JSON line: {err}"),
                        );
                        break;
                    }
                };

                let message_type = message.get("type").and_then(Value::as_str);
                let is_result = message.get("type").and_then(Value::as_str) == Some("result");
                let is_error = message
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let error_summary = is_result.then(|| summarize_error(&message));

                if message_type == Some("control_request") {
                    let approval_mode = match reader_state.claude_approval_mode(&reader_session_id)
                    {
                        Ok(mode) => mode,
                        Err(err) => {
                            let _ = reader_state.fail_turn_if_runtime_matches(
                                &reader_session_id,
                                &reader_runtime_token,
                                &format!(
                                    "failed to resolve Claude approval mode for session: {err:#}"
                                ),
                            );
                            break;
                        }
                    };

                    let action = match classify_claude_control_request(
                        &message,
                        &mut turn_state,
                        approval_mode,
                    ) {
                        Ok(action) => action,
                        Err(err) => {
                            let _ = reader_state.fail_turn_if_runtime_matches(
                                &reader_session_id,
                                &reader_runtime_token,
                                &format!("failed to handle Claude control request: {err:#}"),
                            );
                            break;
                        }
                    };

                    if let Some(action) = action {
                        let action_result = match action {
                            ClaudeControlRequestAction::QueueApproval {
                                title,
                                command,
                                detail,
                                approval,
                            } => recorder.push_claude_approval(&title, &command, &detail, approval),
                            ClaudeControlRequestAction::Respond(decision) => reader_input_tx
                                .send(ClaudeRuntimeCommand::PermissionResponse(decision))
                                .map_err(|err| {
                                    anyhow!("failed to auto-approve Claude tool request: {err}")
                                }),
                        };

                        if let Err(err) = action_result {
                            let _ = reader_state.fail_turn_if_runtime_matches(
                                &reader_session_id,
                                &reader_runtime_token,
                                &format!("failed to handle Claude control request: {err:#}"),
                            );
                            break;
                        }
                    }
                    continue;
                } else if message_type == Some("control_cancel_request") {
                    if let Some(request_id) = message.get("request_id").and_then(Value::as_str) {
                        let _ = reader_state.clear_claude_pending_approval_by_request(
                            &reader_session_id,
                            request_id,
                        );
                    }
                    continue;
                }

                if let Err(err) = handle_claude_event(
                    &message,
                    &mut resolved_session_id,
                    &mut turn_state,
                    &mut recorder,
                ) {
                    let _ = reader_state.fail_turn_if_runtime_matches(
                        &reader_session_id,
                        &reader_runtime_token,
                        &format!("failed to handle Claude event: {err:#}"),
                    );
                    break;
                }

                if is_result {
                    if is_error {
                        if let Some(detail) = error_summary.as_deref() {
                            let _ = reader_state.mark_turn_error_if_runtime_matches(
                                &reader_session_id,
                                &reader_runtime_token,
                                detail,
                            );
                        }
                    } else {
                        let _ = reader_state.finish_turn_ok_if_runtime_matches(
                            &reader_session_id,
                            &reader_runtime_token,
                        );
                    }
                }
            }

            let _ = recorder.finish_streaming_text();
        });
    }

    {
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                eprintln!("claude stderr> {line}");
            }
        });
    }

    {
        let wait_session_id = session_id.clone();
        let wait_state = state.clone();
        let wait_process = process.clone();
        let wait_runtime_token = RuntimeToken::Claude(runtime_id.clone());
        std::thread::spawn(move || {
            loop {
                let status = {
                    let mut child = wait_process.lock().expect("Claude process mutex poisoned");
                    child.try_wait()
                };

                match status {
                    Ok(Some(status)) if status.success() => {
                        let _ = wait_state.handle_runtime_exit_if_matches(
                            &wait_session_id,
                            &wait_runtime_token,
                            None,
                        );
                        break;
                    }
                    Ok(Some(status)) => {
                        let _ = wait_state.handle_runtime_exit_if_matches(
                            &wait_session_id,
                            &wait_runtime_token,
                            Some(&format!("Claude session exited with status {status}")),
                        );
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                    Err(err) => {
                        let _ = wait_state.handle_runtime_exit_if_matches(
                            &wait_session_id,
                            &wait_runtime_token,
                            Some(&format!("failed waiting for Claude session: {err}")),
                        );
                        break;
                    }
                }
            }
        });
    }

    Ok(ClaudeRuntimeHandle {
        runtime_id,
        input_tx,
        process,
    })
}

fn write_claude_initialize(writer: &mut impl Write) -> Result<()> {
    write_claude_message(
        writer,
        &json!({
            "request_id": Uuid::new_v4().to_string(),
            "type": "control_request",
            "request": {
                "subtype": "initialize",
                "hooks": {},
                "systemPrompt": "",
                "appendSystemPrompt": "",
            }
        }),
    )
}

fn write_claude_prompt_message(
    writer: &mut impl Write,
    prompt: &ClaudePromptCommand,
) -> Result<()> {
    let mut content = Vec::new();
    if !prompt.text.trim().is_empty() {
        content.push(json!({
            "type": "text",
            "text": prompt.text.as_str(),
        }));
    }
    for attachment in &prompt.attachments {
        content.push(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": attachment.metadata.media_type.as_str(),
                "data": attachment.data.as_str(),
            }
        }));
    }

    write_claude_message(
        writer,
        &json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": content,
            }
        }),
    )
}

fn write_claude_permission_response(
    writer: &mut impl Write,
    decision: &ClaudePermissionDecision,
) -> Result<()> {
    let message = match decision {
        ClaudePermissionDecision::Allow {
            request_id,
            updated_input,
        } => json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": request_id,
                "response": {
                    "behavior": "allow",
                    "updatedInput": updated_input,
                }
            }
        }),
        ClaudePermissionDecision::Deny {
            request_id,
            message,
        } => json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": request_id,
                "response": {
                    "behavior": "deny",
                    "message": message,
                }
            }
        }),
    };

    write_claude_message(writer, &message)
}

fn write_claude_set_permission_mode(writer: &mut impl Write, mode: &str) -> Result<()> {
    write_claude_message(
        writer,
        &json!({
            "request_id": Uuid::new_v4().to_string(),
            "type": "control_request",
            "request": {
                "subtype": "set_permission_mode",
                "mode": mode,
            }
        }),
    )
}

fn write_claude_message(writer: &mut impl Write, message: &Value) -> Result<()> {
    serde_json::to_writer(&mut *writer, message).context("failed to encode Claude message")?;
    writer
        .write_all(b"\n")
        .context("failed to write Claude message delimiter")?;
    writer.flush().context("failed to flush Claude stdin")
}

fn run_turn_blocking(config: TurnConfig, recorder: &mut dyn TurnRecorder) -> Result<String> {
    match config.agent {
        Agent::Codex => run_codex_turn(
            None,
            None,
            &config.cwd,
            config.external_session_id.as_deref(),
            config
                .codex_sandbox_mode
                .unwrap_or_else(default_codex_sandbox_mode),
            config
                .codex_approval_policy
                .unwrap_or_else(default_codex_approval_policy),
            &config.prompt,
            recorder,
        ),
        Agent::Claude => run_claude_turn(
            &config.cwd,
            config.external_session_id.as_deref(),
            &config.prompt,
            recorder,
        ),
    }
}

trait TurnRecorder {
    fn note_external_session(&mut self, session_id: &str) -> Result<()>;
    fn push_approval(&mut self, title: &str, command: &str, detail: &str) -> Result<()>;
    fn push_text(&mut self, text: &str) -> Result<()>;
    fn push_thinking(&mut self, title: &str, lines: Vec<String>) -> Result<()>;
    fn push_diff(
        &mut self,
        file_path: &str,
        summary: &str,
        diff: &str,
        change_type: ChangeType,
    ) -> Result<()>;
    fn text_delta(&mut self, delta: &str) -> Result<()>;
    fn finish_streaming_text(&mut self) -> Result<()>;
    fn command_started(&mut self, key: &str, command: &str) -> Result<()>;
    fn command_completed(
        &mut self,
        key: &str,
        command: &str,
        output: &str,
        status: CommandStatus,
    ) -> Result<()>;
    fn error(&mut self, detail: &str) -> Result<()>;
}

struct SessionRecorder {
    command_messages: HashMap<String, String>,
    session_id: String,
    state: AppState,
    streaming_text_message_id: Option<String>,
}

impl SessionRecorder {
    fn new(state: AppState, session_id: String) -> Self {
        Self {
            command_messages: HashMap::new(),
            session_id,
            state,
            streaming_text_message_id: None,
        }
    }

    fn push_claude_approval(
        &mut self,
        title: &str,
        command: &str,
        detail: &str,
        approval: ClaudePendingApproval,
    ) -> Result<()> {
        self.finish_streaming_text()?;
        let message_id = self.state.allocate_message_id();
        self.state.push_message(
            &self.session_id,
            Message::Approval {
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: title.to_owned(),
                command: command.to_owned(),
                command_language: Some(shell_language().to_owned()),
                detail: detail.to_owned(),
                decision: ApprovalDecision::Pending,
            },
        )?;
        self.state
            .register_claude_pending_approval(&self.session_id, message_id, approval)
    }

    fn push_codex_approval(
        &mut self,
        title: &str,
        command: &str,
        detail: &str,
        approval: CodexPendingApproval,
    ) -> Result<()> {
        self.finish_streaming_text()?;
        let message_id = self.state.allocate_message_id();
        self.state.push_message(
            &self.session_id,
            Message::Approval {
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: title.to_owned(),
                command: command.to_owned(),
                command_language: Some(shell_language().to_owned()),
                detail: detail.to_owned(),
                decision: ApprovalDecision::Pending,
            },
        )?;
        self.state
            .register_codex_pending_approval(&self.session_id, message_id, approval)
    }
}

impl TurnRecorder for SessionRecorder {
    fn note_external_session(&mut self, session_id: &str) -> Result<()> {
        self.state
            .set_external_session_id(&self.session_id, session_id.to_owned())
    }

    fn push_approval(&mut self, title: &str, command: &str, detail: &str) -> Result<()> {
        self.finish_streaming_text()?;
        self.state.push_message(
            &self.session_id,
            Message::Approval {
                id: self.state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: title.to_owned(),
                command: command.to_owned(),
                command_language: Some(shell_language().to_owned()),
                detail: detail.to_owned(),
                decision: ApprovalDecision::Pending,
            },
        )
    }

    fn push_text(&mut self, text: &str) -> Result<()> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(());
        }

        self.finish_streaming_text()?;
        self.state.push_message(
            &self.session_id,
            Message::Text {
                attachments: Vec::new(),
                id: self.state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: trimmed.to_owned(),
            },
        )
    }

    fn text_delta(&mut self, delta: &str) -> Result<()> {
        if delta.is_empty() {
            return Ok(());
        }

        let message_id = match &self.streaming_text_message_id {
            Some(message_id) => message_id.clone(),
            None => {
                let message_id = self.state.allocate_message_id();
                self.state.push_message(
                    &self.session_id,
                    Message::Text {
                        attachments: Vec::new(),
                        id: message_id.clone(),
                        timestamp: stamp_now(),
                        author: Author::Assistant,
                        text: String::new(),
                    },
                )?;
                self.streaming_text_message_id = Some(message_id.clone());
                message_id
            }
        };

        self.state
            .append_text_delta(&self.session_id, &message_id, delta)
    }

    fn push_thinking(&mut self, title: &str, lines: Vec<String>) -> Result<()> {
        if lines.is_empty() {
            return Ok(());
        }

        self.finish_streaming_text()?;
        self.state.push_message(
            &self.session_id,
            Message::Thinking {
                id: self.state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: title.to_owned(),
                lines,
            },
        )
    }

    fn push_diff(
        &mut self,
        file_path: &str,
        summary: &str,
        diff: &str,
        change_type: ChangeType,
    ) -> Result<()> {
        if diff.trim().is_empty() {
            return Ok(());
        }

        self.finish_streaming_text()?;
        self.state.push_message(
            &self.session_id,
            Message::Diff {
                id: self.state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                file_path: file_path.to_owned(),
                summary: summary.to_owned(),
                diff: diff.to_owned(),
                language: Some("diff".to_owned()),
                change_type,
            },
        )
    }

    fn finish_streaming_text(&mut self) -> Result<()> {
        self.streaming_text_message_id = None;
        Ok(())
    }

    fn command_started(&mut self, key: &str, command: &str) -> Result<()> {
        let message_id = self
            .command_messages
            .entry(key.to_owned())
            .or_insert_with(|| self.state.allocate_message_id())
            .clone();

        self.state.upsert_command_message(
            &self.session_id,
            &message_id,
            command,
            "",
            CommandStatus::Running,
        )
    }

    fn command_completed(
        &mut self,
        key: &str,
        command: &str,
        output: &str,
        status: CommandStatus,
    ) -> Result<()> {
        let message_id = self
            .command_messages
            .entry(key.to_owned())
            .or_insert_with(|| self.state.allocate_message_id())
            .clone();

        self.state
            .upsert_command_message(&self.session_id, &message_id, command, output, status)
    }

    fn error(&mut self, detail: &str) -> Result<()> {
        let cleaned = detail.trim();
        if cleaned.is_empty() {
            return Ok(());
        }

        self.finish_streaming_text()?;
        self.state.push_message(
            &self.session_id,
            Message::Text {
                attachments: Vec::new(),
                id: self.state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: format!("Error: {cleaned}"),
            },
        )
    }
}

#[derive(Default)]
struct ReplPrinter {
    assistant_stream_open: bool,
}

impl TurnRecorder for ReplPrinter {
    fn note_external_session(&mut self, session_id: &str) -> Result<()> {
        println!("session> {session_id}");
        Ok(())
    }

    fn push_approval(&mut self, title: &str, command: &str, detail: &str) -> Result<()> {
        println!("approval> {title}");
        println!("approval> {command}");
        println!("approval> {detail}");
        Ok(())
    }

    fn push_text(&mut self, text: &str) -> Result<()> {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            println!("assistant> {trimmed}");
        }
        Ok(())
    }

    fn text_delta(&mut self, delta: &str) -> Result<()> {
        if delta.is_empty() {
            return Ok(());
        }

        if !self.assistant_stream_open {
            print!("assistant> ");
            self.assistant_stream_open = true;
        }
        print!("{delta}");
        io::stdout().flush().context("failed to flush stdout")?;
        Ok(())
    }

    fn push_thinking(&mut self, title: &str, lines: Vec<String>) -> Result<()> {
        println!("thinking> {title}");
        for line in lines {
            println!("- {line}");
        }
        Ok(())
    }

    fn push_diff(
        &mut self,
        file_path: &str,
        summary: &str,
        diff: &str,
        change_type: ChangeType,
    ) -> Result<()> {
        let label = match change_type {
            ChangeType::Edit => "edit",
            ChangeType::Create => "create",
        };
        println!("diff> {label} {file_path}");
        println!("{summary}");
        println!("{diff}");
        Ok(())
    }

    fn finish_streaming_text(&mut self) -> Result<()> {
        if self.assistant_stream_open {
            println!();
            self.assistant_stream_open = false;
        }
        Ok(())
    }

    fn command_started(&mut self, _key: &str, command: &str) -> Result<()> {
        println!("cmd> {command}");
        Ok(())
    }

    fn command_completed(
        &mut self,
        _key: &str,
        command: &str,
        output: &str,
        status: CommandStatus,
    ) -> Result<()> {
        println!("cmd> completed `{command}` ({})", status.label());
        if !output.trim().is_empty() {
            println!("{output}");
        }
        Ok(())
    }

    fn error(&mut self, detail: &str) -> Result<()> {
        println!("error> {detail}");
        Ok(())
    }
}

fn run_codex_turn(
    state: Option<&AppState>,
    runtime_session_id: Option<&str>,
    cwd: &str,
    external_session_id: Option<&str>,
    sandbox_mode: CodexSandboxMode,
    approval_policy: CodexApprovalPolicy,
    prompt: &str,
    recorder: &mut dyn TurnRecorder,
) -> Result<String> {
    let codex_home = prepare_termal_codex_home(cwd, runtime_session_id.unwrap_or("repl"))?;
    let mut command = codex_command()?;
    command.env("CODEX_HOME", &codex_home);

    match external_session_id {
        Some(session_id) => {
            command.args([
                "-a",
                approval_policy.as_cli_value(),
                "exec",
                "resume",
                "--json",
                session_id,
                "-",
            ]);
        }
        None => {
            command.args([
                "-a",
                approval_policy.as_cli_value(),
                "exec",
                "-s",
                sandbox_mode.as_cli_value(),
                "--json",
                "-C",
                cwd,
                "-",
            ]);
        }
    }

    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start Codex")?;

    let mut child_stdin = child
        .stdin
        .take()
        .context("failed to capture child stdin")?;
    writeln!(child_stdin, "{prompt}").context("failed to write prompt to Codex stdin")?;
    drop(child_stdin);

    let stdout = child
        .stdout
        .take()
        .context("failed to capture child stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("failed to capture child stderr")?;
    let process = Arc::new(Mutex::new(child));
    if let (Some(state), Some(runtime_session_id)) = (state, runtime_session_id) {
        let (input_tx, _input_rx) = mpsc::channel();
        let runtime = CodexRuntimeHandle {
            runtime_id: Uuid::new_v4().to_string(),
            input_tx,
            process: process.clone(),
        };

        if let Err(err) = state.set_codex_runtime(runtime_session_id, runtime) {
            let _ = kill_child_process(&process, "Codex");
            return Err(err).context("failed to register active Codex runtime");
        }
    }
    let mut rollout_streamer = match (state, runtime_session_id, external_session_id) {
        (Some(state), Some(runtime_session_id), Some(thread_id)) => {
            let path = wait_for_codex_rollout_path(&codex_home, thread_id)?;
            path.map(|path| {
                let start_offset = fs::metadata(&path)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
                spawn_codex_rollout_streamer(
                    state.clone(),
                    runtime_session_id.to_owned(),
                    path,
                    start_offset,
                )
            })
        }
        _ => None,
    };

    let stderr_thread = std::thread::spawn(move || -> Vec<String> {
        let reader = BufReader::new(stderr);
        reader.lines().map_while(Result::ok).collect()
    });

    let mut reader = BufReader::new(stdout);
    let mut resolved_session_id = external_session_id.map(str::to_owned);
    let mut deferred_stdout_agent_message: Option<String> = None;
    let mut raw_line = String::new();

    loop {
        raw_line.clear();
        let bytes_read = reader
            .read_line(&mut raw_line)
            .context("failed to read stdout from Codex")?;

        if bytes_read == 0 {
            break;
        }

        let message: Value = serde_json::from_str(raw_line.trim_end())
            .with_context(|| format!("failed to parse Codex JSON line: {}", raw_line.trim_end()))?;

        if rollout_streamer.is_none() {
            if let (Some(state), Some(runtime_session_id)) = (state, runtime_session_id) {
                if message.get("type").and_then(Value::as_str) == Some("thread.started") {
                    if let Some(thread_id) = message.get("thread_id").and_then(Value::as_str) {
                        if let Some(path) = wait_for_codex_rollout_path(&codex_home, thread_id)? {
                            rollout_streamer = Some(spawn_codex_rollout_streamer(
                                state.clone(),
                                runtime_session_id.to_owned(),
                                path,
                                0,
                            ));
                        }
                    }
                }
            }
        }

        handle_codex_event(
            &message,
            &mut resolved_session_id,
            recorder,
            if rollout_streamer.is_some() {
                Some(&mut deferred_stdout_agent_message)
            } else {
                None
            },
        )?;
    }

    let status = {
        let mut child = process.lock().expect("Codex process mutex poisoned");
        child.wait().context("failed waiting for Codex process")?
    };
    let mut rollout_saw_final_answer = false;
    if let Some(streamer) = rollout_streamer {
        streamer.stop.store(true, Ordering::SeqCst);
        let _ = streamer.join.join();
        rollout_saw_final_answer = streamer.saw_final_answer.load(Ordering::SeqCst);
    }
    if !rollout_saw_final_answer {
        if let Some(text) = deferred_stdout_agent_message.take() {
            recorder.push_text(&text)?;
        }
    }
    let stderr_lines = stderr_thread.join().unwrap_or_default();

    if !status.success() {
        let stderr_output = stderr_lines.join("\n");
        if stderr_output.trim().is_empty() {
            bail!("Codex exited with status {status}");
        } else {
            bail!("Codex exited with status {status}: {stderr_output}");
        }
    }

    resolved_session_id.ok_or_else(|| anyhow!("Codex completed without emitting a thread id"))
}

fn default_codex_sandbox_mode() -> CodexSandboxMode {
    match std::env::var("TERMAL_CODEX_SANDBOX").ok().as_deref() {
        Some("read-only") => CodexSandboxMode::ReadOnly,
        Some("danger-full-access") => CodexSandboxMode::DangerFullAccess,
        _ => CodexSandboxMode::WorkspaceWrite,
    }
}

fn default_codex_approval_policy() -> CodexApprovalPolicy {
    match std::env::var("TERMAL_CODEX_APPROVAL").ok().as_deref() {
        Some("untrusted") => CodexApprovalPolicy::Untrusted,
        Some("on-request") => CodexApprovalPolicy::OnRequest,
        Some("on-failure") => CodexApprovalPolicy::OnFailure,
        _ => CodexApprovalPolicy::Never,
    }
}

fn default_claude_approval_mode() -> ClaudeApprovalMode {
    ClaudeApprovalMode::Ask
}

fn handle_codex_event(
    message: &Value,
    session_id: &mut Option<String>,
    recorder: &mut dyn TurnRecorder,
    deferred_stdout_agent_message: Option<&mut Option<String>>,
) -> Result<()> {
    let Some(event_type) = message.get("type").and_then(Value::as_str) else {
        log_unhandled_codex_event("missing top-level event type", message);
        return Ok(());
    };

    match event_type {
        "turn.started" | "turn.completed" => {}
        "thread.started" => {
            let thread_id = get_string(message, &["thread_id"])?;
            *session_id = Some(thread_id.to_owned());
            recorder.note_external_session(thread_id)?;
        }
        "item.started" => match message.pointer("/item/type").and_then(Value::as_str) {
            Some("command_execution") => {
                if let Some(command) = message.pointer("/item/command").and_then(Value::as_str) {
                    let key = codex_item_key(message, command);
                    recorder.command_started(&key, command)?;
                }
            }
            Some("web_search") => {
                let key = codex_item_key(message, "web_search");
                let command = describe_codex_web_search_command(message);
                recorder.command_started(&key, &command)?;
            }
            Some(item_type) => {
                log_unhandled_codex_event(
                    &format!("unhandled Codex item.started type `{item_type}`"),
                    message,
                );
            }
            None => {
                log_unhandled_codex_event("Codex item.started missing item.type", message);
            }
        },
        "item.completed" => {
            let Some(item_type) = message.pointer("/item/type").and_then(Value::as_str) else {
                log_unhandled_codex_event("Codex item.completed missing item.type", message);
                return Ok(());
            };

            match item_type {
                "agent_message" => {
                    if let Some(text) = message.pointer("/item/text").and_then(Value::as_str) {
                        if let Some(slot) = deferred_stdout_agent_message {
                            *slot = Some(text.to_owned());
                        } else {
                            recorder.push_text(text)?;
                        }
                    }
                }
                "command_execution" => {
                    if let Some(command) = message.pointer("/item/command").and_then(Value::as_str)
                    {
                        let key = codex_item_key(message, command);
                        let output = message
                            .pointer("/item/aggregated_output")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let exit_code = message.pointer("/item/exit_code").and_then(Value::as_i64);
                        let status = if exit_code.unwrap_or(-1) == 0 {
                            CommandStatus::Success
                        } else {
                            CommandStatus::Error
                        };
                        recorder.command_completed(&key, command, output, status)?;
                    }
                }
                "web_search" => {
                    let key = codex_item_key(message, "web_search");
                    let command = describe_codex_web_search_command(message);
                    let output = summarize_codex_web_search_output(message);
                    recorder.command_completed(&key, &command, &output, CommandStatus::Success)?;
                }
                _ => {
                    log_unhandled_codex_event(
                        &format!("unhandled Codex item.completed type `{item_type}`"),
                        message,
                    );
                }
            }
        }
        "error" => {
            recorder.error(&summarize_error(message))?;
        }
        _ => {
            log_unhandled_codex_event(
                &format!("unhandled Codex event type `{event_type}`"),
                message,
            );
        }
    }

    Ok(())
}

fn describe_codex_web_search_command(message: &Value) -> String {
    let query = message
        .pointer("/item/query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());

    match message.pointer("/item/action/type").and_then(Value::as_str) {
        Some("open_page") => message
            .pointer("/item/action/url")
            .and_then(Value::as_str)
            .map(|url| format!("Open page: {url}"))
            .unwrap_or_else(|| "Open page".to_owned()),
        Some("find_in_page") => message
            .pointer("/item/action/pattern")
            .and_then(Value::as_str)
            .map(|pattern| format!("Find in page: {pattern}"))
            .unwrap_or_else(|| "Find in page".to_owned()),
        Some("search") | Some("other") | None | Some(_) => query
            .map(|value| format!("Web search: {value}"))
            .unwrap_or_else(|| "Web search".to_owned()),
    }
}

fn summarize_codex_web_search_output(message: &Value) -> String {
    match message.pointer("/item/action/type").and_then(Value::as_str) {
        Some("search") => {
            let queries = message
                .pointer("/item/action/queries")
                .and_then(Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            if !queries.is_empty() {
                return queries.join("\n");
            }
        }
        Some("open_page") => {
            if let Some(url) = message.pointer("/item/action/url").and_then(Value::as_str) {
                return format!("Opened {url}");
            }
        }
        Some("find_in_page") => {
            let pattern = message
                .pointer("/item/action/pattern")
                .and_then(Value::as_str);
            let url = message.pointer("/item/action/url").and_then(Value::as_str);
            return match (pattern, url) {
                (Some(pattern), Some(url)) => format!("Searched for `{pattern}` in {url}"),
                (Some(pattern), None) => format!("Searched for `{pattern}`"),
                (None, Some(url)) => format!("Searched within {url}"),
                (None, None) => "Find in page completed".to_owned(),
            };
        }
        _ => {}
    }

    message
        .pointer("/item/query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Web search completed")
        .to_owned()
}

fn extract_codex_rollout_agent_message(message: &Value) -> Option<(String, String)> {
    let payload = message.get("payload")?;
    if message.get("type").and_then(Value::as_str) != Some("event_msg") {
        return None;
    }

    if payload.get("type").and_then(Value::as_str) != Some("agent_message") {
        return None;
    }

    let text = payload.get("message").and_then(Value::as_str)?.trim();
    if text.is_empty() {
        return None;
    }

    let phase = payload
        .get("phase")
        .and_then(Value::as_str)
        .unwrap_or("message");

    Some((phase.to_owned(), text.to_owned()))
}

fn log_unhandled_codex_event(context: &str, message: &Value) {
    eprintln!("codex diagnostic> {context}: {message}");
}

fn run_claude_turn(
    cwd: &str,
    session_id: Option<&str>,
    prompt: &str,
    recorder: &mut dyn TurnRecorder,
) -> Result<String> {
    let mut command = Command::new("claude");
    command.current_dir(cwd).args([
        "-p",
        "--verbose",
        "--output-format",
        "stream-json",
        "--include-partial-messages",
    ]);

    let expected_session_id = match session_id {
        Some(session_id) => {
            command.args(["--resume", session_id]);
            session_id.to_owned()
        }
        None => {
            let session_id = Uuid::new_v4().to_string();
            command.args(["--session-id", &session_id]);
            session_id
        }
    };

    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start Claude")?;

    let mut child_stdin = child
        .stdin
        .take()
        .context("failed to capture child stdin")?;
    writeln!(child_stdin, "{prompt}").context("failed to write prompt to Claude stdin")?;
    drop(child_stdin);

    let stdout = child
        .stdout
        .take()
        .context("failed to capture child stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("failed to capture child stderr")?;

    let stderr_thread = std::thread::spawn(move || -> Vec<String> {
        let reader = BufReader::new(stderr);
        reader.lines().map_while(Result::ok).collect()
    });

    let mut reader = BufReader::new(stdout);
    let mut resolved_session_id = Some(expected_session_id);
    let mut raw_line = String::new();
    let mut state = ClaudeTurnState::default();

    loop {
        raw_line.clear();
        let bytes_read = reader
            .read_line(&mut raw_line)
            .context("failed to read stdout from Claude")?;

        if bytes_read == 0 {
            break;
        }

        let message: Value = serde_json::from_str(raw_line.trim_end()).with_context(|| {
            format!("failed to parse Claude JSON line: {}", raw_line.trim_end())
        })?;

        handle_claude_event(&message, &mut resolved_session_id, &mut state, recorder)?;
    }

    recorder.finish_streaming_text()?;

    let status = child.wait().context("failed waiting for Claude process")?;
    let stderr_lines = stderr_thread.join().unwrap_or_default();

    if !status.success() {
        let stderr_output = stderr_lines.join("\n");
        if stderr_output.trim().is_empty() {
            bail!("Claude exited with status {status}");
        } else {
            bail!("Claude exited with status {status}: {stderr_output}");
        }
    }

    resolved_session_id.ok_or_else(|| anyhow!("Claude completed without emitting a session id"))
}

#[derive(Default)]
struct ClaudeTurnState {
    approval_keys_this_turn: HashSet<String>,
    permission_denied_this_turn: bool,
    pending_tools: HashMap<String, ClaudeToolUse>,
    saw_text_delta: bool,
}

struct ClaudeToolUse {
    command: Option<String>,
    file_path: Option<String>,
    name: String,
}

struct ClaudeToolPermissionRequest {
    detail: String,
    permission_mode_for_session: Option<String>,
    request_id: String,
    title: String,
    tool_name: String,
    tool_input: Value,
}

fn classify_claude_control_request(
    message: &Value,
    state: &mut ClaudeTurnState,
    approval_mode: ClaudeApprovalMode,
) -> Result<Option<ClaudeControlRequestAction>> {
    let Some(request) = parse_claude_tool_permission_request(message) else {
        return Ok(None);
    };

    let command = describe_claude_tool_request(&request);
    let key = format!("{}\n{}\n{}", request.request_id, request.title, command);
    if !state.approval_keys_this_turn.insert(key) {
        return Ok(None);
    }

    Ok(Some(match approval_mode {
        ClaudeApprovalMode::Ask => ClaudeControlRequestAction::QueueApproval {
            title: request.title,
            command,
            detail: request.detail,
            approval: ClaudePendingApproval {
                permission_mode_for_session: request.permission_mode_for_session,
                request_id: request.request_id,
                tool_input: request.tool_input,
            },
        },
        ClaudeApprovalMode::AutoApprove => {
            ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Allow {
                request_id: request.request_id,
                updated_input: request.tool_input,
            })
        }
    }))
}

fn parse_claude_tool_permission_request(message: &Value) -> Option<ClaudeToolPermissionRequest> {
    if message.get("type").and_then(Value::as_str) != Some("control_request") {
        return None;
    }

    let request = message.get("request")?;
    if request.get("subtype").and_then(Value::as_str) != Some("can_use_tool") {
        return None;
    }

    let request_id = message
        .get("request_id")
        .and_then(Value::as_str)?
        .to_owned();
    let tool_name = request.get("tool_name").and_then(Value::as_str)?;
    let tool_input = request.get("input").cloned().unwrap_or_else(|| json!({}));
    let permission_mode_for_session = request
        .get("permission_suggestions")
        .and_then(Value::as_array)
        .and_then(|suggestions| {
            suggestions.iter().find_map(|suggestion| {
                (suggestion.get("type").and_then(Value::as_str) == Some("setMode")
                    && suggestion.get("destination").and_then(Value::as_str) == Some("session"))
                .then(|| suggestion.get("mode").and_then(Value::as_str))
                .flatten()
                .map(str::to_owned)
            })
        });

    let detail = describe_claude_permission_detail(
        tool_name,
        &tool_input,
        request.get("decision_reason").and_then(Value::as_str),
    );

    Some(ClaudeToolPermissionRequest {
        detail,
        permission_mode_for_session,
        request_id,
        title: "Claude needs approval".to_owned(),
        tool_name: tool_name.to_owned(),
        tool_input,
    })
}

fn handle_claude_event(
    message: &Value,
    session_id: &mut Option<String>,
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    let Some(event_type) = message.get("type").and_then(Value::as_str) else {
        return Ok(());
    };

    match event_type {
        "system" => {
            if message.get("subtype").and_then(Value::as_str) == Some("init") {
                if let Some(found_session_id) = message.get("session_id").and_then(Value::as_str) {
                    *session_id = Some(found_session_id.to_owned());
                    recorder.note_external_session(found_session_id)?;
                }
            }
        }
        "stream_event" => {
            let Some(stream_type) = message.pointer("/event/type").and_then(Value::as_str) else {
                return Ok(());
            };

            match stream_type {
                "content_block_delta" => {
                    if !state.permission_denied_this_turn {
                        if let Some(text) = message
                            .pointer("/event/delta/text")
                            .or_else(|| message.pointer("/event/delta/text_delta"))
                            .and_then(Value::as_str)
                        {
                            let text = if state.saw_text_delta {
                                text
                            } else {
                                text.trim_start_matches('\n')
                            };
                            if !text.is_empty() {
                                recorder.text_delta(text)?;
                                state.saw_text_delta = true;
                            }
                        }
                    }
                }
                "message_stop" => {
                    recorder.finish_streaming_text()?;
                }
                _ => {}
            }
        }
        "assistant" => {
            if let Some(contents) = message
                .pointer("/message/content")
                .and_then(Value::as_array)
            {
                for content in contents {
                    let Some(content_type) = content.get("type").and_then(Value::as_str) else {
                        continue;
                    };

                    match content_type {
                        "text" if !state.saw_text_delta => {
                            if let Some(text) = content.get("text").and_then(Value::as_str) {
                                if state.permission_denied_this_turn {
                                    continue;
                                }
                                recorder.push_text(text)?;
                            }
                        }
                        "thinking" => {
                            if let Some(thinking) = content.get("thinking").and_then(Value::as_str)
                            {
                                let lines = split_thinking_lines(thinking);
                                recorder.push_thinking("Thinking", lines)?;
                            }
                        }
                        "tool_use" => {
                            register_claude_tool_use(content, state, recorder)?;
                        }
                        _ => {}
                    }
                }
            }
        }
        "user" => {
            handle_claude_tool_result(message, state, recorder)?;
        }
        "result" => {
            recorder.finish_streaming_text()?;
            state.saw_text_delta = false;
            state.approval_keys_this_turn.clear();
            state.permission_denied_this_turn = false;

            if message
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                recorder.error(&summarize_error(message))?;
            }
        }
        _ => {}
    }

    Ok(())
}

fn register_claude_tool_use(
    content: &Value,
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    let Some(tool_id) = content.get("id").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(name) = content.get("name").and_then(Value::as_str) else {
        return Ok(());
    };

    let input = content.get("input");
    let command = input
        .and_then(|value| value.get("command"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let file_path = input
        .and_then(|value| value.get("file_path").or_else(|| value.get("filePath")))
        .and_then(Value::as_str)
        .map(str::to_owned);

    state.pending_tools.insert(
        tool_id.to_owned(),
        ClaudeToolUse {
            command: command.clone(),
            file_path,
            name: name.to_owned(),
        },
    );

    if name == "Bash" {
        let description = input
            .and_then(|value| value.get("description"))
            .and_then(Value::as_str);
        let command_label = command.as_deref().or(description).unwrap_or("Bash");
        recorder.command_started(tool_id, command_label)?;
    }

    Ok(())
}

fn handle_claude_tool_result(
    message: &Value,
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    let Some(contents) = message
        .pointer("/message/content")
        .and_then(Value::as_array)
    else {
        return Ok(());
    };

    for content in contents {
        if content.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }

        let Some(tool_use_id) = content.get("tool_use_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(tool_use) = state.pending_tools.remove(tool_use_id) else {
            continue;
        };

        let is_error = content
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let detail = extract_claude_tool_result_text(message, content);

        match tool_use.name.as_str() {
            "Bash" => handle_claude_bash_result(
                tool_use_id,
                &tool_use,
                message.get("tool_use_result"),
                &detail,
                is_error,
                state,
                recorder,
            )?,
            "Write" | "Edit" => handle_claude_file_result(
                &tool_use,
                message.get("tool_use_result"),
                &detail,
                is_error,
                state,
                recorder,
            )?,
            _ => {
                if is_error {
                    recorder.error(&detail)?;
                }
            }
        }
    }

    Ok(())
}

fn handle_claude_bash_result(
    tool_use_id: &str,
    tool_use: &ClaudeToolUse,
    tool_use_result: Option<&Value>,
    detail: &str,
    is_error: bool,
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    if is_error && is_permission_denial(detail) {
        state.permission_denied_this_turn = true;
        record_claude_approval(
            state,
            recorder,
            "Claude needs approval",
            tool_use.command.as_deref().unwrap_or("Bash"),
            detail,
        )?;
        return Ok(());
    }

    let stdout = tool_use_result
        .and_then(|value| value.get("stdout"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let stderr = tool_use_result
        .and_then(|value| value.get("stderr"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let interrupted = tool_use_result
        .and_then(|value| value.get("interrupted"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut output = String::new();
    if !stdout.is_empty() {
        output.push_str(stdout);
    }
    if !stderr.is_empty() {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(stderr);
    }
    if output.trim().is_empty() && !detail.is_empty() {
        output.push_str(detail);
    }

    let status = if is_error || interrupted {
        CommandStatus::Error
    } else {
        CommandStatus::Success
    };
    let command = tool_use.command.as_deref().unwrap_or("Bash");
    recorder.command_completed(tool_use_id, command, output.trim_end(), status)
}

fn handle_claude_file_result(
    tool_use: &ClaudeToolUse,
    tool_use_result: Option<&Value>,
    detail: &str,
    is_error: bool,
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    if is_error {
        if is_permission_denial(detail) {
            state.permission_denied_this_turn = true;
            record_claude_approval(
                state,
                recorder,
                "Claude needs approval",
                &describe_claude_tool_action(tool_use),
                detail,
            )?;
        } else {
            recorder.error(detail)?;
        }
        return Ok(());
    }

    let Some(tool_use_result) = tool_use_result else {
        return Ok(());
    };

    let tool_kind = tool_use_result
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("");
    let Some(file_path) = tool_use_result
        .get("filePath")
        .and_then(Value::as_str)
        .or(tool_use.file_path.as_deref())
    else {
        return Ok(());
    };

    match tool_kind {
        "create" => {
            let content = tool_use_result
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("");
            let diff = content
                .lines()
                .map(|line| format!("+{line}"))
                .collect::<Vec<_>>()
                .join("\n");
            recorder.push_diff(
                file_path,
                &format!("Created {}", short_file_name(file_path)),
                &diff,
                ChangeType::Create,
            )?;
        }
        "update" => {
            let diff = tool_use_result
                .get("structuredPatch")
                .and_then(Value::as_array)
                .map(|patches| flatten_structured_patch(patches.as_slice()))
                .filter(|diff| !diff.trim().is_empty())
                .unwrap_or_else(|| {
                    fallback_file_diff(
                        tool_use_result
                            .get("originalFile")
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                        tool_use_result
                            .get("content")
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                    )
                });
            recorder.push_diff(
                file_path,
                &format!("Updated {}", short_file_name(file_path)),
                &diff,
                ChangeType::Edit,
            )?;
        }
        _ => {}
    }

    Ok(())
}

fn extract_claude_tool_result_text(message: &Value, content: &Value) -> String {
    if let Some(text) = content.get("content").and_then(Value::as_str) {
        return text.to_owned();
    }
    if let Some(text) = message.get("tool_use_result").and_then(Value::as_str) {
        return text.to_owned();
    }
    if let Some(text) = message
        .get("tool_use_result")
        .and_then(|value| value.get("stderr"))
        .and_then(Value::as_str)
    {
        return text.to_owned();
    }

    "Claude tool call failed.".to_owned()
}

fn is_permission_denial(detail: &str) -> bool {
    detail.contains("requested permissions")
}

fn record_claude_approval(
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
    title: &str,
    command: &str,
    detail: &str,
) -> Result<()> {
    let key = format!("{title}\n{command}\n{detail}");
    if state.approval_keys_this_turn.insert(key) {
        recorder.push_approval(title, command, detail)?;
    }

    Ok(())
}

fn describe_claude_tool_request(request: &ClaudeToolPermissionRequest) -> String {
    describe_claude_tool_action_from_parts(&request.tool_name, &request.tool_input)
}

fn describe_claude_tool_action(tool_use: &ClaudeToolUse) -> String {
    match (
        tool_use.name.as_str(),
        tool_use.file_path.as_deref(),
        tool_use.command.as_deref(),
    ) {
        ("Write" | "Edit", Some(file_path), _) => format!("{} {}", tool_use.name, file_path),
        (_, _, Some(command)) => command.to_owned(),
        _ => tool_use.name.clone(),
    }
}

fn describe_claude_tool_action_from_parts(tool_name: &str, tool_input: &Value) -> String {
    match tool_name {
        "Write" | "Edit" => tool_input
            .get("file_path")
            .or_else(|| tool_input.get("filePath"))
            .and_then(Value::as_str)
            .map(|file_path| format!("{tool_name} {file_path}"))
            .unwrap_or_else(|| tool_name.to_owned()),
        "Bash" => tool_input
            .get("command")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| tool_name.to_owned()),
        _ => tool_name.to_owned(),
    }
}

fn describe_claude_permission_detail(
    tool_name: &str,
    tool_input: &Value,
    decision_reason: Option<&str>,
) -> String {
    let specific = match tool_name {
        "Write" => tool_input
            .get("file_path")
            .or_else(|| tool_input.get("filePath"))
            .and_then(Value::as_str)
            .map(|file_path| format!("Claude requested permission to write to {file_path}.")),
        "Edit" => tool_input
            .get("file_path")
            .or_else(|| tool_input.get("filePath"))
            .and_then(Value::as_str)
            .map(|file_path| format!("Claude requested permission to edit {file_path}.")),
        "Bash" => tool_input
            .get("command")
            .and_then(Value::as_str)
            .map(|command| format!("Claude requested permission to run `{command}`.")),
        _ => None,
    };

    match (
        specific,
        decision_reason
            .map(str::trim)
            .filter(|reason| !reason.is_empty()),
    ) {
        (Some(specific), Some(reason)) => format!("{specific} Reason: {reason}."),
        (Some(specific), None) => specific,
        (None, Some(reason)) => format!("Claude requested approval. Reason: {reason}."),
        (None, None) => "Claude requested approval.".to_owned(),
    }
}

fn split_thinking_lines(thinking: &str) -> Vec<String> {
    let lines = thinking
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();

    if lines.is_empty() && !thinking.trim().is_empty() {
        vec![thinking.trim().to_owned()]
    } else {
        lines
    }
}

fn flatten_structured_patch(patches: &[Value]) -> String {
    patches
        .iter()
        .filter_map(|patch| patch.get("lines").and_then(Value::as_array))
        .flat_map(|lines| lines.iter())
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>()
        .join("\n")
}

fn fallback_file_diff(original: &str, updated: &str) -> String {
    let mut lines = Vec::new();
    for line in original.lines() {
        lines.push(format!("-{line}"));
    }
    for line in updated.lines() {
        lines.push(format!("+{line}"));
    }
    lines.join("\n")
}

fn short_file_name(file_path: &str) -> &str {
    file_path
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(file_path)
}

fn codex_item_key(message: &Value, command: &str) -> String {
    message
        .pointer("/item/id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("command:{command}"))
}

fn summarize_error(value: &Value) -> String {
    summarize_structured_error(value).unwrap_or_else(|| {
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    })
}

fn summarize_structured_error(value: &Value) -> Option<String> {
    summarize_retryable_connectivity_error(value)
        .or_else(|| summarize_error_fields(value))
        .or_else(|| value.get("error").and_then(summarize_error_fields))
        .or_else(|| {
            value
                .get("result")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|result| !result.is_empty())
                .map(str::to_owned)
        })
}

fn summarize_error_fields(value: &Value) -> Option<String> {
    let message = trimmed_string_field(value, "message");
    let detail = trimmed_string_field(value, "additionalDetails")
        .or_else(|| trimmed_string_field(value, "detail"))
        .or_else(|| trimmed_string_field(value, "details"));

    match (message, detail) {
        (Some(message), Some(detail)) if contains_ignore_ascii_case(message, detail) => {
            Some(message.to_owned())
        }
        (Some(message), Some(detail)) if contains_ignore_ascii_case(detail, message) => {
            Some(detail.to_owned())
        }
        (Some(message), Some(detail)) => Some(format!("{message} {detail}")),
        (Some(message), None) => Some(message.to_owned()),
        (None, Some(detail)) => Some(detail.to_owned()),
        (None, None) => None,
    }
}

fn summarize_retryable_connectivity_error(value: &Value) -> Option<String> {
    if !is_retryable_connectivity_error(value) {
        return None;
    }

    let mut summary = "Connection dropped before the response finished.".to_owned();
    if let Some(retry_status) = summarize_retry_status(value) {
        summary.push(' ');
        summary.push_str(&retry_status);
    } else {
        summary.push_str(" Retrying automatically.");
    }

    Some(summary)
}

fn is_retryable_connectivity_error(value: &Value) -> bool {
    codex_error_will_retry(value) && has_connectivity_marker(value)
}

fn codex_error_will_retry(value: &Value) -> bool {
    value
        .get("willRetry")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || value
            .get("error")
            .and_then(|error| error.get("willRetry"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

fn has_connectivity_marker(value: &Value) -> bool {
    value
        .pointer("/error/codexErrorInfo/responseStreamDisconnected")
        .is_some_and(|marker| !marker.is_null())
        || [
            trimmed_string_field(value, "message"),
            trimmed_string_field(value, "additionalDetails"),
            value
                .get("error")
                .and_then(|error| trimmed_string_field(error, "message")),
            value
                .get("error")
                .and_then(|error| trimmed_string_field(error, "additionalDetails")),
        ]
        .into_iter()
        .flatten()
        .any(is_connectivity_text)
}

fn summarize_retry_status(value: &Value) -> Option<String> {
    let message = trimmed_string_field(value, "message").or_else(|| {
        value
            .get("error")
            .and_then(|error| trimmed_string_field(error, "message"))
    })?;

    let counts = message
        .strip_prefix("Reconnecting...")
        .or_else(|| message.strip_prefix("Reconnecting…"))
        .map(str::trim);

    let Some(counts) = counts else {
        return Some("Retrying automatically.".to_owned());
    };

    let Some((current, total)) = counts.split_once('/') else {
        return Some("Retrying automatically.".to_owned());
    };

    let current = current.trim().parse::<usize>().ok()?;
    let total = total.trim().parse::<usize>().ok()?;
    Some(format!(
        "Retrying automatically (attempt {current} of {total})."
    ))
}

fn trimmed_string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|field| !field.is_empty())
}

fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    haystack
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

fn is_connectivity_text(text: &str) -> bool {
    let normalized = text.to_ascii_lowercase();
    normalized.contains("stream disconnected before completion")
        || normalized.contains("websocket closed by server before response.completed")
        || normalized.contains("response stream disconnected")
        || normalized.contains("connection dropped")
        || normalized.contains("reconnecting")
}

fn make_preview(text: &str) -> String {
    let first_line = text.lines().next().unwrap_or("").trim();
    let compact = first_line.replace('\t', " ");
    let compact = compact.trim();
    if compact.is_empty() {
        return "Waiting for activity.".to_owned();
    }

    const LIMIT: usize = 88;
    let mut preview = compact.chars().take(LIMIT).collect::<String>();
    if compact.chars().count() > LIMIT {
        preview.push_str("...");
    }
    preview
}

fn image_attachment_summary(count: usize) -> String {
    match count {
        0 => "Waiting for activity.".to_owned(),
        1 => "1 image attached".to_owned(),
        count => format!("{count} images attached"),
    }
}

fn prompt_preview_text(text: &str, attachments: &[MessageImageAttachment]) -> String {
    let trimmed = text.trim();
    if !trimmed.is_empty() {
        return make_preview(trimmed);
    }

    make_preview(&image_attachment_summary(attachments.len()))
}

fn shell_language() -> &'static str {
    "bash"
}

fn infer_language_from_path(path: &FsPath) -> Option<&'static str> {
    let file_name = path.file_name()?.to_str()?.to_ascii_lowercase();
    match file_name.as_str() {
        "dockerfile" => return Some("dockerfile"),
        "makefile" => return Some("makefile"),
        _ => {}
    }

    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "bash" | "sh" | "zsh" => Some("bash"),
        "cjs" | "js" | "jsx" | "mjs" => Some("javascript"),
        "css" => Some("css"),
        "go" => Some("go"),
        "htm" | "html" | "svg" | "xml" => Some("xml"),
        "ini" | "toml" => Some("ini"),
        "json" => Some("json"),
        "md" | "mdx" => Some("markdown"),
        "mts" | "ts" | "tsx" => Some("typescript"),
        "py" => Some("python"),
        "rs" => Some("rust"),
        "sql" => Some("sql"),
        "yaml" | "yml" => Some("yaml"),
        _ => None,
    }
}

fn infer_command_output_language(command: &str) -> Option<&'static str> {
    let normalized = command.to_ascii_lowercase();
    if normalized.contains("git diff")
        || normalized
            .split(command_token_separator)
            .any(|token| token == "diff" || token == "patch")
    {
        return Some("diff");
    }

    if !normalized
        .split(command_token_separator)
        .any(is_file_viewer_command)
    {
        return None;
    }

    command
        .split(command_token_separator)
        .map(clean_command_path_hint)
        .rev()
        .find_map(|candidate| infer_language_from_path(FsPath::new(candidate)))
}

fn command_token_separator(character: char) -> bool {
    character.is_whitespace() || matches!(character, '"' | '\'' | '`' | '|' | '&' | ';')
}

fn is_file_viewer_command(token: &str) -> bool {
    matches!(
        token,
        "bat" | "cat" | "head" | "less" | "more" | "sed" | "tail"
    )
}

fn clean_command_path_hint(token: &str) -> &str {
    let trimmed = token.trim_matches(|character: char| {
        matches!(
            character,
            '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | ':'
        )
    });

    trimmed
        .rsplit_once('=')
        .map(|(_, value)| value)
        .unwrap_or(trimmed)
        .trim_matches(|character: char| {
            matches!(
                character,
                '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | ':'
            )
        })
}

fn codex_user_input_items(prompt: &str, attachments: &[PromptImageAttachment]) -> Vec<Value> {
    let mut input = Vec::with_capacity(attachments.len() + usize::from(!prompt.is_empty()));

    if !prompt.is_empty() {
        input.push(json!({
            "type": "text",
            "text": prompt,
        }));
    }

    input.extend(attachments.iter().map(|attachment| {
        json!({
            "type": "image",
            "url": codex_image_data_url(attachment),
        })
    }));

    input
}

fn codex_image_data_url(attachment: &PromptImageAttachment) -> String {
    format!(
        "data:{};base64,{}",
        attachment.metadata.media_type, attachment.data
    )
}

fn parse_prompt_image_attachments(
    requests: &[SendMessageAttachmentRequest],
) -> std::result::Result<Vec<PromptImageAttachment>, ApiError> {
    requests
        .iter()
        .enumerate()
        .map(|(index, request)| parse_prompt_image_attachment(index, request))
        .collect()
}

fn parse_prompt_image_attachment(
    index: usize,
    request: &SendMessageAttachmentRequest,
) -> std::result::Result<PromptImageAttachment, ApiError> {
    let media_type = request.media_type.trim().to_ascii_lowercase();
    if !matches!(
        media_type.as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    ) {
        return Err(ApiError::bad_request(format!(
            "unsupported image attachment type `{media_type}`"
        )));
    }

    let data = request.data.trim();
    if data.is_empty() {
        return Err(ApiError::bad_request(
            "image attachment data cannot be empty",
        ));
    }

    let decoded = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| ApiError::bad_request("image attachment data is not valid base64"))?;
    if decoded.len() > MAX_IMAGE_ATTACHMENT_BYTES {
        return Err(ApiError::bad_request(format!(
            "image attachment exceeds the {} MB limit",
            MAX_IMAGE_ATTACHMENT_BYTES / (1024 * 1024)
        )));
    }

    let file_name = request
        .file_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(sanitize_attachment_file_name)
        .unwrap_or_else(|| default_attachment_file_name(index, &media_type));

    Ok(PromptImageAttachment {
        data: data.to_owned(),
        metadata: MessageImageAttachment {
            byte_size: decoded.len(),
            file_name,
            media_type,
        },
    })
}

fn default_attachment_file_name(index: usize, media_type: &str) -> String {
    let extension = match media_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "img",
    };

    format!("pasted-image-{}.{}", index + 1, extension)
}

fn sanitize_attachment_file_name(value: &str) -> String {
    let cleaned = value
        .chars()
        .map(|character| match character {
            '/' | '\\' | '\0' => '-',
            other => other,
        })
        .collect::<String>();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "pasted-image".to_owned()
    } else {
        cleaned.to_owned()
    }
}

fn stamp_now() -> String {
    Local::now().format("%H:%M").to_string()
}

fn resolve_persistence_path(default_workdir: &str) -> PathBuf {
    resolve_termal_data_dir(default_workdir).join("sessions.json")
}

fn load_state(path: &FsPath) -> Result<Option<StateInner>> {
    if !path.exists() {
        return Ok(None);
    }

    let raw = fs::read(path).with_context(|| format!("failed to read `{}`", path.display()))?;
    let persisted: PersistedState = serde_json::from_slice(&raw)
        .with_context(|| format!("failed to parse `{}`", path.display()))?;
    Ok(Some(persisted.into_inner()))
}

fn persist_state(path: &FsPath, inner: &StateInner) -> Result<()> {
    let persisted = PersistedState::from_inner(inner);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }

    let encoded =
        serde_json::to_vec_pretty(&persisted).context("failed to serialize persisted state")?;
    fs::write(path, encoded).with_context(|| format!("failed to write `{}`", path.display()))
}

fn deliver_turn_dispatch(state: &AppState, dispatch: TurnDispatch) -> Result<(), ApiError> {
    match dispatch {
        TurnDispatch::PersistentClaude {
            command,
            sender,
            session_id,
        } => {
            if let Err(err) = sender.send(ClaudeRuntimeCommand::Prompt(command)) {
                let _ = state.clear_runtime(&session_id);
                let _ = state.fail_turn(
                    &session_id,
                    &format!("failed to queue prompt for Claude session: {err}"),
                );
                return Err(ApiError::internal(
                    "failed to queue prompt for Claude session",
                ));
            }
        }
        TurnDispatch::PersistentCodex {
            command,
            sender,
            session_id,
        } => {
            if let Err(err) = sender.send(CodexRuntimeCommand::Prompt(command)) {
                let _ = state.clear_runtime(&session_id);
                let _ = state.fail_turn(
                    &session_id,
                    &format!("failed to queue prompt for Codex session: {err}"),
                );
                return Err(ApiError::internal(
                    "failed to queue prompt for Codex session",
                ));
            }
        }
    }

    Ok(())
}

fn get_string<'a>(value: &'a Value, path: &[&str]) -> Result<&'a str> {
    let mut current = value;
    for segment in path {
        current = current
            .get(segment)
            .with_context(|| format!("missing field `{}`", path.join(".")))?;
    }

    current
        .as_str()
        .with_context(|| format!("field `{}` is not a string", path.join(".")))
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { ok: true })
}

async fn get_state(State(state): State<AppState>) -> Json<StateResponse> {
    Json(state.snapshot())
}

async fn read_file(Query(query): Query<FileQuery>) -> Result<Json<FileResponse>, ApiError> {
    let resolved_path = resolve_requested_path(&query.path)?;
    let content = fs::read_to_string(&resolved_path).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound => {
            ApiError::bad_request(format!("file not found: {}", resolved_path.display()))
        }
        io::ErrorKind::InvalidData => ApiError::bad_request(format!(
            "file is not valid UTF-8: {}",
            resolved_path.display()
        )),
        _ => ApiError::internal(format!(
            "failed to read file {}: {err}",
            resolved_path.display()
        )),
    })?;

    Ok(Json(FileResponse {
        path: resolved_path.to_string_lossy().into_owned(),
        content,
        language: infer_language_from_path(&resolved_path).map(str::to_owned),
    }))
}

async fn write_file(Json(request): Json<WriteFileRequest>) -> Result<Json<FileResponse>, ApiError> {
    let resolved_path = resolve_requested_path(&request.path)?;
    if let Some(parent) = resolved_path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            ApiError::internal(format!(
                "failed to create parent directory for {}: {err}",
                resolved_path.display()
            ))
        })?;
    }

    fs::write(&resolved_path, request.content.as_bytes()).map_err(|err| {
        ApiError::internal(format!(
            "failed to write file {}: {err}",
            resolved_path.display()
        ))
    })?;

    Ok(Json(FileResponse {
        path: resolved_path.to_string_lossy().into_owned(),
        content: request.content,
        language: infer_language_from_path(&resolved_path).map(str::to_owned),
    }))
}

async fn read_directory(
    Query(query): Query<FileQuery>,
) -> Result<Json<DirectoryResponse>, ApiError> {
    let resolved_path = resolve_requested_path(&query.path)?;
    let metadata = fs::metadata(&resolved_path).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound => {
            ApiError::bad_request(format!("path not found: {}", resolved_path.display()))
        }
        _ => ApiError::internal(format!(
            "failed to stat path {}: {err}",
            resolved_path.display()
        )),
    })?;

    if !metadata.is_dir() {
        return Err(ApiError::bad_request(format!(
            "path is not a directory: {}",
            resolved_path.display()
        )));
    }

    let mut entries = fs::read_dir(&resolved_path)
        .map_err(|err| {
            ApiError::internal(format!(
                "failed to read directory {}: {err}",
                resolved_path.display()
            ))
        })?
        .map(|entry| {
            let entry = entry.map_err(|err| {
                ApiError::internal(format!(
                    "failed to read directory entry in {}: {err}",
                    resolved_path.display()
                ))
            })?;
            let path = entry.path();
            let metadata = entry.metadata().map_err(|err| {
                ApiError::internal(format!(
                    "failed to stat directory entry {}: {err}",
                    path.display()
                ))
            })?;
            let name = entry.file_name().to_string_lossy().into_owned();

            Ok(DirectoryEntry {
                kind: if metadata.is_dir() {
                    FileSystemEntryKind::Directory
                } else {
                    FileSystemEntryKind::File
                },
                name,
                path: path.to_string_lossy().into_owned(),
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;

    entries.sort_by(|left, right| {
        let kind_order = match (&left.kind, &right.kind) {
            (FileSystemEntryKind::Directory, FileSystemEntryKind::File) => std::cmp::Ordering::Less,
            (FileSystemEntryKind::File, FileSystemEntryKind::Directory) => {
                std::cmp::Ordering::Greater
            }
            _ => std::cmp::Ordering::Equal,
        };

        if kind_order == std::cmp::Ordering::Equal {
            left.name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase())
        } else {
            kind_order
        }
    });

    Ok(Json(DirectoryResponse {
        name: resolved_path
            .file_name()
            .and_then(|value| value.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| resolved_path.to_string_lossy().into_owned()),
        path: resolved_path.to_string_lossy().into_owned(),
        entries,
    }))
}

async fn read_git_status(
    Query(query): Query<FileQuery>,
) -> Result<Json<GitStatusResponse>, ApiError> {
    let workdir = resolve_requested_path(&query.path)?;
    let workdir = if workdir.is_dir() {
        workdir
    } else {
        workdir.parent().map(FsPath::to_path_buf).ok_or_else(|| {
            ApiError::bad_request("cannot inspect git status for a root file path")
        })?
    };

    let Some(repo_root) = resolve_git_repo_root(&workdir)? else {
        return Ok(Json(GitStatusResponse {
            ahead: 0,
            behind: 0,
            branch: None,
            files: Vec::new(),
            is_clean: true,
            repo_root: None,
            upstream: None,
            workdir: workdir.to_string_lossy().into_owned(),
        }));
    };

    let output = Command::new("git")
        .arg("-C")
        .arg(&repo_root)
        .args(["status", "--porcelain=v1", "--branch", "-uall"])
        .output()
        .map_err(|err| ApiError::internal(format!("failed to run git status: {err}")))?;

    if !output.status.success() {
        return Err(ApiError::internal(format!(
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut branch = None;
    let mut upstream = None;
    let mut ahead = 0;
    let mut behind = 0;
    let mut files = Vec::new();

    for line in stdout.lines() {
        if let Some(branch_info) = line.strip_prefix("## ") {
            let parsed = parse_git_branch_status(branch_info);
            branch = parsed.branch;
            upstream = parsed.upstream;
            ahead = parsed.ahead;
            behind = parsed.behind;
            continue;
        }

        if line.len() < 3 {
            continue;
        }

        let status = &line[..2];
        let path_payload = line[3..].trim();
        let (original_path, path) = match path_payload.split_once(" -> ") {
            Some((from, to)) => (Some(from.trim().to_owned()), to.trim().to_owned()),
            None => (None, path_payload.to_owned()),
        };
        let index_status = status.chars().next().and_then(normalize_git_status_code);
        let worktree_status = status.chars().nth(1).and_then(normalize_git_status_code);

        files.push(GitStatusFile {
            index_status,
            original_path,
            path,
            worktree_status,
        });
    }

    let is_clean = files.is_empty();

    Ok(Json(GitStatusResponse {
        ahead,
        behind,
        branch,
        files,
        is_clean,
        repo_root: Some(repo_root.to_string_lossy().into_owned()),
        upstream,
        workdir: workdir.to_string_lossy().into_owned(),
    }))
}

async fn state_events(
    State(state): State<AppState>,
) -> Sse<impl futures_core::Stream<Item = std::result::Result<Event, Infallible>>> {
    let mut state_receiver = state.subscribe_events();
    let mut delta_receiver = state.subscribe_delta_events();
    let initial_payload = serde_json::to_string(&state.snapshot())
        .unwrap_or_else(|_| "{\"revision\":0,\"sessions\":[]}".to_owned());

    let stream = async_stream::stream! {
        yield Ok(Event::default().event("state").data(initial_payload));

        loop {
            tokio::select! {
                biased;

                result = state_receiver.recv() => {
                    match result {
                        Ok(payload) => yield Ok(Event::default().event("state").data(payload)),
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            let payload = serde_json::to_string(&state.snapshot())
                                .unwrap_or_else(|_| "{\"revision\":0,\"sessions\":[]}".to_owned());
                            yield Ok(Event::default().event("state").data(payload));
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }

                result = delta_receiver.recv() => {
                    match result {
                        Ok(payload) => yield Ok(Event::default().event("delta").data(payload)),
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            let payload = serde_json::to_string(&state.snapshot())
                                .unwrap_or_else(|_| "{\"revision\":0,\"sessions\":[]}".to_owned());
                            yield Ok(Event::default().event("state").data(payload));
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn create_session(
    State(state): State<AppState>,
    Json(request): Json<CreateSessionRequest>,
) -> Result<(StatusCode, Json<CreateSessionResponse>), ApiError> {
    let response = state
        .create_session(request)
        .map_err(|err| ApiError::internal(format!("failed to create session: {err:#}")))?;
    Ok((StatusCode::CREATED, Json(response)))
}

async fn update_session_settings(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
    Json(request): Json<UpdateSessionSettingsRequest>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = state.update_session_settings(&session_id, request)?;
    Ok(Json(response))
}

async fn send_message(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
    Json(request): Json<SendMessageRequest>,
) -> Result<(StatusCode, Json<StateResponse>), ApiError> {
    let dispatch = state.dispatch_turn(&session_id, request)?;

    if let DispatchTurnResult::Dispatched(dispatch) = dispatch {
        deliver_turn_dispatch(&state, dispatch)?;
    }

    let snapshot = state.snapshot();

    Ok((StatusCode::ACCEPTED, Json(snapshot)))
}

async fn cancel_queued_prompt(
    AxumPath((session_id, prompt_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = state.cancel_queued_prompt(&session_id, &prompt_id)?;
    Ok(Json(response))
}

async fn stop_session(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = state.stop_session(&session_id)?;
    Ok(Json(response))
}

async fn kill_session(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = state.kill_session(&session_id)?;
    Ok(Json(response))
}

async fn submit_approval(
    AxumPath((session_id, message_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<ApprovalRequest>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = state.update_approval(&session_id, &message_id, request.decision)?;
    Ok(Json(response))
}

#[derive(Debug)]
struct ApiError {
    message: String,
    status: StatusCode,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::BAD_REQUEST,
        }
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::CONFLICT,
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::NOT_FOUND,
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(ErrorResponse {
            error: self.message,
        });
        (self.status, body).into_response()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
enum Agent {
    Codex,
    Claude,
}

impl Agent {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self> {
        let mut args = args;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--agent" => {
                    let value = args.next().context("missing value after `--agent`")?;
                    return Self::from_str(&value);
                }
                "codex" => return Ok(Self::Codex),
                "claude" => return Ok(Self::Claude),
                other => bail!("unknown argument `{other}`"),
            }
        }

        Ok(Self::Codex)
    }

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "codex" => Ok(Self::Codex),
            "claude" => Ok(Self::Claude),
            other => bail!("unknown agent `{other}`; expected `codex` or `claude`"),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
        }
    }

    fn avatar(self) -> &'static str {
        match self {
            Self::Codex => "CX",
            Self::Claude => "CL",
        }
    }

    fn model_label(self) -> &'static str {
        match self {
            Self::Codex => "codex exec",
            Self::Claude => "claude -p",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Session {
    id: String,
    name: String,
    emoji: String,
    agent: Agent,
    workdir: String,
    model: String,
    approval_policy: Option<CodexApprovalPolicy>,
    sandbox_mode: Option<CodexSandboxMode>,
    claude_approval_mode: Option<ClaudeApprovalMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    external_session_id: Option<String>,
    status: SessionStatus,
    preview: String,
    messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending_prompts: Vec<PendingPrompt>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CodexApprovalPolicy {
    Untrusted,
    OnFailure,
    OnRequest,
    Never,
}

impl CodexApprovalPolicy {
    fn as_cli_value(self) -> &'static str {
        match self {
            Self::Untrusted => "untrusted",
            Self::OnFailure => "on-failure",
            Self::OnRequest => "on-request",
            Self::Never => "never",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CodexSandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl CodexSandboxMode {
    fn as_cli_value(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ClaudeApprovalMode {
    Ask,
    AutoApprove,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum SessionStatus {
    Active,
    Idle,
    Approval,
    Error,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Author {
    You,
    Assistant,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum CommandStatus {
    Running,
    Success,
    Error,
}

impl CommandStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Success => "success",
            Self::Error => "error",
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum ChangeType {
    Edit,
    Create,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum ApprovalDecision {
    Pending,
    Accepted,
    AcceptedForSession,
    Rejected,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct MessageImageAttachment {
    byte_size: usize,
    file_name: String,
    media_type: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PendingPrompt {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<MessageImageAttachment>,
    id: String,
    timestamp: String,
    text: String,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Message {
    Text {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<MessageImageAttachment>,
        id: String,
        timestamp: String,
        author: Author,
        text: String,
    },
    Thinking {
        id: String,
        timestamp: String,
        author: Author,
        title: String,
        lines: Vec<String>,
    },
    Command {
        id: String,
        timestamp: String,
        author: Author,
        command: String,
        #[serde(
            default,
            rename = "commandLanguage",
            skip_serializing_if = "Option::is_none"
        )]
        command_language: Option<String>,
        output: String,
        #[serde(
            default,
            rename = "outputLanguage",
            skip_serializing_if = "Option::is_none"
        )]
        output_language: Option<String>,
        status: CommandStatus,
    },
    Diff {
        id: String,
        timestamp: String,
        author: Author,
        #[serde(rename = "filePath")]
        file_path: String,
        summary: String,
        diff: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        #[serde(rename = "changeType")]
        change_type: ChangeType,
    },
    Markdown {
        id: String,
        timestamp: String,
        author: Author,
        title: String,
        markdown: String,
    },
    Approval {
        id: String,
        timestamp: String,
        author: Author,
        title: String,
        command: String,
        #[serde(
            default,
            rename = "commandLanguage",
            skip_serializing_if = "Option::is_none"
        )]
        command_language: Option<String>,
        detail: String,
        decision: ApprovalDecision,
    },
}

impl Message {
    fn preview_text(&self) -> Option<String> {
        match self {
            Self::Text {
                text, attachments, ..
            } => Some(prompt_preview_text(text, attachments)),
            Self::Thinking { title, .. } => Some(make_preview(title)),
            Self::Markdown { title, .. } => Some(make_preview(title)),
            Self::Approval { title, .. } => Some(make_preview(title)),
            Self::Diff { summary, .. } => Some(make_preview(summary)),
            Self::Command { .. } => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalRequest {
    decision: ApprovalDecision,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionRequest {
    agent: Option<Agent>,
    name: Option<String>,
    workdir: Option<String>,
    approval_policy: Option<CodexApprovalPolicy>,
    sandbox_mode: Option<CodexSandboxMode>,
    claude_approval_mode: Option<ClaudeApprovalMode>,
}

#[derive(Deserialize)]
struct FileQuery {
    path: String,
}

#[derive(Deserialize)]
struct WriteFileRequest {
    path: String,
    content: String,
}

#[derive(Serialize)]
struct FileResponse {
    path: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
enum FileSystemEntryKind {
    Directory,
    File,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DirectoryEntry {
    kind: FileSystemEntryKind,
    name: String,
    path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DirectoryResponse {
    entries: Vec<DirectoryEntry>,
    name: String,
    path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GitStatusFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    index_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_path: Option<String>,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    worktree_status: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GitStatusResponse {
    ahead: usize,
    behind: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    files: Vec<GitStatusFile>,
    is_clean: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    upstream: Option<String>,
    workdir: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorResponse {
    error: String,
}

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
}

#[derive(Deserialize)]
struct SendMessageRequest {
    text: String,
    #[serde(default)]
    attachments: Vec<SendMessageAttachmentRequest>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendMessageAttachmentRequest {
    data: String,
    file_name: Option<String>,
    media_type: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateSessionSettingsRequest {
    approval_policy: Option<CodexApprovalPolicy>,
    sandbox_mode: Option<CodexSandboxMode>,
    claude_approval_mode: Option<ClaudeApprovalMode>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rate_limits: Option<CodexRateLimits>,
}

impl CodexState {
    fn is_empty(&self) -> bool {
        self.rate_limits.is_none()
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexRateLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    credits: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plan_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    primary: Option<CodexRateLimitWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secondary: Option<CodexRateLimitWindow>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexRateLimitWindow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resets_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    used_percent: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window_duration_mins: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StateResponse {
    revision: u64,
    #[serde(default, skip_serializing_if = "CodexState::is_empty")]
    codex: CodexState,
    sessions: Vec<Session>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionResponse {
    session_id: String,
    state: StateResponse,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum DeltaEvent {
    TextDelta {
        revision: u64,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        delta: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        preview: Option<String>,
    },
    CommandUpdate {
        revision: u64,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        command: String,
        #[serde(rename = "commandLanguage", skip_serializing_if = "Option::is_none")]
        command_language: Option<String>,
        output: String,
        #[serde(rename = "outputLanguage", skip_serializing_if = "Option::is_none")]
        output_language: Option<String>,
        status: CommandStatus,
        preview: String,
    },
}

fn resolve_requested_path(path: &str) -> Result<PathBuf, ApiError> {
    let raw_path = FsPath::new(path);
    let resolved = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|err| ApiError::internal(format!("failed to resolve cwd: {err}")))?
            .join(raw_path)
    };

    Ok(resolved)
}

struct ParsedGitBranchStatus {
    ahead: usize,
    behind: usize,
    branch: Option<String>,
    upstream: Option<String>,
}

fn resolve_git_repo_root(workdir: &FsPath) -> Result<Option<PathBuf>, ApiError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|err| ApiError::internal(format!("failed to run git rev-parse: {err}")))?;

    if output.status.success() {
        let repo_root = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        return Ok((!repo_root.is_empty()).then(|| PathBuf::from(repo_root)));
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let trimmed = stderr.trim();
    if trimmed.contains("not a git repository") {
        return Ok(None);
    }

    Err(ApiError::internal(format!(
        "git rev-parse failed: {trimmed}"
    )))
}

fn parse_git_branch_status(line: &str) -> ParsedGitBranchStatus {
    let mut branch = None;
    let mut upstream = None;
    let mut ahead = 0;
    let mut behind = 0;

    let (head_segment, counts_segment) = match line.split_once(" [") {
        Some((head, counts)) => (head, Some(counts.trim_end_matches(']'))),
        None => (line, None),
    };

    if let Some((local_branch, upstream_branch)) = head_segment.split_once("...") {
        branch = Some(local_branch.trim().to_owned());
        let upstream_name = upstream_branch.trim();
        if !upstream_name.is_empty() {
            upstream = Some(upstream_name.to_owned());
        }
    } else {
        let trimmed = head_segment.trim();
        if !trimmed.is_empty() {
            branch = Some(trimmed.to_owned());
        }
    }

    if let Some(counts_segment) = counts_segment {
        for item in counts_segment.split(',') {
            let trimmed = item.trim();
            if let Some(value) = trimmed.strip_prefix("ahead ") {
                ahead = value.parse::<usize>().unwrap_or(0);
            } else if let Some(value) = trimmed.strip_prefix("behind ") {
                behind = value.parse::<usize>().unwrap_or(0);
            }
        }
    }

    ParsedGitBranchStatus {
        ahead,
        behind,
        branch,
        upstream,
    }
}

fn normalize_git_status_code(code: char) -> Option<String> {
    match code {
        ' ' => None,
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestRecorder {
        approvals: Vec<(String, String, String)>,
        commands: Vec<(String, String, CommandStatus)>,
        diffs: Vec<(String, String, String, ChangeType)>,
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

        fn error(&mut self, _detail: &str) -> Result<()> {
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
            inner: Arc::new(Mutex::new(StateInner::new())),
        }
    }

    fn test_session_id(state: &AppState, agent: Agent) -> String {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(agent, Some("Test".to_owned()), "/tmp".to_owned());
        let session_id = record.session.id.clone();
        state.commit_locked(&mut inner).unwrap();
        session_id
    }

    fn test_codex_runtime_handle(runtime_id: &str) -> CodexRuntimeHandle {
        let child = Command::new("sh").arg("-c").arg("exit 0").spawn().unwrap();
        let (input_tx, _input_rx) = mpsc::channel();

        CodexRuntimeHandle {
            runtime_id: runtime_id.to_owned(),
            input_tx,
            process: Arc::new(Mutex::new(child)),
        }
    }

    #[test]
    fn creates_claude_sessions_with_default_ask_mode() {
        let mut inner = StateInner::new();

        let record = inner.create_session(Agent::Claude, None, "/tmp".to_owned());

        assert_eq!(
            record.session.claude_approval_mode,
            Some(ClaudeApprovalMode::Ask)
        );
        assert_eq!(record.session.approval_policy, None);
        assert_eq!(record.session.sandbox_mode, None);
    }

    #[test]
    fn creates_codex_sessions_with_requested_prompt_defaults() {
        let state = test_app_state();

        let response = state
            .create_session(CreateSessionRequest {
                agent: Some(Agent::Codex),
                name: Some("Custom Codex".to_owned()),
                workdir: Some("/tmp".to_owned()),
                approval_policy: Some(CodexApprovalPolicy::OnRequest),
                sandbox_mode: Some(CodexSandboxMode::ReadOnly),
                claude_approval_mode: None,
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
        assert_eq!(session.sandbox_mode, Some(CodexSandboxMode::ReadOnly));
        assert_eq!(session.claude_approval_mode, None);

        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .find_session_index(&response.session_id)
            .map(|index| &inner.sessions[index]);
        let record = record.expect("session record should exist");
        assert_eq!(record.codex_approval_policy, CodexApprovalPolicy::OnRequest);
        assert_eq!(record.codex_sandbox_mode, CodexSandboxMode::ReadOnly);
    }

    #[test]
    fn revisions_increase_for_visible_state_changes() {
        let state = test_app_state();

        let created = state
            .create_session(CreateSessionRequest {
                agent: Some(Agent::Codex),
                name: Some("Revision Test".to_owned()),
                workdir: Some("/tmp".to_owned()),
                approval_policy: None,
                sandbox_mode: None,
                claude_approval_mode: None,
            })
            .unwrap();
        assert_eq!(created.state.revision, 1);

        let updated = state
            .update_session_settings(
                &created.session_id,
                UpdateSessionSettingsRequest {
                    sandbox_mode: Some(CodexSandboxMode::ReadOnly),
                    approval_policy: None,
                    claude_approval_mode: None,
                },
            )
            .unwrap();
        assert_eq!(updated.revision, 2);
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
                },
            )
            .unwrap();
        let baseline = state.snapshot().revision;

        state
            .append_text_delta(&session_id, "message-1", " there")
            .unwrap();

        let payload = delta_events.try_recv().expect("delta payload should exist");
        let event: Value = serde_json::from_str(&payload).expect("delta should be valid json");

        assert_eq!(event["type"], "textDelta");
        assert_eq!(event["revision"], json!(baseline + 1));
        assert_eq!(state.snapshot().revision, baseline + 1);
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
    fn cancels_queued_prompts_without_touching_other_items() {
        let state = test_app_state();
        let session_id = test_session_id(&state, Agent::Claude);

        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(&session_id).unwrap();
            inner.sessions[index].session.status = SessionStatus::Active;
            queue_prompt_on_record(
                &mut inner.sessions[index],
                PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-1".to_owned(),
                    timestamp: stamp_now(),
                    text: "first".to_owned(),
                },
                Vec::new(),
            );
            queue_prompt_on_record(
                &mut inner.sessions[index],
                PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-2".to_owned(),
                    timestamp: stamp_now(),
                    text: "second".to_owned(),
                },
                Vec::new(),
            );
            state.commit_locked(&mut inner).unwrap();
        }

        let response = state.cancel_queued_prompt(&session_id, "queued-1").unwrap();
        let session = response
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .unwrap();

        assert_eq!(session.pending_prompts.len(), 1);
        assert_eq!(session.pending_prompts[0].id, "queued-2");
        assert_eq!(session.pending_prompts[0].text, "second");
    }

    #[test]
    fn persists_queued_prompts_across_restart() {
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
                    attachments: vec![SendMessageAttachmentRequest {
                        data: "aGVsbG8=".to_owned(),
                        file_name: Some("pasted.png".to_owned()),
                        media_type: "image/png".to_owned(),
                    }],
                },
            )
            .unwrap();

        assert!(matches!(result, DispatchTurnResult::Queued));

        let reloaded = load_state(state.persistence_path.as_path())
            .unwrap()
            .expect("persisted state should exist");
        let record = reloaded
            .sessions
            .iter()
            .find(|record| record.session.id == session_id)
            .expect("session should reload");

        assert_eq!(record.session.pending_prompts.len(), 1);
        assert_eq!(
            record.session.pending_prompts[0].text,
            "queue this follow-up"
        );
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(record.queued_prompts[0].attachments.len(), 1);
        assert_eq!(record.queued_prompts[0].attachments[0].data, "aGVsbG8=");
        assert_eq!(
            record.queued_prompts[0].attachments[0].metadata.file_name,
            "pasted.png"
        );
        assert_eq!(
            record.queued_prompts[0].attachments[0].metadata.media_type,
            "image/png"
        );
    }

    #[test]
    fn maps_claude_bash_tool_results_to_command_messages() {
        let mut state = ClaudeTurnState::default();
        let mut recorder = TestRecorder::default();
        let mut session_id = None;

        let tool_use = json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "tool-1",
                        "name": "Bash",
                        "input": {
                            "command": "echo hi",
                            "description": "Print hi"
                        }
                    }
                ]
            }
        });
        handle_claude_event(&tool_use, &mut session_id, &mut state, &mut recorder).unwrap();

        let tool_result = json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "tool-1",
                        "content": "hi",
                        "is_error": false
                    }
                ]
            },
            "tool_use_result": {
                "stdout": "hi",
                "stderr": "",
                "interrupted": false
            }
        });
        handle_claude_event(&tool_result, &mut session_id, &mut state, &mut recorder).unwrap();

        assert_eq!(recorder.commands.len(), 2);
        assert_eq!(
            recorder.commands[0],
            ("echo hi".to_owned(), String::new(), CommandStatus::Running)
        );
        assert_eq!(
            recorder.commands[1],
            (
                "echo hi".to_owned(),
                "hi".to_owned(),
                CommandStatus::Success
            )
        );
    }

    #[test]
    fn maps_claude_write_results_to_diff_messages() {
        let mut state = ClaudeTurnState::default();
        let mut recorder = TestRecorder::default();
        let mut session_id = None;

        let tool_use = json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "tool-2",
                        "name": "Write",
                        "input": {
                            "file_path": "/tmp/hello.txt",
                            "content": "bye\n"
                        }
                    }
                ]
            }
        });
        handle_claude_event(&tool_use, &mut session_id, &mut state, &mut recorder).unwrap();

        let tool_result = json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "tool-2",
                        "content": "updated",
                        "is_error": false
                    }
                ]
            },
            "tool_use_result": {
                "type": "update",
                "filePath": "/tmp/hello.txt",
                "content": "bye\n",
                "structuredPatch": [
                    {
                        "lines": ["-hi", "+bye"]
                    }
                ],
                "originalFile": "hi\n"
            }
        });
        handle_claude_event(&tool_result, &mut session_id, &mut state, &mut recorder).unwrap();

        assert_eq!(recorder.diffs.len(), 1);
        assert_eq!(recorder.diffs[0].0, "/tmp/hello.txt");
        assert_eq!(recorder.diffs[0].1, "Updated hello.txt");
        assert_eq!(recorder.diffs[0].2, "-hi\n+bye");
        assert_eq!(recorder.diffs[0].3, ChangeType::Edit);
    }

    #[test]
    fn maps_claude_permission_denials_to_approval_messages() {
        let mut state = ClaudeTurnState::default();
        let mut recorder = TestRecorder::default();
        let mut session_id = None;

        let tool_use = json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "tool-3",
                        "name": "Write",
                        "input": {
                            "file_path": "/tmp/blocked.txt",
                            "content": "hi"
                        }
                    }
                ]
            }
        });
        handle_claude_event(&tool_use, &mut session_id, &mut state, &mut recorder).unwrap();

        let tool_result = json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "tool-3",
                        "content": "Claude requested permissions to write to /tmp/blocked.txt, but you haven't granted it yet.",
                        "is_error": true
                    }
                ]
            },
            "tool_use_result": "Error: Claude requested permissions to write to /tmp/blocked.txt, but you haven't granted it yet."
        });
        handle_claude_event(&tool_result, &mut session_id, &mut state, &mut recorder).unwrap();

        assert_eq!(recorder.approvals.len(), 1);
        assert_eq!(recorder.approvals[0].0, "Claude needs approval");
        assert_eq!(recorder.approvals[0].1, "Write /tmp/blocked.txt");
        assert!(state.permission_denied_this_turn);
    }

    #[test]
    fn dedupes_claude_permission_denials_and_suppresses_follow_up_text() {
        let mut state = ClaudeTurnState::default();
        let mut recorder = TestRecorder::default();
        let mut session_id = None;

        for tool_id in ["tool-4a", "tool-4b"] {
            let tool_use = json!({
                "type": "assistant",
                "message": {
                    "content": [
                        {
                            "type": "tool_use",
                            "id": tool_id,
                            "name": "Write",
                            "input": {
                                "file_path": "/tmp/blocked.txt",
                                "content": "hi"
                            }
                        }
                    ]
                }
            });
            handle_claude_event(&tool_use, &mut session_id, &mut state, &mut recorder).unwrap();

            let tool_result = json!({
                "type": "user",
                "message": {
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": tool_id,
                            "content": "Claude requested permissions to write to /tmp/blocked.txt, but you haven't granted it yet.",
                            "is_error": true
                        }
                    ]
                },
                "tool_use_result": "Error: Claude requested permissions to write to /tmp/blocked.txt, but you haven't granted it yet."
            });
            handle_claude_event(&tool_result, &mut session_id, &mut state, &mut recorder).unwrap();
        }

        let streamed_follow_up = json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "It looks like the write permission was denied."
                }
            }
        });
        handle_claude_event(
            &streamed_follow_up,
            &mut session_id,
            &mut state,
            &mut recorder,
        )
        .unwrap();

        let assistant_follow_up = json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "text",
                        "text": "Could you approve the file write so I can create /tmp/blocked.txt?"
                    }
                ]
            }
        });
        handle_claude_event(
            &assistant_follow_up,
            &mut session_id,
            &mut state,
            &mut recorder,
        )
        .unwrap();

        assert_eq!(recorder.approvals.len(), 1);
        assert_eq!(recorder.text_deltas, Vec::<String>::new());
        assert_eq!(recorder.texts, Vec::<String>::new());
    }

    #[test]
    fn parses_claude_control_requests_into_runtime_approvals() {
        let message = json!({
            "type": "control_request",
            "request_id": "req-1",
            "request": {
                "subtype": "can_use_tool",
                "tool_name": "Write",
                "input": {
                    "file_path": "/tmp/runtime.txt",
                    "content": "hi\n"
                },
                "decision_reason": "Write requires approval",
                "permission_suggestions": [
                    {
                        "type": "setMode",
                        "mode": "acceptEdits",
                        "destination": "session"
                    }
                ]
            }
        });

        let request = parse_claude_tool_permission_request(&message).unwrap();

        assert_eq!(request.request_id, "req-1");
        assert_eq!(request.tool_name, "Write");
        assert_eq!(
            request.detail,
            "Claude requested permission to write to /tmp/runtime.txt. Reason: Write requires approval."
        );
        assert_eq!(
            request.permission_mode_for_session.as_deref(),
            Some("acceptEdits")
        );
        assert_eq!(
            describe_claude_tool_request(&request),
            "Write /tmp/runtime.txt"
        );
    }

    #[test]
    fn queues_claude_control_requests_in_ask_mode() {
        let message = json!({
            "type": "control_request",
            "request_id": "req-ask",
            "request": {
                "subtype": "can_use_tool",
                "tool_name": "Write",
                "input": {
                    "file_path": "/tmp/ask.txt",
                    "content": "hi\n"
                },
                "decision_reason": "Write requires approval",
                "permission_suggestions": [
                    {
                        "type": "setMode",
                        "mode": "acceptEdits",
                        "destination": "session"
                    }
                ]
            }
        });
        let mut state = ClaudeTurnState::default();

        let action = classify_claude_control_request(&message, &mut state, ClaudeApprovalMode::Ask)
            .unwrap()
            .unwrap();

        match action {
            ClaudeControlRequestAction::QueueApproval {
                title,
                command,
                detail,
                approval,
            } => {
                assert_eq!(title, "Claude needs approval");
                assert_eq!(command, "Write /tmp/ask.txt");
                assert_eq!(
                    detail,
                    "Claude requested permission to write to /tmp/ask.txt. Reason: Write requires approval."
                );
                assert_eq!(approval.request_id, "req-ask");
                assert_eq!(
                    approval.permission_mode_for_session.as_deref(),
                    Some("acceptEdits")
                );
                assert_eq!(approval.tool_input["file_path"], "/tmp/ask.txt");
            }
            ClaudeControlRequestAction::Respond(_) => {
                panic!("expected Claude ask mode to queue an approval");
            }
        }
    }

    #[test]
    fn auto_approves_claude_control_requests_in_auto_mode() {
        let message = json!({
            "type": "control_request",
            "request_id": "req-auto",
            "request": {
                "subtype": "can_use_tool",
                "tool_name": "Write",
                "input": {
                    "file_path": "/tmp/auto.txt",
                    "content": "hi\n"
                },
                "decision_reason": "Write requires approval"
            }
        });
        let mut state = ClaudeTurnState::default();

        let action =
            classify_claude_control_request(&message, &mut state, ClaudeApprovalMode::AutoApprove)
                .unwrap()
                .unwrap();

        match action {
            ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Allow {
                request_id,
                updated_input,
            }) => {
                assert_eq!(request_id, "req-auto");
                assert_eq!(updated_input["file_path"], "/tmp/auto.txt");
            }
            ClaudeControlRequestAction::QueueApproval { .. } => {
                panic!("expected Claude auto mode to return an allow response");
            }
            ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Deny { .. }) => {
                panic!("expected Claude auto mode to allow the request");
            }
        }
    }

    #[test]
    fn encodes_claude_allow_permission_response() {
        let mut buffer = Vec::new();
        write_claude_permission_response(
            &mut buffer,
            &ClaudePermissionDecision::Allow {
                request_id: "req-allow".to_owned(),
                updated_input: json!({
                    "file_path": "/tmp/runtime.txt",
                    "content": "hi\n"
                }),
            },
        )
        .unwrap();

        let encoded = String::from_utf8(buffer).unwrap();
        let message: Value = serde_json::from_str(encoded.trim_end()).unwrap();

        assert_eq!(message["type"], "control_response");
        assert_eq!(message["response"]["request_id"], "req-allow");
        assert_eq!(message["response"]["response"]["behavior"], "allow");
        assert_eq!(
            message["response"]["response"]["updatedInput"]["file_path"],
            "/tmp/runtime.txt"
        );
    }

    #[test]
    fn encodes_claude_deny_permission_response() {
        let mut buffer = Vec::new();
        write_claude_permission_response(
            &mut buffer,
            &ClaudePermissionDecision::Deny {
                request_id: "req-deny".to_owned(),
                message: "User rejected this action in TermAl.".to_owned(),
            },
        )
        .unwrap();

        let encoded = String::from_utf8(buffer).unwrap();
        let message: Value = serde_json::from_str(encoded.trim_end()).unwrap();

        assert_eq!(message["type"], "control_response");
        assert_eq!(message["response"]["request_id"], "req-deny");
        assert_eq!(message["response"]["response"]["behavior"], "deny");
        assert_eq!(
            message["response"]["response"]["message"],
            "User rejected this action in TermAl."
        );
    }

    #[test]
    fn codex_agent_message_pushes_immediately_without_rollout_fallback() {
        let message = json!({
            "type": "item.completed",
            "item": {
                "type": "agent_message",
                "text": "Final answer from stdout"
            }
        });
        let mut recorder = TestRecorder::default();
        let mut session_id = None;

        handle_codex_event(&message, &mut session_id, &mut recorder, None).unwrap();

        assert_eq!(recorder.texts, vec!["Final answer from stdout".to_owned()]);
    }

    #[test]
    fn codex_agent_message_is_deferred_when_rollout_streaming_is_active() {
        let message = json!({
            "type": "item.completed",
            "item": {
                "type": "agent_message",
                "text": "Deferred stdout fallback"
            }
        });
        let mut recorder = TestRecorder::default();
        let mut session_id = None;
        let mut deferred = None;

        handle_codex_event(
            &message,
            &mut session_id,
            &mut recorder,
            Some(&mut deferred),
        )
        .unwrap();

        assert!(recorder.texts.is_empty());
        assert_eq!(deferred.as_deref(), Some("Deferred stdout fallback"));
    }

    #[test]
    fn extracts_codex_rollout_agent_message() {
        let message = json!({
            "type": "event_msg",
            "payload": {
                "type": "agent_message",
                "phase": "commentary",
                "message": "  Streaming from rollout  "
            }
        });

        assert_eq!(
            extract_codex_rollout_agent_message(&message),
            Some(("commentary".to_owned(), "Streaming from rollout".to_owned()))
        );
    }

    #[test]
    fn maps_codex_web_search_events_to_command_messages() {
        let mut recorder = TestRecorder::default();
        let mut session_id = None;

        let started = json!({
            "type": "item.started",
            "item": {
                "type": "web_search",
                "id": "ws_1",
                "query": "",
                "action": {
                    "type": "other"
                }
            }
        });
        handle_codex_event(&started, &mut session_id, &mut recorder, None).unwrap();

        let completed = json!({
            "type": "item.completed",
            "item": {
                "type": "web_search",
                "id": "ws_1",
                "query": "https://github.com/google-gemini/gemini-cli",
                "action": {
                    "type": "open_page",
                    "url": "https://github.com/google-gemini/gemini-cli"
                }
            }
        });
        handle_codex_event(&completed, &mut session_id, &mut recorder, None).unwrap();

        assert_eq!(
            recorder.commands,
            vec![
                (
                    "Web search".to_owned(),
                    String::new(),
                    CommandStatus::Running
                ),
                (
                    "Open page: https://github.com/google-gemini/gemini-cli".to_owned(),
                    "Opened https://github.com/google-gemini/gemini-cli".to_owned(),
                    CommandStatus::Success
                )
            ]
        );
    }

    #[test]
    fn summarizes_retryable_connectivity_errors_without_dumping_json() {
        let error = json!({
            "error": {
                "additionalDetails": "stream disconnected before completion: websocket closed by server before response.completed",
                "codexErrorInfo": {
                    "responseStreamDisconnected": {
                        "httpStatusCode": null
                    }
                },
                "message": "Reconnecting... 4/5"
            },
            "threadId": "thread_123",
            "turnId": "turn_123",
            "willRetry": true
        });

        assert_eq!(
            summarize_error(&error),
            "Connection dropped before the response finished. Retrying automatically (attempt 4 of 5)."
        );
    }

    #[test]
    fn retryable_connectivity_notifications_keep_codex_turn_active() {
        let state = test_app_state();
        let session_id = test_session_id(&state, Agent::Codex);
        let runtime_id = "runtime-retry".to_owned();
        let runtime = test_codex_runtime_handle(&runtime_id);
        state.set_codex_runtime(&session_id, runtime).unwrap();

        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(&session_id).unwrap();
            inner.sessions[index].session.status = SessionStatus::Active;
            inner.sessions[index].session.preview = "Waiting for activity.".to_owned();
            state.commit_locked(&mut inner).unwrap();
        }

        let notification = json!({
            "method": "error",
            "params": {
                "error": {
                    "additionalDetails": "stream disconnected before completion: websocket closed by server before response.completed",
                    "codexErrorInfo": {
                        "responseStreamDisconnected": {
                            "httpStatusCode": null
                        }
                    },
                    "message": "Reconnecting... 2/5"
                },
                "threadId": "thread_123",
                "turnId": "turn_123",
                "willRetry": true
            }
        });

        handle_codex_app_server_notification(
            "error",
            &notification,
            &state,
            &session_id,
            &RuntimeToken::Codex(runtime_id),
            &Arc::new(Mutex::new(None)),
            &mut CodexTurnState::default(),
            &mut SessionRecorder::new(state.clone(), session_id.clone()),
        )
        .unwrap();

        let snapshot = state.snapshot();
        let session = snapshot
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .unwrap();

        assert_eq!(session.status, SessionStatus::Active);
        assert_eq!(
            session.preview,
            make_preview(
                "Connection dropped before the response finished. Retrying automatically (attempt 2 of 5)."
            )
        );
        assert!(matches!(
            session.messages.last(),
            Some(Message::Text { text, .. })
                if text == "Connection dropped before the response finished. Retrying automatically (attempt 2 of 5)."
        ));
    }

    #[test]
    fn stores_codex_rate_limits_from_app_server_notifications() {
        let state = test_app_state();
        let session_id = test_session_id(&state, Agent::Codex);

        let notification = json!({
            "method": "account/rateLimits/updated",
            "params": {
                "rateLimits": {
                    "credits": null,
                    "limitId": "codex",
                    "limitName": null,
                    "planType": "plus",
                    "primary": {
                        "resetsAt": 1773205300_u64,
                        "usedPercent": 17_u64,
                        "windowDurationMins": 300_u64
                    },
                    "secondary": {
                        "resetsAt": 1773736282_u64,
                        "usedPercent": 29_u64,
                        "windowDurationMins": 10080_u64
                    }
                }
            }
        });

        handle_codex_app_server_notification(
            "account/rateLimits/updated",
            &notification,
            &state,
            &session_id,
            &RuntimeToken::Codex("runtime-rate-limit".to_owned()),
            &Arc::new(Mutex::new(None)),
            &mut CodexTurnState::default(),
            &mut SessionRecorder::new(state.clone(), session_id.clone()),
        )
        .unwrap();

        let snapshot = state.snapshot();
        assert_eq!(
            snapshot.codex.rate_limits,
            Some(CodexRateLimits {
                credits: None,
                limit_id: Some("codex".to_owned()),
                limit_name: None,
                plan_type: Some("plus".to_owned()),
                primary: Some(CodexRateLimitWindow {
                    resets_at: Some(1773205300),
                    used_percent: Some(17),
                    window_duration_mins: Some(300),
                }),
                secondary: Some(CodexRateLimitWindow {
                    resets_at: Some(1773736282),
                    used_percent: Some(29),
                    window_duration_mins: Some(10080),
                }),
            })
        );
    }

    #[test]
    fn seeds_termal_codex_home_with_auth_and_rules() {
        let root = std::env::temp_dir().join(format!("termal-codex-home-{}", Uuid::new_v4()));
        let source_home = root.join("source");
        let target_home = root.join("target");

        fs::create_dir_all(source_home.join("rules")).unwrap();
        fs::write(source_home.join("auth.json"), "{\"token\":\"abc\"}").unwrap();
        fs::write(source_home.join("config.toml"), "model = \"gpt-5\"").unwrap();
        fs::write(source_home.join("rules").join("team.md"), "be terse").unwrap();

        seed_termal_codex_home_from(&source_home, &target_home).unwrap();

        assert_eq!(
            fs::read_to_string(target_home.join("auth.json")).unwrap(),
            "{\"token\":\"abc\"}"
        );
        assert_eq!(
            fs::read_to_string(target_home.join("config.toml")).unwrap(),
            "model = \"gpt-5\""
        );
        assert_eq!(
            fs::read_to_string(target_home.join("rules").join("team.md")).unwrap(),
            "be terse"
        );

        fs::remove_dir_all(root).unwrap();
    }
}
