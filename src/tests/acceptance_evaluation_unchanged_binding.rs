// Owns the regressions for an acceptance evaluation whose work's named-root
// binding did not change while it ran: a canonical read learning that same
// binding's Bound event (claim, workspace, generation, named_at and run),
// whatever the history had learned before, must not refuse its first write,
// while real invalidations (a rename, an authority epoch change, a
// conflicting naming intent, a history advance not proved to describe the
// evaluated binding, a claim changed or released, a naming away and back)
// still do, and a refusal names its cause. Also owns an
// authority transition in flight during the submission: refused at once as
// retryable with the attempt left open, the same attempt then judged on what
// the pass published, and a recorded attempt never written twice.
// Does not own the reclamation planner leaving an evaluated work's root alone
// (engram_named_root_reclamation_eligibility.rs), nor the evaluation's other validity checks (source fingerprint,
// evidence and acceptance basis: acceptance_evaluation_source.rs) or the
// naming side (engram_named_root_naming_supersede.rs).
// New module.
use super::*;

/// The canonical run the fixture's namings are acknowledged in.
const FIXTURE_RUN_ID: &str = "run-task";
const FIXTURE_RUN_GENERATION: i64 = 1;

/// The fixture work's naming history: its work id and current revision.
fn fixture_history(state: &AppState) -> (String, u64) {
    let inner = state.inner.lock().unwrap();
    assert_eq!(inner.engram_work_naming_history.len(), 1, "one fixture work");
    let history = &inner.engram_work_naming_history[0];
    (history.work_id.clone(), history.revision)
}

/// `entry`'s binding as an evaluation request captures it, in `run_id`.
fn bound_in_run(entry: &EngramWorkSourceRoot, run_id: &str) -> EngramWorkNamingBound {
    EngramWorkNamingBound {
        claim_id: entry.claim_id.clone(),
        workspace: entry.root.clone(),
        generation: entry.generation,
        named_at: entry.named_at.clone(),
        run_id: run_id.to_owned(),
        run_generation: FIXTURE_RUN_GENERATION,
    }
}

/// `entry`'s naming event of `kind` as a canonical read reports it, in `run_id`.
fn naming_event(
    entry: &EngramWorkSourceRoot,
    kind: EngramNamedRootKind,
    run_id: &str,
) -> EngramNamingFrontier {
    EngramNamingFrontier {
        run_id: run_id.to_owned(),
        run_generation: FIXTURE_RUN_GENERATION,
        claim_id: entry.claim_id.clone(),
        event_id: format!("event-{}-{}-{kind:?}", entry.claim_id, entry.generation),
        position: EngramNamedRootReadPosition {
            feed: EngramNamedRootReadFeed {
                kind: "run_execution".to_owned(),
                id: run_id.to_owned(),
            },
            position: i64::try_from(entry.generation).unwrap(),
        },
        workspace_id: entry.root.clone(),
        root_generation: i64::try_from(entry.generation).unwrap(),
        named_at: entry.named_at.clone(),
        kind,
    }
}

/// A canonical read learning `event` as the history's frontier: the
/// revision moves once (engram_learn_canonical_history takes the read's
/// latest event, so one read may pass several events and still move once).
fn learn(state: &AppState, work_id: &str, event: EngramNamingFrontier) {
    let mut inner = state.inner.lock().unwrap();
    let history = inner
        .engram_work_naming_history
        .iter_mut()
        .find(|history| history.work_id == work_id)
        .expect("the work's naming history");
    history.frontier = Some(event);
    history.revision += 1;
}

/// A history advance nothing proves to describe the evaluated binding: only
/// the revision moves, as when a name was given and cleared meanwhile.
fn advance_history_unproved(state: &AppState, work_id: &str) {
    let mut inner = state.inner.lock().unwrap();
    let history = inner
        .engram_work_naming_history
        .iter_mut()
        .find(|history| history.work_id == work_id)
        .expect("the work's naming history");
    history.revision += 1;
}

/// An evaluator delegation whose target carries the work's current naming
/// token, optionally on a named root of that work under the fixture claim.
/// Returns the submit authority captured before any change, as an evaluator
/// would hold it while judging.
struct Paused {
    state: AppState,
    delegation: String,
    authority: AcceptanceEvaluationSubmitAuthority,
    request: SubmitAcceptanceEvaluationRequest,
    work_id: String,
    named: Option<EngramWorkSourceRoot>,
    /// The evaluator session, which submits.
    child: String,
}

