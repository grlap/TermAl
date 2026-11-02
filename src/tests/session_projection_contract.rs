// Owns serialized HTTP coverage of session metadata and private-body boundaries.
// Reuses the real router and route fixtures; does not implement projection policy.
use super::*;

// These keys belong to targeted detail, not the broad metadata protocol.
// Empty optional values may be absent in detail under the normal serde policy.
const TARGETED_KEYS: &[&str] = &[
    "messages",
    "messagesLoaded",
    "promptHistory",
    "promptHistoryRedacted",
    "pendingPrompts",
    "promptDeliveries",
    "promptAdmissionBudgets",
    "providerHandoffs",
    "providerHandoffHistory",
];

fn seed_projection_metadata(state: &AppState, session_id: &str) -> Value {
    let catalog = json!([{
        "label": "Live option", "value": "catalog-option", "description": "Description",
        "badges": ["live"], "supportedClaudeEffortLevels": ["high"],
        "defaultReasoningEffort": "high", "supportedReasoningEfforts": ["high"],
        "serviceTiers": [{"id": "fast", "label": "Fast", "description": "Fast tier"}]
    }]);
    let mut metadata = json!({
        "name": "Projection fixture", "emoji": "P", "projectId": "projection-project",
        "model": "projection-model", "modelOptions": catalog,
        "approvalPolicy": "on-request", "reasoningEffort": "high", "codexFastMode": true,
        "sandboxMode": "workspace-write", "cursorMode": "plan", "claudeEffort": "high",
        "claudeApprovalMode": "ask", "geminiApprovalMode": "default"
    });
    let provider_metadata = json!({
        "opencodeModel": "provider/model", "opencodeEffort": "high",
        "opencodeCurrentEffort": "high", "opencodeEffortOptions": catalog,
        "opencodeMode": "build", "opencodeCurrentMode": "build", "opencodeModeOptions": catalog,
        "opencodeApprovalMode": "ask", "kimiEffort": "high", "kimiCurrentEffort": "high",
        "kimiEffortOptions": catalog, "kimiApprovalMode": "ask", "kimiMode": "plan",
        "kimiCurrentMode": "plan", "externalSessionId": "external-thread"
    });
    let activity_metadata = json!({
        "agentCommandsRevision": 7, "codexThreadState": "active", "status": "active",
        "preview": "Public preview", "queuePaused": true, "providerCapacityWaiting": true,
        "sourceTrackingRecovery": true, "parentDelegationId": "delegation-parent",
        "promptHistory": ["PRIVATE-HISTORY-CONTENT"],
        "pendingPrompts": [{
            "id": "queued-prompt", "timestamp": "12:00", "text": "PRIVATE-QUEUED-CONTENT",
            "expandedText": "PRIVATE-EXPANDED-CONTENT"
        }],
        "markers": [{
            "id": "marker-a", "sessionId": session_id, "kind": "checkpoint", "name": "Checkpoint",
            "body": "Marker body", "color": "#123456", "messageId": "history-message-00002",
            "messageIndexHint": 2, "createdAt": "2026-10-31T12:00:00Z",
            "updatedAt": "2026-10-31T12:00:00Z", "createdBy": "user"
        }],
        "liveActivity": {
            "prompt": format!("start\n {} PRIVATE-LIVE-PROMPT-END", "prompt ".repeat(50)),
            "command": format!("run\n {} PRIVATE-LIVE-COMMAND-END", "command ".repeat(50)),
            "commandStatus": "running"
        }
    });
    for fields in [provider_metadata, activity_metadata] {
        metadata
            .as_object_mut()
            .expect("metadata object")
            .extend(fields.as_object().expect("fixture fields").clone());
    }
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(session_id)
        .expect("fixture session exists");
    let record = &mut inner.sessions[index];
    let mut value = serde_json::to_value(&record.session).expect("session serializes");
    value
        .as_object_mut()
        .expect("session object")
        .extend(metadata.as_object().expect("metadata object").clone());
    record.session = serde_json::from_value(value).expect("metadata fixture is valid");
    // These two wire fields derive from record authority, not the embedded Session.
    record.orchestrator_auto_dispatch_blocked = true;
    record.engram.source_observation_delete_requested = true;
    inner.stamp_session_at_index(index);
    metadata
}

