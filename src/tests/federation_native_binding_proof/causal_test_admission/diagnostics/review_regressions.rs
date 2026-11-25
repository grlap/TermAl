// Deterministic read bounds and recovery at the original settlement commit.
use super::*;

fn page_cache_misses(connection: &rusqlite::Connection) -> i32 {
    let mut misses = 0;
    let mut high_water = 0;
    // The caller has exclusive access. SQLite only reads
    // this connection's pager counters and keeps neither output pointer.
    let status = unsafe {
        rusqlite::ffi::sqlite3_db_status(
            connection.handle(),
            rusqlite::ffi::SQLITE_DBSTATUS_CACHE_MISS,
            &mut misses,
            &mut high_water,
            0,
        )
    };
    assert_eq!(status, rusqlite::ffi::SQLITE_OK);
    misses
}

#[tokio::test]
async fn capped_settlement_detail_read_skips_oversized_payload_pages() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) =
        super::super::super::causal_test_completion::connect(&fixture).await;
    let (input, _, _) = destination(fixture.central.clone(), &fixture.native);
    let key = "oversized-diagnostics-settlement";
    let (_, _, selector) = original(&fixture, key, input).await;
    let store = &fixture.central.mailbox_store;
    let path = resolve_coordination_persistence_path(&fixture.central.persistence_path);
    let detail = rusqlite::Connection::open(&path).unwrap();
    let control = rusqlite::Connection::open(&path).unwrap();
    store
        .note_causal_pending_for_selector(&selector, true, CausalPendingReason::AdmissionPending)
        .unwrap();

    // Model an oversized TEXT value damaged while a disposable store is open.
    let oversized = CAUSAL_SETTLEMENT_MAX_BYTES + 1;
    let row_id = {
        let connection = store.connection().unwrap();
        // SQLite can bypass its pager counter for direct overflow reads from
        // the database file. Keep these new pages in WAL, where SQLite must
        // go through the pager. The fixture writer stays open until both reads.
        connection
            .pragma_update(None, "wal_autocheckpoint", 0)
            .unwrap();
        connection
            .query_row(
                "UPDATE mailbox_relay_intents SET test_request_settlement_json=CAST(zeroblob(?1) AS TEXT)
                 WHERE owner_session_id=?2 AND idempotency_key=?3 RETURNING rowid",
                rusqlite::params![oversized, fixture.native, key],
                |row| row.get(0),
            )
            .unwrap()
    };
    let detail_before = page_cache_misses(&detail);
    let refused = CausalPendingRow {
        intents: true,
        row_id,
    }
    .read_admission_detail(&detail, CAUSAL_SETTLEMENT_MAX_BYTES)
    .unwrap();
    let detail_misses = page_cache_misses(&detail) - detail_before;
    // Positive control: explicitly fetching the value on another connection
    // reads the same payload from WAL, without warming the measured reader.
    let control_before = page_cache_misses(&control);
    let content: Vec<u8> = control
        .query_row(
            "SELECT CAST(test_request_settlement_json AS BLOB) FROM mailbox_relay_intents
             WHERE owner_session_id=?1 AND idempotency_key=?2",
            rusqlite::params![fixture.native, key],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(content.len(), oversized);
    let control_misses = page_cache_misses(&control) - control_before;
    drop(control);
    drop(detail);
    let diagnostic = get_mailbox_relay_wake_diagnostics(State(fixture.central.clone()))
        .await
        .unwrap()
        .0;
    drop(handles);
    server.finish().await;
    fixture.finish().await;

    let pending = &diagnostic["causalCompletionPending"];
    assert_eq!(refused, (true, 0, None));
    assert_eq!(pending["admissionDetailsLimited"], true);
    assert_eq!(pending["records"][0]["rowId"], row_id);
    assert_eq!(
        pending["records"][0]["state"]["reason"],
        "admission_pending"
    );
    assert!(pending["records"][0]["state"]["admissionChecks"].is_null());
    assert!(
        control_misses > 128,
        "positive control must read overflow pages, observed {control_misses} pager misses"
    );
    assert!(
        detail_misses < control_misses / 8,
        "guarded detail read {detail_misses} pages, full-read control {control_misses}"
    );
}

