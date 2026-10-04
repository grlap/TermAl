// Claude CLI subprocess spawn + NDJSON wire message writers.
//
// This file owns the boundary between TermAl and the `claude` CLI:
// spawning the subprocess with the right argv + cwd + env, wiring
// stdin/stdout/stderr through the shared runtime plumbing, and
// formatting each outbound NDJSON message that goes down the
// subprocess's stdin.
//
// `spawn_claude_runtime` is the entry point. Called from
// `session_crud.rs` on first-prompt dispatch for a Claude session.
// It builds the argv (via `claude_cli_*_args` in `claude_args.rs`),
// spawns the child, wraps stdout in the NDJSON reader, and returns
// a `ClaudeRuntimeHandle` the caller parks on `SessionRuntime::Claude`.
//
// The `write_claude_*` helpers format specific outbound messages —
// each follows the CLI's JSON-over-stdio contract (one JSON object
// per line):
//
// - `write_claude_initialize` — opening handshake
// - `write_claude_prompt_message` — user prompt + attachments
// - `write_claude_permission_response` — user's answer to a pending
//   tool-approval request
// - `write_claude_set_permission_mode` — flip approval mode at
//   runtime (e.g. "approve all tools for this session")
// - `write_claude_set_model` — switch the active model mid-session
// - `write_claude_message` — inner helper, all the above call it
//
// Protocol-level parsing of the messages coming *back* from Claude
// lives in `claude.rs` (`handle_claude_message` and friends).

const CLAUDE_TRANSIENT_API_RETRY_ATTEMPTS: u32 = 5;
const CLAUDE_TRANSIENT_API_RETRY_BASE_DELAY: Duration = Duration::from_millis(200);
const CLAUDE_RATE_LIMIT_RETRY_BASE_DELAY: Duration = Duration::from_secs(1);

// Readiness and spawning share this resolver. In particular, Windows batch
// shims are not native executables and this runtime does not launch a shell.
// Unlike find_command_on_path (Codex's shim-capable adapter), this deliberately
// requires native/executable files and canonicalizes before changing cwd.
fn resolve_claude_executable() -> Option<PathBuf> {
    resolve_claude_executable_on_path(&std::env::var_os("PATH")?)
}

fn resolve_claude_executable_on_path(path: &std::ffi::OsStr) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    };
    std::env::split_paths(path).find_map(|dir| {
        let candidate = dir.join(name);
        if !candidate.is_file() {
            return None;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if candidate.metadata().ok()?.permissions().mode() & 0o111 == 0 {
                return None;
            }
        }
        // Pin the discovered path before setting the child's working directory.
        fs::canonicalize(candidate).ok()
    })
}

/// Numeric status codes Claude Code reports for API failures that are safe to
/// retry without changing the request.
///
/// Claude Code 2.1.220 exposes these in terminal stream-json result objects as
/// `api_error_status`. Missing or malformed numeric status stays terminal;
/// result prose is never a substitute for this current wire field.
fn claude_transient_api_status(message: &Value) -> Option<u16> {
    if message.get("type").and_then(Value::as_str) != Some("result")
        || message.get("is_error").and_then(Value::as_bool) != Some(true)
    {
        return None;
    }

    message
        .get("api_error_status")
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok())
        .filter(|status| matches!(status, 429 | 503 | 529))
}

fn claude_transient_api_retry_delay(
    session_id: &str,
    completed_attempts: u32,
    status: u16,
) -> Duration {
    // A 429 represents a capacity window rather than the short-lived server
    // blip signalled by 503/529. Keep the same bounded five-attempt policy, but
    // start at one second so retries do not rapidly consume the rate budget.
    let base_delay = if status == 429 {
        CLAUDE_RATE_LIMIT_RETRY_BASE_DELAY
    } else {
        CLAUDE_TRANSIENT_API_RETRY_BASE_DELAY
    };
    session_stable_retry_delay(base_delay, session_id, completed_attempts)
}

