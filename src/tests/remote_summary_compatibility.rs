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
