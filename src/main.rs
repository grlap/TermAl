/*
Backend composition guide
main.rs owns three top-level concerns:
  1. selecting the process mode
  2. assembling the HTTP router
  3. stitching the backend together through include! fragments
High-level flow:
CLI args
  -> run()
     -> server    -> run_server() -> app_router() -> AppState + HTTP/SSE handlers
     -> repl      -> run_turn_blocking() -> recorder callbacks -> stdout
     -> telegram  -> run_telegram_bot() -> project digest/action bridge
The crate still compiles as one module so shared backend types stay crate-visible
without a dense web of pub mod exports, but the behavior is split across
src/api.rs, src/state.rs, src/runtime.rs, src/turns.rs, src/remote.rs,
src/orchestrators.rs, and src/telegram.rs.
*/

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::convert::Infallible;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::path::{Path as FsPath, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use axum::extract::DefaultBodyLimit;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::Engine as _;
use chrono::Local;
#[cfg(not(test))]
use notify::{
    Config as NotifyConfig, Event as NotifyEvent, EventKind as NotifyEventKind, RecommendedWatcher,
    RecursiveMode, Watcher,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use shared_child::SharedChild;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use uuid::Uuid;

const MAX_IMAGE_ATTACHMENT_BYTES: usize = 5 * 1024 * 1024;
const MAX_FILE_CONTENT_BYTES: usize = 10 * 1024 * 1024;

/// Starts the backend entrypoint.
#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("fatal: {err:#}");
        std::process::exit(1);
    }
}

/// Runs the selected CLI mode.
async fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match Mode::parse(args)? {
        Mode::Server => run_server().await,
        Mode::Repl { agent } => run_repl(agent),
        Mode::Telegram => tokio::task::spawn_blocking(run_telegram_bot)
            .await
            .map_err(|err| anyhow!("telegram adapter task failed: {err}"))?,
    }
}

/// Runs the backend server.
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
    let shutdown_state = state.clone();
    let app = app_router(state).fallback_service(
        ServeDir::new(ui_dist_dir).not_found_service(ServeFile::new(ui_index_file)),
    );

    let listener = TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind backend to {address}"))?;
    disable_socket_inheritance(&listener);
    let bound = listener
        .local_addr()
        .context("failed to read local backend address")?;

    println!("TermAl backend");
    println!("listening: http://{bound}");
    println!("default cwd: {cwd}");
    println!("ui proxy target: /api");

    let result = axum::serve(listener, app)
        .await
        .context("backend server failed");

    // Drop the AppState (which contains reqwest::blocking::Client) on a
    // regular thread so its internal Tokio runtime isn't dropped inside our
    // async context â€” that would panic with "Cannot drop a runtime in a
    // context where blocking is not allowed".
    std::thread::spawn(move || drop(shutdown_state))
        .join()
        .expect("shutdown cleanup thread panicked");

    result
}

