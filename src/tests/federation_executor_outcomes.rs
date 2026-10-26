// Owns: central learning a remote wake's outcome from its receiver. Central
// dispatches the wake over the authenticated loopback pair, the satellite's
// production receive and activation worker queue and deliver it, and
// central's poll asks the satellite, over the same pair, what became of it.
// Message details then show the receiver's answer. Does not own the wake
// dispatch or delivery themselves (federation_executor_recovery_regressions.rs,
// federation_executor_delivery.rs tests). New module, a child of
// federation_executor_recovery_regressions.rs, whose Fixture it uses.
use super::*;

// The fixture's satellite has no persist writer; a healthy one acknowledges.
fn acknowledge_all_fences(state: &mut AppState) {
    state.shutdown_persist_blocking();
    let (tx, rx) = mpsc::channel::<PersistRequest>();
    state.persist_tx = tx;
    std::thread::spawn(move || {
        while let Ok(request) = rx.recv() {
            if let PersistRequest::Fence(fence) = request { fence.finish(Ok(())); }
        }
    });
}

fn root(fixture: &Fixture, label: &str, paused: bool) -> mpsc::Receiver<CodexRuntimeCommand> {
    let (runtime, input) = test_codex_runtime_handle(label);
    let mut inner = fixture.satellite.inner.lock().unwrap();
    let index = inner.find_session_index(&fixture.native).unwrap();
    let record = &mut inner.sessions[index];
    record.runtime = SessionRuntime::Codex(runtime);
    record.session.codex_fast_mode = false;
    record.session.status = SessionStatus::Idle;
    record.orchestrator_auto_dispatch_blocked = paused;
    fixture.satellite.commit_locked(&mut inner).unwrap();
    input
}

/// Central dispatches the pending wake to the satellite over the loopback
/// pair, and the satellite's production activation worker runs once.
async fn dispatch_and_activate(fixture: &Fixture) {
    let request = fixture.central.mailbox_store.reserve_federation_executor_wakes().unwrap()
        .into_iter().next().expect("the appended wake is reserved");
    fixture.central.dispatch_federation_executor_wake(request).await
        .expect("the satellite durably receives the wake");
    let mut worker = FederationExecutorActivationWorker::default();
    worker.tick(&fixture.satellite, true).await;
    worker.shutdown(&fixture.satellite).await;
}

fn message(fixture: &Fixture) -> String {
    fixture.request["wake"]["messageId"].as_str().unwrap().to_owned()
}

fn cause(fixture: &Fixture) -> String {
    fixture.central.mailbox_store.federation_executor_wake_state(&message(fixture)).unwrap()
}

fn outcome_row(fixture: &Fixture) -> Option<FederationExecutorOutcomeRow> {
    let connection = fixture.central.mailbox_store.connection().unwrap();
    fixture.central.mailbox_store.federation_executor_outcome_on(&connection,
        &fixture.target.central.host_id, &format!("mailbox-wake:{}", message(fixture))).unwrap()
}

/// Whether central's poll scan selects this wake at `at`: the backoff checked
/// against an explicit time, never against elapsed real time.
fn due_at(fixture: &Fixture, at: chrono::DateTime<chrono::Utc>) -> bool {
    !fixture.central.mailbox_store.federation_executor_outcome_candidates(&fixture.target.central.host_id,
        &[fixture.target.identity.host_id.clone()], at, FEDERATION_EXECUTOR_OUTCOME_POLL_BATCH).unwrap().is_empty()
}

fn next_poll_at(fixture: &Fixture) -> String {
    fixture.central.mailbox_store.connection().unwrap().query_row(
        "SELECT next_poll_at FROM federation_executor_outcomes", [], |row| row.get(0)).unwrap()
}

fn at(time: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(time).unwrap().with_timezone(&chrono::Utc)
}

