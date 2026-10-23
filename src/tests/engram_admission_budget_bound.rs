// Owns the tests of the overall automatic-recovery budget as it bounds every
// automatic Engram retry kind of a queue head, not only a head that entered
// prepared-begin recovery: repeated certified non-application
// (control_request_expired, control_store_busy) and repeated uncertain
// outcomes run to the head's 240 second budget and end in the explicit
// Resume-or-Cancel hold that keeps the exact retained request and key; a
// success inside the budget is delivered once; a restart mid-sequence keeps
// the original deadline; an explicit Resume opens one new window for the same
// retained request; and persistent bind failures end the same way, with the
// growth of their failure cards measured. Does not own the budget's start,
// its prepared-begin recovery or its window bookkeeping
// (src/tests/engram_begin_preparation_retry.rs), the schedule's delays and
// cap (src/tests/engram_admission_retry.rs), or the replay of a certified
// non-applied request itself (src/tests/engram_control_non_application.rs).
// New module, a child of the admission retry tests whose fixtures it uses.
use super::*;

const CERTIFIED_CODES: [&str; 2] = ["control_request_expired", "control_store_busy"];

/// What the exhausted hold says of the last invocation when its code is a
/// producer certificate that only this invocation committed nothing.
const CERTIFIED_SENTENCE: &str =
    "This invocation committed no changes (earlier attempts with the same key may have).";

/// What the exhausted hold says of the last invocation for every other
/// control outcome: it may have applied.
const UNKNOWN_SENTENCE: &str = "This invocation's outcome is unknown.";

const EXHAUSTED_PREFIX: &str = "Engram: automatic re-admission exhausted after";

fn instant(at: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(at)
        .unwrap()
        .with_timezone(&chrono::Utc)
}

fn requests_named(transport: &ScriptedEngramControlTransport, operation: &str) -> Vec<Value> {
    transport
        .requests()
        .into_iter()
        .filter(|sent| sent.request["operation"] == operation)
        .map(|sent| sent.request)
        .collect()
}

/// The head's saved admission budget.
fn budget(state: &AppState, session: &str) -> EngramBeginPreparationLineage {
    with_record(state, session, |record| {
        record.engram.begin_preparation_lineage.clone()
    })
    .expect("the head's budget started when it was accepted for control admission")
}

fn deadline(window: &EngramBeginPreparationLineage) -> chrono::DateTime<chrono::Utc> {
    let deadline = instant(&window.deadline);
    assert_eq!(
        deadline,
        instant(&window.admitted_at) + chrono::Duration::seconds(ENGRAM_ADMISSION_BUDGET_SECONDS),
        "the deadline is the acceptance plus the 240 second budget"
    );
    deadline
}

/// When the retry holding the head is next due, whichever kind holds it.
fn next_due(state: &AppState, session: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    with_record(state, session, |record| {
        record
            .engram
            .admission_retry
            .as_ref()
            .map(|retry| retry.due_at.clone())
            .or_else(|| record.engram.bind_retry.as_ref().map(|retry| retry.due_at.clone()))
    })
    .map(|due| instant(&due))
}

/// Moves the state's scripted budget clock, already pinned, forward to `at`,
/// so every due time, park and deadline the host reads afterwards sees that
/// simulated time.
fn advance_budget_clock_to(state: &AppState, at: chrono::DateTime<chrono::Utc>) {
    let clock = state.engram_budget_clock();
    let wall = clock.wall_now();
    if at > wall {
        clock.advance((at - wall).to_std().expect("a forward step"));
    }
}

