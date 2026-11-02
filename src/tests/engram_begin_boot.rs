//! Restoring a queue reads old control requests as data, never as work to send.
use super::*;

fn saved_unknown_head(turn: &OrdinaryRoot) -> (String, Value) {
    let id = turn.enqueue("original queued content żółw", QueuedPromptSource::User);
    turn.edit(|record| {
        let mut raw = serde_json::to_value(&record.queued_prompts[0]).unwrap();
        // The ordinary handoff marker owns delivery uncertainty. A complete
        // transcript still proves that this id has no promoted message row.
        raw["engram_ordinary_handoff"] = json!(true);
        raw["promotion_disposition_known"] = json!(true);
        raw["engram_evaluate"] = json!({"connection":{"session":"original"},
            "request":{"operation":"turn_evaluate","idempotency_key":"old-evaluate"},
            "begun_grant_id":null,"prepared_begin":{"grant_id":"old-grant",
                "idempotency_key":"old-begin","delivery_tokens":["old-token"],"unknown":[null,"🐢"]}});
        record.queued_prompts[0] = serde_json::from_value(raw.clone()).unwrap();
        raw
    });
    (id, turn.record(|record| serde_json::to_value(&record.queued_prompts[0]).unwrap()))
}

#[test]
fn restoring_unknown_delivery_preserves_the_whole_request_without_a_live_owner() {
    let turn = OrdinaryRoot::new("historical-begin-boot");
    let (id, head) = saved_unknown_head(&turn);
    let lineage = json!({"admittedAt":"2026-10-01T01:02:03Z","deadline":"2026-10-01T01:06:03Z",
        "heldSince":"2026-10-01T01:02:04Z","attempts":7,"ended":true,"unknown":{"exact":[true,null]}});
    turn.edit(|record| {
        record.engram.historical_control.insert("engramBeginPreparationLineage".into(), lineage.clone());
        record.engram.historical_control.insert("engramUncertainGrantId".into(), json!("old-grant"));
    });
    for _ in 0..2 {
        let loaded = turn.record(|record| PersistedSessionRecord::from_record(record).into_record().unwrap());
        assert_eq!(serde_json::to_value(&loaded.queued_prompts[0]).unwrap(), head);
        assert_eq!(loaded.queued_prompts[0].pending_prompt.id, id);
        assert_eq!(loaded.engram.historical_control["engramBeginPreparationLineage"], lineage);
        assert_eq!(loaded.engram.historical_control["engramUncertainGrantId"], json!("old-grant"));
        assert!(ordinary_queue_position(&loaded).is_none(), "unknown provider delivery is never replayed");
        assert!(matches!(loaded.runtime, SessionRuntime::None));
        assert!(!loaded.engram.historical_control.contains_key("engramOwedBinds"));
        assert!(loaded.engram.unadmitted_active_turn.is_none());
    }
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn absent_or_malformed_old_retry_metadata_does_not_reconstruct_a_scheduler() {
    for raw in [Value::Null, json!(false), json!(17), json!("old format"), json!({"dueAt":"past","attempts":u64::MAX})] {
        let turn = OrdinaryRoot::new("historical-retry-shapes");
        let (_, head) = saved_unknown_head(&turn);
        turn.edit(|record| {
            record.engram.historical_control.insert("engramAdmissionRetry".into(), raw.clone());
            record.engram.historical_control.insert("engramBeginRecorded".into(), json!(false));
        });
        let loaded = turn.record(|record| PersistedSessionRecord::from_record(record).into_record().unwrap());
        assert_eq!(loaded.engram.historical_control["engramAdmissionRetry"], raw);
        assert_eq!(serde_json::to_value(&loaded.queued_prompts[0]).unwrap(), head);
        assert!(ordinary_queue_position(&loaded).is_none());
        assert!(loaded.provider_capacity_retry.is_none(), "old admission records never become runtime retries");
        turn.assert_no_control();
    }
}

#[test]
fn historical_unknown_head_does_not_hide_an_independent_ordinary_successor() {
    let turn = OrdinaryRoot::new("historical-head-successor");
    let (_, history) = saved_unknown_head(&turn);
    let successor = turn.enqueue("independent ordinary successor", QueuedPromptSource::User);
    let loaded = turn.record(|record| PersistedSessionRecord::from_record(record).into_record().unwrap());
    let position = ordinary_queue_position(&loaded).expect("the independent successor is runnable");
    assert_eq!(position, 1);
    assert_eq!(loaded.queued_prompts[position].pending_prompt.id, successor);
    assert_eq!(serde_json::to_value(&loaded.queued_prompts[0]).unwrap(), history);
    assert!(matches!(loaded.runtime, SessionRuntime::None));
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn sqlite_reload_keeps_original_head_lineage_and_operator_pause_as_data() {
    let turn = OrdinaryRoot::new("historical-begin-sqlite");
    let (_, head) = saved_unknown_head(&turn);
    turn.edit(|record| {
        record.engram.operator_paused = true;
        record.set_auto_dispatch_blocked(true);
        record.engram.historical_control.insert("engramAbortRetry".into(), json!({"attempts":3,"code":"storage_error"}));
    });
    let path = turn.state.persistence_path.as_path();
    {
        let inner = turn.state.inner.lock().unwrap();
        persist_state(path, &inner).unwrap();
    }
    let loaded = load_state(path).unwrap().unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&turn.session).unwrap()];
    assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), head);
    assert!(record.engram.operator_paused && record.orchestrator_auto_dispatch_blocked);
    assert_eq!(record.engram.historical_control["engramAbortRetry"]["attempts"], 3);
    assert!(ordinary_queue_position(record).is_none());
    turn.assert_no_prompt();
    turn.assert_no_control();
}
