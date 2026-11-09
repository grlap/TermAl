// Owns naming rejection controls at first admission, returned-lock learning,
// and recovery, plus the explicit-clear and kept-name deadline boundaries.
// Does not own canonical authority or the producer's transition semantics.
// Split from the naming supersession fixtures for the guard regression matrix.

#[test]
fn kept_naming_rejects_selection_and_journal_changes_during_its_proof_read() {
    for fault in ["replace", "remove", "journal"] {
        let (claimed, tree) = root_fixture(&format!("kept-read-{fault}"));
        let named = request_fixture_name(&claimed, Some(&tree)).unwrap();
        let store = naming_store(&claimed);
        let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
        let previous_line = claimed.record(|record| record.engram.pending_source_root_line.clone());
        let state = claimed.state.clone();
        let hook_store = store.clone();
        let hook_binding = binding.clone();
        let rival = Arc::new(Mutex::new(None));
        let observed_rival = rival.clone();
        let fired = std::sync::atomic::AtomicBool::new(false);
        claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary =
            Some(Arc::new(move |phase| {
                if phase != "before_kept_naming_proof"
                    || fired.swap(true, std::sync::atomic::Ordering::SeqCst)
                {
                    return;
                }
                {
                    let mut inner = state.inner.lock().unwrap();
                    if fault == "journal" {
                        let journal = inner.engram_named_root_journal.iter_mut()
                            .find(|journal| journal.store == hook_store
                                && journal.claim_id == hook_binding.claim_id).unwrap();
                        journal.obsolete = !journal.obsolete;
                    } else {
                        let replacement = if fault == "replace" {
                            let mut selected = engram_work_source_root_for_work(
                                &inner.engram_work_source_roots, &hook_store, &hook_binding.work_id,
                            ).unwrap().clone();
                            selected.generation += 50;
                            Some(selected)
                        } else {
                            None
                        };
                        engram_set_work_source_root(
                            &mut inner.engram_work_source_roots, &hook_store,
                            &hook_binding.work_id, replacement,
                        ).unwrap();
                    }
                }
                let owner = state.prepare_engram_authority(
                    &hook_store, &hook_binding, DEADLOCK_GUARD,
                ).unwrap();
                *observed_rival.lock().unwrap() = Some(owner);
            }));
        let result = request_fixture_name(&claimed, Some(&tree));
        claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = None;
        let error = result.expect_err("a kept proof must not adopt a changed local baseline");
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(error.message.starts_with("naming authority changed during canonical recovery"),
            "{fault}: {}", error.message);
        let rival = rival.lock().unwrap().take().expect("the pre-read boundary was reached");
        let inner = claimed.state.inner.lock().unwrap();
        let current_owner = inner.engram_work_naming_history.iter()
            .find(|history| history.store == store && history.work_id == binding.work_id)
            .unwrap().transition.as_ref().unwrap();
        assert_eq!(current_owner.id, rival.id, "refusal must not supersede the rival again");
        assert_eq!(current_owner.version, rival.version);
        let selected = engram_work_source_root_for_work(
            &inner.engram_work_source_roots, &store, &binding.work_id,
        );
        match fault {
            "replace" => assert_eq!(selected.unwrap().generation, named.generation + 50),
            "remove" => assert!(selected.is_none()),
            "journal" => {
                assert_eq!(selected.unwrap().generation, named.generation);
                assert!(engram_root_journal(
                    &inner.engram_named_root_journal, &store, &binding.claim_id,
                ).unwrap().obsolete);
            }
            _ => unreachable!(),
        }
        drop(inner);
        assert_eq!(claimed.record(|record| record.engram.pending_source_root_line.clone()), previous_line);
        assert_eq!(claimed.transport.requests().iter()
            .filter(|request| request.request["operation"] == "named_root_bind").count(), 1);
    }
}

