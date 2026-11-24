// Exercises staggered-upgrade summary decoding through the remote apply boundary.
use super::super::remote::spawn_remote_state_json_response_server;
use super::*;

#[path = "remote_summary_actions.rs"]
mod actions;

fn fixture() -> (AppState, RemoteConfig, StateResponse) {
    let state = test_app_state();
    let mut remote = local_replay_test_remote();
    remote.transport = RemoteTransport::Ssh;
    remote.host = Some("example.com".to_owned());
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let mut snapshot = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    )
    .into_state_response();
    for session in &mut snapshot.sessions {
        session.preview = "Older host summary".to_owned();
    }
    (state, remote, snapshot)
}

fn retired_fields() -> Vec<Value> {
    let hold = json!({
        "kind": "automaticRetry",
        "cause": "provider_capacity",
        "retryAt": "2026-11-13T22:00:00Z"
    });
    vec![
        json!({"engramControlOff": false}),
        json!({"engramBootRecoveryPending": true}),
        json!({"queueHold": hold.clone()}),
        json!({
            "engramControlOff": true,
            "engramBootRecoveryPending": true,
            "queueHold": hold
        }),
    ]
}

fn payload(snapshot: &StateResponse, kind: &str, fields: &Value) -> Value {
    let mut value = match kind {
        "state" => serde_json::to_value(snapshot).expect("snapshot should encode"),
        "sessionCreated" => serde_json::to_value(DeltaEvent::SessionCreated {
            revision: snapshot.revision,
            session_id: snapshot.sessions[0].id.clone(),
            session: snapshot.sessions[0].clone(),
        })
        .expect("created delta should encode"),
        "orchestratorsUpdated" => serde_json::to_value(DeltaEvent::OrchestratorsUpdated {
            revision: snapshot.revision,
            orchestrators: snapshot.orchestrators.clone(),
            sessions: snapshot.sessions.clone(),
        })
        .expect("orchestrator delta should encode"),
        _ => panic!("unknown test payload kind"),
    };
    let additions = fields.as_object().expect("fields should be an object");
    if kind == "sessionCreated" {
        value["session"]
            .as_object_mut()
            .expect("session should be an object")
            .extend(additions.clone());
    } else {
        for session in value["sessions"].as_array_mut().expect("sessions array") {
            session.as_object_mut().unwrap().extend(additions.clone());
        }
    }
    value
}

fn assert_applied(state: &AppState, remote: &RemoteConfig, expected_sessions: usize) {
    let snapshot = state.full_snapshot();
    let sessions = snapshot
        .sessions
        .iter()
        .filter(|session| session.remote_id.as_deref() == Some(remote.id.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(sessions.len(), expected_sessions);
    for session in sessions {
        assert_eq!(session.preview, "Older host summary");
        assert_eq!(session.status, SessionStatus::Idle);
        assert!(!session.queue_paused);
        let projected = serde_json::to_value(session).expect("proxy should encode");
        for retired in ["engramControlOff", "engramBootRecoveryPending", "queueHold"] {
            assert!(projected.get(retired).is_none(), "retained {retired}");
        }
    }
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&2));
}

fn assert_unapplied(state: &AppState, remote: &RemoteConfig) {
    let snapshot = state.full_snapshot();
    assert!(snapshot.sessions.is_empty());
    assert!(snapshot.orchestrators.is_empty());
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.remote_applied_revisions.contains_key(&remote.id));
}

fn accepts_retired_summary_fields(kind: &str) {
    for fields in retired_fields() {
        let (state, remote, snapshot) = fixture();
        let value = payload(&snapshot, kind, &fields);
        let event = if kind == "state" { "state" } else { "delta" };
        dispatch_remote_event(&state, &remote.id, event, &[value.to_string()])
            .unwrap_or_else(|err| panic!("{kind} rejected retired fields {fields}: {err:#}"));
        assert_applied(
            &state,
            &remote,
            if kind == "sessionCreated" {
                1
            } else {
                snapshot.sessions.len()
            },
        );
        if kind != "sessionCreated" {
            assert_eq!(state.full_snapshot().orchestrators.len(), 1);
        }
        let _ = fs::remove_file(state.persistence_path.as_path());
    }
}

