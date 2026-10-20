// Owns observational scan-cache controls on the native executor reader seam.
// Setup may run a native scan; the diagnostic GET must never advance that scan.
use super::*;

fn placed_target(fixture: &JournalFixture) -> FederationMailboxBinding {
    let snapshot = active(&fixture.state, peer(47120));
    let placement = reserve(
        &fixture.state,
        &snapshot,
        "journal-placement",
        "journal-root",
    )
    .unwrap();
    FederationMailboxBinding {
        central: snapshot.local,
        identity: placement.identity,
        placement_generation: placement.placement_generation,
    }
}

// Includes private scan position and window contents, not merely the exported
// summary: an accidental refresh can mutate either without changing counters.
fn cache_snapshot(fixture: &JournalFixture) -> Value {
    let cache = fixture
        .state
        .mailbox_store
        .executor_reservations
        .lock()
        .unwrap();
    json!({"summary": cache.summary(), "scanAfter": cache.scan_after,
        "exhausted": cache.exhausted,
        "domain": cache.domain.as_ref().map(|domain| &domain.image),
        "window": cache.window.iter().map(|(id, (binding, request))|
            json!([id, binding, request])).collect::<Vec<_>>()})
}

#[tokio::test]
async fn federation_journal_exports_last_scan_counters_without_refresh_or_reset() {
    let fixture = JournalFixture::new(true);
    fixture
        .state
        .refresh_federation_executor_reservation_domain()
        .unwrap();
    {
        let mut cache = fixture
            .state
            .mailbox_store
            .executor_reservations
            .lock()
            .unwrap();
        // A partially completed scan's retained observations are not durable
        // totals and must not be reset merely to serve an operator read.
        cache.floor = 31;
        cache.scan_after = 43;
        cache.scanned = 12;
        cache.stale = 4;
        cache.unsupported = 2;
        cache.exhausted = false;
    }
    let before_cache = cache_snapshot(&fixture);
    let before_store = fixture.snapshot();
    let expected = json!({"floorRowid": 31, "scannedLastPass": 12,
        "windowOccupancy": 0, "excluded": {"staleAuthority": 4, "unsupported": 2}});
    for stream in ["operations", "wakes"] {
        let page = fixture
            .get(&format!("?stream={stream}"), StatusCode::OK)
            .await;
        assert_eq!(page["dispatchScan"], expected);
        assert_eq!(cache_snapshot(&fixture), before_cache);
        assert_eq!(fixture.snapshot(), before_store);
    }
}

#[tokio::test]
async fn federation_journal_valid_unscanned_wake_is_unknown_until_native_scan_completes() {
    let fixture = JournalFixture::new(true);
    let target = placed_target(&fixture);
    let (message, _) = fixture.wake_for("valid", target);
    fixture
        .state
        .refresh_federation_executor_reservation_domain()
        .unwrap();
    let before = cache_snapshot(&fixture);
    let page = fixture.get("?stream=wakes", StatusCode::OK).await;
    assert!(
        page["dispatchScan"].is_null(),
        "domain setup is not a completed scan"
    );
    assert_eq!(entries(&page)[0]["messageId"], message);
    assert_eq!(entries(&page)[0]["classificationState"], "unknown");
    assert!(entries(&page)[0]["exclusionReason"].is_null());
    assert_eq!(cache_snapshot(&fixture), before);

    // This explicit SETUP call, rather than the GET, performs the real scan.
    let reserved = fixture
        .state
        .mailbox_store
        .reserve_federation_executor_wakes()
        .unwrap();
    assert_eq!(reserved.len(), 1);
    let after_scan = cache_snapshot(&fixture);
    let page = fixture.get("?stream=wakes", StatusCode::OK).await;
    assert_eq!(entries(&page)[0]["classificationState"], "reserved");
    assert!(entries(&page)[0]["exclusionReason"].is_null());
    assert_eq!(page["dispatchScan"]["windowOccupancy"], 1);
    assert_eq!(cache_snapshot(&fixture), after_scan);
}

