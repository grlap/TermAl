// Fixture-only environment delivery, persistence privacy and declared verdicts.
use super::*;

const NAME: &str = "SUPABASE_TEST_URL";
const VALUE: &str = "fixture-value-not-a-service";

fn values(value: &str) -> ProjectEnvironment {
    ProjectEnvironment(BTreeMap::from([(NAME.to_owned(), value.to_owned())]))
}

fn project(state: &AppState, session_id: &str, root: &FsPath) -> String {
    let mut inner = state.inner.lock().unwrap();
    let project = inner.create_project(Some("environment fixture".to_owned()),
        root.to_string_lossy().into_owned(), LOCAL_REMOTE_ID.to_owned());
    let index = inner.find_session_index(session_id).unwrap();
    inner.sessions[index].session.project_id = Some(project.id.clone());
    inner.sessions[index].session.workdir = root.to_string_lossy().into_owned();
    project.id
}

fn declaration(root: &FsPath, names: &str) {
    fs::write(root.join(ENGRAM_DECLARATION_FILE), format!(
        "[[test]]\ncommand = \"fixture-tests --run\"\nartifact = \".results/out.trx\"\n{names}\n")).unwrap();
}

fn binding(root: &FsPath) -> EngramDeclaredBinding {
    engram_declared_bind(&engram_declared_words("fixture-tests --run").unwrap(),
        &EngramCheckTarget { root: root.to_owned(), directory: root.to_owned(), common_dir_key: None }).0.unwrap()
}

#[test]
fn project_environment_declaration_accepts_names_and_preserves_exact_matching() {
    let root = TestTempRoot::create("project-env-parser");
    declaration(root.path(), "env = [\"SUPABASE_TEST_URL\"]");
    let captured = binding(root.path());
    assert_eq!(captured.entry.env, [NAME]);
    assert!(engram_declared_bind(&engram_declared_words("fixture-tests --other").unwrap(),
        &EngramCheckTarget { root: root.path().to_owned(), directory: root.path().to_owned(), common_dir_key: None }).0.is_none());
    for bad in ["env = [\"A=value\"]", "env = [\"A\", \"A\"]", "env = { A = \"value\" }"] {
        declaration(root.path(), bad);
        assert!(matches!(engram_load_declaration(root.path()), EngramDeclaration::Disabled { .. }));
    }
    declaration(root.path(), "");
    assert!(binding(root.path()).entry.env.is_empty());
}

#[test]
fn project_environment_settings_persist_privately_and_do_not_retarget_applied_values() {
    let state = test_app_state();
    let root = TestTempRoot::create("project-env-settings");
    let session = test_session_id(&state, Agent::Codex);
    let id = project(&state, &session, root.path());
    let response = state.update_project_environment(&id, values(VALUE)).unwrap();
    assert!(!serde_json::to_string(&response).unwrap().contains(VALUE));
    let selected = {
        let inner = state.inner.lock().unwrap();
        let selected = project_environment_for_session_locked(&inner, &session);
        let stored = inner.find_project(&id).unwrap();
        assert!(!format!("{stored:?}").contains(VALUE));
        let restored: Project = serde_json::from_value(serde_json::to_value(stored).unwrap()).unwrap();
        assert_eq!(restored.environment.0.get(NAME).unwrap(), VALUE);
        selected
    };
    state.update_project_environment(&id, values("replacement-fixture")).unwrap();
    assert_eq!(selected.0.get(NAME).unwrap(), VALUE);
    let inner = state.inner.lock().unwrap();
    assert_eq!(project_environment_for_session_locked(&inner, &session).0.get(NAME).unwrap(), "replacement-fixture");
    assert!(!serde_json::to_string(&state.snapshot_from_inner(&inner)).unwrap().contains("replacement-fixture"));
}

