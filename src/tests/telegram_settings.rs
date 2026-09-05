// Telegram settings persistence and state-recovery tests split out of
// `telegram.rs`. This module owns settings status/update/delete normalization,
// token-at-rest behavior, keyring failure handling, and post-validation
// re-sanitization coverage.
//
// It deliberately does not own assistant forwarding, digest delivery, relay
// lifecycle restart behavior, or generic route/rate-limit coverage.

use super::telegram_support::{
    create_telegram_settings_project_and_session, install_telegram_settings_fixture,
};
use super::*;

#[test]
fn telegram_runtime_file_ignores_unknown_fields_without_importing_config_or_secrets() {
    let state = test_app_state();
    let path = state.telegram_bot_file_path();
    let token = "123456:ignored-file-secret";
    let current = json!({
        "chatId": 123,
        "selectedProjectId": "project-live",
        "selectedSessionId": "session-live",
        "lastDigestHash": "digest-live",
        "lastDigestMessageId": 47,
        "nextUpdateId": 991,
        "assistantForwardingCursors": {
            "session-live": { "messageId": "reply", "textChars": 42, "textHash": "hash", "resendIfGrown": true }
        },
        "forwardNextAssistantMessageSessionIds": ["session-live"]
    });
    let mut input = current.clone();
    input["config"] = json!({
        "enabled": true, "forwardAssistantReplies": true,
        "subscribedProjectIds": ["project-live"], "defaultProjectId": "project-live",
        "botToken": token
    });
    input["configMigratedToAppState"] = json!(true);
    input["lastForwardedAssistantMessageId"] = json!("unscoped-reply");
    input["lastForwardedAssistantMessageTextChars"] = json!(100);
    input["forwardNextAssistantMessageSessionId"] = json!("unscoped-session");
    input["futureRuntimeKey"] = json!({"ignored": true});
    let raw = serde_json::to_vec_pretty(&input).expect("input should encode");
    fs::create_dir_all(path.parent().unwrap()).expect("runtime directory should create");
    fs::write(&path, &raw).expect("runtime fixture should write");

    let loaded = state
        .load_telegram_bot_file()
        .expect("settings should read current fields");
    let relay_loaded = load_telegram_bot_state(&path).expect("relay should read current fields");
    assert_eq!(serde_json::to_value(&loaded).unwrap(), current);
    assert_eq!(serde_json::to_value(&relay_loaded).unwrap(), current);
    assert_eq!(
        fs::read(&path).unwrap(),
        raw,
        "reads must not rewrite the runtime file"
    );
    let status = state
        .telegram_status()
        .expect("unknown fields must not block status");
    assert!(!status.configured);
    assert!(!status.enabled);
    assert_eq!(status.linked_chat_id, Some(123));
    assert_eq!(
        state.telegram_config_from_state(),
        TelegramUiConfig::default()
    );
    assert_eq!(state.saved_telegram_bot_token().unwrap(), None);
    assert_eq!(
        fs::read(&path).unwrap(),
        raw,
        "status must not migrate unknown data"
    );

    persist_telegram_bot_state(&path, &loaded)
        .expect("next relay save should emit only current state");
    let saved = fs::read(&path).unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&saved).unwrap(), current);
    assert!(!String::from_utf8(saved).unwrap().contains(token));
    assert_eq!(state.saved_telegram_bot_token().unwrap(), None);
}

