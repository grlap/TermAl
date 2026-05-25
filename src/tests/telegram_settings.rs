// Telegram settings persistence and state-recovery tests split out of
// `telegram.rs`. This module owns settings status/update/delete normalization,
// token-at-rest behavior, keyring failure handling, and post-validation
// re-sanitization coverage.
//
// It deliberately does not own assistant forwarding, digest delivery, relay
// lifecycle restart behavior, or generic route/rate-limit coverage.

use super::telegram_support::create_telegram_settings_project_and_session;
use super::*;

#[test]
fn telegram_state_persist_preserves_settings_config_without_plaintext_token() {
    let path = std::env::temp_dir().join(format!("termal-telegram-state-{}.json", Uuid::new_v4()));
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "config": {
                "enabled": true,
                "botToken": "123456:secret",
                "subscribedProjectIds": ["project-1"],
                "defaultProjectId": "project-1",
                "defaultSessionId": "session-1"
            },
            "chatId": 123
        }))
        .expect("fixture should encode"),
    )
    .expect("fixture should write");

    let state = TelegramBotState {
        chat_id: Some(456),
        next_update_id: Some(99),
        ..TelegramBotState::default()
    };
    persist_telegram_bot_state(&path, &state).expect("state should persist");

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("state file should read"))
        .expect("state file should parse");
    assert!(value["config"].get("botToken").is_none());
    assert_eq!(
        value["config"]["subscribedProjectIds"],
        json!(["project-1"])
    );
    assert_eq!(value["config"]["defaultProjectId"], json!("project-1"));
    assert_eq!(value["config"]["defaultSessionId"], json!("session-1"));
    assert_eq!(value["chatId"], json!(456));
    assert_eq!(value["nextUpdateId"], json!(99));

    fs::remove_file(&path).ok();
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
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-status-sanitize-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
    let state = test_app_state();
    let (project_id, _session_id) = create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();
    fs::create_dir_all(path.parent().expect("settings path should have a parent"))
        .expect("settings dir should create");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "config": {
                "enabled": false,
                "botToken": "123456:secret",
                "subscribedProjectIds": [project_id.clone(), "missing-project"],
                "defaultProjectId": project_id.clone(),
                "defaultSessionId": "missing-session"
            },
            "chatId": 123
        }))
        .expect("fixture should encode"),
    )
    .expect("fixture should write");
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
    assert_eq!(
        value["config"]["subscribedProjectIds"],
        json!([project_id.clone()])
    );
    assert_eq!(value["config"]["defaultProjectId"], json!(project_id));
    assert!(value["config"].get("defaultSessionId").is_none());
    assert!(value["config"].get("botToken").is_none());
    assert_eq!(value["chatId"], json!(123));
}

#[test]
fn telegram_status_does_not_reimport_migrated_file_config_after_default_reset() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-default-reset-stale-mirror-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
    let state = test_app_state();
    let path = state.telegram_bot_file_path();
    fs::create_dir_all(path.parent().expect("settings path should have a parent"))
        .expect("settings dir should create");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "configMigratedToAppState": true,
            "config": {
                "enabled": true,
                "forwardAssistantReplies": true,
                "subscribedProjectIds": ["stale-project"],
                "defaultProjectId": "stale-project",
                "defaultSessionId": "stale-session"
            },
            "chatId": 123
        }))
        .expect("fixture should encode"),
    )
    .expect("fixture should write");

    let response = state
        .telegram_status()
        .expect("status read should ignore migrated stale file config");

    assert!(!response.enabled);
    assert!(!response.forward_assistant_replies);
    assert!(response.subscribed_project_ids.is_empty());
    assert_eq!(response.default_project_id, None);
    assert_eq!(response.default_session_id, None);
    assert_eq!(response.linked_chat_id, Some(123));
    assert_eq!(
        state.snapshot().preferences.telegram,
        TelegramUiConfig::default()
    );

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("settings file should read"))
        .expect("settings file should parse");
    assert_eq!(value["configMigratedToAppState"], json!(true));
    assert_eq!(value["config"]["enabled"], json!(false));
    assert_eq!(value["config"]["forwardAssistantReplies"], json!(false));
    assert!(value["config"].get("subscribedProjectIds").is_none());
    assert!(value["config"].get("defaultProjectId").is_none());
    assert!(value["config"].get("defaultSessionId").is_none());
    assert_eq!(value["chatId"], json!(123));
}

