//! Ordinary Engram host-adapter safety and work-service conformance.

#[path = "engram_session_reconciliation.rs"]
mod session_reconciliation;

#[path = "engram_project_containment.rs"]
mod project_containment;

#[path = "engram_root_dispatch.rs"]
mod root_dispatch;

#[path = "engram_root_recovery_live.rs"]
mod root_recovery_live;

#[path = "engram_control_transport.rs"]
mod control_transport;

#[path = "engram_control_startup_codes.rs"]
mod control_startup_codes;

#[path = "engram_fixture_contracts.rs"]
mod fixture_contracts;

#[path = "engram_boot_recovery_targets.rs"]
mod boot_recovery_targets;

#[path = "engram_codex_profile_exit.rs"]
mod codex_profile_exit;

#[path = "engram_uncertain_begin.rs"]
mod uncertain_begin;

#[path = "engram_turn_observations.rs"]
mod turn_observations;

#[path = "engram_turn_checks.rs"]
mod turn_checks;

use self::turn_checks::{
    EOCheckedTurn, SIZE_TEST, assert_ordinary_observation_has_no_native_credit,
    assert_ordinary_turn_has_no_native_requests,
};

#[path = "engram_held_claims.rs"]
mod held_claims;

#[path = "engram_unqueued_bind_budget.rs"]
mod unqueued_bind_budget;

// Supplemental protocol coverage stays mounted while its owner ports it.
#[path = "engram_bind_retention.rs"]
mod bind_retention;

use self::control_transport::{
    assert_engram_control_descendant_was_terminated, prepare_engram_control_process_tree_fixture,
};
#[cfg(windows)]
use self::control_transport::{
    assert_engram_fixture_teardown_covers_its_parent, engram_fixture_descendant_parent,
};
use super::delegation_support::test_app_state_with_delegation_codex_runtime;
use super::phase_sync::{DEADLOCK_GUARD, receive};
use super::*;

/// Creation has already delivered or rejected this prompt before returning.
/// A rejected create can return Ok with a Failed delegation; waiting on the
/// runtime channel then hides the actual refusal behind a receive timeout.
fn receive_synchronous_engram_prompt(
    state: &AppState,
    receiver: &mpsc::Receiver<CodexRuntimeCommand>,
    phase: &str,
) -> std::result::Result<CodexRuntimeCommand, mpsc::TryRecvError> {
    receiver.try_recv().inspect_err(|error| {
        let inner = state.inner.lock().expect("fixture state mutex poisoned");
        let sessions = inner.sessions.iter().map(|record| &record.session).collect::<Vec<_>>();
        eprintln!(
            "synchronous fixture phase `{phase}` has no command: {error:?}; state={}; control requests are logged at their fixture boundary",
            json!({ "delegations": inner.delegations, "sessions": sessions }),
        );
    })
}

fn install_control_only_transport(state: &AppState, control: Arc<dyn EngramControlTransport>) {
    // Explicitly model consumed advisory context for existing synthetic
    // sessions. Context-process tests supply a real fixture executable.
    // No SessionBind publication or removed dispatch budget is involved.
    {
        let mut inner = state.inner.lock().expect("fixture state mutex poisoned");
        let synthetic = inner
            .sessions
            .iter()
            .filter(|record| {
                engram_project_for_session_locked(&inner, &record.session.id)
                    .and_then(|project| project.engram.as_ref())
                    .and_then(|settings| settings.binary_path.as_deref())
                    .is_some_and(|path| {
                        FsPath::new(path)
                            .file_name()
                            .is_some_and(|name| name == "engram-fixture")
                    })
            })
            .map(|record| record.session.id.clone())
            .collect::<Vec<_>>();
        for session in synthetic {
            let record = inner.session_mut(&session).unwrap();
            assert!(!record.engram.context_nudge_in_progress);
            record.engram.context_nudge_pending = false;
        }
    }
    state.install_test_engram_transport(control);
}

impl AppState {
    /// Synthetic projects use the non-executable `engram-fixture` sentinel and
    /// model already-consumed context. Tests exercising context subprocesses
    /// opt in with an actual fixture executable; their behavior is untouched.
    pub(super) fn install_control_test_transport(&self, control: Arc<dyn EngramControlTransport>) {
        install_control_only_transport(self, control);
    }
}

fn persisted_session_json(state: &AppState, session_id: &str) -> String {
    let connection = rusqlite::Connection::open(state.persistence_path.as_path())
        .expect("persisted state should open");
    connection
        .query_row(
            "SELECT value_json FROM sessions WHERE id = ?1",
            rusqlite::params![session_id],
            |row| row.get(0),
        )
        .expect("persisted session row should exist")
}

#[test]
fn engram_context_nudge_truncation_preserves_utf8_boundaries() {
    let mut context = "a".repeat(ENGRAM_CONTEXT_NUDGE_MAX_BYTES - 1);
    context.push('🦀');
    assert!(truncate_engram_context_nudge(&mut context));
    assert_eq!(context.len(), ENGRAM_CONTEXT_NUDGE_MAX_BYTES - 1);
    assert!(context.is_char_boundary(context.len()));
}

#[test]
fn repository_declaration_changes_reset_existing_session_runtimes() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-declaration-runtime-reset-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram declaration reset");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    enable_test_project_engram(&state, &project_id, &root);
    state.refresh_engram_project_declaration_for_session_off_lock(&session_id);

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        inner.sessions[index].runtime_reset_required = false;
    }
    fs::remove_file(root.join(".engram-project")).expect("declaration should be removable");
    assert!(!state.refresh_engram_project_declaration_for_session_off_lock(&session_id));
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        assert!(inner.sessions[index].runtime_reset_required);
        assert!(engram_mcp_stdio_config_for_session_locked(&inner, &session_id).is_none());
    }

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        inner.sessions[index].runtime_reset_required = false;
    }
    fs::write(root.join(".engram-project"), "engram-project\n")
        .expect("declaration should be restorable");
    assert!(state.refresh_engram_project_declaration_for_session_off_lock(&session_id));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    assert!(inner.sessions[index].runtime_reset_required);
    assert!(engram_mcp_stdio_config_for_session_locked(&inner, &session_id).is_some());
}

#[test]
fn failed_engram_context_refresh_stays_pending_for_retry() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-context-refresh-failure-project");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "engram-project\n")
        .expect("repository declaration should exist");
    let project_id = create_test_project(&state, &root, "Engram context refresh failure");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let project = inner
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
            .expect("project should exist");
        project.engram = Some(EngramProjectSettings {
            acceptance_evaluation: None,
            enabled: true,
            execution_observability: false,
            binary_path: Some(root.join("missing-engram").to_string_lossy().into_owned()),
            home: Some(root.to_string_lossy().into_owned()),
            authority_store_key: None,
            deadline_ms: None,
        });
    }
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);

    assert!(matches!(
        state.prepare_engram_context_nudge_off_lock(&session_id),
        EngramContextNudgePreparation::Failed
    ));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    assert!(inner.sessions[index].engram.context_nudge_pending);
    assert!(!inner.sessions[index].engram.context_nudge_in_progress);
    assert!(inner.sessions[index].engram.pending_context_nudge.is_none());
    drop(inner);
}

#[test]
fn s0_without_project_engram_is_byte_stable_and_never_calls_transport() {
    let (state, runtime_rx) = test_app_state_with_delegation_codex_runtime("engram-s0");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-off-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram-off project");
    let parent_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);

    // An empty scripted transport is a panic-equivalent sentinel for S0:
    // any accidental adapter call is recorded, returns a protocol failure,
    // and would also surface an EngramControl degradation card below.
    let transport = ScriptedEngramControlTransport::new([]);
    state.install_control_test_transport(transport.clone());

    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Exercise the Engram-off invariant.".to_owned(),
                title: Some("Engram S0".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should start without Engram");
    assert!(matches!(
        receive_synchronous_engram_prompt(
            &state,
            &runtime_rx,
            "runtime should receive the ordinary prompt"
        )
        .expect("runtime should receive the ordinary prompt"),
        CodexRuntimeCommand::Prompt { .. }
    ));

    let child_id = created.delegation.child_session_id;
    let runtime_token = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let child = inner
            .sessions
            .iter()
            .find(|record| record.session.id == child_id)
            .expect("child should exist");
        child
            .runtime
            .runtime_token()
            .expect("child runtime should be active")
    };
    state
        .finish_turn_ok_if_runtime_matches(&child_id, &runtime_token)
        .expect("ordinary turn should finish");

    let disposable = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Exercise Engram-off Stop and kill.".to_owned(),
                title: Some("Engram S0 terminal paths".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("second delegation should also start without Engram");
    assert!(matches!(
        receive_synchronous_engram_prompt(
            &state,
            &runtime_rx,
            "runtime should receive the second ordinary prompt"
        )
        .expect("runtime should receive the second ordinary prompt"),
        CodexRuntimeCommand::Prompt { .. }
    ));
    state
        .stop_session(&disposable.delegation.child_session_id)
        .expect("Engram-off Stop should not touch the transport");
    state
        .kill_session(&disposable.delegation.child_session_id)
        .expect("Engram-off kill should not touch the transport");

    assert!(
        transport.requests().is_empty(),
        "an unconfigured project must never touch the Engram transport"
    );
    assert!(
        transport.shutdowns().is_empty(),
        "an unconfigured project must never reap an Engram sidecar"
    );
    let transcript_before_restart = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert!(inner.sessions.iter().all(|record| {
            record
                .session
                .messages
                .iter()
                .all(|message| !matches!(message, Message::EngramControl { .. }))
        }));
        let child = inner
            .sessions
            .iter()
            .find(|record| record.session.id == child_id)
            .expect("child should exist");
        serde_json::to_vec(&child.session.messages)
            .expect("transcript should serialize deterministically")
    };

    let persisted = persisted_session_json(&state, &child_id);
    assert!(!persisted.contains("engramRoutingToken"));
    assert!(!persisted.contains("engramOpenGrantId"));

    let restarted = AppState::new_with_paths(
        root.to_string_lossy().into_owned(),
        state.persistence_path.as_path().to_path_buf(),
        state.orchestrator_templates_path.as_path().to_path_buf(),
    )
    .expect("Engram-off state should restart without an Engram binary");
    let transcript_after_restart = {
        let inner = restarted.inner.lock().expect("state mutex poisoned");
        let child = inner
            .sessions
            .iter()
            .find(|record| record.session.id == child_id)
            .expect("child should reload");
        serde_json::to_vec(&child.session.messages)
            .expect("reloaded transcript should serialize deterministically")
    };
    restarted.shutdown_persist_blocking();

    assert_eq!(transcript_after_restart, transcript_before_restart);
}

#[test]
fn s0_projectless_delegation_is_silent_and_never_calls_transport() {
    let (state, runtime_rx) = test_app_state_with_delegation_codex_runtime("engram-s0-projectless");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-projectless-session");
    fs::create_dir_all(&root).expect("session workdir should exist");
    let parent_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let parent = inner.create_session(
            Agent::Codex,
            Some("Projectless parent".to_owned()),
            root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = parent.session.id.clone();
        state
            .commit_locked(&mut inner)
            .expect("projectless parent should persist");
        session_id
    };

    let transport = ScriptedEngramControlTransport::new([]);
    state.install_control_test_transport(transport.clone());
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Exercise projectless Engram-off dispatch.".to_owned(),
                title: Some("Engram S0 projectless".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("projectless delegation should start without Engram");
    assert!(matches!(
        receive_synchronous_engram_prompt(
            &state,
            &runtime_rx,
            "runtime should receive the ordinary prompt"
        )
        .expect("runtime should receive the ordinary prompt"),
        CodexRuntimeCommand::Prompt { .. }
    ));

    let child_id = created.delegation.child_session_id;
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let child = inner
            .sessions
            .iter()
            .find(|record| record.session.id == child_id)
            .expect("child should exist");
        assert!(
            child
                .session
                .messages
                .iter()
                .all(|message| !matches!(message, Message::EngramControl { .. }))
        );
    }
    assert!(transport.requests().is_empty());
    assert!(transport.shutdowns().is_empty());
    let persisted = persisted_session_json(&state, &child_id);
    assert!(!persisted.contains("engramRoutingToken"));
    assert!(!persisted.contains("engramOpenGrantId"));
}

#[test]
fn s0_explicitly_disabled_project_never_calls_or_persists_engram_state() {
    let (state, runtime_rx) = test_app_state_with_delegation_codex_runtime("engram-s0-disabled");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-disabled-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Explicitly disabled Engram");
    let parent_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
            .expect("project should exist")
            .engram = Some(EngramProjectSettings::default());
        state
            .commit_locked(&mut inner)
            .expect("disabled setting should persist");
    }
    let transport = ScriptedEngramControlTransport::new([]);
    state.install_control_test_transport(transport.clone());
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Exercise explicit disabled mode.".to_owned(),
                title: Some("Engram S0 disabled".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("disabled adapter must preserve ordinary delegation dispatch");
    assert!(matches!(
        receive_synchronous_engram_prompt(
            &state,
            &runtime_rx,
            "runtime should receive the ordinary prompt"
        )
        .expect("runtime should receive the ordinary prompt"),
        CodexRuntimeCommand::Prompt { .. }
    ));
    let child_id = created.delegation.child_session_id;
    state
        .stop_session(&child_id)
        .expect("ordinary turn should stop without touching Engram");
    assert!(transport.requests().is_empty());
    assert!(transport.shutdowns().is_empty());
    let persisted = persisted_session_json(&state, &child_id);
    assert!(!persisted.contains("engramRoutingToken"));
    assert!(!persisted.contains("engramOpenGrantId"));
    state
        .kill_session(&child_id)
        .expect("unconfigured child should be removable without touching Engram");
    assert!(transport.requests().is_empty());
    assert!(transport.shutdowns().is_empty());
}

#[test]
fn never_enabled_engram_project_with_a_delegation_child_can_be_deleted() {
    let (state, runtime_rx) =
        test_app_state_with_delegation_codex_runtime("engram-never-enabled-project-delete");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-never-enabled-project-delete-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Never-enabled Engram deletion");
    let parent_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
            .expect("project should exist")
            .engram = Some(EngramProjectSettings::default());
        state
            .commit_locked(&mut inner)
            .expect("disabled setting should persist");
    }
    let transport = ScriptedEngramControlTransport::new([]);
    state.install_control_test_transport(transport.clone());
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Keep an ordinary child attached during deletion.".to_owned(),
                title: Some("Never-enabled Engram deletion".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("disabled adapter must preserve ordinary delegation dispatch");
    assert!(matches!(
        receive_synchronous_engram_prompt(
            &state,
            &runtime_rx,
            "runtime should receive the ordinary prompt"
        )
        .expect("runtime should receive the ordinary prompt"),
        CodexRuntimeCommand::Prompt { .. }
    ));

    let runtime_tokens = {
        let inner = state.inner.lock().unwrap();
        [&parent_session_id, &created.delegation.child_session_id].map(|session_id| {
            let record = &inner.sessions[inner.find_session_index(session_id).unwrap()];
            assert!(!AppState::engram_session_is_enabled_locked(
                &inner, session_id
            ));
            assert!(
                AppState::engram_source_root_target_locked(&inner, session_id)
                    .unwrap()
                    .is_none()
            );
            assert!(engram_mcp_runtime_config_for_session_locked(&inner, session_id).is_none());
            assert!(record.engram_mcp_installed.is_none());
            record.runtime.runtime_token()
        })
    };
    assert!(
        runtime_tokens[1].is_some(),
        "the ordinary child has a provider runtime"
    );

    state
        .delete_project(&project_id)
        .expect("a never-enabled Engram project should delete without connection settings");

    assert!(transport.requests().is_empty());
    assert!(
        transport.shutdowns().is_empty(),
        "no never-enabled control process existed"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.find_project(&project_id).is_none());
    for (session_id, token) in [&parent_session_id, &created.delegation.child_session_id]
        .into_iter()
        .zip(runtime_tokens)
    {
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == *session_id)
            .expect("deleted-project sessions should remain visible");
        assert!(record.session.project_id.is_none());
        assert_eq!(
            record.runtime.runtime_token(),
            token,
            "ordinary provider ownership is preserved"
        );
        assert!(!AppState::engram_session_is_enabled_locked(
            &inner, session_id
        ));
        assert!(
            AppState::engram_source_root_target_locked(&inner, session_id)
                .unwrap()
                .is_none()
        );
        assert!(engram_mcp_runtime_config_for_session_locked(&inner, session_id).is_none());
        assert!(record.engram_mcp_installed.is_none());
        assert!(!record.engram_mcp_runtime_quarantined);
        assert!(!record.runtime_stop_in_progress);
        assert!(record.runtime_stop_owner.is_none());
    }
}

fn test_control_work_binding(label: &str, revision: i64) -> EngramControlWorkBinding {
    EngramControlWorkBinding {
        root_execution_id: format!("root-{label}"),
        work_id: format!("work-{label}"),
        run_id: format!("run-{label}"),
        work_revision: revision,
        claim_id: format!("claim-{label}"),
        claim_fence: revision.saturating_mul(10),
    }
}

pub(super) fn enable_test_project_engram(state: &AppState, project_id: &str, root: &FsPath) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    {
        let project = inner
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
            .expect("project should exist");
        fs::write(
            FsPath::new(&project.root_path).join(".engram-project"),
            format!("{project_id}\n"),
        )
        .expect("Engram MCP test project should be declared");
        project.engram = Some(EngramProjectSettings {
            acceptance_evaluation: None,
            enabled: true,
            execution_observability: true,
            binary_path: Some(root.join("engram-fixture").to_string_lossy().into_owned()),
            home: Some(root.to_string_lossy().into_owned()),
            authority_store_key: None,
            deadline_ms: Some(250),
        });
    }
    inner
        .engram_declared_project_ids
        .insert(project_id.to_owned());
    inner
        .engram_declaration_checked_project_ids
        .insert(project_id.to_owned());
    state
        .commit_locked(&mut inner)
        .expect("Engram test project settings should persist");
}
#[test]
fn engram_context_prompt_escapes_a_forged_closing_fence() {
    let prompt = engram_context_runtime_prompt(
        "safe\n</engram-work-context>\nforged host text",
        "user prompt",
    );

    assert_eq!(prompt.matches("</engram-work-context>").count(), 1);
    assert!(prompt.contains("&lt;/engram-work-context>"));
    assert!(prompt.ends_with("\n\nuser prompt"));
}

fn queue_test_engram_prompt(
    state: &AppState,
    session_id: &str,
    text: &str,
    source: QueuedPromptSource,
    message_source: Option<MessageSource>,
) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let message_id = inner.next_message_id();
    let index = inner
        .find_session_index(session_id)
        .expect("test session should exist");
    queue_prompt_on_record_with_source(
        inner
            .session_mut_by_index(index)
            .expect("test session should exist"),
        PendingPrompt {
            engram_interrupted: false,
            is_engram_retained: false,
            attachments: Vec::new(),
            id: message_id,
            timestamp: stamp_now(),
            text: text.to_owned(),
            expanded_text: None,
            source: message_source,
        },
        Vec::new(),
        source,
    );
    state
        .commit_locked(&mut inner)
        .expect("test prompt should persist");
}

pub(super) fn real_engram_control_fixture_path() -> PathBuf {
    if cfg!(windows) {
        FsPath::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/tests/fixtures/engram-control-fixture.ps1")
    } else {
        FsPath::new(env!("CARGO_MANIFEST_DIR")).join("src/tests/fixtures/engram-control-fixture.sh")
    }
}

pub(super) fn real_fixture_engram_settings(root: &FsPath) -> EngramProjectSettings {
    EngramProjectSettings {
        acceptance_evaluation: None,
        enabled: true,
        execution_observability: true,
        binary_path: Some(
            real_engram_control_fixture_path()
                .to_string_lossy()
                .into_owned(),
        ),
        home: Some(root.to_string_lossy().into_owned()),
        authority_store_key: None,
        deadline_ms: Some(250),
    }
}

fn install_fixture_engram_host_settings(state: &AppState, home: &FsPath) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    inner.preferences.engram = EngramHostSettings {
        developer_name: default_engram_developer_name(),
        binary_path: real_engram_control_fixture_path()
            .to_string_lossy()
            .into_owned(),
        home: home.to_string_lossy().into_owned(),
    };
    state
        .commit_locked(&mut inner)
        .expect("fixture host settings should persist");
}

fn materialize_fixture_engram_store(
    project_root: &FsPath,
    home: &FsPath,
) -> EngramAuthorityStoreKey {
    let project_id = fs::read_to_string(project_root.join(".engram-project"))
        .expect("fixture project id should exist")
        .trim()
        .to_owned();
    let database_path = work_database_path(home, &project_id);
    fs::create_dir_all(database_path.parent().unwrap()).unwrap();
    fs::write(&database_path, b"fixture database").expect("fixture database should exist");
    EngramAuthorityStoreKey {
        database_path: normalize_user_facing_path(
            &fs::canonicalize(database_path).expect("fixture database should canonicalize"),
        ),
        project_id,
    }
}

#[test]
fn project_engram_verification_is_redacted_and_does_not_mutate_settings() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-project-settings-verify");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "fixture-ready\n").expect("fixture mode should write");
    install_fixture_engram_host_settings(&state, &root);
    let project_id = create_test_project(&state, &root, "Engram settings verification");
    let verification = state
        .verify_project_engram_settings(
            &project_id,
            UpdateProjectEngramSettingsRequest {
                acceptance_evaluation: None,
                enabled: true,
                execution_observability: Some(true),
                binary_path: Some(
                    real_engram_control_fixture_path()
                        .to_string_lossy()
                        .into_owned(),
                ),
                home: Some(root.to_string_lossy().into_owned()),
                deadline_ms: Some(250),
            },
        )
        .expect("fixture settings should verify");

    assert!(verification.verified);
    assert!(verification.ready);
    assert_eq!(verification.full_audit, "not_run");
    assert_eq!(verification.project_id, "fixture-ready");
    assert_eq!(
        verification.binary_path,
        real_engram_control_fixture_path()
            .to_string_lossy()
            .into_owned()
    );
    assert_eq!(verification.home, root.to_string_lossy());
    let encoded =
        serde_json::to_string(&verification).expect("verification response should serialize");
    assert!(!encoded.contains("workAuthorityGrant"));
    assert!(
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .find_project(&project_id)
            .expect("project should remain")
            .engram
            .is_none(),
        "Verify must not persist the proposed settings"
    );
}

#[test]
fn project_engram_verification_and_save_require_no_grant() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-project-settings-missing-grant");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "fixture-ready\n")
        .expect("repository declaration should exist");
    install_fixture_engram_host_settings(&state, &root);
    let project_id = create_test_project(&state, &root, "Engram grant required");

    let verification = state
        .verify_project_engram_settings(
            &project_id,
            UpdateProjectEngramSettingsRequest {
                acceptance_evaluation: None,
                enabled: true,
                execution_observability: Some(false),
                binary_path: None,
                home: None,
                deadline_ms: None,
            },
        )
        .expect("base settings should verify without a grant");

    assert!(verification.verified);
    let snapshot = state
        .patch_project_engram_settings(
            &project_id,
            UpdateProjectEngramSettingsRequest {
                acceptance_evaluation: None,
                enabled: true,
                execution_observability: Some(false),
                binary_path: None,
                home: None,
                deadline_ms: None,
            },
        )
        .expect("base settings should save without a grant");
    let project = snapshot
        .projects
        .iter()
        .find(|project| project.id == project_id)
        .expect("project should remain");
    assert!(
        project
            .engram
            .as_ref()
            .is_some_and(|settings| { settings.enabled && !settings.execution_observability })
    );
}

/// An update that does not name Execution Observability keeps the project's
/// stored choice. Disable sends only `enabled`, and a later enable that does
/// not name it keeps observations off: neither turns them back on without
/// consent. Only an explicit value changes it, ON as well as OFF.
#[test]
fn project_engram_patch_without_observability_keeps_the_stored_choice() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-project-settings-keep-observability");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "fixture-ready\n")
        .expect("repository declaration should exist");
    install_fixture_engram_host_settings(&state, &root);
    let project_id = create_test_project(&state, &root, "Engram keeps observability");
    let patch = |body: Value| -> EngramProjectSettings {
        let request: UpdateProjectEngramSettingsRequest =
            serde_json::from_value(body).expect("the request should parse");
        state
            .patch_project_engram_settings(&project_id, request)
            .expect("the settings should save");
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .find_project(&project_id)
            .expect("project should remain")
            .engram
            .clone()
            .expect("the project keeps its settings")
    };

    let saved = patch(json!({"enabled": true, "executionObservability": false}));
    assert!(saved.enabled && !saved.execution_observability);
    let disabled = patch(json!({"enabled": false}));
    assert!(!disabled.enabled);
    assert!(
        !disabled.execution_observability,
        "disable keeps observations off"
    );
    let enabled = patch(json!({"enabled": true}));
    assert!(enabled.enabled);
    assert!(
        !enabled.execution_observability,
        "an enable that does not name observations keeps them off"
    );
    for observing in [true, false, true] {
        let saved = patch(json!({"enabled": true, "executionObservability": observing}));
        assert_eq!(
            saved.execution_observability, observing,
            "an explicit value is applied"
        );
    }
    let disabled = patch(json!({"enabled": false}));
    assert!(!disabled.enabled);
    assert!(
        disabled.execution_observability,
        "disable keeps an explicit ON as well"
    );
}

