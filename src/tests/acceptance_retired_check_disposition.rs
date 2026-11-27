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
    retired_check_fixture_with_runs(&[outcome]).0
}

fn retired_check_fixture_with_runs(
    outcomes: &[&str],
) -> (RetiredCheckFixture, Vec<TestRunRegistration>) {
    let (state, parent, store, original_root) = named_root_parent();
    let head = commit_named_root(&original_root);
    let mut runs = Vec::new();
    for (index, outcome) in outcomes.iter().enumerate() {
        let run_id = if index == 0 {
            COMMITTED_RUN_ID.to_owned()
        } else {
            format!("test-historical-retired-{index}")
        };
        let mut results = committed_results(outcome, input_record(&head, "clean"));
        results["runId"] = json!(run_id);
        let registration =
            register_committed_run_as(&state, &parent, &original_root, &run_id, results);
        host_observed(&state, &registration, &head, acknowledged("settled"));
        runs.push(state.inner.lock().unwrap().test_run_registrations[&run_id].clone());
    }
    (
        retired_check_on_replacement(state, parent, store, original_root, &head),
        runs,
    )
}

fn retired_check_on_replacement(
    state: AppState,
    parent: String,
    store: EngramAuthorityStoreKey,
    original_root: String,
    head: &str,
) -> RetiredCheckFixture {
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
            head,
        ],
    );
    let current_root = engram_exact_path_key(&replacement);
    {
        let mut inner = state.inner.lock().unwrap();
        inner.engram_work_source_roots[0].root = current_root.clone();
    }
    rename_same_workspace(&state, &parent, &store);
    acceptance_owner_reports(&state, &parent, &current_root, Vec::new(), Vec::new());
    let transport = install_sighting_answer(&state, Ok(replacement_sighting(&current_root)));
    RetiredCheckFixture {
        state,
        parent,
        original_root,
        current_root,
        registration,
        transport,
    }
}

fn replacement_sighting(current_root: &str) -> Value {
    let mut answer = sighting_reply(current_root, 2, present_sighting());
    answer["current_binding"] = json!("fixture-renamed-event");
    answer["root"]["binding_event"] = json!("fixture-renamed-event");
    answer["root"]["binding_position"] = json!(2);
    answer
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
        assert_eq!(
            decide(&fixture.state, &fixture.parent),
            Ok(AcceptanceCommittedInputDecision::Legacy),
            "the same claim keeps the acknowledged exclusion"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_dangling_verification_body_entry_keeps_the_retired_check_blocking() {
    let fixture = retired_check_fixture("passed");
    let body_path = PathBuf::from(&fixture.registration.run_directory)
        .join(ENGRAM_VERIFICATION_RECORD_BODY_FILE);
    fs::remove_file(&body_path).unwrap();
    std::os::unix::fs::symlink("missing-verification-body", &body_path).unwrap();
    assert_eq!(
        fs::File::open(&body_path).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );

    let calls: RecordedEngramCalls = Arc::default();
    let Err(error) = fixture.state.request_acceptance_evaluation_with_runner(
        &fixture.parent,
        disposition_request(COMMITTED_RUN_ID),
        disposition_reader(calls.clone(), None),
    ) else {
        panic!("an unreadable artifact entry must not permit a disposition");
    };
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|(_, args)| args.iter().any(|arg| arg == "note"))
    );
    assert!(
        current_registration(&fixture.state)
            .retired_check_disposition
            .is_none()
    );
    assert!(fixture.state.inner.lock().unwrap().delegations.is_empty());
    assert_eq!(
        decide(&fixture.state, &fixture.parent).unwrap_err().code(),
        "registered_check_root_rebound"
    );
}

