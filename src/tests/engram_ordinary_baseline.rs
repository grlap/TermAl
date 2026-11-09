//! The baseline an ordinary turn's settled closing report establishes for
//! the next turn: only the newest closed turn's source sighting, recorded by
//! Engram as a sighting under the session's current binding. Mounted from
//! `engram_source_observations.rs` to share its report and receipt fixtures.
use super::*;

fn sighting_index(report: &EngramRuntimeObservationReport) -> usize {
    (0..report.envelopes.len())
        .find(|&index| {
            matches!(
                report_observation(report, index).occurrence,
                EngramInterTurnOccurrence::SourceSighting { .. }
            )
        })
        .expect("an attributed closing report carries its source sighting")
}

/// The session as it stands when the report's own turn is the newest one
/// this process closed: bound to the report's claim, at its turn generation.
fn newest_turn_session(report: &EngramRuntimeObservationReport) -> ObservationSession {
    let session = ObservationSession::new();
    session.record(|record| {
        record.engram.work_binding = Some(observation_binding());
        record.active_turn_generation = report.generation;
        record.engram.runtime_closed_turn_ref = Some(report.host_turn_ref.clone());
    });
    session
}

fn with_delivery(
    report: &EngramRuntimeObservationReport,
    index: usize,
    delivery: EngramRuntimeObservationDelivery,
) -> EngramRuntimeObservationReport {
    let mut next = report.next_version();
    next.envelopes[index].delivery = delivery;
    next
}

fn recorded_sighting(
    report: &EngramRuntimeObservationReport,
    index: usize,
) -> EngramRuntimeObservationDelivery {
    EngramRuntimeObservationDelivery::Recorded {
        receipt: observation_receipt(
            &report_observation(report, index),
            &report.observing_session,
        ),
    }
}

fn promoted(
    session: &ObservationSession,
    report: &EngramRuntimeObservationReport,
    next: &EngramRuntimeObservationReport,
    index: usize,
) -> Option<EngramRuntimeCheckpointBasis> {
    session.record(|record| engram_recorded_closing_baseline(record, report, next, index))
}

#[test]
fn a_recorded_closing_sighting_becomes_the_exact_next_baseline() {
    let session = ObservationSession::new();
    let report = session.report();
    let index = sighting_index(&report);
    let session = newest_turn_session(&report);
    let next = with_delivery(&report, index, recorded_sighting(&report, index));
    let baseline =
        promoted(&session, &report, &next, index).expect("the recorded sighting is the baseline");
    let source = observation_source("content-v1:closing");
    assert_eq!(
        baseline,
        EngramRuntimeCheckpointBasis {
            binding: observation_binding(),
            endpoint: EngramMeasuredBaseline {
                source_id: None,
                workspace_id: source.workspace_id,
                source_revision: source.source_revision,
                observed_at: observation_at(50),
            },
            source_root_generation: Some(2),
            source_root_state: Some(EngramSourceRootState::Named),
        }
    );
}