/// The fixture's named root of the work under the fixture claim.
fn fixture_named_root(
    store: &EngramAuthorityStoreKey,
    work_id: &str,
    root: &FsPath,
    parent: &str,
) -> EngramWorkSourceRoot {
    EngramWorkSourceRoot {
        store: store.clone(),
        work_id: work_id.to_owned(),
        short_ref: "w-task".to_owned(),
        claim_id: "fixture-claim".to_owned(),
        claim_fence: 1,
        root: root.join("wt").to_string_lossy().into_owned(),
        common_dir_key: "unused".to_owned(),
        named_by_session: parent.to_owned(),
        named_at: "2026-09-27T00:00:00.000Z".to_owned(),
        generation: 3,
    }
}

/// A paused evaluation, its history's frontier at capture being `prior`
/// (computed from the named root when given) without moving the revision.
fn paused_evaluation_after(
    named_root: bool,
    prior: impl FnOnce(&EngramWorkSourceRoot) -> Option<EngramNamingFrontier>,
) -> Paused {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let workdir = root.to_string_lossy().into_owned();
    let (delegation, child) =
        install_evaluator_delegation(&state, &parent, Some(&project), &workdir, 2);
    let store = established_store(&state, &project).expect("the fixture's store");
    let (work_id, revision) = fixture_history(&state);
    let entry = fixture_named_root(&store, &work_id, &root, &parent);
    {
        let mut inner = state.inner.lock().unwrap();
        inner.engram_work_naming_history[0].frontier = prior(&entry);
    }
    let named = named_root.then_some(entry);
    if let Some(named) = &named {
        state.inner.lock().unwrap().engram_work_source_roots = vec![named.clone()];
    }
    // The token as the request captures it: the work's current history and
    // the binding the requested claim named, if any.
    let mut token = engram_work_naming_token(&state.inner.lock().unwrap(), &store, &work_id);
    assert_eq!(
        (token.epoch, token.revision),
        (ENGRAM_NAMING_HISTORY_EPOCH, revision),
        "the fixture's history is acknowledged"
    );
    token.named_binding = named.as_ref().map(|named| bound_in_run(named, FIXTURE_RUN_ID));
    update_evaluation_target(&state, &delegation, |target| {
        target.naming_history = Some(token.clone());
        target.source_root = named.as_ref().map(AcceptanceEvaluationSourceRoot::from_entry);
    });
    let request = two_verdicts();
    let (authority, _target, _model, _in_flight) = state
        .acceptance_evaluation_submit_context(&child, &request)
        .expect("the evaluator may submit");
    state
        .acceptance_evaluation_declared_target(&child, &authority.target)
        .expect("the declared target resolves before any change");
    // Control: before any change the target is current, so a refusal below
    // can come only from the change the test makes.
    {
        let inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&delegation).unwrap();
        let target = inner.delegations[index].acceptance_evaluation.as_ref().unwrap();
        assert!(
            !acceptance_evaluation_root_changed_locked(&inner, target),
            "the paused evaluation starts current"
        );
    }
    Paused { state, delegation, authority, request, work_id, named, child }
}

/// A paused evaluation whose history had learned no frontier at capture.
fn paused_evaluation(named_root: bool) -> Paused {
    paused_evaluation_after(named_root, |_| None)
}

fn first_write(paused: &Paused) -> Result<(), ApiError> {
    paused
        .state
        .begin_acceptance_evaluation_submission(
            &paused.authority,
            AcceptanceEvaluationOpenWrite {
                verdicts_digest: acceptance_evaluation_payload_digest(
                    &acceptance_evaluation_verdict_args(&paused.request),
                ),
                args: vec!["work".to_owned(), "evaluate".to_owned()],
            },
        )
        .map(|_| ())
}

fn nothing_written(paused: &Paused) -> bool {
    let inner = paused.state.inner.lock().unwrap();
    let index = inner.find_delegation_index(&paused.delegation).unwrap();
    matches!(
        inner.delegations[index].acceptance_evaluation.as_ref().unwrap().submission,
        AcceptanceEvaluationSubmission::None
    )
}

