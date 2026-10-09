//! Causal evidence at actual root completion and queued-head admission.
//! Owns disposable control/writer fixtures, not recovery policy or live stores.
//! Registered by engram_root_dispatch.rs; no provider process is launched.

use super::*;

#[derive(Clone, Copy)]
enum CausalFaultMode {
    LostReply,
    NeverStarted,
    Refuse,
    SecretReply,
    Malformed,
    MissingBindingStatus,
    MissingBeginReceipt,
    MismatchedBegin,
    HealMalformed,
    HealRefused,
    BudgetAfterGrant,
    MalformedRequestSecret,
    RecoveryMalformedRequestSecret,
    MalformedEnvelopeSecret,
    RebindPhaseSecret,
    RecoveryReceiptSecret,
    RecoveryRefusal,
}

struct CausalTransport {
    producer: Arc<StatefulEngramControlTransport>,
    // Settable so a later phase (a cold restore) can select its own fault.
    fault: Mutex<Option<(&'static str, usize)>>,
    attempts: Mutex<HashMap<String, usize>>,
    applications: Mutex<HashSet<(String, String)>>,
    lost_request: Mutex<Option<Value>>,
    // A capability the producer returned, for absence checks.
    echoed: Mutex<Option<String>>,
    mode: Mutex<CausalFaultMode>,
    clock: Mutex<Option<EngramBudgetClock>>,
    after_fault: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl CausalTransport {
    fn new(fault: Option<(&'static str, usize)>) -> Arc<Self> {
        Arc::new(Self {
            producer: StatefulEngramControlTransport::new(),
            fault: Mutex::new(fault),
            attempts: Mutex::new(HashMap::new()),
            applications: Mutex::new(HashSet::new()),
            lost_request: Mutex::new(None),
            echoed: Mutex::new(None),
            mode: Mutex::new(CausalFaultMode::LostReply),
            clock: Mutex::new(None),
            after_fault: Mutex::new(None),
        })
    }

    fn application_count(&self, operation: &str) -> usize {
        self.applications
            .lock()
            .unwrap()
            .iter()
            .filter(|(applied, _)| applied == operation)
            .count()
    }
}

impl EngramControlTransport for CausalTransport {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        let wire = serde_json::to_value(request).unwrap();
        let operation = wire["operation"].as_str().unwrap().to_owned();
        let ordinal = {
            let mut attempts = self.attempts.lock().unwrap();
            let count = attempts.entry(operation.clone()).or_default();
            *count += 1;
            *count
        };
        let fault = *self.fault.lock().unwrap() == Some((operation.as_str(), ordinal));
        let mode = *self.mode.lock().unwrap();
        if fault && matches!(mode, CausalFaultMode::MalformedEnvelopeSecret) {
            // The real framed decoder, fed an envelope whose tag echoes a
            // capability and an opaque value; never a producer application.
            *self.lost_request.lock().unwrap() = Some(wire.clone());
            let tag = format!(
                "opaque-envelope-sentinel-5d21 {}",
                wire["routing_token"].as_str().unwrap()
            );
            let frame = format!("{}\n", json!({"status": tag}));
            return exchange_engram_control_frame(
                &mut Vec::new(),
                &mut std::io::Cursor::new(frame.into_bytes()),
                b"{}",
            );
        }
        if fault && matches!(mode, CausalFaultMode::RecoveryReceiptSecret) {
            // A typed-valid receipt naming another grant; never applied.
            *self.lost_request.lock().unwrap() = Some(wire.clone());
            return Ok(json!({"decision": "checkpointed", "receipt": {
                "grant_id": "opaque-receipt-sentinel-6b0d", "cursor": 1, "confirmed_cursor": 1
            }}));
        }
        if fault && matches!(mode, CausalFaultMode::RecoveryRefusal) {
            // An explicit producer refusal of the recovery checkpoint.
            *self.lost_request.lock().unwrap() = Some(wire.clone());
            return Ok(json!({"decision": "refuse", "code": "lifecycle_hold"}));
        }
        if fault && matches!(mode, CausalFaultMode::MalformedRequestSecret | CausalFaultMode::RecoveryMalformedRequestSecret) {
            // Deliberately invalid protocol vectors, never a producer application.
            // Include both a known capability and an unrelated opaque value.
            *self.lost_request.lock().unwrap() = Some(wire.clone());
            let value = format!("opaque-response-sentinel-73f9 {}", wire["routing_token"].as_str().unwrap());
            return Ok(if operation == "session_status" {
                json!(value)
            } else {
                json!({"decision": value})
            });
        }
        if matches!(
            mode,
            CausalFaultMode::HealMalformed | CausalFaultMode::HealRefused
        ) {
            // Synthetic protocol vectors, not a claim that the stateful
            // producer expired this grant. Exercise the existing heal branch.
            if operation == "turn_begin" && ordinal == 2 {
                return Ok(json!({"decision":"refuse", "code":"grant_expired"}));
            }
            if operation == "turn_evaluate" && ordinal == 3 {
                *self.lost_request.lock().unwrap() = Some(wire.clone());
                return Ok(if matches!(mode, CausalFaultMode::HealMalformed) {
                    json!({"decision":"malformed"})
                } else {
                    json!({"decision":"refuse", "directive": {
                        "directive_id":"fixture-refusal", "code":"lifecycle_hold",
                        "target":"agent", "satisfaction":"unavailable"
                    }})
                });
            }
        }
        if fault
            && matches!(
                mode,
                CausalFaultMode::NeverStarted | CausalFaultMode::Refuse
            )
        {
            *self.lost_request.lock().unwrap() = Some(wire.clone());
            return match mode {
                CausalFaultMode::NeverStarted => Err(EngramTransportError::spawn_failed(
                    "control worker executable missing; request never started",
                )),
                // An explicit response for this request, not a proof about
                // prior same-key applications or permission to retry.
                CausalFaultMode::Refuse => {
                    Ok(json!({"decision":"refuse", "code":"lifecycle_hold"}))
                }
                _ => unreachable!(),
            };
        }
        // The stateful producer applies its real fixture contract first.
        // A lost reply is not evidence that this application never occurred.
        let mut reply = self.producer.request(connection, request, timeout)?;
        if operation == "session_status"
            && matches!(
                mode,
                CausalFaultMode::RecoveryMalformedRequestSecret
                    | CausalFaultMode::RecoveryReceiptSecret
                    | CausalFaultMode::RecoveryRefusal
            )
        {
            // Synthetic open-grant status to reach the existing recovery branch;
            // not a claim that the stateful producer kept this completed turn open.
            // "begun" selects the cold restore's closing branch; warm ignores it.
            reply["phase"] = json!("turn_open");
            reply["open_grant_id"] = json!("fixture-open-recovery-grant");
            reply["open_grant_state"] = json!("begun");
        }
        if operation == "turn_evaluate"
            && reply["decision"] == "grant"
            && matches!(mode, CausalFaultMode::SecretReply)
        {
            // The lifecycle producer models grant/key ownership, not delivery
            // cursors. Supply its optional wire page so the real host builds
            // a nonempty Begin capability list, rather than a helper-only test.
            reply["grant"]["delivery"] = json!({"page": {
                "from_cursor": 0, "to_cursor": 1, "head_cursor": 1,
                "delivery_token": "opaque-capability-7eb9f4d2"
            }});
        }
        if matches!(
            reply["decision"].as_str(),
            Some("grant" | "begin" | "checkpointed")
        ) {
            self.applications.lock().unwrap().insert((
                operation.clone(),
                wire["idempotency_key"].as_str().unwrap().to_owned(),
            ));
        }
        if fault {
            *self.lost_request.lock().unwrap() = Some(wire.clone());
            if let Some(after) = self.after_fault.lock().unwrap().take() {
                after();
            }
            match mode {
                CausalFaultMode::Malformed => return Ok(json!({"decision":"malformed"})),
                CausalFaultMode::MissingBindingStatus => {
                    reply.as_object_mut().unwrap().remove("status");
                    return Ok(reply);
                }
                CausalFaultMode::MissingBeginReceipt => {
                    reply.as_object_mut().unwrap().remove("receipt");
                    return Ok(reply);
                }
                CausalFaultMode::MismatchedBegin => {
                    reply["receipt"]["grant_id"] = json!("different-grant");
                    return Ok(reply);
                }
                CausalFaultMode::RebindPhaseSecret => {
                    // A typed-valid bind whose phase echoes the returned
                    // capability; the producer did apply this bind.
                    let returned = reply["routing_token"].as_str().unwrap().to_owned();
                    reply["status"]["phase"] =
                        json!(format!("opaque-phase-sentinel-91e4 {returned}"));
                    *self.echoed.lock().unwrap() = Some(returned);
                    return Ok(reply);
                }
                CausalFaultMode::BudgetAfterGrant => {
                    self.clock
                        .lock()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .advance(Duration::from_secs(21));
                    return Ok(reply);
                }
                _ => {}
            }
            let secret_detail = if matches!(mode, CausalFaultMode::SecretReply) {
                format!(
                    "; {} {} password = \"secret with spaces\"",
                    wire["routing_token"].as_str().unwrap(),
                    wire["delivery_tokens"]
                        .as_array()
                        .map(|tokens| tokens
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" "))
                        .unwrap_or_default()
                )
            } else {
                String::new()
            };
            return Err(EngramTransportError::transport(format!(
                "control worker reached EOF after applying {operation}; response frame missing{secret_detail}"
            )));
        }
        Ok(reply)
    }

    fn read_observation_policy(
        &self,
        _connection: &EngramConnectionConfig,
        _store: &EngramAuthorityStoreKey,
        _timeout: Duration,
        _host_workdir: &FsPath,
    ) -> std::result::Result<EngramObservationPolicyBasis, EngramTransportError> {
        // This unclaimed lifecycle fixture deliberately has no source policy.
        // Never fall through to a CLI or a live project/store read.
        Err(EngramTransportError::protocol(
            "unclaimed causal fixture has no observation policy",
        ))
    }

    fn shutdown_session(&self, session_id: &str) {
        self.producer.shutdown_session(session_id);
    }
}

struct CausalRoot {
    state: AppState,
    session: String,
    receiver: mpsc::Receiver<CodexRuntimeCommand>,
    transport: Arc<CausalTransport>,
    committed: mpsc::Receiver<std::result::Result<u64, String>>,
    writer_gate: Arc<Mutex<Option<CausalWriterGate>>>,
    fail_writes: Arc<std::sync::atomic::AtomicBool>,
}

struct CausalWriterGate {
    arrived: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
    // Hold the writer at its next tick whatever it carries, not only at its
    // first causal record.
    any_tick: bool,
}

impl CausalRoot {
    fn new(fault: Option<(&'static str, usize)>) -> Self {
        let (mut state, session, receiver, _) = root_fixture([]);
        let transport = CausalTransport::new(fault);
        state.install_control_test_transport(transport.clone());
        state.inner.lock().unwrap().test_engram_dispatch_budget = Some(Duration::from_secs(20));
        let (persist_tx, persist_rx) = mpsc::channel();
        let (committed_tx, committed) = mpsc::channel();
        state.persist_tx = persist_tx;
        state
            .persist_worker_alive
            .store(true, std::sync::atomic::Ordering::Release);
        let inner = Arc::clone(&state.inner);
        let path = Arc::clone(&state.persistence_path);
        let writer_gate = Arc::new(Mutex::new(None::<CausalWriterGate>));
        let worker_gate = Arc::clone(&writer_gate);
        let fail_writes = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_fail = Arc::clone(&fail_writes);
        let worker = std::thread::spawn(move || {
            let mut cache = SqlitePersistConnectionCache::new();
            let mut fences = PersistFenceBatch::default();
            // Blocking event-driven writer: no polling, fake ACK or sleep.
            while let Ok(request) = persist_rx.recv() {
                let shutdown = matches!(fences.accept(request), PersistWorkerWaitOutcome::Shutdown);
                let shutdown = fences.drain(&persist_rx, shutdown);
                let delta = collect_persist_delta_from_shared_state(&inner, 0);
                let has_cause = delta.changed_sessions.iter().any(|record| {
                    record.session.messages.iter().any(|message| {
                        matches!(message, Message::EngramControl { card, .. }
                            if card.causal_failure.is_some())
                    })
                });
                let gate = {
                    let mut gate = worker_gate.lock().unwrap();
                    if gate.as_ref().is_some_and(|gate| has_cause || gate.any_tick) {
                        gate.take()
                    } else {
                        None
                    }
                };
                if let Some(gate) = gate {
                    gate.arrived.send(()).unwrap();
                    gate.release.recv_timeout(DEADLOCK_GUARD).unwrap();
                }
                if worker_fail.load(std::sync::atomic::Ordering::Acquire) {
                    // Real SQLite write rejection on the writer's connection.
                    // Reapply after cache invalidation; never synthesize an ACK.
                    cache.connection_for(&path).unwrap()
                        .execute_batch("PRAGMA query_only = ON").unwrap();
                }
                let result = persist_delta_with_fences(&mut cache, &path, &delta, &mut fences)
                    .map(|_| delta.watermark)
                    .map_err(|error| format!("{error:#}"));
                let _ = committed_tx.send(result);
                if shutdown {
                    fences.fail(PersistFenceError::WorkerStopped);
                    break;
                }
            }
        });
        *state.persist_thread_handle.lock().unwrap() = Some(worker);
        Self {
            state,
            session,
            receiver,
            transport,
            committed,
            writer_gate,
            fail_writes,
        }
    }

    fn start_and_queue(&self) -> (RuntimeToken, Value, u64) {
        deliver_turn_dispatch(
            &self.state,
            root_dispatch(&self.state, &self.session, false),
        )
        .unwrap();
        assert!(matches!(
            self.receiver.try_recv().unwrap(),
            CodexRuntimeCommand::Prompt { .. }
        ));
        assert!(self.receiver.try_recv().is_err());
        let queued = self
            .state
            .dispatch_turn(
                &self.session,
                SendMessageRequest {
                    text: "Exact retained continuation".to_owned(),
                    expanded_text: None,
                    attachments: Vec::new(),
                    source_session_id: None,
                    source_mailbox: None,
                },
            )
            .unwrap();
        assert!(matches!(queued, DispatchTurnResult::Queued));
        let inner = self.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&self.session).unwrap()];
        (
            record.runtime.runtime_token().unwrap(),
            serde_json::to_value(&record.queued_prompts.front().unwrap().pending_prompt).unwrap(),
            record.engram.dispatch_generation,
        )
    }

