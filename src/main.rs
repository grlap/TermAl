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
        .route("/api/git/file", post(apply_git_file_action))
        .route("/api/state", get(get_state))
        .route("/api/events", get(state_events))
        .route("/api/projects", post(create_project))
        .route("/api/projects/pick", post(pick_project_root))
        .route("/api/sessions", post(create_session))
        .route("/api/sessions/{id}/settings", post(update_session_settings))
        .route(
            "/api/sessions/{id}/model-options/refresh",
            post(refresh_session_model_options),
        )
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
                codex_reasoning_effort: Some(default_codex_reasoning_effort()),
                codex_sandbox_mode: Some(default_codex_sandbox_mode()),
                agent,
                claude_approval_mode: Some(default_claude_approval_mode()),
                cwd: cwd.clone(),
                model: agent.default_model().to_owned(),
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
            let default_project = inner.create_project(None, default_workdir.clone());
            inner.create_session(
                Agent::Codex,
                Some("Codex Live".to_owned()),
                default_workdir.clone(),
                Some(default_project.id.clone()),
                None,
            );
            inner.create_session(
                Agent::Claude,
                Some("Claude Live".to_owned()),
                default_workdir.clone(),
                Some(default_project.id.clone()),
                None,
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
        self.snapshot_from_inner(&inner)
    }

    fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<CreateSessionResponse, ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let requested_workdir = request
            .workdir
            .as_deref()
            .map(resolve_session_workdir)
            .transpose()?;
        let project =
            if let Some(project_id) = request.project_id.as_deref() {
                Some(inner.find_project(project_id).cloned().ok_or_else(|| {
                    ApiError::bad_request(format!("unknown project `{project_id}`"))
                })?)
            } else {
                requested_workdir
                    .as_deref()
                    .and_then(|workdir| inner.find_project_for_workdir(workdir).cloned())
            };
        let workdir = requested_workdir.unwrap_or_else(|| {
            project
                .as_ref()
                .map(|entry| entry.root_path.clone())
                .unwrap_or_else(|| self.default_workdir.clone())
        });
        if let Some(project) = project.as_ref() {
            if !path_contains(&project.root_path, FsPath::new(&workdir)) {
                return Err(ApiError::bad_request(format!(
                    "session workdir `{workdir}` must stay inside project `{}`",
                    project.name
                )));
            }
        }
        let agent = request.agent.unwrap_or(Agent::Codex);
        validate_agent_session_setup(agent, &workdir)
            .map_err(|message| ApiError::bad_request(message))?;
        let mut record = inner.create_session(
            agent,
            request.name,
            workdir,
            project.as_ref().map(|entry| entry.id.clone()),
            request
                .model
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
        );
        if record.session.agent.supports_codex_prompt_settings() {
            if let Some(sandbox_mode) = request.sandbox_mode {
                record.codex_sandbox_mode = sandbox_mode;
                record.session.sandbox_mode = Some(sandbox_mode);
            }
            if let Some(approval_policy) = request.approval_policy {
                record.codex_approval_policy = approval_policy;
                record.session.approval_policy = Some(approval_policy);
            }
            if let Some(reasoning_effort) = request.reasoning_effort {
                record.codex_reasoning_effort = reasoning_effort;
                record.session.reasoning_effort = Some(reasoning_effort);
            }
        } else if record.session.agent.supports_claude_approval_mode() {
            if let Some(claude_approval_mode) = request.claude_approval_mode {
                record.session.claude_approval_mode = Some(claude_approval_mode);
            }
        } else if record.session.agent.supports_cursor_mode() {
            if let Some(cursor_mode) = request.cursor_mode {
                record.session.cursor_mode = Some(cursor_mode);
            }
        } else if record.session.agent.supports_gemini_approval_mode() {
            if let Some(gemini_approval_mode) = request.gemini_approval_mode {
                record.session.gemini_approval_mode = Some(gemini_approval_mode);
            }
        }
        match record.session.agent {
            agent if agent.supports_codex_prompt_settings() => {
                if request.claude_approval_mode.is_some()
                    || request.cursor_mode.is_some()
                    || request.gemini_approval_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Codex sessions only support model, sandbox, approval policy, and reasoning effort settings",
                    ));
                }
            }
            agent if agent.supports_claude_approval_mode() => {
                if request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.cursor_mode.is_some()
                    || request.gemini_approval_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Claude sessions only support model and mode settings",
                    ));
                }
            }
            agent if agent.supports_cursor_mode() => {
                if request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.claude_approval_mode.is_some()
                    || request.gemini_approval_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Cursor sessions only support mode settings",
                    ));
                }
            }
            agent if agent.supports_gemini_approval_mode() => {
                if request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.claude_approval_mode.is_some()
                    || request.cursor_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Gemini sessions only support approval mode settings",
                    ));
                }
            }
            _ => {}
        }
        if let Some(slot) = inner
            .find_session_index(&record.session.id)
            .and_then(|index| inner.sessions.get_mut(index))
        {
            *slot = record.clone();
        }
        self.commit_locked(&mut inner)
            .map_err(|err| ApiError::internal(format!("failed to persist session: {err:#}")))?;
        Ok(CreateSessionResponse {
            session_id: record.session.id,
            state: self.snapshot_from_inner(&inner),
        })
    }

    fn create_project(
        &self,
        request: CreateProjectRequest,
    ) -> Result<CreateProjectResponse, ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let root_path = resolve_project_root_path(&request.root_path)?;
        let existing_len = inner.projects.len();
        let project = inner.create_project(request.name, root_path);
        if inner.projects.len() != existing_len {
            self.commit_locked(&mut inner)
                .map_err(|err| ApiError::internal(format!("failed to persist project: {err:#}")))?;
        }
        Ok(CreateProjectResponse {
            project_id: project.id,
            state: self.snapshot_from_inner(&inner),
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
        let payload = serde_json::to_string(&self.snapshot_from_inner(inner))
            .context("failed to serialize session snapshot")?;
        let _ = self.state_events.send(payload);
        Ok(())
    }

    fn snapshot_from_inner(&self, inner: &StateInner) -> StateResponse {
        StateResponse {
            revision: inner.revision,
            codex: inner.codex.clone(),
            agent_readiness: collect_agent_readiness(&self.default_workdir),
            projects: inner.projects.clone(),
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
                if record.runtime_reset_required {
                    if let SessionRuntime::Claude(handle) = &record.runtime {
                        handle.kill().map_err(|err| {
                            ApiError::internal(format!(
                                "failed to restart Claude session runtime: {err:#}"
                            ))
                        })?;
                    }
                    record.runtime = SessionRuntime::None;
                    record.pending_claude_approvals.clear();
                    record.runtime_reset_required = false;
                }

                let handle = match &record.runtime {
                    SessionRuntime::Claude(handle) => handle.clone(),
                    SessionRuntime::Codex(_) => {
                        return Err(ApiError::internal(
                            "unexpected Codex runtime attached to Claude session",
                        ));
                    }
                    SessionRuntime::Acp(_) => {
                        return Err(ApiError::internal(
                            "unexpected ACP runtime attached to Claude session",
                        ));
                    }
                    SessionRuntime::None => {
                        let handle = spawn_claude_runtime(
                            self.clone(),
                            record.session.id.clone(),
                            record.session.workdir.clone(),
                            record.session.model.clone(),
                            record
                                .session
                                .claude_approval_mode
                                .unwrap_or_else(default_claude_approval_mode),
                            record.external_session_id.clone(),
                            None,
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
                if record.runtime_reset_required {
                    if let SessionRuntime::Codex(handle) = &record.runtime {
                        handle.kill().map_err(|err| {
                            ApiError::internal(format!(
                                "failed to restart Codex session runtime: {err:#}"
                            ))
                        })?;
                    }
                    record.runtime = SessionRuntime::None;
                    record.pending_codex_approvals.clear();
                    record.runtime_reset_required = false;
                }

                let handle = match &record.runtime {
                    SessionRuntime::Codex(handle) => handle.clone(),
                    SessionRuntime::Claude(_) => {
                        return Err(ApiError::internal(
                            "unexpected Claude runtime attached to Codex session",
                        ));
                    }
                    SessionRuntime::Acp(_) => {
                        return Err(ApiError::internal(
                            "unexpected ACP runtime attached to Codex session",
                        ));
                    }
                    SessionRuntime::None => {
                        let handle = spawn_codex_runtime(
                            self.clone(),
                            record.session.id.clone(),
                            record.session.workdir.clone(),
                        )
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
                        model: record.session.model.clone(),
                        prompt: prompt.to_owned(),
                        reasoning_effort: record.codex_reasoning_effort,
                        resume_thread_id: record.external_session_id.clone(),
                        sandbox_mode: record.codex_sandbox_mode,
                    },
                    sender: handle.input_tx,
                    session_id: record.session.id.clone(),
                }
            }
            agent @ (Agent::Cursor | Agent::Gemini) => {
                if !attachments.is_empty() {
                    return Err(ApiError::bad_request(format!(
                        "{} sessions do not support image attachments yet",
                        agent.name()
                    )));
                }

                if record.runtime_reset_required {
                    if let SessionRuntime::Acp(handle) = &record.runtime {
                        handle.kill().map_err(|err| {
                            ApiError::internal(format!(
                                "failed to restart {} session runtime: {err:#}",
                                agent.name()
                            ))
                        })?;
                    }
                    record.runtime = SessionRuntime::None;
                    record.pending_acp_approvals.clear();
                    record.runtime_reset_required = false;
                }

                let expected_acp_agent = agent
                    .acp_runtime()
                    .ok_or_else(|| ApiError::internal("missing ACP runtime config"))?;
                let handle = match &record.runtime {
                    SessionRuntime::Acp(handle) if handle.agent == expected_acp_agent => {
                        handle.clone()
                    }
                    SessionRuntime::Acp(_) => {
                        return Err(ApiError::internal(
                            "unexpected ACP runtime attached to session",
                        ));
                    }
                    SessionRuntime::Claude(_) => {
                        return Err(ApiError::internal(
                            "unexpected Claude runtime attached to ACP session",
                        ));
                    }
                    SessionRuntime::Codex(_) => {
                        return Err(ApiError::internal(
                            "unexpected Codex runtime attached to ACP session",
                        ));
                    }
                    SessionRuntime::None => {
                        let handle = spawn_acp_runtime(
                            self.clone(),
                            record.session.id.clone(),
                            record.session.workdir.clone(),
                            expected_acp_agent,
                            record.session.gemini_approval_mode,
                        )
                        .map_err(|err| {
                            ApiError::internal(format!(
                                "failed to start persistent {} session: {err:#}",
                                agent.name()
                            ))
                        })?;
                        record.runtime = SessionRuntime::Acp(handle.clone());
                        handle
                    }
                };

                TurnDispatch::PersistentAcp {
                    command: AcpPromptCommand {
                        cwd: record.session.workdir.clone(),
                        cursor_mode: record.session.cursor_mode,
                        model: record.session.model.clone(),
                        prompt: prompt.to_owned(),
                        resume_session_id: record.external_session_id.clone(),
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
        let mut claude_model_update: Option<(ClaudeRuntimeHandle, String)> = None;
        let mut claude_permission_mode_update: Option<(ClaudeRuntimeHandle, String)> = None;
        let mut acp_model_update: Option<(AcpRuntimeHandle, Value)> = None;

        match record.session.agent {
            agent if agent.supports_codex_prompt_settings() => {
                if request.claude_approval_mode.is_some() {
                    return Err(ApiError::bad_request(
                        "Claude approval mode can only be changed for Claude sessions",
                    ));
                }
                if request.cursor_mode.is_some()
                    || request.gemini_approval_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Codex sessions do not support Cursor or Gemini settings",
                    ));
                }
            }
            agent if agent.supports_claude_approval_mode() => {
                if request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.cursor_mode.is_some()
                    || request.gemini_approval_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Claude sessions only support model and mode settings",
                    ));
                }
            }
            agent if agent.supports_cursor_mode() => {
                if request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.claude_approval_mode.is_some()
                    || request.gemini_approval_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Cursor sessions only support model and mode settings",
                    ));
                }
            }
            agent if agent.supports_gemini_approval_mode() => {
                if request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.claude_approval_mode.is_some()
                    || request.cursor_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Gemini sessions only support model and approval mode settings",
                    ));
                }
            }
            agent => {
                if request.model.is_some()
                    || request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.claude_approval_mode.is_some()
                    || request.cursor_mode.is_some()
                    || request.gemini_approval_mode.is_some()
                {
                    return Err(ApiError::bad_request(format!(
                        "{} sessions do not support prompt settings yet",
                        agent.name()
                    )));
                }
            }
        }

        if let Some(name) = request.name.as_deref() {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                return Err(ApiError::bad_request("session name cannot be empty"));
            }
            record.session.name = trimmed.to_owned();
        }

        if let Some(model) = request.model.as_deref() {
            let trimmed = model.trim();
            if trimmed.is_empty() {
                return Err(ApiError::bad_request("session model cannot be empty"));
            }
        }

        match record.session.agent {
            agent if agent.supports_codex_prompt_settings() => {
                if let Some(model) = request.model.as_deref() {
                    let trimmed = model.trim().to_owned();
                    if record.session.model != trimmed {
                        record.session.model = trimmed;
                    }
                }
                if let Some(sandbox_mode) = request.sandbox_mode {
                    record.codex_sandbox_mode = sandbox_mode;
                    record.session.sandbox_mode = Some(sandbox_mode);
                }
                if let Some(approval_policy) = request.approval_policy {
                    record.codex_approval_policy = approval_policy;
                    record.session.approval_policy = Some(approval_policy);
                }
                if let Some(reasoning_effort) = request.reasoning_effort {
                    record.codex_reasoning_effort = reasoning_effort;
                    record.session.reasoning_effort = Some(reasoning_effort);
                }
            }
            agent if agent.supports_claude_approval_mode() => {
                if let Some(model) = request.model.as_deref() {
                    let trimmed = model.trim().to_owned();
                    if record.session.model != trimmed {
                        record.session.model = trimmed.clone();
                        if let SessionRuntime::Claude(handle) = &record.runtime {
                            claude_model_update = Some((handle.clone(), trimmed));
                        }
                    }
                }
                if let Some(claude_approval_mode) = request.claude_approval_mode {
                    record.session.claude_approval_mode = Some(claude_approval_mode);
                    if let SessionRuntime::Claude(handle) = &record.runtime {
                        claude_permission_mode_update = Some((
                            handle.clone(),
                            claude_approval_mode
                                .session_cli_permission_mode()
                                .to_owned(),
                        ));
                    }
                }
            }
            agent if agent.supports_cursor_mode() => {
                if let Some(model) = request.model.as_deref() {
                    let trimmed = model.trim().to_owned();
                    record.session.model = trimmed.clone();
                    if let (SessionRuntime::Acp(handle), Some(external_session_id)) =
                        (&record.runtime, record.external_session_id.as_deref())
                    {
                        acp_model_update = Some((
                            handle.clone(),
                            json!({
                                "id": Uuid::new_v4().to_string(),
                                "method": "session/set_config_option",
                                "params": {
                                    "sessionId": external_session_id,
                                    "optionId": "model",
                                    "value": trimmed,
                                }
                            }),
                        ));
                    }
                }
                if let Some(cursor_mode) = request.cursor_mode {
                    record.session.cursor_mode = Some(cursor_mode);
                }
            }
            agent if agent.supports_gemini_approval_mode() => {
                if let Some(model) = request.model.as_deref() {
                    record.session.model = model.trim().to_owned();
                }
                if let Some(gemini_approval_mode) = request.gemini_approval_mode {
                    if record.session.gemini_approval_mode != Some(gemini_approval_mode) {
                        record.runtime_reset_required = true;
                    }
                    record.session.gemini_approval_mode = Some(gemini_approval_mode);
                }
            }
            _ => {}
        }

        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;
        let snapshot = self.snapshot_from_inner(&inner);
        drop(inner);

        if let Some((handle, model)) = claude_model_update {
            let _ = handle.input_tx.send(ClaudeRuntimeCommand::SetModel(model));
        }
        if let Some((handle, permission_mode)) = claude_permission_mode_update {
            let _ = handle
                .input_tx
                .send(ClaudeRuntimeCommand::SetPermissionMode(permission_mode));
        }
        if let Some((handle, request)) = acp_model_update {
            let _ = handle
                .input_tx
                .send(AcpRuntimeCommand::JsonRpcMessage(request));
        }

        Ok(snapshot)
    }

    fn refresh_session_model_options(
        &self,
        session_id: &str,
    ) -> std::result::Result<StateResponse, ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = &mut inner.sessions[index];
        let agent = record.session.agent;
        if agent == Agent::Claude {
            if !record.session.model_options.is_empty() {
                return Ok(self.snapshot_from_inner(&inner));
            }

            if record.runtime_reset_required {
                if let SessionRuntime::Claude(handle) = &record.runtime {
                    handle.kill().map_err(|err| {
                        ApiError::internal(format!(
                            "failed to restart Claude session runtime: {err:#}"
                        ))
                    })?;
                }
                record.runtime = SessionRuntime::None;
                record.pending_claude_approvals.clear();
                record.runtime_reset_required = false;
            }

            match &record.runtime {
                SessionRuntime::Acp(_) => {
                    return Err(ApiError::internal(
                        "unexpected ACP runtime attached to Claude session",
                    ));
                }
                SessionRuntime::Codex(_) => {
                    return Err(ApiError::internal(
                        "unexpected Codex runtime attached to Claude session",
                    ));
                }
                SessionRuntime::Claude(_) => {
                    let model_options = record.session.model_options.clone();
                    let current_model = record.session.model.clone();
                    drop(inner);
                    self.sync_session_model_options(session_id, Some(current_model), model_options)
                        .map_err(|err| {
                            ApiError::internal(format!(
                                "failed to sync Claude model options: {err:#}"
                            ))
                        })?;
                    return Ok(self.snapshot());
                }
                SessionRuntime::None => {}
            }

            let (response_tx, response_rx) =
                mpsc::channel::<std::result::Result<Vec<SessionModelOption>, String>>();
            let handle = spawn_claude_runtime(
                self.clone(),
                record.session.id.clone(),
                record.session.workdir.clone(),
                record.session.model.clone(),
                record
                    .session
                    .claude_approval_mode
                    .unwrap_or_else(default_claude_approval_mode),
                record.external_session_id.clone(),
                Some(response_tx),
            )
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to start persistent Claude session: {err:#}"
                ))
            })?;
            record.runtime = SessionRuntime::Claude(handle);
            drop(inner);

            let model_options = match response_rx.recv_timeout(Duration::from_secs(30)) {
                Ok(Ok(model_options)) => model_options,
                Ok(Err(detail)) => {
                    return Err(ApiError::internal(format!(
                        "failed to refresh Claude model options: {detail}"
                    )));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(ApiError::internal(
                        "timed out refreshing Claude model options".to_owned(),
                    ));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(ApiError::internal(
                        "Claude model refresh did not return a result".to_owned(),
                    ));
                }
            };

            self.sync_session_model_options(session_id, None, model_options)
                .map_err(|err| {
                    ApiError::internal(format!("failed to sync Claude model options: {err:#}"))
                })?;
            return Ok(self.snapshot());
        }

        if agent == Agent::Codex {
            if record.runtime_reset_required {
                if let SessionRuntime::Codex(handle) = &record.runtime {
                    handle.kill().map_err(|err| {
                        ApiError::internal(format!(
                            "failed to restart Codex session runtime: {err:#}"
                        ))
                    })?;
                }
                record.runtime = SessionRuntime::None;
                record.pending_codex_approvals.clear();
                record.runtime_reset_required = false;
            }

            let handle = match &record.runtime {
                SessionRuntime::Codex(handle) => handle.clone(),
                SessionRuntime::Acp(_) => {
                    return Err(ApiError::internal(
                        "unexpected ACP runtime attached to Codex session",
                    ));
                }
                SessionRuntime::Claude(_) => {
                    return Err(ApiError::internal(
                        "unexpected Claude runtime attached to Codex session",
                    ));
                }
                SessionRuntime::None => {
                    let handle = spawn_codex_runtime(
                        self.clone(),
                        record.session.id.clone(),
                        record.session.workdir.clone(),
                    )
                    .map_err(|err| {
                        ApiError::internal(format!(
                            "failed to start persistent Codex session: {err:#}"
                        ))
                    })?;
                    record.runtime = SessionRuntime::Codex(handle.clone());
                    handle
                }
            };
            drop(inner);

            let (response_tx, response_rx) =
                mpsc::channel::<std::result::Result<Vec<SessionModelOption>, String>>();
            handle
                .input_tx
                .send(CodexRuntimeCommand::RefreshModelList { response_tx })
                .map_err(|err| {
                    ApiError::internal(format!("failed to queue Codex model refresh: {err}"))
                })?;

            let model_options = match response_rx.recv_timeout(Duration::from_secs(30)) {
                Ok(Ok(model_options)) => model_options,
                Ok(Err(detail)) => {
                    return Err(ApiError::internal(format!(
                        "failed to refresh Codex model options: {detail}"
                    )));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(ApiError::internal(
                        "timed out refreshing Codex model options".to_owned(),
                    ));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(ApiError::internal(
                        "Codex model refresh did not return a result".to_owned(),
                    ));
                }
            };

            self.sync_session_model_options(session_id, None, model_options)
                .map_err(|err| {
                    ApiError::internal(format!("failed to sync Codex model options: {err:#}"))
                })?;
            return Ok(self.snapshot());
        }

        let expected_acp_agent = agent.acp_runtime().ok_or_else(|| {
            ApiError::bad_request(format!(
                "{} sessions do not expose live model options",
                agent.name()
            ))
        })?;

        if record.runtime_reset_required {
            if let SessionRuntime::Acp(handle) = &record.runtime {
                handle.kill().map_err(|err| {
                    ApiError::internal(format!(
                        "failed to restart {} session runtime: {err:#}",
                        agent.name()
                    ))
                })?;
            }
            record.runtime = SessionRuntime::None;
            record.pending_acp_approvals.clear();
            record.runtime_reset_required = false;
        }

        let handle = match &record.runtime {
            SessionRuntime::Acp(handle) if handle.agent == expected_acp_agent => handle.clone(),
            SessionRuntime::Acp(_) => {
                return Err(ApiError::internal(
                    "unexpected ACP runtime attached to session",
                ));
            }
            SessionRuntime::Claude(_) => {
                return Err(ApiError::internal(
                    "unexpected Claude runtime attached to ACP session",
                ));
            }
            SessionRuntime::Codex(_) => {
                return Err(ApiError::internal(
                    "unexpected Codex runtime attached to ACP session",
                ));
            }
            SessionRuntime::None => {
                let handle = spawn_acp_runtime(
                    self.clone(),
                    record.session.id.clone(),
                    record.session.workdir.clone(),
                    expected_acp_agent,
                    record.session.gemini_approval_mode,
                )
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to start persistent {} session: {err:#}",
                        agent.name()
                    ))
                })?;
                record.runtime = SessionRuntime::Acp(handle.clone());
                handle
            }
        };

        let command = AcpPromptCommand {
            cwd: record.session.workdir.clone(),
            cursor_mode: record.session.cursor_mode,
            model: record.session.model.clone(),
            prompt: String::new(),
            resume_session_id: record.external_session_id.clone(),
        };
        drop(inner);

        let (response_tx, response_rx) = mpsc::channel::<std::result::Result<(), String>>();
        handle
            .input_tx
            .send(AcpRuntimeCommand::RefreshSessionConfig {
                command,
                response_tx,
            })
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to queue {} model refresh: {err}",
                    agent.name()
                ))
            })?;

        match response_rx.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok(())) => Ok(self.snapshot()),
            Ok(Err(detail)) => Err(ApiError::internal(format!(
                "failed to refresh {} model options: {detail}",
                agent.name()
            ))),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(ApiError::internal(format!(
                "timed out refreshing {} model options",
                agent.name()
            ))),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ApiError::internal(format!(
                "{} model refresh did not return a result",
                agent.name()
            ))),
        }
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

    fn sync_session_model_options(
        &self,
        session_id: &str,
        current_model: Option<String>,
        model_options: Vec<SessionModelOption>,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let session = &mut inner.sessions[index].session;

        let mut changed = false;
        if let Some(current_model) = current_model
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
        {
            if session.model != current_model {
                session.model = current_model;
                changed = true;
            }
        }
        if session.model_options != model_options {
            session.model_options = model_options;
            changed = true;
        }

        if changed {
            self.commit_locked(&mut inner)?;
        }
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
        reasoning_effort: CodexReasoningEffort,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index].active_codex_sandbox_mode = Some(sandbox_mode);
        inner.sessions[index].active_codex_approval_policy = Some(approval_policy);
        inner.sessions[index].active_codex_reasoning_effort = Some(reasoning_effort);
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
        inner.sessions[index].runtime_reset_required = false;
        inner.sessions[index].pending_claude_approvals.clear();
        inner.sessions[index].pending_codex_approvals.clear();
        inner.sessions[index].pending_acp_approvals.clear();
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
            record.runtime_reset_required = false;
            record.pending_claude_approvals.clear();
            record.pending_codex_approvals.clear();
            record.pending_acp_approvals.clear();

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
                        RuntimeToken::Acp(_) => {
                            "Agent session exited before the active turn completed".to_owned()
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

    fn register_acp_pending_approval(
        &self,
        session_id: &str,
        message_id: String,
        approval: AcpPendingApproval,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index]
            .pending_acp_approvals
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
                SessionRuntime::Acp(handle) => Some(KillableRuntime::Acp(handle.clone())),
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
        Ok(self.snapshot_from_inner(&inner))
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
        Ok(self.snapshot_from_inner(&inner))
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
                SessionRuntime::Acp(handle) => KillableRuntime::Acp(handle.clone()),
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
        let mut acp_runtime_action: Option<(AcpRuntimeHandle, AcpPendingApproval)> = None;
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
                SessionRuntime::Acp(_) => {
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
                SessionRuntime::Claude(_) | SessionRuntime::None | SessionRuntime::Acp(_) => {
                    return Err(ApiError::conflict("Codex session is not currently running"));
                }
            };
            codex_runtime_action = Some((handle, pending));
        } else if matches!(session.agent, Agent::Cursor | Agent::Gemini)
            && matches!(
                decision,
                ApprovalDecision::Accepted
                    | ApprovalDecision::AcceptedForSession
                    | ApprovalDecision::Rejected
            )
        {
            let pending = record
                .pending_acp_approvals
                .get(message_id)
                .cloned()
                .ok_or_else(|| ApiError::conflict("approval request is no longer live"))?;
            let handle = match &record.runtime {
                SessionRuntime::Acp(handle) => handle.clone(),
                SessionRuntime::Claude(_) | SessionRuntime::Codex(_) | SessionRuntime::None => {
                    return Err(ApiError::conflict("agent session is not currently running"));
                }
            };
            acp_runtime_action = Some((handle, pending));
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
        if let Some((handle, pending)) = acp_runtime_action {
            let option_id = match decision {
                ApprovalDecision::Accepted => pending
                    .allow_once_option_id
                    .clone()
                    .or_else(|| pending.allow_always_option_id.clone())
                    .or_else(|| pending.reject_option_id.clone()),
                ApprovalDecision::AcceptedForSession => pending
                    .allow_always_option_id
                    .clone()
                    .or_else(|| pending.allow_once_option_id.clone())
                    .or_else(|| pending.reject_option_id.clone()),
                ApprovalDecision::Rejected => pending
                    .reject_option_id
                    .clone()
                    .or_else(|| pending.allow_once_option_id.clone())
                    .or_else(|| pending.allow_always_option_id.clone()),
                ApprovalDecision::Pending => None,
            }
            .ok_or_else(|| {
                ApiError::conflict("no approval option is available for this request")
            })?;

            handle
                .input_tx
                .send(AcpRuntimeCommand::JsonRpcMessage(json!({
                    "id": pending.request_id.clone(),
                    "result": {
                        "outcome": {
                            "outcome": "selected",
                            "optionId": option_id,
                        }
                    }
                })))
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to deliver approval response to agent session: {err}"
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

        if decision != ApprovalDecision::Pending {
            record.pending_claude_approvals.remove(message_id);
            record.pending_codex_approvals.remove(message_id);
            record.pending_acp_approvals.remove(message_id);
        }
        if session.status == SessionStatus::Approval {
            session.status = if decision == ApprovalDecision::Pending {
                SessionStatus::Approval
            } else {
                SessionStatus::Active
            };
        }
        if session.status == SessionStatus::Approval || session.status == SessionStatus::Active {
            let agent_name = session.agent.name();
            session.preview = match decision {
                ApprovalDecision::Pending => "Approval pending.".to_owned(),
                ApprovalDecision::Accepted => {
                    format!("Approval granted. {agent_name} is continuing…")
                }
                ApprovalDecision::AcceptedForSession => {
                    format!("Approval granted for this session. {agent_name} is continuing…")
                }
                ApprovalDecision::Rejected => {
                    format!("Approval rejected. {agent_name} is continuing…")
                }
            };
        }

        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;
        Ok(self.snapshot_from_inner(&inner))
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
    next_project_number: usize,
    next_session_number: usize,
    next_message_number: u64,
    projects: Vec<Project>,
    sessions: Vec<SessionRecord>,
}

impl StateInner {
    fn new() -> Self {
        Self {
            codex: CodexState::default(),
            revision: 0,
            next_project_number: 1,
            next_session_number: 1,
            next_message_number: 1,
            projects: Vec::new(),
            sessions: Vec::new(),
        }
    }

    fn create_project(&mut self, name: Option<String>, root_path: String) -> Project {
        if let Some(existing) = self
            .projects
            .iter()
            .find(|project| project.root_path == root_path)
            .cloned()
        {
            return existing;
        }

        let number = self.next_project_number;
        self.next_project_number += 1;
        let base_name = name.unwrap_or_else(|| default_project_name(&root_path));
        let project = Project {
            id: format!("project-{number}"),
            name: dedupe_project_name(&self.projects, &base_name),
            root_path,
        };
        self.projects.push(project.clone());
        project
    }

    fn create_session(
        &mut self,
        agent: Agent,
        name: Option<String>,
        workdir: String,
        project_id: Option<String>,
        model: Option<String>,
    ) -> SessionRecord {
        let number = self.next_session_number;
        self.next_session_number += 1;

        let record = SessionRecord {
            active_codex_approval_policy: None,
            active_codex_reasoning_effort: None,
            active_codex_sandbox_mode: None,
            codex_approval_policy: default_codex_approval_policy(),
            codex_reasoning_effort: default_codex_reasoning_effort(),
            codex_sandbox_mode: default_codex_sandbox_mode(),
            external_session_id: None,
            pending_claude_approvals: HashMap::new(),
            pending_codex_approvals: HashMap::new(),
            pending_acp_approvals: HashMap::new(),
            queued_prompts: VecDeque::new(),
            runtime: SessionRuntime::None,
            runtime_reset_required: false,
            session: Session {
                id: format!("session-{number}"),
                name: name.unwrap_or_else(|| format!("{} {}", agent.name(), number)),
                emoji: agent.avatar().to_owned(),
                agent,
                workdir,
                project_id,
                model: model.unwrap_or_else(|| agent.default_model().to_owned()),
                model_options: Vec::new(),
                approval_policy: None,
                reasoning_effort: None,
                sandbox_mode: None,
                cursor_mode: agent
                    .supports_cursor_mode()
                    .then_some(default_cursor_mode()),
                claude_approval_mode: agent
                    .supports_claude_approval_mode()
                    .then_some(default_claude_approval_mode()),
                gemini_approval_mode: agent
                    .supports_gemini_approval_mode()
                    .then_some(default_gemini_approval_mode()),
                external_session_id: None,
                status: SessionStatus::Idle,
                preview: "Ready for a prompt.".to_owned(),
                messages: Vec::new(),
                pending_prompts: Vec::new(),
            },
        };

        let mut record = record;
        if record.session.agent.supports_codex_prompt_settings() {
            record.session.approval_policy = Some(record.codex_approval_policy);
            record.session.reasoning_effort = Some(record.codex_reasoning_effort);
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

    fn find_project(&self, project_id: &str) -> Option<&Project> {
        self.projects
            .iter()
            .find(|project| project.id == project_id)
    }

    fn find_project_for_workdir(&self, workdir: &str) -> Option<&Project> {
        let target = FsPath::new(workdir);
        self.projects
            .iter()
            .filter(|project| path_contains(&project.root_path, target))
            .max_by_key(|project| project.root_path.len())
    }

    fn ensure_projects_consistent(&mut self) {
        let highest_project_number = self
            .projects
            .iter()
            .filter_map(|project| {
                project
                    .id
                    .strip_prefix("project-")
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .max()
            .unwrap_or(0);
        self.next_project_number = self
            .next_project_number
            .max(highest_project_number.saturating_add(1))
            .max(1);

        for index in 0..self.sessions.len() {
            let existing_project_id = self.sessions[index].session.project_id.clone();
            if existing_project_id
                .as_deref()
                .and_then(|project_id| self.find_project(project_id))
                .is_some()
            {
                continue;
            }

            let workdir = self.sessions[index].session.workdir.clone();
            let project_id = self
                .find_project_for_workdir(&workdir)
                .map(|project| project.id.clone())
                .unwrap_or_else(|| self.create_project(None, workdir).id);
            self.sessions[index].session.project_id = Some(project_id);
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedState {
    #[serde(default, skip_serializing_if = "CodexState::is_empty")]
    codex: CodexState,
    #[serde(default)]
    revision: u64,
    #[serde(default)]
    next_project_number: usize,
    next_session_number: usize,
    next_message_number: u64,
    #[serde(default)]
    projects: Vec<Project>,
    sessions: Vec<PersistedSessionRecord>,
}

impl PersistedState {
    fn from_inner(inner: &StateInner) -> Self {
        Self {
            codex: inner.codex.clone(),
            revision: inner.revision,
            next_project_number: inner.next_project_number,
            next_session_number: inner.next_session_number,
            next_message_number: inner.next_message_number,
            projects: inner.projects.clone(),
            sessions: inner
                .sessions
                .iter()
                .map(PersistedSessionRecord::from_record)
                .collect(),
        }
    }

    fn into_inner(self) -> StateInner {
        let mut inner = StateInner {
            codex: self.codex,
            revision: self.revision,
            next_project_number: self.next_project_number.max(1),
            next_session_number: self.next_session_number,
            next_message_number: self.next_message_number,
            projects: self.projects,
            sessions: self
                .sessions
                .into_iter()
                .map(PersistedSessionRecord::into_record)
                .collect(),
        };
        inner.ensure_projects_consistent();
        inner
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedSessionRecord {
    active_codex_approval_policy: Option<CodexApprovalPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_codex_reasoning_effort: Option<CodexReasoningEffort>,
    active_codex_sandbox_mode: Option<CodexSandboxMode>,
    codex_approval_policy: CodexApprovalPolicy,
    #[serde(default = "default_codex_reasoning_effort")]
    codex_reasoning_effort: CodexReasoningEffort,
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
            active_codex_reasoning_effort: record.active_codex_reasoning_effort,
            active_codex_sandbox_mode: record.active_codex_sandbox_mode,
            codex_approval_policy: record.codex_approval_policy,
            codex_reasoning_effort: record.codex_reasoning_effort,
            codex_sandbox_mode: record.codex_sandbox_mode,
            external_session_id: record.external_session_id.clone(),
            queued_prompts: record.queued_prompts.clone(),
            session,
        }
    }

    fn into_record(self) -> SessionRecord {
        let mut session = self.session;
        session.external_session_id = self.external_session_id.clone();
        if session.agent.acp_runtime().is_none() {
            session.model_options.clear();
        }
        if session.agent.supports_cursor_mode() {
            session.cursor_mode.get_or_insert_with(default_cursor_mode);
        } else {
            session.cursor_mode = None;
        }
        if session.agent.supports_claude_approval_mode() {
            session
                .claude_approval_mode
                .get_or_insert_with(default_claude_approval_mode);
        } else {
            session.claude_approval_mode = None;
        }
        if session.agent.supports_gemini_approval_mode() {
            session
                .gemini_approval_mode
                .get_or_insert_with(default_gemini_approval_mode);
        } else {
            session.gemini_approval_mode = None;
        }
        if session.agent.supports_codex_prompt_settings() {
            session
                .reasoning_effort
                .get_or_insert_with(default_codex_reasoning_effort);
        } else {
            session.reasoning_effort = None;
        }
        session.pending_prompts.clear();

        let mut record = SessionRecord {
            active_codex_approval_policy: self.active_codex_approval_policy,
            active_codex_reasoning_effort: self.active_codex_reasoning_effort,
            active_codex_sandbox_mode: self.active_codex_sandbox_mode,
            codex_approval_policy: self.codex_approval_policy,
            codex_reasoning_effort: self.codex_reasoning_effort,
            codex_sandbox_mode: self.codex_sandbox_mode,
            external_session_id: self.external_session_id,
            pending_claude_approvals: HashMap::new(),
            pending_codex_approvals: HashMap::new(),
            pending_acp_approvals: HashMap::new(),
            queued_prompts: self.queued_prompts,
            runtime: SessionRuntime::None,
            runtime_reset_required: false,
            session,
        };
        sync_pending_prompts(&mut record);
        record
    }
}

#[derive(Clone)]
struct SessionRecord {
    active_codex_approval_policy: Option<CodexApprovalPolicy>,
    active_codex_reasoning_effort: Option<CodexReasoningEffort>,
    active_codex_sandbox_mode: Option<CodexSandboxMode>,
    codex_approval_policy: CodexApprovalPolicy,
    codex_reasoning_effort: CodexReasoningEffort,
    codex_sandbox_mode: CodexSandboxMode,
    external_session_id: Option<String>,
    pending_claude_approvals: HashMap<String, ClaudePendingApproval>,
    pending_codex_approvals: HashMap<String, CodexPendingApproval>,
    pending_acp_approvals: HashMap<String, AcpPendingApproval>,
    queued_prompts: VecDeque<QueuedPromptRecord>,
    runtime: SessionRuntime,
    runtime_reset_required: bool,
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
    Acp(AcpRuntimeHandle),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AcpAgent {
    Cursor,
    Gemini,
}

impl AcpAgent {
    fn agent(self) -> Agent {
        match self {
            Self::Cursor => Agent::Cursor,
            Self::Gemini => Agent::Gemini,
        }
    }

    fn command(self, launch_options: AcpLaunchOptions) -> Result<Command> {
        match self {
            Self::Cursor => {
                let exe = find_command_on_path("cursor-agent")
                    .ok_or_else(|| anyhow!("`cursor-agent` was not found on PATH"))?;
                let mut command = Command::new(exe);
                command.arg("acp");
                Ok(command)
            }
            Self::Gemini => {
                let exe = find_command_on_path("gemini")
                    .ok_or_else(|| anyhow!("`gemini` was not found on PATH"))?;
                let mut command = Command::new(exe);
                command.arg("--acp");
                if let Some(approval_mode) = launch_options.gemini_approval_mode {
                    command.args(["--approval-mode", approval_mode.as_cli_value()]);
                }
                Ok(command)
            }
        }
    }

    fn label(self) -> &'static str {
        self.agent().name()
    }
}

#[derive(Clone)]
struct AcpRuntimeHandle {
    agent: AcpAgent,
    runtime_id: String,
    input_tx: Sender<AcpRuntimeCommand>,
    process: Arc<Mutex<Child>>,
}

impl AcpRuntimeHandle {
    fn kill(&self) -> Result<()> {
        kill_child_process(&self.process, self.agent.label())
    }
}

#[derive(Clone)]
enum KillableRuntime {
    Claude(ClaudeRuntimeHandle),
    Codex(CodexRuntimeHandle),
    Acp(AcpRuntimeHandle),
}

impl KillableRuntime {
    fn kill(&self) -> Result<()> {
        match self {
            Self::Claude(handle) => handle.kill(),
            Self::Codex(handle) => handle.kill(),
            Self::Acp(handle) => handle.kill(),
        }
    }
}

#[derive(Clone)]
enum RuntimeToken {
    Claude(String),
    Codex(String),
    Acp(String),
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
            (Self::Acp(handle), RuntimeToken::Acp(runtime_id)) => handle.runtime_id == *runtime_id,
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
    RefreshModelList {
        response_tx: Sender<std::result::Result<Vec<SessionModelOption>, String>>,
    },
}

#[derive(Clone)]
struct CodexPromptCommand {
    approval_policy: CodexApprovalPolicy,
    attachments: Vec<PromptImageAttachment>,
    cwd: String,
    model: String,
    prompt: String,
    reasoning_effort: CodexReasoningEffort,
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
    SetModel(String),
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
enum AcpRuntimeCommand {
    Prompt(AcpPromptCommand),
    JsonRpcMessage(Value),
    RefreshSessionConfig {
        command: AcpPromptCommand,
        response_tx: Sender<std::result::Result<(), String>>,
    },
}

#[derive(Clone, Copy, Default)]
struct AcpLaunchOptions {
    gemini_approval_mode: Option<GeminiApprovalMode>,
}

#[derive(Clone)]
struct AcpPromptCommand {
    cwd: String,
    cursor_mode: Option<CursorMode>,
    model: String,
    prompt: String,
    resume_session_id: Option<String>,
}

#[derive(Clone)]
struct AcpPendingApproval {
    allow_once_option_id: Option<String>,
    allow_always_option_id: Option<String>,
    reject_option_id: Option<String>,
    request_id: Value,
}

#[derive(Default)]
struct AcpRuntimeState {
    current_session_id: Option<String>,
    is_loading_history: bool,
}

#[derive(Default)]
struct AcpTurnState {
    current_agent_message_id: Option<String>,
    thinking_buffer: String,
}

#[derive(Clone)]
struct TurnConfig {
    codex_approval_policy: Option<CodexApprovalPolicy>,
    codex_reasoning_effort: Option<CodexReasoningEffort>,
    codex_sandbox_mode: Option<CodexSandboxMode>,
    agent: Agent,
    claude_approval_mode: Option<ClaudeApprovalMode>,
    cwd: String,
    model: String,
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
    PersistentAcp {
        command: AcpPromptCommand,
        sender: Sender<AcpRuntimeCommand>,
        session_id: String,
    },
}

enum DispatchTurnResult {
    Dispatched(TurnDispatch),
    Queued,
}

type CodexPendingRequestMap =
    Arc<Mutex<HashMap<String, Sender<std::result::Result<Value, String>>>>>;
type AcpPendingRequestMap = Arc<Mutex<HashMap<String, Sender<std::result::Result<Value, String>>>>>;

#[derive(Default)]
struct CodexTurnState {
    streamed_agent_message_item_ids: HashSet<String>,
}

fn spawn_acp_runtime(
    state: AppState,
    session_id: String,
    cwd: String,
    agent: AcpAgent,
    gemini_approval_mode: Option<GeminiApprovalMode>,
) -> Result<AcpRuntimeHandle> {
    let runtime_id = Uuid::new_v4().to_string();
    let mut command = agent.command(AcpLaunchOptions {
        gemini_approval_mode,
    })?;
    command
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to start {} ACP runtime in `{cwd}`", agent.label()))?;
    let stdin = child
        .stdin
        .take()
        .with_context(|| format!("failed to capture {} ACP stdin", agent.label()))?;
    let stdout = child
        .stdout
        .take()
        .with_context(|| format!("failed to capture {} ACP stdout", agent.label()))?;
    let stderr = child
        .stderr
        .take()
        .with_context(|| format!("failed to capture {} ACP stderr", agent.label()))?;
    let process = Arc::new(Mutex::new(child));
    let (input_tx, input_rx) = mpsc::channel::<AcpRuntimeCommand>();
    let pending_requests: AcpPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState::default()));

    {
        let writer_session_id = session_id.clone();
        let writer_state = state.clone();
        let writer_pending_requests = pending_requests.clone();
        let writer_runtime_state = runtime_state.clone();
        let writer_runtime_token = RuntimeToken::Acp(runtime_id.clone());
        std::thread::spawn(move || {
            let mut stdin = stdin;
            let initialize_result = send_acp_json_rpc_request(
                &mut stdin,
                &writer_pending_requests,
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientInfo": {
                        "name": "termal",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "clientCapabilities": {},
                }),
                Duration::from_secs(15),
                agent,
            )
            .and_then(|result| {
                maybe_authenticate_acp_runtime(&mut stdin, &writer_pending_requests, &result, agent)
            });

            if let Err(err) = initialize_result {
                let _ = writer_state.handle_runtime_exit_if_matches(
                    &writer_session_id,
                    &writer_runtime_token,
                    Some(&format!(
                        "failed to initialize {} ACP session: {err:#}",
                        agent.label()
                    )),
                );
                return;
            }

            while let Ok(command) = input_rx.recv() {
                let command_result = match command {
                    AcpRuntimeCommand::Prompt(prompt) => handle_acp_prompt_command(
                        &mut stdin,
                        &writer_pending_requests,
                        &writer_state,
                        &writer_session_id,
                        &writer_runtime_state,
                        &writer_runtime_token,
                        agent,
                        prompt,
                    ),
                    AcpRuntimeCommand::JsonRpcMessage(message) => {
                        write_acp_json_rpc_message(&mut stdin, &message, agent)
                    }
                    AcpRuntimeCommand::RefreshSessionConfig {
                        command,
                        response_tx,
                    } => {
                        let refresh_result = handle_acp_session_config_refresh(
                            &mut stdin,
                            &writer_pending_requests,
                            &writer_state,
                            &writer_session_id,
                            &writer_runtime_state,
                            agent,
                            command,
                        )
                        .map_err(|err| format!("{err:#}"));
                        match refresh_result {
                            Ok(()) => {
                                let _ = response_tx.send(Ok(()));
                                Ok(())
                            }
                            Err(detail) => {
                                let _ = response_tx.send(Err(detail.clone()));
                                Err(anyhow!(detail))
                            }
                        }
                    }
                };

                if let Err(err) = command_result {
                    let _ = writer_state.handle_runtime_exit_if_matches(
                        &writer_session_id,
                        &writer_runtime_token,
                        Some(&format!(
                            "failed to communicate with {} ACP runtime: {err:#}",
                            agent.label()
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
        let reader_runtime_state = runtime_state.clone();
        let reader_input_tx = input_tx.clone();
        let reader_runtime_token = RuntimeToken::Acp(runtime_id.clone());
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut raw_line = String::new();
            let mut turn_state = AcpTurnState::default();
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
                            &format!(
                                "failed to read stdout from {} ACP runtime: {err}",
                                agent.label()
                            ),
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
                            &format!("failed to parse {} ACP JSON line: {err}", agent.label()),
                        );
                        break;
                    }
                };

                if let Err(err) = handle_acp_message(
                    &message,
                    &reader_state,
                    &reader_session_id,
                    &reader_runtime_token,
                    &reader_pending_requests,
                    &reader_runtime_state,
                    &reader_input_tx,
                    &mut turn_state,
                    &mut recorder,
                    agent,
                ) {
                    let _ = reader_state.fail_turn_if_runtime_matches(
                        &reader_session_id,
                        &reader_runtime_token,
                        &format!("failed to handle {} ACP event: {err:#}", agent.label()),
                    );
                    break;
                }
            }

            let _ = finish_acp_turn_state(&mut recorder, &mut turn_state, agent);
            let _ = recorder.finish_streaming_text();
        });
    }

    {
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                eprintln!("{} stderr> {line}", agent.label().to_lowercase());
            }
        });
    }

    {
        let wait_session_id = session_id.clone();
        let wait_state = state.clone();
        let wait_process = process.clone();
        let wait_runtime_token = RuntimeToken::Acp(runtime_id.clone());
        std::thread::spawn(move || {
            loop {
                let status = {
                    let mut child = wait_process.lock().expect("ACP process mutex poisoned");
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
                            Some(&format!(
                                "{} session exited with status {status}",
                                agent.label()
                            )),
                        );
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                    Err(err) => {
                        let _ = wait_state.handle_runtime_exit_if_matches(
                            &wait_session_id,
                            &wait_runtime_token,
                            Some(&format!(
                                "failed waiting for {} session: {err}",
                                agent.label()
                            )),
                        );
                        break;
                    }
                }
            }
        });
    }

    Ok(AcpRuntimeHandle {
        agent,
        runtime_id,
        input_tx,
        process,
    })
}

fn maybe_authenticate_acp_runtime(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    initialize_result: &Value,
    agent: AcpAgent,
) -> Result<()> {
    let Some(method_id) = select_acp_auth_method(initialize_result, agent) else {
        return Ok(());
    };

    send_acp_json_rpc_request(
        writer,
        pending_requests,
        "authenticate",
        json!({ "methodId": method_id }),
        Duration::from_secs(30),
        agent,
    )?;
    Ok(())
}

fn select_acp_auth_method(initialize_result: &Value, agent: AcpAgent) -> Option<String> {
    let methods = initialize_result
        .get("authMethods")
        .and_then(Value::as_array)?;

    let has_method = |target: &str| {
        methods.iter().any(|method| {
            method
                .get("id")
                .and_then(Value::as_str)
                .map(|id| id == target)
                .unwrap_or(false)
        })
    };

    match agent {
        AcpAgent::Cursor => has_method("cursor_login").then_some("cursor_login".to_owned()),
        AcpAgent::Gemini => {
            if std::env::var_os("GEMINI_API_KEY").is_some() && has_method("gemini-api-key") {
                Some("gemini-api-key".to_owned())
            } else if (std::env::var_os("GOOGLE_GENAI_USE_VERTEXAI").is_some()
                || std::env::var_os("GOOGLE_GENAI_USE_GCA").is_some())
                && has_method("vertex-ai")
            {
                Some("vertex-ai".to_owned())
            } else {
                None
            }
        }
    }
}

fn handle_acp_prompt_command(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    state: &AppState,
    session_id: &str,
    runtime_state: &Arc<Mutex<AcpRuntimeState>>,
    runtime_token: &RuntimeToken,
    agent: AcpAgent,
    command: AcpPromptCommand,
) -> Result<()> {
    let external_session_id = ensure_acp_session_ready(
        writer,
        pending_requests,
        state,
        session_id,
        runtime_state,
        agent,
        &command,
    )?;

    send_acp_json_rpc_request(
        writer,
        pending_requests,
        "session/prompt",
        json!({
            "sessionId": external_session_id,
            "prompt": [
                {
                    "type": "text",
                    "text": command.prompt,
                }
            ],
        }),
        Duration::from_secs(60),
        agent,
    )?;

    state.finish_turn_ok_if_runtime_matches(session_id, runtime_token)?;
    Ok(())
}

fn handle_acp_session_config_refresh(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    state: &AppState,
    session_id: &str,
    runtime_state: &Arc<Mutex<AcpRuntimeState>>,
    agent: AcpAgent,
    command: AcpPromptCommand,
) -> Result<()> {
    ensure_acp_session_ready(
        writer,
        pending_requests,
        state,
        session_id,
        runtime_state,
        agent,
        &command,
    )?;
    Ok(())
}

fn ensure_acp_session_ready(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    state: &AppState,
    session_id: &str,
    runtime_state: &Arc<Mutex<AcpRuntimeState>>,
    agent: AcpAgent,
    command: &AcpPromptCommand,
) -> Result<String> {
    if let Some(existing_session_id) = runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned")
        .current_session_id
        .clone()
    {
        return Ok(existing_session_id);
    }

    let session_result = if let Some(resume_session_id) = command.resume_session_id.as_deref() {
        {
            let mut state = runtime_state
                .lock()
                .expect("ACP runtime state mutex poisoned");
            state.is_loading_history = true;
        }
        let result = send_acp_json_rpc_request(
            writer,
            pending_requests,
            "session/load",
            json!({
                "sessionId": resume_session_id,
                "cwd": command.cwd,
                "mcpServers": [],
            }),
            Duration::from_secs(30),
            agent,
        );
        {
            let mut state = runtime_state
                .lock()
                .expect("ACP runtime state mutex poisoned");
            state.is_loading_history = false;
        }
        result.map(|value| (resume_session_id.to_owned(), value))?
    } else {
        let result = send_acp_json_rpc_request(
            writer,
            pending_requests,
            "session/new",
            json!({
                "cwd": command.cwd,
                "mcpServers": [],
            }),
            Duration::from_secs(30),
            agent,
        )?;
        let created_session_id = result
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                anyhow!(
                    "{} ACP session/new did not return a session id",
                    agent.label()
                )
            })?
            .to_owned();
        (created_session_id, result)
    };

    let (external_session_id, session_config) = session_result;
    configure_acp_session(
        writer,
        pending_requests,
        agent,
        &external_session_id,
        &command.model,
        command.cursor_mode,
        &session_config,
    )?;
    state.sync_session_model_options(
        session_id,
        current_acp_config_option_value(&session_config, "model").or_else(|| {
            let requested = command.model.trim();
            (!requested.is_empty()).then(|| requested.to_owned())
        }),
        acp_model_options(&session_config),
    )?;
    state.set_external_session_id(session_id, external_session_id.clone())?;
    runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned")
        .current_session_id = Some(external_session_id.clone());
    Ok(external_session_id)
}

