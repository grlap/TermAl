// Owns the tests of the durable handoff ledger (src/handoffs.rs): a
// registered next step survives a restart; it is offered to its primary
// owner as soon as its prerequisite holds, with no one sending a wake; the
// offer carries the exact accept and complete calls and grants no other
// authority; nothing can be completed without the acceptance fence; the
// acceptance clock starts when the offer is delivered, so an offer queued
// behind a busy owner waits instead of escalating; a delivered offer is
// overdue after 30 seconds and, not accepted within five minutes, goes to the
// recovery owner, and the superseded offer can no longer be accepted; an
// offer removed twice is blocked, not re-queued; an accepted step never reaches a second
// executor; a credit-dependent step is never readied by a PASS alone and a
// changed claim routes it to diagnosis; a paused owner, or a Stop that
// cleared the offer's prompt, is shown and never bypassed. Time and the
// prerequisite are injected. Does not own the queued prompt durability hold
// (queued_prompt_durability.rs tests) or the test-run index.
use super::*;

const LIVE_RUN: &str = "test-handoff-run";

/// Two idle Claude sessions on a real, persisted state: the primary owner and
/// the recovery owner of the handoffs a test registers. Each session's
/// runtime hands the prompts it receives to its own channel.
struct Fixture {
    state: AppState,
    primary: String,
    recovery: String,
    primary_prompts: mpsc::Receiver<ClaudeRuntimeCommand>,
    recovery_prompts: mpsc::Receiver<ClaudeRuntimeCommand>,
    paths: (PathBuf, PathBuf, PathBuf),
    _temp: TestTempRoot,
    _roots: (TestTempRoot, TestTempRoot),
}

fn boot(paths: &(PathBuf, PathBuf, PathBuf)) -> AppState {
    AppState::new_with_paths(
        paths.0.to_string_lossy().into_owned(),
        paths.1.clone(),
        paths.2.clone(),
    )
    .expect("state should boot")
}

fn idle_session(
    state: &AppState,
    label: &str,
    root: &TestTempRoot,
) -> (String, mpsc::Receiver<ClaudeRuntimeCommand>) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner.create_session(
        Agent::Claude,
        Some(format!("Handoff {label}")),
        root.path().to_string_lossy().into_owned(),
        None,
        None,
    );
    let session = record.session.id.clone();
    let index = inner.find_session_index(&session).unwrap();
    let (runtime, prompts) = test_claude_runtime_handle(&format!("handoff-{label}"));
    let record = inner.session_mut_by_index(index).unwrap();
    record.runtime = SessionRuntime::Claude(runtime);
    record.session.status = SessionStatus::Idle;
    state.commit_locked(&mut inner).unwrap();
    (session, prompts)
}

impl Fixture {
    fn new(label: &str) -> Self {
        let (temp, project_root, persistence_path, templates_path) =
            super::delegation_support::temp_delegation_state_paths();
        let paths = (project_root, persistence_path, templates_path);
        let state = boot(&paths);
        let primary_root = TestTempRoot::create(&format!("handoff-{label}-primary"));
        let recovery_root = TestTempRoot::create(&format!("handoff-{label}-recovery"));
        let (primary, primary_prompts) = idle_session(&state, "primary", &primary_root);
        let (recovery, recovery_prompts) = idle_session(&state, "recovery", &recovery_root);
        Self {
            state,
            primary,
            recovery,
            primary_prompts,
            recovery_prompts,
            paths,
            _temp: temp,
            _roots: (primary_root, recovery_root),
        }
    }

    fn spec(&self, key: &str, prerequisite: HandoffPrerequisite) -> HandoffSpec {
        HandoffSpec {
            idempotency_key: key.to_owned(),
            prerequisite,
            next_action: HandoffAction {
                kind: HandoffActionKind::Prepare,
                text: "prepare the landing package for the gated tree".to_owned(),
            },
            primary_owner: self.primary.clone(),
            recovery_owner: self.recovery.clone(),
        }
    }

    fn register(&self, key: &str) -> HandoffRecord {
        self.state
            .register_handoff(
                &self.primary,
                self.spec(key, HandoffPrerequisite::Terminal { run_id: LIVE_RUN.to_owned() }),
            )
            .expect("the handoff registers")
    }

    fn tick(&self, now: chrono::DateTime<chrono::Utc>, outcome: HandoffPrerequisiteOutcome) -> HandoffTickReport {
        self.state
            .handoff_watchdog_tick_with(now, &|_| outcome.clone())
    }

    fn view(&self, handoff_id: &str, now: chrono::DateTime<chrono::Utc>) -> HandoffView {
        self.state
            .list_handoffs_at(now)
            .expect("the list reads")
            .into_iter()
            .find(|view| view.record.handoff_id == handoff_id)
            .expect("the handoff is listed")
    }

    fn record(&self, handoff_id: &str) -> HandoffRecord {
        self.view(handoff_id, chrono::Utc::now()).record
    }

    /// Sets `session`'s status, standing for a turn that is running (Active)
    /// or has ended (Idle).
    fn set_status(&self, session: &str, status: SessionStatus) {
        let mut inner = self.state.inner.lock().unwrap();
        let index = inner.find_session_index(session).unwrap();
        inner.session_mut_by_index(index).unwrap().session.status = status;
        self.state.commit_locked(&mut inner).unwrap();
    }

    /// What Stop does to an owner's queue: its orchestrator prompts are
    /// cleared and the pause latch holds the rest.
    fn stop(&self, session: &str) {
        let mut inner = self.state.inner.lock().unwrap();
        let index = inner.find_session_index(session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record
            .queued_prompts
            .retain(|queued| queued.source != QueuedPromptSource::Orchestrator);
        record.orchestrator_auto_dispatch_blocked = true;
        sync_pending_prompts(record);
        self.state.commit_locked(&mut inner).unwrap();
    }

    fn queued_texts(&self, session: &str) -> Vec<String> {
        let inner = self.state.inner.lock().unwrap();
        let index = inner.find_session_index(session).unwrap();
        inner.sessions[index]
            .queued_prompts
            .iter()
            .map(|queued| queued.pending_prompt.text.clone())
            .collect()
    }

    /// Drains `session`'s queue now, as the end of its turn does.
    fn drain(&self, session: &str) {
        if let Ok(Some(dispatch)) = self.state.dispatch_next_queued_turn(session, false) {
            let _ = deliver_turn_dispatch(&self.state, dispatch);
        }
    }
}

fn t0() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-10-17T16:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc)
}

fn at(seconds: i64) -> chrono::DateTime<chrono::Utc> {
    t0() + chrono::Duration::seconds(seconds)
}

/// The text of the next prompt `prompts` receives within the guard.
fn received_prompt(prompts: &mpsc::Receiver<ClaudeRuntimeCommand>, what: &str) -> String {
    match recv_within_guard(prompts, what) {
        Ok(ClaudeRuntimeCommand::Prompt(command)) => command.text,
        Ok(_) => panic!("{what}: expected a prompt, got another runtime command"),
        Err(error) => panic!("{what}: expected a prompt, got {error:?}"),
    }
}