#[test]
fn telegram_config_update_does_not_reimport_migrated_file_config_after_default_reset() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-update-default-reset-stale-mirror-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
    let state = test_app_state();
    let (project_id, session_id) = create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();
    fs::create_dir_all(path.parent().expect("settings path should have a parent"))
        .expect("settings dir should create");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "configMigratedToAppState": true,
            "config": {
                "enabled": true,
                "forwardAssistantReplies": true,
                "subscribedProjectIds": [project_id.clone()],
                "defaultProjectId": project_id,
                "defaultSessionId": session_id
            },
            "chatId": 123
        }))
        .expect("fixture should encode"),
    )
    .expect("fixture should write");

    let response = state
        .update_telegram_config(UpdateTelegramConfigRequest {
            enabled: None,
            forward_assistant_replies: None,
            bot_token: None,
            subscribed_project_ids: None,
            default_project_id: None,
            default_session_id: None,
        })
        .expect("update should ignore migrated stale file config");

    assert!(!response.enabled);
    assert!(!response.forward_assistant_replies);
    assert!(response.subscribed_project_ids.is_empty());
    assert_eq!(response.default_project_id, None);
    assert_eq!(response.default_session_id, None);
    assert_eq!(response.linked_chat_id, Some(123));
    assert_eq!(
        state.snapshot().preferences.telegram,
        TelegramUiConfig::default()
    );

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("settings file should read"))
        .expect("settings file should parse");
    assert_eq!(value["configMigratedToAppState"], json!(true));
    assert_eq!(value["config"]["enabled"], json!(false));
    assert_eq!(value["config"]["forwardAssistantReplies"], json!(false));
    assert!(value["config"].get("subscribedProjectIds").is_none());
    assert!(value["config"].get("defaultProjectId").is_none());
    assert!(value["config"].get("defaultSessionId").is_none());
    assert_eq!(value["chatId"], json!(123));
}

#[test]
fn telegram_settings_load_defaults_only_for_missing_file() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-settings-load-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
    let state = test_app_state();
    let path = state.telegram_bot_file_path();

    let missing = state
        .load_telegram_bot_file()
        .expect("missing settings file should default");
    assert_eq!(missing.config.bot_token, None);
    assert_eq!(missing.state.chat_id, None);

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
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-stale-config-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
    let state = test_app_state();
    let (project_id, _session_id) = create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();
    fs::create_dir_all(path.parent().expect("settings path should have a parent"))
        .expect("settings dir should create");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "config": {
                "enabled": false,
                "botToken": "123456:secret",
                "subscribedProjectIds": ["missing-project"],
                "defaultProjectId": "missing-project",
                "defaultSessionId": "missing-session"
            },
            "chatId": 123
        }))
        .expect("fixture should encode"),
    )
    .expect("fixture should write");

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
    assert_eq!(value["config"]["enabled"], json!(true));
    assert_eq!(value["configMigratedToAppState"], json!(true));
    assert!(value["config"].get("botToken").is_none());
    assert_eq!(
        value["config"]["subscribedProjectIds"],
        json!([project_id.clone()])
    );
    assert_eq!(value["config"]["defaultProjectId"], json!(project_id));
    assert!(value["config"].get("defaultSessionId").is_none());
    assert_eq!(value["chatId"], json!(123));
}

#[test]
fn telegram_config_update_resanitizes_project_deleted_after_validation_before_persist() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-post-validation-project-delete-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
    let state = test_app_state();
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
    assert_eq!(value["configMigratedToAppState"], json!(true));
    assert_eq!(value["config"]["enabled"], json!(false));
    assert!(value["config"].get("subscribedProjectIds").is_none());
    assert!(value["config"].get("defaultProjectId").is_none());
    assert!(value["config"].get("defaultSessionId").is_none());
    // The test runtime records stop requests even if no relay was running; the
    // important invariant is that no start survives after target removal.
    assert_eq!(
        state.take_telegram_relay_runtime_actions_for_tests(),
        vec![TelegramRelayRuntimeActionForTest::Stop]
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn telegram_config_update_resanitizes_session_deleted_after_validation_before_persist() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-post-validation-session-delete-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
    let state = test_app_state();
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
    assert_eq!(value["configMigratedToAppState"], json!(true));
    assert_eq!(
        value["config"]["subscribedProjectIds"],
        json!([project_id.clone()])
    );
    assert_eq!(
        value["config"]["defaultProjectId"],
        json!(project_id.clone())
    );
    assert!(value["config"].get("defaultSessionId").is_none());
    assert_eq!(
        state.take_telegram_relay_runtime_actions_for_tests(),
        vec![TelegramRelayRuntimeActionForTest::Start {
            project_id: project_id.clone(),
            subscribed_project_ids: vec![project_id],
        }]
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn delete_project_prunes_telegram_config_and_disables_relay_without_project_target() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!("termal-telegram-home-{}", Uuid::new_v4()));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
    let state = test_app_state();
    let (project_id, session_id) = create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();
    fs::create_dir_all(path.parent().expect("state path should have a parent"))
        .expect("settings dir should create");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "config": {
                "enabled": true,
                "botToken": "123456:secret",
                "subscribedProjectIds": [project_id.clone()],
                "defaultProjectId": project_id.clone(),
                "defaultSessionId": session_id
            },
            "chatId": 123
        }))
        .expect("fixture should encode"),
    )
    .expect("fixture should write");

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
    assert_eq!(value["config"]["enabled"], json!(false));
    assert_eq!(value["configMigratedToAppState"], json!(true));
    assert!(value["config"].get("botToken").is_none());
    assert!(
        value["config"].get("subscribedProjectIds").is_none()
            || value["config"]["subscribedProjectIds"] == json!([])
    );
    assert!(value["config"].get("defaultProjectId").is_none());
    assert!(value["config"].get("defaultSessionId").is_none());
    assert_eq!(value["chatId"], json!(123));
    assert_eq!(
        state.take_telegram_relay_runtime_actions_for_tests(),
        vec![TelegramRelayRuntimeActionForTest::Stop]
    );
}

