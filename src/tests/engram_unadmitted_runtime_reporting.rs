//! Reporting of adopted runtime-started turns through the production Claude
//! frame application and real session persistence. Owns producer observations,
//! uncredited check fanout and report replay; does not own turn admission or
//! the already-tested ownership/continuity rules. New module, not a split.

use super::*;

#[path = "engram_unadmitted_reporting_repairs.rs"]
mod repairs;

#[path = "engram_unadmitted_storage.rs"]
mod storage;

#[derive(Clone, Debug)]
struct SentObservation {
    request: Value,
    receipt: Value,
    durable_before_send: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReplyMode {
    Recorded,
    LoseFirst,
    TransientFirst(&'static str),
    Refuse,
    /// Engram holds a different stored request under the report's key.
    Conflict,
}

/// A producer, rather than a host-side reporting implementation. Its replay
/// map accounts each immutable idempotency key once. Report requests must be
/// in the real sessions table before reaching this boundary. Other protocol
/// calls keep the existing fixture's canonical root and grant behavior.
struct ObservationProducer {
    base: Arc<ScriptedEngramControlTransport>,
    binding: RwLock<Option<EngramControlWorkBinding>>,
    path: PathBuf,
    sent: mpsc::Sender<SentObservation>,
    attempts: Mutex<Vec<Value>>,
    records: Mutex<BTreeMap<String, (Value, Value)>>,
    mode: ReplyMode,
    response_gate: (Mutex<bool>, Condvar),
    request_hook: Mutex<Option<Arc<dyn Fn(&EngramControlRequest) + Send + Sync>>>,
}

impl ObservationProducer {
    fn release(&self) {
        *self.response_gate.0.lock().unwrap() = false;
        self.response_gate.1.notify_all();
    }

    fn requests(&self) -> Vec<Value> {
        self.attempts.lock().unwrap().clone()
    }
}

impl EngramControlTransport for ObservationProducer {
    fn read_observation_policy(
        &self,
        connection: &EngramConnectionConfig,
        store: &EngramAuthorityStoreKey,
        timeout: Duration,
        workdir: &FsPath,
    ) -> Result<EngramObservationPolicyBasis, EngramTransportError> {
        self.base
            .read_observation_policy(connection, store, timeout, workdir)
    }

    fn read_work_binding(
        &self,
        _connection: &EngramConnectionConfig,
        _preference: EngramBindingPreference<'_>,
        _timeout: Duration,
    ) -> Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        Ok(self.binding.read().unwrap().clone())
    }

    fn read_held_claims(
        &self,
        connection: &EngramConnectionConfig,
        timeout: Duration,
    ) -> Result<EngramHeldClaims, EngramTransportError> {
        self.base.read_held_claims(connection, timeout)
    }

    fn shutdown_session(&self, session: &str) {
        self.base.shutdown_session(session);
    }

    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        let hook = self.request_hook.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(request);
        }
        let EngramControlRequest::ExecutionObserve { observation, .. } = request else {
            return self.base.request(connection, request, timeout);
        };
        engram_validate_unadmitted_observation(observation, chrono::Utc::now()).unwrap();
        let wire = serde_json::to_value(request).unwrap();
        assert_eq!(
            Some(&observation.binding),
            self.binding.read().unwrap().as_ref()
        );
        assert_eq!(
            observation.causality,
            EngramObservationCausality::Unknown {}
        );
        let occurrence = &wire["occurrence"];
        assert!(
            matches!(
                occurrence["kind"].as_str(),
                Some("unadmitted_turn" | "observed_check")
            ),
            "this producer boundary is only for runtime-turn observations"
        );
        let key = observation.idempotency_key.clone();
        let durable = {
            let db = open_sqlite_state_read_connection(&self.path).unwrap();
            let raw: String = db
                .query_row(
                    "SELECT value_json FROM sessions WHERE id = ?1",
                    rusqlite::params![connection.session_id],
                    |row| row.get(0),
                )
                .unwrap();
            raw.contains(&key) && raw.contains(occurrence["host_turn_ref"].as_str().unwrap())
        };
        let attempt = {
            let mut requests = self.attempts.lock().unwrap();
            requests.push(wire.clone());
            requests.len()
        };
        let receipt = {
            let mut records = self.records.lock().unwrap();
            if let Some((original, receipt)) = records.get(&key) {
                assert_eq!(
                    &wire, original,
                    "an uncertain reply never changes the saved request"
                );
                receipt.clone()
            } else {
                let id = format!("{:032x}", records.len() + 1000);
                let checks: Vec<Value> = if occurrence["kind"] == "observed_check" {
                    vec![occurrence["check"].clone()]
                } else {
                    occurrence["observed_checks"].as_array().unwrap().clone()
                };
                let receipt = json!({
                    "decision": "recorded", "observation": id,
                    "position": {"feed": {"kind": "run_execution", "id": observation.binding.run_id},
                        "position": records.len() + 20},
                    "observing_session": connection.session_id, "binding": observation.binding,
                    "admission": "unadmitted", "causality": observation.causality,
                    "policy_basis": observation.policy_basis,
                    "accounting": if occurrence["kind"] == "unadmitted_turn" && !occurrence["source_change"].is_null() {
                        json!({"kind": "source_change", "source_change": id})
                    } else {json!({"kind": "no_source_change"})},
                    "observed_checks": checks.iter().map(|check| json!({
                        "host_check_id": check["host_check_id"], "credit": "uncredited"
                    })).collect::<Vec<_>>(),
                    "opened_obligations": [],
                    "recorded_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                });
                EngramSourceObservationReceipt::from_unadmitted_result(
                    receipt.clone(),
                    observation,
                    &connection.session_id,
                )
                .unwrap();
                if self.mode != ReplyMode::Refuse
                    && self.mode != ReplyMode::Conflict
                    && !(matches!(self.mode, ReplyMode::TransientFirst(_)) && attempt == 1)
                {
                    records.insert(key, (wire.clone(), receipt.clone()));
                }
                receipt
            }
        };
        self.sent
            .send(SentObservation {
                request: wire,
                receipt: receipt.clone(),
                durable_before_send: durable,
            })
            .unwrap();
        let (gate, changed) = &self.response_gate;
        let mut blocked = gate.lock().unwrap();
        while *blocked {
            let (next, timeout) = changed.wait_timeout(blocked, DEADLOCK_GUARD).unwrap();
            blocked = next;
            assert!(
                !timeout.timed_out(),
                "test must release the producer response barrier"
            );
        }
        match self.mode {
            ReplyMode::TransientFirst(code) if attempt == 1 => {
                Err(EngramTransportError::remote(EngramControlErrorBody {
                    code: code.to_owned(),
                    message: "transient producer response; request was not applied".to_owned(),
                }))
            }
            ReplyMode::Refuse => Err(EngramTransportError::remote(EngramControlErrorBody {
                code: "execution_observation_policy_basis_mismatch".to_owned(),
                message: "definitive producer refusal".to_owned(),
            })),
            ReplyMode::Conflict => Err(EngramTransportError::remote(EngramControlErrorBody {
                code: "control_operation_idempotency_conflict".to_owned(),
                message: "the idempotency key was used for another request".to_owned(),
            })),
            ReplyMode::LoseFirst if attempt == 1 => Err(EngramTransportError::transport(
                "receipt lost after the producer committed the immutable request",
            )),
            _ => Ok(receipt),
        }
    }
}

/// Release a deliberately delayed RPC even if an assertion panics.
struct ReleaseResponse(Arc<ObservationProducer>);
impl Drop for ReleaseResponse {
    fn drop(&mut self) {
        self.0.release();
    }
}

struct ReportingTurn {
    turn: CheckedTurn,
    reader: ClaudeReaderOn,
    producer: Arc<ObservationProducer>,
    sent: mpsc::Receiver<SentObservation>,
    adoption_floor_ms: i64,
    _release: ReleaseResponse,
}

fn install_reporting_source_capture(state: &AppState) {
    // The functional protocol fixture measures the real repository contents,
    // but does not assert that local Git/filesystem I/O finishes within the
    // unrelated 250ms scripted RPC cap. Absence/backoff has its own controlled
    // test; production bounded capture is unchanged.
    state.inner.lock().unwrap().test_engram_source_capture =
        Some(Arc::new(|place, _budget| engram_place_source_basis(place)));
}

/// Restarts a reporting fixture's host from its persisted state, as a new
/// process would: a fresh `AppState` booted over the old one's SQLite file and
/// templates, answering Engram through `transport`. Like the fixture's first
/// state (`CheckedTurn::start_with`), it runs on a scripted Engram budget
/// clock, chosen before boot takes its first snapshot. On real time, the real
/// Git and filesystem work the source capture hook does would spend the
/// fixture's short RPC cap, and a loaded machine would fail the capture.
fn restart_reporting_state(
    root: &FsPath,
    old: &AppState,
    transport: Arc<dyn EngramControlTransport>,
) -> AppState {
    AppState::new_with_paths_engram_transport_and_clock_for_test(
        root.to_string_lossy().into_owned(),
        old.persistence_path.as_ref().clone(),
        old.orchestrator_templates_path.as_ref().clone(),
        transport,
        EngramBudgetClock::scripted(),
    )
    .unwrap()
}

impl ReportingTurn {
    fn new(label: &str, claimed: bool, mode: ReplyMode, blocked: bool) -> Self {
        Self::start(label, claimed, mode, blocked, true)
    }

