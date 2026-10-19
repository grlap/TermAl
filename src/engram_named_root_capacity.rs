// Owns the capacity rules that keep the global named-root table from filling
// with roots nobody can use: a root whose claim Engram shows closed
// (released or completed; or, while no session holds its binding, lapsed long
// enough) is retired on that canonical proof; a legacy root without a
// canonical association, or one whose claim Engram no longer counts as its
// run's, is disposed of
// automatically by a recorded rule; a naming session releases its own root
// even mid-turn; and a release under writer contention waits a bounded time
// or answers definitively that nothing was applied.
// Does not own the status view or the operator release itself
// (engram_named_root_release.rs), the automatic pass and its canonical
// proof rule (engram_named_root_reclamation.rs), or the canonical authority
// (engram_root_authority.rs).
// New module.

/// How long a lapsed claim keeps its root while its naming session is
/// present: workers routinely let a claim lapse and recover it.
const ENGRAM_ROOT_CLAIM_LAPSE_GRACE: Duration = Duration::from_secs(2 * 60 * 60);

/// How long a release waits for a running reclamation pass to finish before
/// answering that nothing was applied.
const ENGRAM_NAMED_ROOT_RELEASE_FLIGHT_WAIT: Duration = Duration::from_secs(10);

/// The authority budget a release spends under the single reclamation flight.
const ENGRAM_NAMED_ROOT_RELEASE_AUTHORITY_BUDGET: Duration = Duration::from_secs(10);

/// Engram's refusal of a named-root read whose claim is no longer its run's
/// claim: a newer claim replaced it, so the root's claim is closed.
const ENGRAM_NAMED_ROOT_CLAIM_NOT_CURRENT: &str = "the claim does not belong to the run";

/// The typed reason newer Engram builds give that same refusal.
const ENGRAM_NAMED_ROOT_CLAIM_NOT_CURRENT_REASON: &str = "claim_not_current_for_run";

/// The audit route of a release the reclamation pass makes on its own rule.
const ENGRAM_NAMED_ROOT_AUTOMATIC_ROUTE: &str = "automatic";

/// The audit route of a naming session's release of its own root.
const ENGRAM_NAMED_ROOT_OWN_ROUTE: &str = "own";

/// Why `proof` shows `root`'s claim closed, if it does: released or
/// completed; or, only while no session holds a binding for the root's claim
/// (`claim_held` false), lapsed beyond the grace (or at all once its naming
/// session is gone). A later fence is not closure: a holder that recovers its
/// own lapsed claim gets a new fence under the same claim, and a same-
/// generation re-name keeps the root's recorded fence (it is recorded, not
/// matched). An active claim that has not lapsed is live and keeps its root.
fn engram_root_claim_retirement_reason(
    root: &EngramWorkSourceRoot,
    proof: &EngramNamedRootReadResponse,
    now: chrono::DateTime<chrono::Utc>,
    naming_session_present: bool,
    claim_held: bool,
) -> Option<&'static str> {
    // As for run facts, a read older than the local naming proves nothing.
    let generation = proof
        .latest_event
        .as_ref()
        .and_then(|event| u64::try_from(event.generation).ok())?;
    if generation < root.generation {
        return None;
    }
    let claim = proof.claim.as_ref()?;
    match claim.state.as_str() {
        "released" => return Some("its claim was released"),
        "completed" => return Some("its claim was completed"),
        _ => {}
    }
    // A held binding's claim may lapse and be recovered by its holder at any
    // time; only an unheld claim's lapse retires its root.
    if claim_held || claim.state != "active" {
        return None;
    }
    let expires = chrono::DateTime::parse_from_rfc3339(claim.expires_at.as_deref()?)
        .ok()?
        .with_timezone(&chrono::Utc);
    if expires >= now {
        return None;
    }
    if !naming_session_present {
        return Some("its claim lapsed and its naming session is gone");
    }
    let lapsed = (now - expires).to_std().unwrap_or_default();
    (lapsed > ENGRAM_ROOT_CLAIM_LAPSE_GRACE).then_some("its claim lapsed more than two hours ago")
}

