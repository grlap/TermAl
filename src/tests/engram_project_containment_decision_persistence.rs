//! Real SQLite/WAL content fences for ordinary delivery, without gate decisions.
use super::*;

#[test]
fn ordinary_write_ahead_wal_proof_requires_committed_exact_receipt_nonce() {
    let (turn, history) = ordinary_with_history("ordinary-wal-receipt-proof");
    let (id, nonce) = fixture_handoff_receipt(&turn, "write-ahead content");
    persist_fixture(&turn);
    let target = PersistFenceTarget::PromptDeliveryWriteAhead {
        session_id: turn.session.clone(), prompt_id: id.clone(), write_ahead: nonce,
    };
    let writer = rusqlite::Connection::open(turn.state.persistence_path.as_ref()).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    let reader = rusqlite::Connection::open(turn.state.persistence_path.as_ref()).unwrap();
    assert!(target.is_already_durable(&reader).unwrap());
    let text: String = writer.query_row("SELECT value_json FROM sessions WHERE id = ?1", [&turn.session], |row| row.get(0)).unwrap();
    let mut row: Value = serde_json::from_str(&text).unwrap();
    row["promptDeliveries"][&id]["writeAhead"] = json!("different-committed-attempt");
    for commit in [false, true] {
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        writer.execute("UPDATE sessions SET value_json = ?1 WHERE id = ?2", rusqlite::params![row.to_string(), turn.session]).unwrap();
        assert!(!target.is_already_durable(&writer).unwrap(), "same writer sees a mismatching uncommitted nonce");
        assert!(target.is_already_durable(&reader).unwrap(), "another snapshot still sees the original committed attempt");
        writer.execute_batch(if commit { "COMMIT" } else { "ROLLBACK" }).unwrap();
        assert_eq!(target.is_already_durable(&reader).unwrap(), !commit);
    }
    turn.assert_no_prompt();
    assert_history(&turn, &history);
}

#[test]
fn ordinary_metadata_commit_cannot_ack_a_deferred_own_session_projection() {
    let (turn, history) = ordinary_with_history("ordinary-deferred-session-proof");
    turn.enqueue("original queued bytes", QueuedPromptSource::User);
    let mut cache = SqlitePersistConnectionCache::new();
    let initial = collect_persist_delta_from_shared_state(&turn.state.inner, 0);
    persist_delta_via_cache(&mut cache, turn.state.persistence_path.as_ref(), &initial).unwrap();
    turn.edit(|record| record.queued_prompts[0].pending_prompt.text.push_str(" changed"));
    let target = row_fence(&turn);
    // Materialize an independently stale row to make the own-row negative
    // decisive even though OrdinaryRoot's normal writer is synchronous.
    let connection = cache.connection.as_ref().unwrap();
    let text: String = connection.query_row("SELECT value_json FROM sessions WHERE id = ?1", [&turn.session], |row| row.get(0)).unwrap();
    let mut stale: Value = serde_json::from_str(&text).unwrap();
    stale["queuedPrompts"][0]["pending_prompt"]["text"] = json!("original queued bytes");
    connection.execute("UPDATE sessions SET value_json = ?1 WHERE id = ?2", rusqlite::params![stale.to_string(), turn.session]).unwrap();
    let mut delta = collect_persist_delta_from_shared_state(&turn.state.inner, 0);
    delta.changed_sessions.retain(|record| record.session.id != turn.session);
    delta.deferred_session_ids.push(turn.session.clone());
    persist_delta_via_cache(&mut cache, turn.state.persistence_path.as_ref(), &delta).unwrap();
    assert!(!target.is_already_durable(cache.connection.as_ref().unwrap()).unwrap());
    persist_fixture(&turn);
    assert!(target.is_already_durable(cache.connection.as_ref().unwrap()).unwrap());
    turn.assert_no_prompt();
    assert_history(&turn, &history);
}