/// Runs every attempt that falls due before `deadline`, then passes the
/// deadline itself, the way the two-second tick does. Time moves on the
/// state's scripted clock, so each park schedules its next attempt from the
/// simulated time, not from real time that the test does not advance.
fn run_to_the_budget(state: &AppState, session: &str, deadline: chrono::DateTime<chrono::Utc>) {
    // Pinned once: its wall reading continues from real time and from then
    // on moves only with the scripted clock.
    state.engram_budget_clock().pin_wall();
    for _ in 0..64 {
        match next_due(state, session) {
            Some(due) if due < deadline => {
                let at = due + chrono::Duration::milliseconds(1);
                advance_budget_clock_to(state, at);
                tick_at(state, session, at);
            }
            _ => break,
        }
    }
    let past = deadline + chrono::Duration::milliseconds(1);
    advance_budget_clock_to(state, past);
    tick_at(state, session, past);
}

fn control_cards(state: &AppState, session: &str) -> usize {
    with_record(state, session, |record| {
        record
            .session
            .messages
            .iter()
            .filter(|message| matches!(message, Message::EngramControl { .. }))
            .count()
    })
}

/// Asserts the head ended its automatic recovery in the explicit hold: no
/// retry of any kind, the prompt retained, waiting and not interrupted, the
/// queue paused, its window ended but kept as `window` was, and a preview
/// that names the attempt count, the last cause and `sentence`, never the
/// other sentence.
fn assert_exhausted_hold(
    state: &AppState,
    session: &str,
    window: &EngramBeginPreparationLineage,
    sentence: &str,
    label: &str,
) {
    let ended = budget(state, session);
    assert!(ended.ended, "{label}: the window ended");
    assert_eq!(
        (&ended.admitted_at, &ended.deadline),
        (&window.admitted_at, &window.deadline),
        "{label}: the window's basis is kept, never restarted"
    );
    with_record(state, session, |record| {
        assert!(record.engram.admission_retry.is_none(), "{label}: no admission retry");
        assert!(record.engram.bind_retry.is_none(), "{label}: no bind retry");
        assert!(record.engram.abort_retry.is_none(), "{label}: no abort retry");
        assert_eq!(record.queued_prompts.len(), 1, "{label}: the prompt is retained");
        let head = &record.queued_prompts[0];
        assert!(
            head.engram_waiting && !head.engram_interrupted,
            "{label}: the resumable hold (waiting {}, interrupted {}, disabled {:?}, operator paused {}, stopping {}, status {:?}, preview {:?})",
            head.engram_waiting,
            head.engram_interrupted,
            record.engram.disabled_reason,
            record.engram.operator_paused,
            record.runtime_stop_in_progress,
            record.session.status,
            record.session.preview
        );
        assert!(record.orchestrator_auto_dispatch_blocked, "{label}: the queue is paused");
        let preview = &record.session.preview;
        assert!(preview.starts_with(EXHAUSTED_PREFIX), "{label}: {preview}");
        assert!(
            preview.contains(&format!("exhausted after {} attempts;", ended.attempts)),
            "{label}: the attempt count: {preview}"
        );
        if let Some(cause) = engram_newest_card_code(record) {
            assert!(
                preview.contains(&format!("last cause {cause}.")),
                "{label}: the last cause: {preview}"
            );
        }
        assert!(preview.contains(sentence), "{label}: {preview}");
        let other = if sentence == CERTIFIED_SENTENCE {
            UNKNOWN_SENTENCE
        } else {
            CERTIFIED_SENTENCE
        };
        assert!(!preview.contains(other), "{label}: never the other wording: {preview}");
    });
}

/// The exhausted hold says what is known of the last invocation, and never
/// contradicts itself: a begin Engram applied whose receipt came after the
/// deadline is neither "unknown" nor "committed nothing" (its own sentence
/// says it was applied), a local preparation cause sent nothing, and only a
/// producer certificate says nothing was committed.
#[test]
fn the_exhausted_hold_wording_follows_what_is_known_of_the_last_invocation() {
    for (cause, sentence) in [
        ("control_request_expired", CERTIFIED_SENTENCE),
        ("control_store_busy", CERTIFIED_SENTENCE),
        ("deadline_exceeded", UNKNOWN_SENTENCE),
        ("control_unavailable", UNKNOWN_SENTENCE),
        ("begin_applied_after_deadline", ""),
        ("begin_preparation_unacknowledged", ""),
        ("begin_preparation_pending", ""),
    ] {
        assert_eq!(
            EngramSpentInvocation::of_cause(cause).sentence().trim(),
            sentence,
            "{cause}"
        );
    }
}