fn refused(paused: &Paused, why: &str) -> ApiError {
    let error = first_write(paused).expect_err(why);
    assert_eq!(error.status, StatusCode::CONFLICT, "{why}: {}", error.message);
    assert!(nothing_written(paused), "{why}: nothing was written");
    error
}

/// An earlier binding of the work, under `claim_id`, at another generation.
fn earlier_binding(named: &EngramWorkSourceRoot, claim_id: &str) -> EngramWorkSourceRoot {
    let mut earlier = named.clone();
    earlier.claim_id = claim_id.to_owned();
    earlier.root = format!("{}-earlier", named.root);
    earlier.generation = 1;
    earlier.named_at = "2026-09-26T00:00:00.000Z".to_owned();
    earlier
}

#[test]
fn a_captured_binding_takes_its_run_from_the_claims_acknowledged_proof() {
    // The request captures the binding with the canonical run its claim's
    // naming was acknowledged in; a claim with no acknowledged proof has no
    // provable binding, so its evaluation gets no exemption.
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let store = established_store(&state, &project).expect("the fixture's store");
    let (work_id, _) = fixture_history(&state);
    let entry = fixture_named_root(&store, &work_id, &root, &parent);
    let inner = state.inner.lock().unwrap();
    let history = &inner.engram_work_naming_history[0];
    let proof = history
        .proofs
        .iter()
        .find(|proof| proof.binding.claim_id == entry.claim_id)
        .expect("the fixture claim's acknowledged proof");
    let bound = EngramWorkNamingBound::of_entry(&entry, history).expect("a provable binding");
    assert_eq!(
        (bound.run_id.as_str(), bound.run_generation),
        (proof.read.run_id.as_str(), proof.read.run.generation),
        "the binding's run is the proof's"
    );
    let mut unproved = entry.clone();
    unproved.claim_id = "claim-without-a-proof".to_owned();
    assert!(
        EngramWorkNamingBound::of_entry(&unproved, history).is_none(),
        "no proof, no provable binding"
    );
}

// Control: with nothing changed, the paused evaluation writes, so a failure
// below can only come from the change the test makes.

#[test]
fn an_unchanged_paused_evaluation_writes() {
    for named_root in [false, true] {
        let paused = paused_evaluation(named_root);
        first_write(&paused).unwrap_or_else(|error| {
            panic!("an unchanged evaluation is admitted (named root {named_root}): {}", error.message)
        });
    }
}

// C1: learning the binding an evaluation was requested on never refuses it.

#[test]
fn a_first_frontier_of_the_same_binding_does_not_refuse_a_named_root_evaluation() {
    // The root stays named at the same generation under the same claim, yet
    // the first canonical read after the naming (the evaluator's own turn
    // admission, typically) moved the history revision by learning it.
    let paused = paused_evaluation(true);
    let named = paused.named.clone().unwrap();
    learn(&paused.state, &paused.work_id, naming_event(&named, EngramNamedRootKind::Bound, FIXTURE_RUN_ID));
    assert!(
        paused.state.inner.lock().unwrap().engram_work_source_roots == vec![named],
        "the binding is unchanged"
    );
    first_write(&paused).expect("an unchanged named binding is still the evaluated tree");
}

#[test]
fn the_same_bindings_bound_event_after_another_claims_frontier_does_not_refuse() {
    // The history had learned an earlier binding of the work under another
    // claim; the evaluation's own read then learns the evaluated binding.
    let paused = paused_evaluation_after(true, |named| {
        Some(naming_event(&earlier_binding(named, "earlier-claim"), EngramNamedRootKind::Bound, "run-earlier"))
    });
    let named = paused.named.clone().unwrap();
    learn(&paused.state, &paused.work_id, naming_event(&named, EngramNamedRootKind::Bound, FIXTURE_RUN_ID));
    first_write(&paused).expect("learning the evaluated binding is no change");
}

#[test]
fn a_single_read_past_the_older_bindings_end_to_the_same_binding_does_not_refuse() {
    // A re-name under the same claim: the history knew the older binding's
    // Bound event; one read passes its Ended event and reports the evaluated
    // binding's Bound event as the latest, so the revision moves once.
    let paused = paused_evaluation_after(true, |named| {
        Some(naming_event(&earlier_binding(named, &named.claim_id), EngramNamedRootKind::Bound, FIXTURE_RUN_ID))
    });
    let named = paused.named.clone().unwrap();
    learn(&paused.state, &paused.work_id, naming_event(&named, EngramNamedRootKind::Bound, FIXTURE_RUN_ID));
    first_write(&paused).expect("the evaluated binding is the latest naming");
}