#[test]
fn remote_snapshot_discards_retired_summary_fields_and_applies_sessions() {
    accepts_retired_summary_fields("state");
}

#[test]
fn classified_proxy_does_not_break_strict_peer_snapshot_decoding() {
    use crate::remote_sync::remote_summary_compat::{RemoteStateResponse, decode_state_event};

    let (state, remote, mut owner_snapshot) = fixture();
    let owner_session_count = owner_snapshot.sessions.len();
    owner_snapshot.sessions[0].parent_delegation_id = Some("owner-delegation".to_owned());
    let owner_wire = serde_json::to_string(&owner_snapshot).expect("encode owner snapshot");
    dispatch_remote_event(&state, &remote.id, "state", &[owner_wire])
        .expect("apply the owner's native child and ordinary root");

    let published = state.summary_snapshot();
    let [classified_id] = published.mirrored_delegation_child_ids.as_slice() else {
        panic!("only the owner's child should be classified");
    };
    let wire = serde_json::to_string(&published).expect("encode mirror snapshot");
    // A peer without mirror classification decodes the same strict row shape
    // and reads unrelated rows from the same snapshot.
    let legacy: StateEventPayload =
        serde_json::from_str(&wire).expect("strict summary rows must remain compatible");
    assert_eq!(legacy.state.sessions.len(), owner_session_count);
    assert!(
        legacy
            .state
            .sessions
            .iter()
            .map(|row| row.id.as_str())
            .eq(published.sessions.iter().map(|row| row.id.as_str()))
    );

    let mut nested = serde_json::to_value(&published).expect("encode snapshot");
    nested["sessions"][0]["mirroredDelegationChild"] = json!(true);
    assert!(serde_json::from_value::<StateEventPayload>(nested).is_err());

    let sse = decode_state_event(&wire).expect("decode mirror SSE snapshot");
    let RemoteStateResponse(http) =
        serde_json::from_str(&wire).expect("decode mirror HTTP response");
    for decoded in [sse.state, http] {
        for row in decoded.sessions {
            assert_eq!(row.mirrored_delegation_child, row.id == *classified_id);
            assert!(row.parent_delegation_id.is_none());
        }
    }
}

#[test]
fn remote_orchestrator_delta_classifies_an_existing_proxy_and_keeps_its_transcript() {
    use crate::remote_sync::remote_summary_compat::decode_delta_event;

    let (state, remote, mut initial) = fixture();
    let mut owner_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        initial.revision,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == initial.sessions[0].id)
    .expect("the owner's orchestrator session should exist");
    owner_session.session_mutation_stamp = Some(7);
    owner_session.messages =
        vec![remote_text_message("message-1", "Cached owner transcript")].into();
    owner_session.messages_loaded = true;
    owner_session.message_count = 1;
    initial.sessions[0] = test_state_session_summary_from_session(&owner_session);
    state
        .apply_remote_state_snapshot(&remote.id, initial.clone())
        .expect("seed the unclassified proxy");
    materialize_remote_proxy_session_transcript_for_test(&state, &remote, &owner_session);
    let local_id = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, &owner_session.id)
            .expect("proxy should already exist");
        inner.sessions[index].session.id.clone()
    };
    let initial_public = state.summary_snapshot();
    let before = initial_public
        .sessions
        .iter()
        .find(|row| row.id == local_id)
        .unwrap();
    assert!(!before.mirrored_delegation_child);
    let mut published = state.subscribe_delta_events();

    let mut frame = serde_json::to_value(DeltaEvent::OrchestratorsUpdated {
        revision: initial.revision + 1,
        orchestrators: initial.orchestrators,
        sessions: initial.sessions,
    })
    .expect("encode orchestrator delta");
    frame["mirroredDelegationChildIds"] = json!([owner_session.id]);
    dispatch_remote_event(&state, &remote.id, "delta", &[frame.to_string()])
        .expect("apply classification without a new owner session stamp");

    let emitted = published.try_recv().expect("republish accepted delta");
    let strict: DeltaEvent = serde_json::from_str(&emitted).expect("strict delta rows decode");
    assert!(
        strict
            .session_summaries()
            .iter()
            .any(|row| row.id == local_id)
    );
    let current = decode_delta_event(&emitted).expect("decode republished delta");
    let after = current
        .session_summaries()
        .iter()
        .find(|row| row.id == local_id)
        .unwrap();
    assert!(after.mirrored_delegation_child);
    assert!(after.parent_delegation_id.is_none());
    assert_ne!(after.session_mutation_stamp, before.session_mutation_stamp);

    let snapshot = state.full_snapshot();
    let retained = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_id)
        .unwrap();
    assert!(retained.mirrored_delegation_child);
    assert!(retained.messages_loaded);
    assert_eq!(retained.messages.len(), 1);
    assert!(matches!(
        retained.messages.iter().next(),
        Some(Message::Text { text, .. }) if text == "Cached owner transcript"
    ));
    let persisted = load_state(state.persistence_path.as_path())
        .expect("read persisted state")
        .expect("state should have been committed");
    let index = persisted
        .find_session_index(&local_id)
        .expect("persisted proxy should exist");
    assert!(persisted.sessions[index].session.mirrored_delegation_child);
}

