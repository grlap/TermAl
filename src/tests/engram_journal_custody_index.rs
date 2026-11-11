// Owns mixed-inventory equivalence and scan-work coverage.
use super::*;

/// Count actual nonblocking result-lock attempts, not elapsed time. Increasing
/// both unrelated history and active checks must not multiply capture reads.
#[test]
fn journal_scan_samples_each_capture_once_as_history_and_checks_grow() {
    let turn = EOCheckedTurn::idle("journal-scan-work", true);
    let worktree = naming_worktree(&turn.root);
    request_naming(
        &turn,
        "journal-scan-work",
        Some(&worktree),
        vec![naming_claim("journal-scan-work")],
    )
    .unwrap();
    turn.begin("Observe journal scan work.");
    turn.recorder()
        .command_started_in(
            "scan-check",
            SIZE_TEST,
            Some(SIZE_TEST),
            Some(worktree.to_str().unwrap()),
        )
        .unwrap();
    turn.wait_for_snapshots();
    let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
    let session = inner.find_session_index(&turn.session_id).unwrap();
    let check = inner.sessions[session].engram.active_turn_checks[0].clone();
    assert!(check.start_basis.measured_value().unwrap().0.is_some());
    let original = inner.engram_named_root_journal[0].clone();
    for size in [8usize, 32, 128] {
        inner.sessions[session].engram.active_turn_checks = vec![check.clone(); size];
        inner.engram_named_root_journal = (0..size)
            .map(|n| {
                let mut journal = original.clone();
                journal.claim_id = format!("unreferenced-{n}");
                journal.retire(EngramRootRetirement::Unbound);
                let (event, _) = journal.confirmed.as_mut().unwrap();
                event.kind = EngramNamedRootKind::Ended;
                event.root.generation += 10000 + n as u64;
                journal
            })
            .collect();
        ENGRAM_JOURNAL_CAPTURE_READS.with(|count| count.set(0));
        let census = engram_root_journal_census(&inner);
        assert_eq!(census.reclaimable, size);
        assert_eq!(census.retained, 0);
        let reads = ENGRAM_JOURNAL_CAPTURE_READS.with(|count| count.get());
        assert!(
            reads > 0,
            "the unprotected journals must actually inspect captures"
        );
        assert!(
            reads <= size,
            "{size} checks and journals used {reads} capture reads; a pass must not rescan captures per journal"
        );
    }
}

