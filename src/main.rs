/*
Backend composition guide
main.rs owns three top-level concerns:
  1. selecting the process mode
  2. assembling the HTTP router
  3. declaring the backend's modules and re-exporting their items
High-level flow:
CLI args
  -> run()
     -> server    -> run_server() -> app_router() -> AppState + HTTP/SSE handlers
     -> repl      -> run_turn_blocking() -> recorder callbacks -> stdout
     -> delegation-mcp -> run_delegation_mcp_bridge() -> parent-scoped MCP tools
     -> sessions / mailbox -> run_coordination_cli() -> loopback mailbox and session tools
Most files are modules, and main.rs imports what they share with `use x::*;`, so
shared backend types stay reachable by their bare names without a dense web of
pub mod exports. The behavior is split across
src/api.rs, src/state.rs, src/runtime.rs, src/turns.rs, src/remote.rs,
src/orchestrators.rs, and the Telegram relay modules named below.
*/

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

// Every `eprintln!` in the crate, in every module,
// goes through the host console log (host_log.rs): in server mode each line
// gets a UTC timestamp and recurring diagnostics are rate limited per kind.
macro_rules! eprintln {
    () => {
        $crate::host_log::host_log_line(::std::format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::host_log::host_log_line(::std::format_args!($($arg)*))
    };
}

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
mod engram_host;
use engram_host::{EngramHost, EngramRecorderObservation};
mod host_command;
mod host_log;
mod host_utilization;
// The foundation is exercised by tests until the delivery routes are mounted.
#[cfg_attr(not(test), allow(dead_code))]
mod remote_prompt_authority;
#[cfg_attr(not(test), allow(dead_code))]
mod remote_prompt_protocol;
use host_command::Command;
#[cfg(windows)]
mod windows_launch;
use std::convert::Infallible;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::path::{Path as FsPath, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, LazyLock, Mutex, OnceLock, RwLock};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use base64::Engine as _;
use chrono::Local;
#[cfg(not(test))]
use notify::{Config as NotifyConfig, RecommendedWatcher, RecursiveMode, Watcher};
use notify::{Event as NotifyEvent, EventKind as NotifyEventKind};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use shared_child::SharedChild;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use uuid::Uuid;

const MAX_IMAGE_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
const MAX_JSON_REQUEST_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_FILE_CONTENT_BYTES: usize = 10 * 1024 * 1024;

/// Starts the backend entrypoint. A delegation's Codex keeper is selected
/// first, before any runtime thread exists, and leaves without one.
fn main() {
    #[cfg(target_os = "linux")]
    if let Some(code) = codex_keeper::run_codex_keeper_if_requested() {
        std::process::exit(code);
    }
    run_main();
}

#[tokio::main]
async fn run_main() {
    capture_federation_process_image();
    process_limits::configure_process_open_file_limit();
    ignore_sigpipe();
    if let Err(err) = run().await {
        eprintln!("fatal: {err:#}");
        // Usage errors of the coordination CLI exit with 2; every other
        // failure keeps the historical exit code 1.
        std::process::exit(coordination_cli_exit_code(&err));
    }
}

/// Ignores SIGPIPE process-wide so writes to a closed child pipe (e.g. a
/// dead shared Codex app-server) return `ErrorKind::BrokenPipe` instead of
/// terminating TermAl. Rust's stdlib leaves SIGPIPE at `SIG_DFL` for shell
/// pipeline compatibility, which is fatal for a server that supervises
/// long-lived child processes — without this, a Codex crash silently kills
/// TermAl before the runtime supervisor in `src/codex.rs` can run its
/// cleanup paths and respawn the child.
#[cfg(unix)]
fn ignore_sigpipe() {
    // SAFETY: setting a process-wide signal disposition once, immediately
    // after tokio starts and before any code writes to a child pipe. No
    // thread inspects the previous handler so the return value is ignored.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

#[cfg(not(unix))]
fn ignore_sigpipe() {}

/// Runs the selected entry-point mode: the HTTP server (default), an
/// interactive REPL, the stdio delegation MCP bridge, or the one-shot
/// coordination CLI (`sessions` / `mailbox`) that talks to a running server.
async fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match Mode::parse(args)? {
        Mode::Server => run_server().await,
        Mode::Repl { agent } => run_repl(agent),
        Mode::ReviewFreeze(args) => {
            tokio::task::spawn_blocking(move || review_freeze_mode(&args)).await?
        }
        Mode::DelegationMcp {
            parent_session_id,
            base_url,
        } => tokio::task::spawn_blocking(move || {
            run_delegation_mcp_bridge(
                parent_session_id,
                base_url.unwrap_or_else(default_termal_http_base_url),
            )
        })
        .await
        .map_err(|err| anyhow!("delegation MCP bridge task failed: {err}"))?,
        Mode::CoordinationCli(invocation) => {
            tokio::task::spawn_blocking(move || run_coordination_cli(invocation))
                .await
                .map_err(|err| anyhow!("coordination CLI task failed: {err}"))?
        }
    }
}

/// Runs the backend server.
async fn run_server() -> Result<()> {
    host_log::enable_host_console();
    let boot_started = std::time::Instant::now();
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

    // The data directory's instance lock is taken before anything is loaded
    // and held until the process exits, after every background writer.
    let state = AppState::new_server(cwd.clone())?;
    let shutdown_state = state.clone();
    let static_files = Router::new()
        .fallback_service(
            ServeDir::new(ui_dist_dir).not_found_service(ServeFile::new(ui_index_file)),
        )
        .layer(axum::middleware::from_fn(output_limit_http_guard));
    let app = app_router(state.clone())
        .fallback_service(static_files)
        .layer(axum::middleware::from_fn(output_size_http_observer))
        // Last response adapter, including static fallback and compressed bodies.
        .layer(axum::middleware::from_fn(chunk_http_response_writes));

    let listener = TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind backend to {address}"))?;
    disable_socket_inheritance(&listener);
    let bound = listener
        .local_addr()
        .context("failed to read local backend address")?;
    state.set_local_http_base_url(format!("http://{bound}"));
    state.prepare_codex_account_restore();
    eprintln!(
        "startup> listener bound elapsed_ms={}",
        boot_started.elapsed().as_millis()
    );

    // Restore the full inventory on the owned boot worker. Its publication
    // prepares per-session readiness before dependent requests can observe it.
    // Independent HTTP routes can serve throughout, and resumed runtimes see
    // the already-bound base URL.
    let post_listen_boot_worker = state.start_post_listen_boot()?;
    // Bound, with recovery started: the journal's own thread records this boot
    // and, from then on, how this instance ends.
    let host_lifecycle = HostLifecycle::start(&state);
    let federation_runtime = start_federation_runtime(state.clone());
    let utilization_runtime = host_utilization::Runtime::start(state.clone())
        .context("failed to start host utilization sampler")?;

    println!("TermAl backend");
    println!("listening: http://{bound}");
    println!("default cwd: {cwd}");
    println!("ui proxy target: /api");
    #[cfg(not(test))]
    {
        let reconcile_state = state.clone();
        let _telegram_reconcile_task = tokio::task::spawn_blocking(move || {
            reconcile_state.reconcile_telegram_relay_from_saved_settings();
        });
    }

    // Compose the graceful-shutdown future:
    //   1. Wait for Ctrl+C / SIGTERM.
    //   2. Notify all live SSE handlers to break out of their stream loops.
    //
    // Step 2 is mandatory: the SSE handler in `api_sse.rs::state_events`
    // holds `broadcast::Receiver` clones for the state/delta/file event
    // channels, and its loop only exits on `RecvError::Closed`. The
    // matching `Sender` halves live on `AppState`, and we deliberately
    // keep the `shutdown_state` clone alive for the post-serve persist
    // drain — so without an explicit notify, `with_graceful_shutdown`
    // would block forever waiting for SSE streams that never finish.
    // See bugs.md "Graceful shutdown blocks forever waiting for SSE
    // streams to drain".
    let shutdown_state_for_signal = shutdown_state.clone();
    let graceful_shutdown_future = async move {
        shutdown_signal().await;
        // Sticky `watch`-based signal: SSE handlers subscribed before this
        // point see the value flip via `changed()`, and handlers that begin
        // setup AFTER this point observe the `true` value on their initial
        // `borrow_and_update()` pre-check — see api_sse.rs::state_events.
        shutdown_state_for_signal.trigger_shutdown_signal();
    };
    let result = serve_http_with_panic_context(listener, app, graceful_shutdown_future)
        .await
        .context("backend server failed");
    federation_runtime.shutdown().await;
    utilization_runtime.shutdown().await;
    match tokio::task::spawn_blocking(move || post_listen_boot_worker.join()).await {
        Ok(Ok(())) => {}
        _ => eprintln!("startup> post-listen boot worker join failed during shutdown"),
    }
    #[cfg(not(test))]
    shutdown_state.stop_telegram_relay_runtime();

    // Graceful-shutdown drain: block until the background persist thread
    // has written every queued mutation to SQLite. Without this the
    // window between "commit_locked fires" and "row durably in SQLite"
    // can drop the last streamed message on a Ctrl+C / SIGTERM, which
    // makes the assistant's last reply disappear from the UI on the
    // next reconnect (the reconnect snapshot has it missing). See
    // bugs.md "Server restart without browser refresh can lose the
    // last streamed message". Hard kills (SIGKILL, power loss) still
    // race the same window — graceful shutdown only covers the
    // user-initiated restart path.
    let persist_result = shutdown_state.shutdown_persist_checked();
    if let Err(err) = &persist_result {
        eprintln!("[termal] final synchronous persist failed during graceful shutdown: {err:#}");
    }
    let capture_result = if shutdown_state.capture_lifetime.stop_and_join() {
        Ok(())
    } else {
        Err(anyhow!(
            "capture_cleanup_pending: host capture cleanup has not completed"
        ))
    };
    let result = result.and(persist_result).and(capture_result);
    shutdown_state.shutdown_state_broadcast_blocking();
    if let Some(lifecycle) = host_lifecycle {
        let outcome = if result.is_ok() {
            HostShutdownOutcome::Graceful
        } else {
            HostShutdownOutcome::Fatal
        };
        lifecycle.finish(outcome);
    }

    // Drop the AppState (which contains reqwest::blocking::Client) on a
    // regular thread so its internal Tokio runtime isn't dropped inside our
    // async context â€” that would panic with "Cannot drop a runtime in a
    // context where blocking is not allowed".
    std::thread::spawn(move || drop(shutdown_state))
        .join()
        .expect("shutdown cleanup thread panicked");

    result
}

/// Resolves on the first Ctrl+C or SIGTERM. Used as the
/// `with_graceful_shutdown` future for `axum::serve` so the HTTP
/// server stops accepting new connections and waits for in-flight
/// requests to finish before the persist drain runs.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            eprintln!("failed to listen for Ctrl+C shutdown signal: {err}");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(err) => {
                eprintln!("failed to listen for SIGTERM shutdown signal: {err}");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

/// Builds the application router.
fn app_router(state: AppState) -> Router {
    app_router_with_acceptance_policy_limiter(state, AcceptancePolicyLimiter::new(2, 1))
}

fn app_router_with_acceptance_policy_limiter(
    state: AppState,
    policy_limiter: AcceptancePolicyLimiter,
) -> Router {
    let cors = CorsLayer::new()
        .allow_origin([
            HeaderValue::from_static("http://127.0.0.1:8787"),
            HeaderValue::from_static("http://localhost:8787"),
        ])
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::PATCH,
            axum::http::Method::DELETE,
        ])
        .allow_headers([axum::http::header::ACCEPT, axum::http::header::CONTENT_TYPE]);

    Router::new()
        .route("/api/health", get(health))
        .route("/api/codex/account", post(codex_account_action))
        .route(
            "/api/diagnostics/persistence",
            get(get_persistence_diagnostics),
        )
        .route(
            "/api/diagnostics/host-lifecycle",
            get(get_host_lifecycle_diagnostics),
        )
        .route(
            "/api/diagnostics/runtime-observations",
            get(get_runtime_observation_counts),
        )
        .route("/api/diagnostics/closing-jobs", get(get_closing_job_counts))
        .route(
            "/api/diagnostics/named-root-journal",
            get(get_engram_root_journal_diagnostics),
        )
        .route(
            "/api/diagnostics/mailbox-relay-wakes",
            get(get_mailbox_relay_wake_diagnostics),
        )
        .route(
            "/api/federation/peers/{id}/enroll",
            post(enroll_federation_satellite),
        )
        .route(
            "/api/federation/peers/{id}/ssh-host",
            post(update_federation_ssh_host_route),
        )
        .route(
            "/api/federation/peers/{id}/verify",
            post(verify_federation_satellite_route),
        )
        .route(
            "/api/federation/peers/{id}/verification",
            get(get_federation_onboarding_report),
        )
        .route(
            "/api/federation/peers/{id}/activate",
            post(activate_federation_satellite_route),
        )
        .route(
            "/api/federation/peers/{id}/placements",
            post(reserve_federation_mailbox_placement_route),
        )
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
        .route("/api/workspaces/{id}/label", patch(patch_workspace_label))
        .route(
            "/api/workspaces/{id}",
            get(get_workspace_layout)
                .put(put_workspace_layout)
                .delete(delete_workspace_layout),
        )
        .route("/api/settings", post(update_app_settings))
        .route("/api/federation", get(get_federation))
        .merge(host_utilization::routes())
        .route("/api/federation/journal", get(get_federation_journal))
        .route(
            "/api/federation/executor-operation",
            get(get_federation_executor_operation),
        )
        .route("/api/federation/settings", post(update_federation_settings))
        .route("/api/engram/settings", patch(update_engram_host_settings))
        .route("/api/remotes/{id}/register", post(register_remote_termal))
        .route("/api/remotes/{id}/upgrade", post(upgrade_remote_termal))
        .route("/api/telegram/status", get(get_telegram_status))
        .route("/api/telegram/config", post(update_telegram_config))
        .route("/api/telegram/test", post(test_telegram_connection))
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
        .route("/api/events/full-state", post(request_full_state))
        .route("/api/projects", post(create_project))
        .route("/api/projects/{id}", delete(delete_project))
        .route(
            "/api/projects/{id}/environment",
            patch(update_project_environment),
        )
        .route(
            "/api/projects/{id}/engram",
            patch(update_project_engram_settings),
        )
        .route(
            "/api/projects/{id}/engram/verify",
            post(verify_project_engram_settings),
        )
        .route(
            "/api/projects/{id}/engram/audit",
            post(full_audit_project_engram),
        )
        .route("/api/projects/{id}/digest", get(get_project_digest))
        .route(
            "/api/projects/{id}/engram/control-policy",
            get(get_project_acceptance_policy),
        )
        .route(
            "/api/projects/{id}/engram/acceptance-evaluation-defaults",
            patch(patch_project_acceptance_defaults),
        )
        .route(
            "/api/projects/{id}/engram/acceptance-evaluation-policy",
            post(post_project_acceptance_policy),
        )
        .route("/api/projects/{id}/work", get(get_project_work))
        .route(
            "/api/projects/{id}/work-memories/{source}",
            get(get_project_work_memories),
        )
        .route(
            "/api/projects/{id}/work/engram/{work_ref}",
            get(get_project_work_detail),
        )
        .route("/api/test-runs", get(get_test_runs))
        .route("/api/test-run-diagnostics", get(get_test_run_phase_traces))
        .route(
            "/api/sessions/{id}/test-runs",
            post(register_session_test_run),
        )
        .route(
            "/api/sessions/{id}/source-owner",
            get(get_session_source_owner),
        )
        .route("/api/test-runs/{run_id}", get(get_test_run))
        .route(
            "/api/test-runs/{run_id}/interrupt-monitoring",
            post(interrupt_test_run_monitoring),
        )
        .route("/api/test-runs/{run_id}/results", get(get_test_run_results))
        .route(
            "/api/test-runs/{run_id}/stages/{stage}/log",
            get(get_test_run_stage_log),
        )
        .route(
            "/api/projects/{id}/actions/{action_id}",
            post(dispatch_project_action),
        )
        .route("/api/projects/pick", post(pick_project_root))
        .route("/api/sessions", post(create_session))
        .route("/api/sessions/{id}", get(get_session))
        .route("/api/sessions/{id}/history", get(get_session_history))
        .route(
            "/api/sessions/{id}/delegations",
            get(list_session_delegations).post(create_session_delegation),
        )
        .route(
            "/api/sessions/{id}/delegations/{delegation_id}",
            get(get_delegation_status),
        )
        .route(
            "/api/sessions/{id}/delegations/{delegation_id}/result",
            get(get_delegation_result),
        )
        .route(
            "/api/sessions/{id}/delegations/{delegation_id}/result/output",
            get(get_delegation_result_output),
        )
        .route(
            "/api/sessions/{id}/delegations/{delegation_id}/cancel",
            post(cancel_delegation),
        )
        .route(
            "/api/sessions/{id}/delegations/{delegation_id}/followup",
            post(followup_delegation),
        )
        .route(
            "/api/sessions/{id}/delegations/{delegation_id}/resume",
            post(resume_delegation),
        )
        .route(
            "/api/sessions/{id}/delegation-waits",
            post(create_delegation_wait),
        )
        .route(
            "/api/sessions/{id}/test-run-waits",
            post(create_test_run_wait),
        )
        .route("/api/handoffs", get(get_handoffs))
        .route(
            "/api/sessions/{id}/handoffs",
            post(register_session_handoff),
        )
        .route(
            "/api/sessions/{id}/handoffs/{handoff_id}/accept",
            post(accept_session_handoff),
        )
        .route(
            "/api/sessions/{id}/handoffs/{handoff_id}/complete",
            post(complete_session_handoff),
        )
        .route(
            "/api/sessions/{id}/engram-source-root",
            post(name_engram_source_root),
        )
        .route(
            "/api/sessions/{id}/engram-named-root-release",
            post(release_own_engram_named_root_route),
        )
        .route("/api/engram/named-roots", get(list_engram_named_roots))
        .route(
            "/api/engram/named-roots/release",
            post(release_engram_named_root),
        )
        .route(
            "/api/sessions/{id}/markers",
            get(list_session_markers).post(create_session_marker),
        )
        .route(
            "/api/sessions/{id}/markers/{marker_id}",
            patch(update_session_marker).delete(delete_session_marker),
        )
        .route("/api/sessions/{id}/settings", post(update_session_settings))
        .route(
            "/api/sessions/{id}/model-options/refresh",
            post(refresh_session_model_options),
        )
        .route(
            "/api/sessions/{id}/codex/mcp-servers",
            get(list_codex_mcp_servers),
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
        .route(
            "/api/sessions/{id}/agent-commands/{name}/resolve",
            post(resolve_agent_command),
        )
        .route("/api/sessions/{id}/messages", post(send_message))
        .route(
            "/api/sessions/{id}/telegram/documents",
            post(send_session_telegram_document),
        )
        .route("/api/sessions/{id}/mailboxes", get(list_mailboxes_api))
        .route(
            "/api/sessions/{id}/mailbox-references",
            get(list_federation_mailbox_references).post(provision_federation_mailbox_reference),
        )
        .route(
            "/api/sessions/{id}/mailbox-references/{reference}/resolve",
            get(resolve_federation_mailbox_reference),
        )
        .route(
            "/api/sessions/{id}/mailboxes/protocol",
            get(negotiate_federation_mailboxes),
        )
        .route(
            "/api/sessions/{id}/mailboxes/domain",
            post(join_federation_mailboxes),
        )
        .route(
            "/api/sessions/{id}/mailboxes/participants",
            get(list_federation_mailbox_participants_with_basis),
        )
        .route(
            "/api/sessions/{id}/mailboxes/directory",
            get(list_federation_mailbox_directory),
        )
        .route(
            "/api/sessions/{id}/mailbox-intents",
            post(prepare_federation_intent_route),
        )
        .route(
            "/api/sessions/{id}/mailbox-intents/{key}",
            get(get_federation_intent_route),
        )
        .route(
            "/api/sessions/{id}/mailbox-intents/{key}/retry",
            post(retry_federation_intent_route),
        )
        .route(
            "/api/sessions/{id}/mailbox-intents/{key}/cancel",
            post(cancel_federation_intent_route),
        )
        .route(
            "/api/sessions/{id}/mailbox-intents/{key}/gc",
            post(gc_federation_intent_route),
        )
        .route(
            "/api/sessions/{id}/mailboxes/create",
            post(get_or_create_federation_mailbox),
        )
        .route(
            "/api/sessions/{id}/mailboxes/send",
            post(send_mailbox_message),
        )
        .route(
            "/api/sessions/{id}/delegation-review-result",
            post(submit_delegation_review_result),
        )
        .route(
            "/api/sessions/{id}/delegation-review-freeze",
            post(verify_delegation_review_freeze),
        )
        .route(
            "/api/sessions/{id}/acceptance-evaluations",
            post(request_session_acceptance_evaluation),
        )
        .route(
            "/api/sessions/{id}/acceptance-evaluation",
            post(submit_session_acceptance_evaluation),
        )
        .route(
            "/api/sessions/{id}/acceptance-evidence",
            post(read_session_acceptance_evidence),
        )
        .route(
            "/api/sessions/{id}/acceptance-evaluation-replay",
            post(replay_session_acceptance_evaluation),
        )
        .route(
            "/api/sessions/{id}/mailboxes/{mailbox_id}/read",
            post(read_mailbox),
        )
        .route(
            "/api/sessions/{id}/mailboxes/{mailbox_id}/acknowledge",
            post(acknowledge_mailbox),
        )
        .route(
            "/api/sessions/{id}/mailbox-messages/{message_id}",
            get(read_mailbox_message),
        )
        .route("/api/sessions/{id}/board", get(list_coordination_board))
        .route(
            "/api/sessions/{id}/board/keys/{key}",
            get(get_coordination_board_key),
        )
        .route(
            "/api/sessions/{id}/board/set",
            post(set_coordination_board_key),
        )
        .route(
            "/api/response-board/tabs",
            get(list_response_board_tabs).post(create_response_board_tab),
        )
        .route(
            "/api/response-board/tabs/reorder",
            post(reorder_response_board_tabs),
        )
        .route(
            "/api/response-board/tabs/{id}",
            get(get_response_board_tab)
                .patch(update_response_board_tab)
                .delete(delete_response_board_tab),
        )
        .route(
            "/api/response-board/cards/stage",
            post(stage_response_board_card),
        )
        .route(
            "/api/response-board/cards/{id}",
            patch(update_response_board_card).delete(delete_response_board_card),
        )
        .route(
            "/api/sessions/{id}/queued-prompts/{prompt_id}/cancel",
            post(cancel_queued_prompt),
        )
        .route(
            "/api/sessions/{id}/queue/resume",
            post(resume_session_queue),
        )
        .route("/api/queues/resume-paused", post(resume_paused_queues))
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
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            delegation_inventory_http_guard,
        ))
        .with_state(state)
        .layer(axum::Extension(policy_limiter))
        .layer(axum::Extension(EngramAuditLimiter(Arc::new(
            tokio::sync::Semaphore::new(1),
        ))))
        .layer(axum::Extension(EngramReadinessLimiter(Arc::new(
            tokio::sync::Semaphore::new(2),
        ))))
        // One 10 MiB image expands to about 13.4 MiB when base64-encoded.
        .layer(DefaultBodyLimit::max(MAX_JSON_REQUEST_BODY_BYTES))
        .layer(axum::middleware::from_fn(output_limit_http_guard))
        .layer(axum::middleware::from_fn(output_size_http_observer))
        .layer(CompressionLayer::new())
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
                codex_fast_mode: false,
                codex_sandbox_mode: Some(default_codex_sandbox_mode()),
                agent,
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
    ReviewFreeze(Vec<String>),
    Repl {
        agent: Agent,
    },
    DelegationMcp {
        parent_session_id: String,
        base_url: Option<String>,
    },
    /// `sessions ...` / `mailbox ...`: the coordination CLI, a loopback HTTP
    /// client for agents whose MCP configuration is locked down.
    CoordinationCli(CoordinationCliInvocation),
}

