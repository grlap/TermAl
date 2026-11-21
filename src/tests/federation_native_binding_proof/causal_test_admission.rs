// Actual authenticated federation request/native producer, then the ordinary
// destination connection. Scripted consumer replies are never claimed as credit.
use super::*;
use crate::tests::test_runs::test_run_registration::causal_completion_fixture::{
    destination, destination_with_targets, finish, producer,
};

mod completion_ready;
mod completion_ready_notification_custody;
mod completion_ready_recovery;
mod completion_ready_scheduling;
mod scan;

fn source() -> Value {
    json!({"commit":"a".repeat(40),"tree":"b".repeat(40),"submodules":[],"changes":[]})
}

#[tokio::test]
async fn large_multi_target_settlement_reopens_and_retries_every_original_operation() {
    let mut fixture = Fixture::production_shape().await;
    let (mut server, handles) = super::causal_test_completion::connect(&fixture).await;
    let (input, transport, scope) = destination_with_targets(
        fixture.central.clone(),
        &fixture.native,
        MAILBOX_TEST_REQUEST_MAX_TARGETS,
    );
    let (run, directory, selector) = original(&fixture, "large-multi-target", input).await;
    let mut source = source();
    source["changes"] = json!(
        (0..350)
            .map(|index| json!({
                "path": format!("item-{index:04}-{}", "x".repeat(190)),
                "status": " M", "origin": null, "blob": "c".repeat(40), "mode": "100644",
            }))
            .collect::<Vec<_>>()
    );
    finish(
        &fixture.satellite,
        &run,
        &directory,
        native_test_completion::Verdict::Pass,
        source.clone(),
        source,
    );
    let delivery = completion(&fixture.central).await;
    let prepared = CausalTestSettlement::new(&delivery);
    assert_eq!(prepared.admissions.len(), MAILBOX_TEST_REQUEST_MAX_TARGETS);
    for admission in &prepared.admissions {
        let wire = admission
            .wire
            .as_ref()
            .expect("each captured target fits its own frame");
        assert!(wire.len() > ENGRAM_CONTROL_MAX_FRAME_BYTES * 3 / 4);
        assert!(wire.len() <= ENGRAM_CONTROL_MAX_FRAME_BYTES);
        assert!(admission.request.is_some());
        assert!(admission.refusal.is_none());
    }
    script(
        &transport,
        &scope,
        &fixture.native,
        Err(EngramTransportError::protocol("lost original reply")),
        true,
    );
    fixture
        .central
        .settle_causal_test_completion(&delivery)
        .unwrap();
    let retained = fixture
        .central
        .mailbox_store
        .causal_test_settlement(&selector, Some(&scope))
        .unwrap()
        .unwrap();
    assert_eq!(retained.completion, delivery.completion);
    assert!(!retained.complete());
    assert_eq!(writes(&transport).len(), 1);

    let reopened = AppState {
        mailbox_store: Arc::new(
            MailboxStore::open(&resolve_coordination_persistence_path(
                &fixture.central.persistence_path,
            ))
            .unwrap(),
        ),
        ..fixture.central.clone()
    };
    // The large retained original must recover without rereading its runner.
    reopened.federation_peer_handles.lock().unwrap().clear();
    let mut receipts = Vec::new();
    for index in 0..MAILBOX_TEST_REQUEST_MAX_TARGETS {
        let accepted = receipt(Some("untracked_debris_pending_disposition"));
        script(
            &transport,
            &scope,
            &fixture.native,
            Ok(accepted.clone()),
            true,
        );
        let retry = completion(&reopened).await;
        assert_eq!(retry.completion, delivery.completion);
        assert_eq!(retry.selector, selector);
        reopened.settle_causal_test_completion(&retry).unwrap();
        receipts.push(accepted);
        let saved = reopened
            .mailbox_store
            .causal_test_settlement(&selector, Some(&scope))
            .unwrap()
            .unwrap();
        assert_eq!(
            saved
                .admissions
                .iter()
                .filter(|a| a.receipt.is_some())
                .count(),
            index + 1
        );
        assert_eq!(saved.completion, retained.completion);
    }
    let requests = writes(&transport);
    assert_eq!(requests.len(), MAILBOX_TEST_REQUEST_MAX_TARGETS + 1);
    assert_eq!(requests[0].request, requests[1].request);
    assert_eq!(requests[0].connection, requests[1].connection);
    let keys: BTreeSet<_> = requests[1..]
        .iter()
        .map(|r| r.request["operation_key"].as_str().unwrap())
        .collect();
    assert_eq!(keys.len(), MAILBOX_TEST_REQUEST_MAX_TARGETS);
    let again = MailboxStore::open(&resolve_coordination_persistence_path(
        &fixture.central.persistence_path,
    ))
    .unwrap();
    let saved = again
        .causal_test_settlement(&selector, Some(&scope))
        .unwrap()
        .unwrap();
    assert!(saved.complete());
    for (index, admission) in saved.admissions.iter().enumerate() {
        assert_eq!(admission.receipt, Some(receipts[index].clone()));
        assert_eq!(admission.request, prepared.admissions[index].request);
        assert_eq!(admission.wire, prepared.admissions[index].wire);
    }
    let count = transport.requests().len();
    reopened.settle_causal_test_completion(&delivery).unwrap();
    assert_eq!(transport.requests().len(), count);
    drop(handles);
    server.finish().await;
    fixture.finish().await;
}

