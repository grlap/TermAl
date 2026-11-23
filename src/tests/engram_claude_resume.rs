//! Engram configuration handed to Claude by the first dispatch after reload.
use super::*;

#[derive(Clone, Copy)]
enum Dispatch {
    HumanSend,
    HumanResume,
    HumanSendWithOlderHead,
    SelectedOrdinaryResume,
    UnselectedOrdinaryResume,
    Automatic,
    Activation,
    OrdinarySend,
}

const CLAUDE_SESSION_ID: &str = "01234567-89ab-4cde-8fab-0123456789ab";

fn restored_claude(dispatch: Dispatch, enabled: bool) -> (AppState, PathBuf, String) {
    let mut state = test_app_state();
    state.agent_runtime_spawning_enabled = true;
    let root = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("project");
    let workdir = root.join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    let project_id = create_test_project(&state, &root, "Restored Claude project");
    let session_id = create_test_project_session(&state, Agent::Claude, &project_id, &workdir);
    state
        .set_external_session_id(&session_id, CLAUDE_SESSION_ID.to_owned())
        .unwrap();
    fs::write(root.join(".engram-project"), format!("{project_id}\n")).unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        inner
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
            .unwrap()
            .engram = Some(EngramProjectSettings {
            enabled,
            execution_observability: enabled,
            binary_path: Some("C:/tools/engram.exe".to_owned()),
            home: Some("C:/engram-home".to_owned()),
            deadline_ms: Some(250),
            ..EngramProjectSettings::default()
        });
        if enabled {
            inner.engram_declared_project_ids.insert(project_id.clone());
        }
        inner
            .engram_declaration_checked_project_ids
            .insert(project_id.clone());
        inner.preferences.engram.developer_name = "resume-test".to_owned();
        let index = inner.find_session_index(&session_id).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.session.status = SessionStatus::Idle;
        let queued_source = match dispatch {
            Dispatch::HumanResume | Dispatch::Automatic | Dispatch::Activation => {
                Some(QueuedPromptSource::Human)
            }
            Dispatch::HumanSendWithOlderHead
            | Dispatch::SelectedOrdinaryResume
            | Dispatch::UnselectedOrdinaryResume => Some(QueuedPromptSource::User),
            _ => None,
        };
        if let Some(queued_source) = queued_source {
            queue_prompt_on_record_with_source(
                record,
                PendingPrompt {
                    id: "saved-human-prompt".to_owned(),
                    timestamp: stamp_now(),
                    text: "Continue the saved session".to_owned(),
                    expanded_text: None,
                    source: None,
                    attachments: Vec::new(),
                    engram_interrupted: false,
                    is_engram_retained: false,
                },
                Vec::new(),
                queued_source,
            );
            record.set_auto_dispatch_blocked(matches!(
                dispatch,
                Dispatch::HumanResume
                    | Dispatch::SelectedOrdinaryResume
                    | Dispatch::UnselectedOrdinaryResume
            ));
            if matches!(dispatch, Dispatch::UnselectedOrdinaryResume) {
                let mut history = record.queued_prompts[0].clone();
                history.pending_prompt.id = "historical-user-prompt".to_owned();
                history.pending_prompt.text = "Prior delivery with unknown outcome".to_owned();
                history.engram_ordinary_handoff = true;
                record.queued_prompts.push_front(history);
                sync_pending_prompts(record);
            }
        }
        state.commit_locked(&mut inner).unwrap();
    }

    let restored = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(!restored.engram_declared_project_ids.contains(&project_id));
    *state.inner.lock().unwrap() = restored;
    (state, root, session_id)
}

struct ClaudeLaunch {
    args: Vec<String>,
    config: Value,
    environment: BTreeMap<String, Option<String>>,
}

