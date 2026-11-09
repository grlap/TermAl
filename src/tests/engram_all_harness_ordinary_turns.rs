//! Ordinary turns on every supported harness, with Engram enabled, through the
//! shared prompt, queue and terminal lifecycle. Owns the harness-by-outcome
//! matrix: an ordinary turn needs no per-turn evaluate, begin, grant or
//! checkpoint, and its end reaches Engram as one host observation of the turn,
//! with its source and check facts and the observed outcome, in the same shape
//! for every harness. Does not own any harness's frame or event parsing, the
//! reporting producer and source capture (parent module), or the removal of
//! per-turn gating itself. New module, not a split.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Harness {
    Codex,
    Claude,
    Cursor,
    Gemini,
    OpenCode,
    Kimi,
}

impl Harness {
    const ALL: [Harness; 6] = [
        Harness::Codex,
        Harness::Claude,
        Harness::Cursor,
        Harness::Gemini,
        Harness::OpenCode,
        Harness::Kimi,
    ];

    fn agent(self) -> Agent {
        match self {
            Harness::Codex => Agent::Codex,
            Harness::Claude => Agent::Claude,
            Harness::Cursor => Agent::Cursor,
            Harness::Gemini => Agent::Gemini,
            Harness::OpenCode => Agent::OpenCode,
            Harness::Kimi => Agent::Kimi,
        }
    }

    fn acp(self) -> Option<AcpAgent> {
        match self {
            Harness::Codex | Harness::Claude => None,
            Harness::Cursor => Some(AcpAgent::Cursor),
            Harness::Gemini => Some(AcpAgent::Gemini),
            Harness::OpenCode => Some(AcpAgent::OpenCode),
            Harness::Kimi => Some(AcpAgent::Kimi),
        }
    }

    /// The observed result of the passing size test as this harness reports
    /// it: Codex and Kimi state exit 0 and Claude reports success, while the
    /// other ACP adapters' plain-text tool output states no exit at all, so
    /// the host honestly cannot call the test passed.
    fn passing_test_result(self) -> &'static str {
        match self {
            Harness::Codex | Harness::Claude | Harness::Kimi => "passed",
            Harness::Cursor | Harness::Gemini | Harness::OpenCode => "indeterminate",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Harness::Codex => "codex",
            Harness::Claude => "claude",
            Harness::Cursor => "cursor",
            Harness::Gemini => "gemini",
            Harness::OpenCode => "opencode",
            Harness::Kimi => "kimi",
        }
    }
}

/// How the ordinary turn ends, through the shared terminal owners.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
}

impl Outcome {
    fn wire(self) -> &'static str {
        match self {
            Outcome::Succeeded => "succeeded",
            Outcome::Failed => "failed",
            Outcome::Cancelled => "cancelled",
            Outcome::Unknown => "unknown",
        }
    }
}

/// Per-turn gating and settlement operations an ordinary turn must not use.
const GATED_OPERATIONS: [&str; 6] = [
    "session_bind",
    "session_status",
    "turn_evaluate",
    "turn_begin",
    "turn_grant_read",
    "turn_checkpoint",
];

/// The channel the harness's runtime receives prompts on.
enum PromptChannel {
    Codex(mpsc::Receiver<CodexRuntimeCommand>),
    Claude(mpsc::Receiver<ClaudeRuntimeCommand>),
    Acp(mpsc::Receiver<AcpRuntimeCommand>),
}

impl PromptChannel {
    /// Waits for the next prompt the runtime is asked to run, answering the
    /// configuration refreshes that may come first. A Claude prompt is
    /// returned, since its frames are owned by the prompt TermAl wrote.
    fn expect_prompt(&self, phase: &str) -> Option<ClaudePromptCommand> {
        match self {
            PromptChannel::Codex(rx) => loop {
                if let CodexRuntimeCommand::Prompt { .. } = receive(rx, phase) {
                    return None;
                }
            },
            PromptChannel::Claude(rx) => loop {
                if let ClaudeRuntimeCommand::Prompt(prompt) = receive(rx, phase) {
                    return Some(prompt);
                }
            },
            PromptChannel::Acp(rx) => loop {
                match receive(rx, phase) {
                    AcpRuntimeCommand::Prompt(_) => return None,
                    AcpRuntimeCommand::RefreshSessionConfig { response_tx, .. } => {
                        let _ = response_tx.send(Ok(()));
                    }
                    _ => {}
                }
            },
        }
    }
}

