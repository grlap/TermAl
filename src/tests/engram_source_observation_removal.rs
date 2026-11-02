//! Ordinary observation removal custody, independent of former turn admission.
use super::*;

#[test]
fn ordinary_observation_public_delete_retains_active_claim_or_unresolved_report() {
    for duty in ["active_claim", "unresolved_report"] {
        let fixture = ObservationSession::new();
        let mut report = fixture.report();
        for envelope in &mut report.envelopes {
            envelope.delivery = EngramRuntimeObservationDelivery::LocalFailure {
                code: "runtime_observation_retry_exhausted".into(), message: "unknown recording outcome".into(),
            };
        }
        fixture.record(|record| {
            if duty == "active_claim" {
                record.engram.unadmitted_active_turn = Some(observation_trace());
                record.engram.work_binding = Some(observation_binding());
                record.session.status = SessionStatus::Active;
            } else {
                record.engram.unadmitted_reports.push(report.clone());
            }
        });
        let error = fixture.state.kill_session(&fixture.id).err().expect("public Delete must retain unresolved observation custody");
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(error.message.contains("Session kept"));
        fixture.record(|record| {
            assert!(record.engram.observation_delete_requested && record.orchestrator_auto_dispatch_blocked);
            assert!(runtime_observation_has_removal_duty(record));
            if duty == "active_claim" {
                assert_eq!(record.engram.unadmitted_active_turn, Some(observation_trace()));
                assert_eq!(record.engram.work_binding, Some(observation_binding()));
                assert_eq!(record.session.status, SessionStatus::Active);
            } else {
                assert_eq!(record.engram.unadmitted_reports, vec![report.clone()]);
            }
            assert!(record.engram.runtime_checkpoint.is_none());
        });
        let target = fixture.removal_target();
        let connection = rusqlite::Connection::open(fixture.state.persistence_path.as_ref()).unwrap();
        assert!(target.is_already_durable(&connection).unwrap(), "the public refusal must retain its exact session-row image");
        assert!(fixture.state.inner.lock().unwrap().find_visible_session_index(&fixture.id).is_some());
    }
}

#[test]
fn ordinary_observation_public_delete_removes_settled_reports_without_recovery() {
    for refused in [false, true] {
        let fixture = ObservationSession::new();
        let mut report = recorded_report(&fixture.report());
        if refused {
            for envelope in &mut report.envelopes {
                envelope.delivery = EngramRuntimeObservationDelivery::Refused {
                    code: "execution_observation_policy_basis_mismatch".into(), message: "refused".into(),
                };
            }
        }
        fixture.retain(report);
        fixture.state.kill_session(&fixture.id).expect("public Delete needs no grant recovery for settled ordinary reports");
        assert!(fixture.state.inner.lock().unwrap().find_session_index(&fixture.id).is_none());
        let connection = rusqlite::Connection::open(fixture.state.persistence_path.as_ref()).unwrap();
        let exists: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)", [&fixture.id], |row| row.get(0)).unwrap();
        assert!(!exists, "successful public Delete must remove the canonical session row too");
    }
}

#[test]
fn ordinary_observation_public_delete_refuses_the_whole_subtree_before_teardown() {
    let fixture = ObservationSession::new();
    let mut report = fixture.report();
    for envelope in &mut report.envelopes {
        envelope.delivery = EngramRuntimeObservationDelivery::LocalFailure {
            code: "runtime_observation_retry_exhausted".into(), message: "unresolved".into(),
        };
    }
    fixture.retain(report.clone());
    let (parent, _) = super::delegation::attach_observation_delegation(&fixture);
    let error = fixture.state.kill_session(&parent).err().expect("a child's unresolved reporting must retain the whole public Delete set");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains(&fixture.id));
    let inner = fixture.state.inner.lock().unwrap();
    for id in [&parent, &fixture.id] {
        let record = &inner.sessions[inner.find_visible_session_index(id).unwrap()];
        assert!(record.engram.observation_delete_requested && record.orchestrator_auto_dispatch_blocked);
        assert!(!inner.removed_session_ids.contains(id));
    }
    assert_eq!(inner.sessions[inner.find_session_index(&fixture.id).unwrap()].engram.unadmitted_reports, vec![report]);
    assert_eq!(inner.delegations.len(), 1, "refusal must precede delegation teardown");
}

