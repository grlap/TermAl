//! The automatic retry of an admission parked on a bind backoff while the
//! prior turn's grant is still active because its closing checkpoint ended
//! unsettled. The park must schedule the admission retry and show it; when the
//! retry is due, the rebind's status reconciliation settles the prior grant
//! and the queued head is delivered once, without a Resume. A refused or
//! mismatched recovery ends in a durable hold, Stop and Cancel end the retry,
//! and an unrelated active grant is never admitted.
//!
//! Owns these witnesses only, on the parent's CausalRoot fixture: its scripted
//! budget clock, its stateful producer behind the fault transport, and its
//! real blocking persist writer, with no sleeps and no synthetic load. Does not
//! own the causal card of that park (the parent module) or the begin replay's
//! own retry (`engram_begin_replay.rs`). New module beside the causal tests,
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

fn write(fixture: &CausalRoot, change: impl FnOnce(&mut SessionRecord)) {
    let mut inner = fixture.state.inner.lock().unwrap();
    let index = inner.find_session_index(&fixture.session).unwrap();
    change(inner.session_mut_by_index(index).unwrap());
}

fn operations(fixture: &CausalRoot) -> Vec<String> {
    fixture.transport.order.lock().unwrap().clone()
}

/// Turn N ends with its closing checkpoint failed by `mode` and the queued
/// continuation parked on the bind backoff. Returns the fixture, the head as
/// queued and the wall time just before the turn closed.
fn parked(mode: CausalFaultMode) -> (CausalRoot, Value, chrono::DateTime<chrono::Utc>) {
    let fixture = CausalRoot::new(Some(("turn_checkpoint", 1)));
    *fixture.transport.mode.lock().unwrap() = mode;
    if matches!(mode, CausalFaultMode::DeadlineAfterApply) {
        *fixture.transport.clock.lock().unwrap() = Some(fixture.state.engram_budget_clock());
    }
    let (token, original_head, _) = fixture.start_and_queue();
    let closed_at = chrono::Utc::now();
    let _ = fixture
        .state
        .finish_turn_ok_if_runtime_matches(&fixture.session, &token);
    fixture.await_causal_publication();
    (fixture, original_head, closed_at)
}