#[test]
fn client_snapshot_derives_repository_engram_declaration_without_grant_fields() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-client-declaration");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "fixture-ready\n")
        .expect("repository declaration should exist");
    let project_id = create_test_project(&state, &root, "Declared Engram project");
    state
        .update_project_engram_settings(&project_id, real_fixture_engram_settings(&root))
        .expect("fixture Engram settings should enable");

    let snapshot = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        state.snapshot_from_inner(&inner)
    };
    let encoded = serde_json::to_value(snapshot).expect("client snapshot should serialize");
    let project = encoded["projects"]
        .as_array()
        .expect("projects should serialize")
        .iter()
        .find(|project| project["id"] == project_id)
        .expect("declared project should be present");

    assert_eq!(project["engramDeclared"], true);
    assert!(project.get("engramGrantConfigured").is_none());
    assert_eq!(project["engramOperatorDisabled"], false);
    assert!(project["engram"].get("workAuthorityGrant").is_none());
}

#[test]
fn host_engram_settings_are_machine_scoped_and_cannot_rotate_while_enabled() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-host-settings");
    let project_root = root.join("project");
    let replacement_home = root.join("replacement-home");
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&replacement_home).expect("replacement home should exist");
    fs::write(project_root.join(".engram-project"), "fixture-ready\n")
        .expect("repository declaration should exist");

    state
        .update_engram_host_settings(UpdateEngramHostSettingsRequest {
            developer_name: "DEV".to_owned(),
            binary_path: real_engram_control_fixture_path()
                .to_string_lossy()
                .into_owned(),
            home: root.to_string_lossy().into_owned(),
        })
        .expect("host Engram settings should persist");
    assert_eq!(
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .preferences
            .engram
            .developer_name,
        "dev",
        "the stable principal is stored in its canonical lowercase form"
    );
    let project_id = create_test_project(&state, &project_root, "Host settings project");
    let mut settings = real_fixture_engram_settings(&root);
    state
        .update_project_engram_settings(&project_id, settings)
        .expect("fixture Engram settings should enable");
    state
        .update_engram_host_settings(UpdateEngramHostSettingsRequest {
            developer_name: "dev".to_owned(),
            binary_path: real_engram_control_fixture_path()
                .to_string_lossy()
                .into_owned(),
            home: root.to_string_lossy().into_owned(),
        })
        .expect("an unchanged host configuration should remain accepted");

    let identity_error = match state.update_engram_host_settings(UpdateEngramHostSettingsRequest {
        developer_name: "greg".to_owned(),
        binary_path: real_engram_control_fixture_path()
            .to_string_lossy()
            .into_owned(),
        home: root.to_string_lossy().into_owned(),
    }) {
        Ok(_) => panic!("enabled projects must fence host principal rotation"),
        Err(error) => error,
    };
    assert!(
        identity_error
            .message
            .contains("disable every enabled Engram project")
    );

    let error = match state.update_engram_host_settings(UpdateEngramHostSettingsRequest {
        developer_name: "dev".to_owned(),
        binary_path: real_engram_control_fixture_path()
            .to_string_lossy()
            .into_owned(),
        home: replacement_home.to_string_lossy().into_owned(),
    }) {
        Ok(_) => panic!("enabled projects must fence host settings rotation"),
        Err(error) => error,
    };
    assert!(
        error
            .message
            .contains("disable every enabled Engram project")
    );

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert_eq!(inner.preferences.engram.home, root.to_string_lossy());
    }
}

#[test]
fn host_engram_settings_validate_the_developer_principal() {
    let home = TestTempRoot::create("termal-developer-principal-home");
    assert_eq!(default_engram_developer_name(), "dev");
    let missing_developer_name = serde_json::from_value::<UpdateEngramHostSettingsRequest>(json!({
        "binaryPath": "engram",
        "home": "C:/engram-home",
    }));
    let error = match missing_developer_name {
        Ok(_) => panic!("the full-replace request must require developerName"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("missing field `developerName`"),
        "unexpected missing-principal error: {error}"
    );

    for developer_name in ["", "two words", "dev/slash", "dev@host", "żółw"] {
        let mut settings = EngramHostSettings::default();
        settings.developer_name = developer_name.to_owned();
        let error = normalize_engram_host_settings(settings)
            .expect_err("invalid developer principals must fail validation");
        assert!(
            error.message.contains("developerName"),
            "unexpected validation error for {developer_name:?}: {error:?}"
        );
    }

    let mut settings = EngramHostSettings::default();
    settings.developer_name = " Greg.Dev_1-Local ".to_owned();
    settings.home = home.path().to_string_lossy().into_owned();
    let normalized = normalize_engram_host_settings(settings)
        .expect("valid developer principals should normalize");
    assert_eq!(normalized.developer_name, "greg.dev_1-local");
}

#[test]
fn project_engram_connection_accepts_the_default_path_command() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-project-settings-path-command");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "fixture-ready\n").expect("fixture mode should write");
    let project_id = create_test_project(&state, &root, "Engram PATH command");
    let project = state
        .inner
        .lock()
        .expect("state mutex poisoned")
        .find_project(&project_id)
        .expect("project should remain")
        .clone();
    let settings = EngramProjectSettings {
        acceptance_evaluation: None,
        enabled: true,
        execution_observability: true,
        binary_path: Some("engram".to_owned()),
        home: Some(root.to_string_lossy().into_owned()),
        authority_store_key: None,
        deadline_ms: Some(2_000),
    };

    let (binary_path, project_file, home) =
        validate_engram_project_connection_paths(&project, &settings)
            .expect("a single command name should resolve through PATH at execution time");

    assert_eq!(binary_path, PathBuf::from("engram"));
    assert_eq!(
        project_file,
        normalize_user_facing_path(&root.join(".engram-project"))
    );
    assert_eq!(home, root);
}

fn attach_engram_mcp_test_runtime(state: &AppState, session_id: &str) {
    let agent = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .expect("test session should exist");
        inner.sessions[index].session.agent
    };
    let runtime = match agent {
        Agent::Claude => {
            let (handle, _input_rx) =
                test_claude_runtime_handle(&format!("engram-mcp-claude-{session_id}"));
            SessionRuntime::Claude(handle)
        }
        Agent::Codex => {
            let (handle, _input_rx) =
                test_codex_runtime_handle(&format!("engram-mcp-codex-{session_id}"));
            SessionRuntime::Codex(handle)
        }
        Agent::Cursor => {
            let (handle, _input_rx) = test_acp_runtime_handle(
                AcpAgent::Cursor,
                &format!("engram-mcp-cursor-{session_id}"),
            );
            SessionRuntime::Acp(handle)
        }
        other => panic!("unsupported Engram MCP test runtime: {other:?}"),
    };
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(session_id)
        .expect("test session should exist");
    let record = inner
        .session_mut_by_index(index)
        .expect("test session index should be valid");
    record.runtime = runtime;
    record.runtime_reset_required = false;
    state
        .commit_locked(&mut inner)
        .expect("test runtime should persist");
}

fn link_engram_mcp_test_descendant(
    state: &AppState,
    parent_session_id: &str,
    child_session_id: &str,
    root: &FsPath,
) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let delegation_id = format!("engram-mcp-delegation-{child_session_id}");
    let child_index = inner
        .find_session_index(child_session_id)
        .expect("Engram MCP descendant should exist");
    let child = inner
        .session_mut_by_index(child_index)
        .expect("Engram MCP descendant index should be valid");
    child.session.parent_delegation_id = Some(delegation_id.clone());
    child.session.status = SessionStatus::Active;
    child.session.preview = "Running delegated review...".to_owned();
    inner.delegations.push(DelegationRecord {
        original_project_id: None,
        project_basis_recorded: false,
        id: delegation_id,
        parent_session_id: parent_session_id.to_owned(),
        child_session_id: child_session_id.to_owned(),
        mode: DelegationMode::Reviewer,
        status: DelegationStatus::Running,
        title: "Engram MCP descendant".to_owned(),
        prompt: "Exercise inherited Engram MCP invalidation.".to_owned(),
        cwd: root.to_string_lossy().into_owned(),
        agent: Agent::Cursor,
        model: None,
        write_policy: DelegationWritePolicy::ReadOnly,
        created_at: stamp_now(),
        started_at: Some(stamp_now()),
        completed_at: None,
        result: None,
        submitted_review_result: None,
        post_submission_transport_error: None,
        base_revision: None,
        review_result_recovery_probe_attempt: None,
        review_result_recovery_error: None,
        review_result_schema_version: None,
        queued_followup_prompt_id: None,
        review_result_submission_attempt: 0,
        acceptance_evaluation: None,
        attempt: DelegationAttemptState::default(),
    });
    let delegation_index = inner.delegations.len() - 1;
    inner.mark_delegation_mutated(delegation_index);
    state
        .commit_locked(&mut inner)
        .expect("test delegation should persist");
}

fn record_test_project_runtime_descriptors(state: &AppState, project_id: &str) {
    let runtimes = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        project_engram_session_ids_locked(&inner, project_id)
            .into_iter()
            .filter_map(|id| {
                let record = inner
                    .find_session_index(&id)
                    .and_then(|i| inner.sessions.get(i))?;
                Some((id, record.session.agent, record.runtime.runtime_token()?))
            })
            .collect::<Vec<_>>()
    };
    for (id, agent, token) in runtimes {
        match agent {
            Agent::Claude => {
                state
                    .engram_mcp_stdio_config_for_runtime(&id, &token)
                    .expect("fixture must install its actual Engram MCP descriptor");
            }
            agent if agent.supports_codex_prompt_settings() => {
                state
                    .termal_delegation_mcp_codex_config_for_runtime(&id, &token)
                    .expect("fixture must install its actual Engram MCP descriptor");
            }
            _ => {
                state
                    .termal_delegation_mcp_acp_servers_for_runtime(&id, &token)
                    .expect("fixture must install its actual Engram MCP descriptor");
            }
        }
    }
}

fn ordinary_runtime_family_fixture(suffix: &str) -> (AppState, PathBuf, String, Vec<String>) {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join(format!("engram-mcp-runtime-{suffix}"));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join(".engram-project"), "fixture-ready\n").unwrap();
    let project_id = create_test_project(&state, &root, "Engram MCP runtime lifecycle");
    state
        .update_project_engram_settings(&project_id, real_fixture_engram_settings(&root))
        .expect("ordinary connection should enable");
    let claude = create_test_project_session(&state, Agent::Claude, &project_id, &root);
    let codex = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    let cursor = create_test_project_session(&state, Agent::Cursor, &project_id, &root);
    let descendant = test_session_id(&state, Agent::Cursor);
    link_engram_mcp_test_descendant(&state, &codex, &descendant, &root);
    let ids = vec![claude, codex, cursor, descendant];
    for id in &ids {
        attach_engram_mcp_test_runtime(&state, id);
    }
    record_test_project_runtime_descriptors(&state, &project_id);
    (state, root, project_id, ids)
}
#[test]
fn runtime_mcp_composition_records_the_exact_installed_engram_descriptor() {
    let (state, root, _project_id, session_ids) =
        ordinary_runtime_family_fixture("installed-descriptor");
    for session_id in &session_ids {
        let (agent, runtime_token) = {
            let inner = state.inner.lock().expect("state mutex poisoned");
            let record = inner
                .find_session_index(session_id)
                .and_then(|index| inner.sessions.get(index))
                .expect("runtime session should exist");
            (
                record.session.agent,
                record
                    .runtime
                    .runtime_token()
                    .expect("attached runtime should have a token"),
            )
        };
        match agent {
            Agent::Claude => {
                state
                    .engram_mcp_stdio_config_for_runtime(session_id, &runtime_token)
                    .expect("Claude runtime should receive Engram MCP config");
            }
            Agent::Codex => {
                state
                    .termal_delegation_mcp_codex_config_for_runtime(session_id, &runtime_token)
                    .expect("Codex runtime should receive Engram MCP config");
            }
            Agent::Cursor => {
                state
                    .termal_delegation_mcp_acp_servers_for_runtime(session_id, &runtime_token)
                    .expect("ACP runtime should receive Engram MCP config");
            }
            other => panic!("unexpected runtime-family agent: {other:?}"),
        }
    }

    let expected_binary = real_engram_control_fixture_path()
        .to_string_lossy()
        .into_owned();
    let expected_home = root.to_string_lossy().into_owned();
    let inner = state.inner.lock().expect("state mutex poisoned");
    for session_id in session_ids {
        let descriptor = inner
            .find_session_index(&session_id)
            .and_then(|index| inner.sessions.get(index))
            .and_then(|record| record.engram_mcp_installed.as_ref())
            .expect("runtime config composition should record its descriptor");
        assert_eq!(descriptor.binary_path, expected_binary);
        assert_eq!(descriptor.home, expected_home);
        assert_eq!(
            descriptor.actor_id,
            engram_seat_id(
                "dev",
                &inner.sessions[inner.find_session_index(&session_id).unwrap()].session
            )
        );
    }
    drop(inner);
}

#[test]
fn engram_enabled_settings_default_blank_home_and_reject_relative_home() {
    // A blank home resolves through USERPROFILE/HOME, and the save verifies
    // the connection by running the (fixture) binary against that home. Scope
    // both variables before the exact test child starts, never in the suite
    // parent. Consumers finish before the parent removes the scoped home.
    home_fixture::run(
        "tests::engram_host_adapter::engram_enabled_settings_default_blank_home_and_reject_relative_home",
        |scoped_user_home| {
            let state = test_app_state();
            let root = state
                .test_temp_root
                .as_ref()
                .expect("test root should exist")
                .path()
                .join("engram-invalid-home");
            fs::create_dir_all(&root).expect("project root should exist");
            fs::write(root.join(".engram-project"), "fixture-ready\n")
                .expect("fixture mode should write");
            let project_id = create_test_project(&state, &root, "Engram invalid home");
            let scoped_engram_home = scoped_user_home.join(".engram");
            fs::create_dir_all(&scoped_engram_home)
                .expect("scoped default Engram home should exist");
            assert_eq!(
                default_engram_home_path().expect("the scoped user home should resolve"),
                scoped_engram_home,
                "the default home must follow the scoped USERPROFILE/HOME"
            );

            let mut blank_home = real_fixture_engram_settings(&root);
            blank_home.home = Some(String::new());
            state
                .update_project_engram_settings(&project_id, blank_home)
                .expect("an enabled blank home should use the documented default");
            let expected_home = scoped_engram_home.to_string_lossy().into_owned();
            assert_eq!(
                state
                    .inner
                    .lock()
                    .expect("state mutex poisoned")
                    .find_project(&project_id)
                    .and_then(|project| project.engram.as_ref())
                    .and_then(|settings| settings.home.as_deref()),
                Some(expected_home.as_str())
            );
            // The verification ran the fixture against the scoped default home: its
            // placeholder store is here, inside the test root, and nowhere else.
            assert!(
                scoped_engram_home
                    .join("projects")
                    .join(sha256_hex(b"fixture-ready"))
                    .join("engram.db")
                    .is_file(),
                "the fixture store must be created under the scoped default home"
            );

            let mut relative_home = real_fixture_engram_settings(&root);
            relative_home.home = Some(".".to_owned());
            let error = match state.update_project_engram_settings(&project_id, relative_home) {
                Ok(_) => panic!("enabled Engram must reject a relative home"),
                Err(error) => error,
            };
            assert_eq!(error.status, StatusCode::BAD_REQUEST);
            assert!(
                error.message.contains("non-empty absolute path"),
                "unexpected relative-home validation error: {}",
                error.message
            );
        },
    );
}

#[test]
fn engram_settings_normalize_binary_and_home_before_persisting_or_composing_runtime_argv() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-normalized-settings-paths");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "fixture-ready\n").expect("fixture mode should write");
    let project_id = create_test_project(&state, &root, "Engram normalized settings paths");
    let session_id = create_test_project_session(&state, Agent::Claude, &project_id, &root);
    let expected_binary = real_engram_control_fixture_path()
        .to_string_lossy()
        .into_owned();
    let expected_home = root.to_string_lossy().into_owned();
    let mut settings = real_fixture_engram_settings(&root);
    settings.binary_path = Some(format!("  {expected_binary}  "));
    settings.home = Some(format!("  {expected_home}  "));

    state
        .update_project_engram_settings(&project_id, settings)
        .expect("trimmed paths should validate and persist");
    attach_engram_mcp_test_runtime(&state, &session_id);
    let runtime_token = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let project_settings = inner
            .find_project(&project_id)
            .and_then(|project| project.engram.as_ref())
            .expect("normalized settings should remain");
        assert_eq!(
            project_settings.binary_path.as_deref(),
            Some(expected_binary.as_str())
        );
        assert_eq!(
            project_settings.home.as_deref(),
            Some(expected_home.as_str())
        );
        inner
            .find_session_index(&session_id)
            .and_then(|index| inner.sessions.get(index))
            .and_then(|record| record.runtime.runtime_token())
            .expect("fixture runtime should have a token")
    };
    let config = state
        .engram_mcp_stdio_config_for_runtime(&session_id, &runtime_token)
        .expect("normalized settings should compose an Engram MCP config");
    assert_eq!(config.command, expected_binary);
    assert_eq!(
        config
            .args
            .windows(2)
            .find(|pair| pair[0] == "--home")
            .map(|pair| pair[1].as_str()),
        Some(expected_home.as_str())
    );

    let mut disabled_with_blank_paths = EngramProjectSettings::default();
    disabled_with_blank_paths.binary_path = Some("  \t  ".to_owned());
    disabled_with_blank_paths.home = Some("   ".to_owned());
    state
        .update_project_engram_settings(&project_id, disabled_with_blank_paths)
        .expect("blank disabled paths should inherit the recovery connection");
    let inner = state.inner.lock().expect("state mutex poisoned");
    let project_settings = inner
        .find_project(&project_id)
        .and_then(|project| project.engram.as_ref())
        .expect("disabled normalized settings should remain");
    assert!(!project_settings.enabled);
    assert_eq!(
        project_settings.binary_path.as_deref(),
        Some(expected_binary.as_str())
    );
    assert_eq!(
        project_settings.home.as_deref(),
        Some(expected_home.as_str())
    );
}

#[test]
fn engram_connection_change_cannot_smuggle_an_unvalidated_binary() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-grant-clear-invalid-binary");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "fixture-ready\n").expect("fixture mode should write");
    let project_id = create_test_project(&state, &root, "Engram invalid clear reconfigure");
    let mut enabled = real_fixture_engram_settings(&root);
    state
        .update_project_engram_settings(&project_id, enabled.clone())
        .expect("initial authority should persist");
    enabled = state
        .inner
        .lock()
        .expect("state mutex poisoned")
        .find_project(&project_id)
        .and_then(|project| project.engram.clone())
        .expect("validated settings should persist the doctor store identity");

    let mut invalid_clear = enabled.clone();
    invalid_clear.binary_path = Some(
        root.join("missing-engram-binary")
            .to_string_lossy()
            .into_owned(),
    );
    let error = match state.update_project_engram_settings(&project_id, invalid_clear) {
        Ok(_) => panic!("grant clear plus binary replacement must run normal validation"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(
        error
            .message
            .contains("binaryPath must be a command on PATH or an existing absolute file")
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(
        inner
            .find_project(&project_id)
            .and_then(|project| project.engram.as_ref()),
        Some(&enabled)
    );
    assert!(inner.engram_retired_work_authority_grants.is_empty());
    drop(inner);
}

#[test]
fn project_engram_patch_rejects_removed_work_authority_grant_field() {
    let error = match serde_json::from_value::<UpdateProjectEngramSettingsRequest>(json!({
        "enabled": true,
        "executionObservability": false,
        "binaryPath": "engram",
        "home": "C:\\engram",
        "workAuthorityGrant": "ab".repeat(32),
        "deadlineMs": 250
    })) {
        Ok(_) => panic!("the removed grant field must fail closed as unknown input"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("unknown field `workAuthorityGrant`")
    );
}

#[test]
fn engram_mcp_enable_marks_runtime_but_deadline_only_patch_does_not() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-mcp-enable-runtime");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "fixture-ready\n").expect("fixture mode should write");
    let project_id = create_test_project(&state, &root, "Engram MCP enable lifecycle");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    attach_engram_mcp_test_runtime(&state, &session_id);
    let settings = real_fixture_engram_settings(&root);

    state
        .update_project_engram_settings(&project_id, settings.clone())
        .expect("enable should persist");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("test session should exist");
        assert!(inner.sessions[index].runtime_reset_required);
        inner
            .session_mut_by_index(index)
            .expect("test session index should be valid")
            .runtime_reset_required = false;
        state
            .commit_locked(&mut inner)
            .expect("test reset flag should persist");
    }

    let mut deadline_only = settings;
    deadline_only.deadline_ms = Some(300);
    state
        .update_project_engram_settings(&project_id, deadline_only)
        .expect("deadline-only patch should persist");
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .find_session_index(&session_id)
        .and_then(|index| inner.sessions.get(index))
        .expect("test session should exist");
    assert!(!record.runtime_reset_required);
    assert!(matches!(record.runtime, SessionRuntime::Codex(_)));
    drop(inner);
}

fn set_test_project_engram_mcp_settings(
    state: &AppState,
    project_id: &str,
    enabled: bool,
    historical_grant: Option<&str>,
) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    {
        let project = inner
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
            .expect("project should exist");
        fs::write(
            FsPath::new(&project.root_path).join(".engram-project"),
            format!("{project_id}\n"),
        )
        .expect("Engram MCP test project should be declared");
        project.engram = Some(EngramProjectSettings {
            acceptance_evaluation: None,
            enabled,
            execution_observability: enabled,
            binary_path: Some("C:/tools/engram.exe".to_owned()),
            home: Some("C:/engram-home".to_owned()),
            authority_store_key: None,
            deadline_ms: Some(250),
        });
    }
    // Historical credentials stay passive and must never enter MCP argv or env.
    if let Some(secret) = historical_grant {
        inner
            .engram_retired_work_authority_grants
            .push(EngramRetiredWorkAuthorityGrant {
                home: "C:/engram-home".to_owned(),
                project_root: String::new(),
                store_key: None,
                project_id: project_id.to_owned(),
                grant_hash: secret.to_owned(),
                retired_at: "2026-08-29T00:00:00Z".to_owned(),
                reason: "passive fixture history".to_owned(),
                revoke_confirmed: false,
            });
    }
    if enabled {
        inner
            .engram_declared_project_ids
            .insert(project_id.to_owned());
    } else {
        inner.engram_declared_project_ids.remove(project_id);
    }
    inner
        .engram_declaration_checked_project_ids
        .insert(project_id.to_owned());
    state
        .commit_locked(&mut inner)
        .expect("Engram MCP settings should persist");
}

fn assert_delegation_mcp_baseline_is_unchanged(state: &AppState, session_id: &str) {
    let command = termal_delegation_mcp_current_exe()
        .expect("TermAl test executable should resolve for delegation MCP");
    let base_url = state.local_http_base_url();
    assert_eq!(
        state
            .termal_delegation_mcp_claude_config_json(session_id)
            .expect("Claude baseline config should compose"),
        termal_delegation_mcp_claude_config_json_with_command(&command, session_id, &base_url),
    );
    assert_eq!(
        state
            .termal_delegation_mcp_acp_servers(session_id)
            .expect("ACP baseline config should compose"),
        termal_delegation_mcp_acp_servers_with_command(&command, session_id, &base_url),
    );
    assert_eq!(
        state
            .termal_delegation_mcp_codex_config(session_id)
            .expect("Codex baseline config should compose"),
        termal_delegation_mcp_codex_config_with_command(&command, session_id, &base_url),
    );
}