fn receipt(reason: Option<&str>) -> Value {
    json!({"operation":Uuid::new_v4().simple().to_string(),
        "origin":Uuid::new_v4().simple().to_string(),
        "verifications": if reason.is_none() { vec![json!({"stage":"full","record_id":Uuid::new_v4().simple().to_string()})] } else { vec![] },
        "qualification": reason.map_or(json!({"credited":true}), |reason| json!({"credited":false,"reason":reason}))})
}

fn script(
    transport: &ScriptedEngramControlTransport,
    scope: &MailboxTestSenderScope,
    session: &str,
    reply: Result<Value, EngramTransportError>,
    supported: bool,
) {
    let capabilities = if supported {
        json!(["host_work_v1", "remote_verification_record_v1"])
    } else {
        json!(["host_work_v1", "verification_record_v1"])
    };
    let read = json!({"work_session":true,"session_id":session,
        "project_id":scope.store.project_id,"capabilities":capabilities});
    let mut responses = transport.responses.lock().unwrap();
    responses.push_back(ScriptedEngramControlResponse::Reply(Ok(read)));
    if supported {
        responses.push_back(ScriptedEngramControlResponse::Reply(reply));
    }
}

fn writes(transport: &ScriptedEngramControlTransport) -> Vec<RecordedEngramControlRequest> {
    transport
        .requests()
        .into_iter()
        .filter(|r| r.request["operation"] == "remote_verification_record")
        .collect()
}

async fn original(
    fixture: &Fixture,
    key: &str,
    input: MailboxTestRequest,
) -> (String, PathBuf, CausalTestCompletionSelector) {
    let (_, run, directory) = producer(fixture.satellite.clone(), key, |runner| async move {
        let capture = fixture
            .central
            .prepare_mailbox_test_request(&fixture.native, key, Some(&input))
            .unwrap()
            .unwrap();
        assert!(
            capture
                .payload
                .targets
                .iter()
                .all(|t| t.association.is_some())
        );
        let request = SendMailboxMessageRequest {
            target_session_id: format!(
                "host:{}::{runner}",
                fixture.satellite_channel.snapshot.local.host_id
            ),
            message: "run original native request".into(),
            idempotency_key: key.into(),
            topic: None,
            state_stamp: None,
            class: None,
        };
        let receipt = fixture
            .central
            .send_native_mailbox_relay_with_test_custody(&fixture.native, &request, Some(&capture))
            .await
            .unwrap()
            .unwrap();
        let page = fixture
            .satellite
            .read_native_mailbox_relay(
                &runner,
                MailboxRelayLocator::decode(&receipt.mailbox_id, false)
                    .unwrap()
                    .unwrap(),
                ReadMailboxRequest {
                    read_key: None,
                    issue_receipt: false,
                    after_sequence: None,
                    limit: 50,
                },
            )
            .await
            .unwrap();
        page.messages
            .into_iter()
            .find(|m| m.id == receipt.message_id)
            .unwrap()
            .id
    })
    .await;
    let registration = fixture
        .satellite
        .inner
        .lock()
        .unwrap()
        .test_run_registrations[&run]
        .clone();
    let causal = test_run_causal_request(&registration).unwrap();
    (
        run,
        directory,
        CausalTestCompletionSelector {
            canonical_message_id: causal.canonical_message_id.clone(),
            payload: causal.payload.clone(),
        },
    )
}

async fn completion(app: &AppState) -> CausalTestCompletionDelivery {
    for _ in 0..8 {
        let mut results = app.read_causal_test_completion_batch().await.unwrap();
        if !results.is_empty() {
            return results.remove(0);
        }
    }
    panic!("original authoritative completion must be recovered");
}

