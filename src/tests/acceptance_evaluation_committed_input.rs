// Tests of the committed-input decision of an acceptance evaluation
// (src/acceptance_committed_input.rs). A task whose claim has a registered
// committed-input run is in the committed flow, whatever the launcher's
// committed-input default: a clean named root whose HEAD an eligible, passed run of the claim was
// judged on, with that run's verification record acknowledged by Engram
// (received or settled) for that commit as the host observed it, is admitted
// without a sighting read; every other case (an unacknowledged record, one
// acknowledged only by the state file beside the run, a root that moves while
// the run is read, another commit, a dirty root, a
// root whose index skips a tracked path, a debris, input-changed, failed or
// ineligible run, a record rewritten with its results to another commit after
// Engram's answer, a run whose results were deleted or read incoherent) is refused with its typed reason, also without a sighting
// read. An admitted evaluation judges and declares `git-commit:<HEAD>`, the
// source the run's verification record names, and is judged again on that
// commit at its first submission. A task with no such run (none registered,
// one of another work, or one that never started on a clean checkout) is in
// the legacy flow, which still declares the content revision. Owns those witnesses and the helpers that register such
// a run through the registration seam and write its results and its record's
// files as the sender leaves them. Does not own the sighting preflight's own
// tests or their fixture, which it uses from its parent module,
// src/tests/acceptance_evaluation_sighting.rs, or the sender's tests
// (src/tests/engram_verification_record.rs). New module, a child of that file
// so its private fixture stays private.
use super::*;

#[path = "acceptance_committed_input_latest.rs"]
mod latest;

#[path = "acceptance_committed_input_selection_edges.rs"]
mod selection_edges;

/// The run every case registers.
const COMMITTED_RUN_ID: &str = "test-committed-input-admission";

/// The producer object a fixture receipt names, a well-formed record id.
const RECORD_PRODUCER: &str = "5f0c2b6e8d4a4e1f9a3b7c6d5e4f3a2b";

/// The named root as a user-facing path: the entry keeps the canonical
/// spelling (on Windows with its verbatim prefix), which the Git test helper
/// is not given.
fn worktree(named_root: &str) -> PathBuf {
    normalize_user_facing_path(FsPath::new(named_root))
}

/// Commits the fixture's untracked file in the named root, which leaves it
/// clean, and returns the root's HEAD.
fn commit_named_root(named_root: &str) -> String {
    let root = worktree(named_root);
    run_git_test_command(&root, &["add", "judged.txt"]);
    run_git_test_command(&root, &["commit", "--quiet", "-m", "judged in the named root"]);
    run_git_test_command_output(&root, &["rev-parse", "HEAD"])
}

/// The launcher's `input` record of a run on `commit` that ended `outcome`.
fn input_record(commit: &str, outcome: &str) -> Value {
    let (clean, paths, eligible) = match outcome {
        "clean" => (true, json!([]), true),
        "debris_pending_disposition" => (
            false,
            json!([{ "status": "??", "path": "debris.txt" }]),
            false,
        ),
        "input_changed" => (
            false,
            json!([{ "status": " M", "path": "judged.txt" }]),
            false,
        ),
        other => panic!("no such outcome {other}"),
    };
    json!({
        "kind": "commit",
        "commit": commit,
        "cleanBefore": true,
        "after": { "commit": commit, "clean": clean, "paths": paths },
        "outcome": outcome,
        "eligible": eligible,
    })
}

/// Terminal results of the run in `state` ("passed" or "failed") carrying
/// `input`.
fn committed_results(state: &str, input: Value) -> Value {
    let code = if state == "passed" { 0 } else { 1 };
    json!({
        "runId": COMMITTED_RUN_ID,
        "state": state,
        "exitCode": code,
        "ended": "2026-10-30T10:10:00.000Z",
        "stages": [
            { "name": "rust-tests", "state": state, "code": code,
              "started": "2026-10-30T10:00:05.000Z", "ended": "2026-10-30T10:10:00.000Z",
              "command": ["sh", "scripts/test-rust.sh"] },
        ],
        "input": input,
    })
}

/// Lays out a committed-input run (`"input": "commit"`) of the named root as
/// the launcher does, with `results`, and registers it through the
/// registration seam as `parent`, whose name of that root pins the run to the
/// task's claim.
fn register_committed_run(
    state: &AppState,
    parent: &str,
    named_root: &str,
    results: Value,
) -> TestRunRegistration {
    register_committed_run_as(state, parent, named_root, COMMITTED_RUN_ID, results)
}

/// [`register_committed_run`] for the run `run_id`, whose `results` name it.
fn register_committed_run_as(
    state: &AppState,
    parent: &str,
    named_root: &str,
    run_id: &str,
    results: Value,
) -> TestRunRegistration {
    try_register_committed_run_as(state, parent, named_root, run_id, results)
        .unwrap_or_else(|error| panic!("the run registers: {}", error.message))
}

fn try_register_committed_run_as(
    state: &AppState,
    parent: &str,
    named_root: &str,
    run_id: &str,
    results: Value,
) -> Result<TestRunRegistration, ApiError> {
    let root = worktree(named_root);
    let common = test_runs_git_common_dir(&root)
        .and_then(|common| fs::canonicalize(common).ok())
        .expect("the named root's common Git directory resolves");
    let directory = normalize_user_facing_path(&common.join("review-runs").join(run_id));
    fs::create_dir_all(&directory).unwrap();
    let request = json!({
        "runId": run_id,
        "root": test_run_display_path(&root),
        "full": true,
        "stages": [{ "name": "rust-tests", "command": "sh", "args": ["scripts/test-rust.sh"] }],
        "owner": parent,
        "started": "2026-10-30T10:00:00.000Z",
        "creatorPid": std::process::id(),
        "detached": true,
        "input": "commit",
    });
    fs::write(directory.join("request.json"), request.to_string()).unwrap();
    fs::write(directory.join("results.json"), results.to_string()).unwrap();
    state
        .register_test_run(
            parent,
            RegisterTestRunRequest {
                run_id: run_id.to_owned(),
                run_directory: test_run_display_path(&directory),
            },
        )?;
    let registration = state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .get(run_id)
        .cloned()
        .expect("the registration is recorded");
    assert_eq!(
        registration
            .work_binding
            .as_ref()
            .map(|binding| (binding.work_id.as_str(), binding.claim_id.as_str())),
        Some((SIGHTED_WORK_ID, "claim-task")),
        "the run is bound to the claim that named its worktree"
    );
    assert_eq!(
        registration
            .named_root
            .as_ref()
            .map(|named| (named.workspace_id.as_str(), named.generation)),
        Some((named_root, 1)),
        "the run is pinned to the named root"
    );
    Ok(registration)
}

