// Owns the host's handling of Engram's terminal control-startup codes on a
// queued head: `control_startup_busy` re-enters the safe re-admission on the
// common recovery schedule, replaying the exact retained request with its
// key and delivering once; `control_startup_refused` fails closed, disabling
// control and holding the retained prompt with nothing sent automatically;
// and a request-level error this host does not treat specially (a live
// `storage_error`, an unknown code) keeps today's explicit hold. Does not own
// the worker eviction or the code classification
// (src/tests/engram_control_startup_codes.rs) or the schedule itself
// (src/tests/engram_admission_retry.rs). New module, a child of the
// admission retry tests whose fixtures it uses.
use super::*;

fn requests_named(transport: &ScriptedEngramControlTransport, operation: &str) -> Vec<Value> {
    transport
        .requests()
        .into_iter()
        .filter(|sent| sent.request["operation"] == operation)
        .map(|sent| sent.request)
        .collect()
}

/// A busy startup answering the evaluate parks the head on the automatic
/// schedule, not in a fatal or explicit hold, and the due attempt replays the
/// exact retained evaluate with its key: the prompt is delivered once, with
/// no fresh authorization.
#[test]
fn startup_busy_on_evaluate_is_readmitted_on_the_schedule() {
    let (state, session, receiver, transport) = root_fixture([
        bind_reply("startup-token"),
        remote_error_reply("control_startup_busy"),
        grant_reply("startup-grant"),
        begin_reply("startup-grant"),
    ]);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    assert!(receiver.try_recv().is_err(), "nothing reached the provider");
    let retry = admission_retry(&state, &session)
        .expect("a busy startup enters the automatic re-admission");
    assert_eq!(retry.code, "control_startup_busy");
    with_record(&state, &session, |record| {
        assert!(record.engram.disabled_reason.is_none(), "busy is not fatal");
        assert!(
            record.queued_prompts[0].engram_evaluate.is_some(),
            "the evaluate stays retained"
        );
    });
    tick_past_due(&state, &session);
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered once");
    let evaluates = requests_named(&transport, "turn_evaluate");
    assert_eq!(evaluates.len(), 2);
    assert_eq!(evaluates[0], evaluates[1], "the exact retained request and key");
    assert!(admission_retry(&state, &session).is_none(), "nothing is held any more");
}

/// A busy startup answering the bind is retried the same way: the exact
/// retained bind is replayed when due, and the prompt is delivered once.
#[test]
fn startup_busy_on_bind_replays_the_exact_bind() {
    let (state, session, receiver, transport) = root_fixture([
        remote_error_reply("control_startup_busy"),
        bind_reply("startup-token"),
        grant_reply("startup-grant"),
        begin_reply("startup-grant"),
    ]);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    assert!(receiver.try_recv().is_err(), "nothing reached the provider");
    with_record(&state, &session, |record| {
        assert!(record.engram.disabled_reason.is_none(), "busy is not fatal");
    });
    tick_past_due(&state, &session);
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered once");
    let binds = requests_named(&transport, "session_bind");
    assert_eq!(binds.len(), 2);
    assert_eq!(binds[0], binds[1], "the exact retained bind");
}

/// A refused startup fails closed: control is disabled with that code, the
/// prompt and its retained evaluate are kept, nothing is scheduled, and no
/// tick sends anything.
#[test]
fn startup_refused_fails_closed_and_holds_the_prompt() {
    let (state, session, receiver, transport) = root_fixture([
        bind_reply("startup-token"),
        remote_error_reply("control_startup_refused"),
    ]);
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    assert!(receiver.try_recv().is_err(), "nothing reached the provider");
    with_record(&state, &session, |record| {
        assert_eq!(
            record.engram.disabled_reason.as_deref(),
            Some("control_startup_refused"),
            "refused disables control"
        );
        assert!(record.engram.admission_retry.is_none(), "never retried automatically");
        assert_eq!(record.queued_prompts.len(), 1, "the prompt is retained");
        assert!(
            record.queued_prompts[0].engram_evaluate.is_some(),
            "the evaluate stays retained"
        );
        assert!(record.orchestrator_auto_dispatch_blocked, "the queue is held");
    });
    let sent = transport.requests().len();
    tick_past_due(&state, &session);
    assert_eq!(transport.requests().len(), sent, "no automatic call");
    assert_eq!(abort_retry::prompts_received(&receiver), 0, "nothing delivered");
}

/// A request-level error this host does not treat specially keeps today's
/// handling: no automatic schedule and control not disabled.
#[test]
fn ordinary_remote_errors_keep_their_existing_handling() {
    for code in ["storage_error", "fixture_future_code"] {
        let (state, session, receiver, _transport) =
            root_fixture([bind_reply("startup-token"), remote_error_reply(code)]);
        let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
        assert!(receiver.try_recv().is_err(), "{code}: nothing delivered");
        with_record(&state, &session, |record| {
            assert!(record.engram.admission_retry.is_none(), "{code}: not scheduled");
            assert!(record.engram.disabled_reason.is_none(), "{code}: not fatal");
        });
    }
}
