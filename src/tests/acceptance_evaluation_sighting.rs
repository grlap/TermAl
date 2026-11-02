// Tests of the initial-sighting preflight of an acceptance evaluation: before
// TermAl spawns an evaluator or hands a same-session brief for a task whose
// claim names a source root, it reads Engram's named-root sighting for the
// task's run at the task's evidence basis, and refuses up front when the root
// has none there. Does not own the rest of the acceptance-evaluation tests,
// whose fixtures and helpers it uses from its parent module. A child module of
// src/tests/acceptance_evaluation.rs, as the source-fingerprint tests are, so
// that file stays within the size the architecture lens allows. These source
// fixtures do not enable evaluation in the running host.
use super::*;

#[path = "acceptance_evaluation_committed_input.rs"]
mod committed_input;

const SIGHTED_WORK_ID: &str = "01a0b6c5-4b4f-7b41-9e32-edc597077acf";

/// A parent session bound to the claim that named a worktree as the work's
/// source root, with that root's authority published: the setup of
/// `acceptance_request_on_a_named_source_root_runs_and_measures_the_evaluator_there`.
/// Returns the state, the parent session, the store and the named root.
fn named_root_parent() -> (AppState, String, EngramAuthorityStoreKey, String) {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    source::make_workdir_a_worktree(&root);
    fs::write(
        root.join(".gitignore"),
        ".engram-project\nprojects/\nwork-read-args.txt\n.worktrees/\n",
    )
    .unwrap();
    run_git_test_command(&root, &["add", ".gitignore"]);
    run_git_test_command(&root, &["commit", "--quiet", "-m", "ignore worktrees"]);
    let worktree = root.join(".worktrees").join("wt");
    run_git_test_command(
        &root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "wt",
            worktree.to_str().unwrap(),
        ],
    );
    fs::write(worktree.join("judged.txt"), "the work's own tree\n").unwrap();
    let store = established_store(&state, &project).expect("the store is established");
    let (named_root, common_dir_key) = validate_engram_source_root(
        &worktree.to_string_lossy(),
        &root.to_string_lossy(),
        &root.to_string_lossy(),
    )
    .expect("the worktree may be named");
    let binding = EngramControlWorkBinding {
        root_execution_id: "root".to_owned(),
        work_id: SIGHTED_WORK_ID.to_owned(),
        run_id: "run".to_owned(),
        work_revision: 1,
        claim_id: "claim-task".to_owned(),
        claim_fence: 1,
    };
    {
        let mut inner = state.inner.lock().unwrap();
        *inner.engram_work_source_roots = vec![EngramWorkSourceRoot {
            store: store.clone(),
            work_id: SIGHTED_WORK_ID.to_owned(),
            short_ref: "w-task".to_owned(),
            claim_id: "claim-task".to_owned(),
            claim_fence: 1,
            root: named_root.clone(),
            common_dir_key,
            named_by_session: parent.clone(),
            named_at: "2026-09-27T00:00:00.000Z".to_owned(),
            generation: 1,
        }];
        let index = inner.find_session_index(&parent).unwrap();
        inner.sessions[index].engram.named_root = Some(EngramNamedRootState::Bound {
            workspace_id: named_root.clone(),
            generation: 1,
            named_at: "2026-09-27T00:00:00.000Z".to_owned(),
        });
        inner.sessions[index].engram.work_binding = Some(binding.clone());
    }
    let owner = state
        .prepare_engram_authority(&store, &binding, Duration::from_secs(2))
        .unwrap();
    let proof: EngramNamedRootReadResponse = serde_json::from_value(json!({
        "project_id":store.project_id,"work_id":binding.work_id,"run_id":binding.run_id,
        "claim_id":binding.claim_id,"root_execution_id":binding.root_execution_id,
        "run":{"state":"open","generation":2},
        "named_root":{"state":"bound","workspace_id":named_root,"generation":1,"named_at":"2026-09-27T00:00:00Z"},
        "latest_event":{"event":"fixture-named-event","position":{"feed":{"kind":"run_execution","id":binding.run_id},"position":1},
            "kind":"bound","workspace_id":named_root,"generation":1,"named_at":"2026-09-27T00:00:00Z"},
        "read_cut":{"feed":{"kind":"run_execution","id":binding.run_id},"position":1}
    }))
    .unwrap();
    AppState::learn_engram_authority_locked(
        &mut state.inner.lock().unwrap(),
        &store,
        &owner,
        &proof,
    )
    .unwrap();
    state
        .publish_engram_authority(&store, &owner, Duration::from_secs(2))
        .unwrap();
    super::delegation_support::install_delegation_codex_runtime(
        &state,
        "acceptance-evaluation-runtime",
    );
    (state, parent, store, named_root)
}

