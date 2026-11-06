// Native verification sender and its ordinary work-session capability proof.
// Owns exact initiating connection/current-child isolation, explicit run
// registration, immutable record/key retries, producer receipt/refusal shapes,
// own-row durable settlement, reload, backoff and fair iteration. Automatic
// Execution Observability neither grants this credit nor disables explicit
// verification. Historical control metadata remains passive and unchanged.
// Does not own ordinary dispatch, runtime lifecycle or producer-side recording.
use super::*;

#[path = "engram_verification_record_rebind.rs"]
mod naming_rebind;

#[path = "test_run_receipt_record_ids.rs"]
mod receipt_record_ids;

#[path = "engram_verification_refusals.rs"]
mod refusals;

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const SESSION: &str = "session-record";
const STARTED: &str = "2026-10-30T10:00:00.000Z";
const ENDED: &str = "2026-10-30T10:05:00.000Z";
/// A `registered_host_run` object id as the producer mints it (32 hex).
const PRODUCER: &str = "5f0c2b6e8d4a4e1f9a3b7c6d5e4f3a2b";

/// Exact mailed producer-authored frames from source 7a812915/tree 88d3c539.
/// The registered check is asserted data, not an execution attestation. This
/// is the mailed JSON, not an independently fetched producer raw artifact.
const PRODUCER_WIRE: &str = include_str!("fixtures/engram-execution-observability-wire.json");

/// Current six-shape native fixture from producer d5bc14da/tree edb30f0e,
/// mailed unchanged. Its emitter passed within the registered-test subset;
/// the enclosing run failed later, so this artifact is no overall gate proof.
/// The older routing-token archive remains a separate, unchanged file.
const REGISTERED_PRODUCER_WIRE: &str =
    include_str!("fixtures/engram-registered-verification-current-wire.json");

/// One named part of the producer's wire.
fn producer_wire(part: &str) -> Value {
    let wire: Value = serde_json::from_str(REGISTERED_PRODUCER_WIRE).expect("the producer wire reads");
    wire.get(part).cloned().unwrap_or_else(|| panic!("the producer's wire has `{part}`"))
}

fn producer_frame(operation: &str) -> Value {
    let wire: Value = serde_json::from_str(PRODUCER_WIRE).expect("the producer's wire reads");
    wire["frames"].as_array().unwrap().iter()
        .find(|frame| frame["request"]["operation"] == operation)
        .unwrap_or_else(|| panic!("the producer captured {operation}"))
        .clone()
}

/// The producer's request as a host sends it.
fn producer_request() -> Value {
    producer_wire("request")
}

/// The producer's receipt for that request: the `result` of its answer.
fn producer_receipt() -> Value {
    producer_wire("response")["result"].clone()
}

/// The producer's refusal envelope.
fn producer_refusal() -> Value {
    producer_wire("refusal")
}

fn binding() -> EngramControlWorkBinding {
    EngramControlWorkBinding {
        root_execution_id: "root-record".to_owned(),
        work_id: "work-record".to_owned(),
        run_id: "run-record".to_owned(),
        work_revision: 2,
        claim_id: "claim-record".to_owned(),
        claim_fence: 4,
    }
}

fn store() -> EngramAuthorityStoreKey {
    EngramAuthorityStoreKey {
        database_path: PathBuf::from("C:/tracker.db"),
        project_id: "github.com/example/record".to_owned(),
    }
}

/// TermAl's own full gate, as the launcher plans it (`requiredStages`).
fn commit_request(root: &FsPath) -> Value {
    json!({
        "root": root.to_string_lossy(),
        "full": true,
        "input": "commit",
        "stages": [
            { "name": "cargo-check", "command": "cargo", "args": ["check"] },
            { "name": "typescript", "command": "node",
              "args": ["node_modules/typescript/bin/tsc", "--noEmit"], "cwd": "ui" },
            { "name": "fingerprint-tests", "command": "node",
              "args": ["--test", "--test-force-exit", "--test-timeout=60000", "scripts/a.test.mjs"] },
            { "name": "rust-tests", "command": "sh", "args": ["scripts/test-rust.sh"] },
            { "name": "vitest", "command": "node",
              "args": ["node_modules/vitest/vitest.mjs", "run"], "cwd": "ui" },
        ],
        "started": STARTED,
    })
}

fn stage(name: &str, state: &str, code: i64, command: &[&str], minute: u32) -> Value {
    json!({
        "name": name, "state": state, "code": code,
        "started": format!("2026-10-30T10:0{minute}:00.000Z"),
        "ended": format!("2026-10-30T10:0{minute}:30.000Z"),
        "command": command,
    })
}

/// The Node helper tests' actual argv as the launcher runs them: its
/// duration reporters come before `--test`.
const FINGERPRINT_TESTS_ARGV: [&str; 9] = [
    "node",
    "--test-reporter=spec",
    "--test-reporter-destination=stdout",
    "--test-reporter=file:///reporter.mjs",
    "--test-reporter-destination=durations.jsonl",
    "--test",
    "--test-force-exit",
    "--test-timeout=60000",
    "scripts/a.test.mjs",
];

const VITEST_ARGV: [&str; 6] = [
    "node",
    "node_modules/vitest/vitest.mjs",
    "run",
    "--reporter=default",
    "--reporter=json",
    "--outputFile.json=durations.json",
];

/// A failed full gate on a clean commit: four stages passed and Vitest
/// failed; the Node and Vitest stages ran argvs longer than the ones planned
/// (the launcher's duration-report flags), and the Rust stage's counts carry
/// the runner's label beside its numbers.
fn commit_results() -> Value {
    let mut rust_tests = stage("rust-tests", "passed", 0, &["sh", "scripts/test-rust.sh"], 3);
    rust_tests["tests"] = json!({ "runner": "cargo-libtest", "passed": 3, "failed": 0, "ignored": 0 });
    json!({
        "state": "failed",
        "exitCode": 1,
        "started": STARTED,
        "ended": ENDED,
        "stages": [
            stage("cargo-check", "passed", 0, &["cargo", "check"], 0),
            stage("typescript", "passed", 0, &["node", "node_modules/typescript/bin/tsc", "--noEmit"], 1),
            stage("fingerprint-tests", "passed", 0, &FINGERPRINT_TESTS_ARGV, 2),
            rust_tests,
            stage("vitest", "failed", 1, &VITEST_ARGV, 4),
        ],
        "input": {
            "kind": "commit", "commit": COMMIT, "cleanBefore": true,
            "after": { "commit": COMMIT, "clean": true, "paths": [] },
            "outcome": "clean", "eligible": true,
        },
    })
}

const STAGES: [&str; 5] = ["cargo-check", "typescript", "fingerprint-tests", "rust-tests", "vitest"];

struct RecordRun {
    run_dir: PathBuf,
    registration: TestRunRegistration,
}

/// A registered, bound run, as its registration records it.
fn record_run(fixture: &RunFixture, run_id: &str, request: Value, results: Value) -> RecordRun {
    let run_dir = write_run(&fixture.runs_dir(), run_id, request, Some(results));
    let TestRunJson::Parsed(written, _, _) = test_run_read_json(&run_dir.join("request.json")) else {
        panic!("the request reads back");
    };
    let registration = TestRunRegistration {
        run_id: run_id.to_owned(),
        native_origin: None,
        native_completion: None,
        project_id: Some(fixture.project_id.clone()),
        run_directory: test_run_display_path(&run_dir),
        locator_identity: None,
        worktree: test_run_display_path(&fixture.root),
        request_identity: test_run_registration_identity(&written)
            .ok()
            .expect("the request has an identity"),
        initiating_session_id: SESSION.to_owned(),
        owning_session_id: SESSION.to_owned(),
        creator_pid: None,
        registered_at: "2026-10-30T09:59:59.000Z".to_owned(),
        started_at: Some(STARTED.to_owned()),
        preset: TestRunPreset::Full,
        detached: Some(false),
        stage_names: STAGES.iter().map(|name| (*name).to_owned()).collect(),
        work_binding: Some(binding()),
        named_root: Some(TestRunNamedRoot {
            workspace_id: test_run_display_path(&fixture.root),
            generation: 3,
        }),
        authority_store: Some(store()),
        validated_unbound: false,
        monitoring_interrupted_at: None,
        verification_record: None,
    };
    RecordRun { run_dir, registration }
}

fn ready(registration: &TestRunRegistration) -> EngramVerificationRecordFrozen {
    match engram_verification_record_prepare(registration) {
        EngramVerificationRecordPrepared::Ready(frozen) => frozen,
        other => panic!("the run should be ready to send, not {other:?}"),
    }
}

fn pending_of(registration: &TestRunRegistration) -> EngramVerificationRecordPending {
    match engram_verification_record_prepare(registration) {
        EngramVerificationRecordPrepared::Pending(reason) => reason,
        other => panic!("the run should be pending, not {other:?}"),
    }
}

fn fingerprint(line: &str) -> String {
    sha256_hex(line.as_bytes())
}

fn connection(session_id: &str, actor_id: &str) -> EngramConnectionConfig {
    EngramConnectionConfig {
        binary_path: PathBuf::from("engram"),
        project_file: PathBuf::from(".engram-project"),
        home: PathBuf::from("home"),
        project_root: PathBuf::from("project"),
        actor_id: actor_id.to_owned(),
        actor_context: None,
        session_id: session_id.to_owned(),
    }
}

/// Answers each call with the next scripted reply and keeps every request's
/// exact bytes. `respawn` stands for the process transport starting a new
/// child for every connection; `observe` sees each request as it goes out.
#[derive(Default)]
struct RecordingTransport {
    replies: Mutex<VecDeque<std::result::Result<Value, EngramTransportError>>>,
    sent: Mutex<Vec<Vec<u8>>>,
    children: AtomicU64,
    observe: Mutex<Option<Box<dyn FnMut(&EngramControlRequest) + Send>>>,
    /// Each call's timeout, in call order.
    timeouts: Mutex<Vec<Duration>>,
    /// Set, the next call's child is replaced while the call is in flight.
    respawn_during_call: AtomicBool,
}

impl RecordingTransport {
    fn script(&self, reply: std::result::Result<Value, EngramTransportError>) {
        self.replies.lock().unwrap().push_back(reply);
    }

    fn respawn(&self) {
        self.children.fetch_add(1, Ordering::SeqCst);
    }

    fn sent(&self) -> Vec<Value> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .map(|bytes| serde_json::from_slice::<Value>(bytes).unwrap())
            .collect()
    }

    fn sent_operations(&self) -> Vec<String> {
        self.sent()
            .iter()
            .map(|request| request["operation"].as_str().unwrap().to_owned())
            .collect()
    }

    fn sent_records(&self) -> Vec<Value> {
        self.sent()
            .into_iter()
            .filter(|request| request["operation"] == "verification_record")
            .collect()
    }
}

impl EngramControlTransport for RecordingTransport {
    fn request(
        &self,
        _connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        self.timeouts.lock().unwrap().push(timeout);
        if let Some(observe) = self.observe.lock().unwrap().as_mut() {
            observe(request);
        }
        self.sent
            .lock()
            .unwrap()
            .push(serde_json::to_vec(request).expect("the request serializes"));
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(EngramTransportError::transport("no reply scripted")));
        if self.respawn_during_call.swap(false, Ordering::SeqCst) {
            self.respawn();
        }
        reply
    }

    fn shutdown_session(&self, _session_id: &str) {}

    fn connection_generation(&self, _session_id: &str) -> Option<u64> {
        Some(self.children.load(Ordering::SeqCst))
    }
}

fn adapter(transport: &Arc<RecordingTransport>) -> EngramHostAdapter {
    EngramHostAdapter {
        host_workdir: PathBuf::new(),
        transport: transport.clone(),
        capabilities: Arc::default(),
    }
}

fn work_session_with(capabilities: Value) -> Value {
    json!({ "project_id": store().project_id, "session_id": SESSION,
        "work_session": true, "capabilities": capabilities })
}

fn supported() -> Value {
    work_session_with(json!(["host_work_v1", "verification_record_v1"]))
}

fn work_supported(session: &str) -> Value {
    json!({"project_id": store().project_id, "session_id": session,
        "work_session": true, "capabilities": ["host_work_v1", "verification_record_v1"]})
}

/// The connection's ordinary work-session read, answering `answer`.
fn read_work_session(
    transport: &RecordingTransport,
    adapter: &EngramHostAdapter,
    connection: &EngramConnectionConfig,
    answer: Value,
) {
    transport.script(Ok(answer));
    adapter
        .request(connection, &EngramControlRequest::WorkSessionRead {}, Duration::from_secs(1))
        .expect("the work-session read answers");
}

fn link<'a>(
    adapter: &'a EngramHostAdapter,
    connection: &'a EngramConnectionConfig,
) -> std::result::Result<EngramVerificationRecordLink<'a>, EngramVerificationRecordPending> {
    Ok(EngramVerificationRecordLink {
        adapter,
        connection,
        timeout: Duration::from_secs(5),
    })
}

fn record_id(stage: usize) -> String {
    ["a", "b", "c", "d", "e"][stage].repeat(32)
}

/// The producer's receipt for the full gate: every stage completed, so each
/// has its record, in the producer's own shape.
fn receipt_for(run_outcome: &str) -> Value {
    json!({
        "operation": PRODUCER,
        "producer": PRODUCER,
        "verifications": STAGES
            .iter()
            .enumerate()
            .map(|(index, stage)| json!({ "stage": stage, "record_id": record_id(index) }))
            .collect::<Vec<_>>(),
        "run_outcome": run_outcome,
    })
}

/// Every current request byte, including its operation key, repeats exactly.
fn record_wire_bytes(request: &Value) -> Vec<u8> {
    assert!(request.get("routing_token").is_none());
    serde_json::to_vec(request).unwrap()
}

