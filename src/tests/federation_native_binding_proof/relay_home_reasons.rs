// Owns native home-refusal diagnostics and unchanged old-receipt ACK witnesses.
// Does not own enrollment, transport retries, live incidents or receipt policy.
// New module using the production-shape authenticated disposable relay fixture.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relay_page_reports_authentication_time_persistence_hold_and_recovers() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let proxy = ProxySend::new(&fixture).await;
    let sent = proxy.send(&fixture, "home-persistence-refusal").await;
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let before = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id)
        .await
        .unwrap();
    let principal = fixture
        .satellite
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap()
        .principal;
    fixture
        .central
        .inner
        .lock()
        .unwrap()
        .set_federation_durable(false, "test_hold");
    let refused = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id).await;
    fixture
        .central
        .inner
        .lock()
        .unwrap()
        .set_federation_durable(true, "test_release");
    let after = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id)
        .await
        .unwrap();
    let diagnostic = get_mailbox_relay_wake_diagnostics(State(fixture.central.clone()))
        .await
        .unwrap()
        .0;
    let rows = diagnostic["federationDurability"]["records"]
        .as_array()
        .unwrap();
    let hold = rows
        .iter()
        .rev()
        .find(|row| row["site"] == "test_hold")
        .unwrap();
    let refusal_record = rows.iter().rev().find(|row| row["status"] == 423).unwrap();
    let release = rows
        .iter()
        .rev()
        .find(|row| row["site"] == "test_release")
        .unwrap();
    assert_eq!(refusal_record["stage"], "channel_authority_check");
    assert_eq!(refusal_record["durable"], false);
    assert_eq!(refusal_record["windowId"], hold["windowId"]);
    assert_eq!(release["windowId"], hold["windowId"]);
    assert!(release["falseForMs"].is_number());
    assert!(
        diagnostic["homeRpcFailures"].as_array().unwrap().is_empty(),
        "423 is separate from the unchanged 5xx history"
    );
    let same_principal = fixture
        .satellite
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap()
        .principal
        == principal;
    let lease_current = fixture.satellite_channel.current_lease().is_ok();
    proxy.finish().await;
    fixture.finish().await;
    let error = refused
        .err()
        .expect("the home durability hold must refuse the page");
    assert_eq!(error.status, StatusCode::LOCKED);
    assert!(
        error
            .message
            .starts_with("native_mailbox_home_persistence_settling;")
    );
    assert!(!error.message.contains("check original identity"));
    assert!(!error.message.contains("outcome may be unknown"));
    assert!(same_principal && lease_current);
    assert_eq!(before.messages[0].id, after.messages[0].id);
    assert_eq!(before.receipt, after.receipt);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_handler_refusals_are_sanitized_and_404_classification_is_unchanged() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for (status, body, expected, prefix) in [
        (
            StatusCode::LOCKED,
            r#"{"error":"federation persistence is settling; retry"}"#,
            StatusCode::LOCKED,
            "native_mailbox_home_persistence_settling;",
        ),
        (
            StatusCode::FORBIDDEN,
            r#"{"error":"SECRET-MAIL-BODY store C:/private/original"}"#,
            StatusCode::FORBIDDEN,
            "native_mailbox_home_reason_unavailable;",
        ),
        (
            StatusCode::NOT_FOUND,
            r#"{"error":"SECRET-MAIL-BODY missing member"}"#,
            StatusCode::NOT_FOUND,
            "authenticated native mailbox resource or membership not found",
        ),
        (
            StatusCode::NOT_FOUND,
            "SECRET-MAIL-BODY unsupported route",
            StatusCode::CONFLICT,
            "native mailbox relay capability or membership unavailable",
        ),
    ] {
        let fixture = Fixture::production_shape().await;
        let checked = fixture.channel.clone();
        let listener = FederationListenerState {
            channel: fixture.channel.clone(),
            ensure_current: Arc::new(move || checked.current_authority().map(|_| ())),
        };
        let entered = Arc::new(AtomicBool::new(false));
        let handler_entered = entered.clone();
        let proxy = ProxySend::with_home_router(&fixture, move |_| {
            Router::new()
                .route(
                    MAILBOX_RELAY_RPC_ROUTE,
                    post(
                        move |peer: axum::Extension<FederationAuthenticatedPeer>,
                              Json(request): Json<MailboxRelayRequest>| {
                            let handler_entered = handler_entered.clone();
                            async move {
                                if matches!(&request.operation, MailboxRelayOperation::Read { .. })
                                {
                                    handler_entered.store(true, Ordering::SeqCst);
                                    (status, [("content-type", "application/json")], body)
                                        .into_response()
                                } else {
                                    serve_mailbox_relay(peer, Json(request))
                                        .await
                                        .into_response()
                                }
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
        let sent = proxy.send(&fixture, "handler-refusal-control").await;
        let result = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id).await;
        let lease_current = fixture.satellite_channel.current_lease().is_ok();
        proxy.finish().await;
        fixture.finish().await;
        let error = result.err().expect("the controlled home handler refused");
        assert!(entered.load(Ordering::SeqCst));
        assert!(lease_current);
        assert_eq!(error.status, expected);
        assert!(error.message.starts_with(prefix), "{}", error.message);
        assert!(!error.message.contains("SECRET-MAIL-BODY"));
        assert!(!error.message.contains("C:/private"));
    }
}

#[test]
fn reason_decoder_rejects_untrusted_shape_and_correlation_without_echoing_prose() {
    let request = MailboxRelayRequest {
        version: MAILBOX_RELAY_VERSION,
        request_id: Uuid::new_v4().to_string(),
        operation: MailboxRelayOperation::Describe {
            native_session_id: "SECRET-NATIVE-IDENTITY".into(),
        },
    };
    let request_body = serde_json::to_vec(&request).unwrap();
    let valid = mailbox_relay_refusal_body(
        &request_body,
        StatusCode::LOCKED,
        br#"{"error":"federation persistence is settling; retry"}"#,
    );
    assert!(valid.len() <= MAILBOX_RELAY_ERROR_BYTES);
    assert!(!String::from_utf8_lossy(&valid).contains("SECRET-NATIVE-IDENTITY"));
    let positive =
        mailbox_relay_verified_refusal(&request, &request_body, StatusCode::LOCKED, &valid);
    assert!(
        positive
            .message
            .starts_with("native_mailbox_home_persistence_settling;")
    );
    let baseline: Value = serde_json::from_slice(&valid).unwrap();
    let mutations: &[fn(&mut Value)] = &[
        |value| value["nativeMailboxError"]["version"] = json!(99),
        |value| value["nativeMailboxError"]["requestId"] = json!("SECRET-MAIL-BODY"),
        |value| value["nativeMailboxError"]["requestDigest"] = json!("bad-digest"),
        |value| value["nativeMailboxError"]["reason"] = json!("SECRET-MAIL-BODY"),
        |value| value["nativeMailboxError"]["body"] = json!("SECRET-MAIL-BODY"),
        |value| value["unexpected"] = json!("SECRET-MAIL-BODY"),
    ];
    let mut malformed = vec![
        b"not JSON SECRET-MAIL-BODY".to_vec(),
        br#"{"error":"SECRET-MAIL-BODY old home prose"}"#.to_vec(),
        vec![b'x'; MAILBOX_RELAY_ERROR_BYTES + 1],
    ];
    for mutate in mutations {
        let mut value = baseline.clone();
        mutate(&mut value);
        malformed.push(serde_json::to_vec(&value).unwrap());
    }
    for bytes in malformed {
        let error =
            mailbox_relay_verified_refusal(&request, &request_body, StatusCode::LOCKED, &bytes);
        assert_eq!(error.status, StatusCode::LOCKED);
        assert!(
            error
                .message
                .starts_with("native_mailbox_home_reason_unavailable;")
        );
        assert!(!error.message.contains("SECRET-MAIL-BODY"));
    }
    let wrong_status =
        mailbox_relay_verified_refusal(&request, &request_body, StatusCode::FORBIDDEN, &valid);
    assert_eq!(wrong_status.status, StatusCode::FORBIDDEN);
    assert!(
        wrong_status
            .message
            .starts_with("native_mailbox_home_reason_unavailable;")
    );
    let oversized = mailbox_relay_refusal_body(
        &request_body,
        StatusCode::LOCKED,
        &vec![b'x'; MAILBOX_RELAY_RAW_ERROR_BYTES + 1],
    );
    assert!(oversized.len() <= MAILBOX_RELAY_ERROR_BYTES);
    let error =
        mailbox_relay_verified_refusal(&request, &request_body, StatusCode::LOCKED, &oversized);
    assert!(
        error
            .message
            .starts_with("native_mailbox_home_reason_unavailable;")
    );
    let unknown = mailbox_relay_refusal_body(
        &request_body,
        StatusCode::LOCKED,
        br#"{"error":"SECRET-MAIL-BODY store C:/private/original"}"#,
    );
    assert!(unknown.len() <= MAILBOX_RELAY_ERROR_BYTES);
    assert!(!String::from_utf8_lossy(&unknown).contains("SECRET-MAIL-BODY"));
    assert!(!String::from_utf8_lossy(&unknown).contains("SECRET-NATIVE-IDENTITY"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tampered_signed_refusal_never_becomes_an_authenticated_home_reason() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let checked = fixture.channel.clone();
    let listener = FederationListenerState {
        channel: fixture.channel.clone(),
        ensure_current: Arc::new(move || checked.current_authority().map(|_| ())),
    };
    let tampered = Arc::new(AtomicBool::new(false));
    let observed = tampered.clone();
    let proxy = ProxySend::with_home_router(&fixture, move |_| {
        Router::new()
            .route(
                MAILBOX_RELAY_RPC_ROUTE,
                post(
                    |peer: axum::Extension<FederationAuthenticatedPeer>,
                     Json(request): Json<MailboxRelayRequest>| async move {
                        if matches!(&request.operation, MailboxRelayOperation::Read { .. }) {
                            ApiError::from_status(
                                StatusCode::LOCKED,
                                "federation persistence is settling; retry",
                            )
                            .into_response()
                        } else {
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
                    let observed = observed.clone();
                    async move {
                        let response = next.run(request).await;
                        if response.status() != StatusCode::LOCKED {
                            return response;
                        }
                        // This outer layer runs AFTER the real response MAC. Keep
                        // its header so only cryptographic verification can refuse.
                        observed.store(true, Ordering::SeqCst);
                        let (parts, _) = response.into_parts();
                        Response::from_parts(
                            parts,
                            axum::body::Body::from(
                                r#"{"nativeMailboxError":{"reason":"SECRET-MAIL-BODY"}}"#,
                            ),
                        )
                    }
                },
            ))
    })
    .await;
    let sent = proxy.send(&fixture, "tampered-home-refusal").await;
    let result = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id).await;
    proxy.finish().await;
    fixture.finish().await;
    let error = result
        .err()
        .expect("a changed MAC-bound body must be refused");
    assert!(tampered.load(Ordering::SeqCst));
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        !error
            .message
            .starts_with("native_mailbox_home_persistence_settling;")
    );
    assert!(!error.message.contains("SECRET-MAIL-BODY"));
}

async fn remote_ack(
    fixture: &Fixture,
    mailbox: &str,
    receipt: String,
) -> Result<MailboxSummary, ApiError> {
    acknowledge_mailbox(
        AxumPath((fixture.native.clone(), mailbox.to_owned())),
        State(fixture.satellite.clone()),
        Json(AcknowledgeMailboxRequest {
            receipt: Some(receipt),
            expected_processed_through: None,
            processed_through: None,
            no_continuation: false,
        }),
    )
    .await
    .map(|summary| summary.0)
}

fn receipt_count(fixture: &Fixture) -> u64 {
    fixture
        .central
        .mailbox_store
        .connection()
        .unwrap()
        .query_row("SELECT count(*) FROM mailbox_read_pages", [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn old_native_receipt_ack_keeps_newer_unread_mail_and_wake() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let proxy = ProxySend::new(&fixture).await;
    let first = proxy.send(&fixture, "old-receipt-first").await;
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let first_page = read_relay_page(&fixture.satellite, &fixture.native, &first.mailbox_id)
        .await
        .unwrap();
    let old = first_page.receipt.unwrap();
    remote_ack(&fixture, &first.mailbox_id, old.clone())
        .await
        .unwrap();
    let second = proxy.send(&fixture, "old-receipt-second").await;
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let second_page = read_relay_page(&fixture.satellite, &fixture.native, &first.mailbox_id)
        .await
        .unwrap();
    remote_ack(&fixture, &first.mailbox_id, second_page.receipt.unwrap())
        .await
        .unwrap();
    let third = proxy.send(&fixture, "old-receipt-third").await;
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let third_page = read_relay_page(&fixture.satellite, &fixture.native, &first.mailbox_id)
        .await
        .unwrap();
    let wake_before = queued_relay_wake(&fixture, &first.mailbox_id).is_some();
    let issued_before = receipt_count(&fixture);
    let repeated = remote_ack(&fixture, &first.mailbox_id, old.clone())
        .await
        .unwrap();
    let issued_after = receipt_count(&fixture);
    let wake_after = queued_relay_wake(&fixture, &first.mailbox_id).is_some();
    let actor = fixture
        .satellite
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap()
        .principal;
    let processed = repeated
        .participants
        .iter()
        .find(|row| row.session_id == actor.participant_key())
        .unwrap()
        .processed_through;
    let fourth = proxy.send(&fixture, "old-receipt-fourth").await;
    let later = read_mailbox(
        AxumPath((fixture.native.clone(), first.mailbox_id.clone())),
        State(fixture.satellite.clone()),
        Json(ReadMailboxRequest {
            after_sequence: Some(third.sequence),
            limit: 20,
            issue_receipt: true,
            read_key: None,
        }),
    )
    .await
    .unwrap()
    .0;
    let gap = remote_ack(&fixture, &first.mailbox_id, later.receipt.unwrap()).await;
    let unissued = remote_ack(&fixture, &first.mailbox_id, Uuid::new_v4().to_string()).await;
    let foreign_page = read_mailbox(
        AxumPath((proxy.caller.clone(), first.mailbox_id.clone())),
        State(fixture.central.clone()),
        Json(ReadMailboxRequest {
            after_sequence: Some(0),
            limit: 20,
            issue_receipt: true,
            read_key: None,
        }),
    )
    .await
    .unwrap()
    .0;
    let foreign = remote_ack(&fixture, &first.mailbox_id, foreign_page.receipt.unwrap()).await;
    let remaining = read_relay_page(&fixture.satellite, &fixture.native, &first.mailbox_id)
        .await
        .unwrap();
    proxy.finish().await;
    fixture.finish().await;
    assert!(
        wake_before && wake_after,
        "old ACK must not suppress the newer unread wake"
    );
    assert_eq!(
        issued_before, issued_after,
        "ACK issues no new page receipt"
    );
    assert_eq!(processed, second.sequence);
    assert_eq!(repeated.unread_count, 1);
    assert_eq!(third_page.messages[0].id, third.message_id);
    assert_eq!(gap.err().unwrap().status, StatusCode::CONFLICT);
    assert_eq!(unissued.err().unwrap().status, StatusCode::BAD_REQUEST);
    assert_eq!(foreign.err().unwrap().status, StatusCode::BAD_REQUEST);
    assert_eq!(remaining.processed_through, second.sequence);
    assert_eq!(
        remaining
            .messages
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        vec![third.message_id.as_str(), fourth.message_id.as_str()]
    );
}
