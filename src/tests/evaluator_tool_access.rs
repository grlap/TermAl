// Owns the tests that an acceptance-evaluator child reaches no tracker MCP
// server: the host installs none for it, a Claude evaluator starts on the
// host's MCP configuration alone, a Codex evaluator gets no server seeded
// from the user's configuration, the evaluator gets no orientation read, and
// a Claude evaluator's read-only gate refuses a tracker call. Does not own
// the Codex child's shell sandbox, nor the evaluator's brief or its
// submission (acceptance_evaluation.rs) nor the tracker MCP descriptor of
// ordinary sessions (engram_host_adapter.rs). New file.
use super::delegation_support::test_app_state_with_delegation_codex_runtime;
use super::*;

const PROJECT_DECLARATION: &str = "evaluator-tool-access";

/// A local project with the tracker enabled and declared, as the operator's
/// enablement leaves it.
pub(super) fn tracker_project(state: &AppState, name: &str) -> (String, PathBuf) {
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join(name);
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(state, &root, name);
    fs::write(
        root.join(".engram-project"),
        format!("{PROJECT_DECLARATION}\n"),
    )
    .expect("the project should be declared");
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    inner
        .projects
        .iter_mut()
        .find(|project| project.id == project_id)
        .expect("project should exist")
        .engram = Some(EngramProjectSettings {
        acceptance_evaluation: None,
        enabled: true,
        turn_gated_control: true,
        binary_path: Some("C:/tools/engram.exe".to_owned()),
        home: Some("C:/engram-home".to_owned()),
        work_authority_grant: None,
        authority_store_key: None,
        deadline_ms: Some(250),
    });
    inner.engram_declared_project_ids.insert(project_id.clone());
    inner
        .engram_declaration_checked_project_ids
        .insert(project_id.clone());
    state
        .commit_locked(&mut inner)
        .expect("tracker settings should persist");
    drop(inner);
    (project_id, root)
}

/// A running read-only delegation child of `parent` in `mode`, configured as
/// delegation creation configures it.
pub(super) fn delegation_child(
    state: &AppState,
    parent: &str,
    project_id: &str,
    root: &FsPath,
    agent: Agent,
    mode: DelegationMode,
) -> String {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let delegation_id = inner.next_delegation_id();
    let child = inner.create_session(
        agent,
        Some("Delegated child".to_owned()),
        root.to_string_lossy().into_owned(),
        Some(project_id.to_owned()),
        None,
    );
    let child_session_id = child.session.id.clone();
    let index = inner.find_session_index(&child_session_id).unwrap();
    inner.sessions[index].session.parent_delegation_id = Some(delegation_id.clone());
    configure_delegation_child_prompt_settings(
        &mut inner.sessions[index],
        mode,
        &DelegationWritePolicy::ReadOnly,
    );
    inner.delegations.push(DelegationRecord {
        id: delegation_id,
        parent_session_id: parent.to_owned(),
        child_session_id: child_session_id.clone(),
        mode,
        status: DelegationStatus::Running,
        title: "Delegated child".to_owned(),
        prompt: "Judge the task.".to_owned(),
        cwd: root.to_string_lossy().into_owned(),
        agent,
        model: None,
        write_policy: DelegationWritePolicy::ReadOnly,
        created_at: stamp_now(),
        started_at: Some(stamp_now()),
        completed_at: None,
        result: None,
        submitted_review_result: None,
        post_submission_transport_error: None,
        review_result_recovery_probe_attempt: None,
        review_result_recovery_error: None,
        review_result_schema_version: None,
        queued_followup_prompt_id: None,
        review_result_submission_attempt: 0,
        acceptance_evaluation: None,
        attempt: DelegationAttemptState::default(),
    });
    state.commit_locked(&mut inner).unwrap();
    child_session_id
}

fn server_names(servers: &Value) -> Vec<String> {
    let mut names = servers
        .as_object()
        .expect("MCP servers should be an object")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    names.sort();
    names
}

