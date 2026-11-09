//! Ordinary queue and dedicated-runtime safety beside passive retry history.
//! The removed control retry scheduler, budget and cap are not reconstructed.
//! Windows witnesses still use the actual production process-tree fixture.
use super::*;
use crate::tests::engram_host_adapter::uncertain_begin::OrdinaryRoot;

#[cfg(windows)]
fn dedicated_ordinary_fixture(
    acp: bool,
) -> (
    OrdinaryRoot,
    crate::tests::delegation_process_cleanup::ProductionWorkerTree,
) {
    let mut turn = OrdinaryRoot::new("ordinary-dedicated-owner");
    turn.state.agent_runtime_spawning_enabled = true;
    turn.edit(|record| {
        record.session.agent = if acp { Agent::Kimi } else { Agent::Claude };
        record.session.model = record.session.agent.default_model().to_owned();
    });
    let cwd = turn.root.join("dedicated-producer");
    fs::create_dir_all(&cwd).unwrap();
    let owner = crate::tests::delegation_process_cleanup::ProductionWorkerTree::spawn(
        &turn.state,
        &turn.session,
        &cwd,
        acp,
    );
    turn.edit(|record| record.session.status = SessionStatus::Idle);
    (turn, owner)
}

#[cfg(windows)]
#[test]
fn dedicated_model_reset_preserves_the_ordinary_paused_queue_off_lock() {
    let (turn, owner) = dedicated_ordinary_fixture(false);
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let id = turn.enqueue("Paused through model reset.", QueuedPromptSource::User);
    let before = turn.record(|record| serde_json::to_value(&record.queued_prompts[0]).unwrap());
    let (claimed_tx, claimed_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *owner.tree.cleanup_gate.lock().unwrap() = Some(TestStopFenceGate {
        claimed_tx,
        release_rx,
    });
    let worker_state = turn.state.clone();
    let worker_session = turn.session.clone();
    let worker = std::thread::spawn(move || {
        worker_state.prepare_dedicated_runtime_reset_off_lock(&worker_session, true)
    });
    phase_sync::receive(&claimed_rx, "model reset owns dedicated cleanup off lock");
    assert!(turn.state.inner.inner.try_lock().is_ok());
    assert!(
        turn.state
            .dispatch_next_queued_turn(&turn.session, false)
            .unwrap()
            .is_none()
    );
    turn.record(|record| {
        assert_eq!(record.queued_prompts[0].pending_prompt.id, id);
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            before
        );
    });
    release_tx.send(()).unwrap();
    let reset = worker.join().unwrap();
    owner.await_exit();
    let confirmed = owner.tree.is_confirmed();
    owner.assert_peers_exited();
    drop(owner);
    reset.unwrap();
    assert!(confirmed);
    turn.record(|record| {
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            before
        )
    });
    turn.assert_no_control();
}

#[cfg(windows)]
fn dedicated_stop_preserves_pending_work(acp: bool) {
    let (turn, owner) = dedicated_ordinary_fixture(acp);
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let id = turn.enqueue(
        "Never handed to the dedicated runtime.",
        QueuedPromptSource::User,
    );
    owner
        .tree
        .termination_failures
        .store(1, std::sync::atomic::Ordering::SeqCst);
    owner.exit_root();
    assert!(turn.record(|record| record.runtime.dedicated_cleanup_failure().is_some()));
    turn.state.request_stop_session(&turn.session).unwrap();
    let guard = phase_sync::PollGuard::new();
    while !turn.record(|record| matches!(record.runtime, SessionRuntime::None)) {
        guard.wait("public Stop disposes its exact unresolved dedicated owner");
    }
    let confirmed = owner.tree.is_confirmed();
    owner.assert_peers_exited();
    drop(owner);
    assert!(confirmed);
    turn.record(|record| {
        assert_eq!(record.queued_prompts[0].pending_prompt.id, id);
        assert!(record.orchestrator_auto_dispatch_blocked);
    });
    turn.assert_no_control();
}

#[cfg(windows)]
#[test]
fn dedicated_stop_cleans_claude_and_keeps_its_unsent_ordinary_head() {
    dedicated_stop_preserves_pending_work(false);
}
#[cfg(windows)]
#[test]
fn dedicated_stop_cleans_acp_and_keeps_its_unsent_ordinary_head() {
    dedicated_stop_preserves_pending_work(true);
}

#[cfg(windows)]
#[test]
fn dedicated_acp_initialize_failure_still_kills_the_actual_process_tree() {
    let (turn, owner) = dedicated_ordinary_fixture(true);
    // Disconnecting the pending initialize applies the writer's real failure
    // cleanup; it is not a measurement of a wall-clock deadline.
    owner.expire_initialize();
    owner.await_exit();
    assert!(owner.tree.is_confirmed());
    owner.assert_peers_exited();
    drop(owner);
    turn.assert_no_control();
}

#[cfg(windows)]
#[test]
fn dedicated_stop_needs_no_historical_wire_intent_to_finish_cleanup() {
    dedicated_stop_preserves_pending_work(false);
}

