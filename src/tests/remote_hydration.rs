//! Remote session hydration and delta localization tests.
//!
//! Split out of remote.rs to keep remote transport setup, hydration, and
//! incremental transcript repair coverage in focused test modules.

use super::remote::{
    make_remote_session_summary_only, remote_text_message,
    seed_remote_proxy_session_via_state_inner_upsert,
    spawn_remote_session_and_state_response_server, spawn_remote_session_response_server,
};
use super::remote_delta_replay::local_replay_test_remote;
use super::*;

#[test]
fn remote_session_created_delta_republishes_metadata_only_session_summary() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    remote_session.messages = vec![remote_text_message(
        "remote-message-1",
        "Remote transcript should stay out of SessionCreated deltas.",
    )];
    remote_session.messages_loaded = true;
    remote_session.message_count = 1;

    let mut delta_receiver = state.subscribe_delta_events();
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: remote_session.id.clone(),
                session: remote_session,
            },
        )
        .expect("remote session create delta should apply");

    let (local_session_id, stored_message_count) = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("remote session should be mirrored locally");
        let record = &inner.sessions[index];
        assert!(record.session.messages_loaded);
        assert_eq!(record.session.messages.len(), 1);
        (record.session.id.clone(), record.session.messages.len())
    };

    let payload = delta_receiver
        .try_recv()
        .expect("localized sessionCreated delta should be published");
    let delta: DeltaEvent = serde_json::from_str(&payload).expect("delta should decode");
    match delta {
        DeltaEvent::SessionCreated {
            revision,
            session_id,
            session,
        } => {
            assert_eq!(revision, state.full_snapshot().revision);
            assert_eq!(session_id, local_session_id);
            assert_eq!(session.id, local_session_id);
            assert!(!session.messages_loaded);
            assert!(session.messages.is_empty());
            assert_eq!(session.message_count, stored_message_count as u32);
        }
        _ => panic!("expected sessionCreated delta"),
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_session_created_duplicate_redelivery_does_not_publish_non_advancing_delta() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    remote_session.messages = vec![remote_text_message(
        "remote-message-1",
        "Remote transcript.",
    )];
    remote_session.messages_loaded = true;
    remote_session.message_count = 1;

    let mut delta_receiver = state.subscribe_delta_events();
    let session_created = || DeltaEvent::SessionCreated {
        revision: 2,
        session_id: remote_session.id.clone(),
        session: remote_session.clone(),
    };

    state
        .apply_remote_delta_event(&remote.id, session_created())
        .expect("first remote session create delta should apply");
    delta_receiver
        .try_recv()
        .expect("first sessionCreated delta should publish");

    state
        .apply_remote_delta_event(&remote.id, session_created())
        .expect("duplicate session create delta should be consumed");
    assert!(
        delta_receiver.try_recv().is_err(),
        "duplicate sessionCreated should not publish a non-advancing delta"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&2));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_session_created_summary_preserves_unloaded_message_count() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    make_remote_session_summary_only(&mut remote_session, 2);

    let mut delta_receiver = state.subscribe_delta_events();
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: remote_session.id.clone(),
                session: remote_session,
            },
        )
        .expect("remote summary session create delta should apply");

    let local_session_id = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("remote session should be mirrored locally");
        let record = &inner.sessions[index];
        assert!(!record.session.messages_loaded);
        assert!(record.session.messages.is_empty());
        assert_eq!(record.session.message_count, 2);
        record.session.id.clone()
    };

    let payload = delta_receiver
        .try_recv()
        .expect("localized sessionCreated delta should be published");
    let delta: DeltaEvent = serde_json::from_str(&payload).expect("delta should decode");
    match delta {
        DeltaEvent::SessionCreated {
            session_id,
            session,
            ..
        } => {
            assert_eq!(session_id, local_session_id);
            assert_eq!(session.id, local_session_id);
            assert!(!session.messages_loaded);
            assert!(session.messages.is_empty());
            assert_eq!(session.message_count, 2);
        }
        _ => panic!("expected sessionCreated delta"),
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_summary_count_decrease_marks_cached_transcript_unloaded() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut full_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    full_remote_session.messages = vec![
        remote_text_message("remote-message-1", "First cached message."),
        remote_text_message("remote-message-2", "Second cached message."),
        remote_text_message("remote-message-3", "Third cached message."),
    ];
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = 3;
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: full_remote_session.id.clone(),
                session: full_remote_session.clone(),
            },
        )
        .expect("remote full session create delta should apply");

    let mut summary_session = full_remote_session;
    make_remote_session_summary_only(&mut summary_session, 1);
    let mut delta_receiver = state.subscribe_delta_events();
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 3,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote summary count decrease should apply");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(!record.session.messages_loaded);
    assert_eq!(record.session.messages.len(), 3);
    assert_eq!(record.session.message_count, 1);
    drop(inner);

    let payload = delta_receiver
        .try_recv()
        .expect("localized summary update should publish a delta");
    let delta: DeltaEvent = serde_json::from_str(&payload).expect("delta should decode");
    match delta {
        DeltaEvent::SessionCreated { session, .. } => {
            assert!(!session.messages_loaded);
            assert!(session.messages.is_empty());
            assert_eq!(session.message_count, 1);
        }
        _ => panic!("expected sessionCreated delta"),
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_summary_same_count_with_new_stamp_marks_cached_transcript_unloaded() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut full_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    full_remote_session.messages = vec![remote_text_message(
        "remote-message-1",
        "Cached text before same-count rewrite.",
    )];
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = 1;
    full_remote_session.session_mutation_stamp = Some(10);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: full_remote_session.id.clone(),
                session: full_remote_session.clone(),
            },
        )
        .expect("remote full session create delta should apply");

    let mut summary_session = full_remote_session;
    make_remote_session_summary_only(&mut summary_session, 1);
    summary_session.preview = "Same-count rewrite happened remotely.".to_owned();
    summary_session.session_mutation_stamp = Some(11);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 3,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote same-count summary with new stamp should apply");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(!record.session.messages_loaded);
    assert_eq!(record.session.messages.len(), 1);
    assert_eq!(record.session.message_count, 1);
    assert_eq!(
        record.session.preview,
        "Same-count rewrite happened remotely."
    );
    assert_eq!(record.session.session_mutation_stamp, Some(11));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_summary_same_count_with_same_stamp_preserves_cached_transcript() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut full_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    full_remote_session.messages = vec![remote_text_message(
        "remote-message-1",
        "Cached text still matches remote stamp.",
    )];
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = 1;
    full_remote_session.session_mutation_stamp = Some(10);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: full_remote_session.id.clone(),
                session: full_remote_session.clone(),
            },
        )
        .expect("remote full session create delta should apply");

    let mut summary_session = full_remote_session;
    make_remote_session_summary_only(&mut summary_session, 1);
    summary_session.session_mutation_stamp = Some(10);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 3,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote same-count summary with same stamp should apply");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(record.session.messages_loaded);
    assert_eq!(record.session.messages.len(), 1);
    assert_eq!(record.session.message_count, 1);
    assert!(matches!(
        &record.session.messages[0],
        Message::Text { text, .. } if text == "Cached text still matches remote stamp."
    ));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_summary_without_stamp_preserves_cached_transcript_and_stamp() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut full_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    full_remote_session.messages = vec![remote_text_message(
        "remote-message-1",
        "Cached text from stamped full transcript.",
    )];
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = 1;
    full_remote_session.session_mutation_stamp = Some(10);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: full_remote_session.id.clone(),
                session: full_remote_session.clone(),
            },
        )
        .expect("remote full session create delta should apply");

    let mut summary_session = full_remote_session;
    make_remote_session_summary_only(&mut summary_session, 1);
    summary_session.preview = "Unstamped metadata refresh.".to_owned();
    summary_session.session_mutation_stamp = None;
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 3,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote unstamped summary should apply");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(record.session.messages_loaded);
    assert_eq!(record.session.messages.len(), 1);
    assert_eq!(record.session.message_count, 1);
    assert_eq!(record.session.preview, "Unstamped metadata refresh.");
    assert_eq!(record.session.session_mutation_stamp, Some(10));
    assert!(matches!(
        &record.session.messages[0],
        Message::Text { text, .. } if text == "Cached text from stamped full transcript."
    ));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_summary_without_any_stamps_preserves_cached_transcript() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut full_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    full_remote_session.messages = vec![remote_text_message(
        "remote-message-1",
        "Cached text from unstamped full transcript.",
    )];
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = 1;
    full_remote_session.session_mutation_stamp = None;
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: full_remote_session.id.clone(),
                session: full_remote_session.clone(),
            },
        )
        .expect("remote unstamped full session create delta should apply");

    let mut summary_session = full_remote_session;
    make_remote_session_summary_only(&mut summary_session, 1);
    summary_session.preview = "Unstamped same-count summary.".to_owned();
    summary_session.session_mutation_stamp = None;
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 3,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote unstamped same-count summary should apply");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(record.session.messages_loaded);
    assert_eq!(record.session.messages.len(), 1);
    assert_eq!(record.session.message_count, 1);
    assert_eq!(record.session.preview, "Unstamped same-count summary.");
    assert_eq!(record.session.session_mutation_stamp, None);
    assert!(matches!(
        &record.session.messages[0],
        Message::Text { text, .. } if text == "Cached text from unstamped full transcript."
    ));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn get_session_hydrates_unloaded_remote_proxy_from_remote_owner() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut full_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    full_remote_session.messages = vec![remote_text_message(
        "remote-message-1",
        "Hydrated transcript from remote owner.",
    )];
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = 1;
    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        3,
        OrchestratorInstanceStatus::Running,
    );
    let mut remote_state_session = full_remote_session.clone();
    make_remote_session_summary_only(&mut remote_state_session, 1);
    remote_state.orchestrators.clear();
    remote_state.sessions = vec![remote_state_session];

    let mut summary_session = full_remote_session.clone();
    make_remote_session_summary_only(&mut summary_session, 1);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote summary session create delta should apply");

    let local_session_id = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("remote proxy session should exist");
        let record = &inner.sessions[index];
        assert!(!record.session.messages_loaded);
        assert!(record.session.messages.is_empty());
        assert_eq!(record.session.message_count, 1);
        record.session.id.clone()
    };

    let (port, requests, server) = spawn_remote_session_and_state_response_server(
        SessionResponse {
            revision: 3,
            session: full_remote_session,
            server_instance_id: "remote-instance".to_owned(),
        },
        Some(remote_state),
    );
    insert_test_remote_connection(&state, &remote, port);

    let response = state
        .get_session(&local_session_id)
        .expect("remote proxy should hydrate from owner");
    assert_eq!(response.session.id, local_session_id);
    assert_eq!(
        response.session.project_id.as_deref(),
        Some(local_project_id.as_str())
    );
    assert!(response.session.messages_loaded);
    assert_eq!(response.session.message_count, 1);
    assert_eq!(response.session.messages.len(), 1);
    assert!(matches!(
        &response.session.messages[0],
        Message::Text { text, .. } if text == "Hydrated transcript from remote owner."
    ));

    let stored = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("remote proxy session should still exist");
        inner.sessions[index].session.clone()
    };
    assert!(stored.messages_loaded);
    assert_eq!(stored.messages.len(), 1);

    let request_lines = requests.lock().expect("requests mutex poisoned").clone();
    assert!(
        request_lines
            .iter()
            .any(|line| line.starts_with("GET /api/sessions/remote-session-1 ")),
        "expected targeted remote session fetch, saw {request_lines:?}"
    );
    assert!(
        request_lines
            .iter()
            .any(|line| line.starts_with("GET /api/state ")),
        "expected remote state resync before applying newer session response, saw {request_lines:?}"
    );
    join_test_server(server);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn get_session_tail_hydrates_unloaded_remote_proxy_before_slicing() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut full_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    full_remote_session.messages = vec![
        remote_text_message("remote-message-1", "Remote line 1."),
        remote_text_message("remote-message-2", "Remote line 2."),
        remote_text_message("remote-message-3", "Remote line 3."),
    ];
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = 3;
    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        3,
        OrchestratorInstanceStatus::Running,
    );
    let mut remote_state_session = full_remote_session.clone();
    make_remote_session_summary_only(&mut remote_state_session, 3);
    remote_state.orchestrators.clear();
    remote_state.sessions = vec![remote_state_session];

    let mut summary_session = full_remote_session.clone();
    make_remote_session_summary_only(&mut summary_session, 3);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote summary session create delta should apply");

    let local_session_id = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("remote proxy session should exist");
        let record = &inner.sessions[index];
        assert!(!record.session.messages_loaded);
        assert!(record.session.messages.is_empty());
        assert_eq!(record.session.message_count, 3);
        record.session.id.clone()
    };

    let (port, requests, server) = spawn_remote_session_and_state_response_server(
        SessionResponse {
            revision: 3,
            session: full_remote_session,
            server_instance_id: "remote-instance".to_owned(),
        },
        Some(remote_state),
    );
    insert_test_remote_connection(&state, &remote, port);

    let response = state
        .get_session_tail(&local_session_id, 2)
        .expect("remote proxy tail should hydrate from owner before slicing");
    assert_eq!(response.session.id, local_session_id);
    assert_eq!(
        response.session.project_id.as_deref(),
        Some(local_project_id.as_str())
    );
    assert!(!response.session.messages_loaded);
    assert_eq!(response.session.message_count, 3);
    assert_eq!(response.session.messages.len(), 2);
    assert!(matches!(
        &response.session.messages[0],
        Message::Text { text, .. } if text == "Remote line 2."
    ));
    assert!(matches!(
        &response.session.messages[1],
        Message::Text { text, .. } if text == "Remote line 3."
    ));

    let stored = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("remote proxy session should still exist");
        inner.sessions[index].session.clone()
    };
    assert!(stored.messages_loaded);
    assert_eq!(stored.messages.len(), 3);

    let request_lines = requests.lock().expect("requests mutex poisoned").clone();
    assert!(
        request_lines
            .iter()
            .any(|line| line.starts_with("GET /api/sessions/remote-session-1 ")),
        "expected targeted remote session fetch, saw {request_lines:?}"
    );
    join_test_server(server);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn get_session_falls_back_to_unloaded_summary_when_remote_owner_returns_summary() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    remote_session.messages = vec![remote_text_message(
        "remote-message-1",
        "Transcript only the owner should have.",
    )];
    remote_session.messages_loaded = true;
    remote_session.message_count = 1;

    let mut local_summary = remote_session.clone();
    make_remote_session_summary_only(&mut local_summary, 1);
    local_summary.preview = "Cached local summary.".to_owned();
    local_summary.session_mutation_stamp = Some(20);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: local_summary.id.clone(),
                session: local_summary,
            },
        )
        .expect("remote summary session create delta should apply");

    let local_session_id = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("remote proxy session should exist");
        inner.sessions[index].session.id.clone()
    };

    let mut owner_summary = remote_session.clone();
    make_remote_session_summary_only(&mut owner_summary, 1);
    owner_summary.preview = "Owner returned only a summary.".to_owned();
    owner_summary.session_mutation_stamp = Some(30);
    let (port, requests, server) = spawn_remote_session_response_server(SessionResponse {
        revision: 3,
        session: owner_summary,
        server_instance_id: "remote-instance".to_owned(),
    });
    insert_test_remote_connection(&state, &remote, port);

    let response = state
        .get_session(&local_session_id)
        .expect("metadata-only owner response should fall back to the cached summary");
    assert_eq!(response.session.id, local_session_id);
    assert_eq!(
        response.session.project_id.as_deref(),
        Some(local_project_id.as_str())
    );
    assert!(!response.session.messages_loaded);
    assert!(response.session.messages.is_empty());
    assert_eq!(response.session.message_count, 1);
    assert_eq!(response.session.preview, "Cached local summary.");
    assert!(response.session.session_mutation_stamp.is_some());

    let stored = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("remote proxy session should still exist");
        inner.sessions[index].session.clone()
    };
    assert!(!stored.messages_loaded);
    assert!(stored.messages.is_empty());
    assert_eq!(stored.message_count, 1);
    assert_eq!(stored.preview, "Cached local summary.");
    assert_eq!(stored.session_mutation_stamp, Some(20));

    let request_lines = requests.lock().expect("requests mutex poisoned").clone();
    assert!(
        request_lines
            .iter()
            .any(|line| line.starts_with("GET /api/sessions/remote-session-1 ")),
        "expected targeted remote session fetch, saw {request_lines:?}"
    );
    assert!(
        request_lines
            .iter()
            .all(|line| !line.starts_with("GET /api/state ")),
        "metadata-only owner response should not trigger a state side fetch, saw {request_lines:?}"
    );
    join_test_server(server);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn get_session_hydration_skips_side_fetch_when_remote_revision_is_already_seen() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut full_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    full_remote_session.messages = vec![remote_text_message(
        "remote-message-1",
        "Hydrated transcript without state side fetch.",
    )];
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = 1;

    let mut summary_session = full_remote_session.clone();
    make_remote_session_summary_only(&mut summary_session, 1);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote summary session create delta should apply");

    let local_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.note_remote_applied_revision(&remote.id, 3);
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("remote proxy session should exist");
        inner.sessions[index].session.id.clone()
    };

    let (port, requests, server) = spawn_remote_session_response_server(SessionResponse {
        revision: 3,
        session: full_remote_session,
        server_instance_id: "remote-instance".to_owned(),
    });
    insert_test_remote_connection(&state, &remote, port);

    let response = state
        .get_session(&local_session_id)
        .expect("remote proxy should hydrate from owner");
    assert!(response.session.messages_loaded);
    assert_eq!(response.session.messages.len(), 1);

    let request_lines = requests.lock().expect("requests mutex poisoned").clone();
    assert!(
        request_lines
            .iter()
            .any(|line| line.starts_with("GET /api/sessions/remote-session-1 ")),
        "expected targeted remote session fetch, saw {request_lines:?}"
    );
    assert!(
        request_lines
            .iter()
            .all(|line| !line.starts_with("GET /api/state ")),
        "remote state resync should be skipped when the watermark already covers the session response, saw {request_lines:?}"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&3));
    drop(inner);

    join_test_server(server);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn get_session_hydration_suppresses_same_revision_delta_for_hydrated_session_only() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let sample_remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    );
    let mut hydrated_session = sample_remote_state
        .sessions
        .iter()
        .find(|session| session.id == "remote-session-1")
        .cloned()
        .expect("sample remote session should exist");
    hydrated_session.messages = vec![remote_text_message("remote-message-1", "Hello world")];
    hydrated_session.messages_loaded = true;
    hydrated_session.message_count = 1;
    hydrated_session.preview = "Hello world".to_owned();
    hydrated_session.session_mutation_stamp = Some(10);

    let mut summary_session = hydrated_session.clone();
    make_remote_session_summary_only(&mut summary_session, 1);
    summary_session.preview = "Hello".to_owned();
    summary_session.session_mutation_stamp = Some(9);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote summary session create delta should apply");

    let mut sibling_session = sample_remote_state
        .sessions
        .iter()
        .find(|session| session.id == "remote-session-2")
        .cloned()
        .expect("sample sibling remote session should exist");
    sibling_session.messages = vec![remote_text_message("remote-message-2", "Other")];
    sibling_session.messages_loaded = true;
    sibling_session.message_count = 1;
    sibling_session.preview = "Other".to_owned();
    sibling_session.session_mutation_stamp = Some(20);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: sibling_session.id.clone(),
                session: sibling_session,
            },
        )
        .expect("sibling remote session create delta should apply");

    let local_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.note_remote_applied_revision(&remote.id, 3);
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("remote proxy session should exist");
        inner.sessions[index].session.id.clone()
    };

    let (port, _requests, server) = spawn_remote_session_response_server(SessionResponse {
        revision: 3,
        session: hydrated_session,
        server_instance_id: "remote-instance".to_owned(),
    });
    insert_test_remote_connection(&state, &remote, port);

    state
        .get_session(&local_session_id)
        .expect("remote proxy should hydrate from owner");

    let mut delta_receiver = state.subscribe_delta_events();
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::TextDelta {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "remote-message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                text_start_byte: None,
                delta: " world".to_owned(),
                preview: Some("Hello world".to_owned()),
                session_mutation_stamp: Some(10),
            },
        )
        .expect("same-revision delta for hydrated session should be skipped");
    assert!(
        delta_receiver.try_recv().is_err(),
        "hydrated-session same-revision delta should not publish"
    );

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::TextDelta {
                revision: 3,
                session_id: "remote-session-2".to_owned(),
                message_id: "remote-message-2".to_owned(),
                message_index: 0,
                message_count: 1,
                text_start_byte: None,
                delta: " updated".to_owned(),
                preview: Some("Other updated".to_owned()),
                session_mutation_stamp: Some(21),
            },
        )
        .expect("same-revision sibling session delta should still apply");
    let published = delta_receiver
        .try_recv()
        .expect("sibling session delta should publish");
    let published_delta: DeltaEvent =
        serde_json::from_str(&published).expect("published delta should decode");
    assert!(matches!(
        published_delta,
        DeltaEvent::TextDelta { delta, .. } if delta == " updated"
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let hydrated_index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("hydrated remote proxy session should exist");
    let sibling_index = inner
        .find_remote_session_index(&remote.id, "remote-session-2")
        .expect("sibling remote proxy session should exist");
    assert!(matches!(
        &inner.sessions[hydrated_index].session.messages[0],
        Message::Text { text, .. } if text == "Hello world"
    ));
    assert!(matches!(
        &inner.sessions[sibling_index].session.messages[0],
        Message::Text { text, .. } if text == "Other updated"
    ));
    let session_watermarks = inner
        .remote_session_transcript_applied_revisions
        .get(&remote.id)
        .expect("remote should have a focused session transcript watermark");
    assert_eq!(session_watermarks.get("remote-session-1"), Some(&3));
    assert_eq!(session_watermarks.get("remote-session-2"), None);
    drop(inner);

    join_test_server(server);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn get_session_propagates_remote_protocol_error_instead_of_cached_summary() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    remote_session.messages = vec![remote_text_message(
        "remote-message-1",
        "Transcript only the owner should have.",
    )];
    remote_session.messages_loaded = true;
    remote_session.message_count = 1;

    let mut local_summary = remote_session.clone();
    make_remote_session_summary_only(&mut local_summary, 1);
    local_summary.preview = "Cached local summary.".to_owned();
    local_summary.session_mutation_stamp = Some(20);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: local_summary.id.clone(),
                session: local_summary,
            },
        )
        .expect("remote summary session create delta should apply");

    let local_session_id = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("remote proxy session should exist");
        inner.sessions[index].session.id.clone()
    };

    remote_session.id = "remote-session-other".to_owned();
    let (port, _requests, server) = spawn_remote_session_response_server(SessionResponse {
        revision: 3,
        session: remote_session,
        server_instance_id: "remote-instance".to_owned(),
    });
    insert_test_remote_connection(&state, &remote, port);

    let result = state.get_session(&local_session_id);
    join_test_server(server);

    let err = match result {
        Ok(_) => panic!("bad owner protocol response should not fall back to cached summary"),
        Err(err) => err,
    };
    assert_eq!(err.status, StatusCode::BAD_GATEWAY);
    assert!(
        err.message.contains("did not match requested session"),
        "unexpected error: {}",
        err.message
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn get_session_returns_local_not_found_when_recoverable_remote_error_loses_cached_summary() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    remote_session.messages = vec![remote_text_message(
        "remote-message-1",
        "Transcript only the owner should have.",
    )];
    remote_session.messages_loaded = true;
    remote_session.message_count = 1;

    let mut local_summary = remote_session.clone();
    make_remote_session_summary_only(&mut local_summary, 1);
    local_summary.preview = "Cached local summary.".to_owned();
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: local_summary.id.clone(),
                session: local_summary,
            },
        )
        .expect("remote summary session create delta should apply");

    let local_session_id = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("remote proxy session should exist");
        inner.sessions[index].session.id.clone()
    };

    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = requests.clone();
    let state_for_server = state.clone();
    let local_session_id_for_server = local_session_id.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "remote local-miss test listener");
            let request = read_test_http_request(&mut stream);
            requests_for_server
                .lock()
                .expect("requests mutex poisoned")
                .push(request.request_line.clone());

            if request.request_line.starts_with("GET /api/health ") {
                write_test_http_response(
                    &mut stream,
                    StatusCode::OK,
                    "application/json",
                    r#"{"ok":true}"#,
                );
                continue;
            }

            if request
                .request_line
                .starts_with("GET /api/sessions/remote-session-1 ")
            {
                let mut inner = state_for_server.inner.lock().expect("state mutex poisoned");
                let index = inner
                    .find_session_index(&local_session_id_for_server)
                    .expect("local proxy should still exist before remote failure");
                inner.remove_session_at(index);
                state_for_server
                    .commit_locked(&mut inner)
                    .expect("local proxy removal should commit");
                // Drop the socket without an HTTP response so the visible
                // hydration path sees a typed recoverable remote-connection
                // miss, then falls back to the now-missing local summary.
                return;
            }

            panic!("unexpected request: {}", request.request_line);
        }
    });
    insert_test_remote_connection(&state, &remote, port);

    let err = match state.get_session(&local_session_id) {
        Ok(_) => panic!("missing cached summary should surface as local not found"),
        Err(err) => err,
    };
    assert_eq!(err.status, StatusCode::NOT_FOUND);
    assert_eq!(err.kind, Some(ApiErrorKind::LocalSessionMissing));
    assert_eq!(err.message, "session not found");

    let request_lines = requests.lock().expect("requests mutex poisoned").clone();
    assert!(
        request_lines
            .iter()
            .any(|line| line.starts_with("GET /api/sessions/remote-session-1 ")),
        "expected targeted remote session fetch, saw {request_lines:?}"
    );
    join_test_server(server);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn get_session_falls_back_to_summary_after_stale_remote_transcript() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut stale_full_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    stale_full_session.messages =
        vec![remote_text_message("remote-message-1", "Stale transcript.")];
    stale_full_session.messages_loaded = true;
    stale_full_session.message_count = 1;
    stale_full_session.preview = "Stale transcript.".to_owned();
    stale_full_session.session_mutation_stamp = Some(30);

    let mut summary_session = stale_full_session.clone();
    make_remote_session_summary_only(&mut summary_session, 1);
    summary_session.session_mutation_stamp = Some(20);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote summary session create delta should apply");

    let local_session_id = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("remote proxy session should exist");
        inner.sessions[index].session.id.clone()
    };

    let mut newer_remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        4,
        OrchestratorInstanceStatus::Running,
    );
    let mut newer_summary = stale_full_session.clone();
    make_remote_session_summary_only(&mut newer_summary, 1);
    newer_summary.preview = "Newer summary from state.".to_owned();
    newer_summary.session_mutation_stamp = Some(40);
    newer_remote_state.orchestrators.clear();
    newer_remote_state.sessions = vec![newer_summary];

    let (port, requests, server) = spawn_remote_session_and_state_response_server(
        SessionResponse {
            revision: 3,
            session: stale_full_session,
            server_instance_id: "remote-instance".to_owned(),
        },
        Some(newer_remote_state),
    );
    insert_test_remote_connection(&state, &remote, port);

    let response = state
        .get_session(&local_session_id)
        .expect("stale remote transcript should fall back to the local summary");
    assert_eq!(response.session.id, local_session_id);
    assert!(!response.session.messages_loaded);
    assert!(response.session.messages.is_empty());
    assert_eq!(response.session.message_count, 1);
    assert_eq!(response.session.preview, "Newer summary from state.");
    assert!(response.session.session_mutation_stamp.is_some());

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should still exist");
    let record = &inner.sessions[index];
    assert!(!record.session.messages_loaded);
    assert!(record.session.messages.is_empty());
    assert_eq!(record.session.message_count, 1);
    assert_eq!(record.session.preview, "Newer summary from state.");
    assert_eq!(record.session.session_mutation_stamp, Some(40));
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&4));
    drop(inner);

    let request_lines = requests.lock().expect("requests mutex poisoned").clone();
    assert!(
        request_lines
            .iter()
            .any(|line| line.starts_with("GET /api/sessions/remote-session-1 ")),
        "expected targeted remote session fetch, saw {request_lines:?}"
    );
    assert!(
        request_lines
            .iter()
            .any(|line| line.starts_with("GET /api/state ")),
        "expected remote state resync before falling back, saw {request_lines:?}"
    );

    join_test_server(server);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_message_delta_hydrates_unloaded_proxy_before_gap_check() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut full_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    full_remote_session.messages = vec![
        remote_text_message("remote-message-1", "Existing remote transcript."),
        remote_text_message(
            "remote-message-2",
            "New remote message from delta revision.",
        ),
    ];
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = 2;
    full_remote_session.session_mutation_stamp = Some(11);

    let mut summary_session = full_remote_session.clone();
    make_remote_session_summary_only(&mut summary_session, 1);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote summary session create delta should apply");

    let (port, _requests, server) = spawn_remote_session_response_server(SessionResponse {
        revision: 3,
        session: full_remote_session,
        server_instance_id: "remote-instance".to_owned(),
    });
    insert_test_remote_connection(&state, &remote, port);

    let mut delta_receiver = state.subscribe_delta_events();
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageCreated {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "remote-message-2".to_owned(),
                message_index: 1,
                message_count: 2,
                message: remote_text_message(
                    "remote-message-2",
                    "New remote message from delta revision.",
                ),
                preview: "New remote message from delta revision.".to_owned(),
                status: SessionStatus::Idle,
                session_mutation_stamp: Some(11),
            },
        )
        .expect("unloaded proxy should hydrate instead of failing gap check");

    assert!(
        delta_receiver.try_recv().is_err(),
        "targeted hydration should publish a state snapshot, not replay the skipped delta"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(record.session.messages_loaded);
    assert_eq!(record.session.message_count, 2);
    assert_eq!(record.session.messages.len(), 2);
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&3));
    drop(inner);

    join_test_server(server);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_text_delta_targeted_hydration_accepts_newer_global_revision_with_matching_metadata() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let sample_remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    );
    let mut full_remote_session = sample_remote_state
        .sessions
        .iter()
        .find(|session| session.id == "remote-session-1")
        .cloned()
        .expect("sample remote session should exist");
    full_remote_session.messages = vec![remote_text_message("remote-message-1", "Hello world")];
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = 1;
    full_remote_session.preview = "Hello world".to_owned();
    full_remote_session.session_mutation_stamp = Some(10);

    let mut summary_session = full_remote_session.clone();
    make_remote_session_summary_only(&mut summary_session, 1);
    summary_session.preview = "Hello".to_owned();
    summary_session.session_mutation_stamp = Some(9);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote summary session create delta should apply");

    let mut sibling_session = sample_remote_state
        .sessions
        .iter()
        .find(|session| session.id == "remote-session-2")
        .cloned()
        .expect("sample sibling remote session should exist");
    sibling_session.messages = vec![remote_text_message("remote-message-2", "Other")];
    sibling_session.messages_loaded = true;
    sibling_session.message_count = 1;
    sibling_session.preview = "Other".to_owned();
    sibling_session.session_mutation_stamp = Some(20);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: sibling_session.id.clone(),
                session: sibling_session,
            },
        )
        .expect("sibling remote session create delta should apply");

    let (port, _requests, server) = spawn_remote_session_response_server(SessionResponse {
        // The remote revision is global, so a busy upstream can already be
        // ahead of the triggering delta even when this session's transcript is
        // exactly at the delta's post-state.
        revision: 5,
        session: full_remote_session,
        server_instance_id: "remote-instance".to_owned(),
    });
    insert_test_remote_connection(&state, &remote, port);

    let mut delta_receiver = state.subscribe_delta_events();
    let replayed_delta = || DeltaEvent::TextDelta {
        revision: 3,
        session_id: "remote-session-1".to_owned(),
        message_id: "remote-message-1".to_owned(),
        message_index: 0,
        message_count: 1,
        text_start_byte: None,
        delta: " world".to_owned(),
        preview: Some("Hello world".to_owned()),
        session_mutation_stamp: Some(10),
    };

    state
        .apply_remote_delta_event(&remote.id, replayed_delta())
        .expect("unloaded proxy should hydrate before applying text delta");
    assert!(
        delta_receiver.try_recv().is_err(),
        "targeted hydration should not replay the triggering text delta"
    );

    state
        .apply_remote_delta_event(&remote.id, replayed_delta())
        .expect("replayed text delta should be skipped after hydration");
    assert!(
        delta_receiver.try_recv().is_err(),
        "same-revision replay should not publish a duplicate text delta"
    );

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageUpdated {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "remote-message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                message: remote_text_message("remote-message-1", "Hello world"),
                preview: "Reviewed remote message.".to_owned(),
                status: SessionStatus::Idle,
                session_mutation_stamp: Some(10),
            },
        )
        .expect("same-session sibling delta should be skipped by transcript hydration");
    assert!(
        delta_receiver.try_recv().is_err(),
        "same-session sibling delta at the hydrated revision should not publish"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(record.session.messages_loaded);
    assert_eq!(record.session.message_count, 1);
    assert_eq!(record.session.session_mutation_stamp, Some(10));
    assert!(matches!(
        &record.session.messages[0],
        Message::Text { text, .. } if text == "Hello world"
    ));
    assert_eq!(record.session.preview, "Hello world");
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&3));
    assert_eq!(
        inner
            .remote_session_transcript_applied_revisions
            .get(&remote.id)
            .and_then(|sessions| sessions.get("remote-session-1")),
        Some(&5)
    );
    assert!(
        !inner.should_skip_remote_applied_delta_revision(&remote.id, 4),
        "targeted hydration must not broadly skip unrelated intermediate deltas"
    );
    assert!(
        inner.should_skip_remote_session_applied_delta_revision(&remote.id, "remote-session-1", 4,),
        "targeted hydration should skip later stale deltas for the repaired session"
    );
    drop(inner);

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::TextDelta {
                revision: 4,
                session_id: "remote-session-2".to_owned(),
                message_id: "remote-message-2".to_owned(),
                message_index: 0,
                message_count: 1,
                text_start_byte: None,
                delta: " updated".to_owned(),
                preview: Some("Other updated".to_owned()),
                session_mutation_stamp: Some(21),
            },
        )
        .expect("intermediate sibling session delta should still apply");
    let published = delta_receiver
        .try_recv()
        .expect("intermediate sibling session delta should publish");
    let published_delta: DeltaEvent =
        serde_json::from_str(&published).expect("published delta should decode");
    assert!(matches!(
        published_delta,
        DeltaEvent::TextDelta { delta, .. } if delta == " updated"
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let sibling_index = inner
        .find_remote_session_index(&remote.id, "remote-session-2")
        .expect("sibling remote proxy session should exist");
    assert!(matches!(
        &inner.sessions[sibling_index].session.messages[0],
        Message::Text { text, .. } if text == "Other updated"
    ));
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&4));
    drop(inner);

    join_test_server(server);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins remote-backed session creation to materializing the same app-level
