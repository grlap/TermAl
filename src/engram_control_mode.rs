// Owns the applicability of gated control and ordinary queue selection at OFF.
// Unknown provider attempts stay in their original queue and ledger. Only
// positively never-promoted envelopes can run as ordinary provider work.

#[derive(Clone, Debug, PartialEq, Eq)]
struct OrdinaryQueueIdentity {
    intent: String,
    stop_generation: u64,
    pause_epoch: u64,
    write_ahead: Option<String>,
}

fn ordinary_off_execution_applies(inner: &StateInner, session_id: &str) -> bool {
    inner.find_session_index(session_id)
        .is_some_and(|index| inner.sessions[index].is_local_session())
        && engram_project_for_session_locked(inner, session_id).is_some_and(|project| {
            project.remote_id == LOCAL_REMOTE_ID
                && project.engram.as_ref().is_some_and(|settings| {
                    settings.is_base_enabled() && !settings.is_control_enabled()
                })
        })
}

fn session_ordinary_off_projection(inner: &StateInner, record: &SessionRecord) -> Option<bool> {
    if record.is_local_session() {
        Some(ordinary_off_execution_applies(inner, &record.session.id))
    } else {
        // Only the executing host can decide this for a proxy, including its
        // inherited project and process-wide integration switch.
        record.session.engram_control_off
    }
}

// One immutable projection cut shares its relationship lookups. Re-running
// the by-id admission helper for every record would scan all sessions twice
// even on a cache hit. This index is temporary; it owns no execution authority.
#[derive(Default)]
struct OrdinaryOffProjection<'a> {
    off_sessions: HashSet<&'a str>,
}

impl<'a> OrdinaryOffProjection<'a> {
    fn new(inner: &'a StateInner) -> Self {
        let mut projection = Self::default();
        if !inner.delegation_inventory_complete() { return projection; }
        let mut projects = HashMap::new();
        for project in &inner.projects {
            projects.entry(project.id.as_str()).or_insert_with(|| {
                project.remote_id == LOCAL_REMOTE_ID
                    && project.engram.as_ref().is_some_and(|settings|
                        settings.is_base_enabled() && !settings.is_control_enabled())
            });
        }
        // Ordinary projects dominate large snapshots. They need neither an
        // ancestry map nor a session lookup to project Some(false).
        if !projects.values().any(|off| *off) { return projection; }
        let mut sessions: HashMap<&str, &SessionRecord> = HashMap::new();
        for record in &inner.sessions {
            sessions.entry(record.session.id.as_str()).or_insert(record);
        }
        let mut parents = HashMap::new();
        for delegation in &inner.delegations {
            parents.entry(delegation.child_session_id.as_str())
                .or_insert(delegation.parent_session_id.as_str());
        }
        let mut resolved = HashMap::new();
        for record in sessions.values().copied().filter(|record| record.is_local_session()) {
            let mut current = record;
            let mut path = Vec::new();
            let mut visited = HashSet::new();
            let off = loop {
                let id = current.session.id.as_str();
                if let Some(off) = resolved.get(id) { break *off; }
                if !visited.insert(id) { break false; }
                path.push(id);
                if let Some(project) = current.session.project_id.as_deref() {
                    break projects.get(project).copied().unwrap_or(false);
                }
                let Some(parent) = parents.get(id) else { break false; };
                let Some(parent) = sessions.get(parent) else { break false; };
                current = parent;
            };
            for id in path { resolved.insert(id, off); }
            if off { projection.off_sessions.insert(record.session.id.as_str()); }
        }
        projection
    }

    fn for_record(&self, record: &SessionRecord) -> Option<bool> {
        if record.is_local_session() {
            Some(self.off_sessions.contains(record.session.id.as_str()))
        } else {
            record.session.engram_control_off
        }
    }
}

// Load-only inputs awaiting complete project inheritance. These belong to the
// inventory publication phase, never to persisted pause or dispatch authority.
#[derive(Clone, Debug)]
struct OrdinaryQueueLoadPause {
    session_id: String,
    paused: bool,
    control_pause: bool,
    pause_epoch: u64,
}