/// The two-second tick, run past the scheduled retry: the bind backoff runs
/// out on the scripted clock, the retry record's durable acknowledgement is
/// awaited, one pass marks it acknowledged and the next starts the attempt.
fn retry_when_due(fixture: &CausalRoot) {
    let clock = fixture.state.engram_budget_clock();
    let (backoff, waiter, due) = read(fixture, |record| {
        (
            record.engram.next_bind_retry_at,
            record.engram.abort_retry_fence.clone(),
            record
                .engram
                .admission_retry
                .as_ref()
                .map(|retry| retry.due_at.clone())
                .expect("the retry is scheduled"),
        )
    });
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

/// A natural closing deadline leaves the prior grant active locally. The
/// following admission meets the bind backoff and retains the exact head.
/// Before the retry fix, inspect the durable refusal before the desired-state
/// assertion fails; after the fix, the same witness checks the retry's visible
/// attempt and due time, then its once-only delivery.
#[test]
fn a_closing_deadline_backoff_shows_the_park_and_schedules_its_retry() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (fixture, original_head, closed_at) = parked(CausalFaultMode::DeadlineAfterApply);
    assert_eq!(prompts(&fixture.receiver), 0, "the continuation is withheld");
    let cards = fixture.durable_cards();
    let closing = cards.iter().find(|card| {
        card["stage"] == "checkpoint" && card["decision"] == "degraded"
    }).expect("the natural closing checkpoint failed");
    assert_eq!(closing["causalFailure"]["operation"], "turn_checkpoint");
    assert_eq!(closing["causalFailure"]["originalCode"], "deadline_exceeded");
    assert_eq!(closing["causalFailure"]["failureClass"], "deadline");
    let admission = cards.last().expect("durable admission card");
    assert_eq!(admission["decision"], "degraded");
    assert_eq!(admission["refusalCode"], "control_unavailable");
    assert_eq!(admission["causalFailure"]["attemptId"], closing["causalFailure"]["attemptId"]);
    assert_eq!(admission["causalFailure"]["continuationId"], original_head["id"]);
    let reason = admission["causalFailure"]["continuationReason"].as_str().unwrap();
    let clock = fixture.state.engram_budget_clock();
    let (retry, preview, head, active, backoff_left) = read(&fixture, |record| {
        (
            record.engram.admission_retry.clone(),
            record.session.preview.clone(),
            record.queued_prompts.front()
                .map(|queued| serde_json::to_value(&queued.pending_prompt).unwrap()),
            record.engram.active_grant_id.clone(),
            record.engram.next_bind_retry_at
                .and_then(|at| at.checked_duration_since(clock.now())),
        )
    });
    assert_eq!(head.as_ref(), Some(&original_head), "the exact head is retained");
    let lost = fixture.transport.lost_request.lock().unwrap().clone().unwrap();
    assert_eq!(active.as_deref(), lost["grant_id"].as_str());
    assert_eq!(fixture.transport.attempts.lock().unwrap()["turn_evaluate"], 1,
        "the backoff parks admission before any new Evaluate");
    let backoff_left = backoff_left.expect("the pending bind backoff is observed");
    assert!(!backoff_left.is_zero());
    if retry.is_none() {
        assert_eq!(preview, ENGRAM_ADMISSION_HELD_PREVIEW);
        assert!(reason.contains("existing scheduler did not admit a retry"),
            "the unchanged-product hold names the scheduling refusal: {reason}");
        eprintln!("closing deadline witness: deadline_exceeded -> control_unavailable; exact_head=true; provider_handoffs=0; scheduled_retry=false; hold={reason}");
    }
    let retry = retry.expect("the closing-deadline backoff park schedules an automatic retry");
    assert_eq!(retry.prompt_id, original_head["id"].as_str().unwrap());
    assert_eq!(retry.attempts, 1);
    assert!(preview.contains("retrying automatically")
        && preview.contains("attempt 1") && preview.contains(&retry.due_at),
        "the preview shows the actual attempt and due time: {preview}");
    assert!(reason.contains("Automatic retry scheduled")
        && reason.contains("attempt 1") && reason.contains(&retry.due_at),
        "the durable card shows the actual attempt and due time: {reason}");
    let due = chrono::DateTime::parse_from_rfc3339(&retry.due_at).unwrap()
        .with_timezone(&chrono::Utc);
    assert!(due >= closed_at + chrono::Duration::from_std(backoff_left).unwrap(),
        "the shared retry respects the observed bind backoff");
    retry_when_due(&fixture);
    assert_eq!(prompts(&fixture.receiver), 1, "delivered once without Resume");
    assert_eq!(read(&fixture, |record| record.queued_prompts.len()), 0);
    assert_eq!(fixture.transport.attempts.lock().unwrap()["turn_evaluate"], 2);
    let order = operations(&fixture);
    assert!(order.iter().rposition(|operation| operation == "session_status").unwrap()
        < order.iter().rposition(|operation| operation == "turn_evaluate").unwrap(),
        "status reconciliation precedes the continuation's Evaluate");
}

