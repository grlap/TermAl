//! A work's retained recovery run that Engram definitively refuses to read is
//! retired and stops blocking the work's confirmation; a damaged store or a
//! transient failure keeps it retained. Mounted from
//! `engram_source_root_naming.rs` to share its naming fixtures.
use super::*;

struct Retained {
    claimed: EOCheckedTurn,
    store: EngramAuthorityStoreKey,
    current: EngramControlWorkBinding,
    earlier: EngramControlWorkBinding,
}

/// A confirmed naming whose work also retains a bare earlier run, with no
/// root journal of its own. The journal shapes a removed or restarted naming
/// session leaves are covered by `retained_through_removed_session`.
fn retained(label: &str) -> Retained {
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(&claimed, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    let store = naming_store(&claimed);
    let current = claimed.record(|record| record.engram.work_binding.clone().unwrap());
    let mut earlier = current.clone();
    earlier.run_id = format!("{label}-earlier-run");
    earlier.claim_id = format!("{label}-earlier-claim");
    earlier.root_execution_id = format!("{label}-earlier-execution");
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        assert_eq!(
            engram_authority_work_unresolved_reason(&inner, &store, &current.work_id),
            None,
            "the naming itself is confirmed"
        );
        history_mut(&mut inner, &store, &current.work_id)
            .unresolved_runs
            .push(earlier.clone());
        assert_eq!(
            engram_authority_work_unresolved_reason(&inner, &store, &current.work_id),
            Some(EngramUnconfirmedRootReason::WorkRunsUnresolved)
        );
    }
    Retained {
        claimed,
        store,
        current,
        earlier,
    }
}

fn history_mut<'a>(
    inner: &'a mut StateInner,
    store: &EngramAuthorityStoreKey,
    work: &str,
) -> &'a mut EngramWorkNamingHistory {
    inner
        .engram_work_naming_history
        .iter_mut()
        .find(|history| &history.store == store && history.work_id == work)
        .expect("the named work has naming history")
}

fn history_json(fixture: &Retained) -> Value {
    let mut inner = fixture.claimed.state.inner.lock().unwrap();
    serde_json::to_value(&*history_mut(
        &mut inner,
        &fixture.store,
        &fixture.current.work_id,
    ))
    .unwrap()
}

/// Engram answers the read of `claim` with its own error, as on the wire.
fn engram_answers(fixture: &Retained, claim: &str, error: Value) {
    fixture
        .claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .root_read_errors
        .insert(claim.to_owned(), error);
}

fn refusal(reason: &str, message: &str) -> Value {
    json!({"code": "named_root_read_refused", "message": message, "reason": reason})
}

fn maintain(fixture: &Retained) {
    fixture
        .claimed
        .state
        .recover_engram_authority_runs(&fixture.claimed.session_id, Duration::from_secs(2));
}

fn unresolved(fixture: &Retained) -> Option<EngramUnconfirmedRootReason> {
    let inner = fixture.claimed.state.inner.lock().unwrap();
    engram_authority_work_unresolved_reason(&inner, &fixture.store, &fixture.current.work_id)
}

const DEFINITIVE: [(&str, &str); 3] = [
    (
        "claim_not_current_for_run",
        "the claim does not belong to the run",
    ),
    ("other_project", "the run belongs to another project"),
    ("run_unknown", "the run is unknown in this store"),
];

