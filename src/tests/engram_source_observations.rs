//! Ordinary execution-observation custody, request preparation and replay.
//! These fixtures exercise host owners with synthetic facts and replies;
//! they do not certify that a native command ran or create a checkpoint.
use super::*;
use crate::tests::*;

fn observation_at(second: u32) -> String {
    format!("2026-10-11T00:00:{second:02}Z")
}

fn observation_policy() -> EngramObservationPolicyBasis {
    serde_json::from_value(json!({
        "mode": "account_if_eligible", "project_policy_epoch": 3,
        "policy": "a".repeat(32), "obligation_rule_set": "b".repeat(64)
    })).unwrap()
}

fn observation_binding() -> EngramControlWorkBinding {
    EngramControlWorkBinding {
        root_execution_id: "observation-root".into(), work_id: "observation-work".into(),
        run_id: "observation-run".into(), claim_id: "observation-claim".into(),
        work_revision: 3, claim_fence: 4,
    }
}

fn observation_source(revision: &str) -> EngramExecutionSourceBasis {
    EngramExecutionSourceBasis {
        workspace_id: "observation-workspace".into(), source_revision: revision.into(),
        source_root_generation: Some(2), source_root_state: Some(EngramSourceRootState::Named),
    }
}

fn observation_trace() -> EngramRuntimeObservedTurn {
    let mut trace = EngramRuntimeObservedTurn::started("observed-prompt".into(), 7);
    trace.host_turn_ref = "ordinary-turn-fixture".into();
    trace.adopted_at = observation_at(10);
    trace.observed_outcome = Some(EngramObservedTurnOutcome::Succeeded);
    trace
}

fn observation_capture() -> EngramRuntimeClosingCapture {
    EngramRuntimeClosingCapture {
        binding: observation_binding(),
        root_basis: serde_json::from_value(json!({
            "capture_run_cut": 11, "latest_event": "c".repeat(32),
            "state": {"state": "bound", "workspace_id": "observation-workspace",
                "generation": 2, "named_at": observation_at(0)}
        })).unwrap(),
        sighting: EngramMeasuredSighting {
            source_basis: observation_source("content-v1:closing"), observed_at: observation_at(50),
        },
        baseline: None,
        registered_check_baselines: Vec::new(),
        policy: observation_policy(),
    }
}

// A passive imported endpoint specimen. Ordinary turns never manufacture
// this value, and successful reporting must not install it as a checkpoint.
fn historical_observation_endpoint() -> EngramRuntimeCheckpointBasis {
    EngramRuntimeCheckpointBasis {
        binding: observation_binding(),
        endpoint: EngramMeasuredBaseline {
            workspace_id: "observation-workspace".into(),
            source_revision: "content-v1:imported".into(), observed_at: observation_at(5),
        },
        source_root_generation: Some(2),
        source_root_state: Some(EngramSourceRootState::Named),
    }
}

fn observation_check(id: &str) -> EngramObservedCheck {
    EngramObservedCheck {
        host_check_id: id.into(), check_kind: EngramVerificationKind::Test,
        observed_result: EngramObservedCheckResult::Passed,
        started_at: Some(observation_at(20)), finished_at: Some(observation_at(30)),
        observed_at: observation_at(30), source_basis: Some(observation_source("content-v1:closing")),
        host_evidence_ref: Some("synthetic-check-fixture".into()),
    }
}

fn observation_command(id: &str) -> EngramRuntimeObservedCommand {
    EngramRuntimeObservedCommand {
        tool_key: id.into(), simple: true, recognized_command: None,
        launch_only: false, invocation_ended: true, declared: None,
        check: observation_check(id),
    }
}

fn observation_connection(root: &FsPath) -> EngramConnectionConfig {
    EngramConnectionConfig {
        binary_path: root.join("unused-fixture-binary"), project_file: root.join(".engram-project"),
        home: root.join("unused-fixture-home"), project_root: root.to_owned(),
        actor_id: "fixture-actor".into(), actor_context: None, session_id: "fixture-observer".into(),
    }
}

