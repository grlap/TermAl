// Owns acknowledged host-side retirement of exact retained named roots.
// Uses the existing authority owner and canonical reader; does not create
// producer events, infer claim end from disk state, or adopt session focus.

const ENGRAM_ROOT_RECLAMATION_READ_LIMIT: usize = 8;
const ENGRAM_ROOT_RECLAMATION_BUDGET: Duration = Duration::from_secs(2);
const ENGRAM_ROOT_RECLAMATION_COOLDOWN: Duration = Duration::from_secs(30);

/// Runtime scheduling only. Durable publication obligations stay in the
/// existing work history; restarting cannot lose a pending retirement.
#[derive(Default)]
struct EngramRootReclamationSchedule {
    flight: Option<String>,
    last_store: Option<EngramAuthorityStoreKey>,
    next_due: Option<std::time::Instant>,
    cooldowns: Vec<(EngramWorkSourceRoot, std::time::Instant)>,
    /// Claims whose canonical read Engram refused as no longer their run's
    /// claim (engram_named_root_capacity.rs): runtime only, so a restart
    /// reads them again before any disposition.
    refused_claims: Vec<(EngramAuthorityStoreKey, String)>,
    /// Roots the disposition pass examined and kept, until when it leaves
    /// them alone (engram_named_root_capacity.rs).
    disposition_cooldowns: Vec<(EngramWorkSourceRoot, std::time::Instant)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum EngramRootReclamationRoute {
    Unfocused,
    Current(String),
    OwnClear(String),
}

#[derive(Clone, Debug)]
struct EngramRootReclamationTask {
    root: EngramWorkSourceRoot,
    route: EngramRootReclamationRoute,
}

struct EngramRootReclamationFlight {
    state: AppState,
    id: String,
}

impl Drop for EngramRootReclamationFlight {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.state.inner.lock()
            && inner.engram_root_reclamation.flight.as_ref() == Some(&self.id)
        {
            inner.engram_root_reclamation.flight = None;
        }
    }
}

// These eligibility and owner questions read the narrow reader view
// (engram_session_authority.rs), never a full binding target: a target also
// captures a root read fence cloning every root, journal and project store,
// and these run under the global lock for every root and session.
fn engram_root_reclamation_session_is_reader(
    inner: &StateInner,
    session_id: &str,
    store: &EngramAuthorityStoreKey,
) -> bool {
    engram_session_reader_view_locked(inner, session_id)
        .is_some_and(|view| view.routing_token.is_some() && view.store == Some(store))
}

fn engram_root_reclamation_reader_exists(inner: &StateInner, store: &EngramAuthorityStoreKey) -> bool {
    inner.sessions.iter().any(|record|
        engram_root_reclamation_session_is_reader(inner, &record.session.id, store))
}

fn engram_named_root_current_owner(inner: &StateInner, root: &EngramWorkSourceRoot) -> Option<String> {
    let binding = engram_root_journal(&inner.engram_named_root_journal, &root.store, &root.claim_id)?
        .read_binding.as_ref()?;
    // A retained local run view is not current store authority. Use this same
    // predicate when planning and when revalidating an off-lock read.
    inner.sessions.iter().find(|record| record.engram.work_binding.as_ref()
        .is_some_and(|current| engram_same_recovery_run(current, binding))
        && engram_root_reclamation_session_is_reader(inner, &record.session.id, &root.store))
        .map(|record| record.session.id.clone())
}

fn engram_named_root_current_binding(inner: &StateInner, root: &EngramWorkSourceRoot) -> bool {
    engram_named_root_current_owner(inner, root).is_some()
}

fn engram_root_reclamation_idle_owner(inner: &StateInner, session_id: &str) -> bool {
    inner.find_session_index(session_id).is_some_and(|index| {
        let record = &inner.sessions[index];
        record.engram.active_grant_id.is_none() && record.engram.uncertain_grant_id.is_none()
            && !record.engram.checkpoint_in_progress
            && matches!(record.session.status, SessionStatus::Idle | SessionStatus::Error)
            && record.unmediated_claude_turn.is_none()
            && engram_session_reader_view_locked(inner, session_id)
                .is_some_and(|view| view.routing_token.is_some())
    })
}

