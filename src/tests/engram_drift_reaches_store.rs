//! Continuity drift reaches the store before the no-edit turn's checkpoint:
//! a change between two named-root turns that the host records as drift is
//! sent as an accounted inter_turn_change before the next turn's provider
//! handoff, and the next turn's own checkpoint, which made no edit, reports
//! source_changed=false after it. The observation is a barrier: while its
//! answer is lost the turn is held and nothing reaches the runtime. With no
//! change between the turns, nothing is observed and continuity is unchanged.
//!
//! Owns these three witnesses only, on the named-root fixture of
//! `engram_turn_observations.rs` (scripted transport, real worktree, real
//! content revisions). Does not own the observation's request shape or its
//! baseline rules (that module and `engram_source_observations.rs`) or the
//! continuity comparison itself (`engram_turn_continuity.rs`). New module
//! beside the turn-observation tests, created instead of growing them.

use super::*;

/// Scripts the second admitted turn on the same claim; returns its grant
/// and the index its first request will have.
fn script_second_turn(claimed: &ClaimedRoot, label: &str) -> (String, usize) {
    let next_grant = format!("turn-observation-{label}-next-grant");
    claimed.transport.responses.lock().unwrap().extend([
        grant_reply(&next_grant),
        begin_reply(&next_grant),
        checkpoint_reply(&next_grant),
    ]);
    // The fixture scripts one binding read for its first turn; the second
    // admission reads the same still-held claim.
    let binding = test_control_work_binding(&format!("turn-observation-{label}"), 1);
    claimed.transport.work_bindings.lock().unwrap().push_back(Ok(Some(binding)));
    (next_grant, claimed.transport.requests().len())
}

/// Scripts and delivers the second admitted turn; returns its grant and the
/// index of its first request.
fn start_second_turn(claimed: &ClaimedRoot, label: &str) -> (String, usize) {
    let (next_grant, start) = script_second_turn(claimed, label);
    // Delivered inline, not scheduled on a worker. This window only bounds
    // the requests by the delivery's return; that the observation precedes
    // the runtime send itself is the barrier witness's to show.
    let delivery = deliver_turn_dispatch(&claimed.state, claimed.dispatch());
    assert!(matches!(delivery, TurnDispatchDeliveryOutcome::Delivered),
        "the next admitted turn reaches provider handoff: {delivery:?}");
    let _ = received_prompt(claimed);
    (next_grant, start)
}

fn canonical_worktree(worktree: &FsPath) -> PathBuf {
    fs::canonicalize(worktree).expect("the named worktree canonicalizes")
}

fn operation_index(requests: &[RecordedEngramControlRequest], from: usize, operation: &str, grant: Option<&str>) -> Option<usize> {
    (from..requests.len()).find(|index| {
        requests[*index].request["operation"] == operation
            && grant.is_none_or(|grant| requests[*index].request["grant_id"] == grant)
    })
}

#[test]
fn drift_between_named_root_turns_reaches_the_store_before_the_no_edit_turns_checkpoint() {
    let label = "drift-reaches-store";
    let (claimed, worktree, first_runtime) = named_root_turn(label, true);
    fs::write(worktree.join("README.md"), "changed during the first turn\n")
        .expect("first source change writes");
    let first = finish_claimed_turn(&claimed, &first_runtime);
    let previous = first["source_basis"]["source_revision"].clone();

    // A write between the turns, which no turn reports.
    fs::write(worktree.join("README.md"), "changed between the named turns\n")
        .expect("the inter-turn source change writes");
    let current = content_revision_of(&worktree);
    assert_ne!(previous, current);
    let (next_grant, second_start) = start_second_turn(&claimed, label);
    let at_handoff = claimed.transport.requests().len();

    // The host records drift for the next turn, its own begin unchanged.
    let (continuity, begin) = claimed.record(|record| {
        (record.engram.active_turn_continuity.clone(), record.engram.active_turn_start_basis.clone())
    });
    let Some((continuity_grant, EngramTurnContinuity::Drifted {
        previous_grant_id, workspace_id, previous_revision, begin_revision, ..
    })) = continuity.clone() else {
        panic!("the host records continuity drift for the next turn: {continuity:?}");
    };
    assert_eq!(continuity_grant, next_grant);
    assert_eq!(previous_grant_id, format!("turn-observation-{label}-grant"), "measured from the first turn's checkpoint");
    assert_eq!(PathBuf::from(&workspace_id), canonical_worktree(&worktree), "in the named worktree");
    assert_eq!(Value::String(previous_revision), previous, "drift starts at the previous checkpoint's end");
    assert_eq!(begin_revision, current);
    assert_eq!(begin.expect("the next turn's begin basis").source_revision, current);

    // The store accounted the drift as a source change before the handoff.
    let saved = load_state(claimed.state.persistence_path.as_path()).unwrap().unwrap();
    let intent = saved.engram_source_sightings.iter()
        .flat_map(|owner| &owner.observations)
        .find(|intent| intent.grant_id == next_grant)
        .expect("the drift's durable observation exists at handoff");
    let EngramSourceObservationPhase::Recorded { request, receipt } = &intent.phase else {
        panic!("the drift is recorded before the handoff");
    };
    assert_eq!(request["occurrence"]["kind"], "inter_turn_change");
    assert_eq!(request["occurrence"]["source_change"]["baseline"]["source_revision"], previous);
    assert_eq!(request["occurrence"]["source_change"]["sighting"]["source_basis"]["source_revision"], current);
    assert_eq!(receipt["accounting"]["kind"], "source_change");
    assert_eq!(receipt["accounting"]["source_change"], receipt["observation"], "accounted as its own change");

    claimed.state
        .finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token())
        .expect("the no-edit turn closes");
    assert_eq!(content_revision_of(&worktree), current, "the next turn made no edit");
    let requests = claimed.transport.requests();
    let observe = operation_index(&requests, second_start, "execution_observe", None)
        .expect("the drift is sent to the store");
    assert_eq!(requests[observe].request["idempotency_key"], request["idempotency_key"],
        "the observation on the wire is the drift's recorded one");
    assert_eq!(
        requests[second_start..at_handoff].iter()
            .filter(|sent| sent.request["operation"] == "execution_observe").count(),
        1,
        "one observation before the handoff"
    );
    let checkpoint = operation_index(&requests, second_start, "turn_checkpoint", Some(&next_grant))
        .expect("the no-edit turn checkpoints");
    assert!(observe < at_handoff && at_handoff <= checkpoint,
        "inter_turn_change ({observe}), then the provider handoff ({at_handoff}), then the checkpoint ({checkpoint})");
    let report = &requests[checkpoint].request["observations"][0];
    assert_eq!(report["source_changed"], false, "the no-edit turn reports no change of its own");
    assert_eq!(report["source_basis"]["source_revision"], current);
}