fn observation_store(root: &FsPath) -> EngramAuthorityStoreKey {
    EngramAuthorityStoreKey { database_path: root.join("unused-engram.sqlite"), project_id: "fixture-project".into() }
}

fn observation_report(root: &FsPath) -> EngramRuntimeObservationReport {
    EngramRuntimeObservationReport::prepare(
        &observation_trace(), observation_capture(), observation_connection(root),
        observation_store(root), "fixture-observer".into(), Duration::from_secs(1),
    ).unwrap()
}

fn report_observation(report: &EngramRuntimeObservationReport, index: usize) -> EngramInterTurnObservation {
    match serde_json::from_value::<EngramControlRequest>(report.envelopes[index].request.clone()).unwrap() {
        EngramControlRequest::ExecutionObserve { observation, .. } => observation,
        _ => panic!("ordinary report must contain execution_observe"),
    }
}

fn observation_receipt(request: &EngramInterTurnObservation, observer: &str) -> Value {
    let (accounting, checks) = match &request.occurrence {
        EngramInterTurnOccurrence::UnadmittedTurn { source_change, observed_checks, .. } => (
            if source_change.is_some() { json!({"kind": "source_change", "source_change": "d".repeat(32)}) }
            else { json!({"kind": "no_source_change"}) },
            observed_checks.iter().map(|check| json!({"host_check_id": check.host_check_id, "credit": "uncredited"})).collect::<Vec<_>>(),
        ),
        EngramInterTurnOccurrence::ObservedCheck { check, .. } => (
            json!({"kind": "no_source_change"}), vec![json!({"host_check_id": check.host_check_id, "credit": "uncredited"})],
        ),
        EngramInterTurnOccurrence::SourceSighting { .. } => (json!({"kind": "sighting"}), Vec::new()),
        _ => panic!("not an ordinary report occurrence"),
    };
    json!({
        "decision": "recorded", "observation": "d".repeat(32),
        "position": {"feed": {"kind": "run_execution", "id": request.binding.run_id}, "position": 12},
        "observing_session": observer, "binding": request.binding, "admission": "unadmitted",
        "causality": request.causality, "policy_basis": request.policy_basis,
        "accounting": accounting, "observed_checks": checks, "opened_obligations": [],
        "recorded_at": observation_at(59)
    })
}

#[derive(Default)]
struct ObservationReplies {
    sent: Mutex<Vec<(EngramConnectionConfig, Value)>>,
    replies: Mutex<VecDeque<Result<Value, EngramTransportError>>>,
    policy_error: Mutex<Option<EngramTransportError>>,
    durable_scope: Mutex<Option<(PathBuf, String)>>,
}

impl EngramControlTransport for ObservationReplies {
    fn request(&self, connection: &EngramConnectionConfig, request: &EngramControlRequest, _timeout: Duration)
        -> Result<Value, EngramTransportError> {
        let wire = serde_json::to_value(request).unwrap();
        if let Some((path, session_id)) = self.durable_scope.lock().unwrap().as_ref() {
            let connection = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            let row: String = connection.query_row("SELECT value_json FROM sessions WHERE id = ?1", [session_id], |row| row.get(0)).unwrap();
            let row: Value = serde_json::from_str(&row).unwrap();
            assert!(row["engramUnadmittedReports"].as_array().unwrap().iter().any(|report|
                report["envelopes"].as_array().unwrap().iter().any(|envelope| envelope["request"] == wire)),
                "the actual worker RPC must carry a request already present in committed SQLite");
        }
        self.sent.lock().unwrap().push((connection.clone(), wire));
        self.replies.lock().unwrap().pop_front().expect("unexpected observation RPC")
    }
    fn read_observation_policy(&self, _connection: &EngramConnectionConfig, _store: &EngramAuthorityStoreKey,
        _timeout: Duration, _host_workdir: &FsPath) -> Result<EngramObservationPolicyBasis, EngramTransportError> {
        if let Some(error) = self.policy_error.lock().unwrap().take() { return Err(error); }
        Ok(observation_policy())
    }
    fn shutdown_session(&self, _session_id: &str) {}
}

