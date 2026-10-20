// Owns HTTP-level redaction, bounded paging and operator-surface regressions.
// Does not prove dispatch, runtime effects or a live cross-host exchange.
use super::*;

#[path = "federation_journal_fixture.rs"]
mod fixture;
use fixture::*;

#[path = "federation_journal_wakes.rs"]
mod wakes;

#[path = "federation_journal_scan.rs"]
mod scan;

#[tokio::test]
async fn federation_journal_empty_read_is_available_without_enabling_or_writing() {
    let fixture = JournalFixture::new(false);
    let before = fixture.snapshot();
    let page = fixture.get("", StatusCode::OK).await;
    assert_eq!(page["stream"], "operations");
    assert!(entries(&page).is_empty());
    assert!(page["nextCursor"].is_null());
    assert!(page["dispatchScan"].is_null());
    assert_eq!(fixture.get("", StatusCode::OK).await, page);
    assert_eq!(
        fixture.snapshot(),
        before,
        "GET must not persist, dispatch or enable"
    );
}

#[tokio::test]
async fn federation_journal_projects_exact_metadata_and_redacts_all_retained_content() {
    let fixture = JournalFixture::new(true);
    let operation = fixture.operation("metadata", false);
    fixture.receive(&operation);
    fixture.queued(&operation);
    fixture.evidence(
        &operation,
        FederationExecutorEvidence::EffectAttempt {
            delivery_attempt_id: "PRIVATE_ATTEMPT_CANARY".into(),
        },
    );
    fixture.evidence(
        &operation,
        FederationExecutorEvidence::RuntimeAccepted {
            delivery_attempt_id: "PRIVATE_ATTEMPT_CANARY".into(),
            acknowledgement: "PRIVATE_RUNTIME_ACK_CANARY".into(),
        },
    );
    fixture.evidence(
        &operation,
        FederationExecutorEvidence::Processed {
            kind: FederationExecutorProcessedKind::MailboxAck,
            evidence: "PRIVATE_PROCESSED_EVIDENCE_CANARY".into(),
        },
    );
    let before = fixture.snapshot();
    let page = fixture.get("", StatusCode::OK).await;
    let rows = entries(&page);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        sorted_keys(&rows[0]),
        [
            "commandKind",
            "deliveryAttemptPresent",
            "effectState",
            "enrollmentGeneration",
            "mirror",
            "operationKey",
            "processedEvidenceKind",
            "queueEntryPresent",
            "runtimeAckPresent",
            "stage",
            "targetBinding"
        ]
    );
    assert_eq!(
        rows[0],
        json!({"operationKey": "metadata", "commandKind": "wake",
        "stage": "processed", "effectState": "confirmed", "mirror": false,
        "queueEntryPresent": true, "deliveryAttemptPresent": true,
        "runtimeAckPresent": true, "processedEvidenceKind": "mailbox_ack",
        "enrollmentGeneration": 3, "targetBinding": fixture.target(&operation)})
    );
    let encoded = serde_json::to_string(&page).unwrap();
    for canary in [
        "PRIVATE_",
        "payload",
        "\"runtimeAck\":",
        "\"processedEvidence\":",
        "queueEntryId",
        "deliveryAttemptId",
        "payloadDigest",
    ] {
        assert!(!encoded.contains(canary), "must not expose {canary}");
    }
    assert_eq!(fixture.snapshot(), before);
}

