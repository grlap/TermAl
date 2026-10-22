// Restart controls for an enqueued canonical release, using the real writer.
use super::*;

fn own_release_request(
    root: &EngramWorkSourceRoot,
    reason: &str,
) -> EngramNamedRootOwnReleaseRequest {
    EngramNamedRootOwnReleaseRequest {
        work: root.short_ref.clone(),
        generation: root.generation,
        reason: reason.to_owned(),
    }
}

async fn own_release_final_deadline_reloads_current_state(canonical: bool) {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let label = if canonical {
        "own-final-canonical"
    } else {
        "own-final-unassociated"
    };
    let (mut claimed, entry) = settled_root(label, "completed");
    let root = if canonical {
        entry.clone()
    } else {
        let orphan = orphan_of(&entry, "own-final", &claimed.session_id, 100);
        retain(&claimed, &orphan);
        orphan
    };
    let capacity_before = engram_named_root_capacity(&claimed.state.inner.lock().unwrap());
    let (tx, rx) = mpsc::channel();
    claimed.state.persist_tx = tx;
    let state = claimed.state.clone();
    let session = claimed.session_id.clone();
    let request = own_release_request(&root, "finished original lane");
    let worker = std::thread::spawn(move || state.release_own_engram_named_root(&session, request));
    let mut cache = SqlitePersistConnectionCache::new();
    let mut batch = PersistFenceBatch::default();
    // ACK every real authority image, but not the final exact release fence.
    let original = loop {
        receive_naming_authority_fence(&rx, &mut batch, &worker);
        if let PersistFenceTarget::EngramNamedRootRelease(audit) = &batch.pending[0].target {
            break audit.as_ref().clone();
        }
        assert!(matches!(
            &batch.pending[0].target,
            PersistFenceTarget::EngramWorkAuthority(_)
        ));
        let delta = collect_persist_delta_from_shared_state(&claimed.state.inner, 0);
        persist_delta_with_fences(
            &mut cache,
            claimed.state.persistence_path.as_path(),
            &delta,
            &mut batch,
        )
        .unwrap();
    };
    assert_eq!(original.selection, root);
    assert_eq!(
        original.requested_by.as_deref(),
        Some(claimed.session_id.as_str())
    );
    assert_eq!(original.route, if canonical { "canonical" } else { "own" });
    assert!(!worker.is_finished());
    let late = collect_persist_delta_from_shared_state(&claimed.state.inner, 0);
    assert!(matches!(
        batch.pending[0]
            .completion
            .poll_at(batch.pending[0].completion.deadline),
        Some(Err(PersistFenceError::Deadline))
    ));
    claimed.state.engram_budget_clock().notify();
    let error = worker
        .join()
        .unwrap()
        .expect_err("the final fence did not ACK");
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        error.durability_deadline.as_ref().unwrap().identity["auditId"],
        original.id
    );
    assert_eq!(
        engram_named_root_capacity(&claimed.state.inner.lock().unwrap()),
        capacity_before - usize::from(canonical),
        "canonical retirement already had its real authority ACK; an own-only release still reserves its slot"
    );
    // Commit the actual selected delta after the failed waiter. Reload must
    // derive acknowledgement from disk, not from a synthetic fence success.
    persist_delta_with_fences(
        &mut cache,
        claimed.state.persistence_path.as_path(),
        &late,
        &mut batch,
    )
    .unwrap();
    drop(rx);
    reload_release_fixture_for(&claimed, &root);
    let durable_audit = audit(&claimed);
    assert_eq!(durable_audit.len(), 1);
    assert_eq!(durable_audit[0].id, original.id);
    assert!(durable_audit[0].acknowledged);
    assert!(!retained(&claimed, &root));
    let requests_before = claimed.transport.requests().len();
    let other_requester = {
        let mut inner = claimed.state.inner.lock().unwrap();
        inner
            .create_session(
                Agent::Codex,
                Some("Different release requester".to_owned()),
                claimed.root.to_string_lossy().into_owned(),
                None,
                None,
            )
            .session
            .id
            .clone()
    };
    let foreign = release_own_engram_named_root_route(
        State(claimed.state.clone()),
        AxumPath(other_requester),
        Ok(Json(own_release_request(&root, "finished original lane"))),
    )
    .await;
    assert_eq!(
        foreign
            .err()
            .expect("another requester cannot inherit this release")
            .status,
        StatusCode::CONFLICT
    );
    assert_eq!(audit(&claimed), durable_audit);
    // Use the actual MCP-facing own HTTP handler, not the generic operator
    // release method that already knew how to find acknowledged audits.
    let changed = release_own_engram_named_root_route(
        State(claimed.state.clone()),
        AxumPath(claimed.session_id.clone()),
        Ok(Json(own_release_request(&root, "changed release reason"))),
    )
    .await;
    assert_eq!(
        changed.err().expect("a changed reason conflicts").status,
        StatusCode::CONFLICT
    );
    let replay = release_own_engram_named_root_route(
        State(claimed.state.clone()),
        AxumPath(claimed.session_id.clone()),
        Ok(Json(own_release_request(&root, "finished original lane"))),
    )
    .await;
    let Json(released) = replay.expect("the unchanged own request sees its durable current state");
    assert!(released.released);
    assert_eq!(released.resolution, "current_state");
    assert_eq!(released.route, original.route);
    assert_eq!(
        claimed.transport.requests().len(),
        requests_before,
        "no new canonical read or bind"
    );
    assert_eq!(
        audit(&claimed),
        durable_audit,
        "no replacement audit or mutation"
    );
    if !canonical {
        assert!(
            retained(&claimed, &entry),
            "the unrelated canonical root stays retained"
        );
    }
    retain(&claimed, &root);
    let retained_again = release_own_engram_named_root_route(
        State(claimed.state.clone()),
        AxumPath(claimed.session_id.clone()),
        Ok(Json(own_release_request(&root, "finished original lane"))),
    )
    .await;
    assert_eq!(
        retained_again
            .err()
            .expect("a retained-again selection conflicts")
            .status,
        StatusCode::CONFLICT
    );
    assert!(retained(&claimed, &root));
    assert_eq!(audit(&claimed), durable_audit);
}