impl Mode {
    /// Parses CLI arguments into an entry-point `Mode`. First arg
    /// selects between the HTTP server (`server`, default), the
    /// delegation MCP bridge (`delegation-mcp`), the coordination CLI
    /// (`sessions` / `mailbox`), or a REPL session (`repl` / `cli` +
    /// agent name, or a bare REPL-capable agent name like `codex`).
    fn parse(args: Vec<String>) -> Result<Self> {
        // Reuse the side-effect-free coordination help path before selecting
        // a server, provider runtime, bridge or filesystem inspection mode.
        let tail_requests_help = || args.iter().skip(1).any(|arg| is_cli_help_argument(arg));
        let requests_help = match args.first().map(String::as_str) {
            Some("--help" | "-h" | "help") => true,
            // Complete verification commands own every positional argument,
            // including paths named help, -h or --help.
            Some("review-freeze-check") => args.len() == 2 && tail_requests_help(),
            // These parsers distinguish options from consumed data values.
            Some("sessions" | "mailbox" | "federation" | "delegation-mcp") | None => false,
            Some("server" | "repl" | "cli" | "--agent" | "telegram" | "telegram-bot") => {
                tail_requests_help()
            }
            Some(agent) => Agent::from_str(agent).is_ok() && tail_requests_help(),
        };
        if requests_help {
            return Ok(Self::CoordinationCli(coordination_cli_help()));
        }
        match args.first().map(String::as_str) {
            None | Some("server") => Ok(Self::Server),
            Some("review-freeze-check") => Ok(Self::ReviewFreeze(args)),
            Some("sessions") | Some("mailbox") | Some("federation") => {
                Ok(Self::CoordinationCli(parse_coordination_cli_args(args)?))
            }
            // Legacy standalone Telegram aliases are intentionally rejected:
            // the relay is owned by server mode and configured from Settings.
            Some("telegram") | Some("telegram-bot") => bail!(
                "Telegram relay runs inside server mode; configure it from Settings -> Telegram"
            ),
            Some("delegation-mcp") => parse_delegation_mcp_mode_args(args.into_iter().skip(1)),
            Some("repl") | Some("cli") => Self::repl(Agent::parse(args.into_iter().skip(1))?),
            _ => Self::repl(Agent::parse(args.into_iter())?),
        }
    }

