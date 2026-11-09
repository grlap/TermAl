// The work binding an ordinary committed-input run's registration
// (`"input": "commit"` in its request) is pinned to, taken from the named
// source root the run's worktree is, not from the launching session's current
// binding (which under turn-gated control OFF is stale or absent); other runs
// keep the session's binding (src/test_run_registration.rs). Owns the
// request's input-kind test, the run worktree's comparison keys, the choice
// of the one named root the launching session currently holds for that worktree
// in the registration's authority store (its original namer is immutable), the
// claim identity selected from canonical naming proofs and refreshed by a
// current held-claim read, and the pinned root
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
    test_run_registration_named_binding_with_held(inner, session_id, store, worktree, None)
}

// A launcher's fresh held read may establish a successor holder of the same
// claim. Native opening/command observers keep the original-namer lookup above.
fn test_run_held_claim_owns_name(
    inner: &StateInner, entry: &EngramWorkSourceRoot, held: &EngramHeldClaims,
) -> bool {
    let Some(previous) = engram_named_root_claim_binding(&inner.engram_work_naming_history, entry)
        else { return false; };
    let bound = engram_work_naming_proofs(&inner.engram_work_naming_history, &entry.store, &entry.work_id)
        .any(|proof| proof.binding == previous
            && matches!(proof.read.run.state.as_str(), "claimed" | "active")
            && matches!(&proof.read.named_root,
                EngramNamedRootState::Bound { workspace_id, generation, named_at }
                if workspace_id == &entry.root
                    && u64::try_from(*generation).ok() == Some(entry.generation)
                    && engram_named_at_matches(named_at, &entry.named_at)));
    bound && held.items.iter().any(|claim| {
        claim.work_id == entry.work_id && claim.claim_id == entry.claim_id
            && claim.control_binding.as_ref()
                .is_some_and(|binding| engram_same_recovery_run(&previous, binding))
    })
}

