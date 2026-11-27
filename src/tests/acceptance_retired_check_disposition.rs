// Explicit retired-check exclusions admit judgment, never verification credit.
// Exercise the real request/spawn boundary with disposable Git worktrees and
// scripted tracker acknowledgements; no live store or coordinator is used.
use super::*;

const DISPOSITION_REASON: &str = "The historical check is irrelevant to this replacement root.";
const DISPOSITION_RECORD: &str = "dddddddddddddddddddddddddddddddd";

struct RetiredCheckFixture {
    state: AppState,
    parent: String,
    original_root: String,
    current_root: String,
    registration: TestRunRegistration,
    transport: Arc<ScriptedEngramControlTransport>,
}

fn retired_check_fixture(outcome: &str) -> RetiredCheckFixture {
    let (state, parent, store, original_root) = named_root_parent();
    let head = commit_named_root(&original_root);
    let registration = register_committed_run(
        &state,
        &parent,
        &original_root,
        committed_results(outcome, input_record(&head, "clean")),
    );
    host_observed(&state, &registration, &head, acknowledged("settled"));
    let registration = current_registration(&state);

    // A different worktree, not merely a recovered generation of the same
    // directory. The original request and verification remain untouched.
    let replacement = worktree(&original_root)
        .parent()
        .unwrap()
        .join("replacement");
    run_git_test_command(
        &worktree(&original_root),
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            replacement.to_str().unwrap(),
            &head,
        ],
    );
    let current_root = engram_exact_path_key(&replacement);
    {
        let mut inner = state.inner.lock().unwrap();
        inner.engram_work_source_roots[0].root = current_root.clone();
    }
    rename_same_workspace(&state, &parent, &store);
    acceptance_owner_reports(&state, &parent, &current_root, Vec::new(), Vec::new());
    let mut answer = sighting_reply(&current_root, 2, present_sighting());
    answer["current_binding"] = json!("fixture-renamed-event");
    answer["root"]["binding_event"] = json!("fixture-renamed-event");
    answer["root"]["binding_position"] = json!(2);
    let transport = install_sighting_answer(&state, Ok(answer));
    RetiredCheckFixture {
        state,
        parent,
        original_root,
        current_root,
        registration,
        transport,
    }
}

fn disposition_request(run_id: &str) -> RequestAcceptanceEvaluationRequest {
    serde_json::from_value(json!({
        "workRef": "w-task",
        "agent": "Codex",
        "retiredCheckDispositions": [{"runId": run_id, "reason": DISPOSITION_REASON}]
    }))
    .expect("an explicit, attributed retired-check disposition is a supported request")
}

fn disposition_reader(
    calls: RecordedEngramCalls,
    write_error: Option<EngramTransportError>,
) -> impl Fn(&EngramConnectionConfig, &[String], Duration) -> Result<Value, EngramTransportError> {
    let ordinary = independent_reader();
    move |connection, args, timeout| {
        calls
            .lock()
            .unwrap()
            .push((connection.clone(), args.to_vec()));
        if args.iter().any(|arg| arg == "note") {
            if let Some(error) = &write_error {
                return Err(error.clone());
            }
            Ok(json!({"operation":"note", "evidence":DISPOSITION_RECORD,
                "work":{"short_ref":"w-task", "work_id":SIGHTED_WORK_ID,
                    "revision":7,"lifecycle":"open"}}))
        } else {
            ordinary(connection, args, timeout)
        }
    }
}

fn assert_original_check_unchanged(fixture: &RetiredCheckFixture, body: &[u8]) {
    let current = current_registration(&fixture.state);
    assert_eq!(current.named_root, fixture.registration.named_root);
    assert_eq!(current.work_binding, fixture.registration.work_binding);
    assert_eq!(
        current.verification_record,
        fixture.registration.verification_record
    );
    assert_eq!(
        fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_run_registrations
            .len(),
        1
    );
    assert_eq!(
        fs::read(PathBuf::from(&current.run_directory).join(ENGRAM_VERIFICATION_RECORD_BODY_FILE))
            .unwrap(),
        body,
        "an exclusion never rewrites or relabels the original verification"
    );
}