    /// Deterministic join on causal publication, without sleeps. Each pending
    /// cause's fence was sent with its commit, and this writer publishes it
    /// inside the tick that resolves the fence, before reporting that tick.
    /// Use only when every write succeeds; a failed ACK stays pending.
    fn await_causal_publication(&self) {
        loop {
            let pending = {
                let inner = self.state.inner.lock().unwrap();
                inner.find_session_index(&self.session).is_some_and(|index| {
                    inner.sessions[index].session.messages.iter().any(|message| {
                        matches!(message, Message::EngramControl { card, .. }
                            if card.causal_failure.as_ref()
                                .is_some_and(|cause| cause.publication_pending))
                    })
                })
            };
            if !pending {
                return;
            }
            let tick = self.committed.recv_timeout(DEADLOCK_GUARD);
            assert!(
                matches!(tick, Ok(Ok(_))),
                "the writer must report the tick that publishes a pending cause"
            );
        }
    }

    fn durable_cards(&self) -> Vec<Value> {
        let (watermark, end_index) = {
            let inner = self.state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&self.session).unwrap()];
            (inner.last_mutation_stamp, record.session.messages.len())
        };
        // Wait for the connected writer's real COMMIT, before Drop's final
        // synchronous snapshot. This is not serializer-only/fallback proof.
        self.state.persist_tx.send(PersistRequest::Delta).unwrap();
        loop {
            let written = self
                .committed
                .recv_timeout(DEADLOCK_GUARD)
                .unwrap()
                .unwrap();
            if written >= watermark {
                break;
            }
        }
        load_persisted_message_range(&self.state.persistence_path, &self.session, 0, end_index)
            .unwrap()
            .into_iter()
            .filter_map(|(_, message)| match message {
                Message::EngramControl { card, .. } => Some(serde_json::to_value(card).unwrap()),
                _ => None,
            })
            .collect()
    }
}