#[test]
fn project_actor_engram_mcp_is_added_to_claude_acp_and_codex_configs() {
    let (state, _runtime_rx) =
        test_app_state_with_delegation_codex_runtime("engram-per-session-mcp");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-per-session-mcp-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram per-session MCP project");
    set_test_project_engram_mcp_settings(&state, &project_id, true, Some("operator-secret-grant"));

    let claude_session = create_test_project_session(&state, Agent::Claude, &project_id, &root);
    let acp_session = create_test_project_session(&state, Agent::Cursor, &project_id, &root);
    let codex_session = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    let project_file = normalize_user_facing_path(&root.join(".engram-project"))
        .to_string_lossy()
        .into_owned();

    let claude: Value = serde_json::from_str(
        &state
            .termal_delegation_mcp_claude_config_json(&claude_session)
            .expect("Claude MCP config should compose"),
    )
    .expect("Claude MCP config should be JSON");
    assert_eq!(
        claude["mcpServers"]
            .as_object()
            .expect("Claude MCP servers should be an object")
            .len(),
        2
    );
    assert_eq!(
        claude["mcpServers"]["engram"]["command"],
        "C:/tools/engram.exe"
    );
    // Base integration supplies only the non-secret host identity/context.
    assert_eq!(
        claude["mcpServers"]["engram"]["args"],
        json!([
            "--project-file",
            project_file,
            "--home",
            "C:/engram-home",
            "mcp",
            "--actor-id",
            "dev/claude",
            "--actor-context",
            "agent=claude;model=default",
            "--session-id",
            claude_session,
        ])
    );
    assert_eq!(
        claude["mcpServers"]["engram"]["env"],
        json!({
            "ENGRAM_ACTOR_CONTEXT": "agent=claude;model=default",
            "ENGRAM_ACTOR_ID": "dev/claude",
            "ENGRAM_HOME": "C:/engram-home",
            "ENGRAM_SESSION_ID": claude_session,
        })
    );
    assert!(
        !claude["mcpServers"]["engram"]["args"]
            .to_string()
            .contains("operator-secret-grant")
    );

    let acp = state
        .termal_delegation_mcp_acp_servers(&acp_session)
        .expect("ACP MCP config should compose");
    assert_eq!(
        acp.as_array()
            .expect("ACP servers should be an array")
            .len(),
        2
    );
    assert_eq!(acp[1]["name"], ENGRAM_MCP_SERVER_NAME);
    assert_eq!(acp[1]["command"], "C:/tools/engram.exe");
    assert_eq!(acp[1]["args"][6], "dev/cursor");
    assert_eq!(acp[1]["args"][8], "agent=cursor;model=auto");
    assert_eq!(acp[1]["args"][10], acp_session);
    assert_eq!(acp[1]["args"].as_array().map(Vec::len), Some(11));
    assert_eq!(
        acp[1]["env"],
        json!([
            { "name": "ENGRAM_ACTOR_CONTEXT", "value": "agent=cursor;model=auto" },
            { "name": "ENGRAM_ACTOR_ID", "value": "dev/cursor" },
            { "name": "ENGRAM_HOME", "value": "C:/engram-home" },
            { "name": "ENGRAM_SESSION_ID", "value": acp_session },
        ])
    );
    assert!(!acp[1]["args"].to_string().contains("operator-secret-grant"));

    let codex = state
        .termal_delegation_mcp_codex_config(&codex_session)
        .expect("Codex MCP config should compose");
    assert_eq!(
        codex["mcp_servers"]
            .as_object()
            .expect("Codex MCP servers should be an object")
            .len(),
        2
    );
    assert_eq!(codex["mcp_servers"]["engram"]["args"][6], "dev/codex");
    assert_eq!(
        codex["mcp_servers"]["engram"]["args"][8],
        "agent=codex;model=gpt-5.4;reasoning=medium"
    );
    assert_eq!(codex["mcp_servers"]["engram"]["args"][10], codex_session);
    assert_eq!(
        codex["mcp_servers"]["engram"]["args"]
            .as_array()
            .map(Vec::len),
        Some(11)
    );
    assert_eq!(
        codex["mcp_servers"]["engram"]["env"],
        json!({
            "ENGRAM_ACTOR_CONTEXT": "agent=codex;model=gpt-5.4;reasoning=medium",
            "ENGRAM_ACTOR_ID": "dev/codex",
            "ENGRAM_HOME": "C:/engram-home",
            "ENGRAM_SESSION_ID": codex_session,
        })
    );
    assert_eq!(
        codex["shell_environment_policy"]["set"],
        Value::Object(expected_codex_shell_environment_set(&codex, &codex_session)),
        "Codex shell policy must contain exactly the Engram and TermAl owned identities"
    );
    assert!(
        !codex["mcp_servers"]["engram"]["args"]
            .to_string()
            .contains("operator-secret-grant")
    );

    let state_json = serde_json::to_value(state.snapshot()).expect("state should serialize");
    let client_engram = state_json["projects"]
        .as_array()
        .and_then(|projects| projects.iter().find(|project| project["id"] == project_id))
        .and_then(|project| project.get("engram"))
        .expect("client project should retain public Engram settings");
    assert!(client_engram.get("workAuthorityGrant").is_none());
}

#[test]
fn engram_actor_context_collapses_controls_and_omits_oversized_fields() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut session = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .sessions
            .iter()
            .find(|record| record.session.id == session_id)
            .expect("Claude session should exist")
            .session
            .clone()
    };
    session.model = "claude\nopus".to_owned();
    session.claude_effort = Some(ClaudeEffortLevel::High);
    assert_eq!(
        engram_actor_context(&session).as_deref(),
        Some("agent=claude;model=claude opus;reasoning=high")
    );

    session.model = "claude;agent=root=spoof%raw".to_owned();
    assert_eq!(
        engram_actor_context(&session).as_deref(),
        Some("agent=claude;model=claude%3Bagent%3Droot%3Dspoof%25raw;reasoning=high"),
        "reserved actor-context delimiters must be encoded inside values"
    );

    session.model = "x".repeat(ENGRAM_ACTOR_CONTEXT_MAX_BYTES);
    assert_eq!(
        engram_actor_context(&session).as_deref(),
        Some("agent=claude;reasoning=high"),
        "an oversized model field must be omitted at a field boundary"
    );
}

#[test]
fn engram_mcp_and_agent_process_identity_match_for_every_agent_kind() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-every-agent-identity");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram every-agent identity");
    set_test_project_engram_mcp_settings(&state, &project_id, true, None);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.preferences.engram.developer_name = "greg.dev".to_owned();
    }

    for (agent, model, reasoning, expected_actor, expected_context) in [
        (
            Agent::Codex,
            "gpt-5.4",
            Some("xhigh"),
            "greg.dev/codex",
            "agent=codex;model=gpt-5.4;reasoning=xhigh",
        ),
        (
            Agent::Claude,
            "claude-opus-4-1",
            Some("high"),
            "greg.dev/claude",
            "agent=claude;model=claude-opus-4-1;reasoning=high",
        ),
        (
            Agent::Cursor,
            "cursor-auto",
            None,
            "greg.dev/cursor",
            "agent=cursor;model=cursor-auto",
        ),
        (
            Agent::Gemini,
            "gemini-pro",
            None,
            "greg.dev/gemini",
            "agent=gemini;model=gemini-pro",
        ),
        (
            Agent::OpenCode,
            "anthropic/claude-sonnet",
            Some("high"),
            "greg.dev/opencode",
            "agent=opencode;model=anthropic/claude-sonnet;reasoning=high",
        ),
    ] {
        let session_id = create_test_project_session(&state, agent, &project_id, &root);
        let descriptor = {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&session_id)
                .expect("agent session should exist");
            let record = inner
                .session_mut_by_index(index)
                .expect("agent session index should remain valid");
            record.session.model = model.to_owned();
            match agent {
                Agent::Codex => record.session.reasoning_effort = Some(CodexReasoningEffort::XHigh),
                Agent::Claude => record.session.claude_effort = Some(ClaudeEffortLevel::High),
                Agent::OpenCode => record.session.opencode_effort = reasoning.map(str::to_owned),
                Agent::Cursor | Agent::Gemini | Agent::Kimi => {}
            }
            engram_mcp_runtime_config_for_session_locked(&inner, &session_id)
                .expect("eligible agent should receive Engram MCP")
                .stdio
        };

        assert_eq!(
            descriptor.env.get(ENGRAM_ACTOR_ID_ENV).map(String::as_str),
            Some(expected_actor),
            "{agent:?} must receive its stable developer/seat principal"
        );
        assert_eq!(
            descriptor
                .env
                .get(ENGRAM_ACTOR_CONTEXT_ENV)
                .map(String::as_str),
            Some(expected_context),
            "{agent:?} model/reasoning metadata belongs only in actor context"
        );
        let argument_after = |flag: &str| {
            descriptor
                .args
                .windows(2)
                .find(|pair| pair[0] == flag)
                .map(|pair| pair[1].as_str())
        };
        assert_eq!(argument_after("--actor-id"), Some(expected_actor));
        assert_eq!(argument_after("--actor-context"), Some(expected_context));
        assert_eq!(argument_after("--session-id"), Some(session_id.as_str()));

        let mut process = Command::new("engram-agent-environment-fixture");
        apply_agent_process_env(&mut process, None, Some(&descriptor))
            .expect("agent process environment should apply");
        let process_env = process
            .get_envs()
            .filter_map(|(name, value)| {
                let name = name.to_string_lossy().into_owned();
                ENGRAM_AGENT_PROCESS_ENV_NAMES
                    .contains(&name.as_str())
                    .then(|| value.map(|value| (name, value.to_string_lossy().into_owned())))
                    .flatten()
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            process_env, descriptor.env,
            "{agent:?} process shell and MCP child must receive byte-identical Engram variables"
        );
    }
}

#[test]
fn acp_session_setup_uses_the_engram_snapshot_that_spawned_its_process() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-acp-runtime-snapshot-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram ACP runtime snapshot");
    set_test_project_engram_mcp_settings(&state, &project_id, true, None);
    let session_id = create_test_project_session(&state, Agent::Cursor, &project_id, &root);
    let runtime_id = "engram-acp-runtime-snapshot";
    let (runtime, _runtime_rx) = test_acp_runtime_handle(AcpAgent::Cursor, runtime_id);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("ACP session should exist");
        inner.sessions[index].runtime = SessionRuntime::Acp(runtime);
    }
    let frozen_engram = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        engram_mcp_runtime_config_for_session_locked(&inner, &session_id)
            .expect("eligible ACP session should have an Engram descriptor")
            .stdio
    };
    let mut process_command = Command::new("engram-acp-runtime-snapshot-fixture");
    apply_agent_process_env(&mut process_command, None, Some(&frozen_engram))
        .expect("frozen descriptor should configure the ACP process environment");
    let process_engram_env = process_command
        .get_envs()
        .filter_map(|(name, value)| {
            let name = name.to_string_lossy().into_owned();
            ENGRAM_AGENT_PROCESS_ENV_NAMES
                .contains(&name.as_str())
                .then(|| {
                    (
                        name,
                        value
                            .expect("eligible ACP identity should set a value")
                            .to_string_lossy()
                            .into_owned(),
                    )
                })
        })
        .collect::<BTreeMap<_, _>>();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
            .and_then(|project| project.engram.as_mut())
            .expect("Engram settings should exist")
            .home = Some("C:/engram-home-after-spawn".to_owned());
    }
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert_eq!(
            engram_mcp_runtime_config_for_session_locked(&inner, &session_id)
                .expect("live Engram descriptor should still be eligible")
                .stdio
                .env
                .get(ENGRAM_HOME_ENV)
                .map(String::as_str),
            Some("C:/engram-home-after-spawn"),
            "the regression requires live state to differ from the runtime snapshot"
        );
    }

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        capabilities: Some(AcpCapabilities {
            supports_session_load: Some(false),
            supports_session_resume: None,
        }),
        ..AcpRuntimeState::default()
    }));
    let writer = SharedBufferWriter::default();
    let thread_writer = writer.clone();
    let thread_pending_requests = pending_requests.clone();
    let thread_state = state.clone();
    let thread_runtime_state = runtime_state.clone();
    let thread_session_id = session_id.clone();
    let runtime_token = RuntimeToken::Acp(runtime_id.to_owned());
    let setup = std::thread::spawn(move || {
        let mut stdin = thread_writer;
        ensure_acp_session_ready_inner(
            &mut stdin,
            &thread_pending_requests,
            &thread_state,
            &thread_session_id,
            &thread_runtime_state,
            AcpEngramMcpSource::Runtime {
                token: &runtime_token,
                engram: Some(&frozen_engram),
            },
            AcpAgent::Cursor,
            &AcpPromptCommand {
                cwd: root.to_string_lossy().into_owned(),
                cursor_mode: Some(CursorMode::Ask),
                model: "auto".to_owned(),
                opencode_effort: None,
                opencode_mode: None,
                prompt: "Use the frozen Engram identity.".to_owned(),
                resume_session_id: None,
            },
            AcpSessionPurpose::Prompt,
        )
    });

    let (_request_id, response_tx) = take_pending_acp_request(&pending_requests);
    response_tx
        .send(Ok(json!({
            "sessionId": "cursor-runtime-snapshot",
            "configOptions": [],
        })))
        .expect("session/new response should send");
    setup
        .join()
        .expect("ACP setup worker should finish")
        .expect("ACP session setup should succeed");

    let request = writer
        .contents()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|request| request["method"] == "session/new")
        .expect("ACP session/new request should be emitted");
    let engram = request["params"]["mcpServers"]
        .as_array()
        .and_then(|servers| {
            servers
                .iter()
                .find(|server| server["name"] == ENGRAM_MCP_SERVER_NAME)
        })
        .expect("ACP session setup should include Engram MCP");
    let mcp_engram_env = engram["env"]
        .as_array()
        .expect("ACP Engram environment should be an array")
        .iter()
        .map(|entry| {
            (
                entry["name"]
                    .as_str()
                    .expect("ACP environment name should be a string")
                    .to_owned(),
                entry["value"]
                    .as_str()
                    .expect("ACP environment value should be a string")
                    .to_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        mcp_engram_env, process_engram_env,
        "ACP process and MCP child must use one frozen descriptor snapshot"
    );
    assert_eq!(
        engram["env"],
        json!([
            { "name": "ENGRAM_ACTOR_CONTEXT", "value": "agent=cursor;model=auto" },
            { "name": "ENGRAM_ACTOR_ID", "value": "dev/cursor" },
            { "name": "ENGRAM_HOME", "value": "C:/engram-home" },
            { "name": "ENGRAM_SESSION_ID", "value": session_id },
        ]),
        "MCP setup must use the descriptor that also configured the ACP process environment"
    );
}

#[test]
fn base_only_engram_injects_mcp_and_refreshes_start_and_compaction_context() {
    let (state, runtime_rx) =
        test_app_state_with_delegation_codex_runtime("engram-base-context-nudge");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-base-context-nudge-project");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "fixture-ready\n")
        .expect("repository declaration should exist");
    let project_id = create_test_project(&state, &root, "Engram base context nudge");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let project = inner
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
            .expect("project should exist");
        project.engram = Some(EngramProjectSettings {
            acceptance_evaluation: None,
            enabled: true,
            execution_observability: false,
            binary_path: Some(
                real_engram_control_fixture_path()
                    .to_string_lossy()
                    .into_owned(),
            ),
            home: Some(root.to_string_lossy().into_owned()),
            authority_store_key: None,
            deadline_ms: Some(250),
        });
    }
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    state.refresh_engram_project_declaration_for_session_off_lock(&session_id);

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert!(engram_mcp_stdio_config_for_session_locked(&inner, &session_id).is_some());
    }

    state.prepare_engram_context_nudge_off_lock(&session_id);
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == session_id)
            .expect("session should exist");
        assert!(!record.engram.context_nudge_pending);
        assert!(
            record
                .engram
                .pending_context_nudge
                .as_deref()
                .is_some_and(|context| context.contains(&session_id))
        );
        assert_eq!(record.engram.context_nudge_generation, 1);
    }

    let source_session_id = test_session_id(&state, Agent::Claude);
    let dispatch = match state
        .dispatch_turn(
            &session_id,
            SendMessageRequest {
                text: "Review the queued work context.".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: Some(source_session_id),
                source_mailbox: None,
            },
        )
        .expect("base-only prompt should dispatch")
    {
        DispatchTurnResult::Dispatched(dispatch) => dispatch,
        DispatchTurnResult::DispatchedAfterQueue(_) | DispatchTurnResult::Queued => {
            panic!("idle base-only prompt should dispatch immediately")
        }
    };
    let runtime_prompt = match &dispatch {
        TurnDispatch::PersistentCodex { command, .. } => command.prompt.as_str(),
        TurnDispatch::PersistentClaude { .. } | TurnDispatch::PersistentAcp { .. } => {
            panic!("Codex session should produce a Codex dispatch")
        }
    };
    assert!(runtime_prompt.starts_with("<engram-work-context>"));
    assert!(
        runtime_prompt
            .find("</engram-work-context>")
            .expect("Engram fence should close")
            < runtime_prompt
                .find("[TermAl cross-session message]")
                .expect("peer envelope should follow host context")
    );
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == session_id)
            .expect("session should exist");
        assert!(matches!(
            record.session.messages.last(),
            Some(Message::Text { text, .. })
                if text == "Review the queued work context."
                    && !text.contains("engram-work-context")
        ));
        assert!(record.engram.pending_context_nudge.is_some());
    }
    deliver_turn_dispatch(&state, dispatch).expect("runtime should accept the contextual prompt");
    assert!(matches!(
        recv_within_guard(&runtime_rx, "contextual prompt should reach Codex")
            .expect("contextual prompt should reach Codex"),
        CodexRuntimeCommand::Prompt { .. }
    ));
    let runtime_token = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == session_id)
            .expect("session should exist");
        assert!(record.engram.pending_context_nudge.is_none());
        assert!(record.engram.context_nudge_delivery_generation.is_none());
        record
            .runtime
            .runtime_token()
            .expect("delivered turn should retain its runtime token")
    };
    state
        .finish_turn_ok_if_runtime_matches(&session_id, &runtime_token)
        .expect("first contextual turn should finish");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        mark_engram_mcp_runtime_resets_locked(&mut inner, std::slice::from_ref(&session_id));
    }
    state.prepare_engram_context_nudge_off_lock(&session_id);
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == session_id)
            .expect("session should exist");
        assert!(!record.engram.context_nudge_pending);
        assert_eq!(record.engram.context_nudge_generation, 2);
    }

    drop(runtime_rx);
    let rejected_dispatch = match state
        .dispatch_turn(
            &session_id,
            SendMessageRequest {
                text: "Preserve context if runtime delivery fails.".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .expect("second contextual prompt should stage")
    {
        DispatchTurnResult::Dispatched(dispatch) => dispatch,
        DispatchTurnResult::DispatchedAfterQueue(_) | DispatchTurnResult::Queued => {
            panic!("idle prompt should stage immediately")
        }
    };
    assert!(deliver_turn_dispatch(&state, rejected_dispatch).is_err());
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("session should exist");
    assert!(
        record.engram.pending_context_nudge.is_some(),
        "failed runtime delivery must preserve the context for retry"
    );
}

#[test]
fn settings_reset_supersedes_an_inflight_engram_context_refresh() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-context-nudge-generation-project");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "fixture-work-next-gated\n")
        .expect("gated fixture declaration should exist");
    let mut context_gate = phase_sync::WorkContextGate::new(&root);
    let project_id = create_test_project(&state, &root, "Engram context generation");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let project = inner
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
            .expect("project should exist");
        project.engram = Some(EngramProjectSettings {
            acceptance_evaluation: None,
            enabled: true,
            execution_observability: false,
            binary_path: Some(
                real_engram_control_fixture_path()
                    .to_string_lossy()
                    .into_owned(),
            ),
            home: Some(root.to_string_lossy().into_owned()),
            authority_store_key: None,
            deadline_ms: Some(250),
        });
    }
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    let refresh_state = state.clone();
    let refresh_session_id = session_id.clone();
    let refresh = std::thread::spawn(move || {
        refresh_state.prepare_engram_context_nudge_off_lock(&refresh_session_id)
    });
    context_gate.wait();
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == session_id)
            .unwrap();
        assert!(record.engram.context_nudge_in_progress);
    }

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        mark_engram_mcp_runtime_resets_locked(&mut inner, std::slice::from_ref(&session_id));
    }
    context_gate.release();
    assert_eq!(
        refresh.join().expect("context refresh thread should join"),
        EngramContextNudgePreparation::Ready
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("session should exist");
    assert_eq!(record.engram.context_nudge_generation, 2);
    assert!(!record.engram.context_nudge_pending);
    assert!(!record.engram.context_nudge_in_progress);
    assert!(record.engram.pending_context_nudge.is_some());
    drop(inner);
}

#[test]
fn claude_mcp_snapshot_composition_is_state_lock_safe() {
    let (state, _runtime_rx) =
        test_app_state_with_delegation_codex_runtime("engram-claude-mcp-lock-safety");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-claude-mcp-lock-safety-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram Claude MCP lock safety");
    set_test_project_engram_mcp_settings(&state, &project_id, true, None);
    let enabled_session = create_test_project_session(&state, Agent::Claude, &project_id, &root);
    let ineligible_session = test_session_id(&state, Agent::Claude);

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        !state.inner.is_not_held_by_current_thread_for_test(),
        "the regression must exercise MCP composition while the caller owns StateInner"
    );

    let enabled_engram = engram_mcp_stdio_config_for_session_locked(&inner, &enabled_session)
        .expect("enabled local Claude session should have an Engram descriptor");
    let enabled: Value = serde_json::from_str(
        &state
            .termal_delegation_mcp_claude_config_json_with_engram(
                &enabled_session,
                Some(&enabled_engram),
            )
            .expect("Claude MCP config should compose from the locked snapshot"),
    )
    .expect("Claude MCP config should be JSON");
    assert_eq!(
        enabled["mcpServers"][ENGRAM_MCP_SERVER_NAME]["command"],
        "C:/tools/engram.exe"
    );

    assert!(
        engram_mcp_stdio_config_for_session_locked(&inner, &ineligible_session).is_none(),
        "projectless Claude session should not receive Engram MCP"
    );
    let baseline: Value = serde_json::from_str(
        &state
            .termal_delegation_mcp_claude_config_json_with_engram(&ineligible_session, None)
            .expect("baseline Claude MCP config should compose from an empty snapshot"),
    )
    .expect("baseline Claude MCP config should be JSON");
    assert_eq!(
        baseline["mcpServers"]
            .as_object()
            .expect("Claude MCP servers should be an object")
            .len(),
        1
    );
    drop(inner);
}

#[test]
fn cold_claude_turn_start_does_not_reenter_state_mutex_for_engram_config() {
    let (mut state, _runtime_rx) =
        test_app_state_with_delegation_codex_runtime("engram-cold-claude-lock-safety");
    state.agent_runtime_spawning_enabled = true;
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-cold-claude-lock-safety-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram cold Claude lock safety");
    set_test_project_engram_mcp_settings(&state, &project_id, true, None);
    let session_id = create_test_project_session(&state, Agent::Claude, &project_id, &root);
    let missing_workdir = root.join("missing-runtime-workdir");
    let (result_tx, result_rx) = mpsc::sync_channel(1);

    std::thread::spawn(move || {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].session.workdir = missing_workdir.to_string_lossy().into_owned();
        let engram_mcp = engram_mcp_runtime_config_for_session_locked(&inner, &session_id);
        let result = state.start_turn_on_record(
            inner
                .session_mut_by_index(index)
                .expect("Claude session index should be valid"),
            "message-cold-claude-lock-safety".to_owned(),
            "Exercise cold Claude runtime startup.".to_owned(),
            Vec::new(),
            None,
            None,
            engram_mcp,
            false,
        );
        let outcome = match result {
            Ok(_) => {
                Err("cold Claude startup unexpectedly succeeded in a missing workdir".to_owned())
            }
            Err(error) => Ok(error.message),
        };
        let _ = result_tx.send(outcome);
    });

    let error = recv_within_guard(
        &result_rx,
        "cold Claude startup deadlocked while composing Engram MCP configuration",
    )
    .expect("cold Claude startup deadlocked while composing Engram MCP configuration")
    .expect("cold Claude startup should fail after lock-safe MCP composition");
    assert!(
        error.starts_with("failed to start persistent Claude session:"),
        "unexpected cold Claude startup error: {error}"
    );
}

