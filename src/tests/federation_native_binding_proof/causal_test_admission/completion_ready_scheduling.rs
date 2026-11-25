// Real authenticated routing with controlled worker overlap and original
// pending settlement. These controls add no completion or persistence hooks.
use super::*;

struct Handles(Vec<AppState>);

impl Drop for Handles {
    fn drop(&mut self) {
        for app in &self.0 {
            app.federation_peer_handles.lock().unwrap().clear();
        }
    }
}

fn ready(fixture: &Fixture, selector: &CausalTestCompletionSelector, run: &str) {
    let reply = fixture
        .central
        .execute_mailbox_relay(
            MailboxRelayOperation::CausalTestCompletionReady {
                hint: CausalTestCompletionReadyHint {
                    requester: selector.payload.requester.clone(),
                    runner: selector.runner().unwrap().clone(),
                    request_send_key: selector.payload.request_send_key.clone(),
                    canonical_message_id: selector.canonical_message_id.clone(),
                    run_id: run.to_owned(),
                },
            },
            &fixture.channel.current_lease().unwrap(),
            &fixture.satellite_channel.snapshot.local,
        )
        .unwrap();
    assert_eq!(reply["status"], "scheduled");
}

async fn join_worker(recovery: &mut CausalTestCompletionRecovery) {
    if let Some(task) = recovery.task.take() {
        tokio::time::timeout(phase_sync::DEADLOCK_GUARD, task)
            .await
            .expect("the controlled recovery worker finishes")
            .expect("the recovery worker does not panic");
    }
}

async fn tick(app: &AppState, fallback: bool) {
    let mut recovery = CausalTestCompletionRecovery::default();
    recovery.tick(app, fallback).await;
    join_worker(&mut recovery).await;
    recovery.shutdown().await;
}

async fn capture_hints(fixture: &Fixture) -> (DomainServer, Handles, Arc<Mutex<Vec<Value>>>) {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let capture = observed.clone();
    let channel = fixture.channel.clone();
    let checked = channel.clone();
    let router = federation_router(
        channel,
        Arc::new(move || checked.current_authority().map(|_| ())),
    );
    let router = router.layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| {
            let capture = capture.clone();
            async move {
                let (parts, body) = request.into_parts();
                let bytes = axum::body::to_bytes(body, FEDERATION_MAILBOX_REQUEST_BYTES)
                    .await
                    .unwrap();
                if parts.uri.path() == MAILBOX_RELAY_RPC_ROUTE {
                    if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                        if value["operation"]["operation"] == "causalTestCompletionReady" {
                            capture.lock().unwrap().push(value["operation"].clone());
                        }
                    }
                }
                next.run(axum::extract::Request::from_parts(parts, Body::from(bytes)))
                    .await
            }
        },
    ));
    let server = DomainServer::start(router).await;
    let channel = fixture.satellite_channel.clone();
    let checked = channel.clone();
    let reverse = FederationRpcClient::new(
        server.port,
        channel,
        Arc::new(move || checked.current_authority().map(|_| ())),
    )
    .unwrap();
    for (app, channel, rpc) in [
        (&fixture.central, &fixture.channel, fixture.rpc.clone()),
        (&fixture.satellite, &fixture.satellite_channel, reverse),
    ] {
        app.federation_peer_handles.lock().unwrap().insert(
            channel.snapshot.peer.host_id.clone(),
            Arc::new(FederationPeerHandle {
                channel: channel.clone(),
                rpc,
            }),
        );
    }
    (
        server,
        Handles(vec![fixture.central.clone(), fixture.satellite.clone()]),
        observed,
    )
}

