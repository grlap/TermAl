// Bounded scheduling regressions beside the malformed-row scan controls.
// Original custody, authenticated native reads and the real admission owner
// are used. Ordinary history is disposable fixture data; receipts are scripted.
use super::*;

const UNRELATED_ROWS: i64 = 4096;
const SETTLEMENT_TICKS: usize = 8;

#[derive(Debug)]
struct LatencyOutcome {
    settled: bool,
    writes: usize,
    ticks_after_completion: usize,
}

async fn joined_tick(recovery: &mut CausalTestCompletionRecovery, app: &AppState) {
    recovery.tick(app, true).await;
    recovery
        .task
        .take()
        .expect("one bounded recovery task")
        .await
        .expect("the owned recovery task joins");
}

async fn origin_completion(
    fixture: &Fixture,
    selector: &CausalTestCompletionSelector,
) -> Option<native_test_completion::Completion> {
    let runner = selector.runner().unwrap();
    let handle = fixture
        .central
        .federation_peer_handle(&runner.host.host_id)
        .expect("the authenticated origin is connected");
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
        .expect("the original causal key has a readable native result")
        .completion
}

async fn recovery_latency(large_history: bool, initially_pending: bool) -> LatencyOutcome {
    recovery_latency_with_restart(large_history, initially_pending, false, false).await
}