    fn start(
        label: &str,
        claimed: bool,
        mode: ReplyMode,
        blocked: bool,
        previous_checkpoint: bool,
    ) -> Self {
        Self::start_with_leftover(label, claimed, mode, blocked, previous_checkpoint, None)
    }

    fn start_with_leftover(
        label: &str,
        claimed: bool,
        mode: ReplyMode,
        blocked: bool,
        previous_checkpoint: bool,
        leftover_grant: Option<&str>,
    ) -> Self {
        let turn = CheckedTurn::start_with(
            label,
            claimed,
            None,
            std::iter::once(checkpoint_reply(CHECK_GRANT))
                .chain(leftover_grant.map(checkpoint_reply))
                .collect(),
        );
        turn.finish();
        if let Some(grant) = leftover_grant {
            turn.record_mut(|record| record.engram.active_grant_id = Some(grant.to_owned()));
        }
        if !previous_checkpoint {
            turn.record_mut(|record| record.engram.runtime_checkpoint = None);
            turn.state
                .inner
                .lock()
                .unwrap()
                .engram_source_sightings
                .clear();
        }
        let (sent, received) = mpsc::channel();
        let producer = Arc::new(ObservationProducer {
            base: turn.transport.clone(),
            binding: RwLock::new(turn.record(|record| record.engram.work_binding.clone())),
            path: turn.state.persistence_path.as_ref().clone(),
            sent,
            attempts: Mutex::new(Vec::new()),
            records: Mutex::new(BTreeMap::new()),
            mode,
            response_gate: (Mutex::new(blocked), Condvar::new()),
            request_hook: Mutex::new(None),
        });
        turn.state.install_control_test_transport(producer.clone());
        install_reporting_source_capture(&turn.state);
        let mut reader = ClaudeReaderOn::new(&turn, new_claude_turn_ownership());
        let adoption_floor_ms = chrono::Utc::now().timestamp_millis();
        for frame in [
            task_notification(),
            init(),
            assistant_text("Inspecting the background result."),
        ] {
            reader.feed(frame);
        }
        assert!(turn.record(|record| record.adopted_claude_turn_generation.is_some()));
        Self {
            turn,
            reader,
            producer: producer.clone(),
            sent: received,
            adoption_floor_ms,
            _release: ReleaseResponse(producer),
        }
    }

    fn finish(&mut self) {
        self.reader.feed(result(false));
    }