#[test]
fn delete_project_does_not_reimport_migrated_file_config_after_default_reset() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-delete-default-reset-stale-mirror-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
    let state = test_app_state();
    let (deleted_project_id, _deleted_session_id) =
        create_telegram_settings_project_and_session(&state);
    let (stale_project_id, stale_session_id) = create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();
    fs::create_dir_all(path.parent().expect("settings path should have a parent"))
        .expect("settings dir should create");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "configMigratedToAppState": true,
            "config": {
                "enabled": true,
                "forwardAssistantReplies": true,
                "subscribedProjectIds": [stale_project_id.clone()],
                "defaultProjectId": stale_project_id.clone(),
                "defaultSessionId": stale_session_id.clone()
            },
            "chatId": 123
        }))
        .expect("fixture should encode"),
    )
    .expect("fixture should write");

    state.reset_telegram_relay_runtime_actions_for_tests();
    let response = state
        .delete_project(&deleted_project_id)
        .expect("project should delete without importing migrated mirror");

    assert_eq!(response.preferences.telegram, TelegramUiConfig::default());
    assert_eq!(
        state.snapshot().preferences.telegram,
        TelegramUiConfig::default()
    );

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("settings file should read"))
        .expect("settings file should parse");
    assert_eq!(value["configMigratedToAppState"], json!(true));
    assert_eq!(value["config"]["enabled"], json!(false));
    assert_eq!(value["config"]["forwardAssistantReplies"], json!(false));
    assert!(value["config"].get("subscribedProjectIds").is_none());
    assert!(value["config"].get("defaultProjectId").is_none());
    assert!(value["config"].get("defaultSessionId").is_none());
    assert_eq!(value["chatId"], json!(123));
    assert!(
        state
            .take_telegram_relay_runtime_actions_for_tests()
            .is_empty()
    );
}

#[test]
fn delete_project_surfaces_telegram_prune_errors() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-delete-prune-error-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
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
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-multi-project-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
    let state = test_app_state();
    let (deleted_project_id, _deleted_session_id) =
        create_telegram_settings_project_and_session(&state);
    let (remaining_project_id, remaining_session_id) =
        create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();
    fs::create_dir_all(path.parent().expect("state path should have a parent"))
        .expect("settings dir should create");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "config": {
                "enabled": true,
                "botToken": "123456:secret",
                "subscribedProjectIds": [deleted_project_id.clone(), remaining_project_id.clone()],
                "defaultProjectId": remaining_project_id.clone(),
                "defaultSessionId": remaining_session_id
            },
            "chatId": 123
        }))
        .expect("fixture should encode"),
    )
    .expect("fixture should write");

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
    assert_eq!(value["configMigratedToAppState"], json!(true));
    assert_eq!(value["config"]["enabled"], json!(true));
    assert!(value["config"].get("botToken").is_none());
    assert_eq!(
        value["config"]["subscribedProjectIds"],
        json!([remaining_project_id.clone()])
    );
    assert_eq!(
        value["config"]["defaultProjectId"],
        json!(remaining_project_id.clone())
    );
    assert_eq!(
        value["config"]["defaultSessionId"],
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
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-delete-active-project-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
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
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn delete_project_migrates_unrelated_telegram_token_without_restarting_relay() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-unrelated-project-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
    let state = test_app_state();
    let (deleted_project_id, _deleted_session_id) =
        create_telegram_settings_project_and_session(&state);
    let (remaining_project_id, remaining_session_id) =
        create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();
    fs::create_dir_all(path.parent().expect("state path should have a parent"))
        .expect("settings dir should create");
    let fixture = serde_json::to_string(&json!({
        "chatId": 123,
        "config": {
            "botToken": "123456:secret",
            "defaultProjectId": remaining_project_id.clone(),
            "defaultSessionId": remaining_session_id,
            "enabled": true,
            "subscribedProjectIds": [remaining_project_id]
        }
    }))
    .expect("fixture should encode");
    fs::write(&path, fixture.as_bytes()).expect("fixture should write");

    state.reset_telegram_relay_runtime_actions_for_tests();
    state
        .delete_project(&deleted_project_id)
        .expect("project should delete");

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("settings file should read"))
        .expect("settings file should parse");
    assert_eq!(value["configMigratedToAppState"], json!(true));
    assert_eq!(value["chatId"], json!(123));
    assert!(value["config"].get("botToken").is_none());
    assert_eq!(value["config"]["enabled"], json!(true));
    assert_eq!(
        value["config"]["defaultProjectId"],
        json!(remaining_project_id)
    );
    assert_eq!(
        value["config"]["defaultSessionId"],
        json!(remaining_session_id)
    );
    assert!(
        state
            .take_telegram_relay_runtime_actions_for_tests()
            .is_empty()
    );
    assert_ne!(
        fs::read(&path).expect("settings file should read"),
        fixture.as_bytes()
    );
}

