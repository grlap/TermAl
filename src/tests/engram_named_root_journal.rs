// Owns behavioral coverage of journal census, admission and capture custody.
// Does not simulate Engram receipts or judge source identities. Naming and
// ordinary-check setup come from engram_source_root_naming.rs (parent module).
use super::*;

#[test]
fn a_new_naming_past_256_preserves_pending_journal_entries() {
    let label = "journal-past-old-capacity";
    let turn = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&turn.root);
    request_naming(&turn, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    let before = {
        let mut inner = turn.state.inner.lock().unwrap();
        let template = inner.engram_named_root_journal[0]
            .confirmed
            .as_ref()
            .unwrap()
            .0
            .clone();
        inner.engram_work_source_roots.clear();
        inner.engram_named_root_journal = (0..256)
            .map(|i| {
                let mut event = template.clone();
                event.root.claim_id = format!("protected-claim-{i}");
                EngramNamedRootJournal {
                    store: event.root.store.clone(),
                    claim_id: event.root.claim_id.clone(),
                    read_binding: None,
                    confirmed: None,
                    pending: Some(event),
                    obsolete: false,
                    retirement: None,
                    reconciliation: None,
                }
            })
            .collect();
        inner.engram_named_root_journal.clone()
    };
    let named = request_naming(&turn, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    let inner = turn.state.inner.lock().unwrap();
    assert_eq!(inner.engram_named_root_journal.len(), 257);
    assert_eq!(
        &inner.engram_named_root_journal[..256],
        before.as_slice(),
        "admission must not discard pending intents to make room"
    );
    let added = &inner.engram_named_root_journal[256];
    assert_eq!(
        added.confirmed.as_ref().unwrap().0.root.generation,
        named.generation
    );
    assert!(
        added.pending.is_none(),
        "the new naming completed through the host"
    );
}

/// An ended-history image with one real live binding and an otherwise full
/// journal. This is deliberately not a claim about today's running host.
fn fill_settled_journal(turn: &EOCheckedTurn) -> (EngramNamedRootEvent, EngramControlWorkBinding) {
    let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
    let original = inner.engram_named_root_journal[0].clone();
    let mut fresh = original.confirmed.as_ref().unwrap().0.clone();
    let mut binding = original.read_binding.clone().unwrap();
    for n in 1..ENGRAM_NAMED_ROOT_JOURNAL_LIMIT {
        let mut journal = original.clone();
        journal.claim_id = format!("ended-claim-{n}");
        let association = journal.read_binding.as_mut().unwrap();
        association.claim_id = journal.claim_id.clone();
        association.work_id = format!("ended-work-{n}");
        association.run_id = format!("ended-run-{n}");
        association.root_execution_id = format!("ended-execution-{n}");
        let (event, receipt) = journal.confirmed.as_mut().unwrap();
        event.root.claim_id = association.claim_id.clone();
        event.root.work_id = association.work_id.clone();
        event.root.generation = fresh.root.generation + n as u64;
        event.kind = EngramNamedRootKind::Ended;
        event.end_reason = Some(EngramNamedRootEndReason::ExplicitClear);
        receipt.kind = EngramNamedRootKind::Ended;
        receipt.generation = event.root.generation as i64;
        receipt.position.feed.id = association.run_id.clone();
        journal.retire(EngramRootRetirement::Unbound);
        inner.engram_named_root_journal.push(journal);
    }
    fresh.root.claim_id = "fresh-claim".to_owned();
    fresh.root.work_id = "fresh-work".to_owned();
    fresh.root.generation += ENGRAM_NAMED_ROOT_JOURNAL_LIMIT as u64;
    binding.claim_id = fresh.root.claim_id.clone();
    binding.work_id = fresh.root.work_id.clone();
    binding.run_id = "fresh-run".to_owned();
    binding.root_execution_id = "fresh-execution".to_owned();
    (fresh, binding)
}

/// This first diagnostic slice preserves the existing retention rule. One
/// pending capture currently protects unrelated ended journals too. Admission
/// plus its positive control demonstrate that behavior without guessing the
/// live journal's contents or deleting in-flight custody to make room.
#[test]
fn journal_census_explains_capacity_held_by_one_pending_capture() {
    let label = "journal-pending-capture-census";
    let turn = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&turn.root);
    request_naming(&turn, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    turn.begin("Observe a check while naming history is retained.");
    turn.recorder()
        .command_started_in(
            "capture-check",
            SIZE_TEST,
            Some(SIZE_TEST),
            Some(worktree.to_str().unwrap()),
        )
        .unwrap();
    turn.wait_for_snapshots();
    let captured = turn.record(|record| {
        record.engram.active_turn_checks[0]
            .start_basis
            .measured_value()
            .unwrap()
            .0
    });
    assert!(
        captured.is_some(),
        "the real start capture is a positive control"
    );
    let workers = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (release, waiting) = std::sync::mpsc::channel::<()>();
    let capture = EngramCapture::spawn(&workers, &turn.state.capture_lifetime, move || {
        let _released_or_dropped = waiting.recv();
        captured
    });
    turn.record_mut(|record| record.engram.active_turn_checks[0].start_basis = capture.clone());
    let (fresh, binding) = fill_settled_journal(&turn);
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let before = inner.engram_named_root_journal.clone();
        let census = engram_root_journal_census(&inner);
        assert_eq!(census.entries, ENGRAM_NAMED_ROOT_JOURNAL_LIMIT);
        assert_eq!(census.selected_roots, 1);
        assert_eq!(
            census.primary_reasons[&EngramRootJournalRetention::CapturePending],
            ENGRAM_NAMED_ROOT_JOURNAL_LIMIT - 1
        );
        assert_eq!(
            census.primary_reasons[&EngramRootJournalRetention::ConfirmedBinding],
            1
        );
        let refusal = turn
            .state
            .stage_engram_root_event_locked(&mut inner, &fresh, Some(&binding))
            .expect_err("the pending capture currently pins all ended history");
        assert_eq!(refusal.status, StatusCode::CONFLICT);
        assert!(
            refusal.message.contains(&format!(
                "capturePending={}",
                ENGRAM_NAMED_ROOT_JOURNAL_LIMIT - 1
            )),
            "{}",
            refusal.message
        );
        assert_eq!(
            inner.engram_named_root_journal, before,
            "refusal preserves custody"
        );
    }
    // Completing the actual capture, rather than clearing the check, frees
    // the unrelated history. Its result still belongs to the live generation.
    release.send(()).unwrap();
    assert!(
        capture
            .wait_until(std::time::Instant::now() + DEADLOCK_GUARD)
            .unwrap()
            .is_some()
    );
    let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
    let census = engram_root_journal_census(&inner);
    assert_eq!(census.reclaimable, ENGRAM_NAMED_ROOT_JOURNAL_LIMIT - 1);
    turn.state
        .stage_engram_root_event_locked(&mut inner, &fresh, Some(&binding))
        .unwrap();
    assert_eq!(inner.engram_named_root_journal.len(), 2);
    assert!(
        inner
            .engram_named_root_journal
            .iter()
            .any(|entry| entry.pending.as_ref() == Some(&fresh))
    );
}