fn engram_root_reclamation_route_current(
    inner: &StateInner,
    root: &EngramWorkSourceRoot,
    route: &EngramRootReclamationRoute,
) -> bool {
    match route {
        EngramRootReclamationRoute::Unfocused => !engram_named_root_current_binding(inner, root),
        EngramRootReclamationRoute::Current(session_id) =>
            engram_named_root_current_owner(inner, root).as_ref() == Some(session_id)
                && engram_root_reclamation_idle_owner(inner, session_id),
        EngramRootReclamationRoute::OwnClear(session_id) =>
            &root.named_by_session == session_id
                && engram_session_reader_view_locked(inner, session_id)
                    .is_some_and(|view| view.store == Some(&root.store)),
    }
}

/// Whether an evaluator delegation of `work_id` in `store` is open (queued,
/// running or held) and has not submitted: its evaluation still has to pass
/// the root check. Its own budget ends it, so it never pins a root for good.
fn engram_work_has_open_evaluation_locked(
    inner: &StateInner,
    store: &EngramAuthorityStoreKey,
    work_id: &str,
) -> bool {
    inner.delegations.iter().any(|delegation| {
        matches!(
            delegation.status,
            DelegationStatus::Queued | DelegationStatus::Running | DelegationStatus::Held
        ) && delegation.acceptance_evaluation.as_ref().is_some_and(|target| {
            target.store.as_ref() == Some(store)
                && matches!(target.submission, AcceptanceEvaluationSubmission::None)
                && (target
                    .naming_history
                    .as_ref()
                    .is_some_and(|token| token.work_id == work_id)
                    || target
                        .source_root
                        .as_ref()
                        .is_some_and(|root| root.work_id == work_id))
        })
    })
}

fn engram_root_reclamation_plan_locked(
    inner: &StateInner,
    now: std::time::Instant,
) -> Vec<EngramRootReclamationTask> {
    let mut roots = inner.engram_work_source_roots.clone();
    roots.extend(inner.engram_work_naming_history.iter().flat_map(|history| &history.retirements)
        .filter(|notice| !notice.published && !inner.engram_work_source_roots.contains(&notice.selection))
        .map(|notice| notice.selection.clone()));
    roots.sort_by(|a, b| a.named_at.cmp(&b.named_at).then(a.generation.cmp(&b.generation)));
    let mut stores: BTreeMap<EngramAuthorityStoreKey, std::collections::VecDeque<EngramRootReclamationTask>> = BTreeMap::new();
    for root in roots {
        if !engram_root_reclamation_reader_exists(inner, &root.store)
            || inner.engram_root_reclamation.cooldowns.iter().any(|(selection, until)|
                selection == &root && now < *until)
        { continue; }
        let Some(journal) = engram_root_journal(&inner.engram_named_root_journal, &root.store, &root.claim_id)
        else { continue; };
        if journal.pending.is_some() || journal.read_binding.as_ref().is_none_or(|binding|
            binding.work_id != root.work_id || binding.claim_id != root.claim_id)
        { continue; }
        let route = match engram_named_root_current_owner(inner, &root) {
            // A pass over a root whose work is being evaluated only re-learns
            // the same binding, and its unpublished window would refuse the
            // evaluator's submission as retryable: leave the root alone
            // until the evaluation is submitted or ends. Retiring an ended
            // claim's root (the Unfocused route) is never held back.
            Some(_) if engram_work_has_open_evaluation_locked(inner, &root.store, &root.work_id) => {
                continue;
            }
            Some(session_id) if engram_root_reclamation_idle_owner(inner, &session_id) => {
                EngramRootReclamationRoute::Current(session_id)
            }
            Some(_) => continue,
            None => EngramRootReclamationRoute::Unfocused,
        };
        stores.entry(root.store.clone()).or_default().push_back(EngramRootReclamationTask { root, route });
    }
    let mut order = stores.keys().cloned().collect::<Vec<_>>();
    if let Some(last) = &inner.engram_root_reclamation.last_store && !order.is_empty() {
        let start = order.partition_point(|store| store <= last) % order.len();
        order.rotate_left(start);
    }
    let mut plan = Vec::new();
    while plan.len() < ENGRAM_ROOT_RECLAMATION_READ_LIMIT {
        let mut advanced = false;
        for store in &order {
            if let Some(task) = stores.get_mut(store).and_then(|roots| roots.pop_front()) {
                plan.push(task);
                advanced = true;
            }
            if plan.len() == ENGRAM_ROOT_RECLAMATION_READ_LIMIT { break; }
        }
        if !advanced { break; }
    }
    plan
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramNamedRootRetirementNotice {
    id: String,
    selection: EngramWorkSourceRoot,
    binding: EngramControlWorkBinding,
    read: EngramNamedRootReadResponse,
    line: String,
    #[serde(default)]
    published: bool,
    #[serde(default)]
    delivered: bool,
    /// The authority transition that removed the selection: the only owner
    /// a removal can happen under. An operator's canonical release credits a
    /// retirement to itself only when this is its own transition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner: Option<String>,
}

/// The retirement notice of exactly `root` made under the authority
/// transition `owner`, if one exists.
fn engram_named_root_retired_by<'a>(
    inner: &'a StateInner,
    root: &EngramWorkSourceRoot,
    owner: &str,
) -> Option<&'a EngramNamedRootRetirementNotice> {
    inner
        .engram_work_naming_history
        .iter()
        .flat_map(|history| &history.retirements)
        .find(|notice| notice.selection == *root && notice.owner.as_deref() == Some(owner))
}