#[test]
fn kill_session_prunes_telegram_state_and_config_references() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-session-prune-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
    let state = test_app_state();
    let (project_id, session_id) = create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();
    fs::create_dir_all(path.parent().expect("state path should have a parent"))
        .expect("settings dir should create");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "config": {
                "enabled": true,
                "botToken": "123456:secret",
                "subscribedProjectIds": [project_id.clone()],
                "defaultProjectId": project_id,
                "defaultSessionId": session_id.clone()
            },
            "selectedSessionId": session_id.clone(),
            "lastDigestHash": "old-digest",
            "lastDigestMessageId": 44,
            "forwardNextAssistantMessageSessionIds": [session_id.clone(), "other-session"],
            "forwardNextAssistantMessageSessionId": session_id.clone(),
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
        .expect("fixture should encode"),
    )
    .expect("fixture should write");

    let response = state
        .kill_session(&session_id)
        .expect("session should kill");
    assert_eq!(response.preferences.telegram.default_session_id, None);

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("settings file should read"))
        .expect("settings file should parse");
    assert_eq!(value["configMigratedToAppState"], json!(true));
    assert!(value["config"].get("botToken").is_none());
    assert!(value["config"].get("defaultSessionId").is_none());
    assert!(value.get("selectedSessionId").is_none());
    assert!(value.get("lastDigestHash").is_none());
    assert!(value.get("lastDigestMessageId").is_none());
    assert_eq!(
        value["forwardNextAssistantMessageSessionIds"],
        json!(["other-session"])
    );
    assert_eq!(
        value["forwardNextAssistantMessageSessionId"],
        json!("other-session")
    );
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

#[test]
fn kill_session_does_not_reimport_migrated_file_config_after_default_reset() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-kill-default-reset-stale-mirror-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
    let state = test_app_state();
    let (project_id, session_id) = create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();
    fs::create_dir_all(path.parent().expect("settings path should have a parent"))
        .expect("settings dir should create");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "configMigratedToAppState": true,
            "config": {
                "enabled": true,
                "forwardAssistantReplies": true,
                "subscribedProjectIds": [project_id.clone()],
                "defaultProjectId": project_id,
                "defaultSessionId": session_id.clone()
            },
            "selectedSessionId": session_id.clone(),
            "assistantForwardingCursors": {
                (session_id.clone()): {
                    "messageId": "message-1",
                    "textChars": 10
                }
            },
            "chatId": 123
        }))
        .expect("fixture should encode"),
    )
    .expect("fixture should write");

    let response = state
        .kill_session(&session_id)
        .expect("session should kill without importing migrated mirror");

    assert_eq!(response.preferences.telegram, TelegramUiConfig::default());
    assert_eq!(
        state.snapshot().preferences.telegram,
        TelegramUiConfig::default()
    );

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("settings file should read"))
        .expect("settings file should parse");
    assert_eq!(value["configMigratedToAppState"], json!(true));
    assert_eq!(value["config"]["enabled"], json!(false));
    assert_eq!(value["config"]["forwardAssistantReplies"], json!(false));
    assert!(value["config"].get("subscribedProjectIds").is_none());
    assert!(value["config"].get("defaultProjectId").is_none());
    assert!(value["config"].get("defaultSessionId").is_none());
    assert!(value.get("selectedSessionId").is_none());
    assert!(value.get("assistantForwardingCursors").is_none());
    assert_eq!(value["chatId"], json!(123));
}