fn capture_claude_launch(
    state: &AppState,
    session_id: &str,
    dispatch: Dispatch,
) -> (std::result::Result<(), ApiError>, Option<ClaudeLaunch>) {
    // Stop at the OS launch boundary so the test needs neither Claude nor
    // Engram installed, while still observing the real command and its file.
    let (capture, launch) = mpsc::sync_channel(1);
    let executable = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("unused-claude");
    TEST_CLAUDE_EXECUTABLE.with(|slot| slot.replace(Some(executable)));
    TEST_DEDICATED_COMMAND_INSPECTOR.with(|slot| {
        slot.replace(Some(Box::new(move |command| {
            let args: Vec<_> = command
                .get_args()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect();
            let config_path = args
                .windows(2)
                .find(|pair| pair[0] == "--mcp-config")
                .map(|pair| &pair[1])
                .context("Claude launch has no MCP config path")?;
            let config: Value = serde_json::from_slice(&fs::read(config_path)?)?;
            let environment: BTreeMap<_, _> = command
                .get_envs()
                .map(|(name, value)| {
                    (
                        name.to_string_lossy().into_owned(),
                        value.map(|value| value.to_string_lossy().into_owned()),
                    )
                })
                .collect();
            capture.send(ClaudeLaunch {
                args,
                config,
                environment,
            })?;
            bail!("fixture stopped before Claude process launch")
        })));
    });
    let request = SendMessageRequest {
        text: if matches!(dispatch, Dispatch::HumanSendWithOlderHead) {
            "New Human prompt behind FIFO"
        } else {
            "Continue the saved session"
        }
        .to_owned(),
        expanded_text: None,
        attachments: Vec::new(),
        source_session_id: None,
        source_mailbox: None,
    };
    let dispatch_result = match dispatch {
        Dispatch::HumanSend | Dispatch::HumanSendWithOlderHead => state
            .dispatch_human_turn(session_id, request)
            .and_then(|result| match result {
                DispatchTurnResult::Dispatched(dispatch)
                | DispatchTurnResult::DispatchedAfterQueue(dispatch) => {
                    deliver_turn_dispatch(state, dispatch)
                        .into_public_result()
                        .map(|_| ())
                }
                DispatchTurnResult::Queued => Ok(()),
            }),
        Dispatch::HumanResume | Dispatch::UnselectedOrdinaryResume => {
            state.resume_session_queue(session_id).map(|_| ())
        }
        Dispatch::SelectedOrdinaryResume => {
            let selection = {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(session_id).unwrap()];
                HumanPromptSelection {
                    prompt: projected_pending_prompts(record)[0].clone(),
                    session_mutation_stamp: record.mutation_stamp,
                }
            };
            state
                .resume_selected_human_prompt(session_id, selection)
                .map(|_| ())
        }
        Dispatch::Automatic => state
            .start_next_queued_turn_off_lock(session_id, false, false)
            .map(|_| ())
            .map_err(|error| ApiError::internal(format!("{error:#}"))),
        Dispatch::Activation => {
            let owner = {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(session_id).unwrap()];
                QueuedTurnOwner::capture(record).unwrap()
            };
            state
                .dispatch_next_queued_turn_for_activation(session_id, owner)
                .map(|_| ())
                .map_err(|error| ApiError::internal(format!("{error:#}")))
        }
        Dispatch::OrdinarySend => state.dispatch_turn(session_id, request).map(|_| ()),
    };
    TEST_CLAUDE_EXECUTABLE.with(|slot| slot.borrow_mut().take());
    TEST_DEDICATED_COMMAND_INSPECTOR.with(|slot| slot.borrow_mut().take());

    (dispatch_result, launch.try_recv().ok())
}

fn assert_claude_launch(
    launch: ClaudeLaunch,
    session_id: &str,
    project_root: &FsPath,
    engram_expected: bool,
) {
    assert_claude_resume(&launch);
    let ClaudeLaunch {
        config,
        environment,
        ..
    } = launch;
    let servers = config["mcpServers"].as_object().unwrap();
    assert!(servers.contains_key(TERMAL_DELEGATION_MCP_SERVER_NAME));
    assert_eq!(
        environment["TERMAL_SESSION_ID"].as_deref(),
        Some(session_id)
    );

    let Some(engram) = servers.get(ENGRAM_MCP_SERVER_NAME) else {
        assert!(
            !engram_expected,
            "restored Claude launch omitted the Engram MCP server"
        );
        for key in ENGRAM_AGENT_PROCESS_ENV_NAMES {
            assert_eq!(environment.get(key), Some(&None));
        }
        return;
    };
    assert!(
        engram_expected,
        "ineligible project installed the Engram MCP server"
    );
    assert_eq!(engram["command"], "C:/tools/engram.exe");
    let engram_args = engram["args"].as_array().unwrap();
    let project_file = engram_args
        .windows(2)
        .find(|pair| pair[0] == "--project-file")
        .and_then(|pair| pair[1].as_str())
        .expect("Engram launch should name its project declaration");
    assert_eq!(
        fs::canonicalize(project_file).unwrap(),
        fs::canonicalize(project_root.join(".engram-project")).unwrap()
    );
    assert!(
        engram_args
            .windows(2)
            .any(|pair| pair[0] == "--home" && pair[1] == "C:/engram-home")
    );
    for (name, expected) in [
        (ENGRAM_HOME_ENV, "C:/engram-home"),
        (ENGRAM_ACTOR_ID_ENV, "resume-test/claude"),
        (ENGRAM_SESSION_ID_ENV, session_id),
    ] {
        assert_eq!(engram["env"][name], expected);
        assert_eq!(environment[name].as_deref(), Some(expected));
    }
    let actor_context = environment[ENGRAM_ACTOR_CONTEXT_ENV].as_deref().unwrap();
    assert!(actor_context.starts_with("agent=claude;model="));
    assert_eq!(engram["env"][ENGRAM_ACTOR_CONTEXT_ENV], actor_context);
}