/// Hands ordinary turn, check and quiet-sighting observations to the
/// reporting producer, and every other request to the scripted protocol
/// fixture. Source changes the host observes
/// between turns (the fixture's own repository setup among them) are not
/// observations of the turn and stay with the protocol fixture.
struct HarnessTransport {
    producer: Arc<ObservationProducer>,
    base: Arc<ScriptedEngramControlTransport>,
}

impl EngramControlTransport for HarnessTransport {
    fn read_observation_policy(
        &self,
        connection: &EngramConnectionConfig,
        store: &EngramAuthorityStoreKey,
        timeout: Duration,
        workdir: &FsPath,
    ) -> Result<EngramObservationPolicyBasis, EngramTransportError> {
        self.producer
            .read_observation_policy(connection, store, timeout, workdir)
    }

    fn read_work_binding(
        &self,
        connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>,
        timeout: Duration,
    ) -> Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.producer
            .read_work_binding(connection, preference, timeout)
    }

    fn read_held_claims(
        &self,
        connection: &EngramConnectionConfig,
        timeout: Duration,
    ) -> Result<EngramHeldClaims, EngramTransportError> {
        self.producer.read_held_claims(connection, timeout)
    }

    fn shutdown_session(&self, session: &str) {
        self.producer.shutdown_session(session);
    }

    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        if matches!(request, EngramControlRequest::WorkSessionRead { .. }) {
            self.base
                .requests
                .lock()
                .unwrap()
                .push(RecordedEngramControlRequest {
                    connection: connection.clone(),
                    request: serde_json::to_value(request).unwrap(),
                });
            return Ok(json!({
                "project_id": fs::read_to_string(&connection.project_file).unwrap().trim(),
                "session_id": connection.session_id,
                "work_session": true,
                "capabilities": ["host_work_v1", "verification_record_v1"],
            }));
        }
        let turn_observation = matches!(request, EngramControlRequest::ExecutionObserve { .. })
            && matches!(
                serde_json::to_value(request).unwrap()["occurrence"]["kind"].as_str(),
                Some("unadmitted_turn" | "observed_check" | "source_sighting")
            );
        if turn_observation {
            self.producer.request(connection, request, timeout)
        } else {
            self.base.request(connection, request, timeout)
        }
    }
}

/// An Engram-enabled root session of one harness, bound to claimed work in a
/// Git repository of its own, with one ordinary prompt delivered to its
/// runtime. Every Engram request goes through `HarnessTransport`.
struct HarnessTurn {
    harness: Harness,
    state: AppState,
    session_id: String,
    root: PathBuf,
    /// The Claude prompt the runtime was last asked to run.
    claude_prompt: Mutex<Option<ClaudePromptCommand>>,
    transport: Arc<ScriptedEngramControlTransport>,
    sent: mpsc::Receiver<SentObservation>,
    prompts: PromptChannel,
    _release: ReleaseResponse,
}

impl HarnessTurn {
    fn start(harness: Harness, label: &str) -> Self {
        Self::start_with_claim(harness, label, true)
    }