/// Lets the next poll find the row due now, as its backoff would.
fn make_due(fixture: &Fixture) {
    fixture.central.mailbox_store.connection().unwrap().execute(
        "UPDATE federation_executor_outcomes SET next_poll_at='2000-01-01T00:00:00.000Z'", []).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_outcome_reports_a_woken_remote_session_as_woken() {
    let mut fixture = Fixture::new();
    acknowledge_all_fences(&mut fixture.satellite);
    let input = root(&fixture, "outcome-woken", false);
    let (stop, server) = attach_loopback(&fixture).await;
    dispatch_and_activate(&fixture).await;
    assert!(cause(&fixture).ends_with("; wake outcome pending"),
        "before any answer the wake is pending, never not-woken: {}", cause(&fixture));
    let CodexRuntimeCommand::Prompt { .. } = phase_sync::receive(&input, "the wake reaches the root's provider")
    else { panic!("the remote root must be woken"); };
    let turn = {
        let inner = fixture.satellite.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
        record.prompt_deliveries[&fixture.operation().queue_identity()].turn_generation
    };
    fixture.satellite.note_prompt_delivery_outcome(&fixture.native, turn, PromptDeliveryOutcome::Accepted);
    fixture.central.poll_federation_executor_outcomes().await;
    let row = outcome_row(&fixture).expect("central recorded the receiver's answer");
    assert_eq!(row.last_outcome, Some(FederationExecutorOutcome::Woken));
    assert!(row.terminal);
    assert!(cause(&fixture).starts_with("remote session woken at "), "{}", cause(&fixture));

    // A terminal answer is never replaced, and a woken wake is not polled again.
    fixture.central.mailbox_store.record_federation_executor_outcome(&fixture.operation(),
        Some(FederationExecutorOutcome::Queued), chrono::Utc::now(), || Ok(())).unwrap();
    assert_eq!(outcome_row(&fixture), Some(row.clone()));
    fixture.central.poll_federation_executor_outcomes().await;
    assert_eq!(outcome_row(&fixture), Some(row));
    let _ = stop.send(());
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_outcome_reports_a_paused_root_as_held_with_its_last_seen_time() {
    let mut fixture = Fixture::new();
    acknowledge_all_fences(&mut fixture.satellite);
    let input = root(&fixture, "outcome-held", true);
    let (stop, server) = attach_loopback(&fixture).await;
    dispatch_and_activate(&fixture).await;
    fixture.central.poll_federation_executor_outcomes().await;
    assert!(input.try_recv().is_err(), "a paused root is not woken");
    let row = outcome_row(&fixture).unwrap();
    assert_eq!(row.last_outcome, Some(FederationExecutorOutcome::HeldBehindPausedQueue));
    assert!(!row.terminal);
    let shown = cause(&fixture);
    assert!(shown.contains("; wake outcome pending; last seen: held behind the remote root's paused queue at "),
        "{shown}");

    // Backoff: the wake is not due again until its first backoff has passed.
    let observed = at(row.last_observed_at.as_deref().unwrap());
    let backoff = chrono::Duration::from_std(federation_executor_outcome_backoff(1)).unwrap();
    assert_eq!(next_poll_at(&fixture), federation_executor_outcome_time(observed + backoff));
    assert!(!due_at(&fixture, observed + backoff - chrono::Duration::milliseconds(1)));
    assert!(due_at(&fixture, observed + backoff));
    // The attempt bound ends polling, and the view says so.
    fixture.central.mailbox_store.connection().unwrap().execute(
        "UPDATE federation_executor_outcomes SET attempts=?1", [FEDERATION_EXECUTOR_OUTCOME_MAX_ATTEMPTS]).unwrap();
    make_due(&fixture);
    fixture.central.poll_federation_executor_outcomes().await;
    assert_eq!(outcome_row(&fixture).unwrap().attempts, FEDERATION_EXECUTOR_OUTCOME_MAX_ATTEMPTS);
    assert!(cause(&fixture).ends_with("; no longer polled"), "{}", cause(&fixture));
    let _ = stop.send(());
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_outcome_from_a_stale_binding_is_refused_and_never_shown() {
    let mut fixture = Fixture::new();
    acknowledge_all_fences(&mut fixture.satellite);
    let _input = root(&fixture, "outcome-stale", true);
    let (stop, server) = attach_loopback(&fixture).await;
    dispatch_and_activate(&fixture).await;
    {
        // The root's exact binding is gone: its answer is no longer evidence.
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        inner.sessions[index].federation_mailbox_binding = None;
    }
    fixture.central.poll_federation_executor_outcomes().await;
    let row = outcome_row(&fixture).expect("the attempt is counted");
    assert_eq!(row.last_outcome, None, "a refused answer stores no outcome");
    assert_eq!(row.attempts, 1);
    assert!(cause(&fixture).ends_with("; wake outcome pending"), "{}", cause(&fixture));
    let _ = stop.send(());
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_outcome_query_from_a_non_central_peer_is_refused() {
    let fixture = Fixture::new();
    let operation = fixture.operation();
    let request = FederationExecutorOutcomeRequest { protocol_version: FEDERATION_PROTOCOL_VERSION,
        payload_digest: operation.digest(), operation: FederationExecutorOperation {
            central_host_id: "another-central".into(), ..operation.clone() } };
    let router = federation_router(fixture.channel.clone(),
        Arc::new({ let check = fixture.channel.clone(); move || check.current_authority().map(|_| ()) }));
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    let context = fixture.outbound.security_context().unwrap();
    let client = FederationRpcClient::new(1, fixture.outbound.clone(),
        Arc::new({ let check = fixture.outbound.clone(); move || check.current_authority().map(|_| ()) })).unwrap();
    let counter = client.allocate_authenticated_counter(&context).await.unwrap();
    let signed = federation_control_fixture::signed(&fixture.outbound, counter, "POST",
        FEDERATION_EXECUTOR_OUTCOME_ROUTE, headers, &serde_json::to_vec(&request).unwrap());
    let response = router.oneshot(signed).await.unwrap();
    context.settle(counter);
    assert_eq!(response.status(), StatusCode::CONFLICT, "an operation from another central is refused");
}

fn view(stage: &str, receipt: Option<PromptDeliveryDisposition>, queued: bool, runtime: bool) -> FederationExecutorOperationView {
    FederationExecutorOperationView {
        operation: FederationExecutorOperation {
            central_host_id: "central".into(), central_epoch: 1, operation_key: "mailbox-wake:m".into(),
            target_host_id: "satellite".into(), enrollment_generation: 1, session_id: "session-1".into(),
            creation_id: "creation".into(), placement_generation: 1, command_kind: "wake".into(), payload: "{}".into(),
        },
        // Every stage past received has been associated with the root's queue.
        stage: stage.into(), effect_state: "not_started".into(),
        queue_entry_id: (stage != "received").then(|| "entry".into()),
        runtime_acknowledged: runtime, receipt, queue_position: queued.then_some(0),
        pending_prompts: Vec::new(), pending_prompt_count: usize::from(queued), pending_prompts_truncated: false,
        root_present: true, executor_local: true, root_status: Some(SessionStatus::Idle),
        root_paused: false, root_stopping: false, binding_current: true,
    }
}

/// Only positive runtime acceptance of this wake is woken. Mailbox evidence
/// from another turn's continuation is its own terminal answer, and a
/// rejected or unconfirmed delivery outranks it.
#[test]
fn federation_outcome_reports_woken_only_on_runtime_acceptance() {
    use PromptDeliveryDisposition as D;
    let from = |stage, receipt, queued, runtime| FederationExecutorOutcome::from_view(&view(stage, receipt, queued, runtime));
    assert_eq!(from("processed", None, false, false), FederationExecutorOutcome::MailboxProcessed);
    // After the mail's ACK this wake may still run: it is not final while it
    // is queued, pending or handing off.
    assert_eq!(from("processed", Some(D::Pending), true, false), FederationExecutorOutcome::Queued);
    assert_eq!(from("processed", Some(D::HandingOff), false, false), FederationExecutorOutcome::Queued);
    assert_eq!(from("processed", Some(D::Pending), false, false), FederationExecutorOutcome::Unknown);
    // An ACK before queue association: activation or recovery may still queue it.
    // A hand-off attempt with no known result is unconfirmed, not "processed without this wake".
    let mut attempted = view("processed", None, false, false);
    attempted.effect_state = "unknown".into();
    assert_eq!(FederationExecutorOutcome::from_view(&attempted), FederationExecutorOutcome::HandedOffUnconfirmed);
    let mut unassociated = view("processed", None, false, false);
    unassociated.queue_entry_id = None;
    assert_eq!(FederationExecutorOutcome::from_view(&unassociated), FederationExecutorOutcome::Received);
    assert_eq!(from("processed", Some(D::Rejected), false, false), FederationExecutorOutcome::Rejected);
    assert_eq!(from("processed", Some(D::Ambiguous), false, false), FederationExecutorOutcome::HandedOffUnconfirmed);
    assert_eq!(from("processed", None, false, true), FederationExecutorOutcome::Woken);
    assert_eq!(from("runtime_accepted", None, false, true), FederationExecutorOutcome::Woken);
    assert_eq!(from("queued", Some(D::Accepted), false, false), FederationExecutorOutcome::Woken);
    assert_eq!(from("received", None, false, false), FederationExecutorOutcome::Received);
    assert_eq!(from("queued", None, false, false), FederationExecutorOutcome::Unknown,
        "an entry gone with no retained receipt is not reported as queued");
    assert_eq!(from("queued", Some(D::Pending), true, false), FederationExecutorOutcome::Queued);
    assert_eq!(from("queued", Some(D::HandingOff), false, false), FederationExecutorOutcome::Queued);
    assert!(FederationExecutorOutcome::MailboxProcessed.terminal());
    assert!(!FederationExecutorOutcome::Unknown.terminal() && !FederationExecutorOutcome::Received.terminal());
}

/// Central's authority refusing at commit drops the answer, but the attempt
/// and its backoff are still recorded, so the wake is not asked every pass.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_outcome_refused_by_central_authority_still_counts_the_attempt() {
    let mut fixture = Fixture::new();
    acknowledge_all_fences(&mut fixture.satellite);
    let _input = root(&fixture, "outcome-refused", true);
    let (stop, server) = attach_loopback(&fixture).await;
    dispatch_and_activate(&fixture).await;
    let seeded = outcome_row(&fixture).expect("settlement creates the outcome row");
    assert_eq!((seeded.attempts, seeded.last_outcome), (0, None));
    let refused_at = at(&federation_executor_outcome_time(chrono::Utc::now()));
    let refused = fixture.central.mailbox_store.record_federation_executor_outcome(&fixture.operation(),
        Some(FederationExecutorOutcome::Woken), refused_at, || -> Result<()> { bail!("authority refused") });
    assert!(refused.is_err());
    let row = outcome_row(&fixture).unwrap();
    assert_eq!(row.attempts, 1, "the refused attempt is counted");
    assert_eq!(row.last_outcome, None, "the refused answer is not stored");
    let backoff = chrono::Duration::from_std(federation_executor_outcome_backoff(1)).unwrap();
    assert_eq!(next_poll_at(&fixture), federation_executor_outcome_time(refused_at + backoff));
    assert!(!due_at(&fixture, refused_at + backoff - chrono::Duration::milliseconds(1)),
        "its backoff holds the next pass");
    let _ = stop.send(());
    server.await.unwrap();
}

/// The age bound counts from settlement, not from the message's creation:
/// once that age has passed the wake is no longer polled, and says so.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_outcome_age_counts_from_settlement_and_ends_polling_visibly() {
    let mut fixture = Fixture::new();
    acknowledge_all_fences(&mut fixture.satellite);
    let _input = root(&fixture, "outcome-aged", true);
    let (stop, server) = attach_loopback(&fixture).await;
    dispatch_and_activate(&fixture).await;
    // Settled just now: the first pass asks, whatever the message's age.
    fixture.central.mailbox_store.connection().unwrap().execute(
        "UPDATE mailbox_messages SET created_at='2000-01-01T00:00:00+00:00'", []).unwrap();
    fixture.central.poll_federation_executor_outcomes().await;
    assert_eq!(outcome_row(&fixture).unwrap().attempts, 1, "an old message that settled now is polled");
    // Past the age bound from settlement, it is not asked again.
    fixture.central.mailbox_store.connection().unwrap().execute(
        "UPDATE federation_executor_outcomes SET first_seen_at='2000-01-01T00:00:00.000Z'", []).unwrap();
    make_due(&fixture);
    fixture.central.poll_federation_executor_outcomes().await;
    assert_eq!(outcome_row(&fixture).unwrap().attempts, 1);
    assert!(cause(&fixture).ends_with("; no longer polled"), "{}", cause(&fixture));
    let _ = stop.send(());
    server.await.unwrap();
}

/// The mail's ACK can precede this wake's own acceptance: the receiver keeps
/// the queued wake, so central keeps polling it and later learns it was woken.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_outcome_keeps_polling_a_wake_still_queued_after_its_mail_is_processed() {
    let mut fixture = Fixture::new();
    acknowledge_all_fences(&mut fixture.satellite);
    let _input = root(&fixture, "outcome-processed-queued", true);
    let (stop, server) = attach_loopback(&fixture).await;
    dispatch_and_activate(&fixture).await;
    let operation = fixture.operation();
    let host = operation.target_host_id.clone();
    fixture.satellite.mailbox_store.record_federation_executor_evidence(&host, &operation,
        FederationExecutorEvidence::Processed { kind: FederationExecutorProcessedKind::MailboxAck,
            evidence: "validated-central-ack".into() }, || Ok(())).unwrap();
    assert_eq!(fixture.inbox().unwrap().stage, "processed");
    fixture.central.poll_federation_executor_outcomes().await;
    let row = outcome_row(&fixture).unwrap();
    assert_eq!(row.last_outcome, Some(FederationExecutorOutcome::HeldBehindPausedQueue),
        "a wake still queued after the ACK is answered by its queue");
    assert!(!row.terminal, "and polling continues");

    // The queued wake later runs and its runtime accepts it.
    let attempt = fixture.inbox().unwrap().delivery_attempt_id.expect("the queued wake's attempt");
    for evidence in [FederationExecutorEvidence::EffectAttempt { delivery_attempt_id: attempt.clone() },
        FederationExecutorEvidence::RuntimeAccepted { delivery_attempt_id: attempt, acknowledgement: "runtime-ack".into() }]
    {
        fixture.satellite.mailbox_store.record_federation_executor_evidence(&host, &operation, evidence, || Ok(())).unwrap();
    }
    make_due(&fixture);
    fixture.central.poll_federation_executor_outcomes().await;
    let row = outcome_row(&fixture).unwrap();
    assert_eq!((row.last_outcome, row.terminal), (Some(FederationExecutorOutcome::Woken), true));
    assert!(cause(&fixture).starts_with("remote session woken at "), "{}", cause(&fixture));
    let _ = stop.send(());
    server.await.unwrap();
}

/// The mail's ACK can precede even the wake's queue association: activation
/// still queues the received wake, so central keeps polling it until woken.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_outcome_keeps_polling_a_wake_whose_mail_is_processed_before_it_is_queued() {
    let mut fixture = Fixture::new();
    acknowledge_all_fences(&mut fixture.satellite);
    let _input = root(&fixture, "outcome-processed-unqueued", true);
    let (stop, server) = attach_loopback(&fixture).await;
    let request = fixture.central.mailbox_store.reserve_federation_executor_wakes().unwrap()
        .into_iter().next().expect("the appended wake is reserved");
    fixture.central.dispatch_federation_executor_wake(request).await.unwrap();
    let operation = fixture.operation();
    let host = operation.target_host_id.clone();
    fixture.satellite.mailbox_store.record_federation_executor_evidence(&host, &operation,
        FederationExecutorEvidence::Processed { kind: FederationExecutorProcessedKind::MailboxAck,
            evidence: "validated-central-ack".into() }, || Ok(())).unwrap();
    let journal = fixture.inbox().unwrap();
    assert_eq!((journal.stage.as_str(), journal.queue_entry_id.is_none()), ("processed", true));
    fixture.central.poll_federation_executor_outcomes().await;
    let row = outcome_row(&fixture).unwrap();
    assert_eq!((row.last_outcome, row.terminal), (Some(FederationExecutorOutcome::Received), false),
        "an ACK before queue association is not final");

    let mut worker = FederationExecutorActivationWorker::default();
    worker.tick(&fixture.satellite, true).await;
    worker.shutdown(&fixture.satellite).await;
    assert!(fixture.inbox().unwrap().queue_entry_id.is_some(), "activation still queues the wake");
    make_due(&fixture);
    fixture.central.poll_federation_executor_outcomes().await;
    let row = outcome_row(&fixture).unwrap();
    assert_eq!((row.last_outcome, row.terminal), (Some(FederationExecutorOutcome::HeldBehindPausedQueue), false));

    let attempt = fixture.inbox().unwrap().delivery_attempt_id.expect("the queued wake's attempt");
    for evidence in [FederationExecutorEvidence::EffectAttempt { delivery_attempt_id: attempt.clone() },
        FederationExecutorEvidence::RuntimeAccepted { delivery_attempt_id: attempt, acknowledgement: "runtime-ack".into() }]
    {
        fixture.satellite.mailbox_store.record_federation_executor_evidence(&host, &operation, evidence, || Ok(())).unwrap();
    }
    make_due(&fixture);
    fixture.central.poll_federation_executor_outcomes().await;
    let row = outcome_row(&fixture).unwrap();
    assert_eq!((row.last_outcome, row.terminal), (Some(FederationExecutorOutcome::Woken), true));
    let _ = stop.send(());
    server.await.unwrap();
}