/// Writes the run's verification record beside it as the sender leaves it:
/// the frozen body the host builds from its own registration and the run's
/// request and results, in its exact serialized bytes; and the record's
/// `progress` state file. A `commit` other than the run's own puts that commit
/// in the body's input, so the body is no longer the host's record.
fn write_record(registration: &TestRunRegistration, commit: &str, progress: Value) {
    let directory = PathBuf::from(&registration.run_directory);
    let read = |name: &str| -> Value {
        serde_json::from_str(&fs::read_to_string(directory.join(name)).unwrap()).unwrap()
    };
    let mut body = engram_verification_record_body(registration, &read("request.json"), &read("results.json"))
        .expect("the fixture run builds its verification record");
    let input = EngramVerificationRecordInput::Commit {
        commit: commit.to_owned(),
    };
    if body.input != input {
        body.input = input;
    }
    fs::write(
        directory.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE),
        engram_verification_record_text(&body).expect("the fixture record serializes"),
    )
    .unwrap();
    fs::write(
        directory.join(ENGRAM_VERIFICATION_RECORD_STATE_FILE),
        progress.to_string(),
    )
    .unwrap();
}

/// Engram's acknowledgement of the fixture record, in `state` ("received" or
/// "settled"): a receipt naming its producer and one record for its check.
/// Structurally valid for the fixture record, so only where it is kept
/// decides whether it acknowledges.
fn acknowledged(state: &str) -> Value {
    json!({
        "state": state,
        "receipt": {
            "operation": RECORD_PRODUCER,
            "producer": RECORD_PRODUCER,
            "verifications": [{ "stage": "rust-tests", "record_id": "a".repeat(32) }],
            "run_outcome": "passed",
        },
    })
}

/// The record as the sender leaves it once the host itself observed
/// `progress` (`acknowledged`'s shape, or a rejection): the frozen body and
/// the state file's mirror beside the run, and the host's own observation on
/// the registration in state, kept through the sender's own function.
/// A receipt is kept with the digest of the body it answered, as the sender
/// keeps it: here the body just written.
fn host_observed(state: &AppState, registration: &TestRunRegistration, commit: &str, progress: Value) {
    write_record(registration, commit, progress.clone());
    let mut progress = progress;
    if progress.get("receipt").is_some() {
        let frozen = engram_verification_record_frozen(
            FsPath::new(&registration.run_directory),
            &registration.run_id,
        )
        .expect("the written body reads back");
        progress["body_digest"] = json!(engram_verification_record_body_digest(&frozen.body));
    }
    let ack: TestRunVerificationAck =
        serde_json::from_value(progress).expect("the fixture's progress is a host observation");
    state.note_engram_verification_record_ack(registration, ack);
    assert!(
        state.inner.lock().unwrap().test_run_registrations[&registration.run_id]
            .verification_record
            .is_some(),
        "the host keeps its observation on the registration"
    );
}

/// The run's registration as it now stands in state.
fn current_registration(state: &AppState) -> TestRunRegistration {
    state.inner.lock().unwrap().test_run_registrations[COMMITTED_RUN_ID].clone()
}

/// The task the fixture's reads describe, naming its work by id as the
/// canonical receipt does, under its claim at the naming fence.
fn fixture_task() -> AcceptanceEvaluationTask {
    task_under(current_claim(1, None))
}

/// The fixture task as requested while its claim stood as `claim`.
fn task_under(claim: AcceptanceCurrentClaim) -> AcceptanceEvaluationTask {
    let mut task = parse_acceptance_evaluation_task(show_receipt(None), full_receipt())
        .expect("the fixture's show is a task");
    task.work_id = Some(SIGHTED_WORK_ID.to_owned());
    task.current_claim = Some(claim);
    task
}

/// The fixture's claim as Engram's held read lists it at `fence`, with the
/// binding it would accept when one is listed.
fn current_claim(fence: i64, binding: Option<EngramControlWorkBinding>) -> AcceptanceCurrentClaim {
    AcceptanceCurrentClaim {
        claim_id: "claim-task".to_owned(),
        claim_fence: fence,
        binding,
    }
}

/// The committed-input decision for the fixture task in the named root, as
/// the request takes it.
fn decide(
    state: &AppState,
    parent: &str,
) -> Result<AcceptanceCommittedInputDecision, AcceptanceCommittedInputRefusal> {
    decide_by(
        state,
        parent,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    )
}

/// [`decide`] under the request deadline `deadline`.
fn decide_by(
    state: &AppState,
    parent: &str,
    deadline: std::time::Instant,
) -> Result<AcceptanceCommittedInputDecision, AcceptanceCommittedInputRefusal> {
    decide_task(state, parent, &fixture_task(), deadline)
}

/// The committed-input decision for `task` in the named root.
fn decide_task(
    state: &AppState,
    parent: &str,
    task: &AcceptanceEvaluationTask,
    deadline: std::time::Instant,
) -> Result<AcceptanceCommittedInputDecision, AcceptanceCommittedInputRefusal> {
    let (target, root) = {
        let inner = state.inner.lock().unwrap();
        let target = acceptance_evaluation_host_target_locked(&inner, parent)
            .expect("the host reader is configured");
        let root = AcceptanceEvaluationSourceRoot::from_entry(&inner.engram_work_source_roots[0]);
        (target, root)
    };
    state.acceptance_committed_input_decision(
        parent,
        &target.connection,
        &target.store,
        task,
        Some(&root),
        deadline,
        &std::time::Instant::now,
    )
}

/// Asserts that the committed flow refused with `code` and that nothing read
/// the sighting.
fn assert_refused_without_a_sighting(
    transport: &ScriptedEngramControlTransport,
    decision: Result<AcceptanceCommittedInputDecision, AcceptanceCommittedInputRefusal>,
    code: &str,
    case: &str,
) -> AcceptanceCommittedInputRefusal {
    let refusal = match decision {
        Err(refusal) => refusal,
        Ok(decision) => panic!("{case}: the committed flow must refuse, not {decision:?}"),
    };
    assert_eq!(refusal.code(), code, "{case}: {refusal:?}");
    let error = refusal.clone().into_api_error("w-task");
    assert_eq!(error.status, StatusCode::CONFLICT, "{case}");
    assert!(
        error.message.starts_with(code),
        "{case}: the message leads with its code: {}",
        error.message
    );
    assert_eq!(sighting_reads(transport), 0, "{case}: no sighting read in this flow");
    refusal
}

/// The request's outcome when the root was not admitted on a committed run
/// and Engram has no sighting of it: the sighting preflight's refusal, after
/// exactly one read.
fn assert_fell_back_to_the_sighting(
    state: &AppState,
    transport: &ScriptedEngramControlTransport,
    response: Result<AcceptanceEvaluationRequestResponse, ApiError>,
    case: &str,
) {
    let error = refusal(state, response, case);
    assert!(
        error.message.contains("has no sighting"),
        "{case}: the sighting preflight decides: {}",
        error.message
    );
    assert_eq!(sighting_reads(transport), 1, "{case}: the sighting was read");
}

/// Requests the fixture's independent evaluation as `parent`.
fn request(
    state: &AppState,
    parent: &str,
) -> Result<AcceptanceEvaluationRequestResponse, ApiError> {
    state.request_acceptance_evaluation_with_runner(
        parent,
        evaluation_request(Some(Agent::Codex)),
        independent_reader(),
    )
}