fn engram_named_root_capacity(inner: &StateInner) -> usize {
    inner.engram_work_source_roots.len()
        + inner.engram_work_naming_history.iter().flat_map(|history| &history.retirements)
            .filter(|notice| !notice.published
                && !inner.engram_work_source_roots.contains(&notice.selection))
            .count()
        // An operator release reserves its slot until the writer commits it.
        + engram_named_root_unacknowledged_releases(inner)
}

/// Marks the work's retirement notices published and returns their stderr
/// lines; the caller writes them with `engram_root_reclamation_emit` once it
/// has released the global guard.
#[must_use]
fn engram_publish_root_retirements_locked(
    inner: &mut StateInner,
    store: &EngramAuthorityStoreKey,
    work: &str,
) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(history) = inner.engram_work_naming_history.iter_mut()
        .find(|history| &history.store == store && history.work_id == work)
    {
        for notice in &mut history.retirements {
            if !notice.published {
                notice.published = true;
                lines.push(format!("engram> {}", notice.line));
            }
        }
    }
    lines
}

// Test-only record of the reclamation lines written to stderr, so a witness
// can show they were written, and written without the global lock.
#[cfg(test)]
thread_local! {
    static TEST_ENGRAM_RECLAMATION_EMITTED: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn take_engram_root_reclamation_emitted() -> Vec<String> {
    TEST_ENGRAM_RECLAMATION_EMITTED.with(|lines| std::mem::take(&mut *lines.borrow_mut()))
}

/// Writes one reclamation line to stderr. Never under the global state lock:
/// a slow or blocked stderr must not extend the guard every session waits on.
fn engram_root_reclamation_emit(state: &AppState, line: &str) {
    #[cfg(test)]
    {
        assert!(
            state.inner.is_not_held_by_current_thread_for_test(),
            "named-root reclamation wrote stderr under the global state lock: {line}"
        );
        TEST_ENGRAM_RECLAMATION_EMITTED.with(|lines| lines.borrow_mut().push(line.to_owned()));
    }
    #[cfg(not(test))]
    let _ = state;
    eprintln!("{line}");
}

fn engram_root_retirement_reason(
    root: &EngramWorkSourceRoot,
    proof: &EngramNamedRootReadResponse,
) -> Option<&'static str> {
    // Even terminal facts must not erase a local generation ahead of the
    // canonical event. A missing event is not proof about a named selection.
    let generation = proof.latest_event.as_ref()
        .and_then(|event| u64::try_from(event.generation).ok())?;
    if generation < root.generation {
        return None;
    }
    match proof.run.state.as_str() {
        "completed" => Some("its run completed"),
        "cancelled" => Some("its run was cancelled"),
        _ if matches!(proof.named_root, EngramNamedRootState::UnboundByRelease { .. }) => Some("its root was released"),
        _ if proof.definitively_unbound() => Some("its canonical binding ended"),
        _ if engram_named_root_read_obsoletes(root, Some(&proof.named_root), generation) => {
            Some("its canonical named-root binding superseded this selection")
        }
        _ => None,
    }
}

