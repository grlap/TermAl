//! Removed Begin/cancel envelopes are opaque historical data. Real session
//! serialization must retain them without rebuilding transport or proof owners.
use super::*;

fn historical_begin_fields() -> BTreeMap<String, Value> {
    BTreeMap::from([
        (
            "engramBeginRecovery".into(),
            json!({"schemaVersion":1,"envelopeId":"original-operation",
            "revision":41,"phase":"Prepared","payload":{"prompt_id":"original-head",
                "original_text":"whole first line\nżółw 🐢\nlast line","grant_id":"old-grant",
                "idempotency_key":"old-key","unknown":[null,{"future":true}]}}),
        ),
        (
            "engramBeginRecoveryAudit".into(),
            json!([{"revision":41,"diagnostic":"original diagnostic"}]),
        ),
        ("engramBeginRecoveryFormat".into(), json!(true)),
        ("engramBeginOperationId".into(), json!("original-operation")),
        (
            "engramBeginCancelCapture".into(),
            json!({"operationId":"original-operation","revision":41,
            "sessionId":"saved-session","dispatchGeneration":97,"grantId":"old-grant","beginKey":"old-key"}),
        ),
        (
            "engramBeginCancelAudit".into(),
            json!({"decision":"operator_cancelled","revision":41,
            "cancellationRevision":42,"messageId":"saved-cancel-message","originalText":"whole first line\nżółw 🐢\nlast line"}),
        ),
        ("engramUncertainGrantId".into(), json!("old-grant")),
    ])
}

fn historical_record(root: &FsPath) -> SessionRecord {
    let mut inner = StateInner::new();
    let session = inner
        .create_session(
            Agent::Codex,
            None,
            root.to_string_lossy().into_owned(),
            None,
            None,
        )
        .session
        .id;
    inner
        .session_mut(&session)
        .unwrap()
        .engram
        .historical_control = historical_begin_fields();
    // Use the actual persisted projection to obtain an owned record; no old
    // recovery/envelope constructor is present in the fixture.
    PersistedSessionRecord::from_record(inner.session_mut(&session).unwrap())
        .into_record()
        .unwrap()
}

#[test]
fn removed_recovery_fields_preserve_raw_malformed_types_and_unicode_without_interpretation() {
    let root = TestTempRoot::create("historical-begin-values");
    let original = historical_begin_fields();
    for key in original.keys() {
        for raw in [
            Value::Null,
            json!(17),
            json!("damaged żółw\n🐢"),
            json!({"future":[true,null,{}]}),
        ] {
            let mut record = historical_record(root.path());
            record
                .engram
                .historical_control
                .insert(key.clone(), raw.clone());
            let expected = record.engram.historical_control.clone();
            let encoded =
                serde_json::to_value(PersistedSessionRecord::from_record(&record)).unwrap();
            let decoded: PersistedSessionRecord = serde_json::from_value(encoded.clone()).unwrap();
            let loaded = decoded.into_record().unwrap();
            assert_eq!(loaded.engram.historical_control, expected, "{key}");
            assert_eq!(encoded[key], raw);
            assert!(loaded.engram.unadmitted_active_turn.is_none());
            assert!(
                !loaded
                    .engram
                    .historical_control
                    .contains_key("engramOwedBinds")
            );
            assert!(matches!(loaded.runtime, SessionRuntime::None));
        }
    }
}

#[test]
fn conflicting_historical_identity_and_cancel_markers_remain_data_not_terminal_authority() {
    let root = TestTempRoot::create("historical-begin-conflicts");
    for changed in [
        "operation",
        "revision",
        "grant",
        "key",
        "head",
        "message",
        "phase",
    ] {
        let mut record = historical_record(root.path());
        match changed {
            "operation" => record
                .engram
                .historical_control
                .insert("engramBeginOperationId".into(), json!("other-operation")),
            "revision" => record
                .engram
                .historical_control
                .insert("engramBeginRecoveryFormat".into(), json!(u64::MAX)),
            "grant" => record
                .engram
                .historical_control
                .insert("engramUncertainGrantId".into(), json!("other-grant")),
            "key" => record.engram.historical_control.insert(
                "engramBeginCancelCapture".into(),
                json!({"beginKey":"other-key"}),
            ),
            "head" => record.engram.historical_control.insert(
                "engramBeginRecovery".into(),
                json!({"payload":{"original_head":"other-head"}}),
            ),
            "message" => record.engram.historical_control.insert(
                "engramBeginCancelAudit".into(),
                json!({"messageId":"other-message"}),
            ),
            "phase" => record.engram.historical_control.insert(
                "engramBeginRecovery".into(),
                json!({"phase":"OperatorCancelled","settlement":{}}),
            ),
            _ => unreachable!(),
        };
        let expected = record.engram.historical_control.clone();
        let loaded = PersistedSessionRecord::from_record(&record)
            .into_record()
            .unwrap();
        assert_eq!(loaded.engram.historical_control, expected, "{changed}");
        assert!(matches!(loaded.runtime, SessionRuntime::None));
        assert!(loaded.engram.unadmitted_active_turn.is_none());
        assert!(
            !loaded
                .engram
                .historical_control
                .contains_key("engramOwedBinds")
        );
        assert!(!loaded.orchestrator_auto_dispatch_blocked);
    }
}

