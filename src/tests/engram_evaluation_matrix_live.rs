//! Ordinary work through the real router, launcher, Git and pinned Engram.
//! Only provider delivery is simulated. Scheduling gates order real RPCs;
//! they do not mint observations, verification records, verdict receipts or
//! seals. This does not cover real provider pacing or host-process startup.
use super::*;

#[path = "engram_evaluation_matrix_cases.rs"]
mod cases;
#[path = "engram_evaluation_matrix_claude.rs"]
mod claude_frames;
#[path = "engram_evaluation_matrix_decisions.rs"]
mod decisions;
#[path = "engram_evaluation_matrix_engram_launcher.rs"]
mod engram_launcher;
#[path = "engram_evaluation_matrix_inflight.rs"]
mod inflight;
#[path = "engram_evaluation_matrix_policy.rs"]
mod policy_off;
#[path = "engram_evaluation_matrix_runtime.rs"]
mod runtime_events;
#[path = "engram_committed_consumers_live.rs"]
mod committed_consumers;
use runtime_events::{CommandEvents, Provider};

/// Pause one genuine closing sighting BEFORE it reaches the producer. The
/// other requests still use the same real per-session control transport.
struct ClosingGateTransport {
    inner: Arc<BoundaryFaultTransport>,
    gate: Mutex<Option<(mpsc::Sender<Value>, mpsc::Receiver<()>)>>,
}

struct ClosingGate {
    entered: mpsc::Receiver<Value>,
    release: Mutex<Option<mpsc::Sender<()>>>,
}

impl ClosingGate {
    fn entered(&self) -> Value {
        self.entered
            .recv_timeout(DEADLOCK_GUARD)
            .expect("production closing worker reaches its actual source-sighting RPC")
    }

    fn release(&self) {
        let release = self.release.lock().unwrap().take().unwrap();
        release.send(()).unwrap();
    }
}

impl Drop for ClosingGate {
    fn drop(&mut self) {
        // A failed assertion must not leave a production worker holding the
        // test root forever. The failed root itself is retained by the fixture.
        if let Some(release) = self.release.get_mut().unwrap().take() {
            let _ = release.send(());
        }
    }
}

impl ClosingGateTransport {
    fn arm(&self) -> ClosingGate {
        let (entered, observed) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let old = self.gate.lock().unwrap().replace((entered, released));
        assert!(old.is_none(), "one ordered RPC gate at a time");
        ClosingGate {
            entered: observed,
            release: Mutex::new(Some(release)),
        }
    }
}

impl EngramControlTransport for ClosingGateTransport {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        let value = serde_json::to_value(request).unwrap();
        let gate = if value["operation"] == "execution_observe"
            && value["occurrence"]["kind"] == "source_sighting"
        {
            self.gate.lock().unwrap().take()
        } else {
            None
        };
        if let Some((entered, released)) = gate {
            entered.send(value).unwrap();
            released
                .recv_timeout(DEADLOCK_GUARD)
                .expect("test releases the closing RPC");
        }
        self.inner.request(connection, request, timeout)
    }

    fn read_work_binding(
        &self,
        connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>,
        timeout: Duration,
    ) -> Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.inner
            .read_work_binding(connection, preference, timeout)
    }

    fn read_held_claims(
        &self,
        connection: &EngramConnectionConfig,
        timeout: Duration,
    ) -> Result<EngramHeldClaims, EngramTransportError> {
        self.inner.read_held_claims(connection, timeout)
    }

    fn connection_generation(&self, session: &str) -> Option<u64> {
        self.inner.connection_generation(session)
    }

    fn shutdown_session(&self, session: &str) {
        self.inner.shutdown_session(session);
    }

    fn shutdown_session_until(
        &self,
        session: &str,
        clock: &EngramBudgetClock,
        deadline: std::time::Instant,
    ) -> Result<(), EngramTransportError> {
        self.inner.shutdown_session_until(session, clock, deadline)
    }
}

#[derive(Clone, Copy, Debug)]
enum Citation {
    BoundVerification,
    UnboundVerification,
    UnboundNote,
}

struct ActualCheck {
    run_id: String,
    directory: PathBuf,
    request: Value,
    results: Value,
    output: String,
    exit: i32,
    records: Vec<String>,
}