    fn repl(agent: Agent) -> Result<Self> {
        if agent == Agent::Claude {
            bail!(
                "Claude REPL mode is not supported; create a Claude session in the TermAl server UI so Claude Code can run as a persistent stdio process"
            );
        }
        Ok(Self::Repl { agent })
    }
}

mod process_limits;

mod remote_authority;
use remote_authority::*;
mod federation_identity;
use federation_identity::*;
mod federation_mailbox_protocol;
use federation_mailbox_protocol::*;
mod mailbox_relay_protocol;
use mailbox_relay_protocol::*;
mod mailbox_test_request;
use mailbox_test_request::*;
mod mailbox_test_request_custody;
use mailbox_test_request_custody::*;
mod test_run_causal_request;
use test_run_causal_request::*;
mod mailbox_relay_errors;
use mailbox_relay_errors::*;
mod federation_durability_diagnostics;
use federation_durability_diagnostics::*;
mod federation_link_diagnostics;
use federation_link_diagnostics::*;
mod mailbox_relay_schema;
use mailbox_relay_schema::*;
mod mailbox_relay_store;
use mailbox_relay_store::*;
mod mailbox_relay_rpc;
use mailbox_relay_rpc::*;
mod mailbox_relay_local_history;

mod mailbox_relay_dispatch;
use mailbox_relay_dispatch::*;
mod mailbox_relay_wakes;
use mailbox_relay_wakes::*;
mod mailbox_relay_routes;
use mailbox_relay_routes::*;
mod federation_mailbox_errors;
use federation_mailbox_errors::*;
mod federation_mailbox_reads;
use federation_mailbox_reads::*;
mod federation_delivery_schema;
use federation_delivery_schema::*;
mod federation_executor_schema;
use federation_executor_schema::*;
mod federation_executor_journal;
use federation_executor_journal::*;
mod remote_prompt_storage_schema;
use remote_prompt_storage_schema::*;
mod remote_prompt_journal;
use remote_prompt_journal::*;
mod federation_executor_reservations;
use federation_executor_reservations::*;
mod federation_executor_mirrors;
use federation_executor_mirrors::*;
mod federation_executor_drain;
use federation_executor_drain::*;
mod federation_executor_lifecycle;
use federation_executor_lifecycle::*;
mod federation_executor_activation;
use federation_executor_activation::*;
mod federation_executor_protocol;
use federation_executor_protocol::*;
mod federation_executor_authority;
use federation_executor_authority::*;
mod federation_executor_delivery;
use federation_executor_delivery::*;
mod federation_executor_effects;
use federation_executor_effects::*;
mod federation_executor_routes;
use federation_executor_routes::*;
mod federation_executor_recovery;
use federation_executor_recovery::*;
mod federation_executor_operation_view;
use federation_executor_operation_view::*;
mod federation_executor_outcomes;
use federation_executor_outcomes::*;
mod federation_delivery;
use federation_delivery::*;
mod federation_delivery_replay;
use federation_delivery_replay::*;
mod federation_delivery_intents;
use federation_delivery_intents::*;
mod federation_delivery_routes;
use federation_delivery_routes::*;
mod federation_mailbox_directory;

