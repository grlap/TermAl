// Exact durable row reads gate notification; a queued event is no authority.
use super::*;
use crate::tests::test_runs::test_run_registration::causal_completion_fixture::compact;

fn read_unit(connection: &rusqlite::Connection, field: &str, key: &str) -> String {
    connection
        .query_row(
            "SELECT value_json FROM state_metadata_units WHERE field_name=?1 AND item_key=?2",
            rusqlite::params![field, format!("/{key}")],
            |row| row.get(0),
        )
        .unwrap()
}

fn replace_unit(connection: &rusqlite::Connection, field: &str, key: &str, encoded: &str) {
    assert_eq!(
        connection
            .execute(
                "UPDATE state_metadata_units SET value_json=?3 WHERE field_name=?1 AND item_key=?2",
                rusqlite::params![field, format!("/{key}"), encoded],
            )
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn readable_store_requires_original_completion_and_provider_before_notifying() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) = super::super::causal_test_completion::connect(&fixture).await;
    let (input, transport, _) = destination(fixture.central.clone(), &fixture.native);
    let (run, directory, selector) = original(&fixture, "keyed-notify-custody", input).await;
    let unfinished = fixture
        .satellite
        .inner
        .lock()
        .unwrap()
        .test_run_registrations[&run]
        .clone();
    assert!(unfinished.native_completion.is_none());
    finish(
        &fixture.satellite,
        &run,
        &directory,
        native_test_completion::Verdict::Pass,
        source(),
        source(),
    );
    let expected = fixture
        .satellite
        .inner
        .lock()
        .unwrap()
        .test_run_registrations[&run]
        .clone();
    let execution = &expected.native_completion.as_ref().unwrap().execution_id;
    let connection =
        rusqlite::Connection::open(fixture.satellite.persistence_path.as_path()).unwrap();
    let registration = read_unit(&connection, "testRunRegistrations", &run);
    let provider = read_unit(&connection, "testRunObservations", execution);

    // The genuine earlier registration in a readable store has not yet
    // persisted the completion already observed in memory.
    replace_unit(
        &connection,
        "testRunRegistrations",
        &run,
        &serde_json::to_string(&unfinished).unwrap(),
    );
    assert!(
        fixture
            .satellite
            .read_durable_native_test_run_for_notification(&expected)
            .unwrap()
            .is_none()
    );
    fixture.satellite.notify_causal_test_completions().await;
    assert!(
        fixture
            .central
            .federation_executor_wake_signal
            .causal_ready
            .lock()
            .unwrap()
            .pending
            .is_empty()
    );

    // The original registration alone is insufficient if its durable
    // provider row has lost the artifact association.
    replace_unit(&connection, "testRunRegistrations", &run, &registration);
    let mut unassociated: Value = serde_json::from_str(&provider).unwrap();
    unassociated["artifact"] = Value::Null;
    replace_unit(
        &connection,
        "testRunObservations",
        execution,
        &unassociated.to_string(),
    );
    assert!(
        fixture
            .satellite
            .read_durable_native_test_run_for_notification(&expected)
            .unwrap()
            .unwrap()
            .completion
            .is_none()
    );
    fixture.satellite.notify_causal_test_completions().await;
    assert!(
        fixture
            .central
            .federation_executor_wake_signal
            .causal_ready
            .lock()
            .unwrap()
            .pending
            .is_empty()
    );

    // Restore the actual persisted original, never a fabricated completion.
    replace_unit(&connection, "testRunObservations", execution, &provider);
    drop(connection);
    fixture.satellite.notify_causal_test_completions().await;
    assert!(
        fixture
            .central
            .federation_executor_wake_signal
            .causal_ready
            .lock()
            .unwrap()
            .pending
            .contains_key(&selector.key())
    );
    assert!(writes(&transport).is_empty());
    let inner = fixture.central.inner.lock().unwrap();
    assert!(inner.test_run_registrations.is_empty());
    assert!(inner.remote_test_run_facts.is_empty());
    drop(inner);
    drop(handles);
    server.finish().await;
    fixture.finish().await;
}

#[tokio::test]
async fn queued_notification_reads_original_compacted_provider_custody() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) = super::super::causal_test_completion::connect(&fixture).await;
    let (input, transport, _) = destination(fixture.central.clone(), &fixture.native);
    let (run, directory, selector) = original(&fixture, "keyed-notify-compacted", input).await;
    finish(
        &fixture.satellite,
        &run,
        &directory,
        native_test_completion::Verdict::Pass,
        source(),
        source(),
    );
    let expected = fixture
        .satellite
        .inner
        .lock()
        .unwrap()
        .test_run_registrations[&run]
        .clone();
    compact(&fixture.satellite, &run);
    // The old full image must still fail exact durable read-back. Compaction
    // clears stage names; it does not replace the original completion proof.
    assert!(!expected.stage_names.is_empty());
    assert!(
        fixture
            .satellite
            .read_durable_native_test_run_for_notification(&expected)
            .unwrap()
            .is_none()
    );
    let compacted = fixture.satellite.inner.lock().unwrap().test_run_compacted[&run]
        .registration
        .as_ref()
        .unwrap()
        .clone();
    assert!(compacted.stage_names.is_empty());
    assert_eq!(compacted.native_origin, expected.native_origin);
    assert_eq!(compacted.native_completion, expected.native_completion);
    assert_eq!(test_run_causal_key(&compacted), Some(selector.key()));
    assert!(
        !fixture
            .satellite
            .inner
            .lock()
            .unwrap()
            .test_run_observations
            .contains_key(&expected.native_completion.as_ref().unwrap().execution_id)
    );
    let durable = fixture
        .satellite
        .read_durable_native_test_run_for_notification(&compacted)
        .unwrap()
        .unwrap();
    assert_eq!(
        durable.completion.unwrap(),
        expected.native_completion.as_ref().unwrap().completion
    );
    fixture.satellite.notify_causal_test_completions().await;
    assert!(
        fixture
            .central
            .federation_executor_wake_signal
            .causal_ready
            .lock()
            .unwrap()
            .pending
            .contains_key(&selector.key())
    );
    assert!(writes(&transport).is_empty());
    let inner = fixture.central.inner.lock().unwrap();
    assert!(inner.test_run_registrations.is_empty());
    assert!(inner.remote_test_run_facts.is_empty());
    drop(inner);
    drop(handles);
    server.finish().await;
    fixture.finish().await;
}