#[test]
fn each_definitive_refusal_retires_the_retained_run_with_its_reason_and_store() {
    for (reason, message) in DEFINITIVE {
        let fixture = retained(&format!("retired-{reason}"));
        engram_answers(
            &fixture,
            &fixture.earlier.claim_id,
            refusal(reason, message),
        );
        maintain(&fixture);
        assert_eq!(
            unresolved(&fixture),
            None,
            "{reason}: a retired run no longer blocks the work"
        );
        let history = history_json(&fixture);
        assert_eq!(history["unresolvedRuns"], json!([]), "{reason}");
        let retired = history["retiredRuns"]
            .as_array()
            .unwrap_or_else(|| panic!("{reason}: the retirement is recorded: {history:#}"));
        assert_eq!(retired.len(), 1, "{reason}");
        assert_eq!(
            retired[0]["binding"],
            serde_json::to_value(&fixture.earlier).unwrap(),
            "{reason}"
        );
        assert_eq!(retired[0]["refusal"], json!(reason));
        assert_eq!(
            retired[0]["store"],
            serde_json::to_value(&fixture.store).unwrap(),
            "{reason}: the store that answered is recorded"
        );
        assert!(
            retired[0]["reason"].as_str().unwrap().contains(message),
            "{reason}: {}",
            retired[0]
        );
        // The retired run is never read again and never retained again.
        let reads = fixture.claimed.transport.requests().len();
        maintain(&fixture);
        assert_eq!(
            fixture.claimed.transport.requests().len(),
            reads,
            "{reason}"
        );
        assert_eq!(unresolved(&fixture), None, "{reason}");
    }
}