    fn start_with_claim(harness: Harness, label: &str, claimed: bool) -> Self {
        use_toolchain_label(Some(FIXTURE_TOOLCHAIN));
        let name = format!("all-harness-{}-{label}", harness.label());
        let (state, codex_rx) = test_app_state_with_delegation_codex_runtime(&name);
        state.select_test_scripted_engram_budget_clock();
        let root = state
            .test_temp_root
            .as_ref()
            .expect("test root should exist")
            .path()
            .join(format!("{name}-project"));
        fs::create_dir_all(&root).expect("project root should exist");
        run_git_test_command(&root, &["init", "--quiet"]);
        run_git_test_command(&root, &["config", "user.email", "termal-tests@example.com"]);
        run_git_test_command(&root, &["config", "user.name", "TermAl tests"]);
        fs::write(root.join("README.md"), "ordinary\n").expect("fixture file should write");
        run_git_test_command(&root, &["add", "README.md"]);
        run_git_test_command(&root, &["commit", "--quiet", "-m", "ordinary"]);
        let project_id = create_test_project(&state, &root, "Engram all-harness turns");
        let workdir = normalize_user_facing_path(&root);
        let session_id =
            create_test_project_session(&state, harness.agent(), &project_id, &workdir);
        enable_test_project_engram(&state, &project_id, &root);
        fs::write(
            root.join(".engram-project"),
            "github.com/example/source-root\n",
        )
        .unwrap();
        let binding = test_control_work_binding(&name, 1);
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&session_id)
                .expect("root should exist");
            inner.sessions[index].engram.context_nudge_pending = false;
            inner.sessions[index].engram.work_binding = claimed.then(|| binding.clone());
            inner
                .projects
                .iter_mut()
                .find(|project| project.id == project_id)
                .expect("project should exist")
                .engram
                .as_mut()
                .expect("Engram should be enabled")
                .authority_store_key = Some(EngramAuthorityStoreKey {
                database_path: root.join("engram.db"),
                project_id: "github.com/example/source-root".to_owned(),
            });
        }
        // No control replies are available: ordinary work reads and the
        // existing canonical root fixture are the only protocol services.
        let transport = ScriptedEngramControlTransport::new([]);
        transport.set_current_work_binding(claimed.then(|| binding.clone()));
        transport.enable_named_roots(&session_id, &binding);
        transport.replace_held_claims([Ok(EngramHeldClaims {
            items: if claimed {
                vec![EngramHeldClaim {
                    work_id: binding.work_id.clone(),
                    short_ref: binding.work_id.clone(),
                    claim_id: binding.claim_id.clone(),
                    claim_fence: binding.claim_fence,
                    focused: true,
                    control_binding: Some(binding.clone()),
                }]
            } else {
                Vec::new()
            },
            omitted: 0,
        })]);
        let (sent, received) = mpsc::channel();
        let producer = Arc::new(ObservationProducer {
            base: transport.clone(),
            binding: RwLock::new(claimed.then_some(binding.clone())),
            path: state.persistence_path.as_ref().clone(),
            sent,
            attempts: Mutex::new(Vec::new()),
            records: Mutex::new(BTreeMap::new()),
            mode: ReplyMode::Recorded,
            response_gate: (Mutex::new(false), Condvar::new()),
            request_hook: Mutex::new(None),
        });
        state.install_control_test_transport(Arc::new(HarnessTransport {
            producer: producer.clone(),
            base: transport.clone(),
        }));
        if claimed {
            // Establish positive fixture authority explicitly before sending;
            // provider handoff performs no canonical read on our behalf.
            state
                .name_engram_source_root(
                    &session_id,
                    EngramSourceRootRequest {
                        work: binding.work_id,
                        path: None,
                    },
                )
                .expect("ordinary public clear acknowledges the fixture's unnamed root");
            assert_eq!(
                state.engram_root_capture(&session_id),
                EngramRootCapture::Unnamed
            );
        }
        install_reporting_source_capture(&state);
        let prompts = match (harness, harness.acp()) {
            (Harness::Codex, _) => PromptChannel::Codex(codex_rx),
            (Harness::Claude, _) => {
                let (runtime, rx) = test_claude_runtime_handle(&format!("{name}-runtime"));
                Self::install_runtime(&state, &session_id, SessionRuntime::Claude(runtime));
                PromptChannel::Claude(rx)
            }
            (_, Some(acp)) => {
                let (runtime, rx) = test_acp_runtime_handle(acp, &format!("{name}-runtime"));
                Self::install_runtime(&state, &session_id, SessionRuntime::Acp(runtime));
                state
                    .set_external_session_id(&session_id, format!("{name}-acp-session"))
                    .expect("the ACP session id should record");
                PromptChannel::Acp(rx)
            }
            (_, None) => unreachable!("every non-Codex, non-Claude harness runs over ACP"),
        };
        let turn = Self {
            harness,
            state,
            session_id,
            root,
            claude_prompt: Mutex::new(None),
            transport,
            sent: received,
            prompts,
            _release: ReleaseResponse(producer),
        };
        turn.send("Ordinary prompt.", true);
        turn
    }

    fn install_runtime(state: &AppState, session_id: &str, runtime: SessionRuntime) {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .expect("root should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        record.runtime = runtime;
        record.session.status = SessionStatus::Idle;
    }

    /// Sends an ordinary prompt; an idle root dispatches it at once, a busy
    /// one queues it.
    fn send(&self, text: &str, expect_dispatch: bool) {
        let result = self
            .state
            .dispatch_turn(
                &self.session_id,
                SendMessageRequest {
                    text: text.to_owned(),
                    expanded_text: None,
                    attachments: Vec::new(),
                    source_session_id: None,
                    source_mailbox: None,
                },
            )
            .expect("an ordinary prompt should be accepted");
        match result {
            DispatchTurnResult::Dispatched(dispatch)
            | DispatchTurnResult::DispatchedAfterQueue(dispatch) => {
                assert!(expect_dispatch, "a busy root should queue the prompt");
                deliver_turn_dispatch(&self.state, dispatch)
                    .expect("the ordinary prompt should be delivered");
                let prompt = self
                    .prompts
                    .expect_prompt("the runtime should receive the ordinary prompt");
                *self.claude_prompt.lock().unwrap() = prompt;
            }
            DispatchTurnResult::Queued => {
                assert!(!expect_dispatch, "an idle root should dispatch the prompt");
            }
        }
    }

    fn record<T>(&self, read: impl FnOnce(&SessionRecord) -> T) -> T {
        let inner = self.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&self.session_id)
            .expect("root should exist");
        read(&inner.sessions[index])
    }

    /// Ends the active turn through the shared terminal owner for `outcome`.
    fn end(&self, outcome: Outcome) {
        let token = self.record(|record| {
            record
                .runtime
                .runtime_token()
                .expect("the ordinary turn should own its runtime")
        });
        match outcome {
            Outcome::Succeeded => self
                .state
                .finish_turn_ok_if_runtime_matches(&self.session_id, &token)
                .expect("the turn should complete"),
            Outcome::Failed => self
                .state
                .fail_turn_if_runtime_matches(
                    &self.session_id,
                    &token,
                    "the provider reported a failure",
                )
                .expect("the turn should fail"),
            Outcome::Cancelled => {
                self.state
                    .stop_session(&self.session_id)
                    .expect("an explicit Stop should be accepted");
            }
            Outcome::Unknown => self
                .state
                .handle_runtime_exit_if_matches(&self.session_id, &token, None)
                .expect("the runtime exit should be handled"),
        }
    }

    /// Every way the ordinary turn departed from the contract, so one run
    /// shows the whole gap for this harness and outcome.
    fn gaps(&self, outcome: Outcome) -> (Vec<String>, Option<Vec<String>>) {
        let (gaps, occurrence) = self.observe(outcome);
        let shape = occurrence
            .as_ref()
            .and_then(Value::as_object)
            .map(|fields| {
                let mut keys: Vec<String> = fields.keys().cloned().collect();
                keys.sort();
                keys
            });
        (gaps, shape)
    }

    /// The gaps `gaps` reports, with the occurrence of the turn's observation
    /// when one reached Engram.
    fn observe(&self, outcome: Outcome) -> (Vec<String>, Option<Value>) {
        let mut gaps = Vec::new();
        let requests = self.transport.requests();
        for operation in GATED_OPERATIONS {
            let count = requests
                .iter()
                .filter(|request| request.request["operation"] == operation)
                .count();
            if count != 0 {
                gaps.push(format!("{operation} was sent {count} time(s)"));
            }
        }
        let next_turn = loop {
            let next = self.sent.recv_timeout(DEADLOCK_GUARD);
            let Ok(observed) = &next else {
                break next;
            };
            if observed.request["occurrence"]["kind"] != "source_sighting" {
                break next;
            }
            assert!(
                observed.durable_before_send,
                "quiet facts use the same durable outbox"
            );
            assert!(
                observed.request["occurrence"]
                    .get("host_turn_ref")
                    .is_none()
            );
            assert_eq!(observed.receipt["accounting"], json!({"kind": "sighting"}));
            assert_eq!(observed.receipt["observed_checks"], json!([]));
            assert_eq!(observed.receipt["opened_obligations"], json!([]));
        };
        let occurrence = match next_turn {
            Ok(observation) => {
                let occurrence = &observation.request["occurrence"];
                if occurrence["kind"] != "unadmitted_turn" {
                    gaps.push(format!(
                        "the observation is a {} occurrence, not the turn",
                        occurrence["kind"]
                    ));
                }
                if occurrence["outcome"] != outcome.wire() {
                    gaps.push(format!(
                        "the turn outcome is {}, not {}",
                        occurrence["outcome"],
                        outcome.wire()
                    ));
                }
                if !occurrence["host_turn_ref"].is_string() {
                    gaps.push("the observation names no host turn".to_owned());
                }
                if !occurrence["observed_checks"].is_array() {
                    gaps.push("the observation carries no check facts".to_owned());
                }
                if occurrence.get("source_change").is_none() {
                    gaps.push("the observation carries no source facts".to_owned());
                }
                Some(occurrence.clone())
            }
            Err(error) => {
                gaps.push(format!(
                    "no observation of the turn reached Engram ({error:?})"
                ));
                None
            }
        };
        (gaps, occurrence)
    }

    /// Runs the size test as the harness's own runtime reports it, through
    /// that harness's event handling, and waits until the host has measured
    /// its start and end.
    fn run_size_test(&self) {
        let mut recorder = SessionRecorder::new(self.state.clone(), self.session_id.clone());
        match (self.harness, self.harness.acp()) {
            (Harness::Codex, _) => {
                let item = |status: &str, code: Option<i64>| {
                    let mut item = json!({
                        "type": "commandExecution", "id": "harness-check",
                        "command": SIZE_TEST, "cwd": self.root, "status": status,
                        "aggregatedOutput": PASSING_OUTPUT
                    });
                    if let Some(code) = code {
                        item["exitCode"] = json!(code);
                    }
                    item
                };
                handle_codex_app_server_item_started(&item("inProgress", None), &mut recorder)
                    .expect("Codex should start the command");
                self.wait_for_measurements();
                handle_codex_app_server_item_completed(
                    &item("completed", Some(0)),
                    &self.state,
                    &self.session_id,
                    &mut CodexTurnState::default(),
                    &mut recorder,
                )
                .expect("Codex should end the command");
            }
            (Harness::Claude, _) => {
                let prompt = self
                    .claude_prompt
                    .lock()
                    .unwrap()
                    .clone()
                    .expect("the Claude runtime received the ordinary prompt");
                let token = self.record(|record| {
                    record
                        .runtime
                        .runtime_token()
                        .expect("the turn owns its runtime")
                });
                let ownership = new_claude_turn_ownership();
                lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&prompt));
                let (input_tx, _input_rx) = mpsc::channel();
                let context = ClaudeRuntimeContext::new(
                    self.state.clone(),
                    self.session_id.clone(),
                    token,
                    ownership,
                    Arc::new(Mutex::new(None)),
                    self.record(|record| record.session.workdir.clone()),
                );
                let mut frames = ClaudeReaderFrames::new(&context, input_tx, None);
                let mut feed = |frame: Value| {
                    let outcome = apply_claude_frame(&context, &mut frames, &mut recorder, &frame);
                    assert_eq!(outcome.next, ClaudeFrameApplied::Continue, "{frame}");
                };
                feed(echo(&prompt));
                feed(json!({"type": "assistant", "message": {"content": [{
                    "type": "tool_use", "id": "harness-check", "name": "Bash",
                    "input": {"command": SIZE_TEST}}]}}));
                self.wait_for_measurements();
                feed(
                    json!({"type": "user", "message": {"role": "user", "content": [{
                    "type": "tool_result", "tool_use_id": "harness-check", "is_error": false,
                    "content": PASSING_OUTPUT}]},
                    "tool_use_result": {"stdout": PASSING_OUTPUT, "stderr": "", "interrupted": false}}),
                );
            }
            (_, Some(acp)) => {
                let (input_tx, _input_rx) = mpsc::channel();
                let mut turn_state = AcpTurnState::default();
                let (token, generation) = self.record(|record| {
                    (
                        record.runtime.runtime_token().unwrap(),
                        record.active_turn_generation,
                    )
                });
                let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
                    observation_turn_generation: Some(generation),
                    ..Default::default()
                }));
                let mut apply = |update: Value| {
                    handle_acp_notification(
                        "session/update",
                        &json!({"params": {"update": update}}),
                        &self.state,
                        &self.session_id,
                        &token,
                        &runtime_state,
                        &input_tx,
                        &mut turn_state,
                        &mut recorder,
                        acp,
                    )
                    .expect("the ACP update should apply");
                };
                if matches!(acp, AcpAgent::Kimi) {
                    // Kimi announces the call before it streams its input.
                    apply(json!({
                        "sessionUpdate": "tool_call", "toolCallId": "harness-check",
                        "title": "Bash", "kind": "execute", "status": "pending",
                        "content": [{"type": "content", "content": {"type": "text", "text": ""}}]
                    }));
                    apply(json!({
                        "sessionUpdate": "tool_call_update", "toolCallId": "harness-check",
                        "title": format!("Running: {SIZE_TEST}"), "kind": "execute",
                        "status": "in_progress", "rawInput": {"command": SIZE_TEST}
                    }));
                } else {
                    apply(json!({
                        "sessionUpdate": "tool_call", "toolCallId": "harness-check",
                        "title": format!("Running: {SIZE_TEST}"), "kind": "execute",
                        "status": "in_progress", "rawInput": {"command": SIZE_TEST}
                    }));
                }
                self.wait_for_measurements();
                apply(json!({
                    "sessionUpdate": "tool_call_update", "toolCallId": "harness-check",
                    "status": "completed",
                    "content": [{"type": "content", "content": {"type": "text", "text": PASSING_OUTPUT}}],
                    "rawOutput": PASSING_OUTPUT
                }));
            }
            (_, None) => unreachable!("every non-Codex, non-Claude harness runs over ACP"),
        }
        self.wait_for_measurements();
    }

    /// Waits until every source measurement the turn's checks started is taken.
    fn wait_for_measurements(&self) {
        let captures = self.record(|record| {
            record
                .engram
                .active_turn_checks
                .iter()
                .flat_map(|check| {
                    std::iter::once(check.start_basis.clone())
                        .chain(check.end.as_ref().map(|end| end.end_basis.clone()))
                })
                .collect::<Vec<_>>()
        });
        for capture in captures {
            capture.wait_until(std::time::Instant::now() + DEADLOCK_GUARD);
        }
    }

    /// Edits a tracked file, as the agent would.
    fn edit(&self) {
        fs::write(self.root.join("README.md"), "edited by the ordinary turn\n")
            .expect("the tracked file should change");
    }
}