#[test]
fn older_remote_snapshot_does_not_unclassify_a_proxy_after_a_newer_tail() {
    let (state, remote, mut initial) = fixture();
    initial.revision = 1;
    let mut owner_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        initial.revision,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == initial.sessions[0].id)
    .unwrap();
    owner_session.messages = vec![remote_text_message("message-1", "Owner transcript")].into();
    owner_session.messages_loaded = true;
    owner_session.message_count = 1;
    initial.sessions[0] = test_state_session_summary_from_session(&owner_session);
    state
        .apply_remote_state_snapshot(&remote.id, initial.clone())
        .expect("seed an unclassified proxy");
    let local_id = {
        let inner = state.inner.lock().unwrap();
        let index = inner
            .find_remote_session_index(&remote.id, &owner_session.id)
            .unwrap();
        inner.sessions[index].session.id.clone()
    };
    let classified = || {
        state
            .summary_snapshot()
            .mirrored_delegation_child_ids
            .contains(&local_id)
    };
    assert!(!classified());
    owner_session.parent_delegation_id = Some("owner-delegation".to_owned());
    let tail_revision = 3;
    let (port, _, server) = spawn_remote_session_response_server(SessionResponse {
        revision: tail_revision,
        server_instance_id: "remote-test-instance".to_owned(),
        session: owner_session.clone(),
    });
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    state
        .get_session(&local_id)
        .expect("hydrate from the newer owner tail");
    join_test_server(server);
    assert!(
        classified(),
        "the classified proxy should leave Sessions after the tail"
    );

    let mut snapshot = initial.clone();
    snapshot.revision = tail_revision - 1;
    snapshot.sessions[0].preview = "Late owner summary".to_owned();
    state
        .apply_remote_state_snapshot(&remote.id, snapshot.clone())
        .unwrap();
    let published = state.summary_snapshot();
    let row = published
        .sessions
        .iter()
        .find(|row| row.id == local_id)
        .unwrap();
    assert_eq!(
        row.preview, "Late owner summary",
        "the older cut really applied"
    );
    assert!(classified(), "the older cut must not bring the child back");

    let mut sessions = initial.sessions;
    sessions[0] = test_state_session_summary_from_session(&owner_session);
    let delta = serde_json::to_string(&DeltaEvent::OrchestratorsUpdated {
        revision: tail_revision,
        orchestrators: initial.orchestrators,
        sessions,
    })
    .unwrap();
    dispatch_remote_event(&state, &remote.id, "delta", &[delta]).unwrap();
    assert!(
        classified(),
        "the matching live delta must keep the child out"
    );

    snapshot.revision = tail_revision;
    state
        .apply_remote_state_snapshot(&remote.id, snapshot.clone())
        .unwrap();
    assert!(
        !classified(),
        "an equal-revision snapshot can clear the flag"
    );

    snapshot.revision += 1;
    snapshot.sessions[0].parent_delegation_id = owner_session.parent_delegation_id;
    state
        .apply_remote_state_snapshot(&remote.id, snapshot)
        .unwrap();
    assert!(classified(), "a newer snapshot can set the flag again");
}

