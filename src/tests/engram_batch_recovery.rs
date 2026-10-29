// Owns original-result recovery with uncredited and independent pending
// batch markers. Exact credited-batch restart/receipt conversion is covered
// by engram_checkpoint_result_effects.rs using a real mediated Test producer.
// Does not own the ordinary closing path (src/tests/engram_batch_gate_flow.rs)
// or the recovery checkpoint itself (src/tests/engram_turn_observations.rs,
// whose mediated-turn fixture this child module uses). New module.
use super::*;

#[test]
fn original_checkpoint_recovery_preserves_uncredited_and_independent_batch_markers() {
    let turn = start_mediated_turn_with_script(
        "batch-recovered",
        ChildWorkspace::IsolatedWorktree,
        ControlBinding::Claimed,
        CheckpointScript::LostThenClosedByRebind,
    );
    turn.change_source("src/lib.rs");
    // These markers have no producer/Test input in the closing report. A
    // matching grant alone must never consume them or invent gate credit.
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
        let mut independent = inner.sessions[index].engram.pending_batches[0].clone();
        independent.checkpoint_grant_id = "independent-batch-grant".to_owned();
        independent.producer_observation_id = "8".repeat(64);
        inner.sessions[index].engram.pending_batches.push(independent);
    }
    let pending = || {
        let inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&turn.child_id).expect("child should exist");
        inner.sessions[index].engram.pending_batches.len()
    };
    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.child_id, &turn.runtime_token)
        .expect("turn completion should tolerate the lost checkpoint");
    assert_eq!(pending(), 2, "both uncredited markers still wait");
    let first = turn.checkpoints();
    assert_eq!(first.len(), 1);
    let (original, batch_debt) = {
        let inner = turn.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&turn.child_id).unwrap()];
        (
            assert_unknown_checkpoint_custody(&inner, &turn.child_id, &first[0], None),
            serde_json::to_value(&record.engram.pending_batches).unwrap(),
        )
    };
    let authority_requests = checkpoint_and_admission_requests(&turn.transport);
    let operation: EngramCheckpointOperation = serde_json::from_value(original.clone()).unwrap();
    let EngramControlRequest::TurnCheckpoint { report, .. } = &operation.original.request else { unreachable!() };
    assert!(!report.observations.iter().any(|producer| producer.observation_id == "9".repeat(64)));
    assert!(report.verification_evidence.is_empty());
    turn.transport.work_bindings.lock().unwrap().extend(
        (0..3).map(|_| Ok(operation.original.work_binding.clone())));
    turn.let_bind_retry_elapse();

    queue_test_engram_prompt(
        &turn.state,
        &turn.child_id,
        "Continue after the lost close.",
        QueuedPromptSource::User,
        None,
    );
    assert_unknown_checkpoint_custody_after_reload(&turn.state, &turn.child_id, &first[0], &original);
    let now = chrono::Utc::now();
    turn.state.engram_abort_retry_tick(now);
    turn.state.engram_abort_retry_tick(now + chrono::Duration::seconds(3));
    assert_original_checkpoint_recovered_after_reload(&turn.state, &original);
    assert_eq!(checkpoint_and_admission_requests(&turn.transport),
        authority_requests.iter().cloned().chain([first[0].clone()]).collect::<Vec<_>>());
    let dispatch = turn
        .state
        .start_next_queued_turn_off_lock(&turn.child_id, false, false)
        .expect("queue inspection should succeed")
        .expect("the queued prompt can dispatch only after original-result settlement")
        .dispatch;
    assert!(matches!(turn._runtime_rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));

    assert_eq!(turn.checkpoints(), [first[0].clone(), first[0].clone()], "the original close is not replaced");
    assert_original_checkpoint_recovered_after_reload(&turn.state, &original);
    let requests = checkpoint_and_admission_requests(&turn.transport);
    let expected = authority_requests.into_iter().chain([first[0].clone()]).collect::<Vec<_>>();
    assert_eq!(&requests[..expected.len()], expected.as_slice(), "original settlement precedes fresh admission");
    assert_eq!(requests[expected.len()..].iter().map(|request| request["operation"].as_str().unwrap()).collect::<Vec<_>>(),
        ["session_bind", "turn_evaluate"], "Begin belongs to dispatch delivery");
    assert_eq!(pending(), 2, "the exact original never credited either marker");
    let owed = {
        let inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&turn.child_id).expect("child should exist");
        assert_eq!(serde_json::to_value(&inner.sessions[index].engram.pending_batches).unwrap(),
            batch_debt, "unmatched and independent markers remain intact live");
        inner.sessions[index].engram.owed_binds.len()
    };
    assert_eq!(owed, 0);
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    let row: String = connection.query_row("SELECT value_json FROM sessions WHERE id=?1",
        [&turn.child_id], |row| row.get(0)).unwrap();
    let persisted: Value = serde_json::from_str(&row).unwrap();
    assert_eq!(persisted["engramPendingBatches"], batch_debt);
    drop(connection);
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&turn.child_id).unwrap()];
    assert_eq!(batch_debt.as_array().unwrap().len(), 2);
    // Neither marker has canonical producer provenance, so both retain the
    // existing lost-credit policy on whole-state load.
    assert!(record.engram.pending_batches.is_empty());
    let lost_credit = engram_batch_member_lost_line("w-member",
        "before Engram's answer named the batch gate's record");
    assert!(record.engram.pending_source_root_line.as_deref().unwrap().contains(&lost_credit));
    assert!(record.engram.owed_binds.is_empty());
    deliver_turn_dispatch(&turn.state, dispatch).expect("the new prompt reaches its runtime after settlement");
    assert!(matches!(receive(&turn._runtime_rx, "runtime should receive the new prompt"),
        CodexRuntimeCommand::Prompt { .. }));
    assert!(matches!(turn._runtime_rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
        "only one new prompt reaches the provider");
    let delivered = checkpoint_and_admission_requests(&turn.transport);
    assert_eq!(&delivered[..expected.len()], expected.as_slice());
    assert_eq!(delivered[expected.len()..].iter().map(|request| request["operation"].as_str().unwrap()).collect::<Vec<_>>(),
        ["session_bind", "turn_evaluate", "turn_begin"]);
}