#[test]
fn an_unrecorded_or_unaccounted_closing_sighting_never_becomes_a_baseline() {
    let session = ObservationSession::new();
    let report = session.report();
    let index = sighting_index(&report);
    let session = newest_turn_session(&report);
    let refusals = [
        (
            "refused",
            EngramRuntimeObservationDelivery::Refused {
                code: "observation_refused".into(),
                message: "refused by the producer".into(),
            },
        ),
        (
            "unresolved",
            EngramRuntimeObservationDelivery::LocalFailure {
                code: "runtime_observation_retry_exhausted".into(),
                message: "no certain answer".into(),
            },
        ),
        (
            "failed locally",
            EngramRuntimeObservationDelivery::LocalFailure {
                code: "runtime_observation_policy_unavailable".into(),
                message: "policy unavailable".into(),
            },
        ),
        (
            "still pending",
            EngramRuntimeObservationDelivery::Pending {
                last_error: Some("timeout".into()),
            },
        ),
    ];
    for (case, delivery) in refusals {
        assert_eq!(
            promoted(
                &session,
                &report,
                &with_delivery(&report, index, delivery),
                index
            ),
            None,
            "{case}"
        );
    }

    let mut audit = observation_receipt(
        &report_observation(&report, index),
        &report.observing_session,
    );
    audit["accounting"] = json!({"kind": "audit_only", "reason": "root_basis_moved"});
    let audited = with_delivery(
        &report,
        index,
        EngramRuntimeObservationDelivery::Recorded { receipt: audit },
    );
    assert_eq!(
        promoted(&session, &report, &audited, index),
        None,
        "a historical audit receipt does not accept the sighting"
    );

    let recorded = with_delivery(&report, index, recorded_sighting(&report, index));
    assert_eq!(
        promoted(&session, &recorded, &recorded.next_version(), index),
        None,
        "only the settlement from pending establishes the baseline, never a later version"
    );

    let first = (0..report.envelopes.len())
        .find(|&other| other != index)
        .unwrap();
    let other = with_delivery(&report, first, recorded_sighting(&report, first));
    assert_eq!(
        promoted(&session, &report, &other, first),
        None,
        "the turn's own observation is not its source sighting"
    );
}

#[test]
fn a_late_or_foreign_closing_sighting_never_becomes_a_baseline() {
    let session = ObservationSession::new();
    let report = session.report();
    let index = sighting_index(&report);
    let next = with_delivery(&report, index, recorded_sighting(&report, index));

    let later_turn = newest_turn_session(&report);
    later_turn.record(|record| record.active_turn_generation = report.generation + 1);
    assert_eq!(
        promoted(&later_turn, &report, &next, index),
        None,
        "a later turn has started; it already took its own baseline"
    );

    let later_close = newest_turn_session(&report);
    let mut successor = report.clone();
    successor.generation = report.generation + 1;
    successor.host_turn_ref = "a-later-turn".into();
    later_close.retain(successor.clone());
    later_close.record(|record| {
        record.active_turn_generation = successor.generation;
        record.engram.runtime_closed_turn_ref = Some(successor.host_turn_ref.clone());
    });
    assert_eq!(
        promoted(&later_close, &report, &next, index),
        None,
        "a later turn has already closed"
    );

    // A retained report from before a restart, with a higher generation than
    // the new process's turns, does not block the turn that just closed.
    let old_high = newest_turn_session(&report);
    let mut retained = report.clone();
    retained.generation = report.generation + 40;
    retained.host_turn_ref = "a-turn-before-the-restart".into();
    old_high.retain(retained);
    assert!(
        promoted(&old_high, &report, &next, index).is_some(),
        "the newest closed turn's own sighting still establishes the baseline"
    );

    let rebound = newest_turn_session(&report);
    rebound.record(|record| {
        record.engram.work_binding = Some(test_control_work_binding("another-claim", 1))
    });
    assert_eq!(
        promoted(&rebound, &report, &next, index),
        None,
        "another claim owns the session now"
    );

    let unbound = newest_turn_session(&report);
    unbound.record(|record| record.engram.work_binding = None);
    assert_eq!(
        promoted(&unbound, &report, &next, index),
        None,
        "no claim owns the session now"
    );
}