fn observation_adapter(transport: Arc<ObservationReplies>, root: &FsPath) -> EngramHostAdapter {
    EngramHostAdapter { host_workdir: root.to_owned(), transport, capabilities: Arc::default() }
}

struct ObservationSession {
    state: AppState,
    id: String,
}

impl ObservationSession {
    fn new() -> Self {
        let state = test_app_state();
        let id = {
            let mut inner = state.inner.lock().unwrap();
            inner.create_session(Agent::Codex, Some("Observation fixture".into()),
                state.default_workdir.clone(), None, None).session.id.clone()
        };
        Self { state, id }
    }
    fn record<T>(&self, f: impl FnOnce(&mut SessionRecord) -> T) -> T {
        let mut inner = self.state.inner.lock().unwrap();
        let index = inner.find_session_index(&self.id).unwrap();
        let result = f(&mut inner.sessions[index]);
        inner.stamp_session_at_index(index);
        result
    }
    fn report(&self) -> EngramRuntimeObservationReport {
        observation_report(FsPath::new(&self.state.default_workdir))
    }
    fn retain(&self, report: EngramRuntimeObservationReport) {
        self.record(|record| record.engram.unadmitted_reports.push(report));
    }
    fn persist(&self) {
        let inner = self.state.inner.lock().unwrap();
        persist_locked_state_to_sqlite(self.state.persistence_path.as_ref(), &inner).unwrap();
    }
    fn removal_target(&self) -> PersistFenceTarget {
        let inner = self.state.inner.lock().unwrap();
        let record = PersistedSessionRecord::from_record(&inner.sessions[inner.find_session_index(&self.id).unwrap()]);
        PersistFenceTarget::EngramObservationRemoval {
            session_id: self.id.clone(), content: source_observation_removal_content(&record),
        }
    }
}

fn recorded_report(report: &EngramRuntimeObservationReport) -> EngramRuntimeObservationReport {
    let mut next = report.next_version();
    for index in 0..next.envelopes.len() {
        next.envelopes[index].delivery = EngramRuntimeObservationDelivery::Recorded {
            receipt: observation_receipt(&report_observation(report, index), &report.observing_session),
        };
    }
    next
}

#[test]
fn ordinary_observation_missing_baseline_stays_missing_after_preparation() {
    let trace = observation_trace();
    let capture = observation_capture();
    let requests = prepare_runtime_observations(&trace, capture.clone(), chrono::Utc::now()).unwrap();
    let request: EngramControlRequest = serde_json::from_value(requests[0].clone()).unwrap();
    let EngramControlRequest::ExecutionObserve { observation, .. } = request else { panic!() };
    assert!(matches!(observation.occurrence, EngramInterTurnOccurrence::UnadmittedTurn {
        source_change: Some(EngramObservedSourceChange::AssumedMissingBaseline { .. }), ..
    }));
    assert!(capture.baseline.is_none());
    assert_eq!(requests.len(), 2, "a separate measured sighting is retained");
}

#[test]
fn ordinary_observation_imported_endpoint_requires_exact_claim_and_workspace() {
    for incompatible in ["claim", "workspace"] {
        let mut capture = observation_capture();
        let mut imported = historical_observation_endpoint();
        if incompatible == "claim" { imported.binding.claim_fence += 1; }
        else { imported.endpoint.workspace_id = "other-workspace".into(); }
        capture.baseline = Some(imported);
        assert!(matches!(capture.source_change(), Some(EngramObservedSourceChange::AssumedMissingBaseline { .. })));
    }
    let mut capture = observation_capture();
    let imported = historical_observation_endpoint();
    capture.baseline = Some(imported.clone());
    assert!(matches!(capture.source_change(), Some(EngramObservedSourceChange::ContentComparison { baseline, .. })
        if baseline == imported.endpoint));
    capture.sighting.source_basis.source_revision = imported.endpoint.source_revision;
    assert!(capture.source_change().is_none());
}