fn launch_row(run: &RecordRun) -> (String, TestRunCommandObservation) {
    let execution_id = format!("observed-{}", run.registration.run_id);
    let row = serde_json::from_value(json!({
        "executionId": execution_id,
        "runtimeIdentity": null,
        "toolIdentity": "Bash",
        "grantIdentity": null,
        "commandFingerprint": "f".repeat(64),
        "turnGeneration": 1,
        "observedAt": ENDED,
        "summary": {
            "runId": execution_id, "projectId": null, "worktree": run.registration.worktree,
            "runDir": run.registration.run_directory, "preset": "full", "command": null,
            "commandTruncated": false, "detached": null, "state": "failed", "interrupted": false,
            "currentStage": null, "stages": [], "ownerSessionId": run.registration.initiating_session_id, "notifyTo": null,
            "notifySessionId": null, "startedAt": STARTED, "endedAt": ENDED, "exitCode": 1,
            "error": null, "detailVersion": null,
        },
        "workBinding": null,
        "sourceRoot": null,
        "validatedUnbound": false,
        "artifact": {
            "runId": run.registration.run_id,
            "runDirectory": run.registration.run_directory,
            "requestIdentity": run.registration.request_identity,
            "terminalDigest": null,
        },
    }))
    .expect("an observation row");
    (execution_id, row)
}

/// The run's own launch observation, its artifact attached, as the host
/// records it once the launcher exits.
fn attach_launch_row(fixture: &RunFixture, run: &RecordRun) -> String {
    let (execution_id, row) = launch_row(run);
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_observations
        .insert(execution_id.clone(), row);
    execution_id
}

/// The records on the run's launch row, in association order.
fn row_records(fixture: &RunFixture, execution_id: &str) -> Vec<Option<String>> {
    let inner = fixture.state.inner.lock().unwrap();
    inner
        .test_run_observations
        .get(execution_id)
        .expect("the row stays")
        .evidence
        .iter()
        .map(|association| association.verification_record_id.clone())
        .collect()
}

/// Join any existing writer before installing a rejecting writer under the
/// AppState worker handle. Normal shutdown joins this writer as well.
fn failing_persist_worker(fixture: &mut RunFixture) {
    fixture.state.shutdown_persist_blocking();
    let (tx, rx) = std::sync::mpsc::channel::<PersistRequest>();
    fixture.state.persist_tx = tx;
    let worker = std::thread::spawn(move || {
        let mut batch = PersistFenceBatch::default();
        while let Ok(request) = rx.recv() {
            let shutdown = matches!(&request, PersistRequest::Shutdown);
            batch.accept(request);
            batch.fail(PersistFenceError::WriteFailed("disk full".to_owned()));
            if shutdown {
                break;
            }
        }
    });
    assert!(fixture.state.persist_thread_handle.lock().unwrap().replace(worker).is_none());
    fixture.state.persist_worker_alive.store(true, Ordering::Release);
}

/// Join the writer before exercising the host's synchronous persistence path.
fn synchronous_persistence(fixture: &mut RunFixture) {
    fixture.state.shutdown_persist_blocking();
}

/// An ordinary work session; automatic EO and explicit verification are separate.
fn engram_session(fixture: &RunFixture, observability: bool) -> (String, Arc<RecordingTransport>) {
    let session = create_test_project_session(&fixture.state, Agent::Codex, &fixture.project_id, &fixture.root);
    let transport = Arc::new(RecordingTransport::default());
    fixture.state.install_test_engram_transport(transport.clone());
    let mut inner = fixture.state.inner.lock().unwrap();
    inner
        .projects
        .iter_mut()
        .find(|project| project.id == fixture.project_id)
        .expect("the fixture's project")
        .engram = Some(EngramProjectSettings {
        acceptance_evaluation: None,
        enabled: true,
        execution_observability: observability,
        binary_path: Some("engram".to_owned()),
        home: Some(fixture.root.to_string_lossy().into_owned()),
        authority_store_key: Some(store()),
        deadline_ms: Some(1000),
    });
    drop(inner);
    (session, transport)
}

fn session_engram<R>(fixture: &RunFixture, session: &str, read: impl FnOnce(&EngramSessionState) -> R) -> R {
    let inner = fixture.state.inner.lock().unwrap();
    let index = inner.find_session_index(session).expect("the session");
    read(&inner.sessions[index].engram)
}

fn with_session_engram(fixture: &RunFixture, session: &str, change: impl FnOnce(&mut EngramSessionState)) {
    let mut inner = fixture.state.inner.lock().unwrap();
    let index = inner.find_session_index(session).expect("the session");
    change(&mut inner.sessions[index].engram);
}

/// A host reload of `session`: its record goes through the persisted form
/// and back, and the process memory (the adapter, its capabilities and every
/// runtime-only field) starts afresh.
fn reload_session(fixture: &RunFixture, session: &str, transport: &Arc<RecordingTransport>) {
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(session).expect("the session");
        let saved = serde_json::to_value(PersistedSessionRecord::from_record(&inner.sessions[index]))
            .expect("the record serializes");
        let loaded: PersistedSessionRecord = serde_json::from_value(saved).expect("the record loads");
        inner.sessions[index] = loaded.into_record().expect("the record reloads");
    }
    fixture.state.install_test_engram_transport(transport.clone());
}

fn initiated_by(run: &mut RecordRun, session: &str) {
    run.registration.initiating_session_id = session.to_owned();
    run.registration.owning_session_id = session.to_owned();
}

fn register(fixture: &RunFixture, runs: &[&RecordRun]) {
    let mut inner = fixture.state.inner.lock().unwrap();
    for run in runs {
        inner.test_run_registrations.insert(run.registration.run_id.clone(), run.registration.clone());
        inner.test_runs.registered.insert(run.registration.run_id.clone());
    }
}

/// The run's registration as it now stands in state, with what the host
/// itself observed of its record.
fn current(fixture: &RunFixture, run: &RecordRun) -> TestRunRegistration {
    fixture.state.inner.lock().unwrap().test_run_registrations[&run.registration.run_id].clone()
}

/// The session's binding state as source-root naming owns it: the sender
/// never changes any of it.
fn naming_state(
    fixture: &RunFixture,
    session: &str,
) -> (Option<EngramControlWorkBinding>, Option<Uuid>, BTreeMap<String, Value>) {
    session_engram(fixture, session, |engram| {
        (
            engram.work_binding.clone(),
            engram.source_root_bind_owner,
            engram.historical_control.clone(),
        )
    })
}

/// The work association retained by the ordinary source-root naming owner.
fn bound_by_naming(fixture: &RunFixture, session: &str) {
    with_session_engram(fixture, session, |engram| {
        engram.work_binding = Some(binding());
    });
}

fn sent_binds(transport: &RecordingTransport) -> Vec<Value> {
    transport
        .sent()
        .into_iter()
        .filter(|request| request["operation"] == "session_bind")
        .collect()
}

#[test]
fn the_capability_is_read_from_an_answer_only_when_listed_as_a_name() {
    use crate::EngramVerificationRecordSupport as Support;
    let cases = [
        (json!({ "phase": "ready", "capabilities": ["other_v1", "verification_record_v1"] }), Support::Supported),
        (json!({ "phase": "ready" }), Support::FieldMissing),
        (json!({ "phase": "ready", "capabilities": null }), Support::FieldMissing),
        (json!({ "phase": "ready", "capabilities": ["verification_record_v2"] }), Support::EntryMissing),
        (json!({ "phase": "ready", "capabilities": [] }), Support::EntryMissing),
        (json!({ "phase": "ready", "capabilities": "verification_record_v1" }), Support::Unparseable),
        (json!({ "phase": "ready", "capabilities": ["verification_record_v1", 7] }), Support::Unparseable),
        (json!({ "phase": "ready", "capabilities": { "verification_record_v1": true } }), Support::Unparseable),
    ];
    for (answer, expected) in cases {
        assert_eq!(engram_reply_verification_record_support(&answer), expected, "{answer}");
    }
}

#[test]
fn ordinary_work_session_support_requires_the_exact_session_and_host_capability() {
    use crate::EngramVerificationRecordSupport as Support;
    let transport = Arc::new(RecordingTransport::default());
    let adapter = adapter(&transport);
    let own = connection(SESSION, "actor");
    for (answer, expected) in [
        (supported(), Support::Supported),
        ({ let mut answer = supported(); answer["session_id"] = json!("another-session"); answer }, Support::NotRead),
        ({ let mut answer = supported(); answer["work_session"] = json!(false); answer }, Support::NotRead),
        (work_session_with(json!(["verification_record_v1"])), Support::NotRead),
        (work_session_with(json!(["host_work_v1"])), Support::EntryMissing),
        (work_session_with(json!(["host_work_v1", "verification_record_v1", false])), Support::NotRead),
    ] {
        read_work_session(&transport, &adapter, &own, answer.clone());
        assert_eq!(adapter.verification_record_route_support(&own), expected, "{answer}");
    }
    assert!(transport.sent().iter().all(|request|
        request["operation"] == "work_session_read" && request.get("routing_token").is_none()));
}

#[test]
fn each_connection_keeps_only_its_current_childs_ordinary_work_session_answer() {
    use crate::EngramVerificationRecordSupport as Support;
    let transport = Arc::new(RecordingTransport::default());
    let adapter = adapter(&transport);
    let own = connection(SESSION, "actor");
    assert_eq!(adapter.verification_record_route_support(&own), Support::NotRead);
    read_work_session(&transport, &adapter, &own, supported());
    assert_eq!(adapter.verification_record_route_support(&own), Support::Supported);
    assert_eq!(adapter.verification_record_route_support(&connection("other", "actor")), Support::NotRead);
    assert_eq!(adapter.verification_record_route_support(&connection(SESSION, "reader")), Support::NotRead);
    let mut another_home = own.clone();
    another_home.home = PathBuf::from("a-different-connection-home");
    assert_eq!(adapter.verification_record_route_support(&another_home), Support::NotRead);

    // A root read carrying a capability-looking extension cannot change proof.
    transport.script(Ok(json!({ "capabilities": [] })));
    adapter.request(&own, &EngramControlRequest::NamedRootRead {
        run_id: "run-record".to_owned(), claim_id: "claim-record".to_owned(),
    }, Duration::from_secs(1)).unwrap();
    assert_eq!(adapter.verification_record_route_support(&own), Support::Supported);

    transport.respawn();
    assert_eq!(adapter.verification_record_route_support(&own), Support::NotRead);
    read_work_session(&transport, &adapter, &own, supported());
    assert_eq!(adapter.verification_record_route_support(&own), Support::Supported);
    read_work_session(&transport, &adapter, &own, work_session_with(json!(["host_work_v1"])));
    assert_eq!(adapter.verification_record_route_support(&own), Support::EntryMissing,
        "a fresh answer replaces the old capability");

    read_work_session(&transport, &adapter, &own, supported());
    transport.script(Err(EngramTransportError::transport("lost work-session reply")));
    assert!(adapter.request(&own, &EngramControlRequest::WorkSessionRead {}, Duration::from_secs(1)).is_err());
    assert_eq!(adapter.verification_record_route_support(&own), Support::NotRead);
    read_work_session(&transport, &adapter, &own, supported());
    adapter.shutdown_session(SESSION);
    assert_eq!(adapter.verification_record_route_support(&own), Support::NotRead);
    assert!(transport.sent().iter().all(|request| request.get("routing_token").is_none()));
}

/// The run the producer's fixture request describes, as TermAl records it:
/// an asserted focused check on a clean commit, registered with the
/// fixture's identity, claim and root. This constructs input data only.
fn producer_fixture_run(fixture: &RunFixture, producer: &Value) -> (TestRunRegistration, Value, Value) {
    let registration = &producer["registration"];
    let run_part = &producer["run"];
    let check = &producer["checks"][0];
    let text = |value: &Value| value.as_str().expect("a producer string").to_owned();
    let commit = text(&producer["input"]["commit"]);
    // The launcher's own stage counts: the producer's numbers plus the
    // runner label the sender must leave out.
    let mut tests = Value::Object(check["counts"].as_object().cloned().unwrap_or_default());
    tests["runner"] = json!("cargo-libtest");
    let argv = match check["command"].as_str().unwrap() {
        "fixture registered check" => ["fixture", "registered", "check"],
        "cargo test --lib" => ["cargo", "test", "--lib"],
        other => panic!("a new producer command needs an explicit argv fixture: {other}"),
    };
    assert_eq!(check["command"], json!(argv.join(" ")));
    let request = json!({
        "root": fixture.root.to_string_lossy(),
        "input": "commit",
        "stages": [{ "name": text(&check["stage"]), "kind": "test", "command": argv[0], "args": &argv[1..] }],
        "started": text(&run_part["started_at"]),
    });
    let results = json!({
        "state": text(&run_part["outcome"]),
        "exitCode": 0,
        "started": text(&run_part["started_at"]),
        "ended": text(&run_part["finished_at"]),
        "command": argv,
        "stages": [{
            "name": text(&check["stage"]), "state": text(&check["outcome"]), "code": 0,
            "started": text(&check["started_at"]), "ended": text(&check["completed_at"]),
            "command": argv,
            "tests": tests,
        }],
        "input": {
            "kind": "commit", "commit": commit, "cleanBefore": true,
            "after": { "commit": commit, "clean": true, "paths": [] },
            "outcome": "clean", "eligible": true,
        },
    });
    let run = record_run(fixture, &text(&registration["host_run_id"]), request.clone(), results.clone());
    let registration = TestRunRegistration {
        request_identity: registration["request_identity"].as_str().unwrap().to_owned(),
        native_origin: None,
        native_completion: None,
        registered_at: registration["registered_at"].as_str().unwrap().to_owned(),
        initiating_session_id: registration["initiating_session_id"].as_str().unwrap().to_owned(),
        owning_session_id: registration["owning_session_id"].as_str()
            .or_else(|| registration["initiating_session_id"].as_str()).unwrap().to_owned(),
        work_binding: Some(serde_json::from_value(registration["binding"].clone()).unwrap()),
        named_root: Some(TestRunNamedRoot {
            workspace_id: registration["root"]["workspace_id"].as_str().unwrap().to_owned(),
            generation: registration["root"]["generation"].as_u64().unwrap(),
        }),
        ..run.registration
    };
    (registration, request, results)
}