#[test]
fn project_environment_selection_uses_only_the_executing_local_project() {
    let state = test_app_state();
    let root = TestTempRoot::create("project-env-isolation");
    let session = test_session_id(&state, Agent::Codex);
    let id = project(&state, &session, root.path());
    state.update_project_environment(&id, values(VALUE)).unwrap();
    let other = test_session_id(&state, Agent::Claude);
    let mut inner = state.inner.lock().unwrap();
    assert!(project_environment_for_session_locked(&inner, &other).is_empty());
    let index = inner.find_session_index(&session).unwrap();
    inner.sessions[index].remote_id = Some("remote-fixture".to_owned());
    inner.sessions[index].remote_session_id = Some("remote-session".to_owned());
    assert!(project_environment_for_session_locked(&inner, &session).is_empty());
    inner.projects.iter_mut().find(|p| p.id == id).unwrap().remote_id = "remote-fixture".to_owned();
    drop(inner);
    assert!(state.update_project_environment(&id, values("must-not-forward")).is_err());
}

#[test]
fn project_environment_child_receives_selected_value() {
    const CHILD: &str = "TERMAL_TEST_PROJECT_ENV_CHILD";
    if std::env::var_os(CHILD).is_some() {
        assert_eq!(std::env::var(NAME).unwrap(), VALUE);
        println!("project-environment-child-verified");
        return;
    }
    let mut child = Command::new(std::env::current_exe().unwrap());
    child.args(["--exact", "tests::project_environment::project_environment_child_receives_selected_value", "--nocapture"])
        .env(CHILD, "1").env(NAME, "inherited-unrelated-value");
    let selected = if cfg!(windows) {
        ProjectEnvironment(BTreeMap::from([(NAME.to_ascii_lowercase(), VALUE.to_owned())]))
    } else { values(VALUE) };
    AppliedProjectEnvironment { values: selected }.apply(&mut child);
    let output = child.output().unwrap();
    assert!(output.status.success(), "fixture child failed: {}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("project-environment-child-verified"),
        "child filter must execute the value assertion");
}

#[test]
fn project_environment_missing_overrides_both_artifact_verdict_routes_and_exit() {
    let root = TestTempRoot::create("project-env-verdict");
    run_git_test_command(root.path(), &["init", "--quiet"]);
    run_git_test_command(root.path(), &["config", "user.email", "fixture@example.invalid"]);
    run_git_test_command(root.path(), &["config", "user.name", "Fixture"]);
    fs::write(root.path().join(".gitignore"), ".results/\n").unwrap();
    declaration(root.path(), "env = [\"SUPABASE_TEST_URL\"]");
    run_git_test_command(root.path(), &["add", ".gitignore", ENGRAM_DECLARATION_FILE]);
    run_git_test_command(root.path(), &["commit", "--quiet", "-m", "fixture"]);
    let mut captured = binding(root.path());
    fs::create_dir(root.path().join(".results")).unwrap();
    fs::write(root.path().join(".results/out.trx"), include_bytes!("fixtures/declared-trx/pass.trx")).unwrap();
    for selected in [ProjectEnvironment::default(), values(" \t")] {
        AppliedProjectEnvironment { values: selected }.bind(&mut captured);
        for exit in [EngramCommandExit::Code(0), EngramCommandExit::Code(1)] {
            assert_eq!(engram_declared_result(&captured, exit).verdict,
                EngramTrxVerdict::Unknown(format!("environment_missing:{NAME}")));
        }
        assert_eq!(engram_declared_artifact_result(&captured).verdict,
            EngramTrxVerdict::Unknown(format!("environment_missing:{NAME}")));
    }
    AppliedProjectEnvironment { values: values(VALUE) }.bind(&mut captured);
    assert!(matches!(engram_declared_result(&captured, EngramCommandExit::Code(0)).verdict, EngramTrxVerdict::Pass));
    assert!(matches!(engram_declared_result(&captured, EngramCommandExit::Code(1)).verdict, EngramTrxVerdict::Fail(_)));
    assert!(!serde_json::to_string(&captured).unwrap().contains(VALUE));
}