#[tokio::test]
async fn federation_journal_evidence_presence_tracks_retained_columns_without_values() {
    let fixture = JournalFixture::new(false);
    let operation = fixture.operation("evidence-presence", false);
    fixture.receive(&operation);
    for (evidence, stage, queued, runtime, processed) in [
        (None, "received", false, false, None),
        (
            Some(FederationExecutorEvidence::Queued {
                queue_entry_id: "PRIVATE_QUEUE_CANARY".into(),
                delivery_attempt_id: "PRIVATE_ATTEMPT_CANARY".into(),
            }),
            "queued",
            true,
            false,
            None,
        ),
        (
            Some(FederationExecutorEvidence::EffectAttempt {
                delivery_attempt_id: "PRIVATE_ATTEMPT_CANARY".into(),
            }),
            "queued",
            true,
            false,
            None,
        ),
        (
            Some(FederationExecutorEvidence::RuntimeAccepted {
                delivery_attempt_id: "PRIVATE_ATTEMPT_CANARY".into(),
                acknowledgement: "PRIVATE_RUNTIME_ACK_CANARY".into(),
            }),
            "runtime_accepted",
            true,
            true,
            None,
        ),
        (
            Some(FederationExecutorEvidence::Processed {
                kind: FederationExecutorProcessedKind::MailboxAck,
                evidence: "PRIVATE_PROCESSED_EVIDENCE_CANARY".into(),
            }),
            "processed",
            true,
            true,
            Some("mailbox_ack"),
        ),
    ] {
        if let Some(evidence) = evidence {
            fixture.evidence(&operation, evidence);
        }
        let before = fixture.snapshot();
        let page = fixture.get("", StatusCode::OK).await;
        let row = &entries(&page)[0];
        assert_eq!(row["stage"], stage);
        assert_eq!(row["queueEntryPresent"], json!(queued));
        assert_eq!(row["deliveryAttemptPresent"], json!(queued));
        assert_eq!(row["runtimeAckPresent"], json!(runtime));
        assert!(row
            .as_object()
            .unwrap()
            .contains_key("processedEvidenceKind"));
        assert_eq!(row["processedEvidenceKind"], json!(processed));
        assert!(!serde_json::to_string(&page).unwrap().contains("PRIVATE_"));
        assert_eq!(
            fixture.snapshot(),
            before,
            "projection must not retain evidence"
        );
    }
}

#[tokio::test]
async fn federation_journal_metadata_read_does_not_decode_opaque_payload_bytes() {
    let fixture = JournalFixture::new(false);
    fixture.seed_opaque_payload();
    let before = fixture.snapshot();
    let page = fixture.get("", StatusCode::OK).await;
    assert_eq!(entries(&page).len(), 1);
    assert_eq!(entries(&page)[0]["operationKey"], "opaque");
    assert_eq!(fixture.snapshot(), before);
}

#[tokio::test]
async fn federation_journal_mirror_depends_on_target_host_not_stage_or_local_role() {
    for is_central in [false, true] {
        let fixture = JournalFixture::new(is_central);
        let local = fixture.operation("local", false);
        let remote = fixture.operation("remote", true);
        fixture.receive(&local);
        fixture.receive(&remote);
        fixture.queued(&local);
        let page = fixture.get("", StatusCode::OK).await;
        assert_eq!(entries(&page).len(), 2);
        assert_eq!(entries(&page)[0]["operationKey"], "remote");
        assert_eq!(entries(&page)[0]["mirror"], true);
        assert_eq!(entries(&page)[1]["operationKey"], "local");
        assert_eq!(entries(&page)[1]["mirror"], false);
        assert_eq!(entries(&page)[1]["stage"], "queued");
    }
}

#[tokio::test]
async fn federation_journal_history_keeps_each_kind_and_does_not_upgrade_unknown_effects() {
    let fixture = JournalFixture::new(false);
    let wake = fixture.operation("received-wake", false);
    fixture.receive(&wake);
    let mut start = fixture.operation("queued-start", false);
    start.command_kind = "start".into();
    fixture.receive(&start);
    fixture.queued(&start);
    let mut stop = fixture.operation("processed-stop", false);
    stop.command_kind = "stop".into();
    fixture.receive(&stop);
    fixture.queued(&stop);
    fixture.evidence(
        &stop,
        FederationExecutorEvidence::EffectAttempt {
            delivery_attempt_id: "PRIVATE_ATTEMPT_CANARY".into(),
        },
    );
    fixture.evidence(
        &stop,
        FederationExecutorEvidence::Processed {
            kind: FederationExecutorProcessedKind::CommandOutcome,
            evidence: "PRIVATE_COMMAND_OUTCOME_CANARY".into(),
        },
    );
    let page = fixture.get("", StatusCode::OK).await;
    let rows = entries(&page);
    assert_eq!(rows.len(), 3);
    for (index, key, kind, stage, effect) in [
        (0, "processed-stop", "stop", "processed", "unknown"),
        (1, "queued-start", "start", "queued", "not_started"),
        (2, "received-wake", "wake", "received", "not_started"),
    ] {
        assert_eq!(rows[index]["operationKey"], key);
        assert_eq!(rows[index]["commandKind"], kind);
        assert_eq!(rows[index]["stage"], stage);
        assert_eq!(rows[index]["effectState"], effect);
    }
    assert_eq!(rows[0]["queueEntryPresent"], true);
    assert_eq!(rows[0]["deliveryAttemptPresent"], true);
    assert_eq!(rows[0]["runtimeAckPresent"], false);
    assert_eq!(rows[0]["processedEvidenceKind"], "command_outcome");
    assert_eq!(rows[1]["queueEntryPresent"], true);
    assert_eq!(rows[1]["deliveryAttemptPresent"], true);
    assert_eq!(rows[1]["runtimeAckPresent"], false);
    assert!(rows[1]["processedEvidenceKind"].is_null());
    assert_eq!(rows[2]["queueEntryPresent"], false);
    assert_eq!(rows[2]["deliveryAttemptPresent"], false);
    assert_eq!(rows[2]["runtimeAckPresent"], false);
    assert!(rows[2]["processedEvidenceKind"].is_null());
    assert!(!serde_json::to_string(&page).unwrap().contains("PRIVATE_"));
}