#[test]
fn historical_recovery_roundtrip_preserves_an_independent_ordinary_successor() {
    let root = TestTempRoot::create("historical-begin-successor");
    let mut record = historical_record(root.path());
    let head: QueuedPromptRecord = serde_json::from_value(json!({"source":"user","attachments":[],
        "promotion_disposition_known":true,"pending_prompt":{"id":"successor-head",
            "timestamp":"2026-10-01T01:02:03Z","text":"successor is preserved"}}))
    .unwrap();
    record.queued_prompts.push_back(head);
    let queue = serde_json::to_value(&record.queued_prompts).unwrap();
    let expected = record.engram.historical_control.clone();
    let loaded = PersistedSessionRecord::from_record(&record)
        .into_record()
        .unwrap();
    assert_eq!(serde_json::to_value(&loaded.queued_prompts).unwrap(), queue);
    assert_eq!(loaded.engram.historical_control, expected);
    assert_eq!(loaded.queued_prompts[0].pending_prompt.id, "successor-head");
    assert_eq!(
        loaded.queued_prompts[0].pending_prompt.text,
        "successor is preserved"
    );
    assert!(loaded.engram.unadmitted_active_turn.is_none());
}

fn stored_messages(connection: &rusqlite::Connection, session: &str) -> Vec<Message> {
    connection
        .prepare("SELECT value_json FROM messages WHERE session_id=?1 ORDER BY position")
        .unwrap()
        .query_map([session], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|row| serde_json::from_str(&row.unwrap()).unwrap())
        .collect()
}

#[test]
fn historical_cancel_readback_keeps_complete_messages_raw_metadata_and_successor() {
    let root = TestTempRoot::create("historical-cancel-readback");
    let mut inner = StateInner::new();
    let session = inner
        .create_session(
            Agent::Codex,
            None,
            root.path().to_string_lossy().into_owned(),
            None,
            None,
        )
        .session
        .id;
    let record = inner.session_mut(&session).unwrap();
    record.engram.historical_control = historical_begin_fields();
    record.queued_prompts.push_back(
        serde_json::from_value(json!({"source":"user","attachments":[],
        "promotion_disposition_known":true,"pending_prompt":{"id":"successor-head",
            "timestamp":"2026-10-01T01:02:03Z","text":"successor is preserved"}}))
        .unwrap(),
    );
    for ordinal in 0..130 {
        record.session.messages.push(Message::Text {
            attachments: Vec::new(),
            id: if ordinal == 0 {
                "saved-cancel-message".into()
            } else {
                format!("later-{ordinal}")
            },
            timestamp: "2026-10-01T01:02:06Z".into(),
            author: Author::System,
            text: if ordinal == 0 {
                "whole first line\nżółw 🐢\nlast line".into()
            } else {
                format!("later message {ordinal}")
            },
            expanded_text: None,
            source: None,
        });
    }
    record.session.message_count = 130;
    record.session.messages_loaded = true;
    record.message_positions = build_message_positions(&record.session.messages);
    let expected_messages = serde_json::to_value(&record.session.messages).unwrap();
    let expected_queue = serde_json::to_value(&record.queued_prompts).unwrap();
    let expected_history = record.engram.historical_control.clone();
    let path = root.path().join("proof.sqlite");
    persist_state(&path, &inner).unwrap();
    let connection = open_sqlite_state_connection(&path).unwrap();
    let encoded: String = connection
        .query_row(
            "SELECT value_json FROM sessions WHERE id=?1",
            [&session],
            |row| row.get(0),
        )
        .unwrap();
    let raw: Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(raw["session"]["messages"], json!([]));
    assert_eq!(raw["queuedPrompts"], expected_queue);
    for (key, value) in &expected_history {
        assert_eq!(&raw[key], value, "{key}");
    }
    let complete = stored_messages(&connection, &session);
    assert_eq!(complete.len(), 130);
    assert_eq!(serde_json::to_value(&complete).unwrap(), expected_messages);
    let mut resident: PersistedSessionRecord = serde_json::from_value(raw.clone()).unwrap();
    load_persisted_session_tail(&connection, &path, &mut resident).unwrap();
    assert_eq!(resident.session.messages.len(), 64);
    assert!(
        !resident
            .session
            .messages
            .iter()
            .any(|message| message.id() == "saved-cancel-message")
    );
    assert_eq!(
        resident.historical_control, expected_history,
        "a partial tail cannot reinterpret cancellation data"
    );
    connection
        .execute(
            "DELETE FROM messages WHERE session_id=?1 AND message_id=?2",
            rusqlite::params![session, "saved-cancel-message"],
        )
        .unwrap();
    assert_eq!(stored_messages(&connection, &session).len(), 129);
    let after: String = connection
        .query_row(
            "SELECT value_json FROM sessions WHERE id=?1",
            [&session],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        after, encoded,
        "message removal does not fabricate or rewrite historical terminal evidence"
    );
}
