//! Refusal healing on a re-fenced binding. A Begin refused
//! grant_scope_mismatch (its grant proven retired) is healed by a re-evaluate
//! that reuses the session's binding; when that binding has been re-fenced,
//! for example after a long park, the re-evaluate is refused stale_fence. The
//! host then rebinds once within the remaining admission budget and
//! re-evaluates once more under its own key. A second stale_fence, or a budget
//! that runs out during the rebind, holds the head instead of ending it.
//!
//! Owns these witnesses only, on the root fixture's scripted transport and
//! clock with no synthetic load. Does not own the healthy healing sequence
//! (`engram_phase_compat.rs`) or the stale_fence-first path. New module beside
//! the root dispatch tests, created instead of growing them.

use super::*;

fn begin_refusal(code: &str) -> ScriptedEngramControlResponse {
    ScriptedEngramControlResponse::Reply(Ok(json!({ "decision": "refuse", "code": code })))
}

fn stale_fence_refusal() -> ScriptedEngramControlResponse {
    ScriptedEngramControlResponse::Reply(Ok(json!({
        "decision": "refuse",
        "directive": {
            "directive_id": "directive-stale_fence",
            "code": "stale_fence",
            "target": "host",
            "satisfaction": "rebind"
        }
    })))
}

/// The healing up to its re-evaluate on the reused binding: a grant, its
/// Begin refused grant_scope_mismatch, a status proving the grant retired, and
/// the re-evaluate refused stale_fence because the binding was re-fenced.
fn healing_refused_on_a_reused_binding() -> Vec<ScriptedEngramControlResponse> {
    vec![
        bind_reply("parked-token"),
        grant_reply("retired-grant"),
        begin_refusal("grant_scope_mismatch"),
        status_reply("ready"),
        stale_fence_refusal(),
    ]
}

/// The healing rebind, as every rebind runs: a status read on the old routing
/// token (a clean session, no open grant), then the fresh bind.
fn healing_rebind(token: &str) -> [ScriptedEngramControlResponse; 2] {
    [status_reply("ready"), bind_reply(token)]
}

fn evaluates(requests: &[RecordedEngramControlRequest]) -> Vec<Value> {
    requests
        .iter()
        .filter(|recorded| recorded.request["operation"] == "turn_evaluate")
        .map(|recorded| recorded.request.clone())
        .collect()
}

fn prompts_delivered(receiver: &mpsc::Receiver<CodexRuntimeCommand>) -> usize {
    receiver
        .try_iter()
        .filter(|command| matches!(command, CodexRuntimeCommand::Prompt { .. }))
        .count()
}

/// The head's hold after the dispatch: whether it is retained and
/// interrupted, its retry record, and the newest card's decision and code.
fn hold(
    state: &AppState,
    session: &str,
) -> (
    usize,
    bool,
    Option<EngramAdmissionRetry>,
    Option<(EngramControlCardDecision, Option<String>)>,
    String,
) {
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    let card = record.session.messages.iter().rev().find_map(|message| match message {
        Message::EngramControl { card, .. } => Some((card.decision, card.refusal_code.clone())),
        _ => None,
    });
    (
        record.queued_prompts.len(),
        record
            .queued_prompts
            .front()
            .is_some_and(|head| head.engram_interrupted),
        record.engram.admission_retry.clone(),
        card,
        record.session.preview.clone(),
    )
}

/// The healing re-evaluate on the reused binding is refused stale_fence: the
/// host rebinds once and re-evaluates once more, under the fresh binding and
/// its own key, within the same admission, and the head is delivered exactly
/// once without a Resume.
#[test]
fn a_healing_reevaluate_refused_stale_fence_is_rebound_once_and_delivered() {
    let mut replies = healing_refused_on_a_reused_binding();
    replies.extend(healing_rebind("fresh-token"));
    replies.extend([grant_reply("fresh-grant"), begin_reply("fresh-grant")]);
    let (state, session, receiver, transport) = root_fixture(replies);
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));

    assert_eq!(
        operations(&transport),
        [
            "session_bind",
            "turn_evaluate",
            "turn_begin",
            "session_status",
            "turn_evaluate",
            "session_status",
            "session_bind",
            "turn_evaluate",
            "turn_begin",
        ],
        "one rebind and one further re-evaluate, in the same admission"
    );
    let evaluates = evaluates(&transport.requests());
    let keys: Vec<&str> = evaluates
        .iter()
        .map(|evaluate| evaluate["idempotency_key"].as_str().unwrap())
        .collect();
    assert!(keys[1].starts_with("termal-reevaluate:"), "{keys:?}");
    assert!(keys[2].starts_with("termal-stale-reevaluate:"), "its own key: {keys:?}");
    assert_eq!(evaluates[1]["routing_token"], "parked-token", "the reused binding");
    assert_eq!(evaluates[2]["routing_token"], "fresh-token", "the fresh binding");
    assert_eq!(prompts_delivered(&receiver), 1, "delivered exactly once, without Resume");
}