#[tokio::test]
async fn federation_journal_satellite_retains_local_diagnostics_but_refuses_wake_stream() {
    let fixture = JournalFixture::new(false);
    let version = operator::operator_view(&fixture.state).await["version"].clone();
    operator::operator_configure(
        &fixture.state,
        json!({
            "enabled": true, "role": "satellite", "expectedVersion": version,
            "central": {"hostId": "33333333-3333-4333-8333-333333333333", "hostGeneration": 1},
        }),
        StatusCode::OK,
    )
    .await;
    fixture.receive(&fixture.operation("satellite-operation", false));
    let before = fixture.snapshot();
    let page = fixture.get("", StatusCode::OK).await;
    assert_eq!(entries(&page).len(), 1);
    assert!(page["dispatchScan"].is_null());
    fixture.get("?stream=wakes", StatusCode::CONFLICT).await;
    assert_eq!(fixture.snapshot(), before);
}

#[tokio::test]
async fn federation_journal_paging_is_newest_first_bounded_and_pinned_across_appends() {
    let fixture = JournalFixture::new(true);
    for index in 0..205 {
        // Deliberately invert lexical key order and insertion order.
        fixture.receive(&fixture.operation(&format!("key-{:03}", 999 - index), false));
    }
    let oldest = fixture.operation("key-999", false);
    fixture.queued(&oldest); // Updating a row must not make it newest.
    let before = fixture.snapshot();
    let first = fixture.get("?limit=100", StatusCode::OK).await;
    assert_eq!(entries(&first).len(), 100);
    assert_eq!(entries(&first)[0]["operationKey"], "key-795");
    assert_eq!(entries(&first)[99]["operationKey"], "key-894");
    assert_eq!(fixture.snapshot(), before);
    fixture.receive(&fixture.operation("new-after-first-page", false));
    let second = fixture
        .get(&continuation(&first, "operations"), StatusCode::OK)
        .await;
    assert_eq!(entries(&second).len(), 100);
    let third = fixture
        .get(&continuation(&second, "operations"), StatusCode::OK)
        .await;
    assert_eq!(entries(&third).len(), 5);
    assert!(third["nextCursor"].is_null());
    let keys: Vec<_> = [&first, &second, &third]
        .into_iter()
        .flat_map(entries)
        .map(|row| row["operationKey"].as_str().unwrap())
        .collect();
    assert_eq!(keys.len(), 205);
    assert_eq!(keys.iter().copied().collect::<HashSet<_>>().len(), 205);
    assert_eq!(keys.last(), Some(&"key-999"));
    assert!(!keys.contains(&"new-after-first-page"));
    let fresh = fixture.get("?limit=1", StatusCode::OK).await;
    assert_eq!(entries(&fresh)[0]["operationKey"], "new-after-first-page");
}

#[tokio::test]
async fn federation_journal_small_page_limit_is_respected_without_an_unbounded_tail() {
    let fixture = JournalFixture::new(false);
    for key in ["a", "z", "b"] {
        fixture.receive(&fixture.operation(key, false));
    }
    let page = fixture.get("?limit=2", StatusCode::OK).await;
    assert_eq!(entries(&page).len(), 2);
    assert_eq!(entries(&page)[0]["operationKey"], "b");
    assert_eq!(entries(&page)[1]["operationKey"], "z");
    let next = fixture
        .get(&continuation(&page, "operations"), StatusCode::OK)
        .await;
    assert_eq!(entries(&next).len(), 1);
    assert_eq!(entries(&next)[0]["operationKey"], "a");
    assert!(next["nextCursor"].is_null());
}