#[test]
fn claude_private_mcp_config_file_carries_only_non_secret_engram_context() {
    let (state, _runtime_rx) =
        test_app_state_with_delegation_codex_runtime("engram-claude-private-mcp-file");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-claude-private-mcp-file-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram Claude private MCP file");
    set_test_project_engram_mcp_settings(&state, &project_id, true, Some("operator-secret-grant"));
    let claude_session = create_test_project_session(&state, Agent::Claude, &project_id, &root);

    let config_json = state
        .termal_delegation_mcp_claude_config_json(&claude_session)
        .expect("Claude MCP config should compose");
    let dir = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("delegations")
        .join("mcp");
    let guard = write_private_claude_mcp_config(&dir, "runtime-claude", &config_json)
        .expect("the private MCP configuration should be written");

    // What reaches Claude's argv is only the path.
    let argv_value = guard.path.to_string_lossy().into_owned();
    assert!(!argv_value.contains("operator-secret-grant"));
    assert!(!argv_value.contains("mcpServers"));

    // The file carries the four base-tier context values and no authority
    // credential anywhere.
    let stored: Value = serde_json::from_str(
        &fs::read_to_string(&guard.path).expect("the file should be readable"),
    )
    .expect("the file should be JSON");
    assert_eq!(
        stored["mcpServers"]["engram"]["env"],
        json!({
            "ENGRAM_ACTOR_CONTEXT": "agent=claude;model=default",
            "ENGRAM_ACTOR_ID": "dev/claude",
            "ENGRAM_HOME": "C:/engram-home",
            "ENGRAM_SESSION_ID": claude_session,
        })
    );
    for (name, server) in stored["mcpServers"]
        .as_object()
        .expect("mcpServers should be an object")
    {
        assert!(
            !server["args"].to_string().contains("operator-secret-grant"),
            "server `{name}` must not carry the grant in args"
        );
        assert!(
            !server["command"]
                .to_string()
                .contains("operator-secret-grant"),
            "server `{name}` must not carry the grant in command"
        );
    }
    assert!(
        !fs::read_to_string(&guard.path)
            .expect("the file should be readable")
            .contains("operator-secret-grant")
    );

    let path = guard.path.clone();
    drop(guard);
    assert!(!path.exists());

    // Legacy grant state does not affect the no-grant base environment.
    set_test_project_engram_mcp_settings(&state, &project_id, true, None);
    let grantless_json = state
        .termal_delegation_mcp_claude_config_json(&claude_session)
        .expect("grant-less Claude MCP config should compose");
    let stored: Value = serde_json::from_str(&grantless_json).expect("config should be JSON");
    assert_eq!(
        stored["mcpServers"]["engram"]["env"],
        json!({
            "ENGRAM_ACTOR_CONTEXT": "agent=claude;model=default",
            "ENGRAM_ACTOR_ID": "dev/claude",
            "ENGRAM_HOME": "C:/engram-home",
            "ENGRAM_SESSION_ID": claude_session,
        })
    );
    assert!(!grantless_json.contains("operator-secret-grant"));
}

#[test]
fn per_session_engram_mcp_uses_base_context_and_preserves_ineligible_baselines() {
    let (state, _runtime_rx) =
        test_app_state_with_delegation_codex_runtime("engram-per-session-mcp-baselines");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-per-session-mcp-baselines-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram MCP baseline project");
    set_test_project_engram_mcp_settings(&state, &project_id, true, None);
    let enabled_session = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    let enabled = state
        .termal_delegation_mcp_codex_config(&enabled_session)
        .expect("enabled Codex MCP config should compose");
    let args = enabled["mcp_servers"]["engram"]["args"]
        .as_array()
        .expect("Engram args should be an array");
    assert!(!args.iter().any(|arg| arg == "--work-authority-grant"));
    assert_eq!(
        enabled["mcp_servers"]["engram"]["env"],
        json!({
            "ENGRAM_ACTOR_CONTEXT": "agent=codex;model=gpt-5.4;reasoning=medium",
            "ENGRAM_ACTOR_ID": "dev/codex",
            "ENGRAM_HOME": "C:/engram-home",
            "ENGRAM_SESSION_ID": enabled_session,
        })
    );
    assert_eq!(
        enabled["shell_environment_policy"]["set"],
        Value::Object(expected_codex_shell_environment_set(
            &enabled,
            &enabled_session
        )),
        "enabled Codex shell policy must contain exactly seven owned identity values"
    );

    set_test_project_engram_mcp_settings(&state, &project_id, false, None);
    assert_delegation_mcp_baseline_is_unchanged(&state, &enabled_session);

    let projectless_session = test_session_id(&state, Agent::Codex);
    assert_delegation_mcp_baseline_is_unchanged(&state, &projectless_session);

    set_test_project_engram_mcp_settings(&state, &project_id, true, None);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter_mut()
            .find(|record| record.session.id == enabled_session)
            .expect("enabled session should exist");
        record.remote_id = Some("remote-test".to_owned());
        record.remote_session_id = Some("remote-session".to_owned());
    }
    assert_delegation_mcp_baseline_is_unchanged(&state, &enabled_session);
}

#[test]
fn project_patch_round_trips_without_removed_work_authority_grant() {
    let (state, _runtime_rx) =
        test_app_state_with_delegation_codex_runtime("engram-authority-patch");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-authority-patch-project");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "fixture-ok\n").expect("fixture mode should write");
    install_fixture_engram_host_settings(&state, &root);
    let project_id = create_test_project(&state, &root, "Engram authority patch project");
    state
        .update_project_engram_settings(&project_id, real_fixture_engram_settings(&root))
        .expect("Engram settings should persist");

    let redacted_settings = serde_json::to_value(state.snapshot()).expect("state should serialize")
        ["projects"]
        .as_array()
        .and_then(|projects| projects.iter().find(|project| project["id"] == project_id))
        .and_then(|project| project.get("engram"))
        .cloned()
        .expect("client project should retain public Engram settings");
    assert!(redacted_settings.get("workAuthorityGrant").is_none());
    let request: UpdateProjectEngramSettingsRequest =
        serde_json::from_value(redacted_settings).expect("redacted PATCH should deserialize");
    state
        .patch_project_engram_settings(&project_id, request)
        .expect("public settings PATCH should round-trip");

    let persisted_after_redacted_patch = sqlite_metadata_state_value(&state.persistence_path);
    let persisted_engram = persisted_after_redacted_patch["projects"]
        .as_array()
        .and_then(|projects| projects.iter().find(|project| project["id"] == project_id))
        .and_then(|project| project.get("engram"))
        .expect("persisted project should retain Engram settings");
    assert!(persisted_engram.get("workAuthorityGrant").is_none());
}

fn slow_doctor_fixture_path() -> PathBuf {
    if cfg!(windows) {
        FsPath::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/tests/fixtures/engram-doctor-slow-fixture.ps1")
    } else {
        FsPath::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/tests/fixtures/engram-doctor-slow-fixture.sh")
    }
}

/// The expiry branch was unreachable from tests while the deadline was a
/// constant, so a five-second wall shipped unexercised and a real migrated
/// store found it first. This drives the branch directly.
///
/// WHAT THIS ASSERTS: that the expiry branch runs and reports the deadline it
/// actually enforced. The fixture outlasts the deadline and then succeeds with
/// a payload that DOES deserialize as EngramDoctorResult, so a regressed
/// deadline returns Ok and fails this test rather than merely returning late.
///
/// WHAT THIS DOES NOT ASSERT: that the child was killed or reaped. The expiry
/// branch returns the same error whether or not `kill` succeeded, so deleting
/// that call would leave this test green. The name says only what is checked.
#[test]
fn enablement_doctor_expiry_names_the_deadline_it_enforced() {
    let root = TestTempRoot::create("termal-engram-doctor-deadline");
    let project_file = root.path().join(".engram-project");
    fs::write(&project_file, "fixture-slow-doctor\n")
        .expect("fixture project declaration should be written");
    let home = root.path().join("home");
    fs::create_dir_all(&home).expect("fixture Engram home should exist");

    // Matched rather than `expect_err` so the doctor result needs no `Debug`:
    // the production type stays untouched by this test's convenience.
    let error = match run_engram_doctor_result_within(
        &slow_doctor_fixture_path(),
        &project_file,
        &home,
        root.path(),
        Duration::ZERO,
        &home,
    ) {
        Ok(_) => panic!("an already expired deadline must fail instead of awaiting the doctor"),
        Err(error) => error,
    };

    assert!(
        error
            .message
            .contains("exceeded the 0 second enablement deadline"),
        "the failure must name the deadline it actually enforced, not a constant: {}",
        error.message
    );
}

/// Makes the sibling test's discriminator a TESTED FACT rather than a comment:
/// the slow fixture's payload really does deserialize, so a regressed deadline
/// would return Ok there instead of the timeout error. Both reviewers of this
/// change pointed out that the earlier fixture emitted `{"healthy":true}`,
/// which cannot deserialize into EngramDoctorResult, so the claimed property
/// did not exist. Asserting it here is cheaper than trusting the comment.
#[test]
fn slow_doctor_fixture_payload_deserializes_when_the_deadline_is_generous() {
    let root = TestTempRoot::create("termal-engram-doctor-payload");
    let project_file = root.path().join(".engram-project");
    fs::write(&project_file, "fixture-slow-doctor\n")
        .expect("fixture project declaration should be written");
    let home = root.path().join("home");
    fs::create_dir_all(&home).expect("fixture Engram home should exist");

    let result = run_engram_doctor_result_within(
        &slow_doctor_fixture_path(),
        &project_file,
        &home,
        root.path(),
        DEADLOCK_GUARD,
        &home,
    )
    .unwrap_or_else(|error| {
        panic!(
            "the fixture payload must parse as a doctor result: {}",
            error.message
        )
    });

    assert!(result.healthy, "the fixture reports a healthy store");
    assert_eq!(result.project_id, "termal-doctor-fixture");
    assert!(
        result.database.is_absolute(),
        "an enablement caller requires an absolute database path: {}",
        result.database.display()
    );
}

#[test]
fn doctor_output_has_an_independent_limit_and_diagnostic() {
    let above_control = vec![b'x'; ENGRAM_CONTROL_MAX_FRAME_BYTES + 1];
    assert_eq!(
        read_engram_doctor_output(above_control.as_slice()).unwrap(),
        above_control
    );
    let at_limit = vec![b'x'; ENGRAM_DOCTOR_MAX_OUTPUT_BYTES];
    assert_eq!(
        read_engram_doctor_output(at_limit.as_slice())
            .unwrap()
            .len(),
        at_limit.len()
    );
    let oversized = vec![b'x'; ENGRAM_DOCTOR_MAX_OUTPUT_BYTES + 1];
    let error = read_engram_doctor_output(oversized.as_slice()).unwrap_err();
    assert!(error.to_string().contains("Engram doctor output exceeds"));
    assert!(!error.to_string().contains("control frame"));
}

#[test]
fn doctor_output_collection_deadline_does_not_require_eof() {
    let (_sender, receiver) = mpsc::channel();
    let started = std::time::Instant::now();
    let error = collect_engram_doctor_output(receiver, "stderr", started).unwrap_err();
    assert!(error.message.contains("output collection exceeded"));
    assert!(started.elapsed() < DEADLOCK_GUARD);
}

#[path = "engram_doctor_deadline.rs"]
mod doctor_deadline;

fn large_doctor_fixture_path() -> PathBuf {
    if cfg!(windows) {
        FsPath::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/tests/fixtures/engram-doctor-large-fixture.ps1")
    } else {
        FsPath::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/tests/fixtures/engram-doctor-large-fixture.sh")
    }
}

/// Distinguishes the old doctor transport from the corrected one, which is the
/// whole point: the fixture writes about 120 KB — far above the pipe buffer
/// measured on this platform, where a child writing 65,536 bytes exits and one
/// writing 100,000 does not — and then exits successfully.
///
/// Under the previous shape (poll for exit, read only afterwards) this child
/// blocked on its own write, never exited, and the call ended at the deadline
/// reporting a slow audit. With both streams drained on their own threads it
/// completes and parses. So a regression to the old ordering turns this Ok back
/// into the timeout error, which no size-independent test would catch.
#[test]
fn doctor_transport_drains_a_report_larger_than_the_pipe_buffer() {
    let root = TestTempRoot::create("termal-engram-doctor-large");
    let project_file = root.path().join(".engram-project");
    fs::write(&project_file, "fixture-large-doctor\n")
        .expect("fixture project declaration should be written");
    let home = root.path().join("home");
    fs::create_dir_all(&home).expect("fixture Engram home should exist");

    let result = run_engram_doctor_result_within(
        &large_doctor_fixture_path(),
        &project_file,
        &home,
        root.path(),
        DEADLOCK_GUARD,
        &home,
    )
    .unwrap_or_else(|error| {
        panic!(
            "a report larger than the pipe buffer must still be collected: {}",
            error.message
        )
    });

    assert!(result.healthy);
    assert_eq!(result.project_id, "termal-doctor-large");
}
#[test]
fn engram_mcp_disable_immediately_tears_down_all_runtime_families_and_descendants() {
    let (state, root, project_id, session_ids) =
        ordinary_runtime_family_fixture("disable-all-families");
    let settings = EngramProjectSettings::default();

    state
        .update_project_engram_settings(&project_id, settings)
        .expect("disable should persist and revoke runtimes");

    let descendant_session_id = session_ids
        .last()
        .expect("fixture should include a descendant")
        .clone();
    let inner = state.inner.lock().expect("state mutex poisoned");
    for session_id in session_ids {
        let record = inner
            .find_session_index(&session_id)
            .and_then(|index| inner.sessions.get(index))
            .expect("affected session should remain");
        assert!(matches!(record.runtime, SessionRuntime::None));
        assert!(!record.runtime_reset_required);
    }
    let delegation = inner
        .delegations
        .iter()
        .find(|delegation| delegation.child_session_id == descendant_session_id)
        .expect("descendant delegation should remain tracked");
    assert_ne!(
        delegation.status,
        DelegationStatus::Running,
        "revocation should refresh the descendant delegation and release waits"
    );
}

#[test]
fn engram_mcp_disable_serializes_with_runtime_exit_waiter() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-mcp-revoke-waiter");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "fixture-ready\n").expect("fixture mode should write");
    let project_id = create_test_project(&state, &root, "Engram MCP revoke waiter");
    let mut enabled = real_fixture_engram_settings(&root);
    state
        .update_project_engram_settings(&project_id, enabled)
        .expect("fixture settings should enable Engram");
    let session_id = create_test_project_session(&state, Agent::Claude, &project_id, &root);

    let process_owner = phase_sync::ParkedProcess::spawn();
    let process = process_owner.process.clone();
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime_id = "engram-mcp-revoke-waiter-runtime".to_owned();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("test session should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("test session index should be valid");
        record.runtime = SessionRuntime::Claude(ClaudeRuntimeHandle {
            runtime_id: runtime_id.clone(),
            input_tx,
            #[cfg(windows)]
            process_tree: None,
            process: process.clone(),
        });
        record.session.status = SessionStatus::Active;
        record.session.preview = "Streaming reply...".to_owned();
        state
            .commit_locked(&mut inner)
            .expect("active runtime should persist");
    }
    record_test_project_runtime_descriptors(&state, &project_id);

    let waiter_state = state.clone();
    let waiter_session_id = session_id.clone();
    let waiter_process = process.clone();
    let waiter_runtime_id = runtime_id.clone();
    let (waiter_done_tx, waiter_done_rx) = mpsc::sync_channel(1);
    let waiter = std::thread::spawn(move || {
        let status = waiter_process.wait().expect("revoked process should exit");
        waiter_state
            .handle_runtime_exit_if_matches(
                &waiter_session_id,
                &RuntimeToken::Claude(waiter_runtime_id),
                (!status.success())
                    .then(|| format!("Claude session exited with status {status}"))
                    .as_deref(),
            )
            .expect("waiter callback should be fenced or become stale");
        let _ = waiter_done_tx.send(());
    });

    state
        .update_project_engram_settings(&project_id, EngramProjectSettings::default())
        .expect("disable should revoke the active runtime");
    let waiter_started = std::time::Instant::now();
    if waiter_done_rx.recv_timeout(DEADLOCK_GUARD).is_err() {
        let _ = process.kill();
        let _ = process.wait();
        panic!(
            "revoked runtime waiter completion phase failed after {:?}",
            waiter_started.elapsed()
        );
    }
    waiter.join().expect("runtime waiter should finish");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .find_session_index(&session_id)
        .and_then(|index| inner.sessions.get(index))
        .expect("revoked session should remain");
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert_eq!(
        record.session.preview,
        "Turn stopped: Engram MCP configuration was revoked."
    );
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.runtime_stop_in_progress);
    assert!(record.deferred_stop_callbacks.is_empty());
    assert!(!record.session.messages.iter().any(|message| {
        matches!(message, Message::Text { text, .. } if text.starts_with("Turn failed:"))
    }));
}

#[test]
fn engram_mcp_disable_gracefully_cancels_opencode_before_teardown() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-mcp-revoke-opencode");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "fixture-ready\n").expect("fixture mode should write");
    let project_id = create_test_project(&state, &root, "Engram MCP revoke OpenCode");
    let enabled = real_fixture_engram_settings(&root);
    state
        .update_project_engram_settings(&project_id, enabled)
        .expect("fixture settings should enable Engram");
    let session_id = create_test_project_session(&state, Agent::OpenCode, &project_id, &root);
    assert!(state.refresh_engram_project_declaration_for_session_off_lock(&session_id));
    let process_owner = phase_sync::ParkedProcess::spawn();
    let process = process_owner.process.clone();
    let (input_tx, input_rx) = mpsc::channel();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("OpenCode session should exist");
        let descriptor = engram_mcp_runtime_config_for_session_locked(&inner, &session_id)
            .expect("the enabled declared project has an MCP configuration")
            .installed;
        let record = inner
            .session_mut_by_index(index)
            .expect("OpenCode session index should be valid");
        record.runtime = SessionRuntime::Acp(AcpRuntimeHandle {
            agent: AcpAgent::OpenCode,
            runtime_id: "engram-mcp-revoke-opencode-runtime".to_owned(),
            input_tx,
            #[cfg(windows)]
            process_tree: None,
            process: process.clone(),
            turn_lifecycle: Arc::new((Mutex::new(false), Condvar::new())),
        });
        record.engram_mcp_installed = Some(descriptor);
        record.session.status = SessionStatus::Active;
        state
            .commit_locked(&mut inner)
            .expect("active OpenCode runtime should persist");
    }

    state
        .update_project_engram_settings(&project_id, EngramProjectSettings::default())
        .expect("disable should revoke OpenCode cleanly");

    assert!(matches!(
        recv_within_guard(
            &input_rx,
            "OpenCode revocation should queue graceful cancellation"
        )
        .expect("OpenCode revocation should queue graceful cancellation"),
        AcpRuntimeCommand::Cancel
    ));
    process
        .wait()
        .expect("revoked OpenCode process should exit");
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .find_session_index(&session_id)
        .and_then(|index| inner.sessions.get(index))
        .expect("OpenCode session should remain");
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.runtime_stop_in_progress);
}

#[test]
fn engram_mcp_disable_defers_behind_existing_stop_owner_without_waiting() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-mcp-revoke-stop-owner");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "fixture-ready\n").expect("fixture mode should write");
    let project_id = create_test_project(&state, &root, "Engram MCP revoke stop owner");
    let mut enabled = real_fixture_engram_settings(&root);
    state
        .update_project_engram_settings(&project_id, enabled)
        .expect("fixture settings should enable Engram");
    let session_id = create_test_project_session(&state, Agent::Claude, &project_id, &root);
    let process_owner = phase_sync::ParkedProcess::spawn();
    let process = process_owner.process.clone();
    let (input_tx, _input_rx) = mpsc::channel();
    let stop_owner_generation = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("test session should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("test session index should be valid");
        record.runtime = SessionRuntime::Claude(ClaudeRuntimeHandle {
            runtime_id: "engram-mcp-revoke-stop-owner-runtime".to_owned(),
            input_tx,
            #[cfg(windows)]
            process_tree: None,
            process: process.clone(),
        });
        record.session.status = SessionStatus::Active;
        let token = record
            .runtime
            .runtime_token()
            .expect("test Stop runtime should have a token");
        record.claim_runtime_stop(RuntimeStopOwnerKind::UserStop, token)
    };

    record_test_project_runtime_descriptors(&state, &project_id);
    let error = state
        .update_project_engram_settings(&project_id, EngramProjectSettings::default())
        .err()
        .expect("pending cleanup must be visible after settings persist");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error
            .message
            .contains("runtime cleanup is pending behind an existing Stop")
    );

    let target = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("test session should remain");
        assert!(inner.sessions[index].runtime_stop_in_progress);
        assert!(inner.sessions[index].engram_mcp_revocation_pending);
        take_pending_engram_mcp_revocation_after_stop_failure_locked(
            &mut inner,
            index,
            stop_owner_generation,
            StopSessionOptions::default(),
        )
        .expect("failed Stop should transfer its fence to revocation")
    };

    state
        .teardown_revoked_engram_mcp_runtimes(
            EngramMcpRuntimeRevocationBatch {
                targets: vec![target],
                pending_session_ids: Vec::new(),
                newly_pending_session_ids: Vec::new(),
            },
            "test pending revocation",
        )
        .expect("transferred revocation should tear down the old runtime");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .find_session_index(&session_id)
        .and_then(|index| inner.sessions.get(index))
        .expect("revoked session should remain");
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.runtime_stop_in_progress);
}

#[test]
fn engram_mcp_pending_revocation_completes_failed_stop_without_resuming_automatic_work() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-mcp-stop-transfer");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "fixture-ready\n").expect("fixture mode should write");
    let project_id = create_test_project(&state, &root, "Engram MCP Stop transfer");
    let mut enabled = real_fixture_engram_settings(&root);
    let mut cleared = enabled.clone();
    cleared.enabled = false;
    state
        .update_project_engram_settings(&project_id, enabled)
        .expect("fixture settings should enable Engram");
    let session_id = create_test_project_session(&state, Agent::Claude, &project_id, &root);
    let process_owner = phase_sync::ParkedProcess::spawn();
    let process = process_owner.process.clone();
    let (input_tx, _input_rx) = mpsc::channel();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("test session should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("test session index should be valid");
        record.runtime = SessionRuntime::Claude(ClaudeRuntimeHandle {
            runtime_id: "engram-mcp-stop-transfer-runtime".to_owned(),
            input_tx,
            #[cfg(windows)]
            process_tree: None,
            process: process.clone(),
        });
        record.session.status = SessionStatus::Active;
        // A Stop that the revocation completes consumes the session's run
        // waits, and only its own (docs/features/test-runs.md).
        *inner.test_run_waits = vec![
            test_run_wait_record("test-run-wait-stopped-session", &session_id),
            test_run_wait_record("test-run-wait-other-session", "session-other"),
        ];
        state
            .commit_locked(&mut inner)
            .expect("active runtime should persist");
    }
    record_test_project_runtime_descriptors(&state, &project_id);
    queue_test_engram_prompt(
        &state,
        &session_id,
        "durable user follow-up",
        QueuedPromptSource::User,
        None,
    );
    queue_test_engram_prompt(
        &state,
        &session_id,
        "automatic orchestrator continuation",
        QueuedPromptSource::Orchestrator,
        None,
    );

    record_test_project_runtime_descriptors(&state, &project_id);
    let stop_gate = install_test_stop_fence_gate(&state, &session_id);
    record_test_project_runtime_descriptors(&state, &project_id);
    let failure_guard = force_test_kill_child_process_failure_once(&process, "Claude");
    let stop_state = state.clone();
    let stop_session_id = session_id.clone();
    let stop_thread = std::thread::spawn(move || stop_state.stop_session(&stop_session_id));
    stop_gate.wait_until_claimed();

    record_test_project_runtime_descriptors(&state, &project_id);
    let error = state
        .update_project_engram_settings(&project_id, cleared)
        .err()
        .expect("pending cleanup must be reported");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error
            .message
            .contains("runtime cleanup is pending behind an existing Stop")
    );
    stop_gate.release();
    stop_thread
        .join()
        .expect("Stop thread should finish")
        .expect("successful transferred revocation should satisfy Stop");
    drop(failure_guard);

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .find_session_index(&session_id)
        .and_then(|index| inner.sessions.get(index))
        .expect("stopped session should remain");
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.runtime_stop_in_progress);
    assert!(record.runtime_stop_owner.is_none());
    assert!(!record.engram_mcp_revocation_pending);
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(
        record.queued_prompts.front().map(|queued| queued.source),
        Some(QueuedPromptSource::User)
    );
    let waits: Vec<&str> = inner
        .test_run_waits
        .iter()
        .map(|wait| wait.id.as_str())
        .collect();
    assert_eq!(waits, vec!["test-run-wait-other-session"]);
}

