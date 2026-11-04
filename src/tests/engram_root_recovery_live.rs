//! Operator-run root-session recovery checks against a real Engram binary.
//!
//! Every test creates a disposable Engram home and SQLite store. Provider
//! delivery remains simulated in-process; only the Engram JSON-lines control
//! path is real. Faults are injected after Engram has produced a reply, at the
//! TermAl transport boundary, so a discarded reply represents an uncertain
//! committed operation rather than a synthetic pre-write failure.

use super::*;

const LIVE_RESPONSE_DELAY: Duration = Duration::from_millis(350);

#[derive(Clone, Copy, Debug)]
enum BoundaryFault {
    DelayAfterReply {
        operation: &'static str,
        delay: Duration,
    },
    DropAfterReply {
        operation: &'static str,
    },
    /// The request never reaches Engram.
    LoseBeforeSend {
        operation: &'static str,
    },
}

impl BoundaryFault {
    fn operation(self) -> &'static str {
        match self {
            Self::DelayAfterReply { operation, .. }
            | Self::DropAfterReply { operation }
            | Self::LoseBeforeSend { operation } => operation,
        }
    }
}

#[derive(Clone, Debug)]
struct BoundaryObservation {
    request: Value,
    timeout: Duration,
    elapsed: Duration,
    disposition: &'static str,
}

struct BoundaryFaultTransport {
    inner: ProcessEngramControlTransport,
    faults: Mutex<VecDeque<BoundaryFault>>,
    observations: Mutex<Vec<BoundaryObservation>>,
    root_reads: Mutex<Vec<EngramNamedRootReadResponse>>,
    request_entered: Mutex<Option<(String, mpsc::Sender<()>)>>,
    reply_pause: Mutex<Option<(String, mpsc::Sender<()>, mpsc::Receiver<()>)>>,
}

impl BoundaryFaultTransport {
    fn new(host_workdir: PathBuf, faults: impl IntoIterator<Item = BoundaryFault>) -> Arc<Self> {
        Arc::new(Self {
            inner: ProcessEngramControlTransport::new(host_workdir),
            faults: Mutex::new(faults.into_iter().collect()),
            observations: Mutex::new(Vec::new()),
            root_reads: Mutex::new(Vec::new()),
            request_entered: Mutex::new(None),
            reply_pause: Mutex::new(None),
        })
    }

    fn matching_fault(&self, operation: &str) -> Option<BoundaryFault> {
        let mut faults = self
            .faults
            .lock()
            .expect("live boundary fault mutex poisoned");
        if faults
            .front()
            .is_some_and(|fault| fault.operation() == operation)
        {
            faults.pop_front()
        } else {
            None
        }
    }

    fn observations(&self) -> Vec<BoundaryObservation> {
        self.observations
            .lock()
            .expect("live boundary observations mutex poisoned")
            .clone()
    }

    fn requests_for(&self, operation: &str) -> Vec<Value> {
        self.observations()
            .into_iter()
            .filter(|observation| observation.request["operation"] == operation)
            .map(|observation| observation.request)
            .collect()
    }
}

impl EngramControlTransport for BoundaryFaultTransport {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        let request_value = serde_json::to_value(request)
            .map_err(|error| EngramTransportError::protocol(error.to_string()))?;
        let operation = request_value["operation"]
            .as_str()
            .expect("Engram request operation should serialize")
            .to_owned();
        let started_at = std::time::Instant::now();
        {
            let mut entered = self.request_entered.lock().unwrap();
            if entered.as_ref().is_some_and(|(expected, _)| expected == &operation) {
                entered.take().unwrap().1.send(()).unwrap();
            }
        }
        {
            let mut faults = self.faults.lock().expect("live boundary fault mutex poisoned");
            if faults.front().is_some_and(|fault| matches!(fault, BoundaryFault::LoseBeforeSend { .. })
                && fault.operation() == operation)
            {
                faults.pop_front();
                return Err(EngramTransportError::transport(format!(
                    "live test lost {operation} before sending it"
                )));
            }
        }
        let result = self.inner.request(connection, request, timeout);
        if operation == "named_root_read"
            && let Ok(reply) = &result
        {
            self.root_reads
                .lock()
                .unwrap()
                .push(parse_engram_result(reply.clone()).expect("real canonical reader response"));
        }
        eprintln!(
            "live boundary operation={operation} elapsed={:?} outcome={}",
            started_at.elapsed(),
            result
                .as_ref()
                .map(|reply| reply
                    .get("decision")
                    .and_then(Value::as_str)
                    .unwrap_or("ok"))
                .unwrap_or("error")
        );
        let pause = {
            let mut pause = self.reply_pause.lock().unwrap();
            if result.is_ok() && pause.as_ref().is_some_and(|(expected, _, _)| expected == &operation) {
                pause.take()
            } else { None }
        };
        if let Some((_, entered, release)) = pause {
            entered.send(()).unwrap();
            release.recv_timeout(DEADLOCK_GUARD).expect("the witness releases the actual producer reply");
        }
        let fault = result
            .as_ref()
            .ok()
            .and_then(|_| self.matching_fault(&operation));