#[test]
fn project_environment_codex_thread_overlay_refreshes_without_mutating_prior_selection() {
    let state = test_app_state();
    let root = TestTempRoot::create("project-env-codex");
    let session = test_session_id(&state, Agent::Codex);
    let id = project(&state, &session, root.path());
    state.update_project_environment(&id, values(VALUE)).unwrap();
    let (runtime, _commands, process) = test_shared_codex_runtime("project-env-setup");
    install_single_wire_codex_fixture(&state, Some(runtime.clone()));
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(), input_tx: runtime.input_tx.clone(), process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle { runtime: runtime.clone(), session_id: session.clone() }),
        });
    }
    let token = RuntimeToken::Codex(runtime.runtime_id.clone());
    let (first, _) = state.termal_delegation_mcp_codex_config_for_shared_runtime(&session, &token, root.path(), CodexApprovalPolicy::Never).unwrap();
    assert_eq!(first.pointer("/shell_environment_policy/set/SUPABASE_TEST_URL").unwrap(), VALUE);
    state.update_project_environment(&id, values("next-runtime")).unwrap();
    {
        let inner = state.inner.lock().unwrap();
        assert_eq!(inner.sessions[inner.find_session_index(&session).unwrap()].project_environment.values.0.get(NAME).unwrap(), VALUE);
    }
    let (next, _) = state.termal_delegation_mcp_codex_config_for_shared_runtime(&session, &token, root.path(), CodexApprovalPolicy::Never).unwrap();
    assert_eq!(next.pointer("/shell_environment_policy/set/SUPABASE_TEST_URL").unwrap(), "next-runtime");
    assert_eq!(first.pointer("/shell_environment_policy/set/SUPABASE_TEST_URL").unwrap(), VALUE);
    state.update_project_environment(&id, ProjectEnvironment::default()).unwrap();
    // Production order: clear/attach first, then thread setup. The attach
    // must retain the previous selection's names for the removal overlay.
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        record.clear_runtime();
        state.start_turn_on_record_with_environment(record, "env-attach".to_owned(), "fixture".to_owned(),
            Vec::new(), None, None, None, false, ProjectEnvironment::default()).unwrap();
    }
    let (removed, _) = state.termal_delegation_mcp_codex_config_for_shared_runtime(&session, &token, root.path(), CodexApprovalPolicy::Never).unwrap();
    assert_eq!(removed.pointer("/shell_environment_policy/set/SUPABASE_TEST_URL").unwrap(), "");
    assert!(process.wait().unwrap().success());
}