#[test]
fn a_retired_run_stays_retired_across_a_restart_and_is_never_retained_again() {
    let fixture = retained("retired-across-restart");
    engram_answers(
        &fixture,
        &fixture.earlier.claim_id,
        refusal(
            "claim_not_current_for_run",
            "the claim does not belong to the run",
        ),
    );
    maintain(&fixture);
    assert_eq!(unresolved(&fixture), None);
    let delta = collect_persist_delta_from_shared_state(&fixture.claimed.state.inner, 0);
    let mut cache = SqlitePersistConnectionCache::new();
    persist_delta_via_cache(
        &mut cache,
        fixture.claimed.state.persistence_path.as_path(),
        &delta,
    )
    .unwrap();
    let restored = load_state(fixture.claimed.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let history = restored
        .engram_work_naming_history
        .iter()
        .find(|history| {
            history.store == fixture.store && history.work_id == fixture.current.work_id
        })
        .unwrap();
    let saved = serde_json::to_value(history).unwrap();
    assert_eq!(
        saved["unresolvedRuns"],
        json!([]),
        "a restart does not revive the retired run"
    );
    assert_eq!(
        saved["retiredRuns"][0]["binding"],
        serde_json::to_value(&fixture.earlier).unwrap()
    );
    assert_eq!(
        engram_authority_work_unresolved_reason(
            &restored,
            &fixture.store,
            &fixture.current.work_id
        ),
        None,
        "the restored work is not blocked"
    );
}

#[test]
fn a_retired_run_is_not_retained_again_by_a_later_authority_owner() {
    let fixture = retained("retired-not-retained-again");
    engram_answers(
        &fixture,
        &fixture.earlier.claim_id,
        refusal("run_unknown", "the run is unknown in this store"),
    );
    maintain(&fixture);
    assert_eq!(unresolved(&fixture), None);
    // A later owner of the same run retains its binding for a read; a retired
    // run is not taken back.
    let _ = fixture.claimed.state.prepare_engram_authority(
        &fixture.store,
        &fixture.earlier,
        DEADLOCK_GUARD,
    );
    let history = history_json(&fixture);
    let earlier = serde_json::to_value(&fixture.earlier).unwrap();
    assert!(
        !history["unresolvedRuns"]
            .as_array()
            .unwrap()
            .contains(&earlier),
        "{history:#}"
    );
}

#[test]
fn a_damaged_store_keeps_the_run_and_raises_the_integrity_alarm() {
    for (case, error) in [
        (
            "invalid-projection",
            json!({"code": "storage_error",
            "message": "work projection contains invalid data: a claim crosses its canonical run"}),
        ),
        (
            "unrecognised-refusal",
            json!({"code": "named_root_read_refused",
            "message": "the run is unknown in this store", "reason": "a_future_reason"}),
        ),
    ] {
        let fixture = retained(&format!("damaged-{case}"));
        engram_answers(&fixture, &fixture.earlier.claim_id, error);
        maintain(&fixture);
        assert_eq!(
            unresolved(&fixture),
            Some(EngramUnconfirmedRootReason::WorkRunsUnresolved),
            "{case}"
        );
        let history = history_json(&fixture);
        assert_eq!(
            history["unresolvedRuns"],
            json!([serde_json::to_value(&fixture.earlier).unwrap()]),
            "{case}"
        );
        assert!(
            history.get("retiredRuns").is_none(),
            "{case}: nothing is retired: {history:#}"
        );
        let reason = history["recoveryReason"].as_str().unwrap();
        if case == "invalid-projection" {
            assert!(reason.starts_with("store integrity alarm: "), "{reason}");
        } else {
            assert!(
                !reason.contains("store integrity alarm"),
                "an unrecognised refusal is neither retired nor store damage: {reason}"
            );
        }
    }
}

#[test]
fn a_contradictory_canonical_answer_keeps_the_run_and_raises_the_integrity_alarm() {
    let fixture = retained("damaged-contradiction");
    // A reply for another run than the one asked for contradicts its association.
    let proof = json!({"project_id": fixture.store.project_id, "work_id": fixture.earlier.work_id,
        "root_execution_id": fixture.earlier.root_execution_id, "run_id": "a-different-run",
        "claim_id": fixture.earlier.claim_id, "run": {"state": "claimed", "generation": 1},
        "named_root": {"state": "none"}, "latest_event": null,
        "read_cut": {"feed": {"kind": "run_execution", "id": "a-different-run"}, "position": 0}});
    fixture
        .claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .root_reads
        .insert(fixture.earlier.claim_id.clone(), proof);
    maintain(&fixture);
    assert_eq!(
        unresolved(&fixture),
        Some(EngramUnconfirmedRootReason::WorkRunsUnresolved)
    );
    let history = history_json(&fixture);
    assert!(history.get("retiredRuns").is_none());
    assert!(
        history["recoveryReason"]
            .as_str()
            .unwrap()
            .starts_with("store integrity alarm: "),
        "{history:#}"
    );
}

#[test]
fn transient_failures_keep_the_run_retained_for_a_later_read() {
    for case in ["timeout", "session-authority", "busy"] {
        let fixture = retained(&format!("transient-{case}"));
        match case {
            "timeout" => {
                fixture
                    .claimed
                    .transport
                    .named_roots
                    .lock()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .root_read_failures
                    .insert(fixture.earlier.claim_id.clone());
            }
            "session-authority" => engram_answers(
                &fixture,
                &fixture.earlier.claim_id,
                json!({"code": "control_connection_superseded", "message": "the connection was superseded"}),
            ),
            _ => engram_answers(
                &fixture,
                &fixture.earlier.claim_id,
                json!({"code": "storage_error", "message": "database is locked"}),
            ),
        }
        maintain(&fixture);
        assert_eq!(
            unresolved(&fixture),
            Some(EngramUnconfirmedRootReason::WorkRunsUnresolved),
            "{case}"
        );
        let history = history_json(&fixture);
        assert!(history.get("retiredRuns").is_none(), "{case}");
        assert!(
            !history["recoveryReason"]
                .as_str()
                .unwrap()
                .contains("store integrity alarm"),
            "{case}"
        );
    }
}

#[test]
fn a_naming_reports_retained_runs_its_reads_could_not_answer() {
    let label = "naming-reports-retained";
    let fixture = retained(label);
    fixture
        .claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .root_read_failures
        .insert(fixture.earlier.claim_id.clone());
    let again = another_worktree(&fixture, "naming-again");
    let response = request_naming(
        &fixture.claimed,
        label,
        Some(&again),
        vec![naming_claim(label)],
    )
    .unwrap();
    assert!(
        response
            .notice
            .contains("Earlier runs of this work are still unresolved"),
        "a naming never reports a confirmed root while runs stay retained: {}",
        response.notice
    );
    assert!(
        response.notice.contains(&fixture.earlier.run_id),
        "{}",
        response.notice
    );
    // Once Engram answers definitively, the next naming reads the run and says nothing more.
    fixture
        .claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .root_read_failures
        .remove(&fixture.earlier.claim_id);
    engram_answers(
        &fixture,
        &fixture.earlier.claim_id,
        refusal(
            "claim_not_current_for_run",
            "the claim does not belong to the run",
        ),
    );
    let third = another_worktree(&fixture, "naming-third");
    let response = request_naming(
        &fixture.claimed,
        label,
        Some(&third),
        vec![naming_claim(label)],
    )
    .unwrap();
    assert!(
        !response.notice.contains("Earlier runs of this work"),
        "{}",
        response.notice
    );
    assert_eq!(unresolved(&fixture), None);
}

fn another_worktree(fixture: &Retained, name: &str) -> PathBuf {
    let path = fixture.claimed.root.join(".worktrees").join(name);
    run_git_test_command(
        &fixture.claimed.root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            name,
            path.to_str().unwrap(),
        ],
    );
    path
}