fn prompts_waiting(prompts: &mpsc::Receiver<ClaudeRuntimeCommand>) -> usize {
    let mut count = 0;
    while let Ok(command) = prompts.try_recv() {
        if matches!(command, ClaudeRuntimeCommand::Prompt(_)) {
            count += 1;
        }
    }
    count
}

/// The incident this ledger exists for: a gate ends while its coordinator is
/// idle and nothing sends the wake. The host itself readies the registered
/// step and offers it to its owner. The offer carries the exact calls and
/// grants no other authority, nothing completes without the acceptance
/// fence, and the owner completes it once, without any user prompt.
#[test]
fn a_missed_wake_handoff_is_offered_accepted_and_completed_without_a_user_prompt() {
    let fixture = Fixture::new("missed-wake");
    let handoff = fixture.register("missed-wake");
    assert_eq!(handoff.state, HandoffState::WaitingPrerequisite);

    let report = fixture.tick(at(0), HandoffPrerequisiteOutcome::Pending);
    assert!(report.offered.is_empty(), "nothing is offered before the run ends");
    assert_eq!(fixture.record(&handoff.handoff_id).state, HandoffState::WaitingPrerequisite);

    let report = fixture.tick(at(2), HandoffPrerequisiteOutcome::Met);
    assert_eq!(
        report.offered,
        vec![(handoff.handoff_id.clone(), fixture.primary.clone(), 1)],
        "the step is offered to its primary owner at once"
    );
    let text = received_prompt(&fixture.primary_prompts, "the primary owner is woken by the host");
    assert!(text.contains(&handoff.handoff_id), "the offer names the handoff: {text}");
    assert!(text.contains("prepare the landing package"), "the offer carries the action: {text}");
    assert!(
        text.contains(&format!("registered by session `{}`", fixture.primary)),
        "the offer names who asked: {text}"
    );
    assert!(
        text.contains("not an instruction from TermAl"),
        "the action is framed as the producer's request: {text}"
    );
    let accept_path = format!(
        "/api/sessions/{}/handoffs/{}/accept",
        fixture.primary, handoff.handoff_id
    );
    assert!(text.contains(&accept_path), "the offer carries the exact accept call: {text}");
    assert!(text.contains("\"offerGeneration\": 1"), "the accept call names its offer: {text}");
    assert!(text.contains("/complete"), "the offer carries the complete call: {text}");
    assert!(text.contains("Accept first"), "the offer says to accept before acting: {text}");
    assert!(
        text.contains("grants no land, push or install authority"),
        "the offer grants nothing beyond the step: {text}"
    );

    let offered = fixture.record(&handoff.handoff_id);
    assert_eq!(offered.state, HandoffState::Offered);
    assert_eq!(offered.offer_generation, 1);
    assert_eq!(offered.offered_to.as_deref(), Some(fixture.primary.as_str()));
    assert!(offered.offered_prompt_id.is_some(), "the row holds the exact queued prompt id");

    assert!(
        fixture
            .state
            .complete_handoff(&fixture.primary, &handoff.handoff_id, "no-fence", json!({ "early": true }))
            .is_err(),
        "nothing completes without the acceptance fence"
    );
    let accepted = fixture
        .state
        .accept_handoff(&fixture.primary, &handoff.handoff_id, 1)
        .expect("the primary accepts its offer");
    assert_eq!(accepted.state, HandoffState::Accepted);
    let fence = accepted.accept_fence.clone().expect("acceptance returns a fence");

    assert_eq!(
        fixture.record(&handoff.handoff_id).accept_fence,
        None,
        "the list never shows the fence"
    );
    assert_eq!(
        fixture.register("missed-wake").accept_fence,
        None,
        "a registration replay never shows the fence"
    );
    let completed = fixture
        .state
        .complete_handoff(
            &fixture.primary,
            &handoff.handoff_id,
            &fence,
            json!({ "prepared": "landing package", "commit": "abc123" }),
        )
        .expect("the owner completes it with its evidence");
    assert_eq!(completed.state, HandoffState::Completed);
    assert!(
        fixture
            .state
            .complete_handoff(&fixture.primary, &handoff.handoff_id, &fence, json!({ "again": true }))
            .is_err(),
        "a completed handoff never completes twice"
    );
    let report = fixture.tick(at(200), HandoffPrerequisiteOutcome::Met);
    assert!(report.offered.is_empty(), "a completed handoff is never offered again");
    assert_eq!(prompts_waiting(&fixture.recovery_prompts), 0, "the recovery owner was never needed");
    fixture.state.shutdown_persist_blocking();
}

/// Registration is idempotent per producer and key, a different body under
/// the same key is refused, and repeated passes and triggers make one offer.
#[test]
fn duplicate_registrations_and_repeated_triggers_make_one_handoff_and_one_offer() {
    let fixture = Fixture::new("duplicates");
    let first = fixture.register("same-key");
    let again = fixture.register("same-key");
    assert_eq!(again.handoff_id, first.handoff_id, "the same body returns the same record");
    let mut other = fixture.spec("same-key", HandoffPrerequisite::Terminal { run_id: LIVE_RUN.to_owned() });
    other.next_action.text = "something else".to_owned();
    assert!(
        fixture.state.register_handoff(&fixture.primary, other).is_err(),
        "the same key with another body is refused"
    );

    fixture.tick(at(0), HandoffPrerequisiteOutcome::Met);
    fixture.tick(at(0), HandoffPrerequisiteOutcome::Met);
    received_prompt(&fixture.primary_prompts, "the one offer");
    fixture.tick(at(2), HandoffPrerequisiteOutcome::Met);
    fixture.tick(at(4), HandoffPrerequisiteOutcome::Met);
    assert_eq!(prompts_waiting(&fixture.primary_prompts), 0, "one offer, however many passes");
    assert_eq!(fixture.record(&first.handoff_id).offer_generation, 1);
    fixture.state.shutdown_persist_blocking();
}

/// The ledger is durable: a registered step and its state survive a restart,
/// and the restarted host still offers it.
#[test]
fn a_registered_handoff_survives_a_restart_and_is_offered_after_it() {
    let fixture = Fixture::new("restart");
    let handoff = fixture.register("restart");
    let primary = fixture.primary.clone();
    let paths = fixture.paths.clone();
    fixture.state.shutdown_persist_blocking();
    drop(fixture.state);

    let restarted = boot(&paths);
    let listed = restarted
        .list_handoffs_at(at(0))
        .expect("the list reads after the restart");
    let record = listed
        .iter()
        .find(|view| view.record.handoff_id == handoff.handoff_id)
        .expect("the handoff survives the restart");
    assert_eq!(record.record.state, HandoffState::WaitingPrerequisite);
    assert_eq!(record.record.spec, handoff.spec);
    let (runtime, prompts) = test_claude_runtime_handle("handoff-restarted");
    {
        let mut inner = restarted.inner.lock().unwrap();
        let index = inner.find_session_index(&primary).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.runtime = SessionRuntime::Claude(runtime);
        record.session.status = SessionStatus::Idle;
    }
    let report = restarted.handoff_watchdog_tick_with(at(4), &|_| HandoffPrerequisiteOutcome::Met);
    assert_eq!(report.offered.len(), 1, "the restarted host offers it");
    received_prompt(&prompts, "the offer after the restart");
    restarted.shutdown_persist_blocking();
}