#[tokio::test]
async fn durability_pending_review_own_canonical_final_deadline_reload() {
    own_release_final_deadline_reloads_current_state(true).await;
}

#[tokio::test]
async fn durability_pending_review_own_unassociated_final_deadline_reload() {
    own_release_final_deadline_reloads_current_state(false).await;
}

#[test]
fn durability_pending_review_own_unpublished_canonical_is_not_current_state() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (mut claimed, root) = settled_root("own-unpublished", "completed");
    let (tx, rx) = mpsc::channel();
    claimed.state.persist_tx = tx;
    let state = claimed.state.clone();
    let session = claimed.session_id.clone();
    let request = own_release_request(&root, "finished unpublished lane");
    let worker = std::thread::spawn(move || state.release_own_engram_named_root(&session, request));
    let mut cache = SqlitePersistConnectionCache::new();
    let mut batch = PersistFenceBatch::default();
    // The real Prepared ACK permits the canonical read, but publication's
    // Candidate ACK is deliberately withheld while its real delta lands late.
    receive_naming_authority_fence(&rx, &mut batch, &worker);
    assert!(
        matches!(&batch.pending[0].target, PersistFenceTarget::EngramWorkAuthority(image)
        if image.history.transition.as_ref().unwrap().phase == EngramAuthorityPhase::Prepared)
    );
    let prepared = collect_persist_delta_from_shared_state(&claimed.state.inner, 0);
    persist_delta_with_fences(
        &mut cache,
        claimed.state.persistence_path.as_path(),
        &prepared,
        &mut batch,
    )
    .unwrap();
    receive_naming_authority_fence(&rx, &mut batch, &worker);
    let candidate = match &batch.pending[0].target {
        PersistFenceTarget::EngramWorkAuthority(image) => image.as_ref().clone(),
        _ => panic!("canonical publication must fence its authority image"),
    };
    assert_eq!(
        candidate.history.transition.as_ref().unwrap().phase,
        EngramAuthorityPhase::Candidate
    );
    assert!(candidate.selection.is_none());
    let original = audit(&claimed)[0].clone();
    let late = collect_persist_delta_from_shared_state(&claimed.state.inner, 0);
    assert!(matches!(
        batch.pending[0]
            .completion
            .poll_at(batch.pending[0].completion.deadline),
        Some(Err(PersistFenceError::Deadline))
    ));
    claimed.state.engram_budget_clock().notify();
    assert_eq!(
        worker.join().unwrap().unwrap_err().status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    persist_delta_with_fences(
        &mut cache,
        claimed.state.persistence_path.as_path(),
        &late,
        &mut batch,
    )
    .unwrap();
    drop(rx);
    reload_release_fixture_for(&claimed, &root);
    let loaded_audit = audit(&claimed);
    assert_eq!(loaded_audit.len(), 1);
    assert_eq!(loaded_audit[0].id, original.id);
    assert!(!loaded_audit[0].acknowledged);
    assert_eq!(
        engram_named_root_capacity(&claimed.state.inner.lock().unwrap()),
        1
    );
    let (tx, rx) = mpsc::channel();
    claimed.state.persist_tx = tx;
    let state = claimed.state.clone();
    let session = claimed.session_id.clone();
    let request = own_release_request(&root, "finished unpublished lane");
    let retry = std::thread::spawn(move || state.release_own_engram_named_root(&session, request));
    receive_naming_authority_fence(&rx, &mut batch, &retry);
    assert!(
        matches!(&batch.pending[0].target, PersistFenceTarget::EngramWorkAuthority(image)
        if image.history.transition.as_ref().unwrap().id == candidate.history.transition.as_ref().unwrap().id)
    );
    assert!(
        !retry.is_finished(),
        "reload alone cannot acknowledge publication"
    );
    batch.fail(PersistFenceError::WriteFailed(
        "unpublished retry remains unconfirmed".to_owned(),
    ));
    let error = retry
        .join()
        .unwrap()
        .expect_err("unpublished state cannot become success");
    assert!(error.status.is_server_error());
    assert!(
        error.durability_deadline.is_none(),
        "write failure is not typed pending"
    );
    assert_eq!(
        audit(&claimed),
        loaded_audit,
        "no replacement or acknowledged audit"
    );
    assert_eq!(
        engram_named_root_capacity(&claimed.state.inner.lock().unwrap()),
        1
    );
    let inner = claimed.state.inner.lock().unwrap();
    assert!(
        inner
            .engram_work_naming_history
            .iter()
            .flat_map(|history| &history.retirements)
            .filter(|notice| notice.selection == root)
            .all(|notice| !notice.published)
    );
}