#[tokio::test]
async fn supervisor_delivers_original_once_and_settled_receipts_stop_federation_polling() {
    let mut fixture = Fixture::production_shape().await;
    let (mut server, handles) = super::causal_test_completion::connect(&fixture).await;
    let (input, transport, scope) = destination(fixture.central.clone(), &fixture.native);
    let (run, directory, selector) = original(&fixture, "automatic-admission", input).await;
    finish(
        &fixture.satellite,
        &run,
        &directory,
        native_test_completion::Verdict::Pass,
        source(),
        source(),
    );
    let accepted = receipt(None);
    script(
        &transport,
        &scope,
        &fixture.native,
        Ok(accepted.clone()),
        true,
    );
    let mut recovery = CausalTestCompletionRecovery::default();
    for _ in 0..8 {
        recovery.tick(&fixture.central, true).await;
        let task = recovery.task.take().unwrap();
        task.await.unwrap();
        if fixture
            .central
            .mailbox_store
            .causal_test_settlement(&selector, Some(&scope))
            .unwrap()
            .is_some_and(|s| s.complete())
        {
            break;
        }
    }
    let saved = fixture
        .central
        .mailbox_store
        .causal_test_settlement(&selector, Some(&scope))
        .unwrap()
        .unwrap();
    assert!(saved.complete());
    let backup = fixture
        .central
        .persistence_path
        .with_extension("coordination-backup.sqlite");
    fixture
        .central
        .mailbox_store
        .connection()
        .unwrap()
        .execute("VACUUM INTO ?1", [backup.to_str().unwrap()])
        .unwrap();
    let copied = MailboxStore::open(&backup).unwrap();
    assert_eq!(
        copied
            .causal_test_settlement(&selector, Some(&scope))
            .unwrap()
            .unwrap(),
        saved
    );
    // Close the reopened backup before the fixture removes its directory.
    drop(copied);
    assert_eq!(saved.admissions[0].receipt, Some(accepted));
    assert_eq!(writes(&transport).len(), 1);
    assert_eq!(writes(&transport)[0].connection.session_id, fixture.native);
    assert_eq!(
        writes(&transport)[0].request["completion"],
        serde_json::to_value(&saved.completion).unwrap()
    );
    assert_eq!(
        writes(&transport)[0].request["association"]["binding"],
        serde_json::to_value(
            &selector.payload.targets[0]
                .association
                .as_ref()
                .unwrap()
                .binding
        )
        .unwrap()
    );
    fixture
        .central
        .federation_peer_handles
        .lock()
        .unwrap()
        .clear();
    // Final receipt is consulted before a disconnected origin could be read.
    for _ in 0..8 {
        assert!(
            fixture
                .central
                .read_causal_test_completion_batch()
                .await
                .unwrap()
                .is_empty()
        );
    }
    let request_count = transport.requests().len();
    {
        let mut inner = fixture.central.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        inner.sessions[index].engram.work_binding = None;
        inner.sessions[index].session.project_id = None;
    }
    let delivery = CausalTestCompletionDelivery {
        selector: selector.clone(),
        scope: Some(scope.clone()),
        completion: saved.completion.clone(),
        pending: CausalPendingRow { intents: true, row_id: fixture.central.mailbox_store.connection().unwrap().query_row(
            "SELECT rowid FROM mailbox_relay_intents WHERE owner_session_id=?1 AND idempotency_key=?2",
            rusqlite::params![fixture.native,selector.payload.request_send_key], |row| row.get(0)).unwrap() },
    };
    fixture
        .central
        .settle_causal_test_completion(&delivery)
        .unwrap();
    assert_eq!(
        transport.requests().len(),
        request_count,
        "local replay precedes any live association/connection lookup"
    );
    assert!(
        fixture
            .central
            .inner
            .lock()
            .unwrap()
            .test_run_registrations
            .is_empty()
    );
    assert!(
        fixture
            .central
            .inner
            .lock()
            .unwrap()
            .test_run_observations
            .is_empty()
    );
    assert!(
        !format!("{saved:?}").contains(&scope.store.database_path.to_string_lossy().to_string())
    );
    drop(handles);
    server.finish().await;
    fixture.finish().await;
}