    fn observation(&self) -> SentObservation {
        self.sent.recv_timeout(DEADLOCK_GUARD).unwrap_or_else(|error| {
            let state = self.turn.record(|record| {
                let closing: Vec<_> = record.engram.unadmitted_closing_jobs.iter().map(|job| {
                    (job.policy.is_some(), job.attempts, job.retry_at.clone(), job.last_error.clone(), job.terminal_failure.clone())
                }).collect();
                let reports: Vec<_> = record.engram.unadmitted_reports.iter().map(|report| {
                    (report.attempts, report.retry_at.clone(), report.envelopes.iter().map(|envelope| match &envelope.delivery {
                        EngramRuntimeObservationDelivery::Pending { last_error } => format!("pending: {last_error:?}"),
                        EngramRuntimeObservationDelivery::LocalFailure { code, .. } => format!("local_failure: {code}"),
                        EngramRuntimeObservationDelivery::Recorded { .. } => "recorded".to_owned(),
                        EngramRuntimeObservationDelivery::Refused { code, .. } => format!("refused: {code}"),
                    }).collect::<Vec<_>>())
                }).collect();
                (record.engram.unadmitted_worker_running, closing, reports)
            });
            panic!("adopted turn must emit its durable unadmitted observation without a coordinator wake: {error:?}; worker/closing/reports={state:?}")
        })
    }

    fn check(&mut self, id: &str, failed: bool) {
        self.reader
            .feed(json!({"type": "assistant", "message": {"content": [{
            "type": "tool_use", "id": id, "name": "Bash", "input": {"command": SIZE_TEST}}]}}));
        let output = if failed {
            "Exit code 101\nrunning 1 test\ntest result: FAILED. 0 passed; 1 failed"
        } else {
            "running 1 test\ntest result: ok. 1 passed; 0 failed"
        };
        self.reader.feed(
            json!({"type": "user", "message": {"role": "user", "content": [{
            "type": "tool_result", "tool_use_id": id, "is_error": failed, "content": output}]},
            "tool_use_result": {"stdout": output, "stderr": "", "interrupted": false}}),
        );
    }

    fn assert_no_granted_activity(&self) {
        self.assert_no_granted_reporting();
        self.turn
            .record(|record| assert!(!record.orchestrator_auto_dispatch_blocked));
    }