/// Expire the first prepared-image fence, then really commit its selected
/// delta AFTER the response. The receiver is dropped on return so subsequent
/// explicit retries use the fixture's existing synchronous SQLite writer.
fn late_canonical_release(
    label: &str,
) -> (
    ClaimedRoot,
    EngramWorkSourceRoot,
    EngramNamedRootReleaseAudit,
    EngramAuthorityTransition,
    ApiError,
) {
    let (mut claimed, entry) = settled_root(label, "completed");
    let (tx, rx) = mpsc::channel();
    claimed.state.persist_tx = tx;
    let state = claimed.state.clone();
    let request = release_request(&entry, "completed original lane");
    let task = {
        let state = state.clone();
        std::thread::spawn(move || {
            state.release_engram_named_root(Some("session-operator"), request)
        })
    };
    let mut batch = PersistFenceBatch::default();
    receive_naming_authority_fence(&rx, &mut batch, &task);
    let owner = match &batch.pending[0].target {
        PersistFenceTarget::EngramWorkAuthority(image) => {
            assert_eq!(image.selection.as_ref(), Some(&entry));
            let owner = image.history.transition.clone().unwrap();
            assert_eq!(owner.phase, EngramAuthorityPhase::Prepared);
            owner
        }
        _ => panic!("canonical release must first enqueue its prepared authority image"),
    };
    let records = audit(&claimed);
    assert_eq!(records.len(), 1);
    let original = records[0].clone();
    assert!(!original.acknowledged);
    let capacity = engram_named_root_capacity(&state.inner.lock().unwrap());
    assert!(matches!(
        batch.pending[0]
            .completion
            .poll_at(batch.pending[0].completion.deadline),
        Some(Err(PersistFenceError::Deadline))
    ));
    state.engram_budget_clock().notify();
    let error = task
        .join()
        .unwrap()
        .expect_err("an expired fence is not a release receipt");
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        state
            .inner
            .lock()
            .unwrap()
            .engram_work_source_roots
            .contains(&entry)
    );
    assert_eq!(
        engram_named_root_capacity(&state.inner.lock().unwrap()),
        capacity
    );
    let late = collect_persist_delta_from_shared_state(&state.inner, 0);
    let mut cache = SqlitePersistConnectionCache::new();
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &late,
        &mut batch,
    )
    .expect("actual late commit, not a fabricated successful fence completion");
    assert!(matches!(
        error
            .durability_deadline
            .as_ref()
            .map(|pending| pending.operation),
        Some(Some(DurabilityPendingOperation::NamedRootRelease))
    ));
    (claimed, entry, original, owner, error)
}

fn reload_release_fixture(claimed: &ClaimedRoot) {
    let selection = audit(claimed)[0].selection.clone();
    reload_release_fixture_for(claimed, &selection);
}