/// Optional host audit text: owning session, check summaries and references.
/// An empty optional dirty-path list is equivalent to its omitted wire form.
fn without_host_text(mut request: Value) -> Value {
    request["registration"].as_object_mut().unwrap().remove("owning_session_id");
    for check in request["checks"].as_array_mut().unwrap() {
        let check = check.as_object_mut().unwrap();
        check.remove("summary");
        check.remove("refs");
    }
    if let Some(post_run) = request.get_mut("post_run").and_then(Value::as_object_mut)
        && post_run.get("dirty_paths").is_some_and(|paths|
            paths.as_array().is_some_and(|paths| paths.is_empty()))
    {
        post_run.remove("dirty_paths");
    }
    request
}

#[test]
fn the_record_matches_the_producers_own_request_fixture() {
    for producer in [producer_request(), producer_frame("verification_record")["request"].clone()] {
    // The sender's types read the producer's shape as it stands.
    let mut body = producer.as_object().unwrap().clone();
    assert!(producer.get("routing_token").is_none());
    for framing in ["operation", "operation_key"] {
        body.remove(framing);
    }
    let read: EngramVerificationRecordBody =
        serde_json::from_value(Value::Object(body)).expect("the producer's request reads as a record");
    assert_eq!(read.checks[0].check_kind, EngramVerificationRecordCheckKind::Test);

    // The sender builds the same request from the same run.
    let fixture = RunFixture::new("verification-record-producer-request");
    let (registration, request, results) = producer_fixture_run(&fixture, &producer);
    let built = engram_verification_record_body(&registration, &request, &results).expect("the run builds");
    let wire = serde_json::to_value(EngramControlRequest::VerificationRecord {
        operation_key: engram_verification_record_key(&registration.run_id),
        record: serde_json::to_value(&built).unwrap(),
    })
    .unwrap();
    assert_eq!(without_host_text(wire), without_host_text(producer.clone()));
    // The check fingerprint is the producer's own over the command text.
    assert_eq!(built.checks[0].check_fingerprint, fingerprint(producer["checks"][0]["command"].as_str().unwrap()));
    assert_eq!(built.registration.owning_session_id.as_deref(), Some(registration.owning_session_id.as_str()));
    }
}

#[test]
fn the_producers_quiet_sighting_is_neither_a_turn_nor_verification_credit() {
    let frame = producer_frame("execution_observe");
    let mut body = frame["request"].as_object().unwrap().clone();
    body.remove("operation");
    let observation: EngramInterTurnObservation =
        serde_json::from_value(Value::Object(body)).unwrap();
    let sighting = EngramMeasuredSighting {
        source_basis: serde_json::from_value(frame["request"]["occurrence"]["source_basis"].clone()).unwrap(),
        observed_at: frame["request"]["occurrence"]["observed_at"].as_str().unwrap().to_owned(),
    };
    let built_at = chrono::DateTime::parse_from_rfc3339(&sighting.observed_at)
        .unwrap().with_timezone(&chrono::Utc);
    let request = engram_source_sighting_request(EngramUnadmittedObservationBasis {
        idempotency_key: observation.idempotency_key.clone(),
        binding: observation.binding.clone(),
        root_basis: observation.root_basis.clone(),
        observed_interval: observation.observed_interval.clone(),
        policy_basis: observation.policy_basis.clone(),
    }, sighting, built_at).unwrap();
    assert_eq!(serde_json::to_value(&request).unwrap(), frame["request"]);
    assert!(frame["request"].get("routing_token").is_none());
    assert!(frame["request"]["occurrence"].get("host_turn_ref").is_none());
    let session = frame["response"]["result"]["observing_session"].as_str().unwrap();
    let check = |receipt| EngramSourceObservationReceipt::from_sighting_result(
        receipt, &observation, session,
    );
    let receipt = frame["response"]["result"].clone();
    assert_eq!(receipt["accounting"], json!({"kind": "sighting"}));
    assert_eq!(receipt["observed_checks"], json!([]));
    assert_eq!(receipt["opened_obligations"], json!([]));
    assert!(check(receipt.clone()).is_ok());
    for accounting in [
        json!({"kind": "source_change", "source_change": receipt["observation"]}),
        json!({"kind": "no_source_change"}),
    ] {
        let mut wrong = receipt.clone();
        wrong["accounting"] = accounting;
        assert!(check(wrong).is_err(), "a sighting cannot become source-change accounting");
    }

    // The shared scripted producer must make the same distinction and replay
    // the exact saved receipt, rather than treating an unknown occurrence as
    // an inter-turn source change.
    let wire: Value = serde_json::from_str(PRODUCER_WIRE).unwrap();
    let root_read = wire["frames"].as_array().unwrap().iter().find(|frame| {
        frame["request"]["operation"] == "named_root_read"
            && frame["response"]["result"]["named_root"]["state"] == "bound"
    }).unwrap()["response"]["result"].clone();
    let mut model = TestEngramNamedRoots::default();
    model.register(&observation.binding);
    model.bindings.insert(session.to_owned(), observation.binding.claim_id.clone());
    model.root_reads.insert(observation.binding.claim_id.clone(), root_read);
    model.run_cuts.insert(observation.binding.run_id.clone(), 3);
    let modeled = model.request(session, &request).unwrap().unwrap();
    assert_eq!(modeled["accounting"], receipt["accounting"]);
    assert_eq!(modeled["observed_checks"], json!([]));
    assert!(check(modeled.clone()).is_ok());
    assert_eq!(model.request(session, &request).unwrap().unwrap(), modeled);
    let mut changed = request.clone();
    let EngramControlRequest::ExecutionObserve { observation, .. } = &mut changed else { unreachable!() };
    let EngramInterTurnOccurrence::SourceSighting { source_basis, .. } = &mut observation.occurrence else { unreachable!() };
    source_basis.source_revision.push_str("-changed");
    assert!(model.request(session, &changed).unwrap().is_err());
}

#[test]
fn the_producers_receipt_and_refusal_fixtures_settle_and_reject() {
    let producer = producer_request();
    let fixture = RunFixture::new("verification-record-producer-receipt");
    let (registration, request, results) = producer_fixture_run(&fixture, &producer);
    let body = engram_verification_record_body(&registration, &request, &results).expect("the run builds");
    let receipt: EngramVerificationRecordReceipt =
        serde_json::from_value(producer_receipt()).expect("the producer's receipt reads");
    assert_eq!(engram_verification_record_receipt_check(&body, &receipt), Ok(()));

    // The current producer's failed payload names only its completed check;
    // no token normalization is needed to read this exact request body.
    let failed_wire = producer_wire("failed_request");
    assert!(failed_wire.get("routing_token").is_none());
    let mut failed = failed_wire.as_object().unwrap().clone();
    for framing in ["operation", "operation_key"] {
        failed.remove(framing);
    }
    let failed_body: EngramVerificationRecordBody =
        serde_json::from_value(Value::Object(failed)).expect("the producer's failed request reads as a record");
    let failed_receipt: EngramVerificationRecordReceipt =
        serde_json::from_value(producer_wire("failed_response")["result"].clone())
            .expect("the producer's failed receipt reads");
    assert_eq!(failed_body.checks.len(), 2);
    assert_eq!(failed_receipt.verifications.len(), 1);
    assert_eq!(engram_verification_record_receipt_check(&failed_body, &failed_receipt), Ok(()));

    // The producer's refused request reads as a record too.
    let refused_wire = producer_wire("refused_request");
    assert!(refused_wire.get("routing_token").is_none());
    let mut refused = refused_wire.as_object().unwrap().clone();
    for framing in ["operation", "operation_key"] {
        refused.remove(framing);
    }
    serde_json::from_value::<EngramVerificationRecordBody>(Value::Object(refused))
        .expect("the producer's refused request reads as a record");

    // Sent and answered with the producer's receipt, the record settles.
    let frozen = EngramVerificationRecordFrozen {
        run_dir: fixture.runs_dir().join("test-native"),
        run_id: "test-native".to_owned(),
        record: serde_json::to_value(&body).unwrap(),
        body: body.clone(),
    };
    let transport = Arc::new(RecordingTransport::default());
    let adapter = adapter(&transport);
    let own = connection(SESSION, "actor");
    read_work_session(&transport, &adapter, &own, supported());
    transport.script(Ok(producer_receipt()));
    let step = engram_verification_record_send(frozen.clone(), link(&adapter, &own));
    assert!(matches!(step, EngramVerificationRecordStep::Received { .. }), "{step:?}");
    assert_eq!(transport.sent_records()[0]["operation_key"], "termal-run:test-native");
    assert!(transport.sent_records()[0].get("idempotency_key").is_none());

    // The producer's refusal, as its envelope carries it, is not sent again.
    let EngramControlResponse::Error { error } =
        serde_json::from_value::<EngramControlResponse>(producer_refusal()).expect("the producer's refusal reads")
    else {
        panic!("the refusal is an error envelope");
    };
    transport.script(Err(EngramTransportError::remote(error.into_body())));
    let step = engram_verification_record_send(frozen, link(&adapter, &own));
    assert!(
        matches!(&step, EngramVerificationRecordStep::Rejected(detail) if detail.contains("verification_record_refused")),
        "{step:?}"
    );
}

#[test]
fn a_committed_input_full_gate_records_every_stage_in_the_producers_shape() {
    let fixture = RunFixture::new("verification-record-full-gate");
    let run = record_run(&fixture, "test-record-full-gate", commit_request(&fixture.root), commit_results());
    let frozen = ready(&run.registration);
    let record = &frozen.record;
    assert_eq!(record["run"]["mode"], "full");
    // The launcher records no command of its own: reconstructed, as text.
    assert_eq!(record["run"]["command"], "node scripts/test-launcher.mjs full --input commit");
    assert_eq!(record["run"]["outcome"], "failed");
    assert_eq!(record["input"], json!({ "kind": "commit", "commit": COMMIT }));
    assert_eq!(
        record["registration"]["root"],
        json!({ "workspace_id": test_run_display_path(&fixture.root), "generation": 3, "state": "named" })
    );
    assert_eq!(
        record["post_run"],
        json!({ "outcome": "clean", "clean_before": true, "clean_after": true })
    );
    let expected = [
        ("cargo-check", "build", "cargo check".to_owned()),
        ("typescript", "build", "node node_modules/typescript/bin/tsc --noEmit".to_owned()),
        ("fingerprint-tests", "test", FINGERPRINT_TESTS_ARGV.join(" ")),
        ("rust-tests", "test", "sh scripts/test-rust.sh".to_owned()),
        ("vitest", "test", VITEST_ARGV.join(" ")),
    ];
    let checks = record["checks"].as_array().unwrap();
    assert_eq!(checks.len(), expected.len());
    for (check, (stage, kind, command)) in checks.iter().zip(&expected) {
        assert_eq!(check["stage"], *stage);
        assert_eq!(check["check_kind"], *kind);
        // The actual argv as text, and the fingerprint over exactly that.
        assert_eq!(check["command"], command.as_str());
        assert_eq!(check["check_fingerprint"], fingerprint(command));
        assert!(check["refs"].as_array().is_some_and(|refs| !refs.is_empty()));
    }
    // Numeric counts only: the runner's label is dropped.
    assert_eq!(checks[3]["counts"], json!({ "failed": 0, "ignored": 0, "passed": 3 }));
    assert!(checks[0].get("counts").is_none());
    assert_eq!(checks[4]["outcome"], "failed");
    assert_ne!(checks[4]["check_fingerprint"], fingerprint("node node_modules/vitest/vitest.mjs run"));

    // The complete tokenless envelope uses the frozen record and stable key.
    let wire = serde_json::to_value(engram_verification_record_request(&frozen)).unwrap();
    assert_eq!(wire["operation"], "verification_record");
    assert!(wire.get("routing_token").is_none());
    assert_eq!(wire["operation_key"], "termal-run:test-record-full-gate");
    assert!(frozen.record.get("routing_token").is_none());
    let persisted = fs::read_to_string(run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE)).unwrap();
    assert!(!persisted.contains("routing_token") && !persisted.contains("token-1"));
}

#[test]
fn counts_keep_only_numeric_entries_within_the_producers_bound() {
    let counts = |tests: Value| engram_verification_record_counts(&json!({ "tests": tests }));
    assert_eq!(
        counts(json!({ "runner": "cargo-libtest", "passed": 3, "failed": 1 })),
        Some(BTreeMap::from([("failed".to_owned(), 1), ("passed".to_owned(), 3)]))
    );
    assert_eq!(counts(json!({ "runner": "node", "nested": { "passed": 1 }, "ratio": 0.5, "negative": -1 })), None);
    let many: serde_json::Map<String, Value> = (0..33).map(|index| (format!("k{index}"), json!(index))).collect();
    assert_eq!(counts(Value::Object(many)), None, "more keys than the producer admits");
    assert_eq!(engram_verification_record_counts(&json!({})), None);
}

#[test]
fn runs_the_producer_would_refuse_stay_pending_with_their_reason() {
    let fixture = RunFixture::new("verification-record-producer-refusals");
    // A checkout left with debris, or changed, is not admitted yet.
    for (label, outcome, after) in [
        ("debris", TestRunCommittedInputOutcome::DebrisPendingDisposition,
         json!({ "commit": COMMIT, "clean": false, "paths": [{ "status": "??", "path": "debris.txt" }] })),
        ("changed", TestRunCommittedInputOutcome::InputChanged,
         json!({ "commit": COMMIT, "clean": false, "paths": [{ "status": " M", "path": "src/main.rs" }] })),
    ] {
        let mut results = commit_results();
        results["input"]["after"] = after;
        results["input"]["outcome"] = serde_json::to_value(outcome).unwrap();
        results["input"]["eligible"] = json!(false);
        let run = record_run(&fixture, &format!("test-record-{label}"), commit_request(&fixture.root), results);
        assert_eq!(pending_of(&run.registration), EngramVerificationRecordPending::PostRunNotClean { outcome });
        assert!(!run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE).exists());
    }
    // A stage recorded as starting before the registration: never adjusted.
    let mut results = commit_results();
    results["stages"][1]["started"] = json!("2026-10-30T09:59:58.000Z");
    let run = record_run(&fixture, "test-record-early-stage", commit_request(&fixture.root), results);
    assert_eq!(
        pending_of(&run.registration),
        EngramVerificationRecordPending::StartedBeforeRegistration { stage: Some("typescript".to_owned()) }
    );
    // A root generation that is not a positive i64.
    for (label, generation) in [("zero", 0), ("huge", u64::MAX)] {
        let mut run = record_run(&fixture, &format!("test-record-gen-{label}"), commit_request(&fixture.root), commit_results());
        run.registration.named_root.as_mut().unwrap().generation = generation;
        assert_eq!(
            pending_of(&run.registration),
            EngramVerificationRecordPending::RootGenerationOutOfRange { generation }
        );
    }
}

