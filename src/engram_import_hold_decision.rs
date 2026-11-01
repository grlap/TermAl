// Owns the pure exact-scope import-hold decision: given the host's
// import-reconciliation journal state (engram_import_reconciliation.rs) and
// one dispatch candidate's exact scope, it says whether the candidate is held
// by import reconciliation, admitted, or unknown. Does not own the journal,
// its persistence or activation, any lock, any dispatch or handoff path, or
// any hold installation or release; nothing in production calls it yet.

#[cfg_attr(not(test), allow(dead_code))]
/// The exact scope of one dispatch candidate, as its caller knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EngramImportDispatchCandidate {
    store_association: String,
    project_id: String,
    original_binding: EngramImportOriginalBinding,
    host_incarnation: String,
}

#[cfg_attr(not(test), allow(dead_code))]
/// One fence that holds a candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EngramImportHeldFence {
    key: String,
    hold_id: String,
    phase: EngramImportFencePhase,
    /// Whether the fence was installed by the current host incarnation. A
    /// fence from another incarnation holds all the same.
    current_incarnation: bool,
}

#[cfg_attr(not(test), allow(dead_code))]
/// Why the decision cannot say whether a candidate is held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngramImportHoldUnknownReason {
    /// The journal could not be established (the import-reconciliation
    /// journal's load refusal, engram_import_reconciliation.rs).
    JournalNotEstablished,
    /// The candidate's original binding is unknown or legacy.
    UnboundCandidate,
    /// No single enrolled owner matches the candidate's exact binding.
    OwnerUnresolved,
    /// The candidate's owner is not positively covered.
    OwnerCoverageUnknown,
    /// An owner-scoped fence's owner is not enrolled in the candidate's
    /// scope with exact bindings other than the candidate's, or has a live
    /// responsibility entry there whose binding is unknown or legacy, so the
    /// candidate cannot be shown to be another owner's work.
    FenceOwnerUnproven,
}

#[cfg_attr(not(test), allow(dead_code))]
/// The typed import-hold disposition of one dispatch candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
enum EngramImportHoldDisposition {
    /// Every fence that covers the candidate, in journal key order.
    HeldByImportReconciliation(Vec<EngramImportHeldFence>),
    Admitted,
    Unknown(EngramImportHoldUnknownReason),
}

#[cfg_attr(not(test), allow(dead_code))]
/// Decide whether `candidate` is held by import reconciliation.
///
/// A fence is in the candidate's scope when its store association and
/// project match exactly. A fence with no owner covers its whole scope. An
/// owner-scoped fence covers the candidate only when the candidate's exact
/// binding resolves to that owner; a candidate that cannot be shown to belong
/// to a different, positively covered owner is unknown, and so is one under
/// a fence whose own owner is not enrolled in the scope with exact bindings
/// other than the candidate's. An owner-scoped fence also covers a candidate
/// whose exact binding a live responsibility entry of the fence's owner
/// names, and a live entry of that owner with an unknown or legacy binding
/// leaves the candidate unknown rather than admitted. Any covering fence
/// holds, whatever its phase: only removing its record releases the guard.
/// Held wins over unknown, and both refuse dispatch.
fn engram_import_hold_disposition<E>(
    journal: Result<&EngramImportReconciliationState, &E>,
    candidate: &EngramImportDispatchCandidate,
) -> EngramImportHoldDisposition {
    let Ok(state) = journal else {
        return EngramImportHoldDisposition::Unknown(
            EngramImportHoldUnknownReason::JournalNotEstablished,
        );
    };
    // Exhaustive, so a new state cannot be admitted without a decision.
    let journal = match state {
        EngramImportReconciliationState::NotActivated => {
            return EngramImportHoldDisposition::Admitted;
        }
        EngramImportReconciliationState::Activated(journal) => journal,
    };
    let mut held = Vec::new();
    let mut unknown = None;
    let mut owner = None;
    for (key, record) in journal.records() {
        let EngramImportReconciliationRecord::LocalImportFence(fence) = record else {
            continue;
        };
        if !engram_import_scope_matches(&fence.scope, candidate) {
            continue;
        }
        let covers = match fence.scope.owner_id.as_deref() {
            None => Ok(true),
            Some(fence_owner) => {
                let entries = engram_import_fence_owner_entries(journal, candidate, fence_owner);
                if entries == EngramImportFenceOwnerEntries::NamesTheCandidate {
                    Ok(true)
                } else {
                    match owner
                        .get_or_insert_with(|| engram_import_candidate_owner(journal, candidate))
                    {
                        Err(reason) => Err(*reason),
                        Ok(owner) if owner.owner_id == fence_owner => Ok(true),
                        Ok(owner) if owner.fully_covered => {
                            if entries == EngramImportFenceOwnerEntries::NoBearing
                                && engram_import_fence_owner_is_another(
                                    journal,
                                    candidate,
                                    fence_owner,
                                )
                            {
                                Ok(false)
                            } else {
                                Err(EngramImportHoldUnknownReason::FenceOwnerUnproven)
                            }
                        }
                        Ok(_) => Err(EngramImportHoldUnknownReason::OwnerCoverageUnknown),
                    }
                }
            }
        };
        match covers {
            Ok(true) => held.push(EngramImportHeldFence {
                key: key.to_owned(),
                hold_id: fence.hold_id.clone(),
                phase: fence.phase,
                current_incarnation: fence.host_incarnation == candidate.host_incarnation,
            }),
            Ok(false) => {}
            Err(reason) => unknown = unknown.or(Some(reason)),
        }
    }
    if !held.is_empty() {
        EngramImportHoldDisposition::HeldByImportReconciliation(held)
    } else if let Some(reason) = unknown {
        EngramImportHoldDisposition::Unknown(reason)
    } else {
        EngramImportHoldDisposition::Admitted
    }
}