/// A completed handoff is completed exactly once and stays so across a
/// restart: an identical retry returns the original completion and changes
/// nothing, a conflicting completion is refused, and after the restart the
/// record, its completion evidence and its history are all still there,
/// the same retry rules hold, and nothing is offered again.
#[test]
fn a_completed_handoff_keeps_its_evidence_and_history_across_a_restart() {
    let fixture = Fixture::new("completed-restart");
    let handoff = fixture.register("completed-restart");
    fixture.tick(at(0), HandoffPrerequisiteOutcome::Met);
    received_prompt(&fixture.primary_prompts, "the offer");
    let fence = fixture
        .state
        .accept_handoff(&fixture.primary, &handoff.handoff_id, 1)
        .expect("the primary accepts")
        .accept_fence
        .expect("acceptance returns a fence");
    let evidence = json!({ "prepared": "landing package", "commit": "abc123" });
    let completed = fixture
        .state
        .complete_handoff(&fixture.primary, &handoff.handoff_id, &fence, evidence.clone())
        .expect("the owner completes it");
    let retried = fixture
        .state
        .complete_handoff(&fixture.primary, &handoff.handoff_id, &fence, evidence.clone())
        .expect("an identical retry returns the original completion");
    assert_eq!(retried.version, completed.version, "an identical retry changes nothing");
    assert_eq!(retried.history, completed.history, "an identical retry adds no history");
    assert!(
        fixture
            .state
            .complete_handoff(&fixture.primary, &handoff.handoff_id, &fence, json!({ "other": true }))
            .is_err(),
        "a conflicting completion is refused"
    );
    let before = fixture.record(&handoff.handoff_id);
    assert_eq!(before.state, HandoffState::Completed);
    assert_eq!(before.version, completed.version, "the refused completion changed nothing");

    let paths = fixture.paths.clone();
    fixture.state.shutdown_persist_blocking();
    drop(fixture.state);
    let restarted = boot(&paths);
    let after = restarted
        .list_handoffs_at(at(600))
        .expect("the list reads after the restart")
        .into_iter()
        .find(|view| view.record.handoff_id == handoff.handoff_id)
        .expect("the completed handoff survives the restart")
        .record;
    assert_eq!(after.state, HandoffState::Completed, "still completed");
    assert_eq!(after.completion.as_ref(), Some(&evidence), "the completion evidence is kept");
    assert_eq!(after.accepted_by, before.accepted_by, "the accepted owner is kept");
    assert_eq!(after.version, before.version, "the version is kept");
    assert_eq!(after.history, before.history, "the full history is kept");
    assert!(
        after.history.iter().any(|entry| entry.event.starts_with("completed by")),
        "the history records the completion"
    );

    let again = restarted
        .complete_handoff(&fixture.primary, &handoff.handoff_id, &fence, evidence.clone())
        .expect("an identical retry after the restart returns the original completion");
    assert_eq!(again.version, before.version, "and changes nothing");
    assert!(
        restarted
            .complete_handoff(&fixture.primary, &handoff.handoff_id, &fence, json!({ "late": true }))
            .is_err(),
        "a conflicting completion after the restart is refused"
    );
    let report = restarted.handoff_watchdog_tick_with(at(900), &|_| HandoffPrerequisiteOutcome::Met);
    assert!(report.offered.is_empty(), "a completed handoff is never offered again");
    restarted.shutdown_persist_blocking();
}

/// Every ledger write is a compare-and-swap on the row's version: a write
/// built from a record another writer has since moved on is refused and
/// leaves the stored row exactly as it was, while a write built from the
/// current version lands.
#[test]
fn a_stale_ledger_write_is_refused_by_the_version_compare_and_swap() {
    let fixture = Fixture::new("cas");
    let handoff = fixture.register("cas");
    let stale_base = fixture.record(&handoff.handoff_id);
    // Another writer moves the row on: the watchdog readies and offers it.
    fixture.tick(at(0), HandoffPrerequisiteOutcome::Met);
    let current = fixture.record(&handoff.handoff_id);
    assert!(current.version > stale_base.version, "the row moved on");

    let path = fixture.state.persistence_path.clone();
    let mut stale = stale_base.next(at(1), "a write built from the old version");
    stale.state = HandoffState::Completed;
    let written = with_handoff_writer(&path, |connection| cas_handoff_on(connection, &stale))
        .expect("the compare-and-swap runs");
    assert!(!written, "a write from a stale version is refused");
    let after = fixture.record(&handoff.handoff_id);
    assert_eq!(after, current, "the refused write left the row exactly as it was");

    let fresh = current.next(at(2), "a write built from the current version");
    let written = with_handoff_writer(&path, |connection| cas_handoff_on(connection, &fresh))
        .expect("the compare-and-swap runs");
    assert!(written, "a write from the current version lands");
    assert_eq!(fixture.record(&handoff.handoff_id).version, fresh.version);
    fixture.state.shutdown_persist_blocking();
}

/// The acceptance clock starts at delivery: an offer queued behind the
/// owner's running turn for five minutes is waiting, not overdue, never goes
/// to the recovery owner, and is accepted by the primary once delivered.
#[test]
fn an_offer_queued_behind_a_busy_owner_waits_instead_of_escalating() {
    let fixture = Fixture::new("busy");
    let handoff = fixture.register("busy");
    fixture.set_status(&fixture.primary, SessionStatus::Active);
    let report = fixture.tick(at(0), HandoffPrerequisiteOutcome::Met);
    assert_eq!(report.offered.len(), 1, "the offer is queued to the busy owner");
    for seconds in [31, 61, 300] {
        let report = fixture.tick(at(seconds), HandoffPrerequisiteOutcome::Met);
        assert!(report.offered.is_empty(), "no escalation at {seconds}s while it is queued");
        let view = fixture.view(&handoff.handoff_id, at(seconds));
        assert!(!view.overdue, "a queued offer is waiting, not overdue, at {seconds}s");
        assert!(view.record.waiting.is_some(), "the wait shows its reason at {seconds}s");
        assert!(view.record.delivered_at.is_none(), "not delivered while queued");
    }
    assert_eq!(prompts_waiting(&fixture.recovery_prompts), 0, "the recovery owner is never woken");

    fixture.set_status(&fixture.primary, SessionStatus::Idle);
    fixture.drain(&fixture.primary);
    received_prompt(&fixture.primary_prompts, "the queued offer reaches the owner when its turn ends");
    fixture.tick(at(301), HandoffPrerequisiteOutcome::Met);
    let delivered = fixture.record(&handoff.handoff_id);
    assert!(delivered.delivered_at.is_some(), "the delivery is recorded");
    let accepted = fixture
        .state
        .accept_handoff(&fixture.primary, &handoff.handoff_id, 1)
        .expect("the primary accepts the offer it was finally given");
    assert_eq!(accepted.accepted_by.as_deref(), Some(fixture.primary.as_str()));
    assert_eq!(prompts_waiting(&fixture.recovery_prompts), 0, "one executor");
    fixture.state.shutdown_persist_blocking();
}