// C3: a history advance not proved to describe the evaluated binding refuses.

#[test]
fn a_history_advance_without_a_named_binding_refuses_a_workdir_evaluation() {
    // With no binding named, nothing can prove a revision step benign: a
    // root may have been named and cleared meanwhile, under any claim, at a
    // claim-local generation no higher than any seen before.
    let paused = paused_evaluation(false);
    advance_history_unproved(&paused.state, &paused.work_id);
    let error = refused(&paused, "an unproved naming step is a real change");
    assert!(
        error.message.contains("naming history has since moved"),
        "the refusal names its cause: {}",
        error.message
    );
}

#[test]
fn the_end_of_the_evaluated_binding_refuses_after_its_bound_event_was_known() {
    let paused = paused_evaluation_after(true, |named| {
        Some(naming_event(named, EngramNamedRootKind::Bound, FIXTURE_RUN_ID))
    });
    let named = paused.named.clone().unwrap();
    learn(&paused.state, &paused.work_id, naming_event(&named, EngramNamedRootKind::Ended, FIXTURE_RUN_ID));
    refused(&paused, "the evaluated binding ended");
}

#[test]
fn a_rename_under_the_same_claim_during_the_evaluation_refuses() {
    // The holder re-named the work to another path while its evaluation ran;
    // the read learns the new binding's Bound event under the same claim.
    let paused = paused_evaluation(true);
    let mut renamed = paused.named.clone().unwrap();
    renamed.root = format!("{}-renamed", renamed.root);
    renamed.generation = 4;
    renamed.named_at = "2026-09-28T00:00:00.000Z".to_owned();
    learn(&paused.state, &paused.work_id, naming_event(&renamed, EngramNamedRootKind::Bound, FIXTURE_RUN_ID));
    let error = refused(&paused, "a re-name is a real change");
    assert!(
        error.message.contains("naming history has since moved"),
        "the refusal names the moved naming: {}",
        error.message
    );
}

#[test]
fn a_first_frontier_of_another_binding_refuses_a_named_root_evaluation() {
    let paused = paused_evaluation(true);
    let mut other = paused.named.clone().unwrap();
    other.named_at = "2026-09-28T00:00:00.000Z".to_owned();
    learn(&paused.state, &paused.work_id, naming_event(&other, EngramNamedRootKind::Bound, FIXTURE_RUN_ID));
    refused(&paused, "another binding's naming is a real change");
}

#[test]
fn the_same_bindings_bound_event_in_another_run_refuses() {
    let paused = paused_evaluation(true);
    let named = paused.named.clone().unwrap();
    learn(&paused.state, &paused.work_id, naming_event(&named, EngramNamedRootKind::Bound, "run-other"));
    refused(&paused, "a naming in another canonical run is not proved to be the same");
}

#[test]
fn a_further_step_after_the_same_bindings_bound_event_refuses() {
    // The exemption covers exactly one step.
    let paused = paused_evaluation(true);
    let named = paused.named.clone().unwrap();
    learn(&paused.state, &paused.work_id, naming_event(&named, EngramNamedRootKind::Bound, FIXTURE_RUN_ID));
    advance_history_unproved(&paused.state, &paused.work_id);
    refused(&paused, "a second step is not proved benign");
}

#[test]
fn an_ended_first_frontier_of_the_same_binding_refuses_a_named_root_evaluation() {
    // Only the binding's Bound event proves the step: its Ended event means
    // the name the evaluation was requested on is gone.
    let paused = paused_evaluation(true);
    let named = paused.named.clone().unwrap();
    learn(&paused.state, &paused.work_id, naming_event(&named, EngramNamedRootKind::Ended, FIXTURE_RUN_ID));
    refused(&paused, "an Ended event is a real change");
}