#[path = "engram_root_causal_publication.rs"]
mod causal_publication;

#[path = "engram_root_causal_rebind.rs"]
mod causal_rebind;

#[path = "engram_root_causal_value_free.rs"]
mod causal_value_free;

impl Drop for CausalRoot {
    fn drop(&mut self) {
        // Join before AppState's declared-last temp owner removes this store.
        // Also runs during an assertion unwind, with no detached writer.
        self.state.shutdown_persist_blocking();
    }
}

fn assert_root_causal_failure(operation: &'static str, ordinal: usize) {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = CausalRoot::new(Some((operation, ordinal)));
    let (token, original_head, generation) = fixture.start_and_queue();
    let completion = fixture
        .state
        .finish_turn_ok_if_runtime_matches(&fixture.session, &token);
    fixture.await_causal_publication();
    let handoffs = fixture
        .receiver
        .try_iter()
        .filter(|command| matches!(command, CodexRuntimeCommand::Prompt { .. }))
        .count();
    let (head, cards, preview, current_generation) = {
        let inner = fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
        (
            record
                .queued_prompts
                .front()
                .map(|queued| serde_json::to_value(&queued.pending_prompt).unwrap()),
            record
                .session
                .messages
                .iter()
                .filter_map(|message| match message {
                    Message::EngramControl { card, .. } => {
                        Some(serde_json::to_value(card).unwrap())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>(),
            record.session.preview.clone(),
            record.engram.dispatch_generation,
        )
    };
    let durable = fixture.durable_cards();
    let lost = fixture
        .transport
        .lost_request
        .lock()
        .unwrap()
        .clone()
        .expect("selected fault reached from actual root completion");
    eprintln!(
        "causal witness operation={operation} key={} grant={} native_completion={completion:?} handoffs={handoffs} generation={generation}->{current_generation} exact_head={} preview={preview:?} applications=evaluate:{}/begin:{}/checkpoint:{} cards={} durable={}",
        lost["idempotency_key"],
        lost["grant_id"],
        head.as_ref() == Some(&original_head),
        fixture.transport.application_count("turn_evaluate"),
        fixture.transport.application_count("turn_begin"),
        fixture.transport.application_count("turn_checkpoint"),
        serde_json::to_string(&cards).unwrap(),
        serde_json::to_string(&durable).unwrap()
    );
    assert_eq!(cards, durable, "actual writer persisted the normal cards");
    let failed = cards
        .iter()
        .rev()
        .find(|card| card["decision"] == "degraded")
        .expect("normal failure card");
    assert_eq!(
        failed["causalFailure"]["operation"], operation,
        "normal card must identify the original failure boundary"
    );
    assert_eq!(
        failed["causalFailure"]["originalCode"],
        "control_unavailable"
    );
    assert_eq!(failed["causalFailure"]["failureClass"], "transport");
    if operation == "turn_checkpoint" {
        let origin = cards
            .iter()
            .find(|card| card["stage"] == "checkpoint" && card["decision"] == "degraded")
            .unwrap();
        assert_eq!(
            origin["causalFailure"]["attemptId"],
            failed["causalFailure"]["attemptId"]
        );
        assert_eq!(
            origin["causalFailure"]["message"],
            failed["causalFailure"]["message"]
        );
        assert_eq!(
            origin["causalFailure"]["continuationId"],
            original_head["id"]
        );
        assert!(failed["causalFailure"]["continuationReason"]
            .as_str()
            .unwrap()
            .contains("no new Evaluate"));
        assert_eq!(
            fixture.transport.attempts.lock().unwrap()["turn_evaluate"],
            1
        );
    }
    assert!(failed["causalFailure"]["message"]
        .as_str()
        .unwrap()
        .contains("response frame missing"));
    assert_eq!(failed["causalFailure"]["remoteApplication"], "unknown");
    assert_eq!(
        failed["causalFailure"]["attemptId"],
        lost["idempotency_key"]
    );
    assert!(failed["causalFailure"]["continuationReason"]
        .as_str()
        .is_some_and(|reason| !reason.is_empty()));
    assert_eq!(
        head.as_ref(),
        Some(&original_head),
        "exact queued prompt remains owned"
    );
    assert_eq!(handoffs, 0, "unsettled continuation never reaches provider");
}

#[test]
fn root_causal_evaluate_reply_loss_survives_card_and_writer() {
    assert_root_causal_failure("turn_evaluate", 2);
}

#[test]
fn root_causal_begin_reply_loss_survives_card_and_writer() {
    assert_root_causal_failure("turn_begin", 2);
}

#[test]
fn root_causal_closing_reply_loss_survives_card_and_writer() {
    assert_root_causal_failure("turn_checkpoint", 1);
}

#[test]
fn root_causal_healthy_completion_hands_off_once_with_actual_applications() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = CausalRoot::new(None);
    let (token, _, _) = fixture.start_and_queue();
    fixture
        .state
        .finish_turn_ok_if_runtime_matches(&fixture.session, &token)
        .unwrap();
    assert!(matches!(
        fixture.receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    assert!(fixture.receiver.try_recv().is_err());
    assert_eq!(fixture.transport.application_count("turn_evaluate"), 2);
    assert_eq!(fixture.transport.application_count("turn_begin"), 2);
    assert_eq!(fixture.transport.application_count("turn_checkpoint"), 1);
    let durable = fixture.durable_cards();
    assert_eq!(durable.len(), 3);
    assert!(durable.iter().all(|card| card["decision"] != "degraded"));
}

#[test]
fn root_causal_never_started_and_explicit_refusal_keep_distinct_knowledge() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for (mode, expected_class, expected_application) in [
        (CausalFaultMode::NeverStarted, "transport", "not_started"),
        (CausalFaultMode::Refuse, "producer_refusal", "refused"),
    ] {
        let fixture = CausalRoot::new(Some(("turn_begin", 2)));
        *fixture.transport.mode.lock().unwrap() = mode;
        let (token, _, _) = fixture.start_and_queue();
        let completion = fixture
            .state
            .finish_turn_ok_if_runtime_matches(&fixture.session, &token);
        if matches!(mode, CausalFaultMode::Refuse) {
            assert_eq!(
                completion.unwrap_err().to_string(),
                "failed to deliver queued turn dispatch: Engram did not authorize this turn for runtime delivery"
            );
        } else {
            completion.unwrap();
        }
        let cards = fixture.durable_cards();
        let cause = &cards.last().unwrap()["causalFailure"];
        assert_eq!(cause["operation"], "turn_begin");
        assert_eq!(cause["failureClass"], expected_class);
        assert_eq!(cause["remoteApplication"], expected_application);
        if matches!(mode, CausalFaultMode::Refuse) {
            assert_eq!(cause["originalCode"], "lifecycle_hold");
            assert!(cause["message"]
                .as_str()
                .unwrap()
                .contains("prior application is not determined"));
        }
        assert_eq!(fixture.transport.application_count("turn_begin"), 1);
        assert!(fixture.receiver.try_recv().is_err());
    }
}

#[test]
fn root_causal_request_secrets_never_reach_committed_cards() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = CausalRoot::new(Some(("turn_begin", 2)));
    *fixture.transport.mode.lock().unwrap() = CausalFaultMode::SecretReply;
    let (token, _, _) = fixture.start_and_queue();
    fixture
        .state
        .finish_turn_ok_if_runtime_matches(&fixture.session, &token)
        .unwrap();
    let cards = fixture.durable_cards();
    let lost = fixture
        .transport
        .lost_request
        .lock()
        .unwrap()
        .clone()
        .unwrap();
    let serialized = serde_json::to_string(&cards).unwrap();
    assert!(!serialized.contains(lost["routing_token"].as_str().unwrap()));
    let tokens = lost["delivery_tokens"].as_array().unwrap();
    assert_eq!(tokens, &[json!("opaque-capability-7eb9f4d2")]);
    for token in tokens {
        assert!(!serialized.contains(token.as_str().unwrap()));
    }
    assert!(!serialized.contains("secret with spaces"));
    assert!(serialized.contains("response frame missing"));
    assert!(serialized.contains("redacted"));
}

#[test]
fn root_causal_public_stop_preserves_cause_and_distinct_operator_hold() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = CausalRoot::new(Some(("turn_evaluate", 2)));
    let (token, original_head, _) = fixture.start_and_queue();
    fixture
        .state
        .finish_turn_ok_if_runtime_matches(&fixture.session, &token)
        .unwrap();
    let before = fixture.durable_cards();
    fixture
        .state
        .request_stop_session(&fixture.session)
        .unwrap();
    {
        let inner = fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
        assert!(record.engram.operator_paused);
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(record.engram.admission_retry.is_none());
        assert!(record.session.preview.contains("authorization canceled"));
        assert_eq!(
            serde_json::to_value(&record.queued_prompts.front().unwrap().pending_prompt).unwrap(),
            original_head
        );
    }
    assert_eq!(
        fixture.durable_cards(),
        before,
        "Stop keeps historical cause, not a new failure"
    );
    assert!(fixture.receiver.try_recv().is_err());
    assert_eq!(
        fixture.transport.attempts.lock().unwrap()["turn_evaluate"],
        2
    );
}

#[test]
fn root_causal_legacy_card_loads_without_inventing_failure_details() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = CausalRoot::new(None);
    fixture.start_and_queue();
    let mut wire = fixture.durable_cards().into_iter().next().unwrap();
    wire.as_object_mut().unwrap().remove("causalFailure");
    let loaded: EngramControlCard = serde_json::from_value(wire).unwrap();
    assert!(loaded.causal_failure.is_none());
    assert!(serde_json::to_value(loaded)
        .unwrap()
        .get("causalFailure")
        .is_none());
}

