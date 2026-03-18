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
        .route("/api/git/diff", post(read_git_diff))
        .route("/api/git/file", post(apply_git_file_action))
        .route("/api/git/commit", post(commit_git_changes))
        .route("/api/state", get(get_state))
        .route("/api/settings", post(update_app_settings))
        .route("/api/instructions/search", get(search_instructions))
        .route("/api/events", get(state_events))
        .route("/api/projects", post(create_project))
        .route("/api/projects/pick", post(pick_project_root))
        .route("/api/sessions", post(create_session))
        .route("/api/sessions/{id}/settings", post(update_session_settings))
        .route(
            "/api/sessions/{id}/model-options/refresh",
            post(refresh_session_model_options),
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

include!("state.rs");
include!("runtime.rs");
include!("turns.rs");
include!("api.rs");

#[cfg(test)]
mod tests;