// default model as local session creation before forwarding the create request.
// Guards against remote sessions silently falling back to the remote host's
// built-in default when the local UI request omits `model`.
#[test]
fn remote_session_create_forwards_configured_default_model() {
    let captured_body = Arc::new(Mutex::new(None::<Value>));
    let captured_body_for_server = captured_body.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let remote_session = Session {
        id: "remote-session-default-model".to_owned(),
        name: "Remote Default Model".to_owned(),
        emoji: Agent::Codex.avatar().to_owned(),
        agent: Agent::Codex,
        workdir: "/remote/repo".to_owned(),
        project_id: Some("remote-project-default-model".to_owned()),
        remote_id: None,
        model: "gpt-5.5".to_owned(),
        model_options: Vec::new(),
        approval_policy: Some(default_codex_approval_policy()),
        reasoning_effort: Some(default_codex_reasoning_effort()),
        sandbox_mode: Some(default_codex_sandbox_mode()),
        cursor_mode: None,
        claude_effort: None,
        claude_approval_mode: None,
        gemini_approval_mode: None,
        external_session_id: None,
        agent_commands_revision: 0,
        codex_thread_state: None,
        status: SessionStatus::Idle,
        preview: "Remote Default Model ready.".to_owned(),
        messages: Vec::new(),
        messages_loaded: true,
        message_count: 0,
        markers: Vec::new(),
        pending_prompts: Vec::new(),
        session_mutation_stamp: None,
        parent_delegation_id: None,
    };
    let remote_response = serde_json::to_string(&CreateSessionResponse {
        session_id: remote_session.id.clone(),
        session: remote_session,
        revision: 7,
        server_instance_id: "remote-server".to_owned(),
    })
    .expect("remote create response should encode");
    let server = std::thread::spawn(move || {
        loop {
            let mut stream = accept_test_connection(&listener, "remote session create listener");
            let request = read_test_http_request(&mut stream);
            if request.request_line.starts_with("GET /api/health ") {
                write_test_http_response(
                    &mut stream,
                    StatusCode::OK,
                    "application/json",
                    r#"{"ok":true}"#,
                );
                continue;
            }

            if request.request_line.starts_with("POST /api/sessions ") {
                *captured_body_for_server
                    .lock()
                    .expect("captured body mutex poisoned") =
                    Some(serde_json::from_str(&request.body).expect("create body should decode"));
                write_test_http_response(
                    &mut stream,
                    StatusCode::OK,
                    "application/json",
                    &remote_response,
                );
                break;
            }

            panic!("unexpected request: {}", request.request_line);
        }
    });

    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-default-model".to_owned(),
        name: "SSH Default Model".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    state
        .update_app_settings(UpdateAppSettingsRequest {
            default_codex_model: Some("gpt-5.5".to_owned()),
            default_claude_model: None,
            default_cursor_model: None,
            default_gemini_model: None,
            default_codex_reasoning_effort: None,
            default_claude_approval_mode: None,
            default_claude_effort: None,
            remotes: Some(vec![RemoteConfig::local(), remote.clone()]),
        })
        .unwrap();
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Default Model",
        "remote-project-default-model",
    );
    insert_test_remote_connection(&state, &remote, port);

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Remote Default Model".to_owned()),
            workdir: None,
            project_id: Some(local_project_id.clone()),
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    assert_eq!(created.session.model, "gpt-5.5");
    assert_eq!(
        created.session.project_id.as_deref(),
        Some(local_project_id.as_str())
    );
    let body = captured_body
        .lock()
        .expect("captured body mutex poisoned")
        .clone()
        .expect("remote create request should be captured");
    assert_eq!(body["model"], Value::String("gpt-5.5".to_owned()));
    assert_eq!(
        body["projectId"],
        Value::String("remote-project-default-model".to_owned())
    );

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_delegation_delta_advances_revision_without_local_record() {
    let state = test_app_state();
    let remote = local_replay_test_remote();
    let delegation = DelegationSummary {
        id: "remote-delegation-1".to_owned(),
        parent_session_id: "remote-parent-session".to_owned(),
        child_session_id: "remote-child-session".to_owned(),
        mode: DelegationMode::Reviewer,
        status: DelegationStatus::Running,
        title: "Remote delegation".to_owned(),
        agent: Agent::Codex,
        model: None,
        write_policy: DelegationWritePolicy::ReadOnly,
        created_at: "2026-04-05 10:00:00".to_owned(),
        started_at: Some("2026-04-05 10:00:01".to_owned()),
        completed_at: None,
        result: None,
    };
    let result = DelegationResultSummary {
        delegation_id: delegation.id.clone(),
        child_session_id: delegation.child_session_id.clone(),
        status: DelegationStatus::Completed,
        summary: "Remote delegation completed.".to_owned(),
    };
    let events = vec![
        DeltaEvent::DelegationCreated {
            revision: 7,
            delegation: delegation.clone(),
        },
        DeltaEvent::DelegationUpdated {
            revision: 8,
            delegation_id: delegation.id.clone(),
            status: DelegationStatus::Running,
            updated_at: "2026-04-05 10:00:02".to_owned(),
        },
        DeltaEvent::DelegationCompleted {
            revision: 9,
            delegation_id: delegation.id.clone(),
            result: result.clone(),
            completed_at: "2026-04-05 10:00:03".to_owned(),
        },
        DeltaEvent::DelegationFailed {
            revision: 10,
            delegation_id: delegation.id.clone(),
            result: DelegationResultSummary {
                status: DelegationStatus::Failed,
                summary: "Remote delegation failed.".to_owned(),
                ..result.clone()
            },
            failed_at: "2026-04-05 10:00:04".to_owned(),
        },
        DeltaEvent::DelegationCanceled {
            revision: 11,
            delegation_id: delegation.id.clone(),
            canceled_at: "2026-04-05 10:00:05".to_owned(),
            reason: Some("remote cancel".to_owned()),
        },
    ];

    for event in events {
        assert!(
            AppState::remote_delta_replay_key(&remote.id, &event).is_none(),
            "remote delegation deltas should not enter replay-key suppression"
        );
        state
            .apply_remote_delta_event(&remote.id, event)
            .expect("remote delegation delta should be consumed as a no-op");
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner.delegations.is_empty(),
        "remote delegation deltas must not materialize local delegation records"
    );
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&11));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn inbound_remote_session_remote_id_is_replaced_by_connection_metadata() {
    let state = test_app_state();
    let remote = local_replay_test_remote();
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let mut remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    remote_session.remote_id = Some("attacker-remote".to_owned());

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let local_session_id = upsert_remote_proxy_session_record(
            &mut inner,
            &remote.id,
            &remote_session,
            Some(local_project_id),
        );
        let index = inner
            .find_remote_session_index(&remote.id, &remote_session.id)
            .expect("remote proxy session should exist");
        let record = &inner.sessions[index];

        assert_eq!(record.session.id, local_session_id);
        assert_eq!(record.remote_id.as_deref(), Some(remote.id.as_str()));
        assert_eq!(
            record.remote_session_id.as_deref(),
            Some(remote_session.id.as_str())
        );
        assert!(
            record.session.remote_id.is_none(),
            "embedded session state must not retain untrusted inbound remote_id"
        );

        let summary = AppState::wire_session_summary_from_record(record);
        let full = AppState::wire_session_from_record(record);
        assert_eq!(summary.remote_id.as_deref(), Some(remote.id.as_str()));
        assert_eq!(full.remote_id.as_deref(), Some(remote.id.as_str()));
        assert_ne!(summary.remote_id.as_deref(), Some("attacker-remote"));
        assert_ne!(full.remote_id.as_deref(), Some("attacker-remote"));

        state
            .commit_locked(&mut inner)
            .expect("remote proxy session should persist");
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn full_remote_state_snapshot_suppresses_same_revision_deltas() {
    let state = test_app_state();
    let remote = local_replay_test_remote();
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    );
    remote_state.revision = 3;
    let remote_session = remote_state
        .sessions
        .iter_mut()
        .find(|session| session.id == "remote-session-1")
        .expect("sample remote session should exist");
    remote_session.messages = vec![remote_text_message("remote-message-1", "Hello")];
    remote_session.messages_loaded = true;
    remote_session.message_count = 1;
    remote_session.preview = "Hello".to_owned();
    remote_session.session_mutation_stamp = Some(10);

    state
        .apply_remote_state_snapshot(&remote.id, remote_state)
        .expect("full remote state snapshot should apply");

    let mut delta_receiver = state.subscribe_delta_events();
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::TextDelta {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "remote-message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                text_start_byte: None,
                delta: " world".to_owned(),
                preview: Some("Hello world".to_owned()),
                session_mutation_stamp: Some(11),
            },
        )
        .expect("same-revision delta after full snapshot should be skipped");

    assert!(
        delta_receiver.try_recv().is_err(),
        "same-revision delta after a full snapshot must not publish"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(matches!(
        &record.session.messages[0],
        Message::Text { text, .. } if text == "Hello"
    ));
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&3));
    assert_eq!(
        inner.remote_snapshot_applied_revisions.get(&remote.id),
        Some(&3)
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn metadata_remote_state_snapshot_allows_same_revision_transcript_delta() {
    let state = test_app_state();
    let remote = local_replay_test_remote();
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut full_remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    );
    full_remote_state.revision = 2;
    let full_remote_session = full_remote_state
        .sessions
        .iter_mut()
        .find(|session| session.id == "remote-session-1")
        .expect("sample remote session should exist");
    full_remote_session.messages = vec![remote_text_message("remote-message-1", "Hello")];
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = 1;
    full_remote_session.preview = "Hello".to_owned();
    full_remote_session.session_mutation_stamp = Some(10);

    state
        .apply_remote_state_snapshot(&remote.id, full_remote_state)
        .expect("full remote state snapshot should apply");

    let mut metadata_remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    );
    metadata_remote_state.revision = 3;
    let metadata_remote_session = metadata_remote_state
        .sessions
        .iter_mut()
        .find(|session| session.id == "remote-session-1")
        .expect("sample remote session should exist");
    metadata_remote_session.messages.clear();
    metadata_remote_session.messages_loaded = false;
    metadata_remote_session.message_count = 1;
    metadata_remote_session.preview = "Hello".to_owned();
    metadata_remote_session.session_mutation_stamp = Some(10);

    state
        .apply_remote_state_snapshot(&remote.id, metadata_remote_state)
        .expect("metadata remote state snapshot should apply");

    let mut delta_receiver = state.subscribe_delta_events();
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::TextDelta {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "remote-message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                text_start_byte: None,
                delta: " world".to_owned(),
                preview: Some("Hello world".to_owned()),
                session_mutation_stamp: Some(11),
            },
        )
        .expect("same-revision delta after metadata snapshot should apply");

    let published = delta_receiver
        .try_recv()
        .expect("same-revision text delta should publish after metadata snapshot");
    let published_delta: DeltaEvent =
        serde_json::from_str(&published).expect("published delta should decode");
    assert!(matches!(
        published_delta,
        DeltaEvent::TextDelta {
            revision: _,
            delta,
            ..
        } if delta == " world"
    ));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(matches!(
        &record.session.messages[0],
        Message::Text { text, .. } if text == "Hello world"
    ));
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&3));
    assert_eq!(
        inner.remote_snapshot_applied_revisions.get(&remote.id),
        Some(&3)
    );
    assert_eq!(
        inner
            .remote_transcript_snapshot_applied_revisions
            .get(&remote.id),
        Some(&2)
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn targeted_remote_hydration_rejects_message_count_length_mismatch() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut full_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    full_remote_session.messages = vec![remote_text_message("remote-message-1", "Hello world")];
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = 2;
    full_remote_session.session_mutation_stamp = Some(10);

    let mut summary_session = full_remote_session.clone();
    make_remote_session_summary_only(&mut summary_session, 1);
    summary_session.session_mutation_stamp = Some(9);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote summary session create delta should apply");

    let (port, _requests, server) = spawn_remote_session_response_server(SessionResponse {
        revision: 3,
        session: full_remote_session,
        server_instance_id: "remote-instance".to_owned(),
    });
    insert_test_remote_connection(&state, &remote, port);

    let err = state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::TextDelta {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "remote-message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                text_start_byte: None,
                delta: " world".to_owned(),
                preview: Some("Hello world".to_owned()),
                session_mutation_stamp: Some(10),
            },
        )
        .expect_err("targeted hydration should reject inconsistent full transcript");
    assert!(
        err.to_string()
            .contains("did not match loaded transcript length"),
        "unexpected error: {err:#}"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(!record.session.messages_loaded);
    assert_eq!(record.session.message_count, 1);
    assert!(record.session.messages.is_empty());
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&2));
    drop(inner);

    join_test_server(server);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_delta_falls_through_when_targeted_hydration_returns_summary() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut summary_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    make_remote_session_summary_only(&mut summary_session, 1);
    summary_session.preview = "Remote summary before delta".to_owned();
    summary_session.session_mutation_stamp = Some(9);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: summary_session.id.clone(),
                session: summary_session.clone(),
            },
        )
        .expect("remote summary session create delta should apply");

    let mut upstream_summary = summary_session;
    upstream_summary.preview = "Chained upstream summary".to_owned();
    upstream_summary.session_mutation_stamp = Some(10);
    let (port, requests, server) = spawn_remote_session_response_server(SessionResponse {
        revision: 3,
        session: upstream_summary,
        server_instance_id: "remote-instance".to_owned(),
    });
    insert_test_remote_connection(&state, &remote, port);

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageCreated {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "remote-message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                message: remote_text_message("remote-message-1", "Delta repaired transcript."),
                preview: "Delta repaired transcript.".to_owned(),
                status: SessionStatus::Idle,
                session_mutation_stamp: Some(10),
            },
        )
        .expect("summary-only targeted hydration should fall through to delta apply");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(!record.session.messages_loaded);
    assert_eq!(record.session.message_count, 1);
    assert_eq!(record.session.preview, "Delta repaired transcript.");
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&3));
    assert!(matches!(
        record.session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Delta repaired transcript."
    ));
    drop(inner);

    let request_lines = requests.lock().expect("requests mutex poisoned").clone();
    assert!(
        request_lines
            .iter()
            .any(|line| line.starts_with("GET /api/sessions/remote-session-1 ")),
        "expected targeted remote session fetch, saw {request_lines:?}"
    );
    join_test_server(server);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_delta_repair_rejects_newer_targeted_session_revision() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut full_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    full_remote_session.messages = vec![
        remote_text_message("remote-message-1", "Existing remote transcript."),
        remote_text_message("remote-message-2", "Newer transcript from future revision."),
    ];
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = 2;
    full_remote_session.session_mutation_stamp = Some(99);

    let mut summary_session = full_remote_session.clone();
    make_remote_session_summary_only(&mut summary_session, 1);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote summary session create delta should apply");

    let (port, requests, server) = spawn_remote_session_response_server(SessionResponse {
        revision: 5,
        session: full_remote_session,
        server_instance_id: "remote-instance".to_owned(),
    });
    insert_test_remote_connection(&state, &remote, port);

    let error = state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageCreated {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "remote-message-2".to_owned(),
                message_index: 1,
                message_count: 2,
                message: remote_text_message(
                    "remote-message-2",
                    "Newer transcript from future revision.",
                ),
                preview: "Newer transcript from future revision.".to_owned(),
                status: SessionStatus::Idle,
                session_mutation_stamp: Some(40),
            },
        )
        .expect_err("newer targeted session response should reject narrow repair");
    assert!(
        format!("{error:#}").contains("newer than targeted repair revision 3"),
        "unexpected error: {error:#}"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(!record.session.messages_loaded);
    assert!(record.session.messages.is_empty());
    assert_eq!(record.session.message_count, 1);
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&2));
    drop(inner);

    let request_lines = requests.lock().expect("requests mutex poisoned").clone();
    assert!(
        request_lines
            .iter()
            .any(|line| line.starts_with("GET /api/sessions/remote-session-1 ")),
        "expected targeted remote session fetch, saw {request_lines:?}"
    );
    join_test_server(server);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn stale_remote_delta_skips_before_targeted_hydration_fetch() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut summary_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    make_remote_session_summary_only(&mut summary_session, 1);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 5,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote summary session create delta should apply");

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageCreated {
                revision: 4,
                session_id: "remote-session-1".to_owned(),
                message_id: "remote-message-2".to_owned(),
                message_index: 1,
                message_count: 2,
                message: remote_text_message(
                    "remote-message-2",
                    "Stale delta should not trigger a remote fetch.",
                ),
                preview: "Stale delta should not trigger a remote fetch.".to_owned(),
                status: SessionStatus::Idle,
                session_mutation_stamp: Some(60),
            },
        )
        .expect("stale unloaded-proxy delta should be skipped before hydration");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(!record.session.messages_loaded);
    assert!(record.session.messages.is_empty());
    assert_eq!(record.session.message_count, 1);
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&5));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