async fn block_first_completion_read(
    fixture: &Fixture,
) -> (
    DomainServer,
    Arc<tokio::sync::Notify>,
    Arc<tokio::sync::Semaphore>,
) {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let first = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let channel = fixture.satellite_channel.clone();
    let checked = channel.clone();
    let router = federation_router(
        channel,
        Arc::new(move || checked.current_authority().map(|_| ())),
    );
    let signal = entered.clone();
    let gate = release.clone();
    let router = router.layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| {
            let signal = signal.clone();
            let gate = gate.clone();
            let first = first.clone();
            async move {
                let (parts, body) = request.into_parts();
                let bytes = axum::body::to_bytes(body, FEDERATION_MAILBOX_REQUEST_BYTES)
                    .await
                    .unwrap();
                let completion_read = parts.uri.path() == MAILBOX_RELAY_RPC_ROUTE
                    && serde_json::from_slice::<Value>(&bytes).is_ok_and(|value| {
                        value["operation"]["operation"] == "readCausalTestCompletion"
                    });
                if completion_read && first.swap(false, Ordering::SeqCst) {
                    signal.notify_one();
                    gate.acquire().await.unwrap().forget();
                }
                next.run(axum::extract::Request::from_parts(parts, Body::from(bytes)))
                    .await
            }
        },
    ));
    let server = DomainServer::start(router).await;
    let channel = fixture.channel.clone();
    let checked = channel.clone();
    let rpc = FederationRpcClient::new(
        server.port,
        channel.clone(),
        Arc::new(move || checked.current_authority().map(|_| ())),
    )
    .unwrap();
    fixture
        .central
        .federation_peer_handles
        .lock()
        .unwrap()
        .insert(
            channel.snapshot.peer.host_id.clone(),
            Arc::new(FederationPeerHandle { channel, rpc }),
        );
    (server, entered, release)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn periodic_fallback_survives_a_busy_hinted_worker_and_recovers_an_unhinted_original() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut reverse, handles, _) = capture_hints(&fixture).await;
    let (input, transport, scope) = destination(fixture.central.clone(), &fixture.native);
    let (running, _, running_selector) = original(&fixture, "busy-running", input.clone()).await;
    let (lost, directory, lost_selector) = original(&fixture, "busy-lost", input.clone()).await;
    finish(
        &fixture.satellite,
        &lost,
        &directory,
        native_test_completion::Verdict::Pass,
        source(),
        source(),
    );
    let (queued, _, queued_selector) = original(&fixture, "busy-queued", input).await;
    let (mut blocked, entered, release) = block_first_completion_read(&fixture).await;
    ready(&fixture, &running_selector, &running);
    let mut recovery = CausalTestCompletionRecovery::default();
    recovery.tick(&fixture.central, false).await;
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, entered.notified())
        .await
        .expect("actual authenticated read reaches the gate");
    assert!(
        recovery
            .task
            .as_ref()
            .is_some_and(|task| !task.is_finished())
    );
    recovery.tick(&fixture.central, true).await;
    ready(&fixture, &queued_selector, &queued);
    assert!(
        writes(&transport).is_empty(),
        "blocked hint is no settlement authority"
    );
    let accepted = receipt(None);
    script(
        &transport,
        &scope,
        &fixture.native,
        Ok(accepted.clone()),
        true,
    );
    release.add_permits(1);
    join_worker(&mut recovery).await;
    recovery.tick(&fixture.central, false).await;
    join_worker(&mut recovery).await;
    recovery.shutdown().await;
    let saved = fixture
        .central
        .mailbox_store
        .causal_test_settlement(&lost_selector, Some(&scope))
        .unwrap();
    drop(handles);
    blocked.finish().await;
    reverse.finish().await;
    let saved = saved.expect(
        "a periodic fallback owed during the busy worker must service the unhinted original",
    );
    assert!(saved.complete());
    assert_eq!(saved.completion.run_id, lost);
    assert_eq!(saved.admissions[0].receipt, Some(accepted));
    assert_eq!(writes(&transport).len(), 1);
    let inner = fixture.central.inner.lock().unwrap();
    assert!(inner.test_run_registrations.is_empty());
    assert!(inner.remote_test_run_facts.is_empty());
    drop(inner);
    fixture.finish().await;
}

fn decision_count(app: &AppState) -> u64 {
    app.federation_executor_wake_signal
        .causal_ready
        .lock()
        .unwrap()
        .diagnostic()["decisions"]
        .as_object()
        .unwrap()
        .values()
        .map(|value| value.as_u64().unwrap())
        .sum()
}