#[test]
fn an_acknowledged_committed_run_at_head_is_admitted_without_a_sighting_read() {
    for progress in ["received", "settled"] {
        let (state, parent, _store, named_root) = named_root_parent();
        let head = commit_named_root(&named_root);
        // Engram has no sighting: a read would refuse the request.
        let transport =
            install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));
        let registration = register_committed_run(
            &state,
            &parent,
            &named_root,
            committed_results("passed", input_record(&head, "clean")),
        );
        host_observed(&state, &registration, &head, acknowledged(progress));

        assert_eq!(
            decide(&state, &parent),
            Ok(AcceptanceCommittedInputDecision::Admitted(
                AcceptanceCommittedInputAdmission {
                    run_id: COMMITTED_RUN_ID.to_owned(),
                    commit: head.clone(),
                }
            )),
            "{progress}"
        );
        // The request itself is admitted the same way.
        let response = request(&state, &parent).unwrap_or_else(|error| {
            panic!("{progress}: the committed run admits the request: {}", error.message)
        });
        let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } = response else {
            panic!("{progress}: an independent evaluation spawns an evaluator");
        };
        assert_eq!(sighting_reads(&transport), 0, "{progress}: no sighting read");
        let record = delegation.delegation;
        let target = record
            .acceptance_evaluation
            .clone()
            .expect("an evaluator carries its target");
        assert_judges_the_admitted_commit(&target, &registration, &named_root, &head, progress);

        // Judged again at the first submission by the committed-input rule:
        // the root still clean at the admitted commit keeps the declaration,
        // which a content revision would never equal.
        let declared = state
            .acceptance_evaluation_declared_target(&record.child_session_id, &target)
            .unwrap_or_else(|error| {
                panic!("{progress}: an unchanged root keeps its commit: {}", error.message)
            });
        assert_eq!(
            declared.source_fingerprint,
            Some(format!("git-commit:{head}")),
            "{progress}"
        );
        // A root no longer clean at that commit is the changed worktree.
        fs::write(worktree(&named_root).join("judged.txt"), "changed after the request\n")
            .unwrap();
        let error = state
            .acceptance_evaluation_declared_target(&record.child_session_id, &target)
            .expect_err("a root no longer clean at the admitted commit is refused");
        assert_eq!(error.status, StatusCode::CONFLICT, "{progress}");
        assert!(
            error.message.contains("the worktree changed while it was evaluated")
                && error.message.contains(&format!("requested at git-commit:{head}"))
                && error.message.contains("is not clean at a commit"),
            "{progress}: {}",
            error.message
        );
    }
}

#[test]
fn a_pinned_child_uses_the_existing_record_and_acceptance_but_child_dirt_refuses() {
    let (state, parent, _store, named_root) = named_root_parent();
    let root = worktree(&named_root);
    let origin = root.join(".worktrees").join("child-origin");
    fs::create_dir_all(&origin).unwrap();
    init_git_document_test_repo(&origin);
    fs::write(origin.join("child.txt"), "pinned child\n").unwrap();
    run_git_test_command(&origin, &["add", "--", "child.txt"]);
    run_git_test_command(&origin, &["commit", "--quiet", "-m", "child"]);
    run_git_test_command(&root, &["-c", "core.autocrlf=false", "-c", "protocol.file.allow=always", "submodule", "add", "--quiet",
        origin.to_str().unwrap(), "dep"]);
    run_git_test_command(&root.join("dep"), &["config", "core.autocrlf", "false"]);
    run_git_test_command(&root, &["add", "--", "judged.txt"]);
    run_git_test_command(&root, &["commit", "--quiet", "-m", "pin child"]);
    let head = run_git_test_command_output(&root, &["rev-parse", "HEAD"]);
    assert_eq!(committed_source_clean_head(&root), Some(head.clone()));
    let transport = install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));
    let registration = register_committed_run(&state, &parent, &named_root,
        committed_results("passed", input_record(&head, "clean")));
    host_observed(&state, &registration, &head, acknowledged("settled"));
    let frozen = engram_verification_record_frozen(FsPath::new(&registration.run_directory), &registration.run_id).unwrap();
    assert_eq!(frozen.record["input"], json!({"kind":"commit", "commit": head}));
    assert_eq!(decide(&state, &parent), Ok(AcceptanceCommittedInputDecision::Admitted(
        AcceptanceCommittedInputAdmission { run_id: COMMITTED_RUN_ID.to_owned(), commit: head },
    )));
    fs::write(root.join("dep/child.txt"), "dirty child\n").unwrap();
    assert_eq!(assert_refused_without_a_sighting(&transport, decide(&state, &parent),
        "committed_input_not_clean", "tracked child edit"), AcceptanceCommittedInputRefusal::NotClean);
    run_git_test_command(&root.join("dep"), &["checkout", "--", "child.txt"]);
    fs::write(root.join("dep/new.txt"), "child debris\n").unwrap();
    assert_eq!(assert_refused_without_a_sighting(&transport, decide(&state, &parent),
        "committed_input_not_clean", "untracked child edit"), AcceptanceCommittedInputRefusal::NotClean);
}

/// Asserts that `target`, an evaluation admitted on `registration`'s run at
/// `head` in `named_root`, judges and declares `git-commit:<head>` in that
/// root: exactly the source the run's verification record names, so the brief
/// lists that record as citable while a record of another revision stays
/// context only.
fn assert_judges_the_admitted_commit(
    target: &DelegationAcceptanceEvaluation,
    registration: &TestRunRegistration,
    named_root: &str,
    head: &str,
    case: &str,
) {
    let judged = format!("git-commit:{head}");
    assert_eq!(
        target.source_fingerprint.as_deref(),
        Some(judged.as_str()),
        "{case}: the evaluation declares the admitted commit"
    );
    let source_root = target.source_root.as_ref().expect("the target records the root");
    assert_eq!(source_root.root, named_root, "{case}");
    let frozen = engram_verification_record_frozen(
        FsPath::new(&registration.run_directory),
        &registration.run_id,
    )
    .expect("the run's record reads back");
    let basis = engram_verification_record_basis(&frozen.body)
        .expect("a committed-input record has a source basis");
    assert_eq!(
        (basis.workspace_id.as_str(), basis.source_revision.as_str()),
        (source_root.root.as_str(), judged.as_str()),
        "{case}: the run's verification record ran on the judged source"
    );

    const ON_SOURCE: &str = "1111aaaa1111aaaa1111aaaa1111aaaa";
    const OFF_SOURCE: &str = "2222bbbb2222bbbb2222bbbb2222bbbb";
    let record = |locator: &str, revision: &str| {
        json!({
            "locator": locator, "kind": "verification", "family": "notes",
            "body_bytes": 22, "by": "greg/claude", "created_at": "2026-10-30T10:20:00Z",
            "non_holder": false, "summary": "the gate passed",
            "verification": {
                "check_kind": "test", "result": "passed",
                "source_revision": revision, "source_workspace": basis.workspace_id,
            }
        })
    };
    let mut show = show_receipt(None);
    show["notes"] = json!([
        record(ON_SOURCE, &basis.source_revision),
        record(OFF_SOURCE, "content-v1:4983be8a"),
    ]);
    let mut task = parse_acceptance_evaluation_task(show, full_receipt())
        .expect("the show is a task");
    task.judged_revision = target.source_fingerprint.clone();
    task.judged_workspace = acceptance_source_workspace(&source_root.root);
    let independent = build_acceptance_evaluator_brief(&task, named_root, MAX_ACCEPTANCE_BRIEF_BYTES)
        .expect("the brief builds")
        .prompt;
    let same_session = build_same_session_acceptance_brief(
        &task,
        target.source_fingerprint.as_deref(),
        MAX_ACCEPTANCE_BRIEF_BYTES,
    )
    .expect("the brief builds");
    for (name, prompt) in [("independent", &independent), ("same_session", &same_session)] {
        let lines = |locator: &str| {
            prompt
                .lines()
                .filter(|line| line.contains(locator) && line.contains("(verification test"))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let on_source = lines(ON_SOURCE);
        assert!(
            !on_source.is_empty() && on_source.iter().all(|line| !line.contains("context only")),
            "{case} {name}: the run's record is on the judged source: {prompt}"
        );
        let off_source = lines(OFF_SOURCE);
        assert!(
            !off_source.is_empty()
                && off_source.iter().all(|line| line.contains(&format!(
                    "[context only: it ran at content-v1:4983be8a, not at {judged}"
                ))),
            "{case} {name}: a record of another revision stays context only: {prompt}"
        );
    }
}

#[test]
fn an_admitted_same_session_request_hands_the_session_the_admitted_commit() {
    let (state, parent, _store, named_root) = named_root_parent();
    let head = commit_named_root(&named_root);
    let transport = install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));
    let registration = register_committed_run(
        &state,
        &parent,
        &named_root,
        committed_results("passed", input_record(&head, "clean")),
    );
    host_observed(&state, &registration, &head, acknowledged("settled"));

    let response = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(None),
            reader_admitting("same_session"),
        )
        .unwrap_or_else(|error| panic!("the committed run admits the request: {}", error.message));
    assert_eq!(sighting_reads(&transport), 0, "no sighting read");
    let wire = serde_json::to_value(&response).unwrap();
    let judged = format!("git-commit:{head}");
    assert_eq!(wire["sourceFingerprint"], judged.as_str());
    assert!(
        wire["brief"]
            .as_str()
            .unwrap()
            .contains(&format!("source_fingerprint {judged}")),
        "{}",
        wire["brief"]
    );
}

