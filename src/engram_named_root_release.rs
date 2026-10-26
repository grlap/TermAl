// Owns what an operator sees of the global named-root table and the one
// audited action that releases a single retained root: one status row per
// retained root (its naming session, claim binding, run association,
// reclamation status and last known run state), served in the capacity
// refusal and by a read-only route, and an explicit release of an exact
// selection that never touches a live claim.
// Does not own the automatic reclamation pass or its canonical proof rule
// (engram_named_root_reclamation.rs), naming itself (engram_source_roots.rs),
// or producer events: a release creates none.
// New module.

/// One retained named root as an operator sees it.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct EngramNamedRootStatusRow {
    project_id: String,
    database_path: String,
    short_ref: String,
    work_id: String,
    claim_id: String,
    generation: u64,
    root: String,
    named_at: String,
    naming_session: String,
    naming_session_state: String,
    claim_binding: String,
    association: String,
    reclamation: String,
    lifecycle: String,
}

/// The exact selection an operator releases, and why. `requested_by` is
/// self-reported (the host API has no caller identity) and only recorded.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngramNamedRootReleaseRequest {
    project_id: String,
    database_path: String,
    work_id: String,
    claim_id: String,
    generation: u64,
    root: String,
    reason: String,
    #[serde(default)]
    requested_by: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct EngramNamedRootReleaseResponse {
    released: bool,
    route: String,
    resolution: &'static str,
    notice: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum EngramNamedRootReleasePhase {
    Enqueued,
}

/// A durable record of one operator release. `acknowledged` turns true once
/// the writer has committed the release (`PersistFenceTarget::
/// EngramNamedRootRelease`); until then an operator release keeps reserving
/// its slot (`engram_named_root_capacity`). On restore (`PersistedState::
/// into_inner`) an enqueued canonical intent survives with its retained
/// selection, without reserving another slot. Legacy intents retain their
/// old restore rule. A removed canonical selection needs its own retirement
/// notice (pending while unpublished); other removed records acknowledge.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct EngramNamedRootReleaseAudit {
    id: String,
    #[serde(default)]
    acknowledged: bool,
    selection: EngramWorkSourceRoot,
    route: String,
    reason: String,
    requested_by: Option<String>,
    lifecycle: String,
    released_at: String,
    /// For a canonical release, the authority transition it prepared,
    /// recorded before that owner can remove anything. The release is this
    /// operator's only if the selection's retirement notice names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    authority_owner: Option<String>,
    /// Stored beside the prepared owner before its first fence is enqueued.
    /// Absence preserves the legacy, never-enqueued restore disposition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    phase: Option<EngramNamedRootReleasePhase>,
}

/// Whether the canonical release `record` retired its selection itself:
/// a retirement notice of that selection names the record's own transition.
fn engram_named_root_release_retired_it(
    inner: &StateInner,
    record: &EngramNamedRootReleaseAudit,
) -> Option<bool> {
    let owner = record.authority_owner.as_deref()?;
    engram_named_root_retired_by(inner, &record.selection, owner).map(|notice| notice.published)
}

/// How many operator releases the audit keeps, newest last.
const ENGRAM_NAMED_ROOT_RELEASE_AUDIT_LIMIT: usize = 256;

/// The session that holds the current binding of `root`'s claim, if any.
fn engram_named_root_claim_holder<'a>(
    inner: &'a StateInner,
    root: &EngramWorkSourceRoot,
) -> Option<&'a SessionRecord> {
    inner.sessions.iter().find(|record| {
        record.engram.work_binding.as_ref().is_some_and(|binding| {
            binding.work_id == root.work_id && binding.claim_id == root.claim_id
        })
    })
}

/// The canonical run association the automatic pass reads `root` by: its
/// claim journal's run binding, when it matches the root's work and claim.
fn engram_named_root_association<'a>(
    inner: &'a StateInner,
    root: &EngramWorkSourceRoot,
) -> Option<&'a EngramControlWorkBinding> {
    engram_root_journal(&inner.engram_named_root_journal, &root.store, &root.claim_id)?
        .read_binding
        .as_ref()
        .filter(|binding| binding.work_id == root.work_id && binding.claim_id == root.claim_id)
}

fn engram_named_root_association_text(inner: &StateInner, root: &EngramWorkSourceRoot) -> String {
    match engram_root_journal(&inner.engram_named_root_journal, &root.store, &root.claim_id) {
        None => "no retained canonical run association (no claim journal)".to_owned(),
        Some(journal) if journal.pending.is_some() => {
            "claim journal with a pending naming event".to_owned()
        }
        Some(_) if engram_named_root_association(inner, root).is_some() => {
            "claim journal with its matching run binding".to_owned()
        }
        Some(_) => {
            "no retained canonical run association (claim journal without a matching run binding)".to_owned()
        }
    }
}

/// What the automatic pass does with `root` now, by the planner's own
/// predicates (`engram_root_reclamation_plan_locked`), and its last outcome.
fn engram_named_root_reclamation_text(
    inner: &StateInner,
    root: &EngramWorkSourceRoot,
    now: std::time::Instant,
) -> String {
    // A root without a canonical association needs no canonical reader: the
    // same flight's disposition decides it by rule.
    let status = if engram_named_root_association(inner, root).is_none() {
        engram_named_root_disposition_text(inner, root, now)
    } else if !engram_root_reclamation_reader_exists(inner, &root.store) {
        format!("skipped: no eligible reader for store {}", root.store.project_id)
    } else if let Some((_, until)) = inner
        .engram_root_reclamation
        .cooldowns
        .iter()
        .find(|(selection, until)| selection == root && now < *until)
    {
        format!(
            "cooling down for {} s after its last pass",
            until.saturating_duration_since(now).as_secs()
        )
    } else if engram_root_journal(&inner.engram_named_root_journal, &root.store, &root.claim_id)
        .is_some_and(|journal| journal.pending.is_some())
    {
        "skipped: a pending naming event belongs to its existing owner".to_owned()
    } else {
        match engram_named_root_current_owner(inner, root) {
            Some(owner) if !engram_root_reclamation_idle_owner(inner, &owner) => {
                format!("skipped: its current owner {owner} is active")
            }
            _ => "planned: eligible for the automatic pass".to_owned(),
        }
    };
    match inner
        .engram_work_naming_history
        .iter()
        .find(|history| history.store == root.store && history.work_id == root.work_id)
        .and_then(|history| history.recovery_reason.as_deref())
    {
        Some(reason) => format!("{status}; last outcome: {reason}"),
        None => status,
    }
}