/// The naming session is removed, as a restart or a deleted session leaves
/// it: its run is retained together with its root journal, and the session
/// that replaces it works under a newer claim, so nothing holds that run.
fn retained_through_removed_session(
    label: &str,
) -> (
    EOCheckedTurn,
    EngramAuthorityStoreKey,
    EngramControlWorkBinding,
) {
    let mut claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(&claimed, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    let store = naming_store(&claimed);
    let original = claimed.record(|record| record.engram.work_binding.clone().unwrap());
    replace_naming_session(&mut claimed, label);
    let mut newer = original.clone();
    newer.claim_id = format!("{label}-newer-claim");
    newer.run_id = format!("{label}-newer-run");
    newer.root_execution_id = format!("{label}-newer-execution");
    claimed.record_mut(|record| record.engram.work_binding = Some(newer.clone()));
    claimed
        .transport
        .enable_named_roots(&claimed.session_id, &newer);
    {
        let inner = claimed.state.inner.lock().unwrap();
        let journal =
            engram_root_journal(&inner.engram_named_root_journal, &store, &original.claim_id)
                .unwrap();
        assert!(
            journal.requires_reconciliation(),
            "the removed session's journal needs reconciliation"
        );
        assert!(
            engram_authority_work_unresolved_reason(&inner, &store, &original.work_id).is_some()
        );
    }
    (claimed, store, original)
}

#[test]
fn a_retirement_settles_the_root_journal_a_removed_naming_session_left() {
    for (reason, message, retirement) in [
        (
            "claim_not_current_for_run",
            "the claim does not belong to the run",
            EngramRootRetirement::Displaced,
        ),
        (
            "run_unknown",
            "the run is unknown in this store",
            EngramRootRetirement::Unbound,
        ),
    ] {
        let label = format!("removed-session-{reason}");
        let (claimed, store, original) = retained_through_removed_session(&label);
        claimed
            .transport
            .named_roots
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .root_read_errors
            .insert(original.claim_id.clone(), refusal(reason, message));
        claimed
            .state
            .recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
        let inner = claimed.state.inner.lock().unwrap();
        assert_eq!(
            engram_authority_work_unresolved_reason(&inner, &store, &original.work_id),
            None,
            "{reason}: neither the run nor its journal blocks the work any more"
        );
        let journal =
            engram_root_journal(&inner.engram_named_root_journal, &store, &original.claim_id);
        if let Some(journal) = journal {
            assert!(!journal.requires_reconciliation(), "{reason}");
            assert_eq!(journal.retirement, Some(retirement), "{reason}");
        }
        let history = inner
            .engram_work_naming_history
            .iter()
            .find(|history| history.store == store && history.work_id == original.work_id)
            .unwrap();
        assert!(history.run_is_retired(&original), "{reason}");
    }
}

#[test]
fn a_retirement_settles_the_journal_shapes_a_restart_leaves() {
    // Restore leaves a gone namer's journal either with a pending cleanup
    // intent whose reporter no longer exists, or retired as unconfirmed.
    for shape in ["abandoned-cleanup-intent", "association-unconfirmed"] {
        let label = format!("restart-shape-{shape}");
        let (claimed, store, original) = retained_through_removed_session(&label);
        {
            let mut inner = claimed.state.inner.lock().unwrap();
            let journal = inner
                .engram_named_root_journal
                .iter_mut()
                .find(|journal| journal.store == store && journal.claim_id == original.claim_id)
                .unwrap();
            if shape == "abandoned-cleanup-intent" {
                let mut intent = journal.confirmed.as_ref().unwrap().0.clone();
                intent.reporter = "a-session-gone-at-restore".to_owned();
                journal.pending = Some(intent);
            } else {
                journal.retire(EngramRootRetirement::AssociationUnconfirmed);
            }
        }
        claimed
            .transport
            .named_roots
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .root_read_errors
            .insert(
                original.claim_id.clone(),
                refusal(
                    "claim_not_current_for_run",
                    "the claim does not belong to the run",
                ),
            );
        claimed
            .state
            .recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
        let inner = claimed.state.inner.lock().unwrap();
        assert_eq!(
            engram_authority_work_unresolved_reason(&inner, &store, &original.work_id),
            None,
            "{shape}"
        );
        if let Some(journal) =
            engram_root_journal(&inner.engram_named_root_journal, &store, &original.claim_id)
        {
            assert!(
                journal.pending.is_none() && !journal.requires_reconciliation(),
                "{shape}"
            );
            if shape == "abandoned-cleanup-intent" {
                let settled = journal
                    .reconciliation
                    .as_ref()
                    .expect("the intent is kept as readback");
                assert!(settled.settled, "{shape}");
            }
        }
    }
}

#[test]
fn a_live_reporters_pending_intent_keeps_a_refused_run_retained() {
    let label = "removed-session-live-intent";
    let (claimed, store, original) = retained_through_removed_session(label);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let successor = claimed.session_id.clone();
        let journal = inner
            .engram_named_root_journal
            .iter_mut()
            .find(|journal| journal.store == store && journal.claim_id == original.claim_id)
            .unwrap();
        let mut intent = journal.confirmed.as_ref().unwrap().0.clone();
        // A live session still reports this intent.
        intent.reporter = successor;
        journal.pending = Some(intent);
    }
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .root_read_errors
        .insert(
            original.claim_id.clone(),
            refusal(
                "claim_not_current_for_run",
                "the claim does not belong to the run",
            ),
        );
    claimed
        .state
        .recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    let inner = claimed.state.inner.lock().unwrap();
    let history = inner
        .engram_work_naming_history
        .iter()
        .find(|history| history.store == store && history.work_id == original.work_id)
        .unwrap();
    assert!(
        !history.run_is_retired(&original),
        "a live reporter's intent is never discarded"
    );
    assert!(
        engram_root_journal(&inner.engram_named_root_journal, &store, &original.claim_id)
            .unwrap()
            .pending
            .is_some()
    );
    assert!(engram_authority_work_unresolved_reason(&inner, &store, &original.work_id).is_some());
}

