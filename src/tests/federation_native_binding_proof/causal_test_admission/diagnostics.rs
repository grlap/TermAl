// Exercise the original relay row and the joined recovery worker end to end.
use super::*;

mod pre_send_refusals;
mod review_regressions;
mod settlement_failures;

async fn pending_state(fixture: &Fixture, key: &str) -> Value {
    let row_id: i64 = fixture
        .central
        .mailbox_store
        .connection()
        .unwrap()
        .query_row(
            "SELECT rowid FROM mailbox_relay_intents WHERE owner_session_id=?1 AND idempotency_key=?2",
            rusqlite::params![fixture.native, key],
            |row| row.get(0),
        )
        .unwrap();
    let diagnostic = get_mailbox_relay_wake_diagnostics(State(fixture.central.clone()))
        .await
        .unwrap()
        .0;
    diagnostic["causalCompletionPending"]["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["table"] == "mailbox_relay_intents" && row["rowId"] == row_id)
        .expect("the original relay row is still pending")["state"]
        .clone()
}

async fn recover(app: &AppState, recovery: &mut CausalTestCompletionRecovery) {
    recovery.tick(app, true).await;
    let task = recovery
        .task
        .take()
        .expect("the fallback schedules a bounded worker");
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, task)
        .await
        .expect("the recovery worker finishes")
        .expect("the recovery worker joins");
}

#[tokio::test]
async fn original_pending_diagnostics_name_pull_and_destination_checks() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) = super::super::causal_test_completion::connect(&fixture).await;
    let (input, transport, scope) = destination(fixture.central.clone(), &fixture.native);
    let key = "causal-diagnostics-original";
    let (run, directory, selector) = original(&fixture, key, input).await;
    let mut recovery = CausalTestCompletionRecovery::default();

    recover(&fixture.central, &mut recovery).await;
    let awaiting_completion = pending_state(&fixture, key).await;
    // The origin sees the exact registration but refuses it while its
    // disposable native project identity disagrees with the registration.
    let original_project = {
        let mut origin = fixture.satellite.inner.lock().unwrap();
        origin
            .test_run_registrations
            .update(&run, |registration| {
                std::mem::replace(
                    &mut registration.native_origin.as_mut().unwrap().project_id,
                    "different-fixture-project".into(),
                )
            })
            .unwrap()
    };
    recover(&fixture.central, &mut recovery).await;
    let origin_refusal = pending_state(&fixture, key).await;
    {
        let mut origin = fixture.satellite.inner.lock().unwrap();
        origin
            .test_run_registrations
            .update(&run, |registration| {
                registration.native_origin.as_mut().unwrap().project_id = original_project;
            })
            .unwrap();
    }
    finish(
        &fixture.satellite,
        &run,
        &directory,
        native_test_completion::Verdict::Pass,
        source(),
        source(),
    );
    let settings = {
        let mut central = fixture.central.inner.lock().unwrap();
        central
            .projects
            .iter_mut()
            .find(|project| project.id == scope.project_id)
            .unwrap()
            .engram
            .take()
            .unwrap()
    };
    recover(&fixture.central, &mut recovery).await;
    let no_destination = pending_state(&fixture, key).await;
    let saved = fixture
        .central
        .mailbox_store
        .causal_test_settlement(&selector, Some(&scope))
        .unwrap()
        .expect("the original completion reached settlement");
    assert_eq!(saved.completion.run_id, run);
    assert!(!saved.complete());
    assert!(writes(&transport).is_empty());

    {
        let mut central = fixture.central.inner.lock().unwrap();
        central
            .projects
            .iter_mut()
            .find(|project| project.id == scope.project_id)
            .unwrap()
            .engram = Some(settings);
    }
    // Recovery uses the already saved completion even with the origin offline.
    drop(handles);
    script(&transport, &scope, &fixture.native, Ok(json!(null)), false);
    recover(&fixture.central, &mut recovery).await;
    let no_capability = pending_state(&fixture, key).await;
    assert!(writes(&transport).is_empty());

    script(
        &transport,
        &scope,
        &fixture.native,
        Err(EngramTransportError::protocol("private transport error")),
        true,
    );
    recover(&fixture.central, &mut recovery).await;
    let transport_failure = pending_state(&fixture, key).await;

    script(&transport, &scope, &fixture.native, Ok(json!({})), true);
    recover(&fixture.central, &mut recovery).await;
    let malformed_receipt = pending_state(&fixture, key).await;

    let accepted = receipt(None);
    script(
        &transport,
        &scope,
        &fixture.native,
        Ok(accepted.clone()),
        true,
    );
    recover(&fixture.central, &mut recovery).await;
    let settled = fixture
        .central
        .mailbox_store
        .causal_test_settlement(&selector, Some(&scope))
        .unwrap()
        .unwrap();
    assert_eq!(settled.admissions[0].receipt, Some(accepted));
    assert_eq!(settled.completion, saved.completion);
    let requests = writes(&transport);
    assert_eq!(requests.len(), 3);
    for retry in &requests[1..] {
        assert_eq!(retry.request, requests[0].request);
        assert_eq!(retry.connection, requests[0].connection);
    }
    recovery.shutdown().await;
    server.finish().await;
    fixture.finish().await;

    assert_eq!(no_destination["check"], "destination_target_unavailable");
    assert_eq!(
        no_destination["releaseCondition"],
        "destination_target_available"
    );
    assert_eq!(awaiting_completion["reason"], "awaiting_completion");
    assert_eq!(
        awaiting_completion["releaseCondition"],
        "origin_completion_available"
    );
    assert_eq!(origin_refusal["reason"], "origin_read_failed");
    assert_eq!(origin_refusal["releaseCondition"], "origin_read_succeeds");
    assert_eq!(no_capability["check"], "capability_unavailable");
    assert_eq!(transport_failure["check"], "admission_protocol_failed");
    assert!(
        !transport_failure
            .to_string()
            .contains("private transport error")
    );
    assert_eq!(malformed_receipt["check"], "admission_receipt_invalid");
}
