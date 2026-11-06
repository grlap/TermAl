// The work binding an ordinary committed-input run's registration
// (`"input": "commit"` in its request) is pinned to, taken from the named
// source root the run's worktree is, not from the launching session's current
// binding (which under turn-gated control OFF is stale or absent); other runs
// keep the session's binding (src/test_run_registration.rs). Owns the
// request's input-kind test, the run worktree's comparison keys, the choice
// of the one named root that the launching session named for that worktree
// in the registration's authority store, the claim identity selected from
// canonical naming proofs and refreshed by a current held-claim read, and the pinned root
// (`TestRunNamedRoot`) the registration records. The held read runs off-lock;
// the root, service and claim identity are revalidated before publication.
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
/// store. Multiple entries refuse registration explicitly. With no store,
/// no keys, no such entry, or no proven binding for its claim, the run stays
/// unbound; the session's own binding and focus never decide.
fn test_run_registration_named_binding(
    inner: &StateInner,
    session_id: &str,
    store: Option<&EngramAuthorityStoreKey>,
    worktree: Option<&TestRunWorktreeKeys>,
) -> Result<(Option<EngramControlWorkBinding>, Option<TestRunNamedRoot>), ApiError> {
    let (Some(store), Some(worktree)) = (store, worktree) else {
        return Ok((None, None));
    };
    let mut matching = inner.engram_work_source_roots.iter().filter(|entry| {
        &entry.store == store
            && entry.named_by_session == session_id
            && entry.common_dir_key == worktree.common_dir
            && engram_exact_path_key(FsPath::new(&entry.root)) == worktree.root
    });
    let Some(entry) = matching.next() else {
        return Ok((None, None));
    };
    if let Some(other) = matching.next() {
        // Keep both identities and the remedy inside the launcher's 384-character
        // host-reason limit. No filesystem paths or arbitrary control text escape.
        let identity = |value: &str| value.chars().take(48)
            .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '?' })
            .collect::<String>();
        return Err(ApiError::conflict(format!(
            "ambiguous named roots: work {} claim {} generation {}; work {} claim {} generation {}. Clear the earlier name or use separate worktrees.",
            identity(&entry.work_id), identity(&entry.claim_id), entry.generation,
            identity(&other.work_id), identity(&other.claim_id), other.generation,
        )));
    }
    Ok(match engram_named_root_claim_binding(&inner.engram_work_naming_history, entry) {
        Some(binding) => (
            Some(binding),
            Some(TestRunNamedRoot {
                workspace_id: entry.root.clone(),
                generation: entry.generation,
            }),
        ),
        None => (None, None),
    })
}

/// An existing registration is immutable. A new one pins an off-lock held
/// read to the exact local root and naming proof it was selected from.
enum TestRunCurrentNamedBinding {
    Replay(TestRunRegistration),
    Fresh {
        store: Option<EngramAuthorityStoreKey>,
        named: (Option<EngramControlWorkBinding>, Option<TestRunNamedRoot>),
        named_fence: Option<i64>,
        current: Option<EngramControlWorkBinding>,
        target: Option<EngramBindingTarget>,
    },
}