/// Retires the claim journal of `root`, which a capacity rule just removed,
/// so compaction can drop that history once nothing else references it: as
/// the canonical pass does when Engram shows a binding definitively ended.
/// A closed claim's journal retires as unbound; one Engram refused as no
/// longer its run's claim retires as displaced by that run's newer claim.
/// Left alone while another retained root names the same claim, or while the
/// journal needs reconciliation (a pending naming event, an unsettled read).
fn engram_named_root_retire_claim_journal_locked(
    inner: &mut StateInner,
    root: &EngramWorkSourceRoot,
    retirement: EngramRootRetirement,
) {
    if inner
        .engram_work_source_roots
        .iter()
        .any(|other| other.store == root.store && other.claim_id == root.claim_id)
    {
        return;
    }
    if let Some(journal) = inner
        .engram_named_root_journal
        .iter_mut()
        .find(|journal| journal.store == root.store && journal.claim_id == root.claim_id)
        && !journal.obsolete
        && !journal.requires_reconciliation()
    {
        journal.retire(retirement);
    }
}

/// Whether a refused named-root read says the root's claim is no longer its
/// run's claim: by its typed reason, or by the message older builds send.
fn engram_named_root_read_refusal_closes_claim(message: &str) -> bool {
    message.contains(ENGRAM_NAMED_ROOT_CLAIM_NOT_CURRENT_REASON)
        || message.contains(ENGRAM_NAMED_ROOT_CLAIM_NOT_CURRENT)
}

/// Whether `session_id` is still a session of this host, or one kept aside
/// at restore that may load again.
fn engram_named_root_session_present(inner: &StateInner, session_id: &str) -> bool {
    inner.find_session_index(session_id).is_some()
        || inner.quarantined_persisted_session_ids.contains(session_id)
}

/// Whether no automatic disposition may touch `root` now, whatever its path
/// or item: a session holds a binding for its claim, its naming session was
/// kept aside at restore, or a naming event of its claim is pending. Checked
/// before any tracker read, so a blocked root costs none.
fn engram_named_root_disposition_blocked(inner: &StateInner, root: &EngramWorkSourceRoot) -> bool {
    // A naming session kept aside at restore may load again, and its binding
    // and activity cannot be read meanwhile: the operator route refuses it
    // for the same reason.
    engram_named_root_claim_holder(inner, root).is_some()
        || inner.quarantined_persisted_session_ids.contains(&root.named_by_session)
        || engram_root_journal(&inner.engram_named_root_journal, &root.store, &root.claim_id)
            .is_some_and(|journal| journal.pending.is_some())
}

/// Why a retained root may be disposed of automatically, if it may: a
/// legacy root (no canonical run association) whose path is gone, whose item
/// is completed or cancelled, or whose naming session is gone with no
/// session holding its claim; or any root whose claim Engram refused as no
/// longer its run's. Never a root a session holds a binding for, nor one with
/// a pending naming event.
fn engram_named_root_automatic_disposition(
    inner: &StateInner,
    root: &EngramWorkSourceRoot,
    path_exists: bool,
    item_lifecycle: Option<&str>,
    claim_refused: bool,
) -> Option<&'static str> {
    if engram_named_root_disposition_blocked(inner, root) {
        return None;
    }
    if claim_refused {
        return Some("Engram no longer counts its claim as its run's claim");
    }
    if engram_named_root_association(inner, root).is_some() {
        return None;
    }
    if !path_exists {
        return Some("its root path no longer exists");
    }
    match item_lifecycle {
        Some("completed") => return Some("its item is completed"),
        Some("cancelled") => return Some("its item is cancelled"),
        _ => {}
    }
    (!engram_named_root_session_present(inner, &root.named_by_session))
        .then_some("its naming session is gone and no session holds its claim")
}

