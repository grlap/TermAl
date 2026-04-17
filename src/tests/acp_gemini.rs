// ACP + Gemini runtime configuration tests — `loadSession` capability
// detection, session resume with/without load support, invalid-identifier
// handling, interactive-shell setting defaults, dotenv path handling,
// auth-method selection, and override-file writing.
//
// Extracted from tests.rs as a cohesive cluster covering
// `acp_supports_session_load`, `acp_session_resume_*`, Gemini settings,
// and `gemini_interactive_shell_warning` surfaces. A later
// `gemini_invalid_session_load_falls_back_to_session_new` test is not
// included here because it sits beyond the agent-readiness cluster in
// mod.rs.

use super::*;

// Tests that ACP initialize reads load-session support from agent capabilities.
#[test]
fn acp_supports_session_load_reads_agent_capabilities() {
    assert_eq!(
        acp_supports_session_load(&json!({
            "agentCapabilities": {
                "loadSession": false,
            }
        })),
        Some(false)
    );
    assert_eq!(
        acp_supports_session_load(&json!({
            "agentCapabilities": {
                "loadSession": true,
            }
        })),
        Some(true)
    );
}

// Tests that ACP initialize also reads legacy capability envelopes.
#[test]
fn acp_supports_session_load_reads_legacy_capabilities() {
    assert_eq!(
        acp_supports_session_load(&json!({
            "capabilities": {
                "loadSession": false,
            }
        })),
        Some(false)
    );
    assert_eq!(acp_supports_session_load(&json!({})), None);
}

// Tests that ACP runtimes do not assume session/load support before initialize reports it.
#[test]
fn acp_runtime_state_defaults_session_load_support_to_unknown() {
    assert_eq!(AcpRuntimeState::default().supports_session_load, None);
}

// Tests that ACP resumes still attempt session/load when initialize omitted the capability bit.
#[test]
fn acp_session_resume_attempts_load_when_session_load_support_is_unknown() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Resume".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Ask),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("Cursor session should be created");
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState::default()));
    let writer = SharedBufferWriter::default();
    let thread_writer = writer.clone();
    let thread_pending_requests = pending_requests.clone();
    let thread_state = state.clone();
    let thread_runtime_state = runtime_state.clone();
    let thread_session_id = created.session_id.clone();
    let handle = std::thread::spawn(move || {
        let mut stdin = thread_writer;
        ensure_acp_session_ready(
            &mut stdin,
            &thread_pending_requests,
            &thread_state,
            &thread_session_id,
            &thread_runtime_state,
            AcpAgent::Cursor,
            &AcpPromptCommand {
                cwd: "/tmp".to_owned(),
                cursor_mode: Some(CursorMode::Ask),
                model: "auto".to_owned(),
                prompt: "Resume the prior session".to_owned(),
                resume_session_id: Some("cursor-session-1".to_owned()),
            },
        )
    });

    let (_load_request_id, load_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    load_sender
        .send(Ok(json!({
            "configOptions": [
                {
                    "id": "model",
                    "currentValue": "auto",
                    "options": [
                        {
                            "value": "auto",
                            "name": "Auto"
                        }
                    ]
                },
                {
                    "id": "mode",
                    "currentValue": "ask",
                    "options": [
                        {
                            "value": "ask",
                            "name": "Ask"
                        }
                    ]
                }
            ]
        })))
        .expect("session/load response should send");

    let external_session_id = handle
        .join()
        .expect("Cursor ACP worker should finish")
        .expect("Cursor resume should reuse the persisted session");
    assert_eq!(external_session_id, "cursor-session-1");

    let written = writer.contents();
    assert!(
        written.contains("\"method\":\"session/load\""),
        "session/load request should be written\n{written}"
    );
    assert!(
        !written.contains("\"method\":\"session/new\""),
        "session/new should not be written when resuming with unknown capability support\n{written}"
    );

    let session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Cursor session should be present");
    assert_eq!(
        session.external_session_id.as_deref(),
        Some("cursor-session-1")
    );

    let runtime_state = runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned");
    assert_eq!(
        runtime_state.current_session_id.as_deref(),
        Some("cursor-session-1")
    );
    assert_eq!(runtime_state.supports_session_load, Some(true));
}

