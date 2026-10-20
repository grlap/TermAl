// Owns the test that a rebind's recovery checkpoint, which closes a grant
// whose closing checkpoint was lost, settles the batch gates that grant's
// report credited (src/engram_batch_binds.rs, `owe_engram_batch_binds`), as
// the ordinary closing checkpoint does.
// Does not own the ordinary closing path (src/tests/engram_batch_gate_flow.rs)
// or the recovery checkpoint itself (src/tests/engram_turn_observations.rs,
// whose mediated-turn fixture this child module uses). New module.
use super::*;

#[test]
fn a_rebinds_recovery_checkpoint_settles_the_batch_gates_its_grant_credited() {
    let turn = start_mediated_turn_with_script(
        "batch-recovered",
        ChildWorkspace::IsolatedWorktree,
        ControlBinding::Claimed,
        CheckpointScript::LostThenClosedByRebind,
    );
    turn.change_source("src/lib.rs");
    // A batch gate this grant's closing report credited, pending its answer.
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&turn.child_id).expect("child should exist");
        inner.sessions[index].engram.pending_batches.push(EngramPendingBatch {
            checkpoint_grant_id: turn.grant_id.clone(),
            producer_observation_id: "9".repeat(64),
            measurement: EngramBindMeasurement {
                workspace_id: "workspace".to_owned(),
                source_revision: "content-v1:batch".to_owned(),
                measured_at: "2026-10-18T23:30:00.000Z".to_owned(),
            },
            targets: vec![EngramBindTarget {
                work_id: "work-member".to_owned(),
                short_ref: "w-member".to_owned(),
                claim_id: "claim-member".to_owned(),
                claim_fence: 1,
                root_generation: 1,
            }],
            pended_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        });
    }
    let pending = || {
        let inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&turn.child_id).expect("child should exist");
        inner.sessions[index].engram.pending_batches.len()
    };
    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.child_id, &turn.runtime_token)
        .expect("turn completion should tolerate the lost checkpoint");
    assert_eq!(pending(), 1, "a lost answer is no answer: the batch still waits");
    turn.let_bind_retry_elapse();

    queue_test_engram_prompt(
        &turn.state,
        &turn.child_id,
        "Continue after the lost close.",
        QueuedPromptSource::User,
        None,
    );
    let dispatch = turn
        .state
        .start_next_queued_turn_off_lock(&turn.child_id, false, false)
        .expect("queue inspection should succeed")
        .expect("the queued prompt should rebind and dispatch")
        .dispatch;
    deliver_turn_dispatch(&turn.state, dispatch)
        .expect("the rebound prompt should reach the runtime");

    assert_eq!(turn.checkpoints().len(), 2, "the lost close and the recovery close");
    // The recovery's answer settled the batch: its report carries no record
    // of that gate, so the member is told and nothing is owed.
    assert_eq!(pending(), 0, "the recovery close settles the batch it answered");
    let owed = {
        let inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&turn.child_id).expect("child should exist");
        inner.sessions[index].engram.owed_binds.len()
    };
    assert_eq!(owed, 0);
}