/// The last canonical run state TermAl read for `root`'s claim, from the
/// work's retained proofs and retirement notices; never read here.
fn engram_named_root_lifecycle_text(inner: &StateInner, root: &EngramWorkSourceRoot) -> String {
    inner
        .engram_work_naming_history
        .iter()
        .find(|history| history.store == root.store && history.work_id == root.work_id)
        .and_then(|history| {
            let proofs = history
                .proofs
                .iter()
                .filter(|proof| proof.binding.claim_id == root.claim_id)
                .map(|proof| &proof.read);
            let retirements = history
                .retirements
                .iter()
                .filter(|notice| notice.selection.claim_id == root.claim_id)
                .map(|notice| &notice.read);
            proofs.chain(retirements).last().map(|read| format!("run {}", read.run.state))
        })
        .unwrap_or_else(|| "unknown (not read)".to_owned())
}

/// The status row of every retained root, under the lock, with no I/O.
fn engram_named_root_status_locked(
    inner: &StateInner,
    now: std::time::Instant,
) -> Vec<EngramNamedRootStatusRow> {
    inner
        .engram_work_source_roots
        .iter()
        .map(|root| {
            let naming_session_state = match inner.find_session_index(&root.named_by_session) {
                Some(index) => {
                    let session = &inner.sessions[index].session;
                    format!("present: {} ({:?})", session.name, session.status)
                }
                None if inner.quarantined_persisted_session_ids.contains(&root.named_by_session) => {
                    "present: kept aside at restore".to_owned()
                }
                None => "gone".to_owned(),
            };
            let claim_binding = match engram_named_root_claim_holder(inner, root) {
                Some(record) if engram_root_reclamation_idle_owner(inner, &record.session.id) => {
                    format!("current, idle: {}", record.session.id)
                }
                Some(record) => format!("current, active: {}", record.session.id),
                None => "none".to_owned(),
            };
            EngramNamedRootStatusRow {
                project_id: root.store.project_id.clone(),
                database_path: root.store.database_path.to_string_lossy().into_owned(),
                short_ref: root.short_ref.clone(),
                work_id: root.work_id.clone(),
                claim_id: root.claim_id.clone(),
                generation: root.generation,
                root: root.root.clone(),
                named_at: root.named_at.clone(),
                naming_session: root.named_by_session.clone(),
                naming_session_state,
                claim_binding,
                association: engram_named_root_association_text(inner, root),
                reclamation: engram_named_root_reclamation_text(inner, root, now),
                lifecycle: engram_named_root_lifecycle_text(inner, root),
            }
        })
        .collect()
}

/// One status row as the capacity refusal lists it.
fn engram_named_root_status_line(row: &EngramNamedRootStatusRow) -> String {
    format!(
        "{} / {} generation {}: naming session {} ({}); claim binding: {}; association: {}; \
         reclamation: {}; lifecycle: {}",
        row.project_id,
        row.short_ref,
        row.generation,
        row.naming_session,
        row.naming_session_state,
        row.claim_binding,
        row.association,
        row.reclamation,
        row.lifecycle
    )
}

/// The operator releases recorded so far.
#[cfg(test)]
fn engram_named_root_release_audit(inner: &StateInner) -> Vec<EngramNamedRootReleaseAudit> {
    inner.engram_named_root_release_audit.clone()
}

/// How many slots releases not yet acknowledged by the writer still reserve:
/// an operator, own or automatic release removes its root in memory only
/// until it commits. A canonical one is reserved by its unpublished
/// retirement instead.
fn engram_named_root_unacknowledged_releases(inner: &StateInner) -> usize {
    inner
        .engram_named_root_release_audit
        .iter()
        .filter(|record| {
            !record.acknowledged
                && record.route != EngramNamedRootReleaseRoute::Canonical.label()
                && !inner.engram_work_source_roots.contains(&record.selection)
        })
        .count()
}

/// Whether `metadata` holds the release `audit` committed: its record is
/// there and its selection is gone. The fence's proof, delta or read-back.
fn engram_named_root_release_in_metadata(
    audit: &EngramNamedRootReleaseAudit,
    metadata: &PersistedState,
) -> bool {
    metadata
        .engram_named_root_release_audit
        .iter()
        .any(|record| record.id == audit.id && record.selection == audit.selection)
        && !metadata.engram_work_source_roots.contains(&audit.selection)
}