#[test]
fn ordinary_observation_imported_endpoint_keeps_its_original_timestamp() {
    let mut capture = observation_capture();
    capture.baseline = Some(historical_observation_endpoint());
    let requests = prepare_runtime_observations(&observation_trace(), capture, chrono::Utc::now()).unwrap();
    let request: EngramControlRequest = serde_json::from_value(requests[0].clone()).unwrap();
    let EngramControlRequest::ExecutionObserve { observation, .. } = request else { panic!() };
    assert_eq!(observation.observed_interval.from, "2026-10-11T00:00:05.000Z");
    let EngramInterTurnOccurrence::UnadmittedTurn { source_change: Some(EngramObservedSourceChange::ContentComparison { baseline, .. }), .. }
        = observation.occurrence else { panic!() };
    assert_eq!(baseline.observed_at, observation_at(5));
}

#[test]
fn ordinary_observation_rejects_reversed_and_future_closing_cuts() {
    for second in [9, 59] {
        let mut capture = observation_capture();
        capture.sighting.observed_at = observation_at(second);
        let built_at = engram_observation_instant(&observation_at(55), "fixture").unwrap();
        assert!(prepare_runtime_observations(&observation_trace(), capture, built_at).is_err());
    }
}

#[test]
fn ordinary_observation_uncertain_source_keeps_checks_without_inventing_measurements() {
    let mut trace = observation_trace();
    trace.source_attribution_uncertain = true;
    trace.commands.push(observation_command("check-one"));
    let requests = prepare_runtime_observations(&trace, observation_capture(), chrono::Utc::now()).unwrap();
    assert_eq!(requests.len(), 1, "uncertain attribution withholds the source sighting too");
    let request: EngramControlRequest = serde_json::from_value(requests[0].clone()).unwrap();
    let EngramControlRequest::ExecutionObserve { observation, .. } = request else { panic!() };
    let EngramInterTurnOccurrence::UnadmittedTurn { source_change, observed_checks, observed_outcome, .. }
        = observation.occurrence else { panic!() };
    assert!(source_change.is_none());
    assert_eq!(observed_checks, vec![observation_check("check-one")]);
    assert_eq!(observed_outcome, Some(EngramObservedTurnOutcome::Succeeded));
}

#[test]
fn ordinary_observation_splits_check_overflow_without_losing_occurrence_identity() {
    let mut trace = observation_trace();
    trace.commands = (0..18).map(|index| observation_command(&format!("check-{index}"))).collect();
    let first = prepare_runtime_observations(&trace, observation_capture(), chrono::Utc::now()).unwrap();
    let replay = prepare_runtime_observations(&trace, observation_capture(), chrono::Utc::now()).unwrap();
    assert_eq!(first, replay);
    assert_eq!(first.len(), 4, "one turn, two overflow checks, one sighting");
    let report = EngramRuntimeObservationReport { envelopes: first.iter().cloned().map(|request|
        EngramRuntimeObservationEnvelope { request, delivery: EngramRuntimeObservationDelivery::Pending { last_error: None } }
    ).collect(), ..observation_report(FsPath::new(".")) };
    let mut ids = Vec::new();
    let mut keys = std::collections::BTreeSet::new();
    for index in 0..report.envelopes.len() {
        let observation = report_observation(&report, index);
        assert!(keys.insert(observation.idempotency_key));
        match observation.occurrence {
            EngramInterTurnOccurrence::UnadmittedTurn { observed_checks, .. } => ids.extend(observed_checks.into_iter().map(|check| check.host_check_id)),
            EngramInterTurnOccurrence::ObservedCheck { check, .. } => ids.push(check.host_check_id),
            EngramInterTurnOccurrence::SourceSighting { .. } => {},
            _ => panic!(),
        }
    }
    assert_eq!(ids, trace.commands.iter().map(|entry| entry.check.host_check_id.clone()).collect::<Vec<_>>());
}