#[test]
fn engram_mcp_pending_revocation_surfaces_shared_codex_stop_interrupt_failure() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-mcp-shared-stop-transfer");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "fixture-ready\n").expect("fixture mode should write");
    let project_id = create_test_project(&state, &root, "Engram shared Codex Stop transfer");
    let mut enabled = real_fixture_engram_settings(&root);
    let mut cleared = enabled.clone();
    cleared.enabled = false;
    state
        .update_project_engram_settings(&project_id, enabled)
        .expect("fixture settings should enable Engram");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);

    let process_owner = phase_sync::ParkedProcess::spawn();
    let process = process_owner.process.clone();
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = SharedCodexRuntime {
        runtime_id: "engram-shared-stop-transfer".to_owned(),
        input_tx,
        process: process.clone(),
        sessions: SharedCodexSessions::new(),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
        stdout_activity: Arc::new(Mutex::new(std::time::Instant::now())),
    };
    install_single_wire_codex_fixture(&state, Some(runtime.clone()));
    runtime
        .sessions
        .lock()
        .expect("shared Codex sessions mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("engram-stop-transfer-thread".to_owned()),
                turn_id: Some("engram-stop-transfer-turn".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread map mutex poisoned")
        .insert("engram-stop-transfer-thread".to_owned(), session_id.clone());
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("test session should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("test session index should be valid");
        record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        record.session.status = SessionStatus::Active;
        record.session.preview = "Streaming reply...".to_owned();
        set_record_external_session_id(record, Some("engram-stop-transfer-thread".to_owned()));
        state
            .commit_locked(&mut inner)
            .expect("active shared runtime should persist");
    }
    record_test_project_runtime_descriptors(&state, &project_id);

    let responder = std::thread::spawn(move || {
        // Stop remains fenced while the settings subprocess runs; observe the
        // interrupt publication, not elapsed time spent in that earlier phase.
        match recv_within_guard(&input_rx, "shared Codex interrupt after settings fence")
            .expect("shared Codex interrupt should arrive")
        {
            CodexRuntimeCommand::InterruptTurn { response_tx, .. } => response_tx
                .send(Err("interrupt rejected during revocation".to_owned()))
                .expect("Stop should still await the interrupt response"),
            _ => panic!("expected shared Codex interrupt command"),
        }
    });
    record_test_project_runtime_descriptors(&state, &project_id);
    let stop_gate = install_test_stop_fence_gate(&state, &session_id);
    let stop_state = state.clone();
    let stop_session_id = session_id.clone();
    let stop_thread = std::thread::spawn(move || stop_state.stop_session(&stop_session_id));
    stop_gate.wait_until_claimed();

    record_test_project_runtime_descriptors(&state, &project_id);
    let error = state
        .update_project_engram_settings(&project_id, cleared)
        .err()
        .expect("pending cleanup must be reported");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error
            .message
            .contains("runtime cleanup is pending behind an existing Stop")
    );
    stop_gate.release();
    let error = match stop_thread.join().expect("Stop thread should finish") {
        Ok(_) => panic!("failed shared Codex interrupt must remain visible"),
        Err(error) => error,
    };
    responder.join().expect("interrupt responder should finish");
    assert!(
        error
            .message
            .contains("shared Codex interrupt failed after detach"),
        "unexpected Stop/revocation error: {}",
        error.message
    );

    assert!(
        !runtime
            .sessions
            .lock()
            .expect("shared Codex sessions mutex poisoned")
            .contains_key(&session_id)
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .find_session_index(&session_id)
        .and_then(|index| inner.sessions.get(index))
        .expect("revoked session should remain");
    assert_eq!(record.session.status, SessionStatus::Error);
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.runtime_stop_in_progress);
    assert!(!record.engram_mcp_revocation_pending);
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(record.external_session_id.is_none());
    assert!(
        inner
            .ignored_discovered_codex_thread_ids
            .contains("engram-stop-transfer-thread")
    );
    drop(inner);
    assert!(
        process
            .try_wait()
            .expect("shared process status should remain observable")
            .is_none(),
        "a failed thread interrupt must not kill the shared app-server"
    );
    process.kill().expect("shared process should clean up");
    process.wait().expect("shared process should exit");
}

#[test]
fn engram_mcp_revocation_fence_queues_dispatch_and_token_mismatch_releases_cleanly() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (old_runtime, _old_input_rx) = test_claude_runtime_handle("engram-revoke-old");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("test session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(old_runtime);
        inner.sessions[index].session.status = SessionStatus::Idle;
    }

    let mut batch = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        claim_engram_mcp_runtime_revocations_locked(&mut inner, std::slice::from_ref(&session_id))
    };
    assert!(batch.pending_session_ids.is_empty());
    let target = batch
        .targets
        .pop()
        .expect("old runtime should be captured under the fence");

    assert!(matches!(
        state
            .dispatch_turn(
                &session_id,
                SendMessageRequest {
                    text: "queue while revocation owns the runtime".to_owned(),
                    expanded_text: None,
                    attachments: Vec::new(),
                    source_session_id: None,
                    source_mailbox: None,
                },
            )
            .expect("dispatch under a revocation fence should queue"),
        DispatchTurnResult::Queued
    ));
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .find_session_index(&session_id)
            .and_then(|index| inner.sessions.get(index))
            .expect("test session should remain");
        assert!(record.runtime_stop_in_progress);
        assert!(record.runtime.matches_runtime_token(&target.token));
        assert_eq!(record.queued_prompts.len(), 1);
    }

    let (new_runtime, _new_input_rx) = test_claude_runtime_handle("engram-revoke-new");
    let new_token = RuntimeToken::Claude(new_runtime.runtime_id.clone());
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("test session should remain");
        let record = inner
            .session_mut_by_index(index)
            .expect("test session index should be valid");
        record.queued_prompts.clear();
        sync_pending_prompts(record);
        record.runtime = SessionRuntime::Claude(new_runtime);
        record.session.status = SessionStatus::Active;
        record
            .deferred_stop_callbacks
            .push(DeferredStopCallback::TurnCompleted {
                active_turn_generation: record.active_turn_generation,
            });
    }

    let finalization = state.finish_revoked_engram_mcp_runtime_if_matches(
        &session_id,
        &target.token,
        target.owner_generation,
        target.stop_options.as_ref(),
        false,
        false,
        None,
    );
    assert!(
        finalization.failures.is_empty(),
        "stale revocation finish should release its fence: {:?}",
        finalization.failures
    );
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .find_session_index(&session_id)
            .and_then(|index| inner.sessions.get(index))
            .expect("successor session should remain");
        assert!(record.runtime.matches_runtime_token(&new_token));
        assert!(!record.runtime_stop_in_progress);
        assert!(record.deferred_stop_callbacks.is_empty());
    }

    state
        .finish_turn_ok_if_runtime_matches(&session_id, &new_token)
        .expect("successor callback should no longer be deferred");
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .find_session_index(&session_id)
        .and_then(|index| inner.sessions.get(index))
        .expect("successor session should remain");
    assert_eq!(record.session.status, SessionStatus::Idle);
}

#[test]
fn stale_engram_mcp_revocation_cannot_release_a_newer_owner_fence() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (old_runtime, _old_input_rx) = test_claude_runtime_handle("engram-owner-old");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("test session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(old_runtime);
        inner.sessions[index].session.status = SessionStatus::Idle;
    }
    let mut batch = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        claim_engram_mcp_runtime_revocations_locked(&mut inner, std::slice::from_ref(&session_id))
    };
    let stale_target = batch.targets.pop().expect("old runtime should be claimed");

    let (new_runtime, _new_input_rx) = test_claude_runtime_handle("engram-owner-new");
    let new_token = RuntimeToken::Claude(new_runtime.runtime_id.clone());
    let newer_generation = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("test session should remain");
        let record = inner
            .session_mut_by_index(index)
            .expect("test session index should be valid");
        record.clear_runtime_stop();
        record.runtime = SessionRuntime::Claude(new_runtime);
        record.session.status = SessionStatus::Active;
        let generation =
            record.claim_runtime_stop(RuntimeStopOwnerKind::EngramMcpRevocation, new_token.clone());
        record.engram_mcp_revocation_pending = true;
        record
            .deferred_stop_callbacks
            .push(DeferredStopCallback::TurnCompleted {
                active_turn_generation: record.active_turn_generation,
            });
        generation
    };

    let finalization = state.finish_revoked_engram_mcp_runtime_if_matches(
        &session_id,
        &stale_target.token,
        stale_target.owner_generation,
        stale_target.stop_options.as_ref(),
        false,
        false,
        None,
    );
    assert!(
        finalization.failures.is_empty(),
        "stale completion should no-op against a newer owner: {:?}",
        finalization.failures
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .find_session_index(&session_id)
        .and_then(|index| inner.sessions.get(index))
        .expect("successor session should remain");
    assert!(record.runtime.matches_runtime_token(&new_token));
    assert!(record.runtime_stop_in_progress);
    assert!(record.runtime_stop_is_owned_by(
        RuntimeStopOwnerKind::EngramMcpRevocation,
        &new_token,
        newer_generation,
    ));
    assert!(record.engram_mcp_revocation_pending);
    assert_eq!(
        record.deferred_stop_callbacks,
        vec![DeferredStopCallback::TurnCompleted {
            active_turn_generation: record.active_turn_generation,
        }]
    );
    drop(inner);
}

#[test]
fn engram_mcp_revocation_reclaims_an_unowned_fence_for_the_same_runtime() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, _input_rx) = test_claude_runtime_handle("engram-reclaim-unowned");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("test session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    let mut batch = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        claim_engram_mcp_runtime_revocations_locked(&mut inner, std::slice::from_ref(&session_id))
    };
    let target = batch
        .targets
        .pop()
        .expect("runtime should be captured under a revocation fence");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("test session should remain");
        let record = inner
            .session_mut_by_index(index)
            .expect("test session index should be valid");
        record.clear_runtime_stop();
        record.engram_mcp_revocation_pending = true;
    }

    let finalization = state.finish_revoked_engram_mcp_runtime_if_matches(
        &session_id,
        &target.token,
        target.owner_generation,
        target.stop_options.as_ref(),
        false,
        false,
        None,
    );
    assert!(finalization.failures.is_empty());
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .find_session_index(&session_id)
        .and_then(|index| inner.sessions.get(index))
        .expect("test session should remain");
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.runtime_stop_in_progress);
    assert!(record.runtime_stop_owner.is_none());
    assert!(!record.engram_mcp_revocation_pending);
    drop(inner);
}

#[test]
fn engram_mcp_disable_dispatches_queued_prompt_with_fresh_runtime() {
    let (state, runtime_rx) =
        test_app_state_with_delegation_codex_runtime("engram-mcp-revoke-queue");
    let transport = ScriptedEngramControlTransport::new([]);
    state.install_control_test_transport(transport.clone());
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-mcp-revoke-queue");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "fixture-ready\n").expect("fixture mode should write");
    let project_id = create_test_project(&state, &root, "Engram MCP revoke queue");
    let mut enabled = real_fixture_engram_settings(&root);
    state
        .update_project_engram_settings(&project_id, enabled)
        .expect("fixture settings should enable Engram");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    attach_engram_mcp_test_runtime(&state, &session_id);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("test session should exist");
        inner
            .session_mut_by_index(index)
            .expect("test session index should be valid")
            .session
            .status = SessionStatus::Active;
        state
            .commit_locked(&mut inner)
            .expect("active session should persist");
    }
    record_test_project_runtime_descriptors(&state, &project_id);
    queue_test_engram_prompt(
        &state,
        &session_id,
        "run after revocation",
        QueuedPromptSource::User,
        None,
    );

    state
        .update_project_engram_settings(&project_id, EngramProjectSettings::default())
        .expect("disable should revoke and continue queued work");

    assert!(matches!(
        recv_within_guard(&runtime_rx, "fresh runtime should receive queued prompt")
            .expect("fresh runtime should receive queued prompt"),
        CodexRuntimeCommand::Prompt { .. }
    ));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .find_session_index(&session_id)
        .and_then(|index| inner.sessions.get(index))
        .expect("queued session should remain");
    assert_eq!(record.session.status, SessionStatus::Active);
    assert!(record.queued_prompts.is_empty());
    assert!(record.session.pending_prompts.is_empty());
    assert!(!record.runtime_stop_in_progress);
    assert_no_admission_or_native_credit_requests(&transport);
}

#[test]
fn engram_mcp_disable_surfaces_shutdown_failure_and_blocks_resume() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-mcp-revoke-shutdown-failure");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "fixture-ready\n").expect("fixture mode should write");
    let project_id = create_test_project(&state, &root, "Engram MCP revoke failure");
    let mut enabled = real_fixture_engram_settings(&root);
    state
        .update_project_engram_settings(&project_id, enabled)
        .expect("fixture settings should enable Engram");
    let session_id = create_test_project_session(&state, Agent::Claude, &project_id, &root);
    let process_owner = phase_sync::ParkedProcess::spawn();
    let process = process_owner.process.clone();
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime_token = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("test session should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("test session index should be valid");
        let runtime_id = "engram-mcp-revoke-failure-runtime".to_owned();
        record.runtime = SessionRuntime::Claude(ClaudeRuntimeHandle {
            runtime_id: runtime_id.clone(),
            input_tx,
            #[cfg(windows)]
            process_tree: None,
            process: process.clone(),
        });
        record.session.status = SessionStatus::Active;
        record.session.preview = "Streaming reply...".to_owned();
        state
            .commit_locked(&mut inner)
            .expect("active runtime should persist");
        RuntimeToken::Claude(runtime_id)
    };
    queue_test_engram_prompt(
        &state,
        &session_id,
        "remain paused after degraded revocation",
        QueuedPromptSource::User,
        None,
    );

    record_test_project_runtime_descriptors(&state, &project_id);
    let failure_guard = force_test_kill_child_process_failure(&process, "Claude");
    let error =
        match state.update_project_engram_settings(&project_id, EngramProjectSettings::default()) {
            Ok(_) => panic!("failed runtime shutdown must be visible to the caller"),
            Err(error) => error,
        };
    assert!(
        error.message.contains("runtime cleanup was degraded"),
        "unexpected revocation error: {}",
        error.message
    );

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .find_session_index(&session_id)
            .and_then(|index| inner.sessions.get(index))
            .expect("revoked session should remain");
        assert_eq!(record.session.status, SessionStatus::Error);
        assert!(record.runtime.matches_runtime_token(&runtime_token));
        assert!(record.runtime_reset_required);
        assert!(record.engram_mcp_runtime_quarantined);
        assert!(!record.runtime_stop_in_progress);
        assert!(record.deferred_stop_callbacks.is_empty());
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(record.session.pending_prompts.len(), 1);
        assert!(
            record
                .session
                .preview
                .contains("Engram MCP configuration was revoked")
        );
        assert!(record.session.messages.iter().any(|message| {
            matches!(message, Message::Text { text, .. } if text.contains("could not be stopped cleanly"))
        }));
        let project = inner
            .find_project(&project_id)
            .expect("project should remain after disable");
        assert_eq!(
            project.engram.as_ref().map(|settings| settings.enabled),
            Some(false),
            "disabled settings must remain durable even when process cleanup fails"
        );
    }

    let retry_error = match state.dispatch_turn(
        &session_id,
        SendMessageRequest {
            text: "retry cleanup from an explicit prompt".to_owned(),
            expanded_text: None,
            attachments: Vec::new(),
            source_session_id: None,
            source_mailbox: None,
        },
    ) {
        Ok(_) => panic!("a still-failing reset must reject explicit dispatch"),
        Err(error) => error,
    };
    assert!(
        retry_error
            .message
            .contains("failed to restart Claude session runtime"),
        "unexpected retained-runtime retry error: {}",
        retry_error.message
    );
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .find_session_index(&session_id)
            .and_then(|index| inner.sessions.get(index))
            .expect("failed retry should retain the revoked session");
        assert!(record.runtime.matches_runtime_token(&runtime_token));
        assert!(record.runtime_reset_required);
        assert!(record.engram_mcp_runtime_quarantined);
        assert!(record.orchestrator_auto_dispatch_blocked);
    }

    assert!(
        process
            .try_wait()
            .expect("failed child status should remain observable")
            .is_none(),
        "unconfirmed shutdown failure must retain the live process handle"
    );
    drop(failure_guard);
    process.kill().expect("test child should clean up");
    process.wait().expect("test child should exit");
    let message_count_after_degradation = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .find_session_index(&session_id)
            .and_then(|index| inner.sessions.get(index))
            .expect("revoked session should remain before the late exit");
        record.session.messages.len()
    };
    state
        .handle_runtime_exit_if_matches(
            &session_id,
            &runtime_token,
            Some("process exited after the quarantine was recorded"),
        )
        .expect("confirmed process exit should release the quarantined runtime");
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .find_session_index(&session_id)
        .and_then(|index| inner.sessions.get(index))
        .expect("revoked session should remain after process exit");
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.engram_mcp_runtime_quarantined);
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert_eq!(
        record.session.messages.len(),
        message_count_after_degradation
    );
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert_eq!(record.queued_prompts.len(), 2);
}

#[test]
fn engram_mcp_degraded_acp_runtime_is_replaced_by_an_explicit_prompt() {
    let state = test_app_state();
    let transport = ScriptedEngramControlTransport::new([]);
    state.install_control_test_transport(transport.clone());
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-mcp-revoke-acp-retry");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "fixture-ready\n").expect("fixture mode should write");
    let project_id = create_test_project(&state, &root, "Engram MCP ACP retry");
    let mut enabled = real_fixture_engram_settings(&root);
    state
        .update_project_engram_settings(&project_id, enabled)
        .expect("fixture settings should enable Engram");
    let session_id = create_test_project_session(&state, Agent::Cursor, &project_id, &root);
    let old_process_owner = phase_sync::ParkedProcess::spawn();
    let old_process = old_process_owner.process.clone();
    let (old_input_tx, _old_input_rx) = mpsc::channel();
    let old_runtime_id = "engram-mcp-revoke-cursor-old".to_owned();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Cursor session should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("Cursor session index should be valid");
        record.runtime = SessionRuntime::Acp(AcpRuntimeHandle {
            agent: AcpAgent::Cursor,
            runtime_id: old_runtime_id.clone(),
            input_tx: old_input_tx,
            #[cfg(windows)]
            process_tree: None,
            process: old_process.clone(),
            turn_lifecycle: Arc::new((Mutex::new(false), Condvar::new())),
        });
        record.session.status = SessionStatus::Active;
        state
            .commit_locked(&mut inner)
            .expect("old Cursor runtime should persist");
    }
    record_test_project_runtime_descriptors(&state, &project_id);

    record_test_project_runtime_descriptors(&state, &project_id);
    let failure_guard = force_test_kill_child_process_failure(&old_process, "Cursor");
    assert!(
        state
            .update_project_engram_settings(&project_id, EngramProjectSettings::default())
            .is_err(),
        "repeated Cursor shutdown failure should surface degradation"
    );
    drop(failure_guard);

    let fresh_process_owner = phase_sync::ParkedProcess::spawn();
    let fresh_process = fresh_process_owner.process.clone();
    let (fresh_input_tx, fresh_input_rx) = mpsc::channel();
    let fresh_runtime_id = "engram-mcp-revoke-cursor-fresh".to_owned();
    state.install_test_acp_runtime_override(
        AcpAgent::Cursor,
        AcpRuntimeHandle {
            agent: AcpAgent::Cursor,
            runtime_id: fresh_runtime_id.clone(),
            input_tx: fresh_input_tx,
            #[cfg(windows)]
            process_tree: None,
            process: fresh_process.clone(),
            turn_lifecycle: Arc::new((Mutex::new(false), Condvar::new())),
        },
    );

    let dispatch = match state
        .dispatch_turn(
            &session_id,
            SendMessageRequest {
                text: "replace the revoked runtime explicitly".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .expect("explicit prompt should retry cleanup and start fresh")
    {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("explicit recovery prompt should dispatch"),
    };
    deliver_turn_dispatch(&state, dispatch)
        .expect("explicit recovery prompt should reach the fresh runtime");
    assert!(matches!(
        recv_within_guard(
            &fresh_input_rx,
            "fresh Cursor runtime should receive the explicit prompt"
        )
        .expect("fresh Cursor runtime should receive the explicit prompt"),
        AcpRuntimeCommand::Prompt(_)
    ));
    old_process
        .wait()
        .expect("old Cursor process should be reaped after retry");
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .find_session_index(&session_id)
            .and_then(|index| inner.sessions.get(index))
            .expect("Cursor session should remain");
        assert!(
            record
                .runtime
                .matches_runtime_token(&RuntimeToken::Acp(fresh_runtime_id))
        );
        assert_eq!(record.session.status, SessionStatus::Active);
        assert!(!record.runtime_reset_required);
        assert!(!record.orchestrator_auto_dispatch_blocked);
        assert_no_admission_or_native_credit_requests(&transport);
    }
    fresh_process
        .kill()
        .expect("fresh Cursor process should clean up");
    fresh_process
        .wait()
        .expect("fresh Cursor process should be reaped");
}

#[test]
fn engram_mcp_mixed_cleanup_error_preserves_pending_session_observability() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-mcp-mixed-cleanup");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "fixture-ready\n").expect("fixture mode should write");
    let project_id = create_test_project(&state, &root, "Engram MCP mixed cleanup");
    let mut enabled = real_fixture_engram_settings(&root);
    state
        .update_project_engram_settings(&project_id, enabled)
        .expect("fixture settings should enable Engram");
    let pending_session_id = create_test_project_session(&state, Agent::Claude, &project_id, &root);
    let failing_session_id = create_test_project_session(&state, Agent::Claude, &project_id, &root);
    let pending_process_owner = phase_sync::ParkedProcess::spawn();
    let pending_process = pending_process_owner.process.clone();
    let failing_process_owner = phase_sync::ParkedProcess::spawn();
    let failing_process = failing_process_owner.process.clone();
    let (pending_input_tx, _pending_input_rx) = mpsc::channel();
    let (failing_input_tx, _failing_input_rx) = mpsc::channel();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        for (session_id, runtime_id, input_tx, process) in [
            (
                &pending_session_id,
                "engram-mixed-pending",
                pending_input_tx,
                pending_process.clone(),
            ),
            (
                &failing_session_id,
                "engram-mixed-failing",
                failing_input_tx,
                failing_process.clone(),
            ),
        ] {
            let index = inner
                .find_session_index(session_id)
                .expect("test session should exist");
            let record = inner
                .session_mut_by_index(index)
                .expect("test session index should be valid");
            record.runtime = SessionRuntime::Claude(ClaudeRuntimeHandle {
                runtime_id: runtime_id.to_owned(),
                input_tx,
                #[cfg(windows)]
                process_tree: None,
                process,
            });
            record.session.status = SessionStatus::Active;
        }
        let pending_index = inner
            .find_session_index(&pending_session_id)
            .expect("pending session should exist");
        let pending_record = inner
            .session_mut_by_index(pending_index)
            .expect("pending session index should be valid");
        let pending_token = pending_record
            .runtime
            .runtime_token()
            .expect("pending runtime should have a token");
        pending_record.claim_runtime_stop(RuntimeStopOwnerKind::UserStop, pending_token);
        state
            .commit_locked(&mut inner)
            .expect("mixed runtime state should persist");
    }

    record_test_project_runtime_descriptors(&state, &project_id);
    let failure_guard = force_test_kill_child_process_failure(&failing_process, "Claude");
    let error =
        match state.update_project_engram_settings(&project_id, EngramProjectSettings::default()) {
            Ok(_) => panic!("mixed cleanup degradation must be visible"),
            Err(error) => error,
        };
    assert!(
        error.message.contains(&pending_session_id),
        "cleanup error should retain pending ids: {}",
        error.message
    );
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let pending = inner
            .find_session_index(&pending_session_id)
            .and_then(|index| inner.sessions.get(index))
            .expect("pending session should remain");
        assert!(pending.runtime_stop_in_progress);
        assert!(pending.engram_mcp_revocation_pending);
        let failing = inner
            .find_session_index(&failing_session_id)
            .and_then(|index| inner.sessions.get(index))
            .expect("failing session should remain");
        assert_eq!(failing.session.status, SessionStatus::Error);
        assert!(failing.orchestrator_auto_dispatch_blocked);
        assert!(!matches!(failing.runtime, SessionRuntime::None));
        assert!(failing.runtime_reset_required);
    }
    drop(failure_guard);
    pending_process
        .kill()
        .expect("pending child should clean up");
    pending_process.wait().expect("pending child should exit");
    failing_process
        .kill()
        .expect("failing child should clean up");
    failing_process.wait().expect("failing child should exit");
}