    fn assert_no_granted_reporting(&self) {
        let requests = self.turn.transport.requests();
        for operation in ["turn_evaluate", "turn_begin", "turn_checkpoint"] {
            assert_eq!(
                requests
                    .iter()
                    .filter(|request| request.request["operation"] == operation)
                    .count(),
                1,
                "only the original mediated turn may use {operation}"
            );
        }
        self.turn.record(|record| {
            assert!(record.engram.active_turn_checks.is_empty());
        });
    }
}

fn occurrence(observation: &SentObservation) -> &Value {
    &observation.request["occurrence"]
}

#[test]
fn adopted_edit_reaches_accounted_observation_with_exact_end_binding_and_basis() {
    let mut fixture = ReportingTurn::new("unadmitted-edit", true, ReplyMode::Recorded, false);
    let before = fixture
        .turn
        .record(|record| record.engram.continuity_anchor.clone().unwrap());
    fixture
        .turn
        .change_readme("changed by the runtime-started turn\n");
    fixture
        .reader
        .feed(json!({"type": "assistant", "message": {"content": [{
        "type": "tool_use", "id": "write", "name": "Write",
        "input": {"file_path": fixture.turn.root.join("README.md"), "content": "changed"}}]}}));
    fixture.reader.feed(
        json!({"type": "user", "message": {"role": "user", "content": [{
        "type": "tool_result", "tool_use_id": "write", "content": "File updated"}]},
        "tool_use_result": {"type": "update", "filePath": fixture.turn.root.join("README.md")}}),
    );
    let after = fixture.turn.revision();
    fixture.finish();
    let observed = fixture.observation();
    assert!(
        observed.durable_before_send,
        "pending report must survive losing its first reply"
    );
    assert_eq!(occurrence(&observed)["kind"], "unadmitted_turn");
    assert_eq!(
        observed.request["binding"],
        serde_json::to_value(fixture.producer.binding.read().unwrap().as_ref().unwrap()).unwrap()
    );
    assert_eq!(observed.request["root_basis"]["state"]["state"], "none");
    assert!(
        observed.request["root_basis"]["capture_run_cut"]
            .as_i64()
            .unwrap()
            > 0
    );
    let change = &occurrence(&observed)["source_change"];
    assert_eq!(change["detection"], "content_comparison");
    assert_eq!(
        change["baseline"]["source_revision"],
        before.source_revision
    );
    assert_eq!(
        change["baseline"]["observed_at"],
        before.observed_at.unwrap(),
        "do not invent a fresh timestamp for the earlier checkpoint measurement"
    );
    let interval = &observed.request["observed_interval"];
    let from = chrono::DateTime::parse_from_rfc3339(interval["from"].as_str().unwrap()).unwrap();
    let baseline_at =
        chrono::DateTime::parse_from_rfc3339(change["baseline"]["observed_at"].as_str().unwrap())
            .unwrap();
    assert_eq!(
        from, baseline_at,
        "the envelope keeps the original earlier baseline cut"
    );
    assert!(
        from.timestamp_millis() <= fixture.adoption_floor_ms,
        "an endpoint comparison does not invent an earlier runtime start"
    );
    let through =
        chrono::DateTime::parse_from_rfc3339(interval["through"].as_str().unwrap()).unwrap();
    let sighting_at =
        chrono::DateTime::parse_from_rfc3339(change["sighting"]["observed_at"].as_str().unwrap())
            .unwrap();
    assert!(through >= sighting_at);
    assert!(through.with_timezone(&chrono::Utc) <= chrono::Utc::now());
    assert_eq!(change["sighting"]["source_basis"]["source_revision"], after);
    assert_eq!(observed.receipt["accounting"]["kind"], "source_change");
    assert_eq!(
        observed.request["policy_basis"]["mode"],
        "account_if_eligible"
    );
    fixture.assert_no_granted_activity();
}

#[test]
fn equal_source_reports_null_change_without_manufacturing_an_edit() {
    let mut fixture = ReportingTurn::new("unadmitted-equal", true, ReplyMode::Recorded, false);
    fixture.finish();
    let observed = fixture.observation();
    assert!(occurrence(&observed)["source_change"].is_null());
    assert_eq!(observed.receipt["accounting"]["kind"], "no_source_change");
    assert!(observed.durable_before_send);
    fixture.assert_no_granted_activity();
}

#[test]
fn absent_previous_checkpoint_is_reported_as_missing_baseline() {
    let mut fixture = ReportingTurn::start(
        "unadmitted-no-baseline",
        true,
        ReplyMode::Recorded,
        false,
        false,
    );
    fixture
        .turn
        .change_readme("unattributed history without a checkpoint basis\n");
    fixture.finish();
    let observed = fixture.observation();
    assert_eq!(
        occurrence(&observed)["source_change"]["detection"],
        "assumed_missing_baseline"
    );
    fixture.assert_no_granted_activity();
}

#[test]
fn seventy_checks_cross_the_local_ring_and_all_reach_the_producer_uncredited_once() {
    let mut fixture = ReportingTurn::new("unadmitted-seventy", true, ReplyMode::Recorded, false);
    for index in 0..70 {
        fixture.check(&format!("observed-{index:02}"), index % 2 == 1);
    }
    assert_eq!(
        fixture
            .turn
            .record(|record| record.unassigned_claude_observations.len()),
        64,
        "positive control: the transcript ring has evicted older observations"
    );
    fixture.finish();
    let first = fixture.observation();
    assert_eq!(occurrence(&first)["kind"], "unadmitted_turn");
    let turn_ref = occurrence(&first)["host_turn_ref"].clone();
    let checks = occurrence(&first)["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 16);
    let mut by_id: BTreeMap<String, Value> = BTreeMap::new();
    let mut outcomes = BTreeMap::<String, usize>::new();
    let mut collect = |report: &SentObservation| {
        assert!(report.durable_before_send);
        assert_eq!(occurrence(report)["host_turn_ref"], turn_ref);
        let checks = if occurrence(report)["kind"] == "observed_check" {
            vec![occurrence(report)["check"].clone()]
        } else {
            occurrence(report)["observed_checks"]
                .as_array()
                .unwrap()
                .clone()
        };
        for check in checks {
            let id = check["host_check_id"].as_str().unwrap().to_owned();
            assert!(
                by_id.insert(id.clone(), check.clone()).is_none(),
                "check {id} sent twice"
            );
            assert!(
                report.request["idempotency_key"]
                    .as_str()
                    .unwrap()
                    .contains(turn_ref.as_str().unwrap())
            );
            if occurrence(report)["kind"] == "observed_check" {
                assert!(
                    report.request["idempotency_key"]
                        .as_str()
                        .unwrap()
                        .contains(&id)
                );
            }
            *outcomes
                .entry(check["observed_result"].as_str().unwrap().to_owned())
                .or_default() += 1;
        }
        assert!(
            report.receipt["observed_checks"]
                .as_array()
                .unwrap()
                .iter()
                .all(|check| check["credit"] == "uncredited")
        );
    };
    collect(&first);
    for _ in 0..54 {
        let next = fixture.observation();
        collect(&next);
    }
    assert_eq!(by_id.len(), 70);
    assert_eq!(outcomes.get("passed"), Some(&35));
    assert_eq!(outcomes.get("failed"), Some(&35));
    assert_eq!(fixture.producer.records.lock().unwrap().len(), 55);
    fixture.assert_no_granted_activity();
}

#[test]
fn lost_reply_resends_the_identical_persisted_request_and_accounts_once() {
    let mut fixture = ReportingTurn::new("unadmitted-lost", true, ReplyMode::LoseFirst, false);
    fixture
        .turn
        .change_readme("edited before an ambiguous producer reply\n");
    fixture.finish();
    let first = fixture.observation();
    let retry = fixture.observation();
    assert!(first.durable_before_send && retry.durable_before_send);
    assert_eq!(first.request, retry.request);
    assert_eq!(first.receipt, retry.receipt);
    assert_eq!(fixture.producer.records.lock().unwrap().len(), 1);
    fixture.assert_no_granted_activity();
}

#[test]
fn restart_before_reply_replays_the_same_report_from_real_session_storage() {
    let mut fixture = ReportingTurn::new("unadmitted-restart", true, ReplyMode::LoseFirst, true);
    let state = fixture.turn.state.clone();
    let producer = fixture.producer.clone();
    let context = fixture.reader.context.clone();
    let mut frames = std::mem::replace(
        &mut fixture.reader.frames,
        ClaudeReaderFrames::new(&context, mpsc::channel().0, None),
    );
    let mut recorder = SessionRecorder::new(state.clone(), fixture.turn.session_id.clone());
    let (closed, closing) = mpsc::channel();
    let close = std::thread::spawn(move || {
        let outcome = apply_claude_frame(&context, &mut frames, &mut recorder, &result(false));
        closed.send(outcome.next).unwrap();
    });
    let first = fixture.observation();
    assert!(first.durable_before_send);
    assert_eq!(
        closing.recv_timeout(DEADLOCK_GUARD).unwrap(),
        ClaudeFrameApplied::Continue,
        "closing the runtime turn must not wait on the producer reply"
    );
    close.join().unwrap();
    // Stop the old host's background workers by their supported shutdown
    // signal. Keep its SQLite file and immutable producer replay map.
    state.trigger_shutdown_signal();
    state.shutdown_persist_blocking();
    assert_eq!(
        *state.runtime_observation_lifetime.installs.lock().unwrap(),
        0
    );
    assert!(state.persist_thread_handle.lock().unwrap().is_none());
    let restarted = restart_reporting_state(&fixture.turn.root, &state, producer.clone());
    install_reporting_source_capture(&restarted);
    let restarting = restarted.clone();
    let boot = std::thread::spawn(move || restarting.run_post_listen_boot());
    producer.release();
    let replay = fixture.observation();
    assert_eq!(first.request, replay.request);
    assert_eq!(first.receipt, replay.receipt);
    assert!(replay.durable_before_send);
    assert_eq!(producer.records.lock().unwrap().len(), 1);
    boot.join().unwrap().unwrap();
    restarted.shutdown_persist_blocking();
}

#[tokio::test]
async fn definitive_refusal_names_its_code_without_holding_or_retrying_the_turn() {
    let mut fixture = ReportingTurn::new("unadmitted-refusal", true, ReplyMode::Refuse, false);
    let mut deltas = fixture.turn.state.subscribe_delta_events();
    let (idle, idle_rx) = mpsc::channel();
    *fixture
        .turn
        .state
        .runtime_observation_lifetime
        .install_hook
        .lock()
        .unwrap() = Some(Arc::new(move |phase| {
        if phase == "idle" {
            let _ = idle.send(());
        }
    }));
    fixture.finish();
    fixture.observation();
    let reason = tokio::time::timeout(DEADLOCK_GUARD, async {
        loop {
            let delta = deltas.recv().await.unwrap();
            if delta.contains("execution_observation_policy_basis_mismatch") {
                break delta;
            }
        }
    })
    .await
    .expect("a definitive refusal must become a visible diagnostic");
    assert!(reason.contains("execution_observation_policy_basis_mismatch"));
    idle_rx.recv_timeout(DEADLOCK_GUARD).unwrap();
    assert_eq!(fixture.producer.requests().len(), 1);
    assert_eq!(
        fixture.turn.record(|record| record.session.status),
        SessionStatus::Idle
    );
    fixture.assert_no_granted_activity();
}

/// An idempotency conflict is a terminal identity collision: Engram compared
/// the stored intent before any write, so this report was rejected unapplied,
/// and an earlier, different operation holds its key. It settles as refused
/// with the conflict code and its key, is told as an identity collision (not
/// recorded, the earlier operation not known), is stored durably in the
/// session row, and is never sent again under its key or a new one.
#[tokio::test]
async fn an_identity_collision_is_terminal_disclosed_and_not_sent_again() {
    let mut fixture = ReportingTurn::new("unadmitted-conflict", true, ReplyMode::Conflict, false);
    let mut deltas = fixture.turn.state.subscribe_delta_events();
    let (idle, idle_rx) = mpsc::channel();
    *fixture
        .turn
        .state
        .runtime_observation_lifetime
        .install_hook
        .lock()
        .unwrap() = Some(Arc::new(move |phase| {
        if phase == "idle" {
            let _ = idle.send(());
        }
    }));
    fixture.finish();
    fixture.observation();
    let reason = tokio::time::timeout(DEADLOCK_GUARD, async {
        loop {
            let delta = deltas.recv().await.unwrap();
            if delta.contains("identity collision") {
                break delta;
            }
        }
    })
    .await
    .expect("an identity collision must become a visible diagnostic");
    assert!(reason.contains("control_operation_idempotency_conflict"), "{reason}");
    assert!(reason.contains("not recorded"), "{reason}");
    assert!(reason.contains("earlier operation is not known"), "{reason}");
    idle_rx.recv_timeout(DEADLOCK_GUARD).unwrap();
    assert_eq!(fixture.producer.requests().len(), 1, "never sent again");
    let report = fixture.turn.record(|record| record.engram.unadmitted_reports[0].clone());
    assert!(
        report.envelopes.iter().any(|envelope| matches!(
            &envelope.delivery,
            EngramRuntimeObservationDelivery::Refused { code, message }
                if code == "control_operation_idempotency_conflict" && message.starts_with("key ")
        )),
        "the envelope settles as a refused collision naming its key: {:?}",
        report.envelopes
    );
    assert!(
        runtime_observation_report_is_durable(
            &fixture.turn.state.persistence_path,
            &fixture.turn.session_id,
            &report
        )
        .unwrap(),
        "the settled collision is stored in the session row"
    );
    fixture.assert_no_granted_activity();

    // A host restarted from that store loads the settled report and never
    // schedules it again.
    let state = fixture.turn.state.clone();
    state.trigger_shutdown_signal();
    state.shutdown_persist_blocking();
    let restarted = restart_reporting_state(&fixture.turn.root, &state, fixture.producer.clone());
    install_reporting_source_capture(&restarted);
    let restarting = restarted.clone();
    std::thread::spawn(move || restarting.run_post_listen_boot())
        .join()
        .unwrap()
        .unwrap();
    restarted.start_runtime_observation_worker(&fixture.turn.session_id);
    {
        let inner = restarted.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.turn.session_id).unwrap()];
        let loaded = &record.engram.unadmitted_reports;
        assert_eq!(loaded.len(), 1, "{loaded:?}");
        assert!(loaded[0].is_settled(), "{loaded:?}");
        assert!(
            loaded[0].envelopes.iter().any(|envelope| matches!(
                &envelope.delivery,
                EngramRuntimeObservationDelivery::Refused { code, .. }
                    if code == ENGRAM_IDEMPOTENCY_CONFLICT_CODE
            )),
            "{loaded:?}"
        );
        assert!(!record.engram.unadmitted_worker_running, "nothing is left to deliver");
    }
    assert_eq!(fixture.producer.requests().len(), 1, "never sent again after a restart");
    restarted.trigger_shutdown_signal();
    restarted.shutdown_persist_blocking();
}

