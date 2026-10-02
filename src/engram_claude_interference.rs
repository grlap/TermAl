// Interference of outstanding Claude work with Engram evidence: the one rule
// that decides whether retained Claude work fences a verification record or
// mixes a grant, and the reconciliation every transition calls.
//
// Owns: the interval clock that orders hazard registrations against record
// ends, the projection of the retained hazards (each live session's
// outstanding and unknown work, and the work deleted sessions left), the
// overlap rule (an interval that may overlap and a place that may hold the
// record, with the exact self-relation of a recognised simple full gate and
// the run it launched as the only exception), the cause told for a fence, and
// the reconciliation in both directions under the caller's state lock: a
// hazard registered, promoted, placed further or orphaned against every
// unpublished check, carried run and live grant; and a check started, carried
// or published, or a grant begun, against every retained hazard. A fence is
// sticky: a later end of the work, or a narrower view of it, never lifts it.
// The current restriction an agent is told is projected from the same facts
// and differs from the fences once the work ends.
//
// Does not own: the registry of outstanding work and its retirement, the
// exclusion of a grant from another turn's activity, or the notices
// (`claude_outstanding_work.rs`); checks (`engram_turn_checks.rs`), carried
// runs (`engram_carried_checks.rs`) or grants (`engram_host_adapter.rs`).
//
// New file; it centralises the fencing `claude_outstanding_work.rs`,
// `engram_turn_checks.rs` and `engram_carried_checks.rs` each did at their own
// entry points.

/// Orders hazard registrations against the ends of the records they may
/// overlap. Process-wide and monotone (one SeqCst counter). It is advanced
/// under the state lock for registrations and off it when a capture
/// completes, where the tick is stored under the capture's own lock with its
/// result.
static ENGRAM_INTERFERENCE_CLOCK: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

/// The next instant of the interference clock.
fn engram_interference_tick() -> u64 {
    ENGRAM_INTERFERENCE_CLOCK.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

/// Whose work a hazard is.
#[derive(Clone, Debug, PartialEq, Eq)]
enum EngramHazardOwner {
    /// A live session, by index, and what another session is told of it.
    Session { index: usize, cause: ClaudeHazardCause },
    /// A deleted session's work, kept on the host.
    Deleted { cause: ClaudeHazardCause },
}

/// One retained piece of outstanding Claude work that restricts: background
/// or subagent work, another runtime's or another turn's, or unknown work
/// (`ClaudeOutstandingEntry::is_hazard`). A root foreground command of its
/// session's current runtime and turn is not one: that turn's running
/// commands mark what it may write under, and it becomes one when its turn
/// or runtime is gone.
#[derive(Clone, Debug)]
struct EngramClaudeHazard {
    owner: EngramHazardOwner,
    /// Its tool-use id; `None` for work kept only as unknown.
    key: Option<String>,
    /// A top-level call of a recognised simple full gate.
    self_gate: bool,
    nested: bool,
    /// Its session may write; a deleted session's work counts as writing.
    owner_writes: bool,
    /// Where it may write; `None` is a worktree TermAl could not name.
    locations: Vec<Option<String>>,
    /// When it was first registered: it may have written from then on.
    registered_at: u64,
}

/// One unpublished verification record of the session at `session`.
struct EngramInterferenceSubject<'a> {
    session: usize,
    key: &'a str,
    /// The path key of the worktree it tested.
    root: String,
    /// When its evidence interval closed: after its command or run ended and
    /// every snapshot it records was taken (`EngramTurnCheck::interference_end`,
    /// `EngramCarriedCheck::interference_end`); `None` while it may still be
    /// reached.
    ended_at: Option<u64>,
    /// Its command is a recognised simple full gate.
    simple_full_gate: bool,
}

/// The hazards of `work`, owned by `owner`, whose session runs `current` at
/// turn `turn_generation`; only those of `keys` when given.
fn engram_hazards_of_work(
    owner: &EngramHazardOwner,
    work: &ClaudeOutstandingWork,
    current: Option<&RuntimeToken>,
    turn_generation: u64,
    owner_writes: bool,
    keys: Option<&[String]>,
) -> Vec<EngramClaudeHazard> {
    let mut hazards: Vec<EngramClaudeHazard> = work
        .entries
        .iter()
        .filter(|entry| keys.is_none_or(|keys| keys.contains(&entry.key)))
        .filter(|entry| entry.is_hazard(current, turn_generation))
        .map(|entry| EngramClaudeHazard {
            owner: owner.clone(),
            key: Some(entry.key.clone()),
            self_gate: entry.self_gate,
            nested: entry.nested,
            owner_writes,
            locations: entry.locations.clone(),
            registered_at: entry.registered_at,
        })
        .collect();
    if work.unknown && keys.is_none() {
        hazards.push(EngramClaudeHazard {
            owner: owner.clone(),
            key: None,
            self_gate: false,
            nested: false,
            owner_writes,
            locations: work.unknown_locations.clone(),
            registered_at: work.unknown_registered_at,
        });
    }
    hazards
}