#[test]
fn a_different_path_without_a_disposition_still_refuses_before_spawning() {
    let fixture = retired_check_fixture("passed");
    let error = refusal(
        &fixture.state,
        request(&fixture.state, &fixture.parent),
        "different path",
    );
    assert!(
        error.message.starts_with("registered_check_root_rebound"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains(&fixture.original_root),
        "{}",
        error.message
    );
    assert!(
        error.message.contains(&fixture.current_root),
        "{}",
        error.message
    );
    assert_eq!(sighting_reads(&fixture.transport), 0);
}

#[test]
fn an_acknowledged_retired_check_disposition_spawns_and_discloses_the_original_outcome() {
    for outcome in ["passed", "failed", "unknown"] {
        let fixture = retired_check_fixture(if outcome == "unknown" {
            "passed"
        } else {
            outcome
        });
        if outcome == "unknown" {
            // The historical acknowledgement remains, but its terminal
            // artifact can no longer be read. An exclusion must disclose
            // that uncertainty instead of borrowing the old pass.
            fs::remove_file(
                PathBuf::from(&fixture.registration.run_directory).join("results.json"),
            )
            .unwrap();
        }
        let body = fs::read(
            PathBuf::from(&fixture.registration.run_directory)
                .join(ENGRAM_VERIFICATION_RECORD_BODY_FILE),
        )
        .unwrap();
        assert_eq!(
            decide(&fixture.state, &fixture.parent).unwrap_err().code(),
            "registered_check_root_rebound",
            "the real retired-root refusal is present first"
        );
        let calls: RecordedEngramCalls = Arc::default();
        let response = fixture
            .state
            .request_acceptance_evaluation_with_runner(
                &fixture.parent,
                disposition_request(COMMITTED_RUN_ID),
                disposition_reader(calls.clone(), None),
            )
            .unwrap_or_else(|error| panic!("{outcome}: {}", error.message));
        let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } = response else {
            panic!("a recorded disposition permits independent judgment, not completion");
        };
        let record = delegation.delegation;
        for required in [
            COMMITTED_RUN_ID,
            outcome,
            DISPOSITION_REASON,
            DISPOSITION_RECORD,
            fixture.parent.as_str(),
        ] {
            assert!(
                record.prompt.contains(required),
                "missing {required}: {}",
                record.prompt
            );
        }
        // Paths in the disclosure are JSON strings, including escaped
        // backslashes on Windows. Compare the disclosed value itself.
        let disclosures: Vec<Value> = serde_json::from_str(
            record
                .prompt
                .split_once("Host-recorded retired check dispositions (context only, no verification credit):\n")
                .expect("the evaluator receives the recorded disposition")
                .1
                .lines()
                .next()
                .expect("the disclosure has a JSON body"),
        )
        .expect("the evaluator receives valid disclosure JSON");
        assert_eq!(disclosures.len(), 1);
        assert_eq!(
            disclosures[0]["checkedRoot"]["workspaceId"].as_str(),
            Some(fixture.original_root.as_str()),
            "the original checked root is disclosed exactly"
        );
        let target = record.acceptance_evaluation.unwrap();
        assert_eq!(target.source_root.unwrap().root, fixture.current_root);
        assert_original_check_unchanged(&fixture, &body);
        assert_eq!(
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, args)| args.iter().any(|arg| arg == "note"))
                .count(),
            1,
            "the exclusion must be recorded on the item's evidence before spawning"
        );
        assert_eq!(
            sighting_reads(&fixture.transport),
            1,
            "disposing an old check does not waive the current root's sighting"
        );
        let later = decide(&fixture.state, &fixture.parent).unwrap_err();
        assert_eq!(
            later.code(),
            "registered_check_root_rebound",
            "the exclusion belongs only to its evaluation request, not the item"
        );
    }
}

#[test]
fn a_disposition_with_an_unknown_write_outcome_never_spawns() {
    let fixture = retired_check_fixture("passed");
    let calls: RecordedEngramCalls = Arc::default();
    let response = fixture.state.request_acceptance_evaluation_with_runner(
        &fixture.parent,
        disposition_request(COMMITTED_RUN_ID),
        disposition_reader(
            calls.clone(),
            Some(EngramTransportError::deadline(
                "the disposition write outcome is unknown",
            )),
        ),
    );
    let error = refusal(&fixture.state, response, "unknown disposition write");
    assert!(error.message.contains("disposition"), "{}", error.message);
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, args)| args.iter().any(|arg| arg == "note"))
            .count(),
        1
    );
    assert_eq!(sighting_reads(&fixture.transport), 0);
}

#[test]
fn a_disposition_without_a_record_acknowledgement_never_spawns() {
    let fixture = retired_check_fixture("passed");
    let ordinary = independent_reader();
    let response = fixture.state.request_acceptance_evaluation_with_runner(
        &fixture.parent,
        disposition_request(COMMITTED_RUN_ID),
        move |connection, args, timeout| {
            if args.iter().any(|arg| arg == "note") {
                // A success-shaped envelope alone does not prove that the
                // disposition became an attributed record on this item.
                Ok(json!({"operation":"note","work":{"short_ref":"w-task"}}))
            } else {
                ordinary(connection, args, timeout)
            }
        },
    );
    let error = refusal(&fixture.state, response, "unacknowledged disposition");
    assert!(error.message.contains("disposition"), "{}", error.message);
    assert_eq!(sighting_reads(&fixture.transport), 0);
}

