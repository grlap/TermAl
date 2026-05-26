//! Delegation metadata/default validation tests split from `delegations.rs`.
//!
//! This module owns model defaulting, write-policy discriminator compatibility,
//! wait-request defaults, and metadata size validation. It deliberately does not
//! own cwd normalization, wait lifecycle, or delegation result recovery tests.

use super::delegation_support::test_app_state_with_drained_delegation_codex_runtime;
use super::*;

fn test_app_state() -> AppState {
    test_app_state_with_drained_delegation_codex_runtime("delegation-metadata-runtime")
}

#[test]
fn delegation_empty_model_uses_agent_default() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Use the default model.".to_owned(),
                title: Some("Default Model".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: Some("   ".to_owned()),
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    assert_eq!(created.child_session.model, Agent::Codex.default_model());
    assert_eq!(
        created.delegation.model.as_deref(),
        Some(Agent::Codex.default_model())
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_empty_model_uses_configured_agent_default() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.preferences.default_codex_model = "gpt-5.5".to_owned();
    }

    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Use the configured default model.".to_owned(),
                title: Some("Configured Default Model".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: Some("   ".to_owned()),
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    assert_eq!(created.child_session.model, "gpt-5.5");
    assert_eq!(created.delegation.model.as_deref(), Some("gpt-5.5"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_default_model_uses_update_app_settings_normalized_value() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let updated = state
        .update_app_settings(UpdateAppSettingsRequest {
            default_codex_model: Some("  gpt-5.5  ".to_owned()),
            default_claude_model: None,
            default_cursor_model: None,
            default_gemini_model: None,
            default_codex_reasoning_effort: None,
            default_claude_approval_mode: None,
            default_claude_effort: None,
            remotes: None,
        })
        .expect("app settings should update");
    assert_eq!(updated.preferences.default_codex_model, "gpt-5.5");

    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Use the normalized configured default model.".to_owned(),
                title: Some("Normalized Configured Default Model".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: Some("   ".to_owned()),
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    assert_eq!(created.child_session.model, "gpt-5.5");
    assert_eq!(created.delegation.model.as_deref(), Some("gpt-5.5"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_omitted_model_uses_selected_agent_default_not_parent_model() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Claude);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.preferences.default_codex_model = "gpt-5.5".to_owned();
    }
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Use the selected agent default model.".to_owned(),
                title: Some("Selected Agent Default Model".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    assert_eq!(created.child_session.agent, Agent::Codex);
    assert_eq!(created.child_session.model, "gpt-5.5");
    assert_eq!(created.delegation.model.as_deref(), Some("gpt-5.5"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_omitted_agent_and_model_use_parent_agent_default_model() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.preferences.default_codex_model = "gpt-5.5".to_owned();
    }
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Use the parent agent default model.".to_owned(),
                title: Some("Parent Agent Default Model".to_owned()),
                cwd: None,
                agent: None,
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    assert_eq!(created.child_session.agent, Agent::Codex);
    assert_eq!(created.child_session.model, "gpt-5.5");
    assert_eq!(created.delegation.model.as_deref(), Some("gpt-5.5"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_explicit_model_is_preserved_verbatim() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Use the requested model.".to_owned(),
                title: Some("Explicit Model".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: Some("custom-model-string".to_owned()),
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    assert_eq!(created.child_session.agent, Agent::Codex);
    assert_eq!(created.child_session.model, "custom-model-string");
    assert_eq!(
        created.delegation.model.as_deref(),
        Some("custom-model-string")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_write_policy_accepts_legacy_snake_case_discriminators() {
    let read_only: DelegationWritePolicy =
        serde_json::from_value(json!({ "kind": "read_only" })).expect("read_only alias");
    assert_eq!(read_only, DelegationWritePolicy::ReadOnly);

    let shared: DelegationWritePolicy = serde_json::from_value(json!({
        "kind": "shared_worktree",
        "ownedPaths": ["src"]
    }))
    .expect("shared_worktree alias");
    assert_eq!(
        shared,
        DelegationWritePolicy::SharedWorktree {
            owned_paths: vec!["src".to_owned()],
        }
    );

    let isolated: DelegationWritePolicy = serde_json::from_value(json!({
        "kind": "isolated_worktree",
        "ownedPaths": ["src"],
        "worktreePath": "C:/tmp/delegation-worktree"
    }))
    .expect("isolated_worktree alias");
    assert_eq!(
        isolated,
        DelegationWritePolicy::IsolatedWorktree {
            owned_paths: vec!["src".to_owned()],
            worktree_path: Some("C:/tmp/delegation-worktree".to_owned()),
        }
    );

    let isolated_without_path: DelegationWritePolicy = serde_json::from_value(json!({
        "kind": "isolatedWorktree",
        "ownedPaths": []
    }))
    .expect("isolatedWorktree should accept omitted worktreePath");
    assert_eq!(
        isolated_without_path,
        DelegationWritePolicy::IsolatedWorktree {
            owned_paths: Vec::new(),
            worktree_path: None,
        }
    );
}

#[test]
fn create_delegation_wait_request_defaults_to_all_mode() {
    let request: CreateDelegationWaitRequest =
        serde_json::from_value(json!({ "delegationIds": ["delegation-1"] }))
            .expect("wait request should deserialize with defaults");

    assert_eq!(request.delegation_ids, vec!["delegation-1".to_owned()]);
    assert_eq!(request.mode, DelegationWaitMode::All);
    assert_eq!(request.title, None);
}

#[test]
fn delegation_prompt_size_is_capped() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let oversized_prompt = "x".repeat(MAX_DELEGATION_PROMPT_BYTES + 1);
    let err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: oversized_prompt,
            title: None,
            cwd: None,
            agent: Some(Agent::Codex),
            model: None,
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        },
    ) {
        Ok(_) => panic!("oversized prompt should be rejected"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("at most"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_title_size_is_capped() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let oversized_title = "x".repeat(MAX_DELEGATION_TITLE_CHARS + 1);
    let err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: "Review this change.".to_owned(),
            title: Some(oversized_title),
            cwd: None,
            agent: Some(Agent::Codex),
            model: None,
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        },
    ) {
        Ok(_) => panic!("oversized title should be rejected"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("title"));
    assert!(err.message.contains("at most"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_title_size_accepts_trimmed_character_boundary() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let boundary_title = "界".repeat(MAX_DELEGATION_TITLE_CHARS);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review this change.".to_owned(),
                title: Some(format!("  {boundary_title}  ")),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("boundary title should be accepted");

    assert_eq!(created.child_session.name, boundary_title);
    assert_eq!(created.delegation.title, boundary_title);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_model_size_is_capped() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let oversized_model = "x".repeat(MAX_DELEGATION_MODEL_CHARS + 1);
    let err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: "Review this change.".to_owned(),
            title: None,
            cwd: None,
            agent: Some(Agent::Codex),
            model: Some(oversized_model),
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        },
    ) {
        Ok(_) => panic!("oversized model should be rejected"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("model"));
    assert!(err.message.contains("at most"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_model_size_accepts_trimmed_character_boundary() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let boundary_model = "界".repeat(MAX_DELEGATION_MODEL_CHARS);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review this change.".to_owned(),
                title: None,
                cwd: None,
                agent: Some(Agent::Codex),
                model: Some(format!("  {boundary_model}  ")),
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("boundary model should be accepted");

    assert_eq!(created.child_session.model, boundary_model);
    assert_eq!(
        created.delegation.model.as_deref(),
        Some(boundary_model.as_str())
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_metadata_size_errors_precede_phase_one_feature_gates() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let oversized_title = format!("{}界", "x".repeat(MAX_DELEGATION_TITLE_CHARS));
    let title_err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: "Review this change.".to_owned(),
            title: Some(oversized_title),
            cwd: None,
            agent: Some(Agent::Codex),
            model: None,
            mode: Some(DelegationMode::Worker),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        },
    ) {
        Ok(_) => panic!("oversized metadata should be rejected before feature gates"),
        Err(err) => err,
    };

    assert_eq!(title_err.status, StatusCode::BAD_REQUEST);
    assert!(title_err.message.contains("title"));

    let oversized_model = format!("{}界", "x".repeat(MAX_DELEGATION_MODEL_CHARS));
    let model_err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: "Review this change.".to_owned(),
            title: None,
            cwd: None,
            agent: Some(Agent::Codex),
            model: Some(oversized_model),
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::SharedWorktree {
                owned_paths: Vec::new(),
            }),
        },
    ) {
        Ok(_) => panic!("oversized metadata should be rejected before feature gates"),
        Err(err) => err,
    };

    assert_eq!(model_err.status, StatusCode::BAD_REQUEST);
    assert!(model_err.message.contains("model"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_metadata_size_errors_precede_setup_failure_injection() {
    let state = test_app_state();
    state
        .test_agent_setup_failures
        .lock()
        .expect("test agent setup failures mutex poisoned")
        .push((
            Agent::Cursor,
            "forced Cursor setup failure for ordering test".to_owned(),
        ));
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let oversized_title = "x".repeat(MAX_DELEGATION_TITLE_CHARS + 1);
    let title_err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: "Review this change.".to_owned(),
            title: Some(oversized_title),
            cwd: None,
            agent: Some(Agent::Cursor),
            model: None,
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        },
    ) {
        Ok(_) => panic!("oversized title should be rejected before readiness setup"),
        Err(err) => err,
    };

    assert_eq!(title_err.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        title_err.message,
        format!("delegation title must be at most {MAX_DELEGATION_TITLE_CHARS} characters")
    );
    assert!(
        !title_err
            .message
            .contains("forced Cursor setup failure for ordering test"),
        "metadata validation should run before setup failure injection: {}",
        title_err.message
    );

    let oversized_model = "x".repeat(MAX_DELEGATION_MODEL_CHARS + 1);
    let model_err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: "Review this change.".to_owned(),
            title: None,
            cwd: None,
            agent: Some(Agent::Cursor),
            model: Some(oversized_model),
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        },
    ) {
        Ok(_) => panic!("oversized model should be rejected before readiness setup"),
        Err(err) => err,
    };

    assert_eq!(model_err.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        model_err.message,
        format!("delegation model must be at most {MAX_DELEGATION_MODEL_CHARS} characters")
    );
    assert!(
        !model_err
            .message
            .contains("forced Cursor setup failure for ordering test"),
        "metadata validation should run before setup failure injection: {}",
        model_err.message
    );

    let setup_err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: "Review this change.".to_owned(),
            title: Some("Valid metadata".to_owned()),
            cwd: None,
            agent: Some(Agent::Cursor),
            model: None,
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        },
    ) {
        Ok(_) => panic!("forced setup failure should reject valid metadata"),
        Err(err) => err,
    };

    assert_eq!(setup_err.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        setup_err.message,
        "forced Cursor setup failure for ordering test"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}