#[derive(Debug, PartialEq, Eq)]
enum ClaudeTransientApiResult {
    Retry {
        completed_attempts: u32,
        delay: Duration,
        status: u16,
    },
    Exhausted {
        completed_attempts: u32,
        status: u16,
    },
}

fn classify_claude_transient_api_result(
    message: &Value,
    session_id: &str,
    prior_completed_attempts: u32,
    replay_safe: bool,
) -> Option<ClaudeTransientApiResult> {
    if !replay_safe {
        return None;
    }
    let status = claude_transient_api_status(message)?;
    let completed_attempts = prior_completed_attempts.saturating_add(1);
    if completed_attempts < CLAUDE_TRANSIENT_API_RETRY_ATTEMPTS {
        Some(ClaudeTransientApiResult::Retry {
            completed_attempts,
            delay: claude_transient_api_retry_delay(session_id, completed_attempts, status),
            status,
        })
    } else {
        Some(ClaudeTransientApiResult::Exhausted {
            completed_attempts,
            status,
        })
    }
}

type ClaudeReplayPrompt = Arc<Mutex<Option<ClaudePromptCommand>>>;

fn claude_replay_generation(replay_prompt: &ClaudeReplayPrompt) -> Option<String> {
    replay_prompt
        .lock()
        .expect("Claude replay prompt mutex poisoned")
        .as_ref()
        .map(|prompt| prompt.replay_generation.clone())
}

fn clear_claude_replay_prompt_if_matches(
    replay_prompt: &ClaudeReplayPrompt,
    replay_generation: &str,
) -> bool {
    let mut replay_prompt = replay_prompt
        .lock()
        .expect("Claude replay prompt mutex poisoned");
    if replay_prompt
        .as_ref()
        .is_some_and(|prompt| prompt.replay_generation == replay_generation)
    {
        *replay_prompt = None;
        true
    } else {
        false
    }
}

/// Writes one runtime command and retains the exact last successfully-written
/// prompt for transient API replay. A prompt's owner is reserved in
/// `ownership` before its bytes are written, so the reader can name the turn
/// the prompt opens however the write and the runtime's frames interleave, and
/// it is released when the write fails.
fn write_claude_runtime_command(
    writer: &mut impl Write,
    replay_prompt: &ClaudeReplayPrompt,
    ownership: &ClaudeTurnOwnership,
    command: ClaudeRuntimeCommand,
) -> Result<()> {
    match command {
        ClaudeRuntimeCommand::Prompt(prompt) => {
            let replay_generation = prompt.replay_generation.clone();
            let wire_prompt = prompt.clone();
            let owner = claude_host_prompt_owner(&prompt);
            let attempt_uuid = owner.attempt_uuid.clone();
            lock_claude_turn_ownership(ownership).reserve(owner);
            *replay_prompt
                .lock()
                .expect("Claude replay prompt mutex poisoned") = Some(prompt);
            if let Err(err) = write_claude_prompt_message(writer, &wire_prompt, &attempt_uuid) {
                clear_claude_replay_prompt_if_matches(replay_prompt, &replay_generation);
                lock_claude_turn_ownership(ownership).release(&attempt_uuid);
                return Err(err);
            }
            Ok(())
        }
        ClaudeRuntimeCommand::RetryLastPrompt { ticket, .. } => {
            let prompt = replay_prompt
                .lock()
                .expect("Claude replay prompt mutex poisoned")
                .as_ref()
                .filter(|prompt| prompt.replay_generation == ticket.replay_generation)
                .cloned();
            if let Some(prompt) = prompt {
                // A fresh uuid for the attempt, so a late frame of the failed
                // attempt never names this one.
                let owner = claude_host_prompt_attempt_owner(
                    &prompt,
                    ticket.attempt,
                    Uuid::new_v4().to_string(),
                );
                let attempt_uuid = owner.attempt_uuid.clone();
                lock_claude_turn_ownership(ownership).reserve(owner);
                if let Err(err) = write_claude_prompt_message(writer, &prompt, &attempt_uuid) {
                    lock_claude_turn_ownership(ownership).release(&attempt_uuid);
                    return Err(err);
                }
            }
            Ok(())
        }
        ClaudeRuntimeCommand::PermissionResponse(decision) => {
            write_claude_permission_response(writer, &decision)
        }
        ClaudeRuntimeCommand::HookResponse(response) => {
            write_claude_message(writer, &claude_hook_response_message(&response))
        }
        ClaudeRuntimeCommand::SetModel(model) => write_claude_set_model(writer, &model),
        ClaudeRuntimeCommand::SetPermissionMode(mode) => {
            write_claude_set_permission_mode(writer, &mode)
        }
    }
}