struct Matrix {
    fixture: CompletionFixture,
    holder: String,
    work: String,
    note: String,
    generic_gate: Option<String>,
    provider: Provider,
    citation: Citation,
    transport: Arc<ClosingGateTransport>,
    service: LoopbackRegistration,
    _reader: LiveRootCleanup,
    _holder: LiveRootCleanup,
}

impl Matrix {
    fn new(citation: Citation, claude: bool) -> Self {
        let fixture = completion_fixture();
        install_launcher(&fixture);
        Self::from_fixture(fixture, citation, claude)
    }

    fn from_fixture(mut fixture: CompletionFixture, citation: Citation, claude: bool) -> Self {
        // The general state fixture deliberately disables mailbox storage.
        // Enable this fixture's own store before the HTTP service clones state,
        // so ordinary mail wakes exercise the real persistence path.
        let mailbox_path =
            resolve_coordination_persistence_path(fixture.live.state.persistence_path.as_ref());
        fixture.live.state.mailbox_store = Arc::new(MailboxStore::open(&mailbox_path).unwrap());
        assert!(fixture
            .live
            .state
            .mailbox_store
            .connection_if_enabled()
            .is_some());
        fixture.command(
            &fixture.live.session_id,
            &[
                "control-policy",
                "set-acceptance-evaluation",
                "--modes",
                "independent-session",
                "--mechanical-basis",
                "observed",
                "--require-source-freshness",
                "--authorized-by",
                "disposable-evaluation-matrix",
                "--idempotency-key",
                "matrix-policy",
            ],
            false,
        );
        let (holder, provider) = Provider::install(&fixture, claude);
        let mut add = vec![
            "add",
            "Ordinary checked work survives unchanged turns",
            "--accept",
            "The intended current source was checked",
            "--json",
        ];
        if matches!(citation, Citation::BoundVerification) {
            add.extend(["--bind", "1=test"]);
        }
        if matches!(citation, Citation::UnboundNote) {
            add.extend(["--accept", "The recorded holder review is complete"]);
        }
        let added = fixture.work(&holder, &add);
        let work = added["work"]["short_ref"]
            .as_str()
            .or_else(|| added["short_ref"].as_str())
            .unwrap()
            .to_owned();
        fixture.work(&holder, &["claim", &work, "--json"]);
        let noted = fixture.work(&holder, &["note", &work,
            "Holder evidence: the disposable work will be checked by the real registered launcher", "--status", "--json"]);
        let note = noted["evidence"]
            .as_str()
            .expect("real holder-issued note locator")
            .to_owned();
        let generic_gate = matches!(citation, Citation::UnboundNote).then(|| {
            let recorded = fixture.work(
                &holder,
                &["gate", "holder-review", "--work-ref", &work, "--json"],
            );
            recorded["evidence"]
                .as_str()
                .expect("real generic gate locator")
                .to_owned()
        });
        fixture
            .live
            .state
            .name_engram_source_root(
                &holder,
                EngramSourceRootRequest {
                    work: work.clone(),
                    path: Some(Some(fixture.root.to_string_lossy().into_owned())),
                },
            )
            .unwrap();
        let transport = Arc::new(ClosingGateTransport {
            inner: fixture.live.transport.clone(),
            gate: Mutex::new(None),
        });
        fixture
            .live
            .state
            .install_test_engram_transport(transport.clone());
        let service = LoopbackRegistration::start(&fixture.live.state);
        let _reader = LiveRootCleanup {
            transport: fixture.live.transport.clone(),
            session_id: WORK_HOST_READER_SESSION_ID.to_owned(),
        };
        let _holder = LiveRootCleanup {
            transport: fixture.live.transport.clone(),
            session_id: holder.clone(),
        };
        Self {
            fixture,
            holder,
            work,
            note,
            generic_gate,
            provider,
            citation,
            transport,
            service,
            _reader,
            _holder,
        }
    }

    fn begin(&self, text: &str) {
        let dispatch = dispatch_live_root(&self.fixture.live.state, &self.holder, text, None);
        deliver_turn_dispatch(&self.fixture.live.state, dispatch).unwrap();
        self.provider.receive(&self.fixture, &self.holder);
        assert!(self.fixture.record(&self.holder, |row| {
            row.engram.unadmitted_active_turn.is_some()
        }));
    }