fn no_requester_native_authority(fixture: &Fixture) {
    let inner = fixture.central.inner.lock().unwrap();
    assert!(inner.test_run_registrations.is_empty());
    assert!(inner.remote_test_run_facts.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unavailable_ready_notification_attempts_end_without_losing_original_fallback() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) = super::super::causal_test_completion::connect(&fixture).await;
    let (input, transport, scope) = destination(fixture.central.clone(), &fixture.native);
    let (run, directory, selector) = original(&fixture, "unavailable-ready", input).await;
    finish(
        &fixture.satellite,
        &run,
        &directory,
        native_test_completion::Verdict::Pass,
        source(),
        source(),
    );
    let registration = fixture
        .satellite
        .inner
        .lock()
        .unwrap()
        .test_run_registrations[&run]
        .clone();
    assert!(
        fixture
            .satellite
            .read_durable_native_test_run(&registration)
            .unwrap()
            .unwrap()
            .completion
            .is_some()
    );
    fixture
        .satellite
        .federation_peer_handles
        .lock()
        .unwrap()
        .clear();
    let before = decision_count(&fixture.satellite);
    for _ in 0..32 {
        fixture.satellite.notify_causal_test_completions().await;
    }
    let bounded = decision_count(&fixture.satellite);
    assert!(
        bounded > before,
        "the real unavailable notification was attempted"
    );
    for _ in 0..32 {
        fixture.satellite.notify_causal_test_completions().await;
    }
    let later = decision_count(&fixture.satellite);
    // The requester still owns the durable original after notification expiry.
    let accepted = receipt(None);
    script(
        &transport,
        &scope,
        &fixture.native,
        Ok(accepted.clone()),
        true,
    );
    tick(&fixture.central, true).await;
    let saved = fixture
        .central
        .mailbox_store
        .causal_test_settlement(&selector, Some(&scope))
        .unwrap()
        .unwrap();
    assert!(saved.complete());
    assert_eq!(saved.completion.run_id, run);
    assert_eq!(saved.admissions[0].receipt, Some(accepted));
    assert_eq!(writes(&transport).len(), 1);
    no_requester_native_authority(&fixture);
    drop(handles);
    server.finish().await;
    assert_eq!(
        later, bounded,
        "expired notification work must stop revisiting and logging retained history"
    );
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settled_history_beyond_the_old_cache_does_not_repeat_confirmed_ready_hints() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles, observed) = capture_hints(&fixture).await;
    let (input, transport, scope) = destination(fixture.central.clone(), &fixture.native);
    let mut keys = BTreeSet::new();
    // Each original uses real registration, provider finish and requester
    // settlement. Sending as it finishes keeps transient queue pressure low.
    for index in 0..65 {
        let key = format!("settled-history-{index:02}");
        let (run, directory, selector) = original(&fixture, &key, input.clone()).await;
        finish(
            &fixture.satellite,
            &run,
            &directory,
            native_test_completion::Verdict::Pass,
            source(),
            source(),
        );
        fixture.satellite.notify_causal_test_completions().await;
        script(&transport, &scope, &fixture.native, Ok(receipt(None)), true);
        tick(&fixture.central, true).await;
        let saved = fixture
            .central
            .mailbox_store
            .causal_test_settlement(&selector, Some(&scope))
            .unwrap()
            .unwrap();
        assert!(saved.complete());
        assert_eq!(saved.completion.run_id, run);
        keys.insert(selector.payload.request_send_key.clone());
    }
    for _ in 0..65 {
        fixture.satellite.notify_causal_test_completions().await;
    }
    let hints = observed.lock().unwrap().clone();
    let delivered: BTreeSet<_> = hints
        .iter()
        .map(|hint| hint["hint"]["requestSendKey"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        delivered, keys,
        "every genuine original was observed through authenticated notification"
    );
    assert_eq!(writes(&transport).len(), 65);
    no_requester_native_authority(&fixture);
    drop(handles);
    server.finish().await;
    assert_eq!(
        hints.len(),
        65,
        "settled originals must stay retired after more than 64 confirmations"
    );
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refused_pending_writes_for_one_hinted_row_keep_the_other_drained_hints() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) = super::super::causal_test_completion::connect(&fixture).await;
    let (input, _, _) = destination(fixture.central.clone(), &fixture.native);
    let mut selectors = Vec::new();
    // Drained hints are ordered by send key, so the refused original, keyed
    // first, is pulled before the other: its failed writes must not stop it.
    for key in ["ready-write-a-refused", "ready-write-b-kept"] {
        let (run, directory, selector) = original(&fixture, key, input.clone()).await;
        // The refused original has no completion yet, so its pull must also
        // record a deferral on the row whose writes are refused.
        if key == "ready-write-b-kept" {
            finish(
                &fixture.satellite,
                &run,
                &directory,
                native_test_completion::Verdict::Pass,
                source(),
                source(),
            );
        }
        // A reason the ready plan disproves, so the pull first rewrites it.
        fixture
            .central
            .mailbox_store
            .note_causal_pending_for_selector(&selector, true, CausalPendingReason::AwaitingReceipt)
            .unwrap();
        let hint = CausalTestCompletionReadyHint {
            requester: selector.payload.requester.clone(),
            runner: selector.runner().unwrap().clone(),
            request_send_key: selector.payload.request_send_key.clone(),
            canonical_message_id: selector.canonical_message_id.clone(),
            run_id: run,
        };
        fixture
            .central
            .federation_executor_wake_signal
            .causal_ready
            .lock()
            .unwrap()
            .pending
            .insert(hint.key(), hint);
        selectors.push(selector);
    }
    // A disposable store refuses pending writes for the first original only.
    fixture
        .central
        .mailbox_store
        .connection()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER fixture_refuse_ready_write
             BEFORE UPDATE OF test_request_pending_json ON mailbox_relay_intents
             WHEN OLD.idempotency_key='ready-write-a-refused'
             BEGIN SELECT RAISE(ABORT, 'fixture refuses this pending write'); END",
        )
        .unwrap();
    let deliveries = tokio::time::timeout(
        phase_sync::DEADLOCK_GUARD,
        fixture.central.read_ready_causal_test_completions(false),
    )
    .await
    .expect("the hinted batch finishes");
    let decisions = fixture
        .central
        .federation_executor_wake_signal
        .causal_ready
        .lock()
        .unwrap()
        .diagnostic()["decisions"]
        .clone();
    fixture
        .central
        .mailbox_store
        .connection()
        .unwrap()
        .execute_batch("DROP TRIGGER fixture_refuse_ready_write")
        .unwrap();
    drop(handles);
    server.finish().await;
    fixture.finish().await;

    let delivered: Vec<_> = deliveries
        .expect("refused writes for one row do not fail the hinted batch")
        .into_iter()
        .map(|delivery| delivery.selector)
        .collect();
    assert_eq!(delivered, vec![selectors[1].clone()]);
    assert_eq!(decisions["ready_pending_write_failed"], 1);
    assert_eq!(decisions["pending_write_failed"], 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sent_read_past_its_deadline_is_an_origin_timeout() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut reverse, handles, _) = capture_hints(&fixture).await;
    let (input, _, _) = destination(fixture.central.clone(), &fixture.native);
    original(&fixture, "sent-read-deadline", input).await;
    let (mut blocked, entered, release) = block_first_completion_read(&fixture).await;
    let plans = fixture
        .central
        .mailbox_store
        .causal_test_completion_plans()
        .unwrap();
    assert_eq!(plans.len(), 1);
    let central = fixture.central.clone();
    let read = tokio::spawn(async move {
        central
            .read_causal_test_completion_plans(plans.into_iter().map(|plan| (plan, None)).collect())
            .await
    });
    // The read was sent: the origin's gate holds it. Then the link's
    // scripted clock moves past the read's deadline.
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, entered.notified())
        .await
        .expect("the read reached the origin");
    fixture
        .channel
        .control
        .clock
        .advance(Duration::from_secs(6));
    let deliveries = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, read)
        .await
        .expect("the read's scripted deadline ends it")
        .unwrap()
        .unwrap();
    let diagnostic = fixture
        .central
        .mailbox_store
        .causal_pending_diagnostics()
        .unwrap();
    release.add_permits(1);
    drop(handles);
    blocked.finish().await;
    reverse.finish().await;
    fixture.finish().await;

    assert!(deliveries.is_empty());
    let state = &diagnostic["records"][0]["state"];
    assert_eq!(state["reason"], "origin_timeout");
    assert_eq!(state["releaseCondition"], "origin_read_succeeds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hinted_pull_with_a_saved_receipt_shows_awaiting_completion_while_it_reads() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut reverse, handles, _) = capture_hints(&fixture).await;
    let (input, _, _) = destination(fixture.central.clone(), &fixture.native);
    let (run, _, selector) = original(&fixture, "hinted-receipt-transition", input).await;
    // The row was noted while its send was not yet confirmed; the receipt is
    // saved now, and only the hint starts the next pull.
    fixture
        .central
        .mailbox_store
        .note_causal_pending_for_selector(&selector, true, CausalPendingReason::AwaitingReceipt)
        .unwrap();
    let (mut blocked, entered, release) = block_first_completion_read(&fixture).await;
    ready(&fixture, &selector, &run);
    let mut recovery = CausalTestCompletionRecovery::default();
    recovery.tick(&fixture.central, false).await;
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, entered.notified())
        .await
        .expect("the hinted authenticated read reaches the gate");
    // While the origin read is held, the row names the pull it waits for.
    let during_read = fixture
        .central
        .mailbox_store
        .causal_pending_diagnostics()
        .unwrap();
    release.add_permits(1);
    join_worker(&mut recovery).await;
    recovery.shutdown().await;
    drop(handles);
    blocked.finish().await;
    reverse.finish().await;
    fixture.finish().await;

    let state = &during_read["records"][0]["state"];
    assert_eq!(state["reason"], "awaiting_completion");
    assert_eq!(state["releaseCondition"], "origin_completion_available");
}