/// The hazards of the live session at `index`, only those of `keys` when
/// given.
fn engram_session_hazards(
    inner: &StateInner,
    index: usize,
    keys: Option<&[String]>,
) -> Vec<EngramClaudeHazard> {
    let record = &inner.sessions[index];
    let owner = EngramHazardOwner::Session {
        index,
        cause: ClaudeHazardCause::OtherSession {
            session_id: record.session.id.clone(),
            name: record.session.name.clone(),
        },
    };
    engram_hazards_of_work(
        &owner,
        &record.claude_outstanding,
        record.runtime.runtime_token().as_ref(),
        record.active_turn_generation,
        engram_session_may_write(inner, index),
        keys,
    )
}

/// The hazards the deleted session `session_id` left on the host.
fn engram_orphaned_hazards(inner: &StateInner, session_id: Option<&str>) -> Vec<EngramClaudeHazard> {
    inner
        .claude_orphaned_work
        .iter()
        .filter(|orphan| session_id.is_none_or(|id| orphan.session_id == id))
        .flat_map(|orphan| {
            let owner = EngramHazardOwner::Deleted {
                cause: ClaudeHazardCause::DeletedSession {
                    session_id: orphan.session_id.clone(),
                },
            };
            // Nothing of a deleted session is its current turn's own.
            engram_hazards_of_work(&owner, &orphan.work, None, 0, true, None)
        })
        .collect()
}

/// Every retained hazard: each live session's, then the deleted sessions'.
fn engram_claude_hazards(inner: &StateInner) -> Vec<EngramClaudeHazard> {
    let mut hazards: Vec<EngramClaudeHazard> = (0..inner.sessions.len())
        .flat_map(|index| engram_session_hazards(inner, index, None))
        .collect();
    hazards.extend(engram_orphaned_hazards(inner, None));
    hazards
}

/// The rule: whether `hazard` may have written under `subject`, and why.
/// It may when its registration is not after the record closed and, for
/// another session's or a deleted session's work, a place it may write in may
/// hold the record's worktree. The session's own restricting work leaves its
/// provenance unresolved whatever workspace it measures. The only exception is
/// relational: the exact call of a recognised simple full gate does not fence
/// the record of the run it launched itself; it fences everything else.
fn engram_claude_interference(
    hazard: &EngramClaudeHazard,
    subject: &EngramInterferenceSubject<'_>,
) -> Option<ClaudeHazardCause> {
    if subject
        .ended_at
        .is_some_and(|ended_at| hazard.registered_at > ended_at)
    {
        return None;
    }
    match &hazard.owner {
        EngramHazardOwner::Session { index, .. } if *index == subject.session => {
            let own_launch = hazard.self_gate
                && !hazard.nested
                && subject.simple_full_gate
                && hazard.key.as_deref() == Some(subject.key);
            (!own_launch).then_some(ClaudeHazardCause::OwnSession)
        }
        EngramHazardOwner::Session { cause, .. } => (hazard.owner_writes
            && engram_worktrees_may_hold(&hazard.locations, &subject.root))
        .then(|| cause.clone()),
        EngramHazardOwner::Deleted { cause } => {
            engram_worktrees_may_hold(&hazard.locations, &subject.root).then(|| cause.clone())
        }
    }
}

/// The first of `hazards` that may have written under `subject`, and why.
fn engram_claude_interference_cause(
    hazards: &[EngramClaudeHazard],
    subject: &EngramInterferenceSubject<'_>,
) -> Option<ClaudeHazardCause> {
    hazards
        .iter()
        .find_map(|hazard| engram_claude_interference(hazard, subject))
}