fn reload_release_fixture_for(claimed: &ClaimedRoot, selection: &EngramWorkSourceRoot) {
    // The source fixture moved on to a successor claim. Re-establish THAT
    // current caller after boot, not the old label's completed claim.
    // work_binding is deliberately runtime-only, absent on a real load.
    let current_binding = {
        let inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        let binding = inner.sessions[index].engram.work_binding.clone().unwrap();
        assert_ne!(
            binding.claim_id, selection.claim_id,
            "this reader has moved on; reload must not reclaim the released claim"
        );
        binding
    };
    let clock = claimed.state.engram_budget_clock();
    let loaded = load_state(claimed.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    *claimed.state.inner.lock().unwrap() = loaded;
    claimed.state.select_test_engram_budget_clock(clock);
    install_control_only_transport(&claimed.state, claimed.transport.clone());
    claimed
        .transport
        .enable_named_roots(&claimed.session_id, &current_binding);
    claimed.record(|record| {
        assert!(
            record.engram.work_binding.is_none(),
            "boot does not restore a local current-claim mirror"
        );
        record
            .engram
            .routing_token
            .get_or_insert_with(|| "fixture-root-token".to_owned());
        record.engram.work_binding = Some(current_binding.clone());
    });
    let inner = claimed.state.inner.lock().unwrap();
    let index = inner.find_session_index(&claimed.session_id).unwrap();
    assert_eq!(
        inner.sessions[index].engram.work_binding.as_ref(),
        Some(&current_binding)
    );
}

fn release_owner(claimed: &ClaimedRoot, entry: &EngramWorkSourceRoot) -> EngramAuthorityTransition {
    claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_work_naming_history
        .iter()
        .find(|history| history.store == entry.store && history.work_id == entry.work_id)
        .unwrap()
        .transition
        .clone()
        .unwrap()
}

#[test]
fn durability_pending_release_restart_keeps_late_prepared_audit_unacknowledged() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (claimed, entry, original, owner, error) = late_canonical_release("release-restart-late");
    let loaded = load_state(claimed.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(loaded.engram_work_source_roots.contains(&entry));
    assert_eq!(
        engram_named_root_capacity(&loaded),
        1,
        "the selection counts once, not twice"
    );
    let records = &loaded.engram_named_root_release_audit;
    assert_eq!(
        records.len(),
        1,
        "an enqueued audit is not a never-enqueued legacy intent"
    );
    let record = &records[0];
    assert_eq!(record.id, original.id);
    assert_eq!(record.authority_owner.as_deref(), Some(owner.id.as_str()));
    assert!(!record.acknowledged);
    assert_eq!(serde_json::to_value(record).unwrap()["phase"], "enqueued");
    let identity = &error.durability_deadline.as_ref().unwrap().identity;
    assert_eq!(identity["auditId"], original.id);
    assert_eq!(identity["phase"], "enqueued");
}

#[test]
fn durability_pending_release_restart_exact_replay_uses_one_audit_and_transition() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let label = "release-restart-replay";
    let (claimed, entry, original, owner, _) = late_canonical_release(label);
    reload_release_fixture(&claimed);
    let result = claimed
        .state
        .release_engram_named_root(
            Some("session-operator"),
            release_request(&entry, "completed original lane"),
        )
        .expect("the same request resumes its retained canonical transition");
    assert!(result.released);
    assert_eq!(
        serde_json::to_value(result).unwrap()["resolution"],
        "resumed"
    );
    let records = audit(&claimed);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, original.id);
    assert_eq!(
        records[0].authority_owner.as_deref(),
        Some(owner.id.as_str())
    );
    assert!(records[0].acknowledged);
    let current = release_owner(&claimed, &entry);
    assert_eq!(current.id, owner.id, "no replacement transition UUID");
    assert_eq!(
        current.version, owner.version,
        "no second transition allocation"
    );
    assert!(!retained(&claimed, &entry));
    assert_eq!(
        engram_named_root_capacity(&claimed.state.inner.lock().unwrap()),
        0
    );
}

#[test]
fn durability_pending_release_restart_changed_owner_conflicts_without_replacement() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let label = "release-restart-owner";
    let (claimed, entry, original, _, _) = late_canonical_release(label);
    reload_release_fixture(&claimed);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let history = inner
            .engram_work_naming_history
            .iter_mut()
            .find(|history| history.store == entry.store && history.work_id == entry.work_id)
            .unwrap();
        history.owner_version += 1;
        let transition = history.transition.as_mut().unwrap();
        transition.id = "changed-current-authority-owner".to_owned();
        transition.version = history.owner_version;
    }
    let owner_before = release_owner(&claimed, &entry);
    let error = claimed
        .state
        .release_engram_named_root(
            Some("session-operator"),
            release_request(&entry, "completed original lane"),
        )
        .expect_err("an old audit cannot commandeer a changed current owner");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error.message.contains(&original.id),
        "the conflict names the retained audit: {}",
        error.message
    );
    assert!(error.message.contains(&entry.generation.to_string()));
    let records = audit(&claimed);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, original.id);
    assert!(!records[0].acknowledged);
    assert_eq!(
        release_owner(&claimed, &entry),
        owner_before,
        "no replacement owner or version"
    );
    assert!(retained(&claimed, &entry));
}