#[test]
fn the_real_fingerprint_tests_argv_is_a_test_fingerprinted_as_it_ran() {
    let fixture = RunFixture::new("verification-record-fingerprint-tests");
    let run = record_run(&fixture, "test-record-fingerprint-tests", commit_request(&fixture.root), commit_results());
    let check = &ready(&run.registration).body.checks[2];
    assert_eq!(check.stage, "fingerprint-tests");
    assert_eq!(check.check_kind, EngramVerificationRecordCheckKind::Test);
    assert_eq!(check.command, FINGERPRINT_TESTS_ARGV.join(" "));
    assert_eq!(check.check_fingerprint, fingerprint(&FINGERPRINT_TESTS_ARGV.join(" ")));
    // The shared rules are unchanged: neither reads this argv as a test.
    let argv: Vec<String> = FINGERPRINT_TESTS_ARGV.iter().map(|word| (*word).to_owned()).collect();
    assert!(!engram_is_test_command("node", &argv[1..]));
    assert!(!engram_launcher_test_stages(&commit_request(&fixture.root)).contains(&"fingerprint-tests".to_owned()));
}

#[test]
fn a_stage_that_is_neither_test_nor_build_keeps_the_run_pending_by_name() {
    let fixture = RunFixture::new("verification-record-unclassified");
    // A stage no rule, nor the sender's own stage table, names.
    let mut results = commit_results();
    results["stages"]
        .as_array_mut()
        .unwrap()
        .push(stage("lint", "passed", 0, &["node", "scripts/lint.mjs"], 5));
    let run = record_run(&fixture, "test-record-unclassified", commit_request(&fixture.root), results);
    let reason = EngramVerificationRecordPending::UnclassifiedStages {
        stages: vec!["lint".to_owned()],
    };
    assert_eq!(pending_of(&run.registration), reason);
    assert!(!run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE).exists(), "nothing is frozen without it");
    assert_eq!(
        engram_verification_record_progress(&run.run_dir),
        Some(EngramVerificationRecordProgress::Pending { reason })
    );
}

#[test]
fn a_recorded_launcher_command_is_used_before_the_reconstruction() {
    let fixture = RunFixture::new("verification-record-command");
    let mut results = commit_results();
    results["command"] = json!(["node", "scripts/test-launcher.mjs", "full", "--input", "commit", "--detach"]);
    let run = record_run(&fixture, "test-record-command", commit_request(&fixture.root), results);
    assert_eq!(
        ready(&run.registration).body.run.command,
        "node scripts/test-launcher.mjs full --input commit --detach"
    );
}

#[test]
fn the_frozen_body_is_byte_identical_when_rebuilt_and_after_a_reload() {
    let fixture = RunFixture::new("verification-record-frozen");
    let run = record_run(&fixture, "test-record-frozen", commit_request(&fixture.root), commit_results());
    let TestRunJson::Parsed(request, _, _) = test_run_read_json(&run.run_dir.join("request.json")) else {
        panic!("request");
    };
    let rebuilt = || {
        engram_verification_record_body(&run.registration, &request, &commit_results())
            .and_then(|body| engram_verification_record_text(&body))
            .expect("the run builds a record")
    };
    assert_eq!(rebuilt(), rebuilt());

    let first = ready(&run.registration);
    let persisted = fs::read_to_string(run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE))
        .expect("the body is persisted beside the run");
    assert_eq!(persisted, rebuilt());
    // A later look reads the persisted body back and sends it only while it
    // is exactly the record the host builds again: the run's files changed
    // under it make it a mismatch, never sent, and it is never rewritten.
    let mut changed = commit_results();
    changed["stages"][0]["ended"] = json!("2026-10-30T10:00:45.000Z");
    fs::write(run.run_dir.join("results.json"), changed.to_string()).unwrap();
    assert_eq!(pending_of(&run.registration), EngramVerificationRecordPending::BodyMismatch);
    assert_eq!(
        fs::read_to_string(run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE)).unwrap(),
        persisted
    );
    fs::write(run.run_dir.join("results.json"), commit_results().to_string()).unwrap();
    let reloaded = ready(&run.registration);
    assert_eq!(
        serde_json::to_vec(&engram_verification_record_request(&first)).unwrap(),
        serde_json::to_vec(&engram_verification_record_request(&reloaded)).unwrap()
    );

    // A body that no longer reads back exactly is never sent or rebuilt.
    fs::write(run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE), format!(" {persisted}")).unwrap();
    assert_eq!(pending_of(&run.registration), EngramVerificationRecordPending::BodyUnreadable);
}

fn interrupted_results() -> Value {
    let mut results = commit_results();
    results["interrupted"] = json!(true);
    for (index, state) in [(3, "running"), (4, "unrun")] {
        let stage = results["stages"][index].as_object_mut().unwrap();
        stage.insert("state".to_owned(), json!(state));
        stage.remove("ended");
        stage.remove("code");
        stage.remove("tests");
        if state == "unrun" {
            stage.remove("started");
            stage.remove("command");
        }
    }
    results
}

#[test]
fn an_interrupted_run_records_its_running_stage_as_indeterminate() {
    let fixture = RunFixture::new("verification-record-interrupted");
    let run = record_run(&fixture, "test-record-interrupted", commit_request(&fixture.root), interrupted_results());
    let frozen = ready(&run.registration);
    assert_eq!(frozen.body.run.outcome, EngramVerificationRecordRunOutcome::Indeterminate);
    let outcomes: Vec<_> = frozen.body.checks.iter().map(|check| check.outcome).collect();
    assert_eq!(
        outcomes,
        [
            EngramVerificationRecordCheckOutcome::Passed,
            EngramVerificationRecordCheckOutcome::Passed,
            EngramVerificationRecordCheckOutcome::Passed,
            EngramVerificationRecordCheckOutcome::Indeterminate,
            EngramVerificationRecordCheckOutcome::NotRun,
        ]
    );
    assert_eq!(frozen.body.checks[3].completed_at, None);
    // An unrun stage is fingerprinted over the argv its request planned.
    assert_eq!(frozen.body.checks[4].command, "node node_modules/vitest/vitest.mjs run");
    // The producer mints records only for completed checks that ran.
    assert_eq!(
        engram_verification_record_emitted_stages(&frozen.body),
        ["cargo-check", "typescript", "fingerprint-tests"]
    );
}

#[test]
fn a_check_command_reads_back_to_its_argv_and_fingerprints_it() {
    let argv: Vec<String> = [
        "node",
        r"C:\Program Files\nodejs\launcher.mjs",
        r#"say "hi""#,
        r"C:\dir with space\",
        "",
        "it's",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let command = engram_verification_record_check_command(&argv, EngramVerificationKind::Test)
        .expect("a command");
    assert_eq!(engram_shell_words(&command.normalized), Some(argv.clone()));
    assert_eq!(command.normalized, engram_verification_record_command_text(&argv));
    assert_eq!(EngramHost::check_fingerprint(&command), sha256_hex(command.normalized.as_bytes()));
    assert_eq!(command.argv, argv);
    assert!(engram_verification_record_check_command(&[], EngramVerificationKind::Test).is_none());
}

#[test]
fn stage_kinds_follow_the_actual_argv_then_the_stage_rule() {
    use crate::EngramVerificationRecordMode::{Focused, Full, Live};
    let words = |line: &str| line.split(' ').map(str::to_owned).collect::<Vec<_>>();
    let node = r"C:\Program Files\nodejs\node.exe";
    let with_node = |rest: &str| {
        let mut argv = vec![node.to_owned()];
        argv.extend(words(rest));
        argv
    };
    let kind = |mode, stage: &str, argv: Vec<String>, stages: &[&str]| {
        let stages: Vec<String> = stages.iter().map(|name| (*name).to_owned()).collect();
        engram_verification_record_check_kind(mode, stage, &argv, &stages)
    };
    let full_gate = ["rust-tests", "vitest"];
    let test = Some(EngramVerificationKind::Test);
    let build = Some(EngramVerificationKind::Build);

    // Every stage is first read by its argv, as a focused launcher command:
    // Node's own test runner, Vitest and cargo tests, in any mode.
    let node_test = with_node("--test --test-force-exit --test-timeout=60000 scripts/a.test.mjs");
    assert_eq!(kind(Focused, "focused", node_test.clone(), &[]), test);
    assert_eq!(kind(Full, "fingerprint-tests", node_test, &full_gate), test);
    assert_eq!(kind(Focused, "focused", words("npx vitest run"), &[]), test);
    assert_eq!(kind(Full, "ui", words("npx vitest run"), &full_gate), test);
    assert_eq!(kind(Full, "unit", words("cargo test --bin termal"), &full_gate), test);
    // Then true build commands.
    assert_eq!(kind(Full, "typescript", with_node("node_modules/typescript/bin/tsc --noEmit"), &full_gate), build);
    assert_eq!(kind(Full, "cargo-check", words("cargo check"), &full_gate), build);
    assert_eq!(kind(Focused, "focused", words("cargo build --release"), &[]), build);
    assert_eq!(kind(Focused, "focused", words("npx tsc --noEmit"), &[]), build);
    // Then the launcher's agreed stage rule, for stages the argv does not
    // name: the gate's Vitest and Rust stages, and a live run's stage.
    let vitest = with_node("node_modules/vitest/vitest.mjs run");
    assert_eq!(kind(Full, "vitest", vitest.clone(), &full_gate), test);
    assert_eq!(kind(Full, "rust-tests", words("sh scripts/test-rust.sh"), &full_gate), test);
    assert_eq!(kind(Live, "engram-live", words("sh scripts/live.sh"), &[]), test);
    // The sender's own launcher stage table: the helper tests behind their
    // reporters, which neither shared rule reads as a test.
    let reported = with_node(
        "--test-reporter=spec --test-reporter-destination=stdout --test scripts/a.test.mjs",
    );
    assert_eq!(kind(Full, "fingerprint-tests", reported.clone(), &full_gate), test);
    // Neither: no kind, and the run waits on it (never dropped).
    assert_eq!(kind(Full, "helper-tests", reported, &full_gate), None);
    assert_eq!(kind(Focused, "focused", vitest, &[]), None);
    assert_eq!(kind(Focused, "focused", words("node scripts/lint.mjs"), &[]), None);
}

#[test]
fn runs_that_are_not_bound_committed_input_runs_never_emit() {
    let fixture = RunFixture::new("verification-record-ineligible");
    let mut staged = commit_request(&fixture.root);
    staged.as_object_mut().unwrap().remove("input");
    let staged = record_run(&fixture, "test-record-staged", staged, commit_results());
    let mut no_input = commit_results();
    no_input.as_object_mut().unwrap().remove("input");
    let missing = record_run(&fixture, "test-record-missing", commit_request(&fixture.root), no_input);
    let mut unbound = record_run(&fixture, "test-record-unbound", commit_request(&fixture.root), commit_results());
    unbound.registration.work_binding = None;
    unbound.registration.named_root = None;
    let running = record_run(&fixture, "test-record-running", commit_request(&fixture.root), running_results(None));

    for run in [&staged, &missing, &unbound] {
        assert!(matches!(
            engram_verification_record_prepare(&run.registration),
            EngramVerificationRecordPrepared::NotEligible
        ));
    }
    assert!(matches!(
        engram_verification_record_prepare(&running.registration),
        EngramVerificationRecordPrepared::Waiting
    ));

    register(&fixture, &[&staged, &missing, &unbound, &running]);
    fixture.state.engram_verification_records_tick();
    for run in [&staged, &missing, &unbound, &running] {
        assert!(!run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE).exists(), "{}", run.registration.run_id);
        assert!(!run.run_dir.join(ENGRAM_VERIFICATION_RECORD_STATE_FILE).exists(), "{}", run.registration.run_id);
    }
}

#[test]
fn only_a_durably_registered_run_is_ever_recorded_never_an_unregistered_or_artifact_only_one() {
    let fixture = RunFixture::new("verification-record-unregistered");
    let session = create_test_project_session(&fixture.state, Agent::Codex, &fixture.project_id, &fixture.root);
    // Three identical eligible committed-input runs, initiated by a live
    // session: one durably registered; one whose registration record exists
    // but was never confirmed durable; one that is only a run directory.
    let mut registered = record_run(&fixture, "test-record-registered", commit_request(&fixture.root), commit_results());
    registered.registration.initiating_session_id = session.clone();
    let mut unconfirmed = record_run(&fixture, "test-record-unconfirmed", commit_request(&fixture.root), commit_results());
    unconfirmed.registration.initiating_session_id = session.clone();
    let mut artifact_only = record_run(&fixture, "test-record-artifact", commit_request(&fixture.root), commit_results());
    artifact_only.registration.initiating_session_id = session.clone();

    register(&fixture, &[&registered]);
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .insert(unconfirmed.registration.run_id.clone(), unconfirmed.registration.clone());
    fixture.state.engram_verification_records_tick();

    // The registered run is looked at: frozen and held pending (its project
    // has no Engram), which shows the pass reached eligible runs at all.
    assert!(registered.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE).exists());
    assert!(matches!(
        engram_verification_record_progress(&registered.run_dir),
        Some(EngramVerificationRecordProgress::Pending { .. })
    ));
    // Neither the unconfirmed registration nor the bare run directory is
    // ever frozen, marked or sent.
    for run in [&unconfirmed, &artifact_only] {
        assert!(!run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE).exists(), "{}", run.registration.run_id);
        assert!(!run.run_dir.join(ENGRAM_VERIFICATION_RECORD_STATE_FILE).exists(), "{}", run.registration.run_id);
    }
}

