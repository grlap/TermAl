// Settlement and recovery-batch failures name their step. A disposable store
// refuses chosen writes through SQLite triggers; the live store is never used.
use super::*;

const REFUSE_SETTLEMENT_WRITES: &str = "CREATE TRIGGER fixture_refuse_settlement
    BEFORE UPDATE OF test_request_settlement_json ON mailbox_relay_intents
    BEGIN SELECT RAISE(ABORT, 'fixture refuses settlement writes'); END";

fn execute(fixture: &Fixture, sql: &str) {
    fixture
        .central
        .mailbox_store
        .connection()
        .unwrap()
        .execute_batch(sql)
        .unwrap();
}

#[tokio::test]
async fn planning_a_saved_incomplete_settlement_replaces_a_stale_pull_reason() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) =
        super::super::super::causal_test_completion::connect(&fixture).await;
    let (input, _, scope) = destination(fixture.central.clone(), &fixture.native);
    let key = "stale-pull-reason-over-settlement";
    let (run, directory, selector) = original(&fixture, key, input).await;
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
    // A real admission blocker saves an incomplete settlement.
    let mut recovery = CausalTestCompletionRecovery::default();
    recover(&fixture.central, &mut recovery).await;
    recovery.shutdown().await;
    // As an earlier build could leave the row: a pull-phase reason over the
    // saved settlement, with no later worker write.
    let store = &fixture.central.mailbox_store;
    store
        .note_causal_pending_for_selector(&selector, true, CausalPendingReason::AwaitingCompletion)
        .unwrap();
    let stale = pending_state(&fixture, key).await;
    // Planning alone, before any pull or worker write, must name the phase
    // the saved settlement is in.
    let plans = store.causal_test_completion_plans().unwrap();
    let planned = pending_state(&fixture, key).await;
    drop(handles);
    server.finish().await;
    fixture.finish().await;

    assert_eq!(plans.len(), 1);
    assert!(plans[0].settlement.is_some());
    assert_eq!(stale["reason"], "awaiting_completion");
    assert_eq!(planned["reason"], "admission_pending");
    assert_eq!(planned["check"], "destination_target_unavailable");
}

#[tokio::test]
async fn a_later_successful_save_replaces_a_recorded_settlement_failure() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) =
        super::super::super::causal_test_completion::connect(&fixture).await;
    let (input, transport, scope) = destination(fixture.central.clone(), &fixture.native);
    let key = "settlement-failure-replaced";
    let (run, directory, _) = original(&fixture, key, input).await;
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
    // First pass: with no destination target, the first save records a
    // blocker and the worker records the admission phase.
    let mut recovery = CausalTestCompletionRecovery::default();
    recover(&fixture.central, &mut recovery).await;
    // Second pass: the target is back, and its connection save is refused.
    {
        let mut central = fixture.central.inner.lock().unwrap();
        central
            .projects
            .iter_mut()
            .find(|project| project.id == scope.project_id)
            .unwrap()
            .engram = Some(settings);
    }
    script(
        &transport,
        &scope,
        &fixture.native,
        Err(EngramTransportError::protocol("fixture transport error")),
        true,
    );
    execute(&fixture, REFUSE_SETTLEMENT_WRITES);
    recover(&fixture.central, &mut recovery).await;
    let refused = pending_state(&fixture, key).await;
    // Third pass: the same save now succeeds, and recovery stops before the
    // worker's own outcome write.
    execute(&fixture, "DROP TRIGGER fixture_refuse_settlement");
    recovery.stop_after_settlement = true;
    recover(&fixture.central, &mut recovery).await;
    let replaced = pending_state(&fixture, key).await;
    recovery.shutdown().await;
    drop(handles);
    server.finish().await;
    fixture.finish().await;

    assert_eq!(refused["reason"], "settlement_save_failed");
    assert_eq!(replaced["reason"], "admission_pending");
    assert_eq!(replaced["check"], "admission_protocol_failed");
}

