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

    fn push_subagent_result(
        &mut self,
        title: &str,
        summary: &str,
        _conversation_id: Option<&str>,
        _turn_id: Option<&str>,
    ) -> Result<()> {
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
        shared_codex_runtime: Arc::new(Mutex::new(None)),
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

fn test_codex_runtime_handle(runtime_id: &str) -> CodexRuntimeHandle {
    let child = test_exit_success_child();
    let (input_tx, _input_rx) = mpsc::channel();

    CodexRuntimeHandle {
        runtime_id: runtime_id.to_owned(),
        input_tx,
        process: Arc::new(Mutex::new(child)),
        shared_session: None,
    }
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
            process: Arc::new(Mutex::new(child)),
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
            process: Arc::new(Mutex::new(child)),
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
    Arc<Mutex<Child>>,
) {
    let child = test_exit_success_child();
    let process = Arc::new(Mutex::new(child));
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
                name: "fix-bug".to_owned(),
                description: "Fix a bug from docs/bugs.md by number.".to_owned(),
                content: "
Fix a bug from docs/bugs.md by number.

$ARGUMENTS
"
                .to_owned(),
                source: ".claude/commands/fix-bug.md".to_owned(),
            },
            AgentCommand {
                name: "review-local".to_owned(),
                description: "Review local changes.".to_owned(),
                content: "Review local changes.

## Step 1
Inspect diffs.
"
                .to_owned(),
                source: ".claude/commands/review-local.md".to_owned(),
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

    fs::remove_dir_all(root).unwrap();
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
    fs::write(root.join("AGENTS.md"), "See docs/backend.md for service rules.\n").unwrap();
    fs::write(root.join("CLAUDE.md"), "Use docs/backend.md for implementation guidance.\n").unwrap();
    fs::write(
        docs_dir.join("backend.md"),
        "# Backend\n\nPrefer dependency injection when module boundaries shift.\n",
    )
    .unwrap();

    let response = search_instruction_phrase(&root, "dependency injection").unwrap();

    assert_eq!(response.matches.len(), 1);
    let matched = &response.matches[0];
    assert!(matched.path.ends_with("docs\\backend.md") || matched.path.ends_with("docs/backend.md"));
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
    assert!(matched
        .root_paths
        .iter()
        .all(|root_path| root_path.steps.len() == 1 && root_path.steps[0].to_path == matched.path));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn instruction_search_expands_directory_discovery_edges() {
    let root =
        std::env::temp_dir().join(format!("termal-instruction-directory-search-{}", Uuid::new_v4()));
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
    assert!(matched.path.ends_with(".claude\\reviewers\\rust.md") || matched.path.ends_with(".claude/reviewers/rust.md"));
    assert_eq!(matched.root_paths.len(), 1);
    let root_path = &matched.root_paths[0];
    assert_eq!(
        root_path.root_path,
        normalize_path_best_effort(&commands_dir.join("review-local.md"))
            .to_string_lossy()
            .into_owned()
    );
    assert_eq!(root_path.steps.len(), 1);
    assert_eq!(root_path.steps[0].relation, InstructionRelation::DirectoryDiscovery);
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
    let root =
        std::env::temp_dir().join(format!("termal-instruction-generic-docs-{}", Uuid::new_v4()));
    let docs_dir = root.join("docs");
    let features_dir = docs_dir.join("features");
    fs::create_dir_all(&features_dir).unwrap();
    fs::write(root.join("AGENTS.md"), "See README.md for additional context.\n").unwrap();
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
    assert!(matched.path.ends_with("docs\\instructions\\shared.md")
        || matched.path.ends_with("docs/instructions/shared.md"));
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
    let root =
        std::env::temp_dir().join(format!("termal-instruction-realtime-search-{}", Uuid::new_v4()));
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
    fs::write(
        internal_skill_dir.join("SKILL.md"),
        "- README.md\n",
    )
    .unwrap();

    let response = search_instruction_phrase(&root, "real-time handling").unwrap();

    assert_eq!(response.matches.len(), 1);
    let matched = &response.matches[0];
    assert!(matched.path.ends_with(".claude\\reviewers\\react-typescript.md")
        || matched
            .path
            .ends_with(".claude/reviewers/react-typescript.md"));
    assert_eq!(matched.root_paths.len(), 1);
    let root_path = &matched.root_paths[0];
    assert_eq!(
        root_path.root_path,
        normalize_path_best_effort(&commands_dir.join("review-local.md"))
            .to_string_lossy()
            .into_owned()
    );
    assert_eq!(root_path.steps.len(), 1);
    assert_eq!(root_path.steps[0].relation, InstructionRelation::DirectoryDiscovery);
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
    let error = state.search_instructions("missing-session", "dependency injection").unwrap_err();

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
fn persists_app_settings_and_applies_them_to_new_sessions() {
    let state = test_app_state();

    let updated = state
        .update_app_settings(UpdateAppSettingsRequest {
            default_codex_reasoning_effort: Some(CodexReasoningEffort::High),
            default_claude_effort: Some(ClaudeEffortLevel::Max),
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

    let reloaded_state = AppState {
        default_workdir: "/tmp".to_owned(),
        persistence_path: state.persistence_path.clone(),
        state_events: broadcast::channel(16).0,
        delta_events: broadcast::channel(16).0,
        shared_codex_runtime: Arc::new(Mutex::new(None)),
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
        process: Arc::new(Mutex::new(child)),
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
fn shared_codex_task_complete_event_records_subagent_result() {
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
    let message = json!({
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
fn subagent_results_insert_before_trailing_assistant_text() {
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
        Some(Message::SubagentResult { .. })
    ));
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Final answer"
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
fn dispatches_saved_queued_prompts_before_new_prompt_after_recovery() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);

    let child = test_exit_success_child();
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "recovered-queue-dispatch".to_owned(),
        input_tx,
        process: Arc::new(Mutex::new(child)),
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