#[test]
fn journal_index_preserves_every_primary_class_and_compaction() {
    use EngramRootJournalRetention as Keep;
    let turn = EOCheckedTurn::idle("journal-index-classes", true);
    let worktree = naming_worktree(&turn.root);
    request_naming(
        &turn,
        "journal-index-classes",
        Some(&worktree),
        vec![naming_claim("journal-index-classes")],
    )
    .unwrap();
    let (delegation_id, _) = crate::tests::acceptance_evaluation::install_evaluator_delegation(
        &turn.state,
        &turn.session_id,
        None,
        worktree.to_str().unwrap(),
        1,
    );
    turn.begin("Exercise retained journal protections.");
    turn.recorder()
        .command_started_in(
            "class-check",
            SIZE_TEST,
            Some(SIZE_TEST),
            Some(worktree.to_str().unwrap()),
        )
        .unwrap();
    turn.wait_for_snapshots();
    let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
    let session = inner.find_session_index(&turn.session_id).unwrap();
    let delegation = inner.find_delegation_index(&delegation_id).unwrap();
    let original = inner.engram_named_root_journal[0].clone();
    let real_root = original.confirmed.as_ref().unwrap().0.root.clone();
    let check = inner.sessions[session].engram.active_turn_checks[0].clone();
    let captured_generation = check
        .start_basis
        .measured_value()
        .unwrap()
        .0
        .unwrap()
        .source_root_generation
        .unwrap() as u64;
    let baseline_sessions = inner.sessions.clone();
    let baseline_history = inner.engram_work_naming_history.clone();
    let baseline_roots = inner.engram_work_source_roots.clone();
    let baseline_delegations = inner.delegations.clone();
    let (pending_release, pending_wait) = std::sync::mpsc::channel::<()>();
    let workers = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let pending_capture = EngramCapture::spawn(&workers, &turn.state.capture_lifetime, move || {
        let _ = pending_wait.recv();
        None
    });

    let cases = [
        None,
        Some(Keep::UnpublishedRetirement),
        Some(Keep::PendingIntent),
        Some(Keep::UnsettledReconciliation),
        Some(Keep::UnresolvedRetirement),
        Some(Keep::Evaluator),
        Some(Keep::ConfirmedBinding),
        Some(Keep::SelectedRoot),
        Some(Keep::RuntimeBinding),
        Some(Keep::RuntimeRoot),
        Some(Keep::RuntimeOpening),
        Some(Keep::ClosingJob),
        Some(Keep::ObservationReport),
        Some(Keep::CapturedGeneration),
        Some(Keep::CapturePending),
        Some(Keep::CaptureLockUnavailable),
    ];
    for expected in cases {
        inner.sessions = baseline_sessions.clone();
        inner.engram_work_naming_history = baseline_history.clone().into();
        inner.engram_work_source_roots = baseline_roots.clone().into();
        inner.delegations = baseline_delegations.clone();
        let mut journal = original.clone();
        journal.claim_id = "indexed-claim".into();
        journal.retire(EngramRootRetirement::Unbound);
        let (event, _) = journal.confirmed.as_mut().unwrap();
        event.kind = EngramNamedRootKind::Ended;
        event.root.claim_id = journal.claim_id.clone();
        event.root.work_id = "indexed-work".into();
        event.root.generation = captured_generation + 1000;
        let event = event.clone();
        let mut binding = original.read_binding.clone().unwrap();
        binding.claim_id = journal.claim_id.clone();
        let mut root = real_root.clone();
        root.claim_id = journal.claim_id.clone();
        root.work_id = event.root.work_id.clone();
        let mut release = None;
        match expected {
            Some(Keep::UnpublishedRetirement) => {
                let read =
                    serde_json::from_value(covering_removed_root_read(&original, false)).unwrap();
                inner.engram_work_naming_history[0].retirements.push(
                    EngramNamedRootRetirementNotice {
                        id: "unpublished-notice".into(),
                        selection: root.clone(),
                        binding: binding.clone(),
                        read,
                        line: "retained notice".into(),
                        published: false,
                        delivered: false,
                        owner: None,
                    },
                );
                // Unpublished proof outranks a pending intent.
                journal.pending = Some(event.clone());
            }
            Some(Keep::PendingIntent) => journal.pending = Some(event.clone()),
            Some(Keep::UnsettledReconciliation) => {
                journal.reconciliation = Some(EngramRootReconciliation {
                    intent: event.clone(),
                    state: EngramNamedRootState::None,
                    observed_by: "fixture".into(),
                    observed_at: String::new(),
                    settled: false,
                })
            }
            Some(Keep::UnresolvedRetirement) => journal.retirement = None,
            Some(Keep::Evaluator) => {
                let target = inner.delegations[delegation]
                    .acceptance_evaluation
                    .as_mut()
                    .unwrap();
                target.store = Some(journal.store.clone());
                target.source_claim = Some(AcceptanceEvaluationSourceClaim {
                    work_id: root.work_id.clone(),
                    claim_id: journal.claim_id.clone(),
                    named_generation_at_request: Some(0),
                });
                // Evaluator custody precedes an otherwise live bound root.
                journal.obsolete = false;
                journal.confirmed.as_mut().unwrap().0.kind = EngramNamedRootKind::Bound;
            }
            Some(Keep::ConfirmedBinding) => {
                journal.obsolete = false;
                journal.confirmed.as_mut().unwrap().0.kind = EngramNamedRootKind::Bound;
                inner.engram_work_source_roots.push(root.clone());
            }
            Some(Keep::SelectedRoot) => inner.engram_work_source_roots.push(root.clone()),
            Some(Keep::RuntimeBinding) => {
                inner.sessions[session].engram.work_binding = Some(binding.clone())
            }
            Some(Keep::RuntimeRoot) => {
                inner.sessions[session]
                    .engram
                    .unadmitted_active_turn
                    .as_mut()
                    .unwrap()
                    .opening_root
                    .as_mut()
                    .unwrap()
                    .source_root = Some(EngramTurnSourceRoot::from_entry(&root));
            }
            Some(Keep::RuntimeOpening) => {
                inner.sessions[session]
                    .engram
                    .unadmitted_active_turn
                    .as_mut()
                    .unwrap()
                    .opening_root
                    .as_mut()
                    .unwrap()
                    .binding = binding.clone();
            }
            Some(Keep::ClosingJob) => {
                let trace = inner.sessions[session]
                    .engram
                    .unadmitted_active_turn
                    .clone()
                    .unwrap();
                let mut scope =
                    EngramRuntimeClosingScope::capture(&inner, &turn.session_id).unwrap();
                scope.binding = Some(binding.clone());
                inner.sessions[session]
                    .engram
                    .unadmitted_closing_jobs
                    .push(EngramRuntimeClosingJob::new(trace, None, scope));
            }
            Some(Keep::ObservationReport) => {
                let trace = inner.sessions[session]
                    .engram
                    .unadmitted_active_turn
                    .clone()
                    .unwrap();
                let scope = EngramRuntimeClosingScope::capture(&inner, &turn.session_id).unwrap();
                inner.sessions[session].engram.unadmitted_reports.push(
                    EngramRuntimeObservationReport {
                        host_turn_ref: trace.host_turn_ref,
                        adopted_at: trace.adopted_at,
                        notice_message_id: trace.notice_message_id,
                        generation: trace.generation,
                        version: 1,
                        observing_session: turn.session_id.clone(),
                        connection: scope.connection.clone(),
                        store: journal.store.clone(),
                        call_timeout_ms: 1000,
                        attempts: 0,
                        retry_at: String::new(),
                        envelopes: vec![EngramRuntimeObservationEnvelope {
                            request: json!({"binding":{"claim_id":journal.claim_id}}),
                            delivery: EngramRuntimeObservationDelivery::Pending {
                                last_error: None,
                            },
                        }],
                    },
                );
            }
            Some(Keep::CapturedGeneration) => {
                journal.confirmed.as_mut().unwrap().0.root.generation = captured_generation
            }
            Some(Keep::CapturePending) => {
                let (tx, rx) = std::sync::mpsc::channel::<()>();
                release = Some(tx);
                let workers = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                inner.sessions[session].engram.active_turn_checks[0].start_basis =
                    EngramCapture::spawn(&workers, &turn.state.capture_lifetime, move || {
                        let _ = rx.recv();
                        None
                    });
            }
            Some(Keep::CaptureLockUnavailable) | None => {}
        }
        if expected.is_some()
            && expected != Some(Keep::CapturePending)
            && expected != Some(Keep::CaptureLockUnavailable)
        {
            // Every known primary class, including a captured generation,
            // must outrank an unrelated pending capture on another check.
            let mut pending_check = check.clone();
            pending_check.start_basis = pending_capture.clone();
            inner.sessions[session]
                .engram
                .active_turn_checks
                .push(pending_check);
        }
        let busy = (expected == Some(Keep::CaptureLockUnavailable)).then(|| {
            check
                .start_basis
                .result
                .lock()
                .expect("capture mutex poisoned")
        });
        inner.engram_named_root_journal = vec![journal.clone()].into();
        let index = EngramJournalCustodyIndex::new(&inner);
        assert_eq!(
            engram_root_journal_retention(&index, &journal),
            expected,
            "index {expected:?}"
        );
        drop(index);
        let census = engram_root_journal_census(&inner);
        assert_eq!(
            census
                .primary_reasons
                .get(&expected.unwrap_or(Keep::ClosingJob))
                .copied()
                .unwrap_or(0),
            usize::from(expected.is_some())
        );
        engram_compact_root_journal(&mut inner);
        assert_eq!(
            inner.engram_named_root_journal.len(),
            usize::from(expected.is_some()),
            "compaction {expected:?}"
        );
        drop(busy);
        drop(release);
    }
    drop(pending_release);
}

