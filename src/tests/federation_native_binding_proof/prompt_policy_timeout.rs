// Owns prompt-policy handler deadline isolation in the real MAC middleware.
// The handler is held pending; no commit or live peer failure is simulated.
use super::*;

#[tokio::test]
async fn federation_prompt_policy_handler_deadline_keeps_pair_link_and_authenticated_retry() {
    prompt_handler_deadline(FEDERATION_PROMPT_POLICY_ROUTE).await;
}

async fn prompt_handler_deadline(route: &'static str) {
    let mut channel = federation_link::channel(FederationRole::Central, 'a');
    channel.control.clock = FederationControlClock::scripted_io();
    let (sender, _proof) = federation_control_fixture::establish(&channel).await;
    let lease = channel.current_lease().unwrap();
    let security = channel.security_context().unwrap();
    let listener = FederationListenerState {
        channel: channel.clone(),
        ensure_current: federation_link::allowed(),
    };
    let entered = Arc::new(tokio::sync::Notify::new());
    let signal = entered.clone();
    let router = Router::new()
        .route(
            route,
            post(move || {
                let entered = signal.clone();
                async move {
                    entered.notify_one();
                    std::future::pending::<StatusCode>().await
                }
            }),
        )
        .route("/federation/v1/health", get(|| async { StatusCode::OK }))
        .route_layer(axum::middleware::from_fn_with_state(
            listener.clone(),
            federation_authenticate,
        ))
        .with_state(listener);
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        "content-type",
        axum::http::HeaderValue::from_static("application/json"),
    );
    let request = federation_control_fixture::signed(&sender, 1, "POST", route, headers, b"{}");
    let running = tokio::spawn(router.clone().oneshot(request));
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, entered.notified())
        .await
        .unwrap();
    channel.control.clock.advance(Duration::from_secs(5));
    let response = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(response.headers().contains_key(FEDERATION_MAC_HEADER));
    let body = axum::body::to_bytes(response.into_body(), REMOTE_PROMPT_STATUS_BYTES)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert!(
        value["error"]
            .as_str()
            .unwrap()
            .contains("outcome is unknown")
    );
    lease.with_incarnation(|| Ok(())).unwrap();
    assert!(
        Arc::ptr_eq(&security, &channel.security_context().unwrap()),
        "a late policy handler cannot fence or replace the pair security context"
    );
    let retry = router
        .oneshot(federation_control_fixture::signed(
            &sender,
            2,
            "GET",
            "/federation/v1/health",
            axum::http::HeaderMap::new(),
            &[],
        ))
        .await
        .unwrap();
    assert_eq!(retry.status(), StatusCode::OK);
    assert!(retry.headers().contains_key(FEDERATION_MAC_HEADER));
    lease.with_incarnation(|| Ok(())).unwrap();
    assert_eq!(channel.bound.permits.available_permits(), 4);
}
