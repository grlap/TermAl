// Open-store metadata corruption beside the original pending/scan regressions.
// Genuine custody and authenticated completions reach the admission owner;
// only disposable SQLite scheduling metadata is damaged, and receipts are scripted.
use super::*;

#[derive(Clone, Copy, Debug)]
enum MetadataFault {
    Json,
    Reason,
    Schema,
    Timestamp,
    Blob,
}

fn damaged_pending_metadata(encoded: &str, fault: MetadataFault) -> rusqlite::types::Value {
    use rusqlite::types::Value as SqlValue;
    if matches!(fault, MetadataFault::Json) {
        return SqlValue::Text("{private-pending-metadata".into());
    }
    if matches!(fault, MetadataFault::Blob) {
        return SqlValue::Blob(b"private-pending-metadata".to_vec());
    }
    let mut state: Value = serde_json::from_str(encoded).unwrap();
    match fault {
        MetadataFault::Reason => state["reason"] = json!("private-unknown-reason"),
        MetadataFault::Schema => state["schemaVersion"] = json!(0),
        MetadataFault::Timestamp => state["firstPendingAt"] = json!("private-invalid-timestamp"),
        MetadataFault::Json | MetadataFault::Blob => unreachable!(),
    }
    SqlValue::Text(serde_json::to_string(&state).unwrap())
}

fn original_pending_custody(fixture: &Fixture, key: &str) -> Vec<rusqlite::types::Value> {
    fixture
        .central
        .mailbox_store
        .connection()
        .unwrap()
        .query_row(
            "SELECT owner_session_id,idempotency_key,test_request_json,test_request_scope_json,
         receipt_json,peer_json,test_request_settlement_json FROM mailbox_relay_intents
         WHERE idempotency_key=?1",
            [key],
            |row| {
                (0..7)
                    .map(|column| row.get(column))
                    .collect::<rusqlite::Result<Vec<_>>>()
            },
        )
        .unwrap()
}

async fn metadata_tick(recovery: &mut CausalTestCompletionRecovery, app: &AppState) {
    recovery.tick(app, true).await;
    recovery
        .task
        .take()
        .expect("one bounded recovery task")
        .await
        .expect("the recovery owner joins");
}

async fn available_metadata_completion(
    fixture: &Fixture,
    selector: &CausalTestCompletionSelector,
) -> native_test_completion::Completion {
    let runner = selector.runner().unwrap();
    let handle = fixture
        .central
        .federation_peer_handle(&runner.host.host_id)
        .expect("the original authenticated runner is connected");
    fixture
        .central
        .request_mailbox_relay::<CausalTestCompletionReply>(
            &handle,
            &handle.channel.current_lease().unwrap(),
            MailboxRelayOperation::ReadCausalTestCompletion {
                selector: selector.clone(),
            },
        )
        .await
        .unwrap()
        .completion
        .expect("terminal completion is available before recovery")
}