mod federation_mailbox_identity;
use federation_mailbox_identity::*;
mod federation_mailbox_bindings;

mod federation_mailbox_participation;
use federation_mailbox_participation::*;
mod federation_mailbox_participation_rpc;
use federation_mailbox_participation_rpc::*;
mod federation_mailbox_catalog;
use federation_mailbox_catalog::*;
mod federation_mailbox_discovery;

mod federation_mailbox_local_history;
use federation_mailbox_local_history::*;
mod federation_entrypoints_protocol;
use federation_entrypoints_protocol::*;
mod federation_entrypoints_activation;
use federation_entrypoints_activation::*;
mod federation_entrypoints_binding;
use federation_entrypoints_binding::*;
mod federation_entrypoints_placement;
use federation_entrypoints_placement::*;
mod federation_mailbox_namespace;
use federation_mailbox_namespace::*;
mod federation_mailbox_authority;
use federation_mailbox_authority::*;
mod federation_mailbox_operations;
use federation_mailbox_operations::*;
mod federation_mailbox_wake_worker;
use federation_mailbox_wake_worker::*;
mod federation_mailbox_routes;
use federation_mailbox_routes::*;
mod federation_native_binding_proof;
use federation_native_binding_proof::*;
#[macro_use]
mod federation_mailbox_references;
use federation_mailbox_references::*;
mod federation_mailbox_reference_routes;
use federation_mailbox_reference_routes::*;
mod federation_mailbox_store;