#[test]
fn a_disposition_receipt_must_match_the_general_revision_when_the_basis_is_older() {
    for receipt_revision in [9, 7] {
        let fixture = retired_check_fixture("passed");
        let ordinary = independent_reader();
        let response = fixture.state.request_acceptance_evaluation_with_runner(
            &fixture.parent,
            disposition_request(COMMITTED_RUN_ID),
            |connection, args, timeout| {
                if args.iter().any(|arg| arg == "note") {
                    return Ok(json!({
                        "operation": "note",
                        "evidence": DISPOSITION_RECORD,
                        "work": {
                            "short_ref": "w-task",
                            "work_id": SIGHTED_WORK_ID,
                            "revision": receipt_revision,
                            "lifecycle": "open"
                        }
                    }));
                }
                let mut receipt = ordinary(connection, args, timeout)?;
                if args.iter().any(|arg| arg == "inspect") {
                    receipt["status"]["work"]["revision"] = json!(9);
                } else if args.iter().any(|arg| arg == "--full") {
                    receipt["work"]["revision"] = json!(9);
                }
                Ok(receipt)
            },
        );
        if receipt_revision == 9 {
            let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } = response.unwrap()
            else {
                panic!("the receipt at the general revision must admit an evaluator");
            };
            assert!(delegation.delegation.prompt.contains(DISPOSITION_RECORD));
        } else {
            let error = refusal(&fixture.state, response, "receipt at the older basis");
            assert_eq!(error.status, StatusCode::BAD_GATEWAY);
            assert!(error.message.contains("supplied no acknowledgement"));
        }
    }
}

#[test]
fn a_current_root_check_cannot_be_excluded() {
    let (state, parent, _store, named_root) = named_root_parent();
    let head = commit_named_root(&named_root);
    let registration = register_committed_run(
        &state,
        &parent,
        &named_root,
        committed_results("passed", input_record(&head, "clean")),
    );
    host_observed(&state, &registration, &head, acknowledged("settled"));
    let calls: RecordedEngramCalls = Arc::default();
    let response = state.request_acceptance_evaluation_with_runner(
        &parent,
        disposition_request(COMMITTED_RUN_ID),
        disposition_reader(calls.clone(), None),
    );
    let error = refusal(&state, response, "current root exclusion");
    assert!(error.message.contains("retired"), "{}", error.message);
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|(_, args)| args.iter().any(|arg| arg == "note")),
        "an inadmissible exclusion is refused before recording it"
    );
}

#[test]
fn a_disposition_cannot_exclude_a_different_run_by_reusing_its_reason() {
    let fixture = retired_check_fixture("passed");
    let calls: RecordedEngramCalls = Arc::default();
    let response = fixture.state.request_acceptance_evaluation_with_runner(
        &fixture.parent,
        disposition_request("test-not-the-retired-check"),
        disposition_reader(calls.clone(), None),
    );
    let error = refusal(&fixture.state, response, "another run");
    assert!(
        error.message.contains("test-not-the-retired-check"),
        "{}",
        error.message
    );
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|(_, args)| args.iter().any(|arg| arg == "note"))
    );
    assert_eq!(sighting_reads(&fixture.transport), 0);
}

fn rejected_check_request() -> RequestAcceptanceEvaluationRequest {
    serde_json::from_value(json!({
        "workRef": "w-task",
        "agent": "Codex",
        "rejectedCheckDisposition": {
            "runId": COMMITTED_RUN_ID,
            "reason": "The passing run was rejected by the verification producer."
        }
    }))
    .unwrap()
}

fn producer_rejection(registration: &TestRunRegistration) -> TestRunVerificationAck {
    let frozen = engram_verification_record_frozen(
        FsPath::new(&registration.run_directory),
        &registration.run_id,
    )
    .unwrap();
    TestRunVerificationAck::Rejected {
        detail: engram_verification_record_refusal_detail(
            Some("verification_record_refused"),
            Some("work_revision"),
        ),
        producer_body_digest: Some(engram_verification_record_body_digest(&frozen.body)),
    }
}

// The disposition takes a second evidence cut. Both cuts see a bound criterion
// with no verification evidence and the same current named-root claim.
struct RejectedCheckEvidenceTransport {
    named: Arc<ScriptedEngramControlTransport>,
    evidence: Arc<evidence_selection::BindingEvidenceTransport>,
}

impl EngramControlTransport for RejectedCheckEvidenceTransport {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        match request {
            EngramControlRequest::AcceptanceBindingRead { .. }
            | EngramControlRequest::AcceptanceVerificationRead { .. }
            | EngramControlRequest::WorkSessionRead { .. } => {
                self.evidence.request(connection, request, timeout)
            }
            _ => self.named.request(connection, request, timeout),
        }
    }

    fn read_held_claims(
        &self,
        connection: &EngramConnectionConfig,
        timeout: Duration,
    ) -> Result<EngramHeldClaims, EngramTransportError> {
        self.named.read_held_claims(connection, timeout)
    }

    fn shutdown_session(&self, session_id: &str) {
        self.named.shutdown_session(session_id);
    }
}