fn configure_acp_session(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    agent: AcpAgent,
    session_id: &str,
    requested_model: &str,
    requested_cursor_mode: Option<CursorMode>,
    config_result: &Value,
) -> Result<()> {
    if let Some(model_value) =
        matching_acp_config_option_value(config_result, "model", requested_model)
    {
        let current_value = current_acp_config_option_value(config_result, "model");
        if current_value.as_deref() != Some(model_value.as_str()) {
            send_acp_json_rpc_request(
                writer,
                pending_requests,
                "session/set_config_option",
                json!({
                    "sessionId": session_id,
                    "optionId": "model",
                    "value": model_value,
                }),
                Duration::from_secs(15),
                agent,
            )?;
        }
    }

    if agent == AcpAgent::Cursor {
        let requested_mode = requested_cursor_mode.unwrap_or_else(default_cursor_mode);
        if let Some(mode_value) =
            matching_acp_config_option_value(config_result, "mode", requested_mode.as_acp_value())
        {
            let current_value = current_acp_config_option_value(config_result, "mode");
            if current_value.as_deref() != Some(mode_value.as_str()) {
                send_acp_json_rpc_request(
                    writer,
                    pending_requests,
                    "session/set_config_option",
                    json!({
                        "sessionId": session_id,
                        "optionId": "mode",
                        "value": mode_value,
                    }),
                    Duration::from_secs(15),
                    agent,
                )?;
            }
        }
    }
    Ok(())
}