#[test]
fn a_listed_oversized_acknowledged_body_can_be_disposed_and_reused() {
    let (state, parent, store, root) = named_root_parent();
    let head = commit_named_root(&root);
    let tree = run_git_test_command_output(&worktree(&root), &["rev-parse", "HEAD^{tree}"]);
    let source = serde_json::from_value(json!({
        "commit": head,
        "tree": tree.trim(),
        "submodules": (0..300).map(|index| json!({
            "path": format!("child-{index:04}"),
            "commit": "d".repeat(40),
            "changes": []
        })).collect::<Vec<_>>(),
        "changes": []
    }))
    .unwrap();
    let registration = try_register_committed_run_with_source_as(
        &state,
        &parent,
        &root,
        COMMITTED_RUN_ID,
        committed_results("passed", input_record(&head, "clean")),
        Some(source),
    )
    .unwrap();
    let body = source_record_compatibility::write_owner_run_body(&registration);
    let frozen_body = engram_verification_record_text(&body).unwrap();
    assert!(frozen_body.len() > ACCEPTANCE_RETIRED_CHECK_DISCLOSURE_MAX);
    let digest = engram_verification_record_body_digest(&body);
    let mut progress = acknowledged("settled");
    progress["body_digest"] = json!(digest);
    state.note_engram_verification_record_ack(
        &registration,
        serde_json::from_value(progress).unwrap(),
    );
    let fixture = retired_check_on_replacement(state, parent, store, root, &head);

    let error = refusal(
        &fixture.state,
        request(&fixture.state, &fixture.parent),
        "oversized retired verification record",
    );
    let checked = fixture.registration.named_root.as_ref().unwrap();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.starts_with("registered_check_root_rebound"));
    assert!(error.message.contains(COMMITTED_RUN_ID));
    assert!(error.message.contains(&checked.generation.to_string()));

    let body_path =
        FsPath::new(&registration.run_directory).join(ENGRAM_VERIFICATION_RECORD_BODY_FILE);
    let mut altered = body;
    altered.run.command.push_str(" --different");
    fs::write(
        &body_path,
        engram_verification_record_text(&altered).unwrap(),
    )
    .unwrap();
    let calls: RecordedEngramCalls = Arc::default();
    let changed = fixture.state.request_acceptance_evaluation_with_runner(
        &fixture.parent,
        disposition_request(COMMITTED_RUN_ID),
        disposition_reader(calls.clone(), None),
    );
    assert_eq!(
        refusal(&fixture.state, changed, "changed body").status,
        StatusCode::CONFLICT
    );
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|(_, args)| args.iter().any(|arg| arg == "note"))
    );
    fs::write(&body_path, &frozen_body).unwrap();

    let result = fixture.state.request_acceptance_evaluation_with_runner(
        &fixture.parent,
        disposition_request(COMMITTED_RUN_ID),
        disposition_reader(calls.clone(), None),
    );
    let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } =
        result.unwrap_or_else(|error| panic!("{}", error.message))
    else {
        panic!("the exact acknowledged record can be disposed of");
    };
    let notes: Vec<_> = calls
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(_, args)| {
            let note = args.iter().position(|arg| arg == "note")?;
            Some(args[note + 2].clone())
        })
        .collect();
    assert_eq!(notes.len(), 1);
    assert!(notes[0].len() <= ACCEPTANCE_RETIRED_CHECK_DISCLOSURE_MAX);
    let note: Value = serde_json::from_str(&notes[0]).unwrap();
    assert_eq!(note["runId"], COMMITTED_RUN_ID);
    assert_eq!(note["reason"], DISPOSITION_REASON);
    assert_eq!(note["verificationBodyDigest"], digest);
    assert_eq!(
        note["originalVerificationReference"]["producerRecord"],
        RECORD_PRODUCER
    );
    assert_eq!(
        note["originalVerificationReference"]["verificationRecord"],
        "a".repeat(32)
    );
    assert_eq!(
        note["originalVerificationReference"]["artifact"],
        ENGRAM_VERIFICATION_RECORD_BODY_FILE
    );
    assert_eq!(note["originalRunOutcome"], "passed");
    assert!(note.get("originalVerification").is_none());
    assert_original_check_unchanged(&fixture, frozen_body.as_bytes());

    set_delegation_status(
        &fixture.state,
        &delegation.delegation.id,
        DelegationStatus::Completed,
    );
    fixture
        .transport
        .script_sighting_reads([Ok(replacement_sighting(&fixture.current_root))]);
    let later_calls: RecordedEngramCalls = Arc::default();
    let later = fixture.state.request_acceptance_evaluation_with_runner(
        &fixture.parent,
        evaluation_request(Some(Agent::Codex)),
        disposition_reader(later_calls.clone(), None),
    );
    let AcceptanceEvaluationRequestResponse::Spawned {
        delegation: later, ..
    } = later.unwrap_or_else(|error| panic!("{}", error.message))
    else {
        panic!("later admission uses the standing exclusion");
    };
    assert_ne!(later.delegation.id, delegation.delegation.id);
    assert!(later.delegation.prompt.contains(DISPOSITION_RECORD));
    assert!(
        !later_calls
            .lock()
            .unwrap()
            .iter()
            .any(|(_, args)| args.iter().any(|arg| arg == "note"))
    );
}