#[test]
fn journal_index_keeps_evaluator_store_and_generation_boundaries() {
    use EngramRootJournalRetention as Keep;
    let turn = EOCheckedTurn::idle("journal-index-evaluator", true);
    let worktree = naming_worktree(&turn.root);
    request_naming(
        &turn,
        "journal-index-evaluator",
        Some(&worktree),
        vec![naming_claim("journal-index-evaluator")],
    )
    .unwrap();
    let (id, _) = crate::tests::acceptance_evaluation::install_evaluator_delegation(
        &turn.state,
        &turn.session_id,
        None,
        worktree.to_str().unwrap(),
        1,
    );
    let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
    let slot = inner.find_delegation_index(&id).unwrap();
    let mut journal = inner.engram_named_root_journal[0].clone();
    journal.claim_id = "later-claim".into();
    journal.confirmed.as_mut().unwrap().0.kind = EngramNamedRootKind::Ended;
    let root = journal.confirmed.as_ref().unwrap().0.root.clone();
    inner.engram_work_source_roots.clear();
    inner.sessions.clear();
    // The source claim is different; only a later generation of the same work
    // in the same store is protected by a running evaluator.
    for (wrong_store, after, obsolete, running, expected) in [
        (
            false,
            root.generation - 1,
            false,
            true,
            Some(Keep::Evaluator),
        ),
        (false, root.generation, false, true, None),
        (true, 0, false, true, None),
        (false, 0, true, true, None),
        (false, 0, false, false, None),
    ] {
        journal.obsolete = obsolete;
        journal.retirement = Some(EngramRootRetirement::Unbound);
        let record = &mut inner.delegations[slot];
        record.status = if running {
            DelegationStatus::Running
        } else {
            DelegationStatus::Completed
        };
        let target = record.acceptance_evaluation.as_mut().unwrap();
        let mut store = journal.store.clone();
        if wrong_store {
            store.project_id.push_str("-other");
        }
        target.store = Some(store);
        target.source_claim = Some(AcceptanceEvaluationSourceClaim {
            work_id: root.work_id.clone(),
            claim_id: "request-claim".into(),
            named_generation_at_request: Some(after),
        });
        target.source_root = None;
        let index = EngramJournalCustodyIndex::new(&inner);
        assert_eq!(engram_root_journal_retention(&index, &journal), expected);
    }
}

