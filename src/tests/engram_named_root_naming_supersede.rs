// Owns the regressions for a naming request whose captured authority image
// another owner supersedes while the caller's claim and requested binding
// stay unchanged: the call completes within its own budget and acknowledges
// the current binding, with no caller retry and no spurious generation. It
// also owns a kept re-name clearing the holder's own stale journal flags.
// The original post-read fail-first witness is retained in Git history;
// this module also covers the bounded recovery and its rejection guards.
// Includes the later canonical-read, selection and kept-name windows and
// their retry limit, scripted deadline and changed-binding controls.
// Does not own the pre-send acknowledgement retry bounds
// (engram_named_root_naming_supersede_bounds.rs),
// the evaluation side (acceptance_evaluation_unchanged_binding.rs) or the
// authority fence's own phases (engram_authority_fence_phases.rs).
// New module.
use super::*;
use super::reclamation::{root_fixture, request_fixture_name};

mod rejections {
    use super::*;
    include!("engram_named_root_naming_supersede_rejections.rs");
}

mod renewed {
    use super::*;
    include!("engram_named_root_naming_supersede_renewed.rs");
}

/// Another owner (such as the automatic reclamation pass) replaces this
/// work's authority image once, at the naming's owner check.
fn supersede_once_at_owner_check(claimed: &EOCheckedTurn) -> Arc<std::sync::atomic::AtomicBool> {
    let superseded = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hook_superseded = superseded.clone();
    let state = claimed.state.clone();
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary =
        Some(Arc::new(move |phase| {
            if phase == "before_owner_check"
                && !hook_superseded.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                let mut inner = state.inner.lock().unwrap();
                if let Some(transition) = inner
                    .engram_work_naming_history
                    .iter_mut()
                    .find_map(|history| history.transition.as_mut())
                {
                    transition.id.push_str("-superseded-by-another-owner");
                }
            }
        }));
    superseded
}

#[test]
fn a_kept_re_name_clears_the_holders_own_stale_journal_flags() {
    // The holder's OWN claim journal was left Displaced or in reconciliation.
    // A same-path, same-generation re-name is "kept": it re-reads Engram's
    // Bound state for this very claim but sends no event, so without a reset
    // every evaluation under the binding would stay refused.
    for stale in ["displaced", "reconciliation"] {
        let label = format!("naming-kept-clears-{stale}");
        let (claimed, tree) = root_fixture(&label);
        let first = request_fixture_name(&claimed, Some(&tree))
            .expect("the first name");
        {
            let mut inner = claimed.state.inner.lock().unwrap();
            let entry = inner.engram_work_source_roots[0].clone();
            let journal = inner
                .engram_named_root_journal
                .iter_mut()
                .find(|journal| journal.store == entry.store && journal.claim_id == entry.claim_id)
                .expect("the holder's journal");
            if stale == "displaced" {
                journal.retire(EngramRootRetirement::Displaced);
            } else {
                journal.reconciliation = Some(EngramRootReconciliation {
                    intent: EngramNamedRootEvent {
                        root: entry.clone(),
                        reporter: entry.named_by_session.clone(),
                        kind: EngramNamedRootKind::Bound,
                        end_reason: None,
                    },
                    state: EngramNamedRootState::Unknown,
                    observed_by: entry.named_by_session.clone(),
                    observed_at: "2026-10-19T15:00:00Z".to_owned(),
                    settled: false,
                });
            }
        }

        let again = request_fixture_name(&claimed, Some(&tree))
            .expect("the same name again");
        assert_eq!(again.generation, first.generation, "{stale}: the re-name is kept");

        let inner = claimed.state.inner.lock().unwrap();
        let entry = &inner.engram_work_source_roots[0];
        let journal = engram_root_journal(&inner.engram_named_root_journal, &entry.store, &entry.claim_id)
            .expect("the holder's journal");
        assert!(
            !journal.obsolete
                && journal.retirement.is_none()
                && !journal.requires_reconciliation(),
            "{stale}: a kept re-name confirmed this claim's binding, so its own stale flags \
             are cleared and evaluations under it are no longer refused: {journal:#?}"
        );
    }
}