#[cfg(windows)]
fn dedicated_stop_retires_promoted_ordinary_work(history: Option<&str>) {
    let (turn, owner) = dedicated_ordinary_fixture(false);
    let id = turn.enqueue(
        "Prepared ordinary dedicated turn.",
        QueuedPromptSource::User,
    );
    if let Some(kind) = history {
        turn.edit(|record| {
            record.queued_prompts[0].engram_evaluate = Some(json!({"historical":kind,
                "prepared_begin":{"idempotency_key":"saved-begin-key"},"begun_grant_id":"old-grant"}));
            record.engram.historical_control.insert("engramActiveGrantId".into(), json!("old-grant"));
        });
    }
    let dispatch = turn
        .state
        .start_next_queued_turn_off_lock(&turn.session, false, false)
        .unwrap()
        .unwrap()
        .dispatch;
    turn.record(|record| assert_eq!(record.session.status, SessionStatus::Active));
    // This dispatch is deliberately never handed to the provider.
    turn.state.stop_session(&turn.session).unwrap();
    owner.await_exit();
    assert!(owner.tree.is_confirmed());
    owner.assert_peers_exited();
    drop(owner);
    drop(dispatch);
    turn.record(|record| {
        assert!(
            !record
                .queued_prompts
                .iter()
                .any(|entry| entry.pending_prompt.id == id)
        );
        if history.is_some() {
            assert_eq!(
                record.engram.historical_control["engramActiveGrantId"],
                "old-grant"
            );
        }
    });
    turn.assert_no_control();
}

#[cfg(windows)]
#[test]
fn dedicated_ordinary_stop_retires_its_prepared_turn() {
    dedicated_stop_retires_promoted_ordinary_work(None);
}
#[cfg(windows)]
#[test]
fn dedicated_ordinary_stop_retires_its_head_beside_saved_begin_history() {
    dedicated_stop_retires_promoted_ordinary_work(Some("saved-begin"));
}
#[cfg(windows)]
#[test]
fn dedicated_ordinary_stop_preserves_passive_grant_bytes_without_checkpointing() {
    dedicated_stop_retires_promoted_ordinary_work(Some("active-grant"));
}

#[cfg(windows)]
fn ordinary_drain_waits_for_dedicated_cleanup(predecessor: bool) {
    let (turn, owner) = dedicated_ordinary_fixture(false);
    let id = turn.enqueue(
        "Wait for the actual cleanup owner.",
        QueuedPromptSource::User,
    );
    owner
        .tree
        .termination_failures
        .store(1, std::sync::atomic::Ordering::SeqCst);
    owner.exit_root();
    turn.edit(|record| {
        if predecessor {
            record.runtime = SessionRuntime::None;
        }
    });
    owner
        .tree
        .termination_failures
        .store(1, std::sync::atomic::Ordering::SeqCst);
    let before = turn.record(|record| serde_json::to_value(&record.queued_prompts[0]).unwrap());
    let result = turn.state.dispatch_next_queued_turn(&turn.session, false);
    turn.record(|record| {
        assert_eq!(record.queued_prompts[0].pending_prompt.id, id);
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            before
        );
    });
    assert!(
        !matches!(result, Ok(Some(_))),
        "cleanup uncertainty cannot prepare a provider turn"
    );
    turn.assert_no_control();
    let was_unconfirmed = !owner.tree.is_confirmed();
    let retained = turn.record(|record| record.retained_dedicated_owners[0].clone());
    // Explicit fixture cleanup follows every observation. A paused drain may
    // leave the injected failure unused; that failure must not break teardown
    // or be mistaken for production cleanup success.
    owner
        .tree
        .termination_failures
        .store(0, std::sync::atomic::Ordering::SeqCst);
    retained
        .tree
        .terminate(&retained.process, retained.label)
        .unwrap();
    owner.assert_peers_exited();
    turn.edit(|record| record.clear_runtime());
    assert!(owner.tree.is_confirmed());
    drop(owner);
    assert!(
        was_unconfirmed,
        "the held production drain cannot claim confirmed cleanup"
    );
}

#[cfg(windows)]
#[test]
fn ordinary_drain_waits_for_the_current_dedicated_cleanup() {
    ordinary_drain_waits_for_dedicated_cleanup(false);
}
#[cfg(windows)]
#[test]
fn ordinary_drain_waits_for_the_predecessor_dedicated_cleanup() {
    ordinary_drain_waits_for_dedicated_cleanup(true);
}