#[test]
fn one_request_disposes_every_reported_retired_check_and_later_requests_reuse_it() {
    let (fixture, runs) = retired_check_fixture_with_runs(&[
        "passed", "failed", "passed", "failed", "passed", "passed",
    ]);
    let missing = runs.last().unwrap();
    const MISSING_BODY_RECORD: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    selection_edges::retain_registration(&fixture.state, missing);
    fs::remove_file(FsPath::new(&missing.run_directory).join(ENGRAM_VERIFICATION_RECORD_BODY_FILE))
        .unwrap();
    let expected: BTreeMap<_, _> = runs
        .iter()
        .map(|row| (row.run_id.clone(), row.named_root.clone().unwrap()))
        .collect();
    let AcceptanceCommittedInputRefusal::RootRebound { checks, current } =
        decide(&fixture.state, &fixture.parent).unwrap_err()
    else {
        panic!("the old registrations block admission before any disposition");
    };
    assert_eq!(checks.into_iter().collect::<BTreeMap<_, _>>(), expected);
    assert!(
        expected
            .values()
            .all(|checked| checked.generation < current.generation)
    );
    let error = refusal(
        &fixture.state,
        request(&fixture.state, &fixture.parent),
        "all historical checks still block",
    );
    assert_eq!(error.status, StatusCode::CONFLICT);
    for (run_id, checked) in &expected {
        assert!(error.message.contains(run_id));
        assert!(error.message.contains(&checked.generation.to_string()));
    }
    assert_eq!(sighting_reads(&fixture.transport), 0);

    let mut disposition = evaluation_request(Some(Agent::Codex));
    disposition.retired_check_dispositions = runs
        .iter()
        .map(|row| AcceptanceRetiredCheckRequest {
            run_id: row.run_id.clone(),
            reason: DISPOSITION_REASON.to_owned(),
        })
        .collect();
    let calls: RecordedEngramCalls = Arc::default();
    let reader = disposition_reader(calls.clone(), None);
    let result = fixture.state.request_acceptance_evaluation_with_runner(
        &fixture.parent,
        disposition,
        |connection, args, timeout| {
            let mut reply = reader(connection, args, timeout)?;
            if let Some(note) = args.iter().position(|arg| arg == "note") {
                let body: Value = serde_json::from_str(&args[note + 2]).unwrap();
                if body["runId"] == missing.run_id.as_str() {
                    reply["evidence"] = json!(MISSING_BODY_RECORD);
                }
            }
            Ok(reply)
        },
    );
    let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } =
        result.unwrap_or_else(|error| panic!("{}", error.message))
    else {
        panic!("one disposition request admits an independent evaluator");
    };
    let noted: BTreeMap<_, _> = calls
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(_, args)| {
            let note = args.iter().position(|arg| arg == "note")?;
            let body: Value = serde_json::from_str(&args[note + 2]).unwrap();
            Some((body["runId"].as_str().unwrap().to_owned(), body))
        })
        .collect();
    assert!(noted.keys().eq(expected.keys()));
    let unavailable = &noted[&missing.run_id];
    assert_eq!(unavailable["originalVerificationArtifact"], "unavailable");
    assert_eq!(unavailable["artifactOutcome"], "unknown");
    assert_eq!(
        unavailable["checkedRoot"]["generation"],
        expected[&missing.run_id].generation
    );
    assert_eq!(
        unavailable["verificationBodyDigest"].as_str(),
        missing
            .verification_record
            .as_ref()
            .and_then(TestRunVerificationAck::body_digest)
    );
    assert_eq!(unavailable["author"]["sessionId"], fixture.parent.as_str());
    assert_eq!(unavailable["reason"], DISPOSITION_REASON);
    assert!(unavailable.get("originalVerification").is_none());
    assert!(unavailable.get("originalRunOutcome").is_none());
    assert!(delegation.delegation.prompt.contains(MISSING_BODY_RECORD));
    set_delegation_status(
        &fixture.state,
        &delegation.delegation.id,
        DelegationStatus::Completed,
    );

    // Read ordinary host persistence back. Keep the active fixture's named
    // root and runner seams, since boot reconciles those independently.
    let persisted = load_state(&fixture.state.persistence_path)
        .unwrap()
        .unwrap();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        inner.test_run_registrations = persisted.test_run_registrations;
        inner.test_run_compacted = persisted.test_run_compacted;
    }
    fixture
        .transport
        .script_sighting_reads([Ok(replacement_sighting(&fixture.current_root))]);
    let later_calls: RecordedEngramCalls = Arc::default();
    let later = fixture.state.request_acceptance_evaluation_with_runner(
        &fixture.parent,
        evaluation_request(Some(Agent::Codex)),
        disposition_reader(later_calls.clone(), None),
    );
    let AcceptanceEvaluationRequestResponse::Spawned {
        delegation: later, ..
    } = later.unwrap_or_else(|error| panic!("{}", error.message))
    else {
        panic!("a later request needs no new retired-check disposition");
    };
    assert_ne!(later.delegation.id, delegation.delegation.id);
    assert!(later.delegation.prompt.contains(MISSING_BODY_RECORD));
    for run in &runs {
        assert!(later.delegation.prompt.contains(&run.run_id));
    }
    assert!(
        !later_calls
            .lock()
            .unwrap()
            .iter()
            .any(|(_, args)| args.iter().any(|arg| arg == "note"))
    );
    assert_eq!(sighting_reads(&fixture.transport), 2);
}

