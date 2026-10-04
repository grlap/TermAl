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

fn engram_root_reclamation_session_is_reader(
    inner: &StateInner,
    session_id: &str,
    store: &EngramAuthorityStoreKey,
) -> bool {
    AppState::engram_binding_target_for_session_shape_locked(inner, session_id, true)
        .ok().flatten().is_some_and(|target| target.routing_token.is_some()
            && target.settings.authority_store_key.as_ref() == Some(store))
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
            && AppState::engram_binding_target_for_session_shape_locked(inner, session_id, true)
                .ok().flatten().is_some_and(|target| target.routing_token.is_some())
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
                && AppState::engram_binding_target_for_session_shape_locked(inner, session_id, true)
                    .ok().flatten().is_some_and(|target|
                        target.settings.authority_store_key.as_ref() == Some(&root.store)),
    }
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
}

fn engram_named_root_capacity(inner: &StateInner) -> usize {
    inner.engram_work_source_roots.len()
        + inner.engram_work_naming_history.iter().flat_map(|history| &history.retirements)
            .filter(|notice| !notice.published
                && !inner.engram_work_source_roots.contains(&notice.selection))
            .count()
}

fn engram_publish_root_retirements_locked(
    inner: &mut StateInner,
    store: &EngramAuthorityStoreKey,
    work: &str,
) {
    if let Some(history) = inner.engram_work_naming_history.iter_mut()
        .find(|history| &history.store == store && history.work_id == work)
    {
        for notice in &mut history.retirements {
            if !notice.published {
                notice.published = true;
                eprintln!("engram> {}", notice.line);
            }
        }
    }
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
    let Some(reason) = engram_root_retirement_reason(root, proof) else { return; };
    let Some(index) = inner.engram_work_source_roots.iter().position(|current| current == root)
    else { return; };
    let Some(history) = inner.engram_work_naming_history.iter_mut()
        .find(|history| history.store == root.store && history.work_id == root.work_id)
    else { return; };
    // Keep exact proof/history with the removal, before the complete-image
    // fence. No success, capacity release or notice delivery precedes its ACK.
    history.retirements.push(EngramNamedRootRetirementNotice {
        id: Uuid::new_v4().to_string(), selection: root.clone(), binding: binding.clone(),
        read: proof.clone(),
        line: format!(
            "[TermAl] Retired named source root {} / {} ({}) at {} (generation {}): {}; canonical read cut {} / {}.",
            format!("{} / {}", root.store.database_path.display(), root.store.project_id),
            root.short_ref, root.work_id, root.root, root.generation,
            reason, proof.read_cut.feed.id, proof.read_cut.position),
        published: false, delivered: false,
    });
    inner.engram_work_source_roots.remove(index);
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
            if plan.is_empty() {
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
            eprintln!("engram> named-root reclamation store={} work={} generation={} route={:?} outcome={}",
                task.root.store.project_id, task.root.work_id, task.root.generation, task.route,
                match outcome { Ok(true) => "retired".to_owned(), Ok(false) => "retained by canonical authority".to_owned(), Err(error) => format!("retained: {}", error.message) });
        }
        eprintln!("engram> named-root reclamation pass attempted={} limit={} elapsed_ms={}",
            attempted, ENGRAM_ROOT_RECLAMATION_READ_LIMIT, clock.now().saturating_duration_since(started).as_millis());
    }

    fn engram_root_capacity_refusal(&self) -> ApiError {
        let scheduled = self.schedule_engram_root_reclamation(true);
        let inner = self.inner.lock().expect("state mutex poisoned");
        let retained = inner.engram_work_source_roots.iter().map(|root| {
            let reason = if !engram_root_reclamation_reader_exists(&inner, &root.store) {
                format!("no eligible reader for store {}", root.store.project_id)
            } else if engram_root_journal(&inner.engram_named_root_journal, &root.store, &root.claim_id)
                .is_none_or(|journal| journal.read_binding.is_none()) {
                "no retained canonical run association".to_owned()
            } else if engram_root_journal(&inner.engram_named_root_journal, &root.store, &root.claim_id)
                .is_some_and(|journal| journal.pending.is_some()) {
                "pending producer intent belongs to its existing owner".to_owned()
            } else {
                inner.engram_work_naming_history.iter()
                    .find(|history| history.store == root.store && history.work_id == root.work_id)
                    .and_then(|history| history.recovery_reason.clone())
                    .unwrap_or_else(|| "no acknowledged obsolete-binding proof; live or unknown entries remain retained".to_owned())
            };
            format!("{} / {} generation {}: {}", root.store.project_id, root.short_ref, root.generation, reason)
        }).collect::<Vec<_>>().join("; ");
        ApiError::conflict(format!("TermAl retains at most {} named source roots globally; {}. No slot release or successful retry is promised. Unacknowledged retirements still reserve capacity. Retained: {}",
            ENGRAM_WORK_SOURCE_ROOT_LIMIT, scheduled, retained))
    }

    fn recover_one_engram_root_until(
        &self,
        root: &EngramWorkSourceRoot,
        route: &EngramRootReclamationRoute,
        deadline: std::time::Instant,
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
            let owner = self.prepare_engram_authority_until(&root.store, &binding, deadline)?;
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