/// The write that would settle a collision fails while the process lives:
/// the envelope is settled in memory, so nothing is sent again, and the store
/// still holds the pending report without the collision. Only a host
/// restarted from that unsettled store sends the report once more, as the
/// identical request under the same key, which Engram rejects again
/// unapplied; that settlement is then stored.
#[test]
fn a_collision_whose_settlement_is_not_stored_is_never_resent_live_and_replays_identically_after_restart() {
    let mut fixture = ReportingTurn::new("unadmitted-conflict-unstored", true, ReplyMode::Conflict, true);
    fixture.finish();
    let first = fixture.observation();
    assert!(first.durable_before_send);
    // The producer holds its answer: the stored image is the pending report.
    let pending = fixture.turn.record(|record| record.engram.unadmitted_reports[0].clone());
    let path = fixture.turn.state.persistence_path.as_ref().clone();
    let session_id = fixture.turn.session_id.clone();
    assert!(runtime_observation_report_is_durable(&path, &session_id, &pending).unwrap());
    // Fail exactly the write that would store the collision.
    let connection = rusqlite::Connection::open(&path).expect("the store opens");
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER fail_observation_collision BEFORE UPDATE ON sessions
             WHEN NEW.id = '{session_id}'
               AND instr(NEW.value_json, '{ENGRAM_IDEMPOTENCY_CONFLICT_CODE}') > 0
             BEGIN SELECT RAISE(ABORT, 'fixture collision settlement failure'); END;"
        ))
        .expect("the trigger installs");
    let (idle, idle_rx) = mpsc::channel();
    *fixture
        .turn
        .state
        .runtime_observation_lifetime
        .install_hook
        .lock()
        .unwrap() = Some(Arc::new(move |phase| {
        if phase == "idle" {
            let _ = idle.send(());
        }
    }));
    fixture.producer.release();
    idle_rx.recv_timeout(DEADLOCK_GUARD).unwrap();

    // Settled in memory, so the live worker sends nothing again.
    let settled = fixture.turn.record(|record| record.engram.unadmitted_reports[0].clone());
    assert!(settled.is_settled(), "{settled:?}");
    fixture.turn.state.start_runtime_observation_worker(&session_id);
    assert_eq!(fixture.producer.requests().len(), 1, "never sent again while the process lives");
    // The store is unsettled: the pending report, no collision.
    assert!(runtime_observation_report_is_durable(&path, &session_id, &pending).unwrap());
    assert!(!runtime_observation_report_is_durable(&path, &session_id, &settled).unwrap());

    // A host restarted from that store sends the identical request once more.
    let state = fixture.turn.state.clone();
    state.trigger_shutdown_signal();
    state.shutdown_persist_blocking();
    connection
        .execute_batch("DROP TRIGGER fail_observation_collision")
        .expect("the trigger drops");
    drop(connection);
    let restarted = restart_reporting_state(&fixture.turn.root, &state, fixture.producer.clone());
    install_reporting_source_capture(&restarted);
    let (idle, idle_rx) = mpsc::channel();
    *restarted.runtime_observation_lifetime.install_hook.lock().unwrap() =
        Some(Arc::new(move |phase| {
            if phase == "idle" {
                let _ = idle.send(());
            }
        }));
    let restarting = restarted.clone();
    let boot = std::thread::spawn(move || restarting.run_post_listen_boot());
    let replay = fixture.observation();
    assert_eq!(replay.request, first.request, "the identical request under the same key");
    assert!(replay.durable_before_send);
    boot.join().unwrap().unwrap();
    let stored = loop {
        idle_rx.recv_timeout(DEADLOCK_GUARD).unwrap();
        let report = {
            let inner = restarted.inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&session_id).unwrap()]
                .engram
                .unadmitted_reports[0]
                .clone()
        };
        if report.is_settled() {
            break report;
        }
    };
    assert!(
        stored.envelopes.iter().any(|envelope| matches!(
            &envelope.delivery,
            EngramRuntimeObservationDelivery::Refused { code, .. }
                if code == ENGRAM_IDEMPOTENCY_CONFLICT_CODE
        )),
        "{stored:?}"
    );
    assert_eq!(fixture.producer.requests().len(), 2);
    // Shutting down drains the restarted host's writer, then the settlement
    // is read back from the store.
    restarted.trigger_shutdown_signal();
    restarted.shutdown_persist_blocking();
    assert!(runtime_observation_report_is_durable(&path, &session_id, &stored).unwrap());
}