fn assert_claude_resume(launch: &ClaudeLaunch) {
    assert!(
        launch
            .args
            .windows(2)
            .any(|pair| pair == ["--resume", CLAUDE_SESSION_ID])
    );
}

fn assert_restored_claude_launch(dispatch: Dispatch, enabled: bool) {
    let (state, root, session_id) = restored_claude(dispatch, enabled);
    let (result, launch) = capture_claude_launch(&state, &session_id, dispatch);
    let launch = launch.unwrap_or_else(|| panic!("expected Claude launch, got {result:?}"));
    assert_claude_launch(launch, &session_id, &root, enabled);
}

#[test]
fn restarted_claude_human_send_installs_engram() {
    assert_restored_claude_launch(Dispatch::HumanSend, true);
}

#[test]
fn restarted_claude_human_resume_installs_engram() {
    assert_restored_claude_launch(Dispatch::HumanResume, true);
}

#[test]
fn restarted_claude_ordinary_send_installs_engram() {
    assert_restored_claude_launch(Dispatch::OrdinarySend, true);
}

#[test]
fn restarted_claude_human_send_keeps_disabled_engram_absent() {
    assert_restored_claude_launch(Dispatch::HumanSend, false);
}

#[test]
fn restarted_claude_human_send_allows_an_undeclared_project() {
    let (state, root, session_id) = restored_claude(Dispatch::HumanSend, true);
    fs::remove_file(root.join(".engram-project")).unwrap();
    let (result, launch) = capture_claude_launch(&state, &session_id, Dispatch::HumanSend);
    let launch = launch.unwrap_or_else(|| panic!("expected Claude launch, got {result:?}"));
    assert_claude_launch(launch, &session_id, &root, false);
}

#[test]
fn restarted_claude_retains_human_prompt_until_project_access_returns() {
    let (state, root, session_id) = restored_claude(Dispatch::HumanSend, true);
    let offline = root.with_file_name("offline-project");
    fs::rename(&root, &offline).unwrap();

    let (result, launch) = capture_claude_launch(&state, &session_id, Dispatch::HumanSend);
    assert!(result.is_ok(), "Send should retain its prompt: {result:?}");
    assert!(launch.is_none());
    let pending_id = {
        let inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session_id).unwrap();
        let record = &inner.sessions[index];
        assert_eq!(record.session.status, SessionStatus::Error);
        let preview = &record.session.preview;
        assert!(preview.contains("Engram eligibility is unavailable"));
        let project = inner
            .find_project(record.session.project_id.as_deref().unwrap())
            .unwrap();
        let declaration = FsPath::new(&project.root_path).join(".engram-project");
        assert!(preview.contains(declaration.to_str().unwrap()));
        assert!(preview.contains("Restore access") && preview.contains("Resume"));
        assert_eq!(record.queued_prompts.len(), 1);
        let pending = &record.queued_prompts[0].pending_prompt;
        assert_eq!(pending.text, "Continue the saved session");
        pending.id.clone()
    };

    let (result, launch) = capture_claude_launch(&state, &session_id, Dispatch::HumanResume);
    assert!(launch.is_none());
    let message = result.unwrap_err().message;
    assert!(message.contains("Engram eligibility is unavailable"));
    assert!(message.contains("Restore access") && message.contains("Resume"));
    {
        let inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session_id).unwrap();
        assert_eq!(inner.sessions[index].queued_prompts.len(), 1);
        assert_eq!(
            inner.sessions[index].queued_prompts[0].pending_prompt.id,
            pending_id
        );
    }

    fs::rename(&offline, &root).unwrap();
    let (result, launch) = capture_claude_launch(&state, &session_id, Dispatch::HumanResume);
    let launch = launch.unwrap_or_else(|| panic!("expected Claude to retry, got {result:?}"));
    assert_claude_launch(launch, &session_id, &root, true);
}

