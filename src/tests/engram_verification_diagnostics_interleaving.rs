// A late factual artifact callback must preserve the exact observation image
// already carrying a valid receipt, including its real SQLite fence readback.
use super::*;

fn diagnostic_during_settlement(stale: bool) {
    let mut fixture = RunFixture::new("verification-diagnostic-interleaving");
    synchronous_persistence(&mut fixture);
    let run = record_run(
        &fixture,
        "test-verification-diagnostic",
        commit_request(&fixture.root),
        commit_results(),
    );
    register(&fixture, &[&run]);
    let execution = attach_launch_row(&fixture, &run);
    let before = fixture.state.inner.lock().unwrap().test_run_observations[&execution].clone();
    assert!(before.artifact.is_some());
    let body = ready(&run.registration).body;
    let receipt: EngramVerificationRecordReceipt =
        serde_json::from_value(receipt_for("failed")).unwrap();
    assert!(engram_verification_record_receipt_check(&body, &receipt).is_ok());
    let EngramVerificationRecordAssociation::Associated(rows) = fixture
        .state
        .settle_engram_verification_record(&run.registration, &body, &receipt)
    else {
        panic!("the original valid receipt must reach its own launch observation");
    };
    assert_eq!(rows.len(), 1);
    let settled = rows[0].clone();
    assert_ne!(
        before, settled,
        "the off-lock callback snapshot precedes actual record association"
    );
    assert_eq!(row_records(&fixture, &execution), settled_records());
    {
        let inner = fixture.state.inner.lock().unwrap();
        fixture.state.persist_internal_locked(&inner).unwrap();
    }
    let fence = PersistFenceTarget::TestRunObservation(Box::new(settled.clone()));
    let connection = rusqlite::Connection::open(fixture.state.persistence_path.as_path()).unwrap();
    assert!(
        fence.is_already_durable(&connection).unwrap(),
        "positive actual SQLite readback before the late event"
    );
    let revision = fixture.state.inner.lock().unwrap().revision;

    fixture.state.retain_test_command_artifact_refusal(
        if stale { &before } else { &settled },
        TestRunNativeCompletionRefusalCode::ArtifactDirectoryMismatch,
        &[(TestRunNativeCompletionFact::DirectoryMatches, false)],
        None,
    );
    {
        let inner = fixture.state.inner.lock().unwrap();
        fixture.state.persist_internal_locked(&inner).unwrap();
    }
    assert!(
        fence.is_already_durable(&connection).unwrap(),
        "a stale or already-associated diagnostic callback must preserve the receipt's pending exact-row fence"
    );
    let inner = fixture.state.inner.lock().unwrap();
    assert_eq!(&inner.test_run_observations[&execution], &settled);
    assert_eq!(
        inner.revision, revision,
        "the discarded callback creates no revision"
    );
    drop(inner);
    assert_eq!(
        fixture
            .state
            .complete_engram_verification_record(&run.registration, &body, &receipt),
        EngramVerificationRecordSettlement::Settled
    );
    assert!(matches!(
        current(&fixture, &run).verification_record,
        Some(TestRunVerificationAck::Settled { .. })
    ));
    assert_eq!(row_records(&fixture, &execution), settled_records());
    assert!(
        current(&fixture, &run).native_completion.is_none(),
        "a diagnostic callback creates no native completion"
    );
}

#[test]
fn stale_artifact_diagnostic_preserves_verification_settlement_fence() {
    diagnostic_during_settlement(true);
}

#[test]
fn associated_artifact_diagnostic_preserves_verification_settlement_fence() {
    diagnostic_during_settlement(false);
}