#[tokio::test]
async fn federation_journal_invalid_limits_streams_and_cursors_are_visible_refusals() {
    let fixture = JournalFixture::new(false);
    for query in [
        "?limit=0",
        "?limit=101",
        "?limit=-1",
        "?limit=wat",
        "?stream=payloads",
        "?cursor=not-a-cursor",
        "?unexpected=value",
    ] {
        let error = fixture.get(query, StatusCode::BAD_REQUEST).await;
        assert!(error["error"].is_string(), "named refusal for {query}");
        assert!(
            error.get("entries").is_none(),
            "invalid input is not an empty success"
        );
    }
    let oversized = format!("?cursor={}", "a".repeat(2049));
    fixture.get(&oversized, StatusCode::BAD_REQUEST).await;
}

#[tokio::test]
async fn federation_journal_cursor_cannot_be_retargeted_to_another_host_or_stream() {
    let first = JournalFixture::new(true);
    let other = JournalFixture::new(true);
    for key in ["a", "b"] {
        first.receive(&first.operation(key, false));
    }
    let page = first.get("?limit=1", StatusCode::OK).await;
    let query = continuation(&page, "operations");
    other.get(&query, StatusCode::CONFLICT).await;
    first
        .get(&continuation(&page, "wakes"), StatusCode::BAD_REQUEST)
        .await;
}

#[tokio::test]
async fn federation_journal_does_not_mount_on_pair_listener_or_allow_writes() {
    let fixture = JournalFixture::new(true);
    fixture.get("", StatusCode::OK).await; // Positive local route control.
    assert_journal_absent_from_pair_listener().await;
    let response = request_response(
        &app_router(fixture.state.clone()),
        Request::builder()
            .method("POST")
            .uri("/api/federation/journal")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn federation_journal_missing_previous_anchor_refuses_with_restart_instead_of_skipping() {
    let fixture = JournalFixture::new(false);
    for key in ["oldest", "previous", "high-water"] {
        fixture.receive(&fixture.operation(key, false));
    }
    let first = fixture.get("?limit=2", StatusCode::OK).await;
    fixture
        .state
        .mailbox_store
        .connection()
        .unwrap()
        .execute(
            "DELETE FROM federation_executor_operations WHERE operation_key='previous'",
            [],
        )
        .unwrap();
    let error = fixture
        .get(&continuation(&first, "operations"), StatusCode::CONFLICT)
        .await;
    assert!(error["error"].as_str().unwrap().contains("restart"));
    assert!(error.get("entries").is_none());
}

#[tokio::test]
async fn federation_journal_reused_high_water_rowid_requires_the_same_primary_identity() {
    let fixture = JournalFixture::new(false);
    for key in ["oldest", "previous", "high-water"] {
        fixture.receive(&fixture.operation(key, false));
    }
    let first = fixture.get("?limit=2", StatusCode::OK).await;
    let old_rowid: i64 = fixture
        .state
        .mailbox_store
        .connection()
        .unwrap()
        .query_row(
            "SELECT rowid FROM federation_executor_operations WHERE operation_key='high-water'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    fixture
        .state
        .mailbox_store
        .connection()
        .unwrap()
        .execute(
            "DELETE FROM federation_executor_operations WHERE operation_key='high-water'",
            [],
        )
        .unwrap();
    fixture.receive(&fixture.operation("replacement-identity", false));
    let new_rowid: i64 = fixture.state.mailbox_store.connection().unwrap().query_row(
        "SELECT rowid FROM federation_executor_operations WHERE operation_key='replacement-identity'",
        [], |row| row.get(0),
    ).unwrap();
    assert_eq!(new_rowid, old_rowid, "arrange native SQLite rowid reuse");
    let error = fixture
        .get(&continuation(&first, "operations"), StatusCode::CONFLICT)
        .await;
    assert!(error["error"].as_str().unwrap().contains("restart"));
    assert!(error.get("entries").is_none());
}

#[tokio::test]
async fn federation_journal_storage_failure_is_not_replaced_with_empty_success() {
    let fixture = JournalFixture::new(false);
    fixture.receive(&fixture.operation("existing", false));
    fixture
        .state
        .mailbox_store
        .connection()
        .unwrap()
        .execute_batch(
            "ALTER TABLE federation_executor_operations RENAME TO unavailable_executor_rows",
        )
        .unwrap();
    let error = fixture.get("", StatusCode::INTERNAL_SERVER_ERROR).await;
    assert!(error["error"].is_string());
    assert!(error.get("entries").is_none());
}
