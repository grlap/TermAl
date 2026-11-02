//! Historical retired grants remain data; ordinary turns never settle or revoke them.
use super::*;

fn retired_grant(root: &FsPath, grant: &str, confirmed: bool) -> EngramRetiredWorkAuthorityGrant {
    EngramRetiredWorkAuthorityGrant {
        home: root.join("home").to_string_lossy().into_owned(),
        project_root: root.to_string_lossy().into_owned(),
        store_key: None,
        project_id: "fixture-project".to_owned(),
        grant_hash: grant.to_owned(),
        retired_at: "2026-09-01T10:00:00.000Z".to_owned(),
        reason: "Original retirement reason".to_owned(),
        revoke_confirmed: confirmed,
    }
}

fn retained_ledger(inner: &StateInner) -> Value {
    serde_json::to_value(&inner.engram_retired_work_authority_grants).unwrap()
}

#[test]
fn repeated_real_boots_preserve_original_retirements_without_claiming_confirmation() {
    let state = test_app_state();
    let root = state.test_temp_root.as_ref().unwrap().path().join("retirement-history");
    fs::create_dir_all(&root).unwrap();
    let project = create_test_project(&state, &root, "Historical retirement");
    let warning = "Original retirement was not confirmed.";
    let original = {
        let mut inner = state.inner.lock().unwrap();
        inner.engram_retired_work_authority_grants.push(retired_grant(&root, "unconfirmed", false));
        inner.engram_retired_work_authority_grants.push(retired_grant(&root, "confirmed", true));
        inner.projects.iter_mut().find(|entry| entry.id == project).unwrap()
            .engram_cleanup_warning = Some(warning.into());
        state.commit_locked(&mut inner).unwrap();
        retained_ledger(&inner)
    };
    state.shutdown_persist_blocking();
    let restart = root.join("restart.sqlite");
    fs::copy(state.persistence_path.as_path(), &restart).unwrap();
    for _ in 0..2 {
        let loaded = load_state(&restart).unwrap().unwrap();
        assert_eq!(retained_ledger(&loaded), original);
        assert_eq!(loaded.find_project(&project).unwrap().engram_cleanup_warning.as_deref(), Some(warning));
        let booted = AppState::new_with_paths(
            root.to_string_lossy().into_owned(), restart.clone(), root.join("orchestrators.json"),
        ).unwrap();
        booted.shutdown_persist_blocking();
        let inner = booted.inner.lock().unwrap();
        assert_eq!(retained_ledger(&inner), original,
            "boot cannot erase an unconfirmed entry or invent revocation evidence");
        assert_eq!(inner.find_project(&project).unwrap().engram_cleanup_warning.as_deref(), Some(warning));
    }
}

#[test]
fn independent_ordinary_work_never_waits_for_or_rewrites_historical_retirements() {
    let label = "ordinary-beside-retirement-history";
    let (state, receiver) = super::delegation_support::test_app_state_with_delegation_codex_runtime(label);
    state.select_test_scripted_engram_budget_clock();
    let root = state.test_temp_root.as_ref().unwrap().path().join(label);
    fs::create_dir_all(&root).unwrap();
    let project = create_test_project(&state, &root, "Ordinary beside retirement history");
    let session = create_test_project_session(&state, Agent::Codex, &project, &root);
    fs::write(root.join(".engram-project"), format!("{project}\n")).unwrap();
    let transport = ScriptedEngramControlTransport::new([]);
    state.install_control_test_transport(transport.clone());
    let original = {
        let mut inner = state.inner.lock().unwrap();
        inner.projects.iter_mut().find(|entry| entry.id == project).unwrap().engram =
            Some(EngramProjectSettings {
                enabled: true, execution_observability: false,
                home: Some(root.to_string_lossy().into_owned()),
                binary_path: Some(root.join("missing-engram.exe").to_string_lossy().into_owned()),
                ..EngramProjectSettings::default()
            });
        inner.engram_declared_project_ids.insert(project.clone());
        inner.engram_declaration_checked_project_ids.insert(project);
        inner.session_mut(&session).unwrap().engram.context_nudge_pending = false;
        inner.engram_retired_work_authority_grants.push(retired_grant(&root, "unconfirmed", false));
        inner.engram_retired_work_authority_grants.push(retired_grant(&root, "confirmed", true));
        state.commit_locked(&mut inner).unwrap();
        retained_ledger(&inner)
    };
    let text = "Fresh ordinary input beside historical grants.";
    let dispatch = match state.dispatch_turn(&session, SendMessageRequest {
        text: text.into(), expanded_text: None, attachments: Vec::new(),
        source_session_id: None, source_mailbox: None,
    }).unwrap() {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("ordinary idle session must dispatch"),
    };
    let token = dispatch.runtime_token().clone();
    deliver_turn_dispatch(&state, dispatch).into_public_result().unwrap();
    let command = loop {
        if let CodexRuntimeCommand::Prompt { session_id, command } =
            super::phase_sync::receive(&receiver, "ordinary provider prompt beside retirement history")
        {
            assert_eq!(session_id, session);
            break command;
        }
    };
    assert!(command.prompt.contains(text));
    assert!(!receiver.try_iter().any(|command| matches!(command, CodexRuntimeCommand::Prompt { .. })),
        "the ordinary prompt is handed off exactly once");
    state.finish_turn_ok_if_runtime_matches(&session, &token).unwrap();
    state.shutdown_persist_blocking();
    let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    assert_eq!(retained_ledger(&loaded), original);
    assert!(!loaded.engram_retired_work_authority_grants[0].revoke_confirmed);
    assert!(loaded.test_run_observations.is_empty());
    assert!(!receiver.try_iter().any(|command| matches!(command, CodexRuntimeCommand::Prompt { .. })),
        "completion and reload cannot dispatch another prompt");
    assert!(transport.requests().is_empty(), "ordinary work never calls the control process");
}
