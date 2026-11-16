// Recovery through real original sends, native completion and the admission
// owner. Only disposable historical rows are damaged; replies are scripted.
use super::*;

mod latency;
mod pending_metadata;
mod pending_metadata_edges;

const HISTORY_ROWS: i64 = 256;
const BAD_ROW: i64 = 129;
const NEIGHBOR_ROW: i64 = 130;
const TAIL_ROW: i64 = 513;
const RECOVERY_TICKS: usize = 64;

#[derive(Debug)]
struct ScanOutcome {
    ticks: usize,
    failed_key_retained: bool,
    neighbor_planned: bool,
    valid_requests_settled: bool,
    writes: usize,
}

fn local_request(fixture: &Fixture, input: &MailboxTestRequest, key: &str) -> String {
    let (recipient, _) = crate::tests::federation_mailboxes::local_participant(
        &fixture.central,
        "Local scan recipient",
    );
    let capture = fixture
        .central
        .prepare_mailbox_test_request(&fixture.native, key, Some(input))
        .unwrap()
        .unwrap();
    let request = SendMailboxMessageRequest {
        target_session_id: recipient,
        message: "original local typed scan request".into(),
        idempotency_key: key.into(),
        topic: None,
        state_stamp: None,
        class: None,
    };
    fixture
        .central
        .append_native_mailbox_message_with_test_custody(
            &fixture.native,
            request,
            NativeMailboxNamespace::Local,
            Some(capture),
        )
        .unwrap()
        .message_id
}

fn history(fixture: &Fixture, template: &str, intent_template: &str) {
    history_with_rows(fixture, template, intent_template, HISTORY_ROWS);
}

fn history_with_rows(fixture: &Fixture, template: &str, intent_template: &str, rows: i64) {
    let connection = fixture.central.mailbox_store.connection().unwrap();
    let transaction = connection.unchecked_transaction().unwrap();
    let connection = &transaction;
    // Traversal positions are fixture scheduling data, never custody authority.
    // Keep the genuine messages/receipts and their original identities intact.
    connection
        .execute("UPDATE mailbox_messages SET rowid=rowid+1000000", [])
        .unwrap();
    connection
        .execute("UPDATE mailbox_relay_intents SET rowid=rowid+1000000", [])
        .unwrap();
    for row in 1..=rows {
        if [BAD_ROW, NEIGHBOR_ROW, TAIL_ROW].contains(&row) {
            continue;
        }
        assert_eq!(
            connection
                .execute(
                    "INSERT INTO mailbox_messages(rowid,id,mailbox_id,sequence,sender_session_id,
             sender_name,target_session_id,target_name,created_at,class,topic,state_stamp,
             body,idempotency_key,unread_depth_at_append,notification_disposition)
             SELECT ?1,?2,mailbox_id,?3,sender_session_id,sender_name,target_session_id,
             target_name,created_at,class,NULL,NULL,body,?4,0,notification_disposition
             FROM mailbox_messages WHERE id=?5",
                    rusqlite::params![
                        row,
                        format!("mailbox-message-{}", Uuid::new_v4()),
                        10000 + row,
                        format!("ordinary-message-{row}"),
                        template
                    ],
                )
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .execute(
                    "INSERT INTO mailbox_relay_intents(rowid,sender_key,owner_session_id,
             idempotency_key,record_version,request_digest,requested_target,
             conversation_json,peer_json,receipt_json,created_at)
             SELECT ?1,sender_key,owner_session_id,?2,record_version,request_digest,
             requested_target,conversation_json,peer_json,NULL,created_at
             FROM mailbox_relay_intents WHERE idempotency_key=?3",
                    rusqlite::params![row, format!("ordinary-intent-{row}"), intent_template],
                )
                .unwrap(),
            1
        );
    }
    for table in ["mailbox_messages", "mailbox_relay_intents"] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert!(
            count >= 250,
            "the control must traverse substantial {table} history"
        );
    }
    transaction.commit().unwrap();
}

fn place(fixture: &Fixture, table: &str, key: &str, row: i64) {
    assert!(["mailbox_messages", "mailbox_relay_intents"].contains(&table));
    let connection = fixture.central.mailbox_store.connection().unwrap();
    assert_eq!(
        connection
            .execute(
                &format!("UPDATE {table} SET rowid=?1 WHERE idempotency_key=?2"),
                rusqlite::params![row, key],
            )
            .unwrap(),
        1
    );
}

