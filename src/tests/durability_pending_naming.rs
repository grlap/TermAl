// Naming's durable intent and generation belong to the unchanged retry.
use super::*;

#[test]
fn durability_pending_orphaned_withholding_reguard_preserves_classification() {
    withholding_reguard(true, false);
}

#[test]
fn durability_pending_orphaned_withholding_legacy_adapter_keeps_transport_class() {
    withholding_reguard(false, false);
}

#[test]
fn durability_pending_orphaned_withholding_rechecks_admission_after_ack() {
    withholding_reguard(true, true);
}

// This is helper-layer coverage: a missing guard reaches the real withholding
// re-guard. It does not claim a second reachable naming endpoint interleaving.
fn withholding_reguard(classified: bool, replace_binding: bool) {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let claimed = ClaimedRoot::new_scripted("withholding-reguard", Vec::new());
    prepare_claimed_root_naming(&claimed, "withholding-reguard");
    claimed_root_store(&claimed);
    claimed.record(|record| record.engram.named_root = Some(EngramNamedRootState::None));
    let mut state = claimed.state.clone();
    let (tx, rx) = mpsc::channel();
    state.persist_tx = tx;
    let target = claimed_root_target(&claimed);
    let worker_state = state.clone();
    let worker_target = target.clone();
    let worker = std::thread::spawn(move || {
        let deadline = worker_state.engram_budget_clock().now() + Duration::from_secs(2);
        let token = worker_target.routing_token.as_deref().unwrap();
        if classified {
            worker_state.apply_engram_root_withholding_classified_until(
                &worker_target,
                token,
                worker_target.work_binding.as_ref(),
                None,
                None,
                deadline,
            )
        } else {
            worker_state
                .apply_engram_root_withholding_until(
                    &worker_target,
                    token,
                    worker_target.work_binding.as_ref(),
                    None,
                    None,
                    deadline,
                )
                .map_err(EngramRootReconcileFailure::AdmissionInvalid)
        }
    });
    let mut batch = PersistFenceBatch::default();
    receive_naming_authority_fence(&rx, &mut batch, &worker);
    assert_eq!(
        claimed.record(|record| record.engram.named_root.clone()),
        Some(EngramNamedRootState::None),
        "Unknown cannot be installed before a real re-guard ACK"
    );
    let late = collect_persist_delta_from_shared_state(&state.inner, 0);
    let mut cache = SqlitePersistConnectionCache::new();
    if replace_binding {
        claimed.record(|record| record.engram.routing_token = Some("successor-token".to_owned()));
        persist_delta_with_fences(
            &mut cache,
            state.persistence_path.as_path(),
            &late,
            &mut batch,
        )
        .unwrap();
    } else {
        assert!(matches!(
            batch.pending[0]
                .completion
                .poll_at(batch.pending[0].completion.deadline),
            Some(Err(PersistFenceError::Deadline))
        ));
        state.engram_budget_clock().notify();
    }
    let error = match worker.join().unwrap() {
        Err(error) => error,
        Ok(()) => panic!("unacknowledged or superseded admission cannot install Unknown"),
    };
    assert_eq!(
        claimed.record(|record| record.engram.named_root.clone()),
        Some(EngramNamedRootState::None)
    );
    if replace_binding {
        let error = error.into_transport();
        assert_eq!(error.kind, EngramTransportErrorKind::LocalState);
        assert_eq!(
            error.message,
            "source uncertainty has no current acknowledged recovery guard"
        );
    } else if classified {
        match error {
            EngramRootReconcileFailure::Infrastructure(error) => {
                assert!(error.is_enqueued_durability_deadline());
            }
            _ => panic!("the classified re-guard must retain the actual local Deadline"),
        }
    } else {
        let error = error.into_transport();
        assert_eq!(error.kind, EngramTransportErrorKind::Transport);
        assert!(
            error
                .message
                .contains("persistence is unconfirmed (Deadline)")
        );
    }
    if !replace_binding {
        persist_delta_with_fences(
            &mut cache,
            state.persistence_path.as_path(),
            &late,
            &mut batch,
        )
        .unwrap();
        let loaded = load_state(state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        assert!(loaded.engram_work_naming_history.iter().any(|history| {
            history
                .transition
                .as_ref()
                .is_some_and(|owner| owner.phase == EngramAuthorityPhase::Prepared)
        }));
        assert!(loaded.engram_work_source_roots.is_empty());
    }
}

#[test]
fn durability_pending_orphaned_legacy_reconcile_keeps_origin_categories() {
    // Real accepted fences through the old wrapper: publication must remain
    // protocol, not become transport merely because it carries an ApiError.
    for publication in [false, true] {
        for failure in [
            PersistFenceError::Deadline,
            PersistFenceError::WorkerStopped,
            PersistFenceError::WriteFailed("legacy writer failure".to_owned()),
            PersistFenceError::Superseded,
        ] {
            let _scope = TestTempRootDirectoryScope::repository_local();
            let claimed = ClaimedRoot::new_scripted("legacy-reconcile", Vec::new());
            prepare_claimed_root_naming(&claimed, "legacy-reconcile");
            claimed_root_store(&claimed);
            let target = claimed_root_target(&claimed);
            let mut state = claimed.state.clone();
            let (tx, rx) = mpsc::channel();
            state.persist_tx = tx;
            let fence = EngramRootReadFence::capture(&state.inner.lock().unwrap());
            let worker_state = state.clone();
            let worker = std::thread::spawn(move || {
                worker_state.reconcile_engram_named_root_with_fence_until(
                    &target.connection.session_id,
                    target.routing_token.as_deref().unwrap(),
                    target.work_binding.as_ref(),
                    Some(EngramNamedRootState::None),
                    &fence,
                    Some(&target.connection),
                    worker_state.engram_budget_clock().now() + Duration::from_secs(2),
                )
            });
            let mut batch = PersistFenceBatch::default();
            let mut cache = SqlitePersistConnectionCache::new();
            receive_naming_authority_fence(&rx, &mut batch, &worker);
            if publication {
                let delta = collect_persist_delta_from_shared_state(&state.inner, 0);
                persist_delta_with_fences(
                    &mut cache,
                    state.persistence_path.as_path(),
                    &delta,
                    &mut batch,
                )
                .unwrap();
                receive_naming_authority_fence(&rx, &mut batch, &worker);
                assert!(
                    matches!(&batch.pending[0].target, PersistFenceTarget::EngramWorkAuthority(image)
                    if image.history.transition.as_ref().is_some_and(|owner| owner.phase == EngramAuthorityPhase::Candidate))
                );
            }
            if failure == PersistFenceError::Deadline {
                assert!(matches!(
                    batch.pending[0]
                        .completion
                        .poll_at(batch.pending[0].completion.deadline),
                    Some(Err(PersistFenceError::Deadline))
                ));
            } else {
                batch.fail(failure.clone());
            }
            state.engram_budget_clock().notify();
            let error = worker
                .join()
                .unwrap()
                .expect_err("the legacy wrapper cannot acknowledge a failed fence");
            assert_eq!(
                error.kind,
                if publication {
                    EngramTransportErrorKind::Protocol
                } else {
                    EngramTransportErrorKind::Transport
                }
            );
            if failure == PersistFenceError::Superseded {
                assert_eq!(
                    error.message,
                    "named-root acknowledgement belongs to a superseded authority image"
                );
            } else {
                assert!(
                    error.message.contains(&format!("({failure:?})")),
                    "{}",
                    error.message
                );
            }
        }
    }
    for reason in [
        EngramRootEvidenceUncertainty::CanonicalRead(ApiError::bad_gateway("plain RPC timeout")),
        EngramRootEvidenceUncertainty::CanonicalHistory(ApiError::conflict("history changed")),
        EngramRootEvidenceUncertainty::Publication(ApiError::internal(
            "ordinary publication failure",
        )),
    ] {
        let error = EngramRootReconcileFailure::EvidenceWithheld(reason).into_transport();
        assert_eq!(error.kind, EngramTransportErrorKind::Protocol);
    }
}

#[tokio::test]
async fn durability_pending_orphaned_naming_guard_retains_its_exact_intent() {
    orphaned_naming_fence(false, false, None).await;
}

#[tokio::test]
async fn durability_pending_orphaned_naming_publication_retains_its_exact_intent() {
    orphaned_naming_fence(true, false, None).await;
}

#[tokio::test]
async fn durability_pending_orphaned_clear_publication_retains_its_exact_intent() {
    orphaned_naming_fence(true, true, None).await;
}

#[tokio::test]
async fn durability_pending_orphaned_naming_writer_failures_stay_untyped() {
    for publication in [false, true] {
        for failure in [
            PersistFenceError::WorkerStopped,
            PersistFenceError::WriteFailed("orphan writer failure".to_owned()),
            PersistFenceError::Superseded,
        ] {
            orphaned_naming_fence(publication, false, Some(failure)).await;
        }
    }
}

async fn orphaned_naming_fence(publication: bool, clear: bool, failure: Option<PersistFenceError>) {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let label = "pending-orphaned-naming";
    let mut claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_claimed_root_naming(&claimed, label);
    if clear {
        name_root(
            &claimed,
            label,
            Some(&worktree),
            vec![claimed_root_held(label)],
        )
        .unwrap();
    }
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .lose_next_reply = true;
    name_root(
        &claimed,
        label,
        if clear { None } else { Some(&worktree) },
        vec![claimed_root_held(label)],
    )
    .unwrap_err();
    let original = claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_named_root_journal[0]
        .pending
        .clone()
        .unwrap();
    let reporter = claimed.session_id.clone();
    let project = claimed.record(|record| record.session.project_id.clone().unwrap());
    claimed.session_id =
        create_test_project_session(&claimed.state, Agent::Codex, &project, &claimed.root);
    prepare_claimed_root_naming(&claimed, label);
    let status_before = claimed
        .transport
        .requests()
        .iter()
        .filter(|request| request.request["operation"] == "session_status")
        .count();
    let binds_before = claimed
        .transport
        .requests()
        .iter()
        .filter(|request| request.request["operation"] == "named_root_bind")
        .count();
    let (tx, rx) = mpsc::channel();
    claimed.state.persist_tx = tx;
    let state = claimed.state.clone();
    let transport = claimed.transport.clone();
    let session = claimed.session_id.clone();
    let worker_tree = worktree.clone();
    let orphaned = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let orphaned_in_worker = orphaned.clone();
    let worker = std::thread::spawn(move || {
        let state = claimed.state.clone();
        // A real reporter can disappear while held-claims is off lock. Keep
        // it available during preparatory maintenance, then exercise the
        // endpoint's orphan branch rather than a maintenance-only recovery.
        TEST_ENGRAM_AFTER_SOURCE_ROOT_HELD_READ.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&reporter).unwrap();
                inner.remove_session_at(index);
                orphaned_in_worker.store(true, std::sync::atomic::Ordering::SeqCst);
            }));
        });
        let result = name_root(
            &claimed,
            label,
            if clear { None } else { Some(&worker_tree) },
            vec![claimed_root_held(label)],
        );
        (claimed, result)
    });
    let mut batch = PersistFenceBatch::default();
    let mut cache = SqlitePersistConnectionCache::new();
    let mut observed = None;
    for _ in 0..32 {
        receive_naming_authority_fence(&rx, &mut batch, &worker);
        let after_status = orphaned.load(std::sync::atomic::Ordering::SeqCst)
            && transport
                .requests()
                .iter()
                .filter(|request| request.request["operation"] == "session_status")
                .count()
                > status_before;
        let image = match &batch.pending[0].target {
            PersistFenceTarget::EngramWorkAuthority(image) => Some(image.as_ref()),
            _ => None,
        };
        let target = after_status
            && image.is_some_and(|image| {
                image.history.transition.as_ref().is_some_and(|owner| {
                    owner.phase
                        == if publication {
                            EngramAuthorityPhase::Candidate
                        } else {
                            EngramAuthorityPhase::Prepared
                        }
                }) && image.journals.iter().any(|journal| {
                    if publication {
                        journal
                            .reconciliation
                            .as_ref()
                            .is_some_and(|read| read.intent == original)
                            && journal.pending.is_none()
                    } else {
                        journal.pending.as_ref() == Some(&original)
                    }
                })
            });
        let delta = collect_persist_delta_from_shared_state(&state.inner, 0);
        if target {
            let image = image.unwrap().clone();
            if let Some(failure) = failure.clone() {
                batch.fail(failure);
            } else {
                assert!(matches!(
                    batch.pending[0]
                        .completion
                        .poll_at(batch.pending[0].completion.deadline),
                    Some(Err(PersistFenceError::Deadline))
                ));
            }
            state.engram_budget_clock().notify();
            observed = Some((image, delta));
            break;
        }
        persist_delta_with_fences(
            &mut cache,
            state.persistence_path.as_path(),
            &delta,
            &mut batch,
        )
        .unwrap();
    }
    if observed.is_none() {
        batch.fail(PersistFenceError::WriteFailed(
            "orphan phase not reached".to_owned(),
        ));
        state.engram_budget_clock().notify();
        drop(rx);
    }
    let (mut claimed, result) = worker.join().unwrap();
    let (image, late) =
        observed.expect("the orphan branch reaches its own guarded phase AFTER SessionStatus");
    let response = match result {
        Err(error) => error.into_response(),
        Ok(_) => panic!("an unacknowledged orphan phase is not successful naming"),
    };
    let status = response.status();
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        transport
            .requests()
            .iter()
            .filter(|request| request.request["operation"] == "named_root_bind")
            .count(),
        binds_before,
        "recovery must not replay an unavailable reporter's send"
    );
    assert_eq!(
        state.engram_root_capture(&session),
        EngramRootCapture::Unconfirmed
    );
    if failure.is_some() {
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
        assert!(body.get("contract").is_none(), "{body}");
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .starts_with("recovering orphaned root intent:")
        );
        return;
    }
    // A timeout is not an ACK: commit the exact real image after the caller
    // returned, then load SQLite into a different AppState before retrying.
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &late,
        &mut batch,
    )
    .unwrap();
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let journal = loaded
        .engram_named_root_journal
        .iter()
        .find(|journal| journal.claim_id == original.root.claim_id)
        .unwrap();
    if publication {
        assert!(journal.pending.is_none());
        assert_eq!(journal.reconciliation.as_ref().unwrap().intent, original);
        assert!(
            journal
                .confirmed
                .as_ref()
                .is_none_or(|(event, _)| event != &original),
            "canonical readback must not invent the lost event receipt"
        );
    } else {
        assert_eq!(journal.pending.as_ref(), Some(&original));
    }
    reload_pending_naming(&mut claimed, loaded);
    let retried = name_root(
        &claimed,
        label,
        if clear { None } else { Some(&worktree) },
        vec![claimed_root_held(label)],
    );
    match retried {
        Err(error) => assert_eq!(error.status, StatusCode::CONFLICT, "{}", error.message),
        Ok(current) if clear => assert!(current.root.is_none()),
        Ok(current) => assert!(
            current.generation > original.root.generation,
            "a fresh application after durable recovery is not the orphan's old ACK"
        ),
    }
    assert_eq!(
        transport
            .requests()
            .iter()
            .filter(|request| request.request["operation"] == "named_root_bind"
                && request.request["generation"] == original.root.generation)
            .count(),
        binds_before,
        "a reload retry never replays the orphan's old generation"
    );
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["contract"], "termal-durability-pending");
    assert_eq!(body["version"], 1);
    assert_eq!(body["operation"], "named_root_naming");
    assert_eq!(body["identity"]["workId"], original.root.work_id);
    assert_eq!(body["identity"]["claimId"], original.root.claim_id);
    assert_eq!(body["identity"]["generation"], original.root.generation);
    assert_eq!(
        body["identity"]["action"],
        if clear { "clear" } else { "name" }
    );
    assert_eq!(body["identity"]["sessionId"], session);
    let owner = image.history.transition.as_ref().unwrap();
    assert_eq!(body["identity"]["ownerId"], owner.id);
    assert_eq!(body["identity"]["ownerVersion"], owner.version);
    assert_eq!(body["retry"]["mode"], "same_request");
}