#[test]
fn rejected_check_admits_and_discloses_context_without_credit() {
    let (state, parent, named_root, head, registration) =
        super::super::source_record_compatibility::owner_run_with_outcome("passed", None);
    let rejection = producer_rejection(&registration);
    state.note_engram_verification_record_ack(&registration, rejection.clone());
    let refusal = decide(&state, &parent)
        .unwrap_err()
        .into_api_error("w-task");
    assert!(refusal.message.contains("the refusal is final"));
    assert!(refusal.message.contains("rejectedCheckDisposition"));
    assert!(!refusal.message.contains("once Engram has received it"));

    let (_, _, mut page) = evidence_selection::canonical_binding_receipts();
    page["rows"][1]["binding"]["obligation"] = Value::Null;
    let evidence = evidence_selection::install_binding_evidence_transport(
        &state,
        &parent,
        vec![page.clone(), page],
    );
    let named = ScriptedEngramControlTransport::new([]);
    script_named_parent_claim(&named);
    state.install_test_engram_transport(Arc::new(RejectedCheckEvidenceTransport {
        named,
        evidence,
    }));
    let calls: RecordedEngramCalls = Arc::default();
    let read = disposition_reader(calls.clone(), None);
    let result = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            rejected_check_request(),
            |connection, args, timeout| {
                let mut value = read(connection, args, timeout)?;
                if args.iter().any(|arg| arg == "--full") {
                    value["work"]["acceptance_bindings"] =
                        json!([{"criterion": 2, "requirement": {"check_kind": "test"}}]);
                }
                Ok(value)
            },
        )
        .unwrap_or_else(|error| panic!("{}", error.message));
    let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } = result else {
        panic!("the recorded disposition must permit independent judgment");
    };
    let record = delegation.delegation;
    let disclosure: Value = serde_json::from_str(record.prompt
        .split_once("Host-recorded rejected check disposition (context only, no verification credit):\n")
        .unwrap().1.lines().next().unwrap()).unwrap();
    assert_eq!(disclosure["runId"], COMMITTED_RUN_ID);
    assert_eq!(disclosure["sourceId"]["commit"], head);
    assert_eq!(disclosure["artifactOutcome"], "passed");
    assert_eq!(disclosure["evidenceRecord"], DISPOSITION_RECORD);
    assert!(
        record
            .prompt
            .contains("cannot satisfy any criterion bound to a check kind")
    );
    assert!(
        record
            .prompt
            .contains("[bound to a host-recorded `test` check")
    );
    assert_eq!(
        record
            .acceptance_evaluation
            .unwrap()
            .source_root
            .unwrap()
            .root,
        named_root
    );
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, args)| args.iter().any(|arg| arg == "note"))
            .count(),
        1,
    );
    assert_eq!(
        current_registration(&state).verification_record,
        Some(rejection)
    );
    assert!(matches!(
        decide(&state, &parent),
        Err(AcceptanceCommittedInputRefusal::RecordRejected { .. })
    ));
}

#[test]
fn rejected_check_admits_an_old_fence_run_after_same_claim_recovery() {
    let (state, parent, _, head, registration) =
        super::super::source_record_compatibility::owner_run_with_outcome("passed", None);
    let original = registration.work_binding.clone().unwrap();
    let rejection = TestRunVerificationAck::Rejected {
        detail: engram_verification_record_refusal_detail(
            Some("verification_record_refused"),
            Some("claim"),
        ),
        // The historical rejection carries no digest of the frozen body.
        // Keep the original terminal observation as its artifact proof.
        producer_body_digest: None,
    };
    state.note_engram_verification_record_ack(&registration, rejection.clone());
    retain_rejected_check_terminal(&state, &registration);

    let mut recovered = original.clone();
    recovered.claim_fence += 1;
    recovered.work_revision += 1;
    let calls: RecordedEngramCalls = Arc::default();
    let read = disposition_reader(calls.clone(), None);
    let response = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            rejected_check_request(),
            |connection, args, timeout| {
                if args.iter().any(|arg| arg == "held") {
                    Ok(json!({"items":[{
                        "work_id": SIGHTED_WORK_ID,
                        "short_ref": "w-task",
                        "claim_id": recovered.claim_id,
                        "claim_fence": recovered.claim_fence,
                        "control_binding": recovered,
                    }],"omitted":0}))
                } else {
                    read(connection, args, timeout)
                }
            },
        )
        .unwrap_or_else(|error| panic!("{}", error.message));
    let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } = response else {
        panic!("the preserved name and recovered claim must admit the rejected run as context");
    };
    let disclosure: Value = serde_json::from_str(
        delegation
            .delegation
            .prompt
            .split_once("Host-recorded rejected check disposition (context only, no verification credit):\n")
            .unwrap()
            .1
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(disclosure["runId"], COMMITTED_RUN_ID);
    assert_eq!(disclosure["sourceId"]["commit"], head);
    assert_eq!(disclosure["artifactOutcome"], "passed");
    assert_eq!(disclosure["verificationRecord"], json!(rejection));
    assert_eq!(disclosure["evidenceRecord"], DISPOSITION_RECORD);
    let retained = current_registration(&state);
    assert_eq!(retained.named_root, registration.named_root);
    assert_eq!(retained.work_binding.as_ref(), Some(&original));
    assert_eq!(retained.verification_record, Some(rejection));
}