        let disposition = match fault {
            Some(BoundaryFault::DelayAfterReply { delay, .. }) => {
                let remaining = timeout.saturating_sub(started_at.elapsed());
                if delay >= remaining {
                    std::thread::sleep(remaining);
                    self.inner.shutdown_session(&connection.session_id);
                    self.observations
                        .lock()
                        .expect("live boundary observations mutex poisoned")
                        .push(BoundaryObservation {
                            request: request_value,
                            timeout,
                            elapsed: started_at.elapsed(),
                            disposition: "deadline_after_reply",
                        });
                    return Err(EngramTransportError::deadline(format!(
                        "live test delayed {operation} reply through its TermAl deadline"
                    )));
                }
                std::thread::sleep(delay);
                "delayed_after_reply"
            }
            Some(BoundaryFault::DropAfterReply { .. }) => {
                self.inner.shutdown_session(&connection.session_id);
                self.observations
                    .lock()
                    .expect("live boundary observations mutex poisoned")
                    .push(BoundaryObservation {
                        request: request_value,
                        timeout,
                        elapsed: started_at.elapsed(),
                        disposition: "dropped_after_reply",
                    });
                return Err(EngramTransportError::transport(format!(
                    "live test discarded committed {operation} reply"
                )));
            }
            Some(BoundaryFault::LoseBeforeSend { .. }) => {
                unreachable!("a lost send is taken before the request")
            }
            None => "returned",
        };

        self.observations
            .lock()
            .expect("live boundary observations mutex poisoned")
            .push(BoundaryObservation {
                request: request_value,
                timeout,
                elapsed: started_at.elapsed(),
                disposition,
            });
        result
    }

    fn shutdown_session(&self, session_id: &str) {
        self.inner.shutdown_session(session_id);
    }

    fn shutdown_session_until(
        &self,
        session_id: &str,
        clock: &EngramBudgetClock,
        deadline: std::time::Instant,
    ) -> Result<(), EngramTransportError> {
        self.inner.shutdown_session_until(session_id, clock, deadline)
    }

    fn read_work_binding(
        &self,
        connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>,
        timeout: Duration,
    ) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.inner
            .read_work_binding(connection, preference, timeout)
    }

    fn connection_generation(&self, session_id: &str) -> Option<u64> {
        self.inner.connection_generation(session_id)
    }

    fn read_held_claims(
        &self,
        connection: &EngramConnectionConfig,
        timeout: Duration,
    ) -> std::result::Result<EngramHeldClaims, EngramTransportError> {
        self.inner.read_held_claims(connection, timeout)
    }
}

struct LiveRootFixture {
    cleanup: LiveRootCleanup,
    state: AppState,
    receiver: mpsc::Receiver<CodexRuntimeCommand>,
    session_id: String,
    transport: Arc<BoundaryFaultTransport>,
}

struct LiveRootCleanup {
    transport: Arc<BoundaryFaultTransport>,
    session_id: String,
}

impl Drop for LiveRootCleanup {
    fn drop(&mut self) {
        self.transport.shutdown_session(&self.session_id);
    }
}

fn live_engram_binary() -> PathBuf {
    let path = std::env::var_os("TERMAL_TEST_LIVE_ENGRAM_BINARY")
        .map(PathBuf::from)
        .expect("TERMAL_TEST_LIVE_ENGRAM_BINARY must name the reviewed Engram binary");
    assert!(
        path.is_absolute() && path.is_file(),
        "live Engram binary must be an absolute existing file: {}",
        path.display()
    );

    let expected = std::env::var("TERMAL_TEST_LIVE_ENGRAM_SHA256")
        .expect("TERMAL_TEST_LIVE_ENGRAM_SHA256 must identify the reviewed binary")
        .to_ascii_lowercase();
    assert_eq!(
        expected.len(),
        64,
        "expected SHA-256 must have 64 hex digits"
    );
    assert!(
        expected.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "expected SHA-256 must contain only hex digits"
    );
    let actual = sha256_hex(&fs::read(&path).expect("live Engram binary should be readable"));
    assert_eq!(actual, expected, "live Engram binary fingerprint changed");

    let version = Command::new(&path)
        .arg("--version")
        .output()
        .expect("live Engram version command should launch");
    assert!(
        version.status.success(),
        "live Engram version command failed: {}",
        String::from_utf8_lossy(&version.stderr)
    );
    eprintln!(
        "live Engram input path={} sha256={} version={}",
        path.display(),
        actual,
        String::from_utf8_lossy(&version.stdout).trim()
    );
    path
}

