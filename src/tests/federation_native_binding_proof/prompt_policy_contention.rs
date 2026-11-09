// Owns actual proof-handler contention after authenticated admission.
// A held StateInner and per-channel clock exercise the real handler/deadline;
// no provider, remote clock or persistence failure is simulated.
use super::*;

struct ReadPhaseRelease(Arc<(Mutex<bool>, std::sync::Condvar)>);
impl Drop for ReadPhaseRelease {
    fn drop(&mut self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}

async fn contended_read_keeps_link(
    app: &AppState,
    mut channel: FederationChannel,
    sender: &FederationChannel,
    operation: FederationPromptPolicyOperation,
) {
    channel.control.clock = FederationControlClock::scripted_io();
    let phase = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let release = ReadPhaseRelease(phase.clone());
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let entered = Mutex::new(Some(entered));
    channel.control.observed = Arc::new(move |event, _| {
        if event == "promptPolicyWorkerEntered"
            && let Some(entered) = entered.lock().unwrap().take()
        {
            entered.send(()).unwrap();
            let (done, _) = phase
                .1
                .wait_timeout_while(
                    phase.0.lock().unwrap(),
                    phase_sync::DEADLOCK_GUARD,
                    |done| !*done,
                )
                .unwrap();
            assert!(*done, "test did not release the admitted read worker");
        }
    });
    let security = channel.security_context().unwrap();
    let lease = channel.current_lease().unwrap();
    let counter = security.received.lock().unwrap().high + 1;
    let check = channel.clone();
    let router = policy_router_with_check(
        channel.clone(),
        Arc::new(move || check.current_authority().map(|_| ())),
    );
    let request = FederationPromptPolicyRequest {
        protocol_version: 1,
        request_id: Uuid::new_v4().to_string(),
        operation,
    };
    let body = serde_json::to_vec(&request).unwrap();
    let signed = |counter| {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "content-type",
            axum::http::HeaderValue::from_static("application/json"),
        );
        federation_control_fixture::signed(
            sender,
            counter,
            "POST",
            FEDERATION_PROMPT_POLICY_ROUTE,
            headers,
            &body,
        )
    };
    let running = tokio::spawn(router.clone().oneshot(signed(counter)));
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, entered_rx)
        .await
        .unwrap()
        .unwrap();
    // The real handler has authenticated and owns its permit, but has not yet
    // entered link -> StateInner. Make the actual read wait on the state lock.
    let held = app.inner.lock().unwrap();
    drop(release);
    channel.control.clock.advance(Duration::from_secs(5));
    let response = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let status = response.status();
    let authenticated = response.headers().contains_key(FEDERATION_MAC_HEADER);
    let capacity_while_held = channel.bound.permits.available_permits();
    // A canceled HTTP waiter must not release the live blocking job's permit.
    drop(held);
    let drained = tokio::time::timeout(
        phase_sync::DEADLOCK_GUARD,
        channel.bound.permits.clone().acquire_many_owned(4),
    )
    .await
    .unwrap()
    .unwrap();
    drop(drained);
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        authenticated,
        "unknown outcome is authenticated even while StateInner is held"
    );
    assert_eq!(capacity_while_held, 3);
    assert!(
        Arc::ptr_eq(&security, &channel.security_context().unwrap()),
        "contended proof read fenced the pair's security context"
    );
    lease.with_current(app, |_, _| Ok(())).unwrap();
    let retry = router.clone().oneshot(signed(counter + 1)).await.unwrap();
    assert_eq!(retry.status(), StatusCode::OK);
    assert!(retry.headers().contains_key(FEDERATION_MAC_HEADER));
    let retry_body = axum::body::to_bytes(retry.into_body(), REMOTE_PROMPT_STATUS_BYTES)
        .await
        .unwrap();
    let _: FederationPromptPolicyReply = serde_json::from_slice(&retry_body).unwrap();
    let health = router
        .oneshot(federation_control_fixture::signed(
            sender,
            counter + 2,
            "GET",
            "/federation/v1/health",
            axum::http::HeaderMap::new(),
            &[],
        ))
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    assert!(Arc::ptr_eq(&security, &channel.security_context().unwrap()));
    assert_eq!(channel.bound.permits.available_permits(), 4);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_prompt_policy_contended_proof_and_grant_reads_return_authenticated_unknown_without_fencing()
 {
    // Grant issuance must name the fixture's actual central placement.
    let mut fixture = policy_fixture(true).await;
    let scope = scope(&fixture, true);
    let consent = consent(&fixture, &scope);
    let grant = fixture
        .central
        .issue_federation_prompt_grant_on_link(
            version(&fixture.central),
            "contention-grant".into(),
            scope.clone(),
            consent.identity.clone(),
            &fixture.channel.current_lease().unwrap(),
            &fixture.rpc,
        )
        .await
        .unwrap();
    contended_read_keeps_link(
        &fixture.satellite,
        fixture.satellite_channel.clone(),
        &fixture.channel,
        FederationPromptPolicyOperation::DestinationProof {
            scope: scope.clone(),
            consent: consent.identity,
        },
    )
    .await;
    contended_read_keeps_link(
        &fixture.central,
        fixture.channel.clone(),
        &fixture.satellite_channel,
        FederationPromptPolicyOperation::CallerGrant {
            scope,
            grant: grant.identity,
        },
    )
    .await;
    fixture.server.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_prompt_policy_contended_authority_admission_keeps_its_permit_until_settled() {
    let mut fixture = policy_fixture(false).await;
    let scope = scope(&fixture, false);
    let consent = consent(&fixture, &scope);
    let mut channel = fixture.satellite_channel.clone();
    channel.control.clock = FederationControlClock::scripted_io();
    let entered = Arc::new(tokio::sync::Notify::new());
    let signal = entered.clone();
    let check = channel.clone();
    let router = policy_router_with_check(
        channel.clone(),
        Arc::new(move || {
            signal.notify_one();
            check.current_authority().map(|_| ())
        }),
    );
    let security = channel.security_context().unwrap();
    let counter = security.received.lock().unwrap().high + 1;
    let request = FederationPromptPolicyRequest {
        protocol_version: 1,
        request_id: Uuid::new_v4().to_string(),
        operation: FederationPromptPolicyOperation::DestinationProof {
            scope,
            consent: consent.identity,
        },
    };
    let body = serde_json::to_vec(&request).unwrap();
    let signed = |counter| {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "content-type",
            axum::http::HeaderValue::from_static("application/json"),
        );
        federation_control_fixture::signed(
            &fixture.channel,
            counter,
            "POST",
            FEDERATION_PROMPT_POLICY_ROUTE,
            headers,
            &body,
        )
    };
    let held = fixture.satellite.inner.lock().unwrap();
    let running = tokio::spawn(router.clone().oneshot(signed(counter)));
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, entered.notified())
        .await
        .unwrap();
    channel.control.clock.advance(Duration::from_secs(5));
    let response = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let status = response.status();
    let authenticated = response.headers().contains_key(FEDERATION_MAC_HEADER);
    let capacity = channel.bound.permits.available_permits();
    drop(held);
    let drained = tokio::time::timeout(
        phase_sync::DEADLOCK_GUARD,
        channel.bound.permits.clone().acquire_many_owned(4),
    )
    .await
    .unwrap()
    .unwrap();
    drop(drained);
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(authenticated);
    assert_eq!(
        capacity, 3,
        "authority job remains capacity-owned after caller timeout"
    );
    assert!(Arc::ptr_eq(&security, &channel.security_context().unwrap()));
    let retry = router.oneshot(signed(counter + 1)).await.unwrap();
    assert_eq!(retry.status(), StatusCode::OK);
    assert_eq!(channel.bound.permits.available_permits(), 4);
    fixture.server.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_prompt_policy_client_preflight_uses_the_rpc_deadline_and_retains_capacity() {
    let mut fixture = policy_fixture(false).await;
    let scope = scope(&fixture, false);
    let consent = consent(&fixture, &scope);
    let mut channel = fixture.channel.clone();
    channel.control.clock = FederationControlClock::scripted_io();
    let entered = Arc::new(tokio::sync::Notify::new());
    let signal = entered.clone();
    let checked = channel.clone();
    let rpc = FederationRpcClient::new(fixture.server.port, channel.clone(), Arc::new(move || {
        signal.notify_one();
        checked.current_authority().map(|_| ())
    })).unwrap();
    let lease = channel.current_lease().unwrap();
    let security = channel.security_context().unwrap();
    let counter = security.sender.lock().unwrap().next;
    let request = FederationPromptPolicyRequest { protocol_version: 1,
        request_id: Uuid::new_v4().to_string(),
        operation: FederationPromptPolicyOperation::DestinationProof { scope, consent: consent.identity } };
    let held = fixture.central.inner.lock().unwrap();
    let requested_lease = lease.clone();
    let running = tokio::spawn(async move {
        rpc.request::<FederationPromptPolicyReply>(FederationRpcRoute::PromptPolicy(requested_lease, request)).await
    });
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, entered.notified()).await.unwrap();
    channel.control.clock.advance(Duration::from_secs(10));
    let result = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, running).await.unwrap().unwrap();
    let capacity = channel.bound.permits.available_permits();
    drop(held);
    let drained = tokio::time::timeout(phase_sync::DEADLOCK_GUARD,
        channel.bound.permits.clone().acquire_many_owned(4)).await.unwrap().unwrap();
    drop(drained);
    let error = result.err().expect("preflight must expire while the owner is held");
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(error.message.contains("outcome is unknown"));
    assert_eq!(capacity, 3, "expired caller cannot release the still-running authority job");
    assert_eq!(security.sender.lock().unwrap().next, counter, "no request counter was allocated");
    assert!(Arc::ptr_eq(&security, &channel.security_context().unwrap()));
    lease.with_current(&fixture.central, |_, _| Ok(())).unwrap();
    fixture.server.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_prompt_policy_verified_reply_local_authority_deadline_does_not_fence_the_pair() {
    let mut fixture = policy_fixture(false).await;
    let scope = scope(&fixture, false);
    let consent = consent(&fixture, &scope);
    let mut channel = fixture.channel.clone();
    channel.control.clock = FederationControlClock::scripted_io();
    let phase = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let release = ReadPhaseRelease(phase.clone());
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let entered = Mutex::new(Some(entered));
    let checks = std::sync::atomic::AtomicUsize::new(0);
    let checked = channel.clone();
    let rpc = FederationRpcClient::new(fixture.server.port, channel.clone(), Arc::new(move || {
        if checks.fetch_add(1, Ordering::SeqCst) == 1 {
            entered.lock().unwrap().take().unwrap().send(()).unwrap();
            let (done, _) = phase.1.wait_timeout_while(phase.0.lock().unwrap(),
                phase_sync::DEADLOCK_GUARD, |done| !*done).unwrap();
            assert!(*done, "verified-response owner check was not released");
        }
        checked.current_authority().map(|_| ())
    })).unwrap();
    let lease = channel.current_lease().unwrap();
    let security = channel.security_context().unwrap();
    let counter = security.sender.lock().unwrap().next;
    let request = FederationPromptPolicyRequest { protocol_version: 1,
        request_id: Uuid::new_v4().to_string(),
        operation: FederationPromptPolicyOperation::DestinationProof { scope, consent: consent.identity } };
    let requested_lease = lease.clone();
    let running = tokio::spawn(async move {
        rpc.request::<FederationPromptPolicyReply>(FederationRpcRoute::PromptPolicy(requested_lease, request)).await
    });
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, entered_rx).await.unwrap().unwrap();
    let held = fixture.central.inner.lock().unwrap();
    drop(release);
    channel.control.clock.advance(Duration::from_secs(10));
    let result = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, running).await.unwrap().unwrap();
    let capacity = channel.bound.permits.available_permits();
    drop(held);
    let drained = tokio::time::timeout(phase_sync::DEADLOCK_GUARD,
        channel.bound.permits.clone().acquire_many_owned(4)).await.unwrap().unwrap();
    drop(drained);
    let error = result.err().expect("expired local recheck cannot return a usable proof");
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(error.message.contains("outcome is unknown"));
    assert_eq!(capacity, 3);
    assert_eq!(security.sender.lock().unwrap().next, counter + 1, "real RPC allocated exactly one counter");
    assert!(!security.sender.lock().unwrap().outstanding.contains_key(&counter),
        "authenticated receipt settles its transport watchdog before a local owner wait");
    assert!(Arc::ptr_eq(&security, &channel.security_context().unwrap()));
    lease.with_current(&fixture.central, |_, _| Ok(())).unwrap();
    fixture.server.finish().await;
}
