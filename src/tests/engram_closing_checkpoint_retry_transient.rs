//! A turn's natural closing checkpoint fails with control_unavailable, the
//! transient control failure of the 2026-10-05 stuck session, while the next
//! prompt is queued behind it. The withheld prompt must recover by itself
//! once control returns, never wait for Resume: its park schedules the shared
//! admission retry, the retry's rebind settles the prior grant before any new
//! evaluate, and the prompt is delivered exactly once. An uncertain grant
//! keeps its explicit hold.
//!
//! Owns these witnesses only, on the parent's CausalRoot fixture (scripted
//! budget clock, stateful producer behind the fault transport, real blocking
//! persist writer; no sleeps, no synthetic load), for both ways the close can
//! fail: applied with its reply lost, and never started. Does not own the
//! retry schedule (`engram_retry_schedule.rs`), its time bound, or the
//! refused and mismatched recovery holds, which the closing-checkpoint retry
//! witnesses beside this module cover. New module beside the causal tests,
//! created instead of growing them.

use super::*;

fn prompts(receiver: &mpsc::Receiver<CodexRuntimeCommand>) -> usize {
    receiver
        .try_iter()
        .filter(|command| matches!(command, CodexRuntimeCommand::Prompt { .. }))
        .count()
}

fn read<T>(fixture: &CausalRoot, look: impl FnOnce(&SessionRecord) -> T) -> T {
    let inner = fixture.state.inner.lock().unwrap();
    look(&inner.sessions[inner.find_session_index(&fixture.session).unwrap()])
}

fn sent(fixture: &CausalRoot, operation: &str) -> usize {
    fixture.transport.attempts.lock().unwrap().get(operation).copied().unwrap_or(0)
}

/// Turn N closes with its checkpoint failing as `mode` says, the queued
/// continuation parks behind it, and the closing failure is control_unavailable.
fn closed_with_control_unavailable(mode: CausalFaultMode) -> (CausalRoot, Value) {
    let fixture = CausalRoot::new(Some(("turn_checkpoint", 1)));
    *fixture.transport.mode.lock().unwrap() = mode;
    let (token, original_head, _) = fixture.start_and_queue();
    let _ = fixture
        .state
        .finish_turn_ok_if_runtime_matches(&fixture.session, &token);
    fixture.await_causal_publication();
    let cards = fixture.durable_cards();
    assert!(
        cards.iter().any(|card| card["causalFailure"]["operation"] == "turn_checkpoint"
            && card["causalFailure"]["originalCode"] == "control_unavailable"),
        "the natural close failed with control_unavailable: {cards:?}"
    );
    assert_eq!(prompts(&fixture.receiver), 0, "the continuation is withheld");
    (fixture, original_head)
}

/// Runs the two-second tick past the scheduled retry, as `retry_when_due`
/// does for the closing-checkpoint retry witnesses.
fn retry_when_due(fixture: &CausalRoot) {
    let clock = fixture.state.engram_budget_clock();
    let (backoff, waiter, due) = read(fixture, |record| {
        (
            record.engram.next_bind_retry_at,
            record.engram.abort_retry_fence.clone(),
            record.engram.admission_retry.as_ref().map(|retry| retry.due_at.clone()),
        )
    });
    let due = due.expect("the park schedules the shared admission retry");
    if let Some(left) = backoff.and_then(|at| at.checked_duration_since(clock.now())) {
        clock.advance(left + Duration::from_millis(1));
    }
    if let Some(waiter) = waiter {
        // Bounded on the fence's own clock, as its deadline is.
        waiter
            .0
            .wait_until(waiter.0.now() + crate::tests::phase_sync::DEADLOCK_GUARD);
    }
    let at = chrono::DateTime::parse_from_rfc3339(&due)
        .unwrap()
        .with_timezone(&chrono::Utc)
        + chrono::Duration::milliseconds(1);
    fixture.state.engram_abort_retry_tick(at);
    fixture.state.engram_abort_retry_tick(at);
}