fn live_root_fixture(
    suffix: &str,
    faults: impl IntoIterator<Item = BoundaryFault>,
) -> LiveRootFixture {
    let binary_path = live_engram_binary();
    let (state, receiver) =
        test_app_state_with_delegation_codex_runtime(&format!("engram-live-root-{suffix}"));
    let base = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path();
    let root = base.join(format!("live-root-project-{suffix}"));
    let home = base.join(format!("live-root-home-{suffix}"));
    fs::create_dir_all(&root).expect("live project root should exist");
    fs::create_dir_all(&home).expect("live Engram home should exist");
    let project_file = root.join(".engram-project");
    fs::write(&project_file, format!("termal-live-root-{suffix}\n"))
        .expect("live Engram project identity should write");

    let init = Command::new(&binary_path)
        .args([
            "--project-file",
            project_file
                .to_str()
                .expect("temporary project path should be Unicode"),
            "--home",
            home.to_str()
                .expect("temporary home path should be Unicode"),
            "init",
        ])
        .output()
        .expect("real Engram init should launch");
    assert!(
        init.status.success(),
        "real Engram init failed: stdout={} stderr={}",
        String::from_utf8_lossy(&init.stdout),
        String::from_utf8_lossy(&init.stderr)
    );

    let project_id = create_test_project(&state, &root, "Live Engram root recovery");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
            .expect("project should exist")
            .engram = Some(EngramProjectSettings {
            acceptance_evaluation: None,
            enabled: true,
            execution_observability: true,
            binary_path: Some(binary_path.to_string_lossy().into_owned()),
            home: Some(home.to_string_lossy().into_owned()),
            authority_store_key: None,
            deadline_ms: None,
        });
        inner.engram_declared_project_ids.insert(project_id.clone());
        inner
            .engram_declaration_checked_project_ids
            .insert(project_id);
        let index = inner
            .find_session_index(&session_id)
            .expect("root session should exist");
        inner.sessions[index].engram.context_nudge_pending = false;
        state
            .commit_locked(&mut inner)
            .expect("live root settings should persist");
    }

    let transport = BoundaryFaultTransport::new(state.engram_host_launch_workdir(), faults);
    state.install_test_engram_transport(transport.clone());
    LiveRootFixture {
        cleanup: LiveRootCleanup {
            transport: transport.clone(),
            session_id: session_id.clone(),
        },
        state,
        receiver,
        session_id,
        transport,
    }
}