fn assert_queued_claude_start_failure(state: &AppState, session_id: &str, reason: &str) {
    let inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(session_id).unwrap();
    let record = &inner.sessions[index];
    assert_eq!(record.session.status, SessionStatus::Error);
    assert!(
        record.session.preview.contains(reason),
        "{}",
        record.session.preview
    );
    assert!(record.session.preview.contains("Resume"));
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(
        record.queued_prompts[0].pending_prompt.text,
        "Continue the saved session"
    );
}

#[test]
fn restarted_claude_refuses_stale_eligibility_after_project_reset() {
    for was_enabled in [false, true] {
        let (state, root, session_id) = restored_claude(Dispatch::HumanSend, was_enabled);
        let changing_state = state.clone();
        let changing_session = session_id.clone();
        TEST_HUMAN_BEFORE_PROMOTION.with(|slot| {
            slot.replace(Some(Box::new(move || {
                let mut inner = changing_state.inner.lock().unwrap();
                let index = inner.find_session_index(&changing_session).unwrap();
                let project_id = inner.sessions[index].session.project_id.clone().unwrap();
                let generation = inner.claim_engram_project_reset(&project_id).unwrap();
                inner
                    .projects
                    .iter_mut()
                    .find(|project| project.id == project_id)
                    .unwrap()
                    .engram
                    .as_mut()
                    .unwrap()
                    .enabled = true;
                assert!(inner.engram_project_resets.release(&project_id, generation));
            })));
        });

        let (result, launch) = capture_claude_launch(&state, &session_id, Dispatch::HumanSend);
        assert!(result.is_ok());
        assert!(
            launch.is_none(),
            "Claude must not launch on a stale Engram check"
        );
        assert_queued_claude_start_failure(
            &state,
            &session_id,
            "Engram project configuration changed",
        );

        let (result, launch) = capture_claude_launch(&state, &session_id, Dispatch::HumanResume);
        let launch = launch.unwrap_or_else(|| panic!("expected Claude to retry, got {result:?}"));
        assert_claude_launch(launch, &session_id, &root, true);
    }
}

#[test]
fn restarted_claude_retains_prompt_when_declaration_read_fails_after_metadata() {
    let (state, root, session_id) = restored_claude(Dispatch::HumanSend, true);
    let declaration = root.join(".engram-project");
    let saved_declaration = root.join("saved-engram-project");
    let saved_for_hook = saved_declaration.clone();
    TEST_CLAUDE_BEFORE_DECLARATION_READ.with(|slot| {
        slot.replace(Some(Box::new(move |path| {
            assert!(fs::metadata(path).unwrap().is_file());
            fs::rename(path, saved_for_hook).unwrap();
            fs::create_dir(path).unwrap();
        })));
    });

    let (result, launch) = capture_claude_launch(&state, &session_id, Dispatch::HumanSend);
    assert!(result.is_ok());
    assert!(
        launch.is_none(),
        "Claude must not launch if the declaration cannot be read"
    );
    assert_queued_claude_start_failure(&state, &session_id, "TermAl could not inspect");

    fs::remove_dir(&declaration).unwrap();
    fs::rename(saved_declaration, declaration).unwrap();
    let (result, launch) = capture_claude_launch(&state, &session_id, Dispatch::HumanResume);
    let launch = launch.unwrap_or_else(|| panic!("expected Claude to retry, got {result:?}"));
    assert_claude_launch(launch, &session_id, &root, true);
}

#[test]
fn restarted_claude_automatic_and_activation_human_heads_bypass_strict_launch_check() {
    for dispatch in [Dispatch::Automatic, Dispatch::Activation] {
        let (state, root, session_id) = restored_claude(dispatch, true);
        remove_declaration_on_strict_read();
        let (result, launch) = capture_claude_launch(&state, &session_id, dispatch);
        TEST_CLAUDE_BEFORE_DECLARATION_READ.with(|slot| slot.borrow_mut().take());

        let launch =
            launch.unwrap_or_else(|| panic!("expected automatic Claude launch, got {result:?}"));
        assert_claude_resume(&launch);
        assert!(root.join(".engram-project").is_file());
    }
}

