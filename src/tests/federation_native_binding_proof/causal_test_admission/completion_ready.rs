// Completion-ready scheduling through the existing authenticated relay and
// original pending owners. New behavioral tests, not split from production.
// A hint carries no native result or destination verification authority.
use super::*;

struct Handles(Vec<AppState>);
impl Drop for Handles {
    fn drop(&mut self) {
        for app in &self.0 {
            app.federation_peer_handles.lock().unwrap().clear();
        }
    }
}

fn hint(selector: &CausalTestCompletionSelector, run: &str) -> Value {
    json!({"operation":"causalTestCompletionReady", "hint": {
        "requester": selector.payload.requester,
        "runner": selector.runner().unwrap(),
        "requestSendKey": selector.payload.request_send_key,
        "canonicalMessageId": selector.canonical_message_id,
        "runId":run
    }})
}

// The dispatch is the same one called after HTTP authentication. Its lease
// comes from the fixture's actual mutual proof, not an installed fake lease.
fn receive(
    fixture: &Fixture,
    value: Value,
    authenticated: &FederationHostIdentity,
) -> Result<Value, ApiError> {
    let operation = serde_json::from_value::<MailboxRelayOperation>(value)
        .map_err(|error| ApiError::bad_request(format!("relay operation refused: {error}")))?;
    fixture.central.execute_mailbox_relay(
        operation,
        &fixture.channel.current_lease()?,
        authenticated,
    )
}

async fn tick(app: &AppState, fallback: bool) {
    let mut recovery = CausalTestCompletionRecovery::default();
    recovery.tick(app, fallback).await;
    if let Some(task) = recovery.task.take() {
        task.await.expect("the actual recovery task joins");
    }
    recovery.shutdown().await;
}

fn pending_attempts(fixture: &Fixture, key: &str) -> u64 {
    let encoded: String = fixture.central.mailbox_store.connection().unwrap().query_row(
        "SELECT test_request_pending_json FROM mailbox_relay_intents WHERE owner_session_id=?1 AND idempotency_key=?2",
        rusqlite::params![fixture.native,key], |row| row.get(0)).unwrap();
    CausalPendingState::decode(&encoded).unwrap().attempts
}

fn no_destination_authority(fixture: &Fixture, transport: &ScriptedEngramControlTransport) {
    assert!(writes(transport).is_empty());
    let inner = fixture.central.inner.lock().unwrap();
    assert!(inner.test_run_registrations.is_empty());
    assert!(inner.remote_test_run_facts.is_empty());
}