/// Builds the application router.
fn app_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin([
            HeaderValue::from_static("http://127.0.0.1:8787"),
            HeaderValue::from_static("http://localhost:8787"),
        ])
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::DELETE,
        ])
        .allow_headers([axum::http::header::ACCEPT, axum::http::header::CONTENT_TYPE]);

    Router::new()
        .route("/api/health", get(health))
        .route("/api/file", get(read_file).put(write_file))
        .route("/api/fs", get(read_directory))
        .route("/api/git/status", get(read_git_status))
        .route("/api/git/diff", post(read_git_diff))
        .route("/api/git/file", post(apply_git_file_action))
        .route("/api/git/commit", post(commit_git_changes))
        .route("/api/git/push", post(push_git_changes))
        .route("/api/git/sync", post(sync_git_changes))
        .route("/api/terminal/run", post(run_terminal_command))
        .route(
            "/api/terminal/run/stream",
            post(run_terminal_command_stream),
        )
        .route("/api/state", get(get_state))
        .route("/api/workspaces", get(list_workspace_layouts))
        .route(
            "/api/workspaces/{id}",
            get(get_workspace_layout)
                .put(put_workspace_layout)
                .delete(delete_workspace_layout),
        )
        .route("/api/settings", post(update_app_settings))
        .route(
            "/api/orchestrators/templates",
            get(list_orchestrator_templates).post(create_orchestrator_template),
        )
        .route(
            "/api/orchestrators/templates/{id}",
            get(get_orchestrator_template)
                .put(update_orchestrator_template)
                .delete(delete_orchestrator_template),
        )
        .route(
            "/api/orchestrators",
            get(list_orchestrator_instances).post(create_orchestrator_instance),
        )
        .route("/api/orchestrators/{id}", get(get_orchestrator_instance))
        .route(
            "/api/orchestrators/{id}/pause",
            post(pause_orchestrator_instance),
        )
        .route(
            "/api/orchestrators/{id}/resume",
            post(resume_orchestrator_instance),
        )
        .route(
            "/api/orchestrators/{id}/stop",
            post(stop_orchestrator_instance),
        )
        .route("/api/instructions/search", get(search_instructions))
        .route("/api/events", get(state_events))
        .route(
            "/api/reviews/{change_set_id}",
            get(get_review).put(put_review),
        )
        .route(
            "/api/reviews/{change_set_id}/summary",
            get(get_review_summary),
        )
        .route("/api/projects", post(create_project))
        .route("/api/projects/{id}", delete(delete_project))
        .route("/api/projects/{id}/digest", get(get_project_digest))
        .route(
            "/api/projects/{id}/actions/{action_id}",
            post(dispatch_project_action),
        )
        .route("/api/projects/pick", post(pick_project_root))
        .route("/api/sessions", post(create_session))
        .route("/api/sessions/{id}", get(get_session))
        .route("/api/sessions/{id}/settings", post(update_session_settings))
        .route(
            "/api/sessions/{id}/model-options/refresh",
            post(refresh_session_model_options),
        )
        .route(
            "/api/sessions/{id}/codex/thread/fork",
            post(fork_codex_thread),
        )
        .route(
            "/api/sessions/{id}/codex/thread/archive",
            post(archive_codex_thread),
        )
        .route(
            "/api/sessions/{id}/codex/thread/unarchive",
            post(unarchive_codex_thread),
        )
        .route(
            "/api/sessions/{id}/codex/thread/compact",
            post(compact_codex_thread),
        )
        .route(
            "/api/sessions/{id}/codex/thread/rollback",
            post(rollback_codex_thread),
        )
        .route(
            "/api/sessions/{id}/agent-commands",
            get(list_agent_commands),
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
        .route(
            "/api/sessions/{id}/user-input/{message_id}",
            post(submit_user_input),
        )
        .route(
            "/api/sessions/{id}/mcp-elicitation/{message_id}",
            post(submit_mcp_elicitation),
        )
        .route(
            "/api/sessions/{id}/codex/requests/{message_id}",
            post(submit_codex_app_request),
        )
        .with_state(state)
        .layer(DefaultBodyLimit::max(10 * 1024 * 1024)) // 10 MB — image attachments are base64 in JSON
        .layer(cors)
}

/// Builds the persisted change-set ID for a diff message.
fn diff_change_set_id(message_id: &str) -> String {
    format!("change-{message_id}")
}

/// Runs the REPL loop for the selected agent.
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
                claude_effort: Some(default_claude_effort()),
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

/// Enumerates value modes.
enum Mode {
    Server,
    Repl { agent: Agent },
    Telegram,
}