#[test]
fn a_note_acknowledged_during_rebind_is_reused_by_the_next_request() {
    let fixture = retired_check_fixture("passed");
    let store = fixture.registration.authority_store.as_ref().unwrap();
    let current = worktree(&fixture.current_root);
    let replacement = current.parent().unwrap().join("rebound");
    run_git_test_command(
        &current,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            replacement.to_str().unwrap(),
            "HEAD",
        ],
    );
    let rebound = engram_exact_path_key(&replacement);
    let calls: RecordedEngramCalls = Arc::default();
    let reader = disposition_reader(calls.clone(), None);
    let first = fixture.state.request_acceptance_evaluation_with_runner(
        &fixture.parent,
        disposition_request(COMMITTED_RUN_ID),
        |connection, args, timeout| {
            let result = reader(connection, args, timeout);
            if args.iter().any(|arg| arg == "note") {
                assert!(result.is_ok(), "the tracker already acknowledged the note");
                fixture.state.inner.lock().unwrap().engram_work_source_roots[0].root =
                    rebound.clone();
                rename_same_workspace(&fixture.state, &fixture.parent, store);
            }
            result
        },
    );
    let error = refusal(&fixture.state, first, "rebound during note reply");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(sighting_reads(&fixture.transport), 0);

    // Use the saved registrations to prove the first request fenced the
    // acknowledged note before refusing to evaluate its stale root.
    let persisted = load_state(&fixture.state.persistence_path)
        .unwrap()
        .unwrap();
    fixture.state.inner.lock().unwrap().test_run_registrations = persisted.test_run_registrations;
    acceptance_owner_reports(
        &fixture.state,
        &fixture.parent,
        &rebound,
        Vec::new(),
        Vec::new(),
    );
    let mut sighting = present_sighting();
    sighting["position"] = json!(3);
    let mut answer = sighting_reply(&rebound, 3, sighting);
    answer["current_binding"] = json!("fixture-renamed-event-3");
    answer["root"]["binding_event"] = json!("fixture-renamed-event-3");
    answer["root"]["binding_position"] = json!(3);
    let rebound_transport = install_sighting_answer(&fixture.state, Ok(answer));

    let later = fixture.state.request_acceptance_evaluation_with_runner(
        &fixture.parent,
        disposition_request(COMMITTED_RUN_ID),
        reader,
    );
    let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } =
        later.unwrap_or_else(|error| panic!("{}", error.message))
    else {
        panic!("the new root can use the acknowledged disposition");
    };
    let record = delegation.delegation;
    assert!(record.prompt.contains(DISPOSITION_RECORD));
    assert_eq!(sighting_reads(&rebound_transport), 1);
    let named = record.acceptance_evaluation.unwrap().source_root.unwrap();
    assert_eq!(named.root, rebound);
    assert_eq!(named.generation, 3);
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, args)| args.iter().any(|arg| arg == "note"))
            .count(),
        1,
        "retry uses the original audit record"
    );
}