#[test]
fn ordinary_observation_uncertain_reply_replays_exact_original_request_and_connection() {
    let fixture = ObservationSession::new();
    let report = fixture.report();
    let transport = Arc::new(ObservationReplies::default());
    let receipt = observation_receipt(&report_observation(&report, 0), &report.observing_session);
    transport.replies.lock().unwrap().extend([
        Err(EngramTransportError::transport("reply lost after remote recording")), Ok(receipt.clone()),
    ]);
    let adapter = observation_adapter(transport.clone(), FsPath::new(&fixture.state.default_workdir));
    let pending = deliver_runtime_observation_envelope(&adapter, &report, 0, Duration::from_secs(1)).unwrap();
    assert!(matches!(pending.envelopes[0].delivery, EngramRuntimeObservationDelivery::Pending { last_error: Some(_) }));
    let recorded = deliver_runtime_observation_envelope(&adapter, &pending, 0, Duration::from_secs(1)).unwrap();
    assert!(matches!(&recorded.envelopes[0].delivery, EngramRuntimeObservationDelivery::Recorded { receipt: value } if value == &receipt));
    assert_eq!(recorded.envelopes[0].request, report.envelopes[0].request);
    let sent = transport.sent.lock().unwrap();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0], sent[1]);
    assert_eq!(sent[0].0, report.connection);
}

#[test]
fn ordinary_observation_policy_unavailable_keeps_the_request_without_sending() {
    let fixture = ObservationSession::new();
    let report = fixture.report();
    let transport = Arc::new(ObservationReplies::default());
    *transport.policy_error.lock().unwrap() = Some(EngramTransportError::protocol("policy disabled"));
    let adapter = observation_adapter(transport.clone(), FsPath::new(&fixture.state.default_workdir));
    let next = deliver_runtime_observation_envelope(&adapter, &report, 0, Duration::from_secs(1)).unwrap();
    assert!(matches!(next.envelopes[0].delivery, EngramRuntimeObservationDelivery::LocalFailure { .. }));
    assert_eq!(next.envelopes[0].request, report.envelopes[0].request);
    assert!(transport.sent.lock().unwrap().is_empty());
}

#[test]
fn ordinary_observation_typed_refusal_and_identity_collision_do_not_become_retries() {
    for code in ["execution_observation_policy_basis_mismatch", "control_operation_idempotency_conflict"] {
        let delivery = engram_runtime_observation_failure_delivery(EngramTransportError::remote(
            EngramControlErrorBody { code: code.into(), message: "rejected".into() }
        ), "original-key");
        let EngramRuntimeObservationDelivery::Refused { code: actual, message } = delivery else { panic!() };
        assert_eq!(actual, code);
        if code == "control_operation_idempotency_conflict" { assert!(message.contains("original-key")); }
    }
    let unknown = engram_runtime_observation_failure_delivery(EngramTransportError::transport("unknown outcome"), "key");
    assert!(matches!(unknown, EngramRuntimeObservationDelivery::Pending { .. }));
}

#[test]
fn ordinary_observation_receipt_settlement_never_creates_a_checkpoint() {
    let fixture = ObservationSession::new();
    let report = fixture.report();
    fixture.retain(report.clone());
    fixture.state.install_runtime_observation_result(&fixture.id, &report, recorded_report(&report), 0).unwrap();
    fixture.record(|record| {
        assert!(record.engram.runtime_checkpoint.is_none());
        assert!(record.engram.unadmitted_reports[0].is_settled());
    });
    fixture.persist();
    let restored = load_state(fixture.state.persistence_path.as_ref()).unwrap().unwrap();
    let record = &restored.sessions[restored.find_session_index(&fixture.id).unwrap()];
    assert!(record.engram.runtime_checkpoint.is_none());
    assert!(record.engram.unadmitted_reports[0].is_settled());
}

#[test]
fn ordinary_observation_old_settlement_preserves_imported_endpoint_and_successor() {
    let fixture = ObservationSession::new();
    let report = fixture.report();
    let endpoint = historical_observation_endpoint();
    let mut successor = observation_trace();
    successor.host_turn_ref = "successor-turn".into();
    successor.generation += 1;
    fixture.record(|record| {
        record.engram.runtime_checkpoint = Some(endpoint.clone());
        record.engram.work_binding = Some(EngramControlWorkBinding { claim_fence: 99, ..observation_binding() });
        record.engram.unadmitted_active_turn = Some(successor.clone());
        record.active_turn_generation = successor.generation;
        record.session.preview = "successor preview".into();
        record.engram.unadmitted_reports.push(report.clone());
    });
    fixture.state.install_runtime_observation_result(&fixture.id, &report, recorded_report(&report), 0).unwrap();
    fixture.record(|record| {
        assert_eq!(record.engram.runtime_checkpoint, Some(endpoint));
        assert_eq!(record.engram.unadmitted_active_turn, Some(successor));
        assert_eq!(record.engram.work_binding.as_ref().unwrap().claim_fence, 99);
        assert_eq!(record.session.preview, "successor preview");
    });
}