#[test]
fn a_legacy_epoch_token_gets_no_exemption() {
    // In the legacy epoch the no-frontier branch moves the revision without
    // any Bound event, so a token not of the current epoch is never exempt,
    // even with a matching frontier.
    let paused = paused_evaluation(true);
    let named = paused.named.clone().unwrap();
    let store = named.store.clone();
    let mut token = {
        let inner = paused.state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&paused.delegation).unwrap();
        inner.delegations[index]
            .acceptance_evaluation
            .as_ref()
            .unwrap()
            .naming_history
            .clone()
            .unwrap()
    };
    assert!(token.named_binding.is_some(), "the token names its binding");
    token.epoch = 0;
    paused.state.inner.lock().unwrap().engram_work_naming_history[0].epoch = 0;
    learn(&paused.state, &paused.work_id, naming_event(&named, EngramNamedRootKind::Bound, FIXTURE_RUN_ID));
    assert_eq!(
        engram_work_naming_change(&paused.state.inner.lock().unwrap(), &store, &token),
        Some(EngramWorkNamingChange::Moved),
        "a legacy-epoch token is not exempt"
    );
}

// C3: real invalidations stay fenced.

#[test]
fn a_rename_away_still_refuses_the_paused_evaluation() {
    let paused = paused_evaluation(true);
    {
        let mut inner = paused.state.inner.lock().unwrap();
        inner.engram_work_source_roots[0].generation = 4;
        let history = inner
            .engram_work_naming_history
            .iter_mut()
            .find(|history| history.work_id == paused.work_id)
            .unwrap();
        history.revision += 1;
        history.known_generation = history.known_generation.max(4);
    }
    refused(&paused, "a rename is a real change");
}

#[test]
fn an_authority_epoch_change_still_refuses_the_paused_evaluation() {
    let paused = paused_evaluation(true);
    {
        let mut inner = paused.state.inner.lock().unwrap();
        let history = inner
            .engram_work_naming_history
            .iter_mut()
            .find(|history| history.work_id == paused.work_id)
            .unwrap();
        history.epoch = history.epoch.wrapping_add(1);
    }
    refused(&paused, "another authority epoch is a real change");
}

// C3 + C4: a conflicting naming intent refuses, and says so.

#[test]
fn a_conflicting_naming_intent_refuses_and_names_itself() {
    // Another claim's naming of the same work reached Engram but never
    // completed here, so its intent is pending.
    let paused = paused_evaluation(true);
    {
        let mut inner = paused.state.inner.lock().unwrap();
        let mut foreign = paused.named.clone().unwrap();
        foreign.claim_id = "claim-of-another-session".to_owned();
        foreign.root = format!("{}-other", foreign.root);
        foreign.generation = 5;
        inner.engram_named_root_journal.push(EngramNamedRootJournal {
            store: foreign.store.clone(),
            claim_id: foreign.claim_id.clone(),
            read_binding: None,
            confirmed: None,
            pending: Some(EngramNamedRootEvent {
                root: foreign,
                reporter: "session-other".to_owned(),
                kind: EngramNamedRootKind::Bound,
                end_reason: None,
            }),
            obsolete: false,
            retirement: None,
            reconciliation: None,
        });
    }
    let error = refused(&paused, "a conflicting naming intent is a real change");
    // The refusal must say what conflicts and whether retrying can help,
    // not that the still-named root is no longer named.
    assert!(
        error.message.contains("pending") && error.message.contains("naming"),
        "the refusal names the pending naming intent: {}",
        error.message
    );
    assert!(
        !error.message.contains("no longer names"),
        "the root is still named, so the refusal must not say otherwise: {}",
        error.message
    );
}

// An authority transition in flight (a maintenance pass between its
// preparation and its publication) is refused at once as retryable, never as
// final and never waited out by the host: the evaluator, which holds the
// attempt, submits the same verdicts again once the pass has published.

/// A pass over the evaluated binding (the fixture claim, in the run its
/// token was taken in) prepared the work's authority and has not published
/// it yet: its owner's run is retained, as preparation retains it.
fn put_transition_in_flight(state: &AppState) {
    put_transition_in_flight_by(state, "fixture-claim", FIXTURE_RUN_ID);
}

/// As `put_transition_in_flight`, for a pass owned by `claim_id` in `run_id`.
fn put_transition_in_flight_by(state: &AppState, claim_id: &str, run_id: &str) {
    let mut inner = state.inner.lock().unwrap();
    let history = &mut inner.engram_work_naming_history[0];
    let transition = history.transition.as_mut().expect("the fixture's authority owner");
    transition.phase = EngramAuthorityPhase::Prepared;
    transition.binding.claim_id = claim_id.to_owned();
    transition.binding.run_id = run_id.to_owned();
    let owner_run = transition.binding.clone();
    history.unresolved_runs = vec![owner_run];
}