#[test]
fn durability_pending_release_restart_legacy_phase_none_still_drops_retained_intent() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (claimed, entry, _, _, _) = late_canonical_release("release-restart-legacy");
    let mut image = serde_json::to_value(PersistedState::from_inner(
        &claimed.state.inner.lock().unwrap(),
    ))
    .unwrap();
    // Round-trip an actual old-format audit: absence, not an invented enum.
    for audit in image["engramNamedRootReleaseAudit"].as_array_mut().unwrap() {
        audit.as_object_mut().unwrap().remove("phase");
    }
    let restored = serde_json::from_value::<PersistedState>(image)
        .unwrap()
        .into_inner()
        .unwrap();
    assert!(restored.engram_work_source_roots.contains(&entry));
    assert!(restored.engram_named_root_release_audit.is_empty());
    assert_eq!(engram_named_root_capacity(&restored), 1);
}

#[test]
fn durability_pending_release_restart_own_notice_preserves_publication_rules() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (claimed, entry) = settled_root("release-restart-notice", "completed");
    let clock = claimed.state.engram_budget_clock();
    let once = Arc::new(std::sync::atomic::AtomicBool::new(false));
    claimed
        .state
        .inner
        .lock()
        .unwrap()
        .test_engram_authority_ack_boundary = Some(Arc::new(move |phase| {
        if phase == "before_publication" && !once.swap(true, std::sync::atomic::Ordering::SeqCst) {
            clock.advance(ENGRAM_NAMED_ROOT_RELEASE_AUTHORITY_BUDGET + Duration::from_secs(1));
        }
    }));
    claimed
        .state
        .release_engram_named_root(
            Some("session-operator"),
            release_request(&entry, "completed original lane"),
        )
        .expect_err("the existing candidate was committed but not published");
    claimed
        .state
        .inner
        .lock()
        .unwrap()
        .test_engram_authority_ack_boundary = None;
    assert!(!retained(&claimed, &entry));
    let original = audit(&claimed)[0].clone();
    for phase in [None, Some("enqueued")] {
        for published in [false, true] {
            let mut persisted = PersistedState::from_inner(&claimed.state.inner.lock().unwrap());
            let notice = persisted
                .engram_work_naming_history
                .iter_mut()
                .flat_map(|history| &mut history.retirements)
                .find(|notice| notice.selection == entry)
                .unwrap();
            assert_eq!(notice.owner, original.authority_owner);
            notice.published = published;
            let mut image = serde_json::to_value(persisted).unwrap();
            let audit = image["engramNamedRootReleaseAudit"][0]
                .as_object_mut()
                .unwrap();
            match phase {
                Some(phase) => {
                    audit.insert("phase".to_owned(), json!(phase));
                }
                None => {
                    audit.remove("phase");
                }
            }
            let restored = serde_json::from_value::<PersistedState>(image)
                .unwrap()
                .into_inner()
                .unwrap();
            assert_eq!(restored.engram_named_root_release_audit.len(), 1);
            let audit = &restored.engram_named_root_release_audit[0];
            assert_eq!(audit.id, original.id);
            assert_eq!(audit.acknowledged, published);
            assert_eq!(
                engram_named_root_capacity(&restored),
                usize::from(!published)
            );
        }
    }
    reload_release_fixture(&claimed);
    let reads_before = claimed.transport.requests().iter()
        .filter(|request| request.request["operation"] == "named_root_read").count();
    let response = claimed
        .state
        .release_engram_named_root(
            Some("session-operator"),
            release_request(&entry, "completed original lane"),
        )
        .expect("resume the already-retired selection's original publication");
    assert_eq!(
        serde_json::to_value(response).unwrap()["resolution"],
        "current_state"
    );
    let records = audit(&claimed);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, original.id);
    assert_eq!(records[0].authority_owner, original.authority_owner);
    assert!(records[0].acknowledged);
    assert_eq!(claimed.transport.requests().iter()
        .filter(|request| request.request["operation"] == "named_root_read").count(), reads_before,
        "an own retirement notice resumes publication, not another canonical read");
}

#[test]
fn durability_pending_release_restart_changed_payload_preserves_original_fields() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let label = "release-restart-payload";
    let (claimed, entry, original, _, _) = late_canonical_release(label);
    reload_release_fixture(&claimed);
    let owner_before = release_owner(&claimed, &entry);
    for (requester, reason) in [
        ("session-operator", "a replacement reason"),
        ("different-requester", "completed original lane"),
    ] {
        let error = claimed
            .state
            .release_engram_named_root(Some(requester), release_request(&entry, reason))
            .expect_err("a different payload is not this pending operation's replay");
        assert_eq!(error.status, StatusCode::CONFLICT);
        let records = audit(&claimed);
        assert_eq!(
            records,
            vec![original.clone()],
            "not one retained audit field may change"
        );
        assert_eq!(release_owner(&claimed, &entry), owner_before);
        assert!(retained(&claimed, &entry));
    }
}