#[test]
fn rejected_check_refuses_nonpasses_wrong_head_and_unconfirmed_records_before_writing() {
    for (outcome, move_head, ack, code) in [
        (
            "failed",
            false,
            Some(true),
            "committed_input_no_eligible_run",
        ),
        (
            "passed",
            true,
            Some(true),
            "committed_input_no_eligible_run",
        ),
        (
            "passed",
            false,
            None,
            "host-held terminal verification rejection",
        ),
        (
            "passed",
            false,
            Some(false),
            "host-held terminal verification rejection",
        ),
    ] {
        let (state, parent, named_root, _, registration) =
            super::super::source_record_compatibility::owner_run_with_outcome(outcome, None);
        if let Some(producer) = ack {
            let ack = if producer {
                producer_rejection(&registration)
            } else {
                TestRunVerificationAck::Rejected {
                    detail: "legacy_source_input_retired: retained commit input is not sent"
                        .to_owned(),
                    producer_body_digest: None,
                }
            };
            state.note_engram_verification_record_ack(&registration, ack);
        } else {
            // The writable progress mirror cannot assert the host's refusal.
            fs::write(
                FsPath::new(&registration.run_directory)
                    .join(ENGRAM_VERIFICATION_RECORD_STATE_FILE),
                serde_json::to_vec(&EngramVerificationRecordProgress::Rejected {
                    detail: engram_verification_record_refusal_detail(
                        Some("verification_record_refused"),
                        None,
                    ),
                })
                .unwrap(),
            )
            .unwrap();
        }
        if move_head {
            run_git_test_command(
                &worktree(&named_root),
                &["commit", "--allow-empty", "--quiet", "-m", "next"],
            );
            acceptance_owner_reports(&state, &parent, &named_root, Vec::new(), Vec::new());
        }
        let calls: RecordedEngramCalls = Arc::default();
        let error = refusal(
            &state,
            state.request_acceptance_evaluation_with_runner(
                &parent,
                rejected_check_request(),
                disposition_reader(calls.clone(), None),
            ),
            code,
        );
        assert!(error.message.contains(code), "{}", error.message);
        assert!(
            !calls
                .lock()
                .unwrap()
                .iter()
                .any(|(_, args)| args.iter().any(|arg| arg == "note"))
        );
    }
}

#[test]
fn rejected_check_cannot_skip_a_newer_failure_on_head() {
    let (state, parent, named_root, head, registration) =
        super::super::source_record_compatibility::owner_run_with_outcome("passed", None);
    state.note_engram_verification_record_ack(&registration, producer_rejection(&registration));
    let newer_id = "test-newer-rejected-check-failure";
    let mut results = committed_results("failed", input_record(&head, "clean"));
    results["runId"] = json!(newer_id);
    results["input"]["eligible"] = json!(false);
    register_committed_run_as(&state, &parent, &named_root, newer_id, results);
    state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .get_mut(newer_id)
        .unwrap()
        .registered_at = "2099-01-01T00:00:00Z".to_owned();
    let calls: RecordedEngramCalls = Arc::default();
    let response = state.request_acceptance_evaluation_with_runner(
        &parent,
        rejected_check_request(),
        disposition_reader(calls.clone(), None),
    );
    let error = refusal(&state, response, "newer failed check");
    assert!(error.message.starts_with("committed_input_no_eligible_run"));
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|(_, args)| args.iter().any(|arg| arg == "note"))
    );
}

#[test]
fn rejected_check_rechecks_registration_set_after_the_note() {
    let (state, parent, named_root, head, registration) =
        super::super::source_record_compatibility::owner_run_with_outcome("passed", None);
    state.note_engram_verification_record_ack(&registration, producer_rejection(&registration));
    let state = Arc::new(state);
    let calls: RecordedEngramCalls = Arc::default();
    let read = disposition_reader(calls.clone(), None);
    let response = state.request_acceptance_evaluation_with_runner(
        &parent,
        rejected_check_request(),
        |connection, args, timeout| {
            let reply = read(connection, args, timeout)?;
            if args.iter().any(|arg| arg == "note") {
                let state = state.clone();
                let parent = parent.clone();
                let named_root = named_root.clone();
                let head = head.clone();
                TEST_ACCEPTANCE_COMMITTED_INPUT_BEFORE_RECHECK.with(|slot| {
                    *slot.borrow_mut() = Some(Box::new(move || {
                        let run_id = "test-newer-after-rejection-disclosure";
                        let mut results = committed_results("failed", input_record(&head, "clean"));
                        results["runId"] = json!(run_id);
                        results["input"]["eligible"] = json!(false);
                        register_committed_run_as(&state, &parent, &named_root, run_id, results);
                    }));
                });
            }
            Ok(reply)
        },
    );
    let error = refusal(&state, response, "new terminal published during fresh scan");
    assert!(
        error.message.starts_with("committed_input_changed"),
        "{}",
        error.message
    );
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, args)| args.iter().any(|arg| arg == "note"))
            .count(),
        1,
    );
}