#[test]
fn telegram_runtime_current_shape_round_trips_through_both_writers() {
    let state = test_app_state();
    let path = state.telegram_bot_file_path();
    let runtime = TelegramBotState {
        chat_id: Some(123),
        next_update_id: Some(991),
        selected_project_id: Some("project-1".to_owned()),
        selected_session_id: Some("session-1".to_owned()),
        last_digest_hash: Some("digest".to_owned()),
        last_digest_message_id: Some(17),
        assistant_forwarding_cursors: HashMap::from([(
            "session-1".to_owned(),
            TelegramAssistantForwardingCursor {
                message_id: Some("message-1".to_owned()),
                text_chars: Some(27),
                text_hash: Some("hash".to_owned()),
                text_start_chars: Some(5),
                resend_if_grown: true,
                sent_chunks: Some(2),
                failed_chunk_send_attempts: Some(1),
                footer_pending: true,
                baseline_while_active: true,
            },
        )]),
        forward_next_assistant_message_session_ids: vec![
            "session-1".to_owned(),
            "session-2".to_owned(),
        ],
        ..Default::default()
    };
    state
        .persist_telegram_bot_file(&runtime)
        .expect("settings runtime writer should save");
    let before = fs::read(&path).unwrap();
    let loaded = load_telegram_bot_state(&path).expect("relay should load current state");
    assert_eq!(
        serde_json::to_value(&loaded).unwrap(),
        serde_json::to_value(&runtime).unwrap()
    );
    persist_telegram_bot_state(&path, &loaded).expect("relay writer should save");
    assert_eq!(
        fs::read(&path).unwrap(),
        before,
        "current single-session map round trip is byte-identical"
    );
    assert_eq!(
        serde_json::to_value(state.load_telegram_bot_file().unwrap()).unwrap(),
        serde_json::to_value(runtime).unwrap()
    );
}

#[test]
fn telegram_unknown_file_config_never_overrides_default_app_preferences() {
    for operation in ["status", "update", "delete-project", "delete-session"] {
        let state = test_app_state();
        let (project_id, session_id) = create_telegram_settings_project_and_session(&state);
        let path = state.telegram_bot_file_path();
        let raw = serde_json::to_vec(&json!({
            "configMigratedToAppState": true,
            "config": { "enabled": true, "forwardAssistantReplies": true,
                "subscribedProjectIds": [project_id], "defaultProjectId": project_id,
                "defaultSessionId": session_id, "botToken": "123456:ignored" },
            "chatId": 123, "nextUpdateId": 991
        }))
        .unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &raw).unwrap();
        match operation {
            "status" => {
                state.telegram_status().unwrap();
            }
            "update" => {
                state
                    .update_telegram_config(serde_json::from_value(json!({})).unwrap())
                    .unwrap();
            }
            "delete-project" => {
                state.delete_project(&project_id).unwrap();
            }
            "delete-session" => {
                state.kill_session(&session_id).unwrap();
            }
            _ => unreachable!(),
        }
        assert_eq!(
            state.telegram_config_from_state(),
            TelegramUiConfig::default(),
            "{operation}"
        );
        assert_eq!(
            state.saved_telegram_bot_token().unwrap(),
            None,
            "{operation}"
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            raw,
            "{operation} must not write a config mirror"
        );
    }
}

#[test]
fn telegram_app_preferences_reject_plaintext_token_fields() {
    assert!(
        serde_json::from_value::<TelegramUiConfig>(
            json!({"enabled": false, "botToken": "123456:secret"})
        )
        .is_err()
    );
}

#[test]
fn telegram_status_sanitizes_stale_project_and_session_references() {
    let state = test_app_state();
    let (project_id, _session_id) = create_telegram_settings_project_and_session(&state);
    let config = TelegramUiConfig {
        subscribed_project_ids: vec![project_id.clone(), "missing-project".to_owned()],
        default_project_id: Some(project_id.clone()),
        default_session_id: Some("missing-session".to_owned()),
        ..TelegramUiConfig::default()
    };

    let sanitized = state.sanitize_telegram_config_for_current_state(config);

    assert_eq!(sanitized.subscribed_project_ids, vec![project_id]);
    assert_eq!(sanitized.default_session_id, None);
}

