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
            fixture.original_root.as_str(),
        ] {
            assert!(
                record.prompt.contains(required),
                "missing {required}: {}",
                record.prompt
            );
        }
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
