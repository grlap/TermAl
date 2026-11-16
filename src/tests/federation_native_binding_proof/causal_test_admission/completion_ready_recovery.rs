// A ready hint cannot supply run identity as native evidence or poison retry.
use super::*;

fn ready(fixture: &Fixture, selector: &CausalTestCompletionSelector, run: &str) -> Value {
    fixture
        .central
        .execute_mailbox_relay(
            MailboxRelayOperation::CausalTestCompletionReady {
                hint: CausalTestCompletionReadyHint {
                    requester: selector.payload.requester.clone(),
                    runner: selector.runner().unwrap().clone(),
                    request_send_key: selector.payload.request_send_key.clone(),
                    canonical_message_id: selector.canonical_message_id.clone(),
                    run_id: run.to_owned(),
                },
            },
            &fixture.channel.current_lease().unwrap(),
            &fixture.satellite_channel.snapshot.local,
        )
        .unwrap()
}

async fn joined_tick(app: &AppState, fallback: bool) {
    let mut recovery = CausalTestCompletionRecovery::default();
    recovery.tick(app, fallback).await;
    recovery.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_ready_run_cannot_supply_completion_and_later_original_hint_recovers() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) = super::super::causal_test_completion::connect(&fixture).await;
    let (input, transport, scope) = destination(fixture.central.clone(), &fixture.native);
    let (run, directory, selector) = original(&fixture, "hint-wrong-run-recovery", input).await;
    finish(
        &fixture.satellite,
        &run,
        &directory,
        native_test_completion::Verdict::Pass,
        source(),
        source(),
    );

    assert_eq!(
        ready(&fixture, &selector, "wrong-original-run")["status"],
        "scheduled"
    );
    joined_tick(&fixture.central, false).await;
    assert!(
        fixture
            .central
            .mailbox_store
            .causal_test_settlement(&selector, Some(&scope))
            .unwrap()
            .is_none()
    );
    assert!(
        writes(&transport).is_empty(),
        "the hint cannot replace authenticated run identity"
    );
    let encoded: String = fixture.central.mailbox_store.connection().unwrap().query_row(
        "SELECT test_request_pending_json FROM mailbox_relay_intents WHERE owner_session_id=?1 AND idempotency_key=?2",
        rusqlite::params![fixture.native, selector.payload.request_send_key], |row| row.get(0)).unwrap();
    assert_eq!(
        CausalPendingState::decode(&encoded).unwrap().reason,
        CausalPendingReason::CompletionRunMismatch
    );

    assert_eq!(ready(&fixture, &selector, &run)["status"], "scheduled");
    let accepted = receipt(None);
    script(
        &transport,
        &scope,
        &fixture.native,
        Ok(accepted.clone()),
        true,
    );
    joined_tick(&fixture.central, false).await;
    let saved = fixture
        .central
        .mailbox_store
        .causal_test_settlement(&selector, Some(&scope))
        .unwrap()
        .unwrap();
    assert!(saved.complete());
    assert_eq!(saved.completion.run_id, run);
    assert_eq!(saved.admissions[0].receipt, Some(accepted));
    assert_eq!(writes(&transport).len(), 1);
    let inner = fixture.central.inner.lock().unwrap();
    assert!(inner.test_run_registrations.is_empty());
    assert!(inner.remote_test_run_facts.is_empty());
    drop(inner);
    drop(handles);
    server.finish().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_ready_hints_leave_fallback_room_for_lost_notifications() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) = super::super::causal_test_completion::connect(&fixture).await;
    let (input, transport, scope) = destination(fixture.central.clone(), &fixture.native);
    let mut completed = Vec::new();
    // The first two ordinary keys remain running. They must not pin completed
    // keys behind them while two separate running requests keep sending hints.
    for index in 0..4 {
        let (run, directory, selector) =
            original(&fixture, &format!("fallback-{index}"), input.clone()).await;
        if index >= 2 {
            finish(
                &fixture.satellite,
                &run,
                &directory,
                native_test_completion::Verdict::Pass,
                source(),
                source(),
            );
            completed.push((run, selector));
        }
    }
    let mut hinted = Vec::new();
    for index in 0..2 {
        let (run, _, selector) =
            original(&fixture, &format!("running-hint-{index}"), input.clone()).await;
        hinted.push((run, selector));
    }
    for _ in &completed {
        script(&transport, &scope, &fixture.native, Ok(receipt(None)), true);
    }
    for _ in 0..2 {
        for (run, selector) in &hinted {
            assert_eq!(ready(&fixture, selector, run)["status"], "scheduled");
        }
        joined_tick(&fixture.central, true).await;
    }
    for (run, selector) in completed {
        let saved = fixture
            .central
            .mailbox_store
            .causal_test_settlement(&selector, Some(&scope))
            .unwrap()
            .unwrap();
        assert!(saved.complete());
        assert_eq!(saved.completion.run_id, run);
    }
    assert_eq!(writes(&transport).len(), 2);
    for (_, selector) in hinted {
        assert!(
            fixture
                .central
                .mailbox_store
                .causal_test_settlement(&selector, Some(&scope))
                .unwrap()
                .is_none()
        );
    }
    drop(handles);
    server.finish().await;
    fixture.finish().await;
}
