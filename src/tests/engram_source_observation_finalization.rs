//! Real SQLite session-row and WAL proof for ordinary observation custody.
use super::*;

#[test]
fn ordinary_observation_actual_worker_sends_committed_requests_then_settles_without_checkpoint() {
    let fixture = ObservationSession::new();
    let report = fixture.report();
    fixture.retain(report.clone());
    fixture.persist();
    let transport = Arc::new(ObservationReplies::default());
    *transport.durable_scope.lock().unwrap() = Some((fixture.state.persistence_path.as_ref().clone(), fixture.id.clone()));
    for index in 0..report.envelopes.len() {
        transport.replies.lock().unwrap().push_back(Ok(observation_receipt(&report_observation(&report, index), &report.observing_session)));
    }
    fixture.state.install_test_engram_transport(transport.clone());
    let (idle_tx, idle_rx) = std::sync::mpsc::channel();
    *fixture.state.runtime_observation_lifetime.install_hook.lock().unwrap() = Some(Arc::new(move |phase| {
        if phase == "idle" { let _ = idle_tx.send(()); }
    }));
    let state = fixture.state.clone();
    let id = fixture.id.clone();
    let worker = std::thread::spawn(move || state.run_runtime_observation_worker(&id));
    let idle = idle_rx.recv_timeout(DEADLOCK_GUARD);
    // Join even on a failed observation; never leave the fixture worker behind.
    fixture.state.shutdown_signal_tx.send_replace(true);
    let joined = worker.join();
    idle.expect("ordinary worker must settle both retained envelopes");
    joined.unwrap();
    let sent = transport.sent.lock().unwrap();
    assert_eq!(sent.len(), report.envelopes.len());
    for (index, (connection, wire)) in sent.iter().enumerate() {
        assert_eq!(connection, &report.connection);
        assert_eq!(wire, &report.envelopes[index].request);
    }
    fixture.record(|record| {
        assert!(record.engram.unadmitted_reports[0].is_settled());
        assert!(record.engram.runtime_checkpoint.is_none());
        assert!(record.engram.work_binding.is_none());
    });
}

#[test]
fn ordinary_observation_report_survives_save_reload_with_exact_retry_bytes() {
    let fixture = ObservationSession::new();
    let report = fixture.report();
    let bytes = serde_json::to_vec(&report).unwrap();
    fixture.retain(report.clone());
    fixture.persist();
    assert!(runtime_observation_report_is_durable(fixture.state.persistence_path.as_ref(), &fixture.id, &report).unwrap());
    let restored = load_state(fixture.state.persistence_path.as_ref()).unwrap().unwrap();
    let record = &restored.sessions[restored.find_session_index(&fixture.id).unwrap()];
    assert_eq!(serde_json::to_vec(&record.engram.unadmitted_reports[0]).unwrap(), bytes);
    assert!(record.engram.runtime_checkpoint.is_none());
    assert_eq!(record.engram.unadmitted_reports[0].pending(), Some(0));
}

#[test]
fn ordinary_observation_wal_uncommitted_or_rolled_back_report_is_not_durable() {
    let fixture = ObservationSession::new();
    let report = fixture.report();
    fixture.persist();
    let connection = rusqlite::Connection::open(fixture.state.persistence_path.as_ref()).unwrap();
    connection.pragma_update(None, "journal_mode", "WAL").unwrap();
    let text: String = connection.query_row("SELECT value_json FROM sessions WHERE id = ?1", [&fixture.id], |row| row.get(0)).unwrap();
    let mut row: Value = serde_json::from_str(&text).unwrap();
    row["engramUnadmittedReports"] = json!([report]);
    for commit in [false, true] {
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        connection.execute("UPDATE sessions SET value_json = ?1 WHERE id = ?2", rusqlite::params![row.to_string(), fixture.id]).unwrap();
        assert!(!runtime_observation_report_is_durable(fixture.state.persistence_path.as_ref(), &fixture.id, &report).unwrap(),
            "a second connection must not certify an uncommitted row");
        connection.execute_batch(if commit { "COMMIT" } else { "ROLLBACK" }).unwrap();
        assert_eq!(runtime_observation_report_is_durable(fixture.state.persistence_path.as_ref(), &fixture.id, &report).unwrap(), commit);
    }
}

#[test]
fn ordinary_observation_removal_fence_requires_the_exact_session_projection() {
    let fixture = ObservationSession::new();
    let report = fixture.report();
    fixture.retain(report);
    fixture.persist();
    fixture.record(|record| {
        record.engram.observation_delete_requested = true;
        record.set_auto_dispatch_blocked(true);
    });
    let target = fixture.removal_target();
    let connection = rusqlite::Connection::open(fixture.state.persistence_path.as_ref()).unwrap();
    assert!(!target.is_already_durable(&connection).unwrap(), "an older row with the same report is insufficient");
    fixture.persist();
    assert!(target.is_already_durable(&connection).unwrap());
    fixture.record(|record| record.engram.unadmitted_reports[0].version += 1);
    let successor = fixture.removal_target();
    assert!(!successor.is_already_durable(&connection).unwrap(), "an older removal marker cannot certify a newer report image");
}

#[test]
fn ordinary_observation_metadata_commit_cannot_ack_a_deferred_session_row() {
    let fixture = ObservationSession::new();
    fixture.retain(fixture.report());
    let mut cache = SqlitePersistConnectionCache::new();
    let initial = collect_persist_delta_from_shared_state(&fixture.state.inner, 0);
    persist_delta_via_cache(&mut cache, fixture.state.persistence_path.as_ref(), &initial).unwrap();
    fixture.record(|record| {
        record.engram.observation_delete_requested = true;
        record.set_auto_dispatch_blocked(true);
    });
    let target = fixture.removal_target();
    let mut delta = collect_persist_delta_from_shared_state(&fixture.state.inner, initial.watermark);
    assert!(delta.changed_sessions.iter().any(|record| record.session.id == fixture.id));
    delta.changed_sessions.retain(|record| record.session.id != fixture.id);
    delta.deferred_session_ids.push(fixture.id.clone());
    persist_delta_via_cache(&mut cache, fixture.state.persistence_path.as_ref(), &delta).unwrap();
    assert!(!target.is_already_durable(cache.connection.as_ref().unwrap()).unwrap());
    fixture.persist();
    assert!(target.is_already_durable(cache.connection.as_ref().unwrap()).unwrap());
}