/// The task's active run, as the fixture's canonical core receipt names it.
const SIGHTED_RUN_ID: &str = "01a0b6c5-4b4f-7b41-9e32-edd7dea8b369";

/// A present sighting record at the cut.
pub(super) fn present_sighting() -> Value {
    json!({"state": "present", "record": "fixture-sighting", "position": 2, "revision": "content-v1:fixture"})
}

/// Engram's answer to the sighting read at the task's evidence basis (42):
/// `named_root` bound at `generation`, with `sighting` there.
fn sighting_reply(named_root: &str, generation: i64, sighting: Value) -> Value {
    json!({
        "schema_version": 1,
        "project_id": "established-project",
        "work_id": SIGHTED_WORK_ID,
        "run_id": SIGHTED_RUN_ID,
        "read_cut": 42,
        "head_cut": 42,
        "current_binding": "fixture-named-event",
        "binding_changed": false,
        "root": {
            "state": "bound",
            "workspace_id": named_root,
            "generation": generation,
            "binding_event": "fixture-named-event",
            "binding_position": 1,
            "sighting": sighting,
        }
    })
}

/// Installs a control transport whose one scripted answer is the sighting
/// read's: `named_root` bound at `generation`, with `sighting` at the cut.
pub(super) fn install_sighting_transport(
    state: &AppState,
    named_root: &str,
    generation: i64,
    sighting: Value,
) -> Arc<ScriptedEngramControlTransport> {
    install_sighting_answer(state, Ok(sighting_reply(named_root, generation, sighting)))
}

/// Installs a control transport whose one scripted answer is `answer`.
fn install_sighting_answer(
    state: &AppState,
    answer: std::result::Result<Value, EngramTransportError>,
) -> Arc<ScriptedEngramControlTransport> {
    let transport = ScriptedEngramControlTransport::new([]);
    transport.script_sighting_reads([answer]);
    state.install_test_engram_transport(transport.clone());
    transport
}

/// The task's reads for an evaluation the policy admits only in `mode`.
fn reader_admitting(
    mode: &'static str,
) -> impl Fn(
    &EngramConnectionConfig,
    &[String],
    Duration,
) -> std::result::Result<Value, EngramTransportError> {
    move |connection, args, timeout| {
        if args.iter().any(|arg| arg == "held") {
            Ok(
                json!({"items":[{"work_id":SIGHTED_WORK_ID,"short_ref":"w-task","claim_id":"claim-task","claim_fence":1}],"omitted":0}),
            )
        } else {
            fixture_reader(
                Arc::default(),
                show_receipt(None),
                Ok(policy_receipt(Some(&[mode]))),
            )(connection, args, timeout)
        }
    }
}

/// The refusal a request got, with the assurance that it created neither an
/// evaluator nor a brief.
fn refusal(
    state: &AppState,
    response: Result<AcceptanceEvaluationRequestResponse, ApiError>,
    case: &str,
) -> ApiError {
    assert!(
        state.inner.lock().unwrap().delegations.is_empty(),
        "{case}: no evaluator may be spawned"
    );
    match response {
        Err(error) => error,
        Ok(_) => panic!("{case}: the request must be refused before any evaluator or brief"),
    }
}

/// The task's reads for an independent evaluation of `w-task` by the claim
/// that named the root.
fn independent_reader() -> impl Fn(
    &EngramConnectionConfig,
    &[String],
    Duration,
) -> std::result::Result<Value, EngramTransportError> {
    |connection, args, timeout| {
        if args.iter().any(|arg| arg == "held") {
            Ok(
                json!({"items":[{"work_id":SIGHTED_WORK_ID,"short_ref":"w-task","claim_id":"claim-task","claim_fence":1}],"omitted":0}),
            )
        } else {
            fixture_reader(
                Arc::default(),
                show_receipt(None),
                Ok(policy_receipt(Some(&["independent_session"]))),
            )(connection, args, timeout)
        }
    }
}