// The host installs the tracker's server for every session of an enabled
// project, delegation children included, except an acceptance evaluator.
#[test]
fn an_acceptance_evaluator_child_gets_no_tracker_server_from_the_host() {
    let (state, _runtime_rx) =
        test_app_state_with_delegation_codex_runtime("evaluator-tool-access");
    let (project_id, root) = tracker_project(&state, "evaluator-tool-access-project");
    let parent = create_test_project_session(&state, Agent::Claude, &project_id, &root);
    let reviewer = delegation_child(
        &state,
        &parent,
        &project_id,
        &root,
        Agent::Claude,
        DelegationMode::Reviewer,
    );
    for agent in [Agent::Claude, Agent::Codex] {
        let evaluator = delegation_child(
            &state,
            &parent,
            &project_id,
            &root,
            agent,
            DelegationMode::Evaluator,
        );
        {
            let inner = state.inner.lock().unwrap();
            assert!(session_is_acceptance_evaluator_child_locked(
                &inner, &evaluator
            ));
            assert!(
                engram_mcp_runtime_config_for_session_locked(&inner, &evaluator).is_none(),
                "{agent:?}"
            );
            // Other sessions of the same project keep the tracker's server.
            assert!(engram_mcp_runtime_config_for_session_locked(&inner, &parent).is_some());
            assert!(engram_mcp_runtime_config_for_session_locked(&inner, &reviewer).is_some());
            assert!(!session_is_acceptance_evaluator_child_locked(
                &inner, &reviewer
            ));
            assert!(!session_is_acceptance_evaluator_child_locked(
                &inner, &parent
            ));
        }
        match agent {
            Agent::Claude => {
                let claude: Value = serde_json::from_str(
                    &state
                        .termal_delegation_mcp_claude_config_json(&evaluator)
                        .expect("Claude MCP config should compose"),
                )
                .unwrap();
                assert_eq!(
                    server_names(&claude["mcpServers"]),
                    [TERMAL_DELEGATION_MCP_SERVER_NAME]
                );
            }
            _ => {
                let codex = state
                    .termal_delegation_mcp_codex_config(&evaluator)
                    .expect("Codex MCP config should compose");
                assert_eq!(
                    server_names(&codex["mcp_servers"]),
                    [TERMAL_DELEGATION_MCP_SERVER_NAME]
                );
                // No tracker identity reaches its shells either.
                let shell = codex["shell_environment_policy"].to_string();
                assert!(!shell.contains(ENGRAM_SESSION_ID_ENV), "{shell}");
            }
        }
    }
    // The reviewer's configuration still carries the tracker's server.
    let claude: Value = serde_json::from_str(
        &state
            .termal_delegation_mcp_claude_config_json(&reviewer)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        server_names(&claude["mcpServers"]),
        [ENGRAM_MCP_SERVER_NAME, TERMAL_DELEGATION_MCP_SERVER_NAME]
    );
}

// A Claude evaluator starts on the host-written MCP configuration alone, so
// a tracker server named in the user's, project's or local Claude settings
// never loads; every other Claude session keeps those servers.
#[test]
fn a_claude_evaluator_child_starts_on_the_host_mcp_configuration_alone() {
    let (state, _runtime_rx) = test_app_state_with_delegation_codex_runtime("evaluator-strict-mcp");
    let (project_id, root) = tracker_project(&state, "evaluator-strict-mcp-project");
    let parent = create_test_project_session(&state, Agent::Claude, &project_id, &root);
    let evaluator = delegation_child(
        &state,
        &parent,
        &project_id,
        &root,
        Agent::Claude,
        DelegationMode::Evaluator,
    );
    let reviewer = delegation_child(
        &state,
        &parent,
        &project_id,
        &root,
        Agent::Claude,
        DelegationMode::Reviewer,
    );
    let config = root.join("claude-mcp.json");
    let launch = |session_id: &str| {
        let inner = state.inner.lock().unwrap();
        claude_cli_mcp_config_args(
            &config,
            session_is_acceptance_evaluator_child_locked(&inner, session_id),
        )
    };
    assert_eq!(
        launch(&evaluator),
        [
            std::ffi::OsString::from("--strict-mcp-config"),
            "--mcp-config".into(),
            config.clone().into_os_string(),
        ]
    );
    for other in [&parent, &reviewer] {
        assert_eq!(
            launch(other),
            [
                std::ffi::OsString::from("--mcp-config"),
                config.clone().into_os_string(),
            ]
        );
    }
}

// An evaluator gets no orientation read of its own: preparing its turn reads
// nothing from the store, while a reviewer of the same project still reads.
#[test]
fn an_acceptance_evaluator_child_gets_no_orientation_read() {
    let (state, _runtime_rx) =
        test_app_state_with_delegation_codex_runtime("evaluator-no-orientation");
    let (project_id, root) = tracker_project(&state, "evaluator-no-orientation-project");
    {
        // A binary that cannot start makes an attempted read fail fast.
        let mut inner = state.inner.lock().unwrap();
        let project = inner
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
            .unwrap();
        project.engram.as_mut().unwrap().binary_path = Some(
            root.join("missing-engram.exe")
                .to_string_lossy()
                .into_owned(),
        );
    }
    let parent = create_test_project_session(&state, Agent::Claude, &project_id, &root);
    let children = [DelegationMode::Evaluator, DelegationMode::Reviewer].map(|mode| {
        let child = delegation_child(&state, &parent, &project_id, &root, Agent::Claude, mode);
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&child).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.engram.context_nudge_pending = true;
        record.engram.context_nudge_generation = 0;
        child
    });
    let generation = |session_id: &str| {
        let inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(session_id).unwrap();
        inner.sessions[index].engram.context_nudge_generation
    };

    let [evaluator, reviewer] = children;
    assert_eq!(
        state.prepare_engram_context_nudge_off_lock(&evaluator),
        EngramContextNudgePreparation::NotApplicable
    );
    assert_eq!(
        generation(&evaluator),
        0,
        "the evaluator's read never began"
    );

    assert_ne!(
        state.prepare_engram_context_nudge_off_lock(&reviewer),
        EngramContextNudgePreparation::NotApplicable
    );
    assert!(generation(&reviewer) > 0, "the reviewer's read began");
}