/// The pass published, discharging its read.
fn publish_transition(state: &AppState) {
    let mut inner = state.inner.lock().unwrap();
    let history = &mut inner.engram_work_naming_history[0];
    history.transition.as_mut().unwrap().phase = EngramAuthorityPhase::Published;
    history.unresolved_runs.clear();
}

/// The evaluator's whole submission, as its tool call runs it.
fn submit(
    paused: &Paused,
    run: impl Fn(
        &EngramConnectionConfig,
        &[String],
        Duration,
    ) -> std::result::Result<EngramCliOutput, EngramTransportError>,
) -> Result<AcceptanceEvaluationSubmitResponse, ApiError> {
    paused
        .state
        .submit_acceptance_evaluation_with_runner(&paused.child, two_verdicts(), run)
}

/// The refusal an authority transition in flight gives: retryable, with
/// the hint the evaluator waits, never the final wording.
fn assert_retryable_in_flight(error: &ApiError) {
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE, "{}", error.message);
    let hint = format!("wait {} seconds", ACCEPTANCE_EVALUATION_IN_FLIGHT_RETRY_AFTER.as_secs());
    assert!(
        error.message.starts_with(ACCEPTANCE_EVALUATION_AUTHORITY_IN_FLIGHT)
            && error.message.contains("still open")
            && error.message.contains(&hint)
            && !error.message.contains("do not submit again"),
        "retryable, never final: {}",
        error.message
    );
}

/// The attempt is still open: nothing written and the evaluator still runs.
fn assert_attempt_open(paused: &Paused) {
    assert!(nothing_written(paused), "nothing was written");
    let inner = paused.state.inner.lock().unwrap();
    let index = inner.find_delegation_index(&paused.delegation).unwrap();
    assert_eq!(inner.delegations[index].status, DelegationStatus::Running);
}

#[test]
fn the_in_flight_retry_hint_is_derived_from_the_passs_read_budget() {
    assert_eq!(ENGRAM_ROOT_RECLAMATION_BUDGET, Duration::from_secs(2));
    assert_eq!(ACCEPTANCE_EVALUATION_IN_FLIGHT_RETRY_AFTER, Duration::from_secs(3));
    assert_eq!(ACCEPTANCE_EVALUATION_IN_FLIGHT_RETRIES, 3);
    // The request side answers a transition in flight the same way.
    let error = acceptance_evaluation_naming_refusal("lead", EngramWorkNamingChange::InFlight);
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE, "{}", error.message);
    assert!(
        error.message.starts_with(ACCEPTANCE_EVALUATION_AUTHORITY_IN_FLIGHT)
            && error.message.contains("again in 3 seconds"),
        "{}",
        error.message
    );
}

#[test]
fn a_submit_during_a_pass_is_retryable_and_the_same_attempt_writes_once_it_publishes() {
    let paused = paused_evaluation(true);
    put_transition_in_flight(&paused.state);
    let error = submit(&paused, runner_must_not_run).expect_err("the pass has not published");
    assert_retryable_in_flight(&error);
    assert_attempt_open(&paused);

    publish_transition(&paused.state);
    let (calls, run) = scripted_runner(vec![Ok(cli_output(true, &evaluate_receipt(false), ""))]);
    submit(&paused, run).expect("the same attempt writes once the pass has published");
    assert_eq!(calls.lock().unwrap().len(), 1, "one tracker write");
    assert!(outcome_of(&paused.state, &paused.delegation).is_some(), "recorded");

    // The same verdicts once more are answered as already recorded, never as
    // a second write.
    let again = submit(&paused, runner_must_not_run).expect_err("one attempt records once");
    assert_eq!(again.message, ACCEPTANCE_EVALUATION_ALREADY_RECORDED_ERROR);
}

#[test]
fn in_flight_refusals_up_to_the_retry_limit_leave_the_attempt_open_for_a_later_submission() {
    // An evaluator that exhausts its retries reports the last refusal; the
    // attempt is still open, so a later submission of it still writes.
    let paused = paused_evaluation(true);
    put_transition_in_flight(&paused.state);
    for _ in 0..=ACCEPTANCE_EVALUATION_IN_FLIGHT_RETRIES {
        let error = submit(&paused, runner_must_not_run).expect_err("still in flight");
        assert_retryable_in_flight(&error);
        assert_attempt_open(&paused);
    }
    publish_transition(&paused.state);
    let (_, run) = scripted_runner(vec![Ok(cli_output(true, &evaluate_receipt(false), ""))]);
    submit(&paused, run).expect("the open attempt writes once the pass has published");
    assert!(outcome_of(&paused.state, &paused.delegation).is_some(), "recorded");
}