// Test-only: makes a release's own persistence confirmation fail, as a
// writer timeout would, without failing an authority fence before it.
#[cfg(test)]
thread_local! {
    static TEST_ENGRAM_NAMED_ROOT_RELEASE_UNCONFIRMED: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// How long a release waits for the writer to commit it.
const ENGRAM_NAMED_ROOT_RELEASE_PERSIST_BUDGET: Duration = Duration::from_secs(10);

/// The exact retained selection `request` names, or `None` when no
/// retained root matches it in every field.
fn engram_named_root_release_selection(
    inner: &StateInner,
    request: &EngramNamedRootReleaseRequest,
) -> Option<EngramWorkSourceRoot> {
    inner
        .engram_work_source_roots
        .iter()
        .find(|root| engram_named_root_release_names(request, root))
        .cloned()
}

/// Whether `request` names `root` exactly, in every field.
fn engram_named_root_release_names(
    request: &EngramNamedRootReleaseRequest,
    root: &EngramWorkSourceRoot,
) -> bool {
    root.store.project_id == request.project_id
        && root.store.database_path == FsPath::new(&request.database_path)
        && root.work_id == request.work_id
        && root.claim_id == request.claim_id
        && root.generation == request.generation
        && root.root == request.root
}

/// The longest `reason` or `requestedBy` a release records.
const ENGRAM_NAMED_ROOT_RELEASE_TEXT_LIMIT: usize = 2048;

/// Refuses a release text over the limit or with a control character: the
/// audit persists it and the release's stderr line carries it.
fn engram_named_root_release_text(field: &str, text: &str) -> Result<(), ApiError> {
    if text.chars().count() > ENGRAM_NAMED_ROOT_RELEASE_TEXT_LIMIT {
        return Err(ApiError::bad_request(format!(
            "a named-root release's {field} is limited to {ENGRAM_NAMED_ROOT_RELEASE_TEXT_LIMIT} \
             characters"
        )));
    }
    if text.chars().any(char::is_control) {
        return Err(ApiError::bad_request(format!(
            "a named-root release's {field} must not contain control characters"
        )));
    }
    Ok(())
}

/// Which release `root` may take now, or why it must stay: never one whose
/// claim a session holds, or whose claim has a pending naming event; a root
/// with a canonical run association goes through the canonical authority;
/// one without (which the canonical pass and an own-clear cannot read) may be
/// released by the operator once its naming session is gone or quiet.
/// `own_session` names the caller when it releases its own root: its own turn
/// is not the busy state the operator route waits out for another session's
/// root.
fn engram_named_root_release_route_for(
    inner: &StateInner,
    root: &EngramWorkSourceRoot,
    own_session: Option<&str>,
) -> Result<EngramNamedRootReleaseRoute, ApiError> {
    if let Some(holder) = engram_named_root_claim_holder(inner, root) {
        return Err(ApiError::conflict(format!(
            "session {} holds a current binding for claim {}; a live claim's root is never \
             released. Nothing was released",
            holder.session.id, root.claim_id
        )));
    }
    if engram_root_journal(&inner.engram_named_root_journal, &root.store, &root.claim_id)
        .is_some_and(|journal| journal.pending.is_some())
    {
        return Err(ApiError::conflict(
            "a naming event for its claim is pending and belongs to its existing owner. Nothing \
             was released",
        ));
    }
    if engram_named_root_association(inner, root).is_some() {
        return Ok(EngramNamedRootReleaseRoute::Canonical);
    }
    // No canonical association, so neither the automatic pass nor an
    // own-clear can free it. Its naming session, gone or still present, must
    // not be mid-work: a busy session may yet act on the root. One kept aside
    // at restore may load again once repaired, and its binding and activity
    // cannot be read meanwhile, so it counts as neither gone nor quiet.
    if inner.quarantined_persisted_session_ids.contains(&root.named_by_session) {
        return Err(ApiError::conflict(format!(
            "its naming session {} was kept aside at restore; its binding and activity cannot \
             be read until it is repaired. Nothing was released",
            root.named_by_session
        )));
    }
    if own_session != Some(root.named_by_session.as_str())
        && let Some(index) = inner.find_session_index(&root.named_by_session)
        && !engram_named_root_session_quiet(&inner.sessions[index])
    {
        return Err(ApiError::conflict(format!(
            "its naming session {} is busy (in a turn, holding a grant or checkpointing); \
             release it once that session is idle. Nothing was released",
            root.named_by_session
        )));
    }
    Ok(EngramNamedRootReleaseRoute::Operator)
}

/// Whether the session in `record` is doing nothing that could act on a
/// root: no grant (active or uncertain), no checkpoint, no unmediated Claude
/// turn, and idle or stopped in error.
fn engram_named_root_session_quiet(record: &SessionRecord) -> bool {
    record.engram.active_grant_id.is_none()
        && record.engram.uncertain_grant_id.is_none()
        && !record.engram.checkpoint_in_progress
        && record.unmediated_claude_turn.is_none()
        && matches!(record.session.status, SessionStatus::Idle | SessionStatus::Error)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngramNamedRootReleaseRoute {
    Canonical,
    Operator,
}

impl EngramNamedRootReleaseRoute {
    fn label(self) -> &'static str {
        match self {
            Self::Canonical => "canonical",
            Self::Operator => "operator",
        }
    }
}

fn engram_named_root_release_record(
    root: &EngramWorkSourceRoot,
    route: EngramNamedRootReleaseRoute,
    reason: &str,
    requested_by: Option<&str>,
    lifecycle: &str,
) -> EngramNamedRootReleaseAudit {
    EngramNamedRootReleaseAudit {
        id: Uuid::new_v4().to_string(),
        acknowledged: false,
        selection: root.clone(),
        route: route.label().to_owned(),
        reason: reason.to_owned(),
        requested_by: requested_by.map(str::to_owned),
        lifecycle: lifecycle.to_owned(),
        released_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        authority_owner: None,
        phase: None,
    }
}

fn engram_named_root_release_durability_error(
    mut error: ApiError,
    audit: &EngramNamedRootReleaseAudit,
) -> ApiError {
    if let Some(deadline) = &mut error.durability_deadline {
        deadline.identity = json!({
            "workId": audit.selection.work_id,
            "workRef": audit.selection.short_ref,
            "claimId": audit.selection.claim_id,
            "generation": audit.selection.generation,
            "auditId": audit.id,
            "phase": audit.phase,
            "reason": audit.reason,
            "requesterSessionId": audit.requested_by,
        });
    }
    error.at_durability_boundary(DurabilityPendingOperation::NamedRootRelease)
}

fn engram_named_root_release_conflict(
    audit: &EngramNamedRootReleaseAudit,
    reason: &str,
) -> ApiError {
    ApiError::conflict(format!(
        "release audit {} generation {} resolution=conflict: {reason}; the original audit is retained",
        audit.id, audit.selection.generation,
    ))
}

impl AppState {
    /// Attach the release to the first prepared image under the same lock.
    /// A replay confirms and continues that owner, never prepares a replacement.
    fn prepare_engram_release_authority_until(
        &self,
        root: &EngramWorkSourceRoot,
        binding: &EngramControlWorkBinding,
        audit_id: &str,
        deadline: std::time::Instant,
    ) -> Result<EngramAuthorityTransition, ApiError> {
        let original_owner = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let audit = inner.engram_named_root_release_audit.iter()
                .find(|audit| audit.id == audit_id)
                .ok_or_else(|| ApiError::conflict("the release audit disappeared"))?;
            match audit.authority_owner.as_deref() {
                None => None,
                Some(id) => {
                    if audit.phase != Some(EngramNamedRootReleasePhase::Enqueued) {
                        return Err(engram_named_root_release_conflict(audit,
                            "the retained audit has no enqueued authority stage"));
                    }
                    let owner = inner.engram_work_naming_history.iter()
                        .find(|history| history.store == root.store && history.work_id == root.work_id)
                        .and_then(|history| history.transition.as_ref()
                            .filter(|owner| owner.version == history.owner_version))
                        .filter(|owner| owner.id == id && owner.binding == *binding)
                        .cloned()
                        .ok_or_else(|| engram_named_root_release_conflict(audit,
                            "the current authority owner no longer matches the enqueued release"))?;
                    Some(owner)
                }
            }
        };
        // A candidate is an existing exact-image publication obligation.
        // Settle it before re-arming the same operation for another read.
        if original_owner.as_ref().is_some_and(|owner| owner.phase == EngramAuthorityPhase::Candidate) {
            self.recover_engram_authority_candidate_until(&root.store, &root.work_id, deadline)?;
        }
        let image = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner.engram_named_root_release_audit.iter()
                .position(|audit| audit.id == audit_id)
                .ok_or_else(|| ApiError::conflict("the release audit disappeared"))?;
            let audit = &inner.engram_named_root_release_audit[index];
            if audit.selection != *root || audit.acknowledged
                || audit.route != EngramNamedRootReleaseRoute::Canonical.label()
                || !inner.engram_work_source_roots.contains(root)
            {
                return Err(engram_named_root_release_conflict(audit,
                    "the exact retained selection or audit changed"));
            }
            engram_named_root_release_route_for(&inner, root, None)
                .map_err(|error| engram_named_root_release_conflict(audit, &error.message))?;
            if let Some(owner_id) = audit.authority_owner.as_deref() {
                let history = inner.engram_work_naming_history.iter()
                    .find(|history| history.store == root.store && history.work_id == root.work_id);
                if audit.phase != Some(EngramNamedRootReleasePhase::Enqueued)
                    || !history.is_some_and(|history| history.transition.as_ref().is_some_and(|owner|
                        owner.id == owner_id && owner.version == history.owner_version
                            && owner.binding == *binding
                            && original_owner.as_ref().is_some_and(|original|
                                original.id == owner.id && original.version == owner.version
                                    && original.binding == owner.binding)
                            && owner.phase != EngramAuthorityPhase::Candidate))
                {
                    return Err(engram_named_root_release_conflict(audit,
                        "the current authority owner no longer matches the enqueued release"));
                }
                let history = inner.engram_work_naming_history.iter_mut()
                    .find(|history| history.store == root.store && history.work_id == root.work_id)
                    .expect("validated release history");
                if history.transition.as_ref().expect("validated release owner").phase
                    == EngramAuthorityPhase::Published
                {
                    // Retain every prior fact and the UUID/version. This new
                    // exact Prepared ACK, not an old Candidate ACK, guards
                    // the original release's current canonical reread.
                    history.transition.as_mut().expect("validated release owner").phase = EngramAuthorityPhase::Prepared;
                    engram_retain_recovery_run(&mut history.unresolved_runs, binding);
                    history.recovery_reason.get_or_insert_with(||
                        "the retained release requires a fresh guarded canonical read".to_owned());
                }
            } else {
                // Prior-candidate recovery belongs before audit allocation.
                // Do not await a concurrently appearing candidate and then
                // advertise this still-unprepared audit as its identity.
                if inner.engram_work_naming_history.iter().any(|history|
                    history.store == root.store && history.work_id == root.work_id
                        && history.transition.as_ref().is_some_and(|owner|
                            owner.phase == EngramAuthorityPhase::Candidate))
                {
                    return Err(engram_named_root_release_conflict(audit,
                        "a candidate appeared before this release was prepared; retry after its authority settles"));
                }
                let prepared = self.prepare_engram_authority_image_locked(&mut inner, &root.store, binding)?;
                let audit = &mut inner.engram_named_root_release_audit[index];
                audit.authority_owner = Some(prepared.history.transition.as_ref()
                    .expect("prepared owner").id.clone());
                audit.phase = Some(EngramNamedRootReleasePhase::Enqueued);
            }
            EngramAuthorityImage::capture(&inner, &root.store, &root.work_id)?
        };
        self.confirm_engram_authority_image_until(&image, deadline)?;
        Ok(image.history.transition.expect("prepared release owner"))
    }

    /// Confirms an earlier release whose writer confirmation failed: the
    /// same fence over its recorded removal and audit, then the record is
    /// acknowledged and its reserved slot freed. Its route, reason and
    /// requester stay those of the original release. A canonical release
    /// whose retirement was committed but not published first finishes that
    /// publication, since the unpublished retirement still holds the slot.
    fn resume_engram_named_root_release(
        &self,
        mut pending: EngramNamedRootReleaseAudit,
    ) -> Result<EngramNamedRootReleaseResponse, ApiError> {
        let mut resolution = "current_state";
        if pending.route == EngramNamedRootReleaseRoute::Canonical.label() {
            let deadline =
                self.engram_budget_clock().now() + ENGRAM_NAMED_ROOT_RELEASE_AUTHORITY_BUDGET;
            let retained_without_notice = {
                let inner = self.inner.lock().expect("state mutex poisoned");
                inner.engram_work_source_roots.contains(&pending.selection)
                    && engram_named_root_release_retired_it(&inner, &pending).is_none()
            };
            if retained_without_notice {
                // A deadline in the authority guard can precede removal.
                // Resume this audit, not a second release record.
                let outcome = self.recover_one_engram_root_traced_until(
                    &pending.selection,
                    &EngramRootReclamationRoute::Unfocused,
                    deadline,
                    Some(&pending.id),
                );
                pending = self.inner.lock().expect("state mutex poisoned")
                    .engram_named_root_release_audit.iter()
                    .find(|record| record.id == pending.id).cloned()
                    .ok_or_else(|| ApiError::conflict("the release audit changed during recovery"))?;
                outcome.map_err(|error| engram_named_root_release_durability_error(error, &pending))?;
                resolution = "resumed";
            }
            self.recover_engram_authority_candidate_until(
                &pending.selection.store,
                &pending.selection.work_id,
                deadline,
            ).map_err(|error| engram_named_root_release_durability_error(error, &pending))?;
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(published) = engram_named_root_release_retired_it(&inner, &pending) else {
                if inner.engram_work_source_roots.contains(&pending.selection) {
                    return Err(engram_named_root_release_conflict(&pending,
                        "the canonical facts still retain this root; retry the unchanged request when authority can settle it"));
                }
                // Another owner retired it: not this release's to record.
                engram_named_root_withdraw_release_locked(&mut inner, &pending.id);
                self.commit_engram_root_locked(&mut inner)
                    .map_err(|error| ApiError::internal(error.to_string()))?;
                return Err(ApiError::conflict(
                    "the root was retired by another owner (such as the automatic reclamation \
                     pass), not by the recorded release, whose record is withdrawn. Nothing was \
                     released by this request",
                ));
            };
            if !published {
                return Err(ApiError::conflict(format!(
                    "the earlier release of {} generation {} removed the root, but its \
                     retirement is not yet published; its slot stays reserved. Retry the \
                     same request",
                    pending.selection.short_ref, pending.selection.generation
                )));
            }
        }
        let deadline = self.engram_budget_clock().now() + ENGRAM_NAMED_ROOT_RELEASE_PERSIST_BUDGET;
        self.confirm_engram_named_root_release_until(&pending, deadline)
            .map_err(|error| engram_named_root_release_durability_error(error, &pending))?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if let Some(record) = inner
            .engram_named_root_release_audit
            .iter_mut()
            .find(|record| record.id == pending.id)
        {
            record.acknowledged = true;
        }
        self.commit_engram_root_locked(&mut inner)
            .map_err(|error| ApiError::internal(error.to_string()))?;
        drop(inner);
        engram_root_reclamation_emit(self, &engram_named_root_release_line(&pending));
        Ok(EngramNamedRootReleaseResponse {
            released: true,
            route: pending.route.clone(),
            resolution,
            notice: format!(
                "The earlier release of named source root {} ({}) generation {} is now \
                 confirmed; its slot is free.",
                pending.selection.short_ref, pending.selection.work_id, pending.selection.generation
            ),
        })
    }

    /// Waits until the writer has committed the release `audit`: its record
    /// and the removal of its selection, read from the committed metadata.
    fn confirm_engram_named_root_release_until(
        &self,
        audit: &EngramNamedRootReleaseAudit,
        deadline: std::time::Instant,
    ) -> Result<(), ApiError> {
        #[cfg(test)]
        if TEST_ENGRAM_NAMED_ROOT_RELEASE_UNCONFIRMED.with(std::cell::Cell::get) {
            return Err(ApiError::internal(format!(
                "the release of {} generation {} is recorded but its persistence is \
                 unconfirmed (test); its slot stays reserved",
                audit.selection.short_ref, audit.selection.generation,
            )));
        }
        let clock = self.engram_budget_clock();
        let (fence, waiter) = PersistFence::new_with_clock(
            PersistFenceTarget::EngramNamedRootRelease(Box::new(audit.clone())),
            deadline,
            clock,
        );
        if self.persist_tx.send(PersistRequest::Fence(Box::new(fence))).is_ok() {
            return match waiter.wait_until(deadline).unwrap_or(Err(PersistFenceError::Deadline)) {
                Ok(()) => Ok(()),
                Err(PersistFenceError::Deadline) => Err(ApiError::enqueued_durability_deadline(format!(
                    "the release of {} generation {} is recorded but its persistence is \
                     unconfirmed (Deadline) while {}; its slot stays reserved",
                    audit.selection.short_ref,
                    audit.selection.generation,
                    waiter.progress().describe(),
                ), json!({
                    "workId": audit.selection.work_id,
                    "claimId": audit.selection.claim_id,
                    "generation": audit.selection.generation,
                    "auditId": audit.id,
                }))),
                Err(error) => Err(ApiError::internal(format!(
                    "the release of {} generation {} is recorded but its persistence is \
                     unconfirmed ({error:?}); its slot stays reserved",
                    audit.selection.short_ref, audit.selection.generation,
                ))),
            };
        }
        // A stopped writer can acknowledge nothing. Manually constructed test
        // states write the same delta off-lock, as the authority fence does.
        if !engram_authority_manual_writer_allowed() {
            return Err(ApiError::internal(format!(
                "the release of {} generation {} is recorded but the state writer has \
                 stopped; its slot stays reserved",
                audit.selection.short_ref, audit.selection.generation,
            )));
        }
        #[cfg(test)]
        {
            let delta = collect_persist_delta_from_shared_state(&self.inner, 0);
            let mut cache = SqlitePersistConnectionCache::new();
            persist_delta_via_cache(&mut cache, self.persistence_path.as_path(), &delta).map_err(
                |error| {
                    ApiError::internal(format!(
                        "the release persistence is unconfirmed: {error:#}; its slot stays reserved"
                    ))
                },
            )?;
            // Omission cannot prove selection absence. The existing scoped
            // fence reader checks both collections in one committed cut.
            if !PersistFenceTarget::EngramNamedRootRelease(Box::new(audit.clone()))
                .is_already_durable(
                    cache.connection.as_ref().expect("committed writer connection"),
                )
                .map_err(|error| ApiError::internal(format!("release read-back failed: {error:#}")))?
            {
                return Err(ApiError::conflict(
                    "the released selection changed before its persistence was confirmed",
                ));
            }
        }
        Ok(())
    }
}