impl Mode {
    /// Parses CLI arguments into an entry-point `Mode`. First arg
    /// selects between the HTTP server (`server`, default), the
    /// Telegram bot (`telegram` / `telegram-bot`), or a REPL
    /// session (`repl` / `cli` + agent name, or a bare agent name
    /// like `claude` / `codex`).
    fn parse(args: Vec<String>) -> Result<Self> {
        match args.first().map(String::as_str) {
            None | Some("server") => Ok(Self::Server),
            Some("telegram") | Some("telegram-bot") => Ok(Self::Telegram),
            Some("repl") | Some("cli") => Ok(Self::Repl {
                agent: Agent::parse(args.into_iter().skip(1))?,
            }),
            _ => Ok(Self::Repl {
                agent: Agent::parse(args.into_iter())?,
            }),
        }
    }
}

include!("remote.rs");
include!("remote_ssh.rs");
include!("remote_terminal.rs");
include!("remote_routes.rs");
include!("remote_create_proxies.rs");
include!("remote_codex_proxies.rs");
include!("remote_session_proxies.rs");
include!("remote_sync.rs");
include!("state.rs");
include!("session_runtime.rs");
include!("session_interaction.rs");
include!("messages.rs");
include!("workspace_watch.rs");
include!("codex_discovery.rs");
include!("codex_validation.rs");
include!("persisted_state.rs");
include!("persist.rs");
include!("runtime.rs");
include!("claude_spawn.rs");
include!("codex_home.rs");
include!("claude_args.rs");
include!("codex_bin.rs");
include!("acp.rs");
include!("codex.rs");
include!("codex_events.rs");
include!("codex_notices.rs");
include!("codex_text_stream.rs");
include!("codex_app_requests.rs");
include!("codex_turn_cleanup.rs");
include!("codex_rpc.rs");
include!("codex_thread_actions.rs");
include!("turn_lifecycle.rs");
include!("session_lifecycle.rs");
include!("session_messages.rs");
include!("codex_submissions.rs");
include!("session_config.rs");
include!("turn_dispatch.rs");
include!("session_crud.rs");
include!("sse_broadcast.rs");
include!("shared_codex_mgr.rs");
include!("claude_spares.rs");
include!("workspace_queries.rs");
include!("session_identity.rs");
include!("session_sync.rs");
include!("state_accessors.rs");
include!("state_boot.rs");
include!("state_inner.rs");
include!("ids.rs");
include!("app_boot.rs");
include!("agent_readiness.rs");
include!("gemini.rs");
include!("turns.rs");
include!("recorders.rs");
include!("claude.rs");
include!("repl_codex.rs");
include!("api.rs");
include!("api_git.rs");
include!("api_files.rs");
include!("api_sse.rs");
include!("api_review.rs");
include!("wire.rs");
include!("wire_git.rs");
include!("wire_terminal.rs");
include!("wire_review.rs");
include!("wire_project_digest.rs");
include!("wire_messages.rs");
include!("instructions.rs");
include!("git.rs");
include!("terminal.rs");
include!("review.rs");
include!("paths.rs");
include!("orchestrators.rs");
include!("orchestrator_lifecycle.rs");
include!("orchestrator_transitions.rs");
include!("telegram.rs");

/// Marks the listening socket as non-inheritable so child processes (agent
/// runtimes) do not keep the server port locked after the parent exits.
#[cfg(windows)]
fn disable_socket_inheritance(listener: &TcpListener) {
    use std::os::windows::io::AsRawSocket as _;
    unsafe extern "system" {
        fn SetHandleInformation(handle: *mut std::ffi::c_void, mask: u32, flags: u32) -> i32;
    }
    const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
    let raw = listener.as_raw_socket() as *mut std::ffi::c_void;
    // Safety: the raw socket is valid for the lifetime of `listener`.
    let updated = unsafe { SetHandleInformation(raw, HANDLE_FLAG_INHERIT, 0) };
    if updated == 0 {
        eprintln!(
            "backend warning> failed to disable socket inheritance: {}",
            io::Error::last_os_error()
        );
    }
}

#[cfg(not(windows))]
fn disable_socket_inheritance(_listener: &TcpListener) {}

#[cfg(test)]
mod tests;
