// Deterministic read bounds and recovery at the original settlement commit.
use super::*;

fn page_cache_misses(connection: &rusqlite::Connection, reset: bool) -> i32 {
    let mut misses = 0;
    let mut high_water = 0;
    // The caller holds the connection guard. SQLite only reads or resets
    // this connection's pager counters and keeps neither output pointer.
    let status = unsafe {
        rusqlite::ffi::sqlite3_db_status(
            connection.handle(),
            rusqlite::ffi::SQLITE_DBSTATUS_CACHE_MISS,
            &mut misses,
            &mut high_water,
            i32::from(reset),
        )
    };
    assert_eq!(status, rusqlite::ffi::SQLITE_OK);
    misses
}

fn reset_page_cache(connection: &rusqlite::Connection) {
    connection.execute_batch("PRAGMA shrink_memory").unwrap();
    page_cache_misses(connection, true);
}

#[tokio::test]
async fn capped_diagnostics_skip_oversized_settlement_pages() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) =
        super::super::super::causal_test_completion::connect(&fixture).await;
    let (input, _, _) = destination(fixture.central.clone(), &fixture.native);
    let key = "oversized-diagnostics-settlement";
    let (_, _, selector) = original(&fixture, key, input).await;
    let store = &fixture.central.mailbox_store;
    store
        .note_causal_pending_for_selector(&selector, true, CausalPendingReason::AdmissionPending)
        .unwrap();

    // Model an oversized TEXT value damaged while a disposable store is open.
    // Its overflow pages must not be fetched to decide whether it fits.
    let oversized = CAUSAL_SETTLEMENT_MAX_BYTES + 1;
    {
        let connection = store.connection().unwrap();
        assert_eq!(
            connection
                .execute(
                    "UPDATE mailbox_relay_intents SET test_request_settlement_json=CAST(zeroblob(?1) AS TEXT)
                     WHERE owner_session_id=?2 AND idempotency_key=?3",
                    rusqlite::params![oversized, fixture.native, key],
                )
                .unwrap(),
            1
        );
        reset_page_cache(&connection);
    }
    let diagnostic = get_mailbox_relay_wake_diagnostics(State(fixture.central.clone()))
        .await
        .unwrap()
        .0;
    let (diagnostic_misses, control_misses) = {
        let connection = store.connection().unwrap();
        let diagnostic_misses = page_cache_misses(&connection, false);
        reset_page_cache(&connection);
        // Positive control: explicitly reading this value traverses its pages
        // on the same connection with the same cold-cache setup.
        let content: Vec<u8> = connection
            .query_row(
                "SELECT CAST(test_request_settlement_json AS BLOB) FROM mailbox_relay_intents
                 WHERE owner_session_id=?1 AND idempotency_key=?2",
                rusqlite::params![fixture.native, key],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(content.len(), oversized);
        (diagnostic_misses, page_cache_misses(&connection, false))
    };
    drop(handles);
    server.finish().await;
    fixture.finish().await;

    let pending = &diagnostic["causalCompletionPending"];
    assert_eq!(pending["admissionDetailsLimited"], true);
    assert!(pending["records"][0]["state"]["admissionChecks"].is_null());
    assert!(control_misses > 128, "the control read the overflow pages");
    assert!(
        diagnostic_misses < control_misses / 8,
        "oversized value was materialized: diagnostics read {diagnostic_misses} pages, control {control_misses}"
    );
}

#[tokio::test]
async fn admission_blocker_survives_interruption_before_worker_marker() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) =
        super::super::super::causal_test_completion::connect(&fixture).await;
    let (input, transport, scope) = destination(fixture.central.clone(), &fixture.native);
    let key = "admission-marker-interruption";
    let (run, directory, selector) = original(&fixture, key, input).await;
    let mut recovery = CausalTestCompletionRecovery::default();
    recover(&fixture.central, &mut recovery).await;
    assert_eq!(
        pending_state(&fixture, key).await["reason"],
        "awaiting_completion"
    );
    finish(
        &fixture.satellite,
        &run,
        &directory,
        native_test_completion::Verdict::Pass,
        source(),
        source(),
    );
    {
        let mut central = fixture.central.inner.lock().unwrap();
        central
            .projects
            .iter_mut()
            .find(|project| project.id == scope.project_id)
            .unwrap()
            .engram = None;
    }
    recovery.stop_after_settlement = true;
    recover(&fixture.central, &mut recovery).await;
    recovery.shutdown().await;

    let reopened = MailboxStore::open(&resolve_coordination_persistence_path(
        &fixture.central.persistence_path,
    ))
    .unwrap();
    let saved = reopened
        .causal_test_settlement(&selector, Some(&scope))
        .unwrap()
        .expect("the blocker was committed before the failpoint");
    assert_eq!(saved.completion.run_id, run);
    assert_eq!(
        saved.admissions[0].pending.as_deref(),
        Some("destination_target_unavailable")
    );
    assert!(!saved.complete());
    assert!(writes(&transport).is_empty());
    let diagnostic = reopened.causal_pending_diagnostics().unwrap();
    drop(reopened);
    drop(handles);
    server.finish().await;
    fixture.finish().await;

    let state = &diagnostic["records"][0]["state"];
    assert_eq!(state["reason"], "admission_pending");
    assert_eq!(state["check"], "destination_target_unavailable");
}