#[test]
fn acceptance_request_refuses_a_named_root_without_a_sighting_before_it_spawns() {
    // The root is bound but Engram has no sighting of it at the task's
    // evidence basis: an evaluator spawned now would have its submission
    // refused, so the request is refused before any evaluator exists.
    let (state, parent, _store, named_root) = named_root_parent();
    let transport = install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));

    let refused = state.request_acceptance_evaluation_with_runner(
        &parent,
        evaluation_request(Some(Agent::Codex)),
        independent_reader(),
    );

    assert!(
        state.inner.lock().unwrap().delegations.is_empty(),
        "no evaluator may be spawned for a root Engram has not sighted"
    );
    let error = match refused {
        Err(error) => error,
        Ok(_) => panic!("a named root without a sighting is refused before any spawn"),
    };
    assert!(
        error.message.contains("has no sighting")
            && error.message.contains("cut 42")
            && error.message.contains("acknowledged capture"),
        "the refusal names the missing sighting, the cut and the remedy: {}",
        error.message
    );
    let reads: Vec<Value> = transport
        .requests()
        .into_iter()
        .map(|recorded| recorded.request)
        .filter(|request| request["operation"] == "named_root_sighting_read")
        .collect();
    assert_eq!(reads.len(), 1, "one sighting read: {reads:?}");
    assert_eq!(reads[0]["work_ref"], SIGHTED_WORK_ID);
    assert_eq!(reads[0]["run_id"], SIGHTED_RUN_ID);
    assert_eq!(
        reads[0]["run_cut"], 42,
        "the read is taken at the task's evidence basis"
    );
}

/// A parent that names no source root: the evaluation reads its workdir.
fn unnamed_root_parent() -> (AppState, String, Arc<ScriptedEngramControlTransport>) {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    super::delegation_support::install_delegation_codex_runtime(
        &state,
        "acceptance-evaluation-runtime",
    );
    let transport = ScriptedEngramControlTransport::new([]);
    state.install_test_engram_transport(transport.clone());
    (state, parent, transport)
}

#[test]
fn acceptance_request_without_a_named_root_reads_the_sighting_and_proceeds_when_none_is_bound() {
    // Neither side binds a root: the read is still made, and the request
    // goes on exactly as before.
    let (state, parent, transport) = unnamed_root_parent();

    let response = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(Some(Agent::Codex)),
            independent_reader(),
        )
        .expect("no root on either side admits the request");

    assert!(matches!(
        response,
        AcceptanceEvaluationRequestResponse::Spawned { .. }
    ));
    let reads: Vec<Value> = transport
        .requests()
        .into_iter()
        .map(|recorded| recorded.request)
        .filter(|request| request["operation"] == "named_root_sighting_read")
        .collect();
    assert_eq!(reads.len(), 1, "{reads:?}");
    assert_eq!(reads[0]["run_cut"], 42);
}

#[test]
fn acceptance_request_without_a_named_root_is_refused_when_engram_binds_one() {
    // TermAl would evaluate the workdir while Engram measures the work in a
    // named root: a conflict, never an evaluation of the wrong tree.
    let (state, parent, transport) = unnamed_root_parent();
    transport.script_sighting_reads([Ok(sighting_reply(
        "C:/elsewhere/.worktrees/wt",
        1,
        present_sighting(),
    ))]);

    let response = state.request_acceptance_evaluation_with_runner(
        &parent,
        evaluation_request(Some(Agent::Codex)),
        independent_reader(),
    );

    let error = refusal(&state, response, "local none, producer bound");
    assert!(
        error
            .message
            .contains("but TermAl has no named source root"),
        "{}",
        error.message
    );
}

#[test]
fn acceptance_same_session_request_is_refused_for_an_unsighted_root_before_its_brief() {
    // The read precedes the mode split, so a same-session evaluation gets no
    // brief for a root Engram has not sighted either.
    let (state, parent, _store, named_root) = named_root_parent();
    let transport = install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));

    let response = state.request_acceptance_evaluation_with_runner(
        &parent,
        evaluation_request(None),
        reader_admitting("same_session"),
    );

    let error = refusal(&state, response, "same_session");
    assert!(
        error.message.contains("has no sighting"),
        "{}",
        error.message
    );
    assert_eq!(
        transport
            .requests()
            .iter()
            .filter(|recorded| recorded.request["operation"] == "named_root_sighting_read")
            .count(),
        1
    );
}