#[test]
fn a_legacy_request_still_declares_the_content_revision_of_the_named_root() {
    // A clean root at a commit, but no committed-input run: the sighting
    // decides and the evaluation declares the content revision, as before.
    let (state, parent, _store, named_root) = named_root_parent();
    let head = commit_named_root(&named_root);
    let transport = install_sighting_transport(&state, &named_root, 1, present_sighting());

    let response = request(&state, &parent)
        .unwrap_or_else(|error| panic!("a sighted root admits the request: {}", error.message));
    assert_eq!(sighting_reads(&transport), 1, "the legacy flow reads the sighting");
    let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } = response else {
        panic!("an independent evaluation spawns an evaluator");
    };
    let record = delegation.delegation;
    let target = record
        .acceptance_evaluation
        .clone()
        .expect("an evaluator carries its target");
    let expected = content_revision(&worktree(&named_root)).unwrap().1;
    assert!(expected.starts_with("content-v1:"), "{expected}");
    assert_eq!(target.source_fingerprint, Some(expected.clone()));
    assert_ne!(target.source_fingerprint, Some(format!("git-commit:{head}")));
    // Its first submission measures the content revision again, as before.
    let declared = state
        .acceptance_evaluation_declared_target(&record.child_session_id, &target)
        .unwrap_or_else(|error| panic!("an unchanged root keeps its revision: {}", error.message));
    assert_eq!(declared.source_fingerprint, Some(expected));
}

#[test]
fn a_record_engram_has_not_acknowledged_at_head_is_refused_without_a_sighting() {
    for case in [
        "no record sent",
        "pending",
        "rejected",
        "acknowledged for another commit",
    ] {
        let (state, parent, _store, named_root) = named_root_parent();
        let head = commit_named_root(&named_root);
        let earlier =
            run_git_test_command_output(&worktree(&named_root), &["rev-parse", "HEAD~1"]);
        let transport =
            install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));
        let registration = register_committed_run(
            &state,
            &parent,
            &named_root,
            committed_results("passed", input_record(&head, "clean")),
        );
        match case {
            "no record sent" => {}
            "pending" => write_record(
                &registration,
                &head,
                json!({"state": "pending", "reason": {"kind": "no_connection"}}),
            ),
            "rejected" => host_observed(
                &state,
                &registration,
                &head,
                json!({"state": "rejected", "detail": "refused by the producer"}),
            ),
            _ => host_observed(&state, &registration, &earlier, acknowledged("settled")),
        }

        let refusal = assert_refused_without_a_sighting(
            &transport,
            decide(&state, &parent),
            "verification_record_not_acknowledged",
            case,
        );
        assert!(
            matches!(
                &refusal,
                AcceptanceCommittedInputRefusal::RecordNotAcknowledged { run_id, commit, .. }
                    if run_id == COMMITTED_RUN_ID && *commit == head
            ),
            "{case}: {refusal:?}"
        );
    }
}

#[test]
fn a_committed_run_on_another_commit_is_refused_without_a_sighting() {
    let (state, parent, _store, named_root) = named_root_parent();
    let head = commit_named_root(&named_root);
    let earlier = run_git_test_command_output(&worktree(&named_root), &["rev-parse", "HEAD~1"]);
    let transport = install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));
    let registration = register_committed_run(
        &state,
        &parent,
        &named_root,
        committed_results("passed", input_record(&earlier, "clean")),
    );
    host_observed(&state, &registration, &earlier, acknowledged("settled"));

    let refused = assert_refused_without_a_sighting(
        &transport,
        decide(&state, &parent),
        "committed_input_no_eligible_run",
        "another commit",
    );
    assert_eq!(refused, AcceptanceCommittedInputRefusal::NoEligibleRun { commit: head });
    // The request itself is refused the same way, before any evaluator.
    let error = refusal(&state, request(&state, &parent), "another commit");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error.message.starts_with("committed_input_no_eligible_run"),
        "{}",
        error.message
    );
    assert_eq!(sighting_reads(&transport), 0, "the request read no sighting");
}

#[test]
fn a_dirty_root_is_refused_without_a_sighting() {
    for (case, dirty) in [
        ("untracked file", "debris.txt"),
        ("modified tracked file", "judged.txt"),
    ] {
        let (state, parent, _store, named_root) = named_root_parent();
        let head = commit_named_root(&named_root);
        let transport =
            install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));
        let registration = register_committed_run(
            &state,
            &parent,
            &named_root,
            committed_results("passed", input_record(&head, "clean")),
        );
        host_observed(&state, &registration, &head, acknowledged("settled"));
        fs::write(worktree(&named_root).join(dirty), "changed after the run\n").unwrap();

        let refusal = assert_refused_without_a_sighting(
            &transport,
            decide(&state, &parent),
            "committed_input_not_clean",
            case,
        );
        assert_eq!(refusal, AcceptanceCommittedInputRefusal::NotClean, "{case}");
    }
}