#[tokio::test]
async fn a_settlement_save_failure_names_its_step_on_the_original_row() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) =
        super::super::super::causal_test_completion::connect(&fixture).await;
    let (input, transport, scope) = destination(fixture.central.clone(), &fixture.native);
    let key = "settlement-save-failure";
    let (run, directory, selector) = original(&fixture, key, input).await;
    finish(
        &fixture.satellite,
        &run,
        &directory,
        native_test_completion::Verdict::Pass,
        source(),
        source(),
    );
    execute(&fixture, REFUSE_SETTLEMENT_WRITES);
    let mut recovery = CausalTestCompletionRecovery::default();
    recover(&fixture.central, &mut recovery).await;
    let state = pending_state(&fixture, key).await;
    let saved = fixture
        .central
        .mailbox_store
        .causal_test_settlement(&selector, Some(&scope))
        .unwrap();
    execute(&fixture, "DROP TRIGGER fixture_refuse_settlement");
    recovery.shutdown().await;
    drop(handles);
    server.finish().await;
    fixture.finish().await;

    assert!(saved.is_none());
    assert!(writes(&transport).is_empty());
    assert_eq!(state["reason"], "settlement_save_failed");
    assert_eq!(
        state["releaseCondition"],
        "original_settlement_save_succeeds"
    );
}

#[tokio::test]
async fn a_completion_that_differs_from_the_saved_settlement_names_the_conflict() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) =
        super::super::super::causal_test_completion::connect(&fixture).await;
    let (input, transport, scope) = destination(fixture.central.clone(), &fixture.native);
    let key = "settlement-completion-conflict";
    let (run, directory, selector) = original(&fixture, key, input).await;
    finish(
        &fixture.satellite,
        &run,
        &directory,
        native_test_completion::Verdict::Pass,
        source(),
        source(),
    );
    let mut delivery = completion(&fixture.central).await;
    fixture
        .central
        .mailbox_store
        .save_causal_test_settlement(
            &selector,
            Some(&scope),
            None,
            &CausalTestSettlement::new(&delivery),
        )
        .unwrap();
    delivery.completion.run_id = format!("{run}-different");
    let failure = fixture
        .central
        .settle_causal_test_completion(&delivery)
        .unwrap_err();
    let saved = fixture
        .central
        .mailbox_store
        .causal_test_settlement(&selector, Some(&scope))
        .unwrap()
        .unwrap();
    drop(handles);
    server.finish().await;
    fixture.finish().await;

    assert_eq!(
        failure.reason,
        CausalPendingReason::CompletionConflictsWithSettlement
    );
    assert_eq!(saved.completion.run_id, run);
    assert!(writes(&transport).is_empty());
}

#[tokio::test]
async fn a_failed_recovery_batch_is_counted_under_its_stage() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) =
        super::super::super::causal_test_completion::connect(&fixture).await;
    let (input, _, _) = destination(fixture.central.clone(), &fixture.native);
    let key = "recovery-batch-failure";
    original(&fixture, key, input).await;
    // The store's relay identity, which belongs to no single row, cannot be
    // read, so the planning batch fails before any row is considered.
    execute(
        &fixture,
        "ALTER TABLE mailbox_relay_store RENAME TO fixture_hidden_relay_store",
    );
    let mut recovery = CausalTestCompletionRecovery::default();
    recover(&fixture.central, &mut recovery).await;
    execute(
        &fixture,
        "ALTER TABLE fixture_hidden_relay_store RENAME TO mailbox_relay_store",
    );
    let diagnostic = get_mailbox_relay_wake_diagnostics(State(fixture.central.clone()))
        .await
        .unwrap()
        .0;
    recovery.shutdown().await;
    drop(handles);
    server.finish().await;
    fixture.finish().await;

    assert_eq!(
        diagnostic["causalCompletionReady"]["decisions"]["pending_read_failed"],
        1
    );
}