#[test]
fn rejected_check_waits_for_a_newer_unfinished_run_before_disclosure() {
    let (state, parent, root, head, registration) =
        super::super::source_record_compatibility::owner_run_with_outcome("passed", None);
    state.note_engram_verification_record_ack(&registration, producer_rejection(&registration));
    let newer_id = "test-unfinished-newer-than-rejected";
    let pending = super::super::selection_edges::pending_results(newer_id, &head);
    register_committed_run_as(&state, &parent, &root, newer_id, pending);
    state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .get_mut(newer_id)
        .unwrap()
        .registered_at = "2099-01-01T00:00:00Z".to_owned();
    let calls: RecordedEngramCalls = Arc::default();
    let error = refusal(
        &state,
        state.request_acceptance_evaluation_with_runner(
            &parent,
            rejected_check_request(),
            disposition_reader(calls.clone(), None),
        ),
        "newer unfinished run",
    );
    assert!(
        error
            .message
            .starts_with("rejected_check_newer_run_pending"),
        "{}",
        error.message
    );
    assert!(error.message.contains(newer_id));
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|(_, args)| args.iter().any(|arg| arg == "note"))
    );
}

#[test]
fn rejected_check_rechecks_registration_set_at_evaluator_creation() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (state, parent, root, head, registration) =
        super::super::source_record_compatibility::owner_run_with_outcome("passed", None);
    state.note_engram_verification_record_ack(&registration, producer_rejection(&registration));
    let state = Arc::new(state);
    let published = Arc::new(AtomicBool::new(false));
    let calls: RecordedEngramCalls = Arc::default();
    let read = disposition_reader(calls.clone(), None);
    let response = state.request_acceptance_evaluation_with_runner(
        &parent,
        rejected_check_request(),
        |connection, args, timeout| {
            let reply = read(connection, args, timeout)?;
            if args.iter().any(|arg| arg == "note") {
                let weak_state = Arc::downgrade(&state);
                let parent = parent.clone();
                let root = root.clone();
                let head = head.clone();
                let published = published.clone();
                let mut inner = state.inner.lock().unwrap();
                let measure = inner.test_observation_source_owner.clone().unwrap();
                // The preflights use a separate source seam. Publish only
                // when the evaluator independently captures its final basis.
                inner.test_observation_source_owner =
                    Some(Arc::new(move |binary, cwd, args, deadline, limit| {
                        if args[0] == "source" && !published.swap(true, Ordering::SeqCst) {
                            let state = weak_state.upgrade().unwrap();
                            let run_id = "test-newer-during-evaluator-source-capture";
                            let mut results =
                                committed_results("failed", input_record(&head, "clean"));
                            results["runId"] = json!(run_id);
                            results["input"]["eligible"] = json!(false);
                            register_committed_run_as(&state, &parent, &root, run_id, results);
                        }
                        measure(binary, cwd, args, deadline, limit)
                    }));
            }
            Ok(reply)
        },
    );
    let error = refusal(&state, response, "new terminal during final owner capture");
    assert!(published.load(Ordering::SeqCst));
    assert!(
        error.message.starts_with("committed_input_changed"),
        "{}",
        error.message
    );
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, args)| args.iter().any(|arg| arg == "note"))
            .count(),
        1
    );
}

#[test]
fn rejected_check_refuses_replacement_passing_artifacts() {
    let (state, parent, _, _, registration) =
        super::super::source_record_compatibility::owner_run_with_outcome("failed", None);
    state.note_engram_verification_record_ack(&registration, producer_rejection(&registration));
    let path = FsPath::new(&registration.run_directory).join("results.json");
    let mut results: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    results["state"] = json!("passed");
    results["exitCode"] = json!(0);
    results["input"]["eligible"] = json!(true);
    results["stages"][0]["state"] = json!("passed");
    results["stages"][0]["code"] = json!(0);
    fs::write(&path, serde_json::to_vec(&results).unwrap()).unwrap();
    let body = super::super::source_record_compatibility::write_owner_run_body(&registration);
    assert!(engram_verification_record_is_host_body(
        &registration,
        &body
    ));
    assert_eq!(body.run.outcome, EngramVerificationRecordRunOutcome::Passed);

    let calls: RecordedEngramCalls = Arc::default();
    let error = refusal(
        &state,
        state.request_acceptance_evaluation_with_runner(
            &parent,
            rejected_check_request(),
            disposition_reader(calls.clone(), None),
        ),
        "replacement passed results and body",
    );
    assert!(
        error
            .message
            .starts_with("rejected_check_evidence_mismatch")
    );
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|(_, args)| args.iter().any(|arg| arg == "note"))
    );
}

fn historical_producer_rejection() -> Value {
    json!({
        "state": "rejected",
        "detail": "Engram refused it: verification_record_refused",
    })
}

