// Shared ACP tool-status regression tests, extracted from src/tests/cursor.rs.
// Owns status normalization and its Cursor transcript/reload integration case.
// Does not own agent mode, permission, model discovery, or deferred-persist
// scheduling coverage. Test logic is unchanged; persistence scope is clarified.

use super::*;

#[test]
fn acp_completed_tool_status_does_not_require_a_shell_exit_code() {
    for raw_output in [
        Value::Null,
        json!({"success": true}),
        json!({"totalDiagnostics": 0, "totalFiles": 0}),
        json!({"content": "file contents"}),
        json!("plain text result"),
        json!({"exitCode": 0}),
        // Tool output is arbitrary data, not another ACP status envelope.
        json!({"success": false, "error": "example data read from a file"}),
    ] {
        let update = json!({"status": "completed", "rawOutput": raw_output});
        assert_eq!(acp_tool_status(&update), CommandStatus::Success, "{update}");
    }
    assert_eq!(
        acp_tool_status(&json!({"status": "completed"})),
        CommandStatus::Success
    );
    for exit_code in [-1, 1, 127] {
        assert_eq!(
            acp_tool_status(&json!({
                "status": "completed", "rawOutput": {"exitCode": exit_code}
            })),
            CommandStatus::Error
        );
    }
    for status in ["failed", "error"] {
        assert_eq!(
            acp_tool_status(&json!({"status": status, "rawOutput": {"exitCode": 0}})),
            CommandStatus::Error
        );
    }
    for status in ["pending", "in_progress", "unknown"] {
        assert_eq!(
            acp_tool_status(&json!({"status": status})),
            CommandStatus::Running
        );
    }
}

#[test]
fn acp_completed_tool_updates_preserve_status_in_cursor_transcript() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Cursor);
    let (input_tx, _input_rx) = mpsc::channel();
    let mut turn_state = AcpTurnState::default();
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let cases = [
        (
            "Read file",
            "completed",
            json!({"success": true}),
            CommandStatus::Success,
        ),
        (
            "Diagnostics",
            "completed",
            json!({"totalDiagnostics": 0}),
            CommandStatus::Success,
        ),
        (
            "Shell failure",
            "completed",
            json!({"exitCode": 1}),
            CommandStatus::Error,
        ),
        ("Read failure", "failed", Value::Null, CommandStatus::Error),
    ];
    for (title, status, raw_output, _) in &cases {
        for update in [
            json!({"sessionUpdate": "tool_call", "toolCallId": title, "title": title}),
            json!({
                "sessionUpdate": "tool_call_update", "toolCallId": title,
                "status": status, "rawOutput": raw_output
            }),
        ] {
            handle_acp_session_update(
                &update,
                &state,
                &session_id,
                &input_tx,
                &mut turn_state,
                &mut recorder,
                AcpAgent::Cursor,
            )
            .unwrap();
        }
    }
    let assert_commands = |session: &Session| {
        let commands = session
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::Command { status, .. } => Some(*status),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            commands,
            cases.iter().map(|case| case.3).collect::<Vec<_>>()
        );
    };
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_commands(&inner.sessions[inner.find_session_index(&session_id).unwrap()].session);
    // Existing-command updates use the non-persisting delta path. This explicit
    // synchronous flush tests the reload representation, not the scheduling or
    // eventual durability of the production deferred-persist path.
    state.persist_internal_locked(&inner).unwrap();
    drop(inner);
    let saved = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_commands(&saved.sessions[saved.find_session_index(&session_id).unwrap()].session);
}
