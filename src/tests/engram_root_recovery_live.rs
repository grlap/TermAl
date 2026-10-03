//! Operator-run root-session recovery checks against a real Engram binary.
//!
//! Every test creates a disposable Engram home and SQLite store. Provider
//! delivery remains simulated in-process; only the Engram JSON-lines control
//! path is real. Faults are injected after Engram has produced a reply, at the
//! TermAl transport boundary, so a discarded reply represents an uncertain
//! committed operation rather than a synthetic pre-write failure.

use super::*;

const OLD_CALL_TIMEOUT: Duration = Duration::from_millis(250);
const OLD_DISPATCH_BUDGET: Duration = Duration::from_millis(600);
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
}

impl BoundaryFault {
    fn operation(self) -> &'static str {
        match self {
            Self::DelayAfterReply { operation, .. } | Self::DropAfterReply { operation } => {
                operation
            }
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
    evaluate_entered: Mutex<Option<mpsc::Sender<()>>>,
}

impl BoundaryFaultTransport {
    fn new(faults: impl IntoIterator<Item = BoundaryFault>) -> Arc<Self> {
        Arc::new(Self {
            inner: ProcessEngramControlTransport::default(),
            faults: Mutex::new(faults.into_iter().collect()),
            observations: Mutex::new(Vec::new()),
            root_reads: Mutex::new(Vec::new()),
            evaluate_entered: Mutex::new(None),
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
        if operation == "turn_evaluate" {
            if let Some(sender) = self.evaluate_entered.lock().unwrap().take() {
                sender.send(()).unwrap();
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

    fn read_work_binding(
        &self,
        connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>,
        timeout: Duration,
    ) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.inner
            .read_work_binding(connection, preference, timeout)
    }

    fn read_work_binding_for_boot(
        &self,
        connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>,
        timeout: Duration,
    ) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.inner
            .read_work_binding_for_boot(connection, preference, timeout)
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
            "--required-assurance",
            ENGRAM_CONTROL_ASSURANCE,
            "--authorized-by",
            "termal-live-root-recovery-test",
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
            turn_gated_control: true,
            binary_path: Some(binary_path.to_string_lossy().into_owned()),
            home: Some(home.to_string_lossy().into_owned()),
            work_authority_grant: None,
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
        assert!(inner.test_engram_dispatch_budget.is_none());
        state
            .commit_locked(&mut inner)
            .expect("live root settings should persist");
    }

    let transport = BoundaryFaultTransport::new(faults);
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
        .expect("live root completion should checkpoint");
}

fn operation_key(request: &Value) -> &str {
    request["idempotency_key"]
        .as_str()
        .expect("mutating Engram request should carry an idempotency key")
}

fn retry_live_bind_without_resume(fixture: &LiveRootFixture) {
    let (retry, prepared_bind, waiter) = {
        let inner = fixture.state.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner
            .find_session_index(&fixture.session_id)
            .expect("live root should exist")];
        let head = record.queued_prompts.front().expect("retained bind head");
        let retry = record.engram.bind_retry.as_ref().expect("typed bind retry");
        assert_eq!(retry.proof.prompt_id, head.pending_prompt.id);
        assert_eq!(retry.proof.phase, engram_bind_retry_phase(head));
        assert!(matches!(
            retry.proof.phase,
            EngramBindRetryPhase::Prepared { .. }
        ));
        assert_eq!(
            retry.proof.dispatch_generation,
            record.engram.dispatch_generation
        );
        assert_eq!(
            retry.proof.promoted_turn_generation,
            record.active_turn_generation
        );
        assert_eq!(
            record.engram.bind_retry_runtime,
            record.runtime.runtime_token()
        );
        assert!(record.engram.bind_retry_runtime.is_some());
        assert_eq!(retry.attempts, 1);
        assert!(
            !retry.acknowledged,
            "park starts with acknowledgement pending"
        );
        assert!(!record.engram.abort_retry_acknowledged);
        assert!(record.engram.abort_retry_saved || record.engram.abort_retry_fence.is_some());
        assert_eq!(record.session.status, SessionStatus::Idle);
        assert!(head.engram_waiting);
        assert!(!head.engram_interrupted);
        assert!(head.engram_evaluate.is_none());
        assert!(record.engram.active_grant_id.is_none());
        assert!(record.engram.uncertain_grant_id.is_none());
        (
            retry.clone(),
            head.engram_bind.clone().expect("prepared bind retained"),
            record.engram.abort_retry_fence.clone(),
        )
    };
    let due = chrono::DateTime::parse_from_rfc3339(&retry.due_at)
        .expect("retry has a valid due time")
        .with_timezone(&chrono::Utc);
    let saved: PersistedSessionRecord =
        serde_json::from_str(&persisted_session_json(&fixture.state, &fixture.session_id))
            .expect("durable parked session");
    assert_eq!(
        serde_json::to_value(
            saved
                .engram_bind_retry
                .as_ref()
                .expect("durable bind retry")
        )
        .unwrap(),
        serde_json::to_value(&retry).unwrap(),
    );
    assert_eq!(
        saved.queued_prompts[0].pending_prompt.id,
        retry.proof.prompt_id
    );
    assert_eq!(
        serde_json::to_value(&saved.queued_prompts[0].engram_bind).unwrap(),
        serde_json::to_value(Some(&prepared_bind)).unwrap(),
    );
    assert!(saved.queued_prompts[0].engram_waiting);
    assert!(saved.orchestrator_auto_dispatch_blocked);
    assert_eq!(fixture.transport.requests_for("session_bind").len(), 1);
    assert!(fixture.transport.requests_for("turn_evaluate").is_empty());
    assert!(fixture.transport.requests_for("turn_begin").is_empty());

    // The fixture uses the production synchronous SQLite fallback, not a
    // fabricated successful fence. If a writer is present, wait off the lock.
    if let Some(waiter) = waiter {
        assert!(matches!(
            waiter
                .0
                .wait_until(std::time::Instant::now() + phase_sync::DEADLOCK_GUARD),
            Some(Ok(()))
        ));
    }
    fixture.state.engram_abort_retry_tick(due);
    {
        let inner = fixture.state.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()];
        assert!(record.engram.bind_retry.as_ref().unwrap().acknowledged);
        assert!(record.engram.abort_retry_acknowledged);
        assert!(record.orchestrator_auto_dispatch_blocked);
    }
    let acknowledged: PersistedSessionRecord =
        serde_json::from_str(&persisted_session_json(&fixture.state, &fixture.session_id))
            .expect("durable acknowledged session");
    assert!(
        acknowledged
            .engram_bind_retry
            .as_ref()
            .unwrap()
            .acknowledged
    );
    assert_eq!(fixture.transport.requests_for("session_bind").len(), 1);
    assert!(
        fixture.receiver.try_recv().is_err(),
        "acknowledgement is not handoff"
    );

    // Give the scheduler its pre-due time explicitly. There is no Resume in
    // this schedule, and no retry authority/backoff field is modified.
    fixture
        .state
        .engram_abort_retry_tick(due - chrono::Duration::milliseconds(1));
    assert_eq!(fixture.transport.requests_for("session_bind").len(), 1);
    assert!(fixture.transport.requests_for("turn_evaluate").is_empty());
    assert!(fixture.transport.requests_for("turn_begin").is_empty());
    assert!(
        fixture.receiver.try_recv().is_err(),
        "a pre-due tick cannot deliver"
    );

    // The producer and the host budget clock are real in this live fixture.
    // Wait for BOTH published deadlines, rather than assuming a fixed sleep
    // expires the retry and transport backoff. The guard diagnoses a hang.
    let guard = phase_sync::PollGuard::new();
    loop {
        let backoff_elapsed = {
            let inner = fixture.state.inner.lock().expect("state mutex poisoned");
            let record = &inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()];
            record
                .engram
                .next_bind_retry_at
                .is_none_or(|at| at <= inner.engram_budget_clock.now())
        };
        if chrono::Utc::now() >= due && backoff_elapsed {
            break;
        }
        guard.wait("live retained-bind retry due and transport backoff elapsed");
    }
    fixture.state.engram_abort_retry_tick(chrono::Utc::now());
    let observations = fixture.transport.observations();
    let admission = observations
        .iter()
        .filter(|observation| {
            matches!(
                observation.request["operation"].as_str(),
                Some("session_bind" | "turn_evaluate" | "turn_begin")
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        admission
            .iter()
            .map(|item| item.request["operation"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "session_bind",
            "session_bind",
            "turn_evaluate",
            "turn_begin"
        ]
    );
    assert_eq!(admission[0].disposition, "dropped_after_reply");
    assert_eq!(admission[1].disposition, "returned");
    assert_eq!(admission[0].request, admission[1].request);
    assert_eq!(
        operation_key(&admission[0].request),
        operation_key(&admission[1].request)
    );
    let inner = fixture.state.inner.lock().expect("state mutex poisoned");
    let record = &inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()];
    assert!(
        record.engram.bind_retry.is_none(),
        "evaluate retires bind-only scheduling"
    );
    assert!(record.engram.bind_retry_runtime.is_none());
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn live_root_deadline_retains_cancelable_orchestrator_prompt() {
    let fixture = live_root_fixture(
        "deadline",
        [BoundaryFault::DelayAfterReply {
            operation: "turn_evaluate",
            delay: Duration::from_millis(ENGRAM_DISPATCH_BUDGET_MS) + Duration::from_secs(1),
        }],
    );
    let prompt = "Preserve orchestrator intent across the real admission deadline.";
    let started = std::time::Instant::now();
    let dispatch = dispatch_live_root(
        &fixture.state,
        &fixture.session_id,
        prompt,
        Some(QueuedPromptSource::Orchestrator),
    );
    deliver_turn_dispatch(&fixture.state, dispatch)
        .expect("deadline is waiting, not a policy denial");
    assert!(started.elapsed() >= Duration::from_millis(ENGRAM_DISPATCH_BUDGET_MS));
    assert!(fixture.receiver.try_recv().is_err());
    assert!(fixture.transport.requests_for("turn_begin").is_empty());
    let id = {
        let inner = fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()];
        assert!(record.session.preview.contains("Waiting/Unknown"));
        let queued = record.queued_prompts.front().unwrap();
        assert_eq!(queued.pending_prompt.text, prompt);
        assert!(matches!(queued.source, QueuedPromptSource::Orchestrator));
        queued.pending_prompt.id.clone()
    };
    fixture
        .state
        .cancel_queued_prompt(&fixture.session_id, &id)
        .expect("cancel works after timeout");
    assert!(fixture.receiver.try_recv().is_err());
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn live_root_real_policy_refusal_never_begins_or_delivers() {
    for (suffix, source) in [
        ("deny-direct", None),
        ("deny-mailbox", Some(QueuedPromptSource::Mailbox)),
    ] {
        let fixture = live_root_fixture(suffix, []);
        let target = fixture
            .state
            .ensure_engram_session_bound_off_lock(&fixture.session_id)
            .unwrap()
            .unwrap();
        let changed = Command::new(&target.connection.binary_path)
            .arg("--project-file")
            .arg(&target.connection.project_file)
            .arg("--home")
            .arg(&target.connection.home)
            .args([
                "control-policy",
                "set-required-assurance",
                "action_gated",
                "--authorized-by",
                "termal-live-test",
                "--idempotency-key",
                suffix,
            ])
            .output()
            .expect("disposable policy command launches");
        assert!(
            changed.status.success(),
            "{}",
            String::from_utf8_lossy(&changed.stderr)
        );
        let dispatch = dispatch_live_root(
            &fixture.state,
            &fixture.session_id,
            "Refuse this exact operation.",
            source,
        );
        assert_eq!(
            deliver_turn_dispatch(&fixture.state, dispatch)
                .unwrap_err()
                .status,
            StatusCode::CONFLICT
        );
        assert!(fixture.receiver.try_recv().is_err());
        assert!(fixture.transport.requests_for("turn_begin").is_empty());
        let inner = fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()];
        assert!(record.session.messages.iter().any(|message| matches!(message,
            Message::EngramControl { card, .. } if card.refusal_code.as_deref() == Some("control_assurance_insufficient"))));
    }
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn live_root_writer_contention_and_retired_sidecar_still_admit_once() {
    let fixture = live_root_fixture("writer-lock", []);
    let target = fixture
        .state
        .ensure_engram_session_bound_off_lock(&fixture.session_id)
        .unwrap()
        .unwrap();
    let project = fs::read_to_string(&target.connection.project_file).unwrap();
    let db = target
        .connection
        .home
        .join("projects")
        .join(sha256_hex(project.trim().as_bytes()))
        .join("engram.db");
    assert!(db.is_file());
    // Same state as an idle-retired worker, without a wall-clock idle sleep.
    fixture.transport.shutdown_session(&fixture.session_id);
    let mut connection = rusqlite::Connection::open(db).unwrap();
    let transaction = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    *fixture.transport.evaluate_entered.lock().unwrap() = Some(entered_tx);
    let prompt = "Wait for the disposable SQLite writer, then deliver exactly once.";
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let dispatch = dispatch_live_root(&fixture.state, &fixture.session_id, prompt, None);
            deliver_turn_dispatch(&fixture.state, dispatch)
        });
        entered_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("evaluation reaches the held writer");
        std::thread::sleep(Duration::from_millis(900));
        transaction.commit().unwrap();
        worker
            .join()
            .unwrap()
            .expect("writer release permits admission");
    });
    assert_one_provider_prompt(&fixture.receiver, &fixture.session_id, prompt);
    let observations = fixture.transport.observations();
    assert!(
        observations
            .iter()
            .any(|item| item.request["operation"] == "turn_evaluate"
                && item.elapsed > OLD_DISPATCH_BUDGET)
    );
    finish_live_root(&fixture.state, &fixture.session_id);
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn live_root_default_budget_allows_replies_beyond_the_retired_limits() {
    assert_eq!(
        EngramProjectSettings::default().call_timeout(),
        Duration::from_secs(20)
    );
    assert_eq!(ENGRAM_DISPATCH_BUDGET_MS, 20_000);

    for (suffix, queued_source) in [
        ("delayed-direct", None),
        ("delayed-mailbox", Some(QueuedPromptSource::Mailbox)),
    ] {
        let fixture = live_root_fixture(
            suffix,
            [
                BoundaryFault::DelayAfterReply {
                    operation: "session_bind",
                    delay: LIVE_RESPONSE_DELAY,
                },
                BoundaryFault::DelayAfterReply {
                    operation: "turn_evaluate",
                    delay: LIVE_RESPONSE_DELAY,
                },
                BoundaryFault::DelayAfterReply {
                    operation: "turn_begin",
                    delay: LIVE_RESPONSE_DELAY,
                },
            ],
        );
        let prompt = format!("Preserve exact {suffix} prompt bytes.");
        let started_at = std::time::Instant::now();
        let dispatch =
            dispatch_live_root(&fixture.state, &fixture.session_id, &prompt, queued_source);
        assert!(fixture.receiver.try_recv().is_err());
        deliver_turn_dispatch(&fixture.state, dispatch)
            .expect("delayed real grant and begin should still admit the prompt");
        let admission_elapsed = started_at.elapsed();
        assert!(
            admission_elapsed > OLD_DISPATCH_BUDGET,
            "real admission should prove the retired 600 ms budget is gone: {admission_elapsed:?}"
        );
        assert_one_provider_prompt(&fixture.receiver, &fixture.session_id, &prompt);
        finish_live_root(&fixture.state, &fixture.session_id);

        let observations = fixture.transport.observations();
        for operation in ["session_bind", "turn_evaluate", "turn_begin"] {
            let observation = observations
                .iter()
                .find(|observation| observation.request["operation"] == operation)
                .expect("delayed operation should be observed");
            assert_eq!(observation.disposition, "delayed_after_reply");
            assert!(observation.elapsed > OLD_CALL_TIMEOUT);
            assert!(observation.timeout <= Duration::from_millis(ENGRAM_DISPATCH_BUDGET_MS));
            assert!(observation.timeout > OLD_CALL_TIMEOUT);
        }

        let first_evaluate = fixture.transport.requests_for("turn_evaluate");
        assert_eq!(first_evaluate.len(), 1);
        let second_prompt = format!("Use a new operation identity after {suffix}.");
        let second = dispatch_live_root(&fixture.state, &fixture.session_id, &second_prompt, None);
        deliver_turn_dispatch(&fixture.state, second).expect("second operation should be admitted");
        assert_one_provider_prompt(&fixture.receiver, &fixture.session_id, &second_prompt);
        finish_live_root(&fixture.state, &fixture.session_id);
        let evaluates = fixture.transport.requests_for("turn_evaluate");
        assert_eq!(evaluates.len(), 2);
        assert_ne!(
            operation_key(&evaluates[0]),
            operation_key(&evaluates[1]),
            "a new prompt must mint a new evaluation identity"
        );
    }
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn live_root_lost_committed_replies_replay_exact_requests_once() {
    for operation in ["session_bind", "turn_evaluate", "turn_begin"] {
        let fixture = live_root_fixture(
            &format!("drop-{operation}"),
            [BoundaryFault::DropAfterReply { operation }],
        );
        let prompt = format!("Retain exact root prompt after lost {operation} reply.");
        let dispatch = dispatch_live_root(&fixture.state, &fixture.session_id, &prompt, None);
        deliver_turn_dispatch(&fixture.state, dispatch)
            .expect("uncertain real admission should park rather than deny");
        assert!(
            fixture.receiver.try_recv().is_err(),
            "an uncertain operation must not reach the provider"
        );
        {
            let inner = fixture.state.inner.lock().expect("state mutex poisoned");
            let record = &inner.sessions[inner
                .find_session_index(&fixture.session_id)
                .expect("live root should exist")];
            let queued = record
                .queued_prompts
                .front()
                .expect("uncertain prompt should remain queued");
            assert_eq!(queued.pending_prompt.text, prompt);
            assert!(queued.has_engram_intent());
            if operation == "session_bind" {
                assert!(record.engram.bind_retry.is_some());
                assert!(
                    record
                        .session
                        .preview
                        .contains("bind deferred before delivery")
                );
            } else {
                assert!(record.engram.bind_retry.is_none());
                assert!(record.session.preview.contains("Waiting/Unknown"));
            }
            assert!(record.orchestrator_auto_dispatch_blocked);
        }
        let persisted = persisted_session_json(&fixture.state, &fixture.session_id);
        assert!(persisted.contains(&prompt));
        assert!(persisted.contains("engram_"));

        if operation == "session_bind" {
            retry_live_bind_without_resume(&fixture);
        } else {
            // Evaluate/begin uncertainty keeps its explicit recovery boundary.
            std::thread::sleep(Duration::from_millis(1_100));
            fixture
                .state
                .resume_session_queue(&fixture.session_id)
                .expect("exact uncertain operation should replay after backoff");
        }
        assert_one_provider_prompt(&fixture.receiver, &fixture.session_id, &prompt);
        fixture.state.engram_abort_retry_tick(chrono::Utc::now());
        assert!(
            fixture.receiver.try_recv().is_err(),
            "a later tick cannot hand off twice"
        );
        finish_live_root(&fixture.state, &fixture.session_id);

        let requests = fixture.transport.requests_for(operation);
        assert_eq!(
            requests.len(),
            if operation == "turn_evaluate" { 3 } else { 2 }
        );
        assert_eq!(
            requests[0], requests[1],
            "lost {operation} must replay the exact serialized request"
        );
        if operation == "turn_evaluate" {
            assert_ne!(
                operation_key(&requests[1]),
                operation_key(&requests[2]),
                "a sidecar restart retires an issued grant; only a fresh evaluation can replace it"
            );
        }
        assert_eq!(
            fixture
                .transport
                .observations()
                .iter()
                .filter(|observation| observation.disposition == "dropped_after_reply")
                .count(),
            1,
            "only the selected committed reply should be discarded"
        );
    }
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn live_root_restart_after_lost_evaluate_replays_issued_grant_without_rebinding() {
    let fixture = live_root_fixture(
        "restart-after-evaluate",
        [BoundaryFault::DropAfterReply {
            operation: "turn_evaluate",
        }],
    );
    let prompt = "Replay this exact root evaluation after restart, then deliver once.";
    let dispatch = dispatch_live_root(&fixture.state, &fixture.session_id, prompt, None);
    deliver_turn_dispatch(&fixture.state, dispatch)
        .expect("lost evaluate receipt should leave an uncertain queued admission");
    assert!(fixture.receiver.try_recv().is_err());
    let original_evaluate = fixture.transport.requests_for("turn_evaluate");
    assert_eq!(original_evaluate.len(), 1);
    let persisted = persisted_session_json(&fixture.state, &fixture.session_id);
    assert!(persisted.contains("engram_evaluate"));
    assert!(persisted.contains(prompt));

    let root_guard = fixture
        .state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .clone();
    let default_workdir = fixture
        .state
        .inner
        .lock()
        .expect("state mutex poisoned")
        .sessions
        .iter()
        .find(|record| record.session.id == fixture.session_id)
        .expect("live root should exist")
        .session
        .workdir
        .clone();
    let persistence_path = fixture.state.persistence_path.as_path().to_path_buf();
    let templates_path = fixture
        .state
        .orchestrator_templates_path
        .as_path()
        .to_path_buf();
    let session_id = fixture.session_id.clone();
    drop(fixture.cleanup);
    drop(fixture.receiver);
    drop(fixture.state);
    drop(fixture.transport);

    let recovery_transport = BoundaryFaultTransport::new([]);
    let restarted = AppState::new_with_paths_and_engram_transport_for_test(
        default_workdir,
        persistence_path,
        templates_path,
        recovery_transport.clone(),
    )
    .expect("TermAl should recover the durable issued-grant admission");
    let _cleanup = LiveRootCleanup {
        transport: recovery_transport.clone(),
        session_id: session_id.clone(),
    };
    // Keep the independent Base context-nudge feature out of the prompt-byte
    // assertion, as in live_root_fixture. The queued user intent is unchanged.
    {
        let mut inner = restarted.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("restored root exists");
        inner.sessions[index].engram.context_nudge_pending = false;
    }
    let (runtime, receiver, _process) =
        test_shared_codex_runtime("engram-live-root-evaluate-restart-provider");
    install_single_wire_codex_fixture(&restarted, Some(runtime));

    restarted
        .resume_session_queue(&session_id)
        .expect("an issued grant should replay the exact evaluation and continue");
    assert_one_provider_prompt(&receiver, &session_id, prompt);
    let replayed_evaluate = recovery_transport.requests_for("turn_evaluate");
    assert_eq!(replayed_evaluate.len(), 2);
    assert_eq!(
        original_evaluate[0], replayed_evaluate[0],
        "cold issued-grant recovery must replay the exact evaluation request"
    );
    let operations = recovery_transport
        .observations()
        .into_iter()
        .map(|observation| {
            observation.request["operation"]
                .as_str()
                .expect("operation should serialize")
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        operations,
        [
            "session_status",
            "turn_evaluate",
            "turn_begin",
            "session_status",
            "turn_evaluate",
            "turn_begin"
        ],
        "reconcile the replayed expired grant before a fresh evaluation, without rebinding"
    );
    finish_live_root(&restarted, &session_id);

    restarted.shutdown_persist_blocking();
    drop(_cleanup);
    drop(restarted);
    drop(root_guard);
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn live_root_restart_after_lost_begin_never_blindly_redelivers() {
    let fixture = live_root_fixture(
        "restart-after-begin",
        [BoundaryFault::DropAfterReply {
            operation: "turn_begin",
        }],
    );
    let prompt = "Do not redeliver this root prompt after a lost begin receipt.";
    let dispatch = dispatch_live_root(&fixture.state, &fixture.session_id, prompt, None);
    deliver_turn_dispatch(&fixture.state, dispatch)
        .expect("lost begin receipt should leave an uncertain queued admission");
    assert!(fixture.receiver.try_recv().is_err());
    let persisted = persisted_session_json(&fixture.state, &fixture.session_id);
    assert!(persisted.contains("engram_evaluate"));
    assert!(persisted.contains(prompt));

    let root_guard = fixture
        .state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .clone();
    let default_workdir = fixture
        .state
        .inner
        .lock()
        .expect("state mutex poisoned")
        .sessions
        .iter()
        .find(|record| record.session.id == fixture.session_id)
        .expect("live root should exist")
        .session
        .workdir
        .clone();
    let persistence_path = fixture.state.persistence_path.as_path().to_path_buf();
    let templates_path = fixture
        .state
        .orchestrator_templates_path
        .as_path()
        .to_path_buf();
    let session_id = fixture.session_id.clone();
    drop(fixture.cleanup);
    drop(fixture.receiver);
    drop(fixture.state);
    drop(fixture.transport);

    let recovery_transport = BoundaryFaultTransport::new([]);
    let restarted = AppState::new_with_paths_and_engram_transport_for_test(
        default_workdir,
        persistence_path,
        templates_path,
        recovery_transport.clone(),
    )
    .expect("TermAl should recover the retained admission without fresh delivery");
    let _cleanup = LiveRootCleanup {
        transport: recovery_transport.clone(),
        session_id: session_id.clone(),
    };
    let (runtime, receiver, _process) =
        test_shared_codex_runtime("engram-live-root-restart-provider");
    install_single_wire_codex_fixture(&restarted, Some(runtime));

    // Resume can acknowledge the reconciliation without starting a turn. Its
    // HTTP success is not evidence of provider delivery; inspect state below.
    let _resume = restarted.resume_session_queue(&session_id);
    assert!(
        receiver.try_recv().is_err(),
        "cold recovery must never redeliver a possibly delivered prompt"
    );
    {
        let inner = restarted.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner
            .find_session_index(&session_id)
            .expect("restarted live root should exist")];
        let queued = record
            .queued_prompts
            .front()
            .expect("interrupted prompt should remain queued");
        assert_eq!(queued.pending_prompt.text, prompt);
        assert!(queued.engram_interrupted);
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(record.session.preview.contains("interrupted/unknown"));
    }
    let operations = recovery_transport
        .observations()
        .into_iter()
        .map(|observation| {
            observation.request["operation"]
                .as_str()
                .expect("operation should serialize")
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(operations, ["session_status", "turn_checkpoint"]);
    assert!(
        !operations
            .iter()
            .any(|operation| operation == "turn_evaluate" || operation == "turn_begin")
    );

    restarted.shutdown_persist_blocking();
    drop(_cleanup);
    drop(restarted);
    drop(root_guard);
}

// Carried background gates against the real store, a child module so the
// launcher's `live` mode runs them.
#[path = "engram_carried_gate_live.rs"]
mod carried_gate_live;

// Claim recovery and named-root completion through the real control plane.
#[path = "engram_named_root_completion_live.rs"]
mod named_root_completion_live;