/// The writer thread's receive loop: each command, in order, until every
/// sender of the channel is gone (the runtime handle and the reader dropped
/// theirs) or the runtime can no longer be written to. Commands still queued
/// when the last sender goes are written first. `context` carries no sender,
/// so the loop never keeps its own channel open.
fn run_claude_writer(
    context: &ClaudeRuntimeContext,
    writer: &mut impl Write,
    input_rx: mpsc::Receiver<ClaudeRuntimeCommand>,
) {
    while let Ok(command) = input_rx.recv() {
        if !apply_claude_writer_command(context, writer, command) {
            break;
        }
    }
}

/// Carries out one command on the runtime's stdin, as the writer thread
/// does. A delayed retry is written only while it is still the pending
/// attempt of the live turn on this runtime, and it is taken here, so it is
/// written at most once; every other command is written as given. Returns
/// `false` once the runtime cannot be written to.
fn apply_claude_writer_command(
    context: &ClaudeRuntimeContext,
    writer: &mut impl Write,
    command: ClaudeRuntimeCommand,
) -> bool {
    if let ClaudeRuntimeCommand::RetryLastPrompt {
        ticket,
        retry_detail,
    } = &command
    {
        let replay_generation = &ticket.replay_generation;
        if !claude_retry_is_current(
            &context.state,
            &context.session_id,
            &context.token,
            &context.replay_prompt,
            &context.ownership,
            ticket,
        ) || !lock_claude_turn_ownership(&context.ownership).take_pending_retry(ticket)
        {
            release_claude_replay_prompt_of_refused_retry(
                &context.replay_prompt,
                &context.ownership,
                ticket,
            );
            return true;
        }
        match context.state.note_turn_retry_if_runtime_and_generation_match(
            &context.session_id,
            &context.token,
            ticket.turn_generation,
            retry_detail,
        ) {
            Ok(true) => {}
            Ok(false) => {
                clear_claude_replay_prompt_if_matches(&context.replay_prompt, replay_generation);
                return true;
            }
            Err(err) => {
                clear_claude_replay_prompt_if_matches(&context.replay_prompt, replay_generation);
                let _ = context.state.fail_turn_if_runtime_matches(
                    &context.session_id,
                    &context.token,
                    &format!("failed to record Claude automatic retry: {err:#}"),
                );
                return true;
            }
        }
        if !context.state.turn_retry_allowed_if_runtime_and_generation_match(
            &context.session_id,
            &context.token,
            ticket.turn_generation,
        ) {
            clear_claude_replay_prompt_if_matches(&context.replay_prompt, replay_generation);
            return true;
        }
    }
    // A hook answer is checked at the write boundary: a cancelled request is
    // not written, and stale context is not either. Its context is delivered
    // once it is written, and only then (`claude_compact_hook.rs`).
    let command = match command {
        ClaudeRuntimeCommand::HookResponse(response) => {
            match claude_hook_response_to_write(context, response) {
                Some(response) => ClaudeRuntimeCommand::HookResponse(response),
                None => return true,
            }
        }
        command => command,
    };
    let hook_write = match &command {
        ClaudeRuntimeCommand::HookResponse(response) => Some(response.write_record()),
        _ => None,
    };
    let result =
        write_claude_runtime_command(writer, &context.replay_prompt, &context.ownership, command);
    if let Some(hook_write) = hook_write {
        finish_claude_hook_write(context, hook_write, result.is_ok());
    }
    if let Err(err) = result {
        let _ = context.state.handle_runtime_exit_if_matches(
            &context.session_id,
            &context.token,
            Some(&format!("failed to write prompt to Claude stdin: {err:#}")),
        );
        return false;
    }
    true
}