fn remove_declaration_on_strict_read() {
    TEST_CLAUDE_BEFORE_DECLARATION_READ.with(|slot| {
        slot.replace(Some(Box::new(|path| fs::remove_file(path).unwrap())));
    });
}

fn attach_warm_claude(state: &AppState, session_id: &str) -> mpsc::Receiver<ClaudeRuntimeCommand> {
    let (runtime, input) = test_claude_runtime_handle("warm-claude");
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(session_id).unwrap();
    inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    input
}

#[test]
fn warm_claude_human_actions_enqueue_without_reading_the_declaration() {
    for dispatch in [
        Dispatch::HumanSend,
        Dispatch::HumanResume,
        Dispatch::HumanSendWithOlderHead,
        Dispatch::SelectedOrdinaryResume,
        Dispatch::UnselectedOrdinaryResume,
    ] {
        let (state, root, session_id) = restored_claude(dispatch, true);
        let input = attach_warm_claude(&state, &session_id);
        remove_declaration_on_strict_read();

        let (result, launch) = capture_claude_launch(&state, &session_id, dispatch);
        TEST_CLAUDE_BEFORE_DECLARATION_READ.with(|slot| slot.borrow_mut().take());

        assert!(result.is_ok(), "{result:?}");
        assert!(launch.is_none(), "warm Claude should be reused");
        let ClaudeRuntimeCommand::Prompt(prompt) = input.try_recv().unwrap() else {
            panic!("expected the queued prompt on the warm Claude runtime");
        };
        assert!(prompt.text.contains("Continue the saved session"));
        assert!(root.join(".engram-project").is_file());
    }
}

#[test]
fn late_claude_runtime_reset_checks_declaration_before_launch() {
    let (state, root, session_id) = restored_claude(Dispatch::HumanSend, true);
    let warm_input = attach_warm_claude(&state, &session_id);
    let resetting_state = state.clone();
    let resetting_session = session_id.clone();
    TEST_HUMAN_BEFORE_PROMOTION.with(|slot| {
        slot.replace(Some(Box::new(move || {
            let mut inner = resetting_state.inner.lock().unwrap();
            let index = inner.find_session_index(&resetting_session).unwrap();
            let record = &mut inner.sessions[index];
            record.runtime = SessionRuntime::None;
            record.runtime_reset_required = true;
            remove_declaration_on_strict_read();
        })));
    });

    let (result, launch) = capture_claude_launch(&state, &session_id, Dispatch::HumanSend);
    TEST_CLAUDE_BEFORE_DECLARATION_READ.with(|slot| slot.borrow_mut().take());
    assert!(result.is_ok(), "{result:?}");
    assert!(
        launch.is_none(),
        "the fresh runtime needs current Engram evidence"
    );
    assert!(warm_input.try_recv().is_err());
    assert!(!root.join(".engram-project").exists());
    assert_queued_claude_start_failure(&state, &session_id, "TermAl could not inspect");
}