async fn recovery_latency_with_restart(
    large_history: bool,
    initially_pending: bool,
    restart: bool,
    upgrade: bool,
) -> LatencyOutcome {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let mut fixture = Fixture::production_shape().await;
    let (mut server, handles) =
        super::super::super::causal_test_completion::connect(&fixture).await;
    let (input, transport, scope) = destination(fixture.central.clone(), &fixture.native);
    let template = local_request(&fixture, &input, "latency-history-template");
    let key = "latency-original";
    let (run, directory, selector) = original(&fixture, key, input).await;
    if large_history {
        // The shared fixture reserves three scheduling slots. Four extra rows
        // leave at least 4096 unrelated rows in each table, even after the
        // pending original below replaces one ordinary intent's rowid.
        history_with_rows(&fixture, &template, key, UNRELATED_ROWS + 4);
        if initially_pending {
            let connection = fixture.central.mailbox_store.connection().unwrap();
            assert_eq!(connection.execute(
                "DELETE FROM mailbox_relay_intents WHERE idempotency_key='ordinary-intent-1'", [],
            ).unwrap(), 1);
            drop(connection);
            place(&fixture, "mailbox_relay_intents", key, 1);
        } else {
            place(&fixture, "mailbox_relay_intents", key, UNRELATED_ROWS + 100);
        }
        let connection = fixture.central.mailbox_store.connection().unwrap();
        for (table, prefix) in [
            ("mailbox_messages", "ordinary-message-%"),
            ("mailbox_relay_intents", "ordinary-intent-%"),
        ] {
            let ordinary: i64 = connection.query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE test_request_json IS NULL AND idempotency_key LIKE ?1"),
                [prefix], |row| row.get(0),
            ).unwrap();
            assert!(
                ordinary >= UNRELATED_ROWS,
                "the {table} control really has large unrelated history"
            );
        }
    }
    let mut recovery = CausalTestCompletionRecovery::default();
    if initially_pending {
        assert!(origin_completion(&fixture, &selector).await.is_none());
        joined_tick(&mut recovery, &fixture.central).await;
        joined_tick(&mut recovery, &fixture.central).await;
        assert!(writes(&transport).is_empty());
        let encoded: String = fixture.central.mailbox_store.connection().unwrap().query_row(
            "SELECT test_request_pending_json FROM mailbox_relay_intents WHERE idempotency_key=?1",[key],|row|row.get(0)).unwrap();
        let state = CausalPendingState::decode(&encoded).unwrap();
        assert!(matches!(
            state.reason,
            CausalPendingReason::AwaitingCompletion
        ));
        assert!(state.attempts >= 2);
        assert!(
            fixture
                .central
                .mailbox_store
                .causal_test_settlement(&selector, Some(&scope))
                .unwrap()
                .is_none(),
            "pending is not a fabricated terminal"
        );
    }
    finish(
        &fixture.satellite,
        &run,
        &directory,
        native_test_completion::Verdict::Pass,
        source(),
        source(),
    );
    if restart {
        recovery.shutdown().await;
        let path = if upgrade {
            legacy_pending_copy(&fixture)
        } else {
            resolve_coordination_persistence_path(&fixture.central.persistence_path)
        };
        fixture.central = AppState {
            mailbox_store: Arc::new(MailboxStore::open(&path).unwrap()),
            ..fixture.central.clone()
        };
        recovery = CausalTestCompletionRecovery::default();
        let diagnostics = fixture
            .central
            .mailbox_store
            .causal_pending_diagnostics()
            .unwrap();
        assert!(
            diagnostics["total"].as_u64().unwrap() >= 1,
            "pending work is known immediately after open, before any tick"
        );
    }
    // Prove that the exact native completion is already durable and readable
    // through the authenticated origin before measuring recovery's tick bound.
    let available = origin_completion(&fixture, &selector)
        .await
        .expect("the original completion is available before the bounded ticks start");
    assert_eq!(available.run_id, run);
    let accepted = receipt(None);
    script(
        &transport,
        &scope,
        &fixture.native,
        Ok(accepted.clone()),
        true,
    );
    let mut outcome = LatencyOutcome {
        settled: false,
        writes: 0,
        ticks_after_completion: 0,
    };
    for _ in 0..SETTLEMENT_TICKS {
        joined_tick(&mut recovery, &fixture.central).await;
        outcome.ticks_after_completion += 1;
        if let Some(saved) = fixture
            .central
            .mailbox_store
            .causal_test_settlement(&selector, Some(&scope))
            .unwrap()
        {
            assert_eq!(saved.completion, available, "no substitute completion");
            if saved.complete() {
                assert_eq!(saved.admissions[0].receipt, Some(accepted.clone()));
                let remaining: Option<String> = fixture.central.mailbox_store.connection().unwrap().query_row(
                    "SELECT test_request_pending_json FROM mailbox_relay_intents WHERE idempotency_key=?1",[key],|row|row.get(0)).unwrap();
                assert!(
                    remaining.is_none(),
                    "actual final receipt retires the original pending key atomically"
                );
                outcome.settled = true;
                break;
            }
        }
    }
    recovery.shutdown().await;
    let sent = writes(&transport);
    outcome.writes = sent.len();
    if let Some(sent) = sent.first() {
        assert_eq!(
            sent.request["completion"],
            serde_json::to_value(&available).unwrap()
        );
        assert_eq!(sent.connection.session_id, fixture.native);
    }
    assert!(
        fixture
            .central
            .inner
            .lock()
            .unwrap()
            .test_run_registrations
            .is_empty(),
        "recovery does not create a requester-local run"
    );
    drop(handles);
    server.finish().await;
    fixture.finish().await;
    outcome
}

#[tokio::test]
async fn small_history_control_settles_the_original_within_eight_ticks() {
    let outcome = recovery_latency(false, false).await;
    assert!(
        outcome.settled && outcome.writes == 1,
        "the healthy connected control must settle once: {outcome:?}"
    );
}

#[tokio::test]
async fn a_new_request_behind_large_unrelated_history_settles_within_eight_ticks() {
    let outcome = recovery_latency(true, false).await;
    assert!(
        outcome.settled && outcome.writes == 1,
        "ordinary mailbox history must not postpone a new causal request beyond eight ticks: {outcome:?}"
    );
}

#[tokio::test]
async fn a_visited_pending_request_retries_within_eight_ticks_after_native_completion() {
    let outcome = recovery_latency(true, true).await;
    assert!(
        outcome.settled && outcome.writes == 1,
        "a visited pending request must retry without waiting for the historical cursor to wrap: {outcome:?}"
    );
}