#[tokio::test]
async fn admission_blocker_survives_interruption_before_worker_marker() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) =
        super::super::super::causal_test_completion::connect(&fixture).await;
    let (input, transport, scope) = destination(fixture.central.clone(), &fixture.native);
    let key = "admission-marker-interruption";
    let (run, directory, selector) = original(&fixture, key, input).await;
    let mut recovery = CausalTestCompletionRecovery::default();
    recover(&fixture.central, &mut recovery).await;
    assert_eq!(
        pending_state(&fixture, key).await["reason"],
        "awaiting_completion"
    );
    finish(
        &fixture.satellite,
        &run,
        &directory,
        native_test_completion::Verdict::Pass,
        source(),
        source(),
    );
    {
        let mut central = fixture.central.inner.lock().unwrap();
        central
            .projects
            .iter_mut()
            .find(|project| project.id == scope.project_id)
            .unwrap()
            .engram = None;
    }
    recovery.stop_after_settlement = true;
    recover(&fixture.central, &mut recovery).await;
    recovery.shutdown().await;

    let reopened = MailboxStore::open(&resolve_coordination_persistence_path(
        &fixture.central.persistence_path,
    ))
    .unwrap();
    let saved = reopened
        .causal_test_settlement(&selector, Some(&scope))
        .unwrap()
        .expect("the blocker was committed before the failpoint");
    assert_eq!(saved.completion.run_id, run);
    assert_eq!(
        saved.admissions[0].pending.as_deref(),
        Some("destination_target_unavailable")
    );
    assert!(!saved.complete());
    assert!(writes(&transport).is_empty());
    let diagnostic = reopened.causal_pending_diagnostics().unwrap();
    drop(reopened);
    drop(handles);
    server.finish().await;
    fixture.finish().await;

    assert_eq!(diagnostic["admissionDetailsLimited"], false);
    let state = &diagnostic["records"][0]["state"];
    assert_eq!(state["reason"], "admission_pending");
    assert_eq!(state["check"], "destination_target_unavailable");
}

#[tokio::test]
async fn unreadable_settlement_text_spends_the_detail_budget() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) =
        super::super::super::causal_test_completion::connect(&fixture).await;
    let (input, _, scope) = destination(fixture.central.clone(), &fixture.native);
    let readable = "detail-budget-readable";
    let unreadable = "detail-budget-unreadable";
    let (_, _, unreadable_selector) = original(&fixture, unreadable, input.clone()).await;
    let (run, directory, _) = original(&fixture, readable, input).await;
    finish(
        &fixture.satellite,
        &run,
        &directory,
        native_test_completion::Verdict::Pass,
        source(),
        source(),
    );
    {
        let mut central = fixture.central.inner.lock().unwrap();
        central
            .projects
            .iter_mut()
            .find(|project| project.id == scope.project_id)
            .unwrap()
            .engram = None;
    }
    // A real admission blocker gives the readable row its stored settlement.
    let mut recovery = CausalTestCompletionRecovery::default();
    recover(&fixture.central, &mut recovery).await;
    recovery.shutdown().await;
    let store = &fixture.central.mailbox_store;
    let (readable_row, readable_bytes): (i64, i64) = store
        .connection()
        .unwrap()
        .query_row(
            "SELECT rowid,octet_length(test_request_settlement_json) FROM mailbox_relay_intents
             WHERE owner_session_id=?1 AND idempotency_key=?2",
            rusqlite::params![fixture.native, readable],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let projected = |diagnostic: &Value| {
        diagnostic["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["rowId"] == readable_row)
            .unwrap()["state"]["admissionChecks"]
            .clone()
    };
    let control = store.causal_pending_diagnostics().unwrap();
    // The other row becomes the newest admission row, with stored text that
    // is invalid UTF-8, sized so that the bytes it costs leave too little
    // budget for the readable row.
    let unreadable_bytes = CAUSAL_SETTLEMENT_MAX_BYTES as i64 - readable_bytes + 1;
    assert_eq!(
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE mailbox_relay_intents
                 SET test_request_settlement_json=CAST(unhex(replace(hex(zeroblob(?1)),'00','FF')) AS TEXT)
                 WHERE owner_session_id=?2 AND idempotency_key=?3",
                rusqlite::params![unreadable_bytes, fixture.native, unreadable],
            )
            .unwrap(),
        1
    );
    store
        .note_causal_pending_for_selector(
            &unreadable_selector,
            true,
            CausalPendingReason::AdmissionPending,
        )
        .unwrap();
    let limited = store.causal_pending_diagnostics().unwrap();
    drop(handles);
    server.finish().await;
    fixture.finish().await;

    assert!(!projected(&control).is_null());
    assert_eq!(limited["admissionDetailsLimited"], true);
    assert!(projected(&limited).is_null());
}