fn current_acp_config_option_value(config_result: &Value, option_id: &str) -> Option<String> {
    acp_config_options(config_result)?
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(option_id))
        .and_then(|entry| entry.get("currentValue").and_then(Value::as_str))
        .map(str::to_owned)
}

fn matching_acp_config_option_value(
    config_result: &Value,
    option_id: &str,
    requested_value: &str,
) -> Option<String> {
    let requested = requested_value.trim();
    if requested.is_empty() {
        return None;
    }
    let requested_normalized = requested.to_ascii_lowercase();
    let option = acp_config_options(config_result)?
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(option_id))?;
    let options = option.get("options").and_then(Value::as_array)?;
    options.iter().find_map(|entry| {
        let value = entry.get("value").and_then(Value::as_str)?;
        let name = entry
            .get("name")
            .or_else(|| entry.get("label"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let value_normalized = value.to_ascii_lowercase();
        let name_normalized = name.to_ascii_lowercase();
        if value_normalized == requested_normalized || name_normalized == requested_normalized {
            Some(value.to_owned())
        } else {
            None
        }
    })
}

fn acp_model_options(config_result: &Value) -> Vec<SessionModelOption> {
    let Some(option) = acp_config_options(config_result).and_then(|entries| {
        entries
            .iter()
            .find(|entry| entry.get("id").and_then(Value::as_str) == Some("model"))
    }) else {
        return Vec::new();
    };

    option
        .get("options")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let value = entry.get("value").and_then(Value::as_str)?.trim();
                    if value.is_empty() {
                        return None;
                    }
                    let label = entry
                        .get("name")
                        .or_else(|| entry.get("label"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|label| !label.is_empty())
                        .unwrap_or(value);
                    Some(SessionModelOption {
                        label: label.to_owned(),
                        value: value.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn acp_config_options(config_result: &Value) -> Option<&Vec<Value>> {
    config_result
        .get("configOptions")
        .or_else(|| config_result.get("config_options"))
        .and_then(Value::as_array)
}

fn handle_acp_message(
    message: &Value,
    state: &AppState,
    session_id: &str,
    runtime_token: &RuntimeToken,
    pending_requests: &AcpPendingRequestMap,
    runtime_state: &Arc<Mutex<AcpRuntimeState>>,
    input_tx: &Sender<AcpRuntimeCommand>,
    turn_state: &mut AcpTurnState,
    recorder: &mut SessionRecorder,
    agent: AcpAgent,
) -> Result<()> {
    if let Some(response_id) = message.get("id") {
        if message.get("result").is_some() || message.get("error").is_some() {
            let key = acp_request_id_key(response_id);
            let sender = pending_requests
                .lock()
                .expect("ACP pending requests mutex poisoned")
                .remove(&key);
            if let Some(sender) = sender {
                let response = if let Some(result) = message.get("result") {
                    Ok(result.clone())
                } else {
                    Err(summarize_acp_json_rpc_error(
                        message.get("error").unwrap_or(&Value::Null),
                    ))
                };
                let _ = sender.send(response);
            }
            return Ok(());
        }
    }

    let Some(method) = message.get("method").and_then(Value::as_str) else {
        log_unhandled_acp_event(agent, "ACP message missing method", message);
        return Ok(());
    };

    if message.get("id").is_some() {
        return handle_acp_request(message, input_tx, recorder, agent);
    }

    handle_acp_notification(
        method,
        message,
        state,
        session_id,
        runtime_token,
        runtime_state,
        turn_state,
        recorder,
        agent,
    )
}

fn handle_acp_request(
    message: &Value,
    input_tx: &Sender<AcpRuntimeCommand>,
    recorder: &mut SessionRecorder,
    agent: AcpAgent,
) -> Result<()> {
    let request_id = message
        .get("id")
        .cloned()
        .ok_or_else(|| anyhow!("ACP request missing id"))?;
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("ACP request missing method"))?;
    let params = message.get("params").unwrap_or(&Value::Null);

    match method {
        "session/request_permission" => {
            let tool_name = params
                .get("toolName")
                .and_then(Value::as_str)
                .unwrap_or("Tool");
            let description = params
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or(tool_name);
            let options = params
                .get("options")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();

            recorder.push_acp_approval(
                &format!("{} needs approval", agent.label()),
                description,
                &format!("{} requested approval for `{tool_name}`.", agent.label()),
                AcpPendingApproval {
                    allow_once_option_id: find_acp_permission_option(
                        &options,
                        &["allow-once", "allow_once", "allow"],
                    ),
                    allow_always_option_id: find_acp_permission_option(
                        &options,
                        &["allow-always", "allow_always", "always", "acceptForSession"],
                    ),
                    reject_option_id: find_acp_permission_option(
                        &options,
                        &["reject-once", "reject_once", "reject", "deny", "decline"],
                    ),
                    request_id,
                },
            )?;
        }
        _ => {
            let _ = input_tx.send(AcpRuntimeCommand::JsonRpcMessage(json!({
                "id": request_id,
                "error": {
                    "code": -32601,
                    "message": format!("unsupported ACP request `{method}`"),
                }
            })));
            log_unhandled_acp_event(agent, &format!("unhandled ACP request `{method}`"), message);
        }
    }

    Ok(())
}

fn handle_acp_notification(
    method: &str,
    message: &Value,
    state: &AppState,
    session_id: &str,
    runtime_token: &RuntimeToken,
    runtime_state: &Arc<Mutex<AcpRuntimeState>>,
    turn_state: &mut AcpTurnState,
    recorder: &mut SessionRecorder,
    agent: AcpAgent,
) -> Result<()> {
    match method {
        "session/update" => {
            if runtime_state
                .lock()
                .expect("ACP runtime state mutex poisoned")
                .is_loading_history
            {
                return Ok(());
            }

            let Some(update) = message.pointer("/params/update") else {
                log_unhandled_acp_event(agent, "ACP session/update missing params.update", message);
                return Ok(());
            };
            handle_acp_session_update(update, state, session_id, turn_state, recorder, agent)?;
        }
        "error" => {
            let payload = message.get("params").unwrap_or(message);
            let detail = summarize_error(payload);
            state.fail_turn_if_runtime_matches(session_id, runtime_token, &detail)?;
        }
        _ => {
            log_unhandled_acp_event(
                agent,
                &format!("unhandled ACP notification `{method}`"),
                message,
            );
        }
    }

    Ok(())
}

fn handle_acp_session_update(
    update: &Value,
    state: &AppState,
    session_id: &str,
    turn_state: &mut AcpTurnState,
    recorder: &mut SessionRecorder,
    agent: AcpAgent,
) -> Result<()> {
    let Some(update_type) = update.get("sessionUpdate").and_then(Value::as_str) else {
        return Ok(());
    };

    match update_type {
        "agent_thought_chunk" => {
            if let Some(text) = update.pointer("/content/text").and_then(Value::as_str) {
                turn_state.thinking_buffer.push_str(text);
            }
        }
        "agent_message_chunk" => {
            finish_acp_thinking(recorder, turn_state, agent)?;
            let next_message_id = update
                .get("messageId")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if turn_state.current_agent_message_id != next_message_id {
                recorder.finish_streaming_text()?;
                turn_state.current_agent_message_id = next_message_id;
            }
            if let Some(text) = update.pointer("/content/text").and_then(Value::as_str) {
                recorder.text_delta(text)?;
            }
        }
        "tool_call" => {
            finish_acp_thinking(recorder, turn_state, agent)?;
            recorder.finish_streaming_text()?;
            if let Some((key, command)) = acp_tool_identity(update) {
                recorder.command_started(&key, &command)?;
            }
        }
        "tool_call_update" => {
            finish_acp_thinking(recorder, turn_state, agent)?;
            if let Some((key, command)) = acp_tool_identity(update) {
                match update.get("status").and_then(Value::as_str) {
                    Some("pending") | Some("in_progress") => {
                        recorder.command_started(&key, &command)?;
                    }
                    Some("completed") | Some("failed") | Some("error") => {
                        recorder.command_completed(
                            &key,
                            &command,
                            &summarize_acp_tool_output(update),
                            acp_tool_status(update),
                        )?;
                    }
                    _ => {}
                }
            }
        }
        "config_options_update" | "config_update" => {
            state.sync_session_model_options(
                session_id,
                current_acp_config_option_value(update, "model"),
                acp_model_options(update),
            )?;
        }
        "available_commands_update" | "mode_update" => {}
        other => {
            log_unhandled_acp_event(
                agent,
                &format!("unhandled ACP session/update `{other}`"),
                update,
            );
        }
    }

    Ok(())
}

fn finish_acp_turn_state(
    recorder: &mut SessionRecorder,
    turn_state: &mut AcpTurnState,
    agent: AcpAgent,
) -> Result<()> {
    finish_acp_thinking(recorder, turn_state, agent)?;
    recorder.finish_streaming_text()
}

fn finish_acp_thinking(
    recorder: &mut SessionRecorder,
    turn_state: &mut AcpTurnState,
    agent: AcpAgent,
) -> Result<()> {
    if turn_state.thinking_buffer.trim().is_empty() {
        turn_state.thinking_buffer.clear();
        return Ok(());
    }

    let lines = turn_state
        .thinking_buffer
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    turn_state.thinking_buffer.clear();
    if lines.is_empty() {
        return Ok(());
    }
    recorder.push_thinking(&format!("{} is thinking", agent.label()), lines)
}

fn acp_tool_identity(update: &Value) -> Option<(String, String)> {
    let key = update.get("toolCallId").and_then(Value::as_str)?.to_owned();
    let command = update
        .pointer("/rawInput/command")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            let title = update.get("title").and_then(Value::as_str)?;
            let kind = update.get("kind").and_then(Value::as_str);
            Some(match kind {
                Some(kind) => format!("{title} ({kind})"),
                None => title.to_owned(),
            })
        })
        .unwrap_or_else(|| "Tool call".to_owned());
    Some((key, command))
}

fn summarize_acp_tool_output(update: &Value) -> String {
    let Some(raw_output) = update.get("rawOutput") else {
        return String::new();
    };

    let stdout = raw_output
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or("");
    let stderr = raw_output
        .get("stderr")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !stdout.is_empty() || !stderr.is_empty() {
        if stdout.is_empty() {
            return stderr.to_owned();
        }
        if stderr.is_empty() {
            return stdout.to_owned();
        }
        return format!("{stdout}\n{stderr}");
    }

    serde_json::to_string_pretty(raw_output).unwrap_or_else(|_| raw_output.to_string())
}

fn acp_tool_status(update: &Value) -> CommandStatus {
    match update.get("status").and_then(Value::as_str) {
        Some("completed") => {
            if update
                .pointer("/rawOutput/exitCode")
                .and_then(Value::as_i64)
                == Some(0)
            {
                CommandStatus::Success
            } else {
                CommandStatus::Error
            }
        }
        Some("failed") | Some("error") => CommandStatus::Error,
        _ => CommandStatus::Running,
    }
}

fn find_acp_permission_option(options: &[Value], hints: &[&str]) -> Option<String> {
    options.iter().find_map(|option| {
        let option_id = option
            .get("optionId")
            .or_else(|| option.get("id"))
            .and_then(Value::as_str)?;
        let normalized = option_id.to_ascii_lowercase();
        hints
            .iter()
            .any(|hint| normalized.contains(&hint.to_ascii_lowercase()))
            .then_some(option_id.to_owned())
    })
}

fn send_acp_json_rpc_request(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    method: &str,
    params: Value,
    timeout: Duration,
    agent: AcpAgent,
) -> Result<Value> {
    let request_id = Uuid::new_v4().to_string();
    let (tx, rx) = mpsc::channel();
    pending_requests
        .lock()
        .expect("ACP pending requests mutex poisoned")
        .insert(request_id.clone(), tx);

    if let Err(err) = write_acp_json_rpc_message(
        writer,
        &json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params,
        }),
        agent,
    ) {
        pending_requests
            .lock()
            .expect("ACP pending requests mutex poisoned")
            .remove(&request_id);
        return Err(err);
    }

    match rx.recv_timeout(timeout) {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(err)) => Err(anyhow!(err)),
        Err(err) => {
            pending_requests
                .lock()
                .expect("ACP pending requests mutex poisoned")
                .remove(&request_id);
            Err(anyhow!(
                "timed out waiting for {} ACP response to `{method}`: {err}",
                agent.label()
            ))
        }
    }
}