fn test_run_registration_named_binding_with_held(
    inner: &StateInner,
    session_id: &str,
    store: Option<&EngramAuthorityStoreKey>,
    worktree: Option<&TestRunWorktreeKeys>,
    held: Option<&EngramHeldClaims>,
) -> Result<(Option<EngramControlWorkBinding>, Option<TestRunNamedRoot>), ApiError> {
    let (Some(store), Some(worktree)) = (store, worktree) else {
        return Ok((None, None));
    };
    let mut matching = inner.engram_work_source_roots.iter().filter(|entry| {
        &entry.store == store
            && entry.common_dir_key == worktree.common_dir
            && engram_exact_path_key(FsPath::new(&entry.root)) == worktree.root
            && (entry.named_by_session == session_id
                || held.is_some_and(|held| test_run_held_claim_owns_name(inner, entry, held)))
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
        roots: Vec<EngramWorkSourceRoot>,
        held: Option<EngramHeldClaims>,
        allow_successor: bool,
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
        self.read_test_run_named_binding_for_scope(session_id, run_id, keys, clock, deadline, diagnostics, false)
    }

    fn read_test_run_current_launcher_named_binding(
        &self,
        session_id: &str,
        run_id: &str,
        keys: Option<&TestRunWorktreeKeys>,
        clock: &EngramBudgetClock,
        deadline: std::time::Instant,
        diagnostics: &mut TestRunRegistrationDiagnostics,
    ) -> Result<TestRunCurrentNamedBinding, ApiError> {
        self.read_test_run_named_binding_for_scope(session_id, run_id, keys, clock, deadline, diagnostics, true)
    }

    fn read_test_run_named_binding_for_scope(
        &self,
        session_id: &str,
        run_id: &str,
        keys: Option<&TestRunWorktreeKeys>,
        clock: &EngramBudgetClock,
        deadline: std::time::Instant,
        diagnostics: &mut TestRunRegistrationDiagnostics,
        allow_successor: bool,
    ) -> Result<TestRunCurrentNamedBinding, ApiError> {
        let (store, original, roots, target) = {
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
            // Preserve the existing ambiguity refusal for this session's own
            // names, but discover retained names before deciding who holds them.
            let original = test_run_registration_named_binding(&inner, session_id, store.as_ref(), keys)?;
            let roots = test_run_registration_root_candidates(&inner, store.as_ref(), keys)
                .into_iter().filter(|entry| allow_successor || entry.named_by_session == session_id).collect::<Vec<_>>();
            let needs_authority = if allow_successor { !roots.is_empty() } else { original.0.is_some() };
            let target = if needs_authority {
                Some(Self::engram_source_root_target_locked(&inner, session_id)
                    .map_err(ApiError::conflict)?
                    .ok_or_else(|| ApiError::conflict("named run's work service is unavailable"))?)
            } else {
                None
            };
            (store, original, roots, target)
        };
        let held = if let Some(target) = &target {
            let timeout = test_run_registration_remaining(clock, deadline, diagnostics)?
                .min(ENGRAM_WORK_BINDING_COMMAND_TIMEOUT).min(target.settings.call_timeout());
            let result = target.adapter.read_held_claims(&target.connection, timeout);
            test_run_registration_remaining(clock, deadline, diagnostics)?;
            Some(result.map_err(|error| ApiError::bad_gateway(format!("test registration held-claim read: {error}")))?)
        } else {
            None
        };
        let named = if !allow_successor {
            original
        } else if roots.is_empty() {
            (None, None)
        } else {
            let inner = diagnostics.lock(&self.inner);
            test_run_registration_named_binding_with_held(
                &inner, session_id, store.as_ref(), keys,
                if allow_successor { held.as_ref() } else { None })?
        };
        if allow_successor && held.as_ref().is_some_and(|held| held.omitted > 0
            && roots.iter().any(|entry| entry.named_by_session != session_id
                && !held.items.iter().any(|claim| {
                    claim.work_id == entry.work_id && claim.claim_id == entry.claim_id
                })))
        {
            return Err(ApiError::conflict("named run's candidate ownership is absent from the bounded held list; other claims were omitted"));
        }
        if allow_successor && named.0.is_none() && held.as_ref().is_some_and(|held| {
            roots.iter().any(|entry| held.items.iter().any(|claim| {
                claim.work_id == entry.work_id && claim.claim_id == entry.claim_id
            }))
        }) {
            return Err(ApiError::conflict("named run's source root has no proven claim held by the launching session"));
        }
        let current = if let (Some(previous), Some(held)) = (&named.0, &held) {
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
            let named_fence = roots.iter().find(|entry| entry.work_id == previous.work_id
                && entry.claim_id == previous.claim_id
                && named.1.as_ref().is_some_and(|named| named.workspace_id == entry.root
                    && named.generation == entry.generation)).map(|entry| entry.claim_fence);
            if Some(binding.claim_fence) != named_fence {
                return Err(ApiError::conflict("named run's claim fence changed; name the source root again"));
            }
            Some(binding.clone())
        } else {
            None
        };
        Ok(TestRunCurrentNamedBinding::Fresh { store, named, roots, held, allow_successor, current, target })
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
            Self::Fresh { store: expected_store, named, roots, held, allow_successor, current, target } => {
                let observed_fence = roots.iter().find(|entry| named.1.as_ref()
                    .is_some_and(|named| named.workspace_id == entry.root && named.generation == entry.generation))
                    .map(|entry| entry.claim_fence);
                if expected_store.as_ref() != store
                    || &test_run_registration_named_binding_with_held(inner, session_id, store, keys,
                        if *allow_successor { held.as_ref() } else { None })? != named
                    || if *allow_successor {
                        &test_run_registration_root_candidates(inner, store, keys) != roots
                    } else {
                        test_run_named_root_fence(inner, session_id, store, named.1.as_ref()) != observed_fence
                    }
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

// Bounded by the existing named-root registry. Preserve the entire entry so a
// handoff never rewrites naming provenance or hides a root/fence race.
fn test_run_registration_root_candidates(inner: &StateInner,
    store: Option<&EngramAuthorityStoreKey>, keys: Option<&TestRunWorktreeKeys>,
) -> Vec<EngramWorkSourceRoot> {
    let (Some(store), Some(keys)) = (store, keys) else { return Vec::new(); };
    inner.engram_work_source_roots.iter().filter(|entry| &entry.store == store
        && entry.common_dir_key == keys.common_dir
        && engram_exact_path_key(FsPath::new(&entry.root)) == keys.root).cloned().collect()
}

fn test_run_named_root_fence(inner: &StateInner, session_id: &str, store: Option<&EngramAuthorityStoreKey>,
    named: Option<&TestRunNamedRoot>) -> Option<i64> {
    let (Some(store), Some(named)) = (store, named) else { return None; };
    inner.engram_work_source_roots.iter().find(|entry| &entry.store == store
        && entry.named_by_session == session_id
        && entry.root == named.workspace_id && entry.generation == named.generation)
        .map(|entry| entry.claim_fence)
}