#[test]
fn telegram_status_persists_sanitized_stale_project_and_session_references() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let state = test_app_state();
    let (project_id, _session_id) = create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();
    fs::create_dir_all(path.parent().expect("settings path should have a parent"))
        .expect("settings dir should create");
    install_telegram_settings_fixture(
        &state,
        serde_json::from_value(json!({
                "enabled": false,
                "subscribedProjectIds": [project_id.clone(), "missing-project"],
                "defaultProjectId": project_id.clone(),
                "defaultSessionId": "missing-session"
        }))
        .expect("current config fixture should decode"),
        Some("123456:secret"),
        serde_json::from_value(json!({
            "chatId": 123
        }))
        .expect("current runtime fixture should decode"),
    );
    let initial_revision = state.snapshot().revision;

    let response = state
        .telegram_status()
        .expect("status read should sanitize stale persisted references");

    assert!(response.configured);
    assert_eq!(response.bot_token_masked.as_deref(), Some("****cret"));
    assert_eq!(response.subscribed_project_ids, vec![project_id.clone()]);
    assert_eq!(
        response.default_project_id.as_deref(),
        Some(project_id.as_str())
    );
    assert_eq!(response.default_session_id, None);
    assert_eq!(response.linked_chat_id, Some(123));
    let snapshot = state.snapshot();
    assert!(snapshot.revision > initial_revision);
    assert_eq!(
        snapshot.preferences.telegram.subscribed_project_ids,
        vec![project_id.clone()]
    );
    assert_eq!(
        snapshot.preferences.telegram.default_project_id.as_deref(),
        Some(project_id.as_str())
    );
    assert_eq!(snapshot.preferences.telegram.default_session_id, None);

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("settings file should read"))
        .expect("settings file should parse");
    let config_value = serde_json::to_value(state.telegram_config_from_state())
        .expect("app config should serialize");
    assert_eq!(
        config_value["subscribedProjectIds"],
        json!([project_id.clone()])
    );
    assert_eq!(config_value["defaultProjectId"], json!(project_id));
    assert!(config_value.get("defaultSessionId").is_none());
    assert!(config_value.get("botToken").is_none());
    assert_eq!(value["chatId"], json!(123));
}

#[test]
fn telegram_settings_load_defaults_only_for_missing_file() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let state = test_app_state();
    let path = state.telegram_bot_file_path();

    let missing = state
        .load_telegram_bot_file()
        .expect("missing settings file should default");
    assert_eq!(missing.chat_id, None);

    fs::create_dir_all(path.parent().expect("settings path should have a parent"))
        .expect("settings dir should create");
    fs::write(&path, b"{ not valid json").expect("malformed settings fixture should write");

    let err = match state.load_telegram_bot_file() {
        Ok(_) => panic!("malformed settings file should fail instead of defaulting"),
        Err(err) => err,
    };
    assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(err.message.contains("failed to parse Telegram settings"));
}

#[test]
fn telegram_config_update_sanitizes_stale_persisted_references_before_validation() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let state = test_app_state();
    let (project_id, _session_id) = create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();
    fs::create_dir_all(path.parent().expect("settings path should have a parent"))
        .expect("settings dir should create");
    install_telegram_settings_fixture(
        &state,
        serde_json::from_value(json!({
                "enabled": false,
                "subscribedProjectIds": ["missing-project"],
                "defaultProjectId": "missing-project",
                "defaultSessionId": "missing-session"
        }))
        .expect("current config fixture should decode"),
        Some("123456:secret"),
        serde_json::from_value(json!({
            "chatId": 123
        }))
        .expect("current runtime fixture should decode"),
    );

    let request: UpdateTelegramConfigRequest = serde_json::from_value(json!({
        "enabled": true,
        "subscribedProjectIds": [project_id.clone()]
    }))
    .expect("request should decode");
    let response = state
        .update_telegram_config(request)
        .expect("unrelated update should sanitize stale persisted references");

    assert!(response.enabled);
    assert_eq!(response.subscribed_project_ids, vec![project_id.clone()]);
    assert_eq!(
        response.default_project_id.as_deref(),
        Some(project_id.as_str())
    );
    assert_eq!(response.default_session_id, None);
    assert_eq!(response.linked_chat_id, Some(123));

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("settings file should read"))
        .expect("settings file should parse");
    let config_value = serde_json::to_value(state.telegram_config_from_state())
        .expect("app config should serialize");
    assert_eq!(config_value["enabled"], json!(true));
    assert!(value.get("configMigratedToAppState").is_none());
    assert!(value.get("config").is_none());
    assert!(config_value.get("botToken").is_none());
    assert_eq!(
        config_value["subscribedProjectIds"],
        json!([project_id.clone()])
    );
    assert_eq!(config_value["defaultProjectId"], json!(project_id));
    assert!(config_value.get("defaultSessionId").is_none());
    assert_eq!(value["chatId"], json!(123));
}