#[test]
fn root_causal_checkpoint_link_rejects_stale_unrelated_reset_and_settled_causes() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = CausalRoot::new(Some(("turn_checkpoint", 1)));
    let (token, _, _) = fixture.start_and_queue();
    // Exercise the actual checkpoint before admission's legitimate promotion.
    fixture.state.checkpoint_engram_turn_off_lock(
        &fixture.session,
        EngramCheckpointPurpose::TurnTerminal,
        Some(&token),
        None,
        EngramNextIntent::Continue,
        Some(EngramExecutionOutcome::Succeeded),
        None,
    );
    let mut inner = fixture.state.inner.lock().unwrap();
    let target =
        AppState::engram_binding_target_for_session_shape_locked(&inner, &fixture.session, true)
            .unwrap()
            .unwrap();
    let index = inner.find_session_index(&fixture.session).unwrap();
    let record = &mut inner.sessions[index];
    let owner = EngramQueuedAdmissionOwner::capture(record).unwrap();
    assert!(engram_checkpoint_cause_for_owner(record, &target, &owner).is_some());
    let mut stale = owner.clone();
    stale.prompt_id = "unrelated-head".to_owned();
    assert!(engram_checkpoint_cause_for_owner(record, &target, &stale).is_none());
    stale = owner.clone();
    stale.active_turn_generation += 1;
    assert!(engram_checkpoint_cause_for_owner(record, &target, &stale).is_none());
    record.engram.project_reset_in_progress = true;
    assert!(engram_checkpoint_cause_for_owner(record, &target, &owner).is_none());
    record.engram.project_reset_in_progress = false;
    record.runtime_stop_in_progress = true;
    assert!(engram_checkpoint_cause_for_owner(record, &target, &owner).is_none());
    record.runtime_stop_in_progress = false;
    let mut other_target = target.clone();
    other_target.connection.actor_id.push_str("-replacement");
    assert!(engram_checkpoint_cause_for_owner(record, &other_target, &owner).is_none());
    // A later successful card for the same grant masks its old failure even
    // before the normal settlement clears the local grant.
    let mut success = record
        .session
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::EngramControl { card, .. } if card.stage == EngramControlStage::Checkpoint => {
                Some(card.clone())
            }
            _ => None,
        })
        .unwrap();
    success.decision = EngramControlCardDecision::Grant;
    success.causal_failure = None;
    record.session.messages.push(Message::EngramControl {
        id: "later-success".to_owned(),
        timestamp: stamp_now(),
        author: Author::Assistant,
        card: success,
    });
    assert!(engram_checkpoint_cause_for_owner(record, &target, &owner).is_none());
}

