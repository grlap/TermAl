// Exact predecessor and transactional pending-set controls; disposable SQLite.
use super::*;

fn pending_predecessor() -> rusqlite::Connection {
    let connection = predecessor();
    connection
        .execute_batch(MAILBOX_TEST_REQUEST_SETTLEMENT_SCHEMA_SQL)
        .unwrap();
    connection
        .execute(
            "UPDATE meta SET value=?1 WHERE key='coordination_schema_version'",
            [COORDINATION_CAUSAL_PENDING_PREDECESSOR],
        )
        .unwrap();
    validate_coordination_schema_version(&connection, COORDINATION_CAUSAL_PENDING_PREDECESSOR)
        .unwrap();
    connection.execute_batch("INSERT INTO mailboxes VALUES('pending-box','pending-pair','stamp',2);
        INSERT INTO mailbox_participants VALUES('pending-box','sender','Sender',0,'stamp',NULL);
        INSERT INTO mailbox_messages(id,mailbox_id,sequence,sender_session_id,sender_name,
         target_session_id,target_name,created_at,class,body,idempotency_key,
         unread_depth_at_append,notification_disposition)
        VALUES('original','pending-box',1,'sender','Sender','recipient','Recipient','stamp','routine','original body','original-key',1,'durableButNotWoken');").unwrap();
    connection
}

#[test]
fn pending_upgrade_rolls_back_columns_seed_marker_and_records() {
    for phase in [
        "coord_pending_columns",
        "coord_pending_seed",
        "coord_header",
        "coord_readback",
    ] {
        let connection = pending_predecessor();
        let schema = coordination_schema_objects(&connection).unwrap();
        let rows = raw_rows(&connection);
        TEST_REQUEST_CUSTODY_FLOOR_FAULT.with(|slot| slot.set(Some(phase)));
        let result = ensure_sqlite_coordination_schema(&connection);
        TEST_REQUEST_CUSTODY_FLOOR_FAULT.with(|slot| slot.set(None));
        assert!(result.is_err(), "{phase}");
        assert_eq!(coordination_schema_objects(&connection).unwrap(), schema);
        assert_eq!(raw_rows(&connection), rows);
        validate_coordination_schema_version(&connection, COORDINATION_CAUSAL_PENDING_PREDECESSOR)
            .unwrap();
    }
}

#[test]
fn pending_upgrade_keeps_malformed_original_with_durable_migration_reason() {
    let connection = pending_predecessor();
    connection
        .execute(
            "UPDATE mailbox_messages SET test_request_json='{' WHERE id='original'",
            [],
        )
        .unwrap();
    let store = mailbox_relay_store_id(&connection).unwrap();
    ensure_sqlite_coordination_schema(&connection).unwrap();
    let (body,payload,encoded,order): (String,String,String,i64) = connection.query_row(
        "SELECT body,test_request_json,test_request_pending_json,test_request_pending_order FROM mailbox_messages WHERE id='original'",
        [],|row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
    assert_eq!(body, "original body");
    assert_eq!(payload, "{");
    let state = CausalPendingState::decode(&encoded).unwrap();
    assert!(matches!(
        state.reason,
        CausalPendingReason::TestRequestDecode
    ));
    assert!(state.migrated);
    assert_eq!(state.attempts, 0);
    assert!(order > 0);
    assert_eq!(mailbox_relay_store_id(&connection).unwrap(), store);
    assert!(
        validate_coordination_schema_version(&connection, COORDINATION_CAUSAL_PENDING_PREDECESSOR)
            .is_err()
    );
    // Current boot can report the retained failure; it never repairs custody or
    // treats the scheduling marker as a completion.
    connection.execute_batch("PRAGMA query_only=ON").unwrap();
    ensure_sqlite_coordination_schema(&connection).unwrap();
}

#[test]
fn pending_membership_and_attempt_reason_commit_or_rollback_with_original_transaction() {
    let connection = pending_predecessor();
    ensure_sqlite_coordination_schema(&connection).unwrap();
    {
        let transaction = connection.unchecked_transaction().unwrap();
        create_causal_pending(&transaction, false, "sender", "original-key").unwrap();
        // Simulate the enclosing send authorization/readback refusing COMMIT.
        transaction.rollback().unwrap();
    }
    let missing: Option<String> = connection
        .query_row(
            "SELECT test_request_pending_json FROM mailbox_messages WHERE id='original'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(missing.is_none());
    {
        let transaction = connection.unchecked_transaction().unwrap();
        create_causal_pending(&transaction, false, "sender", "original-key").unwrap();
        transaction.commit().unwrap();
    }
    let row_id: i64 = connection
        .query_row(
            "SELECT rowid FROM mailbox_messages WHERE id='original'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    {
        let transaction = connection.unchecked_transaction().unwrap();
        note_causal_pending(
            &transaction,
            CausalPendingRow {
                intents: false,
                row_id,
            },
            CausalPendingReason::OriginTimeout,
        )
        .unwrap();
        transaction.commit().unwrap();
    }
    let encoded: String = connection
        .query_row(
            "SELECT test_request_pending_json FROM mailbox_messages WHERE id='original'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let state = CausalPendingState::decode(&encoded).unwrap();
    assert!(matches!(state.reason, CausalPendingReason::OriginTimeout));
    assert_eq!(state.attempts, 1);
    assert!(state.last_attempt_at.is_some());
    assert!(!state.migrated);
}

#[test]
fn pending_keys_use_explicit_membership_indexes() {
    let connection = pending_predecessor();
    ensure_sqlite_coordination_schema(&connection).unwrap();
    for table in ["mailbox_messages", "mailbox_relay_intents"] {
        let plan = connection.prepare(&format!(
            "EXPLAIN QUERY PLAN SELECT rowid,test_request_pending_order FROM {table} WHERE test_request_pending_json IS NOT NULL ORDER BY test_request_pending_order LIMIT 32"))
            .unwrap().query_map([],|row|row.get::<_,String>(3)).unwrap()
            .collect::<rusqlite::Result<Vec<_>>>().unwrap().join(" ");
        assert!(
            plan.contains(&format!("USING INDEX {table}_causal_pending")),
            "{plan}"
        );
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
    }
}

#[test]
fn current_pending_marker_never_bypasses_boot_custody_or_settlement_validation() {
    for (column, malformed) in [
        ("test_request_json", "{"),
        ("test_request_scope_json", "{}"),
        ("test_request_settlement_json", "{}"),
    ] {
        let connection = pending_predecessor();
        ensure_sqlite_coordination_schema(&connection).unwrap();
        let transaction = connection.unchecked_transaction().unwrap();
        create_causal_pending(&transaction, false, "sender", "original-key").unwrap();
        transaction
            .execute(
                &format!("UPDATE mailbox_messages SET {column}=?1 WHERE id='original'"),
                [malformed],
            )
            .unwrap();
        transaction.commit().unwrap();
        // Even a matching failure reason from a CURRENT retry is not legacy
        // migration quarantine. The strict original boot refusal must remain.
        let row_id: i64 = connection
            .query_row(
                "SELECT rowid FROM mailbox_messages WHERE id='original'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let transaction = connection.unchecked_transaction().unwrap();
        note_causal_pending(
            &transaction,
            CausalPendingRow {
                intents: false,
                row_id,
            },
            if column == "test_request_settlement_json" {
                CausalPendingReason::SettlementDecodeOrValidate
            } else if column == "test_request_scope_json" {
                CausalPendingReason::ScopeDecode
            } else {
                CausalPendingReason::TestRequestDecode
            },
        )
        .unwrap();
        transaction.commit().unwrap();
        let before = raw_rows(&connection);
        assert!(
            ensure_sqlite_coordination_schema(&connection).is_err(),
            "{column}"
        );
        assert_eq!(
            raw_rows(&connection),
            before,
            "boot refusal preserves all original values"
        );
    }
}

#[test]
fn incomplete_pending_upgrade_is_refused_without_adopting_partial_columns_or_header() {
    for layout in [
        "one-column",
        "new-layout-old-header",
        "old-layout-new-header",
    ] {
        let connection = pending_predecessor();
        match layout {
            "one-column" => connection
                .execute_batch(
                    "ALTER TABLE mailbox_messages ADD COLUMN test_request_pending_json TEXT;",
                )
                .unwrap(),
            "new-layout-old-header" => connection
                .execute_batch(MAILBOX_CAUSAL_PENDING_SCHEMA_SQL)
                .unwrap(),
            "old-layout-new-header" => {
                connection
                    .execute(
                        "UPDATE meta SET value=?1 WHERE key='coordination_schema_version'",
                        [COORDINATION_SQLITE_SCHEMA_VERSION],
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let schema = coordination_schema_objects(&connection).unwrap();
        let rows = raw_rows(&connection);
        let error = ensure_sqlite_coordination_schema(&connection).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported coordination database schema"),
            "{layout}: {error}"
        );
        assert_eq!(coordination_schema_objects(&connection).unwrap(), schema);
        assert_eq!(raw_rows(&connection), rows);
    }
}

#[test]
fn legacy_quarantine_cannot_hide_later_changes_to_the_original_bad_bytes() {
    let connection = pending_predecessor();
    connection
        .execute(
            "UPDATE mailbox_messages SET test_request_json='{' WHERE id='original'",
            [],
        )
        .unwrap();
    ensure_sqlite_coordination_schema(&connection).unwrap();
    let state: String = connection
        .query_row(
            "SELECT test_request_pending_json FROM mailbox_messages WHERE id='original'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        CausalPendingState::decode(&state)
            .unwrap()
            .legacy_custody_digest
            .is_some()
    );
    // Both payloads fail the same decode check. A reason code alone must not
    // exempt these different, newly corrupted bytes from strict boot admission.
    connection
        .execute(
            "UPDATE mailbox_messages SET test_request_json='[' WHERE id='original'",
            [],
        )
        .unwrap();
    let before = raw_rows(&connection);
    assert!(ensure_sqlite_coordination_schema(&connection).is_err());
    assert_eq!(raw_rows(&connection), before);
    connection
        .execute(
            "UPDATE mailbox_messages SET test_request_json='{' WHERE id='original'",
            [],
        )
        .unwrap();
    ensure_sqlite_coordination_schema(&connection).unwrap();
}
