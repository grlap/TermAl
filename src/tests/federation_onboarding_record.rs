// Owns completeness and Engram-boundary regression tests for onboarding reports.
// Does not replace fake-link route tests or provision machines. New O1 tests
// mounted from tests/mod.rs using the existing M1 disposable-state fixture.
use super::*;

fn complete_report() -> FederationOnboardingReport {
    let state = federation_registry::central();
    let record = federation_registry::peer(47251);
    let basis = record.verification_basis();
    let expected = federation_registry::connected(&state, record);
    FederationOnboardingReport {
        attempt_id: Uuid::new_v4().to_string(),
        expected,
        basis,
        link_incarnation: Uuid::new_v4().to_string(),
        outcomes: FEDERATION_ONBOARDING_CHECKS
            .into_iter()
            .map(|check| FederationOnboardingOutcome {
                check,
                passed: true,
                reason: "observed".to_owned(),
            })
            .collect(),
        probe_session_id: Some(Uuid::new_v4().to_string()),
        wake_outcome_id: Some(Uuid::new_v4().to_string()),
        retained_probe: None,
        running_image: federation_onboarding_fixture::image(),
        reverse_running_image: federation_onboarding_fixture::image(),
        engram: None,
    }
}

#[test]
fn federation_onboarding_record_complete_termal_only_report_can_verify() {
    let report = complete_report();
    assert!(report.termal_complete());
    assert!(report.registry_checks().unwrap().complete());
}

#[test]
fn federation_onboarding_record_engram_failure_never_gates_termal() {
    let mut report = complete_report();
    report.engram = Some(FederationEngramOnboardingOutcome {
        status: FederationEngramProbeStatus::Fail,
        exit_code: Some(1),
        summary: None,
        reason: "Engram verification failed".to_owned(),
    });
    assert!(report.termal_complete());
    assert!(report.registry_checks().unwrap().complete());
}

#[test]
fn federation_onboarding_record_every_missing_or_failed_check_refuses_verification() {
    for index in 0..FEDERATION_ONBOARDING_CHECKS.len() {
        let mut report = complete_report();
        let check = report.outcomes[index].check;
        report.outcomes[index].passed = false;
        report.outcomes[index].reason = "probe failed".to_owned();
        assert!(!report.termal_complete(), "{check:?} failed");
        assert!(report.registry_checks().is_err());
        report.outcomes.remove(index);
        assert!(!report.termal_complete(), "{check:?} missing");
    }
}

#[test]
fn federation_onboarding_record_duplicate_cannot_cover_a_missing_check() {
    let mut report = complete_report();
    report.outcomes[1] = report.outcomes[0].clone();
    assert!(!report.termal_complete());
}

#[test]
fn federation_onboarding_record_requires_distinct_durable_wake_and_session_identity() {
    let mut report = complete_report();
    report.wake_outcome_id = None;
    assert!(!report.termal_complete());
    report.wake_outcome_id = report.probe_session_id.clone();
    assert!(!report.termal_complete());
}