/// After the healing rebind, a second stale_fence ends no further rebind and
/// no loop: the head is held, retained and interrupted, with a card naming
/// stale_fence, and nothing is delivered.
#[test]
fn a_second_stale_fence_after_the_healing_rebind_holds_the_head() {
    let mut replies = healing_refused_on_a_reused_binding();
    replies.extend(healing_rebind("fresh-token"));
    replies.push(stale_fence_refusal());
    let (state, session, receiver, transport) = root_fixture(replies);
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));

    let operations = operations(&transport);
    assert_eq!(
        operations,
        [
            "session_bind",
            "turn_evaluate",
            "turn_begin",
            "session_status",
            "turn_evaluate",
            "session_status",
            "session_bind",
            "turn_evaluate",
        ],
        "exactly one healing rebind, no loop"
    );
    let (retained, interrupted, retry, card, preview) = hold(&state, &session);
    assert_eq!(retained, 1, "the head is retained, not ended");
    assert_eq!(
        card,
        Some((EngramControlCardDecision::Degraded, Some("stale_fence".to_owned()))),
        "a hold naming the code, not a refusal"
    );
    assert!(interrupted, "an explicit hold to reconcile");
    assert!(retry.is_none(), "no automatic retry loop");
    assert!(preview.contains("cancel or reconcile"), "{preview}");
    assert_eq!(prompts_delivered(&receiver), 0);
}

/// The admission budget runs out while the healing rebind is in flight: no
/// further re-evaluate goes out, and the head is held, retained, with a card
/// naming the budget's code.
#[test]
fn a_budget_spent_during_the_healing_rebind_holds_the_head() {
    let (state, session, receiver, _) = root_fixture([]);
    let (rebind, rebind_gate) = gated_engram_step("session_bind", bind_reply("fresh-token"));
    let mut steps: Vec<GatedEngramControlStep> = healing_refused_on_a_reused_binding()
        .into_iter()
        .zip([
            "session_bind",
            "turn_evaluate",
            "turn_begin",
            "session_status",
            "turn_evaluate",
        ])
        .map(|(reply, operation)| immediate_engram_step(operation, reply))
        .collect();
    steps.push(immediate_engram_step("session_status", status_reply("ready")));
    steps.push(rebind);
    let transport = GatedEngramControlTransport::new(steps);
    state.install_control_test_transport(transport.clone());
    let clock = state.engram_budget_clock();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
        });
        rebind_gate.wait();
        // The whole admission budget passes while the rebind is answered.
        clock.advance(Duration::from_millis(ENGRAM_DISPATCH_BUDGET_MS + 1));
        rebind_gate.release();
        worker.join().unwrap();
    });

    let operations: Vec<String> = transport
        .requests()
        .iter()
        .map(|recorded| recorded.request["operation"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        operations,
        [
            "session_bind",
            "turn_evaluate",
            "turn_begin",
            "session_status",
            "turn_evaluate",
            "session_status",
            "session_bind",
        ],
        "no re-evaluate after the budget ran out"
    );
    let (retained, _, _, card, _) = hold(&state, &session);
    assert_eq!(retained, 1, "the head is retained, not ended");
    // The budget runs out inside the rebind, so the rebind itself fails on
    // its deadline, as a rebind of the stale_fence path would, and the hold
    // names that code.
    assert_eq!(
        card,
        Some((
            EngramControlCardDecision::Degraded,
            Some("deadline_exceeded".to_owned())
        )),
        "a hold naming the budget's code"
    );
    assert_eq!(prompts_delivered(&receiver), 0);
}
