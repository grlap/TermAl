// Exercises home diagnostics through authenticated HTTP and the existing read handler.
use super::*;

async fn diagnostics(state: &AppState) -> Value {
    get_mailbox_relay_wake_diagnostics(State(state.clone()))
        .await
        .unwrap()
        .0
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn home_rpc_failures_record_real_5xx_without_changing_responses_and_bound_history() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let status = Arc::new(std::sync::atomic::AtomicU16::new(0));
    let selected = status.clone();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let wire = Arc::new(Mutex::new(Vec::new()));
    let captured = wire.clone();
    let checked = fixture.channel.clone();
    let listener = FederationListenerState {
        channel: fixture.channel.clone(),
        ensure_current: Arc::new(move || checked.current_authority().map(|_| ())),
    };
    let proxy = ProxySend::with_home_router(&fixture, move |_| {
        Router::new()
            .route(
                MAILBOX_RELAY_RPC_ROUTE,
                post(
                    move |peer: axum::Extension<FederationAuthenticatedPeer>,
                          Json(request): Json<MailboxRelayRequest>| {
                        let selected = selected.clone();
                        let seen = seen.clone();
                        async move {
                            let status = selected.load(Ordering::SeqCst);
                            if status != 0
                                && matches!(&request.operation, MailboxRelayOperation::Read { .. })
                            {
                                if status >= 500 {
                                    seen.lock().unwrap().push(request.request_id.clone());
                                }
                                return (
                                    StatusCode::from_u16(status).unwrap(),
                                    [("content-type", "application/json")],
                                    r#"{"error":"SECRET-BODY C:/private/store"}"#,
                                )
                                    .into_response();
                            }
                            serve_mailbox_relay(peer, Json(request))
                                .await
                                .into_response()
                        }
                    },
                ),
            )
            .layer(axum::middleware::from_fn_with_state(
                listener,
                federation_authenticate,
            ))
            .layer(axum::middleware::from_fn(
                move |request: axum::extract::Request, next: axum::middleware::Next| {
                    let captured = captured.clone();
                    async move {
                        let response = next.run(request).await;
                        if !response.status().is_server_error() {
                            return response;
                        }
                        let (parts, body) = response.into_parts();
                        let body = axum::body::to_bytes(body, 4096).await.unwrap();
                        captured.lock().unwrap().push((parts.status, body.to_vec()));
                        Response::from_parts(parts, axum::body::Body::from(body))
                    }
                },
            ))
    })
    .await;
    let sent = proxy.send(&fixture, "rpc-failure-history").await;
    let initial = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id)
        .await
        .unwrap();
    let initial_diagnostic = diagnostics(&fixture.central).await;
    status.store(403, Ordering::SeqCst);
    let client_refusal =
        read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id).await;
    let after_4xx = diagnostics(&fixture.central).await;
    status.store(500, Ordering::SeqCst);
    let server_error = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id).await;
    status.store(503, Ordering::SeqCst);
    let unavailable = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id).await;
    let first_failures = diagnostics(&fixture.central).await;
    for _ in 0..65 {
        let error = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id)
            .await
            .err()
            .unwrap();
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    }
    status.store(0, Ordering::SeqCst);
    let restored = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id)
        .await
        .unwrap();
    let bounded = diagnostics(&fixture.central).await;
    let remote = diagnostics(&fixture.satellite).await;
    let lease_current = fixture.satellite_channel.current_lease().is_ok();
    proxy.finish().await;
    fixture.finish().await;

    assert_eq!(initial_diagnostic["homeRpcFailures"], json!([]));
    assert_eq!(after_4xx["homeRpcFailures"], json!([]));
    assert_eq!(client_refusal.err().unwrap().status, StatusCode::FORBIDDEN);
    let error = server_error.err().unwrap();
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        error
            .message
            .starts_with("native mailbox relay response unavailable;")
    );
    let error = unavailable.err().unwrap();
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(error.message.contains("mailbox page unavailable"));
    assert!(
        error
            .message
            .contains("native mailbox relay response unavailable")
    );
    assert!(!error.message.contains("SECRET-BODY"));
    let ids = requests.lock().unwrap();
    assert_eq!(ids.len(), 67);
    for (index, record) in first_failures["homeRpcFailures"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        assert_eq!(record["requestId"], ids[index]);
        assert_eq!(record["operation"], "read");
        assert_eq!(record["stage"], "handlerResponse");
        assert!(chrono::DateTime::parse_from_rfc3339(record["at"].as_str().unwrap()).is_ok());
    }
    assert_eq!(first_failures["homeRpcFailures"][0]["status"], 500);
    assert_eq!(first_failures["homeRpcFailures"][1]["status"], 503);
    let records = bounded["homeRpcFailures"].as_array().unwrap();
    assert_eq!(records.len(), 64);
    assert_eq!(bounded["homeRpcFailuresEvicted"], 3);
    assert_eq!(bounded["homeRpcFailuresResetOnRestart"], true);
    assert_eq!(
        records
            .iter()
            .map(|row| row["requestId"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ids[3..].iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert!(!bounded.to_string().contains("SECRET-BODY"));
    assert!(!bounded.to_string().contains("C:/private"));
    assert_eq!(
        remote["homeRpcFailures"],
        json!([]),
        "only the home records its failure"
    );
    assert_eq!(initial.receipt, restored.receipt);
    assert!(lease_current);
    let wire = wire.lock().unwrap();
    assert_eq!(wire.len(), 67);
    for (_, bytes) in wire.iter() {
        assert_eq!(
            bytes.as_slice(),
            br#"{"error":"SECRET-BODY C:/private/store"}"#
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn home_rpc_failure_on_send_keeps_the_unknown_outcome_and_request_identity() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let request_id = Arc::new(Mutex::new(None));
    let observed = request_id.clone();
    let checked = fixture.channel.clone();
    let listener = FederationListenerState {
        channel: fixture.channel.clone(),
        ensure_current: Arc::new(move || checked.current_authority().map(|_| ())),
    };
    let proxy = ProxySend::with_home_router(&fixture, move |_| {
        Router::new()
            .route(
                MAILBOX_RELAY_RPC_ROUTE,
                post(
                    move |peer: axum::Extension<FederationAuthenticatedPeer>,
                          Json(request): Json<MailboxRelayRequest>| {
                        let observed = observed.clone();
                        async move {
                            if matches!(&request.operation, MailboxRelayOperation::Send { .. }) {
                                *observed.lock().unwrap() = Some(request.request_id.clone());
                                return Err(ApiError::from_status(
                                    StatusCode::SERVICE_UNAVAILABLE,
                                    "PRIVATE-SEND-CAUSE",
                                ));
                            }
                            serve_mailbox_relay(peer, Json(request)).await
                        }
                    },
                ),
            )
            .layer(axum::middleware::from_fn_with_state(
                listener,
                federation_authenticate,
            ))
    })
    .await;
    let mut satellite_api = DomainServer::start(app_router(fixture.satellite.clone())).await;
    let response = fixture.client.post(format!(
        "http://127.0.0.1:{}/api/sessions/{}/mailboxes/send", satellite_api.port, fixture.native))
        .json(&json!({
            "targetSessionId": format!("host:{}::{}", fixture.channel.snapshot.local.host_id, proxy.caller),
            "message": "SECRET-SEND-BODY", "idempotencyKey": "unchanged-send-key",
        })).send().await.unwrap();
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    let diagnostic = diagnostics(&fixture.central).await;
    satellite_api.finish().await;
    proxy.finish().await;
    fixture.finish().await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains(FEDERATION_MAILBOX_UNKNOWN_OUTCOME)
    );
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("same idempotencyKey")
    );
    let rows = diagnostic["homeRpcFailures"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["operation"], "send");
    assert_eq!(
        rows[0]["requestId"],
        request_id.lock().unwrap().as_ref().unwrap().as_str()
    );
    assert_eq!(rows[0]["status"], 503);
    assert!(!diagnostic.to_string().contains("SECRET-SEND-BODY"));
    assert!(!diagnostic.to_string().contains("PRIVATE-SEND-CAUSE"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn home_rpc_failures_distinguish_admission_deadline_and_post_handler_authority() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for (mode, expected) in [
        ("admission", "requestAdmission"),
        ("deadline", "handlerDeadline"),
        ("authority", "postHandlerAuthority"),
    ] {
        let fixture = Fixture::production_shape().await;
        let armed = Arc::new(AtomicBool::new(false));
        let post_refusal = Arc::new(AtomicBool::new(false));
        let admitting = armed.clone();
        let refusing = post_refusal.clone();
        let checked = fixture.channel.clone();
        let clock = fixture.channel.control.clock.clone();
        let listener = FederationListenerState {
            channel: fixture.channel.clone(),
            ensure_current: Arc::new(move || {
                if (mode == "admission" && admitting.load(Ordering::SeqCst))
                    || refusing.load(Ordering::SeqCst)
                {
                    Err(ApiError::from_status(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "PRIVATE-HOME-DETAIL",
                    ))
                } else {
                    checked.current_authority().map(|_| ())
                }
            }),
        };
        let active = armed.clone();
        let proxy = ProxySend::with_home_router(&fixture, move |_| {
            Router::new()
                .route(
                    MAILBOX_RELAY_RPC_ROUTE,
                    post(
                        move |peer: axum::Extension<FederationAuthenticatedPeer>,
                              Json(request): Json<MailboxRelayRequest>| {
                            let active = active.clone();
                            let post_refusal = post_refusal.clone();
                            let clock = clock.clone();
                            async move {
                                let intercept = active.load(Ordering::SeqCst)
                                    && matches!(
                                        &request.operation,
                                        MailboxRelayOperation::Read { .. }
                                    );
                                if intercept && mode == "deadline" {
                                    clock.advance(Duration::from_secs(5));
                                    return std::future::pending::<Response>().await;
                                }
                                let response = serve_mailbox_relay(peer, Json(request))
                                    .await
                                    .into_response();
                                if intercept && mode == "authority" {
                                    post_refusal.store(true, Ordering::SeqCst);
                                }
                                response
                            }
                        },
                    ),
                )
                .layer(axum::middleware::from_fn_with_state(
                    listener,
                    federation_authenticate,
                ))
        })
        .await;
        let sent = proxy.send(&fixture, "rpc-failure-stages").await;
        read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id)
            .await
            .unwrap();
        armed.store(true, Ordering::SeqCst);
        let refused = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id).await;
        let observed = diagnostics(&fixture.central).await;
        proxy.finish().await;
        fixture.finish().await;
        assert_eq!(
            refused.err().unwrap().status,
            StatusCode::SERVICE_UNAVAILABLE
        );
        let rows = observed["homeRpcFailures"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{mode}: {observed}");
        assert_eq!(rows[0]["stage"], expected);
        assert_eq!(rows[0]["operation"], "read");
        assert_eq!(rows[0]["status"], 503);
        assert!(mailbox_relay_uuid(rows[0]["requestId"].as_str().unwrap()));
        assert!(!observed.to_string().contains("PRIVATE-HOME-DETAIL"));
    }
}

#[test]
fn home_rpc_failures_do_not_copy_invalid_or_unknown_metadata() {
    let failures = MailboxRelayHomeRpcFailures::default();
    for request in [
        b"not JSON".as_slice(),
        br#"{"requestId":"SECRET","operation":{"operation":"SECRET","message":"SECRET"}}"#,
    ] {
        failures.note(
            request,
            StatusCode::SERVICE_UNAVAILABLE,
            MailboxRelayHomeRpcStage::HandlerResponse,
        );
    }
    let (rows, evicted) = failures.snapshot();
    assert_eq!(rows.len(), 2);
    assert_eq!(evicted, 0);
    for row in rows {
        assert_eq!(row.request_id, "unavailable");
        assert_eq!(row.operation, "unavailable");
        assert!(!serde_json::to_string(&row).unwrap().contains("SECRET"));
    }
}