#[test]
fn root_causal_redaction_is_bounded_and_never_upgrades_remote_errors() {
    for text in [
        "EOF; password=hidden",
        "EOF; PASSWORD : 'hidden spaced'",
        "EOF; Authorization: Bearer hidden",
        "EOF; routing_token\":\"hidden\"",
        "EOF; secret = hidden",
        "EOF; token=hidden",
        "EOF; token   = 'hidden spaced'",
        "EOF; \"TOKEN\": \"hidden\"",
    ] {
        let redacted = engram_causal_text(text, &[]);
        assert!(redacted.starts_with("EOF; "));
        assert!(!redacted.contains("hidden"));
    }
    assert_eq!(
        engram_causal_text("EOF\u{1b}[lost] capability", &["capability"]),
        "EOF [lost] [redacted]"
    );
    assert!(engram_causal_text(&"ż".repeat(900), &[]).len() <= 1024);
    let remote = EngramTransportError::remote(EngramControlErrorBody {
        code: "control_unavailable".to_owned(),
        message: "worker stopped after write".to_owned(),
    });
    let cause = EngramCausalFailure::error(&remote, "turn_begin", None, &[]);
    assert_eq!(cause.remote_application, EngramRemoteApplication::Unknown);
    assert_eq!(cause.failure_class, EngramCausalFailureClass::Remote);
    assert!(cause.attempt_id.is_none());
}

