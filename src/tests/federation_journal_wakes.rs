// Owns central wake projections against durable obligation and mirror rows.
// Does not settle ownership, consume acknowledgements or refresh dispatch scans.
use super::*;

#[tokio::test]
async fn federation_journal_wakes_keep_durable_state_separate_from_exact_mirror_presence() {
    let fixture = JournalFixture::new(true);
    let (pending, mut wrong_domain) = fixture.wake("pending");
    wrong_domain.central_host_id = "44444444-4444-4444-8444-444444444444".into();
    fixture.receive(&wrong_domain);
    let (settled, mirror) = fixture.wake("settled");
    fixture.receive(&mirror);
    fixture
        .state
        .mailbox_store
        .connection()
        .unwrap()
        .execute(
            "UPDATE federation_mailbox_wake_obligations SET state='settled' WHERE message_id=?1",
            [&settled],
        )
        .unwrap();
    let before = fixture.snapshot();
    let page = fixture.get("?stream=wakes", StatusCode::OK).await;
    assert_eq!(page["stream"], "wakes");
    let rows = entries(&page);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        sorted_keys(&rows[0]),
        [
            "classificationState",
            "exclusionReason",
            "messageId",
            "mirrorPresent",
            "state",
            "targetBinding"
        ]
    );
    assert_eq!(rows[0]["messageId"], settled);
    assert_eq!(rows[0]["state"], "settled");
    assert_eq!(rows[0]["mirrorPresent"], true);
    assert!(rows[0]["exclusionReason"].is_null());
    assert_eq!(rows[0]["targetBinding"], fixture.target(&mirror));
    assert_eq!(rows[1]["messageId"], pending);
    assert_eq!(rows[1]["state"], "pending");
    assert_eq!(
        rows[1]["mirrorPresent"], false,
        "mirror join must include central identity"
    );
    let encoded = serde_json::to_string(&page).unwrap();
    assert!(!encoded.contains("PRIVATE_"));
    for forbidden in [
        "payload",
        "body",
        "topic",
        "stateStamp",
        "runtimeAck",
        "processedEvidence",
        "senderName",
        "targetName",
    ] {
        assert!(
            !encoded.contains(forbidden),
            "wake projection leaks {forbidden}"
        );
    }
    assert_eq!(fixture.snapshot(), before);
}

#[tokio::test]
async fn federation_journal_wakes_use_a_separate_bounded_newest_first_continuation() {
    let fixture = JournalFixture::new(true);
    let mut inserted = Vec::new();
    for index in 0..101 {
        inserted.push(fixture.wake(&format!("wake-{index}")).0);
    }
    let first = fixture.get("?stream=wakes&limit=100", StatusCode::OK).await;
    assert_eq!(entries(&first).len(), 100);
    assert_eq!(entries(&first)[0]["messageId"], inserted[100]);
    assert_eq!(entries(&first)[99]["messageId"], inserted[1]);
    let next = fixture
        .get(&continuation(&first, "wakes"), StatusCode::OK)
        .await;
    assert_eq!(entries(&next).len(), 1);
    assert_eq!(entries(&next)[0]["messageId"], inserted[0]);
    assert!(next["nextCursor"].is_null());
    fixture
        .get(&continuation(&first, "operations"), StatusCode::BAD_REQUEST)
        .await;
}

#[tokio::test]
async fn federation_journal_uninitialized_scan_is_unknown_and_get_does_not_initialize_it() {
    let fixture = JournalFixture::new(true);
    let (message, _) = fixture.wake("unscanned");
    let before = fixture.snapshot();
    for _ in 0..2 {
        let page = fixture.get("?stream=wakes", StatusCode::OK).await;
        assert!(
            page["dispatchScan"].is_null(),
            "no scan occurred in this process"
        );
        assert_eq!(entries(&page)[0]["messageId"], message);
        assert_eq!(entries(&page)[0]["classificationState"], "unknown");
        assert!(
            entries(&page)[0]["exclusionReason"].is_null(),
            "missing scan is neither capacity nor a permanent exclusion"
        );
    }
    assert_eq!(fixture.snapshot(), before);
}

#[tokio::test]
async fn federation_journal_unsupported_wake_metadata_never_exposes_raw_binding_text() {
    let fixture = JournalFixture::new(true);
    let (message, _) = fixture.wake("bad-binding");
    fixture.state.mailbox_store.connection().unwrap().execute(
        "UPDATE federation_mailbox_wake_obligations SET target_binding_json=?1 WHERE message_id=?2",
        rusqlite::params!["PRIVATE_MALFORMED_BINDING_CANARY", message],
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
    let before = fixture.snapshot();
    let page = fixture.get("?stream=wakes", StatusCode::OK).await;
    let row = &entries(&page)[0];
    assert_eq!(row["messageId"], message);
    assert_eq!(row["classificationState"], "excluded");
    assert_eq!(row["exclusionReason"], "unsupported");
    assert!(row["targetBinding"].is_null());
    assert!(!serde_json::to_string(&page).unwrap().contains("PRIVATE_"));
    assert_eq!(fixture.snapshot(), before);
}