#[tokio::test]
async fn a_saved_send_receipt_moves_the_row_on_to_awaiting_completion() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) =
        super::super::super::causal_test_completion::connect(&fixture).await;
    let (input, _, _) = destination(fixture.central.clone(), &fixture.native);
    let key = "diagnostic-receipt-transition";
    original(&fixture, key, input).await;
    let store = &fixture.central.mailbox_store;
    let receipt: String = store
        .connection()
        .unwrap()
        .query_row(
            "SELECT receipt_json FROM mailbox_relay_intents WHERE owner_session_id=?1 AND idempotency_key=?2",
            rusqlite::params![fixture.native, key],
            |row| row.get(0),
        )
        .unwrap();
    let set_receipt = |receipt: Option<&str>| {
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE mailbox_relay_intents SET receipt_json=?1 WHERE owner_session_id=?2 AND idempotency_key=?3",
                rusqlite::params![receipt, fixture.native, key],
            )
            .unwrap()
    };

    // The send is not confirmed yet: planning waits for its receipt.
    assert_eq!(set_receipt(None), 1);
    assert!(store.causal_test_completion_plans().unwrap().is_empty());
    let awaiting_receipt = pending_state(&fixture, key).await;

    // Once the receipt is saved, the next plan is ready, and the row says so
    // before any origin read has answered.
    assert_eq!(set_receipt(Some(&receipt)), 1);
    assert_eq!(store.causal_test_completion_plans().unwrap().len(), 1);
    let ready = pending_state(&fixture, key).await;
    drop(handles);
    server.finish().await;
    fixture.finish().await;

    assert_eq!(awaiting_receipt["reason"], "awaiting_receipt");
    assert_eq!(
        awaiting_receipt["releaseCondition"],
        "original_send_receipt_available"
    );
    assert_eq!(ready["reason"], "awaiting_completion");
    assert_eq!(ready["releaseCondition"], "origin_completion_available");
}

#[tokio::test]
async fn diagnostics_distinguish_store_read_from_route_checks_before_send_and_after_reply() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) =
        super::super::super::causal_test_completion::connect(&fixture).await;
    let (input, _, _) = destination(fixture.central.clone(), &fixture.native);
    let key = "diagnostic-currentness";
    let (_, _, selector) = original(&fixture, key, input).await;
    let mut plans = fixture
        .central
        .mailbox_store
        .causal_test_completion_plans()
        .unwrap();
    assert_eq!(plans.len(), 1);
    let plan = plans.pop().unwrap();
    let set_store_id = |id: &str| {
        fixture
            .central
            .mailbox_store
            .connection()
            .unwrap()
            .execute(
                "UPDATE mailbox_relay_store SET store_id=?1 WHERE singleton=1",
                [id],
            )
            .unwrap()
    };
    assert_eq!(set_store_id("unreadable-fixture-store-id"), 1);
    assert!(
        fixture
            .central
            .read_causal_test_completion_plans(vec![(plan.clone(), None)])
            .await
            .unwrap()
            .is_empty()
    );
    let store_failure = pending_state(&fixture, key).await;
    assert_eq!(set_store_id(&selector.payload.requester.store_id), 1);

    // The RPC invokes the hook before send and after verifying the real reply.
    // Each case chooses which local guard refuses: the hook before anything is
    // sent, the hook after the reply, or the next local lease check after the
    // hook has passed.
    for (refuse_before_send, reject_inside_rpc) in [(true, false), (false, true), (false, false)] {
        fixture
            .central
            .mailbox_store
            .note_causal_pending_for_selector(
                &selector,
                true,
                CausalPendingReason::AwaitingCompletion,
            )
            .unwrap();
        let channel = fixture.channel.clone();
        let checked_channel = channel.clone();
        let central = fixture.central.clone();
        let authority_checks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let checks = authority_checks.clone();
        let rpc = FederationRpcClient::new(
            fixture.server.port,
            channel.clone(),
            Arc::new(move || {
                let check = checks.fetch_add(1, Ordering::SeqCst);
                if check == 0 && refuse_before_send {
                    return Err(federation_stale_link());
                }
                let after_reply = check == 1;
                if after_reply && reject_inside_rpc {
                    central.inner.lock().unwrap().federation_durable = false;
                }
                let result = checked_channel.current_authority().map(|_| ());
                if after_reply {
                    if reject_inside_rpc {
                        assert_eq!(result.as_ref().unwrap_err().status, StatusCode::LOCKED);
                    } else {
                        assert!(result.is_ok());
                        central.inner.lock().unwrap().federation_durable = false;
                    }
                }
                result
            }),
        )
        .unwrap();
        fixture
            .central
            .federation_peer_handles
            .lock()
            .unwrap()
            .insert(
                channel.snapshot.peer.host_id.clone(),
                Arc::new(FederationPeerHandle { channel, rpc }),
            );
        assert!(
            fixture
                .central
                .read_causal_test_completion_plans(vec![(plan.clone(), None)])
                .await
                .unwrap()
                .is_empty()
        );
        let route_failure = pending_state(&fixture, key).await;
        fixture.central.inner.lock().unwrap().federation_durable = true;
        if refuse_before_send {
            // Nothing reached the origin, so this is no origin read failure.
            assert_eq!(authority_checks.load(Ordering::SeqCst), 1);
            assert_eq!(route_failure["reason"], "route_changed");
        } else {
            assert_eq!(authority_checks.load(Ordering::SeqCst), 2);
            assert_eq!(route_failure["reason"], "post_read_route_changed");
        }
        assert_eq!(route_failure["releaseCondition"], "original_route_current");
    }
    drop(handles);
    server.finish().await;
    fixture.finish().await;

    assert_eq!(store_failure["reason"], "currentness_check_failed");
}