#[test]
fn overlapping_retired_dispositions_record_and_disclose_only_the_winning_note() {
    let fixture = retired_check_fixture("passed");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let state = &fixture.state;
    let parent = &fixture.parent;
    let alternate = "c".repeat(32);
    let alternate_notes = std::cell::Cell::new(0);
    let (first, second) = std::thread::scope(|scope| {
        let first = scope.spawn(move || {
            let base = disposition_reader(Arc::default(), None);
            state.request_acceptance_evaluation_with_runner(
                parent,
                disposition_request(COMMITTED_RUN_ID),
                |connection, args, timeout| {
                    if args.iter().any(|arg| arg == "note") {
                        started_tx.send(()).unwrap();
                        release_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
                    }
                    base(connection, args, timeout)
                },
            )
        });
        started_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
        let base = independent_reader();
        let second = state.request_acceptance_evaluation_with_runner(
            parent,
            disposition_request(COMMITTED_RUN_ID),
            |connection, args, timeout| {
                if args.iter().any(|arg| arg == "note") {
                    alternate_notes.set(alternate_notes.get() + 1);
                    return Ok(json!({"operation":"note","evidence":alternate,
                        "work":{"short_ref":"w-task","work_id":SIGHTED_WORK_ID,
                        "revision":7,"lifecycle":"open"}}));
                }
                base(connection, args, timeout)
            },
        );
        release_tx.send(()).unwrap();
        (first.join().unwrap(), second)
    });
    let Err(second) = second else {
        panic!("another request cannot reserve the same retired check");
    };
    assert_eq!(second.status, StatusCode::CONFLICT);
    assert_eq!(alternate_notes.get(), 0);
    let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } =
        first.unwrap_or_else(|error| panic!("{}", error.message))
    else {
        panic!("the request that reserved the note admits the evaluator");
    };
    assert!(delegation.delegation.prompt.contains(DISPOSITION_RECORD));
    assert!(!delegation.delegation.prompt.contains(&alternate));
}

#[test]
fn concurrent_retired_receipt_cannot_admit_an_older_evidence_cut() {
    let fixture = retired_check_fixture("passed");
    let state = &fixture.state;
    let parent = &fixture.parent;
    let (references_read, wait_for_references) = mpsc::channel();
    let (resume_ordinary, wait_for_ordinary) = mpsc::channel();
    let (receipt_confirmed, wait_for_receipt) = mpsc::channel();
    let (resume_disposition, wait_for_disposition) = mpsc::channel();
    let (ordinary, disposition, no_early_evaluator) = std::thread::scope(|scope| {
        let ordinary = scope.spawn(move || {
            TEST_ACCEPTANCE_AFTER_RETIRED_REFERENCES.with(|step| {
                *step.borrow_mut() = Some(Box::new(move || {
                    references_read.send(()).unwrap();
                    wait_for_ordinary
                        .recv_timeout(phase_sync::DEADLOCK_GUARD)
                        .unwrap();
                }));
            });
            state.request_acceptance_evaluation_with_runner(
                parent,
                evaluation_request(Some(Agent::Codex)),
                independent_reader(),
            )
        });
        wait_for_references
            .recv_timeout(phase_sync::DEADLOCK_GUARD)
            .unwrap();
        let disposition = scope.spawn(move || {
            let reader = disposition_reader(Arc::default(), None);
            let note_written = std::cell::Cell::new(false);
            state.request_acceptance_evaluation_with_runner(
                parent,
                disposition_request(COMMITTED_RUN_ID),
                |connection, args, timeout| {
                    if args.iter().any(|arg| arg == "inspect") && note_written.replace(false) {
                        // The post-disposition bracket has begun, so the
                        // receipt is confirmed but no evaluator can exist yet.
                        receipt_confirmed.send(()).unwrap();
                        wait_for_disposition
                            .recv_timeout(phase_sync::DEADLOCK_GUARD)
                            .unwrap();
                    }
                    let reply = reader(connection, args, timeout);
                    if args.iter().any(|arg| arg == "note") && reply.is_ok() {
                        note_written.set(true);
                    }
                    reply
                },
            )
        });
        wait_for_receipt
            .recv_timeout(phase_sync::DEADLOCK_GUARD)
            .unwrap();
        resume_ordinary.send(()).unwrap();
        let ordinary = ordinary.join().unwrap();
        let no_early_evaluator = state.inner.lock().unwrap().delegations.is_empty();
        resume_disposition.send(()).unwrap();
        (ordinary, disposition.join().unwrap(), no_early_evaluator)
    });

    let Err(ordinary) = ordinary else {
        panic!("a late receipt cannot remove the last blocker of the older cut");
    };
    assert_eq!(ordinary.status, StatusCode::CONFLICT);
    assert!(
        ordinary
            .message
            .starts_with(AcceptanceCommittedInputRefusal::RetiredChecksChanged.code())
    );
    assert!(no_early_evaluator);
    let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } =
        disposition.unwrap_or_else(|error| panic!("{}", error.message))
    else {
        panic!("the fresh bracket can admit using the confirmed receipt");
    };
    assert!(delegation.delegation.prompt.contains(DISPOSITION_RECORD));
}

