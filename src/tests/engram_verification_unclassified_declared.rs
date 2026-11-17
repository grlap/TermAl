use super::*;

#[test]
fn original_native_declared_end_is_not_replaced_by_a_generic_refusal() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = NativeFixture::new("native-unclassified-wait", false);
    fixture.prepare();
    let mut recorder = fixture.recorder();
    fixture.start(&mut recorder);
    let original = fixture.wait();
    let registration = fixture
        .fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations[&original.run_id]
        .clone();
    assert!(registration.declared_native.is_some());
    assert!(registration.declared_terminal.is_none());
    // This is the original-end positive control. The declaration guards in
    // generic rejection are defensive: preparation stops at Waiting here.
    assert!(matches!(
        engram_verification_record_prepare(&registration),
        EngramVerificationRecordPrepared::Waiting
    ));
    fixture.fixture.state.engram_verification_records_tick();
    assert!(
        fixture
            .fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_run_registrations[&original.run_id]
            .verification_record
            .is_none()
    );
    assert!(
        !fixture
            .fixture
            .state
            .reject_unclassified_verification_record(&registration, &["focused".into()])
    );
    fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
    let (_, finished, request, results) = fixture.finish(&mut recorder, 0);
    assert!(
        engram_verification_record_registration_unclassified_stages(&finished, &request, &results)
            .is_empty()
    );
    assert!(matches!(
        engram_verification_record_prepare(&finished),
        EngramVerificationRecordPrepared::Ready(_)
    ));
    let body = engram_verification_record_body(&finished, &request, &results).unwrap();
    assert_eq!(body.run.outcome, EngramVerificationRecordRunOutcome::Passed);
}