/// Each answer to one retained envelope settles it by the Engram contract:
/// only `execution_observe`'s four documented refusals, which record
/// nothing, are final; an idempotency conflict is a terminal identity
/// collision naming its key; any other error, Remote or not, stays pending
/// for the identical replay.
#[test]
fn each_failed_delivery_settles_by_the_execution_observe_contract() {
    let remote = |code: &str| {
        EngramTransportError::remote(EngramControlErrorBody {
            code: code.to_owned(),
            message: format!("Engram answered {code}"),
        })
    };
    for code in [
        "invalid_request",
        "execution_observation_invalid",
        "execution_observation_basis_mismatch",
        "execution_observation_policy_basis_mismatch",
    ] {
        match engram_runtime_observation_failure_delivery(remote(code), "report-key") {
            EngramRuntimeObservationDelivery::Refused { code: refused, .. } => {
                assert_eq!(refused, code)
            }
            other => panic!("{code} records nothing and is final: {other:?}"),
        }
    }
    match engram_runtime_observation_failure_delivery(
        remote("control_operation_idempotency_conflict"),
        "report-key",
    ) {
        EngramRuntimeObservationDelivery::Refused { code, message } => {
            assert_eq!(code, ENGRAM_IDEMPOTENCY_CONFLICT_CODE);
            assert!(message.starts_with("key report-key"), "{message}");
        }
        other => panic!("a conflict is a terminal identity collision: {other:?}"),
    }
    let mut codeless = remote("ignored");
    codeless.code = None;
    for error in [
        remote("storage_error"),
        remote("a_code_from_a_later_engram"),
        remote("control_store_busy"),
        // Certified not applied: this invocation wrote nothing, but an
        // earlier one may have; replayed, never refused.
        remote("control_request_expired"),
        codeless,
        EngramTransportError::transport("the control process exited"),
        EngramTransportError::deadline("no answer within the call bound"),
    ] {
        let label = format!("{:?} {:?}", error.kind, error.code);
        assert!(
            matches!(
                engram_runtime_observation_failure_delivery(error, "report-key"),
                EngramRuntimeObservationDelivery::Pending { .. }
            ),
            "{label}"
        );
    }
}

