//! Closing recovery accepts only work identity retained by the original turn.
use super::*;

#[test]
fn closing_binding_recovery_requires_the_original_opening_association() {
    let turn = EOCheckedTurn::start("closing-binding-recovery", true);
    let opening = turn.record(|record| {
        record
            .engram
            .unadmitted_active_turn
            .as_ref()
            .unwrap()
            .opening_root
            .clone()
            .expect("the claimed turn retains its opening association")
    });
    // A runtime binding can be absent during recovery. The immutable turn
    // image still proves its original owner independently of the next read.
    turn.record_mut(|record| record.engram.work_binding = None);
    let job = {
        let inner = turn.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&turn.session_id).unwrap()];
        let scope = EngramRuntimeClosingScope::capture(&inner, &turn.session_id).unwrap();
        assert!(scope.binding.is_none());
        EngramRuntimeClosingJob::new(
            record.engram.unadmitted_active_turn.clone().unwrap(),
            None,
            scope,
        )
    };
    let recovered = turn
        .state
        .capture_runtime_observation_report(&turn.session_id, &job)
        .expect("the canonical read agrees with the retained opening");
    match recovered {
        EngramRuntimeClosingOutcome::Binding(scope) => {
            assert_eq!(scope.binding.as_ref(), Some(&opening.binding));
            assert_eq!(scope.store, opening.store);
        }
        _ => panic!("the original claim association must remain recoverable"),
    }

    let later = test_control_work_binding("closing-binding-later", 1);
    turn.transport.set_current_work_binding(Some(later));
    let refused = turn
        .state
        .capture_runtime_observation_report(&turn.session_id, &job)
        .err()
        .expect("a canonical claim alone cannot replace the original owner");
    assert_eq!(
        refused.code.as_deref(),
        Some("runtime_observation_scope_changed")
    );
    assert!(turn.record(|record| record.engram.work_binding.is_none()));
    assert!(turn.record(|record| record.engram.unadmitted_reports.is_empty()));
    assert!(ordinary_turn_requests(&turn, &job.trace.host_turn_ref).is_empty());
}