/// C1: an evaluate the producer keeps answering with a certified
/// non-application is replayed exactly while the head's budget runs, and at
/// its deadline automatic recovery ends: no attempt starts after it, the
/// exact request and key are kept, nothing is rebound or granted, and the
/// hold says this invocation committed no changes.
#[test]
fn repeated_certified_non_application_runs_to_the_budget_and_holds() {
    for code in CERTIFIED_CODES {
        let mut replies = vec![bind_reply("budget-token")];
        replies.extend((0..60).map(|_| remote_error_reply(code)));
        let (state, session, receiver, transport) = root_fixture(replies);
        deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
        assert!(admission_retry(&state, &session).is_some(), "{code}: scheduled");
        let window = budget(&state, &session);
        let deadline = deadline(&window);

        run_to_the_budget(&state, &session, deadline);
        let sent = transport.requests().len();
        tick_at(&state, &session, deadline + chrono::Duration::hours(2));
        assert_eq!(
            transport.requests().len(),
            sent,
            "{code}: no automatic attempt after the budget"
        );
        assert_eq!(abort_retry::prompts_received(&receiver), 0, "{code}: nothing delivered");
        let evaluates = requests_named(&transport, "turn_evaluate");
        assert!(evaluates.len() >= 2, "{code}: replayed inside the budget");
        assert!(
            evaluates.iter().all(|request| request == &evaluates[0]),
            "{code}: the exact retained request and key"
        );
        assert_eq!(requests_named(&transport, "session_bind").len(), 1, "{code}: no rebind");
        assert!(requests_named(&transport, "turn_begin").is_empty(), "{code}: no grant begun");
        assert_exhausted_hold(&state, &session, &window, CERTIFIED_SENTENCE, code);
        with_record(&state, &session, |record| {
            assert!(
                record.queued_prompts[0].engram_evaluate.is_some(),
                "{code}: the retained evaluate is kept for Resume"
            );
        });
    }
}

/// C1: repeated uncertain outcomes (an evaluate that keeps missing its
/// deadline) end at the same budget, and the hold says the outcome is
/// unknown, never that nothing was committed; the retained evaluate and the
/// failure cards stay.
#[test]
fn repeated_uncertain_outcomes_run_to_the_budget_and_say_the_outcome_is_unknown() {
    let mut replies = vec![bind_reply("budget-token")];
    replies.extend((0..60).map(|_| deadline_reply()));
    let (state, session, receiver, transport) = root_fixture(replies);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    let window = budget(&state, &session);
    let deadline = deadline(&window);

    run_to_the_budget(&state, &session, deadline);
    let sent = transport.requests().len();
    tick_at(&state, &session, deadline + chrono::Duration::hours(2));
    assert_eq!(transport.requests().len(), sent, "no automatic attempt after the budget");
    assert_eq!(abort_retry::prompts_received(&receiver), 0, "nothing delivered");
    let keys = evaluate_keys(&transport);
    assert!(keys.iter().all(|key| key == &keys[0]), "no new key while unresolved");
    assert!(requests_named(&transport, "turn_begin").is_empty(), "no grant begun");
    assert_exhausted_hold(&state, &session, &window, UNKNOWN_SENTENCE, "deadline_exceeded");
    with_record(&state, &session, |record| {
        assert!(
            record.queued_prompts[0].engram_evaluate.is_some(),
            "the retained evaluate is kept"
        );
    });
    assert!(control_cards(&state, &session) >= 1, "the earlier failure stays visible");
}

