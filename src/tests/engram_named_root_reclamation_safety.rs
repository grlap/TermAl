// Preserves producer ownership, exact own-clear fences and filesystem safety.
// The evaluated-but-not-done witness uses the real producer in the live fixture.
use super::*;

#[test]
fn named_root_reclamation_live_worktree_and_retired_worktree_contents_are_untouched() {
    for proof in ["live", "unknown", "completed"] {
        let (claimed, entry) = settled_root("reclaim-disk-safety", proof);
        let root = PathBuf::from(&entry.root);
        let marker = root.join("operator-content.txt");
        fs::write(&marker, b"preserve operator content exactly\n").unwrap();
        let git_entry = fs::read(root.join(".git")).unwrap();
        let content = fs::read(&marker).unwrap();
        claimed.state.run_engram_root_reclamation_pass(reclamation_plan(&claimed));
        assert!(root.is_dir());
        assert_eq!(fs::read(root.join(".git")).unwrap(), git_entry);
        assert_eq!(fs::read(&marker).unwrap(), content);
        if proof != "completed" { assert_root_retained(&claimed, &entry); }
        else { assert!(!claimed.state.inner.lock().unwrap().engram_work_source_roots.contains(&entry)); }
    }
}

#[test]
fn named_root_reclamation_pending_bound_ended_and_root_invalid_remain_with_the_owner() {
    for kind in ["bound", "ended", "root-invalid"] {
        let (claimed, entry) = settled_root("reclaim-pending-owner", "completed");
        let original = {
            let mut inner = claimed.state.inner.lock().unwrap();
            let journal = &mut inner.engram_named_root_journal[0];
            let mut intent = journal.confirmed.as_ref().unwrap().0.clone();
            if kind != "bound" {
                intent.kind = EngramNamedRootKind::Ended;
                intent.end_reason = Some(if kind == "root-invalid" {
                    EngramNamedRootEndReason::RootInvalid
                } else { EngramNamedRootEndReason::ExplicitClear });
            }
            journal.pending = Some(intent);
            journal.clone()
        };
        let requests = claimed.transport.requests().len();
        assert!(reclamation_plan(&claimed).is_empty());
        let error = claimed.state.recover_one_engram_root_until(&entry,
            &EngramRootReclamationRoute::Unfocused,
            claimed.state.engram_budget_clock().now() + ENGRAM_ROOT_RECLAMATION_BUDGET).unwrap_err();
        assert!(error.message.contains("pending producer intent"));
        assert_root_retained(&claimed, &entry);
        assert_eq!(claimed.state.inner.lock().unwrap().engram_named_root_journal[0], original);
        assert_eq!(claimed.transport.requests().len(), requests);
    }
}

#[test]
fn named_root_reclamation_current_terminal_own_clear_is_synchronous_and_event_free() {
    let (claimed, entry) = settled_root("reclaim-current-terminal-clear", "completed");
    let binding = claimed.state.inner.lock().unwrap().engram_named_root_journal[0]
        .read_binding.clone().unwrap();
    claimed.record_mut(|record| record.engram.work_binding = Some(binding.clone()));
    claimed.transport.enable_named_roots(&claimed.session_id, &binding);
    let before = claimed.transport.requests().iter()
        .filter(|request| request.request["operation"] == "named_root_bind").count();
    let cleared = claimed.state.name_engram_source_root(&claimed.session_id,
        EngramSourceRootRequest { work: entry.short_ref.clone(), path: None }).unwrap();
    assert!(cleared.root.is_none());
    assert_eq!(engram_named_root_capacity(&claimed.state.inner.lock().unwrap()), 0);
    assert_eq!(claimed.transport.requests().iter()
        .filter(|request| request.request["operation"] == "named_root_bind").count(), before);
}

#[test]
fn named_root_reclamation_own_clear_cannot_remove_a_foreign_replacement_after_held_read() {
    let (claimed, entry) = settled_root("reclaim-clear-replacement", "completed");
    let state = claimed.state.clone();
    TEST_ENGRAM_AFTER_SOURCE_ROOT_HELD_READ.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let mut inner = state.inner.lock().unwrap();
            inner.engram_work_source_roots[0].named_by_session = "a-foreign-namer".to_owned();
            inner.engram_work_source_roots[0].generation += 1;
        }));
    });
    let error = claimed.state.name_engram_source_root(&claimed.session_id,
        EngramSourceRootRequest { work: entry.short_ref, path: None }).unwrap_err();
    assert!(error.message.contains("replaced"));
    let inner = claimed.state.inner.lock().unwrap();
    assert_eq!(inner.engram_work_source_roots[0].named_by_session, "a-foreign-namer");
    assert_eq!(inner.engram_work_source_roots[0].generation, entry.generation + 1);
    assert!(inner.engram_work_naming_history[0].retirements.is_empty());
}

#[test]
fn named_root_reclamation_missing_directory_keeps_exact_root_invalid_cleanup_recoverable() {
    let label = "reclaim-root-invalid-owner";
    let (mut claimed, worktree) = root_fixture(label);
    request_fixture_name(&claimed, Some(&worktree)).unwrap();
    let original_receipt = claimed.state.inner.lock().unwrap().engram_named_root_journal[0].confirmed.clone();
    // Simulate a missing Git worktree without deleting any fixture data.
    fs::rename(worktree.join(".git"), worktree.join("saved-git-entry")).unwrap();
    let before = claimed.transport.requests().len();
    let path = claimed.state.persistence_path.clone();
    let failure = claimed.root.join("blocked-cleanup-image");
    fs::create_dir_all(&failure).unwrap();
    claimed.state.persistence_path = Arc::new(failure);
    assert!(engram_named_root_absent(worktree.to_str().unwrap()));
    {
        // The opening-time grant probe was deleted. Exercise the retained
        // cleanup owner with the exact confirmed root-invalid intent.
        let mut inner = claimed.state.inner.lock().unwrap();
        let root = inner.engram_work_source_roots[0].clone();
        engram_queue_root_cleanup(&mut inner.engram_named_root_journal,
            &root, EngramNamedRootEndReason::RootInvalid);
    }
    claimed.state.flush_engram_root_cleanup(&claimed.session_id, Duration::from_secs(2));
    let pending = claimed.state.inner.lock().unwrap().engram_named_root_journal[0].pending.clone().unwrap();
    assert_eq!(pending.kind, EngramNamedRootKind::Ended);
    assert_eq!(pending.end_reason, Some(EngramNamedRootEndReason::RootInvalid));
    assert!(reclamation_plan(&claimed).is_empty());
    assert_eq!(claimed.transport.requests().len(), before, "failed durability cannot send the cleanup event");
    {
        let inner = claimed.state.inner.lock().unwrap();
        assert_eq!(inner.engram_named_root_journal[0].pending.as_ref(), Some(&pending));
        assert_eq!(inner.engram_named_root_journal[0].confirmed, original_receipt);
        assert!(inner.engram_work_naming_history[0].retirements.is_empty());
    }
    claimed.state.persistence_path = path;
    claimed.state.flush_engram_root_cleanup(&claimed.session_id, Duration::from_secs(2));
    let inner = claimed.state.inner.lock().unwrap();
    assert!(inner.engram_named_root_journal[0].pending.is_none());
    let confirmed = &inner.engram_named_root_journal[0].confirmed.as_ref().unwrap().0;
    assert_eq!(confirmed.root, pending.root);
    assert_eq!(confirmed.end_reason, Some(EngramNamedRootEndReason::RootInvalid));
    assert!(inner.engram_work_naming_history[0].retirements.is_empty(), "only the existing producer owner settled cleanup");
}