fn dispatch_live_root(
    state: &AppState,
    session_id: &str,
    prompt: &str,
    queued_source: Option<QueuedPromptSource>,
) -> TurnDispatch {
    if let Some(source) = queued_source {
        queue_test_engram_prompt(state, session_id, prompt, source, None);
        return state
            .start_next_queued_turn_off_lock(session_id, false, false)
            .expect("queued live root admission should not fail locally")
            .expect("queued live root should reach admission")
            .dispatch;
    }

    match state
        .dispatch_turn(
            session_id,
            SendMessageRequest {
                text: prompt.to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .expect("direct live root should reach admission")
    {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("idle live root should dispatch immediately"),
    }
}

fn assert_one_provider_prompt(
    receiver: &mpsc::Receiver<CodexRuntimeCommand>,
    session_id: &str,
    prompt: &str,
) {
    match receiver
        .try_recv()
        .expect("one admitted prompt should reach the simulated provider")
    {
        CodexRuntimeCommand::Prompt {
            session_id: delivered_session,
            command,
        } => {
            assert_eq!(delivered_session, session_id);
            assert_eq!(command.prompt, prompt, "provider prompt bytes changed");
        }
        _ => panic!("live root should produce a provider prompt command"),
    }
    assert!(
        receiver.try_recv().is_err(),
        "an admitted operation must reach the provider exactly once"
    );
}

fn finish_live_root(state: &AppState, session_id: &str) {
    let runtime_token = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner.sessions[inner
            .find_session_index(session_id)
            .expect("live root should exist")]
        .runtime
        .runtime_token()
        .expect("admitted live root should own a runtime token")
    };
    state
        .finish_turn_ok_if_runtime_matches(session_id, &runtime_token)
        .expect("ordinary runtime completion succeeds");
}

fn take_live_provider_prompts(
    receiver: &mpsc::Receiver<CodexRuntimeCommand>,
    session_id: &str,
) -> Vec<CodexPromptCommand> {
    receiver
        .try_iter()
        .filter_map(|command| match command {
            CodexRuntimeCommand::Prompt {
                session_id: delivered_session,
                command,
            } => {
                assert_eq!(delivered_session, session_id);
                Some(command)
            }
            // A fresh runtime can receive initialization/control commands.
            // Only the typed Prompt command is provider handoff evidence.
            _ => None,
        })
        .collect()
}

fn restart_live_root_fixture(fixture: LiveRootFixture) -> LiveRootFixture {
    let root_guard = fixture.state.test_temp_root.as_ref().unwrap().clone();
    let default_workdir = {
        let inner = fixture.state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()]
            .session
            .workdir
            .clone()
    };
    let persistence_path = fixture.state.persistence_path.as_path().to_path_buf();
    let templates_path = fixture.state.orchestrator_templates_path.as_path().to_path_buf();
    let session_id = fixture.session_id;
    fixture.state.shutdown_persist_blocking();
    drop(fixture.cleanup);
    drop(fixture.receiver);
    drop(fixture.state);
    drop(fixture.transport);

    let transport = BoundaryFaultTransport::new(engram_host_workdir(&persistence_path).unwrap(), []);
    let mut state = AppState::new_with_paths_and_engram_transport_for_test(
        default_workdir,
        persistence_path,
        templates_path,
        transport.clone(),
    )
    .expect("fresh host must load the actual persisted live cut");
    // Keep only the disposable directory alive; no old runtime or host state
    // is installed in the restarted AppState.
    state.test_temp_root = Some(root_guard);
    let (runtime, receiver, _process) = test_shared_codex_runtime("engram-live-root-restart-provider");
    install_single_wire_codex_fixture(&state, Some(runtime));
    LiveRootFixture {
        cleanup: LiveRootCleanup {
            transport: transport.clone(),
            session_id: session_id.clone(),
        },
        state,
        receiver,
        session_id,
        transport,
    }
}