#[test]
fn telegram_config_update_resanitizes_project_deleted_after_validation_before_persist() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let state = test_app_state();
    state
        .persist_telegram_bot_file(&TelegramBotState::default())
        .expect("runtime fixture should persist");
    let (project_id, session_id) = create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();
    let request_project_id = project_id.clone();
    let request_session_id = session_id.clone();

    state.reset_telegram_relay_runtime_actions_for_tests();
    let response = state
        .update_telegram_config_with_post_validation_hook(
            UpdateTelegramConfigRequest {
                enabled: Some(true),
                forward_assistant_replies: None,
                bot_token: Some(Some("123456:secret".to_owned())),
                subscribed_project_ids: Some(vec![request_project_id.clone()]),
                default_project_id: Some(Some(request_project_id.clone())),
                default_session_id: Some(Some(request_session_id.clone())),
            },
            move |state| {
                let mut inner = state.inner.lock().expect("state mutex poisoned");
                inner
                    .projects
                    .retain(|project| project.id != request_project_id);
                for record in &mut inner.sessions {
                    if record.session.project_id.as_deref() == Some(request_project_id.as_str()) {
                        record.session.project_id = None;
                    }
                }
                Ok(())
            },
        )
        .expect("post-validation project delete should be scrubbed");

    assert!(!response.enabled);
    assert!(response.configured);
    assert!(!response.running);
    assert!(response.subscribed_project_ids.is_empty());
    assert_eq!(response.default_project_id, None);
    assert_eq!(response.default_session_id, None);
    assert!(
        state
            .snapshot()
            .preferences
            .telegram
            .subscribed_project_ids
            .is_empty()
    );

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("settings file should read"))
        .expect("settings file should parse");
    let config_value = serde_json::to_value(state.telegram_config_from_state())
        .expect("app config should serialize");
    assert!(value.get("configMigratedToAppState").is_none());
    assert!(value.get("config").is_none());
    assert_eq!(config_value["enabled"], json!(false));
    assert!(config_value.get("subscribedProjectIds").is_none());
    assert!(config_value.get("defaultProjectId").is_none());
    assert!(config_value.get("defaultSessionId").is_none());
    // The test runtime records stop requests even if no relay was running; the
    // important invariant is that no start survives after target removal.
    assert_eq!(
        state.take_telegram_relay_runtime_actions_for_tests(),
        vec![TelegramRelayRuntimeActionForTest::Stop]
    );
}

#[test]
fn telegram_config_update_resanitizes_session_deleted_after_validation_before_persist() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let state = test_app_state();
    state
        .persist_telegram_bot_file(&TelegramBotState::default())
        .expect("runtime fixture should persist");
    let (project_id, session_id) = create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();
    let request_project_id = project_id.clone();
    let request_session_id = session_id.clone();

    state.reset_telegram_relay_runtime_actions_for_tests();
    let response = state
        .update_telegram_config_with_post_validation_hook(
            UpdateTelegramConfigRequest {
                enabled: Some(true),
                forward_assistant_replies: None,
                bot_token: Some(Some("123456:secret".to_owned())),
                subscribed_project_ids: Some(vec![request_project_id.clone()]),
                default_project_id: Some(Some(request_project_id.clone())),
                default_session_id: Some(Some(request_session_id.clone())),
            },
            move |state| {
                let mut inner = state.inner.lock().expect("state mutex poisoned");
                inner
                    .sessions
                    .retain(|record| record.session.id != request_session_id);
                Ok(())
            },
        )
        .expect("post-validation session delete should be scrubbed");

    assert!(response.enabled);
    assert!(response.configured);
    assert!(response.running);
    assert_eq!(response.subscribed_project_ids, vec![project_id.clone()]);
    assert_eq!(
        response.default_project_id.as_deref(),
        Some(project_id.as_str())
    );
    assert_eq!(response.default_session_id, None);

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("settings file should read"))
        .expect("settings file should parse");
    let config_value = serde_json::to_value(state.telegram_config_from_state())
        .expect("app config should serialize");
    assert!(value.get("configMigratedToAppState").is_none());
    assert!(value.get("config").is_none());
    assert_eq!(
        config_value["subscribedProjectIds"],
        json!([project_id.clone()])
    );
    assert_eq!(config_value["defaultProjectId"], json!(project_id.clone()));
    assert!(config_value.get("defaultSessionId").is_none());
    assert_eq!(
        state.take_telegram_relay_runtime_actions_for_tests(),
        vec![TelegramRelayRuntimeActionForTest::Start {
            project_id: project_id.clone(),
            subscribed_project_ids: vec![project_id],
        }]
    );
}