fn apply_remote_created_text_message_at(
    state: &AppState,
    remote_id: &str,
    revision: u64,
    message_id: &str,
    message_index: usize,
    message_count: u32,
    text: &str,
) {
    state
        .apply_remote_delta_event(
            remote_id,
            DeltaEvent::MessageCreated {
                revision,
                session_id: "remote-session-1".to_owned(),
                message_id: message_id.to_owned(),
                message_index,
                message_count,
                message: remote_text_message(message_id, text),
                preview: text.to_owned(),
                status: SessionStatus::Active,
                session_mutation_stamp: None,
            },
        )
        .expect("remote message create delta should apply");
}

fn apply_remote_created_text_message(
    state: &AppState,
    remote_id: &str,
    revision: u64,
    message_id: &str,
    text: &str,
) {
    apply_remote_created_text_message_at(state, remote_id, revision, message_id, 0, 1, text);
}

#[test]
fn remote_summary_state_snapshot_preserves_existing_proxy_transcript() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_session_id = seed_remote_proxy_session_via_state_inner_upsert(&state, &remote);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageCreated {
                revision: 2,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                message: remote_text_message("message-1", "Hydrated remote transcript."),
                preview: "Hydrated remote transcript.".to_owned(),
                status: SessionStatus::Active,
                session_mutation_stamp: Some(42),
            },
        )
        .expect("remote message create delta should apply");

    let mut remote_state = state.full_snapshot();
    remote_state.revision = 3;
    let mut remote_session = remote_state
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .cloned()
        .expect("local proxy should be present in snapshot");
    remote_session.id = "remote-session-1".to_owned();
    remote_session.preview = "Summary-only remote update.".to_owned();
    remote_session.messages.clear();
    remote_session.messages_loaded = false;
    remote_session.session_mutation_stamp = Some(42);
    remote_state.sessions = vec![remote_session];

    state
        .apply_remote_state_snapshot(&remote.id, remote_state)
        .expect("summary remote snapshot should apply");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .find_session_index(&local_session_id)
        .and_then(|index| inner.sessions.get(index))
        .expect("local proxy should remain");
    assert_eq!(record.session.preview, "Summary-only remote update.");
    assert!(record.session.messages_loaded);
    assert_eq!(record.session.messages.len(), 1);
    assert_eq!(record.session.messages[0].id(), "message-1");
    assert_eq!(record.message_positions.get("message-1").copied(), Some(0));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_message_created_delta_replaces_and_reorders_existing_message() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_session_id = seed_remote_proxy_session_via_state_inner_upsert(&state, &remote);
    apply_remote_created_text_message(&state, &remote.id, 2, "message-1", "First remote message.");
    apply_remote_created_text_message_at(&state, &remote.id, 3, "message-2", 1, 2, "Second draft.");
    let mut delta_receiver = state.subscribe_delta_events();

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageCreated {
                revision: 4,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-2".to_owned(),
                message_index: 0,
                message_count: 2,
                message: remote_text_message("message-2", "Second final."),
                preview: "Second final.".to_owned(),
                status: SessionStatus::Idle,
                session_mutation_stamp: None,
            },
        )
        .expect("remote message create replay should replace and reorder by id");

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    assert_eq!(session.preview, "Second final.");
    assert_eq!(session.status, SessionStatus::Idle);
    let message_ids: Vec<_> = session
        .messages
        .iter()
        .map(|message| message.id().to_owned())
        .collect();
    assert_eq!(message_ids, vec!["message-2", "message-1"]);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Second final."
    ));
    assert!(matches!(
        session.messages.get(1),
        Some(Message::Text { text, .. }) if text == "First remote message."
    ));

    let delta: DeltaEvent = serde_json::from_str(
        &delta_receiver
            .try_recv()
            .expect("remote message create replay should publish a localized delta"),
    )
    .expect("message create delta should decode");
    match delta {
        DeltaEvent::MessageCreated {
            revision,
            session_id,
            message_id,
            message_index,
            message_count,
            message,
            preview,
            status,
            session_mutation_stamp,
        } => {
            assert_eq!(revision, snapshot.revision);
            assert_eq!(session_id, local_session_id);
            assert_eq!(message_id, "message-2");
            assert_eq!(message_index, 0);
            assert_eq!(message_count, 2);
            assert!(matches!(
                message,
                Message::Text { text, .. } if text == "Second final."
            ));
            assert_eq!(preview, "Second final.");
            assert_eq!(status, SessionStatus::Idle);
            assert_eq!(session_mutation_stamp, session.session_mutation_stamp);
        }
        _ => panic!("expected localized MessageCreated delta"),
    }
    assert!(
        delta_receiver.try_recv().is_err(),
        "remote message create replay should publish exactly one delta"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.should_skip_remote_applied_delta_revision(&remote.id, 4));
    assert!(inner.should_skip_remote_applied_delta_revision(&remote.id, 3));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_message_created_delta_rejects_gap_without_advancing_revision() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_session_id = seed_remote_proxy_session_via_state_inner_upsert(&state, &remote);
    let initial_revision = state.full_snapshot().revision;
    let mut delta_receiver = state.subscribe_delta_events();

    let error = state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageCreated {
                revision: 2,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-1".to_owned(),
                message_index: 1,
                message_count: 1,
                message: remote_text_message("message-1", "Gap message."),
                preview: "Gap message.".to_owned(),
                status: SessionStatus::Active,
                session_mutation_stamp: None,
            },
        )
        .expect_err("gap remote MessageCreated should request resync");
    assert!(
        error.to_string().contains("leaves a gap"),
        "unexpected error: {error:#}"
    );
    assert!(
        delta_receiver.try_recv().is_err(),
        "gap MessageCreated should not publish a local delta"
    );

    let snapshot = state.full_snapshot();
    assert_eq!(snapshot.revision, initial_revision);
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    assert!(session.messages.is_empty());

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.should_skip_remote_applied_delta_revision(&remote.id, 2));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_message_created_delta_rejects_payload_id_mismatch_without_advancing_revision() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_session_id = seed_remote_proxy_session_via_state_inner_upsert(&state, &remote);
    let initial_revision = state.full_snapshot().revision;
    let mut delta_receiver = state.subscribe_delta_events();

    let error = state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageCreated {
                revision: 2,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                message: remote_text_message("different-message", "Wrong message."),
                preview: "Wrong message.".to_owned(),
                status: SessionStatus::Active,
                session_mutation_stamp: None,
            },
        )
        .expect_err("id-mismatched remote MessageCreated should request resync");
    assert!(
        error.to_string().contains("payload id"),
        "unexpected error: {error:#}"
    );
    assert!(
        delta_receiver.try_recv().is_err(),
        "id-mismatched MessageCreated should not publish a local delta"
    );

    let snapshot = state.full_snapshot();
    assert_eq!(snapshot.revision, initial_revision);
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    assert!(session.messages.is_empty());

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.should_skip_remote_applied_delta_revision(&remote.id, 2));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_message_created_delta_rejects_existing_message_out_of_bounds_without_advancing_revision()
{
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_session_id = seed_remote_proxy_session_via_state_inner_upsert(&state, &remote);
    apply_remote_created_text_message(&state, &remote.id, 2, "message-1", "First message.");
    apply_remote_created_text_message_at(
        &state,
        &remote.id,
        3,
        "message-2",
        1,
        2,
        "Second message.",
    );
    let initial_revision = state.full_snapshot().revision;
    let mut delta_receiver = state.subscribe_delta_events();

    let error = state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageCreated {
                revision: 4,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-2".to_owned(),
                message_index: 2,
                message_count: 2,
                message: remote_text_message("message-2", "Second final."),
                preview: "Second final.".to_owned(),
                status: SessionStatus::Idle,
                session_mutation_stamp: None,
            },
        )
        .expect_err("out-of-bounds existing remote MessageCreated should request resync");
    assert!(
        error
            .to_string()
            .contains("out of bounds for existing message"),
        "unexpected error: {error:#}"
    );
    assert!(
        delta_receiver.try_recv().is_err(),
        "out-of-bounds existing MessageCreated should not publish a local delta"
    );

    let snapshot = state.full_snapshot();
    assert_eq!(snapshot.revision, initial_revision);
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    let message_ids: Vec<_> = session
        .messages
        .iter()
        .map(|message| message.id().to_owned())
        .collect();
    assert_eq!(message_ids, vec!["message-1", "message-2"]);
    assert!(matches!(
        session.messages.get(1),
        Some(Message::Text { text, .. }) if text == "Second message."
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.should_skip_remote_applied_revision(&remote.id, 4));
    assert!(inner.should_skip_remote_applied_revision(&remote.id, 3));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_command_update_missing_target_rejects_gap_without_advancing_revision() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_session_id = seed_remote_proxy_session_via_state_inner_upsert(&state, &remote);
    let initial_revision = state.full_snapshot().revision;
    let mut delta_receiver = state.subscribe_delta_events();

    let error = state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::CommandUpdate {
                revision: 2,
                session_id: "remote-session-1".to_owned(),
                message_id: "command-1".to_owned(),
                message_index: 1,
                message_count: 1,
                command: "cargo check".to_owned(),
                command_language: Some("bash".to_owned()),
                output: String::new(),
                output_language: Some("text".to_owned()),
                status: CommandStatus::Running,
                preview: "cargo check".to_owned(),
                session_mutation_stamp: None,
            },
        )
        .expect_err("gap remote CommandUpdate should request resync");
    assert!(
        error
            .to_string()
            .contains("remote CommandUpdate index `1` leaves a gap"),
        "unexpected error: {error:#}"
    );
    assert!(
        delta_receiver.try_recv().is_err(),
        "gap CommandUpdate should not publish a local delta"
    );

    let snapshot = state.full_snapshot();
    assert_eq!(snapshot.revision, initial_revision);
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    assert!(session.messages.is_empty());

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.should_skip_remote_applied_delta_revision(&remote.id, 2));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_parallel_agents_update_missing_target_rejects_gap_without_advancing_revision() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_session_id = seed_remote_proxy_session_via_state_inner_upsert(&state, &remote);
    let initial_revision = state.full_snapshot().revision;
    let mut delta_receiver = state.subscribe_delta_events();

    let error = state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::ParallelAgentsUpdate {
                revision: 2,
                session_id: "remote-session-1".to_owned(),
                message_id: "parallel-1".to_owned(),
                message_index: 1,
                message_count: 1,
                agents: vec![ParallelAgentProgress {
                    detail: Some("Collecting context".to_owned()),
                    id: "reviewer".to_owned(),
                    source: ParallelAgentSource::Tool,
                    status: ParallelAgentStatus::Running,
                    title: "Reviewer".to_owned(),
                }],
                preview: "Running reviewer".to_owned(),
                session_mutation_stamp: None,
            },
        )
        .expect_err("gap remote ParallelAgentsUpdate should request resync");
    assert!(
        error
            .to_string()
            .contains("remote ParallelAgentsUpdate index `1` leaves a gap"),
        "unexpected error: {error:#}"
    );
    assert!(
        delta_receiver.try_recv().is_err(),
        "gap ParallelAgentsUpdate should not publish a local delta"
    );

    let snapshot = state.full_snapshot();
    assert_eq!(snapshot.revision, initial_revision);
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    assert!(session.messages.is_empty());

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.should_skip_remote_applied_delta_revision(&remote.id, 2));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that a remote MessageUpdated delta is an in-place replacement:
// the local proxy keeps the existing transcript position, republishes a
// localized MessageUpdated delta, and advances the remote applied
// revision only after the replacement commits.
// Guards against regressing back to MessageCreated-style insertion.
#[test]
fn remote_message_updated_delta_replaces_existing_message_and_publishes_local_delta() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_session_id = seed_remote_proxy_session_via_state_inner_upsert(&state, &remote);
    apply_remote_created_text_message(&state, &remote.id, 2, "message-1", "Draft remote message.");
    let mut delta_receiver = state.subscribe_delta_events();

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageUpdated {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                message: remote_text_message("message-1", "Final remote message."),
                preview: "Final remote message.".to_owned(),
                status: SessionStatus::Idle,
                session_mutation_stamp: None,
            },
        )
        .expect("remote message update delta should apply");

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    assert_eq!(session.preview, "Final remote message.");
    assert_eq!(session.status, SessionStatus::Idle);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Final remote message."
    ));

    let delta: DeltaEvent = serde_json::from_str(
        &delta_receiver
            .try_recv()
            .expect("remote message update should publish a localized delta"),
    )
    .expect("message update delta should decode");
    match delta {
        DeltaEvent::MessageUpdated {
            revision,
            session_id,
            message_id,
            message_index,
            message_count,
            message,
            preview,
            status,
            session_mutation_stamp,
        } => {
            assert_eq!(revision, snapshot.revision);
            assert_eq!(session_id, local_session_id);
            assert_eq!(message_id, "message-1");
            assert_eq!(message_index, 0);
            assert_eq!(message_count, 1);
            assert!(matches!(
                message,
                Message::Text { text, .. } if text == "Final remote message."
            ));
            assert_eq!(preview, "Final remote message.");
            assert_eq!(status, SessionStatus::Idle);
            assert_eq!(session_mutation_stamp, session.session_mutation_stamp);
        }
        _ => panic!("expected localized MessageUpdated delta"),
    }
    assert!(
        delta_receiver.try_recv().is_err(),
        "remote message update should publish exactly one delta"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.should_skip_remote_applied_delta_revision(&remote.id, 3));
    assert!(inner.should_skip_remote_applied_delta_revision(&remote.id, 2));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_message_updated_delta_uses_message_id_when_remote_index_is_stale() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_session_id = seed_remote_proxy_session_via_state_inner_upsert(&state, &remote);
    apply_remote_created_text_message(&state, &remote.id, 2, "message-1", "First message.");
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageCreated {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-2".to_owned(),
                message_index: 1,
                message_count: 2,
                message: remote_text_message("message-2", "Second draft."),
                preview: "Second draft.".to_owned(),
                status: SessionStatus::Active,
                session_mutation_stamp: None,
            },
        )
        .expect("second remote message create delta should apply");
    let mut delta_receiver = state.subscribe_delta_events();

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageUpdated {
                revision: 4,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-2".to_owned(),
                message_index: 0,
                message_count: 2,
                message: remote_text_message("message-2", "Second final."),
                preview: "Second final.".to_owned(),
                status: SessionStatus::Idle,
                session_mutation_stamp: None,
            },
        )
        .expect("remote message update with stale index should apply by id");

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    let message_ids: Vec<_> = session
        .messages
        .iter()
        .map(|message| message.id().to_owned())
        .collect();
    assert_eq!(message_ids, vec!["message-1", "message-2"]);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "First message."
    ));
    assert!(matches!(
        session.messages.get(1),
        Some(Message::Text { text, .. }) if text == "Second final."
    ));

    let delta: DeltaEvent = serde_json::from_str(
        &delta_receiver
            .try_recv()
            .expect("remote stale-index update should publish a localized delta"),
    )
    .expect("message update delta should decode");
    match delta {
        DeltaEvent::MessageUpdated {
            revision,
            session_id,
            message_id,
            message_index,
            message_count,
            message,
            preview,
            status,
            session_mutation_stamp,
        } => {
            assert_eq!(revision, snapshot.revision);
            assert_eq!(session_id, local_session_id);
            assert_eq!(message_id, "message-2");
            assert_eq!(message_index, 1);
            assert_eq!(message_count, 2);
            assert!(matches!(
                message,
                Message::Text { text, .. } if text == "Second final."
            ));
            assert_eq!(preview, "Second final.");
            assert_eq!(status, SessionStatus::Idle);
            assert_eq!(session_mutation_stamp, session.session_mutation_stamp);
        }
        _ => panic!("expected localized MessageUpdated delta"),
    }
    assert!(
        delta_receiver.try_recv().is_err(),
        "remote stale-index update should publish exactly one delta"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.should_skip_remote_applied_delta_revision(&remote.id, 4));
    assert!(inner.should_skip_remote_applied_delta_revision(&remote.id, 3));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that a remote MessageUpdated targeting an unknown local message is
