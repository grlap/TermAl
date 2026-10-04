// Owns reclamation's cross-store, canonical-safety and durable recovery controls.
// Uses real image persistence and existing transport/notice delivery boundaries.
use super::*;

fn read_count(claimed: &ClaimedRoot) -> usize {
    claimed.transport.requests().iter()
        .filter(|request| request.request["operation"] == "named_root_read").count()
}

fn add_reclamation_store(claimed: &ClaimedRoot, other: &ClaimedRoot) -> EngramWorkSourceRoot {
    // Independent fixtures number their rows locally. Remap those identities
    // before combining stores, so each reader still resolves its own project.
    let other_session = format!("other-{}", Uuid::new_v4());
    let mut selected = other.state.inner.lock().unwrap().engram_work_source_roots[0].clone();
    selected.named_by_session = other_session.clone();
    {
        let source = other.state.inner.lock().unwrap();
        let mut target = claimed.state.inner.lock().unwrap();
        for project in &source.projects {
            let mut project = project.clone();
            let prior = project.id.clone();
            project.id = format!("other-project-{}", Uuid::new_v4());
            if source.engram_declared_project_ids.contains(&prior) {
                target.engram_declared_project_ids.insert(project.id.clone());
            }
            if source.engram_declaration_checked_project_ids.contains(&prior) {
                target.engram_declaration_checked_project_ids.insert(project.id.clone());
            }
            for record in source.sessions.iter().filter(|record| record.session.project_id.as_ref() == Some(&prior)) {
                let mut record = record.clone();
                record.session.project_id = Some(project.id.clone());
                record.session.id = other_session.clone();
                target.sessions.push(record);
            }
            target.projects.push(project);
        }
        target.engram_work_source_roots.push(selected.clone());
        target.engram_named_root_journal.extend(source.engram_named_root_journal.iter().cloned());
        target.engram_work_naming_history.extend(source.engram_work_naming_history.iter().cloned());
    }
    let source = other.transport.named_roots.lock().unwrap();
    let mut target = claimed.transport.named_roots.lock().unwrap();
    target.as_mut().unwrap().root_reads.extend(source.as_ref().unwrap().root_reads.clone());
    selected
}

#[test]
fn named_root_reclamation_other_store_frees_global_capacity_and_rotation_is_fair() {
    let (claimed, retained) = settled_root("reclaim-multi-store-live", "live");
    let (other, _) = settled_root("reclaim-multi-store-ended", "completed");
    let obsolete = add_reclamation_store(&claimed, &other);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        for number in 2..ENGRAM_WORK_SOURCE_ROOT_LIMIT {
            let mut root = retained.clone();
            root.work_id = format!("unassociated-work-{number}");
            root.claim_id = format!("unassociated-claim-{number}");
            inner.engram_work_source_roots.push(root);
        }
        assert_eq!(engram_named_root_capacity(&inner), 64);
    }
    let first = reclamation_plan(&claimed);
    assert_eq!(first.len(), 2);
    assert_ne!(first[0].root.store, first[1].root.store);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        inner.engram_root_reclamation.last_store = Some(first[0].root.store.clone());
    }
    let rotated = reclamation_plan(&claimed);
    assert_eq!(rotated[0].root.store, first[1].root.store);
    let reads = read_count(&claimed);
    claimed.state.run_engram_root_reclamation_pass(rotated);
    assert_eq!(read_count(&claimed) - reads, 2, "one read per candidate, not a second publication read");
    let inner = claimed.state.inner.lock().unwrap();
    assert!(inner.engram_work_source_roots.contains(&retained));
    assert!(!inner.engram_work_source_roots.contains(&obsolete));
    assert_eq!(engram_named_root_capacity(&inner), 63);
    drop(inner);
    let later_label = "reclaim-later-actual-naming";
    let binding = test_control_work_binding(&format!("turn-observation-{later_label}"), 1);
    claimed.record(|record| record.engram.work_binding = Some(binding));
    let later = name_root(&claimed, later_label, Some(&claimed.root), vec![claimed_root_held(later_label)]).unwrap();
    assert!(later.root.is_some(), "the public naming path can use capacity released in another store");
    assert_eq!(engram_named_root_capacity(&claimed.state.inner.lock().unwrap()), 64);
}