fn restore_ordinary_queue_load_pauses(inner: &mut StateInner, images: Vec<OrdinaryQueueLoadPause>) {
    for image in images {
        if !ordinary_off_execution_applies(inner, &image.session_id) { continue; }
        let Some(index) = inner.find_session_index(&image.session_id) else { continue; };
        let record = &mut inner.sessions[index];
        // A pause established after load is owned by its newer caller.
        if record.auto_dispatch_pause_epoch != image.pause_epoch { continue; }
        let paused = image.paused && !image.control_pause;
        let refresh_preview = image.control_pause
            && record.session.preview == ENGRAM_CONTROL_OFF_AUTHORIZATION_PREVIEW;
        let changed = record.orchestrator_auto_dispatch_blocked != paused
            || record.session.queue_paused != paused || refresh_preview;
        record.orchestrator_auto_dispatch_blocked = paused;
        record.session.queue_paused = paused;
        if refresh_preview {
            record.session.preview = ENGRAM_RETAINED_AUTHORIZATION_PREVIEW.to_owned();
        }
        sync_pending_prompts(record);
        if changed { inner.stamp_session_at_index(index); }
    }
}

fn ordinary_queue_position(record: &SessionRecord) -> Option<usize> {
    record.queued_prompts.iter().position(|queued| ordinary_queue_entry_is_runnable(record, queued))
}

fn ordinary_queue_entry_is_runnable(record: &SessionRecord, queued: &QueuedPromptRecord) -> bool {
        queued.promotion_disposition_known
            && queued.promoted_message_index.is_none()
            && !queued.engram_ordinary_handoff
            && record
                .prompt_deliveries
                .get(&queued.pending_prompt.id)
                .is_none_or(|delivery| {
                    matches!(
                        delivery.disposition,
                        PromptDeliveryDisposition::Pending | PromptDeliveryDisposition::HandingOff
                    ) && delivery.turn_generation.is_none()
                })
}

fn queue_has_orphaned_workflow(inner: &StateInner, record: &SessionRecord) -> bool {
    let ordinary_off = ordinary_off_execution_applies(inner, &record.session.id);
    record.queued_prompts.iter()
        .filter(|queued| !ordinary_off || ordinary_queue_entry_is_runnable(record, queued))
        .find(|queued| queued.source != QueuedPromptSource::Mailbox)
        .is_some_and(|queued| queued.source == QueuedPromptSource::Orchestrator)
}

fn ordinary_admission_is_excluded(inner: &StateInner, session_id: &str) -> bool {
    !ordinary_off_execution_applies(inner, session_id)
        && engram_admission_obligation_locked(inner, session_id).is_some()
}

fn control_pause_preview_is_owned(preview: &str) -> bool {
    matches!(preview, ENGRAM_RETAINED_AUTHORIZATION_PREVIEW
        | ENGRAM_CONTROL_OFF_AUTHORIZATION_PREVIEW | ENGRAM_SETTINGS_AUTHORIZATION_PREVIEW)
}

fn control_pause_is_owned(record: &SessionRecord) -> bool {
    let preview = if engram_project_reset_wait_shown(record) {
        record.engram.project_reset_wait_preview.as_deref().unwrap_or(&record.session.preview)
    } else {
        &record.session.preview
    };
    record.orchestrator_auto_dispatch_blocked
        && !record.engram.operator_paused
        && record.provider_capacity_retry.is_none()
        && record.queued_prompts.iter().any(QueuedPromptRecord::is_engram_retained)
        && control_pause_preview_is_owned(preview)
}

fn abandon_ending_turn_engram_dispatch(record: &mut SessionRecord) -> bool {
    if record.unmediated_turn_generation == Some(record.active_turn_generation) {
        false
    } else {
        take_and_abandon_engram_pending_dispatch(record)
    }
}