// treated as a sync gap. It must not synthesize MessageCreated, publish a
// partial local delta, or advance the remote applied revision.
// Guards against masking missed transcript state.
#[test]
fn remote_message_updated_delta_missing_target_errors_without_creating_or_advancing_revision() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_session_id = seed_remote_proxy_session_via_state_inner_upsert(&state, &remote);
    let initial_revision = state.full_snapshot().revision;
    let mut delta_receiver = state.subscribe_delta_events();

    let error = state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageUpdated {
                revision: 5,
                session_id: "remote-session-1".to_owned(),
                message_id: "missing-message".to_owned(),
                message_index: 0,
                message_count: 1,
                message: remote_text_message("missing-message", "Remote drift."),
                preview: "Remote drift.".to_owned(),
                status: SessionStatus::Active,
                session_mutation_stamp: None,
            },
        )
        .expect_err("missing-target remote MessageUpdated should request resync");
    assert!(
        error
            .to_string()
            .contains("remote MessageUpdated for unknown message `missing-message`"),
        "unexpected error: {error:#}"
    );
    assert!(
        delta_receiver.try_recv().is_err(),
        "missing-target MessageUpdated should not publish a local delta"
    );

    let snapshot = state.full_snapshot();
    assert_eq!(snapshot.revision, initial_revision);
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    assert!(session.messages.is_empty());

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.should_skip_remote_applied_delta_revision(&remote.id, 5));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that stale remote MessageUpdated deltas are ignored before any
// local transcript mutation or republish.
// Guards against delayed SSE replay rolling back a newer local proxy.
#[test]
fn stale_remote_message_updated_delta_is_ignored() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_session_id = seed_remote_proxy_session_via_state_inner_upsert(&state, &remote);
    apply_remote_created_text_message(&state, &remote.id, 4, "message-1", "Current text.");
    let revision_after_create = state.full_snapshot().revision;
    let mut delta_receiver = state.subscribe_delta_events();

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageUpdated {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                message: remote_text_message("message-1", "Stale text."),
                preview: "Stale text.".to_owned(),
                status: SessionStatus::Active,
                session_mutation_stamp: None,
            },
        )
        .expect("stale remote message update should be ignored");
    assert!(
        delta_receiver.try_recv().is_err(),
        "stale MessageUpdated should not publish a local delta"
    );

    let snapshot = state.full_snapshot();
    assert_eq!(snapshot.revision, revision_after_create);
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Current text."
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.should_skip_remote_applied_delta_revision(&remote.id, 3));
    assert!(!inner.should_skip_remote_applied_delta_revision(&remote.id, 4));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn stale_remote_message_updated_delta_with_mismatched_payload_id_is_ignored() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_session_id = seed_remote_proxy_session_via_state_inner_upsert(&state, &remote);
    apply_remote_created_text_message(&state, &remote.id, 4, "message-1", "Current text.");
    let revision_after_create = state.full_snapshot().revision;
    let mut delta_receiver = state.subscribe_delta_events();

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageUpdated {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                message: remote_text_message("different-message", "Stale wrong text."),
                preview: "Stale wrong text.".to_owned(),
                status: SessionStatus::Active,
                session_mutation_stamp: None,
            },
        )
        .expect("stale id-mismatched remote message update should be ignored");
    assert!(
        delta_receiver.try_recv().is_err(),
        "stale id-mismatched MessageUpdated should not publish a local delta"
    );

    let snapshot = state.full_snapshot();
    assert_eq!(snapshot.revision, revision_after_create);
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Current text."
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.should_skip_remote_applied_delta_revision(&remote.id, 3));
    assert!(!inner.should_skip_remote_applied_delta_revision(&remote.id, 4));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that a malformed MessageUpdated payload is rejected before state