/// Drive the real prepare/read/candidate path while the canonical root is
/// still live. Only the Candidate ACK expires, then its actual delta commits.
fn late_retained_candidate(
    label: &str,
    explicit: bool,
) -> (ClaimedRoot, EngramWorkSourceRoot, EngramAuthorityImage) {
    let (mut claimed, entry) = settled_root(label, "live");
    let (tx, rx) = mpsc::channel();
    claimed.state.persist_tx = tx;
    let state = claimed.state.clone();
    let root = entry.clone();
    let task = std::thread::spawn(move || {
        if explicit {
            state.release_engram_named_root(Some("session-operator"),
                release_request(&root, "completed original lane")).map(|_| ())
        } else {
            let deadline = state.engram_budget_clock().now() + ENGRAM_NAMED_ROOT_RELEASE_AUTHORITY_BUDGET;
            state.recover_one_engram_root_until(&root, &EngramRootReclamationRoute::Unfocused, deadline)
                .map(|_| ())
        }
    });
    let mut batch = PersistFenceBatch::default();
    let mut cache = SqlitePersistConnectionCache::new();
    receive_naming_authority_fence(&rx, &mut batch, &task);
    let prepared = collect_persist_delta_from_shared_state(&claimed.state.inner, 0);
    persist_delta_with_fences(&mut cache, claimed.state.persistence_path.as_path(), &prepared, &mut batch).unwrap();
    receive_naming_authority_fence(&rx, &mut batch, &task);
    let image = match &batch.pending[0].target {
        PersistFenceTarget::EngramWorkAuthority(image) => image.as_ref().clone(),
        _ => panic!("recovery must fence its Candidate image"),
    };
    assert_eq!(image.history.transition.as_ref().unwrap().phase, EngramAuthorityPhase::Candidate);
    assert_eq!(image.selection.as_ref(), Some(&entry));
    assert!(matches!(batch.pending[0].completion.poll_at(batch.pending[0].completion.deadline),
        Some(Err(PersistFenceError::Deadline))));
    claimed.state.engram_budget_clock().notify();
    let error = task.join().unwrap().expect_err("an expired Candidate is not an ACK");
    assert!(error.is_enqueued_durability_deadline());
    if explicit {
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(audit(&claimed).len(), 1);
    } else {
        assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR, "maintenance stays private");
        assert!(audit(&claimed).is_empty());
    }
    let late = collect_persist_delta_from_shared_state(&claimed.state.inner, 0);
    persist_delta_with_fences(&mut cache, claimed.state.persistence_path.as_path(), &late, &mut batch).unwrap();
    assert!(retained(&claimed, &entry));
    (claimed, entry, image)
}

fn close_retained_candidate_run(claimed: &ClaimedRoot, entry: &EngramWorkSourceRoot) {
    let mut transport = claimed.transport.named_roots.lock().unwrap();
    let proof = transport.as_mut().unwrap().root_reads.get_mut(&entry.claim_id).unwrap();
    proof["run"]["state"] = json!("completed");
    proof["read_cut"]["position"] = json!(proof["read_cut"]["position"].as_i64().unwrap() + 1);
}

fn retained_candidate_retry(published: bool) {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let label = if published { "release-published-retry" } else { "release-candidate-retry" };
    let (claimed, entry, image) = late_retained_candidate(label, true);
    let original = audit(&claimed)[0].clone();
    let owner = image.history.transition.as_ref().unwrap();
    reload_release_fixture(&claimed);
    if published {
        claimed.state.recover_engram_authority_candidate_until(&entry.store, &entry.work_id,
            claimed.state.engram_budget_clock().now() + ENGRAM_NAMED_ROOT_RELEASE_AUTHORITY_BUDGET).unwrap();
        assert_eq!(release_owner(&claimed, &entry).phase, EngramAuthorityPhase::Published);
    }
    close_retained_candidate_run(&claimed, &entry);
    let released = claimed.state.release_engram_named_root(Some("session-operator"),
        release_request(&entry, "completed original lane")).unwrap();
    assert!(released.released);
    assert_eq!(released.resolution, "resumed");
    assert!(!retained(&claimed, &entry));
    let records = audit(&claimed);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, original.id);
    assert_eq!(records[0].authority_owner.as_deref(), Some(owner.id.as_str()));
    assert!(records[0].acknowledged);
    let current = release_owner(&claimed, &entry);
    assert_eq!(current.id, owner.id);
    assert_eq!(current.version, owner.version);
    let inner = claimed.state.inner.lock().unwrap();
    assert_eq!(inner.engram_work_naming_history.iter().flat_map(|history| &history.retirements)
        .filter(|notice| notice.selection == entry).count(), 1);
}

#[test]
fn durability_pending_review_release_candidate_retry_after_reload() {
    retained_candidate_retry(false);
}