#[test]
fn a_refusal_answered_after_the_reader_changed_retires_nothing() {
    let fixture = retained("refused-after-reader-change");
    let host = fixture.claimed.state.clone();
    {
        let mut roots = fixture.claimed.transport.named_roots.lock().unwrap();
        let roots = roots.as_mut().unwrap();
        roots.root_read_errors.insert(
            fixture.earlier.claim_id.clone(),
            refusal("run_unknown", "the run is unknown in this store"),
        );
        // The reader's connection is replaced while the refusal is in flight.
        roots.on_root_read_error = Some(Box::new(move || {
            change_engram_settings(&host, |settings| {
                settings.binary_path = Some("successor-engram.exe".into())
            });
        }));
    }
    // One batch, before any publication reads again with the new reader.
    let owner = fixture
        .claimed
        .state
        .prepare_engram_authority(&fixture.store, &fixture.earlier, DEADLOCK_GUARD)
        .unwrap();
    let clock = fixture.claimed.state.engram_budget_clock();
    fixture
        .claimed
        .state
        .recover_engram_authority_batch_until(
            &fixture.store,
            &owner,
            clock.now() + DEADLOCK_GUARD,
            true,
            None,
        )
        .unwrap();
    let history = history_json(&fixture);
    assert!(
        history.get("retiredRuns").is_none(),
        "only a still-current reader can retire: {history:#}"
    );
    assert!(
        history["unresolvedRuns"]
            .as_array()
            .unwrap()
            .contains(&serde_json::to_value(&fixture.earlier).unwrap()),
        "{history:#}"
    );
}