#[test]
fn a_submit_after_a_pass_retired_the_root_is_refused_on_the_retirement() {
    let paused = paused_evaluation(true);
    put_transition_in_flight(&paused.state);
    assert_retryable_in_flight(&first_write(&paused).expect_err("the pass has not published"));
    // The pass publishes a retirement: the retry meets a real change.
    paused.state.inner.lock().unwrap().engram_work_source_roots.clear();
    publish_transition(&paused.state);
    let error = refused(&paused, "a retired root is a real change");
    assert!(
        error.message.contains("no longer names")
            && !error.message.contains(ACCEPTANCE_EVALUATION_AUTHORITY_IN_FLIGHT),
        "the refusal names the retirement, final: {}",
        error.message
    );
}

#[test]
fn another_claims_prepared_naming_is_refused_finally_not_as_in_flight() {
    // A transition another claim prepared, or one in another run, is a
    // naming of the work by someone else, not a pass over the evaluated
    // binding: the refusal is the final pending one, never retryable.
    for (claim_id, run_id) in [
        ("concurrent-naming-claim", FIXTURE_RUN_ID),
        ("fixture-claim", "another-run"),
    ] {
        let paused = paused_evaluation(true);
        put_transition_in_flight_by(&paused.state, claim_id, run_id);
        let error = refused(&paused, "another owner's prepared naming is a real pending naming");
        assert!(
            error.message.contains("pending")
                && !error.message.contains(ACCEPTANCE_EVALUATION_AUTHORITY_IN_FLIGHT),
            "{claim_id}/{run_id}: {}",
            error.message
        );
    }
}

#[test]
fn an_unresolved_run_beside_a_pass_is_refused_finally() {
    // Another run awaiting recovery is a real pending naming, not a pass.
    let paused = paused_evaluation(true);
    put_transition_in_flight(&paused.state);
    {
        let mut inner = paused.state.inner.lock().unwrap();
        let history = &mut inner.engram_work_naming_history[0];
        let mut other = history.unresolved_runs[0].clone();
        other.run_id = "another-run".to_owned();
        history.unresolved_runs.push(other);
    }
    let error = refused(&paused, "an unresolved run is a real pending naming");
    assert!(
        error.message.contains("pending")
            && !error.message.contains(ACCEPTANCE_EVALUATION_AUTHORITY_IN_FLIGHT),
        "{}",
        error.message
    );
}

#[test]
fn a_pending_naming_intent_beside_a_pass_is_refused_finally() {
    // Another claim's naming of the work awaiting Engram's receipt is a real
    // pending naming too, whatever pass is in flight beside it.
    let paused = paused_evaluation(true);
    put_transition_in_flight(&paused.state);
    {
        let mut inner = paused.state.inner.lock().unwrap();
        let mut foreign = paused.named.clone().unwrap();
        foreign.claim_id = "claim-of-another-session".to_owned();
        foreign.root = format!("{}-other", foreign.root);
        foreign.generation = 5;
        inner.engram_named_root_journal.push(EngramNamedRootJournal {
            store: foreign.store.clone(),
            claim_id: foreign.claim_id.clone(),
            read_binding: None,
            confirmed: None,
            pending: Some(EngramNamedRootEvent {
                root: foreign,
                reporter: "session-other".to_owned(),
                kind: EngramNamedRootKind::Bound,
                end_reason: None,
            }),
            obsolete: false,
            retirement: None,
            reconciliation: None,
        });
    }
    let error = refused(&paused, "a pending naming intent is a real pending naming");
    assert!(
        error.message.contains("pending")
            && !error.message.contains(ACCEPTANCE_EVALUATION_AUTHORITY_IN_FLIGHT),
        "{}",
        error.message
    );
}

#[test]
fn a_capture_after_the_binding_was_already_learned_writes_unchanged() {
    // An earlier evaluation's read learned the binding before this capture:
    // the revision does not move again, so no exemption is needed.
    let paused = paused_evaluation_after(true, |named| {
        Some(naming_event(named, EngramNamedRootKind::Bound, FIXTURE_RUN_ID))
    });
    first_write(&paused).expect("an unchanged, already learned binding is still the evaluated tree");
}