#[test]
fn the_initiating_session_routes_the_record_never_the_owning_one() {
    let fixture = RunFixture::new("verification-record-routing");
    let session = create_test_project_session(&fixture.state, Agent::Codex, &fixture.project_id, &fixture.root);

    // Initiated by a session that is gone: pending as such, even though the
    // owning session exists.
    let mut gone = record_run(&fixture, "test-record-gone", commit_request(&fixture.root), commit_results());
    gone.registration.initiating_session_id = "session-gone".to_owned();
    gone.registration.owning_session_id = session.clone();
    // Initiated by a live session (in a project without Engram), owned by a
    // session that is gone: routed by the initiating one alone.
    let mut live = record_run(&fixture, "test-record-live", commit_request(&fixture.root), commit_results());
    live.registration.initiating_session_id = session.clone();
    live.registration.owning_session_id = "session-gone".to_owned();

    assert!(matches!(
        fixture.state.engram_verification_record_route(&gone.registration),
        Err(EngramVerificationRecordPending::InitiatingSessionGone)
    ));
    assert!(matches!(
        fixture.state.engram_verification_record_route(&live.registration),
        Err(EngramVerificationRecordPending::NoConnection)
    ));

    register(&fixture, &[&gone, &live]);
    fixture.state.engram_verification_records_tick();
    // Both are frozen and left pending with their typed reasons.
    for (run, reason) in [
        (&gone, EngramVerificationRecordPending::InitiatingSessionGone),
        (&live, EngramVerificationRecordPending::NoConnection),
    ] {
        assert!(run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE).exists());
        assert_eq!(
            engram_verification_record_progress(&run.run_dir),
            Some(EngramVerificationRecordProgress::Pending { reason })
        );
    }
}

#[test]
fn historical_control_metadata_is_unchanged_by_native_verification() {
    let fixture = RunFixture::new("verification-record-control");
    let (session, transport) = engram_session(&fixture, true);
    with_session_engram(&fixture, &session, |engram| {
        engram.historical_control.insert("engramRoutingToken".to_owned(), json!("historical-token"));
        engram.historical_control.insert("engramOpenGrantId".to_owned(), json!("historical-grant"));
    });
    let history = session_engram(&fixture, &session, |engram| engram.historical_control.clone());
    let mut run = record_run(&fixture, "test-record-control", commit_request(&fixture.root), commit_results());
    initiated_by(&mut run, &session);
    register(&fixture, &[&run]);
    attach_launch_row(&fixture, &run);

    // A saved token stays inert history; current verification uses the
    // ordinary work session and creates no bind or control intent.
    transport.script(Ok(work_supported(&session)));
    transport.script(Ok(receipt_for("failed")));
    fixture.state.engram_verification_records_tick();
    assert_eq!(transport.sent_operations(), ["work_session_read", "verification_record"]);
    assert!(transport.sent_records()[0].get("routing_token").is_none());
    assert_eq!(
        session_engram(&fixture, &session, |engram| engram.historical_control.clone()),
        history
    );
    assert!(matches!(
        engram_verification_record_prepare(&current(&fixture, &run)),
        EngramVerificationRecordPrepared::Finished
    ));
}

#[test]
fn an_unassociated_ordinary_session_is_never_bound_by_the_sender() {
    let fixture = RunFixture::new("verification-record-control-unbound");
    let (session, transport) = engram_session(&fixture, true);
    let mut run = record_run(&fixture, "test-record-control-unbound", commit_request(&fixture.root), commit_results());
    initiated_by(&mut run, &session);
    transport.script(Ok(work_supported(&session)));
    let route = fixture.state.engram_verification_record_route(&run.registration)
        .expect("ordinary verification does not need a control binding");
    assert_eq!(route.connection.session_id, session);
    assert_eq!(transport.sent_operations(), ["work_session_read"]);
    assert!(sent_binds(&transport).is_empty());
    assert!(session_engram(&fixture, &session, |engram| engram.work_binding.is_none()));
}

#[test]
fn an_existing_binding_is_reused_and_never_replaced() {
    let fixture = RunFixture::new("verification-record-existing");
    let (session, transport) = engram_session(&fixture, false);
    bound_by_naming(&fixture, &session);
    let before = naming_state(&fixture, &session);
    let mut run = record_run(&fixture, "test-record-existing", commit_request(&fixture.root), commit_results());
    initiated_by(&mut run, &session);
    transport.script(Ok(work_supported(&session)));
    let route = match fixture.state.engram_verification_record_route(&run.registration) {
        Ok(route) => route,
        Err(reason) => panic!("the existing binding routes: {reason:?}"),
    };
    assert_eq!(route.connection.session_id, session);
    assert_eq!(transport.sent_operations(), ["work_session_read"]);
    assert_eq!(
        naming_state(&fixture, &session), before
    );
}

#[test]
fn the_sender_never_creates_an_engram_bind_or_intent_on_any_path() {
    // EO off: a fresh session and a reloaded session with passive history.
    for (case, reloaded) in [("unbound", false), ("reloaded", true)] {
        let fixture = RunFixture::new(&format!("verification-record-no-bind-{case}"));
        let (session, transport) = engram_session(&fixture, false);
        if reloaded {
            bound_by_naming(&fixture, &session);
            with_session_engram(&fixture, &session, |engram| {
                engram.historical_control.insert("engramRoutingToken".to_owned(), json!("historical-token"));
            });
            reload_session(&fixture, &session, &transport);
        }
        let mut run = record_run(&fixture, &format!("test-record-no-bind-{case}"), commit_request(&fixture.root), commit_results());
        initiated_by(&mut run, &session);
        register(&fixture, &[&run]);
        attach_launch_row(&fixture, &run);
        let before = naming_state(&fixture, &session);
        assert!(before.0.is_none(), "runtime work association is read afresh: {case}");
        assert!(before.1.is_none(), "no root-naming operation was restored: {case}");
        assert_eq!(before.2.contains_key("engramRoutingToken"), reloaded, "{case}");

        transport.script(Ok(work_supported(&session)));
        transport.script(Ok(receipt_for("failed")));
        fixture.state.engram_verification_records_tick();
        assert!(sent_binds(&transport).is_empty(), "{case}: no session_bind");
        assert_eq!(transport.sent_operations(), ["work_session_read", "verification_record"]);
        assert!(transport.sent_records()[0].get("routing_token").is_none());
        assert!(run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE).exists(), "{case}: frozen");
        assert!(matches!(engram_verification_record_prepare(&current(&fixture, &run)),
            EngramVerificationRecordPrepared::Finished), "{case}");
        assert_eq!(naming_state(&fixture, &session), before, "{case}: naming state untouched");
    }
}

#[test]
fn an_observability_off_session_bound_by_naming_gets_its_explicit_record_delivered() {
    let fixture = RunFixture::new("verification-record-naming-delivered");
    let (session, transport) = engram_session(&fixture, false);
    bound_by_naming(&fixture, &session);
    let mut run = record_run(&fixture, "test-record-naming-delivered", commit_request(&fixture.root), commit_results());
    initiated_by(&mut run, &session);
    register(&fixture, &[&run]);
    attach_launch_row(&fixture, &run);
    let before = naming_state(&fixture, &session);

    // Explicit verification neither depends on EO nor changes naming state.
    transport.script(Ok(work_supported(&session)));
    transport.script(Ok(receipt_for("failed")));
    fixture.state.engram_verification_records_tick();
    assert_eq!(transport.sent_operations(), ["work_session_read", "verification_record"]);
    assert!(transport.sent()[0].get("routing_token").is_none());
    let record = &transport.sent_records()[0];
    assert!(record.get("routing_token").is_none());
    assert_eq!(record["operation_key"], "termal-run:test-record-naming-delivered");
    assert!(matches!(
        engram_verification_record_prepare(&current(&fixture, &run)),
        EngramVerificationRecordPrepared::Finished
    ));
    assert_eq!(naming_state(&fixture, &session), before);
}

#[test]
fn an_unavailable_work_session_keeps_the_frozen_record_until_its_own_read_recovers() {
    let fixture = RunFixture::new("verification-record-naming-later");
    let (session, transport) = engram_session(&fixture, false);
    let mut run = record_run(&fixture, "test-record-naming-later", commit_request(&fixture.root), commit_results());
    initiated_by(&mut run, &session);
    register(&fixture, &[&run]);
    attach_launch_row(&fixture, &run);

    transport.script(Err(EngramTransportError::transport("work session unavailable")));
    fixture.state.engram_verification_records_tick();
    assert_eq!(transport.sent_operations(), ["work_session_read"]);
    assert!(matches!(
        engram_verification_record_progress(&run.run_dir),
        Some(EngramVerificationRecordProgress::Pending {
            reason: EngramVerificationRecordPending::TransportFailed { .. },
        })
    ));
    let body_path = run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE);
    let frozen_text = fs::read_to_string(&body_path).expect("the body is frozen");

    // Recovery reads the same child's ordinary capability and sends the same
    // frozen body/key. No naming operation or control credential is needed.
    let expected = serde_json::to_value(engram_verification_record_request(
        &ready(&current(&fixture, &run)),
    ))
    .expect("the request serializes");
    transport.script(Ok(work_supported(&session)));
    transport.script(Ok(receipt_for("failed")));
    fixture.state.engram_verification_records_tick();
    assert_eq!(transport.sent_operations(), ["work_session_read", "work_session_read", "verification_record"]);
    let sent = transport.sent_records();
    assert_eq!(sent[0], expected, "the same frozen body and key");
    assert_eq!(sent[0]["operation_key"], "termal-run:test-record-naming-later");
    assert_eq!(fs::read_to_string(&body_path).unwrap(), frozen_text, "never rebuilt");
    assert!(sent_binds(&transport).is_empty());
    assert!(matches!(
        engram_verification_record_prepare(&current(&fixture, &run)),
        EngramVerificationRecordPrepared::Finished
    ));
}

#[test]
fn without_the_capability_nothing_is_sent_and_the_run_stays_pending() {
    use crate::EngramVerificationRecordSupport as Support;
    let fixture = RunFixture::new("verification-record-unsupported");
    let run = record_run(&fixture, "test-record-unsupported", commit_request(&fixture.root), commit_results());
    let transport = Arc::new(RecordingTransport::default());
    let adapter = adapter(&transport);
    let own = connection(SESSION, "actor");

    let unsupported = |expected: Support| {
        let step = engram_verification_record_send(ready(&run.registration), link(&adapter, &own));
        let reason = EngramVerificationRecordPending::CapabilityUnsupported { support: expected };
        assert!(matches!(&step, EngramVerificationRecordStep::Pending(pending) if *pending == reason), "{step:?}");
        assert_eq!(
            engram_verification_record_progress(&run.run_dir),
            Some(EngramVerificationRecordProgress::Pending { reason })
        );
    };
    unsupported(Support::NotRead);
    for (answer, expected) in [
        (json!({ "session_id": SESSION, "work_session": true }), Support::NotRead),
        (work_session_with(json!(["host_work_v1", "something_else_v1"])), Support::EntryMissing),
        (work_session_with(json!("verification_record_v1")), Support::NotRead),
    ] {
        read_work_session(&transport, &adapter, &own, answer);
        unsupported(expected);
    }
    assert!(transport.sent_records().is_empty(), "no record is sent without the capability");
    assert_eq!(transport.sent_operations(), ["work_session_read"; 3]);
}

#[test]
fn a_capability_read_off_a_reader_connection_does_not_enable_emission() {
    let fixture = RunFixture::new("verification-record-reader");
    let run = record_run(&fixture, "test-record-reader", commit_request(&fixture.root), commit_results());
    let transport = Arc::new(RecordingTransport::default());
    let adapter = adapter(&transport);
    let own = connection(SESSION, "actor");
    // The host's reader connection answers a work-session read listing the
    // capability; the session's own connection has answered nothing.
    let reader = connection("termal-reader", "reader");
    read_work_session(&transport, &adapter, &reader, work_supported("termal-reader"));
    let step = engram_verification_record_send(ready(&run.registration), link(&adapter, &own));
    assert!(matches!(
        step,
        EngramVerificationRecordStep::Pending(EngramVerificationRecordPending::CapabilityUnsupported {
            support: EngramVerificationRecordSupport::NotRead,
        })
    ));
    assert!(transport.sent_records().is_empty());
}

#[test]
fn a_resend_after_a_child_reconnect_repeats_every_request_byte() {
    let fixture = RunFixture::new("verification-record-reconnect");
    let run = record_run(&fixture, "test-record-reconnect", commit_request(&fixture.root), commit_results());
    let transport = Arc::new(RecordingTransport::default());
    let adapter = adapter(&transport);
    let own = connection(SESSION, "actor");

    read_work_session(&transport, &adapter, &own, supported());
    transport.script(Err(EngramTransportError::transport("lost")));
    let step = engram_verification_record_send(ready(&run.registration), link(&adapter, &own));
    assert!(matches!(
        step,
        EngramVerificationRecordStep::Pending(EngramVerificationRecordPending::TransportFailed { .. })
    ));

    // A new child needs its own ordinary capability proof before any resend.
    transport.respawn();
    let step = engram_verification_record_send(ready(&run.registration), link(&adapter, &own));
    assert!(matches!(step, EngramVerificationRecordStep::Pending(_)));
    assert_eq!(transport.sent_records().len(), 1);
    read_work_session(&transport, &adapter, &own, supported());
    transport.script(Ok(receipt_for("failed")));
    let step = engram_verification_record_send(ready(&run.registration), link(&adapter, &own));
    assert!(matches!(step, EngramVerificationRecordStep::Received { .. }), "{step:?}");

    let sent = transport.sent_records();
    assert_eq!(sent.len(), 2);
    assert!(sent.iter().all(|request| request.get("routing_token").is_none()));
    assert_eq!(record_wire_bytes(&sent[0]), record_wire_bytes(&sent[1]), "the same body and operation key");
    assert_eq!(sent[1]["operation_key"], "termal-run:test-record-reconnect");
}