fn assert_no_live_turn_gate(transport: &BoundaryFaultTransport) {
    for operation in ["session_bind", "session_status", "turn_evaluate", "turn_begin", "turn_checkpoint"] {
        assert!(transport.requests_for(operation).is_empty(), "{operation} is retired");
    }
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn ordinary_live_prompt_does_not_wait_for_a_work_service_deadline() {
    for source in [None, Some(QueuedPromptSource::Orchestrator), Some(QueuedPromptSource::Mailbox)] {
        let fixture = live_root_fixture("ordinary-service-deadline", [
            BoundaryFault::DelayAfterReply { operation: "work_session_read", delay: DEADLOCK_GUARD },
        ]);
        let prompt = "Preserve the ordinary prompt while optional work reporting is unavailable.";
        let dispatch = dispatch_live_root(&fixture.state, &fixture.session_id, prompt, source);
        deliver_turn_dispatch(&fixture.state, dispatch).unwrap();
        assert_one_provider_prompt(&fixture.receiver, &fixture.session_id, prompt);
        assert!(fixture.transport.observations().is_empty(), "handoff makes no work RPC");
        if source == Some(QueuedPromptSource::Orchestrator) {
            queue_test_engram_prompt(&fixture.state, &fixture.session_id,
                "A separate queued orchestrator intent remains cancelable.", QueuedPromptSource::Orchestrator, None);
            let id = {
                let inner = fixture.state.inner.lock().unwrap();
                inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()]
                    .queued_prompts.back().unwrap().pending_prompt.id.clone()
            };
            fixture.state.cancel_queued_prompt(&fixture.session_id, &id).unwrap();
            assert!(fixture.receiver.try_recv().is_err(), "Cancel never sends a second prompt");
        }
        assert_no_live_turn_gate(&fixture.transport);
    }
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn ordinary_live_policy_is_not_a_provider_admission_authority() {
    let fixture = named_root_completion_live::completion_fixture();
    fixture.command(&fixture.live.session_id, &[
        "control-policy", "set-required-assurance", "action_gated", "--authorized-by",
        "termal-live-test", "--idempotency-key", "ordinary-policy",
    ], false);
    let prompt = "An ordinary prompt needs no tracker grant.";
    let dispatch = dispatch_live_root(&fixture.live.state, &fixture.live.session_id, prompt, None);
    deliver_turn_dispatch(&fixture.live.state, dispatch).unwrap();
    assert_one_provider_prompt(&fixture.live.receiver, &fixture.live.session_id, prompt);
    assert_no_live_turn_gate(&fixture.live.transport);
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn ordinary_live_writer_contention_and_retired_sidecar_do_not_block_handoff() {
    let fixture = named_root_completion_live::completion_fixture();
    let session = &fixture.live.session_id;
    let added = fixture.work(session, &["add", "Retain the actual claim during writer contention", "--json"]);
    let work = added["work"]["short_ref"].as_str().or_else(|| added["short_ref"].as_str()).unwrap();
    fixture.work(session, &["claim", work, "--json"]);
    let binding = fixture.claim_binding(session);
    let database = fixture.store().database_path;
    fixture.live.transport.shutdown_session(session);
    let mut connection = rusqlite::Connection::open(&database).unwrap();
    let transaction = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).unwrap();
    let prompt = "Deliver once while the disposable tracker writer is held.";
    let dispatch = dispatch_live_root(&fixture.live.state, session, prompt, None);
    deliver_turn_dispatch(&fixture.live.state, dispatch).unwrap();
    assert_one_provider_prompt(&fixture.live.receiver, session, prompt);
    transaction.commit().unwrap();
    assert_eq!(fixture.claim_binding(session), binding, "the real held claim is unchanged");
    assert_no_live_turn_gate(&fixture.live.transport);
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn ordinary_live_slow_read_keeps_its_real_budget_without_authorizing_a_turn() {
    let fixture = named_root_completion_live::completion_fixture();
    fixture.live.transport.faults.lock().unwrap().push_back(BoundaryFault::DelayAfterReply {
        operation: "work_session_read", delay: LIVE_RESPONSE_DELAY,
    });
    let target = fixture.target(&fixture.live.session_id);
    let session = target.adapter.read_work_session(&target.connection, &fixture.store(), target.settings.call_timeout()).unwrap();
    assert_eq!(session, fixture.live.session_id);
    let observations = fixture.live.transport.observations();
    let delayed = observations.iter().find(|row| row.disposition == "delayed_after_reply").unwrap();
    assert!(delayed.elapsed >= LIVE_RESPONSE_DELAY);
    assert!(delayed.timeout > LIVE_RESPONSE_DELAY);
    let prompt = "A read is evidence of service identity, never a grant.";
    let dispatch = dispatch_live_root(&fixture.live.state, &session, prompt, None);
    deliver_turn_dispatch(&fixture.live.state, dispatch).unwrap();
    assert_one_provider_prompt(&fixture.live.receiver, &session, prompt);
    assert_no_live_turn_gate(&fixture.live.transport);
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn ordinary_live_restart_does_not_redeliver_a_persisted_provider_handoff() {
    let fixture = live_root_fixture("ordinary-post-handoff-restart", []);
    let prompt = "Keep this genuine persisted handoff exactly once.";
    let dispatch = dispatch_live_root(&fixture.state, &fixture.session_id, prompt, None);
    deliver_turn_dispatch(&fixture.state, dispatch).unwrap();
    assert_one_provider_prompt(&fixture.receiver, &fixture.session_id, prompt);
    let saved = persisted_session_json(&fixture.state, &fixture.session_id);
    assert!(saved.contains(prompt));
    let restarted = restart_live_root_fixture(fixture);
    let loaded = persisted_session_json(&restarted.state, &restarted.session_id);
    assert!(loaded.contains(prompt), "private original history survives restart");
    restarted.state.run_post_listen_boot().unwrap();
    for _ in 0..3 {
        let _resume = restarted.state.resume_session_queue(&restarted.session_id);
        assert!(take_live_provider_prompts(&restarted.receiver, &restarted.session_id).is_empty());
    }
    assert_no_live_turn_gate(&restarted.transport);
    restarted.state.shutdown_persist_blocking();
}

#[path = "engram_carried_gate_live.rs"]
mod carried_gate_live;

#[path = "engram_named_root_completion_live.rs"]
mod named_root_completion_live;