fn disturb_naming_context(
    state: &AppState,
    session: &str,
    store: &EngramAuthorityStoreKey,
    binding: &EngramControlWorkBinding,
    fault: &str,
) {
    if fault == "runtime" {
        let (runtime, _receiver) = test_codex_runtime_handle("replacement-naming-runtime");
        let mut inner = state.inner.lock().unwrap();
        let record = inner.session_mut(session).unwrap();
        assert_ne!(
            record.runtime.runtime_token(),
            Some(RuntimeToken::Codex(runtime.runtime_id.clone()))
        );
        record.runtime = SessionRuntime::Codex(runtime);
        return;
    }
    let mut inner = state.inner.lock().unwrap();
    match fault {
        "binding" => {
            inner
                .session_mut(session)
                .unwrap()
                .engram
                .work_binding
                .as_mut()
                .unwrap()
                .claim_fence += 1
        }
        "connection" => inner
            .preferences
            .engram
            .developer_name
            .push_str("-replacement"),
        "settings" => {
            let settings = inner
                .projects
                .iter_mut()
                .find_map(|project| project.engram.as_mut())
                .unwrap();
            settings.deadline_ms = Some(settings.call_timeout().as_millis() as u64 + 1);
        }
        "selection" => {
            let mut entry =
                engram_root_journal(&inner.engram_named_root_journal, store, &binding.claim_id)
                    .unwrap()
                    .pending
                    .as_ref()
                    .unwrap()
                    .root
                    .clone();
            entry.generation += 50;
            engram_set_work_source_root(
                &mut inner.engram_work_source_roots,
                store,
                &binding.work_id,
                Some(entry),
            )
            .unwrap();
        }
        "journal" => {
            let journal = inner
                .engram_named_root_journal
                .iter_mut()
                .find(|journal| journal.store == *store && journal.claim_id == binding.claim_id)
                .unwrap();
            journal.obsolete = !journal.obsolete;
        }
        "pending" => inner
            .engram_named_root_journal
            .iter_mut()
            .find(|journal| journal.store == *store && journal.claim_id == binding.claim_id)
            .unwrap()
            .pending
            .as_mut()
            .unwrap()
            .reporter
            .push_str("-replacement"),
        _ => panic!("unknown context fault: {fault}"),
    }
}