/// A delivered offer the primary owner does not accept is overdue 30 seconds
/// after delivery, but the owner keeps it while its turn's startup reads run;
/// five minutes after delivery it is superseded by an offer to the recovery
/// owner, and the superseded offer can no longer be accepted.
#[test]
fn a_delivered_unaccepted_offer_is_overdue_at_30_seconds_and_handed_to_recovery_after_five_minutes() {
    let fixture = Fixture::new("recovery");
    let handoff = fixture.register("recovery");
    fixture.tick(at(0), HandoffPrerequisiteOutcome::Met);
    received_prompt(&fixture.primary_prompts, "the primary offer");
    fixture.tick(at(1), HandoffPrerequisiteOutcome::Met);
    assert!(
        fixture.record(&handoff.handoff_id).delivered_at.is_some(),
        "the delivery is observed"
    );

    let report = fixture.tick(at(32), HandoffPrerequisiteOutcome::Met);
    assert!(report.offered.is_empty(), "overdue is not yet handed over");
    assert!(fixture.view(&handoff.handoff_id, at(32)).overdue, "30 seconds after delivery it is overdue");

    for seconds in [62, 180, 300] {
        let report = fixture.tick(at(seconds), HandoffPrerequisiteOutcome::Met);
        assert!(report.offered.is_empty(), "the owner keeps a delivered offer at {seconds}s");
        assert!(fixture.view(&handoff.handoff_id, at(seconds)).overdue, "still overdue at {seconds}s");
    }
    assert_eq!(prompts_waiting(&fixture.recovery_prompts), 0, "no recovery wake yet");

    let report = fixture.tick(at(302), HandoffPrerequisiteOutcome::Met);
    assert_eq!(
        report.offered,
        vec![(handoff.handoff_id.clone(), fixture.recovery.clone(), 2)],
        "the recovery owner gets a new offer five minutes after delivery"
    );
    let text = received_prompt(&fixture.recovery_prompts, "the recovery offer");
    assert!(text.contains(&handoff.handoff_id));
    assert!(
        fixture.state.accept_handoff(&fixture.primary, &handoff.handoff_id, 1).is_err(),
        "the superseded primary offer can no longer be accepted"
    );
    let accepted = fixture
        .state
        .accept_handoff(&fixture.recovery, &handoff.handoff_id, 2)
        .expect("the recovery owner accepts the current offer");
    assert_eq!(accepted.accepted_by.as_deref(), Some(fixture.recovery.as_str()));
    fixture.state.shutdown_persist_blocking();
}

/// Once accepted, no timeout gives the step to a second executor.
#[test]
fn an_accepted_handoff_is_never_offered_to_a_second_executor() {
    let fixture = Fixture::new("accepted");
    let handoff = fixture.register("accepted");
    fixture.tick(at(0), HandoffPrerequisiteOutcome::Met);
    received_prompt(&fixture.primary_prompts, "the primary offer");
    fixture
        .state
        .accept_handoff(&fixture.primary, &handoff.handoff_id, 1)
        .expect("the primary accepts");
    for seconds in [31, 61, 600, 3600] {
        let report = fixture.tick(at(seconds), HandoffPrerequisiteOutcome::Met);
        assert!(report.offered.is_empty(), "no offer at {seconds}s after acceptance");
    }
    assert_eq!(prompts_waiting(&fixture.recovery_prompts), 0, "the recovery owner never executes it");
    assert!(
        fixture.state.accept_handoff(&fixture.recovery, &handoff.handoff_id, 1).is_err(),
        "nobody else can accept an accepted handoff"
    );
    let record = fixture.record(&handoff.handoff_id);
    assert_eq!(record.state, HandoffState::Accepted);
    assert_eq!(record.accepted_by.as_deref(), Some(fixture.primary.as_str()));
    fixture.state.shutdown_persist_blocking();
}

/// A credit-dependent step is not readied by a PASS whose credit is still
/// owed. When a restart between the PASS and the closing checkpoint changes
/// the claim, the step takes the diagnostic route, and the offer says so.
#[test]
fn a_pass_without_credit_never_readies_a_credited_step_and_a_changed_claim_routes_to_diagnosis() {
    let fixture = Fixture::new("credit");
    let handoff = fixture
        .state
        .register_handoff(
            &fixture.primary,
            fixture.spec("credit", HandoffPrerequisite::Credited { run_id: LIVE_RUN.to_owned() }),
        )
        .expect("the handoff registers");
    let report = fixture.tick(at(0), HandoffPrerequisiteOutcome::Pending);
    assert!(report.offered.is_empty(), "a PASS whose credit is owed readies nothing");
    assert_eq!(fixture.record(&handoff.handoff_id).state, HandoffState::WaitingPrerequisite);

    // A restart between the PASS and the closing checkpoint.
    let primary = fixture.primary.clone();
    let paths = fixture.paths.clone();
    fixture.state.shutdown_persist_blocking();
    drop(fixture.state);
    let restarted = boot(&paths);
    let (runtime, prompts) = test_claude_runtime_handle("handoff-credit-restarted");
    {
        let mut inner = restarted.inner.lock().unwrap();
        let index = inner.find_session_index(&primary).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.runtime = SessionRuntime::Claude(runtime);
        record.session.status = SessionStatus::Idle;
    }
    let reason = "the claim changed since the run; its credit can no longer settle";
    restarted.handoff_watchdog_tick_with(at(4), &|_| {
        HandoffPrerequisiteOutcome::Diagnostic(reason.to_owned())
    });
    let record = restarted
        .list_handoffs_at(at(4))
        .unwrap()
        .into_iter()
        .find(|view| view.record.handoff_id == handoff.handoff_id)
        .unwrap()
        .record;
    assert_eq!(record.state, HandoffState::Offered, "the diagnostic route is offered");
    assert_eq!(record.diagnostic.as_deref(), Some(reason));
    let text = received_prompt(&prompts, "the diagnostic offer");
    assert!(text.contains(reason), "the offer names the diagnosis: {text}");
    assert!(text.contains("do not treat the step as succeeded"), "the offer forbids the success path: {text}");
    restarted.shutdown_persist_blocking();
}