#[test]
fn journal_census_distinguishes_a_busy_capture_from_a_completed_capture() {
    let label = "journal-busy-capture-census";
    let turn = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&turn.root);
    request_naming(&turn, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    turn.begin("Keep a completed source capture.");
    turn.recorder()
        .command_started_in(
            "busy-check",
            SIZE_TEST,
            Some(SIZE_TEST),
            Some(worktree.to_str().unwrap()),
        )
        .unwrap();
    turn.wait_for_snapshots();
    let capture = turn.record(|record| record.engram.active_turn_checks[0].start_basis.clone());
    fill_settled_journal(&turn);
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let publication = capture
            .result
            .lock()
            .expect("capture result mutex poisoned");
        let census = engram_root_journal_census(&inner);
        assert_eq!(
            census.primary_reasons[&EngramRootJournalRetention::CaptureLockUnavailable],
            ENGRAM_NAMED_ROOT_JOURNAL_LIMIT - 1
        );
        engram_compact_root_journal(&mut inner);
        assert_eq!(
            inner.engram_named_root_journal.len(),
            ENGRAM_NAMED_ROOT_JOURNAL_LIMIT,
            "no result-lock wait or custody loss"
        );
        drop(publication);
    }
    let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
    engram_compact_root_journal(&mut inner);
    assert_eq!(inner.engram_named_root_journal.len(), 1);
    assert_eq!(
        inner.sessions[inner.find_session_index(&turn.session_id).unwrap()]
            .engram
            .active_turn_checks
            .len(),
        1,
        "the completed check remains intact"
    );
}