// Tests that ACP skips session/load when initialize explicitly reports it unsupported.
#[test]
fn acp_session_resume_skips_load_when_session_load_is_explicitly_unsupported() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Resume".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Ask),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("Cursor session should be created");
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: None,
        is_loading_history: false,
        supports_session_load: Some(false),
    }));
    let writer = SharedBufferWriter::default();
    let thread_writer = writer.clone();
    let thread_pending_requests = pending_requests.clone();
    let thread_state = state.clone();
    let thread_runtime_state = runtime_state.clone();
    let thread_session_id = created.session_id.clone();
    let handle = std::thread::spawn(move || {
        let mut stdin = thread_writer;
        ensure_acp_session_ready(
            &mut stdin,
            &thread_pending_requests,
            &thread_state,
            &thread_session_id,
            &thread_runtime_state,
            AcpAgent::Cursor,
            &AcpPromptCommand {
                cwd: "/tmp".to_owned(),
                cursor_mode: Some(CursorMode::Ask),
                model: "auto".to_owned(),
                prompt: "Resume the prior session".to_owned(),
                resume_session_id: Some("cursor-session-1".to_owned()),
            },
        )
    });

    let (_new_request_id, new_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    new_sender
        .send(Ok(json!({
            "sessionId": "cursor-session-new",
            "configOptions": [
                {
                    "id": "model",
                    "currentValue": "auto",
                    "options": [
                        {
                            "value": "auto",
                            "name": "Auto"
                        }
                    ]
                },
                {
                    "id": "mode",
                    "currentValue": "ask",
                    "options": [
                        {
                            "value": "ask",
                            "name": "Ask"
                        }
                    ]
                }
            ]
        })))
        .expect("session/new response should send");

    let external_session_id = handle
        .join()
        .expect("Cursor ACP worker should finish")
        .expect("Cursor resume should start a fresh ACP session");
    assert_eq!(external_session_id, "cursor-session-new");

    let written = writer.contents();
    assert!(
        !written.contains("\"method\":\"session/load\""),
        "session/load should not be written when support is explicitly unavailable\n{written}"
    );
    assert!(
        written.contains("\"method\":\"session/new\""),
        "session/new should be written when support is explicitly unavailable\n{written}"
    );

    let session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Cursor session should be present");
    assert_eq!(
        session.external_session_id.as_deref(),
        Some("cursor-session-new")
    );

    let runtime_state = runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned");
    assert_eq!(
        runtime_state.current_session_id.as_deref(),
        Some("cursor-session-new")
    );
    assert_eq!(runtime_state.supports_session_load, Some(false));
}
// Tests that Gemini invalid-session detection searches wrapped anyhow error chains.
#[test]
fn gemini_invalid_session_load_error_matches_wrapped_chain_messages() {
    let err = anyhow::anyhow!("Invalid session identifier").context("session/load failed");
    assert!(is_gemini_invalid_session_load_error(&err));
}

// Tests that ACP invalid-session data inspection handles wrapper fields and depth limits.
#[test]
fn acp_invalid_session_identifier_detection_handles_wrappers_and_depth_limits() {
    assert!(acp_error_data_indicates_invalid_session_identifier(
        &json!({
            "details": [{
                "error": "invalidSessionId"
            }]
        })
    ));

    let mut boundary = json!("invalidSessionIdentifier");
    for _ in 0..10 {
        boundary = json!({ "details": boundary });
    }
    assert!(acp_error_data_indicates_invalid_session_identifier(
        &boundary
    ));

    let mut nested = json!("invalidSessionIdentifier");
    for _ in 0..11 {
        nested = json!({ "details": nested });
    }
    assert!(!acp_error_data_indicates_invalid_session_identifier(
        &nested
    ));
}