mod federation_mailbox_rpc;
use federation_mailbox_rpc::*;
mod federation_mailbox_bridge;
use federation_mailbox_bridge::*;
mod federation_mailbox_alias;
use federation_mailbox_alias::*;
mod mailbox_append_wake;

mod federation_registry;
use federation_registry::*;
mod federation_prompt_policy;
use federation_prompt_policy::*;
mod federation_prompt_policy_current;
use federation_prompt_policy_current::*;
mod federation_prompt_policy_rpc;
use federation_prompt_policy_rpc::*;
mod federation_prompt_admission;
#[cfg(test)]
use federation_prompt_admission::*;
mod federation_prompt_operator;
#[cfg(test)]
use federation_prompt_operator::*;
mod remote_prompt_projection;
use remote_prompt_projection::*;
mod federation_onboarding_record;
use federation_onboarding_record::*;
mod federation_onboarding_protocol;
use federation_onboarding_protocol::*;
mod federation_onboarding_store;
use federation_onboarding_store::*;
mod federation_onboarding_archive;
use federation_onboarding_archive::*;
mod federation_onboarding_probe;
use federation_onboarding_probe::*;
mod federation_image_identity;
use federation_image_identity::*;
mod federation_onboarding_engram;
use federation_onboarding_engram::*;
mod federation_onboarding_routes;
use federation_onboarding_routes::*;
mod federation_onboarding_verify;
use federation_onboarding_verify::*;
mod federation_requalification;
use federation_requalification::*;
mod federation_onboarding_api;
use federation_onboarding_api::*;
mod federation_snapshots;
use federation_snapshots::*;
mod federation_mutations;
use federation_mutations::*;
mod federation_membership;

mod federation_views;

mod federation_operator;
use federation_operator::*;
mod federation_journal_cursor;
use federation_journal_cursor::*;
mod federation_journal_rows;
use federation_journal_rows::*;
mod federation_journal_scan;
use federation_journal_scan::*;
mod federation_journal;
use federation_journal::*;
mod federation_executor_observation;
use federation_executor_observation::*;
mod federation_placement;
use federation_placement::*;
mod persist_federation;
use persist_federation::*;
mod persist_federation_image;
use persist_federation_image::*;
mod persist_federation_reference_publication;
use persist_federation_reference_publication::*;
mod remote;
use remote::*;
mod remote_ssh;
use remote_ssh::*;
mod federation_ssh;
use federation_ssh::*;
mod federation_bounds;
use federation_bounds::*;
mod federation_basis;
use federation_basis::*;
mod federation_link;
use federation_link::*;
mod federation_crypto;
use federation_crypto::*;
mod federation_security_context;
use federation_security_context::*;
mod federation_attestation_status;
use federation_attestation_status::*;
mod federation_worker_observation;
use federation_worker_observation::*;
mod federation_control_settings;
use federation_control_settings::*;
mod federation_security_install;
use federation_security_install::*;
mod federation_attestation;
use federation_attestation::*;
mod federation_attestation_budget;
use federation_attestation_budget::*;
mod federation_control_heartbeat;
use federation_control_heartbeat::*;
mod federation_stdio_control;
use federation_stdio_control::*;
mod federation_listener_control;
use federation_listener_control::*;
mod federation_http_mac;
use federation_http_mac::*;
mod federation_http_authenticate;
use federation_http_authenticate::*;
mod federation_rpc_authenticated;