    fn close(&self) {
        finish_live_root(&self.fixture.live.state, &self.holder);
    }

    fn settle(&self) {
        self.fixture.wait_for_reports(&self.holder);
    }

    fn mail_wake(&self) {
        let sender = create_test_project_session(
            &self.fixture.live.state,
            Agent::Codex,
            &self.fixture.project_id,
            &self.fixture.root,
        );
        let (status, _body) = self.http(
            &sender,
            "mailboxes/send",
            &json!({
                "targetSessionId":self.holder,"message":"Continue ordinary work after this turn",
                "idempotencyKey":format!("matrix-mail-{}",Uuid::new_v4()),"class":"routine"
            }),
        );
        assert!(status.is_success(), "real ordinary mail wake: <body withheld>");
    }

    fn edit(&self, value: &str) {
        fs::write(self.fixture.root.join("README.md"), value).unwrap();
    }

    fn commit(&self) {
        run_git_test_command(
            &self.fixture.root,
            &["add", "--", "README.md", "src/lib.rs"],
        );
        run_git_test_command(
            &self.fixture.root,
            &["commit", "--quiet", "-m", "fixture intended source"],
        );
    }

    /// Report actual provider events around an actual launcher process. A
    /// delayed completion preserves the original bytes, exit and run locator.
    fn launch(&self, codex_pushd: bool) -> (CommandEvents, ActualCheck) {
        let key = format!("matrix-check-{}", Uuid::new_v4());
        let mut events = self
            .provider
            .events(&self.fixture, &self.holder, &key, codex_pushd);
        events.start();
        self.fixture.wait_for_check_capture(&self.holder);
        let output = isolated_launcher(&self.fixture.root, &self.holder, &self.service)
            .output()
            .expect("actual isolated launcher starts");
        (events, self.actual_check_output(output))
    }

    fn actual_check_output(&self, output: std::process::Output) -> ActualCheck {
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let locator = text
            .lines()
            .find_map(|line| {
                line.strip_prefix("RUN ")
                    .map(|path| (path, false))
                    .or_else(|| line.strip_prefix("STARTED ").map(|path| (path, true)))
            })
            .unwrap_or_else(|| panic!("actual launcher prints its run locator: {text}"));
        if locator.1 {
            // Engram's ordinary foreground launcher announces STARTED, then
            // waits in this process and prints its terminal result. A detached
            // startup receipt alone is not a completed check.
            assert!(
                text.lines().any(|line| line.starts_with("pid=")
                    && line.ends_with(" completion=host-process-wait")),
                "foreground completion is required: {text}"
            );
        }
        let directory = PathBuf::from(locator.0);
        let request: Value =
            serde_json::from_slice(&fs::read(directory.join("request.json")).unwrap()).unwrap();
        let results: Value =
            serde_json::from_slice(&fs::read(directory.join("results.json")).unwrap()).unwrap();
        assert!(
            matches!(results["state"].as_str(), Some("passed" | "failed")),
            "actual terminal result required"
        );
        assert_eq!(results["runId"], request["runId"]);
        assert_eq!(
            results["exitCode"].as_i64(),
            output.status.code().map(i64::from)
        );
        assert_eq!(request["owner"], self.holder);
        assert_eq!(request["input"], "commit");
        assert_eq!(
            fs::canonicalize(directory.parent().unwrap()).unwrap(),
            fs::canonicalize(EngramHost::run_artifact_directory(&self.fixture.root).unwrap())
                .unwrap()
        );
        ActualCheck {
            run_id: request["runId"].as_str().unwrap().to_owned(),
            directory,
            request,
            results,
            output: text,
            exit: output.status.code().expect("native launcher exit"),
            records: Vec::new(),
        }
    }

    fn receive_check(&self, mut events: CommandEvents, check: &mut ActualCheck, placed: bool) {
        events.complete(&check.output, check.exit);
        if placed {
            self.fixture.wait_for_check_capture(&self.holder);
        }
        self.settle_check(check);
    }

    fn settle_check(&self, check: &mut ActualCheck) {
        self.read_check_receipt(check, false);
    }

    fn receive_unplaced_check(&self, mut events: CommandEvents, check: &mut ActualCheck) {
        events.complete(&check.output, check.exit);
        self.read_check_receipt(check, true);
    }