#[test]
fn a_debris_input_changed_failed_or_ineligible_run_is_refused_without_a_sighting()
{
    for case in [
        "debris pending disposition",
        "input changed",
        "failed run",
        "ineligible run",
    ] {
        let (state, parent, _store, named_root) = named_root_parent();
        let head = commit_named_root(&named_root);
        let transport =
            install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));
        let results = match case {
            "debris pending disposition" => {
                committed_results("passed", input_record(&head, "debris_pending_disposition"))
            }
            "input changed" => committed_results("passed", input_record(&head, "input_changed")),
            "failed run" => committed_results("failed", input_record(&head, "clean")),
            _ => {
                let mut input = input_record(&head, "clean");
                input["eligible"] = json!(false);
                committed_results("passed", input)
            }
        };
        let registration = register_committed_run(&state, &parent, &named_root, results);
        host_observed(&state, &registration, &head, acknowledged("settled"));

        let refusal = assert_refused_without_a_sighting(
            &transport,
            decide(&state, &parent),
            "committed_input_no_eligible_run",
            case,
        );
        assert_eq!(
            refusal,
            AcceptanceCommittedInputRefusal::NoEligibleRun { commit: head },
            "{case}"
        );
    }
}

#[test]
fn a_task_without_a_committed_run_is_in_the_legacy_flow() {
    for case in ["no run registered", "a run of another work"] {
        let (state, parent, _store, named_root) = named_root_parent();
        let head = commit_named_root(&named_root);
        let transport =
            install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));
        if case == "a run of another work" {
            let registration = register_committed_run(
                &state,
                &parent,
                &named_root,
                committed_results("passed", input_record(&head, "clean")),
            );
            host_observed(&state, &registration, &head, acknowledged("settled"));
            state
                .inner
                .lock()
                .unwrap()
                .test_run_registrations
                .update(COMMITTED_RUN_ID, |row| {
                    row.work_binding.as_mut().unwrap().work_id =
                        "01a0b6c5-4b4f-7b41-9e32-0000000000b2".to_owned();
                })
                .expect("the registration is there to rebind");
        }

        assert_eq!(
            decide(&state, &parent),
            Ok(AcceptanceCommittedInputDecision::Legacy),
            "{case}"
        );
        assert_eq!(sighting_reads(&transport), 0, "{case}: the decision reads no sighting");
        // The request runs the sighting preflight for the legacy flow.
        let response = request(&state, &parent);
        assert_fell_back_to_the_sighting(&state, &transport, response, case);
    }
}

#[test]
fn an_acknowledged_record_rewritten_with_its_results_to_another_commit_is_not_admitted() {
    let (state, parent, _store, named_root) = named_root_parent();
    let tested = commit_named_root(&named_root);
    let transport = install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));
    let registration = register_committed_run(
        &state,
        &parent,
        &named_root,
        committed_results("passed", input_record(&tested, "clean")),
    );
    // Engram genuinely acknowledged the record of the run on `tested`.
    host_observed(&state, &registration, &tested, acknowledged("received"));
    // The root moves on to an untested commit, and the run's results and its
    // frozen body are rewritten together to it: the body is the host's own
    // rebuild of the rewritten results, and the receipt still answers its
    // stages and outcome.
    let root = worktree(&named_root);
    run_git_test_command(&root, &["commit", "--allow-empty", "--quiet", "-m", "untested"]);
    let untested = run_git_test_command_output(&root, &["rev-parse", "HEAD"]);
    fs::write(
        PathBuf::from(&registration.run_directory).join("results.json"),
        committed_results("passed", input_record(&untested, "clean")).to_string(),
    )
    .unwrap();
    write_record(&registration, &untested, acknowledged("received"));
    let current = current_registration(&state);
    let frozen = engram_verification_record_frozen(FsPath::new(&current.run_directory), COMMITTED_RUN_ID)
        .expect("the rewritten body reads back");
    assert!(
        engram_verification_record_is_host_body(&current, &frozen.body),
        "the rewrite passes the host-rebuild check on its own"
    );

    let refused = assert_refused_without_a_sighting(
        &transport,
        decide(&state, &parent),
        "verification_record_not_acknowledged",
        "rewritten to another commit",
    );
    assert!(
        matches!(
            &refused,
            AcceptanceCommittedInputRefusal::RecordNotAcknowledged { run_id, commit, state }
                if run_id == COMMITTED_RUN_ID && *commit == untested
                    && state.starts_with("acknowledged for another body")
        ),
        "{refused:?}"
    );
    let error = refusal(&state, request(&state, &parent), "rewritten to another commit");
    assert!(
        error.message.starts_with("verification_record_not_acknowledged"),
        "{}",
        error.message
    );
    assert_eq!(sighting_reads(&transport), 0, "the request read no sighting");
}

/// Terminal results of a committed-input run that ended before any stage:
/// refused as not clean before it, or failed once HEAD moved while its input
/// was captured.
fn never_started_results(case: &str, commit: &str) -> Value {
    let (input, error) = match case {
        "refused dirty before the run" => (
            json!({
                "kind": "commit",
                "cleanBefore": false,
                "dirtyPaths": [{ "status": " M", "path": "judged.txt" }],
            }),
            "committed-input run refused: the checkout is not clean before the run; no stage ran",
        ),
        "HEAD moved while the input was captured" => (
            json!({ "kind": "commit", "commit": commit, "cleanBefore": true }),
            "committed-input run refused: HEAD moved while the input was captured; no stage ran",
        ),
        other => panic!("no such case {other}"),
    };
    json!({
        "runId": COMMITTED_RUN_ID,
        "state": "failed",
        "exitCode": 1,
        "ended": "2026-10-30T10:00:01.000Z",
        "stages": [{ "name": "rust-tests", "state": "unrun" }],
        "input": input,
        "error": error,
    })
}

#[test]
fn a_committed_input_run_that_never_started_leaves_the_task_in_the_legacy_flow() {
    for case in ["refused dirty before the run", "HEAD moved while the input was captured"] {
        let (state, parent, _store, named_root) = named_root_parent();
        let head = commit_named_root(&named_root);
        let transport =
            install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));
        register_committed_run(&state, &parent, &named_root, never_started_results(case, &head));

        assert_eq!(
            decide(&state, &parent),
            Ok(AcceptanceCommittedInputDecision::Legacy),
            "{case}"
        );
        // The request runs the sighting preflight, which reads the sighting.
        let response = request(&state, &parent);
        assert_fell_back_to_the_sighting(&state, &transport, response, case);
    }
}