/// A Remote code that proves nothing about what Engram recorded, a storage
/// error or a code this build does not know, is not a refusal: the identical
/// request is sent again.
#[test]
fn an_undocumented_remote_error_replays_the_same_request_instead_of_refusing_it() {
    for code in ["storage_error", "a_code_from_a_later_engram"] {
        let mut fixture = ReportingTurn::new(
            &format!("unadmitted-unresolved-{code}"),
            true,
            ReplyMode::TransientFirst(code),
            false,
        );
        fixture.finish();
        let first = fixture.observation();
        let retry = fixture.observation();
        assert_eq!(first.request, retry.request, "retry after {code} keeps every request byte");
        assert_eq!(fixture.producer.records.lock().unwrap().len(), 1);
        assert!(
            !fixture
                .turn
                .record(system_notices)
                .iter()
                .any(|notice| notice.contains("Engram refused")),
            "{code}"
        );
        fixture.assert_no_granted_activity();
    }
}

#[test]
fn no_claimed_binding_sends_nothing_and_the_notice_explains_that() {
    let mut fixture = ReportingTurn::new("unadmitted-unbound", false, ReplyMode::Recorded, false);
    fixture.check("unbound-check", false);
    fixture.finish();
    assert!(fixture.producer.requests().is_empty());
    let notices = fixture.turn.record(system_notices);
    assert!(
        notices
            .iter()
            .any(|notice| notice.contains("not reported") && notice.contains("Engram")),
        "{notices:?}"
    );
    fixture.assert_no_granted_activity();
}

#[test]
fn a_leftover_grant_is_never_used_to_report_the_observed_turn() {
    let mut fixture = ReportingTurn::new("unadmitted-leftover", true, ReplyMode::Recorded, false);
    fixture
        .turn
        .record_mut(|record| record.engram.active_grant_id = Some("leftover-grant".to_owned()));
    fixture.check("unowned-check", false);
    fixture.finish();
    let observed = fixture.observation();
    assert!(!observed.request.to_string().contains("leftover-grant"));
    assert_eq!(
        fixture
            .turn
            .record(|record| record.engram.active_grant_id.clone())
            .as_deref(),
        Some("leftover-grant")
    );
    fixture.assert_no_granted_activity();
}

#[test]
fn duplicate_result_never_opens_or_accounts_a_second_observed_turn() {
    let mut fixture = ReportingTurn::new("unadmitted-duplicate", true, ReplyMode::Recorded, false);
    fixture.finish();
    let first = fixture.observation();
    fixture.reader.feed(result(false));
    assert_eq!(fixture.producer.records.lock().unwrap().len(), 1);
    assert_eq!(occurrence(&first)["kind"], "unadmitted_turn");
    fixture.assert_no_granted_activity();
}

#[test]
fn stale_runtime_result_does_not_finalize_the_adopted_turn_or_send_observations() {
    let fixture = ReportingTurn::new("unadmitted-stale", true, ReplyMode::Recorded, false);
    let generation = fixture.turn.record(|record| record.active_turn_generation);
    fixture
        .turn
        .state
        .finish_turn_ok_if_runtime_and_generation_match(
            &fixture.turn.session_id,
            &RuntimeToken::Claude("another-runtime".to_owned()),
            generation,
        )
        .unwrap();
    assert_eq!(
        fixture.turn.record(|record| record.session.status),
        SessionStatus::Active
    );
    assert!(fixture.producer.requests().is_empty());
    fixture.assert_no_granted_activity();
}

#[test]
fn end_binding_is_captured_without_borrowing_the_previous_turns_scope() {
    let mut fixture =
        ReportingTurn::new("unadmitted-end-binding", true, ReplyMode::Recorded, false);
    let binding = test_control_work_binding("binding-at-unadmitted-end", 2);
    fixture
        .turn
        .record_mut(|record| record.engram.work_binding = Some(binding.clone()));
    fixture
        .turn
        .transport
        .enable_named_roots(&fixture.turn.session_id, &binding);
    *fixture.producer.binding.write().unwrap() = Some(binding.clone());
    fixture.finish();
    let observed = fixture.observation();
    assert_eq!(
        observed.request["binding"],
        serde_json::to_value(binding).unwrap()
    );
    assert_eq!(
        occurrence(&observed)["source_change"]["detection"],
        "assumed_missing_baseline",
        "the old binding's checkpoint must never become this binding's baseline"
    );
    fixture.assert_no_granted_activity();
}

#[test]
fn ordinary_mediated_checks_keep_their_existing_granted_verification_path() {
    let turn = CheckedTurn::start("unadmitted-mediated-control", true);
    turn.run(
        "mediated-check",
        SIZE_TEST,
        EngramCommandExit::Code(0),
        || {},
    );
    let checkpoint = turn.finish();
    assert_eq!(
        checkpoint["verification_evidence"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        turn.transport
            .requests()
            .iter()
            .all(|request| request.request["occurrence"]["kind"] != "unadmitted_turn")
    );
}

#[test]
fn earlier_baseline_envelope_keeps_the_existing_out_of_window_protocol_rejection() {
    let basis = EngramUnadmittedObservationBasis {
        idempotency_key: "termal-observed:interval-control".to_owned(),
        binding: test_control_work_binding("interval-control", 1),
        root_basis: EngramObservationRootBasis {
            capture_run_cut: 1,
            latest_event: None,
            state: EngramObservationRootState::None {},
        },
        observed_interval: EngramObservationInterval {
            from: "2026-10-18T10:00:00Z".to_owned(),
            through: "2026-10-18T10:02:00Z".to_owned(),
        },
        policy_basis: EngramObservationPolicyBasis::AccountIfEligible {
            project_policy_epoch: 1,
            policy: "a".repeat(32),
            obligation_rule_set: "b".repeat(32),
        },
    };
    let change = EngramObservedSourceChange::ContentComparison {
        workspace_id: "workspace-interval".to_owned(),
        baseline: EngramMeasuredBaseline {
            workspace_id: "workspace-interval".to_owned(),
            source_revision: "content-v1:before".to_owned(),
            observed_at: "2026-10-18T10:00:00Z".to_owned(),
        },
        sighting: EngramMeasuredSighting {
            source_basis: EngramExecutionSourceBasis {
                workspace_id: "workspace-interval".to_owned(),
                source_revision: "content-v1:after".to_owned(),
                source_root_generation: None,
                source_root_state: None,
            },
            observed_at: "2026-10-18T10:02:00Z".to_owned(),
        },
    };
    let built_at = chrono::DateTime::parse_from_rfc3339("2026-10-18T10:03:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let request = engram_unadmitted_turn_request(
        "fixture-routing",
        basis.clone(),
        "interval-control".to_owned(),
        Some(change.clone()),
        Vec::new(),
        built_at,
    )
    .unwrap();
    let wire = serde_json::to_value(&request).unwrap();
    assert!(wire.get("observation").is_none());
    assert_eq!(wire["occurrence"]["kind"], "unadmitted_turn");
    assert_eq!(
        wire["binding"],
        serde_json::to_value(&basis.binding).unwrap()
    );
    assert_eq!(wire["observed_interval"]["from"], "2026-10-18T10:00:00Z");
    let replay: EngramControlRequest = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(replay).unwrap(), wire);
    let mut excluding_the_baseline = basis;
    excluding_the_baseline.observed_interval.from = "2026-10-18T10:01:00Z".to_owned();
    let error = engram_unadmitted_turn_request(
        "fixture-routing",
        excluding_the_baseline,
        "interval-control".to_owned(),
        Some(change),
        Vec::new(),
        built_at,
    )
    .unwrap_err();
    assert!(
        error
            .message
            .contains("source_change.baseline lies outside observed_interval"),
        "the unchanged producer bound still rejects a truly external timestamp: {}",
        error.message
    );
}