#[tokio::test]
async fn restart_with_pending_request_behind_large_history_settles_within_eight_ticks() {
    let outcome = recovery_latency_with_restart(true, true, true, false).await;
    assert!(
        outcome.settled && outcome.writes == 1,
        "restart must retain the original pending key without a history sweep: {outcome:?}"
    );
}

// Build the exact prior schema in a NEW disposable fixture database and copy
// its original columns verbatim. The source store and its pending metadata are
// never downgraded or modified. This models pre-extension in-flight custody.
fn legacy_pending_copy(fixture: &Fixture) -> PathBuf {
    let path = fixture
        .central
        .persistence_path
        .with_file_name("coordination-before-pending.sqlite");
    let legacy = rusqlite::Connection::open(&path).unwrap();
    legacy.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    for sql in [
        COORDINATION_SCHEMA_V1_SQL,
        MAILBOX_READ_PAGES_SCHEMA_SQL,
        FEDERATION_MAILBOX_DELIVERY_SCHEMA_SQL,
        FEDERATION_EXECUTOR_SCHEMA_SQL,
        MAILBOX_CONTINUATION_DECLARATIONS_SCHEMA_SQL,
    ] {
        legacy.execute_batch(sql).unwrap();
    }
    initialize_mailbox_relay_schema(&legacy).unwrap();
    let remote_prompt_schema = remote_prompt_storage_schema_sql();
    for sql in [
        FEDERATION_EXECUTOR_OUTCOMES_SCHEMA_SQL,
        MAILBOX_REVIEW_CUSTODY_INDEX_SCHEMA_SQL,
        remote_prompt_schema.as_str(),
        MAILBOX_TEST_REQUEST_CUSTODY_SCHEMA_SQL,
        MAILBOX_TEST_REQUEST_SETTLEMENT_SCHEMA_SQL,
    ] {
        legacy.execute_batch(sql).unwrap();
    }
    let tables = legacy.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap()
        .query_map([],|row|row.get::<_,String>(0)).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
    let source = fixture.central.mailbox_store.connection().unwrap();
    let transaction = legacy.unchecked_transaction().unwrap();
    // Copy all original relationships before checking them at COMMIT. Keep
    // foreign keys enabled; alphabetical table order is not dependency order.
    transaction
        .execute_batch("PRAGMA defer_foreign_keys=ON")
        .unwrap();
    // Initialization created only this identity row; every other table is
    // empty. Do not delete parent rows after copying their dependent children.
    transaction
        .execute("DELETE FROM mailbox_relay_store", [])
        .unwrap();
    for table in tables {
        let columns = transaction
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let names = columns.join(",");
        let placeholders = (1..=columns.len())
            .map(|n| format!("?{n}"))
            .collect::<Vec<_>>()
            .join(",");
        let mut query = source
            .prepare(&format!("SELECT {names} FROM {table}"))
            .unwrap();
        let rows = query
            .query_map([], |row| {
                (0..columns.len())
                    .map(|n| row.get::<_, rusqlite::types::Value>(n))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap();
        for values in rows {
            transaction
                .execute(
                    &format!("INSERT INTO {table}({names}) VALUES({placeholders})"),
                    rusqlite::params_from_iter(values.unwrap()),
                )
                .unwrap();
        }
    }
    transaction
        .execute(
            "UPDATE meta SET value=?1 WHERE key='coordination_schema_version'",
            [COORDINATION_CAUSAL_PENDING_PREDECESSOR],
        )
        .unwrap();
    transaction.commit().unwrap();
    validate_coordination_schema_version(&legacy, COORDINATION_CAUSAL_PENDING_PREDECESSOR).unwrap();
    path
}

#[tokio::test]
async fn upgrade_seeds_the_existing_original_before_first_tick_without_history_discovery() {
    let outcome = recovery_latency_with_restart(true, true, true, true).await;
    assert!(
        outcome.settled && outcome.writes == 1,
        "the one-time upgrade must recover the existing original behind history: {outcome:?}"
    );
}