#[test]
fn ordinary_observation_direct_removal_retains_pending_report_and_visible_owner() {
    let fixture = ObservationSession::new();
    let report = fixture.report();
    fixture.retain(report.clone());
    let mut inner = fixture.state.inner.lock().unwrap();
    let index = inner.find_session_index(&fixture.id).unwrap();
    assert!(inner.remove_session_at(index).is_none());
    let record = &inner.sessions[inner.find_visible_session_index(&fixture.id).unwrap()];
    assert!(record.engram.observation_delete_requested);
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert_eq!(record.engram.unadmitted_reports, vec![report]);
    assert!(!inner.removed_session_ids.contains(&fixture.id));
}

#[test]
fn ordinary_observation_retain_filter_cannot_discard_pending_report() {
    let fixture = ObservationSession::new();
    fixture.retain(fixture.report());
    let mut inner = fixture.state.inner.lock().unwrap();
    inner.retain_sessions(|record| record.session.id != fixture.id);
    let record = &inner.sessions[inner.find_visible_session_index(&fixture.id).unwrap()];
    assert!(runtime_observation_has_removal_duty(record));
    assert!(record.engram.observation_delete_requested);
    assert!(!inner.removed_session_ids.contains(&fixture.id));
}

#[test]
fn ordinary_observation_hidden_owner_is_retained_across_save_and_reload() {
    let fixture = ObservationSession::new();
    let report = fixture.report();
    fixture.retain(report.clone());
    fixture.record(|record| record.hidden = true);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        inner.retain_hidden_source_observation_sessions();
        assert!(inner.find_visible_session_index(&fixture.id).is_some());
    }
    fixture.persist();
    let restored = load_state(fixture.state.persistence_path.as_ref()).unwrap().unwrap();
    let record = &restored.sessions[restored.find_visible_session_index(&fixture.id).unwrap()];
    assert_eq!(record.engram.unadmitted_reports, vec![report]);
    assert!(record.engram.observation_delete_requested);
}

#[test]
fn ordinary_observation_recorded_or_refused_reports_allow_removal() {
    for refused in [false, true] {
        let fixture = ObservationSession::new();
        let mut report = recorded_report(&fixture.report());
        if refused {
            for envelope in &mut report.envelopes {
                envelope.delivery = EngramRuntimeObservationDelivery::Refused {
                    code: "execution_observation_policy_basis_mismatch".into(), message: "refused".into(),
                };
            }
        }
        fixture.retain(report);
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.id).unwrap();
        assert!(!runtime_observation_has_removal_duty(&inner.sessions[index]));
        assert!(inner.remove_session_at(index).is_some());
        assert!(inner.find_session_index(&fixture.id).is_none());
        assert!(inner.removed_session_ids.contains(&fixture.id));
    }
}

#[test]
fn ordinary_observation_mixed_report_keeps_custody_until_every_envelope_is_terminal() {
    let fixture = ObservationSession::new();
    let mut report = fixture.report();
    report.envelopes[0].delivery = EngramRuntimeObservationDelivery::Recorded {
        receipt: observation_receipt(&report_observation(&report, 0), &report.observing_session),
    };
    assert_eq!(report.pending(), Some(1));
    fixture.retain(report);
    fixture.record(|record| assert!(runtime_observation_has_removal_duty(record)));
}