fn write_acp_json_rpc_message(
    writer: &mut impl Write,
    message: &Value,
    agent: AcpAgent,
) -> Result<()> {
    serde_json::to_writer(&mut *writer, message)
        .with_context(|| format!("failed to encode {} ACP message", agent.label()))?;
    writer
        .write_all(b"\n")
        .with_context(|| format!("failed to write {} ACP message delimiter", agent.label()))?;
    writer
        .flush()
        .with_context(|| format!("failed to flush {} ACP stdin", agent.label()))
}

fn acp_request_id_key(id: &Value) -> String {
    id.as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| id.to_string())
}

fn summarize_acp_json_rpc_error(error: &Value) -> String {
    if let Some(message) = error.get("message").and_then(Value::as_str) {
        return message.to_owned();
    }

    summarize_error(error)
}

fn log_unhandled_acp_event(agent: AcpAgent, context: &str, message: &Value) {
    eprintln!(
        "{} acp diagnostic> {context}: {message}",
        agent.label().to_lowercase()
    );
}

fn spawn_codex_runtime(
    state: AppState,
    session_id: String,
    workdir: String,
) -> Result<CodexRuntimeHandle> {
    let codex_home = prepare_termal_codex_home(&workdir, &session_id)?;
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
                    CodexRuntimeCommand::RefreshModelList { response_tx } => {
                        let refresh_result = handle_codex_model_list_refresh(
                            &mut stdin,
                            &writer_pending_requests,
                        )
                        .map_err(|err| format!("{err:#}"));
                        match refresh_result {
                            Ok(model_options) => {
                                let _ = response_tx.send(Ok(model_options));
                                Ok(())
                            }
                            Err(detail) => {
                                let _ = response_tx.send(Err(detail.clone()));
                                Err(anyhow!(detail))
                            }
                        }
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
                        "model": command.model,
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
                        "model": command.model,
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

    state.record_codex_runtime_config(
        session_id,
        command.sandbox_mode,
        command.approval_policy,
        command.reasoning_effort,
    )?;

    send_codex_json_rpc_request(
        writer,
        pending_requests,
        "turn/start",
        json!({
            "threadId": thread_id,
            "cwd": command.cwd,
            "approvalPolicy": command.approval_policy.as_cli_value(),
            "effort": command.reasoning_effort.as_api_value(),
            "model": command.model,
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

fn handle_codex_model_list_refresh(
    writer: &mut impl Write,
    pending_requests: &CodexPendingRequestMap,
) -> Result<Vec<SessionModelOption>> {
    let mut cursor: Option<String> = None;
    let mut model_options = Vec::new();

    loop {
        let result = send_codex_json_rpc_request(
            writer,
            pending_requests,
            "model/list",
            json!({
                "cursor": cursor,
                "includeHidden": false,
                "limit": 100,
            }),
            Duration::from_secs(30),
        )?;
        model_options.extend(codex_model_options(&result));
        cursor = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }

    Ok(model_options)
}

fn claude_model_options(message: &Value) -> Option<Vec<SessionModelOption>> {
    let models = message.pointer("/response/response/models")?.as_array()?;
    Some(
        models
            .iter()
            .filter_map(|entry| {
                let value = entry
                    .get("value")
                    .or_else(|| entry.get("model"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())?
                    .to_owned();
                let label = entry
                    .get("displayName")
                    .or_else(|| entry.get("label"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|label| !label.is_empty())
                    .unwrap_or(&value)
                    .to_owned();
                Some(SessionModelOption { label, value })
            })
            .collect(),
    )
}

fn codex_model_options(model_list_result: &Value) -> Vec<SessionModelOption> {
    model_list_result
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let value = entry
                .get("model")
                .or_else(|| entry.get("id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?
                .to_owned();
            let label = entry
                .get("displayName")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|label| !label.is_empty())
                .unwrap_or(&value)
                .to_owned();
            Some(SessionModelOption { label, value })
        })
        .collect()
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

fn collect_agent_readiness(default_workdir: &str) -> Vec<AgentReadiness> {
    vec![
        agent_readiness_for(Agent::Cursor, default_workdir),
        agent_readiness_for(Agent::Gemini, default_workdir),
    ]
}

fn validate_agent_session_setup(agent: Agent, workdir: &str) -> std::result::Result<(), String> {
    let readiness = agent_readiness_for(agent, workdir);
    if readiness.blocking {
        return Err(readiness.detail);
    }
    Ok(())
}

fn agent_readiness_for(agent: Agent, workdir: &str) -> AgentReadiness {
    match agent {
        Agent::Cursor => cursor_agent_readiness(),
        Agent::Gemini => gemini_agent_readiness(workdir),
        _ => AgentReadiness {
            agent,
            status: AgentReadinessStatus::Ready,
            blocking: false,
            detail: format!("{} is managed by its local CLI runtime.", agent.name()),
            command_path: None,
        },
    }
}

fn cursor_agent_readiness() -> AgentReadiness {
    let command_path = find_command_on_path("cursor-agent").map(|path| path.display().to_string());
    match command_path {
        Some(command_path) => AgentReadiness {
            agent: Agent::Cursor,
            status: AgentReadinessStatus::Ready,
            blocking: false,
            detail: format!("Cursor Agent is available at `{command_path}`."),
            command_path: Some(command_path),
        },
        None => AgentReadiness {
            agent: Agent::Cursor,
            status: AgentReadinessStatus::Missing,
            blocking: true,
            detail: "Install `cursor-agent` and make sure it is on PATH before creating Cursor sessions."
                .to_owned(),
            command_path: None,
        },
    }
}

fn gemini_agent_readiness(workdir: &str) -> AgentReadiness {
    let command_path = match find_command_on_path("gemini") {
        Some(path) => path,
        None => {
            return AgentReadiness {
                agent: Agent::Gemini,
                status: AgentReadinessStatus::Missing,
                blocking: true,
                detail: "Install the `gemini` CLI and make sure it is on PATH before creating Gemini sessions."
                    .to_owned(),
                command_path: None,
            };
        }
    };
    let command_path_display = command_path.display().to_string();

    if let Some(source) = gemini_api_key_source(workdir) {
        return AgentReadiness {
            agent: Agent::Gemini,
            status: AgentReadinessStatus::Ready,
            blocking: false,
            detail: format!("Gemini CLI is ready with a Gemini API key from {source}."),
            command_path: Some(command_path_display),
        };
    }

    let selected_auth_type = gemini_selected_auth_type(workdir);
    if selected_auth_type.as_deref() == Some("oauth-personal") {
        if let Some(path) = gemini_oauth_credentials_path().filter(|path| path.is_file()) {
            return AgentReadiness {
                agent: Agent::Gemini,
                status: AgentReadinessStatus::Ready,
                blocking: false,
                detail: format!(
                    "Gemini CLI is ready with Google login credentials from {}.",
                    display_path_for_user(&path)
                ),
                command_path: Some(command_path_display),
            };
        }
        return AgentReadiness {
            agent: Agent::Gemini,
            status: AgentReadinessStatus::NeedsSetup,
            blocking: true,
            detail: format!(
                "Gemini is configured for Google login, but {} is missing.",
                gemini_oauth_credentials_path()
                    .as_deref()
                    .map(display_path_for_user)
                    .unwrap_or_else(|| "~/.gemini/oauth_creds.json".to_owned())
            ),
            command_path: Some(command_path_display),
        };
    }

    if selected_auth_type.as_deref() == Some("gemini-api-key") {
        return AgentReadiness {
            agent: Agent::Gemini,
            status: AgentReadinessStatus::NeedsSetup,
            blocking: true,
            detail: gemini_api_key_missing_detail(workdir),
            command_path: Some(command_path_display),
        };
    }

    if selected_auth_type.as_deref() == Some("vertex-ai") {
        if let Some(source) = gemini_vertex_auth_source(workdir) {
            return AgentReadiness {
                agent: Agent::Gemini,
                status: AgentReadinessStatus::Ready,
                blocking: false,
                detail: format!("Gemini CLI is ready with Vertex AI credentials from {source}."),
                command_path: Some(command_path_display),
            };
        }
        return AgentReadiness {
            agent: Agent::Gemini,
            status: AgentReadinessStatus::NeedsSetup,
            blocking: true,
            detail: "Gemini is configured for Vertex AI, but the required credentials are missing. Set `GOOGLE_API_KEY`, or set both `GOOGLE_CLOUD_PROJECT` and `GOOGLE_CLOUD_LOCATION`."
                .to_owned(),
            command_path: Some(command_path_display),
        };
    }

    if selected_auth_type.as_deref() == Some("compute-default-credentials") {
        if let Some(source) = gemini_adc_source() {
            return AgentReadiness {
                agent: Agent::Gemini,
                status: AgentReadinessStatus::Ready,
                blocking: false,
                detail: format!(
                    "Gemini CLI is ready with application default credentials from {source}."
                ),
                command_path: Some(command_path_display),
            };
        }
        return AgentReadiness {
            agent: Agent::Gemini,
            status: AgentReadinessStatus::NeedsSetup,
            blocking: true,
            detail: "Gemini is configured for application default credentials, but no ADC file was found. Set `GOOGLE_APPLICATION_CREDENTIALS` or run `gcloud auth application-default login`."
                .to_owned(),
            command_path: Some(command_path_display),
        };
    }

    if let Some(source) = gemini_vertex_auth_source(workdir) {
        return AgentReadiness {
            agent: Agent::Gemini,
            status: AgentReadinessStatus::Ready,
            blocking: false,
            detail: format!("Gemini CLI is ready with Vertex AI credentials from {source}."),
            command_path: Some(command_path_display),
        };
    }

    AgentReadiness {
        agent: Agent::Gemini,
        status: AgentReadinessStatus::NeedsSetup,
        blocking: true,
        detail: "Gemini CLI needs auth before TermAl can create sessions. Set `GEMINI_API_KEY`, configure Vertex AI env vars, or choose an auth type in `.gemini/settings.json`."
            .to_owned(),
        command_path: Some(command_path_display),
    }
}

fn gemini_api_key_missing_detail(workdir: &str) -> String {
    let env_file = find_gemini_env_file(workdir)
        .map(|path| display_path_for_user(&path))
        .unwrap_or_else(|| ".env".to_owned());
    format!(
        "Gemini is configured for an API key, but `GEMINI_API_KEY` was not found in the process environment or in {env_file}."
    )
}

fn gemini_api_key_source(workdir: &str) -> Option<String> {
    env_var_source("GEMINI_API_KEY").or_else(|| dotenv_var_source(workdir, "GEMINI_API_KEY"))
}

fn gemini_vertex_auth_source(workdir: &str) -> Option<String> {
    let vertex_enabled = env_var_present("GOOGLE_GENAI_USE_VERTEXAI")
        || env_var_present("GOOGLE_GENAI_USE_GCA")
        || dotenv_var_present(workdir, "GOOGLE_GENAI_USE_VERTEXAI")
        || dotenv_var_present(workdir, "GOOGLE_GENAI_USE_GCA");
    if !vertex_enabled && gemini_selected_auth_type(workdir).as_deref() != Some("vertex-ai") {
        return None;
    }

    if let Some(source) =
        env_var_source("GOOGLE_API_KEY").or_else(|| dotenv_var_source(workdir, "GOOGLE_API_KEY"))
    {
        return Some(source);
    }

    let has_project = env_var_present("GOOGLE_CLOUD_PROJECT")
        || dotenv_var_present(workdir, "GOOGLE_CLOUD_PROJECT");
    let has_location = env_var_present("GOOGLE_CLOUD_LOCATION")
        || dotenv_var_present(workdir, "GOOGLE_CLOUD_LOCATION");
    if has_project && has_location {
        return Some(
            env_var_source("GOOGLE_CLOUD_PROJECT")
                .or_else(|| dotenv_var_source(workdir, "GOOGLE_CLOUD_PROJECT"))
                .unwrap_or_else(|| "workspace configuration".to_owned()),
        );
    }

    None
}

fn gemini_adc_source() -> Option<String> {
    if let Some(path) = std::env::var_os("GOOGLE_APPLICATION_CREDENTIALS")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
    {
        return Some(display_path_for_user(&path));
    }

    let home = home_dir()?;
    let default_path = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from).map(|path| {
            path.join("gcloud")
                .join("application_default_credentials.json")
        })
    } else {
        Some(
            home.join(".config")
                .join("gcloud")
                .join("application_default_credentials.json"),
        )
    }?;
    default_path
        .is_file()
        .then(|| display_path_for_user(&default_path))
}

fn gemini_selected_auth_type(workdir: &str) -> Option<String> {
    let workspace_settings = PathBuf::from(workdir).join(".gemini").join("settings.json");
    for path in [
        Some(workspace_settings),
        gemini_user_settings_path(),
        gemini_system_settings_path(),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(selected_type) = gemini_selected_auth_type_from_settings_file(&path) {
            return Some(selected_type);
        }
    }
    None
}

fn gemini_selected_auth_type_from_settings_file(path: &FsPath) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    if let Ok(parsed) = serde_json::from_str::<Value>(&raw) {
        return parsed
            .pointer("/security/auth/selectedType")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    [
        "oauth-personal",
        "gemini-api-key",
        "vertex-ai",
        "compute-default-credentials",
    ]
    .iter()
    .find_map(|candidate| raw.contains(candidate).then_some((*candidate).to_owned()))
}

fn find_gemini_env_file(workdir: &str) -> Option<PathBuf> {
    let mut current = PathBuf::from(workdir);
    loop {
        let gemini_env_path = current.join(".gemini").join(".env");
        if gemini_env_path.is_file() {
            return Some(gemini_env_path);
        }
        let env_path = current.join(".env");
        if env_path.is_file() {
            return Some(env_path);
        }

        let Some(parent) = current.parent() else {
            break;
        };
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
    }

    let home = home_dir()?;
    let home_gemini_env = home.join(".gemini").join(".env");
    if home_gemini_env.is_file() {
        return Some(home_gemini_env);
    }
    let home_env = home.join(".env");
    home_env.is_file().then_some(home_env)
}

fn dotenv_var_source(workdir: &str, key: &str) -> Option<String> {
    let path = find_gemini_env_file(workdir)?;
    dotenv_file_var_present(&path, key).then(|| display_path_for_user(&path))
}

fn dotenv_var_present(workdir: &str, key: &str) -> bool {
    find_gemini_env_file(workdir)
        .as_deref()
        .map(|path| dotenv_file_var_present(path, key))
        .unwrap_or(false)
}

fn dotenv_file_var_present(path: &FsPath, key: &str) -> bool {
    let Ok(raw) = fs::read_to_string(path) else {
        return false;
    };
    raw.lines().any(|line| {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return false;
        }
        let trimmed = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let Some((name, value)) = trimmed.split_once('=') else {
            return false;
        };
        if name.trim() != key {
            return false;
        }
        !value
            .trim()
            .trim_matches(|ch| ch == '"' || ch == '\'')
            .is_empty()
    })
}

fn env_var_source(key: &str) -> Option<String> {
    env_var_present(key).then(|| format!("the `{key}` environment variable"))
}

fn env_var_present(key: &str) -> bool {
    std::env::var(key)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

fn gemini_oauth_credentials_path() -> Option<PathBuf> {
    home_dir().map(|home| home.join(".gemini").join("oauth_creds.json"))
}

fn gemini_user_settings_path() -> Option<PathBuf> {
    home_dir().map(|home| home.join(".gemini").join("settings.json"))
}

fn gemini_system_settings_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("GEMINI_CLI_SYSTEM_SETTINGS_PATH") {
        return Some(PathBuf::from(path));
    }
    Some(if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/GeminiCli/settings.json")
    } else if cfg!(windows) {
        PathBuf::from("C:\\ProgramData\\gemini-cli\\settings.json")
    } else {
        PathBuf::from("/etc/gemini-cli/settings.json")
    })
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

fn display_path_for_user(path: &FsPath) -> String {
    if let Some(home) = home_dir() {
        if let Ok(relative) = path.strip_prefix(&home) {
            return if relative.as_os_str().is_empty() {
                "~".to_owned()
            } else {
                format!("~/{}", relative.display())
            };
        }
    }
    path.display().to_string()
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
    model: String,
    approval_mode: ClaudeApprovalMode,
    resume_session_id: Option<String>,
    model_options_tx: Option<Sender<std::result::Result<Vec<SessionModelOption>, String>>>,
) -> Result<ClaudeRuntimeHandle> {
    let runtime_id = Uuid::new_v4().to_string();
    let mut command = Command::new("claude");
    command.current_dir(&cwd).args([
        "--model",
        &model,
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
    if let Some(permission_mode) = approval_mode.initial_cli_permission_mode() {
        command.args(["--permission-mode", permission_mode]);
    }
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
                    ClaudeRuntimeCommand::SetModel(model) => {
                        write_claude_set_model(&mut stdin, &model)
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
            let mut initialize_model_options_tx = model_options_tx;

            loop {
                raw_line.clear();
                let bytes_read = match reader.read_line(&mut raw_line) {
                    Ok(bytes_read) => bytes_read,
                    Err(err) => {
                        if let Some(tx) = initialize_model_options_tx.take() {
                            let _ = tx.send(Err(format!("failed to read stdout from Claude: {err}")));
                        }
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
                        if let Some(tx) = initialize_model_options_tx.take() {
                            let _ = tx.send(Err(format!(
                                "failed to parse Claude JSON line: {err}"
                            )));
                        }
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

                if let Some(model_options) = claude_model_options(&message) {
                    if let Err(err) = reader_state.sync_session_model_options(
                        &reader_session_id,
                        None,
                        model_options.clone(),
                    ) {
                        if let Some(tx) = initialize_model_options_tx.take() {
                            let _ = tx.send(Err(format!(
                                "failed to sync Claude model options: {err:#}"
                            )));
                        }
                        let _ = reader_state.fail_turn_if_runtime_matches(
                            &reader_session_id,
                            &reader_runtime_token,
                            &format!("failed to sync Claude model options: {err:#}"),
                        );
                        break;
                    }

                    if let Some(tx) = initialize_model_options_tx.take() {
                        let _ = tx.send(Ok(model_options));
                    }
                }

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

            if let Some(tx) = initialize_model_options_tx.take() {
                let _ = tx.send(Err("Claude exited before reporting model options".to_owned()));
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

fn write_claude_set_model(writer: &mut impl Write, model: &str) -> Result<()> {
    write_claude_message(
        writer,
        &json!({
            "request_id": Uuid::new_v4().to_string(),
            "type": "control_request",
            "request": {
                "subtype": "set_model",
                "model": model,
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
            &config.model,
            config
                .codex_sandbox_mode
                .unwrap_or_else(default_codex_sandbox_mode),
            config
                .codex_approval_policy
                .unwrap_or_else(default_codex_approval_policy),
            config
                .codex_reasoning_effort
                .unwrap_or_else(default_codex_reasoning_effort),
            &config.prompt,
            recorder,
        ),
        Agent::Claude => run_claude_turn(
            &config.cwd,
            config.external_session_id.as_deref(),
            &config.model,
            config
                .claude_approval_mode
                .unwrap_or_else(default_claude_approval_mode),
            &config.prompt,
            recorder,
        ),
        Agent::Cursor => run_acp_turn(
            AcpAgent::Cursor,
            &config.cwd,
            config.external_session_id.as_deref(),
            &config.model,
            &config.prompt,
            recorder,
        ),
        Agent::Gemini => run_acp_turn(
            AcpAgent::Gemini,
            &config.cwd,
            config.external_session_id.as_deref(),
            &config.model,
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

    fn push_acp_approval(
        &mut self,
        title: &str,
        command: &str,
        detail: &str,
        approval: AcpPendingApproval,
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
            .register_acp_pending_approval(&self.session_id, message_id, approval)
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

fn run_acp_turn(
    agent: AcpAgent,
    _cwd: &str,
    _external_session_id: Option<&str>,
    _model: &str,
    _prompt: &str,
    _recorder: &mut dyn TurnRecorder,
) -> Result<String> {
    bail!("{} REPL mode is not supported yet", agent.label())
}

fn run_codex_turn(
    state: Option<&AppState>,
    runtime_session_id: Option<&str>,
    cwd: &str,
    external_session_id: Option<&str>,
    model: &str,
    sandbox_mode: CodexSandboxMode,
    approval_policy: CodexApprovalPolicy,
    reasoning_effort: CodexReasoningEffort,
    prompt: &str,
    recorder: &mut dyn TurnRecorder,
) -> Result<String> {
    let codex_home = prepare_termal_codex_home(cwd, runtime_session_id.unwrap_or("repl"))?;
    let mut command = codex_command()?;
    command
        .env("CODEX_HOME", &codex_home)
        .args(["-m", model, "-c"])
        .arg(format!(
            "model_reasoning_effort=\"{}\"",
            reasoning_effort.as_api_value()
        ));

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

fn default_codex_reasoning_effort() -> CodexReasoningEffort {
    match std::env::var("TERMAL_CODEX_REASONING_EFFORT")
        .ok()
        .as_deref()
    {
        Some("none") => CodexReasoningEffort::None,
        Some("minimal") => CodexReasoningEffort::Minimal,
        Some("low") => CodexReasoningEffort::Low,
        Some("high") => CodexReasoningEffort::High,
        Some("xhigh") => CodexReasoningEffort::XHigh,
        _ => CodexReasoningEffort::Medium,
    }
}

fn default_claude_approval_mode() -> ClaudeApprovalMode {
    ClaudeApprovalMode::Ask
}

fn default_cursor_mode() -> CursorMode {
    CursorMode::Agent
}

fn default_gemini_approval_mode() -> GeminiApprovalMode {
    GeminiApprovalMode::Default
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
    model: &str,
    approval_mode: ClaudeApprovalMode,
    prompt: &str,
    recorder: &mut dyn TurnRecorder,
) -> Result<String> {
    let mut command = Command::new("claude");
    command.current_dir(cwd).args([
        "--model",
        model,
        "-p",
        "--verbose",
        "--output-format",
        "stream-json",
        "--include-partial-messages",
    ]);
    if let Some(permission_mode) = approval_mode.initial_cli_permission_mode() {
        command.args(["--permission-mode", permission_mode]);
    }

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
        ClaudeApprovalMode::Plan => {
            ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Deny {
                request_id: request.request_id,
                message: "TermAl denied this tool request because Claude is in plan mode."
                    .to_owned(),
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
        TurnDispatch::PersistentAcp {
            command,
            sender,
            session_id,
        } => {
            if let Err(err) = sender.send(AcpRuntimeCommand::Prompt(command)) {
                let _ = state.clear_runtime(&session_id);
                let _ = state.fail_turn(
                    &session_id,
                    &format!("failed to queue prompt for ACP session: {err}"),
                );
                return Err(ApiError::internal(
                    "failed to queue prompt for agent session",
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
    Ok(Json(load_git_status_for_path(&workdir)?))
}

async fn apply_git_file_action(
    Json(request): Json<GitFileActionRequest>,
) -> Result<Json<GitStatusResponse>, ApiError> {
    let workdir = resolve_requested_path(&request.workdir)?;
    let workdir = normalize_git_workdir_path(&workdir)?;
    let Some(repo_root) = resolve_git_repo_root(&workdir)? else {
        return Err(ApiError::bad_request("no git repository found"));
    };

    let current_path = normalize_git_repo_relative_path(&request.path)?;
    let original_path = request
        .original_path
        .as_deref()
        .map(normalize_git_repo_relative_path)
        .transpose()?;

    match request.action {
        GitFileAction::Stage => {
            let pathspecs = collect_git_pathspecs(&current_path, original_path.as_deref());
            run_git_pathspec_command(
                &repo_root,
                &["add", "-A"],
                &pathspecs,
                "failed to stage git changes",
            )?;
        }
        GitFileAction::Unstage => {
            let pathspecs = collect_git_pathspecs(&current_path, original_path.as_deref());
            run_git_pathspec_command(
                &repo_root,
                &["restore", "--staged"],
                &pathspecs,
                "failed to unstage git changes",
            )?;
        }
        GitFileAction::Revert => {
            revert_git_file_action(
                &repo_root,
                &current_path,
                original_path.as_deref(),
                request.status_code.as_deref(),
            )?;
        }
    }

    Ok(Json(load_git_status_for_path(&workdir)?))
}

fn load_git_status_for_path(path: &FsPath) -> Result<GitStatusResponse, ApiError> {
    let workdir = normalize_git_workdir_path(path)?;
    let Some(repo_root) = resolve_git_repo_root(&workdir)? else {
        return Ok(GitStatusResponse {
            ahead: 0,
            behind: 0,
            branch: None,
            files: Vec::new(),
            is_clean: true,
            repo_root: None,
            upstream: None,
            workdir: workdir.to_string_lossy().into_owned(),
        });
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

    Ok(GitStatusResponse {
        ahead,
        behind,
        branch,
        files,
        is_clean,
        repo_root: Some(repo_root.to_string_lossy().into_owned()),
        upstream,
        workdir: workdir.to_string_lossy().into_owned(),
    })
}

fn normalize_git_workdir_path(path: &FsPath) -> Result<PathBuf, ApiError> {
    if path.is_dir() {
        return Ok(path.to_path_buf());
    }

    path.parent()
        .map(FsPath::to_path_buf)
        .ok_or_else(|| ApiError::bad_request("cannot inspect git status for a root file path"))
}

fn normalize_git_repo_relative_path(path: &str) -> Result<String, ApiError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request("git file path cannot be empty"));
    }

    if FsPath::new(trimmed).is_absolute() {
        return Err(ApiError::bad_request(
            "git file actions require repository-relative paths",
        ));
    }

    if trimmed.contains('\0') {
        return Err(ApiError::bad_request(
            "git file path contains invalid characters",
        ));
    }

    Ok(trimmed.to_owned())
}

fn collect_git_pathspecs(current_path: &str, original_path: Option<&str>) -> Vec<String> {
    let mut pathspecs = Vec::new();
    if let Some(original_path) = original_path.filter(|original| *original != current_path) {
        pathspecs.push(original_path.to_owned());
    }
    pathspecs.push(current_path.to_owned());
    pathspecs
}

fn revert_git_file_action(
    repo_root: &FsPath,
    current_path: &str,
    original_path: Option<&str>,
    status_code: Option<&str>,
) -> Result<(), ApiError> {
    if let Some(original_path) = original_path.filter(|original| *original != current_path) {
        run_git_pathspec_command(
            repo_root,
            &["restore", "--worktree", "--source=HEAD"],
            &[original_path.to_owned()],
            "failed to restore the original git path",
        )?;
    }

    if status_code.is_some_and(|status| status.trim() == "?") {
        run_git_pathspec_command(
            repo_root,
            &["clean", "-f"],
            &[current_path.to_owned()],
            "failed to remove untracked git path",
        )?;
    } else {
        run_git_pathspec_command(
            repo_root,
            &["restore", "--worktree", "--source=HEAD"],
            &[current_path.to_owned()],
            "failed to revert git changes",
        )?;
    }

    Ok(())
}

fn run_git_pathspec_command(
    repo_root: &FsPath,
    args: &[&str],
    pathspecs: &[String],
    error_context: &str,
) -> Result<(), ApiError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .arg("--")
        .args(pathspecs)
        .output()
        .map_err(|err| ApiError::internal(format!("{error_context}: {err}")))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let detail = if stderr.is_empty() { stdout } else { stderr };

    if detail.is_empty() {
        Err(ApiError::internal(error_context))
    } else {
        Err(ApiError::internal(format!("{error_context}: {detail}")))
    }
}

async fn state_events(
    State(state): State<AppState>,
) -> Sse<impl futures_core::Stream<Item = std::result::Result<Event, Infallible>>> {
    let mut state_receiver = state.subscribe_events();
    let mut delta_receiver = state.subscribe_delta_events();
    let initial_payload = serde_json::to_string(&state.snapshot())
        .unwrap_or_else(|_| "{\"revision\":0,\"projects\":[],\"sessions\":[]}".to_owned());

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
                                .unwrap_or_else(|_| "{\"revision\":0,\"projects\":[],\"sessions\":[]}".to_owned());
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
                                .unwrap_or_else(|_| "{\"revision\":0,\"projects\":[],\"sessions\":[]}".to_owned());
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
    let response = state.create_session(request)?;
    Ok((StatusCode::CREATED, Json(response)))
}

async fn create_project(
    State(state): State<AppState>,
    Json(request): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<CreateProjectResponse>), ApiError> {
    let response = state.create_project(request)?;
    Ok((StatusCode::CREATED, Json(response)))
}

async fn pick_project_root(
    State(state): State<AppState>,
) -> Result<Json<PickProjectRootResponse>, ApiError> {
    let default_workdir = state.default_workdir.clone();
    let path = tokio::task::spawn_blocking(move || pick_project_root_path(&default_workdir))
        .await
        .map_err(|err| ApiError::internal(format!("folder picker task failed: {err}")))??;
    Ok(Json(PickProjectRootResponse { path }))
}

async fn update_session_settings(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
    Json(request): Json<UpdateSessionSettingsRequest>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = state.update_session_settings(&session_id, request)?;
    Ok(Json(response))
}

async fn refresh_session_model_options(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = state.refresh_session_model_options(&session_id)?;
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
    Cursor,
    Gemini,
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
                "cursor" | "cursor-agent" => return Ok(Self::Cursor),
                "gemini" | "gemini-cli" => return Ok(Self::Gemini),
                other => bail!("unknown argument `{other}`"),
            }
        }

        Ok(Self::Codex)
    }

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "codex" => Ok(Self::Codex),
            "claude" => Ok(Self::Claude),
            "cursor" | "cursor-agent" => Ok(Self::Cursor),
            "gemini" | "gemini-cli" => Ok(Self::Gemini),
            other => {
                bail!("unknown agent `{other}`; expected `codex`, `claude`, `cursor`, or `gemini`")
            }
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
            Self::Cursor => "Cursor",
            Self::Gemini => "Gemini",
        }
    }

    fn avatar(self) -> &'static str {
        match self {
            Self::Codex => "CX",
            Self::Claude => "CL",
            Self::Cursor => "CR",
            Self::Gemini => "GM",
        }
    }

    fn default_model(self) -> &'static str {
        match self {
            Self::Codex => "gpt-5.4",
            Self::Claude => "sonnet",
            Self::Cursor => "auto",
            Self::Gemini => "auto",
        }
    }

    fn supports_codex_prompt_settings(self) -> bool {
        matches!(self, Self::Codex)
    }

    fn supports_claude_approval_mode(self) -> bool {
        matches!(self, Self::Claude)
    }

    fn supports_cursor_mode(self) -> bool {
        matches!(self, Self::Cursor)
    }

    fn supports_gemini_approval_mode(self) -> bool {
        matches!(self, Self::Gemini)
    }

    fn acp_runtime(self) -> Option<AcpAgent> {
        match self {
            Self::Cursor => Some(AcpAgent::Cursor),
            Self::Gemini => Some(AcpAgent::Gemini),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Project {
    id: String,
    name: String,
    root_path: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Session {
    id: String,
    name: String,
    emoji: String,
    agent: Agent,
    workdir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project_id: Option<String>,
    model: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    model_options: Vec<SessionModelOption>,
    approval_policy: Option<CodexApprovalPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<CodexReasoningEffort>,
    sandbox_mode: Option<CodexSandboxMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor_mode: Option<CursorMode>,
    claude_approval_mode: Option<ClaudeApprovalMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gemini_approval_mode: Option<GeminiApprovalMode>,
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
enum CodexReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
}

impl CodexReasoningEffort {
    fn as_api_value(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ClaudeApprovalMode {
    Ask,
    AutoApprove,
    Plan,
}

impl ClaudeApprovalMode {
    fn initial_cli_permission_mode(self) -> Option<&'static str> {
        match self {
            Self::Plan => Some("plan"),
            Self::Ask | Self::AutoApprove => None,
        }
    }

    fn session_cli_permission_mode(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Ask | Self::AutoApprove => "default",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum CursorMode {
    Agent,
    Plan,
    Ask,
}

impl CursorMode {
    fn as_acp_value(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Plan => "plan",
            Self::Ask => "ask",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum GeminiApprovalMode {
    Default,
    AutoEdit,
    Yolo,
    Plan,
}

impl GeminiApprovalMode {
    fn as_cli_value(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AutoEdit => "auto_edit",
            Self::Yolo => "yolo",
            Self::Plan => "plan",
        }
    }
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
    project_id: Option<String>,
    model: Option<String>,
    approval_policy: Option<CodexApprovalPolicy>,
    reasoning_effort: Option<CodexReasoningEffort>,
    sandbox_mode: Option<CodexSandboxMode>,
    cursor_mode: Option<CursorMode>,
    claude_approval_mode: Option<ClaudeApprovalMode>,
    gemini_approval_mode: Option<GeminiApprovalMode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateProjectRequest {
    name: Option<String>,
    root_path: String,
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

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum GitFileAction {
    Stage,
    Unstage,
    Revert,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitFileActionRequest {
    action: GitFileAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_path: Option<String>,
    path: String,
    #[serde(default)]
    status_code: Option<String>,
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
    name: Option<String>,
    model: Option<String>,
    approval_policy: Option<CodexApprovalPolicy>,
    reasoning_effort: Option<CodexReasoningEffort>,
    sandbox_mode: Option<CodexSandboxMode>,
    cursor_mode: Option<CursorMode>,
    claude_approval_mode: Option<ClaudeApprovalMode>,
    gemini_approval_mode: Option<GeminiApprovalMode>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionModelOption {
    label: String,
    value: String,
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

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
enum AgentReadinessStatus {
    Ready,
    Missing,
    NeedsSetup,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentReadiness {
    agent: Agent,
    status: AgentReadinessStatus,
    blocking: bool,
    detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command_path: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StateResponse {
    revision: u64,
    #[serde(default, skip_serializing_if = "CodexState::is_empty")]
    codex: CodexState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    agent_readiness: Vec<AgentReadiness>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    projects: Vec<Project>,
    sessions: Vec<Session>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionResponse {
    session_id: String,
    state: StateResponse,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateProjectResponse {
    project_id: String,
    state: StateResponse,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PickProjectRootResponse {
    path: Option<String>,
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

fn resolve_directory_path(path: &str, label: &str) -> Result<String, ApiError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request(format!("{label} cannot be empty")));
    }

    let resolved = resolve_requested_path(trimmed)?;
    let directory = if resolved.is_dir() {
        resolved
    } else {
        return Err(ApiError::bad_request(format!(
            "`{}` is not a directory",
            trimmed
        )));
    };
    let canonical = fs::canonicalize(&directory).unwrap_or(directory);
    Ok(canonical.to_string_lossy().into_owned())
}

fn resolve_project_root_path(path: &str) -> Result<String, ApiError> {
    resolve_directory_path(path, "project root path")
}

fn resolve_session_workdir(path: &str) -> Result<String, ApiError> {
    resolve_directory_path(path, "session workdir")
}

fn pick_project_root_path(default_workdir: &str) -> Result<Option<String>, ApiError> {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("osascript")
            .arg("-e")
            .arg("on run argv")
            .arg("-e")
            .arg("set defaultLocation to POSIX file (item 1 of argv)")
            .arg("-e")
            .arg("try")
            .arg("-e")
            .arg(
                "set chosenFolder to choose folder with prompt \"Choose a folder for this project\" default location defaultLocation",
            )
            .arg("-e")
            .arg("return POSIX path of chosenFolder")
            .arg("-e")
            .arg("on error number -128")
            .arg("-e")
            .arg("return \"\"")
            .arg("-e")
            .arg("end try")
            .arg("-e")
            .arg("end run")
            .arg(default_workdir)
            .output()
            .map_err(|err| ApiError::internal(format!("failed to open folder picker: {err}")))?;

        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            let message = if detail.is_empty() {
                "folder picker failed".to_owned()
            } else {
                format!("folder picker failed: {detail}")
            };
            return Err(ApiError::internal(message));
        }

        let selected = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if selected.is_empty() {
            return Ok(None);
        }

        return resolve_project_root_path(&selected).map(Some);
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = default_workdir;
        Err(ApiError::bad_request(
            "Folder picker is unavailable on this platform. Enter the path manually.",
        ))
    }
}

fn default_project_name(root_path: &str) -> String {
    let path = FsPath::new(root_path);
    path.file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| root_path.to_owned())
}

fn dedupe_project_name(existing: &[Project], base_name: &str) -> String {
    let existing_names = existing
        .iter()
        .map(|project| project.name.as_str())
        .collect::<HashSet<_>>();
    if !existing_names.contains(base_name) {
        return base_name.to_owned();
    }

    let mut suffix = 2usize;
    loop {
        let candidate = format!("{base_name} {suffix}");
        if !existing_names.contains(candidate.as_str()) {
            return candidate;
        }
        suffix += 1;
    }
}

fn path_contains(root_path: &str, candidate_path: &FsPath) -> bool {
    let root = normalize_path_best_effort(FsPath::new(root_path));
    let candidate = normalize_path_best_effort(candidate_path);
    candidate == root || candidate.starts_with(root)
}

fn normalize_path_best_effort(path: &FsPath) -> PathBuf {
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    fs::canonicalize(&resolved).unwrap_or(resolved)
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

        let record = inner.create_session(Agent::Claude, None, "/tmp".to_owned(), None, None);

        assert_eq!(
            record.session.claude_approval_mode,
            Some(ClaudeApprovalMode::Ask)
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
                gemini_approval_mode: None,
            })
            .unwrap();

        let child = Command::new("sh").arg("-c").arg("exit 0").spawn().unwrap();
        let (input_tx, input_rx) = mpsc::channel();
        let runtime = ClaudeRuntimeHandle {
            runtime_id: "claude-model-update".to_owned(),
            input_tx,
            process: Arc::new(Mutex::new(child)),
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
    fn refreshes_claude_model_options_from_session_cache() {
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
                gemini_approval_mode: None,
            })
            .unwrap();

        let model_options = vec![
            SessionModelOption {
                label: "Default (recommended)".to_owned(),
                value: "default".to_owned(),
            },
            SessionModelOption {
                label: "Sonnet".to_owned(),
                value: "sonnet".to_owned(),
            },
        ];

        state
            .sync_session_model_options(&created.session_id, None, model_options.clone())
            .expect("Claude model options should sync");

        let refreshed = state
            .refresh_session_model_options(&created.session_id)
            .expect("Claude model refresh should succeed");
        let session = refreshed
            .sessions
            .iter()
            .find(|session| session.id == created.session_id)
            .expect("refreshed Claude session should be present");

        assert_eq!(
            session.model_options,
            model_options
        );
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
                gemini_approval_mode: None,
            })
            .unwrap();

        let child = Command::new("sh").arg("-c").arg("exit 0").spawn().unwrap();
        let (input_tx, input_rx) = mpsc::channel();
        let runtime = CodexRuntimeHandle {
            runtime_id: "codex-model-refresh".to_owned(),
            input_tx,
            process: Arc::new(Mutex::new(child)),
        };

        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&created.session_id)
                .expect("Codex session should exist");
            inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
        }

        std::thread::spawn(move || {
            let command = input_rx.recv().expect("Codex refresh command should arrive");
            match command {
                CodexRuntimeCommand::RefreshModelList { response_tx } => {
                    let _ = response_tx.send(Ok(vec![
                        SessionModelOption {
                            label: "gpt-5.4".to_owned(),
                            value: "gpt-5.4".to_owned(),
                        },
                        SessionModelOption {
                            label: "gpt-5.3-codex".to_owned(),
                            value: "gpt-5.3-codex".to_owned(),
                        },
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
                SessionModelOption {
                    label: "gpt-5.4".to_owned(),
                    value: "gpt-5.4".to_owned(),
                },
                SessionModelOption {
                    label: "gpt-5.3-codex".to_owned(),
                    value: "gpt-5.3-codex".to_owned(),
                },
            ]
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
                gemini_approval_mode: None,
            })
            .unwrap();

        let model_options = vec![
            SessionModelOption {
                label: "Auto".to_owned(),
                value: "auto".to_owned(),
            },
            SessionModelOption {
                label: "GPT-5.3 Codex".to_owned(),
                value: "gpt-5.3-codex".to_owned(),
            },
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
    fn creates_projects_and_assigns_sessions_to_them() {
        let state = test_app_state();
        let expected_root = resolve_project_root_path("/tmp").unwrap();

        let project = state
            .create_project(CreateProjectRequest {
                name: None,
                root_path: "/tmp".to_owned(),
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
        });
        let error = match result {
            Ok(_) => panic!("empty project path should fail"),
            Err(error) => error,
        };

        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.message, "project root path cannot be empty");
    }

    #[test]
    fn persisted_state_without_projects_migrates_cleanly() {
        let path =
            std::env::temp_dir().join(format!("termal-project-migrate-{}.json", Uuid::new_v4()));
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
    fn denies_claude_control_requests_in_plan_mode() {
        let message = json!({
            "type": "control_request",
            "request_id": "req-plan",
            "request": {
                "subtype": "can_use_tool",
                "tool_name": "Write",
                "input": {
                    "file_path": "/tmp/plan.txt",
                    "content": "hi\n"
                },
                "decision_reason": "Write requires approval"
            }
        });
        let mut state = ClaudeTurnState::default();

        let action =
            classify_claude_control_request(&message, &mut state, ClaudeApprovalMode::Plan)
                .unwrap()
                .unwrap();

        match action {
            ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Deny {
                request_id,
                message,
            }) => {
                assert_eq!(request_id, "req-plan");
                assert_eq!(
                    message,
                    "TermAl denied this tool request because Claude is in plan mode."
                );
            }
            ClaudeControlRequestAction::QueueApproval { .. } => {
                panic!("expected Claude plan mode to deny the request");
            }
            ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Allow { .. }) => {
                panic!("expected Claude plan mode to deny the request");
            }
        }
    }

    #[test]
    fn parses_claude_model_options_from_initialize_response() {
        let message = json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": "init-1",
                "response": {
                    "models": [
                        {
                            "value": "default",
                            "displayName": "Default (recommended)"
                        },
                        {
                            "value": "sonnet",
                            "displayName": "Sonnet"
                        }
                    ]
                }
            }
        });

        assert_eq!(
            claude_model_options(&message),
            Some(vec![
                SessionModelOption {
                    label: "Default (recommended)".to_owned(),
                    value: "default".to_owned(),
                },
                SessionModelOption {
                    label: "Sonnet".to_owned(),
                    value: "sonnet".to_owned(),
                },
            ])
        );
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
    fn encodes_claude_set_model_request() {
        let mut buffer = Vec::new();
        write_claude_set_model(&mut buffer, "claude-sonnet-4-6").unwrap();

        let encoded = String::from_utf8(buffer).unwrap();
        let message: Value = serde_json::from_str(encoded.trim_end()).unwrap();

        assert_eq!(message["type"], "control_request");
        assert_eq!(message["request"]["subtype"], "set_model");
        assert_eq!(message["request"]["model"], "claude-sonnet-4-6");
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

    #[test]
    fn git_file_actions_stage_and_unstage_modified_files() {
        let repo_root = create_test_git_repo();
        let file_path = repo_root.join("tracked.txt");
        fs::write(&file_path, "changed\n").unwrap();

        run_git_pathspec_command(
            &repo_root,
            &["add", "-A"],
            &["tracked.txt".to_owned()],
            "failed to stage git changes",
        )
        .unwrap();

        let staged_status = load_git_status_for_path(&repo_root).unwrap();
        let staged_file = staged_status
            .files
            .iter()
            .find(|file| file.path == "tracked.txt")
            .unwrap();
        assert_eq!(staged_file.index_status.as_deref(), Some("M"));
        assert_eq!(staged_file.worktree_status.as_deref(), None);

        run_git_pathspec_command(
            &repo_root,
            &["restore", "--staged"],
            &["tracked.txt".to_owned()],
            "failed to unstage git changes",
        )
        .unwrap();

        let unstaged_status = load_git_status_for_path(&repo_root).unwrap();
        let unstaged_file = unstaged_status
            .files
            .iter()
            .find(|file| file.path == "tracked.txt")
            .unwrap();
        assert_eq!(unstaged_file.index_status.as_deref(), None);
        assert_eq!(unstaged_file.worktree_status.as_deref(), Some("M"));

        fs::remove_dir_all(repo_root).unwrap();
    }

    #[test]
    fn git_file_actions_revert_untracked_files() {
        let repo_root = create_test_git_repo();
        let file_path = repo_root.join("scratch.txt");
        fs::write(&file_path, "temp\n").unwrap();

        revert_git_file_action(&repo_root, "scratch.txt", None, Some("?")).unwrap();

        assert!(!file_path.exists());
        let status = load_git_status_for_path(&repo_root).unwrap();
        assert!(status.files.is_empty());

        fs::remove_dir_all(repo_root).unwrap();
    }

    fn create_test_git_repo() -> PathBuf {
        let repo_root = std::env::temp_dir().join(format!("termal-git-action-{}", Uuid::new_v4()));
        fs::create_dir_all(&repo_root).unwrap();
        run_git_test_command(&repo_root, &["init", "-q"]);
        run_git_test_command(&repo_root, &["config", "user.email", "test@example.com"]);
        run_git_test_command(&repo_root, &["config", "user.name", "TermAl Test"]);
        fs::write(repo_root.join("tracked.txt"), "base\n").unwrap();
        run_git_test_command(&repo_root, &["add", "tracked.txt"]);
        run_git_test_command(&repo_root, &["commit", "-q", "-m", "init"]);
        repo_root
    }

    fn run_git_test_command(repo_root: &FsPath, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(args)
            .output()
            .unwrap();

        if !output.status.success() {
            panic!(
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