fn engram_retire_exact_root_locked(
    inner: &mut StateInner,
    root: &EngramWorkSourceRoot,
    binding: &EngramControlWorkBinding,
    proof: &EngramNamedRootReadResponse,
) {
    // A run fact first; otherwise the read claim's own state
    // (engram_named_root_capacity.rs) may show the claim closed, judging a
    // lapse only while no session holds a binding for it.
    let naming_session_present = engram_named_root_session_present(inner, &root.named_by_session);
    // A naming session kept aside at restore may still hold the claim, and
    // its binding cannot be read meanwhile: it counts as holding it, as the
    // disposition and operator routes treat it.
    let claim_held = engram_named_root_claim_holder(inner, root).is_some()
        || inner.quarantined_persisted_session_ids.contains(&root.named_by_session);
    let run_reason = engram_root_retirement_reason(root, proof);
    let claim_reason = run_reason.is_none().then(|| {
        engram_root_claim_retirement_reason(
            root, proof, chrono::Utc::now(), naming_session_present, claim_held,
        )
    }).flatten();
    let Some(reason) = run_reason.or(claim_reason) else { return; };
    let Some(index) = inner.engram_work_source_roots.iter().position(|current| current == root)
    else { return; };
    let Some(history) = inner.engram_work_naming_history.iter_mut()
        .find(|history| history.store == root.store && history.work_id == root.work_id)
    else { return; };
    // Keep exact proof/history with the removal, before the complete-image
    // fence. No success, capacity release or notice delivery precedes its ACK.
    let owner = history.transition.as_ref().map(|transition| transition.id.clone());
    history.retirements.push(EngramNamedRootRetirementNotice {
        id: Uuid::new_v4().to_string(), selection: root.clone(), binding: binding.clone(),
        read: proof.clone(),
        line: format!(
            "[TermAl] Retired named source root {} / {} ({}) at {} (generation {}): {}; canonical read cut {} / {}.",
            format!("{} / {}", root.store.database_path.display(), root.store.project_id),
            root.short_ref, root.work_id, root.root, root.generation,
            reason, proof.read_cut.feed.id, proof.read_cut.position),
        published: false, delivered: false, owner,
    });
    inner.engram_work_source_roots.remove(index);
    // Engram may still show this binding Bound (a lapsed claim's open run),
    // which would keep its journal forever; the claim itself is closed.
    if claim_reason.is_some() {
        engram_named_root_retire_claim_journal_locked(inner, root, EngramRootRetirement::Unbound);
    }
}

fn acknowledge_engram_root_retirements_locked(
    inner: &mut StateInner,
    session_id: &str,
    delivered: &[EngramSourceRootNotice],
) {
    for notice in inner.engram_work_naming_history.iter_mut()
        .flat_map(|history| &mut history.retirements)
    {
        if notice.published && notice.selection.named_by_session == session_id
            && delivered.iter().any(|item| item.id == notice.id)
        {
            notice.delivered = true;
        }
    }
}