fn reload_pending_naming(claimed: &mut ClaimedRoot, loaded: StateInner) {
    let mut restarted = test_app_state();
    restarted.persistence_path = claimed.state.persistence_path.clone();
    restarted.inner = Arc::new(StateMutex::new(loaded));
    restarted.select_test_engram_budget_clock(EngramBudgetClock::scripted());
    install_control_only_transport(&restarted, claimed.transport.clone());
    assert!(!Arc::ptr_eq(&restarted.inner, &claimed.state.inner));
    claimed.state = restarted;
}

#[tokio::test]
async fn durability_pending_naming_first_guard_has_no_invented_generation_after_reload() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let label = "pending-first-guard";
    let mut claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_claimed_root_naming(&claimed, label);
    let (tx, rx) = mpsc::channel();
    claimed.state.persist_tx = tx;
    let state = claimed.state.clone();
    let tree = worktree.clone();
    let worker = std::thread::spawn(move || {
        let result = name_root(&claimed, label, Some(&tree), vec![claimed_root_held(label)]);
        (claimed, result)
    });
    let mut batch = PersistFenceBatch::default();
    let mut cache = SqlitePersistConnectionCache::new();
    receive_naming_authority_fence(&rx, &mut batch, &worker);
    let owner = match &batch.pending[0].target {
        PersistFenceTarget::EngramWorkAuthority(image) => {
            assert!(image.selection.is_none());
            assert!(
                image
                    .journals
                    .iter()
                    .all(|journal| journal.pending.is_none())
            );
            image.history.transition.as_ref().unwrap().clone()
        }
        _ => panic!("the first-contact guard owns an exact authority image"),
    };
    let late = collect_persist_delta_from_shared_state(&state.inner, 0);
    assert!(matches!(
        batch.pending[0]
            .completion
            .poll_at(batch.pending[0].completion.deadline),
        Some(Err(PersistFenceError::Deadline))
    ));
    state.engram_budget_clock().notify();
    let (mut claimed, result) = worker.join().unwrap();
    let response = result
        .expect_err("an unacknowledged first guard is not a named root")
        .into_response();
    let status = response.status();
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        !claimed
            .transport
            .requests()
            .iter()
            .any(|request| request.request["operation"] == "named_root_bind")
    );
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &late,
        &mut batch,
    )
    .unwrap();
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(loaded.engram_work_source_roots.is_empty());
    assert!(
        loaded
            .engram_named_root_journal
            .iter()
            .all(|journal| journal.pending.is_none())
    );
    reload_pending_naming(&mut claimed, loaded);
    // This is a fresh application under the fixture's current held claim,
    // not acknowledgement of a generation the old guard never allocated.
    let named = name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    assert!(named.generation > 0);
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["operation"], "named_root_naming");
    assert_eq!(body["identity"]["generation"], Value::Null);
    assert!(body["identity"].get("generation").is_some());
    assert_eq!(body["identity"]["ownerId"], owner.id);
    assert_eq!(body["identity"]["ownerVersion"], owner.version);
}