/// A receiver that stalls its outcome answer holds only the polling pass:
/// the drain owner's wake dispatch finishes while that poll is still held.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_outcome_stalled_poll_does_not_hold_wake_dispatch() {
    let mut fixture = Fixture::new();
    acknowledge_all_fences(&mut fixture.satellite);
    let (stop, server) = attach_loopback(&fixture).await;
    let request = fixture.central.mailbox_store.reserve_federation_executor_wakes().unwrap()
        .into_iter().next().expect("the appended wake is reserved");
    fixture.central.dispatch_federation_executor_wake(request).await.unwrap();
    assert!(outcome_row(&fixture).is_some(), "the settled wake is due for its first poll");
    let (entered, held) = mpsc::channel();
    let release = Arc::new(tokio::sync::Notify::new());
    *fixture.central.federation_executor_wake_signal.outcome_poll_hold.lock().unwrap() =
        Some((entered, release.clone()));

    // First tick: the poll starts and is held; nothing is left to dispatch.
    let mut drain = FederationExecutorWakeDrain::default();
    drain.tick(&fixture.central);
    let done = drain.pending.take().expect("one owned drain pass");
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, done).await
        .expect("an empty dispatch pass must not wait for a stalled outcome poll").unwrap();
    phase_sync::receive(&held, "the outcome poll is held at its receiver call");

    // With that poll still held, a new wake is appended and dispatched.
    append_wake(&fixture, "Second sender", "second");
    drain.tick(&fixture.central);
    let done = drain.pending.take().expect("a second owned drain pass");
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, done).await
        .expect("wake dispatch must not wait for a stalled outcome poll").unwrap();
    let pending: u64 = fixture.central.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM federation_mailbox_wake_obligations WHERE state='pending'", [],
        |row| row.get(0)).unwrap();
    assert_eq!(pending, 0, "the second wake was dispatched while the poll was held");
    release.notify_one();
    drain.shutdown().await;
    let _ = stop.send(());
    server.await.unwrap();
}