// C4: a request for a work never named on this host says what to do.

#[test]
fn a_request_for_a_never_named_work_says_to_name_a_source_root_first() {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    // No naming of the work was ever acknowledged here.
    state.inner.lock().unwrap().engram_work_naming_history.clear();
    let error = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(None),
            fixture_reader(
                Arc::default(),
                show_receipt(None),
                Ok(policy_receipt(Some(&["same_session"]))),
            ),
        )
        .err()
        .expect("a work with no naming history cannot be evaluated");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error.message.contains("Name a source root first"),
        "the refusal says how to proceed: {}",
        error.message
    );
    assert!(state.inner.lock().unwrap().delegations.is_empty(), "nothing was spawned");
}

// C3: a claim changed or released, or a naming away and back again, while
// the evaluation is paused refuses it.

#[test]
fn a_claim_changed_during_the_evaluation_refuses_it() {
    // A recovered claim named the same path at the same generation: the
    // evaluated binding belonged to the old claim, which no longer names it.
    let paused = paused_evaluation(true);
    {
        let mut inner = paused.state.inner.lock().unwrap();
        let entry = &mut inner.engram_work_source_roots[0];
        entry.claim_id = "fixture-claim-recovered".to_owned();
        entry.claim_fence += 1;
    }
    let error = refused(&paused, "a changed claim is a real change");
    assert!(
        error.message.contains("no longer names"),
        "the refusal names the claim's lost root: {}",
        error.message
    );
}

#[test]
fn a_claim_released_during_the_evaluation_refuses_it() {
    // The release ended the binding: its entry is gone and the history
    // learned its Ended event.
    let paused = paused_evaluation_after(true, |named| {
        Some(naming_event(named, EngramNamedRootKind::Bound, FIXTURE_RUN_ID))
    });
    let named = paused.named.clone().unwrap();
    paused.state.inner.lock().unwrap().engram_work_source_roots.clear();
    learn(&paused.state, &paused.work_id, naming_event(&named, EngramNamedRootKind::Ended, FIXTURE_RUN_ID));
    let error = refused(&paused, "a released claim is a real change");
    assert!(
        error.message.contains("no longer names"),
        "the refusal names the released root: {}",
        error.message
    );
}

#[test]
fn a_naming_away_and_back_cannot_revive_the_paused_evaluation() {
    // Renamed away, then back: back at a new generation, as a real re-name
    // would be, and, strictest, back to the very entry evaluated, with the
    // history's frontier once more that binding's own Bound event. Two steps
    // are not the one proved step, so neither revives the request.
    for back_exactly in [false, true] {
        let paused = paused_evaluation(true);
        let named = paused.named.clone().unwrap();
        let mut away = named.clone();
        away.root = format!("{}-away", named.root);
        away.generation = 4;
        away.named_at = "2026-09-28T00:00:00.000Z".to_owned();
        paused.state.inner.lock().unwrap().engram_work_source_roots = vec![away.clone()];
        learn(&paused.state, &paused.work_id, naming_event(&away, EngramNamedRootKind::Bound, FIXTURE_RUN_ID));
        let back = if back_exactly {
            named.clone()
        } else {
            let mut back = named.clone();
            back.generation = 5;
            back.named_at = "2026-09-29T00:00:00.000Z".to_owned();
            back
        };
        paused.state.inner.lock().unwrap().engram_work_source_roots = vec![back.clone()];
        learn(&paused.state, &paused.work_id, naming_event(&back, EngramNamedRootKind::Bound, FIXTURE_RUN_ID));
        if back_exactly {
            assert!(
                paused.state.inner.lock().unwrap().engram_work_source_roots == vec![named.clone()],
                "the evaluated entry is restored exactly"
            );
        }
        let error = refused(&paused, "a naming away and back is a real change");
        assert!(
            error.message.contains("no longer names") || error.message.contains("has since moved"),
            "back exactly {back_exactly}: the refusal names its cause: {}",
            error.message
        );
        if back_exactly {
            assert!(
                error.message.contains("has since moved"),
                "the restored entry is still named, so the history decides: {}",
                error.message
            );
        }
    }
}