#[test]
fn retryable_remote_responses_replay_the_same_request_instead_of_refusing_it() {
    for code in [
        "control_store_busy",
        "control_request_expired",
        "control_unavailable",
        "control_backoff",
    ] {
        let mut fixture = ReportingTurn::new(
            &format!("unadmitted-transient-{code}"),
            true,
            ReplyMode::TransientFirst(code),
            false,
        );
        fixture.finish();
        let first = fixture.observation();
        let retry = fixture.observation();
        assert!(first.durable_before_send && retry.durable_before_send);
        assert_eq!(
            first.request, retry.request,
            "retry after {code} keeps every request byte"
        );
        assert_eq!(fixture.producer.records.lock().unwrap().len(), 1);
        assert!(
            !fixture
                .turn
                .record(system_notices)
                .iter()
                .any(|notice| notice.contains("Engram refused"))
        );
        fixture.assert_no_granted_activity();
    }
}

#[test]
fn a_previous_turns_nested_check_never_acquires_the_next_adopted_turn_identity() {
    let mut fixture =
        ReportingTurn::new("unadmitted-nested-origin", true, ReplyMode::Recorded, false);
    fixture.reader.feed(root_task("prior-task"));
    fixture.finish();
    let prior = fixture.observation();
    for frame in [
        task_notification(),
        init(),
        assistant_text("A different background notice opened this turn."),
    ] {
        fixture.reader.feed(frame);
    }
    fixture
        .reader
        .feed(nested_bash("foreign-nested-test", "prior-task"));
    fixture
        .reader
        .feed(nested_bash_result("foreign-nested-test", "prior-task"));
    fixture.check("current-root-test", false);
    let current_ref = fixture.turn.record(|record| {
        record
            .engram
            .unadmitted_active_turn
            .as_ref()
            .unwrap()
            .host_turn_ref
            .clone()
    });
    let current_key = test_private_command_key(&fixture.reader.recorder, "current-root-test");
    fixture.finish();
    let current = fixture.observation();
    assert_ne!(
        occurrence(&prior)["host_turn_ref"],
        occurrence(&current)["host_turn_ref"]
    );
    let checks = occurrence(&current)["observed_checks"].as_array().unwrap();
    assert_eq!(
        checks.len(),
        1,
        "a previous task's nested frame must not become this turn's check"
    );
    assert_eq!(
        checks[0]["host_check_id"],
        unadmitted_host_check_id(&current_ref, &current_key)
    );
    assert_eq!(
        current.receipt["observed_checks"][0]["credit"],
        "uncredited"
    );
    fixture.assert_no_granted_activity();
}

#[test]
fn owned_exit_reader_failure_and_stop_close_and_deliver_the_adopted_trace() {
    for terminal in [
        "runtime-exit",
        "reader-failure",
        "missing-runtime",
        "user-stop",
    ] {
        let mut fixture = ReportingTurn::new(
            &format!("unadmitted-{terminal}"),
            true,
            ReplyMode::Recorded,
            false,
        );
        fixture.check("observed-before-terminal", false);
        let expected = fixture.turn.record(|record| {
            record
                .engram
                .unadmitted_active_turn
                .as_ref()
                .unwrap()
                .host_turn_ref
                .clone()
        });
        let token = fixture
            .turn
            .record(|record| record.runtime.runtime_token().unwrap());
        match terminal {
            "runtime-exit" => fixture
                .turn
                .state
                .handle_runtime_exit_if_matches(
                    &fixture.turn.session_id,
                    &token,
                    Some("fixture runtime exited"),
                )
                .unwrap(),
            "reader-failure" => fixture
                .turn
                .state
                .fail_turn_if_runtime_matches(
                    &fixture.turn.session_id,
                    &token,
                    "fixture reader failed",
                )
                .unwrap(),
            "missing-runtime" => {
                let generation = fixture.turn.record(|record| record.active_turn_generation);
                fixture.turn.record_mut(|record| record.clear_runtime());
                fixture
                    .turn
                    .state
                    .fail_active_turn_if_runtime_missing(
                        &fixture.turn.session_id,
                        generation,
                        "fixture runtime missing",
                    )
                    .unwrap();
            }
            "user-stop" => {
                fixture
                    .turn
                    .state
                    .stop_session(&fixture.turn.session_id)
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let observed = fixture.observation();
        assert_eq!(occurrence(&observed)["host_turn_ref"], expected);
        assert_eq!(
            occurrence(&observed)["observed_checks"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(observed.durable_before_send);
        assert!(
            fixture
                .turn
                .record(|record| record.engram.unadmitted_active_turn.is_none())
        );
        fixture.assert_no_granted_reporting();
    }
}