#[test]
fn project_environment_stale_setup_does_not_retire_sibling_runtime_or_apply_values() {
    for stale in ["stopped", "removed", "replaced"] {
        let state = test_app_state();
        let root = TestTempRoot::create("project-env-stale-setup");
        let session = test_session_id(&state, Agent::Codex);
        let id = project(&state, &session, root.path());
        state.update_project_environment(&id, values(VALUE)).unwrap();
        let sibling = test_session_id(&state, Agent::Codex);
        let (runtime, _commands, process) = test_shared_codex_runtime("project-env-shared");
        install_single_wire_codex_fixture(&state, Some(runtime.clone()));
        let token = RuntimeToken::Codex(runtime.runtime_id.clone());
        {
            let mut inner = state.inner.lock().unwrap();
            for current in [&session, &sibling] {
                let index = inner.find_session_index(current).unwrap();
                let record = &mut inner.sessions[index];
                record.session.project_id = Some(id.clone());
                record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                    runtime_id: runtime.runtime_id.clone(), input_tx: runtime.input_tx.clone(),
                    process: process.clone(), shared_session: Some(SharedCodexSessionHandle {
                        runtime: runtime.clone(), session_id: current.clone(),
                    }),
                });
                record.session.status = SessionStatus::Active;
                record.project_environment = AppliedProjectEnvironment { values: values("prior-selection") };
            }
            let index = inner.find_session_index(&session).unwrap();
            match stale {
                "stopped" => inner.sessions[index].clear_runtime(),
                "removed" => { assert!(inner.remove_session_at(index).is_some()); }
                _ => {
                    let SessionRuntime::Codex(handle) = &mut inner.sessions[index].runtime else { panic!("Codex fixture"); };
                    handle.runtime_id = "replacement-runtime".to_owned();
                }
            }
        }
        let pending: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
        let (input_tx, _input_rx) = mpsc::channel::<CodexRuntimeCommand>();
        let mut writer = Vec::new();
        let home = test_missing_shared_codex_home();
        let command = || CodexPromptCommand {
            active_turn_generation: 0, approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(), cwd: root.path().to_string_lossy().into_owned(),
            model: "gpt-5.4".to_owned(), prompt: "queued prompt".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium, service_tier: None,
            resume_thread_id: None, sandbox_mode: CodexSandboxMode::WorkspaceWrite,
            delivery_receipt: None,
        };
        let result = handle_shared_codex_prompt_command(
            &mut writer, &pending, &state, &runtime.runtime_id, &home,
            &runtime.sessions, &runtime.thread_sessions, &input_tx, None, &session, command(),
        );
        let result = handle_shared_codex_prompt_command_result(&state, &session, &token, 0, result);
        // The production writer retires the shared runtime for this outcome.
        if let Err(error) = &result {
            state.handle_shared_codex_runtime_exit(&runtime.runtime_id, Some(&format!("{error:#}"))).unwrap();
        }
        {
            let inner = state.inner.lock().unwrap();
            let sibling_record = &inner.sessions[inner.find_session_index(&sibling).unwrap()];
            assert!(sibling_record.runtime.matches_runtime_token(&token), "{stale}: sibling runtime retired");
            assert_eq!(sibling_record.session.status, SessionStatus::Active);
            if let Some(index) = inner.find_session_index(&session) {
                assert_eq!(inner.sessions[index].project_environment.values.0.get(NAME).unwrap(), "prior-selection");
            }
        }
        result.unwrap();
        let stale_request: Value = serde_json::from_slice(&writer).unwrap();
        assert_eq!(stale_request["method"], "thread/start");
        assert!(stale_request.pointer("/params/config/shell_environment_policy/set/SUPABASE_TEST_URL").is_none());
        assert!(!String::from_utf8_lossy(&writer).contains(VALUE));
        assert!(!String::from_utf8_lossy(&writer).contains("prior-selection"));
        assert!(state.shared_codex_runtime.lock().unwrap().as_ref()
            .is_some_and(|current| current.runtime_id == runtime.runtime_id));
        // The same writer can still emit a sibling's setup, with its own values.
        writer.clear();
        let result = handle_shared_codex_prompt_command(
            &mut writer, &pending, &state, &runtime.runtime_id, &home,
            &runtime.sessions, &runtime.thread_sessions, &input_tx, None, &sibling, command(),
        );
        handle_shared_codex_prompt_command_result(&state, &sibling, &token, 0, result).unwrap();
        let sibling_request: Value = serde_json::from_slice(&writer).unwrap();
        assert_eq!(sibling_request["method"], "thread/start");
        assert_eq!(sibling_request.pointer("/params/config/shell_environment_policy/set/SUPABASE_TEST_URL").unwrap(), VALUE);
        super::shared_codex_thread_setup::retire_pending_codex_thread_setups(&pending);
        assert!(process.wait().unwrap().success());
    }
}

#[test]
fn project_environment_start_notice_is_names_only_and_absence_does_not_affect_other_entries() {
    let state = test_app_state();
    let root = TestTempRoot::create("project-env-notice");
    run_git_test_command(root.path(), &["init", "--quiet"]);
    let session = test_session_id(&state, Agent::Codex);
    project(&state, &session, root.path());
    fs::create_dir(root.path().join("nested")).unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].session.workdir = root.path().join("nested").to_string_lossy().into_owned();
    }
    declaration(root.path(), "env = [\"SUPABASE_TEST_URL\"]");
    assert!(state.project_environment_notice(&session).unwrap().contains("environment_missing:SUPABASE_TEST_URL"));
    let mut captured = binding(root.path());
    AppliedProjectEnvironment { values: values(VALUE) }.bind(&mut captured);
    assert!(captured.environment_missing.is_none());
    declaration(root.path(), "");
    assert!(state.project_environment_notice(&session).is_none());
    let mut no_env = binding(root.path());
    AppliedProjectEnvironment::default().bind(&mut no_env);
    assert!(no_env.environment_missing.is_none());
}

