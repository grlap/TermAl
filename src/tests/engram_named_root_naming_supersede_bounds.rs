// Owns the bounds of a naming whose captured authority image another owner
// supersedes. One superseded at every attempt refuses after its bounded
// retries and says the identical request can be retried. One whose claim or
// work binding changed while it was superseded refuses at once, without a
// retry, and says retrying the same request cannot help: at the claim
// lifecycle guard (the session's binding moved or was released) and at the
// event send (the claim's run association moved).
// Does not own the naming that completes under its unchanged binding
// (engram_named_root_naming_supersede.rs), the evaluation side
// (acceptance_evaluation_unchanged_binding.rs) or the authority fence's own
// phases (engram_authority_fence_phases.rs).
// Split out of engram_named_root_naming_supersede.rs, which keeps only tests
// that compile without the bounded retry, so that file alone is the naming
// side's test-only fail-first witness.
use super::*;

/// Another owner (such as the automatic reclamation pass) replaces this
/// work's authority image.
fn supersede(inner: &mut StateInner) {
    if let Some(transition) = inner
        .engram_work_naming_history
        .iter_mut()
        .find_map(|history| history.transition.as_mut())
    {
        transition.id.push_str("-superseded-by-another-owner");
    }
}

fn named_any(claimed: &ClaimedRoot, label: &str) -> bool {
    claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_work_source_roots
        .iter()
        .any(|entry| entry.short_ref == format!("w-{label}"))
}

/// Another owner replaces this work's authority image at EVERY owner check;
/// returns how many owner checks ran.
fn supersede_every_owner_check(claimed: &ClaimedRoot) -> Arc<std::sync::atomic::AtomicUsize> {
    let checks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hook_checks = checks.clone();
    let state = claimed.state.clone();
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary =
        Some(Arc::new(move |phase| {
            if phase == "before_owner_check" {
                hook_checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                supersede(&mut state.inner.lock().unwrap());
            }
        }));
    checks
}

#[test]
fn a_naming_superseded_at_every_attempt_refuses_after_its_bounded_retries() {
    let label = "naming-always-superseded";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let tree = add_claimed_root_worktree(&claimed.root);
    let checks = supersede_every_owner_check(&claimed);

    let named = name_root(&claimed, label, Some(&tree), vec![claimed_root_held(label)]);
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = None;

    let error = named.expect_err("a naming superseded at every attempt does not complete");
    let attempts = 1 + ENGRAM_NAMING_SUPERSEDED_RETRIES;
    assert_eq!(
        checks.load(std::sync::atomic::Ordering::SeqCst),
        attempts,
        "one attempt plus the bounded retries, and no more"
    );
    // The refusal keeps its cause and says what happened and that the
    // identical request can help, since the claim and binding never moved.
    assert!(
        error.message.contains("superseded authority image")
            && error.message.contains(&format!("each of {attempts} attempts"))
            && error.message.contains("nothing was named")
            && error.message.contains("can be retried")
            && !error.message.contains("cannot help"),
        "{}",
        error.message
    );
    assert!(!named_any(&claimed, label), "nothing was named");
}

/// While the naming's claim-lifecycle guard is acknowledged, another owner
/// supersedes its image and `change` alters the session's work binding.
/// Returns how many owner checks ran.
fn supersede_at_the_guard_while(
    claimed: &ClaimedRoot,
    change: fn(&mut Option<EngramControlWorkBinding>),
) -> Arc<std::sync::atomic::AtomicUsize> {
    let checks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hook_checks = checks.clone();
    let state = claimed.state.clone();
    let session_id = claimed.session_id.clone();
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary =
        Some(Arc::new(move |phase| {
            if phase == "before_owner_check" {
                hook_checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut inner = state.inner.lock().unwrap();
                supersede(&mut inner);
                let index = inner.find_session_index(&session_id).unwrap();
                change(&mut inner.sessions[index].engram.work_binding);
            }
        }));
    checks
}

#[test]
fn a_naming_whose_claim_changed_while_superseded_refuses_at_once_and_says_retrying_cannot_help() {
    let cases: [(&str, fn(&mut Option<EngramControlWorkBinding>)); 2] = [
        ("claim-changed", |binding| {
            let binding = binding.as_mut().expect("the session is bound");
            binding.claim_id.push_str("-recovered");
            binding.claim_fence += 1;
        }),
        ("claim-released", |binding| *binding = None),
    ];
    for (case, change) in cases {
        let label = format!("naming-superseded-{case}");
        let claimed = ClaimedRoot::new_scripted(&label, Vec::new());
        let tree = add_claimed_root_worktree(&claimed.root);
        let checks = supersede_at_the_guard_while(&claimed, change);

        let named = name_root(&claimed, &label, Some(&tree), vec![claimed_root_held(&label)]);
        claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = None;

        let error = named.expect_err("a naming whose claim moved does not complete");
        assert_eq!(
            checks.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "{case}: refused at its first supersession, with no retry"
        );
        assert!(
            error.message.contains("superseded authority image")
                && error.message.contains("claim or work binding changed")
                && error.message.contains("cannot help")
                && error.message.contains("nothing was named")
                && !error.message.contains("can be retried"),
            "{case}: {}",
            error.message
        );
        assert!(!named_any(&claimed, &label), "{case}: nothing was named");
    }
}

#[test]
fn a_naming_whose_claim_run_changed_while_its_event_was_superseded_refuses_at_once() {
    // The guard passes undisturbed; once the naming's intent is pending and
    // its event's image is being acknowledged, another owner supersedes it
    // and the claim's run association moves (as a recovery would move it).
    let label = "naming-event-superseded-run-changed";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let tree = add_claimed_root_worktree(&claimed.root);
    let event_checks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hook_checks = event_checks.clone();
    let state = claimed.state.clone();
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary =
        Some(Arc::new(move |phase| {
            if phase != "before_owner_check" {
                return;
            }
            let mut inner = state.inner.lock().unwrap();
            let Some(index) = inner
                .engram_named_root_journal
                .iter()
                .position(|journal| journal.pending.is_some())
            else {
                return;
            };
            hook_checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            supersede(&mut inner);
            if let Some(binding) = inner.engram_named_root_journal[index].read_binding.as_mut() {
                binding.claim_fence += 1;
            }
        }));

    let named = name_root(&claimed, label, Some(&tree), vec![claimed_root_held(label)]);
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = None;

    let error = named.expect_err("a naming whose run association moved does not complete");
    assert_eq!(
        event_checks.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the event's image was refused at its first supersession, with no retry"
    );
    assert!(
        error.message.contains("superseded authority image")
            && error.message.contains("cannot help")
            && !error.message.contains("can be retried"),
        "{}",
        error.message
    );
    assert!(!named_any(&claimed, label), "nothing was named");
}