fn damage(fixture: &Fixture, table: &str, fault: &str) -> (String, String) {
    if fault == "many-payload" {
        fixture
            .central
            .mailbox_store
            .connection()
            .unwrap()
            .execute(
                &format!("UPDATE {table} SET test_request_json='{{' WHERE rowid<?1"),
                [BAD_ROW],
            )
            .unwrap();
    }
    if fault == "many-payload" {
        let connection = fixture.central.mailbox_store.connection().unwrap();
        let transaction = connection.unchecked_transaction().unwrap();
        let owner = if table == "mailbox_messages" {
            "sender_session_id"
        } else {
            "owner_session_id"
        };
        let keys = transaction
            .prepare(&format!(
                "SELECT {owner},idempotency_key FROM {table} WHERE rowid<?1"
            ))
            .unwrap()
            .query_map([BAD_ROW], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for (owner, key) in keys {
            create_causal_pending(&transaction, table == "mailbox_relay_intents", &owner, &key)
                .unwrap();
        }
        transaction.commit().unwrap();
    }
    if fault == "selector" {
        assert_eq!(table, "mailbox_relay_intents");
        let connection = fixture.central.mailbox_store.connection().unwrap();
        let encoded: String = connection
            .query_row(
                "SELECT receipt_json FROM mailbox_relay_intents WHERE idempotency_key='scan-bad'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let mut receipt: Value = serde_json::from_str(&encoded).unwrap();
        receipt["messageId"] = json!("non-canonical-message");
        let encoded = serde_json::to_string(&receipt).unwrap();
        connection
            .execute(
                "UPDATE mailbox_relay_intents SET receipt_json=?1 WHERE idempotency_key='scan-bad'",
                [&encoded],
            )
            .unwrap();
        return ("receipt_json".into(), encoded);
    }
    if fault == "invalid-payload" {
        let connection = fixture.central.mailbox_store.connection().unwrap();
        let encoded: String = connection
            .query_row(
                &format!("SELECT test_request_json FROM {table} WHERE idempotency_key='scan-bad'"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        let mut payload: Value = serde_json::from_str(&encoded).unwrap();
        payload["schemaVersion"] = json!(0);
        let encoded = serde_json::to_string(&payload).unwrap();
        assert_eq!(
            connection
                .execute(
                    &format!(
                        "UPDATE {table} SET test_request_json=?1 WHERE idempotency_key='scan-bad'"
                    ),
                    [&encoded],
                )
                .unwrap(),
            1
        );
        return ("test_request_json".into(), encoded);
    }
    let (column, value) = match fault {
        "payload" | "many-payload" => ("test_request_json", "{"),
        "scope" => ("test_request_scope_json", "{"),
        "receipt" => ("receipt_json", "{"),
        "peer" => ("peer_json", "{"),
        "settlement" => ("test_request_settlement_json", "{"),
        "invalid-settlement" => ("test_request_settlement_json", "{}"),
        _ => panic!("unknown disposable row fault"),
    };
    let connection = fixture.central.mailbox_store.connection().unwrap();
    assert_eq!(
        connection
            .execute(
                &format!("UPDATE {table} SET {column}=?1 WHERE idempotency_key='scan-bad'"),
                [value],
            )
            .unwrap(),
        1
    );
    (column.into(), value.into())
}

async fn exercise_history(table: &str, fault: Option<&str>) -> ScanOutcome {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let initial = get_mailbox_relay_wake_diagnostics(State(fixture.central.clone()))
        .await
        .unwrap()
        .0;
    assert_eq!(initial["causalCompletionPending"]["records"], json!([]));
    let (mut server, handles) = super::super::causal_test_completion::connect(&fixture).await;
    let (input, transport, scope) = destination(fixture.central.clone(), &fixture.native);

    // A real prior settlement is the immutable control, not a fabricated receipt
    // inserted into the send row. It must survive every later recovery tick.
    let (settled_run, settled_dir, settled_selector) =
        original(&fixture, "scan-already-settled", input.clone()).await;
    finish(
        &fixture.satellite,
        &settled_run,
        &settled_dir,
        native_test_completion::Verdict::Pass,
        source(),
        source(),
    );
    let settled_delivery = completion(&fixture.central).await;
    let old_receipt = receipt(None);
    script(&transport, &scope, &fixture.native, Ok(old_receipt), true);
    fixture
        .central
        .settle_causal_test_completion(&settled_delivery)
        .unwrap();
    let old_settlement = fixture
        .central
        .mailbox_store
        .causal_test_settlement(&settled_selector, Some(&scope))
        .unwrap()
        .unwrap();
    assert!(old_settlement.complete());

    let mut expected = Vec::new();
    let template = if table == "mailbox_messages" {
        let bad = local_request(&fixture, &input, "scan-bad");
        local_request(&fixture, &input, "scan-neighbor");
        bad
    } else {
        for key in ["scan-bad", "scan-neighbor"] {
            let (run, dir, selector) = original(&fixture, key, input.clone()).await;
            finish(
                &fixture.satellite,
                &run,
                &dir,
                native_test_completion::Verdict::Pass,
                source(),
                source(),
            );
            if key == "scan-neighbor" {
                expected.push(selector);
            }
        }
        // The ordinary-history template is a genuine local message too.
        local_request(&fixture, &input, "scan-template")
    };
    let (tail_run, tail_dir, tail_selector) = original(&fixture, "scan-tail", input).await;
    finish(
        &fixture.satellite,
        &tail_run,
        &tail_dir,
        native_test_completion::Verdict::Pass,
        source(),
        source(),
    );
    expected.push(tail_selector);
    history(&fixture, &template, "scan-tail");
    place(&fixture, table, "scan-bad", BAD_ROW);
    place(&fixture, table, "scan-neighbor", NEIGHBOR_ROW);
    place(&fixture, "mailbox_relay_intents", "scan-tail", TAIL_ROW);
    let damaged = fault.map(|fault| damage(&fixture, table, fault));

    // The same bounded pending selection includes the healthy neighbor even
    // when a damaged original cannot yield a plan. Ordinary rowids do not
    // participate in scheduling.
    let planned = fixture.central.mailbox_store.causal_test_completion_plans();
    let neighbor_planned = planned.as_ref().is_ok_and(|plans| {
        plans
            .iter()
            .any(|plan| plan.selector.payload.request_send_key == "scan-neighbor")
    });
    if let Err(error) = &planned {
        eprintln!("scan control table={table} fault={fault:?} planner_error={error}");
    }
    // Script only the expected healthy admissions. These replies are fixture
    // data, not real Engram credit; the actual sender persists their receipts.
    for _ in &expected {
        script(&transport, &scope, &fixture.native, Ok(receipt(None)), true);
    }
    let mut recovery = CausalTestCompletionRecovery::default();
    let mut outcome = ScanOutcome {
        ticks: 0,
        failed_key_retained: fault.is_none(),
        neighbor_planned,
        valid_requests_settled: false,
        writes: 0,
    };
    for _ in 0..RECOVERY_TICKS {
        recovery.tick(&fixture.central, true).await;
        recovery
            .task
            .take()
            .expect("one bounded recovery task")
            .await
            .expect("recovery task joined");
        outcome.ticks += 1;
        if fault.is_some() {
            let connection = fixture.central.mailbox_store.connection().unwrap();
            outcome.failed_key_retained = connection.query_row(&format!(
                "SELECT test_request_pending_json IS NOT NULL FROM {table} WHERE idempotency_key='scan-bad'"), [], |row| row.get(0)).unwrap();
        }
        outcome.valid_requests_settled = expected.iter().all(|selector| {
            fixture
                .central
                .mailbox_store
                .causal_test_settlement(selector, Some(&scope))
                .unwrap()
                .is_some_and(|saved| saved.complete())
        });
        if outcome.valid_requests_settled && outcome.ticks >= 8 {
            break;
        }
    }
    outcome.writes = writes(&transport).len();
    assert_eq!(
        fixture
            .central
            .mailbox_store
            .causal_test_settlement(&settled_selector, Some(&scope))
            .unwrap()
            .unwrap(),
        old_settlement,
        "settled history must not be changed or replayed"
    );
    if let Some((column, original)) = damaged {
        let retained: String = fixture
            .central
            .mailbox_store
            .connection()
            .unwrap()
            .query_row(
                &format!("SELECT {column} FROM {table} WHERE idempotency_key='scan-bad'"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            retained, original,
            "skipping is not deleting or repairing the bad row"
        );
    }
    assert!(
        fixture
            .central
            .inner
            .lock()
            .unwrap()
            .test_run_registrations
            .is_empty(),
        "remote settlement never manufactures a requester-local test run"
    );
    eprintln!("scan recovery table={table} fault={fault:?} {outcome:?}");
    let diagnostic = get_mailbox_relay_wake_diagnostics(State(fixture.central.clone()))
        .await
        .unwrap()
        .0;
    let pending = &diagnostic["causalCompletionPending"];
    let records = pending["records"].as_array().unwrap();
    if let Some(fault) = fault {
        let record = records
            .iter()
            .find(|row| row["table"] == table && row["rowId"] == BAD_ROW);
        let state = if fault == "many-payload" {
            // A bounded projection need not contain every failing key. Its
            // durable original-row reason remains independently readable.
            let encoded: String = fixture
                .central
                .mailbox_store
                .connection()
                .unwrap()
                .query_row(
                    &format!("SELECT test_request_pending_json FROM {table} WHERE rowid=?1"),
                    [BAD_ROW],
                    |row| row.get(0),
                )
                .unwrap();
            serde_json::to_value(CausalPendingState::decode(&encoded).unwrap()).unwrap()
        } else {
            record.expect("the damaged original has supported durable diagnostics")["state"].clone()
        };
        let expected_reason = match fault {
            "payload" | "invalid-payload" | "many-payload" => "test_request_decode",
            "scope" => "scope_decode",
            "receipt" => "receipt_decode",
            "selector" => "selector_validate",
            "peer" => "peer_decode",
            "settlement" | "invalid-settlement" => "settlement_decode_or_validate",
            _ => panic!("unknown diagnostic fault"),
        };
        assert_eq!(state["reason"], expected_reason);
        assert!(state["attempts"].as_u64().unwrap() >= 2);
        assert!(
            chrono::DateTime::parse_from_rfc3339(state["firstPendingAt"].as_str().unwrap()).is_ok()
        );
        assert!(
            chrono::DateTime::parse_from_rfc3339(state["lastAttemptAt"].as_str().unwrap()).is_ok()
        );
        assert!(
            !diagnostic.to_string().contains("scan-bad"),
            "no send key or custody is projected"
        );
        if fault == "many-payload" {
            assert_eq!(records.len(), CAUSAL_PENDING_DIAGNOSTICS);
            assert!(pending["limited"].as_bool().unwrap());
            assert!(pending["total"].as_u64().unwrap() > CAUSAL_PENDING_DIAGNOSTICS as u64);
        }
    }
    assert_eq!(pending["resetOnRestart"], false);
    drop(handles);
    server.finish().await;
    fixture.finish().await;
    outcome
}

#[tokio::test]
async fn causal_scan_large_history_control_settles_the_real_request() {
    let outcome = exercise_history("mailbox_messages", None).await;
    assert!(
        outcome.neighbor_planned && outcome.failed_key_retained && outcome.valid_requests_settled,
        "healthy history control: {outcome:?}"
    );
    assert_eq!(outcome.writes, 2, "one old receipt and one new admission");
}

#[tokio::test]
async fn causal_scan_bad_message_keeps_same_page_plan_and_later_admission() {
    for fault in ["payload", "scope", "settlement"] {
        let outcome = exercise_history("mailbox_messages", Some(fault)).await;
        assert!(
            outcome.neighbor_planned
                && outcome.failed_key_retained
                && outcome.valid_requests_settled,
            "valid request must settle past damaged message ({fault}): {outcome:?}"
        );
        assert_eq!(outcome.writes, 2);
    }
}

#[tokio::test]
async fn causal_scan_bad_intent_keeps_same_page_plan_and_later_admission() {
    for fault in [
        "payload",
        "scope",
        "receipt",
        "selector",
        "peer",
        "settlement",
    ] {
        let outcome = exercise_history("mailbox_relay_intents", Some(fault)).await;
        assert!(
            outcome.neighbor_planned
                && outcome.failed_key_retained
                && outcome.valid_requests_settled,
            "valid request must settle past damaged intent ({fault}): {outcome:?}"
        );
        assert_eq!(outcome.writes, 3);
    }
}

#[tokio::test]
async fn causal_scan_validation_refusals_and_bounded_diagnostics_preserve_recovery() {
    for (table, fault) in [
        ("mailbox_relay_intents", "invalid-payload"),
        ("mailbox_messages", "invalid-settlement"),
        ("mailbox_messages", "many-payload"),
    ] {
        let outcome = exercise_history(table, Some(fault)).await;
        assert!(
            outcome.neighbor_planned
                && outcome.failed_key_retained
                && outcome.valid_requests_settled,
            "valid request must settle after validation refusal or bounded diagnostic projection ({fault}): {outcome:?}"
        );
        assert_eq!(
            outcome.writes,
            if table == "mailbox_messages" { 2 } else { 3 }
        );
    }
}