/// Whether the delayed retry `ticket` may still be written: it is the retry
/// pending (no newer prompt or attempt replaced it), the replay prompt still
/// holds its prompt, and its turn is still the session's live turn on this
/// runtime (no stop, no new turn, no new runtime).
fn claude_retry_is_current(
    state: &AppState,
    session_id: &str,
    runtime_token: &RuntimeToken,
    replay_prompt: &ClaudeReplayPrompt,
    ownership: &ClaudeTurnOwnership,
    ticket: &ClaudeRetryTicket,
) -> bool {
    lock_claude_turn_ownership(ownership).retry_is_pending(ticket)
        && claude_replay_generation(replay_prompt).as_deref()
            == Some(ticket.replay_generation.as_str())
        && state.turn_retry_allowed_if_runtime_and_generation_match(
            session_id,
            runtime_token,
            ticket.turn_generation,
        )
}

/// Releases the replay prompt of a refused retry `ticket`, unless another
/// retry of the same prompt is pending and still needs it.
fn release_claude_replay_prompt_of_refused_retry(
    replay_prompt: &ClaudeReplayPrompt,
    ownership: &ClaudeTurnOwnership,
    ticket: &ClaudeRetryTicket,
) {
    let other_pending = lock_claude_turn_ownership(ownership)
        .pending_retry
        .as_ref()
        .is_some_and(|pending| {
            pending != ticket && pending.replay_generation == ticket.replay_generation
        });
    if !other_pending {
        clear_claude_replay_prompt_if_matches(replay_prompt, &ticket.replay_generation);
    }
}

fn dispatch_claude_retry_if_current(
    state: &AppState,
    session_id: &str,
    runtime_token: &RuntimeToken,
    retry_sender: &Sender<ClaudeRuntimeCommand>,
    replay_prompt: &ClaudeReplayPrompt,
    ownership: &ClaudeTurnOwnership,
    ticket: &ClaudeRetryTicket,
    retry_detail: &str,
) -> bool {
    if !claude_retry_is_current(
        state,
        session_id,
        runtime_token,
        replay_prompt,
        ownership,
        ticket,
    ) {
        release_claude_replay_prompt_of_refused_retry(replay_prompt, ownership, ticket);
        return false;
    }
    if let Err(err) = retry_sender.send(ClaudeRuntimeCommand::RetryLastPrompt {
        ticket: ticket.clone(),
        retry_detail: retry_detail.to_owned(),
    }) {
        clear_claude_replay_prompt_if_matches(replay_prompt, &ticket.replay_generation);
        let _ = state.fail_turn_if_runtime_matches(
            session_id,
            runtime_token,
            &format!("failed to queue Claude automatic retry: {err}"),
        );
        return false;
    }
    true
}

// Claude receives its MCP configuration as a private file path, never as an
// inline JSON argv value. The file lives under TermAl's own data tree, is
// created exclusively (`create_new`) with owner-only permissions on POSIX, is
// released as soon as Claude's first valid stdout line proves the
// configuration was read, and is removed by the guard's drop when the spawn
// fails or the runtime exits without ever printing. There is deliberately no
// boot-time sweep: TermAl holds no single-instance lock, so a second process
// sharing the data directory could not tell a crashed owner's leftover from a
// live file another process wrote a moment ago; a leftover after a crash in
// that window carries only the grant hash that the protected state database
// already stores.
const CLAUDE_MCP_CONFIG_FILE_PREFIX: &str = "claude-mcp-";
const CLAUDE_MCP_CONFIG_FILE_SUFFIX: &str = ".json";

// Derived from the persistence path, never from HOME: production resolves to
// the `~/.termal` tree next to `termal.sqlite`, and tests stay inside their
// explicit temporary persistence root instead of touching the operator's
// real data directory.
fn claude_mcp_config_dir(persistence_path: &FsPath) -> PathBuf {
    persistence_path
        .parent()
        .map(FsPath::to_path_buf)
        .unwrap_or_else(|| persistence_path.to_path_buf())
        .join("delegations")
        .join("mcp")
}