#[test]
fn acceptance_request_proceeds_on_engrams_sighting_without_any_local_record_of_it() {
    // TermAl keeps no record of sightings: one made by another session on the
    // same run, or before this host restarted, admits the request on
    // Engram's answer alone. This parent never checkpointed a turn here.
    let (state, parent, _store, named_root) = named_root_parent();
    install_sighting_transport(&state, &named_root, 1, present_sighting());

    let response = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(Some(Agent::Codex)),
            independent_reader(),
        )
        .expect("a sighted root admits the request");

    assert!(matches!(
        response,
        AcceptanceEvaluationRequestResponse::Spawned { .. }
    ));
}

#[test]
fn acceptance_request_refuses_a_moved_or_different_binding_as_a_conflict() {
    for (case, answer, wording) in [
        (
            "binding moved after the cut",
            {
                let mut answer = sighting_reply("ROOT", 1, present_sighting());
                answer["binding_changed"] = json!(true);
                answer
            },
            "moved after",
        ),
        (
            "no root bound",
            {
                let mut answer = sighting_reply("ROOT", 1, present_sighting());
                answer["root"] = json!({"state":"none"});
                answer
            },
            "binds no source root",
        ),
        (
            "another workspace",
            sighting_reply("C:/elsewhere/wt", 1, present_sighting()),
            "another source root or generation",
        ),
        (
            "another generation",
            sighting_reply("ROOT", 2, present_sighting()),
            "another source root or generation",
        ),
    ] {
        let (state, parent, _store, named_root) = named_root_parent();
        let answer: Value = serde_json::from_str(
            &answer
                .to_string()
                .replace("\"ROOT\"", &json!(named_root).to_string()),
        )
        .unwrap();
        install_sighting_answer(&state, Ok(answer));

        let response = state.request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(Some(Agent::Codex)),
            independent_reader(),
        );

        let error = refusal(&state, response, case);
        assert!(error.message.contains(wording), "{case}: {}", error.message);
        assert!(
            !error.message.contains("has no sighting"),
            "{case}: {}",
            error.message
        );
    }
}

#[test]
fn acceptance_request_treats_an_unreadable_sighting_as_unknown_never_as_absent() {
    let unsupported = Err(EngramTransportError::remote(EngramControlErrorBody {
        code: "unknown_operation".to_owned(),
        message: "named_root_sighting_read is not an operation of this Engram".to_owned(),
    }));
    let mut newer_schema = sighting_reply("ROOT", 1, json!({"state":"absent"}));
    newer_schema["schema_version"] = json!(2);
    let mut other_cut = sighting_reply("ROOT", 1, json!({"state":"absent"}));
    other_cut["read_cut"] = json!(41);
    let mut unknown_field = sighting_reply("ROOT", 1, json!({"state":"absent"}));
    unknown_field["root"]["sighting"]["note"] = json!("an unknown shape");
    let mut no_root = sighting_reply("ROOT", 1, json!({"state":"absent"}));
    no_root.as_object_mut().unwrap().remove("root");
    let mut other_project = sighting_reply("ROOT", 1, present_sighting());
    other_project["project_id"] = json!("another-store-project");
    let mut other_work = sighting_reply("ROOT", 1, present_sighting());
    other_work["work_id"] = json!("01a0b6c5-4b4f-7b41-9e32-0000000000b2");
    let mut other_run = sighting_reply("ROOT", 1, present_sighting());
    other_run["run_id"] = json!("01a0b6c5-4b4f-7b41-9e32-0000000000b3");
    for (case, answer) in [
        ("unsupported operation", unsupported),
        ("newer schema", Ok(newer_schema)),
        ("another cut", Ok(other_cut)),
        ("unknown field", Ok(unknown_field)),
        ("missing root", Ok(no_root)),
        ("another project", Ok(other_project)),
        ("another work", Ok(other_work)),
        ("another run", Ok(other_run)),
    ] {
        let (state, parent, _store, named_root) = named_root_parent();
        let answer = answer.map(|answer| {
            serde_json::from_str(
                &answer
                    .to_string()
                    .replace("\"ROOT\"", &json!(named_root).to_string()),
            )
            .unwrap()
        });
        install_sighting_answer(&state, answer);

        let response = state.request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(Some(Agent::Codex)),
            independent_reader(),
        );

        let error = refusal(&state, response, case);
        assert!(
            error.message.contains("could not be determined"),
            "{case}: {}",
            error.message
        );
        assert!(
            !error.message.contains("has no sighting"),
            "{case}: {}",
            error.message
        );
    }
}