#[test]
fn named_root_reclamation_full_refusal_is_prompt_and_single_flight_without_success_promises() {
    let (claimed, entry) = settled_root("reclaim-full-refusal", "unknown");
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        for number in 1..ENGRAM_WORK_SOURCE_ROOT_LIMIT {
            let mut root = entry.clone();
            root.work_id = format!("full-unassociated-work-{number}");
            root.claim_id = format!("full-unassociated-claim-{number}");
            inner.engram_work_source_roots.push(root);
        }
    }
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    // Dropping the sender also releases the worker if an assertion fails.
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let worker_release = Mutex::new(release_rx);
    let first = std::sync::atomic::AtomicBool::new(true);
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = Some(Arc::new(move |boundary| {
        if boundary == "before_delta" && first.swap(false, std::sync::atomic::Ordering::SeqCst) {
            entered_tx.send(()).unwrap();
            let _ = worker_release.lock().unwrap().recv_timeout(TEST_PHASE_DEADLOCK_GUARD);
        } else if boundary == "after_reclamation_pass" {
            done_tx.send(()).unwrap();
        }
    }));
    let reads = read_count(&claimed);
    let request = || EngramSourceRootRequest { work: "w-full-new-work".to_owned(),
        path: Some(Some(claimed.root.to_string_lossy().into_owned())) };
    let refusal = claimed.state.name_engram_source_root(&claimed.session_id, request()).unwrap_err();
    assert!(refusal.message.contains("a bounded reclamation pass was scheduled"));
    assert!(refusal.message.contains("No slot release or successful retry is promised"));
    assert!(refusal.message.contains("no retained canonical run association"));
    entered_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap();
    assert_eq!(read_count(&claimed), reads, "the refusal did not spend any lifecycle read budget");
    let second = claimed.state.name_engram_source_root(&claimed.session_id, request()).unwrap_err();
    assert!(second.message.contains("already running"));
    claimed.state.engram_host().test_run_tick();
    assert_eq!(read_count(&claimed), reads);
    release_tx.send(()).unwrap();
    done_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap();
    assert_eq!(read_count(&claimed) - reads, 1);
    assert_eq!(engram_named_root_capacity(&claimed.state.inner.lock().unwrap()), 64);
    let third = claimed.state.name_engram_source_root(&claimed.session_id, request()).unwrap_err();
    assert!(third.message.contains("no eligible reclamation pass was scheduled"));
    assert!(third.message.contains("No slot release or successful retry is promised"));
    assert_root_retained(&claimed, &entry);
}

#[test]
fn named_root_reclamation_recurring_host_tick_retires_without_an_agent_prompt() {
    let (claimed, entry) = settled_root("reclaim-host-tick", "completed");
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = Some(Arc::new(move |boundary| {
        if boundary == "after_reclamation_pass" { done_tx.send(()).unwrap(); }
    }));
    claimed.state.engram_host().test_run_tick();
    done_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap();
    let inner = claimed.state.inner.lock().unwrap();
    assert!(!inner.engram_work_source_roots.contains(&entry));
    assert!(inner.engram_root_reclamation.flight.is_none());
    assert!(inner.engram_work_naming_history[0].retirements[0].published);
    assert!(matches!(claimed.runtime_rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
}

#[test]
fn named_root_reclamation_unknown_oldest_cools_down_and_later_terminal_entry_progresses() {
    let (claimed, unknown) = settled_root("reclaim-oldest-unknown", "unknown");
    let (other, _) = settled_root("reclaim-later-terminal", "completed");
    let ended = add_reclamation_store(&claimed, &other);
    let first = reclamation_plan(&claimed).into_iter().find(|task| task.root == unknown).unwrap();
    claimed.state.run_engram_root_reclamation_pass(vec![first]);
    assert_root_retained(&claimed, &unknown);
    let next = reclamation_plan(&claimed);
    assert!(next.iter().all(|task| task.root != unknown));
    assert!(next.iter().any(|task| task.root == ended));
    claimed.state.run_engram_root_reclamation_pass(next);
    assert!(!claimed.state.inner.lock().unwrap().engram_work_source_roots.contains(&ended));
    assert_root_retained(&claimed, &unknown);
}

#[test]
fn named_root_reclamation_no_reader_is_retained_until_same_store_authority_returns() {
    let (claimed, entry) = settled_root("reclaim-reader-restored", "completed");
    let token = claimed.record(|record| record.engram.routing_token.take());
    assert!(reclamation_plan(&claimed).is_empty());
    assert_root_retained(&claimed, &entry);
    claimed.record(|record| record.engram.routing_token = token);
    claimed.state.run_engram_root_reclamation_pass(reclamation_plan(&claimed));
    assert!(!claimed.state.inner.lock().unwrap().engram_work_source_roots.contains(&entry));
}

#[test]
fn named_root_reclamation_validated_bound_any_difference_retires_but_older_and_mismatch_do_not() {
    for case in ["workspace", "generation", "named-at", "older", "mismatch"] {
        let (claimed, entry) = settled_root("reclaim-bound-differences", "live");
        {
            let mut fixture = claimed.transport.named_roots.lock().unwrap();
            let proof = fixture.as_mut().unwrap().root_reads.get_mut(&entry.claim_id).unwrap();
            match case {
                "workspace" => {},
                "generation" => proof["named_root"]["generation"] = json!(entry.generation + 1),
                "named-at" => {},
                "older" => {},
                "mismatch" => proof["claim_id"] = json!("a-different-claim"),
                _ => unreachable!(),
            }
            proof["latest_event"]["workspace_id"] = proof["named_root"]["workspace_id"].clone();
            proof["latest_event"]["generation"] = proof["named_root"]["generation"].clone();
            proof["latest_event"]["named_at"] = proof["named_root"]["named_at"].clone();
            if case == "generation" {
                proof["latest_event"]["event"] = json!("external-successor-binding");
                let cut = proof["read_cut"]["position"].as_i64().unwrap() + 1;
                proof["latest_event"]["position"]["position"] = json!(cut);
                proof["read_cut"]["position"] = json!(cut);
            }
        }
        {
            // At the same generation the canonical event cannot change its
            // payload. Exercise an obsolete local tuple, not an impossible
            // producer event that the canonical-order guard must refuse.
            let mut inner = claimed.state.inner.lock().unwrap();
            let local = &mut inner.engram_work_source_roots[0];
            match case {
                "workspace" => local.root.push_str("-stale-local-selection"),
                "named-at" => local.named_at = "2026-10-01T00:00:00Z".to_owned(),
                "older" => local.generation += 1,
                _ => {},
            }
        }
        claimed.state.run_engram_root_reclamation_pass(reclamation_plan(&claimed));
        let inner = claimed.state.inner.lock().unwrap();
        if matches!(case, "older" | "mismatch") {
            assert_eq!(inner.engram_work_source_roots.len(), 1, "{case}");
            assert!(inner.engram_work_naming_history[0].retirements.is_empty(), "{case}");
        } else {
            assert!(inner.engram_work_source_roots.is_empty(), "{case}");
            assert_eq!(inner.engram_work_naming_history[0].retirements.len(), 1, "{case}");
        }
    }
}

#[test]
fn named_root_reclamation_reader_authority_change_during_read_retains_the_entry() {
    let (claimed, entry) = settled_root("reclaim-reader-race", "completed");
    let state = claimed.state.clone();
    let session = claimed.session_id.clone();
    TEST_ENGRAM_DURING_ROOT_LIFECYCLE_READ.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].engram.routing_token = Some("replaced-reader-token".to_owned());
        }));
    });
    claimed.state.run_engram_root_reclamation_pass(reclamation_plan(&claimed));
    assert_root_retained(&claimed, &entry);
    assert!(claimed.state.inner.lock().unwrap().engram_work_naming_history[0].retirements.is_empty());
}

