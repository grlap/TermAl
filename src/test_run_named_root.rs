// The work binding an ordinary committed-input run's registration
// (`"input": "commit"` in its request) is pinned to, taken from the named
// source root the run's worktree is, not from the launching session's current
// binding (which under turn-gated control OFF is stale or absent); other runs
// keep the session's binding (src/test_run_registration.rs). Owns the
// request's input-kind test, the run worktree's comparison keys, the choice
// of the one named root that the launching session named for that worktree
// in the registration's authority store, the full claim binding read from
// that work's canonical naming proofs, and the pinned root
// (`TestRunNamedRoot`) the registration records.
// Does not own the registration itself (src/test_run_registration.rs), the
// named-root entries and their lifecycle (src/engram_source_roots.rs), the
// naming history (src/engram_root_authority.rs), or the binding of commands a
// provider reports (`TestRunCommandOrigin::capture` in
// src/test_run_observations.rs), which knows no worktree to compare. New
// module, split beside src/test_run_registration.rs to keep it from growing.

/// The named root a registered run was pinned to: its `workspace_id` (the
/// entry's canonical root) and the name's generation. The state is implied:
/// the root was named (bound) when the run registered.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct TestRunNamedRoot {
    workspace_id: String,
    generation: u64,
}

/// A run worktree's keys, as named-root entries hold theirs: the exact key
/// of its canonical path and of its own common Git directory.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TestRunWorktreeKeys {
    root: String,
    common_dir: String,
}

/// Whether the launcher's request names an ordinary committed-input run
/// (`"input": "commit"`), the only kind whose registration is bound from a
/// named root; every other run keeps the launching session's binding.
fn test_run_request_is_committed_input(request: &Value) -> bool {
    request.get("input").and_then(Value::as_str) == Some("commit")
}

/// The keys of `worktree`, read off the state lock; none when it no longer
/// resolves or is not a worktree root with its own `.git` entry.
fn test_run_worktree_keys(worktree: &FsPath) -> Option<TestRunWorktreeKeys> {
    let canonical = fs::canonicalize(worktree).ok()?;
    let common = engram_source_root_common_dir(&canonical)?;
    Some(TestRunWorktreeKeys {
        root: engram_exact_path_key(&canonical),
        common_dir: engram_exact_path_key(&common),
    })
}

/// The full binding of the claim that named `entry`, from its work's
/// canonical naming proofs: the claim's only proven recovery run or, when the
/// claim has proofs of several, the only one whose read binds this very root
/// and generation. None when no proof, or no single one, says so.
fn engram_named_root_claim_binding(
    histories: &[EngramWorkNamingHistory],
    entry: &EngramWorkSourceRoot,
) -> Option<EngramControlWorkBinding> {
    let proofs = engram_work_naming_proofs(histories, &entry.store, &entry.work_id)
        .filter(|proof| {
            proof.binding.work_id == entry.work_id && proof.binding.claim_id == entry.claim_id
        })
        .collect::<Vec<_>>();
    fn only(proofs: &[&EngramAuthorityProof]) -> Option<EngramControlWorkBinding> {
        match proofs {
            [proof] => Some(proof.binding.clone()),
            _ => None,
        }
    }
    only(proofs.as_slice()).or_else(|| {
        let binding_this_root = proofs
            .iter()
            .copied()
            .filter(|proof| {
                matches!(&proof.read.named_root,
                    EngramNamedRootState::Bound { workspace_id, generation, .. }
                    if workspace_id == &entry.root && *generation == entry.generation as i64)
            })
            .collect::<Vec<_>>();
        only(binding_this_root.as_slice())
    })
}

/// The binding and pinned root of a run that `session_id` launched in the
/// worktree with `worktree` keys, under the registration's authority `store`:
/// those of the one named root that session named for that worktree in that
/// store. With no store, no keys, no such entry, more than one, or no proven
/// binding for its claim, the run stays unbound; the session's own binding,
/// and which session or work has focus, never decide.
fn test_run_registration_named_binding(
    inner: &StateInner,
    session_id: &str,
    store: Option<&EngramAuthorityStoreKey>,
    worktree: Option<&TestRunWorktreeKeys>,
) -> (Option<EngramControlWorkBinding>, Option<TestRunNamedRoot>) {
    let (Some(store), Some(worktree)) = (store, worktree) else {
        return (None, None);
    };
    let mut matching = inner.engram_work_source_roots.iter().filter(|entry| {
        &entry.store == store
            && entry.named_by_session == session_id
            && entry.common_dir_key == worktree.common_dir
            && engram_exact_path_key(FsPath::new(&entry.root)) == worktree.root
    });
    let (Some(entry), None) = (matching.next(), matching.next()) else {
        return (None, None);
    };
    match engram_named_root_claim_binding(&inner.engram_work_naming_history, entry) {
        Some(binding) => (
            Some(binding),
            Some(TestRunNamedRoot {
                workspace_id: entry.root.clone(),
                generation: entry.generation,
            }),
        ),
        None => (None, None),
    }
}