/// Answers the sighting read with a scripted sighting after renaming the
/// local root's generation, as a rename racing the read would. It holds the
/// state weakly: it is installed in that state, so a strong reference would
/// keep the state, and its temp directory, alive past the test.
struct RenamingSightingTransport {
    inner: std::sync::Weak<StateMutex<StateInner>>,
    named_root: String,
    sighting: Value,
}

impl EngramControlTransport for RenamingSightingTransport {
    fn request(
        &self,
        _connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        _timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        assert!(matches!(
            request,
            EngramControlRequest::NamedRootSightingRead { .. }
        ));
        let inner = self
            .inner
            .upgrade()
            .expect("the state outlives its request");
        inner.lock().unwrap().engram_work_source_roots[0].generation = 2;
        Ok(sighting_reply(&self.named_root, 1, self.sighting.clone()))
    }

    fn shutdown_session(&self, _session_id: &str) {}
}

#[test]
fn acceptance_request_refuses_when_the_local_root_is_renamed_during_the_read() {
    for present in [true, false] {
        let (state, parent, _store, named_root) = named_root_parent();
        state.install_test_engram_transport(Arc::new(RenamingSightingTransport {
            inner: Arc::downgrade(&state.inner),
            named_root,
            sighting: if present { present_sighting() } else { json!({"state":"absent"}) },
        }));

        let response = state.request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(Some(Agent::Codex)),
            independent_reader(),
        );

        let error = refusal(&state, response, "renamed during the read");
        assert!(
            error.message.contains("changed while its sighting was read"),
            "{}",
            error.message
        );
        assert!(!error.message.contains("Then request a new evaluation from a later turn"),
            "{}", error.message);
    }
}

fn sighting_reads(transport: &ScriptedEngramControlTransport) -> usize {
    transport.requests().iter()
        .filter(|recorded| recorded.request["operation"] == "named_root_sighting_read")
        .count()
}

// Trusted metadata fixtures for the ordinary source preflight. They model
// an observed turn and its opening root; they neither run a provider nor
// produce a source measurement, verification or Engram sighting receipt.
fn ordinary_naming_trace(
    state: &AppState,
    parent: &str,
    opened_in_root: bool,
) -> EngramRuntimeObservedTurn {
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(parent).unwrap()];
    let mut trace = EngramRuntimeObservedTurn::started(
        "fixture-ordinary-prompt".to_owned(), record.active_turn_generation,
    );
    if opened_in_root {
        let entry = &inner.engram_work_source_roots[0];
        trace.opening_root = Some(EngramRuntimeOpeningRoot {
            store: entry.store.clone(),
            binding: record.engram.work_binding.clone().unwrap(),
            provenance: EngramRootCapture::Unconfirmed,
            source_root: Some(EngramTurnSourceRoot::from_entry(entry)),
        });
    }
    trace
}

fn ordinary_naming_event(
    root: &EngramWorkSourceRoot,
    reporter: &str,
    kind: EngramNamedRootKind,
) -> EngramNamedRootEvent {
    EngramNamedRootEvent {
        root: root.clone(), reporter: reporter.to_owned(), kind, end_reason: None,
    }
}

fn install_ordinary_naming_turn(
    state: &AppState,
    parent: &str,
    trace: EngramRuntimeObservedTurn,
) {
    let mut inner = state.inner.lock().unwrap();
    let entry = inner.engram_work_source_roots[0].clone();
    let index = inner.find_session_index(parent).unwrap();
    let engram = &mut inner.sessions[index].engram;
    engram.unadmitted_active_turn = Some(trace);
    // The public naming path calls this same owner with the current observed
    // turn identity when it installs the selection, under the same state lock.
    let naming_turn = engram.observed_turn_ref().map(str::to_owned);
    record_engram_root_named_in_turn(
        engram, &entry.store,
        &ordinary_naming_event(&entry, parent, EngramNamedRootKind::Bound),
        naming_turn,
    );
}