impl AppState {
    fn read_test_run_current_named_binding(
        &self,
        session_id: &str,
        run_id: &str,
        keys: Option<&TestRunWorktreeKeys>,
        clock: &EngramBudgetClock,
        deadline: std::time::Instant,
        diagnostics: &mut TestRunRegistrationDiagnostics,
    ) -> Result<TestRunCurrentNamedBinding, ApiError> {
        let (store, named, named_fence, target) = {
            let inner = diagnostics.lock(&self.inner);
            if let Some(earlier) = inner.test_run_registrations.get(run_id).or_else(|| {
                inner.test_run_compacted.get(run_id).and_then(|row| row.registration.as_ref())
            }) {
                return Ok(TestRunCurrentNamedBinding::Replay(earlier.clone()));
            }
            // Use the same direct local project as registration capture,
            // including when an unbound run has no ready delegation inventory.
            let store = inner.find_visible_session_index(session_id)
                .and_then(|index| inner.sessions[index].session.project_id.as_deref())
                .and_then(|id| inner.projects.iter().find(|project| project.id == id))
                .filter(|project| project.remote_id == LOCAL_REMOTE_ID)
                .and_then(|project| project.engram.as_ref())
                .and_then(|settings| settings.authority_store_key.clone());
            let named = test_run_registration_named_binding(&inner, session_id, store.as_ref(), keys)?;
            let named_fence = test_run_named_root_fence(&inner, session_id, store.as_ref(), named.1.as_ref());
            let target = if named.0.is_some() {
                Some(Self::engram_source_root_target_locked(&inner, session_id)
                    .map_err(ApiError::conflict)?
                    .ok_or_else(|| ApiError::conflict("named run's work service is unavailable"))?)
            } else {
                None
            };
            (store, named, named_fence, target)
        };
        let current = if let (Some(previous), Some(target)) = (&named.0, &target) {
            let timeout = test_run_registration_remaining(clock, deadline, diagnostics)?
                .min(ENGRAM_WORK_BINDING_COMMAND_TIMEOUT).min(target.settings.call_timeout());
            let result = target.adapter.read_held_claims(&target.connection, timeout);
            test_run_registration_remaining(clock, deadline, diagnostics)?;
            let held = result
                .map_err(|error| ApiError::bad_gateway(format!("test registration held-claim read: {error}")))?;
            let mut exact = held.items.iter().filter(|claim| {
                claim.work_id == previous.work_id && claim.claim_id == previous.claim_id
            });
            let claim = match (exact.next(), exact.next()) {
                (Some(claim), None) => claim,
                (None, _) if held.omitted > 0 => return Err(ApiError::conflict(
                    "named run's exact held claim is absent from the bounded held list; other claims were omitted")),
                _ => return Err(ApiError::conflict("named run's exact held claim is unavailable or ambiguous")),
            };
            let binding = claim.control_binding.as_ref()
                .filter(|binding| engram_same_recovery_run(previous, binding)
                    && binding.claim_fence == claim.claim_fence
                    && binding.claim_fence >= previous.claim_fence
                    && binding.work_revision >= previous.work_revision)
                .ok_or_else(|| ApiError::conflict("named run's held association changed"))?;
            if Some(binding.claim_fence) != named_fence {
                return Err(ApiError::conflict("named run's claim fence changed; name the source root again"));
            }
            Some(binding.clone())
        } else {
            None
        };
        Ok(TestRunCurrentNamedBinding::Fresh { store, named, named_fence, current, target })
    }
}

impl TestRunCurrentNamedBinding {
    fn resolve_locked(
        &self,
        inner: &StateInner,
        session_id: &str,
        store: Option<&EngramAuthorityStoreKey>,
        keys: Option<&TestRunWorktreeKeys>,
    ) -> Result<(Option<EngramControlWorkBinding>, Option<TestRunNamedRoot>), ApiError> {
        match self {
            Self::Replay(earlier) => {
                let saved = inner.test_run_registrations.get(&earlier.run_id).or_else(|| {
                    inner.test_run_compacted.get(&earlier.run_id).and_then(|row| row.registration.as_ref())
                });
                if !saved.is_some_and(|actual| actual.matches_registration_snapshot(earlier)) {
                    return Err(ApiError::conflict("test run registration changed during its read"));
                }
                Ok((earlier.work_binding.clone(), earlier.named_root.clone()))
            }
            Self::Fresh { store: expected_store, named, named_fence, current, target } => {
                if expected_store.as_ref() != store
                    || &test_run_registration_named_binding(inner, session_id, store, keys)? != named
                    || &test_run_named_root_fence(inner, session_id, store, named.1.as_ref()) != named_fence
                {
                    return Err(ApiError::conflict("test run named root changed during its claim read"));
                }
                if let Some(target) = target {
                    let now = AppState::engram_source_root_target_locked(inner, session_id)
                        .map_err(ApiError::conflict)?
                        .ok_or_else(|| ApiError::conflict("test run work service changed during its read"))?;
                    if now.connection != target.connection
                        || !now.settings.same_work_service_settings(&target.settings)
                        || !Arc::ptr_eq(&now.adapter, &target.adapter)
                        || !inner.find_session_index(session_id).is_some_and(|index| {
                            AppState::engram_root_runtime_is_current(&inner.sessions[index], &target.runtime_snapshot)
                        })
                    {
                        return Err(ApiError::conflict("test run work authority changed during its read"));
                    }
                }
                Ok((current.clone(), named.1.clone()))
            }
        }
    }
}

fn test_run_named_root_fence(inner: &StateInner, session_id: &str, store: Option<&EngramAuthorityStoreKey>,
    named: Option<&TestRunNamedRoot>) -> Option<i64> {
    let (Some(store), Some(named)) = (store, named) else { return None; };
    inner.engram_work_source_roots.iter().find(|entry| &entry.store == store
        && entry.named_by_session == session_id
        && entry.root == named.workspace_id && entry.generation == named.generation)
        .map(|entry| entry.claim_fence)
}
