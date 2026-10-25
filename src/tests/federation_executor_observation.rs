// Owns local operator read coverage for exact executor operations and queue metadata.
// Does not exercise remote transport or grant execution authority. Shares the
// authenticated delivery fixture without changing its wake/recovery machinery.

async fn observe(fixture: &Fixture, key: &str, expected: StatusCode) -> Value {
    let mut url =
        reqwest::Url::parse("http://localhost/api/federation/executor-operation").unwrap();
    url.query_pairs_mut()
        .append_pair("centralHostId", &fixture.target.central.host_id)
        .append_pair("operationKey", key);
    observe_path(
        fixture,
        &format!("{}?{}", url.path(), url.query().unwrap()),
        expected,
    )
    .await
}

async fn observe_path(fixture: &Fixture, path: &str, expected: StatusCode) -> Value {
    let request = Request::builder().uri(path).body(Body::empty()).unwrap();
    let response = request_response(&app_router(fixture.satellite.clone()), request).await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    assert_eq!(status, expected, "{}", String::from_utf8_lossy(&bytes));
    serde_json::from_slice(&bytes).expect("executor observation returns JSON")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn executor_observation_received_is_distinct_from_an_empty_recipient_queue() {
    let fixture = Fixture::new();
    let operation = fixture.operation();
    fixture
        .satellite
        .mailbox_store
        .admit_federation_executor_operation(&operation, || Ok(()))
        .unwrap();
    let value = observe(&fixture, &operation.operation_key, StatusCode::OK).await;
    assert_eq!(value["operation"]["stage"], "received");
    assert_eq!(value["operation"]["effectState"], "not_started");
    assert_eq!(value["operation"]["queueEntryId"], Value::Null);
    assert_eq!(value["operation"]["targetBinding"], json!(fixture.target));
    assert_eq!(value["recipient"]["availability"], "available");
    assert_eq!(value["recipient"]["sessionId"], fixture.native);
    assert_eq!(value["recipient"]["pendingPrompts"], json!([]));
    assert_eq!(value["recipient"]["pendingPromptCount"], 0);
    assert_eq!(value["recipient"]["pendingPromptsTruncated"], false);
    assert!(
        fixture.queue(&fixture.satellite).is_empty(),
        "read must not recover the received wake"
    );
    assert_eq!(fixture.inbox().unwrap().stage, "received");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn executor_observation_exposes_the_exact_queue_entry_without_prompt_contents_or_writes() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.receive(&fixture.request).await.0,
        StatusCode::ACCEPTED
    );
    let queued = fixture.queued().await;
    let before = fixture.inbox().unwrap();
    let changes = fixture
        .satellite
        .mailbox_store
        .connection()
        .unwrap()
        .total_changes();
    let value = observe(&fixture, &before.operation.operation_key, StatusCode::OK).await;
    assert_eq!(value["operation"]["stage"], before.stage);
    assert_eq!(value["operation"]["effectState"], before.effect_state);
    assert_eq!(value["operation"]["queueEntryId"], queued.pending_prompt.id);
    assert_eq!(value["recipient"]["queuePaused"], true);
    assert_eq!(value["recipient"]["pendingPromptCount"], 1);
    assert_eq!(
        value["recipient"]["pendingPrompts"][0]["id"],
        queued.pending_prompt.id
    );
    assert_eq!(value["recipient"]["pendingPrompts"][0]["source"], "mailbox");
    assert_eq!(
        value["recipient"]["pendingPrompts"][0]["timestamp"],
        queued.pending_prompt.timestamp
    );
    let text = value.to_string();
    for private in [
        "ordinary agent work",
        "expandedText",
        "attachments",
        "runtimeAck\"",
        "payload\"",
    ] {
        assert!(!text.contains(private), "diagnostic must omit {private}");
    }
    assert_eq!(
        fixture
            .satellite
            .mailbox_store
            .connection()
            .unwrap()
            .total_changes(),
        changes
    );
    assert_eq!(fixture.inbox().unwrap(), before);
    assert_eq!(fixture.queue(&fixture.satellite).len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn executor_observation_exact_key_namespace_and_query_validation() {
    let fixture = Fixture::new();
    let operation = fixture.operation();
    fixture
        .satellite
        .mailbox_store
        .admit_federation_executor_operation(&operation, || Ok(()))
        .unwrap();
    observe(&fixture, "absent-operation", StatusCode::NOT_FOUND).await;
    let route = "/api/federation/executor-operation";
    observe_path(
        &fixture,
        &format!(
            "{route}?centralHostId=another-central&operationKey={}",
            operation.operation_key
        ),
        StatusCode::NOT_FOUND,
    )
    .await;
    for query in [
        "",
        "centralHostId=x",
        "centralHostId=&operationKey=x",
        "centralHostId=x&operationKey=%00",
        "centralHostId=x&operationKey=x&unknown=true",
    ] {
        observe_path(
            &fixture,
            &format!("{route}?{query}"),
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    observe(&fixture, &"x".repeat(1025), StatusCode::BAD_REQUEST).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn executor_observation_never_attributes_a_replaced_binding_queue_to_the_old_operation() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.receive(&fixture.request).await.0,
        StatusCode::ACCEPTED
    );
    fixture.queued().await;
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        inner.sessions[index]
            .federation_mailbox_binding
            .as_mut()
            .unwrap()
            .identity
            .creation_id = Uuid::new_v4().to_string();
    }
    let value = observe(&fixture, &fixture.operation().operation_key, StatusCode::OK).await;
    assert_eq!(value["operation"]["targetBinding"], json!(fixture.target));
    assert_eq!(value["recipient"]["availability"], "staleBinding");
    assert_eq!(value["recipient"]["pendingPrompts"], Value::Null);
    assert_eq!(value["recipient"]["pendingPromptCount"], Value::Null);
    assert_eq!(fixture.queue(&fixture.satellite).len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn executor_observation_pending_queue_is_bounded_and_reports_omissions() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.receive(&fixture.request).await.0,
        StatusCode::ACCEPTED
    );
    let queued = fixture.queued().await;
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        for number in 1..105 {
            let mut extra = queued.clone();
            extra.pending_prompt.id = format!("extra-{number}");
            extra.pending_prompt.text = "PRIVATE_PENDING_TEXT".into();
            inner.sessions[index].queued_prompts.push_back(extra);
        }
    }
    let value = observe(&fixture, &fixture.operation().operation_key, StatusCode::OK).await;
    assert_eq!(value["recipient"]["pendingPromptCount"], 105);
    assert_eq!(
        value["recipient"]["pendingPrompts"]
            .as_array()
            .unwrap()
            .len(),
        100
    );
    assert_eq!(value["recipient"]["pendingPromptsTruncated"], true);
    assert_eq!(
        value["recipient"]["pendingPrompts"][0]["id"],
        queued.pending_prompt.id
    );
    assert_eq!(value["recipient"]["pendingPrompts"][99]["id"], "extra-99");
    assert!(!value.to_string().contains("PRIVATE_PENDING_TEXT"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn executor_observation_distinguishes_missing_recipients_and_ownership_mirrors() {
    let fixture = Fixture::new();
    let mut missing = fixture.operation();
    missing.operation_key = "missing-recipient".into();
    missing.session_id = "no-such-session".into();
    fixture
        .satellite
        .mailbox_store
        .admit_federation_executor_operation(&missing, || Ok(()))
        .unwrap();
    let value = observe(&fixture, &missing.operation_key, StatusCode::OK).await;
    assert_eq!(value["recipient"]["availability"], "missing");
    assert_eq!(value["recipient"]["pendingPrompts"], Value::Null);
    let mut mirror = fixture.operation();
    mirror.operation_key = "ownership-mirror".into();
    mirror.target_host_id = Uuid::new_v4().to_string();
    fixture
        .satellite
        .mailbox_store
        .admit_federation_executor_operation(&mirror, || Ok(()))
        .unwrap();
    let value = observe(&fixture, &mirror.operation_key, StatusCode::OK).await;
    assert_eq!(value["operation"]["mirror"], true);
    assert_eq!(value["recipient"]["availability"], "notLocalExecutor");
    assert_eq!(value["recipient"]["pendingPrompts"], Value::Null);
    assert_eq!(value["recipient"]["status"], Value::Null);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn executor_observation_processed_does_not_upgrade_an_unknown_effect() {
    let fixture = Fixture::new();
    let operation = fixture.operation();
    let store = &fixture.satellite.mailbox_store;
    store
        .admit_federation_executor_operation(&operation, || Ok(()))
        .unwrap();
    for evidence in [
        FederationExecutorEvidence::Queued {
            queue_entry_id: operation.queue_identity(),
            delivery_attempt_id: "attempt".into(),
        },
        FederationExecutorEvidence::EffectAttempt {
            delivery_attempt_id: "attempt".into(),
        },
        FederationExecutorEvidence::Processed {
            kind: FederationExecutorProcessedKind::MailboxAck,
            evidence: "PRIVATE_ACK_EVIDENCE".into(),
        },
    ] {
        store
            .record_federation_executor_evidence(
                &operation.target_host_id,
                &operation,
                evidence,
                || Ok(()),
            )
            .unwrap();
    }
    let value = observe(&fixture, &operation.operation_key, StatusCode::OK).await;
    assert_eq!(value["operation"]["stage"], "processed");
    assert_eq!(value["operation"]["effectState"], "unknown");
    assert_eq!(value["operation"]["runtimeAckPresent"], false);
    assert_eq!(value["recipient"]["pendingPrompts"], json!([]));
    assert_eq!(value["recipient"]["receiptDisposition"], Value::Null);
    assert!(!value.to_string().contains("PRIVATE_ACK_EVIDENCE"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn executor_observation_retains_receipt_disposition_after_the_prompt_leaves_the_queue() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.receive(&fixture.request).await.0,
        StatusCode::ACCEPTED
    );
    let queued = fixture.queued().await;
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        let record = &mut inner.sessions[index];
        record.queued_prompts.clear();
        record
            .prompt_deliveries
            .get_mut(&queued.pending_prompt.id)
            .unwrap()
            .disposition = PromptDeliveryDisposition::Ambiguous;
    }
    let value = observe(&fixture, &fixture.operation().operation_key, StatusCode::OK).await;
    assert_eq!(value["operation"]["queueEntryId"], queued.pending_prompt.id);
    assert_eq!(value["recipient"]["pendingPrompts"], json!([]));
    assert_eq!(value["recipient"]["receiptDisposition"], "ambiguous");
    assert_eq!(value["recipient"]["queuePosition"], Value::Null);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn executor_observation_unavailable_identity_is_neither_missing_operation_nor_mirror() {
    let fixture = Fixture::new();
    let operation = fixture.operation();
    fixture
        .satellite
        .mailbox_store
        .admit_federation_executor_operation(&operation, || Ok(()))
        .unwrap();
    fixture.satellite.inner.lock().unwrap().federation.local = None;
    let value = observe(
        &fixture,
        &operation.operation_key,
        StatusCode::SERVICE_UNAVAILABLE,
    )
    .await;
    assert!(value.get("operation").is_none());
    assert!(value.get("recipient").is_none());
}