#[cfg(windows)]
#[test]
fn late_effort_change_cleans_up_the_attached_claude_job_before_launch() {
    for dispatch in [
        Dispatch::HumanSend,
        Dispatch::HumanSendWithOlderHead,
        Dispatch::SelectedOrdinaryResume,
    ] {
        let (state, root, session_id) = restored_claude(dispatch, true);
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "tests::engram_claude_resume::attached_claude_job_fixture",
                "--ignored",
            ])
            .current_dir(root.join("workdir"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = spawn_dedicated_runtime(&mut command, "Claude").unwrap();
        child
            .launch_guard
            .resume(&child.process, &child.tree)
            .unwrap();
        let (input_tx, input) = mpsc::channel();
        let token = RuntimeToken::Claude("attached-claude-job".to_owned());
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session_id).unwrap();
            let record = &mut inner.sessions[index];
            record.session.claude_effort = Some(ClaudeEffortLevel::Default);
            record.runtime = SessionRuntime::Claude(ClaudeRuntimeHandle {
                runtime_id: "attached-claude-job".to_owned(),
                input_tx,
                process: child.process.clone(),
                process_tree: Some(child.tree.clone()),
            });
        }
        assert!(child.process.try_wait().unwrap().is_none());
        let changing_state = state.clone();
        let changing_session = session_id.clone();
        TEST_CLAUDE_BEFORE_ENGRAM_LAUNCH_CHECK.with(|slot| {
            slot.replace(Some(Box::new(move || {
                changing_state
                    .update_session_settings(
                        &changing_session,
                        serde_json::from_value(json!({"claudeEffort": "high"})).unwrap(),
                    )
                    .unwrap();
                let inner = changing_state.inner.lock().unwrap();
                let index = inner.find_session_index(&changing_session).unwrap();
                let record = &inner.sessions[index];
                assert!(record.runtime_reset_required);
                assert!(record.runtime.matches_runtime_token(&token));
            })));
        });

        let (result, launch) = capture_claude_launch(&state, &session_id, dispatch);
        TEST_CLAUDE_BEFORE_ENGRAM_LAUNCH_CHECK.with(|slot| slot.borrow_mut().take());
        let launch =
            launch.unwrap_or_else(|| panic!("expected replacement Claude launch, got {result:?}"));
        assert!(child.tree.is_confirmed());
        assert!(child.process.try_wait().unwrap().is_some());
        assert!(input.try_recv().is_err());
        assert!(
            launch
                .args
                .windows(2)
                .any(|pair| pair == ["--effort", "high"]),
            "{:?}",
            launch.args
        );
        assert_claude_launch(launch, &session_id, &root, true);
    }
}

#[cfg(windows)]
#[test]
#[ignore = "run as a subprocess held open by the attached Claude job test"]
fn attached_claude_job_fixture() {
    use std::io::Read;
    let _ = std::io::stdin().read_exact(&mut [0]);
}

#[test]
fn explicit_claude_actions_keep_an_unreadable_ordinary_fifo_head_after_reload() {
    for dispatch in [
        Dispatch::HumanSendWithOlderHead,
        Dispatch::SelectedOrdinaryResume,
    ] {
        let (state, root, session_id) = restored_claude(dispatch, true);
        let declaration = root.join(".engram-project");
        let contents = fs::read(&declaration).unwrap();
        remove_declaration_on_strict_read();

        let (result, launch) = capture_claude_launch(&state, &session_id, dispatch);
        TEST_CLAUDE_BEFORE_DECLARATION_READ.with(|slot| slot.borrow_mut().take());
        assert!(
            launch.is_none(),
            "Claude must not launch for an unreadable declaration"
        );
        assert!(
            !declaration.exists(),
            "the strict launch check should read the declaration"
        );
        let reason = {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session_id).unwrap()];
            let head = record.queued_prompts.front().unwrap();
            assert!(ordinary_known_unpromoted_head(record, head));
            assert_eq!(head.pending_prompt.id, "saved-human-prompt");
            assert_eq!(head.pending_prompt.text, "Continue the saved session");
            assert!(matches!(record.runtime, SessionRuntime::None));
            if matches!(dispatch, Dispatch::HumanSendWithOlderHead) {
                assert!(result.is_ok(), "{result:?}");
                assert_eq!(record.queued_prompts.len(), 2);
                assert_eq!(
                    record.queued_prompts[1].pending_prompt.text,
                    "New Human prompt behind FIFO"
                );
                record.session.preview.clone()
            } else {
                assert_eq!(record.queued_prompts.len(), 1);
                assert!(record.orchestrator_auto_dispatch_blocked);
                result.unwrap_err().message
            }
        };
        assert!(
            reason.contains("TermAl could not inspect") && reason.contains("Resume"),
            "{reason}"
        );

        fs::write(declaration, contents).unwrap();
        let (result, launch) =
            capture_claude_launch(&state, &session_id, Dispatch::SelectedOrdinaryResume);
        let launch = launch
            .unwrap_or_else(|| panic!("expected preserved FIFO head to retry, got {result:?}"));
        assert_claude_launch(launch, &session_id, &root, true);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn unselected_claude_resume_keeps_fifo_held_behind_a_historical_row() {
    let (state, root, session_id) = restored_claude(Dispatch::UnselectedOrdinaryResume, true);
    let declaration = root.join(".engram-project");
    let contents = fs::read(&declaration).unwrap();
    fs::remove_file(&declaration).unwrap();
    std::os::unix::fs::symlink("missing-declaration", &declaration).unwrap();

    let (send, launch) =
        capture_claude_launch(&state, &session_id, Dispatch::HumanSendWithOlderHead);
    assert!(send.is_ok(), "{send:?}");
    assert!(launch.is_none());
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session_id).unwrap()];
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(record.session.preview.contains("TermAl could not inspect"));
        assert!(record.session.preview.contains("Resume"));
        let projected = projected_pending_prompts(record);
        assert_eq!(projected[0].id, "historical-user-prompt");
        assert!(projected[0].is_engram_retained);
        assert_eq!(ordinary_queue_position(record), Some(1));
        assert_eq!(projected[1].id, "saved-human-prompt");
        assert_eq!(projected[2].text, "New Human prompt behind FIFO");
    }

    // Use the same empty-body POST the UI sends when the leading row is historical.
    let mut http_state = state.clone();
    http_state.agent_runtime_spawning_enabled = false;
    let response = app_router(http_state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{session_id}/queue/resume"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(!response.status().is_success());
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let message = String::from_utf8_lossy(&body);
    assert!(
        message.contains("TermAl could not inspect") && message.contains("Resume"),
        "{message}"
    );
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session_id).unwrap()];
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(matches!(record.runtime, SessionRuntime::None));
        assert_eq!(record.queued_prompts.len(), 3);
        assert_eq!(
            record.queued_prompts[0].pending_prompt.id,
            "historical-user-prompt"
        );
        assert!(record.queued_prompts[0].engram_ordinary_handoff);
        assert_eq!(ordinary_queue_position(record), Some(1));
        let head = &record.queued_prompts[1];
        assert_eq!(head.pending_prompt.id, "saved-human-prompt");
        assert!(ordinary_known_unpromoted_head(record, head));
    }

    fs::remove_file(&declaration).unwrap();
    fs::write(declaration, contents).unwrap();
    let (result, launch) =
        capture_claude_launch(&state, &session_id, Dispatch::UnselectedOrdinaryResume);
    let launch =
        launch.unwrap_or_else(|| panic!("expected the retained head to retry, got {result:?}"));
    assert_claude_launch(launch, &session_id, &root, true);
}