/// The status view's account of what the disposition pass does with `root`,
/// which has no canonical run association. Lock-only.
fn engram_named_root_disposition_text(
    inner: &StateInner,
    root: &EngramWorkSourceRoot,
    now: std::time::Instant,
) -> String {
    if engram_named_root_disposition_blocked(inner, root) {
        return "skipped: no retained canonical run association; kept while a session holds its \
                claim, its naming session is kept aside at restore, or a naming event is pending"
            .to_owned();
    }
    if let Some((_, until)) = inner
        .engram_root_reclamation
        .disposition_cooldowns
        .iter()
        .find(|(cooling, until)| cooling == root && now < *until)
    {
        return format!(
            "disposition: kept for now (no rule applied, or its tracker read was deferred); \
             examined again in {} s",
            until.saturating_duration_since(now).as_secs()
        );
    }
    "disposition: no retained canonical run association; the automatic pass releases it when its \
     path no longer exists, its item is completed or cancelled, or its naming session is gone"
        .to_owned()
}

/// How long the disposition pass leaves a root it examined and kept.
const ENGRAM_NAMED_ROOT_DISPOSITION_COOLDOWN: Duration = Duration::from_secs(5 * 60);

/// How long a reclamation pass may hold the single flight before its
/// disposition stops, counted from the pass's start (its canonical reads
/// included): less than a release waits for that flight, so a release that
/// arrives during a pass still applies.
const ENGRAM_NAMED_ROOT_DISPOSITION_BUDGET: Duration = Duration::from_secs(8);

/// The time left in the pass's budget that a tracker read needs to start:
/// the bound on one production Work read (`ENGRAM_WORK_BINDING_COMMAND_TIMEOUT`
/// outside tests), so a read never carries the pass past its budget.
const ENGRAM_NAMED_ROOT_DISPOSITION_READ_RESERVE: Duration = Duration::from_secs(6);

/// How many item tracker reads one disposition pass may make.
const ENGRAM_NAMED_ROOT_DISPOSITION_READ_LIMIT: usize = 8;

/// A release record the writer has not acknowledged although its selection
/// is gone, which some route other than the canonical one reserves a slot
/// for: the disposition pass confirms it again.
fn engram_named_root_release_pending(
    inner: &StateInner,
    record: &EngramNamedRootReleaseAudit,
) -> bool {
    !record.acknowledged
        && record.route != EngramNamedRootReleaseRoute::Canonical.label()
        && !inner.engram_work_source_roots.contains(&record.selection)
}

/// Whether a disposition pass has work now: a pending release to confirm, or
/// a legacy or claim-refused root it has not examined lately. Lock-only.
fn engram_named_root_disposition_due_locked(inner: &StateInner, now: std::time::Instant) -> bool {
    let schedule = &inner.engram_root_reclamation;
    inner
        .engram_named_root_release_audit
        .iter()
        .any(|record| engram_named_root_release_pending(inner, record))
        || inner.engram_work_source_roots.iter().any(|root| {
            (engram_named_root_association(inner, root).is_none()
                || engram_named_root_claim_refused(inner, root))
                && !engram_named_root_disposition_blocked(inner, root)
                && !schedule
                    .disposition_cooldowns
                    .iter()
                    .any(|(cooling, until)| cooling == root && *until > now)
        })
}

// Test-only: replaces the item tracker read, so a test decides the answer
// without a process.
#[cfg(test)]
thread_local! {
    static TEST_ENGRAM_NAMED_ROOT_ITEM_LIFECYCLE: std::cell::RefCell<
        Option<Box<dyn Fn(&EngramWorkSourceRoot) -> Option<String>>>,
    > = const { std::cell::RefCell::new(None) };
}

/// Whether a canonical read of `root`'s claim was refused as no longer its
/// run's claim since the last restart.
fn engram_named_root_claim_refused(inner: &StateInner, root: &EngramWorkSourceRoot) -> bool {
    inner
        .engram_root_reclamation
        .refused_claims
        .iter()
        .any(|(store, claim_id)| *store == root.store && *claim_id == root.claim_id)
}

/// Records that a canonical read of `claim_id` in `store` was refused as no
/// longer its run's claim, for the disposition pass.
fn engram_named_root_record_refused_claim_locked(
    inner: &mut StateInner,
    store: &EngramAuthorityStoreKey,
    claim_id: &str,
) {
    let refused = &mut inner.engram_root_reclamation.refused_claims;
    if !refused.iter().any(|(known, claim)| known == store && claim == claim_id) {
        refused.push((store.clone(), claim_id.to_owned()));
    }
}