#[tokio::test]
async fn federation_journal_native_scan_keeps_central_local_wakes_unknown() {
    let fixture = JournalFixture::new(true);
    let (session, _) = super::super::super::federation_mailboxes::local_participant(
        &fixture.state,
        "Journal local target",
    );
    let local = fixture
        .state
        .federation_mailbox_actor(&session)
        .unwrap()
        .expect("registered local participant has a mailbox actor")
        .binding;
    assert_eq!(local.identity.host_id, local.central.host_id);
    let (local_message, _) = fixture.wake_for("local-scan", local);
    let remote = placed_target(&fixture);
    let (remote_message, _) = fixture.wake_for("remote-scan-control", remote);
    fixture
        .state
        .refresh_federation_executor_reservation_domain()
        .unwrap();
    let reserved = fixture
        .state
        .mailbox_store
        .reserve_federation_executor_wakes()
        .unwrap();
    assert_eq!(reserved.len(), 1);
    assert_eq!(reserved[0].wake.message_id, remote_message);
    let before_cache = cache_snapshot(&fixture);
    let before_store = fixture.snapshot();
    let page = fixture.get("?stream=wakes", StatusCode::OK).await;
    assert_eq!(entries(&page).len(), 2);
    let local_row = entries(&page)
        .iter()
        .find(|row| row["messageId"] == local_message)
        .unwrap();
    assert_eq!(local_row["state"], "pending");
    assert_eq!(local_row["classificationState"], "unknown");
    assert!(local_row["exclusionReason"].is_null());
    assert_eq!(local_row["mirrorPresent"], false);
    let remote_row = entries(&page)
        .iter()
        .find(|row| row["messageId"] == remote_message)
        .unwrap();
    assert_eq!(remote_row["classificationState"], "reserved");
    assert!(remote_row["exclusionReason"].is_null());
    assert_eq!(page["dispatchScan"]["windowOccupancy"], 1);
    assert_eq!(
        page["dispatchScan"]["excluded"],
        json!({"staleAuthority": 0, "unsupported": 0})
    );
    assert_eq!(cache_snapshot(&fixture), before_cache);
    assert_eq!(fixture.snapshot(), before_store);
}

#[tokio::test]
async fn federation_journal_native_scan_exclusions_keep_stale_and_unsupported_distinct() {
    let fixture = JournalFixture::new(true);
    let valid = placed_target(&fixture);
    let mut stale = valid.clone();
    stale.identity.creation_id.push_str("-retired");
    let (stale_message, _) = fixture.wake_for("stale", stale);
    let (unsupported_message, _) = fixture.wake_for("unsupported", valid);
    fixture.state.mailbox_store.connection().unwrap().execute(
        "UPDATE federation_mailbox_wake_obligations SET target_binding_json=?1 WHERE message_id=?2",
        rusqlite::params!["PRIVATE_INVALID_BINDING", unsupported_message],
    ).unwrap();
    fixture
        .state
        .refresh_federation_executor_reservation_domain()
        .unwrap();
    assert!(
        fixture
            .state
            .mailbox_store
            .reserve_federation_executor_wakes()
            .unwrap()
            .is_empty()
    );
    let before_cache = cache_snapshot(&fixture);
    let before_store = fixture.snapshot();
    let page = fixture.get("?stream=wakes", StatusCode::OK).await;
    assert_eq!(entries(&page).len(), 2);
    for (message, reason) in [
        (stale_message, "stale_authority"),
        (unsupported_message, "unsupported"),
    ] {
        let row = entries(&page)
            .iter()
            .find(|row| row["messageId"] == message)
            .unwrap();
        assert_eq!(row["classificationState"], "excluded");
        assert_eq!(row["exclusionReason"], reason);
    }
    assert_eq!(
        page["dispatchScan"]["excluded"],
        json!({"staleAuthority": 1, "unsupported": 1})
    );
    assert_eq!(cache_snapshot(&fixture), before_cache);
    assert_eq!(fixture.snapshot(), before_store);
}