/// Owns one private Claude MCP configuration file; dropping the guard removes it.
struct ClaudeMcpConfigFile {
    path: PathBuf,
}

impl Drop for ClaudeMcpConfigFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Releases the private configuration as soon as Claude has proven it read it:
/// the first valid stdout line means CLI startup and configuration parsing are
/// complete, so the file must not stay readable for the rest of the session
/// (an agent tool could otherwise follow its own `--mcp-config` argv path to
/// the secret). Idempotent; the guard's drop remains the fallback.
fn release_private_claude_mcp_config(slot: &mut Option<ClaudeMcpConfigFile>) {
    slot.take();
}

/// Stops a live Claude child after a reader-side protocol failure and leaves
/// the actionable reason for the waiter that owns token-guarded runtime
/// cleanup. On a kill failure the override is removed so the caller can
/// record the teardown failure directly without the waiter duplicating it.
fn terminate_claude_runtime_after_control_failure(
    process: &Arc<SharedChild>,
    runtime_exit_error_override: &Arc<Mutex<Option<String>>>,
    detail: &str,
) -> Result<()> {
    *runtime_exit_error_override
        .lock()
        .expect("Claude runtime-exit override mutex poisoned") = Some(detail.to_owned());
    if let Err(error) = kill_child_process(process, "Claude") {
        runtime_exit_error_override
            .lock()
            .expect("Claude runtime-exit override mutex poisoned")
            .take();
        return Err(error);
    }
    Ok(())
}

/// Waits for the stdout reader to finish publishing any reader-side failure
/// before the process waiter consumes the one-shot exit override. A Claude
/// process can exit while its final control message is still being handled;
/// joining here prevents that last diagnostic from losing a race with
/// `SharedChild::wait`.
fn take_claude_runtime_exit_error_after_reader(
    reader_thread: std::thread::JoinHandle<()>,
    runtime_exit_error_override: &Arc<Mutex<Option<String>>>,
) -> Option<String> {
    let reader_panicked = reader_thread.join().is_err();
    let mut error_override = runtime_exit_error_override
        .lock()
        .expect("Claude runtime-exit override mutex poisoned");
    if reader_panicked && error_override.is_none() {
        *error_override = Some("Claude stdout reader panicked".to_owned());
    }
    error_override.take()
}

fn claude_mcp_config_file_name(runtime_id: &str) -> String {
    format!("{CLAUDE_MCP_CONFIG_FILE_PREFIX}{runtime_id}{CLAUDE_MCP_CONFIG_FILE_SUFFIX}")
}