#[test]
fn delete_project_prunes_telegram_config_and_disables_relay_without_project_target() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let state = test_app_state();
    let (project_id, session_id) = create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();
    fs::create_dir_all(path.parent().expect("state path should have a parent"))
        .expect("settings dir should create");
    install_telegram_settings_fixture(
        &state,
        serde_json::from_value(json!({
                "enabled": true,
                "subscribedProjectIds": [project_id.clone()],
                "defaultProjectId": project_id.clone(),
                "defaultSessionId": session_id
        }))
        .expect("current config fixture should decode"),
        Some("123456:secret"),
        serde_json::from_value(json!({
            "chatId": 123
        }))
        .expect("current runtime fixture should decode"),
    );

    state.reset_telegram_relay_runtime_actions_for_tests();
    let response = state
        .delete_project(&project_id)
        .expect("project should delete");
    assert!(!response.preferences.telegram.enabled);
    assert!(
        response
            .preferences
            .telegram
            .subscribed_project_ids
            .is_empty()
    );
    assert_eq!(response.preferences.telegram.default_project_id, None);
    assert_eq!(response.preferences.telegram.default_session_id, None);

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("settings file should read"))
        .expect("settings file should parse");
    let config_value = serde_json::to_value(state.telegram_config_from_state())
        .expect("app config should serialize");
    assert_eq!(config_value["enabled"], json!(false));
    assert!(value.get("configMigratedToAppState").is_none());
    assert!(value.get("config").is_none());
    assert!(config_value.get("botToken").is_none());
    assert!(
        config_value.get("subscribedProjectIds").is_none()
            || config_value["subscribedProjectIds"] == json!([])
    );
    assert!(config_value.get("defaultProjectId").is_none());
    assert!(config_value.get("defaultSessionId").is_none());
    assert_eq!(value["chatId"], json!(123));
    assert_eq!(
        state.take_telegram_relay_runtime_actions_for_tests(),
        vec![TelegramRelayRuntimeActionForTest::Stop]
    );
}

