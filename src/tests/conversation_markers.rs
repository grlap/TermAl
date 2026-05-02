use super::*;

fn test_session_with_two_messages(state: &AppState) -> String {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner.create_session(
        Agent::Codex,
        Some("Marked".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let session_id = record.session.id.clone();
    let index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    let record = inner
        .session_mut_by_index(index)
        .expect("session index should be valid");
    push_message_on_record(
        record,
        Message::Text {
            attachments: Vec::new(),
            id: "message-1".to_owned(),
            timestamp: stamp_now(),
            author: Author::You,
            text: "First".to_owned(),
            expanded_text: None,
        },
    );
    push_message_on_record(
        record,
        Message::Text {
            attachments: Vec::new(),
            id: "message-2".to_owned(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: "Second".to_owned(),
            expanded_text: None,
        },
    );
    state.commit_locked(&mut inner).unwrap();
    session_id
}

#[test]
fn conversation_marker_crud_updates_session_and_publishes_deltas() {
    let state = test_app_state();
    let session_id = test_session_with_two_messages(&state);
    let mut delta_events = state.subscribe_delta_events();

    let created = state
        .create_conversation_marker(
            &session_id,
            CreateConversationMarkerRequest {
                kind: ConversationMarkerKind::Decision,
                name: " Decision point ".to_owned(),
                body: Some(" Keep the overview rail. ".to_owned()),
                color: "#3B82F6".to_owned(),
                message_id: "message-1".to_owned(),
                end_message_id: None,
            },
        )
        .expect("marker should be created");
    assert_eq!(created.marker.name, "Decision point");
    assert_eq!(
        created.marker.body.as_deref(),
        Some("Keep the overview rail.")
    );
    assert_eq!(created.marker.color, "#3b82f6");
    assert_eq!(created.marker.message_index_hint, 0);

    let payload = delta_events
        .try_recv()
        .expect("create marker should publish a delta");
    let delta: DeltaEvent = serde_json::from_str(&payload).expect("delta should deserialize");
    match delta {
        DeltaEvent::ConversationMarkerCreated {
            session_id: delta_session_id,
            marker,
            session_mutation_stamp,
            ..
        } => {
            assert_eq!(delta_session_id, session_id);
            assert_eq!(marker.id, created.marker.id);
            assert!(session_mutation_stamp.is_some());
        }
        _ => panic!("expected ConversationMarkerCreated delta"),
    }

    let listed = state
        .list_conversation_markers(&session_id)
        .expect("markers should list");
    assert_eq!(listed.markers.len(), 1);

    let updated = state
        .update_conversation_marker(
            &session_id,
            &created.marker.id,
            UpdateConversationMarkerRequest {
                kind: Some(ConversationMarkerKind::Checkpoint),
                name: Some("Checkpoint".to_owned()),
                body: Some(None),
                color: Some("#22c55e".to_owned()),
                message_id: None,
                end_message_id: Some(Some("message-2".to_owned())),
            },
        )
        .expect("marker should update");
    assert_eq!(updated.marker.kind, ConversationMarkerKind::Checkpoint);
    assert_eq!(updated.marker.body, None);
    assert_eq!(updated.marker.end_message_id.as_deref(), Some("message-2"));
    assert_eq!(updated.marker.end_message_index_hint, Some(1));

    let payload = delta_events
        .try_recv()
        .expect("update marker should publish a delta");
    let delta: DeltaEvent = serde_json::from_str(&payload).expect("delta should deserialize");
    match delta {
        DeltaEvent::ConversationMarkerUpdated { marker, .. } => {
            assert_eq!(marker.name, "Checkpoint");
        }
        _ => panic!("expected ConversationMarkerUpdated delta"),
    }

    let deleted = state
        .delete_conversation_marker(&session_id, &created.marker.id)
        .expect("marker should delete");
    assert_eq!(deleted.marker_id, created.marker.id);

    let payload = delta_events
        .try_recv()
        .expect("delete marker should publish a delta");
    let delta: DeltaEvent = serde_json::from_str(&payload).expect("delta should deserialize");
    match delta {
        DeltaEvent::ConversationMarkerDeleted { marker_id, .. } => {
            assert_eq!(marker_id, created.marker.id);
        }
        _ => panic!("expected ConversationMarkerDeleted delta"),
    }

    let listed = state
        .list_conversation_markers(&session_id)
        .expect("markers should list after delete");
    assert!(listed.markers.is_empty());
}

#[test]
fn conversation_marker_rejects_missing_message_anchor() {
    let state = test_app_state();
    let session_id = test_session_with_two_messages(&state);

    let err = state
        .create_conversation_marker(
            &session_id,
            CreateConversationMarkerRequest {
                kind: ConversationMarkerKind::Bug,
                name: "Missing anchor".to_owned(),
                body: None,
                color: "#ef4444".to_owned(),
                message_id: "missing-message".to_owned(),
                end_message_id: None,
            },
        )
        .expect_err("missing message anchor should be rejected");

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("message id was not found"));
}