#[test]
fn empty_journal_compaction_skips_custody_inventories() {
    let turn = EOCheckedTurn::idle("journal-empty-scan", true);
    let worktree = naming_worktree(&turn.root);
    request_naming(
        &turn,
        "journal-empty-scan",
        Some(&worktree),
        vec![naming_claim("journal-empty-scan")],
    )
    .unwrap();
    let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
    inner.engram_named_root_journal.clear();
    let before = serde_json::to_vec(&PersistedState::from_inner(&inner)).unwrap();
    ENGRAM_JOURNAL_SESSION_VISITS.with(|count| count.set(0));
    let census = engram_root_journal_census(&inner);
    assert_eq!(census.entries, 0);
    assert!(
        ENGRAM_JOURNAL_SESSION_VISITS.with(|count| count.get()) > 0,
        "diagnostics still reads resident custody even without journal entries"
    );
    ENGRAM_JOURNAL_SESSION_VISITS.with(|count| count.set(0));
    engram_compact_root_journal(&mut inner);
    assert_eq!(
        ENGRAM_JOURNAL_SESSION_VISITS.with(|count| count.get()),
        0,
        "empty compaction must not visit the session custody inventory"
    );
    assert_eq!(
        serde_json::to_vec(&PersistedState::from_inner(&inner)).unwrap(),
        before
    );
}