#[test]
fn an_ordinary_resend_keeps_its_registration_despite_focus_or_historical_metadata_changes() {
    let fixture = RunFixture::new("verification-record-recovery");
    let (session, transport) = engram_session(&fixture, true);
    let mut run = record_run(&fixture, "test-record-recovery", commit_request(&fixture.root), commit_results());
    initiated_by(&mut run, &session);
    register(&fixture, &[&run]);

    transport.script(Ok(work_supported(&session)));
    transport.script(Err(EngramTransportError::transport("lost")));
    fixture.state.engram_verification_records_tick();

    // Its live focus and passive history change; the registration does not.
    let focus = EngramControlWorkBinding {
        work_id: "work-focus".to_owned(),
        claim_id: "claim-focus".to_owned(),
        ..binding()
    };
    with_session_engram(&fixture, &session, |engram| {
        engram.historical_control.insert("engramRoutingToken".to_owned(), json!("changed-historical-token"));
        engram.work_binding = Some(focus);
    });
    transport.script(Ok(work_supported(&session)));
    transport.script(Ok(receipt_for("failed")));
    fixture.state.engram_verification_records_tick();

    let sent = transport.sent_records();
    assert_eq!(sent.len(), 2);
    assert!(sent[0].get("routing_token").is_none());
    assert!(sent[1].get("routing_token").is_none());
    assert_eq!(record_wire_bytes(&sent[0]), record_wire_bytes(&sent[1]), "the same body and operation key");
    assert_eq!(sent[1]["operation_key"], "termal-run:test-record-recovery");
    assert_eq!(sent[1]["registration"]["binding"], serde_json::to_value(binding()).unwrap());
    assert!(!transport.sent_operations().iter().any(|operation| operation == "session_bind"));
}

#[test]
fn a_receipt_that_does_not_answer_the_record_is_rejected_and_not_resent() {
    let fixture = RunFixture::new("verification-record-bad-receipt");
    let run = record_run(&fixture, "test-record-bad-receipt", commit_request(&fixture.root), commit_results());
    let frozen = ready(&run.registration);
    let check = |receipt: Value| {
        let receipt: EngramVerificationRecordReceipt = serde_json::from_value(receipt).unwrap();
        engram_verification_record_receipt_check(&frozen.body, &receipt)
    };
    assert!(check(receipt_for("failed")).is_ok());
    assert!(check(receipt_for("passed")).is_err(), "another run outcome");
    let mut partial = receipt_for("failed");
    partial["verifications"].as_array_mut().unwrap().pop();
    assert!(check(partial).is_err(), "a partial list");
    let mut repeated = receipt_for("failed");
    repeated["verifications"][1]["record_id"] = json!(record_id(0));
    assert!(check(repeated).is_err(), "a repeated record");
    let mut malformed = receipt_for("failed");
    malformed["verifications"][0]["record_id"] = json!("not-a-record");
    assert!(check(malformed).is_err(), "a malformed record");
    let mut foreign = receipt_for("failed");
    foreign["verifications"][2]["stage"] = json!("other-stage");
    assert!(check(foreign).is_err(), "a stage the record does not name");
    let mut operation = receipt_for("failed");
    operation["operation"] = json!("9".repeat(32));
    assert!(check(operation).is_err(), "an operation that is not its producer");
    let mut producer = receipt_for("failed");
    producer["operation"] = json!("not-an-id");
    producer["producer"] = json!("not-an-id");
    assert!(check(producer).is_err(), "a malformed producer");

    // For an interrupted run the producer records only the completed checks
    // that ran: exactly those, and no more.
    let interrupted = record_run(&fixture, "test-record-bad-receipt-interrupted", commit_request(&fixture.root), interrupted_results());
    let interrupted = ready(&interrupted.registration);
    let interrupted_check = |count: usize| {
        let mut receipt = receipt_for("indeterminate");
        receipt["verifications"].as_array_mut().unwrap().truncate(count);
        let receipt: EngramVerificationRecordReceipt = serde_json::from_value(receipt).unwrap();
        engram_verification_record_receipt_check(&interrupted.body, &receipt)
    };
    assert!(interrupted_check(3).is_ok());
    assert!(interrupted_check(4).is_err(), "a record for a stage still running");
    assert!(interrupted_check(5).is_err(), "a record for a stage that never ran");

    let transport = Arc::new(RecordingTransport::default());
    let adapter = adapter(&transport);
    let own = connection(SESSION, "actor");
    read_work_session(&transport, &adapter, &own, supported());
    transport.script(Ok(receipt_for("passed")));
    let step = engram_verification_record_send(frozen, link(&adapter, &own));
    assert!(matches!(step, EngramVerificationRecordStep::Rejected(_)));
    // The host keeps what it observed on the registration, as the tick does.
    register(&fixture, &[&run]);
    fixture.state.note_engram_verification_record_step(&run.registration, &step);
    assert!(matches!(
        engram_verification_record_prepare(&current(&fixture, &run)),
        EngramVerificationRecordPrepared::Finished
    ));
}

/// The test checks' records on the row, in check order: fingerprint-tests,
/// rust-tests, vitest.
fn settled_records() -> Vec<Option<String>> {
    vec![Some(record_id(2)), Some(record_id(3)), Some(record_id(4))]
}

#[test]
fn a_receipt_settles_the_runs_own_launch_observation_without_a_checkpoint() {
    let fixture = RunFixture::new("verification-record-settle");
    let run = record_run(&fixture, "test-record-settle", commit_request(&fixture.root), commit_results());
    register(&fixture, &[&run]);
    let frozen = ready(&run.registration);
    let (execution_id, row) = launch_row(&run);
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_observations
        .insert(execution_id.clone(), row);
    let receipt: EngramVerificationRecordReceipt =
        serde_json::from_value(receipt_for("failed")).unwrap();
    fixture
        .state
        .complete_engram_verification_record(&run.registration, &frozen.body, &receipt);

    let inner = fixture.state.inner.lock().unwrap();
    let row = inner.test_run_observations.get(&execution_id).expect("the row stays");
    // One association per test check; the build stages settle nothing here.
    let settled: Vec<_> = row
        .evidence
        .iter()
        .map(|association| {
            (
                association.verification_record_id.clone(),
                association.checkpoint_id.clone(),
                association.reported_outcome,
                association.binding == binding() && association.store == store(),
            )
        })
        .collect();
    let records = settled_records();
    assert_eq!(
        settled,
        [
            (records[0].clone(), None, EngramExecutionOutcome::Succeeded, true),
            (records[1].clone(), None, EngramExecutionOutcome::Succeeded, true),
            (records[2].clone(), None, EngramExecutionOutcome::Failed, true),
        ]
    );
    let producer = sha256_hex(b"termal-run:test-record-settle:rust-tests");
    assert_eq!(row.evidence[1].producer_observation_id, producer);
    assert_eq!(row.evidence[1].action_fingerprint, fingerprint("sh scripts/test-rust.sh"));
    assert_eq!(row.evidence[1].source_basis.source_revision, format!("git-commit:{COMMIT}"));
    assert_eq!(row.evidence[1].source_basis.source_root_generation, Some(3));
    drop(inner);
    assert_eq!(
        current(&fixture, &run).verification_record,
        Some(TestRunVerificationAck::Settled {
            receipt,
            body_digest: engram_verification_record_body_digest(&frozen.body),
        }),
        "the host keeps the settlement on the registration"
    );
    assert!(matches!(
        engram_verification_record_prepare(&current(&fixture, &run)),
        EngramVerificationRecordPrepared::Finished
    ));
}

#[test]
fn a_crash_after_the_receipt_settles_from_the_held_receipt_without_a_resend() {
    let fixture = RunFixture::new("verification-record-crash");
    let run = record_run(&fixture, "test-record-crash", commit_request(&fixture.root), commit_results());
    register(&fixture, &[&run]);
    let execution_id = attach_launch_row(&fixture, &run);
    let transport = Arc::new(RecordingTransport::default());
    let adapter = adapter(&transport);
    let own = connection(SESSION, "actor");
    read_work_session(&transport, &adapter, &own, supported());

    // A valid receipt is held at once, on the registration as the host
    // observed it and mirrored beside the run; it is not yet the record's
    // settlement.
    transport.script(Ok(receipt_for("failed")));
    let step = engram_verification_record_send(ready(&run.registration), link(&adapter, &own));
    fixture.state.note_engram_verification_record_step(&run.registration, &step);
    let EngramVerificationRecordStep::Received { body, receipt } = step else {
        panic!("a valid receipt is received");
    };
    assert_eq!(
        current(&fixture, &run).verification_record,
        Some(TestRunVerificationAck::Received {
            receipt: receipt.clone(),
            body_digest: engram_verification_record_body_digest(&body),
        })
    );
    assert_eq!(
        engram_verification_record_progress(&run.run_dir),
        Some(EngramVerificationRecordProgress::Received { receipt: receipt.clone() })
    );
    // The associations are written; the host stops before marking settled.
    assert!(matches!(
        fixture.state.settle_engram_verification_record(&run.registration, &body, &receipt),
        EngramVerificationRecordAssociation::Associated(_)
    ));

    // After the restart the held receipt settles the record: nothing is sent
    // again, and no association is duplicated.
    let EngramVerificationRecordPrepared::ReceiptHeld { frozen, receipt: held } =
        engram_verification_record_prepare(&current(&fixture, &run))
    else {
        panic!("the receipt is held");
    };
    assert_eq!(held, receipt);
    assert_eq!(
        fixture.state.complete_engram_verification_record(&run.registration, &frozen.body, &held),
        EngramVerificationRecordSettlement::Settled
    );
    assert_eq!(transport.sent_records().len(), 1, "never sent again");
    assert_eq!(row_records(&fixture, &execution_id), settled_records(), "no association is duplicated");
    assert!(matches!(
        engram_verification_record_prepare(&current(&fixture, &run)),
        EngramVerificationRecordPrepared::Finished
    ));
}

#[test]
fn a_held_receipt_never_settles_a_body_rewritten_with_its_results_to_another_commit() {
    const UNTESTED: &str = "89abcdef0123456789abcdef0123456789abcdef";
    let fixture = RunFixture::new("verification-record-rewritten");
    let run = record_run(&fixture, "test-record-rewritten", commit_request(&fixture.root), commit_results());
    register(&fixture, &[&run]);
    let execution_id = attach_launch_row(&fixture, &run);
    let transport = Arc::new(RecordingTransport::default());
    let adapter = adapter(&transport);
    let own = connection(SESSION, "actor");
    read_work_session(&transport, &adapter, &own, supported());

    // Engram genuinely answers the record of the run on COMMIT; the host
    // holds the receipt, bound to that body, but has not settled it yet.
    transport.script(Ok(receipt_for("failed")));
    let step = engram_verification_record_send(ready(&run.registration), link(&adapter, &own));
    fixture.state.note_engram_verification_record_step(&run.registration, &step);
    let EngramVerificationRecordStep::Received { body: answered, receipt } = step else {
        panic!("a valid receipt is received");
    };
    let held = current(&fixture, &run).verification_record;
    assert_eq!(
        held,
        Some(TestRunVerificationAck::Received {
            receipt: receipt.clone(),
            body_digest: engram_verification_record_body_digest(&answered),
        })
    );

    // The run's results and its frozen body are rewritten together to an
    // untested commit: the body is the host's own rebuild of those results,
    // and the receipt still answers its stages and outcome.
    let mut results = commit_results();
    results["input"]["commit"] = json!(UNTESTED);
    results["input"]["after"]["commit"] = json!(UNTESTED);
    fs::write(run.run_dir.join("results.json"), results.to_string()).unwrap();
    let TestRunJson::Parsed(request, _, _) = test_run_read_json(&run.run_dir.join("request.json")) else {
        panic!("the request reads back");
    };
    let rewritten = engram_verification_record_body(&run.registration, &request, &results).expect("a record builds");
    fs::write(
        run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE),
        engram_verification_record_text(&rewritten).unwrap(),
    )
    .unwrap();
    assert!(engram_verification_record_is_host_body(&run.registration, &rewritten));
    assert_eq!(engram_verification_record_receipt_check(&rewritten, &receipt), Ok(()));

    // Not settled from the held receipt, by the look or by a direct settle.
    assert_eq!(
        pending_of(&current(&fixture, &run)),
        EngramVerificationRecordPending::AcknowledgedBodyChanged
    );
    assert_eq!(
        fixture.state.complete_engram_verification_record(&run.registration, &rewritten, &receipt),
        EngramVerificationRecordSettlement::AcknowledgedBodyChanged
    );
    fixture.state.engram_verification_records_tick();
    assert!(row_records(&fixture, &execution_id).is_empty(), "no record is attached to the rewritten run");
    assert_eq!(current(&fixture, &run).verification_record, held, "the held receipt stays bound to its body");
    assert_eq!(transport.sent_records().len(), 1, "never sent again");
}

/// A project's per-call Engram deadline, raised where a test drives the
/// persistence worker by hand: the deadline only bounds a broken fixture.
fn set_call_deadline(fixture: &RunFixture, deadline_ms: u64) {
    let mut inner = fixture.state.inner.lock().unwrap();
    inner
        .projects
        .iter_mut()
        .find(|project| project.id == fixture.project_id)
        .and_then(|project| project.engram.as_mut())
        .expect("the fixture's Engram settings")
        .deadline_ms = Some(deadline_ms);
}