// Observe actual HTTP bodies, forwarding every byte to the real authenticated
// router. No success response, authentication header or completion is fabricated.
async fn observed_reverse(fixture: &Fixture) -> (DomainServer, Handles, Arc<Mutex<Vec<Value>>>) {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let capture = observed.clone();
    let channel = fixture.channel.clone();
    let check = channel.clone();
    let router = federation_router(
        channel,
        Arc::new(move || check.current_authority().map(|_| ())),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_completion_sends_original_key_hint_over_authenticated_relay_without_credit() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles, observed) = observed_reverse(&fixture).await;
    let (input, transport, _) = destination(fixture.central.clone(), &fixture.native);
    let (run, directory, selector) = original(&fixture, "hint-durable", input).await;
    tick(&fixture.satellite, true).await;
    assert!(
        observed.lock().unwrap().is_empty(),
        "running is not durable completion"
    );
    finish(
        &fixture.satellite,
        &run,
        &directory,
        native_test_completion::Verdict::Pass,
        source(),
        source(),
    );
    let retained = fixture
        .satellite
        .inner
        .lock()
        .unwrap()
        .test_run_registrations[&run]
        .clone();
    assert!(
        fixture
            .satellite
            .read_durable_native_test_run(&retained)
            .unwrap()
            .unwrap()
            .completion
            .is_some(),
        "real durable registration and provider precede notification"
    );
    tick(&fixture.satellite, true).await;
    let hints = observed.lock().unwrap().clone();
    assert!(
        !hints.is_empty(),
        "durable causal completion must notify the original requester"
    );
    assert!(hints.iter().all(|value| value == &hint(&selector, &run)));
    no_destination_authority(&fixture, &transport);
    drop(handles);
    server.finish().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_native_persistence_emits_no_ready_hint_and_repaired_durability_can_notify() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let mut fixture = Fixture::production_shape().await;
    let (mut server, handles, observed) = observed_reverse(&fixture).await;
    let (input, transport, _) = destination(fixture.central.clone(), &fixture.native);
    let (run, directory, selector) = original(&fixture, "hint-persist-failure", input).await;
    let original_path = fixture.satellite.persistence_path.clone();
    let invalid = original_path.with_extension("not-a-state-file");
    fs::create_dir(&invalid).unwrap();
    fixture.satellite.persistence_path = Arc::new(invalid);
    fixture
        .satellite
        .persist_worker_alive
        .store(false, Ordering::Release);
    let (sender, receiver) = mpsc::channel();
    drop(receiver);
    fixture.satellite.persist_tx = sender;
    finish(
        &fixture.satellite,
        &run,
        &directory,
        native_test_completion::Verdict::Pass,
        source(),
        source(),
    );
    tick(&fixture.satellite, true).await;
    assert!(
        observed.lock().unwrap().is_empty(),
        "failed persistence must not announce success"
    );
    no_destination_authority(&fixture, &transport);
    fixture.satellite.persistence_path = original_path;
    finish(
        &fixture.satellite,
        &run,
        &directory,
        native_test_completion::Verdict::Pass,
        source(),
        source(),
    );
    {
        let inner = fixture.satellite.inner.lock().unwrap();
        fixture.satellite.persist_internal_locked(&inner).unwrap();
    }
    let retained = fixture
        .satellite
        .inner
        .lock()
        .unwrap()
        .test_run_registrations[&run]
        .clone();
    assert!(
        fixture
            .satellite
            .read_durable_native_test_run(&retained)
            .unwrap()
            .unwrap()
            .completion
            .is_some(),
        "the repaired original must be durable before expecting its hint"
    );
    tick(&fixture.satellite, true).await;
    assert!(
        observed
            .lock()
            .unwrap()
            .iter()
            .any(|v| v == &hint(&selector, &run)),
        "retry after proven durability must still notify the original key"
    );
    drop(handles);
    server.finish().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ready_hint_targets_only_original_pending_key_then_uses_authenticated_pull_and_settlement()
{
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) = super::super::causal_test_completion::connect(&fixture).await;
    let (input, transport, scope) = destination(fixture.central.clone(), &fixture.native);
    let mut others = Vec::new();
    for index in 0..CAUSAL_COMPLETION_BATCH + 1 {
        let key = format!("hint-other-{index}");
        original(&fixture, &key, input.clone()).await;
        others.push(key);
    }
    let (run, directory, selector) = original(&fixture, "hint-target", input).await;
    finish(
        &fixture.satellite,
        &run,
        &directory,
        native_test_completion::Verdict::Pass,
        source(),
        source(),
    );
    let before: Vec<_> = others
        .iter()
        .map(|key| pending_attempts(&fixture, key))
        .collect();
    let reply = receive(
        &fixture,
        hint(&selector, &run),
        &fixture.satellite_channel.snapshot.local,
    )
    .expect("a current original ready hint is accepted by the relay owner");
    assert_eq!(reply["status"], "scheduled");
    no_destination_authority(&fixture, &transport);
    let accepted = receipt(None);
    script(
        &transport,
        &scope,
        &fixture.native,
        Ok(accepted.clone()),
        true,
    );
    tick(&fixture.central, false).await;
    let saved = fixture
        .central
        .mailbox_store
        .causal_test_settlement(&selector, Some(&scope))
        .unwrap()
        .expect("one targeted tick reaches the original pending key");
    assert!(saved.complete());
    assert_eq!(saved.completion.run_id, run);
    assert_eq!(saved.admissions[0].receipt, Some(accepted));
    assert_eq!(writes(&transport).len(), 1);
    assert_eq!(
        others
            .iter()
            .map(|key| pending_attempts(&fixture, key))
            .collect::<Vec<_>>(),
        before,
        "a hint schedules only its key, not an unrelated pending sweep"
    );
    let duplicate = receive(
        &fixture,
        hint(&selector, &run),
        &fixture.satellite_channel.snapshot.local,
    )
    .expect("duplicate hints have a readable idempotent outcome");
    assert_eq!(duplicate["status"], "ignored");
    assert!(
        duplicate["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty() && reason.len() <= 512)
    );
    assert_eq!(writes(&transport).len(), 1);
    drop(handles);
    server.finish().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forged_and_stale_hints_refuse_without_scheduling_or_creating_authority() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) = super::super::causal_test_completion::connect(&fixture).await;
    let (input, transport, _) = destination(fixture.central.clone(), &fixture.native);
    let (run, _, selector) = original(&fixture, "hint-refusals", input).await;
    // Positive admission first proves later negative cases reach the new owner,
    // rather than passing merely because an older binary lacks the operation.
    assert_eq!(
        receive(
            &fixture,
            hint(&selector, &run),
            &fixture.satellite_channel.snapshot.local
        )
        .unwrap()["status"],
        "scheduled"
    );
    let generation = fixture
        .central
        .federation_executor_wake_signal
        .generation
        .load(Ordering::SeqCst);
    for fault in [
        "authenticated-origin",
        "runner",
        "requester",
        "canonical",
        "send-key",
    ] {
        let mut altered = hint(&selector, &run);
        let mut origin = fixture.satellite_channel.snapshot.local.clone();
        match fault {
            "authenticated-origin" => origin.host_id = Uuid::new_v4().to_string(),
            "runner" => {
                altered["hint"]["runner"]["nativeCreationId"] = json!(Uuid::new_v4().to_string())
            }
            "requester" => {
                altered["hint"]["requester"]["storeId"] = json!(Uuid::new_v4().to_string())
            }
            "canonical" => {
                altered["hint"]["canonicalMessageId"] =
                    json!(format!("mailbox-message-{}", Uuid::new_v4()))
            }
            _ => altered["hint"]["requestSendKey"] = json!("not-the-original-key"),
        }
        let result = receive(&fixture, altered, &origin);
        let reason = match result {
            Err(error) => error.message,
            Ok(reply) => {
                assert_eq!(reply["status"], "ignored", "{fault}");
                reply["reason"].as_str().unwrap().to_owned()
            }
        };
        assert!(
            !reason.is_empty() && reason.len() <= 512,
            "{fault}: {reason}"
        );
        assert_eq!(
            fixture
                .central
                .federation_executor_wake_signal
                .generation
                .load(Ordering::SeqCst),
            generation,
            "{fault}"
        );
        no_destination_authority(&fixture, &transport);
    }
    drop(fixture.attested.lock().unwrap().take());
    let retired_generation = fixture
        .central
        .federation_executor_wake_signal
        .generation
        .load(Ordering::SeqCst);
    let error = receive(
        &fixture,
        hint(&selector, &run),
        &fixture.satellite_channel.snapshot.local,
    )
    .expect_err("a retired authenticated link cannot schedule a hint");
    assert!(!error.message.is_empty() && error.message.len() <= 512);
    assert_eq!(
        fixture
            .central
            .federation_executor_wake_signal
            .generation
            .load(Ordering::SeqCst),
        retired_generation
    );
    no_destination_authority(&fixture, &transport);
    drop(handles);
    server.finish().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lost_ready_hint_and_reopened_pending_store_recover_original_completion_without_new_send() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) = super::super::causal_test_completion::connect(&fixture).await;
    let (input, transport, scope) = destination(fixture.central.clone(), &fixture.native);
    let (run, directory, selector) = original(&fixture, "hint-lost-restart", input).await;
    finish(
        &fixture.satellite,
        &run,
        &directory,
        native_test_completion::Verdict::Pass,
        source(),
        source(),
    );
    let path = resolve_coordination_persistence_path(&fixture.central.persistence_path);
    let reopened = AppState {
        mailbox_store: Arc::new(MailboxStore::open(&path).unwrap()),
        ..fixture.central.clone()
    };
    let count = || -> u64 {
        reopened
            .mailbox_store
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM mailbox_relay_intents", [], |row| {
                row.get(0)
            })
            .unwrap()
    };
    let before = count();
    let accepted = receipt(None);
    script(
        &transport,
        &scope,
        &fixture.native,
        Ok(accepted.clone()),
        true,
    );
    tick(&reopened, true).await;
    let saved = reopened
        .mailbox_store
        .causal_test_settlement(&selector, Some(&scope))
        .unwrap()
        .unwrap();
    assert!(saved.complete());
    assert_eq!(saved.completion.run_id, run);
    assert_eq!(saved.admissions[0].receipt, Some(accepted));
    assert_eq!(
        count(),
        before,
        "fallback must not create a new request or send key"
    );
    assert_eq!(writes(&transport).len(), 1);
    drop(handles);
    server.finish().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_ready_hint_schedules_once_and_never_supplies_a_terminal_or_receipt() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) = super::super::causal_test_completion::connect(&fixture).await;
    let (input, transport, scope) = destination(fixture.central.clone(), &fixture.native);
    let (run, _, selector) = original(&fixture, "hint-duplicate", input).await;
    let pending = pending_attempts(&fixture, &selector.payload.request_send_key);
    let value = hint(&selector, &run);
    assert_eq!(
        receive(
            &fixture,
            value.clone(),
            &fixture.satellite_channel.snapshot.local
        )
        .unwrap()["status"],
        "scheduled"
    );
    let generation = fixture
        .central
        .federation_executor_wake_signal
        .generation
        .load(Ordering::SeqCst);
    let second = receive(&fixture, value, &fixture.satellite_channel.snapshot.local).unwrap();
    assert_eq!(second["status"], "ignored");
    assert!(
        second["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty() && reason.len() <= 512)
    );
    assert_eq!(
        fixture
            .central
            .federation_executor_wake_signal
            .generation
            .load(Ordering::SeqCst),
        generation
    );
    assert_eq!(
        pending_attempts(&fixture, &selector.payload.request_send_key),
        pending,
        "receiving a hint does not execute the origin read"
    );
    assert!(
        fixture
            .central
            .mailbox_store
            .causal_test_settlement(&selector, Some(&scope))
            .unwrap()
            .is_none()
    );
    no_destination_authority(&fixture, &transport);
    tick(&fixture.central, false).await;
    assert!(
        fixture
            .central
            .mailbox_store
            .causal_test_settlement(&selector, Some(&scope))
            .unwrap()
            .is_none(),
        "a hint cannot stand in for a still-running producer's authenticated completion"
    );
    no_destination_authority(&fixture, &transport);
    drop(handles);
    server.finish().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completion_ready_http_without_pair_authentication_cannot_schedule_pending_work() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles, observed) = observed_reverse(&fixture).await;
    let (input, transport, _) = destination(fixture.central.clone(), &fixture.native);
    let (run, _, selector) = original(&fixture, "hint-unauthenticated", input).await;
    let before = fixture
        .central
        .federation_executor_wake_signal
        .generation
        .load(Ordering::SeqCst);
    let reply = fixture
        .client
        .post(format!(
            "http://127.0.0.1:{}{MAILBOX_RELAY_RPC_ROUTE}",
            server.port
        ))
        .json(
            &json!({"version":MAILBOX_RELAY_VERSION,"requestId":Uuid::new_v4().to_string(),
            "operation":hint(&selector,&run)}),
        )
        .send()
        .await
        .unwrap();
    assert!(!reply.status().is_success());
    let body = reply.text().await.unwrap();
    assert!(
        !body.is_empty() && body.len() <= 4096,
        "authentication refusal is bounded and readable"
    );
    assert_eq!(
        fixture
            .central
            .federation_executor_wake_signal
            .generation
            .load(Ordering::SeqCst),
        before
    );
    assert_eq!(
        observed.lock().unwrap().len(),
        1,
        "the actual HTTP request reached the authenticated listener"
    );
    no_destination_authority(&fixture, &transport);
    drop(handles);
    server.finish().await;
    fixture.finish().await;
}