#[test]
fn receipt_confirmation_refuses_fresh_and_reused_attempt_installation() {
    for reuse in [false, true] {
        let fixture = retired_check_fixture("passed");
        let state = &fixture.state;
        let parent = &fixture.parent;
        let head =
            run_git_test_command_output(&worktree(&fixture.current_root), &["rev-parse", "HEAD"]);
        let current_run = "test-current-root-pass";
        let mut results = committed_results("passed", input_record(&head, "clean"));
        results["runId"] = json!(current_run);
        let current =
            register_committed_run_as(state, parent, &fixture.current_root, current_run, results);
        host_observed(state, &current, &head, acknowledged("settled"));
        let mut evaluation = evaluation_request(Some(Agent::Codex));
        if reuse {
            let original =
                request(state, parent).unwrap_or_else(|error| panic!("{}", error.message));
            let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } = original else {
                panic!("the current-root pass admits the original evaluator");
            };
            evaluation.reuse_delegation_id = Some(delegation.delegation.id.clone());
            let mut inner = state.inner.lock().unwrap();
            let index = inner
                .find_delegation_index(&delegation.delegation.id)
                .unwrap();
            inner.delegations[index].status = DelegationStatus::Completed;
            inner.mark_delegation_mutated(index);
            inner
                .session_mut(&delegation.delegation.child_session_id)
                .unwrap()
                .queued_prompts
                .clear();
            state.commit_locked(&mut inner).unwrap();
        }
        let digest = fixture
            .registration
            .verification_record
            .as_ref()
            .and_then(TestRunVerificationAck::body_digest)
            .unwrap();
        let pending: AcceptanceRetiredCheckReceipt = serde_json::from_value(json!({
            "bodyDigest": digest,
            "evidenceRecord": DISPOSITION_RECORD
        }))
        .unwrap();
        let before = {
            let mut inner = state.inner.lock().unwrap();
            // Keep the acknowledged note in the state left by a failed fence.
            inner
                .test_run_registrations
                .get_mut(COMMITTED_RUN_ID)
                .unwrap()
                .retired_check_disposition = Some(pending.clone());
            inner
                .pending_retired_check_receipts
                .insert(COMMITTED_RUN_ID.into(), pending);
            inner.delegations.iter().cloned().collect::<Vec<_>>()
        };
        TEST_ACCEPTANCE_BEFORE_ATTEMPT_INSTALL.with(|slot| {
            let state = state.clone();
            let parent = parent.clone();
            *slot.borrow_mut() = Some(Box::new(move |reused| {
                assert_eq!(reused, reuse, "the intended installation path was reached");
                let (target, root) = {
                    let inner = state.inner.lock().unwrap();
                    (
                        acceptance_evaluation_host_target_locked(&inner, &parent).unwrap(),
                        AcceptanceEvaluationSourceRoot::from_entry(
                            &inner.engram_work_source_roots[0],
                        ),
                    )
                };
                let mut task = fixture_task();
                task.canonical_identity =
                    acceptance_core_identity(&evidence_selection::canonical_core_receipt())
                        .unwrap();
                let disposition = state
                    .record_acceptance_check_dispositions(
                        &parent,
                        &target.connection,
                        &target.store,
                        &task,
                        Some(&root),
                        &disposition_request(COMMITTED_RUN_ID).retired_check_dispositions,
                        None,
                        &|_, _, _| panic!("reconfirmation does not write another note"),
                        std::time::Instant::now() + phase_sync::DEADLOCK_GUARD,
                        &std::time::Instant::now,
                    )
                    .unwrap_or_else(|error| panic!("{}", error.message));
                assert!(disposition.disclosures.is_empty());
                assert!(
                    !state
                        .inner
                        .lock()
                        .unwrap()
                        .pending_retired_check_receipts
                        .contains_key(COMMITTED_RUN_ID)
                );
            }));
        });
        let result = state.request_acceptance_evaluation_with_runner(
            parent,
            evaluation,
            independent_reader(),
        );
        let Err(error) = result else {
            panic!("receipt drift cannot install the attempt");
        };
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(
            error
                .message
                .starts_with(AcceptanceCommittedInputRefusal::RetiredChecksChanged.code())
        );
        let inner = state.inner.lock().unwrap();
        assert!(inner.delegations.iter().eq(&before));
    }
}

