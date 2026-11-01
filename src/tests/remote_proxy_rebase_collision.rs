// Owns the witnesses that a remote proxy's tail rebase over a peer insert
// moves a durable peer identity to its upstream position without touching the
// rest of the omitted history, even when the message's body changed, and that
// a local transcript in the same shape, a different message under the same
// peer id, or a duplicate inside the window still refuses the identity. Does not own the differential writer,
// the remote apply paths it drives, or any isolation of a conflicting session.
use super::remote::remote_text_message;
use super::*;

fn peer_messages(ids: &[&str]) -> MessageSequence {
    ids.iter()
        .map(|id| remote_text_message(id, &format!("body of {id}")))
        .collect::<Vec<_>>()
        .into()
}

fn durable_rows(connection: &rusqlite::Connection, session_id: &str) -> Vec<(i64, String)> {
    connection
        .prepare("SELECT position, message_id FROM messages WHERE session_id = ?1 ORDER BY position")
        .unwrap()
        .query_map([session_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn rows(expected: &[(i64, &str)]) -> Vec<(i64, String)> {
    expected
        .iter()
        .map(|(position, id)| (*position, (*id).to_owned()))
        .collect()
}

#[test]
fn a_tail_rebase_over_an_earlier_insert_moves_the_durable_peer_identity() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-proxy-rebase".to_owned(),
        name: "Proxy Rebase".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/proxy-rebase",
        "Proxy Rebase",
        "remote-project-proxy-rebase",
    );
    let initial_state = sample_remote_orchestrator_state(
        "remote-project-proxy-rebase",
        "/remote/proxy-rebase",
        3,
        OrchestratorInstanceStatus::Running,
    );
    let peer_session = initial_state
        .sessions
        .iter()
        .find(|session| session.id == "remote-session-1")
        .expect("sample remote session should exist")
        .clone();
    state
        .apply_remote_state_snapshot(&remote.id, initial_state.into_state_response())
        .expect("initial remote state should apply");

    // The peer's whole transcript, as one full bounded-tail hydration.
    let mut full = peer_session.clone();
    full.messages = peer_messages(&["peer-0", "peer-1", "peer-2", "peer-3", "peer-4"]);
    full.message_count = 5;
    full.messages_loaded = true;
    full.session_mutation_stamp = Some(10);
    // The peer then inserts one message before the proxy's next tail window,
    // as a queued prompt promoted behind a streaming reply does, and the proxy
    // receives only the last three messages of six.
    let mut rebased_tail = peer_session.clone();
    rebased_tail.messages = peer_messages(&["peer-2", "peer-3", "peer-4"]);
    rebased_tail.message_count = 6;
    rebased_tail.messages_loaded = false;
    rebased_tail.session_mutation_stamp = Some(11);

    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote session should be mirrored");
    let project_id = inner.sessions[index].session.project_id.clone();
    apply_remote_session_to_record(
        inner
            .session_mut_by_index(index)
            .expect("mirrored session index should be valid"),
        &remote.id,
        project_id.clone(),
        &full,
    );
    state
        .commit_remote_localization_locked(&mut inner)
        .expect("the full proxy transcript should persist at positions 0..5");

    apply_remote_session_to_record(
        inner
            .session_mut_by_index(index)
            .expect("mirrored session index should be valid"),
        &remote.id,
        project_id,
        &rebased_tail,
    );
    let record = &inner.sessions[index];
    assert_eq!(
        record.message_start_index, 3,
        "the tail rebase places peer-2 at position 3 while its durable row stays at 2"
    );
    let local_session_id = record.session.id.clone();
    state
        .commit_remote_localization_locked(&mut inner)
        .expect("the rebased proxy tail should persist");
    drop(inner);

    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    assert_eq!(
        durable_rows(&connection, &local_session_id),
        rows(&[
            (0, "peer-0"),
            (1, "peer-1"),
            (3, "peer-2"),
            (4, "peer-3"),
            (5, "peer-4"),
        ]),
        "peer-2 moves to its upstream position and the omitted history keeps its rows"
    );
    drop(connection);

    // The moved identity leaves a gap in the retained rows, so a reload does
    // not certify the local rows as the current transcript: the proxy starts
    // unhydrated and its history comes from the peer again.
    let reloaded = load_state(state.persistence_path.as_path())
        .expect("the rebased proxy state should load")
        .expect("the rebased proxy state should exist");
    let proxy = reloaded
        .sessions
        .iter()
        .find(|record| record.session.id == local_session_id)
        .expect("the proxy should survive reload");
    assert_eq!(proxy.session.message_count, 6);
    assert!(proxy.session.messages.is_empty());
    assert!(!proxy.session.messages_loaded);
}

fn window(
    local_identity_namespace: bool,
    start: usize,
    ids: &[&str],
    message_count: usize,
) -> SerializedPersistedSession {
    SerializedPersistedSession {
        session_id: "session-shifted-window".into(),
        local_identity_namespace,
        message_start_index: start,
        message_count,
        truncate_to: Some(message_count),
        snapshot: peer_messages(ids),
        write_overview: false,
        prompt_history_value_json: None,
        value_json: "{}".into(),
        messages: ids
            .iter()
            .enumerate()
            .map(|(index, id)| SerializedPersistedMessage {
                position: start + index,
                message_id: (*id).to_owned(),
                value_json: text_body(id, "assistant", &format!("body of {id}")),
                overview_kind: ConversationOverviewKind::Text,
                is_user: false,
            })
            .collect(),
    }
}

fn text_body(id: &str, author: &str, text: &str) -> String {
    json!({"type": "text", "id": id, "author": author, "text": text}).to_string()
}

fn command_body(id: &str, status: &str) -> String {
    json!({"type": "command", "id": id, "author": "assistant", "command": "cargo test", "status": status})
        .to_string()
}

/// Writes peer rows 0..5 durably, then the window shifted by one peer insert
/// before it (message-2..message-4 at positions 3..6), with the given durable
/// and incoming bodies of message-2.
fn write_shifted_window_with(
    local_identity_namespace: bool,
    durable_message_two: String,
    incoming_message_two: String,
) -> (rusqlite::Connection, Result<()>) {
    let mut connection = rusqlite::Connection::open_in_memory().unwrap();
    ensure_sqlite_state_schema(&connection).unwrap();
    let mut durable = window(
        local_identity_namespace,
        0,
        &["message-0", "message-1", "message-2", "message-3", "message-4"],
        5,
    );
    durable.messages[2].value_json = durable_message_two;
    let tx = connection.transaction().unwrap();
    write_serialized_persisted_session(&tx, &durable).unwrap();
    tx.commit().unwrap();
    let result = {
        let mut incoming = window(
            local_identity_namespace,
            3,
            &["message-2", "message-3", "message-4"],
            6,
        );
        incoming.messages[0].value_json = incoming_message_two;
        let tx = connection.transaction().unwrap();
        let result = write_serialized_persisted_messages(&tx, &incoming);
        if result.is_ok() {
            tx.commit().unwrap();
        }
        result
    };
    (connection, result)
}

fn write_shifted_window(local_identity_namespace: bool) -> (rusqlite::Connection, Result<()>) {
    let body = text_body("message-2", "assistant", "body of message-2");
    write_shifted_window_with(local_identity_namespace, body.clone(), body)
}

fn assert_moved(connection: &rusqlite::Connection) {
    assert_eq!(
        durable_rows(connection, "session-shifted-window"),
        rows(&[
            (0, "message-0"),
            (1, "message-1"),
            (3, "message-2"),
            (4, "message-3"),
            (5, "message-4"),
        ]),
        "message-2 moves to its upstream position and the omitted rows stay"
    );
}

fn assert_refused_and_preserved(connection: &rusqlite::Connection, result: Result<()>) {
    let error = format!(
        "{:#}",
        result.expect_err("a row that is not the same message relocated must not be released")
    );
    assert!(
        error.contains("UNIQUE constraint failed: messages.session_id, messages.message_id"),
        "{error}"
    );
    assert_eq!(
        durable_rows(connection, "session-shifted-window"),
        rows(&[
            (0, "message-0"),
            (1, "message-1"),
            (2, "message-2"),
            (3, "message-3"),
            (4, "message-4"),
        ]),
        "the refused write rolls back with everything it released"
    );
}

#[test]
fn a_local_transcript_in_the_shifted_shape_still_refuses_the_duplicate_identity() {
    let (connection, result) = write_shifted_window(true);
    let error = format!("{:#}", result.expect_err("a local identity must not move silently"));
    assert!(
        error.contains("failed to write transcript position 3 for `session-shifted-window`"),
        "{error}"
    );
    assert!(
        error.contains("UNIQUE constraint failed: messages.session_id, messages.message_id"),
        "{error}"
    );
    drop(connection);

    let (connection, result) = write_shifted_window(false);
    result.expect("a peer identity moves to its upstream position");
    assert_moved(&connection);
}

#[test]
fn a_relocated_peer_message_moves_even_when_its_body_changed() {
    // Streamed text grows under the same id.
    let (connection, result) = write_shifted_window_with(
        false,
        text_body("message-2", "assistant", "partial"),
        text_body("message-2", "assistant", "partial answer, now complete"),
    );
    result.expect("a grown text body is the same message relocated");
    assert_moved(&connection);
    drop(connection);

    // A running command settles as an error under the same id.
    let (connection, result) = write_shifted_window_with(
        false,
        command_body("message-2", "running"),
        command_body("message-2", "error"),
    );
    result.expect("a settled command is the same message relocated");
    assert_moved(&connection);
}

#[test]
fn a_peer_identity_arriving_as_a_different_message_still_fails() {
    // Another author under the same peer id.
    let (connection, result) = write_shifted_window_with(
        false,
        text_body("message-2", "assistant", "an answer"),
        text_body("message-2", "you", "a prompt"),
    );
    assert_refused_and_preserved(&connection, result);
    drop(connection);

    // Another message type under the same peer id.
    let (connection, result) = write_shifted_window_with(
        false,
        text_body("message-2", "assistant", "an answer"),
        command_body("message-2", "running"),
    );
    assert_refused_and_preserved(&connection, result);
    drop(connection);

    // A durable row whose author cannot be read is never proof of sameness.
    let (connection, result) = write_shifted_window_with(
        false,
        json!({"type": "text", "id": "message-2", "text": "no author"}).to_string(),
        text_body("message-2", "assistant", "an answer"),
    );
    assert_refused_and_preserved(&connection, result);
    drop(connection);

    // Nor is a durable body that names another id.
    let (connection, result) = write_shifted_window_with(
        false,
        text_body("message-9", "assistant", "an answer"),
        text_body("message-2", "assistant", "an answer"),
    );
    assert_refused_and_preserved(&connection, result);
}

#[test]
fn a_duplicate_peer_identity_inside_the_window_fails_and_keeps_the_durable_rows() {
    let mut connection = rusqlite::Connection::open_in_memory().unwrap();
    ensure_sqlite_state_schema(&connection).unwrap();
    let tx = connection.transaction().unwrap();
    write_serialized_persisted_session(
        &tx,
        &window(
            false,
            0,
            &["message-0", "message-1", "message-2", "message-3", "message-4"],
            5,
        ),
    )
    .unwrap();
    tx.commit().unwrap();

    let result = {
        let tx = connection.transaction().unwrap();
        // The same peer id twice inside one retained window is not a
        // relocation; nothing may be released for it.
        write_serialized_persisted_messages(
            &tx,
            &window(
                false,
                0,
                &["message-0", "message-1", "message-2", "message-2", "message-4"],
                5,
            ),
        )
    };
    assert_refused_and_preserved(&connection, result);
}