/// A naming session's release of its own root. The caller is the session the
/// route names; the MCP bridge always names its own serving session.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngramNamedRootOwnReleaseRequest {
    work: String,
    generation: u64,
    reason: String,
}

/// Every required field of a release body that is missing or of the wrong
/// type, in body order, so one answer names them all.
fn engram_named_root_release_missing_fields(body: &Value) -> Vec<&'static str> {
    const TEXT: [&str; 6] = ["projectId", "databasePath", "workId", "claimId", "root", "reason"];
    let mut missing = Vec::new();
    for field in ["projectId", "databasePath", "workId", "claimId", "generation", "root", "reason"] {
        let value = body.get(field);
        let valid = if TEXT.contains(&field) {
            value.is_some_and(Value::is_string)
        } else {
            value.is_some_and(Value::is_u64)
        };
        if !valid {
            missing.push(field);
        }
    }
    missing
}

impl AppState {
    /// Takes the single reclamation flight for one release, waiting a bounded
    /// time for a running pass to finish; a pass that outlasts the wait means
    /// nothing was applied.
    fn acquire_engram_named_root_release_flight(
        &self,
    ) -> Result<EngramRootReclamationFlight, ApiError> {
        let clock = self.engram_budget_clock();
        let deadline = clock.now() + ENGRAM_NAMED_ROOT_RELEASE_FLIGHT_WAIT;
        // The same bound in wall time, so a clock nobody advances still ends.
        let wall_deadline = std::time::Instant::now() + ENGRAM_NAMED_ROOT_RELEASE_FLIGHT_WAIT;
        let id = format!("named-root-release-{}", Uuid::new_v4());
        loop {
            {
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                if inner.engram_root_reclamation.flight.is_none() {
                    inner.engram_root_reclamation.flight = Some(id.clone());
                    return Ok(EngramRootReclamationFlight { state: self.clone(), id });
                }
            }
            if clock.now() >= deadline || std::time::Instant::now() >= wall_deadline {
                return Err(ApiError::conflict(
                    "a named-root reclamation pass or another release is still running; nothing \
                     was applied. Retry the same request shortly",
                ));
            }
            #[cfg(test)]
            self.test_engram_authority_ack_boundary("named_root_release_waiting_for_flight");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Releases `session_id`'s own retained root of `request.work` at
    /// `request.generation`, even while that session is in a turn.
    fn release_own_engram_named_root(
        &self,
        session_id: &str,
        request: EngramNamedRootOwnReleaseRequest,
    ) -> Result<EngramNamedRootReleaseResponse, ApiError> {
        let work = request.work.trim();
        let full = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if inner.find_session_index(session_id).is_none() {
                return Err(ApiError::not_found("session not found"));
            }
            let names = |root: &EngramWorkSourceRoot| {
                (root.work_id == work || root.short_ref == work)
                    && root.generation == request.generation
            };
            // The caller's own retained root first (the same work or short
            // reference may be retained in another store, or by another
            // session); then any match, so a wrong caller learns whose it is;
            // then an earlier release of one this session made whose
            // confirmation is still pending.
            let selection = inner
                .engram_work_source_roots
                .iter()
                .find(|root| names(root) && root.named_by_session == session_id)
                .or_else(|| inner.engram_work_source_roots.iter().find(|root| names(root)))
                .cloned()
                .or_else(|| {
                    inner
                        .engram_named_root_release_audit
                        .iter()
                        .rev()
                        .find(|record| {
                            !record.acknowledged
                                && names(&record.selection)
                                && record.requested_by.as_deref() == Some(session_id)
                        })
                        .map(|record| record.selection.clone())
                })
                .ok_or_else(|| {
                    ApiError::conflict(format!(
                        "no retained named root of {work} at generation {}. Nothing was released",
                        request.generation
                    ))
                })?;
            if selection.named_by_session != session_id {
                return Err(ApiError::conflict(format!(
                    "only its naming session may release a root as its own; this root's naming \
                     session is {}. Nothing was released",
                    selection.named_by_session
                )));
            }
            EngramNamedRootReleaseRequest {
                project_id: selection.store.project_id.clone(),
                database_path: selection.store.database_path.to_string_lossy().into_owned(),
                work_id: selection.work_id.clone(),
                claim_id: selection.claim_id.clone(),
                generation: selection.generation,
                root: selection.root.clone(),
                reason: request.reason.clone(),
                requested_by: Some(session_id.to_owned()),
            }
        };
        self.release_engram_named_root_as(Some(session_id), Some(session_id), full)
    }

    /// The tracker lifecycle of `root`'s item (open, completed, cancelled),
    /// read off the lock through the Work host reader of the project whose
    /// established store is `root`'s; `None` whenever it cannot be read, so
    /// the root is kept.
    fn read_engram_named_root_item_lifecycle(&self, root: &EngramWorkSourceRoot) -> Option<String> {
        #[cfg(test)]
        if let Some(lifecycle) = TEST_ENGRAM_NAMED_ROOT_ITEM_LIFECYCLE
            .with(|hook| hook.borrow().as_ref().map(|read| read(root)))
        {
            return lifecycle;
        }
        let target = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner.projects.iter().find_map(|project| {
                work_host_reader(&inner, project)
                    .ok()
                    .filter(|target| target.store == root.store)
            })?
        };
        validate_work_read_target(&target).ok()?;
        let value = run_work_read_command(
            &target.connection,
            &["show".to_owned(), root.work_id.clone(), "--json".to_owned()],
            &self.engram_host_launch_workdir(),
        )
        .ok()?;
        value["status"]["work"]["lifecycle"].as_str().map(str::to_owned)
    }