fn write_private_claude_mcp_config(
    dir: &FsPath,
    runtime_id: &str,
    contents: &str,
) -> Result<ClaudeMcpConfigFile> {
    fs::create_dir_all(dir).with_context(|| format!("failed to create `{}`", dir.display()))?;
    // The directory must be a real directory in TermAl's data tree, not a
    // symlink that redirects the secret somewhere else.
    let dir_metadata = fs::symlink_metadata(dir)
        .with_context(|| format!("failed to inspect `{}`", dir.display()))?;
    if dir_metadata.file_type().is_symlink() || !dir_metadata.is_dir() {
        return Err(anyhow!(
            "refusing to write the Claude MCP configuration: `{}` is not a plain directory",
            dir.display()
        ));
    }
    let path = dir.join(claude_mcp_config_file_name(runtime_id));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .with_context(|| format!("failed to create `{}`", path.display()))?;
    let guard = ClaudeMcpConfigFile { path };
    file.write_all(contents.as_bytes())
        .with_context(|| format!("failed to write `{}`", guard.path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to flush `{}`", guard.path.display()))?;
    Ok(guard)
}

/// Spawns Claude runtime.
fn spawn_claude_runtime(
    state: AppState,
    session_id: String,
    cwd: String,
    model: String,
    approval_mode: ClaudeApprovalMode,
    effort: ClaudeEffortLevel,
    resume_session_id: Option<String>,
    delegation_mcp_config: String,
    engram_mcp: Option<&TermalDelegationMcpStdioConfig>,
    host_mcp_servers_only: bool,
    model_options_tx: Option<Sender<std::result::Result<Vec<SessionModelOption>, String>>>,
) -> Result<ClaudeRuntimeHandle> {
    if !state.agent_runtime_spawning_enabled {
        return Err(anyhow!(
            "agent runtime spawning is disabled for this AppState"
        ));
    }

    let runtime_id = Uuid::new_v4().to_string();
    let cwd = normalize_local_user_facing_path(&cwd);
    // Written before the spawn so a spawn failure drops the guard and removes
    // the file; on success the guard moves into the stdout reader, which
    // releases it on Claude's first valid JSON line and otherwise drops it
    // when the reader ends.
    let mcp_config_file = write_private_claude_mcp_config(
        &claude_mcp_config_dir(&state.persistence_path),
        &runtime_id,
        &delegation_mcp_config,
    )?;
    let executable = resolve_claude_executable()
        .ok_or_else(|| anyhow!("a launchable Claude CLI was not found on PATH"))?;
    let mut command = Command::new(executable);
    command.current_dir(&cwd);
    command.args(claude_cli_persistent_args(
        &model,
        approval_mode,
        effort,
        resume_session_id.as_deref(),
    ));
    command.args(claude_cli_mcp_config_args(
        &mcp_config_file.path,
        host_mcp_servers_only,
    ));
    command.env("CLAUDE_CODE_ENTRYPOINT", "termal");
    let termal_env = termal_agent_process_env(&session_id, &state.local_http_base_url())?;
    apply_agent_process_env(&mut command, Some(&termal_env), engram_mcp)?;

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
    let process = Arc::new(SharedChild::new(child).context("failed to share Claude child")?);

    let (input_tx, input_rx) = mpsc::channel::<ClaudeRuntimeCommand>();

    // Decided once, before initialize advertises it, from the Engram MCP
    // configuration the caller resolved under its state lock (this function
    // runs under that lock and takes none): the reader answers the hook the
    // writer registers, and a later settings change does not add or remove it
    // for this runtime (`claude_compact_hook.rs`).
    let compact_hook = claude_compact_hook_for_runtime(engram_mcp, &runtime_id).map(Arc::new);

    let replay_prompt = Arc::new(Mutex::new(None));
    // Which turn each stdout frame belongs to, shared by the writer (which
    // reserves a prompt's owner before writing it) and the reader.
    let turn_ownership = new_claude_turn_ownership();
    // Reader-side protocol failures can require terminating a still-live
    // child. The waiter owns runtime cleanup, so carry the actionable reason
    // across the process exit instead of first recording one failure and then
    // appending a second generic exit-status failure.
    let runtime_exit_error_override = Arc::new(Mutex::new(None::<String>));

    {
        let writer_session_id = session_id.clone();
        let writer_state = state.clone();
        let writer_runtime_token = RuntimeToken::Claude(runtime_id.clone());
        let writer_replay_prompt = replay_prompt.clone();
        let writer_turn_ownership = turn_ownership.clone();
        let writer_cwd = cwd.clone();
        let writer_compact_hook = compact_hook.clone();
        // The writer owns no sender of the command channel it receives on,
        // so its loop ends once the runtime handle and the reader are gone.
        std::thread::spawn(move || {
            let mut stdin = stdin;
            if let Err(err) = write_claude_initialize(
                &mut stdin,
                &writer_state,
                &writer_session_id,
                writer_compact_hook.as_deref(),
            ) {
                let _ = writer_state.handle_runtime_exit_if_matches(
                    &writer_session_id,
                    &writer_runtime_token,
                    Some(&format!("failed to initialize Claude session: {err:#}")),
                );
                return;
            }

            let writer_context = ClaudeRuntimeContext::new(
                writer_state,
                writer_session_id,
                writer_runtime_token,
                writer_turn_ownership,
                writer_replay_prompt,
                writer_cwd,
            )
            .with_compact_hook(writer_compact_hook);
            run_claude_writer(&writer_context, &mut stdin, input_rx);
        });
    }

    let reader_thread = {
        let reader_session_id = session_id.clone();
        let reader_state = state.clone();
        let reader_input_tx = input_tx.clone();
        let reader_runtime_token = RuntimeToken::Claude(runtime_id.clone());
        let reader_replay_prompt = replay_prompt.clone();
        let reader_turn_ownership = turn_ownership.clone();
        let reader_process = process.clone();
        let reader_runtime_exit_error_override = runtime_exit_error_override.clone();
        // The reviewer child's own working directory, pre-normalized. The read-only
        // permission checker compares `cd` targets against it so a same-folder `cd`
        // (a no-op) does not trip the cd+git exec-sink guard.
        let reader_cwd = cwd.clone();
        let reader_compact_hook = compact_hook.clone();
        // The private MCP configuration file is owned by the stdout reader: it
        // is released on the first valid line Claude prints (startup and
        // configuration parsing are complete by then) and, as the fallback,
        // when this thread ends without ever seeing one.
        let mut reader_mcp_config_file = Some(mcp_config_file);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut raw_line = String::new();
            let context = ClaudeRuntimeContext::new(
                reader_state.clone(),
                reader_session_id.clone(),
                reader_runtime_token.clone(),
                reader_turn_ownership.clone(),
                reader_replay_prompt.clone(),
                reader_cwd.clone(),
            )
            .with_compact_hook(reader_compact_hook);
            let mut frames =
                ClaudeReaderFrames::new(&context, reader_input_tx.clone(), model_options_tx);
            let mut recorder =
                SessionRecorder::new(reader_state.clone(), reader_session_id.clone());

            loop {
                raw_line.clear();
                let bytes_read = match reader.read_line(&mut raw_line) {
                    Ok(bytes_read) => bytes_read,
                    Err(err) => {
                        if let Some(tx) = frames.initialize_model_options_tx.take() {
                            let _ =
                                tx.send(Err(format!("failed to read stdout from Claude: {err}")));
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
                        if let Some(tx) = frames.initialize_model_options_tx.take() {
                            let _ =
                                tx.send(Err(format!("failed to parse Claude JSON line: {err}")));
                        }
                        let _ = reader_state.fail_turn_if_runtime_matches(
                            &reader_session_id,
                            &reader_runtime_token,
                            &format!("failed to parse Claude JSON line: {err}"),
                        );
                        break;
                    }
                };
                release_private_claude_mcp_config(&mut reader_mcp_config_file);

                // Every effect of the frame is applied by the one shared
                // function (`claude_frame_application.rs`); only stopping the
                // process, on its explicit outcome, happens here.
                match apply_claude_frame(&context, &mut frames, &mut recorder, &message).next {
                    ClaudeFrameApplied::Continue => {}
                    ClaudeFrameApplied::Stop => break,
                    ClaudeFrameApplied::TerminateRuntime(detail) => {
                        if let Err(kill_error) = terminate_claude_runtime_after_control_failure(
                            &reader_process,
                            &reader_runtime_exit_error_override,
                            &detail,
                        ) {
                            let _ = reader_state.fail_turn_if_runtime_matches(
                                &reader_session_id,
                                &reader_runtime_token,
                                &format!(
                                    "{detail}; failed to stop the Claude runtime: {kill_error:#}"
                                ),
                            );
                        }
                        break;
                    }
                }
            }

            if let Some(tx) = frames.initialize_model_options_tx.take() {
                let _ = tx.send(Err(
                    "Claude exited before reporting model options".to_owned()
                ));
            }
            let _ = recorder.finish_streaming_text();
        })
    };

    {
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                let timestamp = runtime_stderr_timestamp();
                let prefix = format_runtime_stderr_prefix("claude", &timestamp);
                eprintln!("{prefix} {line}");
            }
        });
    }

    {
        let wait_session_id = session_id.clone();
        let wait_state = state.clone();
        let wait_process = process.clone();
        let wait_runtime_token = RuntimeToken::Claude(runtime_id.clone());
        let wait_runtime_exit_error_override = runtime_exit_error_override.clone();
        std::thread::spawn(move || {
            let wait_result = wait_process.wait();
            let error_override = take_claude_runtime_exit_error_after_reader(
                reader_thread,
                &wait_runtime_exit_error_override,
            );
            match wait_result {
                Ok(status) if status.success() => {
                    let _ = wait_state.handle_runtime_exit_if_matches(
                        &wait_session_id,
                        &wait_runtime_token,
                        error_override.as_deref(),
                    );
                }
                Ok(status) => {
                    let detail = error_override
                        .unwrap_or_else(|| format!("Claude session exited with status {status}"));
                    let _ = wait_state.handle_runtime_exit_if_matches(
                        &wait_session_id,
                        &wait_runtime_token,
                        Some(&detail),
                    );
                }
                Err(err) => {
                    let detail = error_override
                        .unwrap_or_else(|| format!("failed waiting for Claude session: {err}"));
                    let _ = wait_state.handle_runtime_exit_if_matches(
                        &wait_session_id,
                        &wait_runtime_token,
                        Some(&detail),
                    );
                }
            }
        })
    };

    Ok(ClaudeRuntimeHandle {
        runtime_id,
        input_tx,
        process,
    })
}