#[test]
fn a_report_from_before_a_restart_never_establishes_a_baseline() {
    let session = ObservationSession::new();
    let report = session.report();
    let index = sighting_index(&report);
    let next = with_delivery(&report, index, recorded_sighting(&report, index));

    // Persisted with its pending sighting, then reloaded as a restart does.
    let before = newest_turn_session(&report);
    before.retain(report.clone());
    before.persist();
    let mut loaded = load_state(before.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let index_loaded = loaded.find_session_index(&before.id).unwrap();
    let restored = &mut loaded.sessions[index_loaded];
    assert_eq!(restored.engram.unadmitted_reports, vec![report.clone()]);
    assert_eq!(
        restored.engram.runtime_closed_turn_ref, None,
        "which turn this process closed is never restored"
    );
    // After the restart, the new process closes its own turn at the old
    // report's generation, with the same claim. The old report's receipt that
    // finally arrives is still not that turn's sighting.
    restored.engram.work_binding = Some(observation_binding());
    restored.active_turn_generation = report.generation;
    assert_eq!(
        engram_recorded_closing_baseline(restored, &report, &next, index),
        None,
        "nothing this process closed yet"
    );
    restored.engram.runtime_closed_turn_ref = Some("the-new-process-turn".into());
    assert_eq!(
        engram_recorded_closing_baseline(restored, &report, &next, index),
        None,
        "a pre-restart sighting cannot pass for a new turn that reused its generation"
    );

    let other_turn = newest_turn_session(&report);
    other_turn.record(|record| record.engram.runtime_closed_turn_ref = Some("another-turn".into()));
    assert_eq!(
        promoted(&other_turn, &report, &next, index),
        None,
        "only the turn this process closed last can establish the baseline"
    );
}

#[test]
fn installing_a_recorded_sighting_persists_the_baseline_and_a_refusal_does_not() {
    let session = ObservationSession::new();
    let report = session.report();
    let index = sighting_index(&report);

    let refused_session = newest_turn_session(&report);
    refused_session.retain(report.clone());
    let refused = with_delivery(
        &report,
        index,
        EngramRuntimeObservationDelivery::Refused {
            code: "observation_refused".into(),
            message: "refused by the producer".into(),
        },
    );
    refused_session
        .state
        .install_runtime_observation_result(&refused_session.id, &report, refused, index)
        .unwrap();
    assert_eq!(
        refused_session.record(|record| record.engram.runtime_checkpoint.clone()),
        None
    );

    let session = newest_turn_session(&report);
    session.retain(report.clone());
    let next = with_delivery(&report, index, recorded_sighting(&report, index));
    let expected = promoted(&session, &report, &next, index).unwrap();
    session
        .state
        .install_runtime_observation_result(&session.id, &report, next, index)
        .unwrap();
    assert_eq!(
        session.record(|record| record.engram.runtime_checkpoint.clone()),
        Some(expected.clone())
    );
    session.persist();
    let loaded = load_state(session.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        loaded.sessions[loaded.find_session_index(&session.id).unwrap()]
            .engram
            .runtime_checkpoint,
        Some(expected),
        "the baseline survives a restart with the settled report"
    );
}

fn turn_report(session: &ObservationSession, turn: usize) -> EngramRuntimeObservationReport {
    let mut report = session.report();
    report.host_turn_ref = format!("turn-{turn}");
    report
}

fn settle_turn(session: &ObservationSession, turn: usize) {
    let report = turn_report(session, turn);
    session.retain(report.clone());
    session
        .state
        .install_runtime_observation_result(&session.id, &report, recorded_report(&report), 0)
        .unwrap();
}

fn retained_turns(session: &ObservationSession) -> Vec<String> {
    session.record(|record| {
        record
            .engram
            .unadmitted_reports
            .iter()
            .map(|report| report.host_turn_ref.clone())
            .collect()
    })
}

fn turn_refs(turns: std::ops::Range<usize>) -> Vec<String> {
    turns.map(|turn| format!("turn-{turn}")).collect()
}

#[test]
fn a_session_keeps_only_its_newest_discharged_reports_through_a_reload() {
    let session = ObservationSession::new();
    let turns = ENGRAM_DISCHARGED_REPORT_RETENTION + 12;
    for turn in 0..turns {
        settle_turn(&session, turn);
    }
    let reports = session.record(|record| record.engram.unadmitted_reports.clone());
    let row_bytes = serde_json::to_string(&reports).unwrap().len();
    let one_settled = recorded_report(&turn_report(&session, 0));
    let one_bytes = serde_json::to_string(&one_settled).unwrap().len();
    assert!(
        row_bytes <= (one_bytes + 16) * ENGRAM_DISCHARGED_REPORT_RETENTION,
        "{turns} settled turns left {row_bytes} bytes in the row, one report being {one_bytes}",
    );
    let newest = turn_refs(turns - ENGRAM_DISCHARGED_REPORT_RETENTION..turns);
    assert_eq!(retained_turns(&session), newest);
    session.persist();
    let loaded = load_state(session.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&session.id).unwrap()];
    assert_eq!(
        serde_json::to_value(&record.engram.unadmitted_reports).unwrap(),
        serde_json::to_value(&reports).unwrap(),
        "the bound survives a restart"
    );
}