#[test]
fn named_root_reclamation_failed_candidate_commit_reserves_capacity_then_restart_acks_once() {
    let label = "reclaim-crash-candidate";
    let (claimed, entry) = settled_root(label, "completed");
    let clock = claimed.state.engram_budget_clock();
    let binding = engram_root_journal(&claimed.state.inner.lock().unwrap().engram_named_root_journal,
        &entry.store, &entry.claim_id).unwrap().read_binding.clone().unwrap();
    let original_receipt = claimed.state.inner.lock().unwrap().engram_named_root_journal[0].confirmed.clone();
    let mut state = claimed.state.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    state.persist_tx = tx;
    let worker = state.clone();
    let worker_entry = entry.clone();
    let task = std::thread::spawn(move || worker.recover_one_engram_root_until(&worker_entry,
        &EngramRootReclamationRoute::Unfocused, worker.engram_budget_clock().now() + TEST_PHASE_DEADLOCK_GUARD));
    let mut batch = PersistFenceBatch::default();
    let mut cache = SqlitePersistConnectionCache::new();
    receive_naming_authority_fence(&rx, &mut batch, &task);
    let prepared = collect_persist_delta_from_shared_state(&state.inner, 0);
    persist_delta_with_fences(&mut cache, state.persistence_path.as_path(), &prepared, &mut batch).unwrap();
    receive_naming_authority_fence(&rx, &mut batch, &task);
    let candidate = collect_persist_delta_from_shared_state(&state.inner, prepared.watermark);
    persist_delta_via_cache(&mut cache, state.persistence_path.as_path(), &candidate).unwrap();
    batch.fail(PersistFenceError::WriteFailed("committed candidate but acknowledgement lost".to_owned()));
    assert!(task.join().unwrap().is_err());
    let mut restored = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    assert!(!restored.engram_work_source_roots.contains(&entry));
    assert_eq!(engram_named_root_capacity(&restored), 1, "a committed unacknowledged retirement still reserves its slot");
    assert!(!restored.engram_work_naming_history[0].retirements[0].published);
    assert_eq!(restored.engram_named_root_journal[0].confirmed, original_receipt);
    engram_compact_root_journal(&mut restored);
    assert_eq!(restored.engram_named_root_journal[0].confirmed, original_receipt);
    assert_eq!(engram_named_root_capacity(&restored), 1,
        "compaction cannot release a lost-ACK publication's reservation");
    let notice_id = restored.engram_work_naming_history[0].retirements[0].id.clone();
    *claimed.state.inner.lock().unwrap() = restored;
    claimed.state.install_test_engram_budget_clock(clock);
    install_control_only_transport(&claimed.state, claimed.transport.clone());
    prepare_claimed_root_naming(&claimed, label);
    let before = read_count(&claimed);
    claimed.state.recover_one_engram_root_until(&entry,
        &EngramRootReclamationRoute::OwnClear(claimed.session_id.clone()),
        claimed.state.engram_budget_clock().now() + ENGRAM_ROOT_RECLAMATION_BUDGET).unwrap();
    let inner = claimed.state.inner.lock().unwrap();
    assert_eq!(engram_named_root_capacity(&inner), 0);
    assert_eq!(inner.engram_work_naming_history[0].retirements.len(), 1);
    assert_eq!(inner.engram_work_naming_history[0].retirements[0].id, notice_id);
    assert!(inner.engram_work_naming_history[0].retirements[0].published);
    assert_eq!(inner.engram_work_naming_history[0].retirements[0].binding, binding);
    assert_eq!(inner.engram_named_root_journal[0].confirmed, original_receipt);
    drop(inner);
    assert_eq!(read_count(&claimed), before, "recover the existing durable publication, not a new canonical read");
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        assert_eq!(inner.sessions[index].engram.work_binding.as_ref().unwrap().claim_id, entry.claim_id);
        engram_compact_root_journal(&mut inner);
        assert_eq!(inner.engram_named_root_journal.len(), 1,
            "restart's matching current binding still protects its journal after publication");
        inner.sessions[index].engram.work_binding = None;
        engram_compact_root_journal(&mut inner);
        assert!(inner.engram_named_root_journal.is_empty());
        assert_eq!(inner.engram_work_naming_history[0].retirements[0].id, notice_id);
        assert!(!inner.engram_work_naming_history[0].retirements[0].delivered);
    }
    claimed.state.recover_one_engram_root_until(&entry,
        &EngramRootReclamationRoute::OwnClear(claimed.session_id.clone()),
        claimed.state.engram_budget_clock().now() + ENGRAM_ROOT_RECLAMATION_BUDGET)
        .expect_err("a completed publication replay cannot recreate a compacted journal");
    assert_eq!(read_count(&claimed), before);
    assert_eq!(claimed.state.inner.lock().unwrap().engram_work_naming_history[0].retirements.len(), 1);
}