#[test]
fn an_unsuperseded_naming_completes() {
    // Control: the fixture names successfully when nothing supersedes it.
    let label = "naming-unsuperseded";
    let (claimed, tree) = root_fixture(label);
    let named = request_fixture_name(&claimed, Some(&tree))
        .expect("an undisturbed naming completes");
    assert!(named.generation > 0, "{named:?}");
}

#[test]
fn a_naming_superseded_mid_acknowledgement_completes_under_its_unchanged_binding() {
    let label = "naming-superseded-image";
    let (claimed, tree) = root_fixture(label);
    let superseded = supersede_once_at_owner_check(&claimed);

    let named = request_fixture_name(&claimed, Some(&tree));
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = None;

    assert!(
        superseded.load(std::sync::atomic::Ordering::SeqCst),
        "the authority image was superseded during the naming"
    );
    let named = named.unwrap_or_else(|error| {
        panic!(
            "a naming whose claim and binding are unchanged completes within its budget, \
             without a caller retry: {}",
            error.message
        )
    });
    let inner = claimed.state.inner.lock().unwrap();
    let entries = inner
        .engram_work_source_roots
        .iter()
        .filter(|entry| entry.work_id == test_control_work_binding(&format!("eo-check-{label}"), 1).work_id)
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 1, "exactly one effective naming: {entries:#?}");
    assert_eq!(entries[0].generation, named.generation, "no spurious generation change");
    let journal = engram_root_journal(&inner.engram_named_root_journal, &entries[0].store, &entries[0].claim_id)
        .expect("the claim's journal");
    assert!(journal.pending.is_none(), "the naming settled: {journal:#?}");
}

// The existing hook runs after a canonical reply has decoded, before the
// owner learns it. Skip preparatory reads; supersede only once an actual
// naming event is pending, using the production prepare owner with the same
// binding. No claim, runtime, connection or producer reply is changed.
fn supersede_after_event_read(
    state: AppState,
    store: EngramAuthorityStoreKey,
    binding: EngramControlWorkBinding,
    witness: Arc<Mutex<Option<Value>>>,
) {
    TEST_ENGRAM_DURING_ROOT_LIFECYCLE_READ.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let pending_owner = {
                let inner = state.inner.lock().unwrap();
                engram_root_journal(&inner.engram_named_root_journal, &store, &binding.claim_id)
                    .and_then(|journal| journal.pending.as_ref())
                    .map(|event| {
                        let owner = inner.engram_work_naming_history.iter()
                            .find(|history| history.store == store && history.work_id == binding.work_id)
                            .unwrap().transition.clone().unwrap();
                        (event.clone(), owner)
                    })
            };
            let Some((event, before)) = pending_owner else {
                supersede_after_event_read(state, store, binding, witness);
                return;
            };
            let after = state.prepare_engram_authority(&store, &binding, DEADLOCK_GUARD).unwrap();
            assert_eq!(before.binding, after.binding, "the competing owner keeps the exact binding");
            assert_ne!(before.id, after.id);
            assert!(after.version > before.version);
            *witness.lock().unwrap() = Some(json!({
                "event": event.request().unwrap(),
                "before": {"id":before.id,"version":before.version},
                "after": {"id":after.id,"version":after.version},
            }));
        }));
    });
}

#[test]
fn a_naming_superseded_post_read_completes_with_its_live_binding() {
    naming_post_read_supersession(false);
}

#[test]
fn a_rename_superseded_post_read_completes_with_its_live_binding() {
    naming_post_read_supersession(true);
}

fn naming_post_read_supersession(rename: bool) {
    naming_owner_supersession(rename, false);
}

#[test]
fn a_naming_superseded_before_selection_completes_with_its_live_binding() {
    naming_owner_supersession(false, true);
}