#[tokio::test]
async fn unknown_write_recovers_same_bytes_after_reopen_and_reordered_newer_delivery() {
    let mut fixture = Fixture::production_shape().await;
    let (mut server, handles) = super::causal_test_completion::connect(&fixture).await;
    let (input, transport, scope) = destination(fixture.central.clone(), &fixture.native);
    let (first_run, first_dir, first) = original(&fixture, "older-original", input.clone()).await;
    let (second_run, second_dir, second) = original(&fixture, "newer-original", input).await;
    finish(
        &fixture.satellite,
        &first_run,
        &first_dir,
        native_test_completion::Verdict::Fail,
        source(),
        source(),
    );
    finish(
        &fixture.satellite,
        &second_run,
        &second_dir,
        native_test_completion::Verdict::Interrupted,
        source(),
        source(),
    );
    let mut deliveries = Vec::new();
    for _ in 0..8 {
        deliveries.extend(
            fixture
                .central
                .read_causal_test_completion_batch()
                .await
                .unwrap(),
        );
        if deliveries.len() >= 2 {
            break;
        }
    }
    let older = deliveries.iter().find(|d| d.selector == first).unwrap();
    let newer = deliveries.iter().find(|d| d.selector == second).unwrap();
    script(
        &transport,
        &scope,
        &fixture.native,
        Err(EngramTransportError::protocol("lost committed reply")),
        true,
    );
    fixture
        .central
        .settle_causal_test_completion(older)
        .unwrap();
    let before = fixture
        .central
        .mailbox_store
        .causal_test_settlement(&first, Some(&scope))
        .unwrap()
        .unwrap();
    assert!(!before.complete());
    let newer_receipt = receipt(Some("result_not_pass"));
    script(
        &transport,
        &scope,
        &fixture.native,
        Ok(newer_receipt.clone()),
        true,
    );
    fixture
        .central
        .settle_causal_test_completion(newer)
        .unwrap();
    let reopened = AppState {
        mailbox_store: Arc::new(
            MailboxStore::open(&resolve_coordination_persistence_path(
                &fixture.central.persistence_path,
            ))
            .unwrap(),
        ),
        ..fixture.central.clone()
    };
    // No new origin read is needed to recover a frozen pending operation.
    reopened.federation_peer_handles.lock().unwrap().clear();
    let original_receipt = receipt(Some("result_not_pass"));
    script(
        &transport,
        &scope,
        &fixture.native,
        Ok(original_receipt.clone()),
        true,
    );
    let retry = completion(&reopened).await;
    assert_eq!(retry.selector, first);
    reopened.settle_causal_test_completion(&retry).unwrap();
    let requests = writes(&transport);
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].request, requests[2].request);
    assert_eq!(requests[0].connection, requests[2].connection);
    assert_ne!(
        requests[0].request["operation_key"],
        requests[1].request["operation_key"]
    );
    assert_eq!(
        reopened
            .mailbox_store
            .causal_test_settlement(&first, Some(&scope))
            .unwrap()
            .unwrap()
            .admissions[0]
            .receipt,
        Some(original_receipt)
    );
    assert_eq!(
        reopened
            .mailbox_store
            .causal_test_settlement(&second, Some(&scope))
            .unwrap()
            .unwrap()
            .admissions[0]
            .receipt,
        Some(newer_receipt)
    );
    assert_eq!(before.completion, retry.completion);
    drop(handles);
    server.finish().await;
    fixture.finish().await;
}

#[tokio::test]
async fn missing_capability_never_writes_and_recorded_uncredited_reasons_are_final() {
    let mut fixture = Fixture::production_shape().await;
    let (mut server, handles) = super::causal_test_completion::connect(&fixture).await;
    let (input, transport, scope) = destination(fixture.central.clone(), &fixture.native);
    for (index, reason) in [
        "producer_unsupported",
        "association_stale",
        "result_conflict",
        "not_full_mode",
        "untracked_debris_pending_disposition",
        "result_not_pass",
        "commit_mismatch",
        "tree_mismatch",
        "source_id_missing_or_changed",
    ]
    .into_iter()
    .enumerate()
    {
        let key = format!("final-reason-{index}");
        let (run, directory, selector) = original(&fixture, &key, input.clone()).await;
        finish(
            &fixture.satellite,
            &run,
            &directory,
            native_test_completion::Verdict::Pass,
            source(),
            source(),
        );
        let delivery = completion(&fixture.central).await;
        assert_eq!(delivery.selector, selector);
        let count = writes(&transport).len();
        script(&transport, &scope, &fixture.native, Ok(Value::Null), false);
        fixture
            .central
            .settle_causal_test_completion(&delivery)
            .unwrap();
        assert_eq!(writes(&transport).len(), count);
        assert!(
            !fixture
                .central
                .mailbox_store
                .causal_test_settlement(&selector, Some(&scope))
                .unwrap()
                .unwrap()
                .complete()
        );
        let final_receipt = receipt(Some(reason));
        script(
            &transport,
            &scope,
            &fixture.native,
            Ok(final_receipt.clone()),
            true,
        );
        fixture
            .central
            .settle_causal_test_completion(&delivery)
            .unwrap();
        assert_eq!(writes(&transport).len(), count + 1);
        assert_eq!(
            fixture
                .central
                .mailbox_store
                .causal_test_settlement(&selector, Some(&scope))
                .unwrap()
                .unwrap()
                .admissions[0]
                .receipt,
            Some(final_receipt)
        );
        fixture
            .central
            .settle_causal_test_completion(&delivery)
            .unwrap();
        assert_eq!(
            writes(&transport).len(),
            count + 1,
            "recorded uncredited is final, not a retry"
        );
    }
    drop(handles);
    server.finish().await;
    fixture.finish().await;
}