#[tokio::test]
async fn durability_pending_naming_replays_its_retained_generation_after_a_late_intent_commit() {
    naming_late_intent_commit(false).await;
}

#[tokio::test]
async fn durability_pending_review_naming_clock_expiry_preserves_pending_contract() {
    naming_late_intent_commit(true).await;
}

async fn naming_late_intent_commit(advance_clock: bool) {
    let _temp_scope = TestTempRootDirectoryScope::repository_local();
    let label = "durability-pending-naming";
    let mut claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_claimed_root_naming(&claimed, label);
    let (tx, rx) = mpsc::channel();
    claimed.state.persist_tx = tx;
    let state = claimed.state.clone();
    let session = claimed.session_id.clone();
    let transport = claimed.transport.clone();
    let worker_tree = worktree.clone();
    let worker = std::thread::spawn(move || {
        let result = name_root(
            &claimed,
            label,
            Some(&worker_tree),
            vec![claimed_root_held(label)],
        );
        (claimed, result)
    });
    let mut batch = PersistFenceBatch::default();
    let mut cache = SqlitePersistConnectionCache::new();
    // First-contact protection must commit before the naming intent exists.
    receive_naming_authority_fence(&rx, &mut batch, &worker);
    let guard = collect_persist_delta_from_shared_state(&state.inner, 0);
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &guard,
        &mut batch,
    )
    .unwrap();
    receive_naming_authority_fence(&rx, &mut batch, &worker);
    let intent = state.inner.lock().unwrap().engram_named_root_journal[0]
        .pending
        .clone()
        .unwrap();
    assert!(!worker.is_finished());
    assert!(
        !transport
            .requests()
            .iter()
            .any(|r| r.request["operation"] == "named_root_bind")
    );
    assert_eq!(
        state.engram_root_capture(&session),
        EngramRootCapture::Unconfirmed
    );
    assert!(matches!(
        batch.pending[0]
            .completion
            .poll_at(batch.pending[0].completion.deadline),
        Some(Err(PersistFenceError::Deadline))
    ));
    let clock = state.engram_budget_clock();
    if advance_clock {
        clock.advance(
            batch.pending[0]
                .completion
                .deadline
                .saturating_duration_since(clock.now()),
        );
    } else {
        clock.notify();
    }
    let (mut claimed, result) = worker.join().unwrap();
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("an unacknowledged naming intent cannot publish authority"),
    };
    let response = error.into_response();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    let pending: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        !transport
            .requests()
            .iter()
            .any(|r| r.request["operation"] == "named_root_bind")
    );
    assert_eq!(
        state.engram_root_capture(&session),
        EngramRootCapture::Unconfirmed
    );
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
    assert_eq!(
        disk.engram_named_root_journal[0].pending.as_ref(),
        Some(&intent)
    );

    let state = if !advance_clock {
        let mut restarted = test_app_state();
        restarted.persistence_path = state.persistence_path.clone();
        restarted.persist_tx = state.persist_tx.clone();
        restarted.inner = Arc::new(StateMutex::new(disk));
        restarted.select_test_engram_budget_clock(EngramBudgetClock::scripted());
        install_control_only_transport(&restarted, transport.clone());
        assert!(!Arc::ptr_eq(&state.inner, &restarted.inner));
        let index = restarted
            .inner
            .lock()
            .unwrap()
            .find_session_index(&session)
            .unwrap();
        assert!(
            restarted.inner.lock().unwrap().sessions[index]
                .engram
                .work_binding
                .is_none(),
            "boot does not certify the caller's current claim from an old local mirror"
        );
        claimed.state = restarted;
        claimed.state.clone()
    } else {
        state
    };

    // The same request is explicitly resent. Step only real fences from the
    // production route, including recovery fences, until its result arrives.
    let worker = std::thread::spawn(move || {
        let result = name_root(
            &claimed,
            label,
            Some(&worktree),
            vec![claimed_root_held(label)],
        );
        (claimed, result)
    });
    let started = std::time::Instant::now();
    while !worker.is_finished() {
        assert!(
            started.elapsed() < phase_sync::DEADLOCK_GUARD,
            "naming retry fixture deadlocked"
        );
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(request) => {
                let is_fence = matches!(request, PersistRequest::Fence(_));
                batch.accept(request);
                if is_fence {
                    let delta = collect_persist_delta_from_shared_state(&state.inner, 0);
                    persist_delta_with_fences(
                        &mut cache,
                        state.persistence_path.as_path(),
                        &delta,
                        &mut batch,
                    )
                    .unwrap();
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(error) => panic!("naming writer disconnected: {error}"),
        }
    }
    let (_claimed, named) = worker.join().unwrap();
    let named = named.unwrap();
    assert_eq!(
        named.generation, intent.root.generation,
        "retry must not mint a replacement generation"
    );
    let disk = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let journal = &disk.engram_named_root_journal[0];
    assert_eq!(journal.confirmed.as_ref().unwrap().0, intent);
    assert!(journal.pending.is_none());
    assert_eq!(
        transport
            .requests()
            .iter()
            .filter(|r| r.request["operation"] == "named_root_bind")
            .count(),
        1
    );
    assert_ne!(
        state.engram_root_capture(&session),
        EngramRootCapture::Unconfirmed
    );

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{pending}");
    assert_eq!(pending["contract"], "termal-durability-pending");
    assert_eq!(pending["version"], 1);
    assert_eq!(pending["code"], "durability_pending");
    assert_eq!(pending["outcome"], "pending");
    assert_eq!(pending["operation"], "named_root_naming");
    assert_eq!(pending["identity"]["workId"], intent.root.work_id);
    assert_eq!(pending["identity"]["claimId"], intent.root.claim_id);
    assert_eq!(pending["identity"]["generation"], intent.root.generation);
    assert_eq!(pending["identity"]["action"], "name");
    assert_eq!(pending["retry"]["mode"], "same_request");
}