#[test]
fn a_receipt_is_held_until_the_launch_row_appears_then_settles_without_a_resend() {
    let fixture = RunFixture::new("verification-record-held");
    let (session, transport) = engram_session(&fixture, true);
    let mut run = record_run(&fixture, "test-record-held", commit_request(&fixture.root), commit_results());
    initiated_by(&mut run, &session);
    register(&fixture, &[&run]);
    let receipt: EngramVerificationRecordReceipt =
        serde_json::from_value(receipt_for("failed")).expect("the receipt reads");

    // Answered before the launcher's row carries the run's artifact: held,
    // not settled.
    transport.script(Ok(work_supported(&session)));
    transport.script(Ok(receipt_for("failed")));
    fixture.state.engram_verification_records_tick();
    assert_eq!(
        engram_verification_record_progress(&run.run_dir),
        Some(EngramVerificationRecordProgress::Received { receipt: receipt.clone() })
    );
    // A later look with still no row neither sends nor settles.
    fixture.state.engram_verification_records_tick();
    assert_eq!(transport.sent_operations(), ["work_session_read", "verification_record"]);
    assert!(matches!(
        engram_verification_record_prepare(&current(&fixture, &run)),
        EngramVerificationRecordPrepared::ReceiptHeld { .. }
    ));

    // The row appears: the next look settles from the held receipt alone.
    let execution_id = attach_launch_row(&fixture, &run);
    fixture.state.engram_verification_records_tick();
    assert_eq!(
        transport.sent_operations(),
        ["work_session_read", "verification_record"],
        "never sent again"
    );
    assert_eq!(row_records(&fixture, &execution_id), settled_records());
    assert_eq!(
        engram_verification_record_progress(&run.run_dir),
        Some(EngramVerificationRecordProgress::Settled { receipt })
    );
}

#[test]
fn a_held_receipt_whose_association_is_not_durable_stays_held_and_retries() {
    let mut fixture = RunFixture::new("verification-record-not-durable");
    let (session, transport) = engram_session(&fixture, true);
    let mut run = record_run(&fixture, "test-record-not-durable", commit_request(&fixture.root), commit_results());
    initiated_by(&mut run, &session);
    register(&fixture, &[&run]);
    let execution_id = attach_launch_row(&fixture, &run);
    let receipt: EngramVerificationRecordReceipt =
        serde_json::from_value(receipt_for("failed")).expect("the receipt reads");

    // The real background channel, its worker failing every write: the
    // associations are queued, never read back durable, so the receipt
    // stays held.
    failing_persist_worker(&mut fixture);
    transport.script(Ok(work_supported(&session)));
    transport.script(Ok(receipt_for("failed")));
    fixture.state.engram_verification_records_tick();
    assert_eq!(
        engram_verification_record_progress(&run.run_dir),
        Some(EngramVerificationRecordProgress::Received { receipt: receipt.clone() })
    );

    // The writer recovers: the next look settles from the held receipt, with
    // no resend.
    synchronous_persistence(&mut fixture);
    fixture.state.engram_verification_records_tick();
    assert_eq!(transport.sent_operations(), ["work_session_read", "verification_record"]);
    assert_eq!(row_records(&fixture, &execution_id), settled_records());
    assert_eq!(
        engram_verification_record_progress(&run.run_dir),
        Some(EngramVerificationRecordProgress::Settled { receipt })
    );
}

#[test]
fn a_refused_work_session_never_sends_a_record_or_changes_historical_binding() {
    // Passive historical data cannot replace a current ordinary-child proof.
    for (case, observability) in [("eo-on", true), ("eo-off", false)] {
        let fixture = RunFixture::new(&format!("verification-record-{case}-refused"));
        let (session, transport) = engram_session(&fixture, observability);
        bound_by_naming(&fixture, &session);
        with_session_engram(&fixture, &session, |engram| {
            engram.historical_control.insert("engramRoutingToken".to_owned(), json!("historical-token"));
        });
        let mut run = record_run(&fixture, &format!("test-record-{case}-refused"), commit_request(&fixture.root), commit_results());
        initiated_by(&mut run, &session);
        register(&fixture, &[&run]);
        let before = naming_state(&fixture, &session);

        transport.script(Err(EngramTransportError::transport("work read refused")));
        fixture.state.engram_verification_records_tick();
        // Each failed ordinary read leaves the record pending, without ever
        // falling back to control status, a bind or the saved token.
        transport.script(Err(EngramTransportError::transport("work read refused again")));
        fixture.state.engram_verification_records_tick();
        assert_eq!(
            transport.sent_operations(),
            ["work_session_read", "work_session_read"],
            "{case}"
        );
        assert!(matches!(
            engram_verification_record_progress(&run.run_dir),
            Some(EngramVerificationRecordProgress::Pending {
                reason: EngramVerificationRecordPending::TransportFailed { .. },
            })
        ), "{case}");
        // The binding and historical data stay with their existing owners.
        assert_eq!(naming_state(&fixture, &session), before, "{case}");
    }
}

#[test]
fn only_a_definite_refusal_rejects_the_record_and_any_other_answer_resends_it_unchanged() {
    let fixture = RunFixture::new("verification-record-deferred");
    let run = record_run(&fixture, "test-record-deferred", commit_request(&fixture.root), commit_results());
    let transport = Arc::new(RecordingTransport::default());
    let adapter = adapter(&transport);
    let own = connection(SESSION, "actor");
    read_work_session(&transport, &adapter, &own, supported());
    let remote = |code: &str| -> std::result::Result<Value, EngramTransportError> {
        Err(EngramTransportError::remote(EngramControlErrorBody {
            code: code.to_owned(),
            message: format!("{code} from Engram"),
        }))
    };

    // Engram's transient answers, and a code this host does not know: the
    // record stays pending and goes again, the same body under the same key.
    let deferred = [
        "deadline_exceeded",
        "control_unavailable",
        "busy",
        "dispatch_budget_exhausted",
        "control_startup_busy",
        "a_code_from_a_later_engram",
    ];
    register(&fixture, &[&run]);
    for code in deferred {
        transport.script(remote(code));
        let step = engram_verification_record_send(ready(&run.registration), link(&adapter, &own));
        let reason = EngramVerificationRecordPending::RemoteDeferred { code: code.to_owned() };
        assert!(
            matches!(&step, EngramVerificationRecordStep::Pending(pending) if *pending == reason),
            "{code}: {step:?}"
        );
        assert_eq!(
            engram_verification_record_progress(&run.run_dir),
            Some(EngramVerificationRecordProgress::Pending { reason })
        );
        // A deferred answer acknowledges nothing: the host keeps no receipt.
        fixture.state.note_engram_verification_record_step(&run.registration, &step);
        assert_eq!(current(&fixture, &run).verification_record, None, "{code}");
    }

    // A definite refusal of the record is final: rejected, never sent again.
    transport.script(remote("invalid_request"));
    let step = engram_verification_record_send(ready(&run.registration), link(&adapter, &own));
    assert!(
        matches!(&step, EngramVerificationRecordStep::Rejected(detail) if detail.contains("invalid_request")),
        "{step:?}"
    );
    // The host keeps the refusal it observed on the registration, as the
    // tick does.
    register(&fixture, &[&run]);
    fixture.state.note_engram_verification_record_step(&run.registration, &step);
    assert!(matches!(
        engram_verification_record_prepare(&current(&fixture, &run)),
        EngramVerificationRecordPrepared::Finished
    ));
    let sent = transport.sent_records();
    assert_eq!(sent.len(), deferred.len() + 1);
    for request in &sent[1..] {
        assert_eq!(record_wire_bytes(request), record_wire_bytes(&sent[0]), "the same body and operation key");
    }
    assert_eq!(sent[0]["operation_key"], "termal-run:test-record-deferred");
}

#[test]
fn a_tick_keeps_to_its_own_budget_and_stops_between_runs_on_shutdown() {
    let fixture = RunFixture::new("verification-record-tick-budget");
    let (session, transport) = engram_session(&fixture, true);
    set_call_deadline(&fixture, 600_000);
    let mut first = record_run(&fixture, "test-record-budget-a", commit_request(&fixture.root), commit_results());
    initiated_by(&mut first, &session);
    let mut second = record_run(&fixture, "test-record-budget-b", commit_request(&fixture.root), commit_results());
    initiated_by(&mut second, &session);
    register(&fixture, &[&first, &second]);
    let frozen = |run: &RecordRun| run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE).exists();

    // A spent budget looks at no run.
    assert_eq!(fixture.state.engram_verification_records_tick_within(Duration::ZERO, &|| false), 0);
    assert!(transport.sent().is_empty());
    assert!(!frozen(&first) && !frozen(&second));

    // Shutdown signalled once the first run was looked at: the pass stops
    // before the next.
    let looks = std::sync::atomic::AtomicUsize::new(0);
    transport.script(Ok(work_supported(&session)));
    transport.script(Ok(receipt_for("failed")));
    let looked = fixture.state.engram_verification_records_tick_within(
        ENGRAM_VERIFICATION_RECORD_TICK_BUDGET,
        &|| looks.fetch_add(1, Ordering::SeqCst) >= 1,
    );
    assert_eq!(looked, 1);
    assert_eq!(transport.sent_records().len(), 1);
    assert_ne!(frozen(&first), frozen(&second), "exactly one run was looked at");

    // Each call gets at most what is left of the pass's own budget, never
    // the connection's longer per-call deadline.
    let budget = Duration::from_secs(30);
    let before = transport.timeouts.lock().unwrap().len();
    transport.script(Ok(work_supported(&session)));
    transport.script(Ok(receipt_for("failed")));
    fixture.state.engram_verification_records_tick_within(budget, &|| false);
    let timeouts = transport.timeouts.lock().unwrap()[before..].to_vec();
    assert_eq!(transport.sent_records().len(), 2);
    assert!(!timeouts.is_empty());
    assert!(timeouts.iter().all(|timeout| *timeout <= budget), "{timeouts:?}");
}

#[test]
fn a_run_whose_calls_spend_the_pass_never_starves_the_runs_after_it() {
    let fixture = RunFixture::new("verification-record-round-robin");
    let clock = fixture.state.select_test_scripted_engram_budget_clock();
    let (session, transport) = engram_session(&fixture, true);
    let mut early = record_run(&fixture, "test-record-rr-a", commit_request(&fixture.root), commit_results());
    initiated_by(&mut early, &session);
    let mut later = record_run(&fixture, "test-record-rr-b", commit_request(&fixture.root), commit_results());
    initiated_by(&mut later, &session);
    register(&fixture, &[&early, &later]);
    // Every call that sends the early run's record spends the whole pass.
    {
        let clock = clock.clone();
        *transport.observe.lock().unwrap() = Some(Box::new(move |request: &EngramControlRequest| {
            if let EngramControlRequest::VerificationRecord { operation_key, .. } = request
                && operation_key == "termal-run:test-record-rr-a"
            {
                clock.advance(ENGRAM_VERIFICATION_RECORD_TICK_BUDGET);
            }
        }));
    }
    let mut cursor = EngramVerificationRecordCursor::default();
    let pass = |cursor: &mut EngramVerificationRecordCursor| {
        fixture
            .state
            .engram_verification_records_tick_from(ENGRAM_VERIFICATION_RECORD_TICK_BUDGET, &|| false, cursor)
    };
    transport.script(Ok(work_supported(&session)));
    transport.script(Err(EngramTransportError::deadline("the call spent the pass")));
    assert_eq!(pass(&mut cursor), 1, "the early run spent the first pass");
    // The next pass, once the early run's first backoff ran out, starts
    // after it: the later run goes first and is sent, and the early run
    // follows behind it.
    clock.advance(ENGRAM_VERIFICATION_RECORD_BACKOFF_START);
    transport.script(Ok(work_supported(&session)));
    transport.script(Ok(receipt_for("failed")));
    transport.script(Ok(work_supported(&session)));
    transport.script(Err(EngramTransportError::deadline("the early call spends its next pass")));
    assert_eq!(pass(&mut cursor), 2);
    let keys: Vec<String> = transport
        .sent_records()
        .iter()
        .map(|request| request["operation_key"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(keys, ["termal-run:test-record-rr-a", "termal-run:test-record-rr-b", "termal-run:test-record-rr-a"]);
    let receipt: EngramVerificationRecordReceipt =
        serde_json::from_value(receipt_for("failed")).expect("the receipt reads");
    assert_eq!(
        engram_verification_record_progress(&later.run_dir),
        Some(EngramVerificationRecordProgress::Received { receipt })
    );
}

/// The record's frozen text for the run's own request and results, built
/// for `registration`.
fn body_text_for(run: &RecordRun, registration: &TestRunRegistration) -> String {
    let TestRunJson::Parsed(request, _, _) = test_run_read_json(&run.run_dir.join("request.json")) else {
        panic!("the request reads back");
    };
    engram_verification_record_body(registration, &request, &commit_results())
        .and_then(|body| engram_verification_record_text(&body))
        .expect("a record builds")
}

/// The registration under another claim of the same work.
fn bound_elsewhere(registration: &TestRunRegistration) -> TestRunRegistration {
    let mut other = registration.clone();
    other.work_binding = Some(EngramControlWorkBinding {
        claim_id: "claim-other".to_owned(),
        ..binding()
    });
    other
}

#[test]
fn a_body_written_before_the_host_froze_it_is_never_sent() {
    let fixture = RunFixture::new("verification-record-prewritten");
    let mut run = record_run(&fixture, "test-record-prewritten", commit_request(&fixture.root), commit_results());
    // Another binding's record, in the exact frozen form, already beside the
    // run before the host's first look.
    let planted = body_text_for(&run, &bound_elsewhere(&run.registration));
    let body_path = run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE);
    fs::write(&body_path, &planted).unwrap();
    assert_eq!(pending_of(&run.registration), EngramVerificationRecordPending::BodyMismatch);
    assert_eq!(fs::read_to_string(&body_path).unwrap(), planted, "never rewritten");

    // Through the tick, nothing goes out for it.
    let (session, transport) = engram_session(&fixture, true);
    initiated_by(&mut run, &session);
    register(&fixture, &[&run]);
    fixture.state.engram_verification_records_tick();
    assert!(transport.sent_records().is_empty());
}

#[test]
fn a_body_tampered_after_freezing_is_never_sent() {
    let fixture = RunFixture::new("verification-record-tampered");
    let run = record_run(&fixture, "test-record-tampered", commit_request(&fixture.root), commit_results());
    ready(&run.registration);
    let body_path = run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE);
    let mut tampered: Value = serde_json::from_str(&fs::read_to_string(&body_path).unwrap()).unwrap();
    tampered["registration"]["binding"]["claim_id"] = json!("claim-forged");
    // Still in the exact frozen form: it reads back to its own bytes.
    fs::write(&body_path, serde_json::to_string(&tampered).unwrap()).unwrap();
    assert_eq!(pending_of(&run.registration), EngramVerificationRecordPending::BodyMismatch);
    assert_eq!(
        engram_verification_record_progress(&run.run_dir),
        Some(EngramVerificationRecordProgress::Pending {
            reason: EngramVerificationRecordPending::BodyMismatch,
        })
    );
}