    /// Confirms again the releases the writer has not acknowledged although
    /// their selection is gone, then disposes of the retained roots
    /// `engram_named_root_automatic_disposition` allows, each with its own
    /// audit record. Bounded in time and in tracker reads; a root it examines
    /// and keeps is left alone for a cooldown. `item_lifecycle` answers an
    /// item's tracker lifecycle when it can. `flight_started` is when the
    /// flight's pass began, which the budget runs from.
    fn run_engram_named_root_disposition_pass(
        &self,
        flight_started: std::time::Instant,
        item_lifecycle: &dyn Fn(&EngramWorkSourceRoot) -> Option<String>,
    ) {
        let clock = self.engram_budget_clock();
        let deadline = flight_started + ENGRAM_NAMED_ROOT_DISPOSITION_BUDGET;
        let pending = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .engram_named_root_release_audit
                .iter()
                .filter(|record| engram_named_root_release_pending(&inner, record))
                .cloned()
                .collect::<Vec<_>>()
        };
        for record in pending {
            if clock.now() >= deadline {
                return;
            }
            self.acknowledge_engram_named_root_release(&record, deadline);
        }
        let now = clock.now();
        let candidates = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .engram_work_source_roots
                .iter()
                .filter(|root| {
                    !inner
                        .engram_root_reclamation
                        .disposition_cooldowns
                        .iter()
                        .any(|(cooling, until)| cooling == *root && *until > now)
                })
                // Blocked roots go before any path check or tracker read.
                .filter(|root| !engram_named_root_disposition_blocked(&inner, root))
                .filter_map(|root| {
                    let refused = engram_named_root_claim_refused(&inner, root);
                    let legacy = engram_named_root_association(&inner, root).is_none();
                    (refused || legacy).then(|| {
                        let present = engram_named_root_session_present(&inner, &root.named_by_session);
                        (root.clone(), refused, present)
                    })
                })
                .collect::<Vec<_>>()
        };
        let mut reads = 0;
        for (root, claim_refused, naming_present) in candidates {
            if clock.now() >= deadline {
                return;
            }
            // Off the lock: the path check, and the tracker read only when
            // nothing cheaper decides.
            let path_exists = FsPath::new(&root.root).exists();
            let needs_read = !claim_refused && path_exists && naming_present;
            let read_skipped = needs_read
                && (reads >= ENGRAM_NAMED_ROOT_DISPOSITION_READ_LIMIT
                    || clock.now() + ENGRAM_NAMED_ROOT_DISPOSITION_READ_RESERVE > deadline);
            let lifecycle = if needs_read && !read_skipped {
                reads += 1;
                item_lifecycle(&root)
            } else {
                None
            };
            let audit = {
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                let Some(index) =
                    inner.engram_work_source_roots.iter().position(|current| *current == root)
                else {
                    continue;
                };
                let Some(reason) = engram_named_root_automatic_disposition(
                    &inner,
                    &root,
                    path_exists,
                    lifecycle.as_deref(),
                    claim_refused,
                ) else {
                    // A root the read limit skipped comes back soon, so later
                    // passes reach every root; one examined in full waits longer.
                    let until = clock.now()
                        + if read_skipped {
                            ENGRAM_ROOT_RECLAMATION_COOLDOWN
                        } else {
                            ENGRAM_NAMED_ROOT_DISPOSITION_COOLDOWN
                        };
                    let cooldowns = &mut inner.engram_root_reclamation.disposition_cooldowns;
                    cooldowns.retain(|(cooling, until)| *cooling != root && *until > now);
                    cooldowns.push((root.clone(), until));
                    continue;
                };
                let lifecycle_text = engram_named_root_lifecycle_text(&inner, &root);
                // Removed in memory; the unacknowledged record reserves its
                // slot until the writer commits both.
                inner.engram_work_source_roots.remove(index);
                let mut record = engram_named_root_release_record(
                    &root,
                    EngramNamedRootReleaseRoute::Operator,
                    reason,
                    Some("termal"),
                    &lifecycle_text,
                );
                record.route = ENGRAM_NAMED_ROOT_AUTOMATIC_ROUTE.to_owned();
                engram_named_root_record_release_locked(&mut inner, record.clone());
                inner
                    .engram_root_reclamation
                    .refused_claims
                    .retain(|(store, claim)| !(*store == root.store && *claim == root.claim_id));
                if claim_refused {
                    engram_named_root_retire_claim_journal_locked(
                        &mut inner,
                        &root,
                        EngramRootRetirement::Displaced,
                    );
                }
                let committed = self.commit_engram_root_locked(&mut inner);
                drop(inner);
                if let Err(error) = committed {
                    // The record stays pending; a later pass confirms it.
                    engram_root_reclamation_emit(
                        self,
                        &format!(
                            "engram> named-root automatic release of {} generation {} not \
                             committed yet: {error}",
                            root.short_ref, root.generation
                        ),
                    );
                    continue;
                }
                record
            };
            self.acknowledge_engram_named_root_release(&audit, deadline);
        }
    }

    /// Confirms the release `audit` with the writer, by `deadline` at the
    /// latest, and acknowledges it, freeing its reserved slot. A failure is
    /// logged and the record stays pending for a later pass.
    fn acknowledge_engram_named_root_release(
        &self,
        audit: &EngramNamedRootReleaseAudit,
        deadline: std::time::Instant,
    ) {
        let deadline = deadline
            .min(self.engram_budget_clock().now() + ENGRAM_NAMED_ROOT_RELEASE_PERSIST_BUDGET);
        if let Err(error) = self.confirm_engram_named_root_release_until(audit, deadline) {
            engram_root_reclamation_emit(
                self,
                &format!(
                    "engram> named-root release of {} generation {} still unconfirmed; its slot \
                     stays reserved: {}",
                    audit.selection.short_ref, audit.selection.generation, error.message
                ),
            );
            return;
        }
        let committed = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if let Some(record) = inner
                .engram_named_root_release_audit
                .iter_mut()
                .find(|record| record.id == audit.id)
            {
                record.acknowledged = true;
            }
            self.commit_engram_root_locked(&mut inner)
        };
        if let Err(error) = committed {
            engram_root_reclamation_emit(
                self,
                &format!("engram> named-root release acknowledgement not committed: {error}"),
            );
        }
        engram_root_reclamation_emit(self, &engram_named_root_release_line(audit));
    }
}

async fn release_own_engram_named_root_route(
    State(state): State<AppState>,
    AxumPath(session_id): AxumPath<String>,
    request: Result<Json<EngramNamedRootOwnReleaseRequest>, JsonRejection>,
) -> Result<Json<EngramNamedRootReleaseResponse>, ApiError> {
    let Json(request) = request
        .map_err(|rejection| api_json_rejection("own named-root release request", rejection))?;
    let response =
        run_blocking_api(move || state.release_own_engram_named_root(&session_id, request))
            .await?;
    Ok(Json(response))
}
