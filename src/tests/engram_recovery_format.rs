//! Historical recovery metadata is inert when ordinary queued content loads.
//! Executable recovery envelopes and their admission protocol are no longer owned here.
use super::*;

#[test]
fn historical_recovery_metadata_cannot_replace_or_pause_ordinary_queued_content() {
    let root = TestTempRoot::create("historical-recovery-queue");
    let mut inner = StateInner::new();
    let id = inner
        .create_session(
            Agent::Codex,
            None,
            root.path().to_string_lossy().into_owned(),
            None,
            None,
        )
        .session
        .id;
    let index = inner.find_session_index(&id).unwrap();
    let record = &mut inner.sessions[index];
    for (id, text) in [
        ("ordinary-first", "Original first line\nsecond line\nżółw 🐢"),
        ("ordinary-next", "A distinct queued successor"),
    ] {
        record.queued_prompts.push_back(
            serde_json::from_value(json!({
                "source": "user",
                "attachments": [],
                "promoted_message_index": 7,
                "promotion_disposition_known": true,
                "pending_prompt": {
                    "id": id,
                    "timestamp": "2026-10-01T01:02:03Z",
                    "text": text
                }
            }))
            .unwrap(),
        );
    }
    let expected = record
        .queued_prompts
        .iter()
        .map(|queued| {
            (
                serde_json::to_value(&queued.pending_prompt).unwrap(),
                queued.source,
                queued.promoted_message_index,
                queued.promotion_disposition_known,
            )
        })
        .collect::<Vec<_>>();
    let row = serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap();
    let mut historical = row.clone();
    let mut expected_history = BTreeMap::new();
    for field in [
        "engramBeginRecovery",
        "engramBeginRecoveryAudit",
        "engramBeginRecoveryFormat",
        "engramBeginOperationId",
        "engramBeginCancelCapture",
        "engramBeginCancelAudit",
    ] {
        assert!(row.get(field).is_none(), "ordinary persistence adds no {field}");
        let value = json!({
            "unknown": "Historical operation; application remains unknown",
            "promptId": "a-different-historical-prompt",
            "revision": 41
        });
        historical[field] = value.clone();
        expected_history.insert(field.to_owned(), value);
    }
    for (field, value) in [
        ("engramRoutingToken", json!("historical-token-not-a-current-route")),
        ("engramOpenGrantId", json!("historical-grant")),
        ("engramUncertainGrantId", json!("historical-grant")),
        ("engramBeginsRecorded", json!(true)),
        ("engramDispatchGeneration", json!(17)),
        ("engramPendingBatches", json!([
            {
                "checkpointId": "original-checkpoint-key",
                "outcome": "unknown",
                "futureProducerFields": {
                    "opaque": [null, false, {"text": "Original late facts\nżółw 🐢"}],
                    "verificationRecord": "0123456789abcdef0123456789abcdef"
                }
            },
            {"unrecognizedBatchKind": {"attempts": [1, 2], "applied": null}}
        ])),
        ("engramCheckpointOperation", json!({
            "original": {
                "id": "historical-checkpoint-key",
                "request": {
                    "operation": "turn_checkpoint",
                    "routing_token": "historical-token-not-a-current-route",
                    "grant_id": "historical-grant",
                    "idempotency_key": "historical-checkpoint-key",
                    "opaque": {"text": "Do not reconstruct this request\nżółw 🐢"}
                }
            },
            "attempts": [{"result": {"kind": "unknown", "detail": "reply was lost"}}]
        })),
    ] {
        assert!(row.get(field).is_none(), "a new ordinary row adds no {field}");
        historical[field] = value.clone();
        expected_history.insert(field.to_owned(), value);
    }
    for (encoded, retained) in [(row, false), (historical, true)] {
        let persisted: PersistedSessionRecord = serde_json::from_value(encoded).unwrap();
        let restored = persisted.into_record().unwrap();
        assert!(!restored.orchestrator_auto_dispatch_blocked);
        assert!(!restored.session.queue_paused);
        let actual = restored
            .queued_prompts
            .iter()
            .map(|queued| {
                (
                    serde_json::to_value(&queued.pending_prompt).unwrap(),
                    queued.source,
                    queued.promoted_message_index,
                    queued.promotion_disposition_known,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, expected, "history cannot replace or reorder queued content");
        if retained {
            let roundtrip = serde_json::to_value(PersistedSessionRecord::from_record(&restored))
                .unwrap();
            for (field, original) in &expected_history {
                assert_eq!(restored.engram.historical_control.get(field), Some(original));
                assert_eq!(roundtrip.get(field), Some(original),
                    "original camelCase history survives without becoming runtime authority: {field}");
            }
        } else {
            assert!(restored.engram.historical_control.is_empty());
        }
    }
}