#[test]
fn a_committed_input_run_whose_results_show_neither_a_start_nor_none_is_refused() {
    // A started committed-input run whose results were deleted, or whose
    // record no longer reads coherently, is not shown never to have started:
    // the request is refused, and the sighting Engram would accept is not read.
    for case in ["results deleted", "incoherent input record"] {
        let (state, parent, _store, named_root) = named_root_parent();
        let head = commit_named_root(&named_root);
        let transport = install_sighting_transport(&state, &named_root, 1, present_sighting());
        let registration = register_committed_run(
            &state,
            &parent,
            &named_root,
            committed_results("passed", input_record(&head, "clean")),
        );
        let results = PathBuf::from(&registration.run_directory).join("results.json");
        if case == "results deleted" {
            fs::remove_file(&results).unwrap();
        } else {
            // After the run on another commit, while its outcome says clean.
            let mut input = input_record(&head, "clean");
            input["after"]["commit"] = json!("a".repeat(40));
            assert_ne!(input["after"]["commit"], json!(head), "{case}");
            fs::write(&results, committed_results("passed", input).to_string()).unwrap();
        }

        let refused = assert_refused_without_a_sighting(
            &transport,
            decide(&state, &parent),
            "committed_input_scan_incomplete",
            case,
        );
        assert_eq!(refused, AcceptanceCommittedInputRefusal::ScanIncomplete, "{case}");
        let error = refusal(&state, request(&state, &parent), case);
        assert!(
            error.message.starts_with("committed_input_scan_incomplete"),
            "{case}: {}",
            error.message
        );
        assert_eq!(sighting_reads(&transport), 0, "{case}: the request read no sighting");
    }
}

#[test]
fn a_committed_input_run_that_started_on_a_clean_checkout_selects_the_committed_flow() {
    let (state, parent, _store, named_root) = named_root_parent();
    let head = commit_named_root(&named_root);
    let transport = install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));
    // It started cleanly and failed: no admission, but the committed flow
    // decides, and no sighting is read.
    register_committed_run(
        &state,
        &parent,
        &named_root,
        committed_results("failed", input_record(&head, "clean")),
    );
    let refused = assert_refused_without_a_sighting(
        &transport,
        decide(&state, &parent),
        "committed_input_no_eligible_run",
        "a started run",
    );
    assert_eq!(refused, AcceptanceCommittedInputRefusal::NoEligibleRun { commit: head });
}

#[test]
fn an_acknowledgment_only_in_the_run_directory_is_not_admitted_and_does_not_stop_the_send() {
    // The state file beside the run is the launcher's directory, which an
    // agent can write: a structurally valid receipt there (its operation its
    // producer, well-formed ids, the record's own stage and outcome), or a
    // settlement or rejection, that the host never observed acknowledges
    // nothing and neither holds nor finishes the record.
    for progress in [
        acknowledged("received"),
        acknowledged("settled"),
        json!({"state": "rejected", "detail": "written beside the run"}),
    ] {
        let case = progress["state"].as_str().unwrap().to_owned();
        let (state, parent, _store, named_root) = named_root_parent();
        let head = commit_named_root(&named_root);
        let transport =
            install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));
        let registration = register_committed_run(
            &state,
            &parent,
            &named_root,
            committed_results("passed", input_record(&head, "clean")),
        );
        write_record(&registration, &head, progress);
        let current = current_registration(&state);
        assert_eq!(current.verification_record, None, "{case}: the host observed nothing");

        let refusal = assert_refused_without_a_sighting(
            &transport,
            decide(&state, &parent),
            "verification_record_not_acknowledged",
            &case,
        );
        assert!(
            matches!(
                &refusal,
                AcceptanceCommittedInputRefusal::RecordNotAcknowledged { run_id, commit, .. }
                    if run_id == COMMITTED_RUN_ID && *commit == head
            ),
            "{case}: {refusal:?}"
        );
        // The sender neither settles from the file nor takes it as finished.
        let prepared = engram_verification_record_prepare(&current);
        assert!(
            !matches!(
                prepared,
                EngramVerificationRecordPrepared::Finished
                    | EngramVerificationRecordPrepared::ReceiptHeld { .. }
            ),
            "{case}: {prepared:?}"
        );
    }
}

#[test]
fn a_root_that_moves_while_the_run_is_read_is_refused_as_changed() {
    for case in ["a new commit", "a dirty worktree"] {
        let (state, parent, _store, named_root) = named_root_parent();
        let head = commit_named_root(&named_root);
        let transport =
            install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));
        let registration = register_committed_run(
            &state,
            &parent,
            &named_root,
            committed_results("passed", input_record(&head, "clean")),
        );
        host_observed(&state, &registration, &head, acknowledged("received"));
        // Between the reads of the run and its record and the root's second
        // read, the root moves on or is written to.
        let root = worktree(&named_root);
        let dirty = case == "a dirty worktree";
        TEST_ACCEPTANCE_COMMITTED_INPUT_BEFORE_RECHECK.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                if dirty {
                    fs::write(root.join("judged.txt"), "changed during the check\n").unwrap();
                } else {
                    run_git_test_command(&root, &["commit", "--allow-empty", "--quiet", "-m", "moved on"]);
                }
            }));
        });

        let refusal = assert_refused_without_a_sighting(
            &transport,
            decide(&state, &parent),
            "committed_input_changed",
            case,
        );
        assert_eq!(refusal, AcceptanceCommittedInputRefusal::Changed, "{case}");
    }
}

/// The results files the decision has read on this thread since the last call.
fn take_results_reads() -> usize {
    TEST_ACCEPTANCE_COMMITTED_INPUT_RESULTS_READS.with(|reads| reads.replace(0))
}

#[test]
fn more_matching_runs_than_the_bound_none_started_is_refused_as_an_incomplete_scan() {
    let (state, parent, _store, named_root) = named_root_parent();
    let head = commit_named_root(&named_root);
    let transport = install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));
    let registration = register_committed_run(
        &state,
        &parent,
        &named_root,
        never_started_results("refused dirty before the run", &head),
    );
    // More registrations of the claim than one request reads, none of which
    // started: the scan cannot tell whether an unread one did.
    {
        let mut inner = state.inner.lock().unwrap();
        for index in 0..ACCEPTANCE_COMMITTED_INPUT_CANDIDATES_MAX + 1 {
            let mut row = registration.clone();
            row.run_id = format!("{COMMITTED_RUN_ID}-{index}");
            row.run_directory = PathBuf::from(&registration.run_directory)
                .with_file_name(&row.run_id).to_string_lossy().into_owned();
            let mut results = never_started_results("refused dirty before the run", &head);
            results["runId"] = json!(row.run_id);
            fs::create_dir_all(&row.run_directory).unwrap();
            fs::write(PathBuf::from(&row.run_directory).join("results.json"), results.to_string()).unwrap();
            inner.test_runs.registered.insert(row.run_id.clone());
            inner.test_run_registrations.insert(row.run_id.clone(), row);
        }
    }
    take_results_reads();

    let refusal = assert_refused_without_a_sighting(
        &transport,
        decide(&state, &parent),
        "committed_input_scan_incomplete",
        "more runs than the bound",
    );
    assert_eq!(refusal, AcceptanceCommittedInputRefusal::ScanIncomplete);
    assert_eq!(
        take_results_reads(),
        ACCEPTANCE_COMMITTED_INPUT_CANDIDATES_MAX,
        "the scan reads at most the bound's results files"
    );
}

#[test]
fn a_request_deadline_already_past_is_refused_without_reading_a_run_or_the_sighting() {
    let (state, parent, _store, named_root) = named_root_parent();
    let head = commit_named_root(&named_root);
    let transport = install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));
    let registration = register_committed_run(
        &state,
        &parent,
        &named_root,
        committed_results("passed", input_record(&head, "clean")),
    );
    host_observed(&state, &registration, &head, acknowledged("settled"));
    take_results_reads();

    let refusal = assert_refused_without_a_sighting(
        &transport,
        decide_by(&state, &parent, std::time::Instant::now()),
        "committed_input_scan_incomplete",
        "deadline past",
    );
    assert_eq!(refusal, AcceptanceCommittedInputRefusal::ScanIncomplete);
    assert_eq!(take_results_reads(), 0, "no run's results are read past the deadline");
}