async fn metadata_progress(fault: MetadataFault) {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) =
        super::super::super::causal_test_completion::connect(&fixture).await;
    let (input, transport, scope) = destination(fixture.central.clone(), &fixture.native);

    // A healthy plan is selected before the damaged row, so rolling back the
    // shared planning transaction would also discard that healthy progress.
    let (neighbor_run, neighbor_dir, neighbor) =
        original(&fixture, "metadata-neighbor", input.clone()).await;
    let (bad_run, bad_dir, bad) = original(&fixture, "metadata-bad", input.clone()).await;
    // These are genuine local original requests awaiting a runner, not ordinary
    // history. They place the completed tail outside the first 32-key selection.
    for number in 0..40 {
        local_request(&fixture, &input, &format!("metadata-waiting-{number}"));
    }
    let (tail_run, tail_dir, tail) = original(&fixture, "metadata-tail", input).await;
    for (run, directory) in [
        (&neighbor_run, &neighbor_dir),
        (&bad_run, &bad_dir),
        (&tail_run, &tail_dir),
    ] {
        finish(
            &fixture.satellite,
            run,
            directory,
            native_test_completion::Verdict::Pass,
            source(),
            source(),
        );
    }
    let available_neighbor = available_metadata_completion(&fixture, &neighbor).await;
    let available_tail = available_metadata_completion(&fixture, &tail).await;
    assert_eq!(
        available_metadata_completion(&fixture, &bad).await.run_id,
        bad_run
    );
    let custody = original_pending_custody(&fixture, "metadata-bad");
    let (bad_row, encoded): (i64, String) = fixture.central.mailbox_store.connection().unwrap()
        .query_row("SELECT rowid,test_request_pending_json FROM mailbox_relay_intents WHERE idempotency_key='metadata-bad'",
            [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    let damaged = damaged_pending_metadata(&encoded, fault);
    assert_eq!(
        fixture
            .central
            .mailbox_store
            .connection()
            .unwrap()
            .execute(
                "UPDATE mailbox_relay_intents SET test_request_pending_json=?1 WHERE rowid=?2",
                rusqlite::params![&damaged, bad_row]
            )
            .unwrap(),
        1
    );

    let planned = fixture.central.mailbox_store.causal_test_completion_plans();
    let neighbor_planned = planned.as_ref().is_ok_and(|plans| {
        plans
            .iter()
            .any(|plan| plan.selector.canonical_message_id == neighbor.canonical_message_id)
    });
    let tail_outside_first_page = planned.as_ref().is_ok_and(|plans| {
        plans
            .iter()
            .all(|plan| plan.selector.canonical_message_id != tail.canonical_message_id)
    });
    let accepted = receipt(None);
    for _ in 0..2 {
        script(
            &transport,
            &scope,
            &fixture.native,
            Ok(accepted.clone()),
            true,
        );
    }
    let mut recovery = CausalTestCompletionRecovery::default();
    // There are 43 genuine pending requests and at most four plans per tick.
    // Sixteen joined ticks bound this fixture's pending work without a wall-clock wait.
    for _ in 0..16 {
        metadata_tick(&mut recovery, &fixture.central).await;
        if [&neighbor, &tail].iter().all(|selector| {
            fixture
                .central
                .mailbox_store
                .causal_test_settlement(selector, Some(&scope))
                .unwrap()
                .is_some_and(|saved| saved.complete())
        }) {
            break;
        }
    }
    recovery.shutdown().await;
    let settled = [&neighbor, &tail]
        .iter()
        .zip([&available_neighbor, &available_tail])
        .all(|(selector, completion)| {
            fixture
                .central
                .mailbox_store
                .causal_test_settlement(selector, Some(&scope))
                .unwrap()
                .is_some_and(|saved| {
                    saved.complete()
                        && saved.completion == *completion
                        && saved.admissions[0].receipt == Some(accepted.clone())
                })
        });
    let sent = writes(&transport);
    let actual_runs = sent
        .iter()
        .map(|write| {
            write.request["completion"]["run_id"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        original_pending_custody(&fixture, "metadata-bad"),
        custody,
        "a skip never changes original custody, canonical receipt or settlement"
    );
    let retained: rusqlite::types::Value = fixture
        .central
        .mailbox_store
        .connection()
        .unwrap()
        .query_row(
            "SELECT test_request_pending_json FROM mailbox_relay_intents WHERE rowid=?1",
            [bad_row],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        retained, damaged,
        "malformed bytes and their SQLite type remain unchanged"
    );
    assert!(
        fixture
            .central
            .mailbox_store
            .causal_test_settlement(&bad, Some(&scope))
            .unwrap()
            .is_none(),
        "bad scheduling metadata never supplies an admission"
    );
    let diagnostic = get_mailbox_relay_wake_diagnostics(State(fixture.central.clone()))
        .await
        .ok()
        .map(|response| response.0);
    let readable = diagnostic.as_ref().is_some_and(|value| {
        let pending = &value["causalCompletionPending"];
        pending["records"].as_array().is_some_and(|records| {
            records.len() <= CAUSAL_PENDING_DIAGNOSTICS
                && records.iter().any(|record| {
                    record["table"] == "mailbox_relay_intents"
                        && record["rowId"] == bad_row
                        && record["state"]["reason"] == "pending_metadata_decode"
                })
        }) && pending["resetOnRestart"] == false
            && !value.to_string().contains("private-")
            && !value.to_string().contains("metadata-bad")
            && !value.to_string().contains("legacyCustodyDigest")
    });
    assert!(
        fixture
            .central
            .inner
            .lock()
            .unwrap()
            .test_run_registrations
            .is_empty(),
        "remote settlement never creates a requester-local run"
    );
    // Runtime isolation is not a boot-validation exemption for damaged metadata.
    assert!(
        ensure_sqlite_coordination_schema(&fixture.central.mailbox_store.connection().unwrap())
            .is_err()
    );
    let observed = (
        neighbor_planned,
        tail_outside_first_page,
        settled,
        sent.len(),
        readable,
    );
    drop(handles);
    server.finish().await;
    fixture.finish().await;
    assert_eq!(observed, (true, true, true, 2, true), "fault={fault:?}");
    assert_eq!(
        actual_runs,
        [neighbor_run, tail_run]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        "only the two healthy original completions are admitted exactly once"
    );
}

#[tokio::test]
async fn malformed_pending_metadata_keeps_healthy_plans_and_later_original_settlement() {
    for fault in [
        MetadataFault::Json,
        MetadataFault::Reason,
        MetadataFault::Schema,
        MetadataFault::Timestamp,
        MetadataFault::Blob,
    ] {
        metadata_progress(fault).await;
    }
}

#[tokio::test]
async fn malformed_pending_metadata_diagnostics_are_bounded_private_and_leave_raw_rows() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (input, _transport, _sender_scope) = destination(fixture.central.clone(), &fixture.native);
    let malformed = "{private-pending-metadata";
    for number in 0..70 {
        local_request(&fixture, &input, &format!("metadata-diagnostic-{number}"));
    }
    let before = {
        let connection = fixture.central.mailbox_store.connection().unwrap();
        assert_eq!(connection.execute(
            "UPDATE mailbox_messages SET test_request_pending_json=?1 WHERE idempotency_key LIKE 'metadata-diagnostic-%'",
            [malformed]).unwrap(), 70);
        connection.prepare("SELECT rowid,test_request_json,test_request_scope_json,test_request_pending_json FROM mailbox_messages WHERE idempotency_key LIKE 'metadata-diagnostic-%' ORDER BY rowid")
            .unwrap().query_map([], |row| (0..4).map(|column| row.get::<_, rusqlite::types::Value>(column))
                .collect::<rusqlite::Result<Vec<_>>>()).unwrap()
            .collect::<rusqlite::Result<Vec<_>>>().unwrap()
    };
    let diagnostic = get_mailbox_relay_wake_diagnostics(State(fixture.central.clone()))
        .await
        .ok()
        .map(|response| response.0);
    let after = fixture.central.mailbox_store.connection().unwrap()
        .prepare("SELECT rowid,test_request_json,test_request_scope_json,test_request_pending_json FROM mailbox_messages WHERE idempotency_key LIKE 'metadata-diagnostic-%' ORDER BY rowid")
        .unwrap().query_map([], |row| (0..4).map(|column| row.get::<_, rusqlite::types::Value>(column))
            .collect::<rusqlite::Result<Vec<_>>>()).unwrap()
        .collect::<rusqlite::Result<Vec<_>>>().unwrap();
    assert_eq!(
        after, before,
        "reading diagnostics never repairs or drops original rows"
    );
    fixture.finish().await;
    let diagnostic = diagnostic.expect("one malformed row must not fail supported diagnostics");
    let pending = &diagnostic["causalCompletionPending"];
    let records = pending["records"].as_array().unwrap();
    assert_eq!(records.len(), CAUSAL_PENDING_DIAGNOSTICS);
    assert_eq!(pending["total"], 70);
    assert_eq!(pending["limited"], true);
    assert_eq!(pending["resetOnRestart"], false);
    assert!(
        records
            .iter()
            .all(|record| record["state"]["reason"] == "pending_metadata_decode")
    );
    assert!(!diagnostic.to_string().contains("private-"));
    assert!(!diagnostic.to_string().contains("metadata-diagnostic-"));
    assert!(!diagnostic.to_string().contains("legacyCustodyDigest"));
}
