//! Terminal capture failure releases scheduling, without making a report or
//! weakening the association that a later opening must validate for itself.
use super::*;

fn assert_no_report_successor(fixture: &TwoClaims, turn_ref: &str) {
    let guard = phase_sync::PollGuard::new();
    loop {
        if fixture
            .turn
            .record(|record| record.engram.unadmitted_active_turn.is_some())
        {
            break;
        }
        guard.wait("the queued successor opens after terminal no-report settlement");
    }
    fixture.turn.record(|record| {
        assert!(!engram_named_selection_capture_pending(record));
        let next = record.engram.unadmitted_active_turn.as_ref().unwrap();
        assert_ne!(next.host_turn_ref, turn_ref);
        assert_eq!(next.opening_root.as_ref().unwrap().binding, fixture.second);
        assert!(record.queued_prompts.is_empty());
        assert!(
            record
                .engram
                .unadmitted_reports
                .iter()
                .all(|report| report.host_turn_ref != turn_ref)
        );
        assert!(record.session.messages.iter().any(|message| {
            matches!(message, Message::Text { text, .. }
                if text.contains(turn_ref) && text.contains("no predecessor report:"))
        }));
    });
}

#[test]
fn scope_refusal_releases_a_named_successor_without_an_old_report() {
    let fixture = TwoClaims::new("named-terminal-scope-refusal");
    let job = fixture.stop_before_capture(false);
    let mut different_fence = fixture.first.clone();
    different_fence.claim_fence += 1;
    fixture
        .turn
        .transport
        .set_current_work_binding(Some(different_fence));
    fixture.release_worker();
    assert_no_report_successor(&fixture, &job.trace.host_turn_ref);
    fixture.turn.record(|record| {
        let failed = record
            .engram
            .unadmitted_closing_jobs
            .iter()
            .find(|retained| retained.trace.host_turn_ref == job.trace.host_turn_ref)
            .unwrap();
        assert!(failed.terminal_failure.is_some());
        assert!(
            failed
                .last_error
                .as_ref()
                .unwrap()
                .contains("no predecessor report:")
        );
    });
}

#[test]
fn exhausted_capture_releases_a_named_successor_without_a_fabricated_report() {
    let fixture = TwoClaims::new("named-terminal-capture-exhausted");
    let job = fixture.stop_before_capture(false);
    let prepared = {
        let mut inner = fixture.turn.state.inner.lock().unwrap();
        let record = inner.session_mut(&fixture.turn.session_id).unwrap();
        let retained = record
            .engram
            .unadmitted_closing_jobs
            .iter_mut()
            .find(|retained| retained.trace.host_turn_ref == job.trace.host_turn_ref)
            .unwrap();
        assert!(retained.policy.is_none());
        retained.attempts = ENGRAM_RUNTIME_OBSERVATION_MAX_ATTEMPTS - 1;
        let prepared = retained.clone();
        fixture
            .turn
            .state
            .commit_session_locked(&mut inner, &fixture.turn.session_id)
            .expect("persist the prepared retry image before releasing its worker");
        prepared
    };
    assert!(
        runtime_observation_session_entry_is_durable(
            &fixture.turn.state.persistence_path,
            &fixture.turn.session_id,
            "engramUnadmittedClosingJobs",
            &serde_json::to_value(&prepared).unwrap(),
        )
        .expect("read back the exact prepared retry image")
    );
    // One real worker attempt reaches the already-retained retry limit. The
    // fixture does not sleep through retries or alter their production budget.
    fixture
        .turn
        .transport
        .observation_policy_errors
        .lock()
        .unwrap()
        .push_back(EngramTransportError::protocol(
            "the producer remains unavailable",
        ));
    fixture.release_worker();
    assert_no_report_successor(&fixture, &job.trace.host_turn_ref);
    fixture.turn.record(|record| {
        let failed = record
            .engram
            .unadmitted_closing_jobs
            .iter()
            .find(|retained| retained.trace.host_turn_ref == job.trace.host_turn_ref)
            .unwrap();
        assert_eq!(failed.attempts, ENGRAM_RUNTIME_OBSERVATION_MAX_ATTEMPTS);
        assert!(failed.terminal_failure.is_some());
    });
}

#[test]
fn a_removed_closing_job_cannot_keep_the_acknowledged_choice_held() {
    let fixture = TwoClaims::new("named-terminal-job-removed");
    let job = fixture.stop_before_capture(false);
    fixture.turn.record_mut(|record| {
        record.engram.unadmitted_closing_jobs.clear();
    });
    let started = fixture
        .turn
        .state
        .dispatch_next_queued_turn_with_prompt(&fixture.turn.session_id, false)
        .unwrap()
        .expect("an absent predecessor cannot hold this queue forever");
    fixture.turn.state.deliver_queued_turn_dispatch(started);
    assert_no_report_successor(&fixture, &job.trace.host_turn_ref);
}