#[test]
fn a_received_record_settled_for_the_same_body_during_the_check_is_still_admitted() {
    let (state, parent, _store, named_root) = named_root_parent();
    let head = commit_named_root(&named_root);
    let transport = install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));
    let registration = register_committed_run(
        &state,
        &parent,
        &named_root,
        committed_results("passed", input_record(&head, "clean")),
    );
    host_observed(&state, &registration, &head, acknowledged("received"));
    // Between the reads and the re-check the host settles the very receipt
    // it held, for the same body.
    let settling = state.clone();
    TEST_ACCEPTANCE_COMMITTED_INPUT_BEFORE_RECHECK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            let current = current_registration(&settling);
            let Some(TestRunVerificationAck::Received { receipt, body_digest }) =
                current.verification_record.clone()
            else {
                panic!("the host holds a received receipt");
            };
            settling.note_engram_verification_record_ack(
                &current,
                TestRunVerificationAck::Settled { receipt, body_digest },
            );
            assert!(matches!(
                current_registration(&settling).verification_record,
                Some(TestRunVerificationAck::Settled { .. })
            ));
        }));
    });

    assert_eq!(
        decide(&state, &parent),
        Ok(AcceptanceCommittedInputDecision::Admitted(
            AcceptanceCommittedInputAdmission {
                run_id: COMMITTED_RUN_ID.to_owned(),
                commit: head,
            }
        ))
    );
    assert_eq!(sighting_reads(&transport), 0);
}

/// The fixture's independent-evaluation reads, with the requester's held read
/// listing the task's claim at `fence`.
fn reader_at_fence(
    fence: i64,
) -> impl Fn(&EngramConnectionConfig, &[String], Duration) -> std::result::Result<Value, EngramTransportError>
{
    let reads = independent_reader();
    move |connection, args, timeout| {
        if args.iter().any(|arg| arg == "held") {
            Ok(json!({"items":[{"work_id":SIGHTED_WORK_ID,"short_ref":"w-task",
                "claim_id":"claim-task","claim_fence":fence}],"omitted":0}))
        } else {
            reads(connection, args, timeout)
        }
    }
}

#[test]
fn a_committed_run_survives_recovery_only_with_the_same_work_run_binding() {
    let (state, parent, _store, named_root) = named_root_parent();
    let head = commit_named_root(&named_root);
    let transport = install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));
    let deadline = || std::time::Instant::now() + std::time::Duration::from_secs(60);
    let naming_fence = state.inner.lock().unwrap().engram_work_source_roots[0].claim_fence;
    assert_eq!(naming_fence, 1, "the root is named at fence F");

    // (a) A run registered at F survives a live renewal by the same holder,
    // which moves the claim's revision but keeps its fence: once Engram
    // acknowledges its record, it is admitted.
    let at_naming = register_committed_run(
        &state,
        &parent,
        &named_root,
        committed_results("passed", input_record(&head, "clean")),
    );
    let mut renewed = at_naming.work_binding.clone().expect("the run is bound");
    assert_eq!(renewed.claim_fence, naming_fence, "the run is registered at F");
    renewed.work_revision += 1;
    let live = current_claim(naming_fence, Some(renewed.clone()));
    assert_refused_without_a_sighting(
        &transport,
        decide_task(&state, &parent, &task_under(live.clone()), deadline()),
        "verification_record_not_acknowledged",
        "renewed, before the acknowledgement",
    );
    host_observed(&state, &at_naming, &head, acknowledged("settled"));
    assert_eq!(
        decide_task(&state, &parent, &task_under(live), deadline()),
        Ok(AcceptanceCommittedInputDecision::Admitted(
            AcceptanceCommittedInputAdmission {
                run_id: COMMITTED_RUN_ID.to_owned(),
                commit: head.clone(),
            }
        )),
        "a live renewal at F keeps the run admissible"
    );

    // (b) The claim lapses and is recovered under the same claim id at F+1,
    // and the root is not named again. Its existing record survives when
    // the current binding still proves the same work run. Registering a new
    // run still requires a name under the recovered claim's current fence.
    let mut same_holder = renewed.clone();
    same_holder.claim_fence = naming_fence + 1;
    let another_holder = EngramControlWorkBinding {
        run_id: "run-recovered".to_owned(),
        root_execution_id: "root-recovered".to_owned(),
        ..same_holder.clone()
    };
    let recovered = [
        current_claim(naming_fence + 1, Some(same_holder.clone())),
        current_claim(naming_fence + 1, Some(another_holder)),
        current_claim(naming_fence + 1, None),
    ];
    let after_recovery_id = "test-committed-input-after-recovery";
    let mut after_recovery_results = committed_results("passed", input_record(&head, "clean"));
    after_recovery_results["runId"] = json!(after_recovery_id);
    transport.replace_held_claims([Ok(EngramHeldClaims {
        items: vec![EngramHeldClaim {
            work_id: same_holder.work_id.clone(), short_ref: "w-task".to_owned(),
            claim_id: same_holder.claim_id.clone(), claim_fence: same_holder.claim_fence,
            focused: false, control_binding: Some(same_holder.clone()),
        }], omitted: 0,
    })]);
    let error = try_register_committed_run_as(
        &state,
        &parent,
        &named_root,
        after_recovery_id,
        after_recovery_results,
    ).unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("name the source root again"), "{}", error.message);
    assert!(!state.inner.lock().unwrap().test_run_registrations.contains_key(after_recovery_id));
    assert_eq!(
        decide_task(&state, &parent, &task_under(recovered[0].clone()), deadline()),
        Ok(AcceptanceCommittedInputDecision::Admitted(
            AcceptanceCommittedInputAdmission {
                run_id: COMMITTED_RUN_ID.to_owned(), commit: head.clone(),
            }
        )),
        "the same work run keeps its original record after claim recovery"
    );
    for claim in &recovered[1..] {
        assert_eq!(
            decide_task(&state, &parent, &task_under(claim.clone()), deadline()),
            Ok(AcceptanceCommittedInputDecision::Legacy),
            "a different or unproven work run cannot inherit the record: {claim:?}"
        );
    }
    assert_eq!(sighting_reads(&transport), 0, "the decision reads no sighting");
    // This reader omits the live binding: the new fence alone cannot prove
    // the original work run, so the legacy sighting still decides this case.
    let response = state.request_acceptance_evaluation_with_runner(
        &parent,
        evaluation_request(Some(Agent::Codex)),
        reader_at_fence(naming_fence + 1),
    );
    assert_fell_back_to_the_sighting(&state, &transport, response, "recovered without a new name");

    // (c) The root is named again in the recovered epoch, at F+1, and a run
    // registered against that name is admitted; the runs at F stay as they
    // were registered and are not relabelled.
    {
        let mut inner = state.inner.lock().unwrap();
        inner.engram_work_source_roots[0].claim_fence = naming_fence + 1;
        for history in inner.engram_work_naming_history.iter_mut() {
            for proof in &mut history.proofs { proof.binding = same_holder.clone(); }
        }
    }
    assert_eq!(
        decide_task(&state, &parent, &task_under(recovered[0].clone()), deadline()),
        Ok(AcceptanceCommittedInputDecision::Legacy),
        "the runs at F are not candidates under the F+1 name"
    );
    let renamed_id = "test-committed-input-after-renaming";
    let mut renamed_results = committed_results("passed", input_record(&head, "clean"));
    renamed_results["runId"] = json!(renamed_id);
    register_committed_run_as(&state, &parent, &named_root, renamed_id, renamed_results);
    let renamed = state.inner.lock().unwrap().test_run_registrations[renamed_id].clone();
    host_observed(&state, &renamed, &head, acknowledged("settled"));
    assert_eq!(
        decide_task(&state, &parent, &task_under(recovered[0].clone()), deadline()),
        Ok(AcceptanceCommittedInputDecision::Admitted(
            AcceptanceCommittedInputAdmission {
                run_id: renamed_id.to_owned(),
                commit: head,
            }
        )),
        "a run at F+1 under the F+1 name is admitted"
    );
    assert_eq!(
        state.inner.lock().unwrap().test_run_registrations[COMMITTED_RUN_ID]
            .work_binding.as_ref().map(|binding| binding.claim_fence),
        Some(naming_fence),
        "the original run keeps the fence it was registered at"
    );
}