    fn persisted_verification(&self, check: &ActualCheck) -> Option<TestRunVerificationAck> {
        let loaded = load_state(self.fixture.live.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        loaded.test_run_registrations[&check.run_id]
            .verification_record
            .clone()
    }

    fn stored_objects(&self) -> Vec<(String, Value)> {
        let database = self
            .fixture
            .live
            .state
            .inner
            .lock()
            .unwrap()
            .find_project(&self.fixture.project_id)
            .unwrap()
            .engram
            .as_ref()
            .unwrap()
            .authority_store_key
            .as_ref()
            .unwrap()
            .database_path
            .clone();
        assert!(database.starts_with(&self.fixture.home));
        let connection = rusqlite::Connection::open_with_flags(
            database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut query = connection
            .prepare("SELECT object_id, canonical_json FROM objects")
            .unwrap();
        let objects = query
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
            })
            .unwrap()
            .map(|row| {
                let (id, bytes) = row.unwrap();
                (id, serde_json::from_slice(&bytes).unwrap())
            })
            .collect();
        objects
    }

    fn assert_objects_unchanged(&self, originals: Vec<(String, Value)>) {
        for (id, original) in originals {
            assert!(
                self.fixture.object(&id) == original,
                "new decisions must preserve every earlier immutable object: {id}"
            );
        }
    }