#[test]
fn no_change_between_named_root_turns_sends_no_observation() {
    let label = "no-drift-no-store";
    let (claimed, worktree, first_runtime) = named_root_turn(label, true);
    fs::write(worktree.join("README.md"), "changed during the first turn\n")
        .expect("first source change writes");
    let first = finish_claimed_turn(&claimed, &first_runtime);
    let previous = first["source_basis"]["source_revision"].clone();
    assert_eq!(Value::String(content_revision_of(&worktree)), previous, "nothing changed between the turns");

    let (next_grant, second_start) = start_second_turn(&claimed, label);
    let continuity = claimed.record(|record| record.engram.active_turn_continuity.clone());
    let first_grant = format!("turn-observation-{label}-grant");
    assert!(
        matches!(&continuity, Some((grant, EngramTurnContinuity::Unchanged { previous_grant_id }))
            if *grant == next_grant && *previous_grant_id == first_grant),
        "the host records no drift from the first turn's checkpoint: {continuity:?}"
    );
    let saved = load_state(claimed.state.persistence_path.as_path()).unwrap().unwrap();
    assert!(
        !saved.engram_source_sightings.iter()
            .flat_map(|owner| &owner.observations)
            .any(|intent| intent.grant_id == next_grant),
        "no observation is prepared for the next turn, sent or not"
    );
    claimed.state
        .finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token())
        .expect("the no-edit turn closes");
    let requests = claimed.transport.requests();
    assert_eq!(operation_index(&requests, second_start, "execution_observe", None), None,
        "no inter-turn observation without a change");
    let checkpoint = operation_index(&requests, second_start, "turn_checkpoint", Some(&next_grant))
        .expect("the no-edit turn checkpoints");
    assert_eq!(requests[checkpoint].request["observations"][0]["source_changed"], false);
}

/// The drift observation is a barrier before the provider handoff, not a
/// record taken beside it: while Engram's answer for it is lost, the next
/// turn is held and nothing reaches the runtime, although the request was
/// sent. Only when the retry replays the identical observation and it is
/// accounted is the prompt handed to the runtime. A delivery that sent the
/// prompt first and recorded the drift afterwards would hand it over here,
/// in the held attempt.
#[test]
fn the_drift_observation_gates_the_provider_handoff() {
    let label = "drift-gates-handoff";
    let (claimed, worktree, first_runtime) = named_root_turn(label, true);
    fs::write(worktree.join("README.md"), "changed during the first turn\n")
        .expect("first source change writes");
    let _ = finish_claimed_turn(&claimed, &first_runtime);
    fs::write(worktree.join("README.md"), "changed between the named turns\n")
        .expect("the inter-turn source change writes");

    let (next_grant, second_start) = script_second_turn(&claimed, label);
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
        .lose_next_observation_reply = true;
    let delivery = deliver_turn_dispatch(&claimed.state, claimed.dispatch());
    assert!(matches!(delivery, TurnDispatchDeliveryOutcome::Held { .. }),
        "an unaccounted drift observation holds the turn: {delivery:?}");
    assert!(claimed.runtime_rx.try_recv().is_err(), "nothing reaches the runtime before the drift is accounted");
    let held = claimed.transport.requests();
    let sent = operation_index(&held, second_start, "execution_observe", None)
        .expect("the drift was sent before the hold");
    let key = held[sent].request["idempotency_key"].clone();
    claimed.record(|record| {
        assert_eq!(record.engram.active_grant_id.as_deref(), Some(next_grant.as_str()));
        assert!(!engram_source_observation_can_deliver(record), "the barrier is still raised");
    });

    claimed.state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::seconds(2));
    let _ = received_prompt(&claimed);
    let requests = claimed.transport.requests();
    let replays: Vec<_> = (second_start..requests.len())
        .filter(|index| requests[*index].request["operation"] == "execution_observe")
        .collect();
    assert_eq!(replays.len(), 2, "the held observation and its one replay: {replays:?}");
    assert_eq!(requests[replays[1]].request, requests[replays[0]].request, "the replay is the identical request");
    assert_eq!(requests[replays[1]].request["idempotency_key"], key);
    let saved = load_state(claimed.state.persistence_path.as_path()).unwrap().unwrap();
    let intent = saved.engram_source_sightings.iter()
        .flat_map(|owner| &owner.observations)
        .find(|intent| intent.grant_id == next_grant)
        .expect("the drift's durable observation exists at handoff");
    assert!(matches!(&intent.phase, EngramSourceObservationPhase::Recorded { receipt, .. }
        if receipt["accounting"]["kind"] == "source_change"
            && receipt["accounting"]["source_change"] == receipt["observation"]),
        "accounted before the prompt was handed over");
}