/// The production check of a changed claim: the same work under another
/// claim or fence shows it; another work, or no binding, does not.
#[test]
fn a_changed_claim_or_fence_on_the_same_work_is_recognized() {
    let run = EngramControlWorkBinding {
        root_execution_id: "root".to_owned(),
        work_id: "work-a".to_owned(),
        run_id: "run-1".to_owned(),
        work_revision: 3,
        claim_id: "claim-1".to_owned(),
        claim_fence: 1,
    };
    assert_eq!(handoff_credit_claim_changed(&run, Some(&run)), None, "the same claim settles");
    let mut refenced = run.clone();
    refenced.claim_fence = 2;
    assert!(handoff_credit_claim_changed(&run, Some(&refenced)).is_some(), "a re-fenced claim");
    let mut reclaimed = run.clone();
    reclaimed.claim_id = "claim-2".to_owned();
    assert!(handoff_credit_claim_changed(&run, Some(&reclaimed)).is_some(), "another claim");
    let mut other_work = run.clone();
    other_work.work_id = "work-b".to_owned();
    other_work.claim_id = "claim-9".to_owned();
    assert_eq!(handoff_credit_claim_changed(&run, Some(&other_work)), None, "another work shows nothing");
    assert_eq!(handoff_credit_claim_changed(&run, None), None, "no binding shows nothing");
}

/// A primary owner whose queue is paused is never bypassed: the handoff shows
/// it as blocked, and the recovery owner gets the offer at 60 seconds.
#[test]
fn a_paused_primary_is_shown_blocked_and_never_bypassed() {
    let fixture = Fixture::new("paused");
    let handoff = fixture.register("paused");
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.primary).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.orchestrator_auto_dispatch_blocked = true;
        fixture.state.commit_locked(&mut inner).unwrap();
    }
    let report = fixture.tick(at(0), HandoffPrerequisiteOutcome::Met);
    assert!(report.offered.is_empty(), "a paused queue is not bypassed");
    assert_eq!(report.blocked.len(), 1, "the block is reported");
    assert!(fixture.record(&handoff.handoff_id).blocked.is_some(), "the block is visible");
    assert_eq!(prompts_waiting(&fixture.primary_prompts), 0);

    let report = fixture.tick(at(61), HandoffPrerequisiteOutcome::Met);
    assert_eq!(
        report.offered,
        vec![(handoff.handoff_id.clone(), fixture.recovery.clone(), 1)],
        "the recovery owner gets the step"
    );
    received_prompt(&fixture.recovery_prompts, "the recovery offer");
    fixture.state.shutdown_persist_blocking();
}

/// The row holds the offer's exact prompt id, so a Stop that cleared that
/// prompt is seen at the next pass: the handoff shows the owner as blocked,
/// nothing re-wakes the stopped owner, and the recovery owner gets the step.
#[test]
fn a_stop_that_cleared_the_offer_is_seen_and_handed_to_recovery() {
    let fixture = Fixture::new("stopped");
    let handoff = fixture.register("stopped");
    fixture.set_status(&fixture.primary, SessionStatus::Active);
    fixture.tick(at(0), HandoffPrerequisiteOutcome::Met);
    assert_eq!(fixture.queued_texts(&fixture.primary).len(), 1, "the offer is queued");
    fixture.stop(&fixture.primary);
    assert!(fixture.queued_texts(&fixture.primary).is_empty(), "Stop cleared it");

    let report = fixture.tick(at(2), HandoffPrerequisiteOutcome::Met);
    assert!(report.offered.is_empty(), "the stopped owner is not re-woken");
    assert!(
        fixture.record(&handoff.handoff_id).blocked.is_some(),
        "the cleared offer shows its owner as blocked"
    );
    assert!(fixture.queued_texts(&fixture.primary).is_empty(), "nothing is re-queued past the Stop");

    let report = fixture.tick(at(61), HandoffPrerequisiteOutcome::Met);
    assert_eq!(
        report.offered,
        vec![(handoff.handoff_id.clone(), fixture.recovery.clone(), 2)],
        "the recovery owner gets the step"
    );
    received_prompt(&fixture.recovery_prompts, "the recovery offer");
    fixture.state.shutdown_persist_blocking();
}

/// An offer whose prompt disappears from an owner that is not paused (a
/// Cancel) is queued again once under the same id; a second disappearance
/// of the same offer shows it blocked instead of re-queueing it every pass,
/// and the recovery owner gets the step.
#[test]
fn an_offer_removed_twice_is_blocked_instead_of_requeued_again() {
    let fixture = Fixture::new("cancelled");
    let handoff = fixture.register("cancelled");
    fixture.set_status(&fixture.primary, SessionStatus::Active);
    fixture.tick(at(0), HandoffPrerequisiteOutcome::Met);
    let prompt_id = fixture
        .record(&handoff.handoff_id)
        .offered_prompt_id
        .expect("the row holds the prompt id");
    let cancel = || {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.primary).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record
            .queued_prompts
            .retain(|queued| queued.pending_prompt.id != prompt_id);
        sync_pending_prompts(record);
        fixture.state.commit_locked(&mut inner).unwrap();
    };

    cancel();
    let report = fixture.tick(at(2), HandoffPrerequisiteOutcome::Met);
    assert!(report.offered.is_empty(), "a re-queue is not a new offer");
    assert_eq!(fixture.queued_texts(&fixture.primary).len(), 1, "queued again once");
    assert_eq!(fixture.record(&handoff.handoff_id).offer_generation, 1, "same generation");

    cancel();
    fixture.tick(at(4), HandoffPrerequisiteOutcome::Met);
    fixture.tick(at(6), HandoffPrerequisiteOutcome::Met);
    assert!(fixture.queued_texts(&fixture.primary).is_empty(), "never queued a third time");
    assert!(
        fixture.record(&handoff.handoff_id).blocked.is_some(),
        "the twice-removed offer shows as blocked"
    );

    let report = fixture.tick(at(61), HandoffPrerequisiteOutcome::Met);
    assert_eq!(
        report.offered,
        vec![(handoff.handoff_id.clone(), fixture.recovery.clone(), 2)],
        "the recovery owner gets the step"
    );
    received_prompt(&fixture.recovery_prompts, "the recovery offer");
    fixture.state.shutdown_persist_blocking();
}