#[test]
fn an_automatic_clean_start_keeps_the_committed_flow_after_dirty_work_and_a_plain_run() {
    for code in [0, 7] {
        let (state, parent, _store, named_root) = named_root_parent();
        commit_named_root(&named_root);
        let root = worktree(&named_root);
        let transport =
            install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));
        assert_eq!(decide(&state, &parent), Ok(AcceptanceCommittedInputDecision::Legacy));
        // Use real automatic selection and execution, including a failed RED
        // baseline. The second run explicitly selects plain after the edit.
        let source = r#"
const {pathToFileURL}=await import('node:url');
const {readFileSync,writeFileSync}=await import('node:fs');
const {join}=await import('node:path');
const source=process.argv[2], root=process.argv[3], owner=process.argv[4], code=Number(process.argv[5]);
const {createRun,executeRun}=await import(pathToFileURL(join(source,'scripts/test-launcher.mjs')));
const {isolatedGitEnvironment}=await import(pathToFileURL(join(source,'scripts/review-freeze-fingerprint.mjs')));
const {withHostedRegistrationFixture}=await import(pathToFileURL(join(source,'scripts/test-launcher-registration-fixtures.mjs')));
const env=isolatedGitEnvironment({...process.env,TERMAL_BASE_URL:'http://127.0.0.1:0',TERMAL_SESSION_ID:owner,TERMAL_CLI:process.execPath});
await withHostedRegistrationFixture(root,env,async hosted=>{
  const automatic=await createRun({root,automaticInput:true,stages:[{name:'baseline',command:process.execPath,args:['-e',`process.exit(${code})`]}]},hosted);
  const first=await executeRun(automatic,hosted);
  writeFileSync(join(root,'judged.txt'),'uncommitted fix after baseline\n');
  const plain=await createRun({root,input:'plain',stages:[{name:'plain',command:process.execPath,args:['-e','']}]},hosted);
  const second=await executeRun(plain,hosted);
  const request=directory=>JSON.parse(readFileSync(join(directory,'request.json'),'utf8'));
  console.log('LAUNCHER_RESULT='+JSON.stringify({automatic,plain,first,second,automaticRequest:request(automatic),plainRequest:request(plain)}));
});
"#;
        let output = Command::new("node")
            .args(["--input-type=module", "-e", source, "fixture"])
            .arg(env!("CARGO_MANIFEST_DIR"))
            .arg(&root)
            .arg(&parent)
            .arg(code.to_string())
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let stdout = String::from_utf8(output.stdout).unwrap();
        let frame: Value = serde_json::from_str(
            stdout.lines().find_map(|line| line.strip_prefix("LAUNCHER_RESULT=")).unwrap(),
        )
        .unwrap();
        assert_eq!(frame["automaticRequest"]["input"], "commit");
        assert_eq!(frame["first"]["exitCode"], code);
        assert_eq!(frame["first"]["input"]["cleanBefore"], true);
        assert!(frame["first"]["stages"][0]["started"].is_string());
        assert_eq!(frame["first"]["state"], if code == 0 { "passed" } else { "failed" });
        assert!(frame["plainRequest"].get("input").is_none());
        assert_eq!(frame["second"]["state"], "passed");
        assert!(frame["second"].get("input").is_none());
        // Keep the real launcher's immutable native pin, but script the Rust
        // metadata subprocess boundary as the registration unit tests do. This
        // test observes run/claim behavior, not the installed source CLI version.
        {
            let mut inner = state.inner.lock().unwrap();
            let project_id = engram_project_for_session_locked(&inner, &parent).unwrap().id.clone();
            inner.projects.iter_mut().find(|project| project.id == project_id).unwrap()
                .engram.as_mut().unwrap().binary_path = Some(
                    frame["automaticRequest"]["sourceOwner"]["binaryPath"].as_str().unwrap().to_owned(),
                );
            let version = frame["automaticRequest"]["sourceOwner"]["binaryVersion"]
                .as_str().unwrap().to_owned();
            inner.test_native_source_owner_metadata = Some(Arc::new(move |state, session, deadline| {
                state.source_owner_metadata_snapshot_with(session, deadline, |_, _, args, _, _| {
                    let mut reply = crate::tests::engram_source_owner::source_owner_reply(args);
                    if args == ["--version"] {
                        reply.stdout = format!("{version}\n").into_bytes();
                    }
                    Ok(reply)
                })
            }));
        }
        // Register the actual immutable requests and results through the host
        // seam, bound to the same claim, fence and naming generation.
        for (directory, request) in [("automatic", "automaticRequest"), ("plain", "plainRequest")] {
            let run_id = frame[request]["runId"].as_str().unwrap();
            state
                .register_test_run(
                    &parent,
                    RegisterTestRunRequest {
                        run_id: run_id.to_owned(),
                        run_directory: frame[directory].as_str().unwrap().to_owned(),
                    },
                )
                .unwrap_or_else(|error| panic!("{directory}: {}", error.message));
            let inner = state.inner.lock().unwrap();
            let registration = &inner.test_run_registrations[run_id];
            assert!(inner.test_runs.registered.contains(run_id));
            assert_eq!(registration.work_binding.as_ref().unwrap().work_id, SIGHTED_WORK_ID);
        }
        assert_eq!(
            assert_refused_without_a_sighting(
                &transport,
                decide(&state, &parent),
                "committed_input_not_clean",
                "automatic start, dirty edit, later plain run",
            ),
            AcceptanceCommittedInputRefusal::NotClean,
        );
        let error = refusal(&state, request(&state, &parent), "dirty after automatic start");
        assert!(error.message.starts_with("committed_input_not_clean"));
        assert!(error.message.contains("commit or remove its changes, run the gate on that commit"));
        assert_eq!(sighting_reads(&transport), 0);
    }
}