#[test]
fn remote_session_created_discards_retired_summary_fields_and_applies_session() {
    accepts_retired_summary_fields("sessionCreated");
}

#[test]
fn remote_orchestrators_updated_discards_retired_summary_fields_and_applies_sessions() {
    accepts_retired_summary_fields("orchestratorsUpdated");
}

#[test]
fn remote_summary_events_still_refuse_genuinely_unknown_fields_before_apply() {
    for kind in ["state", "sessionCreated", "orchestratorsUpdated"] {
        let (state, remote, snapshot) = fixture();
        let mut value = payload(&snapshot, kind, &json!({}));
        let session = if kind == "sessionCreated" {
            &mut value["session"]
        } else {
            &mut value["sessions"][1]
        };
        session["futureSummaryField"] = json!(true);
        let event = if kind == "state" { "state" } else { "delta" };
        let error = dispatch_remote_event(&state, &remote.id, event, &[value.to_string()])
            .expect_err("unknown summary field must refuse");
        assert!(format!("{error:#}").contains("futureSummaryField"));
        assert_unapplied(&state, &remote);
        let _ = fs::remove_file(state.persistence_path.as_path());
    }
}

fn http_resync(fields: Value, should_apply: bool) {
    let (state, remote, snapshot) = fixture();
    // Pin an HTTP connection so resync exercises the same lease and JSON path
    // as a remote SSE fallback, without launching an external transport.
    let value = payload(&snapshot, "state", &fields);
    let (port, requests, server) = spawn_remote_state_json_response_server(value.to_string());
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    let result = resync_remote_state_snapshot_with_authority(&state, &remote.id, None, false);
    join_test_server(server);
    assert_eq!(
        *requests.lock().expect("requests mutex poisoned"),
        vec!["GET /api/health HTTP/1.1", "GET /api/state HTTP/1.1"]
    );
    if should_apply {
        result.expect("older HTTP snapshot should apply");
        assert_applied(&state, &remote, snapshot.sessions.len());
    } else {
        let error = result.expect_err("unknown HTTP summary field must refuse");
        assert!(format!("{error:#}").contains("futureSummaryField"));
        assert_unapplied(&state, &remote);
    }
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_http_resync_discards_retired_summary_fields_and_applies_sessions() {
    http_resync(retired_fields().pop().unwrap(), true);
}

#[test]
fn remote_http_resync_still_refuses_genuinely_unknown_summary_fields() {
    // Keep this independent of retired-field compatibility, so its RED result
    // cannot be satisfied by refusing a retired field earlier in decoding.
    http_resync(json!({"futureSummaryField": true}), false);
}

#[test]
fn remote_summary_compatibility_keeps_duplicate_live_fields_invalid() {
    for kind in ["state", "sessionCreated", "orchestratorsUpdated"] {
        let (state, remote, snapshot) = fixture();
        let value = payload(&snapshot, kind, &retired_fields().pop().unwrap());
        let encoded = value.to_string().replace(
            r#""preview":"Older host summary""#,
            r#""preview":"first","preview":"Older host summary""#,
        );
        assert_ne!(
            encoded,
            value.to_string(),
            "fixture must contain duplicate preview fields"
        );
        let event = if kind == "state" { "state" } else { "delta" };
        let error = dispatch_remote_event(&state, &remote.id, event, &[encoded])
            .expect_err("duplicate live summary field must refuse");
        assert!(format!("{error:#}").contains("duplicate field `preview`"));
        assert_unapplied(&state, &remote);
        let _ = fs::remove_file(state.persistence_path.as_path());
    }
}

#[test]
fn canonical_summary_decode_stays_strict_outside_remote_ingress() {
    let (state, _remote, snapshot) = fixture();
    for fields in retired_fields() {
        let value = payload(&snapshot, "sessionCreated", &fields);
        let error = serde_json::from_value::<StateSessionSummary>(value["session"].clone())
            .expect_err("canonical summary contract must not accept retired fields");
        assert!(error.to_string().contains("unknown field"));
    }
    let _ = fs::remove_file(state.persistence_path.as_path());
}
