// Adopted Claude terminal outcomes at the durable observation producer boundary.
// Uses the production frame router; no grant or outcome is synthesized by the test.
use super::*;

fn reporting_claude(label: &str) -> (ReportingTurn, mpsc::Receiver<ClaudeRuntimeCommand>) {
    let (runtime, receiver) = test_claude_runtime_handle(label);
    let fixture = ReportingTurn::start_with_history_and_runtime(
        label,
        true,
        ReplyMode::Recorded,
        true,
        true,
        None,
        Some(runtime),
    );
    fixture.turn.record(|record| {
        assert_eq!(record.session.agent, Agent::Claude);
        assert!(matches!(
            record.runtime.runtime_token(),
            Some(RuntimeToken::Claude(_))
        ));
    });
    (fixture, receiver)
}

fn assert_reported_outcome(fixture: &ReportingTurn, turn_ref: &str, expected: &str) {
    let observed = fixture.observation();
    assert!(
        observed.durable_before_send,
        "the reported facts are durable"
    );
    let event = occurrence(&observed);
    assert_eq!(event["kind"], "unadmitted_turn");
    assert_eq!(event["host_turn_ref"], turn_ref);
    assert_eq!(
        event["outcome"], expected,
        "the observed result survives closing"
    );
    assert_eq!(
        fixture
            .producer
            .requests()
            .iter()
            .filter(|request| {
                request["occurrence"]["kind"] == "unadmitted_turn"
                    && request["occurrence"]["host_turn_ref"] == turn_ref
            })
            .count(),
        1,
        "one immutable closing report owns the adopted turn"
    );
    assert!(
        fixture
            .turn
            .record(|record| record.engram.unadmitted_active_turn.is_none())
    );
    fixture.assert_no_granted_reporting();
}

fn assert_result_outcome(is_error: bool, unresolved_identity: bool) {
    let (mut fixture, _runtime_rx) =
        reporting_claude(&format!("adopted-outcome-{is_error}-{unresolved_identity}"));
    let turn_ref = fixture.turn.record(|record| {
        assert_eq!(
            record.adopted_claude_turn_generation,
            Some(record.active_turn_generation)
        );
        record
            .engram
            .unadmitted_active_turn
            .as_ref()
            .expect("the runtime-started turn was adopted")
            .host_turn_ref
            .clone()
    });
    let mut frame = result(is_error);
    if unresolved_identity {
        frame["user_message_uuid"] = json!("99999999-0000-4000-8000-000000000009");
    }
    fixture.reader.feed(frame);
    assert_reported_outcome(
        &fixture,
        &turn_ref,
        if is_error { "failed" } else { "succeeded" },
    );
}

#[test]
fn adopted_runtime_success_reports_succeeded() {
    assert_result_outcome(false, false);
}

#[test]
fn adopted_runtime_error_reports_failed() {
    assert_result_outcome(true, false);
}

#[test]
fn adopted_unresolved_success_reports_succeeded() {
    assert_result_outcome(false, true);
}

#[test]
fn adopted_unresolved_error_reports_failed() {
    assert_result_outcome(true, true);
}

#[test]
fn adopted_close_without_observed_result_reports_unknown() {
    let (fixture, _runtime_rx) = reporting_claude("adopted-no-result");
    let (token, generation, turn_ref) = fixture.turn.record(|record| {
        (
            record.runtime.runtime_token().expect("current runtime"),
            record.adopted_claude_turn_generation,
            record
                .engram
                .unadmitted_active_turn
                .as_ref()
                .expect("the adopted turn has not received a result")
                .host_turn_ref
                .clone(),
        )
    });
    fixture
        .turn
        .state
        .close_runtime_observation(
            &fixture.turn.session_id,
            &token,
            generation,
            EngramObservedTurnOutcome::Unknown,
        )
        .expect("an end without a result is still reportable");
    assert_reported_outcome(&fixture, &turn_ref, "unknown");
}