fn dispatch_queue_position(inner: &StateInner, index: usize) -> Option<usize> {
    let record = &inner.sessions[index];
    if !ordinary_off_execution_applies(inner, &record.session.id) {
        (!record.queued_prompts.is_empty()).then_some(0)
    } else {
        ordinary_queue_position(record)
    }
}

impl AppState {
    fn ordinary_off_execution_applies(&self, session_id: &str) -> bool {
        let inner = self.inner.lock().expect("state mutex poisoned");
        ordinary_off_execution_applies(&inner, session_id)
    }

    fn resume_queue_without_gated_control(
        &self,
        session_id: &str,
        selection: Option<&HumanPromptSelection>,
    ) -> Result<StateResponse, ApiError> {
        if let Some(delivery) =
            self.resume_local_queue_without_gated_control(session_id, selection, None)?
        {
            delivery.into_public_result()?;
        }
        Ok(self.snapshot())
    }

    fn resume_local_queue_without_gated_control(
        &self,
        session_id: &str,
        selection: Option<&HumanPromptSelection>,
        expected: Option<&EngramQueuedAdmissionOwner>,
    ) -> Result<Option<TurnDispatchDeliveryOutcome>, ApiError> {
        let owner = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            if !ordinary_off_execution_applies(&inner, session_id) {
                return Err(ApiError::conflict(
                    "gated control settings changed during Resume",
                ));
            }
            let record = &inner.sessions[index];
            if expected.is_some_and(|owner| {
                !owner.matches(record)
                    || ordinary_queue_position(record).is_none_or(|position| {
                        record.queued_prompts[position].pending_prompt.id != owner.prompt_id
                    })
            }) {
                return Ok(Some(TurnDispatchDeliveryOutcome::Superseded));
            }
            if selection.is_some_and(|selection| {
                ordinary_queue_position(record) != Some(0)
                    || record.mutation_stamp != selection.session_mutation_stamp
                    || projected_pending_prompts(record)
                        .first()
                        .is_none_or(|head| {
                            serde_json::to_value(head).ok()
                                != serde_json::to_value(&selection.prompt).ok()
                        })
            }) {
                return Err(ApiError::conflict(
                    "the selected queued prompt changed; refresh before resuming",
                ));
            }
            if record.runtime_stop_in_progress
                || record.engram.project_reset_in_progress
                || matches!(
                    record.session.status,
                    SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
                )
                || engram_project_for_session_locked(&inner, session_id)
                    .is_some_and(|project| inner.engram_project_resets.contains(&project.id))
            {
                return Err(ApiError::conflict("session is busy or stopping"));
            }
            if record.codex_terminal_identity_unresolved || record_has_archived_codex_thread(record)
            {
                return Err(ApiError::conflict(
                    "session runtime identity is unavailable",
                ));
            }
            if let Some(position) = ordinary_queue_position(record)
                && let Some(refusal) = queued_prompt_durability_resume_refusal(
                    record, &record.queued_prompts[position],
                )
            {
                return Err(ApiError::conflict(refusal));
            }
            let before = inner.sessions[index].clone();
            let record = inner
                .session_mut_by_index(index)
                .expect("validated session");
            clear_provider_capacity_retry(record);
            // Control recovery state, including the source-observation hold,
            // is historical data at OFF. Only release the ordinary latch.
            record.orchestrator_auto_dispatch_blocked = false;
            record.session.queue_paused = false;
            record.engram.operator_paused = false;
            if let Err(error) = self.commit_session_locked(&mut inner, session_id) {
                inner.sessions[index] = before;
                return Err(ApiError::internal(format!(
                    "failed persisting Resume: {error:#}"
                )));
            }
            EngramQueuedAdmissionOwner::capture_ordinary(&inner.sessions[index])
        };
        let started = owner
            .map(|owner| self.dispatch_next_queued_turn_for_resume_with_prompt(session_id, owner))
            .transpose()
            .map_err(|error| ApiError::internal(format!("failed resuming queue: {error:#}")))?
            .flatten();
        Ok(started.map(|started| deliver_turn_dispatch(self, started.dispatch)))
    }
}