#[test]
fn pending_retired_check_remains_blocking_and_refences_the_same_note_on_retry() {
    for compacted in [false, true] {
        let (mut fixture, runs) = retired_check_fixture_with_runs(&["passed", "failed"]);
        if compacted {
            selection_edges::retain_registration(&fixture.state, &runs[0]);
        }
        let (tx, rx) = mpsc::channel();
        fixture.state.persist_tx = tx;
        let calls: RecordedEngramCalls = Arc::default();
        let request = || {
            fixture.state.request_acceptance_evaluation_with_runner(
                &fixture.parent,
                disposition_request(COMMITTED_RUN_ID),
                disposition_reader(calls.clone(), None),
            )
        };
        let next_fence = || loop {
            let event = rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
            if let PersistRequest::Fence(fence) = &event
                && matches!(
                    &fence.target,
                    PersistFenceTarget::RetiredCheckDisposition { .. }
                )
            {
                break event;
            }
        };
        let blockers = || {
            let decision = decide(&fixture.state, &fixture.parent);
            let Err(AcceptanceCommittedInputRefusal::RootRebound { checks, .. }) = decision else {
                panic!("expected retired blockers, got {decision:?}");
            };
            checks
                .into_iter()
                .map(|(run_id, _)| run_id)
                .collect::<BTreeSet<_>>()
        };
        std::thread::scope(|scope| {
            let mut batch = PersistFenceBatch::default();
            let first = scope.spawn(&request);
            batch.accept(next_fence());
            let both = runs
                .iter()
                .map(|row| row.run_id.clone())
                .collect::<BTreeSet<_>>();
            let during_first = blockers();
            batch.fail(PersistFenceError::WriteFailed(
                "fixture refused the write".into(),
            ));
            let first = refusal(
                &fixture.state,
                first.join().unwrap(),
                "failed persistence fence",
            );
            assert_eq!(during_first, both);
            assert_eq!(first.status, StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(blockers(), both);

            let retry = scope.spawn(&request);
            batch.accept(next_fence());
            let during_retry = blockers();
            let delta = collect_persist_delta_from_shared_state(&fixture.state.inner, 0);
            persist_delta_with_fences(
                &mut SqlitePersistConnectionCache::new(),
                &fixture.state.persistence_path,
                &delta,
                &mut batch,
            )
            .unwrap();
            let later = refusal(
                &fixture.state,
                retry.join().unwrap(),
                "other retired check still blocks",
            );
            assert_eq!(
                during_retry, both,
                "retry still awaits the exact writer result"
            );
            assert_eq!(later.status, StatusCode::CONFLICT);
            assert_eq!(blockers(), BTreeSet::from([runs[1].run_id.clone()]));
        });
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
}

#[test]
fn receipt_only_retry_refuses_a_store_change_during_confirmation() {
    assert_retry_refuses_store_change_during_confirmation(false);
}

#[test]
fn mixed_retry_refuses_a_store_change_before_writing_a_new_disposition() {
    assert_retry_refuses_store_change_during_confirmation(true);
}

fn assert_retry_refuses_store_change_during_confirmation(include_undisposed_run: bool) {
    let (mut fixture, runs) = retired_check_fixture_with_runs(&["passed", "failed"]);
    let new_run_id = &runs[1].run_id;
    let mut request = disposition_request(COMMITTED_RUN_ID);
    if include_undisposed_run {
        request
            .retired_check_dispositions
            .push(AcceptanceRetiredCheckRequest {
                run_id: new_run_id.clone(),
                reason: DISPOSITION_REASON.to_owned(),
            });
    }
    let (tx, rx) = mpsc::channel();
    fixture.state.persist_tx = tx;
    let digest = fixture
        .registration
        .verification_record
        .as_ref()
        .and_then(TestRunVerificationAck::body_digest)
        .unwrap();
    let receipt: AcceptanceRetiredCheckReceipt = serde_json::from_value(json!({
        "bodyDigest": digest,
        "evidenceRecord": DISPOSITION_RECORD,
    }))
    .unwrap();
    let (project, target) = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let project = acceptance_evaluation_project_locked(&inner, &fixture.parent)
            .unwrap()
            .id
            .clone();
        let target = acceptance_evaluation_host_target_locked(&inner, &fixture.parent).unwrap();
        inner
            .test_run_registrations
            .get_mut(COMMITTED_RUN_ID)
            .unwrap()
            .retired_check_disposition = Some(receipt.clone());
        inner
            .pending_retired_check_receipts
            .insert(COMMITTED_RUN_ID.into(), receipt.clone());
        (project, target)
    };
    let calls: RecordedEngramCalls = Arc::default();
    let (response, reads_before_confirmation) = std::thread::scope(|scope| {
        let reader = disposition_reader(calls.clone(), None);
        let state = &fixture.state;
        let parent = &fixture.parent;
        let retry = scope.spawn(move || {
            state.request_acceptance_evaluation_with_runner(parent, request, reader)
        });
        let mut batch = PersistFenceBatch::default();
        loop {
            let event = rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
            if let PersistRequest::Fence(fence) = &event
                && matches!(
                    &fence.target,
                    PersistFenceTarget::RetiredCheckDisposition { .. }
                )
            {
                batch.accept(event);
                break;
            }
        }
        let reads_before_confirmation = calls.lock().unwrap().len();
        let new_store = rotate_store(&fixture.state, &project, &target.connection.project_root);
        assert_ne!(new_store, target.store);
        let delta = collect_persist_delta_from_shared_state(&fixture.state.inner, 0);
        persist_delta_with_fences(
            &mut SqlitePersistConnectionCache::new(),
            &fixture.state.persistence_path,
            &delta,
            &mut batch,
        )
        .unwrap();
        (retry.join().unwrap(), reads_before_confirmation)
    });
    let calls = calls.lock().unwrap();
    assert!(
        calls
            .iter()
            .all(|(_, args)| !args.iter().any(|arg| arg == "note"))
    );
    assert!(
        current_registration(&fixture.state).retired_check_disposition == Some(receipt),
        "the confirmed receipt remains on the original registration"
    );
    let error = refusal(
        &fixture.state,
        response,
        "disposition retry on another store",
    );
    assert_store_changed(&error);
    assert_eq!(calls.len(), reads_before_confirmation);
    let inner = fixture.state.inner.lock().unwrap();
    assert!(
        inner.test_run_registrations[new_run_id.as_str()]
            .retired_check_disposition
            .is_none()
    );
    assert!(
        !inner
            .pending_retired_check_receipts
            .contains_key(COMMITTED_RUN_ID)
    );
    assert!(
        inner
            .engram_work_source_roots
            .iter()
            .any(|root| { root.store == target.store && root.root == fixture.current_root })
    );
}

#[test]
fn retired_note_references_yield_to_the_complete_contract_and_selected_proof() {
    let mut task = fixture_task();
    let criterion = "criterion text ".repeat(3_000);
    let proof = "Selected evidence must remain complete.\n".repeat(250);
    let locator = "e".repeat(32);
    task.criteria = vec![criterion.trim_end().to_owned()];
    task.indexed_evidence = vec![AcceptanceEvaluationEvidence {
        locator: locator.clone(),
        kind: "note".into(),
        by: None,
        created_at: None,
        summary: Some(proof.clone()),
        non_holder: false,
        body_bytes: Some(proof.len() as u64),
        cut_by_tracker: false,
        verification: None,
    }];
    task.criterion_evidence = vec![AcceptanceCriterionEvidence {
        criterion: 1,
        locators: vec![locator],
        association: "requester".into(),
    }];
    let references: Vec<_> = (0..33)
        .map(|index| {
            json!({
                "runId": format!("test-historical-run-number-{index:02}"),
                "generation": index,
                "evidenceRecord": format!("{index:032x}"),
            })
        })
        .collect();
    let notices = acceptance_retired_context_notices(&[], &references).unwrap();
    for same_session in [false, true] {
        let build = |budget| {
            if same_session {
                build_same_session_acceptance_brief_with_plan(&task, None, budget)
                    .map(|brief| brief.0)
            } else {
                build_acceptance_evaluator_brief_for_agent(&task, ".", Agent::Codex, budget)
                    .map(|brief| brief.prompt)
            }
        };
        let baseline = build(MAX_ACCEPTANCE_BRIEF_BYTES).unwrap();
        let budget = baseline.len() + 512;
        assert!(
            build(budget.saturating_sub(notices[0].len())).is_err(),
            "carrying all audit references would displace required content"
        );
        let (brief, selected, _) =
            acceptance_brief_with_retired_context(budget, &notices, build, String::len).unwrap();
        assert!(brief.contains(criterion.trim_end()));
        assert!(brief.contains(proof.trim_end()));
        assert!(brief.len() + selected.len() <= budget);
        assert!(
            !selected.is_empty(),
            "the brief can still point to the notes"
        );
        assert!(!selected.contains("test-historical-run-number-00"));
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