#[test]
fn durability_pending_review_release_published_retry_after_reload() {
    retained_candidate_retry(true);
}

#[test]
fn durability_pending_review_release_live_reread_keeps_original_audit() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (claimed, entry, image) = late_retained_candidate("release-candidate-live", true);
    let original = audit(&claimed)[0].clone();
    reload_release_fixture(&claimed);
    let error = claimed.state.release_engram_named_root(Some("session-operator"),
        release_request(&entry, "completed original lane")).unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("canonical facts still retain"), "{}", error.message);
    assert!(retained(&claimed, &entry));
    assert_eq!(audit(&claimed), vec![original]);
    let current = release_owner(&claimed, &entry);
    let owner = image.history.transition.as_ref().unwrap();
    assert_eq!(current.id, owner.id);
    assert_eq!(current.version, owner.version);
}

#[tokio::test]
async fn durability_pending_review_prior_candidate_has_no_phantom_release_audit() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (mut claimed, entry, image) = late_retained_candidate("release-prior-candidate", false);
    let owner = image.history.transition.as_ref().unwrap();
    let (tx, rx) = mpsc::channel();
    claimed.state.persist_tx = tx;
    let state = claimed.state.clone();
    let request = release_request(&entry, "completed original lane");
    let worker = std::thread::spawn(move || state.release_engram_named_root(Some("session-operator"), request));
    let mut batch = PersistFenceBatch::default();
    receive_naming_authority_fence(&rx, &mut batch, &worker);
    assert!(matches!(&batch.pending[0].target,
        PersistFenceTarget::EngramWorkAuthority(candidate)
            if candidate.history.transition.as_ref() == Some(owner)));
    let audit_count_before_reply = audit(&claimed).len();
    assert!(matches!(batch.pending[0].completion.poll_at(batch.pending[0].completion.deadline),
        Some(Err(PersistFenceError::Deadline))));
    claimed.state.engram_budget_clock().notify();
    let error = worker.join().unwrap().unwrap_err();
    let response = error.into_response();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8192).await.unwrap();
    let pending: Value = serde_json::from_slice(&bytes).unwrap();
    let late = collect_persist_delta_from_shared_state(&claimed.state.inner, 0);
    let mut cache = SqlitePersistConnectionCache::new();
    persist_delta_with_fences(&mut cache, claimed.state.persistence_path.as_path(), &late, &mut batch).unwrap();
    drop(rx);
    let disk = load_state(claimed.state.persistence_path.as_path()).unwrap().unwrap();
    assert_eq!(audit_count_before_reply, 0, "no audit before the older Candidate settles");
    assert!(disk.engram_named_root_release_audit.is_empty());
    assert!(disk.engram_work_source_roots.contains(&entry));
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{pending}");
    assert_eq!(pending["contract"], "termal-durability-pending");
    assert_eq!(pending["operation"], "named_root_release");
    assert_eq!(pending["identity"]["auditId"], Value::Null);
    assert_eq!(pending["identity"]["ownerId"], owner.id);
    assert_eq!(pending["identity"]["ownerVersion"], owner.version);
    assert_eq!(pending["identity"]["releaseStage"], "prior_authority");
    assert_eq!(pending["identity"]["requestedSelection"]["generation"], entry.generation);
    assert!(durability_pending_for_request(status, "POST",
        "/api/sessions/session-operator/engram-named-root-release", "session-operator",
        &json!({"work": entry.short_ref, "generation": entry.generation, "reason": "completed original lane"}),
        &pending).is_some(), "the prior-Candidate wire identity must not be flattened by MCP");
    assert_eq!(pending["retry"]["mode"], "same_request");
    reload_release_fixture_for(&claimed, &entry);
    close_retained_candidate_run(&claimed, &entry);
    let released = claimed.state.release_engram_named_root(Some("session-operator"),
        release_request(&entry, "completed original lane")).unwrap();
    assert!(released.released);
    assert_eq!(released.resolution, "applied_fresh");
    assert_eq!(audit(&claimed).len(), 1);
}

#[test]
fn durability_pending_review_release_changed_candidate_binding_conflicts() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (claimed, entry, _) = late_retained_candidate("release-candidate-binding", true);
    let original = audit(&claimed)[0].clone();
    reload_release_fixture(&claimed);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        inner.engram_work_naming_history.iter_mut()
            .find(|history| history.store == entry.store && history.work_id == entry.work_id).unwrap()
            .transition.as_mut().unwrap().binding.work_revision += 1;
    }
    let owner = release_owner(&claimed, &entry);
    let before = claimed.transport.requests().len();
    let error = claimed.state.release_engram_named_root(Some("session-operator"),
        release_request(&entry, "completed original lane")).unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains(&original.id));
    assert_eq!(claimed.transport.requests().len(), before, "no reread under a changed binding");
    assert_eq!(audit(&claimed), vec![original]);
    assert_eq!(release_owner(&claimed, &entry), owner);
    assert!(retained(&claimed, &entry));
}