/// Turn N's closing checkpoint is applied by the producer but its reply is
/// lost, so grant N stays active locally; the queued continuation's admission
/// then meets the bind backoff and parks. The park schedules the automatic
/// admission retry, and the preview and the card say so, with the attempt and
/// a due time no earlier than the bind backoff. When it is due, the rebind's
/// status reconciliation clears grant N (the producer shows no open grant)
/// before any new evaluate, and the continuation is delivered exactly once
/// without a Resume.
#[test]
fn a_bind_backoff_park_after_an_unsettled_closing_checkpoint_retries_and_delivers_once() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (fixture, original_head, closed_at) = parked(CausalFaultMode::LostReply);
    assert_eq!(prompts(&fixture.receiver), 0, "the continuation is parked first");

    let clock = fixture.state.engram_budget_clock();
    let (retry, preview, head, backoff_left) = read(&fixture, |record| {
        (
            record.engram.admission_retry.clone(),
            record.session.preview.clone(),
            record
                .queued_prompts
                .front()
                .map(|queued| serde_json::to_value(&queued.pending_prompt).unwrap()),
            record
                .engram
                .next_bind_retry_at
                .and_then(|at| at.checked_duration_since(clock.now())),
        )
    });
    assert_eq!(head.as_ref(), Some(&original_head), "the exact continuation is retained");
    let retry = retry.expect("the bind-backoff park schedules the automatic admission retry");
    assert_eq!(retry.prompt_id, original_head["id"].as_str().unwrap());
    assert_eq!(retry.attempts, 1);
    assert!(
        preview.contains("retrying automatically")
            && preview.contains("attempt 1")
            && preview.contains(&retry.due_at),
        "the preview shows the scheduled retry, its attempt and due time: {preview}"
    );
    let backoff_left = backoff_left.expect("the park is behind a pending bind backoff");
    let due = chrono::DateTime::parse_from_rfc3339(&retry.due_at)
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert!(
        due >= closed_at + chrono::Duration::from_std(backoff_left).unwrap(),
        "the retry is due no earlier than the bind backoff"
    );
    let cards = fixture.durable_cards();
    let reason = cards.last().unwrap()["causalFailure"]["continuationReason"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        reason.contains("Automatic retry scheduled")
            && reason.contains("attempt 1")
            && reason.contains(&retry.due_at),
        "the card shows the post-backoff transition, attempt and due time: {reason}"
    );
    let evaluates_before = operations(&fixture)
        .iter()
        .filter(|operation| *operation == "turn_evaluate")
        .count();

    retry_when_due(&fixture);

    assert_eq!(prompts(&fixture.receiver), 1, "delivered exactly once, without Resume");
    let (retained, active) = read(&fixture, |record| {
        (record.queued_prompts.len(), record.engram.active_grant_id.clone())
    });
    assert_eq!(retained, 0, "the continuation was handed to the provider");
    let lost = fixture.transport.lost_request.lock().unwrap().clone().unwrap();
    assert_ne!(
        active.as_deref(),
        lost["grant_id"].as_str(),
        "grant N was settled before the continuation's own grant"
    );
    let order = operations(&fixture);
    let status = order.iter().rposition(|operation| operation == "session_status");
    let evaluate = order.iter().rposition(|operation| operation == "turn_evaluate");
    assert!(
        status.is_some() && status < evaluate,
        "the status reconciliation precedes the new evaluate: {order:?}"
    );
    assert_eq!(
        order.iter().filter(|operation| *operation == "turn_evaluate").count(),
        evaluates_before + 1,
        "one new evaluate"
    );
}

/// The closing checkpoint never reached the producer, so grant N is still
/// open there. When the retry is due, the rebind's status finds it open and
/// the recovery checkpoint closes it exactly once, before the continuation's
/// evaluate; the continuation is then delivered exactly once.
#[test]
fn a_retry_closes_the_still_open_prior_grant_once_before_its_evaluate() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (fixture, _, _) = parked(CausalFaultMode::NeverStarted);
    assert!(
        read(&fixture, |record| record.engram.admission_retry.is_some()),
        "the park schedules the automatic admission retry"
    );
    assert_eq!(fixture.transport.application_count("turn_checkpoint"), 0);

    retry_when_due(&fixture);

    assert_eq!(prompts(&fixture.receiver), 1, "delivered exactly once, without Resume");
    assert_eq!(
        fixture.transport.application_count("turn_checkpoint"),
        1,
        "grant N closed exactly once"
    );
    let order = operations(&fixture);
    let close = order.iter().rposition(|operation| operation == "turn_checkpoint");
    let evaluate = order.iter().rposition(|operation| operation == "turn_evaluate");
    assert!(close < evaluate, "grant N closes before the new evaluate: {order:?}");
    assert_eq!(read(&fixture, |record| record.queued_prompts.len()), 0);
}