#[test]
fn engram_mcp_shared_codex_interrupt_failure_detaches_thread_and_surfaces_degradation() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-mcp-shared-codex-interrupt-failure");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "fixture-ready\n").expect("fixture mode should write");
    let project_id = create_test_project(&state, &root, "Engram shared Codex revoke");
    let mut enabled = real_fixture_engram_settings(&root);
    state
        .update_project_engram_settings(&project_id, enabled)
        .expect("fixture settings should enable Engram");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);

    let process_owner = phase_sync::ParkedProcess::spawn();
    let process = process_owner.process.clone();
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = SharedCodexRuntime {
        runtime_id: "engram-shared-revoke".to_owned(),
        input_tx,
        process: process.clone(),
        sessions: SharedCodexSessions::new(),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
        stdout_activity: Arc::new(Mutex::new(std::time::Instant::now())),
    };
    install_single_wire_codex_fixture(&state, Some(runtime.clone()));
    runtime
        .sessions
        .lock()
        .expect("shared Codex sessions mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("engram-thread-old".to_owned()),
                turn_id: Some("engram-turn-old".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread map mutex poisoned")
        .insert("engram-thread-old".to_owned(), session_id.clone());
    drop(input_rx);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("test session should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("test session index should be valid");
        record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        record.session.status = SessionStatus::Active;
        record.session.preview = "Streaming reply...".to_owned();
        set_record_external_session_id(record, Some("engram-thread-old".to_owned()));
        state
            .commit_locked(&mut inner)
            .expect("active shared runtime should persist");
    }
    record_test_project_runtime_descriptors(&state, &project_id);

    let error =
        match state.update_project_engram_settings(&project_id, EngramProjectSettings::default()) {
            Ok(_) => panic!("unconfirmed shared interrupt should remain user-visible"),
            Err(error) => error,
        };
    assert!(
        error
            .message
            .contains("the old thread may remain alive with its prior MCP capabilities"),
        "unexpected cleanup error: {}",
        error.message
    );
    assert!(
        !error
            .message
            .contains("the revoked grant blocks further mutations"),
        "an unconfirmed shared-Codex interrupt must not claim fail-closed revocation"
    );

    assert!(
        !runtime
            .sessions
            .lock()
            .expect("shared Codex sessions mutex poisoned")
            .contains_key(&session_id),
        "revocation must detach the shared session even when interrupt delivery fails"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .find_session_index(&session_id)
        .and_then(|index| inner.sessions.get(index))
        .expect("revoked session should remain");
    assert_eq!(record.session.status, SessionStatus::Error);
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.runtime_stop_in_progress);
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(record.external_session_id.is_none());
    assert!(record.session.external_session_id.is_none());
    assert!(
        inner
            .ignored_discovered_codex_thread_ids
            .contains("engram-thread-old"),
        "the tainted shared thread must not be rediscovered"
    );
    assert!(
        process
            .try_wait()
            .expect("shared process status should remain observable")
            .is_none(),
        "revocation must not kill the shared app-server used by unrelated sessions"
    );
    assert!(
        state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned")
            .is_some(),
        "the shared runtime should remain available for unrelated sessions"
    );
    drop(inner);
    process.kill().expect("shared process should clean up");
    process.wait().expect("shared process should exit");
}

#[test]
fn engram_mcp_buffered_runtime_exit_avoids_quarantining_a_dead_runtime() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, _input_rx) = test_claude_runtime_handle("engram-buffered-exit");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("test session should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("test session index should be valid");
        record.runtime = SessionRuntime::Claude(runtime);
        record.session.status = SessionStatus::Active;
    }
    let mut batch = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        claim_engram_mcp_runtime_revocations_locked(&mut inner, std::slice::from_ref(&session_id))
    };
    let target = batch.targets.pop().expect("runtime should be claimed");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("test session should remain");
        let active_turn_generation = inner.sessions[index].active_turn_generation;
        inner.sessions[index]
            .deferred_stop_callbacks
            .push(DeferredStopCallback::RuntimeExited {
                active_turn_generation,
                message: None,
            });
    }

    let outcome =
        state.finalize_revoked_engram_mcp_runtimes(EngramMcpRuntimeRevocationShutdownBatch {
            shutdowns: vec![EngramMcpRuntimeRevocationShutdown {
                target,
                shutdown_error: Some("scripted kill failure before waiter exit".to_owned()),
                retain_runtime_for_retry: true,
                suppress_codex_thread_resume: false,
            }],
            pending_session_ids: Vec::new(),
        });
    assert!(
        outcome.failures.is_empty(),
        "the buffered exit confirms cleanup: {:?}",
        outcome.failures
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .find_session_index(&session_id)
        .and_then(|index| inner.sessions.get(index))
        .expect("test session should remain");
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.runtime_reset_required);
    assert!(!record.engram_mcp_runtime_quarantined);
    assert!(!record.runtime_stop_in_progress);
    assert!(record.deferred_stop_callbacks.is_empty());
    assert_eq!(record.session.status, SessionStatus::Idle);
    drop(inner);
}

fn assert_no_admission_or_native_credit_requests(transport: &ScriptedEngramControlTransport) {
    for request in transport.requests() {
        assert!(
            !matches!(
                request.request["operation"].as_str(),
                Some(
                    "session_bind"
                        | "session_status"
                        | "turn_grant_read"
                        | "turn_evaluate"
                        | "turn_begin"
                        | "turn_checkpoint"
                        | "verification_bind"
                        | "verification_record"
                )
            ),
            "ordinary adapter work must not acquire admission or native credit: {}",
            request.request
        );
    }
}

fn ordinary_work_connection(session_id: &str) -> EngramConnectionConfig {
    EngramConnectionConfig {
        binary_path: PathBuf::from("engram-fixture"),
        project_file: PathBuf::from("fixture/.engram-project"),
        home: PathBuf::from("fixture/home"),
        project_root: PathBuf::from("fixture"),
        actor_id: "dev/codex".to_owned(),
        actor_context: None,
        session_id: session_id.to_owned(),
    }
}

#[test]
fn ordinary_work_session_requires_exact_current_identity_and_explicit_support() {
    let connection = ordinary_work_connection("ordinary-session");
    let store = EngramAuthorityStoreKey {
        project_id: "ordinary-project".to_owned(),
        database_path: PathBuf::from("fixture/home/ordinary-project/engram.db"),
    };
    let valid = json!({"project_id": store.project_id, "session_id": connection.session_id,
        "work_session": true, "capabilities": ["host_work_v1"]});
    for answer in [
        json!({}),
        json!({"project_id": store.project_id, "session_id": "foreign-session",
            "work_session": true, "capabilities": ["host_work_v1"]}),
        json!({"project_id": "foreign-project", "session_id": connection.session_id,
            "work_session": true, "capabilities": ["host_work_v1"]}),
        json!({"project_id": store.project_id, "session_id": connection.session_id,
            "work_session": false, "capabilities": ["host_work_v1"]}),
        json!({"project_id": store.project_id, "session_id": connection.session_id,
            "work_session": true, "capabilities": []}),
    ] {
        let transport =
            ScriptedEngramControlTransport::new([ScriptedEngramControlResponse::Reply(Ok(answer))]);
        let adapter = EngramHostAdapter {
            host_workdir: PathBuf::from("fixture"),
            transport: transport.clone(),
            capabilities: Arc::default(),
        };
        assert!(
            adapter
                .read_work_session(&connection, &store, DEADLOCK_GUARD)
                .is_err()
        );
        assert_eq!(transport.requests().len(), 1);
        assert_no_admission_or_native_credit_requests(&transport);
    }
    let transport = ScriptedEngramControlTransport::new([ScriptedEngramControlResponse::Reply(
        Ok(valid.clone()),
    )]);
    let adapter = EngramHostAdapter {
        host_workdir: PathBuf::from("fixture"),
        transport: transport.clone(),
        capabilities: Arc::default(),
    };
    assert_eq!(
        adapter
            .read_work_session(&connection, &store, DEADLOCK_GUARD)
            .unwrap(),
        connection.session_id
    );
    assert_eq!(
        transport.requests().len(),
        1,
        "the scripted child has one stable current generation"
    );
    assert_no_admission_or_native_credit_requests(&transport);

    let exhausted =
        ScriptedEngramControlTransport::new([ScriptedEngramControlResponse::Reply(Ok(valid))]);
    let adapter = EngramHostAdapter {
        host_workdir: PathBuf::from("fixture"),
        transport: Arc::new(UnknownGenerationWorkTransport(exhausted.clone())),
        capabilities: Arc::default(),
    };
    assert!(
        adapter
            .read_work_session(&connection, &store, DEADLOCK_GUARD)
            .is_err(),
        "an exhausted current-service script is never an implicit positive answer"
    );
    assert_eq!(exhausted.requests().len(), 2);
}

#[test]
fn ordinary_turn_emits_observation_without_native_verification_credit() {
    let turn = EOCheckedTurn::start("generic-ordinary-observation", true);
    turn.run(
        "generic-ordinary-check",
        SIZE_TEST,
        EngramCommandExit::Code(0),
        || {},
    );
    let request = turn.finish();
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
    assert_ordinary_turn_has_no_native_requests(&turn);
}

#[test]
fn ordinary_user_mailbox_and_orchestrator_sources_deliver_once_without_admission() {
    let (state, runtime_rx) = test_app_state_with_delegation_codex_runtime("engram-source-kinds");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-source-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram source project");
    let parent_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    enable_test_project_engram(&state, &project_id, &root);

    let transport = ScriptedEngramControlTransport::new([]);
    state.install_control_test_transport(transport.clone());

    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Initial user source.".to_owned(),
                title: Some("Engram source kinds".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should start");
    let initial_prompt = build_delegation_prompt(&created.delegation);
    let child_id = created.delegation.child_session_id;
    assert!(matches!(
        receive_synchronous_engram_prompt(&state, &runtime_rx, "user prompt should reach runtime")
            .expect("user prompt should reach runtime"),
        CodexRuntimeCommand::Prompt { .. }
    ));
    let runtime_token = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .sessions
            .iter()
            .find(|record| record.session.id == child_id)
            .and_then(|record| record.runtime.runtime_token())
            .expect("child runtime should be active")
    };

    state
        .finish_turn_ok_if_runtime_matches(&child_id, &runtime_token)
        .expect("user turn should finish");
    queue_test_engram_prompt(
        &state,
        &child_id,
        "Mailbox source wake.",
        QueuedPromptSource::Mailbox,
        Some(MessageSource::mailbox(
            parent_session_id.clone(),
            "Source parent".to_owned(),
            MailboxMessageSource {
                mailbox_id: "mailbox-source".to_owned(),
                message_id: "mailbox-message-source".to_owned(),
                sequence: 1,
                unread_count: 1,
            },
        )),
    );
    let mailbox_dispatch = state
        .start_next_queued_turn_off_lock(&child_id, false, false)
        .expect("mailbox queue promotion should succeed")
        .expect("mailbox wake should dispatch")
        .dispatch;
    deliver_turn_dispatch(&state, mailbox_dispatch)
        .expect("mailbox prompt should be accepted by runtime");
    assert!(matches!(
        recv_within_guard(&runtime_rx, "mailbox prompt should reach runtime")
            .expect("mailbox prompt should reach runtime"),
        CodexRuntimeCommand::Prompt { .. }
    ));

    state
        .finish_turn_ok_if_runtime_matches(&child_id, &runtime_token)
        .expect("mailbox turn should finish");
    queue_test_engram_prompt(
        &state,
        &child_id,
        "Orchestrator source wake.",
        QueuedPromptSource::Orchestrator,
        None,
    );
    let orchestrator_dispatch = state
        .start_next_queued_turn_off_lock(&child_id, false, false)
        .expect("orchestrator queue promotion should succeed")
        .expect("orchestrator wake should dispatch")
        .dispatch;
    deliver_turn_dispatch(&state, orchestrator_dispatch)
        .expect("orchestrator prompt should be accepted by runtime");
    assert!(matches!(
        recv_within_guard(&runtime_rx, "orchestrator prompt should reach runtime")
            .expect("orchestrator prompt should reach runtime"),
        CodexRuntimeCommand::Prompt { .. }
    ));
    state
        .finish_turn_ok_if_runtime_matches(&child_id, &runtime_token)
        .expect("orchestrator turn should finish");

    assert_no_admission_or_native_credit_requests(&transport);
    assert!(
        matches!(runtime_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "each source must deliver exactly one provider command"
    );
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&child_id).unwrap()];
    for text in [
        initial_prompt.as_str(),
        "Mailbox source wake.",
        "Orchestrator source wake.",
    ] {
        assert_eq!(record.session.messages.iter().filter(|message|
            matches!(message, Message::Text { text: actual, .. } if actual == text)).count(), 1);
    }
    assert!(record.queued_prompts.is_empty());
}

#[test]
fn remote_proxy_dispatch_never_enters_the_local_engram_adapter() {
    let (state, runtime_rx) = test_app_state_with_delegation_codex_runtime("engram-s9");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-remote-proxy-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram remote proxy");
    let parent_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    enable_test_project_engram(&state, &project_id, &root);
    let transport = ScriptedEngramControlTransport::new([]);
    state.install_control_test_transport(transport.clone());

    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Create a local child before converting the test record to a proxy."
                    .to_owned(),
                title: Some("Engram S9".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should start");
    assert!(matches!(
        receive_synchronous_engram_prompt(
            &state,
            &runtime_rx,
            "runtime should receive the initial prompt"
        )
        .expect("runtime should receive the initial prompt"),
        CodexRuntimeCommand::Prompt { .. }
    ));
    let child_id = created.delegation.child_session_id;
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&child_id)
            .expect("child should exist");
        let child = inner
            .session_mut_by_index(index)
            .expect("child should be mutable");
        child.remote_id = Some("missing-test-remote".to_owned());
        child.remote_session_id = Some("remote-child".to_owned());
    }
    let calls_before_proxy = transport.requests().len();
    let error = match state.dispatch_turn(
        &child_id,
        SendMessageRequest {
            text: "This must be proxied, never evaluated locally.".to_owned(),
            expanded_text: None,
            attachments: Vec::new(),
            source_session_id: None,
            source_mailbox: None,
        },
    ) {
        Ok(_) => panic!("the deliberately missing remote config should reject proxying"),
        Err(error) => error,
    };
    assert!(error.message.contains("remote"));
    assert_eq!(transport.requests().len(), calls_before_proxy);
}

#[test]
fn ordinary_approval_pause_preserves_runtime_and_does_not_promote_queued_work() {
    let turn = uncertain_begin::OrdinaryRoot::new("ordinary-approval-pause");
    let dispatch = turn.prepare("Pause this begun turn for an approval.");
    deliver_turn_dispatch(&turn.state, dispatch).expect("initial prompt reaches the runtime");
    assert!(
        turn.prompt()
            .prompt
            .contains("Pause this begun turn for an approval.")
    );
    let runtime_token = turn.record(|record| record.runtime.runtime_token().unwrap());
    turn.edit(|record| record.session.status = SessionStatus::Approval);
    let queued = turn.enqueue("wait for approval", QueuedPromptSource::User);
    assert!(
        turn.state
            .start_next_queued_turn_off_lock(&turn.session, false, false)
            .unwrap()
            .is_none()
    );
    turn.assert_no_prompt();
    turn.record(|record| {
        assert_eq!(record.session.status, SessionStatus::Approval);
        assert!(record.runtime.matches_runtime_token(&runtime_token));
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(record.queued_prompts[0].pending_prompt.id, queued);
        assert_eq!(
            record.queued_prompts[0].pending_prompt.text,
            "wait for approval"
        );
    });
    // Remove only this fixture's queued follow-up before completing the turn.
    turn.state
        .cancel_queued_prompt(&turn.session, &queued)
        .unwrap();
    turn.edit(|record| record.session.status = SessionStatus::Active);
    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.session, &runtime_token)
        .expect("ordinary turn finishes after approval resumes it");
    turn.record(|record| {
        assert_eq!(record.session.status, SessionStatus::Idle);
        assert!(record.runtime.matches_runtime_token(&runtime_token));
        assert!(record.queued_prompts.is_empty());
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
    assert_no_admission_or_native_credit_requests(&turn.transport);
}

#[test]
fn global_disable_values_are_explicit() {
    for value in ["1", "true", "TRUE", " yes ", "On"] {
        assert!(
            engram_disable_env_value_is_truthy(value),
            "`{value}` should enable the global Engram kill switch"
        );
    }
    for value in ["", "0", "false", "no", "off", "disabled"] {
        assert!(
            !engram_disable_env_value_is_truthy(value),
            "`{value}` should leave Engram available"
        );
    }
}

#[derive(Clone, Copy)]
enum EngramTerminationCase {
    Stop,
    Kill,
    FailTurn,
    MarkError,
    RuntimeExit,
}

fn assert_ordinary_terminal_case(case: EngramTerminationCase, label: &str) {
    let (state, runtime_rx) =
        test_app_state_with_delegation_codex_runtime(&format!("engram-s8-{label}"));
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join(format!("engram-terminal-{label}"));
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram terminal checkpoint");
    let parent_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    enable_test_project_engram(&state, &project_id, &root);
    let transport = ScriptedEngramControlTransport::new([]);
    state.install_control_test_transport(transport.clone());

    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: format!("Exercise {label} checkpoint."),
                title: Some(format!("Engram S8 {label}")),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should start");
    assert!(matches!(
        receive_synchronous_engram_prompt(&state, &runtime_rx, "runtime should receive the prompt")
            .expect("runtime should receive the prompt"),
        CodexRuntimeCommand::Prompt { .. }
    ));
    let child_id = created.delegation.child_session_id;
    let (runtime_token, active_turn_generation) = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == child_id)
            .expect("child runtime should be active");
        (
            record
                .runtime
                .runtime_token()
                .expect("child runtime should be active"),
            record.active_turn_generation,
        )
    };

    match case {
        EngramTerminationCase::Stop => {
            state.stop_session(&child_id).expect("stop should succeed");
        }
        EngramTerminationCase::Kill => {
            state.kill_session(&child_id).expect("kill should succeed");
        }
        EngramTerminationCase::FailTurn => {
            state
                .fail_rejected_turn_delivery(
                    &child_id,
                    &runtime_token,
                    active_turn_generation,
                    "test delivery failure",
                )
                .expect("fail turn should succeed");
        }
        EngramTerminationCase::MarkError => {
            state
                .mark_turn_error_if_runtime_matches(&child_id, &runtime_token, "test error")
                .expect("mark error should succeed");
        }
        EngramTerminationCase::RuntimeExit => {
            state
                .handle_runtime_exit_if_matches(
                    &child_id,
                    &runtime_token,
                    Some("fixture runtime exit"),
                )
                .expect("runtime exit should succeed");
        }
    }

    assert_no_admission_or_native_credit_requests(&transport);
    let inner = state.inner.lock().unwrap();
    if matches!(case, EngramTerminationCase::Kill) {
        assert!(
            inner.find_session_index(&child_id).is_none(),
            "Kill removes its session"
        );
        return;
    }
    let child = &inner.sessions[inner.find_session_index(&child_id).unwrap()];
    assert_ne!(child.session.status, SessionStatus::Active);
    assert_ne!(child.session.status, SessionStatus::Stopping);
    assert!(!child.runtime_stop_in_progress);
}

#[test]
fn ordinary_terminal_paths_do_not_need_an_engram_checkpoint() {
    for (case, label) in [
        (EngramTerminationCase::Stop, "stop"),
        (EngramTerminationCase::Kill, "kill"),
        (EngramTerminationCase::FailTurn, "failed-delivery"),
        (EngramTerminationCase::MarkError, "error"),
        (EngramTerminationCase::RuntimeExit, "runtime-exit"),
    ] {
        assert_ordinary_terminal_case(case, label);
    }
}

#[test]
fn ordinary_stop_then_identical_followup_has_a_new_provider_turn() {
    let (state, runtime_rx) = test_app_state_with_delegation_codex_runtime("engram-stop-followup");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-stop-followup-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram stop follow-up");
    let parent_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    enable_test_project_engram(&state, &project_id, &root);
    let prompt = "Repeat this exact prompt.";
    let transport = ScriptedEngramControlTransport::new([]);
    install_control_only_transport(&state, transport.clone());

    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: prompt.to_owned(),
                title: Some("Engram resumable stop".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should start");
    assert!(matches!(
        receive(&runtime_rx, "runtime should receive the first prompt"),
        CodexRuntimeCommand::Prompt { .. }
    ));
    let initial_prompt = build_delegation_prompt(&created.delegation);
    let child_id = created.delegation.child_session_id;
    let generation = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&child_id).unwrap()].active_turn_generation
    };
    state.stop_session(&child_id).expect("stop should succeed");
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert!(
            AppState::engram_session_is_enabled_locked(&inner, &child_id),
            "a resumable Stop must keep the Engram child eligible"
        );
    }

    let (fresh_runtime, fresh_rx, _fresh_process) =
        test_shared_codex_runtime("engram-stop-followup-runtime");
    install_single_wire_codex_fixture(&state, Some(fresh_runtime));
    let followup = state
        .dispatch_turn(
            &child_id,
            SendMessageRequest {
                text: prompt.to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .expect("the stopped session should accept an identical follow-up");
    let followup_dispatch = match followup {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => {
            panic!("an explicit user follow-up must resume a stopped child immediately")
        }
    };
    deliver_turn_dispatch(&state, followup_dispatch)
        .expect("the granted follow-up should reach the resumed runtime");

    match receive(
        &fresh_rx,
        "identical follow-up reaches the new provider turn",
    ) {
        CodexRuntimeCommand::Prompt {
            session_id,
            command,
        } => {
            assert_eq!(session_id, child_id);
            assert!(command.prompt.ends_with(prompt));
        }
        _ => panic!("expected the ordinary follow-up"),
    }
    assert!(matches!(
        fresh_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&child_id).unwrap()];
    assert!(record.active_turn_generation > generation);
    for expected in [initial_prompt.as_str(), prompt] {
        assert_eq!(record.session.messages.iter().filter(|message|
            matches!(message, Message::Text { text, author: Author::You, .. } if text == expected)).count(), 1);
    }
    assert_no_admission_or_native_credit_requests(&transport);
}

#[test]
fn nested_child_inherits_engram_project_through_an_isolated_parent_chain() {
    let (state, runtime_rx) =
        test_app_state_with_delegation_codex_runtime("engram-nested-effective-project");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-nested-effective-project-root");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram nested effective project");
    let root_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);

    let isolated = state
        .create_read_only_delegation(
            &root_session_id,
            CreateDelegationRequest {
                prompt: "Create the isolated-style parent before Engram is enabled.".to_owned(),
                title: Some("Engram isolated parent".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("first-level delegation should start");
    assert!(matches!(
        receive_synchronous_engram_prompt(
            &state,
            &runtime_rx,
            "runtime should receive the first-level prompt"
        )
        .expect("runtime should receive the first-level prompt"),
        CodexRuntimeCommand::Prompt { .. }
    ));
    let isolated_id = isolated.delegation.child_session_id;
    let isolated_runtime_token = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&isolated_id)
            .expect("isolated parent should exist");
        let token = inner.sessions[index]
            .runtime
            .runtime_token()
            .expect("isolated parent runtime should be active");
        inner
            .session_mut_by_index(index)
            .expect("isolated parent should be mutable")
            .session
            .project_id = None;
        let delegation = inner
            .delegations
            .iter_mut()
            .find(|delegation| delegation.child_session_id == isolated_id)
            .expect("first-level delegation should exist");
        delegation.write_policy = DelegationWritePolicy::IsolatedWorktree {
            base: None,
            owned_paths: Vec::new(),
            worktree_path: Some(
                root.join("isolated-worktree")
                    .to_string_lossy()
                    .into_owned(),
            ),
        };
        token
    };
    state
        .finish_turn_ok_if_runtime_matches(&isolated_id, &isolated_runtime_token)
        .expect("first-level turn should finish");

    enable_test_project_engram(&state, &project_id, &root);
    let transport = ScriptedEngramControlTransport::new([]);
    state.install_control_test_transport(transport.clone());
    let nested = state
        .create_read_only_delegation(
            &isolated_id,
            CreateDelegationRequest {
                prompt: "Resolve Engram through the full delegation ancestry.".to_owned(),
                title: Some("Engram nested child".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("nested delegation should inherit the effective project");
    assert!(matches!(
        receive_synchronous_engram_prompt(
            &state,
            &runtime_rx,
            "runtime should receive the nested prompt"
        )
        .expect("runtime should receive the nested prompt"),
        CodexRuntimeCommand::Prompt { .. }
    ));
    let nested_id = nested.delegation.child_session_id;
    assert_no_admission_or_native_credit_requests(&transport);
    let inner = state.inner.lock().expect("state mutex poisoned");
    let effective_project = engram_project_for_session_locked(&inner, &nested_id)
        .expect("nested child should resolve the root project");
    assert_eq!(effective_project.id, project_id);
    let nested_record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == nested_id)
        .expect("nested child should exist");
    assert!(
        engram_mcp_stdio_config_for_session_locked(&inner, &nested_record.session.id).is_some()
    );
}

#[test]
fn historical_control_survives_restart_as_passive_redacted_data() {
    let state = test_app_state();
    let session = test_session_id(&state, Agent::Codex);
    let operation = turn_checks::checkpoint_result_recovery::passive_checkpoint(
        FsPath::new(&state.default_workdir),
        &session,
    );
    let history = BTreeMap::from([
        (
            "engramRoutingToken".to_owned(),
            json!("historical-routing-secret"),
        ),
        (
            "engramOpenGrantId".to_owned(),
            json!("historical-grant-secret"),
        ),
        (
            "engramCheckpointOperation".to_owned(),
            serde_json::to_value(&operation).unwrap(),
        ),
    ]);
    assert_eq!(
        history["engramCheckpointOperation"]["original"]["request"]["observations"][0]["unknown_nested"],
        json!([1, null, {"x": true}]),
        "unknown original report bytes stay part of the valid stored image"
    );
    {
        let mut inner = state.inner.lock().unwrap();
        turn_checks::checkpoint_result_recovery::install_historical_checkpoint(
            &mut inner, &session, &operation,
        );
        inner
            .session_mut(&session)
            .unwrap()
            .engram
            .historical_control = history.clone();
        state.commit_locked(&mut inner).unwrap();
        let debug = format!("{:?}", inner.session_mut(&session).unwrap().engram);
        assert!(!debug.contains("historical-routing-secret"));
        assert!(!debug.contains("historical-grant-secret"));
    }
    let client = serde_json::to_string(&state.snapshot()).unwrap();
    assert!(!client.contains("historical-routing-secret"));
    assert!(!client.contains("historical-grant-secret"));
    state.shutdown_persist_blocking();
    let persisted: Value = serde_json::from_str(&persisted_session_json(&state, &session)).unwrap();
    for (key, value) in &history {
        assert_eq!(&persisted[key], value);
    }

    let restarted = AppState::new_with_paths(
        state.default_workdir.clone(),
        state.persistence_path.as_path().to_path_buf(),
        state.orchestrator_templates_path.as_path().to_path_buf(),
    )
    .unwrap();
    {
        let inner = restarted.inner.lock().unwrap();
        assert_eq!(
            inner.sessions[inner.find_session_index(&session).unwrap()]
                .engram
                .historical_control,
            history
        );
    }
    restarted.shutdown_persist_blocking();
    let reloaded: Value =
        serde_json::from_str(&persisted_session_json(&restarted, &session)).unwrap();
    for (key, value) in &history {
        assert_eq!(&reloaded[key], value);
    }
}

#[test]
fn historical_control_does_not_admit_block_or_credit_an_ordinary_turn() {
    let turn = EOCheckedTurn::idle("generic-passive-history", true);
    let operation =
        turn_checks::checkpoint_result_recovery::passive_checkpoint(&turn.root, &turn.session_id);
    {
        let mut inner = turn.state.inner.lock().unwrap();
        turn_checks::checkpoint_result_recovery::install_historical_checkpoint(
            &mut inner,
            &turn.session_id,
            &operation,
        );
        inner
            .session_mut(&turn.session_id)
            .unwrap()
            .engram
            .historical_control
            .insert("engramOpenGrantId".to_owned(), json!("old-open-grant"));
        turn.state.commit_locked(&mut inner).unwrap();
    }
    let before = turn.record(|record| record.engram.historical_control.clone());
    let command = turn.begin_prompt("Run despite passive legacy control.");
    assert!(
        command
            .prompt
            .ends_with("Run despite passive legacy control.")
    );
    let request = turn.finish();
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
    assert_eq!(
        turn.record(|record| record.engram.historical_control.clone()),
        before
    );
}

#[test]
fn observability_toggle_is_metadata_only_and_preserves_live_runtime_and_queue() {
    let (state, _root, project, ids) = ordinary_runtime_family_fixture("metadata-toggle");
    let session = &ids[0];
    queue_test_engram_prompt(
        &state,
        session,
        "preserved user follow-up",
        QueuedPromptSource::User,
        None,
    );
    let (mut settings, token, queued) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(session).unwrap()];
        (
            inner
                .find_project(&project)
                .unwrap()
                .engram
                .clone()
                .unwrap(),
            record.runtime.runtime_token().unwrap(),
            serde_json::to_value(&record.session.pending_prompts).unwrap(),
        )
    };
    for enabled in [false, true] {
        settings.execution_observability = enabled;
        state
            .update_project_engram_settings(&project, settings.clone())
            .unwrap();
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(session).unwrap()];
        assert!(record.runtime.matches_runtime_token(&token));
        assert!(!record.runtime_reset_required);
        assert!(!record.runtime_stop_in_progress);
        assert_eq!(
            serde_json::to_value(&record.session.pending_prompts).unwrap(),
            queued
        );
        assert!(!inner.engram_project_resets.contains(&project));
    }
}