mod federation_protocol;
use federation_protocol::*;
mod federation_channel_authority;

mod federation_listener;
use federation_listener::*;
mod federation_listener_io;
use federation_listener_io::*;
mod federation_rpc;
use federation_rpc::*;
mod federation_credentials;
use federation_credentials::*;
mod federation_ssh_diagnostics;
use federation_ssh_diagnostics::*;
mod federation_process;
use federation_process::*;
mod federation_runtime_io;
use federation_runtime_io::*;
mod federation_ssh_settlement;
use federation_ssh_settlement::*;
mod federation_credential_read;
use federation_credential_read::*;
mod federation_control_clock;
use federation_control_clock::*;
mod federation_recovery;
use federation_recovery::*;
mod federation_runtime;
use federation_runtime::*;
mod federation_peer;
use federation_peer::*;
mod federation_monitor;
use federation_monitor::*;
mod remote_terminal;
use remote_terminal::*;
mod remote_routes;
#[cfg(test)]
use remote_routes::*;
mod remote_delta_apply;

mod remote_create_proxies;
use remote_create_proxies::*;
mod remote_codex_proxies;

mod remote_session_proxies;
#[cfg(test)]
use remote_session_proxies::*;
mod remote_sync;
use remote_sync::*;
#[cfg(test)]
mod test_temp_paths;
#[cfg(test)]
use test_temp_paths::*;
#[cfg(test)]
mod test_temp_root;
#[cfg(test)]
use test_temp_root::*;
#[cfg(test)]
mod test_panic_journal;

mod state;
use state::*;
mod state_lock_observer;
use state_lock_observer::*;
mod engram_budget;
use engram_budget::*;
mod engram_connection_reset;
use engram_connection_reset::*;
mod engram_host_adapter;
use engram_host_adapter::*;
mod engram_store_transient;
use engram_store_transient::*;
mod engram_control_non_application;
use engram_control_non_application::*;
mod engram_session_authority;
use engram_session_authority::*;
mod engram_causal_failure;
use engram_causal_failure::*;
mod engram_host_launch;
use engram_host_launch::*;
mod engram_turn_observations;
use engram_turn_observations::*;
mod engram_source_sightings;
use engram_source_sightings::*;
mod engram_source_observation_protocol;
use engram_source_observation_protocol::*;
mod engram_unadmitted_observation;
use engram_unadmitted_observation::*;
mod engram_unadmitted_capture;
use engram_unadmitted_capture::*;
mod engram_unadmitted_closing;
use engram_unadmitted_closing::*;
mod engram_unadmitted_delivery;
use engram_unadmitted_delivery::*;
mod engram_runtime_observation_counts;
use engram_runtime_observation_counts::*;
mod engram_closing_job_counts;
use engram_closing_job_counts::*;
#[cfg(test)]
#[path = "tests/engram_closing_job_counts.rs"]
mod closing_job_counts_tests;

mod engram_unadmitted_worker;
use engram_unadmitted_worker::*;
mod engram_unadmitted_lifecycle;
use engram_unadmitted_lifecycle::*;
mod engram_source_observation_removal;
use engram_source_observation_removal::*;
mod engram_held_claims;
use engram_held_claims::*;
mod engram_turn_checks;
use engram_turn_checks::*;
mod engram_carried_checks;
use engram_carried_checks::*;
mod engram_watcher_order;
use engram_watcher_order::*;
mod engram_verification_bind;
use engram_verification_bind::*;
mod engram_batch_binds;
use engram_batch_binds::*;
mod engram_reported_ends;
use engram_reported_ends::*;
mod engram_claude_interference;
use engram_claude_interference::*;
mod engram_check_recognition;
use engram_check_recognition::*;
mod engram_build_checks;
use engram_build_checks::*;
mod engram_focused_node_checks;
use engram_focused_node_checks::*;
mod engram_launcher_stages;
use engram_launcher_stages::*;
mod engram_one_call;
use engram_one_call::*;
mod engram_declared_tests;
use engram_declared_tests::*;
mod engram_trx_artifact;
use engram_trx_artifact::*;
mod engram_dart_artifact;
use engram_dart_artifact::*;
mod engram_declared_diagnostics;
use engram_declared_diagnostics::*;
mod engram_check_paths;
use engram_check_paths::*;
mod engram_write_places;
use engram_write_places::*;
mod engram_check_toolchain;
use engram_check_toolchain::*;
mod engram_control_stderr;
use engram_control_stderr::*;
mod engram_control_transport;
use engram_control_transport::*;
mod engram_readiness;
use engram_readiness::*;
#[cfg(test)]
mod engram_readiness_test_seam;
#[cfg(test)]
use engram_readiness_test_seam::*;
#[cfg(test)]
mod engram_doctor_deadline_test_seam;
#[cfg(test)]
use engram_doctor_deadline_test_seam::*;
#[cfg(test)]
mod engram_test_child_temp;
#[cfg(test)]
use engram_test_child_temp::*;
#[cfg(test)]
mod terminal_test_child_temp;
#[cfg(test)]
use terminal_test_child_temp::*;
mod engram_containment_checkpoint;
use engram_containment_checkpoint::*;
mod engram_session_obligations;
use engram_session_obligations::*;
mod engram_import_reconciliation;
use engram_import_reconciliation::*;
mod engram_import_hold_decision;
#[cfg(test)]
use engram_import_hold_decision::*;
mod persist_import_reconciliation;
use persist_import_reconciliation::*;
mod engram_control_mode;
use engram_control_mode::*;
mod human_prompts;
use human_prompts::*;
mod human_queue_choices;
use human_queue_choices::*;
mod coordination_instructions;
use coordination_instructions::*;
mod delegation_mcp;
use delegation_mcp::*;
mod delegation_mcp_source_root;
use delegation_mcp_source_root::*;
mod delegation_mcp_named_root_release;
use delegation_mcp_named_root_release::*;
mod delegation_mcp_timeouts;
use delegation_mcp_timeouts::*;
mod delegation_mcp_durability_pending;
use delegation_mcp_durability_pending::*;
mod delegation_mcp_unknown_outcome;
use delegation_mcp_unknown_outcome::*;
mod delegation_mcp_review_freeze;

mod review_freeze;
use review_freeze::*;
mod capture_lifetime;
use capture_lifetime::*;
mod source_capture;
use source_capture::*;
mod engram_source_id;
use engram_source_id::*;
mod engram_source_command;
use engram_source_command::*;
mod engram_source_owner;
use engram_source_owner::*;
mod engram_observation_source;
use engram_observation_source::*;
mod engram_source_roots;
use engram_source_roots::*;
mod engram_naming_source;