/// A recovery that cannot settle the prior grant: a status naming a different
/// open grant whose recovery checkpoint is refused, or answered with a receipt
/// for another grant. Each ends in a durable hold whose card names the code and
/// the grant, with the head retained and no further automatic attempt.
#[test]
fn a_refused_or_mismatched_recovery_ends_in_a_durable_actionable_hold() {
    for (mode, code) in [
        (CausalFaultMode::RecoveryRefusal, "lifecycle_hold"),
        (
            CausalFaultMode::RecoveryReceiptSecret,
            "restart_checkpoint_receipt_mismatch",
        ),
    ] {
        let _placement = TestTempRootDirectoryScope::repository_local();
        let (fixture, original_head, _) = parked(CausalFaultMode::LostReply);
        assert!(read(&fixture, |record| record.engram.admission_retry.is_some()));
        *fixture.transport.lost_request.lock().unwrap() = None;
        *fixture.transport.fault.lock().unwrap() = Some(("turn_checkpoint", 2));
        *fixture.transport.mode.lock().unwrap() = mode;

        retry_when_due(&fixture);

        let refused = fixture
            .transport
            .lost_request
            .lock()
            .unwrap()
            .clone()
            .expect("the recovery checkpoint was sent");
        assert_eq!(refused["operation"], "turn_checkpoint");
        assert_eq!(prompts(&fixture.receiver), 0, "no provider handoff");
        let cards = fixture.durable_cards();
        let card = cards.last().unwrap();
        assert_eq!(card["causalFailure"]["originalCode"], code, "the hold names the code");
        // The grant id is a received value, so the hold names it by the exact
        // request it refused, whose key carries the grant, not by card text.
        let grant = refused["grant_id"].as_str().unwrap();
        let attempt = card["causalFailure"]["attemptId"].as_str().unwrap();
        assert_eq!(attempt, refused["idempotency_key"].as_str().unwrap());
        assert!(attempt.contains(grant), "the hold names the grant: {attempt}");
        let (retry, preview, head) = read(&fixture, |record| {
            (
                record.engram.admission_retry.clone(),
                record.session.preview.clone(),
                record.queued_prompts.front().map(|queued| queued.pending_prompt.id.clone()),
            )
        });
        assert_eq!(retry, None, "no silent retry loop");
        assert!(!preview.contains("retrying automatically"), "{preview}");
        assert_eq!(head.as_deref(), original_head["id"].as_str(), "the head is retained");
        let sent = operations(&fixture).len();
        fixture
            .state
            .engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::hours(1));
        assert_eq!(operations(&fixture).len(), sent, "nothing more is sent");
    }
}

/// Stop and Cancel each end the scheduled retry: nothing is sent when it would
/// have been due and the head is not delivered.
#[test]
fn stop_and_cancel_end_the_scheduled_retry() {
    for stop in [true, false] {
        let _placement = TestTempRootDirectoryScope::repository_local();
        let (fixture, original_head, _) = parked(CausalFaultMode::LostReply);
        let due = read(&fixture, |record| {
            record
                .engram
                .admission_retry
                .as_ref()
                .map(|retry| retry.due_at.clone())
                .expect("the park schedules the automatic admission retry")
        });
        if stop {
            fixture.state.request_stop_session(&fixture.session).unwrap();
        } else {
            fixture
                .state
                .cancel_queued_prompt(&fixture.session, original_head["id"].as_str().unwrap())
                .unwrap();
        }
        let sent = operations(&fixture).len();
        let at = chrono::DateTime::parse_from_rfc3339(&due)
            .unwrap()
            .with_timezone(&chrono::Utc)
            + chrono::Duration::hours(1);
        let clock = fixture.state.engram_budget_clock();
        clock.advance(Duration::from_secs(3600));
        fixture.state.engram_abort_retry_tick(at);
        fixture.state.engram_abort_retry_tick(at);
        assert_eq!(operations(&fixture).len(), sent, "nothing is sent (stop={stop})");
        assert_eq!(prompts(&fixture.receiver), 0, "not delivered (stop={stop})");
        assert!(
            read(&fixture, |record| record.engram.admission_retry.is_none()),
            "the retry ended (stop={stop})"
        );
    }
}

/// Scope guard: the exception admits only the prior turn's grant whose newest
/// checkpoint card is the Degraded closing checkpoint naming this head. Any
/// other active grant keeps the hold: the retry is dropped, nothing is sent and
/// the head is not delivered.
#[test]
fn an_unrelated_active_grant_still_holds_the_head() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (fixture, _, _) = parked(CausalFaultMode::LostReply);
    let due = read(&fixture, |record| {
        record
            .engram
            .admission_retry
            .as_ref()
            .map(|retry| retry.due_at.clone())
            .expect("the park schedules the automatic admission retry")
    });
    write(&fixture, |record| {
        record.engram.active_grant_id = Some("unrelated-active-grant".to_owned());
    });
    let sent = operations(&fixture).len();
    let at = chrono::DateTime::parse_from_rfc3339(&due)
        .unwrap()
        .with_timezone(&chrono::Utc)
        + chrono::Duration::milliseconds(1);
    fixture.state.engram_budget_clock().advance(Duration::from_secs(3600));
    fixture.state.engram_abort_retry_tick(at);
    fixture.state.engram_abort_retry_tick(at);
    assert_eq!(operations(&fixture).len(), sent, "nothing is sent");
    assert_eq!(prompts(&fixture.receiver), 0, "the head is not delivered");
    let (retry, preview) = read(&fixture, |record| {
        (record.engram.admission_retry.clone(), record.session.preview.clone())
    });
    assert_eq!(retry, None, "the retry is dropped");
    assert!(!preview.contains("retrying automatically"), "{preview}");
}