/// Claude's native stream-json compact boundary means the next prompt needs a
/// fresh base-tier Engram work context. Hook names are not part of this stream
/// contract, so a similarly named field must not arm a refresh by accident.
fn claude_event_marks_engram_context_nudge(message: &Value) -> bool {
    message.get("type").and_then(Value::as_str) == Some("system")
        && message.get("subtype").and_then(Value::as_str) == Some("compact_boundary")
}

/// Claude Code 2.1.288 reports `status: compacting` when a compaction starts,
/// before its SessionStart compact hook and its compact_boundary.
fn claude_event_starts_compaction(message: &Value) -> bool {
    message.get("type").and_then(Value::as_str) == Some("system")
        && message.get("subtype").and_then(Value::as_str) == Some("status")
        && message.get("status").and_then(Value::as_str) == Some("compacting")
}

/// Writes Claude initialize, with the runtime's SessionStart compact hook
/// when it registers one.
fn write_claude_initialize(
    writer: &mut impl Write,
    state: &AppState,
    session_id: &str,
    compact_hook: Option<&ClaudeCompactHook>,
) -> Result<()> {
    let guidance = termal_root_mailbox_guidance(state, session_id).unwrap_or_default();
    write_claude_message(
        writer,
        &json!({
            "request_id": Uuid::new_v4().to_string(),
            "type": "control_request",
            "request": {
                "subtype": "initialize",
                "hooks": claude_initialize_hooks(compact_hook),
                "systemPrompt": "",
                "appendSystemPrompt": guidance,
            }
        }),
    )
}

/// Writes Claude prompt message.
fn write_claude_prompt_message(
    writer: &mut impl Write,
    prompt: &ClaudePromptCommand,
    attempt_uuid: &str,
) -> Result<()> {
    // The attempt's own identity: the replay generation for a prompt's first
    // attempt, a fresh uuid for each retry. A runtime that reports message
    // lifecycles names it back in `command_lifecycle` frames and in the
    // `result` of the attempt's turn (`claude_turn_ownership.rs`).
    write_claude_message(
        writer,
        &json!({
            "type": "user",
            "uuid": attempt_uuid,
            "message": {
                "role": "user",
                "content": claude_prompt_content(prompt),
            }
        }),
    )
}

/// Writes Claude permission response.
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

/// Writes Claude set permission mode.
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

/// Writes Claude set model.
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

/// Writes Claude message.
fn write_claude_message(writer: &mut impl Write, message: &Value) -> Result<()> {
    serde_json::to_writer(&mut *writer, message).context("failed to encode Claude message")?;
    writer
        .write_all(b"\n")
        .context("failed to write Claude message delimiter")?;
    writer.flush().context("failed to flush Claude stdin")
}