/// A prompt promoted into the owner's transcript but held in its queue (a
/// deferred admission or bind) never reached the runtime: the offer stays
/// waiting and its acceptance clock does not start until the prompt leaves
/// the queue.
#[test]
fn an_offer_promoted_but_held_before_the_runtime_is_waiting_not_delivered() {
    let fixture = Fixture::new("retained");
    let handoff = fixture.register("retained");
    fixture.set_status(&fixture.primary, SessionStatus::Active);
    fixture.tick(at(0), HandoffPrerequisiteOutcome::Met);
    let prompt_id = fixture.record(&handoff.handoff_id).offered_prompt_id.unwrap();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.primary).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        let local = push_message_on_record(
            record,
            Message::Text {
                attachments: Vec::new(),
                id: prompt_id.clone(),
                timestamp: stamp_now(),
                author: Author::You,
                text: "the offer".to_owned(),
                expanded_text: None,
                source: None,
            },
        );
        let queued = record
            .queued_prompts
            .iter_mut()
            .find(|queued| queued.pending_prompt.id == prompt_id)
            .expect("the offer is still queued");
        queued.promoted_message_index = Some(local);
    }
    for seconds in [2, 40, 400] {
        fixture.tick(at(seconds), HandoffPrerequisiteOutcome::Met);
        let view = fixture.view(&handoff.handoff_id, at(seconds));
        assert!(view.record.delivered_at.is_none(), "not delivered at {seconds}s");
        assert!(
            view.record.waiting.as_deref().is_some_and(|reason| reason.contains("held before")),
            "the wait names the hold at {seconds}s: {:?}",
            view.record.waiting
        );
        assert!(!view.overdue, "a held offer is not overdue at {seconds}s");
    }

    // The hold releases: the prompt leaves the queue, and it is delivered.
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.primary).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record
            .queued_prompts
            .retain(|queued| queued.pending_prompt.id != prompt_id);
        sync_pending_prompts(record);
    }
    fixture.tick(at(401), HandoffPrerequisiteOutcome::Met);
    assert!(fixture.record(&handoff.handoff_id).delivered_at.is_some(), "delivered once released");
    fixture.state.shutdown_persist_blocking();
}

/// Every open handoff is looked at on every pass: many that stay pending
/// never starve a later one whose prerequisite holds.
#[test]
fn many_pending_handoffs_never_starve_a_later_ready_one() {
    let fixture = Fixture::new("many");
    for index in 0..70 {
        fixture
            .state
            .register_handoff(
                &fixture.primary,
                fixture.spec(
                    &format!("pending-{index}"),
                    HandoffPrerequisite::Terminal { run_id: format!("run-pending-{index}") },
                ),
            )
            .expect("a pending handoff registers");
    }
    let ready = fixture.register("ready-last");
    let report = fixture.state.handoff_watchdog_tick_with(at(0), &|prerequisite| {
        if prerequisite.run_id() == LIVE_RUN {
            HandoffPrerequisiteOutcome::Met
        } else {
            HandoffPrerequisiteOutcome::Pending
        }
    });
    assert_eq!(
        report.offered,
        vec![(ready.handoff_id.clone(), fixture.primary.clone(), 1)],
        "the 71st handoff is offered on the first pass"
    );
    received_prompt(&fixture.primary_prompts, "the late handoff's offer");
    fixture.state.shutdown_persist_blocking();
}

/// The settlement wake records its exact prompt id first. An owner that
/// cannot take it is never bypassed and gets it once it can; a wake lost
/// after that is not queued again past the one more queueing.
#[test]
fn the_settlement_wake_waits_for_a_paused_owner_and_is_queued_once_more_at_most() {
    let fixture = Fixture::new("settle");
    let handoff = fixture
        .state
        .register_handoff(
            &fixture.primary,
            fixture.spec("settle", HandoffPrerequisite::Credited { run_id: LIVE_RUN.to_owned() }),
        )
        .unwrap();
    let owner = fixture.recovery.clone();
    fixture.set_status(&owner, SessionStatus::Active);
    let pause = |paused: bool| {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&owner).unwrap();
        inner.session_mut_by_index(index).unwrap().orchestrator_auto_dispatch_blocked = paused;
        fixture.state.commit_locked(&mut inner).unwrap();
    };
    let settle = HandoffPrerequisiteOutcome::AwaitingSettlement { owner: owner.clone() };
    let settle_wakes = || {
        fixture
            .queued_texts(&owner)
            .iter()
            .filter(|text| text.contains("waits for its verification credit"))
            .count()
    };

    pause(true);
    fixture.tick(at(0), settle.clone());
    fixture.tick(at(2), settle.clone());
    let record = fixture.record(&handoff.handoff_id);
    assert!(record.settle_prompt_id.is_some(), "the wake's id is recorded first");
    assert_eq!(settle_wakes(), 0, "a paused owner is never bypassed");

    pause(false);
    fixture.tick(at(4), settle.clone());
    assert_eq!(settle_wakes(), 1, "the wake is queued once the owner can take it");
    fixture.tick(at(6), settle.clone());
    assert_eq!(settle_wakes(), 1, "and never twice");

    let prompt_id = fixture.record(&handoff.handoff_id).settle_prompt_id.unwrap();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&owner).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.queued_prompts.retain(|queued| queued.pending_prompt.id != prompt_id);
        sync_pending_prompts(record);
    }
    fixture.tick(at(8), settle.clone());
    fixture.tick(at(10), settle);
    assert_eq!(settle_wakes(), 0, "a wake lost after its one more queueing is not queued again");
    assert_eq!(
        fixture.record(&handoff.handoff_id).state,
        HandoffState::WaitingPrerequisite,
        "settling readies nothing by itself"
    );
    fixture.state.shutdown_persist_blocking();
}

/// When the primary's offer was cleared and the recovery owner is blocked
/// too, the row is written once and then left alone, not rewritten every
/// pass.
#[test]
fn two_blocked_owners_leave_the_row_stable() {
    let fixture = Fixture::new("both-blocked");
    let handoff = fixture.register("both-blocked");
    fixture.set_status(&fixture.primary, SessionStatus::Active);
    fixture.tick(at(0), HandoffPrerequisiteOutcome::Met);
    fixture.stop(&fixture.primary);
    fixture.stop(&fixture.recovery);
    fixture.tick(at(2), HandoffPrerequisiteOutcome::Met);
    fixture.tick(at(61), HandoffPrerequisiteOutcome::Met);
    let settled = fixture.record(&handoff.handoff_id);
    assert!(settled.blocked.is_some());
    for seconds in [63, 65, 67, 69] {
        let report = fixture.tick(at(seconds), HandoffPrerequisiteOutcome::Met);
        assert!(report.offered.is_empty());
    }
    let after = fixture.record(&handoff.handoff_id);
    assert_eq!(after.version, settled.version, "no write while nothing changes");
    assert_eq!(after.history.len(), settled.history.len(), "no history growth");
    fixture.state.shutdown_persist_blocking();
}

const RUN_OWNER: &str = "session-run-owner";

/// A run's indexed summary as the test-run index holds it.
fn run_summary(run_id: &str, state: &str, unknown_reason: Option<&str>, ended_at: &str) -> TestRunSummary {
    serde_json::from_value(json!({
        "runId": run_id,
        "projectId": null,
        "worktree": "C:/worktree",
        "runDir": "C:/worktree/.git/review-runs/run",
        "preset": "focused",
        "command": null,
        "commandTruncated": false,
        "detached": null,
        "state": state,
        "unknownReason": unknown_reason,
        "interrupted": false,
        "currentStage": null,
        "stages": [],
        "ownerSessionId": RUN_OWNER,
        "notifyTo": null,
        "notifySessionId": null,
        "startedAt": null,
        "endedAt": ended_at,
        "exitCode": 0,
        "error": null,
        "detailVersion": format!("sha256:{RUN_DIGEST}")
    }))
    .expect("a run summary")
}