#[test]
fn a_rename_superseded_before_selection_completes_with_its_live_binding() {
    naming_owner_supersession(true, true);
}

fn supersede_at_naming_phase(
    state: &AppState,
    store: EngramAuthorityStoreKey,
    binding: EngramControlWorkBinding,
    phase: &'static str,
    witness: Arc<Mutex<Option<Value>>>,
) {
    let changed = std::sync::atomic::AtomicBool::new(false);
    let competing = state.clone();
    state.inner.lock().unwrap().test_engram_authority_ack_boundary = Some(Arc::new(move |observed| {
        if observed != phase || changed.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let (before, event) = {
            let inner = competing.inner.lock().unwrap();
            let before = inner.engram_work_naming_history.iter()
                .find(|history| history.store == store && history.work_id == binding.work_id)
                .unwrap().transition.clone().unwrap();
            let event = engram_root_journal(&inner.engram_named_root_journal, &store, &binding.claim_id)
                .and_then(|journal| journal.pending.as_ref()).map(|event| event.request().unwrap());
            (before, event)
        };
        let after = competing.prepare_engram_authority(&store, &binding, DEADLOCK_GUARD).unwrap();
        assert_eq!(before.binding, after.binding);
        assert_ne!(before.id, after.id);
        assert!(after.version > before.version);
        *witness.lock().unwrap() = Some(json!({"event":event,
            "before":{"id":before.id,"version":before.version},
            "after":{"id":after.id,"version":after.version}}));
    }));
}

fn naming_owner_supersession(rename: bool, before_selection: bool) {
    let label = if rename { "rename-post-read-owner" } else { "name-post-read-owner" };
    let (claimed, first) = root_fixture(label);
    let path = if rename {
        request_fixture_name(&claimed, Some(&first)).unwrap();
        let second = claimed.root.join(".worktrees").join("second");
        run_git_test_command(&claimed.root, &[
            "worktree", "add", "--quiet", "-b", "second", second.to_str().unwrap(),
        ]);
        second
    } else {
        first
    };
    let store = naming_store(&claimed);
    let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
    let witness = Arc::new(Mutex::new(None));
    if before_selection {
        supersede_at_naming_phase(&claimed.state, store, binding.clone(),
            "before_naming_selection", witness.clone());
    } else {
        supersede_after_event_read(claimed.state.clone(), store, binding.clone(), witness.clone());
    }
    let result = request_fixture_name(&claimed, Some(&path));
    // A failure must not retain a thread-local closure holding the fixture.
    TEST_ENGRAM_DURING_ROOT_LIFECYCLE_READ.with(|hook| hook.borrow_mut().take());
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = None;
    let witness = witness.lock().unwrap().take().expect("the post-event read was superseded");
    assert_eq!(claimed.record(|record| record.engram.work_binding.clone()), Some(binding));
    let sends = claimed.transport.requests().into_iter()
        .filter(|request| request.request["operation"] == "named_root_bind")
        .map(|request| request.request).collect::<Vec<_>>();
    assert_eq!(sends.last(), Some(&witness["event"]), "the raced proof follows the actual send");
    assert_eq!(sends.len(), if rename { 2 } else { 1 }, "owner recovery re-reads without another producer transition");
    let named = result.unwrap_or_else(|error| panic!(
        "same-binding post-read supersession must complete for the live holder: status={}, error={}",
        error.status, error.message,
    ));
    let inner = claimed.state.inner.lock().unwrap();
    assert_eq!(inner.engram_work_source_roots.len(), 1);
    assert_eq!(inner.engram_work_source_roots[0].generation, named.generation);
    assert!(inner.engram_named_root_journal.iter().all(|journal| journal.pending.is_none()));
    assert_eq!(named.generation, witness["event"]["generation"].as_u64().unwrap());
}