fn retain_rejected_check_terminal(state: &AppState, registration: &TestRunRegistration) {
    let run_dir = FsPath::new(&registration.run_directory);
    let disk = TestRunDisk::read(run_dir, None).unwrap();
    let summary = {
        let inner = state.inner.lock().unwrap();
        test_run_summary(
            &inner,
            &TestRunScanned {
                disk: Arc::new(disk),
                state: TestRunState::Passed,
                unknown_reason: None,
                project_id: registration.project_id.clone(),
            },
        )
    };
    let terminal_digest = summary
        .detail_version
        .as_deref()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap()
        .to_owned();
    let observation = TestRunCommandObservation {
        execution_id: "fixture-original-launch".into(),
        runtime_identity: None,
        tool_identity: "fixture-tool".into(),
        grant_identity: None,
        command_fingerprint: String::new(),
        turn_generation: 1,
        observed_at: registration.registered_at.clone(),
        summary,
        work_binding: registration.work_binding.clone(),
        source_root: None,
        validated_unbound: false,
        monitoring_interrupted_at: None,
        evidence: Vec::new(),
        artifact: Some(TestRunObservedArtifact {
            run_id: registration.run_id.clone(),
            run_directory: registration.run_directory.clone(),
            request_identity: Some(registration.request_identity.clone()),
            terminal_digest: Some(terminal_digest),
        }),
        native_completion_refusal: None,
        declared_artifact: None,
        declared_binding: None,
        declared_native: None,
        launch_only: true,
        launcher: None,
        final_unbound_proof: None,
        declared_verdict_pending: false,
    };
    state
        .inner
        .lock()
        .unwrap()
        .test_run_observations
        .insert(observation.execution_id.clone(), observation);
}

#[test]
fn older_rejection_needs_the_exact_host_retained_terminal() {
    let (state, parent, _, _, registration) =
        super::super::source_record_compatibility::owner_run_with_outcome("passed", None);
    state.note_engram_verification_record_ack(
        &registration,
        serde_json::from_value(historical_producer_rejection()).unwrap(),
    );
    let run_dir = FsPath::new(&registration.run_directory);
    let registration = current_registration(&state);
    let body = engram_verification_record_frozen(run_dir, &registration.run_id)
        .unwrap()
        .body;
    assert!(!state.engram_rejected_verification_body_matches(&registration, &body));
    retain_rejected_check_terminal(&state, &registration);
    assert!(state.engram_rejected_verification_body_matches(&registration, &body));

    let calls: RecordedEngramCalls = Arc::default();
    let response = state.request_acceptance_evaluation_with_runner(
        &parent,
        rejected_check_request(),
        disposition_reader(calls, None),
    );
    assert!(matches!(
        response,
        Ok(AcceptanceEvaluationRequestResponse::Spawned { .. })
    ));

    let result_path = run_dir.join("results.json");
    let mut reformatted = fs::read(&result_path).unwrap();
    reformatted.push(b'\n');
    fs::write(result_path, reformatted).unwrap();
    assert!(engram_verification_record_is_host_body(
        &registration,
        &body
    ));
    assert!(!state.engram_rejected_verification_body_matches(&registration, &body));
}

#[test]
fn rejected_check_admits_original_commit_format_as_context() {
    let (state, parent, _, root) = named_root_parent();
    let head = commit_named_root(&root);
    let registration = register_committed_run(
        &state,
        &parent,
        &root,
        committed_results("passed", input_record(&head, "clean")),
    );
    host_observed(
        &state,
        &registration,
        &head,
        historical_producer_rejection(),
    );
    let registration = current_registration(&state);
    let body_path =
        FsPath::new(&registration.run_directory).join(ENGRAM_VERIFICATION_RECORD_BODY_FILE);
    let original_body = fs::read(&body_path).unwrap();
    retain_rejected_check_terminal(&state, &registration);

    let calls: RecordedEngramCalls = Arc::default();
    let response = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            rejected_check_request(),
            disposition_reader(calls, None),
        )
        .unwrap_or_else(|error| panic!("{}", error.message));
    let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } = response else {
        panic!("the historical passing run must admit evaluation");
    };
    let record = delegation.delegation;
    assert!(
        record
            .prompt
            .contains("context only, no verification credit")
    );
    let source = record.acceptance_evaluation.unwrap().source_id.unwrap();
    assert_eq!(source.commit, head);
    assert_eq!(
        source.tree,
        run_git_test_command_output(&worktree(&root), &["rev-parse", "HEAD^{tree}"])
    );
    assert_eq!(
        current_registration(&state).verification_record,
        registration.verification_record
    );
    assert_eq!(fs::read(body_path).unwrap(), original_body);
}

fn unheld_rejected_check() -> (AppState, String, String, String, String) {
    let (state, requester, root, original) = super::super::super::successor::successor_fixture();
    acceptance_owner_reports(&state, &requester, &root, Vec::new(), Vec::new());
    let source = EngramSourceId {
        git_delta: None,
        commit: run_git_test_command_output(&worktree(&root), &["rev-parse", "HEAD"]),
        tree: run_git_test_command_output(&worktree(&root), &["rev-parse", "HEAD^{tree}"]),
        changes: Vec::new(),
        submodules: Vec::new(),
    };
    let results = committed_results("passed", input_record(&source.commit, "clean"));
    let registration = try_register_committed_run_with_source_as(
        &state,
        &original.named_by_session,
        &root,
        COMMITTED_RUN_ID,
        results,
        Some(source),
    )
    .unwrap();
    super::super::source_record_compatibility::write_owner_run_body(&registration);
    state.note_engram_verification_record_ack(&registration, producer_rejection(&registration));
    let head = run_git_test_command_output(&worktree(&root), &["rev-parse", "HEAD"]);
    (state, requester, root, original.named_by_session, head)
}