// mutation: the event id and embedded message id must match.
// Guards against localizing the wrong remote message.
#[test]
fn remote_message_updated_delta_rejects_payload_id_mismatch() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_session_id = seed_remote_proxy_session_via_state_inner_upsert(&state, &remote);
    apply_remote_created_text_message(&state, &remote.id, 2, "message-1", "Current text.");
    let revision_after_create = state.full_snapshot().revision;
    let mut delta_receiver = state.subscribe_delta_events();

    let error = state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageUpdated {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                message: remote_text_message("different-message", "Wrong text."),
                preview: "Wrong text.".to_owned(),
                status: SessionStatus::Active,
                session_mutation_stamp: None,
            },
        )
        .expect_err("id-mismatched remote MessageUpdated should fail");
    assert!(
        error
            .to_string()
            .contains("did not match event id `message-1`"),
        "unexpected error: {error:#}"
    );
    assert!(
        delta_receiver.try_recv().is_err(),
        "id-mismatched MessageUpdated should not publish a local delta"
    );

    let snapshot = state.full_snapshot();
    assert_eq!(snapshot.revision, revision_after_create);
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Current text."
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.should_skip_remote_applied_delta_revision(&remote.id, 3));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that two deltas sharing the same remote revision both apply in
// order when they affect different fields; applied-revision dedupe
// recognizes only strictly-older revisions as stale.
// Guards against the dedupe window being too tight (drops valid
// sibling deltas) or too loose (replays already-applied ones).
#[test]
fn remote_same_revision_deltas_apply_in_sequence() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");

    let local_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let local_session_id = upsert_remote_proxy_session_record(
            &mut inner,
            &remote.id,
            &remote_session,
            Some(local_project_id),
        );
        state
            .commit_locked(&mut inner)
            .expect("remote proxy session should persist");
        local_session_id
    };
    let mut delta_receiver = state.subscribe_delta_events();

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageCreated {
                revision: 2,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                message: Message::Text {
                    attachments: Vec::new(),
                    id: "message-1".to_owned(),
                    timestamp: "2026-04-05 10:00:00".to_owned(),
                    author: Author::Assistant,
                    text: "First remote message.".to_owned(),
                    expanded_text: None,
                    source: None,
                },
                preview: "First remote message.".to_owned(),
                status: SessionStatus::Active,
                session_mutation_stamp: None,
            },
        )
        .expect("first same-revision delta should apply");
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::CommandUpdate {
                revision: 2,
                session_id: "remote-session-1".to_owned(),
                message_id: "command-1".to_owned(),
                message_index: 1,
                message_count: 2,
                command: "echo ok".to_owned(),
                command_language: Some("bash".to_owned()),
                output: "ok".to_owned(),
                output_language: Some("text".to_owned()),
                status: CommandStatus::Success,
                preview: "echo ok".to_owned(),
                session_mutation_stamp: None,
            },
        )
        .expect("second same-revision delta should apply");

    let first_delta: DeltaEvent = serde_json::from_str(
        &delta_receiver
            .try_recv()
            .expect("first same-revision delta should publish"),
    )
    .expect("first delta payload should decode");
    match first_delta {
        DeltaEvent::MessageCreated { message_id, .. } => assert_eq!(message_id, "message-1"),
        _ => panic!("unexpected first delta variant"),
    }

    let second_delta: DeltaEvent = serde_json::from_str(
        &delta_receiver
            .try_recv()
            .expect("second same-revision delta should publish"),
    )
    .expect("second delta payload should decode");
    // A missing remote command message is localized by creating the message
    // first, so the published delta is normalized to MessageCreated.
    match second_delta {
        DeltaEvent::MessageCreated { message_id, .. } => assert_eq!(message_id, "command-1"),
        _ => panic!("unexpected second delta variant"),
    }

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    assert_eq!(session.preview, "echo ok");
    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(session.messages.len(), 2);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "First remote message."
    ));
    assert!(matches!(
        session.messages.get(1),
        Some(Message::Command {
            command,
            output,
            status: CommandStatus::Success,
            ..
        }) if command == "echo ok" && output == "ok"
    ));

    let _ = fs::remove_file(state.persistence_path.as_path());
}