#[tokio::test]
async fn durability_pending_review_clear_publication_keeps_original_generation() {
    let _temp_scope = TestTempRootDirectoryScope::repository_local();
    let label = "durability-pending-clear-publication";
    let mut claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let original = claimed.state.inner.lock().unwrap().engram_work_source_roots[0].clone();
    let (tx, rx) = mpsc::channel();
    claimed.state.persist_tx = tx;
    let state = claimed.state.clone();
    let worker = std::thread::spawn(move || {
        let result = name_root(&claimed, label, None, vec![claimed_root_held(label)]);
        (claimed, result)
    });
    let mut batch = PersistFenceBatch::default();
    let mut cache = SqlitePersistConnectionCache::new();
    let mut observed = None;
    for _ in 0..16 {
        receive_naming_authority_fence(&rx, &mut batch, &worker);
        let is_clear_candidate = matches!(&batch.pending[0].target,
            PersistFenceTarget::EngramWorkAuthority(image)
                if image.selection.is_none()
                    && image.history.transition.as_ref().is_some_and(|owner|
                        owner.phase == EngramAuthorityPhase::Candidate)
                    && image.journals.iter().any(|journal| journal.confirmed.as_ref()
                        .is_some_and(|(event, _)| event.kind == EngramNamedRootKind::Ended
                            && event.root == original)));
        if is_clear_candidate {
            observed = Some(collect_persist_delta_from_shared_state(&state.inner, 0));
            assert!(matches!(
                batch.pending[0]
                    .completion
                    .poll_at(batch.pending[0].completion.deadline),
                Some(Err(PersistFenceError::Deadline))
            ));
            state.engram_budget_clock().notify();
            break;
        }
        let delta = collect_persist_delta_from_shared_state(&state.inner, 0);
        persist_delta_with_fences(
            &mut cache,
            state.persistence_path.as_path(),
            &delta,
            &mut batch,
        )
        .unwrap();
    }
    // A broken driver must release its waiter before joining or asserting.
    if observed.is_none() {
        batch.fail(PersistFenceError::WriteFailed(
            "clear candidate was not reached".to_owned(),
        ));
        drop(rx);
    }
    let (mut claimed, result) = worker.join().unwrap();
    let late = observed.expect("the real clear reaches its final Candidate-image fence");
    let error = result.expect_err("a clear is not confirmed before its exact publication ACK");
    let response = error.into_response();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    let pending: Value = serde_json::from_slice(&bytes).unwrap();
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
    assert!(!disk.engram_work_source_roots.contains(&original));
    assert!(disk.engram_named_root_journal.iter().any(|journal| {
        journal.confirmed.as_ref().is_some_and(|(event, _)| {
            event.kind == EngramNamedRootKind::Ended && event.root == original
        })
    }));
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{pending}");
    assert_eq!(pending["contract"], "termal-durability-pending");
    assert_eq!(pending["version"], 1);
    assert_eq!(pending["operation"], "named_root_naming");
    assert_eq!(pending["identity"]["action"], "clear");
    assert_eq!(pending["identity"]["generation"], original.generation);
    assert_eq!(pending["identity"]["claimId"], original.claim_id);
    assert_eq!(pending["retry"]["mode"], "same_request");
    let generation_counter = disk.engram_source_root_generation;
    let end_count = claimed
        .transport
        .requests()
        .iter()
        .filter(|request| {
            request.request["operation"] == "named_root_bind" && request.request["kind"] == "ended"
        })
        .count();
    assert_eq!(end_count, 1);
    reload_pending_naming(&mut claimed, disk);
    let cleared = name_root(&claimed, label, None, vec![claimed_root_held(label)]).unwrap();
    assert!(cleared.root.is_none());
    assert_eq!(cleared.generation, 0);
    assert_eq!(
        claimed
            .state
            .inner
            .lock()
            .unwrap()
            .engram_source_root_generation,
        generation_counter
    );
    assert_eq!(
        claimed
            .transport
            .requests()
            .iter()
            .filter(|request| request.request["operation"] == "named_root_bind"
                && request.request["kind"] == "ended")
            .count(),
        end_count,
        "a durable clear after reload is current state, not a second end event"
    );
    drop(claimed);
}