fn assert_ordinary_rotation_replans_quarantine(initial: bool, final_state: bool) {
    let (state, root, project, ids) = ordinary_runtime_family_fixture(if final_state {
        "quarantine-appears"
    } else {
        "quarantine-disappears"
    });
    {
        let mut inner = state.inner.lock().unwrap();
        inner
            .session_mut(&ids[0])
            .unwrap()
            .engram_mcp_runtime_quarantined = initial;
    }
    let hook_state = state.clone();
    let hook_session = ids[0].clone();
    let changed_session = hook_session.clone();
    TEST_ENGRAM_SETTINGS_MUTATION_BOUNDARY.with(|slot| {
        *slot.borrow_mut() = Some((
            "before_commit",
            Box::new(move || {
                hook_state
                    .inner
                    .lock()
                    .unwrap()
                    .session_mut(&changed_session)
                    .unwrap()
                    .engram_mcp_runtime_quarantined = final_state;
            }),
        ));
    });
    let next_home = root.join("next-home");
    fs::create_dir_all(&next_home).unwrap();
    let mut next = real_fixture_engram_settings(&root);
    next.home = Some(next_home.to_string_lossy().into_owned());
    state
        .update_project_engram_settings(&project, next)
        .unwrap();
    let inner = state.inner.lock().unwrap();
    assert!(!inner.engram_project_resets.contains(&project));
    for id in ids {
        let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
        if final_state && id == hook_session {
            assert!(matches!(record.runtime, SessionRuntime::None));
            assert!(!record.runtime_reset_required);
        } else {
            assert!(!matches!(record.runtime, SessionRuntime::None));
            assert!(record.runtime_reset_required);
        }
        assert!(!record.engram_mcp_runtime_quarantined);
        assert!(!record.runtime_stop_in_progress);
    }
}

#[test]
fn ordinary_rotation_rechecks_quarantine_that_appears_during_validation() {
    assert_ordinary_rotation_replans_quarantine(false, true);
}

#[test]
fn ordinary_rotation_preserves_healthy_runtimes_when_quarantine_clears_during_validation() {
    assert_ordinary_rotation_replans_quarantine(true, false);
}

#[test]
fn ordinary_connection_rotation_marks_all_runtime_families_for_the_next_send() {
    let (state, root, project, ids) = ordinary_runtime_family_fixture("connection-rotation");
    let old_tokens = {
        let inner = state.inner.lock().unwrap();
        ids.iter()
            .map(|id| {
                inner.sessions[inner.find_session_index(id).unwrap()]
                    .runtime
                    .runtime_token()
                    .unwrap()
            })
            .collect::<Vec<_>>()
    };
    let next_home = root.join("new-home");
    fs::create_dir_all(&next_home).unwrap();
    let mut next = real_fixture_engram_settings(&root);
    next.home = Some(next_home.to_string_lossy().into_owned());
    state
        .update_project_engram_settings(&project, next)
        .unwrap();
    let inner = state.inner.lock().unwrap();
    for (id, token) in ids.iter().zip(old_tokens) {
        let record = &inner.sessions[inner.find_session_index(id).unwrap()];
        assert!(record.runtime.matches_runtime_token(&token));
        assert!(record.runtime_reset_required);
        assert!(!record.runtime_stop_in_progress);
        assert!(record.session.messages.iter().any(|message|
            matches!(message, Message::Text { text, .. }
                if text == "Engram connection changed. This runtime will use the current configuration on the next send.")));
    }
    assert!(!inner.engram_project_resets.contains(&project));
}

#[test]
fn connection_generation_fences_overlap_and_stale_release_preserves_the_new_owner() {
    let (state, root, project, ids) = ordinary_runtime_family_fixture("connection-generation");
    let (old, newer, batch) = {
        let mut inner = state.inner.lock().unwrap();
        let old = inner.claim_engram_project_reset(&project).unwrap();
        let batch =
            claim_engram_mcp_runtime_revocations_locked(&mut inner, std::slice::from_ref(&ids[0]));
        assert!(inner.engram_project_resets.release(&project, old));
        let newer = inner.claim_engram_project_reset(&project).unwrap();
        (old, newer, batch)
    };
    for settings in [
        real_fixture_engram_settings(&root),
        EngramProjectSettings::default(),
    ] {
        let error = state
            .update_project_engram_settings(&project, settings)
            .err()
            .expect("overlapping connection changes must refuse");
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert_eq!(
            error.message,
            "Engram connection change is already in progress"
        );
    }
    assert_eq!(
        state.delete_project(&project).err().unwrap().status,
        StatusCode::CONFLICT
    );
    {
        let mut inner = state.inner.lock().unwrap();
        assert!(!inner.engram_project_resets.release(&project, old));
        let (_, changed) =
            release_engram_mcp_runtime_revocations_without_teardown_locked(&mut inner, batch);
        assert!(changed);
        assert!(inner.engram_project_resets.is_owned_by(&project, newer));
        let record = inner.session_mut(&ids[0]).unwrap();
        assert!(!record.runtime_stop_in_progress);
        assert!(record.runtime_stop_owner.is_none());
        assert!(inner.engram_project_resets.release(&project, newer));
    }
}

#[test]
fn readiness_validation_failure_returns_its_own_error_before_a_connection_claim() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("missing-project-marker");
    fs::create_dir_all(&root).unwrap();
    let project = create_test_project(&state, &root, "Missing Engram declaration");
    let error = state
        .update_project_engram_settings(&project, real_fixture_engram_settings(&root))
        .err()
        .expect("a missing declaration must refuse enablement");
    assert!(
        error.message.contains("is missing or unreadable"),
        "{}",
        error.message
    );
    let inner = state.inner.lock().unwrap();
    assert!(!inner.engram_project_resets.contains(&project));
    assert!(inner.find_project(&project).unwrap().engram.is_none());
}

fn assert_ordinary_settings_persistence_rollback(disable: bool) {
    let (mut state, root, project, ids) = ordinary_runtime_family_fixture(if disable {
        "disable-rollback"
    } else {
        "rotation-rollback"
    });
    let id = &ids[0];
    queue_test_engram_prompt(
        &state,
        id,
        "pending through settings rollback",
        QueuedPromptSource::User,
        None,
    );
    let (old_settings, old_token, old_messages, old_pending) = {
        let mut inner = state.inner.lock().unwrap();
        let settings = inner
            .find_project(&project)
            .unwrap()
            .engram
            .clone()
            .unwrap();
        let record = inner.session_mut(id).unwrap();
        record.engram.pending_context_nudge = Some("undelivered context".to_owned());
        record.engram.context_refresh_needed = true;
        (
            settings,
            record.runtime.runtime_token().unwrap(),
            serde_json::to_value(&record.session.messages).unwrap(),
            serde_json::to_value(&record.session.pending_prompts).unwrap(),
        )
    };
    let mut next = old_settings.clone();
    if disable {
        next.enabled = false;
    } else {
        let next_home = root.join("next-home");
        fs::create_dir_all(&next_home).unwrap();
        next.home = Some(next_home.to_string_lossy().into_owned());
    }
    state.shutdown_persist_blocking();
    let invalid_store = root.join("settings-persistence-is-a-directory.sqlite");
    fs::create_dir_all(&invalid_store).unwrap();
    state.persistence_path = Arc::new(invalid_store);
    let error = state
        .update_project_engram_settings(&project, next)
        .err()
        .expect("settings persistence failure must remain visible");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        error
            .message
            .contains("failed to persist Engram project settings")
    );
    let inner = state.inner.lock().unwrap();
    assert_eq!(
        inner.find_project(&project).unwrap().engram.as_ref(),
        Some(&old_settings)
    );
    assert!(!inner.engram_project_resets.contains(&project));
    let record = &inner.sessions[inner.find_session_index(id).unwrap()];
    assert!(record.runtime.matches_runtime_token(&old_token));
    assert!(!record.runtime_reset_required);
    assert!(!record.runtime_stop_in_progress);
    assert!(record.runtime_stop_owner.is_none());
    assert!(!record.engram_mcp_revocation_pending);
    assert_eq!(
        serde_json::to_value(&record.session.messages).unwrap(),
        old_messages
    );
    assert_eq!(
        serde_json::to_value(&record.session.pending_prompts).unwrap(),
        old_pending
    );
    assert_eq!(
        record.engram.pending_context_nudge.as_deref(),
        Some("undelivered context")
    );
    assert!(record.engram.context_refresh_needed);
}

#[test]
fn ordinary_connection_rotation_persist_failure_restores_settings_context_and_runtime() {
    assert_ordinary_settings_persistence_rollback(false);
}

#[test]
fn ordinary_disable_persist_failure_releases_exact_runtime_and_project_fences() {
    assert_ordinary_settings_persistence_rollback(true);
}

#[test]
fn project_deletion_persist_failure_keeps_its_connection_and_sessions_attached() {
    let (mut state, root, project, ids) = ordinary_runtime_family_fixture("delete-rollback");
    let before = {
        let inner = state.inner.lock().unwrap();
        inner.find_project(&project).unwrap().engram.clone()
    };
    state.shutdown_persist_blocking();
    let invalid_store = root.join("deletion-persistence-is-a-directory.sqlite");
    fs::create_dir_all(&invalid_store).unwrap();
    state.persistence_path = Arc::new(invalid_store);
    let error = state
        .delete_project(&project)
        .err()
        .expect("deletion must report failed durability");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        error
            .message
            .contains("failed to persist Engram project settings")
    );
    let inner = state.inner.lock().unwrap();
    assert_eq!(inner.find_project(&project).unwrap().engram, before);
    assert!(!inner.engram_project_resets.contains(&project));
    for id in ids {
        let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
        // A linked descendant keeps its original projectless scope.
        if record.session.parent_delegation_id.is_none() {
            assert_eq!(record.session.project_id.as_deref(), Some(project.as_str()));
        }
        assert!(!matches!(record.runtime, SessionRuntime::None));
        assert!(!record.runtime_stop_in_progress);
        assert!(!record.engram_mcp_revocation_pending);
    }
}

#[test]
fn ordinary_cleanup_warnings_are_deduplicated_durable_and_cleared_by_exact_kind() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("cleanup-warning");
    fs::create_dir_all(&root).unwrap();
    let project = create_test_project(&state, &root, "Cleanup warning without sessions");
    state.note_engram_cleanup_degraded(&project, "cleanup A", "fixture");
    state.note_engram_cleanup_degraded(&project, "cleanup A", "fixture duplicate");
    assert_eq!(
        state
            .inner
            .lock()
            .unwrap()
            .find_project(&project)
            .unwrap()
            .engram_cleanup_warning
            .as_deref(),
        Some("cleanup A")
    );
    let session = create_test_project_session(&state, Agent::Claude, &project, &root);
    state.note_engram_cleanup_degraded(&project, "cleanup A", "fixture session");
    state.note_engram_cleanup_degraded(&project, "cleanup A", "fixture session duplicate");
    state.note_engram_cleanup_degraded(&project, "cleanup B", "fixture");
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        for notice in ["cleanup A", "cleanup B"] {
            assert_eq!(
                record
                    .session
                    .messages
                    .iter()
                    .filter(
                        |message| matches!(message, Message::Text { text, .. } if text == notice)
                    )
                    .count(),
                1
            );
        }
    }
    {
        let inner = state.inner.lock().unwrap();
        assert_eq!(
            inner
                .find_project(&project)
                .unwrap()
                .engram_cleanup_warning
                .as_deref(),
            Some("cleanup A\n\ncleanup B")
        );
    }
    state.clear_engram_cleanup_warning_if_matches(&project, "unrelated", "fixture");
    state.clear_engram_cleanup_warning_if_matches(&project, "cleanup A", "fixture");
    state.shutdown_persist_blocking();
    let persisted = sqlite_metadata_state_value(&state.persistence_path);
    let saved = persisted["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value["id"] == project)
        .unwrap();
    assert_eq!(saved["engramCleanupWarning"], "cleanup B");
    state.clear_engram_cleanup_warning_if_matches(&project, "cleanup B", "fixture");
    assert!(
        state
            .inner
            .lock()
            .unwrap()
            .find_project(&project)
            .unwrap()
            .engram_cleanup_warning
            .is_none()
    );
}

#[test]
fn current_project_settings_reject_removed_control_fields() {
    for field in ["turnGatedControl", "workAuthorityGrant"] {
        let mut input = json!({"enabled": true, "executionObservability": true});
        input[field] = json!(true);
        let error = serde_json::from_value::<UpdateProjectEngramSettingsRequest>(input)
            .err()
            .expect("removed admission settings must be rejected");
        assert!(error.to_string().contains(field));
    }
}

#[test]
fn ordinary_readiness_adopts_store_identity_without_assurance_tier() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("ordinary-readiness");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join(".engram-project"), "fixture-ready\n").unwrap();
    let project = create_test_project(&state, &root, "Ordinary readiness");
    state
        .update_project_engram_settings(&project, real_fixture_engram_settings(&root))
        .unwrap();
    let inner = state.inner.lock().unwrap();
    let settings = inner
        .find_project(&project)
        .unwrap()
        .engram
        .as_ref()
        .unwrap();
    assert!(settings.enabled);
    assert_eq!(
        settings.authority_store_key.as_ref().unwrap().project_id,
        "fixture-ready"
    );
    assert_eq!(
        settings.authority_store_key.as_ref().unwrap().database_path,
        normalize_user_facing_path(&work_database_path(&root, "fixture-ready"))
    );
}

#[test]
fn ordinary_readiness_rejects_malformed_and_failed_replies_without_mutating_settings() {
    for mode in ["fixture-readiness-malformed", "fixture-readiness-refusal"] {
        let state = test_app_state();
        let root = state.test_temp_root.as_ref().unwrap().path().join(mode);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(".engram-project"), format!("{mode}\n")).unwrap();
        let project = create_test_project(&state, &root, "Invalid readiness");
        let error = state
            .update_project_engram_settings(&project, real_fixture_engram_settings(&root))
            .err()
            .expect("invalid readiness must refuse");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        let inner = state.inner.lock().unwrap();
        assert!(inner.find_project(&project).unwrap().engram.is_none());
        assert!(!inner.engram_project_resets.contains(&project));
    }
}

struct UnknownGenerationWorkTransport(Arc<ScriptedEngramControlTransport>);

impl EngramControlTransport for UnknownGenerationWorkTransport {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        self.0.request(connection, request, timeout)
    }
    fn shutdown_session(&self, session: &str) {
        self.0.shutdown_session(session);
    }
    fn connection_generation(&self, _session: &str) -> Option<u64> {
        None
    }
}

struct PausedOrdinaryWorkTransport {
    inner: Arc<ScriptedEngramControlTransport>,
    entered: Mutex<Option<mpsc::SyncSender<()>>>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl EngramControlTransport for PausedOrdinaryWorkTransport {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        if matches!(request, EngramControlRequest::WorkSessionRead {}) {
            let entered = self.entered.lock().unwrap().take();
            if let Some(entered) = entered {
                entered.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
                return Err(EngramTransportError::deadline(
                    "fixture ordinary service unavailable",
                ));
            }
        }
        self.inner.request(connection, request, timeout)
    }
    fn read_observation_policy(
        &self,
        connection: &EngramConnectionConfig,
        store: &EngramAuthorityStoreKey,
        timeout: Duration,
        host_workdir: &FsPath,
    ) -> Result<EngramObservationPolicyBasis, EngramTransportError> {
        self.inner
            .read_observation_policy(connection, store, timeout, host_workdir)
    }
    fn read_work_binding(
        &self,
        connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>,
        timeout: Duration,
    ) -> Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.inner
            .read_work_binding(connection, preference, timeout)
    }
    fn read_held_claims(
        &self,
        connection: &EngramConnectionConfig,
        timeout: Duration,
    ) -> Result<EngramHeldClaims, EngramTransportError> {
        self.inner.read_held_claims(connection, timeout)
    }
    fn shutdown_session(&self, session: &str) {
        self.inner.shutdown_session(session);
    }
    fn shutdown_session_until(
        &self,
        session: &str,
        clock: &EngramBudgetClock,
        deadline: std::time::Instant,
    ) -> Result<(), EngramTransportError> {
        self.inner.shutdown_session_until(session, clock, deadline)
    }
}

struct OrdinaryRequestRelease(Option<mpsc::Sender<()>>);

impl Drop for OrdinaryRequestRelease {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

#[test]
fn slow_ordinary_work_service_holds_no_state_lock_and_does_not_gate_provider_delivery() {
    let turn = EOCheckedTurn::idle("generic-slow-work-service", true);
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::channel();
    let mut release = OrdinaryRequestRelease(Some(release_tx));
    let transport = Arc::new(PausedOrdinaryWorkTransport {
        inner: turn.transport.clone(),
        entered: Mutex::new(Some(entered_tx)),
        release: Mutex::new(release_rx),
    });
    turn.state.install_control_test_transport(transport);
    let naming_state = turn.state.clone();
    let naming_session = turn.session_id.clone();
    let path = turn.root.to_string_lossy().into_owned();
    let work = turn.record(|record| record.engram.work_binding.as_ref().unwrap().work_id.clone());
    let naming = std::thread::spawn(move || {
        naming_state.name_engram_source_root(
            &naming_session,
            EngramSourceRootRequest {
                work,
                path: Some(Some(path)),
            },
        )
    });
    receive(&entered_rx, "ordinary work service reached its request");
    assert!(
        turn.state.inner.try_lock_now().is_some(),
        "work-service I/O must run off-lock"
    );
    let command = turn.begin_prompt("Provider delivery is independent of the naming read.");
    assert!(
        command
            .prompt
            .ends_with("Provider delivery is independent of the naming read.")
    );
    release.0.take().unwrap().send(()).unwrap();
    assert!(
        naming.join().unwrap().is_err(),
        "the simulated service failure stays visible"
    );
    let request = turn.finish();
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}

#[tokio::test]
async fn asynchronous_ordinary_stop_publishes_stopping_before_runtime_cleanup() {
    let (state, _root, _project, ids) = ordinary_runtime_family_fixture("async-ordinary-stop");
    let id = ids[0].clone();
    {
        let mut inner = state.inner.lock().unwrap();
        inner.session_mut(&id).unwrap().session.status = SessionStatus::Active;
        state.commit_locked(&mut inner).unwrap();
    }
    let transport = ScriptedEngramControlTransport::new([]);
    state.install_control_test_transport(transport.clone());
    let gate = install_test_stop_fence_gate(&state, &id);
    let mut events = state.subscribe_events();
    let response = state.request_stop_session(&id).unwrap();
    assert_eq!(
        response
            .sessions
            .iter()
            .find(|session| session.id == id)
            .unwrap()
            .status,
        SessionStatus::Stopping
    );
    gate.wait_until_claimed();
    assert_eq!(
        state
            .snapshot()
            .sessions
            .iter()
            .find(|session| session.id == id)
            .unwrap()
            .status,
        SessionStatus::Stopping
    );
    gate.release();
    tokio::time::timeout(DEADLOCK_GUARD, async {
        loop {
            let status = state
                .snapshot()
                .sessions
                .iter()
                .find(|session| session.id == id)
                .unwrap()
                .status;
            if status != SessionStatus::Stopping {
                assert_eq!(status, SessionStatus::Idle);
                break;
            }
            events.recv().await.unwrap();
        }
    })
    .await
    .expect("ordinary Stop must publish its terminal outcome");
    assert_no_admission_or_native_credit_requests(&transport);
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
    assert!(!record.runtime_stop_in_progress);
    assert!(record.engram.operator_paused);
}

fn assert_engram_mcp_disable_or_delete_revokes_existing_runtimes(delete_project: bool) {
    let suffix = if delete_project { "delete" } else { "disable" };
    let (state, root, project_id, session_ids) = ordinary_runtime_family_fixture(suffix);
    if delete_project {
        state
            .delete_project(&project_id)
            .expect("project deletion should revoke MCP runtimes");
    } else {
        state
            .update_project_engram_settings(&project_id, EngramProjectSettings::default())
            .expect("disable should revoke MCP runtimes");
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    if !delete_project {
        assert_eq!(
            inner
                .find_project(&project_id)
                .and_then(|project| project.engram.as_ref())
                .map(|settings| settings.enabled),
            Some(false),
            "disable must remain durable after the actual runtime cleanup"
        );
    }
    for session_id in session_ids {
        let record = inner
            .find_session_index(&session_id)
            .and_then(|index| inner.sessions.get(index))
            .expect("affected session should remain");
        assert!(
            matches!(record.runtime, SessionRuntime::None),
            "{suffix} should detach runtime for session {session_id}"
        );
        assert!(
            !record.runtime_reset_required,
            "{suffix} should clear reset flag for session {session_id}"
        );
    }
}

#[test]
fn engram_mcp_disable_immediately_revokes_existing_runtimes() {
    assert_engram_mcp_disable_or_delete_revokes_existing_runtimes(false);
}

#[test]
fn engram_mcp_project_delete_immediately_revokes_existing_runtimes() {
    assert_engram_mcp_disable_or_delete_revokes_existing_runtimes(true);
}

fn assert_engram_mcp_quarantined_runtime_retried_by_followup(delete_project: bool) {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-delete-quarantined-disabled-runtime");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "fixture-ready\n").expect("fixture mode should write");
    let project_id = create_test_project(&state, &root, "Engram delete quarantined runtime");
    let session_id = create_test_project_session(&state, Agent::Claude, &project_id, &root);
    let mut enabled = real_fixture_engram_settings(&root);
    state
        .update_project_engram_settings(&project_id, enabled)
        .expect("fixture authority should persist");
    let process_owner = phase_sync::ParkedProcess::spawn();
    let process = process_owner.process.clone();
    let (input_tx, _input_rx) = mpsc::channel();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("test session should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("test session index should be valid");
        record.runtime = SessionRuntime::Claude(ClaudeRuntimeHandle {
            runtime_id: "engram-delete-quarantined-runtime".to_owned(),
            input_tx,
            #[cfg(windows)]
            process_tree: None,
            process: process.clone(),
        });
        state
            .commit_locked(&mut inner)
            .expect("fixture runtime should persist");
    }

    record_test_project_runtime_descriptors(&state, &project_id);
    let failure_guard = force_test_kill_child_process_failure(&process, "Claude");
    let disable_error =
        match state.update_project_engram_settings(&project_id, EngramProjectSettings::default()) {
            Ok(_) => panic!("failed disable teardown must remain visible"),
            Err(error) => error,
        };
    assert!(
        disable_error
            .message
            .contains("runtime cleanup was degraded")
    );
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .find_session_index(&session_id)
            .and_then(|index| inner.sessions.get(index))
            .expect("quarantined session should remain");
        assert!(record.engram_mcp_runtime_quarantined);
        assert!(!matches!(record.runtime, SessionRuntime::None));
        assert!(
            !inner
                .find_project(&project_id)
                .and_then(|project| project.engram.as_ref())
                .is_some_and(|settings| settings.enabled),
            "the disable must remain durable despite failed process cleanup"
        );
    }
    drop(failure_guard);

    if delete_project {
        state
            .delete_project(&project_id)
            .expect("project deletion should retry and finish quarantined cleanup");
    } else {
        let mut disabled_edit = EngramProjectSettings::default();
        disabled_edit.deadline_ms = Some(987);
        state
            .update_project_engram_settings(&project_id, disabled_edit)
            .expect("disabled settings edit should retry quarantined cleanup");
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.find_project(&project_id).is_none(), delete_project);
    let record = inner
        .find_session_index(&session_id)
        .and_then(|index| inner.sessions.get(index))
        .expect("session should remain after project deletion");
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.engram_mcp_runtime_quarantined);
    assert_eq!(record.session.project_id.is_none(), delete_project);
    if !delete_project {
        assert_eq!(
            inner
                .find_project(&project_id)
                .and_then(|project| project.engram.as_ref())
                .and_then(|settings| settings.deadline_ms),
            Some(987)
        );
    }
    drop(inner);
    phase_sync::process_exit(&process, "follow-up reaped quarantined Claude process");
}