#[tokio::test]
async fn federation_journal_byte_capacity_is_a_wait_reason_not_permanent_staleness() {
    let fixture = JournalFixture::new(true);
    let target = placed_target(&fixture);
    // Reconstruct retained legacy history at the byte quota with a pending
    // obligation. Pressure rows obey real payload-size and digest constraints.
    let mut full = fixture.operation("full-byte-history", false);
    full.payload = "x".repeat(FEDERATION_EXECUTOR_MAX_PAYLOAD);
    fixture.receive(&full);
    let (message, _) = fixture.wake_for("capacity", target);
    let rows = FEDERATION_EXECUTOR_MAX_BYTES / FEDERATION_EXECUTOR_MAX_PAYLOAD as u64;
    fixture
        .state
        .mailbox_store
        .connection()
        .unwrap()
        .execute(
            "WITH RECURSIVE slots(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM slots WHERE n<?1)
         INSERT INTO federation_executor_operations
          (central_host_id,operation_key,central_epoch,target_host_id,enrollment_generation,
           session_id,creation_id,placement_generation,command_kind,payload_digest,payload,
           payload_bytes,stage,effect_state)
         SELECT central_host_id,'full-byte-'||n,central_epoch,target_host_id,enrollment_generation,
           session_id,creation_id,placement_generation,command_kind,payload_digest,payload,
           payload_bytes,stage,effect_state FROM federation_executor_operations,slots
         WHERE operation_key='full-byte-history'",
            [rows - 1],
        )
        .unwrap();
    fixture
        .state
        .refresh_federation_executor_reservation_domain()
        .unwrap();
    assert!(
        fixture
            .state
            .mailbox_store
            .reserve_federation_executor_wakes()
            .unwrap()
            .is_empty()
    );
    let before_cache = cache_snapshot(&fixture);
    let before_store = fixture.snapshot();
    let page = fixture.get("?stream=wakes", StatusCode::OK).await;
    let row = &entries(&page)[0];
    assert_eq!(row["messageId"], message);
    assert_eq!(row["state"], "pending");
    assert_eq!(row["classificationState"], "excluded");
    assert_eq!(row["exclusionReason"], "capacity");
    assert_eq!(
        page["dispatchScan"]["excluded"],
        json!({"staleAuthority": 0, "unsupported": 0})
    );
    assert_eq!(page["dispatchScan"]["windowOccupancy"], 1);
    assert_eq!(cache_snapshot(&fixture), before_cache);
    assert_eq!(fixture.snapshot(), before_store);
}

#[tokio::test]
async fn federation_journal_changed_durable_domain_does_not_reuse_old_scan_classification() {
    let fixture = JournalFixture::new(true);
    let target = placed_target(&fixture);
    fixture.wake_for("domain-change", target.clone());
    fixture
        .state
        .refresh_federation_executor_reservation_domain()
        .unwrap();
    assert_eq!(
        fixture
            .state
            .mailbox_store
            .reserve_federation_executor_wakes()
            .unwrap()
            .len(),
        1
    );
    let current = fixture
        .state
        .federation_peer_snapshot(&target.identity.host_id)
        .unwrap();
    fixture
        .state
        .transition_federation_peer(&current, FederationMembershipState::Revoked)
        .unwrap();
    let before_cache = cache_snapshot(&fixture);
    let before_store = fixture.snapshot();
    let page = fixture.get("?stream=wakes", StatusCode::OK).await;
    assert_eq!(entries(&page)[0]["classificationState"], "unknown");
    assert!(entries(&page)[0]["exclusionReason"].is_null());
    assert_eq!(
        cache_snapshot(&fixture),
        before_cache,
        "GET must not rebuild the old scan"
    );
    assert_eq!(fixture.snapshot(), before_store);
}

#[tokio::test]
async fn federation_journal_full_window_is_a_known_prefix_with_unknown_unscanned_tail() {
    let fixture = JournalFixture::new(true);
    let target = placed_target(&fixture);
    let mut messages = Vec::new();
    for index in 0..=FEDERATION_EXECUTOR_RESERVATION_WINDOW {
        messages.push(
            fixture
                .wake_for(&format!("window-{index}"), target.clone())
                .0,
        );
    }
    fixture
        .state
        .refresh_federation_executor_reservation_domain()
        .unwrap();
    assert_eq!(
        fixture
            .state
            .mailbox_store
            .reserve_federation_executor_wakes()
            .unwrap()
            .len(),
        FEDERATION_EXECUTOR_RESERVATION_WINDOW
    );
    let before_cache = cache_snapshot(&fixture);
    assert_eq!(before_cache["exhausted"], false);
    let before_store = fixture.snapshot();
    let page = fixture.get("?stream=wakes&limit=100", StatusCode::OK).await;
    assert_eq!(entries(&page).len(), 100);
    assert_eq!(
        entries(&page)[0]["messageId"],
        messages[FEDERATION_EXECUTOR_RESERVATION_WINDOW]
    );
    assert_eq!(entries(&page)[0]["classificationState"], "unknown");
    assert!(entries(&page)[0]["exclusionReason"].is_null());
    for row in &entries(&page)[1..] {
        assert_eq!(row["classificationState"], "reserved");
        assert!(row["exclusionReason"].is_null());
    }
    assert_eq!(
        page["dispatchScan"]["windowOccupancy"],
        FEDERATION_EXECUTOR_RESERVATION_WINDOW
    );
    assert_eq!(cache_snapshot(&fixture), before_cache);
    assert_eq!(fixture.snapshot(), before_store);
}