#[test]
fn root_causal_parse_mismatch_and_healing_refusal_keep_actual_request_identity() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for (mode, operation, expected_class, application) in [
        (
            CausalFaultMode::Malformed,
            "turn_evaluate",
            "protocol",
            "unknown",
        ),
        (
            CausalFaultMode::MismatchedBegin,
            "turn_begin",
            "protocol",
            "unknown",
        ),
        (
            CausalFaultMode::HealMalformed,
            "turn_evaluate",
            "protocol",
            "unknown",
        ),
        (
            CausalFaultMode::HealRefused,
            "turn_evaluate",
            "producer_refusal",
            "refused",
        ),
    ] {
        let fault_op = if matches!(
            mode,
            CausalFaultMode::HealMalformed | CausalFaultMode::HealRefused
        ) {
            "turn_begin"
        } else {
            operation
        };
        let fixture = CausalRoot::new(Some((fault_op, 2)));
        *fixture.transport.mode.lock().unwrap() = mode;
        let (token, original_head, _) = fixture.start_and_queue();
        let _completion = fixture
            .state
            .finish_turn_ok_if_runtime_matches(&fixture.session, &token);
        let cards = fixture.durable_cards();
        let cause = &cards.last().unwrap()["causalFailure"];
        let request = fixture
            .transport
            .lost_request
            .lock()
            .unwrap()
            .clone()
            .unwrap();
        assert_eq!(cause["operation"], operation);
        assert_eq!(cause["attemptId"], request["idempotency_key"]);
        assert_eq!(cause["failureClass"], expected_class);
        assert_eq!(cause["remoteApplication"], application);
        if matches!(
            mode,
            CausalFaultMode::HealMalformed | CausalFaultMode::HealRefused
        ) {
            assert!(cause["continuationReason"]
                .as_str()
                .unwrap()
                .contains("grant_expired"));
        }
        let head = {
            let inner = fixture.state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
            record
                .queued_prompts
                .front()
                .map(|queued| serde_json::to_value(&queued.pending_prompt).unwrap())
        };
        if matches!(mode, CausalFaultMode::HealRefused) {
            assert_eq!(head, None, "explicit refusal retires the promoted prompt");
        } else {
            assert_eq!(
                head.as_ref(),
                Some(&original_head),
                "unresolved request retains its exact prompt"
            );
        }
        assert!(fixture.receiver.try_recv().is_err());
    }
}