#[test]
fn a_reporter_that_returns_during_a_refused_read_keeps_its_intent() {
    let label = "removed-session-reporter-returns";
    let (claimed, store, original) = retained_through_removed_session(label);
    let returning = "session-returning-reporter".to_owned();
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let journal = inner
            .engram_named_root_journal
            .iter_mut()
            .find(|journal| journal.store == store && journal.claim_id == original.claim_id)
            .unwrap();
        let mut intent = journal.confirmed.as_ref().unwrap().0.clone();
        // Gone when the batch starts, so the intent counts as abandoned.
        intent.reporter = returning.clone();
        journal.pending = Some(intent);
    }
    let host = claimed.state.clone();
    let successor = claimed.session_id.clone();
    {
        let mut roots = claimed.transport.named_roots.lock().unwrap();
        let roots = roots.as_mut().unwrap();
        roots.root_read_errors.insert(
            original.claim_id.clone(),
            refusal(
                "claim_not_current_for_run",
                "the claim does not belong to the run",
            ),
        );
        // The reporter comes back while the refusal is in flight.
        roots.on_root_read_error = Some(Box::new(move || {
            let mut inner = host.inner.lock().unwrap();
            let index = inner.find_session_index(&successor).unwrap();
            let mut back = inner.sessions[index].clone();
            back.session.id = returning;
            back.engram.work_binding = None;
            inner.sessions.push(back);
        }));
    }
    claimed
        .state
        .recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    let inner = claimed.state.inner.lock().unwrap();
    let history = inner
        .engram_work_naming_history
        .iter()
        .find(|history| history.store == store && history.work_id == original.work_id)
        .unwrap();
    assert!(
        !history.run_is_retired(&original),
        "a returned reporter's intent is never discarded"
    );
    assert!(
        engram_root_journal(&inner.engram_named_root_journal, &store, &original.claim_id)
            .unwrap()
            .pending
            .is_some()
    );
}

/// A run retired while its root stays named: the naming session moved on to
/// a newer claim, so its root row and the run's journal survive.
fn retired_while_named(
    label: &str,
    reason: &str,
    message: &str,
) -> (
    EOCheckedTurn,
    EngramAuthorityStoreKey,
    EngramControlWorkBinding,
) {
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(&claimed, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    let store = naming_store(&claimed);
    let original = claimed.record(|record| record.engram.work_binding.clone().unwrap());
    let mut newer = original.clone();
    newer.claim_id = format!("{label}-newer-claim");
    newer.run_id = format!("{label}-newer-run");
    newer.root_execution_id = format!("{label}-newer-execution");
    claimed.record_mut(|record| record.engram.work_binding = Some(newer.clone()));
    claimed
        .transport
        .enable_named_roots(&claimed.session_id, &newer);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        history_mut(&mut inner, &store, &original.work_id)
            .unresolved_runs
            .push(original.clone());
    }
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .root_read_errors
        .insert(original.claim_id.clone(), refusal(reason, message));
    claimed
        .state
        .recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        assert!(history_mut(&mut inner, &store, &original.work_id).run_is_retired(&original));
        assert!(
            engram_root_journal(&inner.engram_named_root_journal, &store, &original.claim_id)
                .is_some(),
            "the named root keeps its journal"
        );
    }
    (claimed, store, original)
}