/// C1: a certified non-application followed by success inside the budget is
/// delivered exactly once, with the one retained key.
#[test]
fn certified_non_application_then_success_inside_the_budget_delivers_once() {
    for code in CERTIFIED_CODES {
        let (state, session, receiver, transport) = root_fixture([
            bind_reply("budget-token"),
            remote_error_reply(code),
            remote_error_reply(code),
            grant_reply("budget-grant"),
            begin_reply("budget-grant"),
        ]);
        deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
        let deadline = deadline(&budget(&state, &session));
        tick_past_due(&state, &session);
        assert!(admission_retry(&state, &session).is_some(), "{code}: still scheduled");
        let due = next_due(&state, &session).expect("the next attempt is due");
        assert!(due < deadline, "{code}: inside the budget");
        tick_past_due(&state, &session);
        assert_eq!(abort_retry::prompts_received(&receiver), 1, "{code}: exactly once");
        let evaluates = requests_named(&transport, "turn_evaluate");
        assert_eq!(evaluates.len(), 3, "{code}");
        assert!(
            evaluates.iter().all(|request| request == &evaluates[0]),
            "{code}: the one retained request and key"
        );
        with_record(&state, &session, |record| {
            assert!(record.engram.admission_retry.is_none(), "{code}");
            assert!(record.queued_prompts.is_empty(), "{code}: delivered");
        });
    }
}

/// C1: a restart mid-sequence keeps the head's original budget: the saved
/// start and deadline are read back unchanged, and the first tick past the
/// ORIGINAL deadline ends automatic recovery with no control call, rather
/// than granting 240 seconds from the restart.
#[test]
fn a_restart_mid_sequence_keeps_the_original_budget() {
    let code = CERTIFIED_CODES[0];
    let mut replies = vec![bind_reply("budget-token")];
    replies.extend((0..60).map(|_| remote_error_reply(code)));
    let (state, session, receiver, transport) = root_fixture(replies);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    let window = budget(&state, &session);
    let deadline = deadline(&window);
    tick_past_due(&state, &session);
    let_backoff_pass(&state, &session);
    abort_retry::await_settlement_acknowledgement(&state, &session);
    state.engram_abort_retry_tick(chrono::Utc::now());
    assert!(
        admission_retry(&state, &session).is_some_and(|retry| retry.acknowledged),
        "a durable retry before the restart"
    );

    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let saved = PersistedSessionRecord::from_record(&inner.sessions[index]);
        inner.sessions[index] = saved.into_record().unwrap();
        inner.recover_interrupted_sessions();
    }
    let reloaded = budget(&state, &session);
    assert_eq!(
        (&reloaded.admitted_at, &reloaded.deadline),
        (&window.admitted_at, &window.deadline),
        "the restart reads back the original budget"
    );
    assert!(!reloaded.ended, "still running before its deadline");

    let sent = transport.requests().len();
    tick_at(&state, &session, deadline + chrono::Duration::milliseconds(1));
    tick_at(&state, &session, deadline + chrono::Duration::seconds(30));
    assert_eq!(
        transport.requests().len(),
        sent,
        "no control call past the original deadline after a restart"
    );
    assert_eq!(abort_retry::prompts_received(&receiver), 0, "nothing delivered");
    assert_exhausted_hold(&state, &session, &window, CERTIFIED_SENTENCE, "after a restart");
}