fn naming_context_rejection(fault: &'static str, window: &'static str) {
    let (claimed, tree) = root_fixture(&format!("reject-{window}-{fault}"));
    let store = naming_store(&claimed);
    let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
    let state = claimed.state.clone();
    let session = claimed.session_id.clone();
    let original_event = Arc::new(Mutex::new(None));
    let event_seen = original_event.clone();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hook_calls = calls.clone();
    let armed = std::sync::atomic::AtomicBool::new(false);
    let hook_store = store.clone();
    let hook_binding = binding.clone();
    claimed
        .state
        .inner
        .lock()
        .unwrap()
        .test_engram_authority_ack_boundary = Some(Arc::new(move |phase| {
        if phase == "before_naming_selection" {
            armed.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        if !armed.load(std::sync::atomic::Ordering::SeqCst)
            || phase
                != if window == "selection" {
                    "before_naming_selection"
                } else {
                    "before_naming_learning_lock"
                }
        {
            return;
        }
        let call = hook_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if window == "recovery" && call == 1 {
            state
                .prepare_engram_authority(&hook_store, &hook_binding, DEADLOCK_GUARD)
                .unwrap();
            return;
        }
        if call != if window == "recovery" { 2 } else { 1 } {
            return;
        }
        let before = {
            let inner = state.inner.lock().unwrap();
            *event_seen.lock().unwrap() = engram_root_journal(
                &inner.engram_named_root_journal,
                &hook_store,
                &hook_binding.claim_id,
            )
            .unwrap()
            .pending
            .clone();
            inner
                .engram_work_naming_history
                .iter()
                .find(|history| {
                    history.store == hook_store && history.work_id == hook_binding.work_id
                })
                .unwrap()
                .transition
                .clone()
                .unwrap()
        };
        disturb_naming_context(&state, &session, &hook_store, &hook_binding, fault);
        let inner = state.inner.lock().unwrap();
        let after = inner
            .engram_work_naming_history
            .iter()
            .find(|history| history.store == hook_store && history.work_id == hook_binding.work_id)
            .unwrap()
            .transition
            .as_ref()
            .unwrap();
        assert_eq!(
            after.id, before.id,
            "context changes without superseding the local owner"
        );
        assert_eq!(after.version, before.version);
    }));
    let result = request_fixture_name(&claimed, Some(&tree));
    claimed
        .state
        .inner
        .lock()
        .unwrap()
        .test_engram_authority_ack_boundary = None;
    let error = result.expect_err("changed context must not install the requested selection");
    if window != "selection" && (fault == "connection" || fault == "selection") {
        assert!(
            error.message.starts_with("naming authority changed during canonical recovery"),
            "{window}/{fault}: the returned-lock helper must refuse before installation: {}",
            error.message
        );
    } else if window == "selection" && (fault == "selection" || fault == "pending") {
        assert!(
            error.message.starts_with("named-root selection changed before installation"),
            "{window}/{fault}: installation must reject the changed original selection or intent: {}",
            error.message
        );
    }
    assert_eq!(
        error.status,
        StatusCode::CONFLICT,
        "{window}/{fault}: {}",
        error.message
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        if window == "recovery" { 2 } else { 1 }
    );
    let event = original_event
        .lock()
        .unwrap()
        .take()
        .expect("the mutation boundary was reached");
    let inner = claimed.state.inner.lock().unwrap();
    assert!(
        inner
            .engram_work_source_roots
            .iter()
            .all(|entry| *entry != event.root),
        "refusal must not install the stale request or replace a competing selection"
    );
    if fault == "selection" {
        assert_eq!(
            inner.engram_work_source_roots[0].generation,
            event.root.generation + 50
        );
    } else {
        assert!(inner.engram_work_source_roots.is_empty());
    }
    let journal =
        engram_root_journal(&inner.engram_named_root_journal, &store, &binding.claim_id).unwrap();
    assert!(journal.confirmed.is_none());
    assert_eq!(journal.pending.as_ref().unwrap().root, event.root);
    assert_eq!(journal.read_binding.as_ref(), Some(&binding));
    drop(inner);
    assert_eq!(
        claimed
            .transport
            .requests()
            .iter()
            .filter(|request| request.request["operation"] == "named_root_bind")
            .count(),
        1
    );
}

#[test]
fn first_selection_learning_refuses_binding_or_runtime_changes_without_owner_supersession() {
    for fault in ["binding", "runtime"] {
        naming_context_rejection(fault, "selection");
    }
}

#[test]
fn naming_learning_rechecks_each_context_under_its_returned_lock() {
    for window in ["first", "recovery"] {
        for fault in [
            "binding",
            "runtime",
            "connection",
            "settings",
            "selection",
            "journal",
        ] {
            naming_context_rejection(fault, window);
        }
    }
}

#[test]
fn naming_installation_rechecks_the_selection_and_exact_pending_event_after_learning() {
    for fault in ["selection", "pending"] {
        naming_context_rejection(fault, "selection");
    }
}

#[test]
fn naming_owner_recovery_refuses_a_different_valid_canonical_proof() {
    let (claimed, tree) = root_fixture("recovery-proof-moved");
    let store = naming_store(&claimed);
    let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
    let state = claimed.state.clone();
    let transport = claimed.transport.clone();
    let changed = std::sync::atomic::AtomicBool::new(false);
    let hook_binding = binding.clone();
    let hook_store = store.clone();
    claimed
        .state
        .inner
        .lock()
        .unwrap()
        .test_engram_authority_ack_boundary = Some(Arc::new(move |phase| {
        if phase != "before_naming_selection"
            || changed.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        state
            .prepare_engram_authority(&hook_store, &hook_binding, DEADLOCK_GUARD)
            .unwrap();
        let mut roots = transport.named_roots.lock().unwrap();
        let roots = roots.as_mut().unwrap();
        let latest = roots.latest.get_mut(&hook_binding.claim_id).unwrap();
        latest["generation"] = json!(latest["generation"].as_i64().unwrap() + 1);
        latest["event"] = json!("newer-canonical-naming");
        latest["position"]["position"] =
            json!(latest["position"]["position"].as_i64().unwrap() + 1);
    }));
    let result = request_fixture_name(&claimed, Some(&tree));
    claimed
        .state
        .inner
        .lock()
        .unwrap()
        .test_engram_authority_ack_boundary = None;
    assert_eq!(
        result
            .expect_err("a newer canonical fact cannot authorize the old intent")
            .status,
        StatusCode::CONFLICT
    );
    let inner = claimed.state.inner.lock().unwrap();
    assert!(inner.engram_work_source_roots.is_empty());
    let journal =
        engram_root_journal(&inner.engram_named_root_journal, &store, &binding.claim_id).unwrap();
    assert!(journal.pending.is_some());
    assert!(journal.confirmed.is_none());
    let target = AppState::engram_source_root_target_locked(&inner, &claimed.session_id)
        .unwrap()
        .unwrap();
    let generation = journal.pending.as_ref().unwrap().root.generation;
    drop(inner);
    let current = claimed
        .state
        .read_engram_authority_fact_until(
            &target,
            &binding,
            claimed.state.engram_budget_clock().now() + DEADLOCK_GUARD,
        )
        .unwrap();
    assert!(
        matches!(current.named_root, EngramNamedRootState::Bound { generation: actual, .. }
        if actual == generation as i64 + 1),
        "the refusal followed a valid newer canonical fact"
    );
    assert_eq!(
        claimed
            .transport
            .requests()
            .iter()
            .filter(|request| request.request["operation"] == "named_root_bind")
            .count(),
        1
    );
}

#[test]
fn explicit_clear_recovers_supersession_without_another_producer_transition() {
    for phase in ["post_read", "before_naming_selection"] {
        let (claimed, tree) = root_fixture(&format!("clear-superseded-{phase}"));
        let named = request_fixture_name(&claimed, Some(&tree)).unwrap();
        let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
        let witness = Arc::new(Mutex::new(None));
        if phase == "post_read" {
            supersede_after_event_read(
                claimed.state.clone(),
                naming_store(&claimed),
                binding.clone(),
                witness.clone(),
            );
        } else {
            supersede_at_naming_phase(
                &claimed.state,
                naming_store(&claimed),
                binding.clone(),
                phase,
                witness.clone(),
            );
        }
        let result = request_fixture_name(&claimed, None);
        TEST_ENGRAM_DURING_ROOT_LIFECYCLE_READ.with(|hook| hook.borrow_mut().take());
        claimed
            .state
            .inner
            .lock()
            .unwrap()
            .test_engram_authority_ack_boundary = None;
        assert!(witness.lock().unwrap().is_some());
        let cleared = result.expect("same-binding clear completes");
        assert!(cleared.root.is_none());
        assert_eq!(cleared.generation, 0);
        let store = naming_store(&claimed);
        let inner = claimed.state.inner.lock().unwrap();
        assert!(inner.engram_work_source_roots.is_empty());
        let journal =
            engram_root_journal(&inner.engram_named_root_journal, &store, &binding.claim_id)
                .unwrap();
        assert!(journal.pending.is_none());
        let (event, _) = journal.confirmed.as_ref().unwrap();
        assert_eq!(event.kind, EngramNamedRootKind::Ended);
        assert_eq!(event.root.generation, named.generation);
        drop(inner);
        let sends = claimed
            .transport
            .requests()
            .into_iter()
            .filter(|request| request.request["operation"] == "named_root_bind")
            .collect::<Vec<_>>();
        assert_eq!(sends.len(), 2);
        assert_eq!(sends[1].request["kind"], "ended");
    }
}

#[test]
fn kept_naming_deadline_stops_recovery_before_another_canonical_read() {
    let (claimed, tree) = root_fixture("kept-recovery-original-deadline");
    let named = request_fixture_name(&claimed, Some(&tree)).unwrap();
    let state = claimed.state.clone();
    let store = naming_store(&claimed);
    let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
    let armed = std::sync::atomic::AtomicBool::new(false);
    let reads = Arc::new(Mutex::new(None));
    let hook_reads = reads.clone();
    let transport = claimed.transport.clone();
    claimed
        .state
        .inner
        .lock()
        .unwrap()
        .test_engram_authority_ack_boundary = Some(Arc::new(move |phase| {
        if phase == "before_kept_naming_proof" {
            armed.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        if phase != "before_naming_learning_lock"
            || !armed.swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        state
            .prepare_engram_authority(&store, &binding, DEADLOCK_GUARD)
            .unwrap();
        *hook_reads.lock().unwrap() = Some(
            transport
                .requests()
                .iter()
                .filter(|request| request.request["operation"] == "named_root_read")
                .count(),
        );
        state
            .engram_budget_clock()
            .advance(ENGRAM_SOURCE_ROOT_NAMING_BUDGET);
    }));
    let result = request_fixture_name(&claimed, Some(&tree));
    claimed
        .state
        .inner
        .lock()
        .unwrap()
        .test_engram_authority_ack_boundary = None;
    let error = result.expect_err("the kept-name retry cannot refresh its original budget");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error
            .message
            .contains("identical naming request can be retried"),
        "the helper reports its retryable refusal, not a later zero-budget prepare: {}",
        error.message
    );
    assert_eq!(
        claimed
            .transport
            .requests()
            .iter()
            .filter(|request| request.request["operation"] == "named_root_read")
            .count(),
        reads.lock().unwrap().unwrap()
    );
    assert_eq!(
        claimed
            .transport
            .requests()
            .iter()
            .filter(|request| request.request["operation"] == "named_root_bind")
            .count(),
        1
    );
    let inner = claimed.state.inner.lock().unwrap();
    assert_eq!(
        inner.engram_work_source_roots[0].generation,
        named.generation
    );
    assert!(
        inner
            .engram_named_root_journal
            .iter()
            .all(|journal| journal.pending.is_none())
    );
    drop(inner);
    let retried = request_fixture_name(&claimed, Some(&tree))
        .expect("an identical kept request gets its own budget");
    assert_eq!(retried.generation, named.generation);
    assert_eq!(
        claimed
            .transport
            .requests()
            .iter()
            .filter(|request| request.request["operation"] == "named_root_bind")
            .count(),
        1
    );
}

#[test]
fn kept_naming_rejects_changed_context_and_bounds_repeated_supersession() {
    for fault in [
        "binding",
        "runtime",
        "connection",
        "settings",
        "journal",
        "repeated",
    ] {
        let (claimed, tree) = root_fixture(&format!("kept-context-{fault}"));
        let named = request_fixture_name(&claimed, Some(&tree)).unwrap();
        let store = naming_store(&claimed);
        let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
        let state = claimed.state.clone();
        let session = claimed.session_id.clone();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hook_calls = calls.clone();
        let armed = std::sync::atomic::AtomicBool::new(false);
        claimed
            .state
            .inner
            .lock()
            .unwrap()
            .test_engram_authority_ack_boundary = Some(Arc::new(move |phase| {
            if phase == "before_kept_naming_proof" {
                armed.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            if phase != "before_naming_learning_lock"
                || !armed.load(std::sync::atomic::Ordering::SeqCst)
            {
                return;
            }
            hook_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if fault == "repeated" {
                state
                    .prepare_engram_authority(&store, &binding, DEADLOCK_GUARD)
                    .unwrap();
            } else {
                armed.store(false, std::sync::atomic::Ordering::SeqCst);
                disturb_naming_context(&state, &session, &store, &binding, fault);
            }
        }));
        let result = request_fixture_name(&claimed, Some(&tree));
        claimed
            .state
            .inner
            .lock()
            .unwrap()
            .test_engram_authority_ack_boundary = None;
        assert_eq!(
            result
                .expect_err("kept selection cannot bypass its current context")
                .status,
            StatusCode::CONFLICT,
            "{fault}"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            if fault == "repeated" {
                1 + ENGRAM_NAMING_SUPERSEDED_RETRIES
            } else {
                1
            }
        );
        let inner = claimed.state.inner.lock().unwrap();
        assert_eq!(
            inner.engram_work_source_roots[0].generation,
            named.generation
        );
        assert!(
            inner
                .engram_named_root_journal
                .iter()
                .all(|journal| journal.pending.is_none())
        );
        drop(inner);
        assert_eq!(
            claimed
                .transport
                .requests()
                .iter()
                .filter(|request| request.request["operation"] == "named_root_bind")
                .count(),
            1
        );
        if fault == "repeated" {
            assert_eq!(
                request_fixture_name(&claimed, Some(&tree))
                    .unwrap()
                    .generation,
                named.generation
            );
            assert_eq!(
                claimed
                    .transport
                    .requests()
                    .iter()
                    .filter(|request| request.request["operation"] == "named_root_bind")
                    .count(),
                1
            );
        }
    }
}

#[test]
fn historical_cleanup_keeps_strict_owner_learning_after_the_live_association_moves() {
    for superseded in [false, true] {
        let (claimed, tree) = root_fixture(&format!("historical-cleanup-{superseded}"));
        request_fixture_name(&claimed, Some(&tree)).unwrap();
        let store = naming_store(&claimed);
        let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
        {
            let mut inner = claimed.state.inner.lock().unwrap();
            let root = inner.engram_work_source_roots[0].clone();
            engram_queue_root_cleanup(
                &mut inner.engram_named_root_journal,
                &root,
                EngramNamedRootEndReason::RootInvalid,
            );
            inner
                .session_mut(&claimed.session_id)
                .unwrap()
                .engram
                .work_binding = None;
        }
        let witness = Arc::new(Mutex::new(None));
        if superseded {
            supersede_after_event_read(
                claimed.state.clone(),
                store.clone(),
                binding.clone(),
                witness.clone(),
            );
        }
        claimed
            .state
            .flush_engram_root_cleanup(&claimed.session_id, DEADLOCK_GUARD);
        TEST_ENGRAM_DURING_ROOT_LIFECYCLE_READ.with(|hook| hook.borrow_mut().take());
        if superseded {
            let inner = claimed.state.inner.lock().unwrap();
            let journal =
                engram_root_journal(&inner.engram_named_root_journal, &store, &binding.claim_id)
                    .unwrap();
            assert!(
                journal.pending.is_some(),
                "a stale historical owner stays withheld rather than using live naming recovery"
            );
        } else {
            assert_historical_cleanup_published(&claimed, &store, &binding);
        }
        let sends = || {
            claimed
                .transport
                .requests()
                .into_iter()
                .filter(|request| request.request["operation"] == "named_root_bind")
                .map(|request| request.request)
                .collect::<Vec<_>>()
        };
        assert_eq!(sends().len(), 2);
        if superseded {
            assert!(witness.lock().unwrap().is_some());
            claimed
                .state
                .flush_engram_root_cleanup(&claimed.session_id, DEADLOCK_GUARD);
            let requests = sends();
            assert_eq!(requests.len(), 3);
            assert_eq!(
                requests[1], requests[2],
                "historical recovery replays only the retained intent"
            );
            assert_historical_cleanup_published(&claimed, &store, &binding);
        }
        assert!(claimed.record(|record| record.engram.work_binding.is_none()));
    }
}

fn assert_historical_cleanup_published(
    claimed: &EOCheckedTurn,
    store: &EngramAuthorityStoreKey,
    binding: &EngramControlWorkBinding,
) {
    let target = {
        let inner = claimed.state.inner.lock().unwrap();
        assert!(
            engram_work_source_root_for_work(
                &inner.engram_work_source_roots,
                store,
                &binding.work_id
            )
            .is_none()
        );
        // Publication may compact an unreferenced Ended journal. Its absence
        // alone is not evidence of completion: require published canonical
        // history below and independently read the producer's final state.
        if let Some(journal) =
            engram_root_journal(&inner.engram_named_root_journal, store, &binding.claim_id)
        {
            assert!(journal.pending.is_none());
            assert_eq!(
                journal.confirmed.as_ref().unwrap().0.kind,
                EngramNamedRootKind::Ended
            );
        }
        let history = inner
            .engram_work_naming_history
            .iter()
            .find(|history| history.store == *store && history.work_id == binding.work_id)
            .unwrap();
        assert_eq!(
            history.transition.as_ref().unwrap().phase,
            EngramAuthorityPhase::Published
        );
        assert_eq!(
            history.frontier.as_ref().unwrap().kind,
            EngramNamedRootKind::Ended
        );
        assert!(!engram_authority_work_unresolved(
            &inner,
            store,
            &binding.work_id
        ));
        AppState::engram_source_root_target_locked(&inner, &claimed.session_id)
            .unwrap()
            .unwrap()
    };
    let proof = claimed
        .state
        .read_engram_authority_fact_until(
            &target,
            binding,
            claimed.state.engram_budget_clock().now() + DEADLOCK_GUARD,
        )
        .unwrap();
    assert_eq!(proof.named_root, EngramNamedRootState::None);
    assert_eq!(
        proof.latest_event.as_ref().unwrap().kind,
        EngramNamedRootKind::Ended
    );
}