/// The operator-visible stderr line of a confirmed release, written once
/// its record is acknowledged, on the first attempt or on its resume.
fn engram_named_root_release_line(audit: &EngramNamedRootReleaseAudit) -> String {
    format!(
        "engram> named-root release route={} store={} work={} generation={} requested_by={:?} \
         lifecycle={} reason={:?}",
        audit.route,
        audit.selection.store.project_id,
        audit.selection.work_id,
        audit.selection.generation,
        audit.requested_by.as_deref().unwrap_or("operator"),
        audit.lifecycle,
        audit.reason,
    )
}

/// Withdraws the release record `id` of a release that removed nothing.
fn engram_named_root_withdraw_release_locked(inner: &mut StateInner, id: &str) {
    inner.engram_named_root_release_audit.retain(|record| record.id != id);
}

fn engram_named_root_record_release_locked(
    inner: &mut StateInner,
    record: EngramNamedRootReleaseAudit,
) {
    let audit = &mut inner.engram_named_root_release_audit;
    audit.push(record);
    // Only completed history is trimmed, oldest first: an unacknowledged
    // release is a pending obligation (its retry and, for the operator route,
    // its reserved slot), kept until its commit is confirmed.
    let mut excess = audit.len().saturating_sub(ENGRAM_NAMED_ROOT_RELEASE_AUDIT_LIMIT);
    audit.retain(|record| {
        if excess > 0 && record.acknowledged {
            excess -= 1;
            false
        } else {
            true
        }
    });
}