#[test]
fn root_causal_expired_pre_begin_budget_is_current_request_not_started() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = CausalRoot::new(Some(("turn_evaluate", 2)));
    *fixture.transport.mode.lock().unwrap() = CausalFaultMode::BudgetAfterGrant;
    *fixture.transport.clock.lock().unwrap() =
        Some(fixture.state.select_test_scripted_engram_budget_clock());
    let (token, _, _) = fixture.start_and_queue();
    fixture
        .state
        .finish_turn_ok_if_runtime_matches(&fixture.session, &token)
        .unwrap();
    let cards = fixture.durable_cards();
    let cause = &cards.last().unwrap()["causalFailure"];
    assert_eq!(cause["operation"], "turn_begin");
    assert_eq!(cause["originalCode"], "dispatch_budget_exhausted");
    assert_eq!(cause["failureClass"], "deadline");
    assert_eq!(cause["remoteApplication"], "not_started");
    assert_eq!(fixture.transport.attempts.lock().unwrap()["turn_begin"], 1);
    assert!(fixture.receiver.try_recv().is_err());
}

#[test]
fn root_causal_local_preparation_preserves_error_but_not_prior_application_certainty() {
    let error =
        EngramTransportError::local_state("Failed to persist prepared evaluate: writer stopped");
    let cause = EngramCausalFailure::local(&error, "turn_evaluate", "Evaluate durable preparation");
    assert_eq!(cause.failure_class, EngramCausalFailureClass::LocalState);
    assert_eq!(
        cause.remote_application,
        EngramRemoteApplication::NotStarted
    );
    assert!(cause.message.contains("writer stopped"));
    let mut earlier = EngramTransportError::transport("lost reply to an earlier operation");
    earlier.causal_failure = Some(EngramCausalFailure::error(
        &earlier,
        "turn_checkpoint",
        Some("prior-key"),
        &[],
    ));
    let carried =
        EngramCausalFailure::local(&earlier, "turn_evaluate", "Evaluate durable preparation");
    assert_eq!(carried.operation, "turn_checkpoint");
    assert_eq!(carried.remote_application, EngramRemoteApplication::Unknown);
}