// A Codex child puts no tool call to the host, so a server the user's own
// configuration names, under any name, would reach the tracker. A Codex
// evaluator gets the TermAl-owned servers alone; its shell policy stays.
#[test]
fn a_codex_evaluator_child_gets_no_server_seeded_from_the_users_configuration() {
    let (state, _runtime_rx) =
        test_app_state_with_delegation_codex_runtime("evaluator-seeded-servers");
    let (project_id, root) = tracker_project(&state, "evaluator-seeded-servers-project");
    let parent = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    let evaluator = delegation_child(
        &state,
        &parent,
        &project_id,
        &root,
        Agent::Codex,
        DelegationMode::Evaluator,
    );
    let reviewer = delegation_child(
        &state,
        &parent,
        &project_id,
        &root,
        Agent::Codex,
        DelegationMode::Reviewer,
    );
    let codex_home = root.join("codex-home");
    fs::create_dir_all(&codex_home).unwrap();
    fs::write(
        codex_home.join("config.toml"),
        "[mcp_servers.engram]\ncommand = \"engram\"\nargs = [\"mcp\"]\n\n\
         [mcp_servers.tracker-by-another-name]\ncommand = \"engram\"\n\n\
         [shell_environment_policy]\ninherit = \"all\"\n",
    )
    .unwrap();

    // Even a tracker descriptor handed in by a caller is left out.
    let tracker_descriptor = state
        .engram_mcp_stdio_config_for_session(&reviewer)
        .expect("the reviewer has the tracker's descriptor");
    let evaluator_config = state
        .termal_delegation_mcp_codex_config_with_engram(
            &evaluator,
            Some(tracker_descriptor),
            Some(&codex_home),
        )
        .expect("the evaluator's config should compose");
    assert_eq!(
        server_names(&evaluator_config["mcp_servers"]),
        [TERMAL_DELEGATION_MCP_SERVER_NAME]
    );
    assert_eq!(
        evaluator_config["shell_environment_policy"]["inherit"],
        "all"
    );
    let shell = evaluator_config["shell_environment_policy"].to_string();
    assert!(!shell.contains(ENGRAM_SESSION_ID_ENV), "{shell}");

    // Any other session keeps the user's servers, with TermAl's owned names
    // laid over them.
    let reviewer_config = state
        .termal_delegation_mcp_codex_config_with_engram(&reviewer, None, Some(&codex_home))
        .unwrap();
    assert_eq!(
        server_names(&reviewer_config["mcp_servers"]),
        [
            ENGRAM_MCP_SERVER_NAME,
            TERMAL_DELEGATION_MCP_SERVER_NAME,
            "tracker-by-another-name"
        ]
    );
}

// A Claude evaluator runs under the read-only gate, which refuses a guessed
// tracker write whatever the child's configuration offers.
#[test]
fn a_claude_evaluator_childs_guessed_tracker_write_is_refused() {
    let (state, _runtime_rx) =
        test_app_state_with_delegation_codex_runtime("evaluator-guessed-write");
    let (project_id, root) = tracker_project(&state, "evaluator-guessed-write-project");
    let parent = create_test_project_session(&state, Agent::Claude, &project_id, &root);
    let evaluator = delegation_child(
        &state,
        &parent,
        &project_id,
        &root,
        Agent::Claude,
        DelegationMode::Evaluator,
    );
    let (approval_mode, delegation_child) = state
        .claude_control_request_context(&evaluator)
        .expect("the evaluator's control context should resolve");
    assert_eq!(approval_mode, ClaudeApprovalMode::ReadOnlyAutoApprove);
    for (index, (tool, input)) in [
        (
            "mcp__engram__note",
            json!({"work_ref":"w-other", "text":"x"}),
        ),
        ("mcp__engram__evaluate", json!({"work_ref":"w-other"})),
        ("mcp__engram__claim", json!({"work_ref":"w-other"})),
        ("mcp__engram__memories", json!({})),
        // With no tracker tool, the next guess is the tracker's CLI in the
        // shell, with a home of its own choosing; the checked Bash refuses it.
        ("Bash", json!({"command":"engram work note w-other \"x\""})),
        (
            "Bash",
            json!({"command":"engram --home C:/engram-home work evaluate w-other"}),
        ),
        (
            "Bash",
            json!({"command":"git status --short && engram work claim w-other"}),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let message = json!({"type":"control_request", "request_id":format!("evaluator-{index}"),
            "request":{"subtype":"can_use_tool", "tool_name":tool, "input":input}});
        let action = classify_claude_control_request(
            &message,
            &mut ClaudeTurnState::default(),
            approval_mode,
            delegation_child,
            ".",
            state.claude_control_plane_request_allowed(&evaluator, &message),
        )
        .unwrap()
        .expect("the request reaches the host's gate");
        assert!(
            matches!(
                action,
                ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Deny { .. })
            ),
            "{tool}"
        );
    }
}
