// Owns the regression for the named-root guard right before Begin: when its
// authority fence stalls, the guard must fail while Begin's share of the
// shared admission budget is still left, with Begin unsent, instead of
// spending the whole budget. Drives the ClaimedRoot fixture on its scripted
// budget clock with a gated persist writer; no real load. Does not own the
// fence deadline diagnostics (src/tests/engram_authority_fence_phases.rs)
// or Begin's own call bound (src/engram_begin_replay.rs). New file.
use super::*;

/// The guard before Begin sends its Prepared authority fence after the
/// grant: earlier Prepared fences belong to the guards before bind.
fn is_begin_guard_fence(claimed: &ClaimedRoot, fence: &PersistFence) -> bool {
    let PersistFenceTarget::EngramWorkAuthority(image) = &fence.target else {
        return false;
    };
    image
        .history
        .transition
        .as_ref()
        .is_some_and(|owner| owner.phase == EngramAuthorityPhase::Prepared)
        && claimed
            .transport
            .requests()
            .iter()
            .any(|request| request.request["operation"] == "turn_evaluate")
}

#[test]
fn begin_guard_failure_leaves_begins_share_of_the_admission_budget() {
    // The production budget is twice Begin's floor, so the share is the floor.
    let left = stall_the_begin_guard("begin-guard-reserve", None);
    assert_eq!(
        left,
        Duration::from_millis(ENGRAM_BEGIN_MIN_CALL_TIMEOUT_MS)
    );
}

#[test]
fn begin_guard_on_a_short_budget_leaves_begin_half_of_it() {
    // Below twice Begin's floor, the guard and Begin split the budget.
    let budget = Duration::from_millis(ENGRAM_BEGIN_MIN_CALL_TIMEOUT_MS * 3 / 2);
    let left = stall_the_begin_guard("begin-guard-short-budget", Some(budget));
    assert_eq!(left, budget / 2);
}

/// Stalls the guard before Begin until its fence's deadline and returns the
/// admission budget still left at that moment, after checking that the
/// guard failed with Begin unsent and at least Begin's share left.
fn stall_the_begin_guard(label: &str, budget: Option<Duration>) -> Duration {
    let grant = format!("{label}-grant");
    let mut claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind_reply(&format!("{label}-token")),
            grant_reply(&grant),
            begin_reply(&grant),
            checkpoint_reply(&grant),
        ],
    );
    prepare_confirmed_claimed_opening(&claimed, label);
    if budget.is_some() {
        claimed
            .state
            .inner
            .lock()
            .unwrap()
            .test_engram_dispatch_budget = budget;
    }
    let clock = claimed.state.engram_budget_clock();
    let budget = claimed
        .state
        .inner
        .lock()
        .unwrap()
        .test_engram_dispatch_budget
        .unwrap_or(Duration::from_millis(ENGRAM_DISPATCH_BUDGET_MS));
    // The scripted clock moves only when this test advances it, so the
    // admission budget starts at this instant.
    let started = clock.now();
    let (tx, rx) = std::sync::mpsc::channel();
    claimed.state.persist_tx = tx;
    let worker = claimed.state.clone();
    let session = claimed.session_id.clone();
    let task = std::thread::spawn(move || {
        let dispatch = match worker
            .dispatch_turn(
                &session,
                SendMessageRequest {
                    text: "Continue while the guard's fence stalls.".to_owned(),
                    expanded_text: None,
                    attachments: Vec::new(),
                    source_session_id: None,
                    source_mailbox: None,
                },
            )
            .unwrap()
        {
            DispatchTurnResult::Dispatched(dispatch)
            | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
            DispatchTurnResult::Queued => panic!("idle fixture must dispatch"),
        };
        deliver_turn_dispatch(&worker, dispatch)
    });
    let mut batch = PersistFenceBatch::default();
    let mut cache = SqlitePersistConnectionCache::new();
    let mut stalled = None;
    while !task.is_finished() {
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(PersistRequest::Fence(fence))
                if stalled.is_none() && is_begin_guard_fence(&claimed, &fence) =>
            {
                // The writer never reaches this fence before its deadline,
                // as on a saturated disk: run the clock out to it.
                let guard_deadline = fence.completion.deadline;
                clock.wait_for_scripted_waiter();
                clock.advance(guard_deadline.saturating_duration_since(clock.now()));
                stalled = Some((fence, guard_deadline));
            }
            Ok(PersistRequest::Fence(fence)) => {
                batch.accept(PersistRequest::Fence(fence));
                let delta = collect_persist_delta_from_shared_state(&claimed.state.inner, 0);
                persist_delta_with_fences(
                    &mut cache,
                    claimed.state.persistence_path.as_path(),
                    &delta,
                    &mut batch,
                )
                .unwrap();
            }
            Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(error) => panic!("writer fixture disconnected: {error}"),
        }
    }
    let outcome = task.join().unwrap();
    let (_fence, guard_deadline) =
        stalled.expect("the guard before Begin must send its authority fence");
    let left_for_begin = (started + budget).saturating_duration_since(guard_deadline);
    let begins_share = Duration::from_millis(ENGRAM_BEGIN_MIN_CALL_TIMEOUT_MS).min(budget / 2);
    assert!(
        left_for_begin >= begins_share,
        "the guard may not spend Begin's share: {left_for_begin:?} left of {budget:?}, \
         Begin needs {begins_share:?}"
    );
    assert!(
        !matches!(outcome, TurnDispatchDeliveryOutcome::Delivered),
        "a failed guard must hold the turn: {outcome:?}"
    );
    assert!(
        !claimed
            .transport
            .requests()
            .iter()
            .any(|request| request.request["operation"] == "turn_begin"),
        "Begin stays unsent when the guard before it fails"
    );
    left_for_begin
}