#[test]
fn named_root_reclamation_notice_survives_restart_and_focus_change_and_acks_once() {
    let (claimed, entry) = settled_root("reclaim-notice-restart", "completed");
    claimed.state.run_engram_root_reclamation_pass(reclamation_plan(&claimed));
    let bytes = {
        let inner = claimed.state.inner.lock().unwrap();
        serde_json::to_vec(&PersistedState::from_inner(&inner)).unwrap()
    };
    let mut restored = serde_json::from_slice::<PersistedState>(&bytes).unwrap().into_inner().unwrap();
    let index = restored.find_session_index(&claimed.session_id).unwrap();
    let notices = refresh_engram_source_root_notices_locked(&mut restored, index);
    assert_eq!(notices.len(), 1);
    let notice = &notices[0];
    assert!(matches!(&notice.kind, EngramSourceRootNoticeKind::Reclaimed { selection, .. } if selection == &entry));
    for component in [&entry.store.project_id, &entry.short_ref, &entry.root] {
        assert!(notice.line().contains(component));
    }
    assert!(notice.line().contains("its run completed"));
    assert!(notice.line().contains("canonical read cut"));
    assert!(!notice.line().contains("Name its worktree again"));
    assert_eq!(refresh_engram_source_root_notices_locked(&mut restored, index), notices);
    acknowledge_engram_root_retirements_locked(&mut restored, "a-different-session", &notices);
    assert!(!restored.engram_work_naming_history[0].retirements[0].delivered);
    acknowledge_engram_root_retirements_locked(&mut restored, &claimed.session_id, &notices);
    acknowledge_engram_source_root_notices_locked(&mut restored.sessions[index], &notices);
    assert!(refresh_engram_source_root_notices_locked(&mut restored, index).is_empty());
    let bytes = serde_json::to_vec(&PersistedState::from_inner(&restored)).unwrap();
    let mut restored = serde_json::from_slice::<PersistedState>(&bytes).unwrap().into_inner().unwrap();
    let index = restored.find_session_index(&claimed.session_id).unwrap();
    assert!(refresh_engram_source_root_notices_locked(&mut restored, index).is_empty());
    assert_eq!(restored.engram_work_naming_history[0].retirements.len(), 1);
    assert!(matches!(claimed.runtime_rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
}

#[test]
fn named_root_reclamation_published_notices_do_not_exhaust_the_journal() {
    let label = "reclaim-published-journal-capacity";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let clock = claimed.state.engram_budget_clock();
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_claimed_root_naming(&claimed, label);
    claimed_root_store(&claimed);
    for occurrence in 0..=ENGRAM_NAMED_ROOT_JOURNAL_LIMIT {
        prepare_claimed_root_naming(&claimed, label);
        let mut held = claimed_root_held(label);
        let binding = held.items[0].control_binding.as_mut().unwrap();
        binding.claim_id = format!("retired-claim-{occurrence}");
        binding.run_id = format!("retired-run-{occurrence}");
        binding.root_execution_id = format!("retired-execution-{occurrence}");
        let binding = binding.clone();
        held.items[0].claim_id = binding.claim_id.clone();
        claimed.record(|record| {
            record.engram.work_binding = Some(binding.clone());
            record.engram.named_root = None;
        });
        claimed.transport.enable_named_roots(&claimed.session_id, &binding);
        claimed.transport.replace_held_claims([Ok(held)]);
        let named = claimed.state.name_engram_source_root(&claimed.session_id,
            EngramSourceRootRequest { work: format!("w-{label}"),
                path: Some(Some(worktree.to_string_lossy().into_owned())) });
        assert!(named.is_ok(),
            "new naming {occurrence} must progress after acknowledged retirements: {named:?}");
        if occurrence == ENGRAM_NAMED_ROOT_JOURNAL_LIMIT { break; }

        let (entry, journal) = {
            let inner = claimed.state.inner.lock().unwrap();
            let entry = inner.engram_work_source_roots[0].clone();
            let journal = engram_root_journal(&inner.engram_named_root_journal,
                &entry.store, &entry.claim_id).unwrap().clone();
            (entry, journal)
        };
        let mut proof = covering_removed_root_read(&journal, false);
        proof["run"]["generation"] = json!(claimed.transport.named_roots.lock().unwrap()
            .as_ref().unwrap().run_generations[&binding.run_id]);
        serde_json::from_value::<EngramNamedRootReadResponse>(proof.clone()).unwrap()
            .validate_authority(&entry.store, &binding).unwrap();
        claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
            .root_reads.insert(entry.claim_id.clone(), proof);
        // Move the reader away from the completed claim, without removing
        // its store authority or fabricating a producer binding-end event.
        let mut reader = binding.clone();
        reader.work_id = "capacity-reader-work".to_owned();
        reader.claim_id = "capacity-reader-claim".to_owned();
        reader.run_id = "capacity-reader-run".to_owned();
        reader.root_execution_id = "capacity-reader-execution".to_owned();
        claimed.record(|record| record.engram.work_binding = Some(reader.clone()));
        claimed.transport.enable_named_roots(&claimed.session_id, &reader);
        assert!(claimed.state.recover_one_engram_root_until(&entry,
            &EngramRootReclamationRoute::Unfocused,
            claimed.state.engram_budget_clock().now() + ENGRAM_ROOT_RECLAMATION_BUDGET).unwrap());
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        let notices = refresh_engram_source_root_notices_locked(&mut inner, index);
        let retired = notices.into_iter().filter(|notice|
            matches!(&notice.kind, EngramSourceRootNoticeKind::Reclaimed { selection, .. }
                if selection == &entry)).collect::<Vec<_>>();
        assert_eq!(retired.len(), 1);
        acknowledge_engram_root_retirements_locked(&mut inner, &claimed.session_id, &retired);
        acknowledge_engram_source_root_notices_locked(&mut inner.sessions[index], &retired);
        assert_eq!(engram_named_root_capacity(&inner), 0);
        assert!(inner.engram_work_source_roots.is_empty());
        assert_eq!(inner.engram_work_naming_history[0].retirements.len(), occurrence + 1);
        assert!(inner.engram_work_naming_history[0].retirements.iter()
            .all(|notice| notice.published && notice.delivered));
        if occurrence + 1 == ENGRAM_NAMED_ROOT_JOURNAL_LIMIT {
            // Both vectors persist. Restart must preserve the exact durable
            // notice identities without making them scarce journal pins.
            let bytes = serde_json::to_vec(&PersistedState::from_inner(&inner)).unwrap();
            let restored = serde_json::from_slice::<PersistedState>(&bytes).unwrap().into_inner().unwrap();
            assert_eq!(restored.engram_work_naming_history[0].retirements,
                inner.engram_work_naming_history[0].retirements);
            *inner = restored;
            drop(inner);
            claimed.state.install_test_engram_budget_clock(clock.clone());
            install_control_only_transport(&claimed.state, claimed.transport.clone());
        }
    }
    let mut inner = claimed.state.inner.lock().unwrap();
    engram_compact_root_journal(&mut inner);
    assert_eq!(inner.engram_named_root_journal.len(), 1,
        "only the fresh live claim still requires a journal");
    assert_eq!(engram_named_root_capacity(&inner), 1);
    assert_eq!(inner.engram_work_naming_history[0].retirements.len(), ENGRAM_NAMED_ROOT_JOURNAL_LIMIT);
    let index = inner.find_session_index(&claimed.session_id).unwrap();
    assert!(refresh_engram_source_root_notices_locked(&mut inner, index).iter()
        .all(|notice| !matches!(notice.kind, EngramSourceRootNoticeKind::Reclaimed { .. })),
        "acknowledged retirement notices must not be redelivered after restart");
}

#[test]
fn named_root_reclamation_published_undelivered_notice_survives_compacted_restart() {
    let (claimed, entry) = settled_root("reclaim-compacted-undelivered", "completed");
    claimed.state.run_engram_root_reclamation_pass(reclamation_plan(&claimed));
    let mut inner = claimed.state.inner.lock().unwrap();
    let notice = inner.engram_work_naming_history[0].retirements[0].clone();
    assert!(notice.published && !notice.delivered);
    engram_compact_root_journal(&mut inner);
    assert!(inner.engram_named_root_journal.is_empty());
    assert_eq!(engram_named_root_capacity(&inner), 0);
    assert_eq!(inner.engram_work_naming_history[0].retirements, vec![notice.clone()]);
    let bytes = serde_json::to_vec(&PersistedState::from_inner(&inner)).unwrap();
    drop(inner);
    let mut restored = serde_json::from_slice::<PersistedState>(&bytes).unwrap().into_inner().unwrap();
    assert!(restored.engram_named_root_journal.is_empty());
    assert_eq!(restored.engram_work_naming_history[0].retirements, vec![notice]);
    let index = restored.find_session_index(&claimed.session_id).unwrap();
    let pending = refresh_engram_source_root_notices_locked(&mut restored, index);
    assert_eq!(pending.len(), 1);
    assert!(matches!(&pending[0].kind, EngramSourceRootNoticeKind::Reclaimed { selection, .. }
        if selection == &entry));
    acknowledge_engram_root_retirements_locked(&mut restored, &claimed.session_id, &pending);
    acknowledge_engram_source_root_notices_locked(&mut restored.sessions[index], &pending);
    let bytes = serde_json::to_vec(&PersistedState::from_inner(&restored)).unwrap();
    let mut restored = serde_json::from_slice::<PersistedState>(&bytes).unwrap().into_inner().unwrap();
    assert!(refresh_engram_source_root_notices_locked(&mut restored, index).is_empty());
    assert_eq!(restored.engram_work_naming_history[0].retirements.len(), 1);
    assert!(restored.engram_work_naming_history[0].retirements[0].delivered);
    assert!(restored.engram_named_root_journal.is_empty());
}

#[test]
fn named_root_reclamation_published_notice_of_removed_session_is_retained_without_pins() {
    let (claimed, _) = settled_root("reclaim-compacted-removed-session", "completed");
    claimed.state.run_engram_root_reclamation_pass(reclamation_plan(&claimed));
    let mut inner = claimed.state.inner.lock().unwrap();
    let notice = inner.engram_work_naming_history[0].retirements[0].clone();
    inner.sessions.retain(|record| record.session.id != claimed.session_id);
    engram_compact_root_journal(&mut inner);
    assert!(inner.engram_named_root_journal.is_empty());
    assert_eq!(engram_named_root_capacity(&inner), 0);
    let bytes = serde_json::to_vec(&PersistedState::from_inner(&inner)).unwrap();
    let restored = serde_json::from_slice::<PersistedState>(&bytes).unwrap().into_inner().unwrap();
    assert_eq!(restored.engram_work_naming_history[0].retirements, vec![notice]);
    assert!(restored.engram_named_root_journal.is_empty());
    assert_eq!(engram_named_root_capacity(&restored), 0);
}

#[test]
fn named_root_reclamation_published_notice_keeps_other_journal_protections() {
    let (claimed, entry) = settled_root("reclaim-compaction-protections", "completed");
    claimed.state.run_engram_root_reclamation_pass(reclamation_plan(&claimed));
    let mut inner = claimed.state.inner.lock().unwrap();
    let journal = inner.engram_named_root_journal[0].clone();
    let index = inner.find_session_index(&claimed.session_id).unwrap();
    let original_runtime = inner.sessions[index].engram.clone();
    let notice = inner.engram_work_naming_history[0].retirements[0].clone();
    for protection in ["pending-bound", "pending-ended", "pending-invalid", "binding", "capture", "evaluator", "replacement", "live-bound"] {
        inner.engram_named_root_journal = vec![journal.clone()];
        inner.sessions[index].engram = original_runtime.clone();
        match protection {
            "pending-bound" | "pending-ended" | "pending-invalid" => {
                let mut event = journal.confirmed.as_ref().unwrap().0.clone();
                event.kind = match protection {
                    "pending-bound" => EngramNamedRootKind::Bound,
                    _ => EngramNamedRootKind::Ended,
                };
                event.end_reason = match protection {
                    "pending-ended" => Some(EngramNamedRootEndReason::ExplicitClear),
                    "pending-invalid" => Some(EngramNamedRootEndReason::RootInvalid),
                    _ => None,
                };
                inner.engram_named_root_journal[0].pending = Some(event);
            }
            "binding" => inner.sessions[index].engram.work_binding = journal.read_binding.clone(),
            "capture" => inner.sessions[index].engram.active_turn_root_capture = Some(EngramRootCapture::Recorded {
                generation: i64::try_from(entry.generation).unwrap(), state: EngramSourceRootState::Named,
                workspace_id: entry.root.clone(),
            }),
            "evaluator" => inner.delegations.push(serde_json::from_value(json!({
                "id":"compaction-evaluator", "parentSessionId":claimed.session_id,
                "childSessionId":"compaction-evaluator-child", "mode":DelegationMode::Evaluator,
                "status":DelegationStatus::Running, "title":"Compaction control", "prompt":"Read only",
                "cwd":entry.root, "agent":Agent::Claude, "writePolicy":DelegationWritePolicy::ReadOnly,
                "createdAt":stamp_now(), "acceptanceEvaluation":{
                    "workRef":entry.short_ref, "mode":AcceptanceEvaluationMode::IndependentSession,
                    "acceptanceBasis":1, "evidenceBasis":1, "criteriaCount":1,
                    "attemptKey":"compaction-evaluator", "store":entry.store,
                    "sourceClaim":{"workId":entry.work_id, "claimId":entry.claim_id}
                }
            })).unwrap()),
            "replacement" => {
                let mut replacement = entry.clone();
                replacement.generation += 1;
                replacement.root.push_str("-successor");
                inner.engram_work_source_roots.push(replacement);
            }
            "live-bound" => inner.engram_named_root_journal[0].obsolete = false,
            _ => unreachable!(),
        }
        engram_compact_root_journal(&mut inner);
        assert_eq!(inner.engram_named_root_journal.len(), 1, "{protection}");
        assert_eq!(inner.engram_work_naming_history[0].retirements, vec![notice.clone()]);
        inner.engram_work_source_roots.clear();
        inner.delegations.clear();
    }
    inner.sessions[index].engram = original_runtime;
    inner.engram_named_root_journal = vec![journal];
    engram_compact_root_journal(&mut inner);
    assert!(inner.engram_named_root_journal.is_empty(), "no protection must still make progress");
}

#[test]
fn named_root_reclamation_stale_publication_image_cannot_restore_compacted_journal() {
    let (claimed, entry) = settled_root("reclaim-compaction-stale-image", "completed");
    claimed.state.run_engram_root_reclamation_pass(reclamation_plan(&claimed));
    let (image, notice, floor) = {
        let inner = claimed.state.inner.lock().unwrap();
        (EngramAuthorityImage::capture(&inner, &entry.store, &entry.work_id).unwrap(),
            inner.engram_work_naming_history[0].retirements[0].clone(), inner.engram_source_root_generation)
    };
    let state = claimed.state.clone();
    let scanned = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hook_scanned = scanned.clone();
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = Some(Arc::new(move |boundary| {
        if boundary == "before_owner_check" {
            let mut inner = state.inner.lock().unwrap();
            engram_compact_root_journal(&mut inner);
            assert!(inner.engram_named_root_journal.is_empty());
            hook_scanned.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }));
    let error = claimed.state.confirm_engram_authority_image_until(&image,
        claimed.state.engram_budget_clock().now() + ENGRAM_ROOT_RECLAMATION_BUDGET).unwrap_err();
    assert!(scanned.load(std::sync::atomic::Ordering::SeqCst));
    assert!(error.message.contains("superseded authority image"), "{error:?}");
    let mut inner = claimed.state.inner.lock().unwrap();
    inner.test_engram_authority_ack_boundary = None;
    assert!(!image.still_owned(&inner));
    assert!(!image.matches_metadata(&PersistedState::from_inner(&inner)));
    assert!(inner.engram_named_root_journal.is_empty());
    assert!(inner.engram_work_source_roots.is_empty());
    assert_eq!(inner.engram_work_naming_history[0].retirements, vec![notice.clone()]);
    assert_eq!(inner.engram_source_root_generation, floor);
    let bytes = serde_json::to_vec(&PersistedState::from_inner(&inner)).unwrap();
    let restored = serde_json::from_slice::<PersistedState>(&bytes).unwrap().into_inner().unwrap();
    assert!(restored.engram_named_root_journal.is_empty());
    assert!(restored.engram_work_source_roots.is_empty());
    assert_eq!(restored.engram_work_naming_history[0].retirements, vec![notice]);
    assert_eq!(restored.engram_source_root_generation, floor);
}

#[test]
fn named_root_reclamation_transition_controls_unpublished_journal_publishes_before_unpin() {
    let (claimed, entry) = settled_root("reclaim-publication-before-unpin", "completed");
    let state = claimed.state.clone();
    let reached = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hook_reached = reached.clone();
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = Some(Arc::new(move |boundary| {
        if boundary == "before_publication" {
            let mut inner = state.inner.lock().unwrap();
            assert_eq!(inner.engram_work_naming_history[0].transition.as_ref().unwrap().phase,
                EngramAuthorityPhase::Candidate);
            assert!(!inner.engram_work_naming_history[0].retirements[0].published);
            assert!(inner.engram_work_source_roots.is_empty());
            engram_compact_root_journal(&mut inner);
            assert_eq!(inner.engram_named_root_journal.len(), 1,
                "the captured unpublished retirement still pins its journal after ACK");
            hook_reached.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }));
    claimed.state.run_engram_root_reclamation_pass(reclamation_plan(&claimed));
    assert!(reached.load(std::sync::atomic::Ordering::SeqCst));
    let mut inner = claimed.state.inner.lock().unwrap();
    inner.test_engram_authority_ack_boundary = None;
    let notice = inner.engram_work_naming_history[0].retirements[0].clone();
    assert!(notice.published && !notice.delivered);
    assert_eq!(notice.selection, entry);
    assert_eq!(inner.engram_work_naming_history[0].transition.as_ref().unwrap().phase,
        EngramAuthorityPhase::Published);
    assert_eq!(inner.engram_named_root_journal.len(), 1,
        "publication precedes the later compaction that removes this journal");
    engram_compact_root_journal(&mut inner);
    assert!(inner.engram_named_root_journal.is_empty());
    assert_eq!(inner.engram_work_naming_history[0].retirements, vec![notice]);
}

#[test]
fn named_root_reclamation_transition_controls_compacted_owner_recovers_candidate_after_restart() {
    for compact_before_capture in [false, true] {
        let (claimed, entry) = settled_root("reclaim-compacted-owner-recovery", "completed");
        claimed.state.run_engram_root_reclamation_pass(reclamation_plan(&claimed));
        let clock = claimed.state.engram_budget_clock();
        let (notice, floor) = {
            let inner = claimed.state.inner.lock().unwrap();
            assert_eq!(inner.engram_named_root_journal.len(), 1);
            (inner.engram_work_naming_history[0].retirements[0].clone(),
                inner.engram_source_root_generation)
        };
        assert!(notice.published && !notice.delivered);
        let reached = Arc::new(std::sync::atomic::AtomicBool::new(false));
        if compact_before_capture {
            engram_compact_root_journal(&mut claimed.state.inner.lock().unwrap());
        } else {
            let state = claimed.state.clone();
            let hook_reached = reached.clone();
            claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = Some(Arc::new(move |boundary| {
                if boundary == "before_owner_check" {
                    let mut inner = state.inner.lock().unwrap();
                    assert_eq!(inner.engram_work_naming_history[0].transition.as_ref().unwrap().phase,
                        EngramAuthorityPhase::Prepared);
                    assert_eq!(inner.engram_named_root_journal.len(), 1);
                    engram_compact_root_journal(&mut inner);
                    assert!(inner.engram_named_root_journal.is_empty());
                    hook_reached.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }));
        }
        let prepared = claimed.state.prepare_engram_authority(&entry.store, &notice.binding,
            ENGRAM_ROOT_RECLAMATION_BUDGET);
        if compact_before_capture {
            assert_eq!(prepared.unwrap().phase, EngramAuthorityPhase::Prepared,
                "pre-capture compaction is part of a valid newly acknowledged image");
        } else {
            let error = prepared.unwrap_err();
            assert!(error.message.contains("superseded authority image"), "{error:?}");
            assert!(reached.load(std::sync::atomic::Ordering::SeqCst));
        }
        let bytes = {
            let mut inner = claimed.state.inner.lock().unwrap();
            inner.test_engram_authority_ack_boundary = None;
            assert!(inner.engram_named_root_journal.is_empty());
            assert!(inner.engram_work_source_roots.is_empty());
            assert_eq!(inner.engram_work_naming_history[0].retirements, vec![notice.clone()]);
            serde_json::to_vec(&PersistedState::from_inner(&inner)).unwrap()
        };
        *claimed.state.inner.lock().unwrap() = serde_json::from_slice::<PersistedState>(&bytes)
            .unwrap().into_inner().unwrap();
        claimed.state.install_test_engram_budget_clock(clock.clone());
        install_control_only_transport(&claimed.state, claimed.transport.clone());
        let owner = claimed.state.prepare_engram_authority(&entry.store, &notice.binding,
            ENGRAM_ROOT_RECLAMATION_BUDGET).unwrap();
        AppState::learn_engram_authority_locked(&mut claimed.state.inner.lock().unwrap(),
            &entry.store, &owner, &notice.read).unwrap();

        // Commit the actual new Candidate, then lose its ACK. The existing
        // owner recovery must capture current compacted state, not old bytes.
        let mut worker_state = claimed.state.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        worker_state.persist_tx = tx;
        let worker = worker_state.clone();
        let store = entry.store.clone();
        let worker_owner = owner.clone();
        let task = std::thread::spawn(move || worker.publish_engram_authority_image_until(
            &store, &worker_owner, worker.engram_budget_clock().now() + TEST_PHASE_DEADLOCK_GUARD));
        let mut batch = PersistFenceBatch::default();
        let mut cache = SqlitePersistConnectionCache::new();
        receive_naming_authority_fence(&rx, &mut batch, &task);
        let candidate = collect_persist_delta_from_shared_state(&worker_state.inner, 0);
        persist_delta_via_cache(&mut cache, worker_state.persistence_path.as_path(), &candidate).unwrap();
        batch.fail(PersistFenceError::WriteFailed("compacted candidate committed; ACK lost".to_owned()));
        assert!(task.join().unwrap().is_err());
        let restored = load_state(worker_state.persistence_path.as_path()).unwrap().unwrap();
        assert_eq!(restored.engram_work_naming_history[0].transition.as_ref().unwrap().phase,
            EngramAuthorityPhase::Candidate);
        assert!(restored.engram_named_root_journal.is_empty());
        *claimed.state.inner.lock().unwrap() = restored;
        claimed.state.install_test_engram_budget_clock(clock);
        install_control_only_transport(&claimed.state, claimed.transport.clone());
        let reads = read_count(&claimed);
        assert!(claimed.state.recover_engram_authority_candidate_until(&entry.store, &entry.work_id,
            claimed.state.engram_budget_clock().now() + ENGRAM_ROOT_RECLAMATION_BUDGET).unwrap());
        assert_eq!(read_count(&claimed), reads);
        let inner = claimed.state.inner.lock().unwrap();
        let published = inner.engram_work_naming_history[0].transition.as_ref().unwrap();
        assert_eq!(published.id, owner.id);
        assert_eq!(published.version, owner.version);
        assert_eq!(published.phase, EngramAuthorityPhase::Published);
        assert!(!engram_authority_work_unresolved(&inner, &entry.store, &entry.work_id));
        assert!(inner.engram_work_source_roots.is_empty());
        assert!(inner.engram_named_root_journal.is_empty());
        assert_eq!(inner.engram_work_naming_history[0].retirements, vec![notice]);
        assert_eq!(inner.engram_source_root_generation, floor);
        assert_eq!(engram_named_root_capacity(&inner), 0);
    }
}