#[test]
fn an_oversized_row_is_trimmed_at_the_next_settlement() {
    let session = ObservationSession::new();
    let earlier = ENGRAM_DISCHARGED_REPORT_RETENTION + 40;
    for turn in 0..earlier {
        session.retain(recorded_report(&turn_report(&session, turn)));
    }
    settle_turn(&session, earlier);
    assert_eq!(
        retained_turns(&session),
        turn_refs(earlier + 1 - ENGRAM_DISCHARGED_REPORT_RETENTION..earlier + 1)
    );
}

#[test]
fn pending_and_locally_failed_reports_are_never_evicted_and_keep_their_bytes() {
    let session = ObservationSession::new();
    let pending = turn_report(&session, 1000);
    let mut failed = turn_report(&session, 1001);
    for envelope in &mut failed.envelopes {
        envelope.delivery = EngramRuntimeObservationDelivery::LocalFailure {
            code: "runtime_observation_retry_exhausted".into(),
            message: "gave up".into(),
        };
    }
    session.retain(pending.clone());
    session.retain(failed.clone());
    let turns = ENGRAM_DISCHARGED_REPORT_RETENTION + 5;
    for turn in 0..turns {
        settle_turn(&session, turn);
    }
    let reports = session.record(|record| record.engram.unadmitted_reports.clone());
    assert_eq!(
        reports[0], pending,
        "a pending report keeps its exact content for a retry"
    );
    assert_eq!(
        reports[1], failed,
        "a local failure stays for its resolution"
    );
    assert_eq!(
        retained_turns(&session)[2..],
        turn_refs(turns - ENGRAM_DISCHARGED_REPORT_RETENTION..turns)
    );
    assert!(
        session.record(|record| runtime_observation_has_removal_duty(record)),
        "the session still owes those two"
    );

    // Retrying the kept report still finds it, and its turn then counts as the
    // oldest discharged one, so it leaves first.
    session
        .state
        .install_runtime_observation_result(&session.id, &pending, recorded_report(&pending), 0)
        .unwrap();
    let after = retained_turns(&session);
    assert_eq!(after[0], "turn-1001");
    assert!(!after.contains(&"turn-1000".to_owned()));
    assert_eq!(
        after[1..],
        turn_refs(turns - ENGRAM_DISCHARGED_REPORT_RETENTION..turns)
    );
}

#[test]
fn a_partly_answered_report_is_never_evicted() {
    let session = ObservationSession::new();
    let mut partly_delivered = recorded_report(&turn_report(&session, 1000));
    assert!(
        partly_delivered.envelopes.len() >= 2,
        "a closing report has its turn and its sighting"
    );
    partly_delivered.envelopes[1].delivery =
        EngramRuntimeObservationDelivery::Pending { last_error: None };
    let mut partly_failed = recorded_report(&turn_report(&session, 1001));
    partly_failed.envelopes[1].delivery = EngramRuntimeObservationDelivery::LocalFailure {
        code: "runtime_observation_retry_exhausted".into(),
        message: "gave up".into(),
    };
    session.retain(partly_delivered.clone());
    session.retain(partly_failed.clone());
    for turn in 0..ENGRAM_DISCHARGED_REPORT_RETENTION + 5 {
        settle_turn(&session, turn);
    }
    let reports = session.record(|record| record.engram.unadmitted_reports.clone());
    assert_eq!(
        reports[0], partly_delivered,
        "its pending envelope still has to be delivered"
    );
    assert_eq!(
        reports[1], partly_failed,
        "its failed envelope is still unresolved"
    );
}