#[test]
fn acceptance_request_in_the_turn_that_named_the_root_is_refused_before_the_read() {
    let (state, parent, _store, named_root) = named_root_parent();
    let transport = install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));
    let trace = ordinary_naming_trace(&state, &parent, false);
    install_ordinary_naming_turn(&state, &parent, trace);
    let response = state.request_acceptance_evaluation_with_runner(
        &parent, evaluation_request(Some(Agent::Codex)), independent_reader(),
    );
    let error = refusal(&state, response, "ordinary naming turn before the root opens");
    assert_eq!(error.kind, Some(ApiErrorKind::AcceptanceEvaluationNamedRootTurnPending));
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("no turn has opened in it since")
        && error.message.contains("(named_root_turn_pending)")
        && error.message.contains("not from the turn the root was named in"), "{}", error.message);
    assert_eq!(sighting_reads(&transport), 0, "the local ordinary boundary precedes the producer read");
}

#[test]
fn acceptance_request_after_the_next_turn_opens_in_the_named_root_proceeds_past_the_check() {
    for same_turn_opening in [false, true] {
        let (state, parent, _store, named_root) = named_root_parent();
        let transport = install_sighting_transport(&state, &named_root, 1, present_sighting());
        let naming_trace = ordinary_naming_trace(&state, &parent, false);
        let naming_turn = naming_trace.host_turn_ref.clone();
        install_ordinary_naming_turn(&state, &parent, naming_trace);
        let mut opening_trace = ordinary_naming_trace(&state, &parent, true);
        if same_turn_opening {
            // A root capture can follow selection installation within the
            // same turn. Its actual opening, rather than a clock, decides.
            opening_trace.host_turn_ref = naming_turn;
        } else {
            opening_trace.generation = opening_trace.generation.saturating_add(1);
        }
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&parent).unwrap();
            inner.sessions[index].active_turn_generation = opening_trace.generation;
            inner.sessions[index].engram.unadmitted_active_turn = Some(opening_trace);
        }
        let response = state.request_acceptance_evaluation_with_runner(
            &parent, evaluation_request(Some(Agent::Codex)), independent_reader(),
        ).unwrap();
        assert!(matches!(response, AcceptanceEvaluationRequestResponse::Spawned { .. }));
        assert_eq!(sighting_reads(&transport), 1,
            "an opening permits the real producer receipt check; it does not replace it");
    }
}

#[test]
fn acceptance_request_keeps_the_refusal_after_another_claim_is_named_in_the_same_turn() {
    let (state, parent, store, named_root) = named_root_parent();
    let transport = install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));
    let trace = ordinary_naming_trace(&state, &parent, false);
    install_ordinary_naming_turn(&state, &parent, trace);
    {
        let mut inner = state.inner.lock().unwrap();
        let other = EngramWorkSourceRoot {
            work_id: "01a0b6c5-4b4f-7b41-9e32-0000000000b2".to_owned(),
            claim_id: "claim-other".to_owned(), generation: 2,
            ..inner.engram_work_source_roots[0].clone()
        };
        let index = inner.find_session_index(&parent).unwrap();
        let engram = &mut inner.sessions[index].engram;
        let naming_turn = engram.observed_turn_ref().map(str::to_owned);
        record_engram_root_named_in_turn(
            engram, &store,
            &ordinary_naming_event(&other, &parent, EngramNamedRootKind::Bound), naming_turn,
        );
    }
    let response = state.request_acceptance_evaluation_with_runner(
        &parent, evaluation_request(Some(Agent::Codex)), independent_reader(),
    );
    let error = refusal(&state, response, "another claim named during the ordinary turn");
    assert_eq!(error.kind, Some(ApiErrorKind::AcceptanceEvaluationNamedRootTurnPending));
    assert_eq!(sighting_reads(&transport), 0);
}