    fn read_check_receipt(&self, check: &mut ActualCheck, unassociated: bool) {
        // Test builds do not spawn the periodic scan/verification threads.
        // Drive their production entries; never prepare/send/settle a record
        // or associate evidence by hand (the older fixture does that).
        self.fixture.live.state.refresh_test_runs();
        self.fixture.live.state.engram_verification_records_tick();
        let loaded = load_state(self.fixture.live.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        let row = &loaded.test_run_registrations[&check.run_id];
        let clean_terminal = TestRunCommittedInput::read(&check.results).is_some_and(|input| {
            input.clean_before
                && input.outcome == Some(TestRunCommittedInputOutcome::Clean)
                && input.after.as_ref().is_some_and(|after| after.clean)
        }) && check.results["interrupted"] != true
            && matches!(check.results["state"].as_str(), Some("passed" | "failed"));
        if clean_terminal {
            let receipt = match (&row.verification_record, unassociated) {
                (Some(TestRunVerificationAck::Settled { receipt, .. }), false) => receipt,
                (
                    Some(TestRunVerificationAck::Received {
                        receipt,
                        body_digest,
                    }),
                    true,
                ) => {
                    // A real producer receipt is not a durable launch-row
                    // association. Keep Received visible and exercise what
                    // evaluation and completion actually do with its records.
                    assert!(!body_digest.is_empty());
                    let inner = self.fixture.live.state.inner.lock().unwrap();
                    assert!(
                        inner
                            .test_run_observations
                            .index()
                            .artifact_run(&check.run_id)
                            .next()
                            .is_none(),
                        "this row must not invent an ordinary launch observation"
                    );
                    receipt
                }
                _ => panic!(
                    "expected actual {} receipt, not synthetic settlement: {:?}; actual run={}",
                    if unassociated {
                        "unassociated Received"
                    } else {
                        "durably Settled"
                    },
                    row.verification_record,
                    check.output
                ),
            };
            check.records = receipt
                .verifications
                .iter()
                .map(|entry| entry.record_id.clone())
                .collect();
            assert!(!check.records.is_empty());
            for record in &check.records {
                let original = self.fixture.object(record);
                assert_eq!(
                    original["result"],
                    if check.exit == 0 { "passed" } else { "failed" },
                    "a clean failing invocation records failure, never passed credit"
                );
                let source: EngramSourceId = serde_json::from_value(
                    check.results["sourceIdBefore"].clone()).unwrap();
                source.validate(std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET).unwrap();
                assert!(check.results["sourceIdBefore"] == check.results["sourceIdAfter"],
                    "the actual check preserves its complete owner identity");
                assert!(original["source_basis"]["source_id"] ==
                    serde_json::to_value(&source).unwrap(),
                    "the real producer retains the complete launcher input");
                assert!(original["source_basis"].get("source_revision").is_none(),
                    "a fresh owner record has no revision-string surrogate");
            }
        } else {
            assert!(
                !matches!(
                    row.verification_record,
                    Some(TestRunVerificationAck::Settled { .. })
                ),
                "a dirty or incomplete input cannot mint registered evidence"
            );
        }
        eprintln!(
            "MATRIX_ACTUAL_CHECK {}",
            json!({"run": check.run_id, "state": check.results["state"],
            "eligible": check.results["input"]["eligible"],
            "settled": matches!(row.verification_record, Some(TestRunVerificationAck::Settled { .. }))})
        );
    }

    fn check(&self, codex_pushd: bool) -> ActualCheck {
        let (events, mut check) = self.launch(codex_pushd);
        self.receive_check(events, &mut check, true);
        assert_eq!(check.exit, 0, "{}", check.output);
        assert_eq!(check.results["input"]["eligible"], true);
        check
    }

    fn http(&self, session: &str, endpoint: &str, body: &Value) -> (StatusCode, Value) {
        let response = reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(format!(
                "{}/api/sessions/{session}/{endpoint}",
                self.service.url
            ))
            .timeout(DEADLOCK_GUARD)
            .json(body)
            .send()
            .expect("real fixture HTTP response");
        let status = response.status();
        let text = response.text().unwrap();
        let value = serde_json::from_str(&text).unwrap_or_else(|_| {
            panic!("real endpoint returned non-JSON ({status})")
        });
        (status, value)
    }

    fn cited(&self, check: &ActualCheck) -> String {
        match self.citation {
            Citation::UnboundNote => self.note.clone(),
            _ => check
                .records
                .first()
                .expect("actual producer-issued verification citation")
                .clone(),
        }
    }

    fn criterion_citations(&self, check: &ActualCheck) -> Vec<(usize, String)> {
        let mut citations = vec![(1, self.cited(check))];
        if let Some(gate) = &self.generic_gate {
            citations.push((2, gate.clone()));
        }
        citations
    }

    fn request(&self, check: &ActualCheck) -> Result<(DelegationRecord, LiveRootCleanup), Value> {
        let citations: Vec<_> = self
            .criterion_citations(check)
            .into_iter()
            .map(|(criterion, locator)| json!({"criterion":criterion,"locators":[locator]}))
            .collect();
        let request = json!({
            "workRef": self.work, "agent": Agent::Codex,
            "criterionEvidence": citations
        });
        let _: RequestAcceptanceEvaluationRequest = serde_json::from_value(request.clone())
            .expect("fixture request must decode as the production HTTP request type");
        let (status, body) = self.http(&self.holder, "acceptance-evaluations", &request);
        eprintln!(
            "MATRIX_EVALUATION_REQUEST {}",
            json!({"status":status.as_u16(),"bodyBytes":serde_json::to_vec(&body).unwrap().len()})
        );
        if !status.is_success() {
            return Err(body);
        }
        let child = self
            .fixture
            .live
            .state
            .inner
            .lock()
            .unwrap()
            .delegations
            .iter()
            .rev()
            .find(|row| {
                row.parent_session_id == self.holder
                    && row
                        .acceptance_evaluation
                        .as_ref()
                        .is_some_and(|target| target.work_ref == self.work)
            })
            .expect("real endpoint created its evaluator delegation")
            .clone();
        assert_ne!(child.child_session_id, self.holder);
        assert_eq!(child.write_policy, DelegationWritePolicy::ReadOnly);
        let target = child.acceptance_evaluation.as_ref().unwrap();
        assert_eq!(target.mode, AcceptanceEvaluationMode::IndependentSession);
        assert!(
            target.source_id.is_some() && target.source_fingerprint.is_none(),
            "unmeasured source cannot supply the positive witness"
        );
        assert!(target.source_id.as_ref() == Some(&self.source_id_now()),
            "the request retains its complete fresh owner identity privately");
        assert!(!child.prompt.contains("\"changes\":[") && !child.prompt.contains("\"submodules\":["),
            "the evaluator receives bounded summaries, not the identity object");
        let cleanup = LiveRootCleanup {
            transport: self.fixture.live.transport.clone(),
            session_id: child.child_session_id.clone(),
        };
        Ok((child, cleanup))
    }

    fn submit(&self, child: &DelegationRecord, check: &ActualCheck) -> (StatusCode, Value) {
        let target = child.acceptance_evaluation.as_ref().unwrap();
        let verdicts: Vec<_> = self.criterion_citations(check).into_iter().map(|(criterion, locator)|
            json!({"criterion":criterion,"verdict":"pass","basis":
                if matches!(self.citation, Citation::UnboundNote) { "judgment" } else { "observed" },
                "rationale":"The actual registered launcher checked this input; ordinary unchanged turns preserve it",
                "evidence":[locator]})).collect();
        let result = self.http(
            &child.child_session_id,
            "acceptance-evaluation",
            &json!({
                "schemaVersion": ACCEPTANCE_EVALUATION_SUBMISSION_SCHEMA_VERSION,
                "attemptKey": target.attempt_key,
                "verdicts": verdicts
            }),
        );
        eprintln!(
            "MATRIX_EVALUATION_SUBMISSION {}",
            json!({"status":result.0.as_u16(),"bodyBytes":serde_json::to_vec(&result.1).unwrap().len()})
        );
        result
    }

    fn done(&self) -> Value {
        self.done_after_decision(|| {})
    }

    fn done_after_decision(&self, after_decision: impl FnOnce()) -> Value {
        // The holder declares a fresh complete owner identity, independently
        // of the evaluator's saved declaration and the earlier check result.
        let source = self.source_id_now();
        let output = self.done_output_source(&source);
        assert!(output.status.success(), "ordinary checked input must complete");
        let done: Value = serde_json::from_slice(&output.stdout)
            .expect("successful completion returns its actual receipt");
        // The real done command has returned its decision. A delayed report
        // need not remain blocked during the independent post-seal reads.
        after_decision();
        let shown = self
            .fixture
            .work(&self.holder, &["show", &self.work, "--json"]);
        assert_eq!(
            shown["status"]["work"]["lifecycle"], "completed",
            "<shown withheld>"
        );
        let core = self
            .fixture
            .work(&self.holder, &["core", "inspect", &self.work, "--json"]);
        assert_eq!(core["run"]["state"], "completed", "<core withheld>");
        let seal = core["run"]["completion_seal"]
            .as_str()
            .expect("completion ends in a real seal");
        let stored_seal = self.fixture.object(seal);
        assert!(
            stored_seal.is_object(),
            "the producer retains the actual completion seal"
        );
        eprintln!(
            "MATRIX_COMPLETED_SEAL {}",
            json!({"seal":seal,"source":acceptance_attempt_source_label(Some(&source),None)})
        );
        done
    }

    fn assert_checked_source(&self, check: &ActualCheck) {
        assert_eq!(check.exit, 0, "{}", check.output);
        assert_eq!(check.results["state"], "passed");
        assert_eq!(check.results["input"]["eligible"], true);
        let checked: EngramSourceId = serde_json::from_value(check.results["sourceIdBefore"].clone()).unwrap();
        assert!(checked == self.source_id_now(),
            "a positive decision requires the exact checked root and direct-child identities");
    }

    fn source_id_now(&self) -> EngramSourceId {
        let deadline = std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET;
        let owner = self.fixture.live.state.source_owner_metadata_until(&self.holder, deadline)
            .expect("the live matrix pins its actual source owner");
        engram_source_id_until(&owner, &self.fixture.root, deadline)
            .expect("the actual owner captures the complete current identity")
    }

    fn evaluate(&self, check: &ActualCheck, during: impl FnOnce()) -> (String, Value) {
        self.assert_checked_source(check);
        let (child, _cleanup) = self.request(check).unwrap_or_else(|_| {
            panic!("correct ordinary checked input must reach an independent evaluator: <details withheld>")
        });
        during();
        self.assert_checked_source(check);
        let (status, body) = self.submit(&child, check);
        assert!(
            status.is_success(),
            "correct ordinary checked input must be accepted: <body withheld>"
        );
        let evaluation_id = body["receipt"]["evaluation"]["evaluation"]
            .as_str()
            .expect("real producer-issued evaluation identity")
            .to_owned();
        let evaluation = self.fixture.object(&evaluation_id);
        let recorded: AcceptanceSourceBasis = serde_json::from_value(evaluation["source_basis"].clone())
            .expect("the stored evaluation retains its typed declared source");
        let AcceptanceSourceBasis::Shared(recorded) = recorded else {
            panic!("a new evaluation cannot retain a legacy fingerprint");
        };
        assert!(Some(&recorded.source_id) == child.acceptance_evaluation.as_ref().unwrap().source_id.as_ref(),
            "the consumer stores the exact complete identity from the host attempt");
        assert!(recorded.source_id == self.source_id_now(),
            "the live consumer's evaluated source matches the current full identity");
        let saved = self
            .fixture
            .live
            .state
            .inner
            .lock()
            .unwrap()
            .delegations
            .iter()
            .find(|row| row.id == child.id)
            .unwrap()
            .acceptance_evaluation
            .clone()
            .unwrap();
        assert!(matches!(
            saved.submission,
            AcceptanceEvaluationSubmission::Recorded { .. }
        ));
        (evaluation_id, evaluation)
    }

    fn done_output(&self, check: &ActualCheck) -> std::process::Output {
        // Present the formerly checked complete identity. Engram must inspect
        // the root now rather than trust this stale declaration or wait for a
        // turn report to reveal the edit.
        let checked: EngramSourceId = serde_json::from_value(check.results["sourceIdBefore"].clone())
            .expect("the actual check retained its complete input identity");
        self.done_output_source(&checked)
    }

    fn done_output_source(&self, source: &EngramSourceId) -> std::process::Output {
        let file = AcceptanceSourcePayload {
            source_id: source.clone(),
            source_file: None,
        }
        .publish(&self.fixture.home.join("completion-sources"))
        .expect("the fixture retains an immutable completion declaration");
        let output = self.fixture.command_output(
            &self.holder,
            &[
                "done",
                &self.work,
                "Attempt completion on the previously checked input",
                "--source-id-file",
                file.path().to_str().unwrap(),
                "--json",
            ],
            true,
        );
        // Diagnostics expose only closed categories, never a source payload,
        // locator, remote free text or the returned body's private fields.
        let body: Value = serde_json::from_slice(&output.stdout)
            .or_else(|_| serde_json::from_slice(&output.stderr))
            .unwrap_or(Value::Null);
        let code = body["error"]["code"].as_str().or_else(|| body["code"].as_str());
        let category = match code {
            Some("work_completion_refused") => "completion_refused",
            Some("acceptance_evaluation_stale") => "evaluation_stale",
            Some("missing_acceptance_evaluation") => "missing_evaluation",
            _ if output.status.success() => "completed",
            _ => "unclassified_failure",
        };
        eprintln!("MATRIX_DONE_DECISION {}", json!({"category":category,"exit":output.status.code()}));
        if output.status.success() {
            file.terminal();
        }
        output
    }

    fn refuse_done(&self, check: &ActualCheck, evaluation_recorded: bool) {
        let output = self.done_output(check);
        eprintln!(
            "MATRIX_DONE_REFUSAL exit={:?} stdout_bytes={} stderr_bytes={}",
            output.status.code(),
            output.stdout.len(),
            output.stderr.len()
        );
        self.assert_completion_refusal(&output, evaluation_recorded);
        let core = self
            .fixture
            .work(&self.holder, &["core", "inspect", &self.work, "--json"]);
        assert!(core["run"]["completion_seal"].is_null(), "<core withheld>");
    }

    fn accept(&self, check: &ActualCheck, during: impl FnOnce()) {
        self.accept_after_done(check, during, || {});
    }

    fn accept_after_done(
        &self,
        check: &ActualCheck,
        during: impl FnOnce(),
        after_decision: impl FnOnce(),
    ) {
        let originals: Vec<_> = check
            .records
            .iter()
            .map(|id| self.fixture.object(id))
            .collect();
        let (evaluation_id, evaluation) = self.evaluate(check, during);
        self.assert_checked_source(check);
        self.done_after_decision(after_decision);
        assert!(
            self.fixture.object(&evaluation_id) == evaluation,
            "done never rewrites its evaluation"
        );
        for (id, original) in check.records.iter().zip(originals) {
            assert!(
                self.fixture.object(id) == original,
                "completion never rewrites old verification"
            );
        }
    }

    fn refuse(&self, check: &ActualCheck, during: impl FnOnce()) {
        match self.request(check) {
            Err(error) => Self::assert_source_refusal(&error),
            Ok((child, _cleanup)) => {
                during();
                let (status, body) = self.submit(&child, check);
                assert!(
                    !status.is_success(),
                    "real post-check source change must be refused: <body withheld>"
                );
                Self::assert_source_refusal(&body);
                // A refused submission leaves the evaluator's provider turn
                // running. Model its instructed final refusal report before
                // requesting a fresh judgment later in the same fixture.
                let mut recorder = SessionRecorder::new(
                    self.fixture.live.state.clone(),
                    child.child_session_id.clone(),
                );
                recorder
                    .push_text("Acceptance submission refused; the parent must read the recorded bounded diagnostic.")
                    .unwrap();
                finish_live_root(&self.fixture.live.state, &child.child_session_id);
                let finished = self
                    .fixture
                    .live
                    .state
                    .get_delegation(&self.holder, &child.id)
                    .unwrap();
                assert_eq!(finished.delegation.status, DelegationStatus::Completed);
                assert!(
                    !matches!(
                        finished
                            .delegation
                            .acceptance_evaluation
                            .as_ref()
                            .unwrap()
                            .submission,
                        AcceptanceEvaluationSubmission::Recorded { .. }
                    ),
                    "finishing a refused evaluator must not fabricate an evaluation"
                );
            }
        }
        let shown = self
            .fixture
            .work(&self.holder, &["show", &self.work, "--json"]);
        assert_eq!(shown["status"]["work"]["lifecycle"], "open");
    }

    fn assert_source_refusal(body: &Value) {
        let text = body.to_string().to_ascii_lowercase();
        assert!(
            text.contains("source")
                || text.contains("worktree")
                || text.contains("input")
                || text
                    .contains("registered check's named root is unavailable or has been rebound"),
            "must preserve a real source/input refusal, not a CLI/fixture/setup failure: <body withheld>"
        );
        assert!(
            !text.contains("invalid request") && !text.contains("unknown argument"),
            "<body withheld>"
        );
    }

    fn assert_completion_refusal(&self, output: &std::process::Output, evaluation_recorded: bool) {
        assert!(
            !output.status.success(),
            "an unchecked input cannot complete"
        );
        let body: Value = serde_json::from_slice(&output.stdout)
            .or_else(|_| serde_json::from_slice(&output.stderr))
            .unwrap_or_else(|_| {
                panic!("ordinary done must return its typed refusal: <details withheld>; <details withheld>")
            });
        if body["error"]["code"] == "work_completion_refused" {
            assert!(evaluation_recorded, "<body withheld>");
            assert_eq!(body["error"]["details"]["work_ref"], self.work, "<body withheld>");
            let reason = body["error"]["details"]["reason"]
                .as_str()
                .expect("the completion refusal names its actual cause");
            assert!(
                (reason.starts_with("registered root ") && reason.contains(" is dirty;"))
                    || (reason.starts_with("registered check ran on git-commit:")
                        && reason.contains(", but the named root is at git-commit:"))
                    || reason == "the named root's full source_id changed since the registered check; run a check on the current source state",
                "a measured dirty or different-commit refusal: <body withheld>"
            );
            return;
        }
        assert_eq!(body["operation"], "done", "<body withheld>");
        assert_eq!(body["recovery"]["item"]["ref"], self.work, "<body withheld>");
        let cause = &body["recovery"]["cause"];
        if evaluation_recorded {
            assert_eq!(body["code"], "acceptance_evaluation_stale", "<body withheld>");
            assert_eq!(cause["kind"], "acceptance_evaluation_stale", "<body withheld>");
            assert!(
                matches!(
                    cause["reason"].as_str(),
                    Some("source" | "mutation" | "unadmitted_change" | "verification_source")
                ),
                "a source barrier, not an unrelated policy/setup refusal: <body withheld>"
            );
        } else {
            assert_eq!(body["code"], "missing_acceptance_evaluation", "<body withheld>");
            assert_eq!(cause["kind"], "missing_acceptance_evaluation", "<body withheld>");
        }
    }
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn current_source_id_matrix_tick_retains_actual_receipt_and_basis_without_evaluation() {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    matrix.begin("Run a real check and retain its full owner-backed evidence.");
    let check = matrix.check(false);
    matrix.assert_checked_source(&check);
    assert!(!check.records.is_empty(), "the production tick returned actual producer receipts");
    matrix.close();
    matrix.settle();
}