#[test]
fn root_causal_superseded_response_never_decorates_replacement_prompt() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for held in [false, true] {
        let fixture = CausalRoot::new(Some(("turn_evaluate", 2)));
        let (token, mut replacement, _) = fixture.start_and_queue();
        replacement["id"] = json!("replacement-prompt");
        let shared = Arc::clone(&fixture.state.inner);
        let session = fixture.session.clone();
        *fixture.transport.after_fault.lock().unwrap() = Some(Box::new(move || {
            let mut inner = shared.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            record.queued_prompts.front_mut().unwrap().pending_prompt.id =
                "replacement-prompt".to_owned();
            record.engram.dispatch_generation += 1;
            // Separate intentional hold: the runnable companion must still
            // demonstrate that a stale response does not block its successor.
            if held {
                record.set_auto_dispatch_blocked(true);
            }
        }));
        fixture
            .state
            .finish_turn_ok_if_runtime_matches(&fixture.session, &token)
            .unwrap();
        let cards = fixture.durable_cards();
        let lost = fixture
            .transport
            .lost_request
            .lock()
            .unwrap()
            .clone()
            .unwrap();
        assert!(cards
            .iter()
            .all(|card| card["causalFailure"]["attemptId"] != lost["idempotency_key"]));
        let (head, blocked, active_generation) = {
            let inner = fixture.state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
            (
                record
                    .queued_prompts
                    .front()
                    .map(|queued| serde_json::to_value(&queued.pending_prompt).unwrap()),
                record.orchestrator_auto_dispatch_blocked,
                record.active_turn_generation,
            )
        };
        if held {
            assert_eq!(head.as_ref(), Some(&replacement));
            assert!(blocked, "stale response must preserve the independent hold");
            assert_eq!(fixture.transport.application_count("turn_begin"), 1);
        } else {
            assert_eq!(head, None, "runnable replacement was legitimately promoted");
            assert!(!blocked);
            match fixture.receiver.try_recv().unwrap() {
                CodexRuntimeCommand::Prompt {
                    session_id,
                    command,
                } => {
                    assert_eq!(session_id, fixture.session);
                    assert_eq!(command.active_turn_generation, active_generation);
                    assert!(command.prompt.contains("Exact retained continuation"));
                }
                _ => panic!("replacement must reach its provider as a prompt"),
            }
            assert_eq!(fixture.transport.application_count("turn_begin"), 2);
        }
        assert!(fixture.receiver.try_recv().is_err(), "no duplicate handoff");
    }
}
