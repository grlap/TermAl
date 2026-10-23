// Owns the regressions for a naming request whose captured authority image
// another owner supersedes while the caller's claim and requested binding
// stay unchanged: the call completes within its own budget and acknowledges
// the current binding, with no caller retry and no spurious generation. It
// also owns a kept re-name clearing the holder's own stale journal flags.
// Uses only interfaces the pre-fix tree has, so this file alone is the
// naming side's test-only fail-first witness.
// Does not own the bounds of the retry (a naming superseded at every attempt,
// or whose claim changed meanwhile: engram_named_root_naming_supersede_bounds.rs),
// the evaluation side (acceptance_evaluation_unchanged_binding.rs) or the
// authority fence's own phases (engram_authority_fence_phases.rs).
// New module.
use super::*;

/// Another owner (such as the automatic reclamation pass) replaces this
/// work's authority image once, at the naming's owner check.
fn supersede_once_at_owner_check(claimed: &ClaimedRoot) -> Arc<std::sync::atomic::AtomicBool> {
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
        let claimed = ClaimedRoot::new_scripted(&label, Vec::new());
        let tree = add_claimed_root_worktree(&claimed.root);
        let first = name_root(&claimed, &label, Some(&tree), vec![claimed_root_held(&label)])
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

        let again = name_root(&claimed, &label, Some(&tree), vec![claimed_root_held(&label)])
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
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let tree = add_claimed_root_worktree(&claimed.root);
    let named = name_root(&claimed, label, Some(&tree), vec![claimed_root_held(label)])
        .expect("an undisturbed naming completes");
    assert!(named.generation > 0, "{named:?}");
}

#[test]
fn a_naming_superseded_mid_acknowledgement_completes_under_its_unchanged_binding() {
    let label = "naming-superseded-image";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let tree = add_claimed_root_worktree(&claimed.root);
    let superseded = supersede_once_at_owner_check(&claimed);

    let named = name_root(&claimed, label, Some(&tree), vec![claimed_root_held(label)]);
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
        .filter(|entry| entry.short_ref == format!("w-{label}"))
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 1, "exactly one effective naming: {entries:#?}");
    assert_eq!(entries[0].generation, named.generation, "no spurious generation change");
    let journal = engram_root_journal(&inner.engram_named_root_journal, &entries[0].store, &entries[0].claim_id)
        .expect("the claim's journal");
    assert!(journal.pending.is_none(), "the naming settled: {journal:#?}");
}