#[tokio::test]
async fn project_environment_api_rejects_invalid_input_without_echoing_values() {
    let state = test_app_state();
    let response = app_router(state).oneshot(Request::builder().method("PATCH")
        .uri("/api/projects/missing/environment").header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"environment":{{"A":{{"secret":"{VALUE}"}}}}}}"#))).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), 8192).await.unwrap();
    assert!(!String::from_utf8_lossy(&body).contains(VALUE));
}

#[test]
fn project_environment_presence_uses_the_platforms_environment_name_equivalence() {
    let root = TestTempRoot::create("project-env-case");
    declaration(root.path(), "env = [\"SUPABASE_TEST_URL\"]");
    let mut captured = binding(root.path());
    let lower = ProjectEnvironment(BTreeMap::from([(NAME.to_ascii_lowercase(), VALUE.to_owned())]));
    AppliedProjectEnvironment { values: lower.runtime_selection() }.bind(&mut captured);
    assert_eq!(captured.environment_missing.is_none(), cfg!(windows));
    if !cfg!(windows) { assert_eq!(captured.environment_missing.as_deref(), Some(NAME)); }
}

#[test]
fn project_environment_notice_reads_the_linked_worktree_instead_of_the_project_checkout() {
    let state = test_app_state();
    let root = TestTempRoot::create("project-env-linked");
    let main = root.path().join("main");
    let linked = root.path().join("linked");
    fs::create_dir(&main).unwrap();
    run_git_test_command(&main, &["init", "--quiet"]);
    declaration(&main, "env = [\"MAIN_ONLY\"]");
    run_git_test_command(&main, &["add", ENGRAM_DECLARATION_FILE]);
    run_git_test_command(&main, &["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
        "commit", "--quiet", "--no-gpg-sign", "-m", "fixture"]);
    run_git_test_command(&main, &["worktree", "add", "--detach", linked.to_str().unwrap(), "HEAD"]);
    declaration(&linked, "env = [\"LINKED_ONLY\"]");
    fs::create_dir(linked.join("nested")).unwrap();
    let session = test_session_id(&state, Agent::Codex);
    project(&state, &session, &main);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].session.workdir = linked.join("nested").to_string_lossy().into_owned();
    }
    let notice = state.project_environment_notice(&session).unwrap();
    assert!(notice.contains("environment_missing:LINKED_ONLY"));
    assert!(!notice.contains("MAIN_ONLY"));
}

#[test]
fn project_environment_claude_writer_preserves_native_slash_commands_and_warns_on_ordinary_prompts() {
    let state = test_app_state();
    let root = TestTempRoot::create("project-env-claude-writer");
    run_git_test_command(root.path(), &["init", "--quiet"]);
    let session = test_session_id(&state, Agent::Claude);
    project(&state, &session, root.path());
    declaration(root.path(), "env = [\"SUPABASE_TEST_URL\"]");
    let replay = Arc::new(Mutex::new(None));
    let context = ClaudeRuntimeContext::new(state, session,
        RuntimeToken::Claude("fixture-writer".to_owned()), new_claude_turn_ownership(),
        replay.clone(), root.path().to_string_lossy().into_owned());
    for text in ["/compact", "/context", "/fixture-skill arg", "ordinary task"] {
        let mut wire = Vec::new();
        let prompt = ClaudePromptCommand { attachments: Vec::new(), replay_generation: Uuid::new_v4().to_string(),
            text: text.to_owned(), turn_generation: 0 };
        assert!(apply_claude_writer_command(&context, &mut wire, ClaudeRuntimeCommand::Prompt(prompt)));
        let written = replay.lock().unwrap().as_ref().unwrap().text.clone();
        let frame: Value = serde_json::from_slice(&wire).unwrap();
        let content = frame["message"]["content"].to_string();
        if text.starts_with('/') {
            assert_eq!(written, text);
            assert!(!content.contains("environment_missing"));
        } else {
            assert!(written.contains("environment_missing:SUPABASE_TEST_URL"));
            assert!(content.contains("ordinary task"));
            assert!(content.contains("environment_missing:SUPABASE_TEST_URL"));
        }
    }
}
