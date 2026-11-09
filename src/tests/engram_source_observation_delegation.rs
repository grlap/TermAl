//! Observation delivery is independent of the child's ordinary delegation attempt.
use super::*;

pub(super) fn attach_observation_delegation(fixture: &ObservationSession) -> (String, String) {
    let mut inner = fixture.state.inner.lock().unwrap();
    let parent = inner
        .create_session(
            Agent::Codex,
            Some("Observation parent".into()),
            fixture.state.default_workdir.clone(),
            None,
            None,
        )
        .session
        .id
        .clone();
    let id = inner.next_delegation_id();
    let record: DelegationRecord = serde_json::from_value(json!({
        "id": id, "parentSessionId": parent, "childSessionId": fixture.id,
        "mode": "reviewer", "status": "running", "title": "Observation child",
        "prompt": "Retained ordinary prompt", "cwd": fixture.state.default_workdir,
        "agent": "Codex", "writePolicy": {"kind": "readOnly"}, "createdAt": stamp_now()
    }))
    .unwrap();
    let index = inner.find_session_index(&fixture.id).unwrap();
    inner.sessions[index].session.parent_delegation_id = Some(id.clone());
    inner.sessions[index].session.status = SessionStatus::Active;
    inner.delegations.push(record);
    (parent, id)
}

#[test]
fn ordinary_observation_pending_and_failed_reports_do_not_hold_a_running_delegation() {
    let fixture = ObservationSession::new();
    let (_, id) = attach_observation_delegation(&fixture);
    let report = fixture.report();
    fixture.retain(report.clone());
    for terminal in [false, true] {
        if terminal {
            fixture.record(|record| {
                record.engram.unadmitted_reports[0].envelopes[0].delivery =
                    EngramRuntimeObservationDelivery::LocalFailure {
                        code: "runtime_observation_retry_exhausted".into(),
                        message: "unknown outcome".into(),
                    }
            });
        }
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&id).unwrap();
        sync_delegation_attempt_state_locked(&mut inner, index, false);
        assert_eq!(
            public_delegation_status(&inner.delegations[index]),
            DelegationStatus::Running
        );
        assert!(inner.delegations[index].attempt.hold.is_none());
    }
}

#[test]
fn ordinary_observation_late_receipt_cannot_change_delegation_attempt_or_parent() {
    let fixture = ObservationSession::new();
    let (parent, id) = attach_observation_delegation(&fixture);
    let report = fixture.report();
    fixture.retain(report.clone());
    let before = {
        let inner = fixture.state.inner.lock().unwrap();
        (
            serde_json::to_value(&inner.delegations[inner.find_delegation_index(&id).unwrap()])
                .unwrap(),
            serde_json::to_value(
                &inner.sessions[inner.find_session_index(&parent).unwrap()].session,
            )
            .unwrap(),
        )
    };
    fixture
        .state
        .install_runtime_observation_result(&fixture.id, &report, recorded_report(&report), 0)
        .unwrap();
    let inner = fixture.state.inner.lock().unwrap();
    assert_eq!(
        serde_json::to_value(&inner.delegations[inner.find_delegation_index(&id).unwrap()])
            .unwrap(),
        before.0
    );
    assert_eq!(
        serde_json::to_value(&inner.sessions[inner.find_session_index(&parent).unwrap()].session)
            .unwrap(),
        before.1
    );
}