#[cfg_attr(not(test), allow(dead_code))]
/// Whether an owner-scoped fence's owner is provably not the candidate's:
/// it is enrolled in the candidate's scope, and each of its records there
/// has an exact binding other than the candidate's. An owner that is not
/// enrolled, or whose binding is unknown or legacy, may own the candidate's
/// work, so its fence does not let the candidate through.
fn engram_import_fence_owner_is_another(
    journal: &EngramImportReconciliationJournal,
    candidate: &EngramImportDispatchCandidate,
    fence_owner: &str,
) -> bool {
    let mut enrolled = false;
    for (_, record) in journal.records() {
        let EngramImportReconciliationRecord::EnrolledResponsibilityOwner(owner) = record else {
            continue;
        };
        if owner.owner_id != fence_owner || !engram_import_scope_matches(&owner.scope, candidate) {
            continue;
        }
        if !matches!(owner.original_binding, EngramImportOriginalBinding::Exact(_))
            || owner.original_binding == candidate.original_binding
        {
            return false;
        }
        enrolled = true;
    }
    enrolled
}

#[cfg_attr(not(test), allow(dead_code))]
/// What a fence owner's live responsibility entries say about a candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngramImportFenceOwnerEntries {
    /// No live entry of the fence owner bears on the candidate.
    NoBearing,
    /// A live entry of the fence owner has an unknown or legacy binding, so
    /// it may be the candidate's work.
    Unbound,
    /// A live entry of the fence owner names the candidate's exact binding,
    /// so the owner's fence covers the candidate.
    NamesTheCandidate,
}

#[cfg_attr(not(test), allow(dead_code))]
/// Read the fence owner's live (not terminal) responsibility entries in the
/// candidate's store and project. An entry with the candidate's exact
/// binding ties the fence to the candidate, whatever else is enrolled; an
/// entry whose binding is unknown or legacy may be the candidate's work.
/// Terminal entries, other owners' entries and other exact bindings say
/// nothing about the candidate. The fence owner's own enrollment,
/// association and coverage are not read: its entries count without them.
fn engram_import_fence_owner_entries(
    journal: &EngramImportReconciliationJournal,
    candidate: &EngramImportDispatchCandidate,
    fence_owner: &str,
) -> EngramImportFenceOwnerEntries {
    let mut found = EngramImportFenceOwnerEntries::NoBearing;
    for (_, record) in journal.records() {
        let EngramImportReconciliationRecord::ResponsibilityEntry(entry) = record else {
            continue;
        };
        if entry.owner_id != fence_owner
            || entry.state == EngramImportEntryState::Terminal
            || !engram_import_scope_matches(&entry.scope, candidate)
        {
            continue;
        }
        match entry.original_binding {
            // An exact entry equal to the candidate's binding implies the
            // candidate is exactly bound too.
            EngramImportOriginalBinding::Exact(_) => {
                if entry.original_binding == candidate.original_binding {
                    return EngramImportFenceOwnerEntries::NamesTheCandidate;
                }
            }
            _ => found = EngramImportFenceOwnerEntries::Unbound,
        }
    }
    found
}

#[cfg_attr(not(test), allow(dead_code))]
fn engram_import_scope_matches(
    scope: &EngramImportReconciliationScope,
    candidate: &EngramImportDispatchCandidate,
) -> bool {
    scope.store_association == candidate.store_association
        && scope.project_id == candidate.project_id
}

#[cfg_attr(not(test), allow(dead_code))]
/// The one enrolled owner a candidate's exact binding resolves to.
struct EngramImportCandidateOwner {
    owner_id: String,
    /// Every record of the owner for the candidate's exact binding in its
    /// store and project is on an admitted association with complete
    /// coverage.
    fully_covered: bool,
}

#[cfg_attr(not(test), allow(dead_code))]
fn engram_import_candidate_owner(
    journal: &EngramImportReconciliationJournal,
    candidate: &EngramImportDispatchCandidate,
) -> Result<EngramImportCandidateOwner, EngramImportHoldUnknownReason> {
    if !matches!(
        candidate.original_binding,
        EngramImportOriginalBinding::Exact(_)
    ) {
        return Err(EngramImportHoldUnknownReason::UnboundCandidate);
    }
    let mut resolved: Option<EngramImportCandidateOwner> = None;
    for (_, record) in journal.records() {
        let EngramImportReconciliationRecord::EnrolledResponsibilityOwner(owner) = record else {
            continue;
        };
        if !engram_import_scope_matches(&owner.scope, candidate)
            || owner.original_binding != candidate.original_binding
        {
            continue;
        }
        let covered = owner.association == EngramImportAssociationState::Admitted
            && owner.coverage == EngramImportCoverage::Complete;
        match resolved.as_mut() {
            Some(found) if found.owner_id != owner.owner_id => {
                return Err(EngramImportHoldUnknownReason::OwnerUnresolved);
            }
            Some(found) => found.fully_covered &= covered,
            None => {
                resolved = Some(EngramImportCandidateOwner {
                    owner_id: owner.owner_id.clone(),
                    fully_covered: covered,
                });
            }
        }
    }
    resolved.ok_or(EngramImportHoldUnknownReason::OwnerUnresolved)
}