#[test]
fn engram_mcp_project_delete_retries_a_runtime_quarantined_by_failed_disable() {
    assert_engram_mcp_quarantined_runtime_retried_by_followup(true);
}

#[test]
fn engram_mcp_disabled_settings_edit_retries_a_quarantined_runtime() {
    assert_engram_mcp_quarantined_runtime_retried_by_followup(false);
}

#[test]
fn ordinary_home_alias_preserves_verified_store_identity_and_runtime() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-home-alias");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "  fixture-ready  \n")
        .expect("fixture mode should write");
    let exact_store_key = materialize_fixture_engram_store(&root, &root);
    assert_eq!(exact_store_key.project_id, "fixture-ready");
    let project_id = create_test_project(&state, &root, "Engram home alias");
    let session_id = create_test_project_session(&state, Agent::Claude, &project_id, &root);
    let mut enabled = real_fixture_engram_settings(&root);
    state
        .update_project_engram_settings(&project_id, enabled.clone())
        .expect("fixture settings should enable Engram");
    attach_engram_mcp_test_runtime(&state, &session_id);

    let alias_home = root.join(".").to_string_lossy().into_owned();
    enabled.home = Some(alias_home.clone());
    state
        .update_project_engram_settings(&project_id, enabled)
        .expect("an alias for the same authority store should remain current");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let persisted_home = inner
        .find_project(&project_id)
        .and_then(|project| project.engram.as_ref())
        .and_then(|settings| settings.home.as_deref());
    assert_eq!(
        persisted_home,
        Some(alias_home.as_str()),
        "TermAl should preserve the operator's home spelling"
    );
    assert_eq!(
        inner
            .find_project(&project_id)
            .and_then(|project| project.engram.as_ref())
            .and_then(|settings| settings.authority_store_key.clone()),
        Some(exact_store_key),
        "doctor identity must remain stable across a home path alias"
    );
    let record = inner
        .find_session_index(&session_id)
        .and_then(|index| inner.sessions.get(index))
        .expect("session should remain");
    assert!(!record.runtime_stop_in_progress);
    assert!(!matches!(record.runtime, SessionRuntime::None));
    assert!(!record.session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. }
            if text.contains("TermAl is retiring this runtime's previous write authority")
    )));
    drop(inner);
}

#[test]
fn disabled_connection_rejects_a_relative_home_and_keeps_distinct_absolute_homes() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("disabled-home-validation");
    fs::create_dir_all(&root).unwrap();
    let project = create_test_project(&state, &root, "Disabled home validation");
    let mut settings = EngramProjectSettings::default();
    settings.home = Some("relative/home".to_owned());
    let error = state
        .update_project_engram_settings(&project, settings.clone())
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("non-empty absolute path"));
    for suffix in ["home-a", "home-b"] {
        let path = root.join(suffix).to_string_lossy().into_owned();
        settings.home = Some(path.clone());
        state
            .update_project_engram_settings(&project, settings.clone())
            .unwrap();
        let inner = state.inner.lock().unwrap();
        assert_eq!(
            inner
                .find_project(&project)
                .unwrap()
                .engram
                .as_ref()
                .unwrap()
                .home
                .as_deref(),
            Some(path.as_str())
        );
    }
}

#[test]
fn disabling_installed_mcp_does_not_need_the_repository_marker() {
    let (state, root, project, ids) = ordinary_runtime_family_fixture("disable-missing-marker");
    fs::remove_file(root.join(".engram-project")).unwrap();
    state
        .update_project_engram_settings(&project, EngramProjectSettings::default())
        .unwrap();
    let inner = state.inner.lock().unwrap();
    for id in ids {
        let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
        assert!(matches!(record.runtime, SessionRuntime::None));
        assert!(!record.runtime_stop_in_progress);
    }
    assert!(!inner.engram_project_resets.contains(&project));
}

#[test]
fn project_deletion_uses_the_installed_descriptor_when_current_binary_is_missing() {
    let (state, _root, project, ids) = ordinary_runtime_family_fixture("delete-missing-binary");
    {
        let mut inner = state.inner.lock().unwrap();
        inner
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .unwrap()
            .engram
            .as_mut()
            .unwrap()
            .binary_path = None;
    }
    state.delete_project(&project).unwrap();
    let inner = state.inner.lock().unwrap();
    assert!(inner.find_project(&project).is_none());
    for id in ids {
        let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
        assert!(matches!(record.runtime, SessionRuntime::None));
        assert!(!record.runtime_stop_in_progress);
    }
}

#[test]
fn global_disable_keeps_ordinary_stop_and_installed_descriptor_cleanup_available() {
    const MARKER: &str = "TERMAL_TEST_ORDINARY_ENGRAM_DISABLE_CHILD";
    if std::env::var_os(MARKER).is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("tests::engram_host_adapter::global_disable_keeps_ordinary_stop_and_installed_descriptor_cleanup_available")
            .arg("--nocapture")
            .env(MARKER, "1").env(ENGRAM_GLOBAL_DISABLE_ENV, "1")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        return;
    }
    let (state, _root, project, ids) = {
        let _enabled = ScopedEnvVar::remove(ENGRAM_GLOBAL_DISABLE_ENV);
        ordinary_runtime_family_fixture("global-disable-cleanup")
    };
    assert!(engram_globally_disabled());
    let transport = ScriptedEngramControlTransport::new([]);
    state.install_control_test_transport(transport.clone());
    {
        let mut inner = state.inner.lock().unwrap();
        inner.session_mut(&ids[0]).unwrap().session.status = SessionStatus::Active;
    }
    state.stop_session(&ids[0]).unwrap();
    state.delete_project(&project).unwrap();
    assert_no_admission_or_native_credit_requests(&transport);
    let inner = state.inner.lock().unwrap();
    assert!(inner.find_project(&project).is_none());
    for id in ids {
        let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
        assert!(matches!(record.runtime, SessionRuntime::None));
        assert!(!record.runtime_stop_in_progress);
    }
}

#[test]
fn ordinary_queued_prompt_persist_failure_keeps_the_head_and_never_reaches_the_provider() {
    let mut turn = EOCheckedTurn::idle("generic-queue-persist-failure", true);
    queue_test_engram_prompt(
        &turn.state,
        &turn.session_id,
        "retain the exact queue head",
        QueuedPromptSource::User,
        None,
    );
    let before = turn.record(|record| {
        let head = record.queued_prompts.front().unwrap();
        (
            head.pending_prompt.id.clone(),
            head.pending_prompt.text.clone(),
            serde_json::to_value(&record.session.pending_prompts).unwrap(),
        )
    });
    turn.state.shutdown_persist_blocking();
    let invalid_store = turn.root.join("queue-persistence-is-a-directory.sqlite");
    fs::create_dir_all(&invalid_store).unwrap();
    turn.state.persistence_path = Arc::new(invalid_store);
    assert!(
        turn.state
            .start_next_queued_turn_off_lock(&turn.session_id, false, false)
            .is_err()
    );
    assert!(matches!(
        turn._runtime_rx.lock().unwrap().try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    turn.record(|record| {
        assert_eq!(record.queued_prompts.len(), 1);
        let head = record.queued_prompts.front().unwrap();
        assert_eq!(head.pending_prompt.id, before.0);
        assert_eq!(head.pending_prompt.text, before.1);
        assert_eq!(
            serde_json::to_value(&record.session.pending_prompts).unwrap(),
            before.2
        );
    });
    assert_ordinary_turn_has_no_native_requests(&turn);
}

fn prepare_ordinary_prompt(state: &AppState, session: &str, text: &str) -> TurnDispatch {
    match state
        .dispatch_turn(
            session,
            SendMessageRequest {
                text: text.to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .unwrap()
    {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("the explicit idle prompt should prepare"),
    }
}

#[test]
fn stale_ordinary_dispatch_and_terminal_callbacks_cannot_overwrite_a_live_successor() {
    let turn = EOCheckedTurn::idle("generic-stale-provider-dispatch", true);
    let old = prepare_ordinary_prompt(&turn.state, &turn.session_id, "old prepared prompt");
    let (old_token, old_generation) = turn.record(|record| {
        (
            record.runtime.runtime_token().unwrap(),
            record.active_turn_generation,
        )
    });
    turn.state.stop_session(&turn.session_id).unwrap();
    let (fresh_runtime, fresh_rx, _process) =
        test_shared_codex_runtime("generic-successor-provider");
    install_single_wire_codex_fixture(&turn.state, Some(fresh_runtime));
    let successor = prepare_ordinary_prompt(&turn.state, &turn.session_id, "live successor prompt");
    let generation = turn.record(|record| record.active_turn_generation);
    assert!(generation > old_generation);
    assert!(matches!(
        deliver_turn_dispatch(&turn.state, old),
        TurnDispatchDeliveryOutcome::Superseded
    ));
    turn.state
        .fail_rejected_turn_delivery(
            &turn.session_id,
            &old_token,
            old_generation,
            "stale rejected handoff",
        )
        .unwrap();
    turn.state
        .finish_turn_ok_if_runtime_and_generation_match(
            &turn.session_id,
            &old_token,
            old_generation,
        )
        .unwrap();
    turn.record(|record| {
        assert_eq!(record.active_turn_generation, generation);
        assert_eq!(record.session.status, SessionStatus::Active);
        assert!(!record.session.messages.iter().any(|message|
            matches!(message, Message::Text { text, .. } if text.contains("stale rejected handoff"))));
    });
    loop {
        match turn._runtime_rx.lock().unwrap().try_recv() {
            Ok(CodexRuntimeCommand::Prompt { .. }) => {
                panic!("the retired runtime received an old prompt")
            }
            Ok(_) => {}
            Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
        }
    }
    assert!(matches!(
        fresh_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    deliver_turn_dispatch(&turn.state, successor).unwrap();
    match receive(&fresh_rx, "only the live successor may reach the provider") {
        CodexRuntimeCommand::Prompt {
            session_id,
            command,
        } => {
            assert_eq!(session_id, turn.session_id);
            assert!(command.prompt.ends_with("live successor prompt"));
        }
        _ => panic!("expected the successor prompt"),
    }
    assert!(matches!(
        fresh_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert_ordinary_turn_has_no_native_requests(&turn);
}

#[test]
fn ordinary_naming_claim_mismatch_is_visible_without_withholding_provider_work() {
    let turn = EOCheckedTurn::idle("generic-claim-mismatch", true);
    let binding = turn.record(|record| record.engram.work_binding.clone().unwrap());
    let work = binding.work_id.clone();
    let mut changed = binding.clone();
    changed.claim_fence += 1;
    turn.transport.replace_held_claims([Ok(EngramHeldClaims {
        items: vec![EngramHeldClaim {
            work_id: work.clone(),
            short_ref: work.clone(),
            claim_id: binding.claim_id,
            claim_fence: binding.claim_fence,
            focused: true,
            control_binding: Some(changed),
        }],
        omitted: 0,
    })]);
    assert!(
        turn.state
            .name_engram_source_root(
                &turn.session_id,
                EngramSourceRootRequest {
                    work,
                    path: Some(Some(turn.root.to_string_lossy().into_owned())),
                }
            )
            .is_err(),
        "naming cannot use a held row whose exact binding fence differs"
    );
    let command = turn.begin_prompt("Continue ordinary work after the naming refusal.");
    assert!(
        command
            .prompt
            .ends_with("Continue ordinary work after the naming refusal.")
    );
    assert_ordinary_turn_has_no_native_requests(&turn);
    turn.end();
}

struct PausedConnectionShutdownTransport {
    entered: Mutex<Option<mpsc::SyncSender<()>>>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl EngramControlTransport for PausedConnectionShutdownTransport {
    fn request(
        &self,
        _connection: &EngramConnectionConfig,
        _request: &EngramControlRequest,
        _timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        Err(EngramTransportError::protocol(
            "no work request scripted during connection cleanup",
        ))
    }
    fn shutdown_session(&self, _session: &str) {}
    fn shutdown_session_until(
        &self,
        _session: &str,
        _clock: &EngramBudgetClock,
        _deadline: std::time::Instant,
    ) -> Result<(), EngramTransportError> {
        if let Some(entered) = self.entered.lock().unwrap().take() {
            entered.send(()).unwrap();
            self.release.lock().unwrap().recv().unwrap();
        }
        Ok(())
    }
}

fn assert_ordinary_connection_cleanup_fences(lose_project_generation: bool) {
    let (state, _root, project, ids) =
        ordinary_runtime_family_fixture(if lose_project_generation {
            "lost-live-connection-generation"
        } else {
            "live-connection-fences"
        });
    let token = {
        let mut inner = state.inner.lock().unwrap();
        let record = inner.session_mut(&ids[0]).unwrap();
        record.session.status = SessionStatus::Active;
        record.runtime.runtime_token().unwrap()
    };
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::channel();
    let mut release = OrdinaryRequestRelease(Some(release_tx));
    state.install_control_test_transport(Arc::new(PausedConnectionShutdownTransport {
        entered: Mutex::new(Some(entered_tx)),
        release: Mutex::new(release_rx),
    }));
    let worker_state = state.clone();
    let worker_project = project.clone();
    let worker = std::thread::spawn(move || {
        worker_state
            .update_project_engram_settings(&worker_project, EngramProjectSettings::default())
    });
    receive(
        &entered_rx,
        "connection cleanup reached its off-lock child shutdown",
    );
    assert!(
        state.inner.try_lock_now().is_some(),
        "shutdown must not hold StateInner"
    );
    {
        let inner = state.inner.lock().unwrap();
        assert!(inner.engram_project_resets.contains(&project));
        assert!(ids.iter().all(|id| {
            inner.sessions[inner.find_session_index(id).unwrap()].runtime_stop_in_progress
        }));
    }
    let overlap = state
        .update_project_engram_settings(&project, EngramProjectSettings::default())
        .err()
        .expect("the in-flight ordinary connection generation must fence overlap");
    assert_eq!(overlap.status, StatusCode::CONFLICT);
    state
        .finish_turn_ok_if_runtime_matches(&ids[0], &token)
        .unwrap();
    let newer = {
        let mut inner = state.inner.lock().unwrap();
        let record = inner.session_mut(&ids[0]).unwrap();
        assert!(
            record
                .deferred_stop_callbacks
                .iter()
                .any(|callback| matches!(callback, DeferredStopCallback::TurnCompleted { .. }))
        );
        record.set_auto_dispatch_blocked(true);
        if lose_project_generation {
            let old = inner
                .engram_project_resets
                .claimed_at
                .get(&project)
                .unwrap()
                .generation;
            assert!(inner.engram_project_resets.release(&project, old));
            Some(inner.claim_engram_project_reset(&project).unwrap())
        } else {
            None
        }
    };
    queue_test_engram_prompt(
        &state,
        &ids[0],
        "preserve while connection teardown owns callbacks",
        QueuedPromptSource::User,
        None,
    );
    release.0.take().unwrap().send(()).unwrap();
    let outcome = worker.join().unwrap();
    if lose_project_generation {
        let error = outcome
            .err()
            .expect("lost project ownership must remain visible");
        assert!(error.message.contains("ownership changed during cleanup"));
    } else {
        outcome.unwrap();
    }
    let mut inner = state.inner.lock().unwrap();
    assert!(
        !inner
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap()
            .enabled
    );
    for id in &ids {
        let record = &inner.sessions[inner.find_session_index(id).unwrap()];
        assert!(matches!(record.runtime, SessionRuntime::None));
        assert!(!record.runtime_stop_in_progress);
        assert!(record.runtime_stop_owner.is_none());
        assert!(record.deferred_stop_callbacks.is_empty());
    }
    let record = &inner.sessions[inner.find_session_index(&ids[0]).unwrap()];
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(record.session.queue_paused);
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(
        record.queued_prompts.front().unwrap().pending_prompt.text,
        "preserve while connection teardown owns callbacks"
    );
    if let Some(newer) = newer {
        assert!(inner.engram_project_resets.is_owned_by(&project, newer));
        assert!(inner.engram_project_resets.release(&project, newer));
    } else {
        assert!(!inner.engram_project_resets.contains(&project));
    }
}

#[test]
fn ordinary_connection_cleanup_fences_overlap_callbacks_and_preserves_paused_work() {
    assert_ordinary_connection_cleanup_fences(false);
}

#[test]
fn ordinary_lost_project_generation_still_releases_its_exact_runtime_fences() {
    assert_ordinary_connection_cleanup_fences(true);
}

#[test]
fn a_new_delegation_stays_queued_while_the_connection_generation_is_owned() {
    let (state, runtime_rx) =
        test_app_state_with_delegation_codex_runtime("ordinary-new-child-fence");
    let root = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("new-child-connection-fence");
    fs::create_dir_all(&root).unwrap();
    let project = create_test_project(&state, &root, "Connection-fenced child");
    let parent = create_test_project_session(&state, Agent::Codex, &project, &root);
    enable_test_project_engram(&state, &project, &root);
    let transport = ScriptedEngramControlTransport::new([]);
    state.install_control_test_transport(transport.clone());
    let generation = state
        .inner
        .lock()
        .unwrap()
        .claim_engram_project_reset(&project)
        .unwrap();
    let created = state
        .create_read_only_delegation(
            &parent,
            CreateDelegationRequest {
                prompt: "Keep this new child's exact prompt.".to_owned(),
                title: Some("Ordinary connection-fenced child".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .unwrap();
    let initial_prompt = build_delegation_prompt(&created.delegation);
    let child = created.delegation.child_session_id;
    assert!(matches!(
        runtime_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&child).unwrap();
        assert!(session_resource_reset_in_progress(
            &inner,
            &inner.sessions[index]
        ));
        let record = inner.session_mut(&child).unwrap();
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(
            record.queued_prompts.front().unwrap().pending_prompt.text,
            initial_prompt
        );
        assert!(inner.engram_project_resets.release(&project, generation));
    }
    let dispatch = state
        .start_next_queued_turn_off_lock(&child, false, false)
        .unwrap()
        .unwrap()
        .dispatch;
    deliver_turn_dispatch(&state, dispatch).unwrap();
    match receive(
        &runtime_rx,
        "the queued child reaches the ordinary provider after release",
    ) {
        CodexRuntimeCommand::Prompt {
            session_id,
            command,
        } => {
            assert_eq!(session_id, child);
            assert!(command.prompt.ends_with(&initial_prompt));
        }
        _ => panic!("expected the child prompt"),
    }
    assert!(matches!(
        runtime_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert_no_admission_or_native_credit_requests(&transport);
}

#[test]
fn ordinary_binary_spelling_change_uses_current_settings_without_retiring_authority() {
    let (state, root, project, ids) = ordinary_runtime_family_fixture("binary-spelling");
    let binary = real_engram_control_fixture_path();
    let mut next = real_fixture_engram_settings(&root);
    next.binary_path = Some(
        binary
            .parent()
            .unwrap()
            .join(".")
            .join(binary.file_name().unwrap())
            .to_string_lossy()
            .into_owned(),
    );
    state
        .update_project_engram_settings(&project, next.clone())
        .unwrap();
    let inner = state.inner.lock().unwrap();
    assert_eq!(
        inner
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap()
            .binary_path,
        next.binary_path
    );
    assert!(inner.engram_retired_work_authority_grants.is_empty());
    for id in ids {
        let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
        assert!(!record.runtime_stop_in_progress);
        assert!(!matches!(record.runtime, SessionRuntime::None));
    }
}

#[test]
fn historical_retired_authority_is_preserved_without_eviction_or_settlement() {
    let state = test_app_state();
    let entries = (0..256)
        .map(|index| EngramRetiredWorkAuthorityGrant {
            home: "historical-home".to_owned(),
            project_root: "historical-project".to_owned(),
            store_key: None,
            project_id: "historical-project".to_owned(),
            grant_hash: format!("passive-secret-{index}"),
            retired_at: "2026-08-30T00:00:00Z".to_owned(),
            reason: "historical".to_owned(),
            revoke_confirmed: index % 2 == 0,
        })
        .collect::<Vec<_>>();
    {
        let mut inner = state.inner.lock().unwrap();
        *inner.engram_retired_work_authority_grants = entries.clone();
        state.commit_locked(&mut inner).unwrap();
        let debug = format!("{:?}", inner.engram_retired_work_authority_grants);
        assert!(!debug.contains("passive-secret-"));
    }
    state.shutdown_persist_blocking();
    let saved = sqlite_metadata_state_value(&state.persistence_path);
    assert_eq!(
        saved["engramRetiredWorkAuthorityGrants"],
        serde_json::to_value(&entries).unwrap()
    );
    let reloaded = AppState::new_with_paths(
        state.default_workdir.clone(),
        state.persistence_path.as_path().to_path_buf(),
        state.orchestrator_templates_path.as_path().to_path_buf(),
    )
    .unwrap();
    assert_eq!(
        reloaded
            .inner
            .lock()
            .unwrap()
            .engram_retired_work_authority_grants,
        entries
    );
    reloaded.shutdown_persist_blocking();
}
