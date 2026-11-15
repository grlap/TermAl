// Recovery through real original sends, native completion and the admission
// owner. Only disposable historical rows are damaged; replies are scripted.
use super::*;

const HISTORY_ROWS: i64 = 256;
const BAD_ROW: i64 = 129;
const NEIGHBOR_ROW: i64 = 130;
const TAIL_ROW: i64 = 513;
const RECOVERY_TICKS: usize = 64;

#[derive(Debug)]
struct ScanOutcome {
    ticks: usize,
    page_advances: usize,
    repeated_cursor: usize,
    passed_bad_row: bool,
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
    let connection = fixture.central.mailbox_store.connection().unwrap();
    // Traversal positions are fixture scheduling data, never custody authority.
    // Keep the genuine messages/receipts and their original identities intact.
    connection
        .execute("UPDATE mailbox_messages SET rowid=rowid+1000000", [])
        .unwrap();
    connection
        .execute("UPDATE mailbox_relay_intents SET rowid=rowid+1000000", [])
        .unwrap();
    for row in 1..=HISTORY_ROWS {
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
    let (column, value) = match fault {
        "payload" => ("test_request_json", "{"),
        "scope" => ("test_request_scope_json", "{"),
        "receipt" => ("receipt_json", "{"),
        "peer" => ("peer_json", "{"),
        "settlement" => ("test_request_settlement_json", "{"),
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
    let settled_delivery = completion(&fixture.central, &mut Default::default()).await;
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

    // Ask the real planner about precisely the page containing both the bad
    // original and its healthy neighbor. A page error must not lose the latter.
    let mut page_cursor = CausalTestCompletionCursor {
        messages_after: BAD_ROW - 1,
        intents_after: BAD_ROW - 1,
        intents: table == "mailbox_relay_intents",
    };
    let planned = fixture
        .central
        .mailbox_store
        .causal_test_completion_plans(&mut page_cursor);
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
        page_advances: 0,
        repeated_cursor: 0,
        passed_bad_row: false,
        neighbor_planned,
        valid_requests_settled: false,
        writes: 0,
    };
    for _ in 0..RECOVERY_TICKS {
        let before = (
            recovery.cursor.messages_after,
            recovery.cursor.intents_after,
            recovery.cursor.intents,
        );
        recovery.tick(&fixture.central, true).await;
        recovery.cursor = recovery
            .task
            .take()
            .expect("one bounded recovery task")
            .await
            .expect("recovery task joined");
        outcome.ticks += 1;
        let after = (
            recovery.cursor.messages_after,
            recovery.cursor.intents_after,
            recovery.cursor.intents,
        );
        if before == after {
            outcome.repeated_cursor += 1;
        } else {
            outcome.page_advances += 1;
        }
        outcome.passed_bad_row |= if table == "mailbox_messages" {
            recovery.cursor.messages_after > BAD_ROW
        } else {
            recovery.cursor.intents_after > BAD_ROW
        };
        outcome.valid_requests_settled = expected.iter().all(|selector| {
            fixture
                .central
                .mailbox_store
                .causal_test_settlement(selector, Some(&scope))
                .unwrap()
                .is_some_and(|saved| saved.complete())
        });
        if outcome.valid_requests_settled {
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
    drop(handles);
    server.finish().await;
    fixture.finish().await;
    outcome
}

#[tokio::test]
async fn causal_scan_large_history_control_settles_the_real_request() {
    let outcome = exercise_history("mailbox_messages", None).await;
    assert!(
        outcome.neighbor_planned && outcome.passed_bad_row && outcome.valid_requests_settled,
        "healthy history control: {outcome:?}"
    );
    assert_eq!(outcome.writes, 2, "one old receipt and one new admission");
}

#[tokio::test]
async fn causal_scan_bad_message_keeps_same_page_plan_and_later_admission() {
    for fault in ["payload", "scope", "settlement"] {
        let outcome = exercise_history("mailbox_messages", Some(fault)).await;
        assert!(
            outcome.neighbor_planned && outcome.passed_bad_row && outcome.valid_requests_settled,
            "valid request must settle past damaged message ({fault}): {outcome:?}"
        );
        assert_eq!(outcome.writes, 2);
    }
}

#[tokio::test]
async fn causal_scan_bad_intent_keeps_same_page_plan_and_later_admission() {
    for fault in ["payload", "scope", "receipt", "peer", "settlement"] {
        let outcome = exercise_history("mailbox_relay_intents", Some(fault)).await;
        assert!(
            outcome.neighbor_planned && outcome.passed_bad_row && outcome.valid_requests_settled,
            "valid request must settle past damaged intent ({fault}): {outcome:?}"
        );
        assert_eq!(outcome.writes, 3);
    }
}