// Tests that Gemini settings overrides preserve existing fields while disabling interactive shell.
#[test]
fn disable_gemini_interactive_shell_in_settings_preserves_other_values() {
    let mut settings = json!({
        "security": {
            "auth": {
                "selectedType": "oauth-personal"
            }
        },
        "tools": {
            "shell": {
                "enableInteractiveShell": true,
                "pager": "less"
            }
        }
    });

    disable_gemini_interactive_shell_in_settings(&mut settings);

    assert_eq!(
        settings.pointer("/tools/shell/enableInteractiveShell"),
        Some(&Value::Bool(false))
    );
    assert_eq!(
        settings.pointer("/tools/shell/pager"),
        Some(&Value::String("less".to_owned()))
    );
    assert_eq!(
        settings.pointer("/security/auth/selectedType"),
        Some(&Value::String("oauth-personal".to_owned()))
    );
}

// Tests that Gemini settings overrides create the full shell path from an empty object.
#[test]
fn disable_gemini_interactive_shell_in_settings_builds_shell_path_from_empty_object() {
    let mut settings = json!({});

    disable_gemini_interactive_shell_in_settings(&mut settings);

    assert_eq!(
        settings.pointer("/tools/shell/enableInteractiveShell"),
        Some(&Value::Bool(false))
    );
}

// Tests that malformed Gemini settings do not block the Windows override path.
#[test]
fn load_gemini_settings_json_ignores_malformed_input() {
    let settings_path =
        std::env::temp_dir().join(format!("termal-gemini-settings-invalid-{}", Uuid::new_v4()));
    fs::write(
        &settings_path,
        r#"{"security": { "auth": { "selectedType": "oauth-personal" }"#,
    )
    .expect("invalid Gemini settings should be written");

    let loaded = load_gemini_settings_json(Some(settings_path.as_path()));
    assert_eq!(loaded, json!({}));
    assert_eq!(
        gemini_selected_auth_type_from_settings_file(settings_path.as_path()),
        None
    );

    let _ = fs::remove_file(settings_path);
}

// Tests that Gemini ACP launch ignores repository dotenv files for child env injection.
#[test]
fn gemini_dotenv_env_pairs_ignore_workspace_env_files() {
    let project_root =
        std::env::temp_dir().join(format!("termal-gemini-dotenv-env-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should be created");
    fs::write(
        project_root.join(".env"),
        "GEMINI_API_KEY=dotenv-gemini-key\nexport GOOGLE_API_KEY='vertex-key'\nGOOGLE_CLOUD_PROJECT=demo-project\nGOOGLE_CLOUD_LOCATION=us-central1\n",
    )
    .expect("Gemini dotenv file should be written");

    let overrides = gemini_dotenv_env_pairs()
        .into_iter()
        .collect::<HashMap<_, _>>();

    assert!(overrides.is_empty());

    let _ = fs::remove_dir_all(project_root);
}

// Tests that Gemini dotenv lookup resolves home-directory files without walking the workdir.
#[test]
fn find_gemini_env_file_reads_home_directory_env_files() {
    let _env_lock = TEST_HOME_ENV_MUTEX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home_dir = std::env::temp_dir().join(format!("termal-gemini-home-env-{}", Uuid::new_v4()));
    let gemini_dir = home_dir.join(".gemini");
    fs::create_dir_all(&gemini_dir).expect("Gemini home directory should be created");

    {
        let _home_env = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home_dir);
        assert_eq!(find_gemini_env_file(), None);
        let gemini_env = gemini_dir.join(".env");
        fs::write(&gemini_env, "GEMINI_API_KEY=home-gemini-key\n")
            .expect("Gemini home env should be written");
        assert_eq!(find_gemini_env_file(), Some(gemini_env.clone()));

        fs::remove_file(&gemini_env).expect("Gemini home env should be removed");
        let fallback_env = home_dir.join(".env");
        fs::write(&fallback_env, "GEMINI_API_KEY=home-fallback-key\n")
            .expect("home fallback env should be written");
        assert_eq!(find_gemini_env_file(), Some(fallback_env));
    }

    let _ = fs::remove_dir_all(home_dir);
}