async fn get_json(app: &Router, uri: String) -> Value {
    let (status, body) = request_json(app, Request::get(uri).body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    body
}

#[tokio::test]
async fn serialized_session_projection_preserves_metadata_and_redacts_private_bodies() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    seed_loaded_history_messages(&state, &session_id, 8);
    let metadata = seed_projection_metadata(&state, &session_id);
    let full = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session exists");
        AppState::wire_session_for_inner(&inner, &state.server_instance_id, &inner.sessions[index])
    };
    let full = serde_json::to_value(full).expect("full projection serializes");
    let full_object = full.as_object().unwrap();
    assert_eq!(full["engramControlOff"], false,
        "a plain local session is outside the positive Base/OFF lane");
    let app = app_router(state.clone());

    // No handoffFrom/handoffTo query is used here: those optional bounds
    // deliberately filter providerHandoffs through the existing API owner.
    for limit in [1, 8] {
        let body = get_json(&app, format!("/api/sessions/{session_id}?tail={limit}")).await;
        let tail = body["session"].as_object().expect("HTTP tail is an object");
        // All serialized fields inherit full detail; only transcript residency narrows.
        assert_eq!(tail.len(), full_object.len());
        for (key, value) in full_object {
            if !matches!(key.as_str(), "messages" | "messagesLoaded") {
                assert_eq!(tail.get(key), Some(value), "tail lost full field {key}");
            }
        }
        // Independent fixture values also catch a field dropped by BOTH builders.
        for (key, value) in metadata.as_object().unwrap() {
            assert_eq!(tail.get(key), Some(value), "targeted metadata lost {key}");
        }
        assert_eq!(tail["messages"].as_array().unwrap().len(), limit);
        assert_eq!(tail["messagesLoaded"], limit == 8);
        assert_eq!(tail["messageCount"], 8);
    }

    let body = get_json(&app, "/api/state".to_owned()).await;
    let summary = body["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == session_id)
        .unwrap()
        .as_object()
        .unwrap();
    for key in TARGETED_KEYS {
        assert!(
            !summary.contains_key(*key),
            "private key leaked into broad state: {key}"
        );
    }
    for (key, value) in full_object {
        if !TARGETED_KEYS.contains(&key.as_str()) && key != "liveActivity" {
            assert_eq!(summary.get(key), Some(value), "summary lost metadata {key}");
        }
    }
    // mailboxParticipantId is an optional broad discovery hint, not a Session field.
    // Valid local bodySeq/bodySeqEpoch pairs must match; unpaired legacy pairs
    // normalize together to absent in summary, under their existing currency owner.
    for key in summary.keys() {
        assert!(
            full_object.contains_key(key) || key == "mailboxParticipantId",
            "unclassified summary field {key}"
        );
    }
    for (field, tail_sentinel) in [
        ("prompt", "PRIVATE-LIVE-PROMPT-END"),
        ("command", "PRIVATE-LIVE-COMMAND-END"),
    ] {
        let hint = summary["liveActivity"][field].as_str().unwrap();
        assert_eq!(hint.chars().count(), 160);
        assert!(!hint.contains('\n'));
        assert!(!hint.contains(tail_sentinel));
        assert!(
            full["liveActivity"][field]
                .as_str()
                .unwrap()
                .contains(tail_sentinel)
        );
    }
    assert_eq!(summary["liveActivity"]["commandStatus"], "running");
    let broad = serde_json::to_string(summary).unwrap();
    for private in [
        "PRIVATE-HISTORY-CONTENT",
        "PRIVATE-QUEUED-CONTENT",
        "PRIVATE-EXPANDED-CONTENT",
    ] {
        assert!(!broad.contains(private));
    }
}