#[test]
fn removing_the_naming_session_after_a_retirement_does_not_reopen_its_journal() {
    let (claimed, store, original) =
        retired_while_named("rrm", "run_unknown", "the run is unknown in this store");
    let mut inner = claimed.state.inner.lock().unwrap();
    let index = inner.find_session_index(&claimed.session_id).unwrap();
    inner.remove_session_at(index);
    assert!(
        engram_root_journal(&inner.engram_named_root_journal, &store, &original.claim_id)
            .is_none_or(|journal| !journal.requires_reconciliation()),
        "a retired run's settled journal does not reopen"
    );
    assert_ne!(
        engram_authority_work_unresolved_reason(&inner, &store, &original.work_id),
        Some(EngramUnconfirmedRootReason::WorkJournalReconciliation)
    );
}

#[test]
fn a_reopened_journal_of_a_retired_run_is_settled_by_the_next_recovery_pass() {
    for live_reporter in [false, true] {
        let label = format!("rrj-{live_reporter}");
        let (claimed, store, original) = retired_while_named(
            &label,
            "claim_not_current_for_run",
            "the claim does not belong to the run",
        );
        {
            let mut inner = claimed.state.inner.lock().unwrap();
            let reporter = claimed.session_id.clone();
            let journal = inner
                .engram_named_root_journal
                .iter_mut()
                .find(|journal| journal.store == store && journal.claim_id == original.claim_id)
                .expect("the named root keeps its journal");
            // Some other path reopens the settled journal.
            journal.retire(EngramRootRetirement::AssociationUnconfirmed);
            if live_reporter {
                let mut intent = journal.confirmed.as_ref().unwrap().0.clone();
                intent.reporter = reporter;
                journal.pending = Some(intent);
            }
            assert!(
                engram_authority_work_unresolved_reason(&inner, &store, &original.work_id)
                    .is_some()
            );
        }
        let reads = claimed.transport.requests().len();
        claimed
            .state
            .recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
        let inner = claimed.state.inner.lock().unwrap();
        assert!(
            claimed.transport.requests()[reads..]
                .iter()
                .all(|request| request.request["claim_id"] != original.claim_id.as_str()),
            "a retired run is never read again"
        );
        let journal =
            engram_root_journal(&inner.engram_named_root_journal, &store, &original.claim_id);
        if live_reporter {
            assert!(
                journal.unwrap().pending.is_some(),
                "a live reporter's intent is left alone"
            );
        } else {
            assert_eq!(
                engram_authority_work_unresolved_reason(&inner, &store, &original.work_id),
                None,
                "settled again without a read"
            );
            assert!(journal.is_none_or(|journal| !journal.requires_reconciliation()));
        }
    }
}

#[test]
fn a_retired_runs_root_is_disposed_of_and_never_planned_for_a_read() {
    for (label, reason, message, wording) in [
        (
            "rrd-u",
            "run_unknown",
            "the run is unknown in this store",
            "definitively refused its run",
        ),
        (
            "rrd-c",
            "claim_not_current_for_run",
            "the claim does not belong to the run",
            "no longer counts its claim",
        ),
    ] {
        let (claimed, store, original) = retired_while_named(label, reason, message);
        // The fixture's clock reads the state, so it is read before the guard.
        let now = claimed.state.engram_budget_clock().now();
        let root = {
            let inner = claimed.state.inner.lock().unwrap();
            let root = inner
                .engram_work_source_roots
                .iter()
                .find(|root| root.store == store && root.claim_id == original.claim_id)
                .unwrap()
                .clone();
            assert!(
                engram_named_root_claim_refused(&inner, &root),
                "{reason}: durable across a restart"
            );
            let plan = EngramRootReclamationSnapshot::capture(&inner).plan(now);
            assert!(
                plan.iter().all(|task| task.root != root),
                "{reason}: nothing to read"
            );
            assert!(
                engram_root_reclamation_plan_locked(&inner, now)
                    .iter()
                    .all(|task| task.root != root),
                "{reason}: the locked planner agrees"
            );
            assert!(
                engram_named_root_reclamation_text(&inner, &root, now)
                    .contains("definitively refused its run"),
                "{reason}: the status names the disposition"
            );
            assert!(
                EngramRootReclamationSnapshot::capture(&inner).disposition_due(now),
                "{reason}"
            );
            root
        };
        claimed.state.run_engram_named_root_disposition_pass(
            claimed.state.engram_budget_clock().now(),
            &|root| {
                panic!(
                    "a retired run's root needs no tracker read: {}",
                    root.short_ref
                )
            },
        );
        let inner = claimed.state.inner.lock().unwrap();
        assert!(
            !inner.engram_work_source_roots.contains(&root),
            "{reason}: the root is disposed of"
        );
        let audit = serde_json::to_string(&*inner.engram_named_root_release_audit).unwrap();
        assert!(audit.contains(wording), "{reason}: {audit}");
    }
}