#[test]
fn ordinary_observation_stale_result_cannot_replace_a_newer_report_version() {
    let fixture = ObservationSession::new();
    let old = fixture.report();
    let mut newer = old.next_version();
    newer.envelopes[0].delivery = EngramRuntimeObservationDelivery::Pending { last_error: Some("newer attempt".into()) };
    fixture.retain(newer.clone());
    fixture.state.install_runtime_observation_result(&fixture.id, &old, recorded_report(&old), 0).unwrap();
    fixture.record(|record| assert_eq!(record.engram.unadmitted_reports, vec![newer]));
}

#[test]
fn ordinary_observation_repeated_excursions_keep_distinct_turn_keys() {
    let first = observation_trace();
    let mut second = first.clone();
    second.host_turn_ref = "second-excursion".into();
    let a = prepare_runtime_observations(&first, observation_capture(), chrono::Utc::now()).unwrap();
    let b = prepare_runtime_observations(&second, observation_capture(), chrono::Utc::now()).unwrap();
    assert_ne!(a[0], b[0]);
    assert_ne!(a.last().unwrap(), b.last().unwrap());
    for wire in [&a[0], &b[0]] {
        let request: EngramControlRequest = serde_json::from_value(wire.clone()).unwrap();
        let EngramControlRequest::ExecutionObserve { observation, .. } = request else { panic!() };
        assert!(matches!(observation.occurrence, EngramInterTurnOccurrence::UnadmittedTurn {
            source_change: Some(EngramObservedSourceChange::AssumedMissingBaseline { .. }), ..
        }), "an earlier observation does not supply a comparison baseline");
    }
}

#[test]
fn ordinary_observation_check_owner_requires_current_trace_generation_and_claim() {
    let fixture = ObservationSession::new();
    let mut trace = observation_trace();
    trace.opening_root = Some(EngramRuntimeOpeningRoot {
        store: observation_store(FsPath::new(&fixture.state.default_workdir)),
        binding: observation_binding(), provenance: EngramRootCapture::Unconfirmed, source_root: None,
    });
    fixture.record(|record| {
        record.active_turn_generation = trace.generation;
        record.engram.work_binding = Some(observation_binding());
        record.engram.unadmitted_active_turn = Some(trace.clone());
        assert_eq!(runtime_observation_check_owner(record), Some(trace.host_turn_ref.as_str()));
        record.engram.work_binding.as_mut().unwrap().claim_fence += 1;
        assert!(runtime_observation_check_owner(record).is_none());
        record.engram.work_binding = Some(observation_binding());
        record.active_turn_generation += 1;
        assert!(runtime_observation_check_owner(record).is_none());
        mark_runtime_observation_source_uncertain(record);
        assert!(!record.engram.unadmitted_active_turn.as_ref().unwrap().source_attribution_uncertain);
        record.active_turn_generation = trace.generation;
        mark_runtime_observation_source_uncertain(record);
        assert!(record.engram.unadmitted_active_turn.as_ref().unwrap().source_attribution_uncertain);
        assert!(record.engram.runtime_checkpoint.is_none());
    });
}

#[path = "engram_source_observation_audit.rs"]
mod audit;
#[path = "engram_source_observation_finalization.rs"]
mod finalization;
#[path = "engram_source_observation_delegation.rs"]
mod delegation;
#[path = "engram_source_observation_removal.rs"]
mod removal;
#[path = "engram_source_observation_delete_races.rs"]
mod delete_races;
#[path = "engram_ordinary_baseline.rs"]
mod ordinary_baseline;