/// The 2026-10-05 case, both ways the close can fail. The withheld prompt's
/// park schedules the shared admission retry (on a base without that retry,
/// this is where the Resume-only hold shows). When it is due, the prior grant
/// is settled before the one new evaluate (closed once by the recovery
/// checkpoint when the producer still holds it open, cleared when it is
/// already closed), and the prompt is delivered exactly once.
#[test]
fn a_control_unavailable_close_recovers_the_withheld_prompt_without_resume() {
    for (mode, closes_on_recovery) in [
        (CausalFaultMode::NeverStarted, true),
        (CausalFaultMode::LostReply, false),
    ] {
        let _placement = TestTempRootDirectoryScope::repository_local();
        let (fixture, original_head) = closed_with_control_unavailable(mode);
        let (retry, preview) = read(&fixture, |record| {
            (record.engram.admission_retry.clone(), record.session.preview.clone())
        });
        let retry = retry.expect("the park schedules the shared admission retry, not a Resume-only hold");
        assert_eq!(retry.prompt_id, original_head["id"].as_str().unwrap());
        assert!(preview.contains("retrying automatically"), "{preview}");
        let checkpoints_before = sent(&fixture, "turn_checkpoint");
        let evaluates_before = sent(&fixture, "turn_evaluate");
        let applied_before = fixture.transport.application_count("turn_checkpoint");

        retry_when_due(&fixture);

        assert_eq!(prompts(&fixture.receiver), 1, "delivered exactly once, without Resume");
        assert_eq!(read(&fixture, |record| record.queued_prompts.len()), 0);
        assert_eq!(sent(&fixture, "turn_evaluate"), evaluates_before + 1, "one new evaluate");
        if closes_on_recovery {
            assert_eq!(sent(&fixture, "turn_checkpoint"), checkpoints_before + 1, "one recovery close");
            assert_eq!(
                fixture.transport.application_count("turn_checkpoint"),
                applied_before + 1,
                "the prior grant closed exactly once"
            );
        } else {
            assert_eq!(sent(&fixture, "turn_checkpoint"), checkpoints_before,
                "a grant the producer already closed is cleared, not closed again");
            assert_eq!(fixture.transport.application_count("turn_checkpoint"), applied_before);
        }
        let lost = fixture.transport.lost_request.lock().unwrap().clone().unwrap();
        assert_ne!(
            read(&fixture, |record| record.engram.active_grant_id.clone()).as_deref(),
            lost["grant_id"].as_str(),
            "the prior grant was settled"
        );
    }
}

/// An uncertain grant is truly unknown authority: its scheduled retry is
/// dropped when due, nothing is sent and the prompt stays held.
#[test]
fn an_uncertain_grant_after_a_control_unavailable_close_keeps_the_hold() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (fixture, _) = closed_with_control_unavailable(CausalFaultMode::NeverStarted);
    let due = read(&fixture, |record| {
        record.engram.admission_retry.as_ref().map(|retry| retry.due_at.clone())
    })
    .expect("the park schedules the shared admission retry");
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.engram.uncertain_grant_id = record.engram.active_grant_id.clone();
    }
    let total = |fixture: &CausalRoot| fixture.transport.attempts.lock().unwrap().values().sum::<usize>();
    let before = total(&fixture);
    fixture.state.engram_budget_clock().advance(Duration::from_secs(3600));
    let at = chrono::DateTime::parse_from_rfc3339(&due)
        .unwrap()
        .with_timezone(&chrono::Utc)
        + chrono::Duration::milliseconds(1);
    fixture.state.engram_abort_retry_tick(at);
    fixture.state.engram_abort_retry_tick(at);
    assert_eq!(total(&fixture), before, "nothing is sent for an uncertain grant");
    assert_eq!(prompts(&fixture.receiver), 0, "the prompt stays held");
    let (retry, held) = read(&fixture, |record| {
        (record.engram.admission_retry.clone(), record.queued_prompts.len())
    });
    assert_eq!(retry, None, "the automatic retry ends");
    assert_eq!(held, 1, "the prompt is retained for an explicit decision");
}