/// C1 with Greg's Resume card: an explicit accepted Resume of an exhausted
/// head opens exactly one new window, from the Resume, and replays the same
/// retained request and key; a second Resume while that window runs keeps
/// it.
#[test]
fn resume_after_exhaustion_opens_one_window_with_the_same_request() {
    let code = CERTIFIED_CODES[1];
    let mut replies = vec![bind_reply("budget-token")];
    replies.extend((0..60).map(|_| remote_error_reply(code)));
    let (state, session, receiver, transport) = root_fixture(replies);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    let window = budget(&state, &session);
    run_to_the_budget(&state, &session, deadline(&window));
    assert_exhausted_hold(&state, &session, &window, CERTIFIED_SENTENCE, "before Resume");
    let before = requests_named(&transport, "turn_evaluate");

    let _ = state.resume_session_queue(&session);
    let opened = budget(&state, &session);
    assert!(!opened.ended, "the new window runs");
    assert!(
        instant(&opened.admitted_at) > instant(&window.admitted_at),
        "the new window starts at the Resume"
    );
    assert_eq!(
        instant(&opened.deadline),
        instant(&opened.admitted_at) + chrono::Duration::seconds(ENGRAM_ADMISSION_BUDGET_SECONDS),
        "a full budget"
    );
    let after = requests_named(&transport, "turn_evaluate");
    assert_eq!(after.len(), before.len() + 1, "the Resume replays once");
    assert!(
        after.iter().all(|request| request == &before[0]),
        "the same retained request and key, never a fresh one"
    );
    assert_eq!(abort_retry::prompts_received(&receiver), 0, "still not applied");

    state
        .release_engram_begin_preparation_hold_for_resume(&session, None)
        .expect("a repeated Resume is answered");
    assert_eq!(
        budget(&state, &session),
        opened,
        "a Resume while the window runs neither resets nor replaces it"
    );
}

/// C1 (the persistent bind failures a reviewer recorded): a bind that keeps
/// failing, uncertain or certified, is replayed exactly while the budget runs
/// and ends in the same hold at its deadline, keeping its exact wire identity.
/// No failure card is added once automatic recovery has ended; the cards
/// posted inside the window are printed as a measurement.
#[test]
fn persistent_bind_failures_end_at_the_budget_with_their_exact_identity() {
    let cases: [(&str, fn() -> ScriptedEngramControlResponse, &str); 2] = [
        (
            "deadline_exceeded",
            || {
                ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline(
                    "bind reply lost",
                )))
            },
            UNKNOWN_SENTENCE,
        ),
        ("control_store_busy", || remote_error_reply("control_store_busy"), CERTIFIED_SENTENCE),
    ];
    for (label, failure, sentence) in cases {
        // The bind retry's park dates its next attempt from real time, which
        // the scripted clock cannot move, so a failure that opens no circuit
        // is attempted on every step of `run_to_the_budget` (at most 64).
        // Script more failures than that, so every attempt meets one.
        let (state, session, receiver, transport) = root_fixture((0..80).map(|_| failure()));
        deliver_turn_dispatch(&state, root_dispatch(&state, &session, false))
            .expect("the original prompt is retained");
        let window = budget(&state, &session);
        run_to_the_budget(&state, &session, deadline(&window));
        let cards = control_cards(&state, &session);
        let sent = transport.requests().len();
        tick_at(&state, &session, deadline(&window) + chrono::Duration::hours(2));
        assert_eq!(transport.requests().len(), sent, "{label}: no bind after the budget");
        assert_eq!(
            control_cards(&state, &session),
            cards,
            "{label}: no failure card after automatic recovery ended"
        );
        let binds = requests_named(&transport, "session_bind");
        // How many failure cards a persistent bind failure posts inside the
        // window is measured, not asserted: that growth predates the bound
        // and is tracked on its own. What the bound owns is asserted above
        // and below.
        eprintln!("bounded bind failures: {label} made {} binds and {cards} cards", binds.len());
        assert!(
            binds.iter().all(|request| request == &binds[0]),
            "{label}: the exact bind every time"
        );
        assert!(requests_named(&transport, "turn_evaluate").is_empty(), "{label}: no grant");
        assert_eq!(abort_retry::prompts_received(&receiver), 0, "{label}: nothing delivered");
        assert_exhausted_hold(&state, &session, &window, sentence, label);
    }
}
