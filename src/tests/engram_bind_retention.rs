//! Persisted queued-bind images stay passive. The removed non-admission
//! retained original was process memory only; no stored owner is invented.
use super::*;
use super::uncertain_begin::OrdinaryRoot;

fn install_queued_bind_history(turn: &OrdinaryRoot) -> Value {
    turn.enqueue("historical queued payload", QueuedPromptSource::User);
    turn.edit(|record| {
        let mut image = serde_json::to_value(&record.queued_prompts[0]).unwrap();
        image["promotion_disposition_known"] = json!(false);
        image["engram_bind"] = json!({"request":{"operation":"session_bind","title":"original title",
            "idempotency_key":"original-bind-key","work_binding":{"work_id":"original-work"}},
            "outcome":"unknown","refusal":{"code":"redaction_refused","earlierSend":"unknown"},
            "unknown":{"whole":[null,"żółw 🐢"]}});
        record.queued_prompts[0] = serde_json::from_value(image).unwrap();
        serde_json::to_value(&record.queued_prompts[0]).unwrap()
    })
}

#[test]
fn original_queued_bind_payload_and_unknown_outcome_roundtrip_without_replay() {
    let turn = OrdinaryRoot::new("passive-bind-history");
    // Persist an actual prefix before evicting it. A complete empty transcript
    // would prove no promotion, so it cannot model this unknown history.
    turn.edit(|record| {
        assert!(record.session.messages.is_empty());
        for index in 0..=SQLITE_SESSION_TAIL_MESSAGES {
            push_message_on_record(record, Message::Text {
                id: format!("bind-history-{index}"), timestamp: stamp_now(),
                author: Author::Assistant, text: format!("Earlier history {index}"),
                expanded_text: None, source: None, attachments: Vec::new(),
            });
        }
    });
    turn.edit(|record| trim_retained_session_messages(record, SQLITE_SESSION_TAIL_MESSAGES));
    let prefix = serde_json::to_value(load_persisted_message_range(
        &turn.state.persistence_path, &turn.session, 0, 1).unwrap()).unwrap();
    assert_eq!(prefix.as_array().unwrap().len(), 1);
    install_queued_bind_history(&turn);
    let history = turn.edit(|record| {
        assert_eq!(record.message_start_index, 1);
        let queued = &mut record.queued_prompts[0];
        queued.engram_ordinary_handoff = true;
        serde_json::to_value(&*queued).unwrap()
    });
    let path = turn.state.persistence_path.as_path();
    {
        let inner = turn.state.inner.lock().unwrap();
        persist_state(&path, &inner).unwrap();
    }
    for _ in 0..2 {
        let loaded = load_state(&path).unwrap().unwrap();
        let record = &loaded.sessions[loaded.find_session_index(&turn.session).unwrap()];
        assert_eq!(record.message_start_index, 1);
        assert!(!record.queued_prompts[0].promotion_disposition_known);
        assert!(record.queued_prompts[0].engram_ordinary_handoff);
        assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), history);
        assert!(ordinary_queue_position(record).is_none());
        assert!(!record.engram.historical_control.contains_key("engramOwedBinds"));
        assert!(record.engram.unadmitted_active_turn.is_none());
    }
    let dispatch = turn.prepare("independent ordinary work");
    assert!(matches!(deliver_turn_dispatch(&turn.state, dispatch), TurnDispatchDeliveryOutcome::Delivered));
    assert!(turn.prompt().prompt.contains("independent ordinary work"));
    turn.record(|record| assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), history));
    assert_eq!(serde_json::to_value(load_persisted_message_range(
        &turn.state.persistence_path, &turn.session, 0, 1).unwrap()).unwrap(), prefix);
    turn.assert_no_control();
}

#[test]
fn ordinary_settings_toggle_preserves_unknown_queued_bind_history_and_new_content() {
    let turn = OrdinaryRoot::new("ordinary-settings-history");
    let history = install_queued_bind_history(&turn);
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let id = turn.enqueue("ordinary queued content", QueuedPromptSource::User);
    let queue = turn.record(|record| serde_json::to_value(&record.queued_prompts).unwrap());
    let mut settings = turn.state.inner.lock().unwrap().find_project(&turn.project).unwrap().engram.clone().unwrap();
    settings.enabled = false;
    turn.state.update_project_engram_settings(&turn.project, settings).unwrap();
    turn.record(|record| {
        assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), history);
        assert_eq!(serde_json::to_value(&record.queued_prompts).unwrap(), queue);
        assert_eq!(record.queued_prompts[1].pending_prompt.id, id);
        assert!(record.orchestrator_auto_dispatch_blocked);
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn changing_ordinary_session_title_does_not_send_an_old_bind_or_allocate_a_bind_key() {
    let turn = OrdinaryRoot::new("ordinary-title-no-bind");
    let before = install_queued_bind_history(&turn);
    for title in ["different title", "original title"] {
        turn.edit(|record| record.session.name = title.into());
        turn.record(|record| assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), before));
    }
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_same_or_different_connection_save_never_reinterprets_the_old_unknown_outcome() {
    for different_home in [false, true] {
        let turn = OrdinaryRoot::new("ordinary-connection-history");
        let history = install_queued_bind_history(&turn);
        turn.edit(|record| {
            record.engram.historical_control.insert("engramUncertainGrantId".into(), json!("historical-grant"));
            record.set_auto_dispatch_blocked(true);
        });
        let id = turn.enqueue("ordinary independent content", QueuedPromptSource::User);
        let mut settings = turn.state.inner.lock().unwrap().find_project(&turn.project).unwrap().engram.clone().unwrap();
        settings.enabled = false;
        if different_home {
            let home = turn.root.join("different-store");
            fs::create_dir_all(&home).unwrap();
            settings.home = Some(home.to_string_lossy().into_owned());
        }
        turn.state.update_project_engram_settings(&turn.project, settings).unwrap();
        turn.record(|record| {
            assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), history);
            assert_eq!(record.engram.historical_control["engramUncertainGrantId"], "historical-grant");
            assert_eq!(record.queued_prompts[1].pending_prompt.id, id);
        });
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}