#[test]
fn ordinary_observation_retry_exhaustion_is_settled_for_delivery_but_retains_custody() {
    let fixture = ObservationSession::new();
    let mut report = fixture.report();
    for envelope in &mut report.envelopes {
        envelope.delivery = EngramRuntimeObservationDelivery::LocalFailure {
            code: "runtime_observation_retry_exhausted".into(), message: "private error payload".into(),
        };
    }
    assert!(report.is_settled());
    let request = report.envelopes[0].request.clone();
    fixture.retain(report);
    fixture.record(|record| {
        assert!(runtime_observation_has_removal_duty(record));
        let notice = runtime_observation_terminal_removal_notice(record).unwrap();
        assert!(notice.contains("Delivery outcome is unknown"));
        assert!(notice.contains("Resume cannot resolve"));
        assert!(!notice.contains("private error payload"));
        assert_eq!(record.engram.unadmitted_reports[0].envelopes[0].request, request);
    });
}

#[test]
fn ordinary_observation_terminal_notice_redacts_untrusted_turn_and_error_fields() {
    let fixture = ObservationSession::new();
    let mut report = fixture.report();
    report.host_turn_ref = "secret\nforged-status".into();
    report.envelopes[0].delivery = EngramRuntimeObservationDelivery::LocalFailure {
        code: "secret-remote-code".into(), message: "secret-remote-message".into(),
    };
    fixture.retain(report);
    fixture.record(|record| {
        let notice = runtime_observation_terminal_removal_notice(record).unwrap();
        assert!(notice.contains("turn unavailable"));
        assert!(notice.contains("runtime_observation_local_failure"));
        assert!(!notice.contains("secret"));
        assert!(!notice.contains('\n'));
    });
}

#[test]
fn ordinary_observation_claimed_active_trace_retains_custody_without_connection() {
    let fixture = ObservationSession::new();
    fixture.record(|record| {
        record.engram.unadmitted_active_turn = Some(observation_trace());
        record.engram.work_binding = Some(observation_binding());
        assert!(runtime_observation_active_removal_duty(record));
        assert!(runtime_observation_terminal_removal_notice(record).unwrap().contains("Stop the turn"));
        record.engram.work_binding = None;
        assert!(!runtime_observation_has_removal_duty(record), "an unclaimed trace cannot become a report at removal");
    });
}

#[test]
fn ordinary_observation_delete_retains_the_whole_delegated_subtree_only() {
    let fixture = ObservationSession::new();
    let report = fixture.report();
    fixture.retain(report.clone());
    let (parent, _) = super::delegation::attach_observation_delegation(&fixture);
    let mut inner = fixture.state.inner.lock().unwrap();
    let sibling = inner.create_session(Agent::Codex, Some("Unrelated session".into()),
        fixture.state.default_workdir.clone(), None, None).session.id.clone();
    assert!(inner.defer_source_observation_removal(&parent));
    let affected = source_observation_removal_set(&inner, &parent);
    assert_eq!(affected.len(), 2);
    assert!(affected.contains(&fixture.id));
    for id in [&parent, &fixture.id] {
        let record = &inner.sessions[inner.find_visible_session_index(id).unwrap()];
        assert!(record.engram.observation_delete_requested);
        assert!(record.orchestrator_auto_dispatch_blocked);
    }
    let unrelated = &inner.sessions[inner.find_session_index(&sibling).unwrap()];
    assert!(!unrelated.engram.observation_delete_requested);
    assert!(!unrelated.orchestrator_auto_dispatch_blocked);
    assert_eq!(source_observation_removal_blocker(&inner, &affected), Some(fixture.id.clone()));
}

#[test]
fn ordinary_observation_pending_report_does_not_capture_a_same_claim_peer() {
    let fixture = ObservationSession::new();
    fixture.retain(fixture.report());
    let mut inner = fixture.state.inner.lock().unwrap();
    let peer = inner.create_session(Agent::Codex, Some("Same claim peer".into()),
        fixture.state.default_workdir.clone(), None, None).session.id.clone();
    let index = inner.find_session_index(&peer).unwrap();
    inner.sessions[index].engram.work_binding = Some(observation_binding());
    assert!(!runtime_observation_has_removal_duty(&inner.sessions[index]));
    assert!(!inner.defer_source_observation_removal(&peer));
    assert!(inner.remove_session_at(index).is_some());
    assert!(inner.find_session_index(&fixture.id).is_some());
}
