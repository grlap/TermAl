// Telegram relay lifecycle tests split out of `telegram.rs`. This module owns
// startup-from-saved-settings, fallback project selection, missing-config stop,
// config-save restart, and graceful-shutdown relay coverage.
//
// It deliberately does not own settings persistence normalization, assistant
// forwarding, digest delivery, or generic route/rate-limit coverage.

use super::telegram_support::create_telegram_settings_project_and_session;
use super::*;

#[test]
fn telegram_relay_runtime_reports_lifecycle_transitions_as_running_until_idle() {
    assert!(!TelegramRelayRuntimeState::Idle.is_running());
    assert!(TelegramRelayRuntimeState::Spawning.is_running());
    assert!(TelegramRelayRuntimeState::Running.is_running());
    assert!(TelegramRelayRuntimeState::Stopping.is_running());
}

#[test]
fn telegram_config_update_reflects_in_process_relay_runtime_status() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-runtime-status-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
    let state = test_app_state();
    let (project_id, _session_id) = create_telegram_settings_project_and_session(&state);
    let initial_revision = state.snapshot().revision;

    state.reset_telegram_relay_runtime_actions_for_tests();
    let response = state
        .update_telegram_config(UpdateTelegramConfigRequest {
            enabled: Some(true),
            forward_assistant_replies: None,
            bot_token: Some(Some("123456:secret".to_owned())),
            subscribed_project_ids: Some(vec![project_id.clone()]),
            default_project_id: None,
            default_session_id: None,
        })
        .expect("enabled relay config should save and start");

    assert!(response.enabled);
    assert!(response.configured);
    assert!(response.running);
    assert_eq!(response.lifecycle, TelegramLifecycle::InProcess);
    let snapshot = state.snapshot();
    assert!(snapshot.revision > initial_revision);
    assert!(snapshot.preferences.telegram.enabled);
    assert_eq!(
        snapshot.preferences.telegram.subscribed_project_ids,
        vec![project_id.clone()]
    );
    let path = state.telegram_bot_file_path();
    let value: Value = serde_json::from_slice(&fs::read(&path).expect("settings file should read"))
        .expect("settings file should parse");
    assert!(value["config"].get("botToken").is_none());
    assert_eq!(
        state
            .saved_telegram_bot_token()
            .expect("saved token should read")
            .as_deref(),
        Some("123456:secret")
    );

    let stopped = state
        .update_telegram_config(UpdateTelegramConfigRequest {
            enabled: Some(false),
            forward_assistant_replies: None,
            bot_token: None,
            subscribed_project_ids: None,
            default_project_id: None,
            default_session_id: None,
        })
        .expect("disabled relay config should save and stop");

    assert!(!stopped.enabled);
    assert!(stopped.configured);
    assert!(!stopped.running);
    assert_eq!(stopped.lifecycle, TelegramLifecycle::InProcess);
    assert_eq!(
        state.take_telegram_relay_runtime_actions_for_tests(),
        vec![
            TelegramRelayRuntimeActionForTest::Start {
                project_id: project_id.clone(),
                subscribed_project_ids: vec![project_id],
            },
            TelegramRelayRuntimeActionForTest::Stop,
        ]
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn telegram_startup_from_saved_settings_starts_relay_with_single_subscribed_fallback() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-startup-single-fallback-home-{}",
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
                "enabled": true,
                "botToken": "123456:secret",
                "subscribedProjectIds": [project_id.clone()]
            },
            "chatId": 123
        }))
        .expect("fixture should encode"),
    )
    .expect("fixture should write");

    state.reset_telegram_relay_runtime_actions_for_tests();
    state.reconcile_telegram_relay_from_saved_settings();

    let status = state.telegram_status().expect("status should load");
    assert!(status.enabled);
    assert!(status.configured);
    assert!(status.running);
    assert_eq!(status.subscribed_project_ids, vec![project_id.clone()]);
    assert_eq!(
        status.default_project_id.as_deref(),
        Some(project_id.as_str())
    );
    assert_eq!(
        state.take_telegram_relay_runtime_actions_for_tests(),
        vec![TelegramRelayRuntimeActionForTest::Start {
            project_id: project_id.clone(),
            subscribed_project_ids: vec![project_id.clone()],
        }]
    );

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("settings file should read"))
        .expect("settings file should parse");
    assert_eq!(value["configMigratedToAppState"], json!(true));
    assert!(value["config"].get("botToken").is_none());
    assert_eq!(
        value["config"]["subscribedProjectIds"],
        json!([project_id.clone()])
    );
    assert_eq!(value["config"]["defaultProjectId"], json!(project_id));
    assert_eq!(value["chatId"], json!(123));
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn telegram_startup_with_multiple_subscribed_projects_and_no_default_stops_relay() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-startup-no-default-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
    let state = test_app_state();
    let (project_1, _session_1) = create_telegram_settings_project_and_session(&state);
    let (project_2, _session_2) = create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();
    fs::create_dir_all(path.parent().expect("settings path should have a parent"))
        .expect("settings dir should create");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "config": {
                "enabled": true,
                "botToken": "123456:secret",
                "subscribedProjectIds": [project_1.clone(), project_2.clone()]
            },
            "chatId": 123
        }))
        .expect("fixture should encode"),
    )
    .expect("fixture should write");

    state.reset_telegram_relay_runtime_actions_for_tests();
    state.reconcile_telegram_relay_from_saved_settings();

    let status = state.telegram_status().expect("status should load");
    assert!(status.enabled);
    assert!(status.configured);
    assert!(!status.running);
    assert_eq!(
        status.subscribed_project_ids,
        vec![project_1.clone(), project_2.clone()]
    );
    assert_eq!(status.default_project_id, None);
    assert_eq!(
        state.take_telegram_relay_runtime_actions_for_tests(),
        vec![TelegramRelayRuntimeActionForTest::Stop]
    );

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("settings file should read"))
        .expect("settings file should parse");
    assert_eq!(value["configMigratedToAppState"], json!(true));
    assert!(value["config"].get("botToken").is_none());
    assert_eq!(
        value["config"]["subscribedProjectIds"],
        json!([project_1, project_2])
    );
    assert!(value["config"].get("defaultProjectId").is_none());
    assert_eq!(value["chatId"], json!(123));
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn telegram_startup_from_saved_settings_stops_running_relay_when_token_missing() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-startup-missing-token-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
    let state = test_app_state();
    let (project_id, _session_id) = create_telegram_settings_project_and_session(&state);

    let started = state
        .update_telegram_config(UpdateTelegramConfigRequest {
            enabled: Some(true),
            forward_assistant_replies: None,
            bot_token: Some(Some("123456:secret".to_owned())),
            subscribed_project_ids: Some(vec![project_id.clone()]),
            default_project_id: None,
            default_session_id: None,
        })
        .expect("enabled relay config should save and start");
    assert!(started.running);
    state
        .delete_saved_telegram_bot_token()
        .expect("saved token should delete");

    state.reconcile_telegram_relay_from_saved_settings();

    let status = state.telegram_status().expect("status should load");
    assert!(status.enabled);
    assert!(!status.configured);
    assert!(!status.running);
    assert_eq!(
        state.take_telegram_relay_runtime_actions_for_tests(),
        vec![
            TelegramRelayRuntimeActionForTest::Start {
                project_id: project_id.clone(),
                subscribed_project_ids: vec![project_id],
            },
            TelegramRelayRuntimeActionForTest::Stop,
        ]
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn telegram_config_save_restarts_running_relay_with_new_default_project() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-config-save-restart-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
    let state = test_app_state();
    let (project_1, _session_1) = create_telegram_settings_project_and_session(&state);
    let (project_2, _session_2) = create_telegram_settings_project_and_session(&state);

    state.reset_telegram_relay_runtime_actions_for_tests();
    let started = state
        .update_telegram_config(UpdateTelegramConfigRequest {
            enabled: Some(true),
            forward_assistant_replies: None,
            bot_token: Some(Some("123456:secret".to_owned())),
            subscribed_project_ids: Some(vec![project_1.clone(), project_2.clone()]),
            default_project_id: Some(Some(project_1.clone())),
            default_session_id: None,
        })
        .expect("initial relay config should save and start");
    assert!(started.running);
    assert_eq!(
        started.default_project_id.as_deref(),
        Some(project_1.as_str())
    );

    let restarted = state
        .update_telegram_config(UpdateTelegramConfigRequest {
            enabled: None,
            forward_assistant_replies: None,
            bot_token: None,
            subscribed_project_ids: None,
            default_project_id: Some(Some(project_2.clone())),
            default_session_id: None,
        })
        .expect("changed default project should restart relay");

    assert!(restarted.running);
    assert_eq!(
        restarted.default_project_id.as_deref(),
        Some(project_2.as_str())
    );
    assert_eq!(
        state.take_telegram_relay_runtime_actions_for_tests(),
        vec![
            TelegramRelayRuntimeActionForTest::Start {
                project_id: project_1.clone(),
                subscribed_project_ids: vec![project_1.clone(), project_2.clone()],
            },
            TelegramRelayRuntimeActionForTest::Start {
                project_id: project_2.clone(),
                subscribed_project_ids: vec![project_1, project_2],
            },
        ]
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn telegram_graceful_shutdown_stops_running_in_process_relay() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-graceful-shutdown-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
    let state = test_app_state();
    let (project_id, _session_id) = create_telegram_settings_project_and_session(&state);

    state.reset_telegram_relay_runtime_actions_for_tests();
    let started = state
        .update_telegram_config(UpdateTelegramConfigRequest {
            enabled: Some(true),
            forward_assistant_replies: None,
            bot_token: Some(Some("123456:secret".to_owned())),
            subscribed_project_ids: Some(vec![project_id.clone()]),
            default_project_id: None,
            default_session_id: None,
        })
        .expect("enabled relay config should save and start");
    assert!(started.running);

    state.stop_telegram_relay_runtime();

    let status = state.telegram_status().expect("status should load");
    assert!(status.enabled);
    assert!(status.configured);
    assert!(!status.running);
    assert_eq!(
        state.take_telegram_relay_runtime_actions_for_tests(),
        vec![
            TelegramRelayRuntimeActionForTest::Start {
                project_id: project_id.clone(),
                subscribed_project_ids: vec![project_id],
            },
            TelegramRelayRuntimeActionForTest::Stop,
        ]
    );
    let _ = fs::remove_dir_all(&home);
}