/// The terminal digest of the launcher run's results, as its artifact names
/// it.
const RUN_DIGEST: &str = "d1d1d1";

fn claim(fence: i64) -> EngramControlWorkBinding {
    EngramControlWorkBinding {
        root_execution_id: "root".to_owned(),
        work_id: "work-a".to_owned(),
        run_id: "engram-run".to_owned(),
        work_revision: 1,
        claim_id: format!("claim-{fence}"),
        claim_fence: fence,
    }
}

/// A recorded command observation of launcher run `launcher_run` under
/// `binding`, shaped as production records it: the row carries its own
/// `observed-…` execution id as its run id and names the launcher run only
/// through its artifact (with the run's directory and terminal digest). It
/// has one verification association whose record id is
/// `verification_record_id`.
fn observation(
    launcher_run: &str,
    binding: Option<EngramControlWorkBinding>,
    verification_record_id: Option<&str>,
    outcome: EngramExecutionOutcome,
) -> TestRunCommandObservation {
    let execution_id = format!("observed-{launcher_run}-{}", Uuid::new_v4());
    let summary = run_summary(&execution_id, "passed", None, "2026-10-17T15:59:30Z");
    let mut row: TestRunCommandObservation = serde_json::from_value(json!({
        "executionId": execution_id,
        "runtimeIdentity": null,
        "toolIdentity": "Bash",
        "grantIdentity": null,
        "commandFingerprint": "f".repeat(64),
        "turnGeneration": 1,
        "observedAt": "2026-10-17T15:59:00Z",
        "summary": serde_json::to_value(summary).unwrap(),
        "workBinding": binding,
        "sourceRoot": null,
        "validatedUnbound": false,
        "artifact": {
            "runId": launcher_run,
            "runDirectory": "C:/worktree/.git/review-runs/run",
            "requestIdentity": null,
            "terminalDigest": RUN_DIGEST
        }
    }))
    .expect("an observation row");
    if let Some(binding) = binding {
        row.evidence.push(TestRunEvidenceAssociation {
            store: EngramAuthorityStoreKey {
                database_path: PathBuf::from("C:/tracker.db"),
                project_id: "project".to_owned(),
            },
            binding,
            source_basis: EngramExecutionSourceBasis {
                workspace_id: "C:/worktree".to_owned(),
                source_revision: "a".repeat(64),
                source_root_generation: Some(1),
                source_root_state: None,
            },
            producer_observation_id: "b".repeat(64),
            action_fingerprint: "c".repeat(64),
            reported_outcome: outcome,
            verification_record_id: verification_record_id.map(str::to_owned),
            checkpoint_id: None,
            confirmed_cursor: None,
            lifecycle_read: None,
        });
    }
    row
}

fn decide(
    prerequisite: HandoffPrerequisite,
    summary: Option<TestRunSummary>,
    observations: &[TestRunCommandObservation],
    owner_binding: Option<EngramControlWorkBinding>,
) -> HandoffPrerequisiteOutcome {
    let rows = observations.iter().collect::<Vec<_>>();
    handoff_prerequisite_outcome(&prerequisite, summary.as_ref(), &rows, owner_binding.as_ref(), at(60))
}

fn terminal() -> HandoffPrerequisite {
    HandoffPrerequisite::Terminal { run_id: LIVE_RUN.to_owned() }
}

fn credited() -> HandoffPrerequisite {
    HandoffPrerequisite::Credited { run_id: LIVE_RUN.to_owned() }
}

/// The production reader for a terminal prerequisite: a passed or failed run
/// meets it, a run whose process is gone without a result is diagnosed, and
/// anything else (running, uncertain, unindexed, another run) waits.
#[test]
fn the_production_reader_meets_a_terminal_prerequisite_only_on_a_result() {
    let ended = "2026-10-17T16:00:30Z";
    for state in ["passed", "failed"] {
        assert_eq!(
            decide(terminal(), Some(run_summary(LIVE_RUN, state, None, ended)), &[], None),
            HandoffPrerequisiteOutcome::Met,
            "a {state} run"
        );
    }
    assert!(matches!(
        decide(terminal(), Some(run_summary(LIVE_RUN, "unknown", Some("processGone"), ended)), &[], None),
        HandoffPrerequisiteOutcome::Diagnostic(_)
    ));
    for (state, reason) in [("running", None), ("unknown", Some("heartbeatStale"))] {
        assert_eq!(
            decide(terminal(), Some(run_summary(LIVE_RUN, state, reason, ended)), &[], None),
            HandoffPrerequisiteOutcome::Pending,
            "a {state} run waits"
        );
    }
    assert_eq!(decide(terminal(), None, &[], None), HandoffPrerequisiteOutcome::Pending);
    assert_eq!(
        decide(terminal(), Some(run_summary("another-run", "passed", None, ended)), &[], None),
        HandoffPrerequisiteOutcome::Pending,
        "another run's result is not this one's"
    );
}

/// A launcher run (`test-…`) finds the credited observations that its
/// artifact association names exactly, even though each carries its own
/// `observed-…` id; an observation of another run, of another owner, or
/// whose terminal digest does not match is not its credit. The credited one
/// then meets the prerequisite.
#[test]
fn a_launcher_run_finds_its_credit_through_the_exact_artifact_association() {
    let run = run_summary(LIVE_RUN, "passed", None, "2026-10-17T16:00:30Z");
    let record = Some("record");
    let ok = EngramExecutionOutcome::Succeeded;
    let mine = observation(LIVE_RUN, Some(claim(1)), record, ok);
    assert_ne!(mine.summary.run_id, LIVE_RUN, "the row carries its own observed id");
    let other_run = observation("test-another-run", Some(claim(1)), record, ok);
    let mut other_owner = observation(LIVE_RUN, Some(claim(1)), record, ok);
    other_owner.summary.owner_session_id = Some("session-someone-else".to_owned());
    let mut other_digest = observation(LIVE_RUN, Some(claim(1)), record, ok);
    other_digest.artifact.as_mut().unwrap().terminal_digest = Some("e2e2e2".to_owned());

    let mut inner = StateInner::new();
    for row in [&mine, &other_run, &other_owner, &other_digest] {
        inner.test_run_observations.insert(row.execution_id.clone(), row.clone());
    }
    let found = handoff_run_observations(&inner, &run, None);
    assert_eq!(
        found.iter().map(|row| row.execution_id.as_str()).collect::<Vec<_>>(),
        vec![mine.execution_id.as_str()],
        "only the exactly associated observation is the run's"
    );
    assert_eq!(
        handoff_prerequisite_outcome(&credited(), Some(&run), &found, Some(&claim(1)), at(60)),
        HandoffPrerequisiteOutcome::Met,
        "its recorded credit meets the prerequisite"
    );

    let mut uncredited = StateInner::new();
    for row in [&other_run, &other_owner, &other_digest] {
        uncredited.test_run_observations.insert(row.execution_id.clone(), row.clone());
    }
    let found = handoff_run_observations(&uncredited, &run, None);
    assert!(found.is_empty(), "no other run's, owner's or digest's credit is the run's");
    assert_eq!(
        handoff_prerequisite_outcome(&credited(), Some(&run), &found, Some(&claim(1)), at(60)),
        HandoffPrerequisiteOutcome::AwaitingSettlement { owner: RUN_OWNER.to_owned() },
    );
}