// Tests that Gemini ACP auth selection ignores workspace dotenv credentials.
#[test]
fn select_acp_auth_method_ignores_workspace_dotenv_credentials() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-gemini-auth-method-dotenv-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should be created");
    fs::write(
        project_root.join(".env"),
        "GEMINI_API_KEY=dotenv-gemini-key\n",
    )
    .expect("Gemini dotenv file should be written");

    let initialize_result = json!({
        "authMethods": [
            { "id": "vertex-ai" },
            { "id": "gemini-api-key" }
        ]
    });
    assert_eq!(
        select_acp_auth_method(
            &initialize_result,
            AcpAgent::Gemini,
            project_root
                .to_str()
                .expect("temp path should be valid UTF-8"),
        ),
        None
    );

    let _ = fs::remove_dir_all(project_root);
}

// Tests that TermAl prepares a Windows Gemini system-settings override file.
#[test]
fn prepare_termal_gemini_system_settings_writes_override_file() {
    if !cfg!(windows) {
        return;
    }

    let project_root =
        std::env::temp_dir().join(format!("termal-gemini-system-settings-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("Gemini override project root should be created");
    let workdir = project_root
        .to_str()
        .expect("test workdir should be valid UTF-8");

    let settings_path = prepare_termal_gemini_system_settings(workdir)
        .expect("Gemini settings override should prepare")
        .expect("Windows should create a Gemini settings override");
    let written: Value = serde_json::from_str(
        &fs::read_to_string(&settings_path).expect("Gemini override file should be readable"),
    )
    .expect("Gemini override file should parse");

    assert_eq!(
        written.pointer("/tools/shell/enableInteractiveShell"),
        Some(&Value::Bool(false))
    );

    let _ = fs::remove_dir_all(project_root);
}

// Tests that Gemini interactive-shell warnings explain the TermAl override on Windows.
#[test]
fn gemini_interactive_shell_warning_respects_workspace_settings() {
    if !cfg!(windows) {
        return;
    }

    // Hold the home-env mutex so this test's USERPROFILE and
    // GEMINI_CLI_SYSTEM_SETTINGS_PATH redirects don't race with other
    // home-env tests that run in parallel.
    let _env_lock = TEST_HOME_ENV_MUTEX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let project_root = std::env::temp_dir().join(format!(
        "termal-gemini-interactive-shell-{}",
        Uuid::new_v4()
    ));
    let settings_dir = project_root.join(".gemini");
    fs::create_dir_all(&settings_dir).expect("Gemini settings directory should be created");
    let settings_path = settings_dir.join("settings.json");
    let workdir = project_root
        .to_str()
        .expect("test workdir should be valid UTF-8");

    // Point GEMINI_CLI_SYSTEM_SETTINGS_PATH at a path that does not exist so
    // the real C:\ProgramData\gemini-cli\settings.json (written by TermAl with
    // enableInteractiveShell=false) does not shadow the project setting we are
    // testing here.
    let absent_system_settings = project_root.join("no-system-settings.json");
    let _system_env =
        ScopedEnvVar::set_path("GEMINI_CLI_SYSTEM_SETTINGS_PATH", &absent_system_settings);

    // Redirect USERPROFILE to an empty temp dir so the developer's real
    // ~/.gemini/settings.json is not consulted either.
    let empty_home = std::env::temp_dir().join(format!("termal-gemini-home-{}", Uuid::new_v4()));
    fs::create_dir_all(&empty_home).expect("empty home dir should be created");
    let _home_env = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &empty_home);

    fs::write(
        &settings_path,
        r#"{"tools":{"shell":{"enableInteractiveShell":true}}}"#,
    )
    .expect("enabled Gemini settings should be written");
    let enabled_warning = gemini_interactive_shell_warning(workdir)
        .expect("enabled interactive shell should warn on Windows");
    assert!(enabled_warning.contains("TermAl forces Gemini"));
    assert!(enabled_warning.contains(&display_path_for_user(&settings_path)));

    fs::write(
        &settings_path,
        r#"{"tools":{"shell":{"enableInteractiveShell":false}}}"#,
    )
    .expect("disabled Gemini settings should be written");
    assert_eq!(gemini_interactive_shell_warning(workdir), None);

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(empty_home);
}