#[test]
fn ordinary_cancelled_owner_cannot_resume_the_successor() {
    let turn = OrdinaryRoot::new("ordinary-cancelled-retry-owner");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let old = turn.enqueue("Old head.", QueuedPromptSource::User);
    let owner = turn.record(QueuedTurnOwner::capture).unwrap();
    let next = turn.enqueue("Successor.", QueuedPromptSource::User);
    turn.state
        .cancel_queued_prompt(&turn.session, &old)
        .unwrap();
    let before = turn.saved();
    assert!(matches!(
        turn.state
            .resume_local_session_queue(&turn.session, Some(owner))
            .unwrap(),
        None | Some(TurnDispatchDeliveryOutcome::Superseded)
    ));
    assert_eq!(turn.saved(), before);
    turn.record(|record| assert_eq!(record.queued_prompts[0].pending_prompt.id, next));
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_owner_is_invalidated_by_content_generation_or_a_new_pause() {
    for change in ["content", "generation", "pause", "stop"] {
        let turn = OrdinaryRoot::new("ordinary-stale-retry-owner");
        turn.edit(|record| record.set_auto_dispatch_blocked(true));
        turn.enqueue("Original head.", QueuedPromptSource::User);
        let owner = turn.record(QueuedTurnOwner::capture).unwrap();
        turn.edit(|record| match change {
            "content" => record.queued_prompts[0]
                .pending_prompt
                .text
                .push_str(" changed"),
            "generation" => record.active_turn_generation += 1,
            "pause" => record.set_auto_dispatch_blocked(true),
            _ => {
                record.runtime_stop_generation += 1;
            }
        });
        let before = turn.saved();
        assert!(matches!(
            turn.state
                .resume_local_session_queue(&turn.session, Some(owner))
                .unwrap(),
            None | Some(TurnDispatchDeliveryOutcome::Superseded)
        ));
        assert_eq!(turn.saved(), before, "{change}");
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}

#[test]
fn an_operator_pause_survives_reload_and_only_explicit_resume_releases_the_queue() {
    let turn = OrdinaryRoot::new("ordinary-operator-retry-pause");
    deliver_turn_dispatch(&turn.state, turn.prepare("Running ordinary turn.")).unwrap();
    turn.prompt();
    let id = turn.enqueue("Queued behind Stop.", QueuedPromptSource::User);
    turn.state.stop_session(&turn.session).unwrap();
    let saved = turn.record(PersistedSessionRecord::from_record);
    let loaded = saved.into_record().unwrap();
    assert!(loaded.engram.operator_paused);
    assert!(loaded.orchestrator_auto_dispatch_blocked);
    assert_eq!(loaded.queued_prompts[0].pending_prompt.id, id);
    assert!(
        turn.state
            .dispatch_next_queued_turn(&turn.session, false)
            .unwrap()
            .is_none()
    );
    turn.assert_no_prompt();
    turn.state.resume_session_queue(&turn.session).unwrap();
    assert!(turn.prompt().prompt.contains("Queued behind Stop."));
    turn.assert_no_prompt();
    turn.record(|record| assert!(!record.engram.operator_paused));
    turn.assert_no_control();
}

#[test]
fn historical_admission_and_bind_retries_load_as_data_without_a_runtime_or_provider_retry() {
    for key in ["engramAdmissionRetry", "engramBindRetry"] {
        let turn = OrdinaryRoot::new("ordinary-passive-retry-load");
        let history = json!({"acknowledged":true,"attempts":7,"due_at":"2000-01-01T00:00:00Z",
            "idempotency_key":"old-key","future":{"żółw":[null,true]}});
        turn.edit(|record| {
            record
                .engram
                .historical_control
                .insert(key.into(), history.clone());
        });
        let loaded = turn.record(|record| {
            PersistedSessionRecord::from_record(record)
                .into_record()
                .unwrap()
        });
        assert_eq!(loaded.engram.historical_control[key], history);
        assert!(matches!(loaded.runtime, SessionRuntime::None));
        assert!(loaded.provider_capacity_retry.is_none());
        turn.assert_no_prompt();
        turn.assert_no_control();
        deliver_turn_dispatch(&turn.state, turn.prepare("Fresh ordinary input.")).unwrap();
        turn.prompt();
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}

#[test]
fn the_test_run_index_tick_cannot_replay_passive_admission_history() {
    let turn = OrdinaryRoot::new("ordinary-passive-retry-index-tick");
    turn.edit(|record| {
        record.set_auto_dispatch_blocked(true);
        record.engram.historical_control.insert(
            "engramAdmissionRetry".into(),
            json!({"acknowledged":true,"due_at":"2000-01-01T00:00:00Z"}),
        );
    });
    turn.enqueue(
        "Paused independently of old retry data.",
        QueuedPromptSource::User,
    );
    let before = turn.saved();
    turn.state.engram_host().test_run_tick();
    assert_eq!(turn.saved(), before);
    turn.assert_no_prompt();
    turn.assert_no_control();
}

// Keep every descendant mounted. Their source migration belongs to their
// existing owners; no legacy fixture alias is supplied to hide that work.
#[path = "engram_admission_budget_bound.rs"]
mod admission_budget_bound;
#[path = "engram_begin_replay.rs"]
mod begin_replay;
#[path = "engram_begin_unknown_recovery.rs"]
mod begin_unknown_recovery;
#[path = "engram_control_non_application.rs"]
mod control_non_application;