const PASSING_OUTPUT: &str = "running 1 test\ntest result: ok. 1 passed; 0 failed";

/// Runs one ordinary turn of `harness` that runs a test and edits a tracked
/// file, through the harness's own event handling, then ends it successfully.
/// The turn's one observation must carry the test and the edit.
fn assert_ordinary_turn_reports_its_execution(harness: Harness) {
    let turn = HarnessTurn::start(harness, "execution");
    turn.run_size_test();
    turn.edit();
    turn.end(Outcome::Succeeded);
    let (mut gaps, occurrence) = turn.observe(Outcome::Succeeded);
    if let Some(occurrence) = occurrence {
        // An observed check carries its kind and result, not the command.
        let checks = occurrence["observed_checks"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        match checks.as_slice() {
            [check] => {
                if check["check_kind"] != "test" {
                    gaps.push(format!("the observed check is not the test: {check}"));
                }
                if check["observed_result"] != harness.passing_test_result() {
                    gaps.push(format!(
                        "the test's observed result is {}, not {}",
                        check["observed_result"],
                        harness.passing_test_result()
                    ));
                }
                if !check["finished_at"].is_string() {
                    gaps.push(format!("the observed test never finished: {check}"));
                }
            }
            _ => gaps.push(format!(
                "the observation should carry exactly the one test it ran: {checks:?}"
            )),
        }
        if occurrence["source_change"].is_null() {
            gaps.push("the observation carries no fact of the edit it made".to_owned());
        }
    }
    assert!(
        gaps.is_empty(),
        "{harness:?} ordinary turn that ran a test and edited a file: {}",
        gaps.join("; ")
    );
}

macro_rules! execution_tests {
    ($($name:ident: $harness:ident;)*) => {
        $(
            #[test]
            fn $name() {
                assert_ordinary_turn_reports_its_execution(Harness::$harness);
            }
        )*
    };
}

execution_tests! {
    codex_ordinary_turn_reports_its_test_and_edit: Codex;
    claude_ordinary_turn_reports_its_test_and_edit: Claude;
    cursor_ordinary_turn_reports_its_test_and_edit: Cursor;
    gemini_ordinary_turn_reports_its_test_and_edit: Gemini;
    opencode_ordinary_turn_reports_its_test_and_edit: OpenCode;
    kimi_ordinary_turn_reports_its_test_and_edit: Kimi;
}

/// Runs one ordinary turn of `harness` to `outcome`. A succeeding turn also
/// carries a queued ordinary prompt, which must dispatch when it ends.
fn ordinary_turn(harness: Harness, outcome: Outcome) -> (Vec<String>, Option<Vec<String>>) {
    let label = outcome.wire();
    let turn = HarnessTurn::start(harness, label);
    if outcome == Outcome::Succeeded {
        turn.send("Queued ordinary prompt.", false);
    }
    turn.end(outcome);
    let (mut gaps, shape) = turn.gaps(outcome);
    if outcome == Outcome::Succeeded {
        let queued = turn.record(|record| record.session.pending_prompts.len());
        if queued != 0 {
            gaps.push(format!("{queued} queued prompt(s) did not dispatch"));
        } else {
            turn.prompts
                .expect_prompt("the runtime should receive the queued prompt");
        }
    }
    assert_eq!(turn.harness, harness);
    (gaps, shape)
}

fn assert_ordinary_turn(harness: Harness, outcome: Outcome) {
    let (gaps, _) = ordinary_turn(harness, outcome);
    assert!(
        gaps.is_empty(),
        "{harness:?} ordinary turn ending {}: {}",
        outcome.wire(),
        gaps.join("; ")
    );
}

macro_rules! ordinary_turn_tests {
    ($($name:ident: $harness:ident, $outcome:ident;)*) => {
        $(
            #[test]
            fn $name() {
                assert_ordinary_turn(Harness::$harness, Outcome::$outcome);
            }
        )*
    };
}

ordinary_turn_tests! {
    codex_ordinary_turn_succeeds_without_gating: Codex, Succeeded;
    codex_ordinary_turn_fails_without_gating: Codex, Failed;
    codex_ordinary_turn_is_cancelled_without_gating: Codex, Cancelled;
    codex_ordinary_turn_ends_unknown_without_gating: Codex, Unknown;
    claude_ordinary_turn_succeeds_without_gating: Claude, Succeeded;
    claude_ordinary_turn_fails_without_gating: Claude, Failed;
    claude_ordinary_turn_is_cancelled_without_gating: Claude, Cancelled;
    claude_ordinary_turn_ends_unknown_without_gating: Claude, Unknown;
    cursor_ordinary_turn_succeeds_without_gating: Cursor, Succeeded;
    cursor_ordinary_turn_fails_without_gating: Cursor, Failed;
    cursor_ordinary_turn_is_cancelled_without_gating: Cursor, Cancelled;
    cursor_ordinary_turn_ends_unknown_without_gating: Cursor, Unknown;
    gemini_ordinary_turn_succeeds_without_gating: Gemini, Succeeded;
    gemini_ordinary_turn_fails_without_gating: Gemini, Failed;
    gemini_ordinary_turn_is_cancelled_without_gating: Gemini, Cancelled;
    gemini_ordinary_turn_ends_unknown_without_gating: Gemini, Unknown;
    opencode_ordinary_turn_succeeds_without_gating: OpenCode, Succeeded;
    opencode_ordinary_turn_fails_without_gating: OpenCode, Failed;
    opencode_ordinary_turn_is_cancelled_without_gating: OpenCode, Cancelled;
    opencode_ordinary_turn_ends_unknown_without_gating: OpenCode, Unknown;
    kimi_ordinary_turn_succeeds_without_gating: Kimi, Succeeded;
    kimi_ordinary_turn_fails_without_gating: Kimi, Failed;
    kimi_ordinary_turn_is_cancelled_without_gating: Kimi, Cancelled;
    kimi_ordinary_turn_ends_unknown_without_gating: Kimi, Unknown;
}

/// One shared observation shape for every harness: no harness reaches Engram
/// through a pipeline or payload of its own.
#[test]
fn every_harness_reports_its_ordinary_turn_in_one_shared_shape() {
    let mut gaps = Vec::new();
    let mut shapes: Vec<(Harness, Vec<String>)> = Vec::new();
    for harness in Harness::ALL {
        let (harness_gaps, shape) = ordinary_turn(harness, Outcome::Succeeded);
        gaps.extend(
            harness_gaps
                .into_iter()
                .map(|gap| format!("{harness:?}: {gap}")),
        );
        match shape {
            Some(shape) => shapes.push((harness, shape)),
            None => gaps.push(format!("{harness:?}: no observation shape to compare")),
        }
    }
    if let Some((first, expected)) = shapes.first().cloned() {
        for (harness, shape) in &shapes {
            if shape != &expected {
                gaps.push(format!(
                    "{harness:?} reports fields {shape:?}, unlike {first:?}'s {expected:?}"
                ));
            }
        }
    }
    assert!(gaps.is_empty(), "{}", gaps.join("; "));
}

#[test]
fn unclaimed_ordinary_chats_settle_without_retry_custody_or_transcript_errors() {
    for harness in Harness::ALL {
        let turn = HarnessTurn::start_with_claim(harness, "unclaimed", false);
        for attempt in 0..3 {
            if attempt != 0 {
                turn.send("Another ordinary chat.", true);
            }
            turn.end(Outcome::Succeeded);
            let guard = phase_sync::PollGuard::new();
            loop {
                let settled = turn.record(|record| {
                    record.engram.unadmitted_active_turn.is_none()
                        && record.engram.unadmitted_closing_jobs.is_empty()
                        && !record.engram.unadmitted_worker_running
                });
                if settled {
                    break;
                }
                guard.wait(format_args!(
                    "{harness:?} unclaimed chat must settle quietly"
                ));
            }
            turn.record(|record| {
                assert!(!runtime_observation_has_removal_duty(record));
                assert!(record.engram.unadmitted_reports.is_empty());
                assert!(
                    !record
                        .session
                        .messages
                        .iter()
                        .any(|message| matches!(message,
                    Message::Text { author: Author::System, text, .. }
                        if text.contains("not reported to Engram")))
                );
            });
            assert!(
                turn.sent.try_recv().is_err(),
                "unclaimed facts have no work recipient"
            );
        }
    }
}

#[test]
fn replaced_acp_readers_and_old_turns_cannot_write_successor_observations() {
    for harness in [
        Harness::Cursor,
        Harness::Gemini,
        Harness::OpenCode,
        Harness::Kimi,
    ] {
        let turn = HarnessTurn::start(harness, "stale-reader");
        let acp = harness.acp().unwrap();
        let (old_token, old_generation) = turn.record(|record| {
            (
                record.runtime.runtime_token().unwrap(),
                record.active_turn_generation,
            )
        });
        let (runtime, _runtime_input) = test_acp_runtime_handle(acp, "successor-runtime");
        let new_token = {
            let mut inner = turn.state.inner.lock().unwrap();
            let index = inner.find_session_index(&turn.session_id).unwrap();
            let record = &mut inner.sessions[index];
            record.runtime = SessionRuntime::Acp(runtime);
            record.active_turn_generation += 1;
            record.engram.unadmitted_active_turn = Some(EngramRuntimeObservedTurn::started(
                "successor-prompt".into(),
                record.active_turn_generation,
            ));
            record.runtime.runtime_token().unwrap()
        };
        let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
            observation_turn_generation: Some(old_generation),
            ..Default::default()
        }));
        let (input_tx, _input_rx) = mpsc::channel();
        let mut recorder = SessionRecorder::new(turn.state.clone(), turn.session_id.clone());
        let mut turn_state = AcpTurnState::default();
        for token in [&old_token, &new_token] {
            handle_acp_notification(
                "session/update",
                &json!({"params": {"update": {
                    "sessionUpdate": "tool_call", "toolCallId": "stale-command",
                    "title": format!("Running: {SIZE_TEST}"), "status": "in_progress",
                    "kind": "execute", "rawInput": {"command": SIZE_TEST}
                }}}),
                &turn.state,
                &turn.session_id,
                token,
                &runtime_state,
                &input_tx,
                &mut turn_state,
                &mut recorder,
                acp,
            )
            .unwrap();
            turn.record(|record| {
                assert!(
                    record
                        .engram
                        .unadmitted_active_turn
                        .as_ref()
                        .unwrap()
                        .commands
                        .is_empty(),
                    "stale runtime or generation: {harness:?}"
                )
            });
        }
        runtime_state.lock().unwrap().observation_turn_generation = Some(old_generation + 1);
        handle_acp_notification(
            "session/update",
            &json!({"params": {"update": {
                "sessionUpdate": "tool_call", "toolCallId": "current-command",
                "title": format!("Running: {SIZE_TEST}"), "status": "in_progress",
                "kind": "execute", "rawInput": {"command": SIZE_TEST}
            }}}),
            &turn.state,
            &turn.session_id,
            &new_token,
            &runtime_state,
            &input_tx,
            &mut turn_state,
            &mut recorder,
            acp,
        )
        .unwrap();
        turn.record(|record| {
            let trace = record.engram.unadmitted_active_turn.as_ref().unwrap();
            assert_eq!(trace.notice_message_id, "successor-prompt");
            assert_eq!(
                trace.commands.len(),
                1,
                "{harness:?} current event is still observed"
            );
            assert_eq!(trace.commands[0].tool_key, "current-command");
        });
    }
}