#[test]
fn evaluator_custody_uses_the_earliest_active_request_in_either_order() {
    use EngramRootJournalRetention as Keep;
    let turn = EOCheckedTurn::idle("journal-evaluator-min", true);
    let worktree = naming_worktree(&turn.root);
    request_naming(
        &turn,
        "journal-evaluator-min",
        Some(&worktree),
        vec![naming_claim("journal-evaluator-min")],
    )
    .unwrap();
    for _ in 0..2 {
        crate::tests::acceptance_evaluation::install_evaluator_delegation(
            &turn.state,
            &turn.session_id,
            None,
            worktree.to_str().unwrap(),
            1,
        );
    }
    let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
    let mut journal = inner.engram_named_root_journal[0].clone();
    journal.claim_id = "later-claim".into();
    let (event, _) = journal.confirmed.as_mut().unwrap();
    event.kind = EngramNamedRootKind::Ended;
    event.root.generation += 10;
    let root = event.root.clone();
    inner.engram_work_source_roots.clear();
    inner.sessions.clear();
    assert_eq!(inner.delegations.len(), 2);
    for (record, after) in inner
        .delegations
        .iter_mut()
        .zip([root.generation - 1, root.generation + 1])
    {
        let target = record.acceptance_evaluation.as_mut().unwrap();
        target.store = Some(journal.store.clone());
        target.source_root = None;
        target.source_claim = Some(AcceptanceEvaluationSourceClaim {
            work_id: root.work_id.clone(),
            claim_id: "request-claim".into(),
            named_generation_at_request: Some(after),
        });
    }
    inner.engram_named_root_journal = vec![journal.clone()].into();
    for _ in 0..2 {
        let census = engram_root_journal_census(&inner);
        assert_eq!(census.primary_reasons.get(&Keep::Evaluator), Some(&1));
        assert_eq!(census.reclaimable, 0);
        inner.delegations.reverse();
    }
    let earliest = inner
        .delegations
        .iter_mut()
        .find(|record| {
            record
                .acceptance_evaluation
                .as_ref()
                .unwrap()
                .source_claim
                .as_ref()
                .unwrap()
                .named_generation_at_request
                .unwrap()
                < root.generation
        })
        .unwrap();
    earliest.status = DelegationStatus::Completed;
    let census = engram_root_journal_census(&inner);
    assert_eq!(
        census.reclaimable, 1,
        "the later request alone cannot hold an earlier generation"
    );
    engram_compact_root_journal(&mut inner);
    assert!(inner.engram_named_root_journal.is_empty());
}

#[test]
fn runtime_custody_priority_is_independent_of_session_insertion_order() {
    use EngramRootJournalRetention as Keep;
    let turn = EOCheckedTurn::idle("journal-runtime-priority", true);
    let worktree = naming_worktree(&turn.root);
    request_naming(
        &turn,
        "journal-runtime-priority",
        Some(&worktree),
        vec![naming_claim("journal-runtime-priority")],
    )
    .unwrap();
    turn.begin("Retain the original runtime opening.");
    let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
    let original_session = inner.find_session_index(&turn.session_id).unwrap();
    let mut journal = inner.engram_named_root_journal[0].clone();
    journal.claim_id = "shared-runtime-claim".into();
    journal.retire(EngramRootRetirement::Unbound);
    journal.confirmed.as_mut().unwrap().0.kind = EngramNamedRootKind::Ended;
    let mut binding = journal.read_binding.clone().unwrap();
    binding.claim_id = journal.claim_id.clone();
    // The first session owns only an opening, a lower-priority protection.
    inner.sessions[original_session].engram.work_binding = None;
    let opening = inner.sessions[original_session]
        .engram
        .unadmitted_active_turn
        .as_mut()
        .unwrap()
        .opening_root
        .as_mut()
        .unwrap();
    opening.source_root = None;
    opening.binding = binding.clone();
    let second_id = inner
        .create_session(
            Agent::Codex,
            Some("Binding holder".into()),
            worktree.to_string_lossy().into_owned(),
            None,
            None,
        )
        .session
        .id;
    inner.session_mut(&second_id).unwrap().engram.work_binding = Some(binding);
    inner.engram_work_source_roots.clear();
    inner.engram_named_root_journal = vec![journal].into();
    for _ in 0..2 {
        let census = engram_root_journal_census(&inner);
        assert_eq!(census.primary_reasons.get(&Keep::RuntimeBinding), Some(&1));
        assert!(!census.primary_reasons.contains_key(&Keep::RuntimeOpening));
        inner.sessions.reverse();
    }
    inner.session_mut(&second_id).unwrap().engram.work_binding = None;
    assert_eq!(
        engram_root_journal_census(&inner)
            .primary_reasons
            .get(&Keep::RuntimeOpening),
        Some(&1)
    );
    inner
        .session_mut(&turn.session_id)
        .unwrap()
        .engram
        .unadmitted_active_turn = None;
    assert_eq!(engram_root_journal_census(&inner).reclaimable, 1);
    engram_compact_root_journal(&mut inner);
    assert!(
        inner.engram_named_root_journal.is_empty(),
        "last runtime custody release permits reclamation"
    );
}