/// The production locked reader resolves a run outside the disk index too: a
/// launcher run compacted out of the disk history is read from its retained
/// record (its summary and its own observations), and a run the host
/// observed itself is its own observation row. A run the host does not know
/// waits.
#[test]
fn the_locked_reader_resolves_compacted_and_host_observed_runs() {
    let ok = EngramExecutionOutcome::Succeeded;
    let compacted = |observations: Vec<TestRunCommandObservation>| TestRunCompacted {
        compacted: true,
        request_identity: None,
        summary: run_summary(LIVE_RUN, "passed", None, "2026-10-17T16:00:30Z"),
        registration: None,
        observations,
        request_stamp: None,
        results_stamp: None,
        reason: "history limit".to_owned(),
        artifact_conflict: false,
        artifact_unavailable: false,
    };

    let mut inner = StateInner::new();
    inner.test_runs.scanned_once = true;
    assert_eq!(
        handoff_prerequisite_outcome_locked(&inner, &credited(), at(60)),
        HandoffPrerequisiteOutcome::Pending,
        "an unknown run waits"
    );
    inner.test_run_compacted.insert(
        LIVE_RUN.to_owned(),
        compacted(vec![observation(LIVE_RUN, Some(claim(1)), Some("record"), ok)]),
    );
    assert_eq!(
        handoff_prerequisite_outcome_locked(&inner, &credited(), at(60)),
        HandoffPrerequisiteOutcome::Met,
        "a compacted run's retained credit meets the prerequisite"
    );
    assert_eq!(
        handoff_prerequisite_outcome_locked(&inner, &terminal(), at(60)),
        HandoffPrerequisiteOutcome::Met,
        "a compacted run's result meets a terminal prerequisite"
    );
    inner
        .test_run_compacted
        .insert(LIVE_RUN.to_owned(), compacted(Vec::new()));
    assert_eq!(
        handoff_prerequisite_outcome_locked(&inner, &credited(), at(60)),
        HandoffPrerequisiteOutcome::AwaitingSettlement { owner: RUN_OWNER.to_owned() },
        "a compacted PASS without credit still never meets it"
    );

    let observed = observation(LIVE_RUN, Some(claim(1)), Some("record"), ok);
    let observed_run = observed.execution_id.clone();
    let mut inner = StateInner::new();
    inner.test_runs.scanned_once = true;
    inner.test_run_observations.insert(observed_run.clone(), observed);
    assert_eq!(
        handoff_prerequisite_outcome_locked(
            &inner,
            &HandoffPrerequisite::Credited { run_id: observed_run },
            at(60)
        ),
        HandoffPrerequisiteOutcome::Met,
        "a host-observed run is its own observation, with its own credit"
    );
}

/// The production reader for a credited prerequisite. A PASS alone, or a
/// PASS whose only association has no verification record, never meets it:
/// the run owner is asked to settle. Only
/// a recorded, succeeded verification association of this run meets it. A
/// failed run, a changed claim on the same work, and a credit not recorded
/// within the grace take the diagnostic route; a released claim (no
/// binding) is not diagnosed before the grace.
#[test]
fn the_production_reader_never_meets_a_credited_prerequisite_on_a_pass_alone() {
    let fresh = Some(run_summary(LIVE_RUN, "passed", None, "2026-10-17T16:00:30Z"));
    let settle = HandoffPrerequisiteOutcome::AwaitingSettlement { owner: RUN_OWNER.to_owned() };
    let run_claim = Some(claim(1));

    assert_eq!(decide(credited(), fresh.clone(), &[], run_claim.clone()), settle, "a PASS alone");
    assert_eq!(
        decide(
            credited(),
            fresh.clone(),
            &[observation(LIVE_RUN, run_claim.clone(), None, EngramExecutionOutcome::Succeeded)],
            run_claim.clone()
        ),
        settle,
        "an association without a verification record"
    );
    assert_eq!(
        decide(
            credited(),
            fresh.clone(),
            &[observation(LIVE_RUN, run_claim.clone(), Some("record"), EngramExecutionOutcome::Failed)],
            run_claim.clone()
        ),
        settle,
        "a recorded association whose reported outcome did not succeed"
    );
    assert_eq!(
        decide(
            credited(),
            fresh.clone(),
            &[observation(LIVE_RUN, run_claim.clone(), Some("record"), EngramExecutionOutcome::Succeeded)],
            run_claim.clone()
        ),
        HandoffPrerequisiteOutcome::Met,
        "this run's recorded, succeeded verification"
    );

    let changed = decide(
        credited(),
        fresh.clone(),
        &[observation(LIVE_RUN, run_claim.clone(), None, EngramExecutionOutcome::Succeeded)],
        Some(claim(2)),
    );
    assert!(
        matches!(&changed, HandoffPrerequisiteOutcome::Diagnostic(reason) if reason.contains("no longer the owner's claim")),
        "a re-fenced claim on the same work is diagnosed: {changed:?}"
    );
    assert_eq!(
        decide(
            credited(),
            fresh.clone(),
            &[observation(LIVE_RUN, run_claim.clone(), None, EngramExecutionOutcome::Succeeded)],
            None
        ),
        settle,
        "a released claim waits for the grace"
    );

    assert!(matches!(
        decide(credited(), Some(run_summary(LIVE_RUN, "failed", None, "2026-10-17T16:00:30Z")), &[], None),
        HandoffPrerequisiteOutcome::Diagnostic(_)
    ));
    let late = decide(
        credited(),
        Some(run_summary(LIVE_RUN, "passed", None, "2026-10-17T15:49:00Z")),
        &[],
        run_claim,
    );
    assert!(
        matches!(&late, HandoffPrerequisiteOutcome::Diagnostic(reason) if reason.contains("not recorded within")),
        "a credit not recorded within the grace is diagnosed: {late:?}"
    );
}

/// Registration names real, distinct owners.
#[test]
fn registration_refuses_unknown_or_identical_owners() {
    let fixture = Fixture::new("owners");
    let mut unknown = fixture.spec("unknown", HandoffPrerequisite::Terminal { run_id: LIVE_RUN.to_owned() });
    unknown.recovery_owner = "session-that-does-not-exist".to_owned();
    assert!(fixture.state.register_handoff(&fixture.primary, unknown).is_err());
    let mut same = fixture.spec("same", HandoffPrerequisite::Terminal { run_id: LIVE_RUN.to_owned() });
    same.recovery_owner = same.primary_owner.clone();
    assert!(fixture.state.register_handoff(&fixture.primary, same).is_err());
    fixture.state.shutdown_persist_blocking();
}
