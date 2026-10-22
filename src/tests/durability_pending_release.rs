// A late committed release is not permission to free its slot before ACK.
use super::*;

mod restart {
    include!("durability_pending_release_restart.rs");
}

#[tokio::test]
async fn durability_pending_release_retains_its_audit_and_reserved_slot_on_unchanged_retry() {
    let _temp_scope = TestTempRootDirectoryScope::repository_local();
    let (mut claimed, entry) = settled_root("durability-pending-release", "live");
    let orphan = orphan_of(&entry, "durability-pending", "session-gone", 100);
    retain(&claimed, &orphan);
    let capacity_before = engram_named_root_capacity(&claimed.state.inner.lock().unwrap());
    let (tx, rx) = mpsc::channel();
    claimed.state.persist_tx = tx;
    let state = claimed.state.clone();
    let request = release_request(&orphan, "finished lane");
    let worker = {
        let state = state.clone();
        let request = request.clone();
        std::thread::spawn(move || {
            state.release_engram_named_root(Some("session-operator"), request)
        })
    };
    let mut batch = PersistFenceBatch::default();
    receive_naming_authority_fence(&rx, &mut batch, &worker);
    let pending_audit = match &batch.pending[0].target {
        PersistFenceTarget::EngramNamedRootRelease(audit) => audit.as_ref().clone(),
        _ => panic!("the operator release must fence its exact removal and audit"),
    };
    assert_eq!(pending_audit.selection, orphan);
    assert_eq!(pending_audit.reason, "finished lane");
    assert!(!pending_audit.acknowledged);
    assert!(!worker.is_finished());
    assert_eq!(
        engram_named_root_capacity(&state.inner.lock().unwrap()),
        capacity_before
    );
    assert!(matches!(
        batch.pending[0]
            .completion
            .poll_at(batch.pending[0].completion.deadline),
        Some(Err(PersistFenceError::Deadline))
    ));
    state.engram_budget_clock().notify();
    let error = match worker.join().unwrap() {
        Err(error) => error,
        Ok(_) => panic!("a deadline is not a release receipt"),
    };
    let response = error.into_response();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    let pending: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        engram_named_root_capacity(&state.inner.lock().unwrap()),
        capacity_before
    );
    assert_eq!(audit(&claimed).len(), 1);
    assert!(!audit(&claimed)[0].acknowledged);

    // Commit AFTER the failed waiter. No synthetic successful completion:
    // the expired fence stays terminal while the exact operation lands.
    let mut cache = SqlitePersistConnectionCache::new();
    let late = collect_persist_delta_from_shared_state(&state.inner, 0);
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &late,
        &mut batch,
    )
    .unwrap();
    let disk = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(!disk.engram_work_source_roots.contains(&orphan));
    assert_eq!(disk.engram_named_root_release_audit.len(), 1);
    assert_eq!(disk.engram_named_root_release_audit[0].id, pending_audit.id);
    let mut changed = request.clone();
    changed.reason = "a different release reason".to_owned();
    assert_eq!(
        state
            .release_engram_named_root(Some("session-operator"), changed)
            .unwrap_err()
            .status,
        StatusCode::CONFLICT,
        "a changed payload is not the pending release's replay"
    );
    assert_eq!(
        state
            .release_engram_named_root(Some("another-operator"), request.clone())
            .unwrap_err()
            .status,
        StatusCode::CONFLICT,
        "a different requester cannot resume the audit as its own request"
    );
    assert_eq!(
        engram_named_root_capacity(&state.inner.lock().unwrap()),
        capacity_before,
        "a late commit alone does not publish an ACK to the running caller"
    );

    let worker = {
        let state = state.clone();
        std::thread::spawn(move || {
            state.release_engram_named_root(Some("session-operator"), request)
        })
    };
    receive_naming_authority_fence(&rx, &mut batch, &worker);
    let replay_audit = match &batch.pending[0].target {
        PersistFenceTarget::EngramNamedRootRelease(audit) => audit,
        _ => panic!("same release content fence"),
    };
    assert_eq!(replay_audit.id, pending_audit.id);
    assert_eq!(replay_audit.selection, pending_audit.selection);
    let replay = collect_persist_delta_from_shared_state(&state.inner, 0);
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &replay,
        &mut batch,
    )
    .unwrap();
    let released = worker.join().unwrap().unwrap();
    assert!(released.released);
    assert_eq!(
        engram_named_root_capacity(&state.inner.lock().unwrap()) + 1,
        capacity_before
    );
    let records = audit(&claimed);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, pending_audit.id);
    assert!(records[0].acknowledged);

    // Fail on the old 500/body only after actual late commit and retry settle.
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{pending}");
    assert_eq!(pending["contract"], "termal-durability-pending");
    assert_eq!(pending["version"], 1);
    assert_eq!(pending["code"], "durability_pending");
    assert_eq!(pending["outcome"], "pending");
    assert_eq!(pending["operation"], "named_root_release");
    assert_eq!(pending["identity"]["workId"], orphan.work_id);
    assert_eq!(pending["identity"]["claimId"], orphan.claim_id);
    assert_eq!(pending["identity"]["generation"], orphan.generation);
    assert_eq!(pending["identity"]["auditId"], pending_audit.id);
    assert!(durability_pending_for_request(status, "POST",
        "/api/sessions/session-operator/engram-named-root-release", "session-operator",
        &json!({"work": orphan.short_ref, "generation": orphan.generation, "reason": "finished lane"}),
        &pending).is_some(), "the actual release envelope must survive the MCP validator");
    assert_eq!(pending["retry"]["mode"], "same_request");
}