#[test]
fn delete_project_surfaces_telegram_prune_errors() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let state = test_app_state();
    let (project_id, _session_id) = create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();
    fs::create_dir_all(path.parent().expect("state path should have a parent"))
        .expect("settings dir should create");
    fs::write(&path, b"{ not valid json").expect("malformed settings fixture should write");

    let err = match state.delete_project(&project_id) {
        Ok(_) => panic!("Telegram prune failure should surface"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(err.message.contains("failed to parse Telegram settings"));
}

#[test]
fn delete_project_prunes_telegram_config_and_keeps_relay_enabled_with_remaining_target() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let state = test_app_state();
    let (deleted_project_id, _deleted_session_id) =
        create_telegram_settings_project_and_session(&state);
    let (remaining_project_id, remaining_session_id) =
        create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();
    fs::create_dir_all(path.parent().expect("state path should have a parent"))
        .expect("settings dir should create");
    install_telegram_settings_fixture(
        &state,
        serde_json::from_value(json!({
                "enabled": true,
                "subscribedProjectIds": [deleted_project_id.clone(), remaining_project_id.clone()],
                "defaultProjectId": remaining_project_id.clone(),
                "defaultSessionId": remaining_session_id
        }))
        .expect("current config fixture should decode"),
        Some("123456:secret"),
        serde_json::from_value(json!({
            "chatId": 123
        }))
        .expect("current runtime fixture should decode"),
    );

    state.reset_telegram_relay_runtime_actions_for_tests();
    let response = state
        .delete_project(&deleted_project_id)
        .expect("project should delete");
    assert!(response.preferences.telegram.enabled);
    assert_eq!(
        response.preferences.telegram.subscribed_project_ids,
        vec![remaining_project_id.clone()]
    );
    assert_eq!(
        response.preferences.telegram.default_project_id.as_deref(),
        Some(remaining_project_id.as_str())
    );
    assert_eq!(
        response.preferences.telegram.default_session_id.as_deref(),
        Some(remaining_session_id.as_str())
    );

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("settings file should read"))
        .expect("settings file should parse");
    let config_value = serde_json::to_value(state.telegram_config_from_state())
        .expect("app config should serialize");
    assert!(value.get("configMigratedToAppState").is_none());
    assert!(value.get("config").is_none());
    assert_eq!(config_value["enabled"], json!(true));
    assert!(config_value.get("botToken").is_none());
    assert_eq!(
        config_value["subscribedProjectIds"],
        json!([remaining_project_id.clone()])
    );
    assert_eq!(
        config_value["defaultProjectId"],
        json!(remaining_project_id.clone())
    );
    assert_eq!(
        config_value["defaultSessionId"],
        json!(remaining_session_id)
    );
    assert_eq!(value["chatId"], json!(123));
    assert_eq!(
        state.take_telegram_relay_runtime_actions_for_tests(),
        vec![TelegramRelayRuntimeActionForTest::Start {
            project_id: remaining_project_id.clone(),
            subscribed_project_ids: vec![remaining_project_id],
        }]
    );
}

#[test]
fn delete_project_restarts_running_telegram_relay_with_remaining_effective_project() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let state = test_app_state();
    let (deleted_project_id, _deleted_session_id) =
        create_telegram_settings_project_and_session(&state);
    let (remaining_project_id, _remaining_session_id) =
        create_telegram_settings_project_and_session(&state);

    state.reset_telegram_relay_runtime_actions_for_tests();
    let started = state
        .update_telegram_config(UpdateTelegramConfigRequest {
            enabled: Some(true),
            forward_assistant_replies: None,
            bot_token: Some(Some("123456:secret".to_owned())),
            subscribed_project_ids: Some(vec![
                deleted_project_id.clone(),
                remaining_project_id.clone(),
            ]),
            default_project_id: Some(Some(deleted_project_id.clone())),
            default_session_id: None,
        })
        .expect("relay config should save and start");
    assert!(started.running);
    assert_eq!(
        started.default_project_id.as_deref(),
        Some(deleted_project_id.as_str())
    );

    let response = state
        .delete_project(&deleted_project_id)
        .expect("project should delete");
    assert!(response.preferences.telegram.enabled);
    assert_eq!(
        response.preferences.telegram.subscribed_project_ids,
        vec![remaining_project_id.clone()]
    );
    assert_eq!(
        response.preferences.telegram.default_project_id.as_deref(),
        Some(remaining_project_id.as_str())
    );

    let status = state.telegram_status().expect("status should load");
    assert!(status.running);
    assert_eq!(
        status.subscribed_project_ids,
        vec![remaining_project_id.clone()]
    );
    assert_eq!(
        status.default_project_id.as_deref(),
        Some(remaining_project_id.as_str())
    );
    assert_eq!(
        state.take_telegram_relay_runtime_actions_for_tests(),
        vec![
            TelegramRelayRuntimeActionForTest::Start {
                project_id: deleted_project_id.clone(),
                subscribed_project_ids: vec![deleted_project_id, remaining_project_id.clone()],
            },
            TelegramRelayRuntimeActionForTest::Start {
                project_id: remaining_project_id.clone(),
                subscribed_project_ids: vec![remaining_project_id],
            },
        ]
    );
}