/// A receiver whose outcome read is still blocked keeps its RPC slot after
/// the HTTP handler is cancelled, as the listener's deadline cancels it, so
/// abandoned reads cannot pile up past the peer's bound.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_outcome_cancelled_receiver_read_keeps_its_rpc_slot_until_it_ends() {
    let fixture = Fixture::new();
    let operation = fixture.operation();
    let request = FederationExecutorOutcomeRequest { protocol_version: FEDERATION_PROTOCOL_VERSION,
        payload_digest: operation.digest(), operation };
    let slots = fixture.channel.bound.permits.clone();
    let all = slots.available_permits();
    let peer = FederationAuthenticatedPeer { lease: Some(fixture.channel.current_lease().unwrap()),
        mailbox_permit: Some(Arc::new(fixture.channel.bound.try_acquire().unwrap())) };
    // The receiver's state is held, so its blocking read cannot finish. Only
    // observe while it is held: an assertion there would poison the state.
    let (answered, free_while_held) = {
        let _held = fixture.satellite.inner.lock().unwrap();
        let cancelled = tokio::time::timeout(Duration::from_millis(50),
            serve_federation_executor_outcome(axum::Extension(peer), Json(request))).await;
        (cancelled.is_ok(), slots.available_permits())
    };
    assert!(!answered, "the held read cannot answer");
    assert_eq!(free_while_held, all - 1, "the cancelled read still holds its slot");
    let started = std::time::Instant::now();
    while slots.available_permits() != all {
        assert!(started.elapsed() < phase_sync::DEADLOCK_GUARD, "the finished read releases its slot");
        tokio::task::yield_now().await;
    }
}