#[test]
fn unknown_frozen_body_fields_are_rejected_without_sending_or_settling() {
    for field in ["routing_token", "operation", "nested_extension"] {
        let fixture = RunFixture::new(&format!("verification-record-extra-{field}"));
        let (session, transport) = engram_session(&fixture, false);
        let (runtime, provider) = test_codex_runtime_handle(&format!("native-extra-{field}"));
        {
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
            inner.sessions[index].session.status = SessionStatus::Idle;
        }
        let mut run = record_run(&fixture, &format!("test-native-extra-{field}"),
            commit_request(&fixture.root), commit_results());
        initiated_by(&mut run, &session);
        register(&fixture, &[&run]);
        let execution_id = attach_launch_row(&fixture, &run);
        ready(&current(&fixture, &run));
        let path = run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE);
        let original = fs::read_to_string(&path).unwrap();
        assert!(engram_verification_record_parse(&original).is_some(), "generated body control");
        let mut raw: Value = serde_json::from_str(&original).unwrap();
        match field {
            "routing_token" => raw["routing_token"] = json!("planted-control-route"),
            "operation" => raw["operation"] = json!("planted-operation"),
            "nested_extension" => raw["checks"][0]["unrecognized"] = json!({"opaque": true}),
            _ => unreachable!(),
        }
        let planted = serde_json::to_string(&raw).unwrap();
        assert!(engram_verification_record_parse(&planted).is_none(), "{field}");
        fs::write(&path, &planted).unwrap();
        assert_eq!(pending_of(&current(&fixture, &run)), EngramVerificationRecordPending::BodyUnreadable);
        fixture.state.engram_verification_records_tick();
        assert!(transport.sent_operations().is_empty(), "{field}: rejected before capability/RPC send");
        assert!(matches!(provider.try_recv(), Err(mpsc::TryRecvError::Empty)));
        assert!(current(&fixture, &run).verification_record.is_none(), "no held receipt");
        assert!(row_records(&fixture, &execution_id).is_empty(), "no native association");
        assert_eq!(fs::read_to_string(&path).unwrap(), planted, "no silent normalization");

        // Correcting the input to the exact host-generated body makes the
        // normal native sender eligible; no new key or registration is made.
        fs::write(&path, &original).unwrap();
        transport.script(Ok(work_supported(&session)));
        transport.script(Ok(receipt_for("failed")));
        fixture.state.engram_verification_records_tick();
        assert_eq!(transport.sent_operations(), ["work_session_read", "verification_record"]);
        assert_eq!(transport.sent_records()[0]["operation_key"],
            engram_verification_record_key(&run.registration.run_id));
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert!(matches!(engram_verification_record_prepare(&current(&fixture, &run)),
            EngramVerificationRecordPrepared::Finished));
        let records = row_records(&fixture, &execution_id);
        assert_eq!(records.len(), 3);
        assert!(records.iter().all(Option::is_some));
        assert!(matches!(provider.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }
}

#[test]
fn a_forged_receipt_beside_the_run_is_never_settled() {
    let fixture = RunFixture::new("verification-record-forged-receipt");
    let run = record_run(&fixture, "test-record-forged-receipt", commit_request(&fixture.root), commit_results());
    register(&fixture, &[&run]);
    let frozen = ready(&run.registration);
    let execution_id = attach_launch_row(&fixture, &run);
    // A receipt that does not answer the frozen body (another run outcome),
    // written beside the run as if Engram had answered: the file is not
    // where a receipt is held, so the record is still ready to send.
    let forged: EngramVerificationRecordReceipt =
        serde_json::from_value(receipt_for("passed")).expect("the receipt reads");
    engram_verification_record_set_progress(
        &run.run_dir,
        &run.registration.run_id,
        EngramVerificationRecordProgress::Received { receipt: forged.clone() },
    );
    assert!(matches!(
        engram_verification_record_prepare(&current(&fixture, &run)),
        EngramVerificationRecordPrepared::Ready(_)
    ));
    assert!(matches!(
        fixture.state.complete_engram_verification_record(&run.registration, &frozen.body, &forged),
        EngramVerificationRecordSettlement::ReceiptMismatch { .. }
    ));
    assert!(row_records(&fixture, &execution_id).is_empty(), "nothing is settled from it");
    assert_eq!(current(&fixture, &run).verification_record, None, "nor is it kept");

    // A receipt the host did receive, held over a body that is no longer
    // the host's own, is not settled from either.
    let held: EngramVerificationRecordReceipt =
        serde_json::from_value(receipt_for("failed")).expect("the receipt reads");
    fixture.state.note_engram_verification_record_ack(
        &run.registration,
        TestRunVerificationAck::Received {
            receipt: held,
            body_digest: engram_verification_record_body_digest(&frozen.body),
        },
    );
    fs::write(
        run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE),
        body_text_for(&run, &bound_elsewhere(&run.registration)),
    )
    .unwrap();
    assert_eq!(pending_of(&current(&fixture, &run)), EngramVerificationRecordPending::BodyMismatch);
    assert!(row_records(&fixture, &execution_id).is_empty());
}

#[test]
fn a_valid_receipt_settlement_or_rejection_forged_beside_the_run_never_stops_the_send() {
    // A receipt in the producer's exact shape that answers the frozen body
    // (its operation its producer, well-formed distinct ids, every emitted
    // stage and the run's own outcome), but with record ids Engram never
    // minted.
    let mut forged = receipt_for("failed");
    for (index, verification) in forged["verifications"].as_array_mut().unwrap().iter_mut().enumerate() {
        verification["record_id"] = json!(["1", "2", "3", "4", "5"][index].repeat(32));
    }
    let forged: EngramVerificationRecordReceipt = serde_json::from_value(forged).expect("the receipt reads");
    for (case, progress) in [
        ("received", EngramVerificationRecordProgress::Received { receipt: forged.clone() }),
        ("settled", EngramVerificationRecordProgress::Settled { receipt: forged.clone() }),
        ("rejected", EngramVerificationRecordProgress::Rejected { detail: "written beside the run".to_owned() }),
    ] {
        let fixture = RunFixture::new(&format!("verification-record-forged-{case}"));
        let (session, transport) = engram_session(&fixture, true);
        let mut run = record_run(&fixture, &format!("test-record-forged-{case}"), commit_request(&fixture.root), commit_results());
        initiated_by(&mut run, &session);
        register(&fixture, &[&run]);
        let execution_id = attach_launch_row(&fixture, &run);
        let frozen = ready(&run.registration);
        assert_eq!(engram_verification_record_receipt_check(&frozen.body, &forged), Ok(()), "{case}");
        engram_verification_record_set_progress(&run.run_dir, &run.registration.run_id, progress);

        // The record goes out, and only Engram's own answer settles.
        transport.script(Ok(work_supported(&session)));
        transport.script(Ok(receipt_for("failed")));
        fixture.state.engram_verification_records_tick();
        assert_eq!(transport.sent_operations(), ["work_session_read", "verification_record"], "{case}");
        assert_eq!(row_records(&fixture, &execution_id), settled_records(), "{case}: never the forged ids");
        let receipt: EngramVerificationRecordReceipt =
            serde_json::from_value(receipt_for("failed")).expect("the receipt reads");
        assert_eq!(
            current(&fixture, &run).verification_record,
            Some(TestRunVerificationAck::Settled {
                receipt,
                body_digest: engram_verification_record_body_digest(&frozen.body),
            }),
            "{case}"
        );
    }
}

#[test]
fn an_answer_read_across_a_respawn_is_never_recorded_as_the_new_childs() {
    use crate::EngramVerificationRecordSupport as Support;
    let transport = Arc::new(RecordingTransport::default());
    let adapter = adapter(&transport);
    let own = connection(SESSION, "actor");
    // The child is replaced while the work-session read is in flight: its answer
    // belongs to the old child, so nothing is recorded for the new one.
    transport.respawn_during_call.store(true, Ordering::SeqCst);
    read_work_session(&transport, &adapter, &own, supported());
    assert_eq!(adapter.verification_record_route_support(&own), Support::NotRead);
    // A read the same child answers is recorded.
    read_work_session(&transport, &adapter, &own, supported());
    assert_eq!(adapter.verification_record_route_support(&own), Support::Supported);
}

/// One pass of the tick over the fixture's runs, carrying `cursor` across
/// passes as the tick's own thread does.
fn pass_with(fixture: &RunFixture, cursor: &mut EngramVerificationRecordCursor) -> usize {
    fixture
        .state
        .engram_verification_records_tick_from(ENGRAM_VERIFICATION_RECORD_TICK_BUDGET, &|| false, cursor)
}

#[test]
fn a_deferred_record_is_sent_again_only_once_its_backoff_runs_out() {
    let fixture = RunFixture::new("verification-record-backoff");
    let clock = fixture.state.select_test_scripted_engram_budget_clock();
    let (session, transport) = engram_session(&fixture, true);
    let mut run = record_run(&fixture, "test-record-backoff", commit_request(&fixture.root), commit_results());
    initiated_by(&mut run, &session);
    register(&fixture, &[&run]);
    let busy = || -> std::result::Result<Value, EngramTransportError> {
        Err(EngramTransportError::remote(EngramControlErrorBody {
            code: "busy".to_owned(),
            message: "busy from Engram".to_owned(),
        }))
    };
    let mut cursor = EngramVerificationRecordCursor::default();

    transport.script(Ok(work_supported(&session)));
    transport.script(busy());
    assert_eq!(pass_with(&fixture, &mut cursor), 1);
    assert_eq!(transport.sent_records().len(), 1);
    assert_eq!(
        engram_verification_record_progress(&run.run_dir),
        Some(EngramVerificationRecordProgress::Pending {
            reason: EngramVerificationRecordPending::RemoteDeferred { code: "busy".to_owned() },
        })
    );
    // The very next tick neither sends it again nor looks at it.
    assert_eq!(pass_with(&fixture, &mut cursor), 0);
    assert_eq!(transport.sent_operations(), ["work_session_read", "verification_record"]);

    // Once its first backoff ran out, the same body goes again.
    clock.advance(ENGRAM_VERIFICATION_RECORD_BACKOFF_START);
    transport.script(Ok(work_supported(&session)));
    transport.script(busy());
    assert_eq!(pass_with(&fixture, &mut cursor), 1);
    let sent = transport.sent_records();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0], sent[1], "the same body under the same key");

    // Deferred again, its wait doubled: not after the first wait, after twice it.
    clock.advance(ENGRAM_VERIFICATION_RECORD_BACKOFF_START);
    assert_eq!(pass_with(&fixture, &mut cursor), 0);
    assert_eq!(transport.sent_records().len(), 2);
    clock.advance(ENGRAM_VERIFICATION_RECORD_BACKOFF_START);
    transport.script(Ok(work_supported(&session)));
    transport.script(busy());
    assert_eq!(pass_with(&fixture, &mut cursor), 1);
    assert_eq!(transport.sent_records().len(), 3);
}

#[test]
fn an_unavailable_work_child_backs_off_then_recovers_without_rebinding() {
    let fixture = RunFixture::new("verification-record-refused-backoff");
    let clock = fixture.state.select_test_scripted_engram_budget_clock();
    let (session, transport) = engram_session(&fixture, true);
    bound_by_naming(&fixture, &session);
    let naming_before = naming_state(&fixture, &session);
    let mut run = record_run(&fixture, "test-record-refused-backoff", commit_request(&fixture.root), commit_results());
    initiated_by(&mut run, &session);
    register(&fixture, &[&run]);
    let mut cursor = EngramVerificationRecordCursor::default();

    transport.script(Err(EngramTransportError::transport("ordinary child unavailable")));
    assert_eq!(pass_with(&fixture, &mut cursor), 1);
    assert_eq!(transport.sent_operations(), ["work_session_read"]);

    // A failed ordinary read still obeys sender backoff. A repeated tick
    // cannot supply the missing current child proof.
    assert_eq!(pass_with(&fixture, &mut cursor), 0);
    assert_eq!(transport.sent_operations(), ["work_session_read"]);

    clock.advance(ENGRAM_VERIFICATION_RECORD_BACKOFF_MAX);
    transport.script(Ok(work_supported(&session)));
    transport.script(Ok(receipt_for("failed")));
    assert_eq!(pass_with(&fixture, &mut cursor), 1);
    assert_eq!(
        transport.sent_operations(),
        ["work_session_read", "work_session_read", "verification_record"]
    );
    let sent = transport.sent();
    assert!(sent[2].get("routing_token").is_none(), "{sent:?}");
    assert_eq!(naming_state(&fixture, &session), naming_before,
        "unavailable transport and retry do not mutate naming state");
}

#[test]
fn a_settled_or_rejected_run_is_not_looked_at_again() {
    for (case, ack) in [
        (
            "settled",
            TestRunVerificationAck::Settled {
                receipt: serde_json::from_value(receipt_for("failed")).expect("the receipt reads"),
                body_digest: "0".repeat(64),
            },
        ),
        ("rejected", TestRunVerificationAck::Rejected { detail: "refused".to_owned() }),
    ] {
        let fixture = RunFixture::new(&format!("verification-record-finished-{case}"));
        let (session, transport) = engram_session(&fixture, true);
        let mut run = record_run(&fixture, &format!("test-record-finished-{case}"), commit_request(&fixture.root), commit_results());
        initiated_by(&mut run, &session);
        register(&fixture, &[&run]);
        fixture.state.note_engram_verification_record_ack(&run.registration, ack);
        // Its files are gone: a look would find nothing to read.
        fs::remove_dir_all(&run.run_dir).unwrap();

        assert_eq!(
            fixture
                .state
                .engram_verification_records_tick_within(ENGRAM_VERIFICATION_RECORD_TICK_BUDGET, &|| false),
            0,
            "{case}: the finished run is not selected"
        );
        assert!(transport.sent().is_empty(), "{case}");
    }
}