impl AppState {
    /// Called by the unconditional test-run index tick. A single global flight
    /// also implies single-flight per store. Control reads stay off its thread.
    fn schedule_engram_root_reclamation(&self, force: bool) -> &'static str {
        let now = self.engram_budget_clock().now();
        let (id, plan) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if inner.engram_root_reclamation.flight.is_some() {
                return "a bounded reclamation pass is already running";
            }
            if !force && inner.engram_root_reclamation.next_due.is_some_and(|due| now < due) {
                return "the recurring pass is not due yet";
            }
            let plan = engram_root_reclamation_plan_locked(&inner, now);
            // A disposition that is due runs in the same flight even when no
            // canonical read is planned (engram_named_root_capacity.rs).
            if plan.is_empty() && !engram_named_root_disposition_due_locked(&inner, now) {
                inner.engram_root_reclamation.next_due = Some(now + ENGRAM_ROOT_RECLAMATION_BUDGET);
                return "no eligible reclamation pass was scheduled";
            }
            let id = Uuid::new_v4().to_string();
            inner.engram_root_reclamation.flight = Some(id.clone());
            inner.engram_root_reclamation.next_due = Some(now + ENGRAM_ROOT_RECLAMATION_BUDGET);
            (id, plan)
        };
        let state = self.clone();
        let flight = EngramRootReclamationFlight { state: self.clone(), id };
        match std::thread::Builder::new().name("engram-root-reclamation".to_owned())
            .spawn(move || {
                let flight = flight;
                state.run_engram_root_reclamation_pass(plan);
                drop(flight);
                #[cfg(test)]
                state.test_engram_authority_ack_boundary("after_reclamation_pass");
            })
        {
            Ok(_) => "a bounded reclamation pass was scheduled",
            Err(error) => {
                // Dropping the unstarted closure also drops its flight guard.
                eprintln!("engram> named-root reclamation could not start: {error}");
                "no reclamation pass was scheduled because its worker could not start"
            }
        }
    }

    fn run_engram_root_reclamation_pass(&self, plan: Vec<EngramRootReclamationTask>) {
        let clock = self.engram_budget_clock();
        let started = clock.now();
        let deadline = started + ENGRAM_ROOT_RECLAMATION_BUDGET;
        let mut attempted = 0;
        for task in plan.into_iter().take(ENGRAM_ROOT_RECLAMATION_READ_LIMIT) {
            if clock.now() >= deadline { break; }
            self.inner.lock().expect("state mutex poisoned").engram_root_reclamation.last_store = Some(task.root.store.clone());
            attempted += 1;
            // Current bindings go to the existing canonical recovery owner,
            // never a second directory-probe or provider turn.
            let outcome = self.recover_one_engram_root_until(&task.root, &task.route, deadline);
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let still_retained = inner.engram_work_source_roots.contains(&task.root)
                || inner.engram_work_naming_history.iter().flat_map(|history| &history.retirements)
                    .any(|notice| notice.selection == task.root && !notice.published);
            inner.engram_root_reclamation.cooldowns.retain(|(root, until)|
                root != &task.root && *until > clock.now());
            if still_retained {
                inner.engram_root_reclamation.cooldowns.push((task.root.clone(), clock.now() + ENGRAM_ROOT_RECLAMATION_COOLDOWN));
            }
            drop(inner);
            // Formatted and written after the guard is released.
            engram_root_reclamation_emit(self, &format!(
                "engram> named-root reclamation store={} work={} generation={} route={:?} outcome={}",
                task.root.store.project_id, task.root.work_id, task.root.generation, task.route,
                match outcome { Ok(true) => "retired".to_owned(), Ok(false) => "retained by canonical authority".to_owned(), Err(error) => format!("retained: {}", error.message) }));
        }
        // Inside the same flight: legacy and claim-refused roots by their
        // recorded rule, reading an item's tracker lifecycle through the Work
        // host reader when it decides (engram_named_root_capacity.rs).
        // Its budget runs from this pass's start, so the whole flight stays
        // within a release's wait for it.
        self.run_engram_named_root_disposition_pass(started, &|root| {
            self.read_engram_named_root_item_lifecycle(root)
        });
        engram_root_reclamation_emit(self, &format!(
            "engram> named-root reclamation pass attempted={} limit={} elapsed_ms={}",
            attempted, ENGRAM_ROOT_RECLAMATION_READ_LIMIT, clock.now().saturating_duration_since(started).as_millis()));
    }

    fn engram_root_capacity_refusal(&self) -> ApiError {
        let scheduled = self.schedule_engram_root_reclamation(true);
        let now = self.engram_budget_clock().now();
        let inner = self.inner.lock().expect("state mutex poisoned");
        // One row per retained root (engram_named_root_release.rs), so an
        // operator can route an own-clear or an audited release.
        let retained = engram_named_root_status_locked(&inner, now).iter()
            .map(engram_named_root_status_line).collect::<Vec<_>>().join(" | ");
        ApiError::conflict(format!("TermAl retains at most {} named source roots globally; {}. No slot release or successful retry is promised. Unacknowledged retirements still reserve capacity. Retained: {}",
            ENGRAM_WORK_SOURCE_ROOT_LIMIT, scheduled, retained))
    }

    fn recover_one_engram_root_until(
        &self,
        root: &EngramWorkSourceRoot,
        route: &EngramRootReclamationRoute,
        deadline: std::time::Instant,
    ) -> Result<bool, ApiError> {
        self.recover_one_engram_root_traced_until(root, route, deadline, None)
    }

    /// Explicit releases attach their audit before the prepared-image fence
    /// and resume its retained owner. Maintenance passes no release audit and
    /// keeps the ordinary preparation/publication path.
    fn recover_one_engram_root_traced_until(
        &self,
        root: &EngramWorkSourceRoot,
        route: &EngramRootReclamationRoute,
        deadline: std::time::Instant,
        release_audit: Option<&str>,
    ) -> Result<bool, ApiError> {
        let (binding, journal_snapshot, pending_publication) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if !engram_root_reclamation_reader_exists(&inner, &root.store) {
                return Err(ApiError::conflict("state unknown: no eligible reader in the original store; entry retained"));
            }
            match route {
                EngramRootReclamationRoute::Unfocused if engram_named_root_current_binding(&inner, root) => {
                    return Err(ApiError::conflict("the entry now has a current binding; its existing owner must reconcile it"));
                }
                EngramRootReclamationRoute::Current(session_id)
                    if engram_named_root_current_owner(&inner, root).as_ref() != Some(session_id)
                        || !engram_root_reclamation_idle_owner(&inner, session_id) => {
                    return Err(ApiError::conflict("the current reconciliation owner changed or is active; entry retained"));
                }
                EngramRootReclamationRoute::OwnClear(session_id) if &root.named_by_session != session_id => {
                    return Err(ApiError::conflict("not the naming session; entry retained"));
                }
                _ => {}
            }
            let journal = engram_root_journal(&inner.engram_named_root_journal, &root.store, &root.claim_id)
                .ok_or_else(|| ApiError::conflict("state unknown: no retained canonical run association; entry retained"))?;
            if journal.pending.is_some() {
                return Err(ApiError::conflict("pending producer intent belongs to its existing owner; entry retained"));
            }
            let binding = journal.read_binding.clone()
                .filter(|binding| binding.work_id == root.work_id && binding.claim_id == root.claim_id)
                .ok_or_else(|| ApiError::conflict("state unknown: retained run association does not match the entry"))?;
            let pending_publication = inner.engram_work_naming_history.iter()
                .find(|history| history.store == root.store && history.work_id == root.work_id)
                .filter(|history| history.retirements.iter().any(|notice|
                    notice.selection == *root && notice.binding == binding && !notice.published))
                .and_then(|history| history.transition.clone());
            if !inner.engram_work_source_roots.contains(root) && (pending_publication.is_none()
                || inner.engram_work_source_roots.iter().any(|current|
                    current.store == root.store && current.work_id == root.work_id))
            {
                return Err(ApiError::conflict("the exact retained selection was replaced; entry retained"));
            }
            (binding, journal.clone(), pending_publication)
        };
        if let Some(owner) = pending_publication {
            // This is the old publication obligation, not a new lifecycle
            // read or removal. Retrying its exact image cannot mint a notice.
            self.publish_engram_authority_image_until(&root.store, &owner, deadline)?;
        } else {
            let owner = match release_audit {
                Some(audit) => self.prepare_engram_release_authority_until(
                    root, &binding, audit, deadline)?,
                None => self.prepare_engram_authority_until(&root.store, &binding, deadline)?,
            };
            {
                let inner = self.inner.lock().expect("state mutex poisoned");
                if !inner.engram_work_source_roots.contains(root)
                    || engram_root_journal(&inner.engram_named_root_journal, &root.store, &root.claim_id) != Some(&journal_snapshot)
                {
                    return Err(ApiError::conflict("selection or journal changed while the existing authority owner was prepared"));
                }
            }
            self.recover_engram_authority_batch_until(&root.store, &owner, deadline, false, Some((root, route)))?;
            self.publish_engram_authority_image_until(&root.store, &owner, deadline)?;
        }
        let inner = self.inner.lock().expect("state mutex poisoned");
        Ok(inner.engram_work_naming_history.iter()
            .filter(|history| history.store == root.store && history.work_id == root.work_id)
            .flat_map(|history| &history.retirements)
            .any(|notice| notice.selection == *root && notice.binding == binding && notice.published)
            && !inner.engram_work_source_roots.iter().any(|current|
                current.store == root.store && current.work_id == root.work_id))
    }

    fn clear_obsolete_own_engram_root_until(
        &self,
        session_id: &str,
        root: &EngramWorkSourceRoot,
        deadline: std::time::Instant,
    ) -> Result<EngramSourceRootResponse, ApiError> {
        if !self.recover_one_engram_root_until(root,
            &EngramRootReclamationRoute::OwnClear(session_id.to_owned()), deadline)?
        {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let reason = inner.engram_work_naming_history.iter()
                .find(|history| history.store == root.store && history.work_id == root.work_id)
                .and_then(|history| history.recovery_reason.as_deref())
                .unwrap_or("the canonical run and binding remain open, or the read is older than this selection");
            return Err(ApiError::conflict(format!("retained name was not cleared: {reason}; no obsolete-binding proof was acknowledged")));
        }
        Ok(EngramSourceRootResponse {
            work_ref: root.short_ref.clone(), work_id: root.work_id.clone(), root: None,
            source_revision: None, unmeasured: None, generation: 0, sealed: None,
            notice: format!("`{}`: obsolete retained name retired with canonical authority; no producer event was created.", root.short_ref),
        })
    }
}