mod engram_named_root_binding;
use engram_named_root_binding::*;
mod engram_named_root_journal;
use engram_named_root_journal::*;
mod engram_root_capture_reason;
use engram_root_capture_reason::*;
mod engram_root_authority;
use engram_root_authority::*;
mod engram_retained_run_settlement;
use engram_retained_run_settlement::*;
mod engram_named_root_reclamation;
use engram_named_root_reclamation::*;
mod engram_named_root_release;
use engram_named_root_release::*;
mod engram_named_root_capacity;
use engram_named_root_capacity::*;
#[cfg(test)]
mod engram_named_root_test_transport;
#[cfg(test)]
use engram_named_root_test_transport::*;
mod bounded_read_cleanup;
#[cfg(any(unix, test))]
use bounded_read_cleanup::*;
mod bounded_read_retained;
#[cfg(any(unix, test))]
use bounded_read_retained::*;
mod bounded_read_process;
use bounded_read_process::*;
mod review_freeze_process;
use review_freeze_process::*;
mod review_freeze_api;
use review_freeze_api::*;
mod acceptance_evaluation;
use acceptance_evaluation::*;
mod acceptance_source;
use acceptance_source::*;
mod acceptance_evaluation_attempts;
use acceptance_evaluation_attempts::*;
mod acceptance_evaluation_refusal;
use acceptance_evaluation_refusal::*;
mod acceptance_evaluation_citation_retry;
use acceptance_evaluation_citation_retry::*;
mod acceptance_evaluation_reuse;
use acceptance_evaluation_reuse::*;
mod acceptance_evaluation_brief_delivery;

mod acceptance_evaluation_api;
use acceptance_evaluation_api::*;
mod acceptance_evidence_recovery;
use acceptance_evidence_recovery::*;
mod acceptance_evaluation_replay;
use acceptance_evaluation_replay::*;
mod acceptance_named_root_sighting;
use acceptance_named_root_sighting::*;
mod acceptance_committed_input;
use acceptance_committed_input::*;
mod acceptance_retired_checks;
use acceptance_retired_checks::*;
mod acceptance_native_input;
use acceptance_native_input::*;
mod acceptance_evidence_selection;
use acceptance_evidence_selection::*;
mod acceptance_verification_evidence;

mod acceptance_settings;
use acceptance_settings::*;
mod acceptance_evaluation_cards;
use acceptance_evaluation_cards::*;
mod coordination_cli;
use coordination_cli::*;
mod engram_mcp_config;
use engram_mcp_config::*;
mod work_visualizer_types;
use work_visualizer_types::*;
mod work_visualizer_process;
use work_visualizer_process::*;
mod work_visualizer;
use work_visualizer::*;
mod work_visualizer_detail;
use work_visualizer_detail::*;
mod work_memories;
use work_memories::*;
mod test_runs;
use test_runs::*;
mod test_run_registration;
use test_run_registration::*;
mod declared_run_source;
use declared_run_source::*;
mod declared_run_observer;
use declared_run_observer::*;
mod native_test_completion;

mod test_run_native_completion;
use test_run_native_completion::*;
mod test_run_causal_completion;
use test_run_causal_completion::*;
mod mailbox_test_request_pending;
use mailbox_test_request_pending::*;
mod test_run_causal_completion_scan;
use test_run_causal_completion_scan::*;
mod test_run_causal_completion_batch;
use test_run_causal_completion_batch::*;
mod coordination_causal_completion_ready;
use coordination_causal_completion_ready::*;
mod test_run_causal_completion_ready;
use test_run_causal_completion_ready::*;
mod test_run_causal_completion_notify;
use test_run_causal_completion_notify::*;
mod mailbox_test_request_settlement;
use mailbox_test_request_settlement::*;
mod engram_causal_completion_delivery;
use engram_causal_completion_delivery::*;
mod test_run_named_root;
use test_run_named_root::*;
mod test_run_registration_target;
use test_run_registration_target::*;
mod test_run_exact_query;
use test_run_exact_query::*;
mod test_run_phase_trace;
use test_run_phase_trace::*;
#[cfg(test)]
mod test_run_history_probe;
#[cfg(test)]
use test_run_history_probe::*;
mod test_run_history_store;
#[cfg(test)]
use test_run_history_store::{
    SkipContractGuardForTiming, TEST_HISTORY_INDEX_MAINTENANCE, contract_guard_skipped,
};
use test_run_history_store::{
    TestRunCompactedStore, TestRunLifecycleKey, TestRunObservationStore, TestRunRegistrationStore,
};
mod test_run_observations;
use test_run_observations::*;
mod test_run_overview;
use test_run_overview::*;
mod test_run_transport_projection;
use test_run_transport_projection::*;
mod test_run_evidence;
use test_run_evidence::*;
mod test_run_artifacts;
use test_run_artifacts::*;
mod test_run_compaction;
use test_run_compaction::*;
mod test_runs_disk;
use test_runs_disk::*;
mod test_run_committed_input;
use test_run_committed_input::*;
mod engram_capabilities;
use engram_capabilities::*;
mod engram_verification_record;
use engram_verification_record::*;
mod engram_verification_unclassified;
use engram_verification_unclassified::*;
mod engram_remote_verification;

mod test_runs_api;
use test_runs_api::*;
mod test_run_cards;
use test_run_cards::*;
mod test_run_waits;
use test_run_waits::*;
mod test_run_remote_waits;
use test_run_remote_waits::*;
mod session_runtime;
use session_runtime::*;
mod runtime_process_tree;
use runtime_process_tree::*;
mod session_interaction;
use session_interaction::*;
mod queued_prompt_durability;
use queued_prompt_durability::*;
mod handoffs;
use handoffs::*;
mod prompt_delivery;
use prompt_delivery::*;
mod remote_prompt_primary;
use remote_prompt_primary::*;
mod prompt_delivery_shutdown;

mod engram_queued_admission;
use engram_queued_admission::*;
mod engram_retry_schedule;

