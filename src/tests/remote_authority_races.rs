//! Remote routing-authority, registry-publication, and recovery races.
//!
//! This module owns cross-cutting authority tests for registry publication,
//! request leases, bridge claims, bounded hydration, streaming responses, and
//! settings durability. It deliberately does not own project/session/orchestrator
//! creation races, which remain in project_creation_races.rs.
//!
//! Extracted from project_creation_races.rs so each test module has one coherent
//! lifecycle domain and stays comfortably below the test-file size threshold.

use super::project_creation_races::{
    RemoteRequestFailurePhase, install_post_decode_a_to_b_to_a_cycle, remote_config,
    remote_settings_request, remove_remote_settings, replace_remote_settings,
    spawn_remote_request_failure_after_replacement_server,
};
use super::remote::{
    make_remote_session_summary_only, remote_text_message, spawn_remote_session_response_server,
};
use super::*;

#[test]
fn registry_publication_fences_requests_before_connection_reconciliation() {
    let state = test_app_state();
    let original = remote_config("remote-registry-stale-lookup");
    let mut replacement = original.clone();
    replacement.host = Some("replacement.example.com".to_owned());
    replacement.port = Some(2222);
    replacement.user = Some("bob".to_owned());

    state
        .remote_registry
        .reconcile(&[RemoteConfig::local(), original.clone()]);
    let original_lease = state
        .remote_registry
        .connection(&original)
        .expect("published original remote should resolve");
    assert_eq!(original_lease.connection.config(), original);
    let publication = state
        .remote_registry
        .publish_configs(&[RemoteConfig::local(), replacement.clone()]);
    assert!(publication.changed_ids.contains(&replacement.id));
    assert_eq!(
        original_lease
            .connection
            .ensure_pinned_route(&original_lease.pinned)
            .expect_err("a published replacement must retire an acquired lease")
            .message,
        REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST
    );

    let error = match state.remote_registry.connection(&original) {
        Ok(_) => panic!("stale snapshot should fail before reconciliation"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST);
    let capability_error = match state
        .remote_registry
        .cached_supports_inline_orchestrator_templates(&original)
    {
        Ok(_) => panic!("capability lookup must propagate stale routing authority"),
        Err(error) => error,
    };
    assert_eq!(capability_error.status, StatusCode::CONFLICT);
    assert_eq!(
        capability_error.message,
        REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST
    );

    let current_lease = state
        .remote_registry
        .connection(&replacement)
        .expect("current snapshot should create a replacement connection");
    assert!(!Arc::ptr_eq(
        &original_lease.connection,
        &current_lease.connection
    ));
    assert_eq!(current_lease.connection.config(), replacement);
    state.remote_registry.finish_config_publication(publication);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn endpoint_replacement_scopes_delta_hydration_ownership_by_generation() {
    let state = test_app_state();
    let original = remote_config("remote-hydration-generation");
    replace_remote_settings(&state, original.clone());
    let original_generation = state
        .remote_registry
        .config_generation
        .load(Ordering::Acquire);
    let original_guard = state
        .try_begin_remote_delta_hydration(RemoteDeltaHydrationKey {
            remote_id: original.id.clone(),
            remote_session_id: "remote-session-shared".to_owned(),
            authority_generation: original_generation,
        })
        .expect("the original endpoint should claim hydration ownership");

    let mut replacement = original.clone();
    replacement.host = Some("replacement.example.com".to_owned());
    replacement.port = Some(2222);
    replace_remote_settings(&state, replacement);
    let replacement_generation = state
        .remote_registry
        .config_generation
        .load(Ordering::Acquire);
    assert_ne!(replacement_generation, original_generation);

    let replacement_guard = state
        .try_begin_remote_delta_hydration(RemoteDeltaHydrationKey {
            remote_id: original.id.clone(),
            remote_session_id: "remote-session-shared".to_owned(),
            authority_generation: replacement_generation,
        })
        .expect("the replacement endpoint must not be blocked by the retired hydration");
    assert_eq!(
        state
            .remote_delta_hydrations_in_flight
            .lock()
            .expect("remote delta hydration mutex poisoned")
            .len(),
        2
    );

    drop(replacement_guard);
    assert_eq!(
        state
            .remote_delta_hydrations_in_flight
            .lock()
            .expect("remote delta hydration mutex poisoned")
            .len(),
        1
    );
    drop(original_guard);
    assert!(
        state
            .remote_delta_hydrations_in_flight
            .lock()
            .expect("remote delta hydration mutex poisoned")
            .is_empty()
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

fn seed_unloaded_remote_proxy_for_authority_test(
    state: &AppState,
    remote: &RemoteConfig,
) -> Session {
    let root_path = format!("/remote/{}", remote.id);
    create_test_remote_project(
        state,
        remote,
        &root_path,
        "Remote Hydration Authority",
        "remote-project-hydration-authority",
    );
    let mut full_session = sample_remote_orchestrator_state(
        "remote-project-hydration-authority",
        &root_path,
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    full_session.messages = vec![remote_text_message(
        "remote-message-1",
        "Authoritative hydrated message.",
    )];
    full_session.messages_loaded = true;
    full_session.message_count = 1;
    full_session.session_mutation_stamp = Some(10);

    let mut summary_session = full_session.clone();
    make_remote_session_summary_only(&mut summary_session, 1);
    summary_session.session_mutation_stamp = Some(9);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 1,
                session_id: summary_session.id.clone(),
                session: summary_session,
            },
        )
        .expect("remote summary session should localize");
    full_session
}

fn assert_hydration_rejects_authority_change_before_target(restore_original: bool) {
    let state = test_app_state();
    let original = remote_config(if restore_original {
        "remote-hydration-target-cycle"
    } else {
        "remote-hydration-target-replacement"
    });
    seed_unloaded_remote_proxy_for_authority_test(&state, &original);
    insert_test_remote_connection(
        &state,
        &original,
        47991,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    let original_connection = state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .get(&original.id)
        .cloned()
        .expect("original connection should exist");
    let state_for_hook = state.clone();
    let original_for_hook = original.clone();
    let mut replacement = original.clone();
    replacement.host = Some("replacement.example.com".to_owned());
    replacement.port = Some(2222);
    state
        .remote_registry
        .set_test_before_remote_delta_hydration_target(move || {
            replace_remote_settings(&state_for_hook, replacement);
            if restore_original {
                replace_remote_settings(&state_for_hook, original_for_hook);
            }
        });

    let error = state
        .apply_remote_delta_event_for_bridge(
            &original,
            &original_connection,
            DeltaEvent::MessageCreated {
                revision: 2,
                session_id: "remote-session-1".to_owned(),
                message_id: "remote-message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                message: remote_text_message(
                    "remote-message-1",
                    "Stale bridge message must not hydrate.",
                ),
                preview: "Stale bridge message must not hydrate.".to_owned(),
                status: SessionStatus::Idle,
                session_mutation_stamp: Some(10),
            },
        )
        .expect_err("retired bridge authority must fail before hydration selects a target");
    assert!(
        format!("{error:#}").contains(REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST),
        "unexpected authority error: {error:#}"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&original.id, "remote-session-1")
        .expect("remote proxy session should remain");
    assert!(inner.sessions[index].session.messages.is_empty());
    assert_ne!(inner.remote_applied_revisions.get(&original.id), Some(&2));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_delta_hydration_rejects_endpoint_replacement_before_target_selection() {
    assert_hydration_rejects_authority_change_before_target(false);
}

#[test]
fn remote_delta_hydration_rejects_a_to_b_to_a_before_target_selection() {
    assert_hydration_rejects_authority_change_before_target(true);
}

#[test]
fn remote_delta_repair_rejects_post_decode_a_to_b_to_a_before_apply() {
    let state = test_app_state();
    let original = remote_config("remote-repair-post-decode-cycle");
    let full_session = seed_unloaded_remote_proxy_for_authority_test(&state, &original);
    let (port, _requests, server) = spawn_remote_session_response_server(SessionResponse {
        revision: 2,
        session: full_session,
        server_instance_id: "remote-instance".to_owned(),
    });
    insert_test_remote_connection(
        &state,
        &original,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    let original_connection = state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .get(&original.id)
        .cloned()
        .expect("original connection should exist");
    install_post_decode_a_to_b_to_a_cycle(&state, &original);
    let event = DeltaEvent::MessageCreated {
        revision: 2,
        session_id: "remote-session-1".to_owned(),
        message_id: "remote-message-1".to_owned(),
        message_index: 0,
        message_count: 1,
        message: remote_text_message("remote-message-1", "Trigger bounded repair."),
        preview: "Trigger bounded repair.".to_owned(),
        status: SessionStatus::Idle,
        session_mutation_stamp: Some(10),
    };

    let error = state
        .repair_remote_session_tail_after_delta_error_for_bridge(
            &original,
            &original_connection,
            &event,
        )
        .expect_err("pre-cycle bounded repair response must not localize after A -> B -> A");
    assert!(
        format!("{error:#}").contains(REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST),
        "unexpected authority error: {error:#}"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&original.id, "remote-session-1")
        .expect("remote proxy session should remain");
    assert!(inner.sessions[index].session.messages.is_empty());
    assert_ne!(inner.remote_applied_revisions.get(&original.id), Some(&2));
    drop(inner);

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn retired_endpoint_replay_insert_cannot_suppress_replacement_delta() {
    let state = test_app_state();
    let original = remote_config("remote-replay-generation");
    replace_remote_settings(&state, original.clone());
    let event = DeltaEvent::CodexUpdated {
        revision: 17,
        codex: CodexState::default(),
    };
    let original_generation = state
        .remote_registry
        .config_generation
        .load(Ordering::Acquire);
    let original_key =
        AppState::remote_delta_replay_key_for_generation(&original.id, original_generation, &event);

    let mut replacement = original.clone();
    replacement.host = Some("replacement.example.com".to_owned());
    replacement.port = Some(2223);
    replace_remote_settings(&state, replacement);
    let replacement_generation = state
        .remote_registry
        .config_generation
        .load(Ordering::Acquire);
    assert_ne!(replacement_generation, original_generation);

    // Model endpoint A finishing its post-publish replay bookkeeping after
    // settings publication already cleared A's cache entries.
    state.note_remote_applied_delta_replay(&original_key);
    let replacement_key = AppState::remote_delta_replay_key_for_generation(
        &original.id,
        replacement_generation,
        &event,
    );
    assert!(state.should_skip_remote_applied_delta_replay(&original_key));
    assert!(
        !state.should_skip_remote_applied_delta_replay(&replacement_key),
        "endpoint A bookkeeping must not suppress endpoint B's identical delta"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn disabled_remote_keeps_bridge_subscription_without_retry_worker() {
    let state = test_app_state();
    let enabled = remote_config("remote-disabled-bridge");
    replace_remote_settings(&state, enabled.clone());
    let claimed = state
        .remote_registry
        .claim_event_bridge(&enabled.id)
        .expect("enabled bridge claim should resolve")
        .expect("enabled bridge should be newly claimed");

    let mut disabled = enabled.clone();
    disabled.enabled = false;
    let disabled_publication = state
        .remote_registry
        .publish_configs(&[RemoteConfig::local(), disabled.clone()]);
    assert!(disabled_publication.bridges_to_restart.is_empty());
    state
        .remote_registry
        .finish_config_publication(disabled_publication);
    assert!(claimed.retired.load(Ordering::SeqCst));
    assert!(claimed.event_bridge_shutdown.load(Ordering::SeqCst));
    assert!(
        state
            .remote_registry
            .claim_event_bridge(&disabled.id)
            .expect("disabled bridge claim should resolve")
            .is_none(),
        "disabled remotes must not create a retry worker"
    );
    assert!(
        !state
            .remote_registry
            .connections
            .lock()
            .expect("remote registry mutex poisoned")
            .contains_key(&disabled.id)
    );

    let enabled_publication = state
        .remote_registry
        .publish_configs(&[RemoteConfig::local(), enabled.clone()]);
    assert_eq!(enabled_publication.bridges_to_restart, vec![enabled.id]);
    state
        .remote_registry
        .finish_config_publication(enabled_publication);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn readded_disabled_remote_with_proxy_session_rearms_bridge_subscription() {
    let state = test_app_state();
    let enabled = remote_config("remote-readded-bridge");
    replace_remote_settings(&state, enabled.clone());
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Re-added remote proxy".to_owned()),
            "/remote/readded-bridge".to_owned(),
            None,
            None,
        );
        let session_id = record.session.id;
        let index = inner
            .find_session_index(&session_id)
            .expect("new session should be inserted");
        let record = inner
            .session_mut_by_index(index)
            .expect("new session index should remain valid");
        record.remote_id = Some(enabled.id.clone());
        record.remote_session_id = Some("remote-session-readded-bridge".to_owned());
        state
            .commit_locked(&mut inner)
            .expect("proxy session should persist");
    }
    let claimed = state
        .remote_registry
        .claim_event_bridge(&enabled.id)
        .expect("initial bridge claim should resolve")
        .expect("initial bridge should be newly claimed");

    remove_remote_settings(&state);
    assert!(claimed.retired.load(Ordering::SeqCst));
    assert!(
        !state
            .remote_registry
            .desired_event_bridges
            .lock()
            .expect("remote event bridge subscription mutex poisoned")
            .contains(&enabled.id),
        "removing a remote should drop its bridge subscription"
    );

    let mut disabled = enabled.clone();
    disabled.enabled = false;
    replace_remote_settings(&state, disabled.clone());
    assert!(
        state
            .remote_registry
            .desired_event_bridges
            .lock()
            .expect("remote event bridge subscription mutex poisoned")
            .contains(&disabled.id),
        "re-adding a remote still referenced by a proxy session should restore its bridge subscription"
    );
    assert!(
        !state
            .remote_registry
            .connections
            .lock()
            .expect("remote registry mutex poisoned")
            .contains_key(&disabled.id),
        "a disabled re-added remote must not start a retry worker"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn full_a_b_a_cycle_requires_a_fresh_connection() {
    let state = test_app_state();
    let first = remote_config("remote-a-b-a-cycle");
    replace_remote_settings(&state, first.clone());
    let original_lease = state
        .remote_registry
        .connection(&first)
        .expect("first A route should resolve");

    let mut second = first.clone();
    second.host = Some("endpoint-b.example.com".to_owned());
    second.port = Some(2224);
    replace_remote_settings(&state, second);
    replace_remote_settings(&state, first.clone());

    assert_eq!(
        original_lease
            .connection
            .ensure_pinned_route(&original_lease.pinned)
            .expect_err("the original A connection must stay retired")
            .message,
        REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST
    );
    let fresh_lease = state
        .remote_registry
        .connection(&first)
        .expect("a fresh connection should accept authoritative A again");
    assert!(!Arc::ptr_eq(
        &original_lease.connection,
        &fresh_lease.connection
    ));
    assert_eq!(fresh_lease.pinned, first);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn display_name_edit_retires_continuity_but_preserves_routing_identity() {
    let state = test_app_state();
    let original = remote_config("remote-display-name-edit");
    replace_remote_settings(&state, original.clone());
    let original_lease = state
        .remote_registry
        .connection(&original)
        .expect("original route should resolve");

    let mut renamed = original.clone();
    renamed.name = "Renamed Remote".to_owned();
    assert!(same_remote_routing_config(&original, &renamed));
    replace_remote_settings(&state, renamed.clone());

    assert!(original_lease.connection.retired.load(Ordering::SeqCst));
    assert_eq!(
        original_lease
            .connection
            .ensure_pinned_route(&original_lease.pinned)
            .expect_err("the pre-rename connection should be retired")
            .message,
        REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST
    );
    let fresh_lease = state
        .remote_registry
        .connection(&renamed)
        .expect("the renamed route should resolve through a fresh connection");
    assert_eq!(fresh_lease.pinned, renamed);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn request_send_failure_prefers_endpoint_replacement_conflict() {
    let state = test_app_state();
    let remote = remote_config("remote-send-failure-authority");
    create_test_remote_project(
        &state,
        &remote,
        "/remote/send-failure-authority",
        "Send Failure Authority",
        "remote-project-send-failure-authority",
    );
    let mut replacement = remote.clone();
    replacement.host = Some("replacement.example.com".to_owned());
    replacement.port = Some(2222);
    replacement.user = Some("bob".to_owned());
    let (port, server) = spawn_remote_request_failure_after_replacement_server(
        state.clone(),
        replacement,
        RemoteRequestFailurePhase::BeforeResponseHeaders,
    );
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );

    let error = match state.remote_registry.request_json::<HealthResponse>(
        &remote,
        Method::GET,
        "/authority-failure",
        &[],
        None,
    ) {
        Ok(_) => panic!("replacement during send must reject the retired route"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST);

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn request_decode_failure_prefers_endpoint_replacement_conflict() {
    let state = test_app_state();
    let remote = remote_config("remote-decode-failure-authority");
    create_test_remote_project(
        &state,
        &remote,
        "/remote/decode-failure-authority",
        "Decode Failure Authority",
        "remote-project-decode-failure-authority",
    );
    let mut replacement = remote.clone();
    replacement.host = Some("replacement.example.com".to_owned());
    replacement.port = Some(2223);
    replacement.user = Some("bob".to_owned());
    let (port, server) = spawn_remote_request_failure_after_replacement_server(
        state.clone(),
        replacement,
        RemoteRequestFailurePhase::DuringJsonBody,
    );
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );

    let error = match state.remote_registry.request_json::<HealthResponse>(
        &remote,
        Method::GET,
        "/authority-failure",
        &[],
        None,
    ) {
        Ok(_) => panic!("replacement during body decoding must reject the retired route"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST);

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delayed_bridge_restart_claims_the_latest_published_endpoint() {
    let state = test_app_state();
    let first = remote_config("remote-bridge-latest-authority");
    let mut second = first.clone();
    second.host = Some("second.example.com".to_owned());
    second.port = Some(2202);
    let mut third = first.clone();
    third.host = Some("third.example.com".to_owned());
    third.port = Some(2203);

    state
        .remote_registry
        .reconcile(&[RemoteConfig::local(), first.clone()]);
    let first_connection = state
        .remote_registry
        .claim_event_bridge(&first.id)
        .expect("first bridge claim should resolve")
        .expect("first bridge should be newly claimed");
    assert!(first_connection.event_bridge_started.load(Ordering::SeqCst));

    let second_publication = state
        .remote_registry
        .publish_configs(&[RemoteConfig::local(), second]);
    let delayed_restarts = state
        .remote_registry
        .finish_config_publication(second_publication);
    assert_eq!(delayed_restarts, vec![first.id.clone()]);
    assert!(first_connection.retired.load(Ordering::SeqCst));
    assert!(
        first_connection
            .event_bridge_shutdown
            .load(Ordering::SeqCst)
    );

    let third_publication = state
        .remote_registry
        .publish_configs(&[RemoteConfig::local(), third.clone()]);
    assert_eq!(third_publication.bridges_to_restart, vec![first.id.clone()]);
    let latest_restarts = state
        .remote_registry
        .finish_config_publication(third_publication);

    // Both publications preserve the desired bridge subscription. If the
    // earlier restart is delayed until after the later publication, both
    // restart intents resolve against current authority and the atomic claim
    // still creates exactly one bridge for the latest endpoint.
    let latest_connections = delayed_restarts
        .into_iter()
        .chain(latest_restarts)
        .map(|remote_id| {
            state
                .remote_registry
                .claim_event_bridge(&remote_id)
                .expect("delayed restart should resolve current authority")
        })
        .flatten()
        .collect::<Vec<_>>();
    assert_eq!(latest_connections.len(), 1);
    let latest_connection = latest_connections
        .into_iter()
        .next()
        .expect("one delayed restart should claim the latest endpoint");
    assert_eq!(latest_connection.config(), third);
    assert!(
        latest_connection
            .event_bridge_started
            .load(Ordering::SeqCst)
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn bridge_claim_sets_started_before_publication_can_observe_the_connection() {
    let state = test_app_state();
    let original = remote_config("remote-bridge-claim-linearization");
    let mut replacement = original.clone();
    replacement.host = Some("replacement.example.com".to_owned());
    replacement.port = Some(2244);
    state
        .remote_registry
        .reconcile(&[RemoteConfig::local(), original.clone()]);

    let (publish_tx, publish_rx) = std::sync::mpsc::channel();
    let (attempted_tx, attempted_rx) = std::sync::mpsc::channel();
    let (publication_tx, publication_rx) = std::sync::mpsc::channel();
    let state_for_publisher = state.clone();
    let publisher = std::thread::spawn(move || {
        publish_rx
            .recv()
            .expect("claim hook should release the publisher");
        attempted_tx
            .send(())
            .expect("publisher attempt should be observable");
        let publication = state_for_publisher
            .remote_registry
            .publish_configs(&[RemoteConfig::local(), replacement]);
        publication_tx
            .send(publication)
            .expect("publication should be returned to the test");
    });

    let claimed = state
        .remote_registry
        .claim_event_bridge_with_locked_claim(&original.id, |connection| {
            assert!(
                connection.event_bridge_started.load(Ordering::SeqCst),
                "bridge ownership must be visible before either registry lock is released"
            );
            publish_tx
                .send(())
                .expect("publisher should be released while claim locks are held");
            attempted_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("publisher should attempt publication");
            assert!(
                state.remote_registry.configs.try_lock().is_err(),
                "config publication must remain serialized with the started transition"
            );
            assert!(
                state.remote_registry.connections.try_lock().is_err(),
                "connection publication must remain serialized with the started transition"
            );
        })
        .expect("bridge claim should resolve")
        .expect("bridge should be newly claimed");

    let publication = publication_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("publication should complete after claim releases its locks");
    assert_eq!(publication.bridges_to_restart, vec![original.id.clone()]);
    assert!(claimed.retired.load(Ordering::SeqCst));
    state.remote_registry.finish_config_publication(publication);
    publisher.join().expect("publisher thread should exit");

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn settings_persist_failure_still_finishes_retired_connection_teardown() {
    let mut state = test_app_state();
    let original_persistence_path = state.persistence_path.clone();
    let original = remote_config("remote-settings-persist-failure");
    create_test_remote_project(
        &state,
        &original,
        "/remote/settings-persist-failure",
        "Remote Settings Persist Failure",
        "remote-project-settings-persist-failure",
    );
    insert_test_remote_connection(
        &state,
        &original,
        47991,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    let retired_connection = state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .get(&original.id)
        .cloned()
        .expect("test connection should exist");
    retired_connection
        .event_bridge_started
        .store(false, Ordering::SeqCst);

    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-settings-persist-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("a directory at the persistence path should force failure");
    state.shutdown_persist_blocking();
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    let mut replacement = original.clone();
    replacement.host = Some("replacement.example.com".to_owned());
    replacement.port = Some(2299);
    let mut peer_state_events = state.subscribe_events();

    let error = match state.update_app_settings(remote_settings_request(vec![
        RemoteConfig::local(),
        replacement.clone(),
    ])) {
        Ok(_) => panic!("settings persistence failure should propagate"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to persist app settings"));
    let peer_snapshot: StateResponse = serde_json::from_str(
        &peer_state_events
            .try_recv()
            .expect("a peer subscriber should receive the in-memory authoritative settings"),
    )
    .expect("peer settings snapshot should decode");
    assert_eq!(
        peer_snapshot.preferences.remotes,
        vec![RemoteConfig::local(), replacement.clone()]
    );
    assert!(retired_connection.retired.load(Ordering::SeqCst));
    assert!(
        retired_connection
            .event_bridge_shutdown
            .load(Ordering::SeqCst),
        "publication teardown must run before returning the commit error"
    );
    assert!(
        !state
            .remote_registry
            .connections
            .lock()
            .expect("remote registry mutex poisoned")
            .contains_key(&original.id)
    );
    assert_eq!(
        state
            .remote_registry
            .configs
            .lock()
            .expect("remote registry config mutex poisoned")
            .get(&original.id),
        Some(&replacement)
    );

    fs::remove_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should be removable");
    state.persistence_path = original_persistence_path.clone();
    state
        .update_app_settings(remote_settings_request(vec![
            RemoteConfig::local(),
            replacement.clone(),
        ]))
        .expect("an identical retry must persist the in-memory routing authority");
    let reloaded = load_state(original_persistence_path.as_path())
        .expect("persisted state should load")
        .expect("persisted state should exist");
    assert_eq!(
        reloaded.preferences.remotes,
        vec![RemoteConfig::local(), replacement]
    );
    assert!(!reloaded.settings_persist_dirty);
    assert!(!reloaded.remote_settings_persist_dirty);

    let _ = fs::remove_file(original_persistence_path.as_path());
}

#[test]
fn unrelated_synchronous_commit_clears_settings_persist_retry_markers() {
    let state = test_app_state();
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    inner.settings_persist_dirty = true;
    inner.remote_settings_persist_dirty = true;
    inner.remote_delta_persist_dirty = true;

    state
        .commit_locked(&mut inner)
        .expect("an unrelated synchronous commit should persist the full settings snapshot");

    assert!(!inner.settings_persist_dirty);
    assert!(!inner.remote_settings_persist_dirty);
    assert!(!inner.remote_delta_persist_dirty);
    drop(inner);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn streaming_response_stops_before_forwarding_replaced_endpoint_bytes() {
    let state = test_app_state();
    let remote = remote_config("remote-stream-endpoint-replacement");
    create_test_remote_project(
        &state,
        &remote,
        "/remote/stream-endpoint-replacement",
        "Remote Stream Endpoint Replacement",
        "remote-project-stream-endpoint-replacement",
    );

    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("remote stream race listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        loop {
            let mut stream = accept_test_connection(&listener, "remote stream race listener");
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
            assert!(
                request.request_line.starts_with("GET /stream "),
                "unexpected request: {}",
                request.request_line
            );
            write_test_http_response(
                &mut stream,
                StatusCode::OK,
                "application/octet-stream",
                "old-endpoint-stream-body",
            );
            break;
        }
    });
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    let mut response = state
        .remote_registry
        .request_without_timeout(&remote, Method::GET, "/stream", &[], None)
        .expect("stream response should be acquired under the original authority");

    let mut replacement = remote.clone();
    replacement.host = Some("replacement.example.com".to_owned());
    replacement.port = Some(2233);
    let publication = state
        .remote_registry
        .publish_configs(&[RemoteConfig::local(), replacement]);
    state.remote_registry.finish_config_publication(publication);

    let mut body = String::new();
    let error = response
        .read_to_string(&mut body)
        .expect_err("retired stream bytes must not be forwarded");
    assert_eq!(error.kind(), std::io::ErrorKind::ConnectionAborted);
    assert!(body.is_empty());

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn registry_refuses_unknown_remote_without_inserting_connection() {
    let state = test_app_state();
    let stale = remote_config("remote-registry-never-published");

    let error = match state.remote_registry.connection(&stale) {
        Ok(_) => panic!("an unknown remote must fail closed"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, format!("unknown remote `{}`", stale.id));
    assert!(
        !state
            .remote_registry
            .connections
            .lock()
            .expect("remote registry mutex poisoned")
            .contains_key(&stale.id)
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn app_boot_seeds_registry_authority_from_persisted_remote_settings() {
    let unique = Uuid::new_v4();
    let state_root = std::env::temp_dir().join(format!("termal-registry-boot-{unique}"));
    let persistence_path = state_root.join("termal.sqlite");
    let templates_path = state_root.join("orchestrators.json");
    fs::create_dir_all(&state_root).expect("state root should exist");
    let remote = remote_config("remote-registry-boot-seed");
    let mut initial_inner = StateInner::new();
    initial_inner.preferences.remotes = vec![RemoteConfig::local(), remote.clone()];
    persist_state(&persistence_path, &initial_inner).expect("remote settings should persist");

    let state = AppState::new_with_paths(
        state_root.to_string_lossy().into_owned(),
        persistence_path.clone(),
        templates_path,
    )
    .expect("state should restart from persisted settings");
    let lease = state
        .remote_registry
        .connection(&remote)
        .expect("boot should seed registry authority before first request");
    assert_eq!(lease.connection.config(), remote);

    state.shutdown_persist_blocking();
    let _ = fs::remove_dir_all(state_root);
}