#[test]
fn unheld_rejected_check_requires_canonical_root_before_disclosure() {
    let (state, requester, root, _, _) = unheld_rejected_check();
    let transport = install_sighting_transport(&state, &root, 2, present_sighting());
    let calls: RecordedEngramCalls = Arc::default();
    let read = disposition_reader(calls.clone(), None);
    let error = refusal(
        &state,
        state.request_acceptance_evaluation_with_runner(
            &requester,
            rejected_check_request(),
            |connection, args, timeout| {
                if args.iter().any(|arg| arg == "held") {
                    Ok(json!({"items":[],"omitted":0}))
                } else {
                    read(connection, args, timeout)
                }
            },
        ),
        "different canonical generation",
    );
    assert_eq!(
        sighting_reads(&transport),
        1,
        "no disclosure before the sighting"
    );
    assert!(error.message.contains("generation"), "{}", error.message);
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|(_, args)| args.iter().any(|arg| arg == "note"))
    );
}

struct DuringRejectedCheckSighting {
    scripted: Arc<ScriptedEngramControlTransport>,
    on_sighting: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl EngramControlTransport for DuringRejectedCheckSighting {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        let result = self.scripted.request(connection, request, timeout)?;
        if matches!(request, EngramControlRequest::NamedRootSightingRead { .. }) {
            let on_sighting = self.on_sighting.lock().unwrap().take();
            if let Some(on_sighting) = on_sighting {
                on_sighting();
            }
        }
        Ok(result)
    }

    fn read_held_claims(
        &self,
        connection: &EngramConnectionConfig,
        timeout: Duration,
    ) -> Result<EngramHeldClaims, EngramTransportError> {
        self.scripted.read_held_claims(connection, timeout)
    }

    fn shutdown_session(&self, session_id: &str) {
        self.scripted.shutdown_session(session_id);
    }
}

#[test]
fn rejected_check_rechecks_selection_after_canonical_sighting() {
    let (state, requester, root, holder, head) = unheld_rejected_check();
    let state = Arc::new(state);
    let scripted = install_sighting_transport(&state, &root, 1, present_sighting());
    let weak_state = Arc::downgrade(&state);
    let run_id = "test-newer-during-canonical-sighting";
    state.install_test_engram_transport(Arc::new(DuringRejectedCheckSighting {
        scripted: scripted.clone(),
        on_sighting: Mutex::new(Some(Box::new(move || {
            let state = weak_state.upgrade().unwrap();
            let pending = super::super::selection_edges::pending_results(run_id, &head);
            register_committed_run_as(&state, &holder, &root, run_id, pending);
        }))),
    }));
    let calls: RecordedEngramCalls = Arc::default();
    let read = disposition_reader(calls.clone(), None);
    let response = state.request_acceptance_evaluation_with_runner(
        &requester,
        rejected_check_request(),
        |connection, args, timeout| {
            if args.iter().any(|arg| arg == "held") {
                Ok(json!({"items":[],"omitted":0}))
            } else {
                read(connection, args, timeout)
            }
        },
    );
    let error = refusal(
        &state,
        response,
        "new registration during a successful sighting",
    );
    assert_eq!(sighting_reads(&scripted), 1);
    assert!(
        error.message.starts_with("committed_input_changed"),
        "{}",
        error.message
    );
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|(_, args)| args.iter().any(|arg| arg == "note"))
    );
}

#[test]
fn rejected_check_rechecks_source_after_canonical_sighting() {
    let (state, requester, root, _, _) = unheld_rejected_check();
    let state = Arc::new(state);
    let scripted = install_sighting_transport(&state, &root, 1, present_sighting());
    let weak_state = Arc::downgrade(&state);
    let session = requester.clone();
    state.install_test_engram_transport(Arc::new(DuringRejectedCheckSighting {
        scripted: scripted.clone(),
        on_sighting: Mutex::new(Some(Box::new(move || {
            let state = weak_state.upgrade().unwrap();
            run_git_test_command(
                &worktree(&root),
                &[
                    "commit",
                    "--allow-empty",
                    "--quiet",
                    "-m",
                    "advance during sighting",
                ],
            );
            acceptance_owner_reports(&state, &session, &root, Vec::new(), Vec::new());
        }))),
    }));
    let calls: RecordedEngramCalls = Arc::default();
    let read = disposition_reader(calls.clone(), None);
    let response = state.request_acceptance_evaluation_with_runner(
        &requester,
        rejected_check_request(),
        |connection, args, timeout| {
            if args.iter().any(|arg| arg == "held") {
                Ok(json!({"items":[],"omitted":0}))
            } else {
                read(connection, args, timeout)
            }
        },
    );
    let error = refusal(
        &state,
        response,
        "checkout advanced during a successful sighting",
    );
    assert_eq!(sighting_reads(&scripted), 1);
    assert!(
        error.message.starts_with("committed_input_changed"),
        "{}",
        error.message
    );
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|(_, args)| args.iter().any(|arg| arg == "note"))
    );
}