mod provider_capacity_retry;
use provider_capacity_retry::*;
mod queue_pause_reason;
use queue_pause_reason::*;
mod queue_resume_all;
use queue_resume_all::*;
mod message_sequence;
use message_sequence::SharedSequence;
type MessageSequence = SharedSequence<Message>;
type SessionSequence = SharedSequence<SessionRecord>;
mod messages;
use messages::*;
mod workspace_watch;
use workspace_watch::*;
mod file_event_attribution;
use file_event_attribution::*;
mod codex_discovery;
use codex_discovery::*;
mod codex_validation;
use codex_validation::*;
mod persisted_state;
use persisted_state::*;
mod persist;
use persist::*;
mod persist_fence;
use persist_fence::*;
mod persist_tick_timings;
use persist_tick_timings::*;
mod persist_observer;
use persist_observer::*;
mod host_lifecycle;
use host_lifecycle::*;
mod coordination_persist;
use coordination_persist::*;
mod mailboxes;
use mailboxes::*;
mod mailbox_status_text;
use mailbox_status_text::*;
mod mailbox_current_status;
use mailbox_current_status::*;
mod mailbox_relay_progress;
use mailbox_relay_progress::*;
mod mailbox_acknowledged_wake;
use mailbox_acknowledged_wake::*;
mod mailbox_continuation_coverage;
use mailbox_continuation_coverage::*;
mod delegation_review_results;
use delegation_review_results::*;
mod delegation_review_attempts;
use delegation_review_attempts::*;
mod delegation_review_custody;
use delegation_review_custody::*;
mod coordination_board;
use coordination_board::*;
mod board_routes;
use board_routes::*;
mod response_board;
use response_board::*;
mod runtime;
use runtime::*;
mod claude_spawn;
use claude_spawn::*;
mod claude_turn_ownership;
use claude_turn_ownership::*;
mod claude_frame_router;
use claude_frame_router::*;
mod claude_frame_application;
use claude_frame_application::*;
mod claude_compact_hook;
use claude_compact_hook::*;
mod claude_runtime_turns;
use claude_runtime_turns::*;
mod claude_outstanding_work;
use claude_outstanding_work::*;
mod codex_home;
use codex_home::*;
mod codex_account;
use codex_account::*;
mod claude_args;
use claude_args::*;
mod codex_bin;
use codex_bin::*;
mod acp;
use acp::*;
mod acp_permission_summary;
use acp_permission_summary::*;
mod acp_opencode_config;
use acp_opencode_config::*;
mod codex;
use codex::*;
mod owned_codex_server;
use owned_codex_server::*;
mod codex_engram_bootstrap;
#[cfg(target_os = "linux")]
mod codex_keeper;
use codex_engram_bootstrap::*;
mod codex_thread_recovery;
use codex_thread_recovery::*;
mod codex_approval_sandbox;
use codex_approval_sandbox::*;
mod codex_events;
use codex_events::*;
mod codex_launcher_output;
use codex_launcher_output::*;
mod codex_terminal_ownership;
use codex_terminal_ownership::*;
mod codex_terminal_effects;
use codex_terminal_effects::*;
mod codex_notices;
use codex_notices::*;
mod codex_text_stream;
use codex_text_stream::*;
mod codex_app_requests;
use codex_app_requests::*;
mod codex_turn_cleanup;
use codex_turn_cleanup::*;
mod codex_rpc;
use codex_rpc::*;
mod codex_thread_actions;

mod codex_delegation_release;
use codex_delegation_release::*;
mod codex_mcp;
#[cfg(test)]
use codex_mcp::*;
mod codex_reviewer_mcp;
use codex_reviewer_mcp::*;
mod codex_reviewer_mcp_lifecycle;
use codex_reviewer_mcp_lifecycle::*;
mod turn_lifecycle;
use turn_lifecycle::*;
mod session_lifecycle;
#[cfg(test)]
use session_lifecycle::*;
mod session_stop_idle_wait;

mod session_messages;

mod codex_submissions;
use codex_submissions::*;
mod session_config;

mod session_markers;
use session_markers::*;
mod turn_dispatch;
use turn_dispatch::*;
mod session_crud;
use session_crud::*;
mod project_environment;
use project_environment::*;
mod sse_broadcast;
use sse_broadcast::*;
mod state_broadcast_worker;
use state_broadcast_worker::*;
mod state_broadcast_diff;
use state_broadcast_diff::*;
mod shared_codex_mgr;

mod shared_codex_profile;
use shared_codex_profile::*;
mod workspace_queries;
use workspace_queries::*;
mod session_identity;

mod session_sync;

mod state_accessors;
use state_accessors::*;
mod session_history;
use session_history::*;
mod session_body_sequence;
use session_body_sequence::*;
mod state_boot;

mod session_restart_recovery;
use session_restart_recovery::*;
mod state_inner;
use state_inner::*;
mod cold_transcript;
use cold_transcript::*;
mod delegations;
use delegations::*;
mod delegation_inventory;
use delegation_inventory::*;
mod delegation_worker;
use delegation_worker::*;
mod delegation_worker_handoff;
use delegation_worker_handoff::*;
mod delegation_worktree_location;
use delegation_worktree_location::*;
mod delegation_worktree_owner;
use delegation_worktree_owner::*;
mod delegation_attempt_state;
use delegation_attempt_state::*;
mod delegation_followup_admission;
use delegation_followup_admission::*;
mod delegation_result_parser;
use delegation_result_parser::*;
mod ids;
use ids::*;
mod app_boot;
use app_boot::*;
mod server_instance_lock;
#[cfg(test)]
use server_instance_lock::*;
mod telegram_settings;
use telegram_settings::*;
mod agent_readiness;
use agent_readiness::*;
mod gemini;
use gemini::*;
mod opencode;
use opencode::*;
mod kimi;
use kimi::*;
mod kimi_read_only;
use kimi_read_only::*;
mod kimi_approvals;
use kimi_approvals::*;
mod kimi_questions;
use kimi_questions::*;
mod turns;
use turns::*;
mod recorders;
use recorders::*;
mod claude;
use claude::*;
mod claude_read_only_git;
use claude_read_only_git::*;
mod claude_read_only_refusal;
use claude_read_only_refusal::*;
mod repl_codex;
use repl_codex::*;
mod api;
use api::*;
mod http_write_chunks;
use http_write_chunks::*;
mod http_panic_context;
use http_panic_context::*;
mod api_git;
use api_git::*;
mod api_files;
use api_files::*;
mod api_sse;
use api_sse::*;
mod wire;
use wire::*;
mod output_encoding;
use output_encoding::*;
mod output_size;
use output_size::*;
mod wire_git;
use wire_git::*;
mod wire_terminal;
use wire_terminal::*;
mod wire_project_digest;
use wire_project_digest::*;
mod wire_messages;
use wire_messages::*;
mod instructions;
use instructions::*;
mod git;
use git::*;
mod terminal;
use terminal::*;
mod paths;
use paths::*;
// Isolated module owns COM lifetimes, pub(super) entry point and platform tests.
#[cfg(windows)]
mod windows_folder_picker;

mod orchestrators;
use orchestrators::*;
mod orchestrator_lifecycle;

mod orchestrator_transitions;
use orchestrator_transitions::*;
mod telegram_runtime;
use telegram_runtime::*;
mod telegram_state;
use telegram_state::*;
mod telegram_clients;
use telegram_clients::*;
mod telegram_messages;
use telegram_messages::*;
mod telegram_forwarding;
use telegram_forwarding::*;
mod telegram_digest;
use telegram_digest::*;
mod telegram;
use telegram::*;
mod telegram_documents;
use telegram_documents::*;
mod telegram_document_files;
use telegram_document_files::*;
mod telegram_document_handle;
use telegram_document_handle::*;
mod telegram_document_upload;
use telegram_document_upload::*;

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
