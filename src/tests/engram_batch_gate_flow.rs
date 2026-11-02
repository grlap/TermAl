// Ordinary launch member snapshots and passive batch history. A recorded
// artifact is not a checkpoint or a native verification binding. Retired
// owed/pending JSON survives the real store and cannot schedule work.
use super::*;

const MEMBER_GENERATION: u64 = 5;

// A member-root selector fixture. The launching turn's own root was named
// through the real ordinary work service; this additional row is test data,
// not a second naming RPC, receipt, verification record or bind obligation.
fn name_member(turn: &EOCheckedTurn) -> EngramWorkSourceRoot {
    let mut inner = turn.state.inner.lock().unwrap();
    let launch = inner.engram_work_source_roots.iter()
        .find(|entry| entry.named_by_session == turn.session_id).cloned().unwrap();
    let member = EngramWorkSourceRoot {
        work_id: "work-member".to_owned(), short_ref: "w-member".to_owned(),
        claim_id: "claim-member".to_owned(), claim_fence: 1,
        generation: MEMBER_GENERATION, ..launch
    };
    inner.engram_work_source_roots.push(member.clone());
    member
}

fn member_binding(fence: i64) -> EngramControlWorkBinding {
    EngramControlWorkBinding {
        root_execution_id: "root-member".to_owned(), work_id: "work-member".to_owned(),
        run_id: "run-member".to_owned(), work_revision: 1,
        claim_id: "claim-member".to_owned(), claim_fence: fence,
    }
}

fn script_members(turn: &EOCheckedTurn, member_fence: Option<i64>, omitted: u64) {
    let launch = turn.record(|record| record.engram.work_binding.clone().unwrap());
    let mut items = vec![EngramHeldClaim {
        work_id: launch.work_id.clone(), short_ref: launch.work_id.clone(),
        claim_id: launch.claim_id.clone(), claim_fence: launch.claim_fence,
        focused: true, control_binding: Some(launch),
    }];
    if let Some(fence) = member_fence {
        items.push(EngramHeldClaim {
            work_id: "work-member".to_owned(), short_ref: "w-member".to_owned(),
            claim_id: "claim-member".to_owned(), claim_fence: fence,
            focused: false, control_binding: Some(member_binding(fence)),
        });
    }
    turn.transport.replace_held_claims([Ok(EngramHeldClaims { items, omitted })]);
}

#[test]
fn an_ordinary_launch_preserves_the_members_original_fence_without_binding_its_artifact() {
    let (turn, worktree) = ordinary_carried_turn("ordinary-member-fence");
    name_member(&turn);
    script_members(&turn, Some(2), 0);
    ordinary_launch_gate(&turn, &worktree, false);
    let original = turn.record(|record| record.engram.carried_checks[0].bind_targets.clone());
    assert_eq!(original, vec![EngramBindTarget {
        work_id: "work-member".to_owned(), short_ref: "w-member".to_owned(),
        claim_id: "claim-member".to_owned(), claim_fence: 2,
        root_generation: MEMBER_GENERATION,
    }]);
    script_members(&turn, Some(3), 0);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_artifact_custody(&turn, &run, true);
    turn.record(|record| {
        assert_eq!(record.engram.carried_checks[0].bind_targets, original);
        let persisted = serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap();
        assert!(persisted.get("engramOwedBinds").is_none());
        assert!(persisted.get("engramPendingBatches").is_none());
    });
}