impl AppState {
    /// The read-only view the route serves.
    fn engram_named_root_status(&self) -> Vec<EngramNamedRootStatusRow> {
        let now = self.engram_budget_clock().now();
        let inner = self.inner.lock().expect("state mutex poisoned");
        engram_named_root_status_locked(&inner, now)
    }

    /// Releases one exact retained root on an operator's explicit request,
    /// with its reason recorded in the audit. A root with a canonical run
    /// association is released only on the canonical authority's proof, as
    /// the automatic pass does; one without, only once no session holds its
    /// claim and its naming session is gone or quiet. No producer event is
    /// created.
    fn release_engram_named_root(
        &self,
        requested_by: Option<&str>,
        request: EngramNamedRootReleaseRequest,
    ) -> Result<EngramNamedRootReleaseResponse, ApiError> {
        self.release_engram_named_root_as(requested_by, None, request)
    }

    /// `release_engram_named_root`, where `own_session` is set when the
    /// caller is the root's naming session releasing its own root
    /// (engram_named_root_capacity.rs): its own turn does not hold the
    /// release back, and the audit records route `own`. Every release takes
    /// the single reclamation flight first, so it never races the automatic
    /// pass. A proven refusal is a conflict; an enqueued fence deadline is
    /// pending because the recorded authority image may still commit.
    fn release_engram_named_root_as(
        &self,
        requested_by: Option<&str>,
        own_session: Option<&str>,
        request: EngramNamedRootReleaseRequest,
    ) -> Result<EngramNamedRootReleaseResponse, ApiError> {
        let reason = request.reason.trim().to_owned();
        if reason.is_empty() {
            return Err(ApiError::bad_request(
                "a named-root release needs a reason, which its audit records",
            ));
        }
        engram_named_root_release_text("reason", &reason)?;
        // Self-reported and only recorded: trimmed, and blank counts as absent.
        let requested_by = requested_by.map(str::trim).filter(|text| !text.is_empty());
        if let Some(requested_by) = requested_by {
            engram_named_root_release_text("requestedBy", requested_by)?;
        }
        // Serialize the audit lookup as well as the removal: two callers
        // cannot both observe no pending audit and allocate distinct intents.
        let _flight = self.acquire_engram_named_root_release_flight()?;
        let (root, route) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if let Some(previous) = inner.engram_named_root_release_audit.iter().rev()
                .find(|record| engram_named_root_release_names(&request, &record.selection))
                .cloned()
            {
                if previous.reason != reason || previous.requested_by.as_deref() != requested_by
                    || own_session.is_some_and(|session| previous.selection.named_by_session != session)
                {
                    return Err(engram_named_root_release_conflict(&previous,
                        "this release identity already has a different reason or requester; retry the unchanged original request"));
                }
                if previous.acknowledged {
                    if inner.engram_work_source_roots.contains(&previous.selection) {
                        return Err(ApiError::conflict(
                            "the acknowledged release selection is retained again; reconcile its owner before any further release",
                        ));
                    }
                    return Ok(EngramNamedRootReleaseResponse {
                        released: true,
                        route: previous.route,
                        resolution: "current_state",
                        notice: "This exact named-root release is already acknowledged; no new release was applied.".to_owned(),
                    });
                }
                if inner.engram_work_source_roots.contains(&previous.selection) {
                    engram_named_root_release_route_for(&inner, &previous.selection, own_session)
                        .map_err(|error| engram_named_root_release_conflict(&previous, &error.message))?;
                }
                drop(inner);
                return self.resume_engram_named_root_release(previous);
            }
            let Some(root) = engram_named_root_release_selection(&inner, &request) else {
                // An earlier release of exactly this selection whose writer
                // confirmation failed is resumed: its slot is freed once the
                // commit is confirmed, without a restart.
                let pending = inner
                    .engram_named_root_release_audit
                    .iter()
                    .rev()
                    .find(|record| {
                        !record.acknowledged
                            && engram_named_root_release_names(&request, &record.selection)
                    })
                    .cloned();
                drop(inner);
                return match pending {
                    Some(pending) => {
                        // A resume finishes the same retirement, so it takes
                        // the flight as the release itself did.
                        self.resume_engram_named_root_release(pending)
                    }
                    None => Err(ApiError::conflict(
                        "the named selection is no longer retained exactly (store, work, \
                         claim, generation and root must all match). Nothing was released",
                    )),
                };
            };
            let route = engram_named_root_release_route_for(&inner, &root, own_session)?;
            (root, route)
        };
        let lifecycle;
        let mut audit;
        match route {
            EngramNamedRootReleaseRoute::Canonical => {
                let deadline =
                    self.engram_budget_clock().now() + ENGRAM_NAMED_ROOT_RELEASE_AUTHORITY_BUDGET;
                // This older authority obligation is not a new release's
                // preparation. Its Deadline must identify that exact owner,
                // with no phantom audit that restore would discard.
                self.recover_engram_authority_candidate_until(&root.store, &root.work_id, deadline)
                    .map_err(|mut error| {
                        if let Some(pending) = &mut error.durability_deadline {
                            pending.identity["auditId"] = Value::Null;
                            pending.identity["workRef"] = json!(root.short_ref);
                            pending.identity["reason"] = json!(reason);
                            pending.identity["requesterSessionId"] = json!(requested_by);
                            pending.identity["phase"] = json!("candidate");
                            pending.identity["releaseStage"] = json!("prior_authority");
                            pending.identity["requestedSelection"] = json!({
                                "workId": root.work_id, "claimId": root.claim_id,
                                "generation": root.generation, "root": root.root,
                            });
                        }
                        error.at_durability_boundary(DurabilityPendingOperation::NamedRootRelease)
                    })?;
                // The operator's intent is recorded BEFORE the retirement, so
                // the authority's own image commits it with the removal: a
                // failure or crash after the removal leaves an unacknowledged
                // record that an identical retry resumes.
                {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if !inner.engram_work_source_roots.contains(&root)
                        || engram_named_root_release_route_for(&inner, &root, own_session)? != route
                    {
                        return Err(ApiError::conflict(
                            "the exact release selection changed while prior authority settled; no new release was prepared"));
                    }
                    if inner.engram_work_naming_history.iter().any(|history|
                        history.store == root.store && history.work_id == root.work_id
                            && history.transition.as_ref().is_some_and(|owner|
                                owner.phase == EngramAuthorityPhase::Candidate))
                    {
                        return Err(ApiError::conflict(
                            "authority changed while the prior candidate settled; retry before creating a release"));
                    }
                    lifecycle = engram_named_root_lifecycle_text(&inner, &root);
                    audit = engram_named_root_release_record(
                        &root, route, &reason, requested_by, &lifecycle,
                    );
                    engram_named_root_record_release_locked(&mut inner, audit.clone());
                    self.commit_engram_root_locked(&mut inner)
                        .map_err(|error| ApiError::internal(error.to_string()))?;
                }
                // The transition this release prepares is recorded on its
                // intent before that owner can remove anything, so the image
                // that commits a removal by it also commits the link.
                let outcome = self.recover_one_engram_root_traced_until(
                    &root,
                    &EngramRootReclamationRoute::Unfocused,
                    deadline,
                    Some(&audit.id),
                );
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                // Preparation records the owner and enqueued phase under its
                // lock, before the first authority image can reach the writer.
                audit = inner.engram_named_root_release_audit.iter()
                    .find(|record| record.id == audit.id).cloned().unwrap_or(audit);
                let still_retained = inner.engram_work_source_roots.contains(&root);
                let retired_it = inner
                    .engram_named_root_release_audit
                    .iter()
                    .find(|record| record.id == audit.id)
                    .and_then(|record| engram_named_root_release_retired_it(&inner, record));
                match (retired_it, outcome) {
                    // This release retired and published it.
                    (Some(true), _) => {}
                    // This release removed it, but its publication or commit
                    // is unconfirmed: the record stays for the retry to resume.
                    (Some(false), Err(error)) => return Err(
                        engram_named_root_release_durability_error(error, &audit)
                    ),
                    (Some(false), Ok(_)) => {
                        return Err(ApiError::conflict(format!(
                            "this release removed {} generation {}, but its retirement is not \
                             yet published; its slot stays reserved. Retry the same request",
                            root.short_ref, root.generation
                        )));
                    }
                    // This release removed nothing: the intent is withdrawn.
                    (None, outcome) => {
                        if let Err(error) = outcome.as_ref()
                            && error.is_enqueued_durability_deadline()
                        {
                            // The exact authority image can still commit; do
                            // not withdraw its audit or claim not-applied.
                            let error = outcome.err().expect("matched deadline");
                            return Err(engram_named_root_release_durability_error(error, &audit));
                        }
                        engram_named_root_withdraw_release_locked(&mut inner, &audit.id);
                        // Only revision exhaustion fails here. No enqueued
                        // deadline is withdrawn above; a proven refusal or
                        // another owner's retirement is not this release.
                        let committed = self.commit_engram_root_locked(&mut inner);
                        let why = inner
                            .engram_work_naming_history
                            .iter()
                            .find(|history| {
                                history.store == root.store && history.work_id == root.work_id
                            })
                            .and_then(|history| history.recovery_reason.clone())
                            .unwrap_or_else(|| {
                                "the canonical run and binding remain open".to_owned()
                            });
                        drop(inner);
                        if let Err(error) = committed {
                            engram_root_reclamation_emit(
                                self,
                                &format!(
                                    "engram> named-root release withdrawal not committed: {error}"
                                ),
                            );
                        }
                        if !still_retained {
                            // Retired meanwhile by another owner, such as the
                            // automatic pass: not this release's to record.
                            return Err(ApiError::conflict(
                                "the root was already retired by another owner (such as the \
                                 automatic reclamation pass) while this release ran. Nothing \
                                 was released by this request",
                            ));
                        }
                        if let Err(error) = outcome {
                            // Nothing this release did was applied: say so,
                            // never an unknown outcome.
                            return Err(ApiError::conflict(format!(
                                "the release was not applied: {}; the root is still retained. \
                                 Retry the same request",
                                error.message
                            )));
                        }
                        return Err(ApiError::conflict(format!(
                            "the canonical authority keeps this root: {why}. Nothing was released"
                        )));
                    }
                }
            }
            EngramNamedRootReleaseRoute::Operator => {
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                // Decided again under this hold: nothing may have moved since.
                let Some(index) =
                    inner.engram_work_source_roots.iter().position(|current| *current == root)
                else {
                    return Err(ApiError::conflict(
                        "the named selection changed while the release was checked. Nothing \
                         was released",
                    ));
                };
                if engram_named_root_release_route_for(&inner, &root, own_session)?
                    != EngramNamedRootReleaseRoute::Operator
                {
                    return Err(ApiError::conflict(
                        "the selection gained a canonical run association while the release \
                         was checked. Nothing was released",
                    ));
                }
                lifecycle = engram_named_root_lifecycle_text(&inner, &root);
                // Removed in memory, but its slot stays reserved by the
                // unacknowledged record until the writer commits both.
                inner.engram_work_source_roots.remove(index);
                let mut record =
                    engram_named_root_release_record(&root, route, &reason, requested_by, &lifecycle);
                if own_session.is_some() {
                    record.route = ENGRAM_NAMED_ROOT_OWN_ROUTE.to_owned();
                }
                audit = record;
                engram_named_root_record_release_locked(&mut inner, audit.clone());
                self.commit_engram_root_locked(&mut inner)
                    .map_err(|error| ApiError::internal(error.to_string()))?;
            }
        }
        // Success only once the writer committed the removal and its audit.
        let deadline = self.engram_budget_clock().now() + ENGRAM_NAMED_ROOT_RELEASE_PERSIST_BUDGET;
        self.confirm_engram_named_root_release_until(&audit, deadline)
            .map_err(|error| engram_named_root_release_durability_error(error, &audit))?;
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if let Some(record) = inner
                .engram_named_root_release_audit
                .iter_mut()
                .find(|record| record.id == audit.id)
            {
                record.acknowledged = true;
            }
            self.commit_engram_root_locked(&mut inner)
                .map_err(|error| ApiError::internal(error.to_string()))?;
        }
        engram_root_reclamation_emit(self, &engram_named_root_release_line(&audit));
        Ok(EngramNamedRootReleaseResponse {
            released: true,
            route: audit.route.clone(),
            resolution: "applied_fresh",
            notice: format!(
                "Released named source root {} ({}) generation {} by the {} route; no producer \
                 event was created.",
                root.short_ref, root.work_id, root.generation, audit.route
            ),
        })
    }
}

async fn list_engram_named_roots(
    State(state): State<AppState>,
) -> Result<Json<Vec<EngramNamedRootStatusRow>>, ApiError> {
    let rows = run_blocking_api(move || Ok(state.engram_named_root_status())).await?;
    Ok(Json(rows))
}

async fn release_engram_named_root(
    State(state): State<AppState>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<EngramNamedRootReleaseResponse>, ApiError> {
    let Json(body) =
        body.map_err(|rejection| api_json_rejection("named-root release request", rejection))?;
    // One answer names every missing or mistyped field.
    let missing = engram_named_root_release_missing_fields(&body);
    if !missing.is_empty() {
        return Err(ApiError::from_status(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "named-root release request is missing or has invalid fields: {}",
                missing.join(", ")
            ),
        ));
    }
    let request: EngramNamedRootReleaseRequest = serde_json::from_value(body).map_err(|error| {
        ApiError::from_status(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("invalid named-root release request: {error}"),
        )
    })?;
    let response = run_blocking_api(move || {
        let requested_by = request.requested_by.clone();
        state.release_engram_named_root(requested_by.as_deref(), request)
    })
    .await?;
    Ok(Json(response))
}