/// Applies `hazards` to the unpublished records and the live grant of the
/// session at `consumer`, or of every session: each open or closed turn check
/// and each carried run they may overlap is fenced with the first cause, and
/// the grant is mixed by the session's own restricting work, whatever gate it
/// launched. A record already fenced keeps its first cause. Under the state
/// lock, in the section of the transition.
fn engram_apply_claude_hazards(
    inner: &mut StateInner,
    hazards: &[EngramClaudeHazard],
    consumer: Option<usize>,
) {
    if hazards.is_empty() {
        return;
    }
    for (index, record) in inner.sessions.iter_mut().enumerate() {
        if consumer.is_some_and(|consumer| consumer != index) {
            continue;
        }
        for check in &mut record.engram.active_turn_checks {
            let subject = EngramInterferenceSubject {
                session: index,
                key: &check.key,
                root: engram_path_key(&check.target.root),
                ended_at: check.interference_end(),
                simple_full_gate: engram_is_simple_full_launcher(&check.command),
            };
            if let Some(cause) = engram_claude_interference_cause(hazards, &subject) {
                check.overlapped = true;
                check.fenced_by_outstanding.get_or_insert(cause);
            }
        }
        for carried in &mut record.engram.carried_checks {
            if carried.fence.is_some() {
                continue;
            }
            // A carried run stays open until it is consumed or refused: any
            // later settlement may record what the work wrote. Work
            // registered before its run was read as terminal precedes every
            // candidate's closure, so it fences it outright. Work registered
            // after that is kept as potential interference, which each
            // candidate is judged against by its own closure at consumption
            // (`engram_take_settled_carried_checks`).
            let subject = EngramInterferenceSubject {
                session: index,
                key: &carried.check.key,
                root: engram_path_key(&carried.check.target.root),
                ended_at: None,
                simple_full_gate: engram_is_simple_full_launcher(&carried.check.command),
            };
            let terminal_at = carried
                .terminal_digest
                .as_ref()
                .and(carried.terminal_at);
            for hazard in hazards {
                let Some(cause) = engram_claude_interference(hazard, &subject) else {
                    continue;
                };
                match terminal_at {
                    Some(terminal_at) if hazard.registered_at > terminal_at => {
                        if carried
                            .potential
                            .as_ref()
                            .is_none_or(|(earliest, _)| hazard.registered_at < *earliest)
                        {
                            carried.potential = Some((hazard.registered_at, cause));
                        }
                    }
                    _ => {
                        carried.fence = Some(cause.describe());
                        carried.check.fenced_by_outstanding.get_or_insert(cause);
                        break;
                    }
                }
            }
        }
        let mixes_grant = hazards.iter().any(|hazard| {
            matches!(hazard.owner, EngramHazardOwner::Session { index: owner, .. } if owner == index)
        });
        if mixes_grant {
            claude_mark_grant_mixed(record);
        }
    }
}

/// Direction one: the work `keys` of the session at `owner` was registered,
/// promoted or placed further, so every unpublished record and live grant it
/// may overlap is reconciled with it, back to when it was registered.
fn engram_reconcile_claude_work(inner: &mut StateInner, owner: usize, keys: &[String]) {
    let hazards = engram_session_hazards(inner, owner, Some(keys));
    engram_apply_claude_hazards(inner, &hazards, None);
}

/// Direction one: the deleted session `session_id` left its work on the
/// host, which now counts wherever it may write.
fn engram_reconcile_orphaned_claude_work(inner: &mut StateInner, session_id: &str) {
    let hazards = engram_orphaned_hazards(inner, Some(session_id));
    engram_apply_claude_hazards(inner, &hazards, None);
}

/// Direction two: a record of the session at `consumer` started, was
/// carried or is about to be published, so its unpublished records and its
/// live grant are reconciled with every retained hazard.
fn engram_reconcile_claude_consumer(inner: &mut StateInner, consumer: usize) {
    let hazards = engram_claude_hazards(inner);
    engram_apply_claude_hazards(inner, &hazards, Some(consumer));
}

/// The cause a check of the session at `index`, starting now as the call
/// `key` running `command` in the worktree with path key `root`, is fenced
/// by: direction two at its start.
fn engram_claude_start_cause(
    inner: &StateInner,
    index: usize,
    key: &str,
    command: &EngramCheckCommand,
    root: &str,
) -> Option<ClaudeHazardCause> {
    let subject = EngramInterferenceSubject {
        session: index,
        key,
        root: root.to_owned(),
        ended_at: None,
        simple_full_gate: engram_is_simple_full_launcher(command),
    };
    engram_claude_interference_cause(&engram_claude_hazards(inner), &subject)
}

/// The current restriction, projected from the same facts as the fences: why
/// a check of the session at `index` (or of no session), starting now in the
/// worktree with path key `root`, would be fenced by work of any other
/// session or a deleted one. Differs from a stored fence once that work ends.
fn engram_claude_current_restriction(
    inner: &StateInner,
    index: Option<usize>,
    root: &str,
) -> Option<ClaudeHazardCause> {
    let subject = EngramInterferenceSubject {
        session: index.unwrap_or(usize::MAX),
        key: "",
        root: root.to_owned(),
        ended_at: None,
        simple_full_gate: false,
    };
    engram_claude_hazards(inner)
        .iter()
        .filter(|hazard| {
            !matches!(hazard.owner, EngramHazardOwner::Session { index: owner, .. } if Some(owner) == index)
        })
        .find_map(|hazard| engram_claude_interference(hazard, &subject))
}