#[test]
fn a_naming_session_kept_aside_at_restore_holds_its_run() {
    let fixture = retained("quarantined-holder");
    {
        let mut inner = fixture.claimed.state.inner.lock().unwrap();
        let mut root = inner.engram_work_source_roots[0].clone();
        root.claim_id = fixture.earlier.claim_id.clone();
        root.named_by_session = "session-kept-aside-at-restore".to_owned();
        inner.engram_work_source_roots.push(root);
        inner
            .quarantined_persisted_session_ids
            .insert("session-kept-aside-at-restore".to_owned());
    }
    engram_answers(
        &fixture,
        &fixture.earlier.claim_id,
        refusal(
            "claim_not_current_for_run",
            "the claim does not belong to the run",
        ),
    );
    maintain(&fixture);
    assert!(
        history_json(&fixture).get("retiredRuns").is_none(),
        "it may load again holding the run"
    );
}

#[test]
fn a_malformed_canonical_answer_raises_the_alarm_and_an_unknown_projection_does_not() {
    for (case, alarm) in [("undecodable", true), ("unknown-projection", false)] {
        let fixture = retained(&format!("answer-{case}"));
        let mut reply = json!({"project_id": fixture.store.project_id, "work_id": fixture.earlier.work_id,
            "root_execution_id": fixture.earlier.root_execution_id, "run_id": fixture.earlier.run_id,
            "claim_id": fixture.earlier.claim_id, "run": {"state": "claimed", "generation": 1},
            "named_root": {"state": "unknown"}, "latest_event": null,
            "read_cut": {"feed": {"kind": "run_execution", "id": fixture.earlier.run_id}, "position": 0}});
        if case == "undecodable" {
            reply.as_object_mut().unwrap().remove("run");
        }
        fixture
            .claimed
            .transport
            .named_roots
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .root_reads
            .insert(fixture.earlier.claim_id.clone(), reply);
        maintain(&fixture);
        assert_eq!(
            unresolved(&fixture),
            Some(EngramUnconfirmedRootReason::WorkRunsUnresolved),
            "{case}"
        );
        let history = history_json(&fixture);
        assert!(history.get("retiredRuns").is_none(), "{case}");
        assert_eq!(
            history["recoveryReason"]
                .as_str()
                .unwrap()
                .starts_with("store integrity alarm: "),
            alarm,
            "{case}: {history:#}"
        );
    }
}

#[test]
fn a_refusal_of_a_run_a_session_still_holds_is_not_retired() {
    let fixture = retained("refused-held-run");
    // Another session still holds the refused binding as its work.
    {
        let mut inner = fixture.claimed.state.inner.lock().unwrap();
        let index = inner
            .find_session_index(&fixture.claimed.session_id)
            .unwrap();
        let mut holder = inner.sessions[index].clone();
        holder.session.id = "session-still-holding-the-earlier-run".to_owned();
        holder.session.project_id = None;
        holder.engram.work_binding = Some(fixture.earlier.clone());
        inner.sessions.push(holder);
    }
    engram_answers(
        &fixture,
        &fixture.earlier.claim_id,
        refusal(
            "claim_not_current_for_run",
            "the claim does not belong to the run",
        ),
    );
    maintain(&fixture);
    assert_eq!(
        unresolved(&fixture),
        Some(EngramUnconfirmedRootReason::WorkRunsUnresolved),
        "a held binding stays withheld instead of being credited"
    );
    assert!(history_json(&fixture).get("retiredRuns").is_none());
}

#[path = "engram_root_retained_run_reads.rs"]
mod reads;