#[test]
fn delete_project_preserves_unrelated_telegram_settings_without_restarting_relay() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let state = test_app_state();
    let (deleted_project_id, _deleted_session_id) =
        create_telegram_settings_project_and_session(&state);
    let (remaining_project_id, remaining_session_id) =
        create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();
    fs::create_dir_all(path.parent().expect("state path should have a parent"))
        .expect("settings dir should create");
    install_telegram_settings_fixture(
        &state,
        TelegramUiConfig {
            enabled: true,
            subscribed_project_ids: vec![remaining_project_id.clone()],
            default_project_id: Some(remaining_project_id.clone()),
            default_session_id: Some(remaining_session_id.clone()),
            ..Default::default()
        },
        Some("123456:secret"),
        TelegramBotState {
            chat_id: Some(123),
            ..Default::default()
        },
    );
    let fixture = fs::read(&path).expect("runtime should read");
    state.reset_telegram_relay_runtime_actions_for_tests();
    state
        .delete_project(&deleted_project_id)
        .expect("project should delete");

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("settings file should read"))
        .expect("settings file should parse");
    let config_value = serde_json::to_value(state.telegram_config_from_state())
        .expect("app config should serialize");
    assert!(value.get("configMigratedToAppState").is_none());
    assert!(value.get("config").is_none());
    assert_eq!(value["chatId"], json!(123));
    assert!(config_value.get("botToken").is_none());
    assert_eq!(config_value["enabled"], json!(true));
    assert_eq!(
        config_value["defaultProjectId"],
        json!(remaining_project_id)
    );
    assert_eq!(
        config_value["defaultSessionId"],
        json!(remaining_session_id)
    );
    assert!(
        state
            .take_telegram_relay_runtime_actions_for_tests()
            .is_empty()
    );
    assert_eq!(fs::read(&path).expect("settings file should read"), fixture);
}

#[test]
fn kill_session_prunes_telegram_state_and_config_references() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let state = test_app_state();
    let (project_id, session_id) = create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();
    fs::create_dir_all(path.parent().expect("state path should have a parent"))
        .expect("settings dir should create");
    install_telegram_settings_fixture(
        &state,
        serde_json::from_value(json!({
                "enabled": true,
                "subscribedProjectIds": [project_id.clone()],
                "defaultProjectId": project_id,
                "defaultSessionId": session_id.clone()
        }))
        .expect("current config fixture should decode"),
        Some("123456:secret"),
        serde_json::from_value(json!({
            "selectedSessionId": session_id.clone(),
            "lastDigestHash": "old-digest",
            "lastDigestMessageId": 44,
            "forwardNextAssistantMessageSessionIds": [session_id.clone(), "other-session"],
            "assistantForwardingCursors": {
                (session_id.clone()): {
                    "messageId": "message-1",
                    "textChars": 10
                },
                "other-session": {
                    "messageId": "message-2",
                    "textChars": 20
                }
            },
            "chatId": 123
        }))
        .expect("current runtime fixture should decode"),
    );

    let response = state
        .kill_session(&session_id)
        .expect("session should kill");
    assert_eq!(response.preferences.telegram.default_session_id, None);

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("settings file should read"))
        .expect("settings file should parse");
    let config_value = serde_json::to_value(state.telegram_config_from_state())
        .expect("app config should serialize");
    assert!(value.get("configMigratedToAppState").is_none());
    assert!(value.get("config").is_none());
    assert!(config_value.get("botToken").is_none());
    assert!(config_value.get("defaultSessionId").is_none());
    assert!(value.get("selectedSessionId").is_none());
    assert!(value.get("lastDigestHash").is_none());
    assert!(value.get("lastDigestMessageId").is_none());
    assert_eq!(
        value["forwardNextAssistantMessageSessionIds"],
        json!(["other-session"])
    );
    assert!(value.get("forwardNextAssistantMessageSessionId").is_none());
    assert!(
        value["assistantForwardingCursors"]
            .get(&session_id)
            .is_none()
    );
    assert_eq!(
        value["assistantForwardingCursors"]["other-session"]["messageId"],
        json!("message-2")
    );
    assert_eq!(value["chatId"], json!(123));
}