#[test]
fn an_unread_member_claim_creates_no_captured_target_or_native_binding() {
    let (turn, worktree) = ordinary_carried_turn("ordinary-member-unread");
    name_member(&turn);
    turn.transport.replace_held_claims([Err(EngramTransportError::transport("held read unavailable"))]);
    ordinary_launch_gate(&turn, &worktree, false);
    turn.record(|record| {
        assert!(record.engram.carried_checks[0].bind_targets.is_empty());
        let line = record.engram.pending_source_root_line.as_deref().unwrap();
        assert!(line.contains("w-member") && line.contains("could not be read"), "{line}");
    });
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn a_member_missing_from_a_cut_short_held_list_stays_unknown_without_a_binding() {
    let (turn, worktree) = ordinary_carried_turn("ordinary-member-cut");
    name_member(&turn);
    script_members(&turn, None, 1);
    ordinary_launch_gate(&turn, &worktree, false);
    turn.record(|record| {
        assert!(record.engram.carried_checks[0].bind_targets.is_empty());
        let line = record.engram.pending_source_root_line.as_deref().unwrap();
        assert!(line.contains("w-member") && line.contains("was cut short"), "{line}");
    });
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

fn historical_batch_data() -> Value {
    json!({
        "engramOwedBinds": [
            {"original": "1".repeat(32), "measurement": {
                "workspace_id": "historical workspace", "source_revision": "content-v1:old",
                "measured_at": "2026-10-18T23:30:00.000Z"},
             "target": {"work_id": "old-member", "short_ref": "old-ref", "claim_id": "old-claim",
                "claim_fence": 7, "root_generation": 5},
             "decided": null, "attempts": 0, "firstOwedAt": "2026-10-18T23:30:00.000Z",
             "lastDetail": null, "nextAttemptAt": "2026-10-18T23:30:05.000Z",
             "futureField": {"unicode": "zażółć", "unknown": [null, false, 9]}},
            {"original": "2".repeat(64), "decided": {"binding": {"unknownOriginal": true},
                "sighting": {"observation": "3".repeat(32), "source_revision": "old-revision"}},
             "attempts": 8, "firstOwedAt": "2026-10-18T23:30:00.000Z",
             "lastDetail": "reply lost after send", "nextAttemptAt": "2099-01-01T00:00:00Z",
             "unknownDisposition": "possibly-applied"},
        ],
        "engramPendingBatches": [{"checkpointGrantId": "historical-only",
            "producerObservationId": "4".repeat(64), "targets": [{"unknown": null}],
            "pendedAt": "2026-10-18T23:30:00.000Z", "future": ["unchanged", 42]}],
    })
}

fn assert_batch_history(record: &SessionRecord, history: &Value) {
    let wire = serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap();
    for key in ["engramOwedBinds", "engramPendingBatches"] {
        assert_eq!(&wire[key], &history[key], "{key} is passive, complete original data");
    }
    assert!(record.engram.pending_source_root_line.is_none(),
        "raw history cannot invent a collision, retry or expiry notice");
}

#[test]
fn historical_owed_and_pending_batches_survive_sqlite_and_an_ordinary_turn_without_work() {
    let (turn, _) = ordinary_carried_turn("ordinary-passive-batches");
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
    let guard = phase_sync::PollGuard::new();
    loop {
        if turn.record(|record| record.engram.unadmitted_active_turn.is_none()
            && !record.engram.unadmitted_worker_running) { break; }
        guard.wait("the initial ordinary observation completes before loading history");
    }
    let history = historical_batch_data();
    let runtime = turn.record(|record| record.runtime.clone());
    // Read the fixture's association before the stored record replaces the
    // live one: the persisted form carries no work binding.
    let binding = turn.record(|record| record.engram.work_binding.clone())
        .expect("the fixture associated its claim");
    let stored = turn.record(|record| {
        let mut wire = serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap();
        for key in ["engramOwedBinds", "engramPendingBatches"] {
            wire[key] = history[key].clone();
        }
        serde_json::from_value::<PersistedSessionRecord>(wire).unwrap().into_record().unwrap()
    });
    assert_batch_history(&stored, &history);
    turn.state.shutdown_persist_blocking();
    {
        let mut inner = turn.state.inner.lock().unwrap();
        let index = inner.find_session_index(&turn.session_id).unwrap();
        inner.sessions[index] = stored;
        inner.session_mut_by_index(index).unwrap();
        turn.state.persist_synchronous_locked(&inner).unwrap();
    }
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    assert_batch_history(&loaded.sessions[loaded.find_session_index(&turn.session_id).unwrap()], &history);
    // Boot restores no current-claim mirror and no in-memory fixture hooks.
    // Reattach exactly what the fixture installed: its clock, transport,
    // source capture and explicit work association (as the fixture does
    // before naming), never a persisted credential.
    let clock = turn.state.engram_budget_clock();
    *turn.state.inner.lock().unwrap() = loaded;
    turn.state.select_test_engram_budget_clock(clock);
    turn.state.install_control_test_transport(turn.transport.clone());
    turn.state.inner.lock().unwrap().test_engram_source_capture =
        Some(Arc::new(|place, _budget| engram_place_source_basis(place)));
    turn.record_mut(|record| {
        assert!(record.engram.work_binding.is_none(), "the reload restores no claim association");
        record.runtime = runtime;
        record.engram.context_nudge_pending = false;
        record.engram.work_binding = Some(binding);
    });
    // Drive the surviving native worker and actual provider turn. No removed
    // owed-bind tick or fake elapsed retry invokes anything from this data.
    turn.state.engram_verification_records_tick();
    turn.begin("Read the ordinary session history.");
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
    turn.record(|record| assert_batch_history(record, &history));
    let project_id = turn.record(|record| record.session.project_id.clone().unwrap());
    turn.state.update_project_engram_settings(&project_id,
        EngramProjectSettings { enabled: false, ..EngramProjectSettings::default() }).unwrap();
    turn.record(|record| assert_batch_history(record, &history));
    {
        let inner = turn.state.inner.lock().unwrap();
        turn.state.persist_synchronous_locked(&inner).unwrap();
    }
    let again = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    assert_batch_history(&again.sessions[again.find_session_index(&turn.session_id).unwrap()], &history);
    assert_ordinary_turn_has_no_native_requests(&turn);
}