#[test]
fn naming_marks_are_kept_per_claim_and_only_for_the_running_turn() {
    let (state, parent, store, _named_root) = named_root_parent();
    let first = state.inner.lock().unwrap().engram_work_source_roots[0].clone();
    let second = EngramWorkSourceRoot {
        work_id: "01a0b6c5-4b4f-7b41-9e32-0000000000b2".to_owned(),
        claim_id: "claim-other".to_owned(), generation: 2, ..first.clone()
    };
    let mut engram = EngramSessionState::default();
    let trace = ordinary_naming_trace(&state, &parent, false);
    let naming_turn = trace.host_turn_ref.clone();
    engram.unadmitted_active_turn = Some(trace);
    let marks = |engram: &EngramSessionState| engram.roots_named_in_turn.iter()
        .map(|mark| (mark.claim_id.clone(), mark.generation, mark.host_turn_ref.clone()))
        .collect::<Vec<_>>();
    let mark = |claim: &str, generation: u64, turn: &str| (claim.to_owned(), generation, turn.to_owned());
    let record = |engram: &mut EngramSessionState, root: &EngramWorkSourceRoot,
                  kind: EngramNamedRootKind, turn: &str| {
        record_engram_root_named_in_turn(engram, &store,
            &ordinary_naming_event(root, &parent, kind), Some(turn.to_owned()))
    };
    record(&mut engram, &first, EngramNamedRootKind::Bound, &naming_turn);
    record(&mut engram, &second, EngramNamedRootKind::Bound, &naming_turn);
    assert_eq!(marks(&engram), [mark("claim-task", 1, &naming_turn), mark("claim-other", 2, &naming_turn)]);
    record(&mut engram, &second, EngramNamedRootKind::Ended, &naming_turn);
    assert_eq!(marks(&engram), [mark("claim-task", 1, &naming_turn)]);
    record(&mut engram, &second, EngramNamedRootKind::Bound, "ended-ordinary-turn");
    assert_eq!(marks(&engram), [mark("claim-task", 1, &naming_turn)],
        "a transition from an ended turn cannot mark the current one");
    let next_trace = ordinary_naming_trace(&state, &parent, false);
    let next_turn = next_trace.host_turn_ref.clone();
    engram.unadmitted_active_turn = Some(next_trace);
    record(&mut engram, &second, EngramNamedRootKind::Bound, &next_turn);
    assert_eq!(marks(&engram), [mark("claim-other", 2, &next_turn)],
        "old-turn marks are pruned by the existing ordinary owner");
}

#[test]
fn acceptance_request_reads_engram_without_a_local_capture() {
    let (state, parent, _store, named_root) = named_root_parent();
    let transport = install_sighting_transport(
        &state, &named_root, 1, json!({"state":"absent"}),
    );
    let response = state.request_acceptance_evaluation_with_runner(
        &parent, evaluation_request(Some(Agent::Codex)), independent_reader(),
    );
    let error = refusal(&state, response, "ordinary named root without a capture");
    assert_eq!(error.kind,
        Some(ApiErrorKind::AcceptanceEvaluationNamedRootSightingUnavailable));
    assert!(error.message.contains("has no sighting")
        && error.message.contains("acknowledged matching named-root sighting")
        && error.message.contains("supported measured capture"), "{}", error.message);
    assert_eq!(sighting_reads(&transport), 1,
        "naming alone never substitutes for Engram's read");
}

#[test]
fn acceptance_request_uses_the_ordinary_sighting_receipt_without_manufacturing_evidence() {
    for present in [false, true] {
        let (state, parent, _store, named_root) = named_root_parent();
        let transport = install_sighting_transport(
            &state, &named_root, 1,
            if present { present_sighting() } else { json!({"state":"absent"}) },
        );
        let response = state.request_acceptance_evaluation_with_runner(
            &parent, evaluation_request(Some(Agent::Codex)), independent_reader(),
        );
        assert_eq!(sighting_reads(&transport), 1);
        if present {
            assert!(matches!(response.unwrap(), AcceptanceEvaluationRequestResponse::Spawned { .. }),
                "an existing matching producer sighting passes the source preflight");
        } else {
            let error = refusal(&state, response, "no producer sighting");
            assert_eq!(error.status, StatusCode::CONFLICT);
            assert_eq!(error.kind, Some(ApiErrorKind::AcceptanceEvaluationNamedRootSightingUnavailable));
            assert!(error.message.contains("has no sighting")
                && error.message.contains("cut 42")
                && error.message.contains("eligible measured execution observation")
                && error.message.contains("source_root_generation")
                && error.message.contains("supported measured capture")
                && error.message.contains("(named_root_sighting_unavailable)")
                && !error.message.contains("Then request a new evaluation from a later turn"),
                "the refusal describes the evidence actually required: {}", error.message);
        }
        assert!(transport.requests().iter().all(|request| matches!(
            request.request["operation"].as_str(),
            Some("work_session_read" | "named_root_sighting_read")
        )), "a sighting read cannot create a measurement or verification");
    }
}