#[test]
fn durability_pending_review_old_candidate_ack_cannot_ack_rearmed_read() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (mut claimed, entry, _) = late_retained_candidate("release-candidate-stale-ack", true);
    let original = audit(&claimed)[0].clone();
    reload_release_fixture(&claimed);
    close_retained_candidate_run(&claimed, &entry);
    let old_image = {
        let inner = claimed.state.inner.lock().unwrap();
        EngramAuthorityImage::capture(&inner, &entry.store, &entry.work_id).unwrap()
    };
    assert_eq!(old_image.history.transition.as_ref().unwrap().phase, EngramAuthorityPhase::Candidate);
    // Capture an actual selected delta, not a synthetic fence success.
    let prior_delta = collect_persist_delta_from_shared_state(&claimed.state.inner, 0);
    assert!(old_image.matches_metadata(&prior_delta.metadata));
    let reads_before = claimed.transport.requests().iter()
        .filter(|request| request.request["operation"] == "named_root_read").count();
    let (tx, rx) = mpsc::channel();
    claimed.state.persist_tx = tx;
    let state = claimed.state.clone();
    let root = entry.clone();
    let worker = std::thread::spawn(move || state.release_engram_named_root(Some("session-operator"),
        release_request(&root, "completed original lane")));
    let mut batch = PersistFenceBatch::default();
    let mut cache = SqlitePersistConnectionCache::new();
    // First finish the already-existing Candidate, without a canonical read.
    receive_naming_authority_fence(&rx, &mut batch, &worker);
    assert!(matches!(&batch.pending[0].target,
        PersistFenceTarget::EngramWorkAuthority(image) if **image == old_image));
    let candidate = collect_persist_delta_from_shared_state(&claimed.state.inner, 0);
    persist_delta_with_fences(&mut cache, claimed.state.persistence_path.as_path(), &candidate, &mut batch).unwrap();
    // Its same owner must now obtain a NEW full Prepared-image ACK.
    receive_naming_authority_fence(&rx, &mut batch, &worker);
    assert!(matches!(&batch.pending[0].target,
        PersistFenceTarget::EngramWorkAuthority(image)
            if image.history.transition.as_ref().is_some_and(|owner|
                owner.phase == EngramAuthorityPhase::Prepared
                    && owner.id == old_image.history.transition.as_ref().unwrap().id
                    && owner.version == old_image.history.transition.as_ref().unwrap().version)));
    assert!(!batch.pending[0].target.is_in_delta(&prior_delta));
    let clock = claimed.state.engram_budget_clock();
    let deadline = clock.now() + ENGRAM_NAMED_ROOT_RELEASE_AUTHORITY_BUDGET;
    let (stale, waiter) = PersistFence::new_with_clock(
        PersistFenceTarget::EngramWorkAuthority(Box::new(old_image)), deadline, clock);
    batch.accept(PersistRequest::Fence(Box::new(stale)));
    // A delayed old snapshot can really commit/ACK its own image, but cannot
    // ACK the new guard or permit its canonical read/publication.
    persist_delta_with_fences(&mut cache, claimed.state.persistence_path.as_path(), &prior_delta, &mut batch).unwrap();
    assert!(matches!(waiter.wait_until(deadline), Some(Ok(()))));
    assert_eq!(batch.pending.len(), 1);
    assert!(!worker.is_finished());
    assert_eq!(claimed.transport.requests().iter()
        .filter(|request| request.request["operation"] == "named_root_read").count(), reads_before);
    let guarded = collect_persist_delta_from_shared_state(&claimed.state.inner, 0);
    persist_delta_with_fences(&mut cache, claimed.state.persistence_path.as_path(), &guarded, &mut batch).unwrap();
    let started = std::time::Instant::now();
    while !worker.is_finished() {
        assert!(started.elapsed() < phase_sync::DEADLOCK_GUARD, "release retry writer stalled");
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(request) => {
                let fence = matches!(request, PersistRequest::Fence(_));
                batch.accept(request);
                if fence {
                    let delta = collect_persist_delta_from_shared_state(&claimed.state.inner, 0);
                    persist_delta_with_fences(&mut cache, claimed.state.persistence_path.as_path(), &delta, &mut batch).unwrap();
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(error) => panic!("release retry writer disconnected: {error}"),
        }
    }
    let released = worker.join().unwrap().unwrap();
    assert!(released.released);
    assert_eq!(audit(&claimed).len(), 1);
    assert_eq!(audit(&claimed)[0].id, original.id);
    assert!(!retained(&claimed, &entry));
}