#[test]
fn a_kept_naming_superseded_after_selection_persistence_completes_without_another_event() {
    let (claimed, tree) = root_fixture("kept-post-selection-owner");
    let first = request_fixture_name(&claimed, Some(&tree)).unwrap();
    let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
    let witness = Arc::new(Mutex::new(None));
    supersede_at_naming_phase(&claimed.state, naming_store(&claimed), binding.clone(),
        "before_kept_naming_proof", witness.clone());
    let result = request_fixture_name(&claimed, Some(&tree));
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = None;
    assert!(witness.lock().unwrap().is_some(), "the persisted kept selection reached its later proof");
    let named = result.unwrap_or_else(|error| panic!("kept selection recovery: {}", error.message));
    assert_eq!(named.generation, first.generation);
    assert_eq!(claimed.record(|record| record.engram.work_binding.clone()), Some(binding));
    assert_eq!(claimed.transport.requests().iter()
        .filter(|request| request.request["operation"] == "named_root_bind").count(), 1);
    let inner = claimed.state.inner.lock().unwrap();
    assert_eq!(inner.engram_work_source_roots.len(), 1);
    assert!(inner.engram_named_root_journal.iter().all(|journal| journal.pending.is_none()));
}

fn supersede_pending_reads(
    state: AppState,
    store: EngramAuthorityStoreKey,
    binding: EngramControlWorkBinding,
    count: Arc<std::sync::atomic::AtomicUsize>,
    fault: &'static str,
) {
    TEST_ENGRAM_DURING_ROOT_LIFECYCLE_READ.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let pending = {
                let inner = state.inner.lock().unwrap();
                engram_root_journal(&inner.engram_named_root_journal, &store, &binding.claim_id)
                    .is_some_and(|journal| journal.pending.is_some())
            };
            if !pending {
                supersede_pending_reads(state, store, binding, count, fault);
                return;
            }
            state.prepare_engram_authority(&store, &binding, DEADLOCK_GUARD).unwrap();
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if fault == "deadline" {
                state.engram_budget_clock().advance(ENGRAM_SOURCE_ROOT_NAMING_BUDGET);
            } else if fault == "binding" {
                let mut inner = state.inner.lock().unwrap();
                let record = inner.sessions.iter_mut()
                    .find(|record| record.engram.work_binding.as_ref() == Some(&binding))
                    .expect("the fixture's exact live holder");
                record.engram.work_binding.as_mut().unwrap().claim_fence += 1;
            } else {
                supersede_pending_reads(state, store, binding, count, fault);
            }
        }));
    });
}

#[test]
fn post_read_naming_owner_recovery_keeps_its_limit_deadline_and_binding_fence() {
    for fault in ["repeated", "deadline", "binding"] {
        let (claimed, tree) = root_fixture(&format!("post-read-bound-{fault}"));
        let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
        let store = naming_store(&claimed);
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        supersede_pending_reads(claimed.state.clone(), store.clone(), binding.clone(), count.clone(), fault);
        let result = request_fixture_name(&claimed, Some(&tree));
        TEST_ENGRAM_DURING_ROOT_LIFECYCLE_READ.with(|hook| hook.borrow_mut().take());
        let error = result.expect_err("bounded recovery cannot complete under this fault");
        assert_eq!(error.status, StatusCode::CONFLICT,
            "{fault}: {}", error.message);
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst),
            if fault == "repeated" { 1 + ENGRAM_NAMING_SUPERSEDED_RETRIES } else { 1 },
            "{fault}: only unchanged authority within the original budget can be retried");
        assert!(!error.message.contains("nothing was named"), "{fault}: {}", error.message);
        assert_eq!(claimed.transport.requests().iter()
            .filter(|request| request.request["operation"] == "named_root_bind").count(), 1);
        let inner = claimed.state.inner.lock().unwrap();
        assert!(inner.engram_work_source_roots.is_empty());
        let journal = engram_root_journal(&inner.engram_named_root_journal, &store, &binding.claim_id).unwrap();
        assert_eq!(journal.read_binding.as_ref(), Some(&binding));
        assert_eq!(journal.pending.as_ref().unwrap().root.claim_fence, binding.claim_fence);
        assert!(journal.confirmed.is_none());
    }
}