#[test]
fn journal_census_claim_custody_outranks_another_sessions_pending_capture() {
    let label = "journal-two-session-census";
    let turn = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&turn.root);
    request_naming(&turn, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    turn.begin("Keep a check in the first session.");
    turn.recorder()
        .command_started_in(
            "first-session-check",
            SIZE_TEST,
            Some(SIZE_TEST),
            Some(worktree.to_str().unwrap()),
        )
        .unwrap();
    turn.wait_for_snapshots();
    let pending = Arc::new(EngramBasisCapture::default());
    turn.record_mut(|record| record.engram.active_turn_checks[0].start_basis = pending.clone());
    fill_settled_journal(&turn);
    let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
    let binding = inner.engram_named_root_journal[1]
        .read_binding
        .clone()
        .unwrap();
    let second = inner.create_session(
        Agent::Codex,
        Some("Second reference owner".to_owned()),
        turn.state.default_workdir.clone(),
        None,
        None,
    );
    let second_id = second.session.id.clone();
    inner.session_mut(&second_id).unwrap().engram.work_binding = Some(binding);
    let census = engram_root_journal_census(&inner);
    assert_eq!(
        census.primary_reasons[&EngramRootJournalRetention::RuntimeBinding],
        1
    );
    assert_eq!(
        census.primary_reasons[&EngramRootJournalRetention::CapturePending],
        ENGRAM_NAMED_ROOT_JOURNAL_LIMIT - 2
    );
    let first_index = inner.find_session_index(&turn.session_id).unwrap();
    let second_index = inner.find_session_index(&second_id).unwrap();
    inner.sessions.swap(first_index, second_index);
    assert_eq!(
        engram_root_journal_census(&inner).primary_reasons,
        census.primary_reasons,
        "inserting a session earlier cannot change the reported remedy"
    );
    pending.finish(None);
    let census = engram_root_journal_census(&inner);
    assert_eq!(
        census.retained, 2,
        "finishing a capture cannot release another session's binding"
    );
    assert_eq!(census.reclaimable, ENGRAM_NAMED_ROOT_JOURNAL_LIMIT - 2);
    engram_compact_root_journal(&mut inner);
    assert_eq!(inner.engram_named_root_journal.len(), 2);
    inner.session_mut(&second_id).unwrap().engram.work_binding = None;
    engram_compact_root_journal(&mut inner);
    assert_eq!(
        inner.engram_named_root_journal.len(),
        1,
        "last actual reference controls reclamation"
    );
}

#[tokio::test]
async fn journal_census_route_is_bounded_private_and_read_only() {
    use tower::ServiceExt;
    let label = "private-journal-census";
    let turn = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&turn.root);
    request_naming(&turn, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    fill_settled_journal(&turn);
    let (before, revision) = {
        let inner = turn.state.inner.lock().expect("state mutex poisoned");
        (inner.engram_named_root_journal.clone(), inner.revision)
    };
    let response = app_router(turn.state.clone())
        .oneshot(
            axum::http::Request::get("/api/diagnostics/named-root-journal")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let view: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(view["revision"], revision);
    assert_eq!(view["journal"]["limit"], 1024);
    assert_eq!(view["journal"]["entries"], ENGRAM_NAMED_ROOT_JOURNAL_LIMIT);
    assert_eq!(view["journal"]["retained"], 1);
    assert_eq!(
        view["journal"]["reclaimable"],
        ENGRAM_NAMED_ROOT_JOURNAL_LIMIT - 1
    );
    assert_eq!(view["journal"]["primaryReasons"]["confirmedBinding"], 1);
    assert_eq!(view["serverInstanceId"], turn.state.server_instance_id);
    let wire = String::from_utf8(bytes.to_vec()).unwrap();
    for private in [
        label,
        turn.session_id.as_str(),
        worktree.to_str().unwrap(),
        "ended-claim-",
    ] {
        assert!(
            !wire.contains(private),
            "the census contains no private identity: {wire}"
        );
    }
    let inner = turn.state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.revision, revision);
    assert_eq!(
        inner.engram_named_root_journal, before,
        "GET cannot compact the journal"
    );
}