#[test]
fn restarted_claude_local_declaration_does_not_reset_the_codex_sibling() {
    for dispatch in [
        Dispatch::HumanSend,
        Dispatch::HumanSendWithOlderHead,
        Dispatch::SelectedOrdinaryResume,
    ] {
        let (state, root, session_id) = restored_claude(dispatch, true);
        let project_id = {
            let inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session_id).unwrap();
            inner.sessions[index].session.project_id.clone().unwrap()
        };
        let codex_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
        let (codex_runtime, _input) = test_codex_runtime_handle("sibling-codex");
        let generation = {
            let mut inner = state.inner.lock().unwrap();
            inner.engram_declared_project_ids.insert(project_id.clone());
            inner
                .engram_declaration_checked_project_ids
                .insert(project_id.clone());
            let codex_index = inner.find_session_index(&codex_id).unwrap();
            inner.sessions[codex_index].runtime = SessionRuntime::Codex(codex_runtime);
            let index = inner.find_session_index(&session_id).unwrap();
            let cache = &mut inner.sessions[index].engram;
            cache.mark_context_refresh_needed(None);
            cache.context_nudge_generation
        };
        fs::remove_file(root.join(".engram-project")).unwrap();

        let (result, launch) = capture_claude_launch(&state, &session_id, dispatch);
        let launch = launch.unwrap_or_else(|| panic!("expected Claude launch, got {result:?}"));
        assert_claude_launch(launch, &session_id, &root, false);
        let inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&codex_id).unwrap();
        assert!(!inner.sessions[index].runtime_reset_required);
        assert!(
            inner.sessions[index]
                .runtime
                .matches_runtime_token(&RuntimeToken::Codex("sibling-codex".to_owned()))
        );
        assert!(inner.engram_declared_project_ids.contains(&project_id));
        assert!(
            inner
                .engram_declaration_checked_project_ids
                .contains(&project_id)
        );
        let index = inner.find_session_index(&session_id).unwrap();
        let cache = &inner.sessions[index].engram;
        // A reservation advances this generation synchronously, before the worker runs.
        assert_eq!(cache.context_nudge_generation, generation);
        assert!(cache.context_needs_refresh());
    }
}