/// A receiver whose outcome answer misses its handler deadline refuses the
/// query retryably over the authenticated pair: the link stays current on both
/// sides, the poll counts only an attempt, and the same link answers next.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_outcome_answer_past_its_handler_deadline_keeps_the_link() {
    let mut fixture = Fixture::new();
    acknowledge_all_fences(&mut fixture.satellite);
    let _input = root(&fixture, "outcome-slow-answer", true);
    let (stop, server) = attach_loopback(&fixture).await;
    dispatch_and_activate(&fixture).await;
    let (entered, held) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    *fixture.satellite.federation_executor_wake_signal.outcome_answer_hold.lock().unwrap() = Some((entered, released));
    let central = fixture.central.clone();
    let polling = tokio::spawn(async move { central.poll_federation_executor_outcomes().await });
    phase_sync::receive(&held, "the receiver's outcome answer is held past its handler deadline");
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, polling).await.unwrap().unwrap();
    drop(release);
    let row = outcome_row(&fixture).unwrap();
    assert_eq!((row.attempts, row.last_outcome), (1, None), "the refused poll counts only an attempt");
    // A fenced link loses its security context.
    assert!(fixture.channel.security_context().is_ok(), "the receiver's link is not fenced");
    assert!(fixture.outbound.security_context().is_ok(), "central's link is not fenced");
    make_due(&fixture);
    fixture.central.poll_federation_executor_outcomes().await;
    assert_eq!(outcome_row(&fixture).unwrap().last_outcome, Some(FederationExecutorOutcome::HeldBehindPausedQueue),
        "the same link answers the next poll");
    let _ = stop.send(());
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_outcome_query_with_a_wrong_digest_is_refused() {
    let fixture = Fixture::new();
    let operation = fixture.operation();
    let request = FederationExecutorOutcomeRequest { protocol_version: FEDERATION_PROTOCOL_VERSION,
        payload_digest: "0".repeat(64), operation };
    let router = federation_router(fixture.channel.clone(),
        Arc::new({ let check = fixture.channel.clone(); move || check.current_authority().map(|_| ()) }));
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    let context = fixture.outbound.security_context().unwrap();
    let client = FederationRpcClient::new(1, fixture.outbound.clone(),
        Arc::new({ let check = fixture.outbound.clone(); move || check.current_authority().map(|_| ()) })).unwrap();
    let counter = client.allocate_authenticated_counter(&context).await.unwrap();
    let signed = federation_control_fixture::signed(&fixture.outbound, counter, "POST",
        FEDERATION_EXECUTOR_OUTCOME_ROUTE, headers, &serde_json::to_vec(&request).unwrap());
    let response = router.oneshot(signed).await.unwrap();
    context.settle(counter);
    assert_eq!(response.status(), StatusCode::CONFLICT, "a digest that is not the operation's is refused");
}
