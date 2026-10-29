// Owns action-specific access to an independently retained checkpoint.
// Does not grant binding, admission, provider delivery or generic exclusion bypass.
// Split from the checkpoint replay/containment target selection paths.

struct EngramCheckpointRecoveryView<'a> {
    operation: &'a EngramCheckpointOperation,
    record: &'a SessionRecord,
    project: &'a Project,
    title: String,
    effects: Vec<EngramEffect>,
}

// Load and mutation boundaries validate the complete ledger. A lookup validates
// only its selected record and the identities that could conflict with it;
// unrelated frozen reports are not serialized on ordinary Send/drain paths.
fn selected_checkpoint_operation<'a>(
    ledger: &'a EngramSessionObligationLedger,
    mirror: &EngramCheckpointOperation,
) -> Option<&'a EngramCheckpointOperation> {
    if ledger.schema_version != ENGRAM_SESSION_OBLIGATION_LEDGER_VERSION {
        return None;
    }
    let mut matches = ledger
        .checkpoint_operations
        .iter()
        .filter(|entry| entry.original.id == mirror.original.id);
    let selected = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    selected.validate().ok()?;
    if !std::ptr::eq(selected, mirror)
        && serde_json::to_value(selected).ok()? != serde_json::to_value(mirror).ok()? {
        return None;
    }
    let EngramControlRequest::TurnCheckpoint {
        idempotency_key, ..
    } = &selected.original.request
    else {
        return None;
    };
    if ledger.checkpoint_operations.iter().any(|other| {
        other.original.id != selected.original.id
            && other.original.authority == selected.original.authority
            && (matches!(&other.original.request,
                EngramControlRequest::TurnCheckpoint { idempotency_key: key, .. }
                    if key == idempotency_key)
                || other.protection.phase != EngramSessionObligationPhase::Confirmed
                    && selected.protection.phase != EngramSessionObligationPhase::Confirmed
                    && other.protection.external_ref == selected.protection.external_ref
                    && other.protection.grant_id == selected.protection.grant_id)
    }) {
        return None;
    }
    let mut adopted = ledger
        .entries
        .iter()
        .filter(|entry| entry.id == selected.original.id);
    if let Some(entry) = adopted.next() {
        if adopted.next().is_some() {
            return None;
        }
        entry.validate().ok()?;
        let image: EngramUnsettledControlImage =
            serde_json::from_value(entry.frozen_request.clone()).ok()?;
        if serde_json::to_value(&image.existing_checkpoint.as_ref()?.original).ok()?
            != serde_json::to_value(&selected.original).ok()?
        {
            return None;
        }
    }
    Some(selected)
}

impl<'a> EngramCheckpointRecoveryView<'a> {
    fn capture(inner: &'a StateInner, session_id: &str) -> Option<Self> {
        let record = &inner.sessions[inner.find_visible_session_index(session_id)?];
        let mirror = record.engram.checkpoint_operation.as_ref()?;
        let operation = selected_checkpoint_operation(&inner.engram_session_obligations, mirror)?;
        if !record.is_local_session() {
            return None;
        }
        let original = &operation.original;
        let project = engram_project_for_session_locked(inner, session_id)?;
        let settings = project.engram.as_ref()?;
        if project.id != operation.protection.project_id
            || project.remote_id != LOCAL_REMOTE_ID
            || original.connection.session_id != session_id
            || std::path::Path::new(&project.root_path) != original.connection.project_root
            || original.connection.project_file
                != original.connection.project_root.join(".engram-project")
            || settings.home.as_deref().map(std::path::Path::new)
                != Some(original.connection.home.as_path())
            || settings.binary_path.as_deref().map(std::path::Path::new)
                != Some(original.connection.binary_path.as_path())
            || settings
                .authority_store_key
                .as_ref()
                .is_some_and(|store| store != &operation.protection.authority)
            || record.engram.work_binding != original.work_binding
        {
            return None;
        }
        let (external_ref, title, effects) =
            if AppState::engram_session_has_child_binding_shape_locked(inner, session_id) {
                let delegation = inner
                    .delegations
                    .iter()
                    .find(|entry| entry.child_session_id == session_id)?;
                (
                    format!("termal:delegation:{}", delegation.id),
                    delegation.title.clone(),
                    engram_effects_for_write_policy(&delegation.write_policy),
                )
            } else {
                (
                    format!("termal:session:{session_id}"),
                    record.session.name.clone(),
                    vec![
                        EngramEffect::Observe,
                        EngramEffect::Communicate,
                        EngramEffect::MutateLocal,
                    ],
                )
            };
        let attempt = operation.attempts.last()?;
        if external_ref != operation.protection.external_ref
            || record.engram.routing_token.as_deref()
                != Some(operation.protection.routing_token.as_str())
            || record.engram.active_grant_id.as_deref()
                != Some(operation.protection.grant_id.as_str())
            || record.engram.dispatch_generation != attempt.dispatch_generation
            || record.active_turn_generation != attempt.active_turn_generation
            || format!("{:?}", record.runtime.runtime_token()) != attempt.runtime_episode
            || attempt.queued_prompt_id.as_ref().is_some_and(|id| {
                record.queued_prompts.front().is_none_or(|head| {
                    &head.pending_prompt.id != id
                        || Some(containment_prompt_fingerprint(head)) != attempt.queued_fingerprint
                })
            })
        {
            return None;
        }
        Some(Self {
            operation,
            record,
            project,
            title,
            effects,
        })
    }

    // A transport snapshot is private to the qualified checkpoint action. Its
    // frozen connection and work binding never become a fresh admission target.
    fn transport_target(&self, inner: &StateInner) -> EngramBindingTarget {
        let original = &self.operation.original;
        EngramBindingTarget {
            runtime_snapshot: self.record.runtime.runtime_token(),
            work_binding: original.work_binding.clone(),
            source_root_read_fence: EngramRootReadFence::capture(inner),
            adapter: inner.engram_host_adapter.clone(),
            budget_clock: inner.engram_budget_clock_snapshot(),
            admission_started_at: None,
            admission_deadline: engram_admission_budget_deadline(
                self.record,
                &inner.engram_budget_clock_snapshot(),
            ),
            #[cfg(test)]
            test_dispatch_budget: inner.test_engram_dispatch_budget,
            #[cfg(test)]
            test_root_setup_deadline: None,
            connection: original.connection.clone(),
            settings: original.settings.clone(),
            project_id: self.project.id.clone(),
            project_reset_owner_generation: original.checkpoint_owner_generation,
            external_ref: self.operation.protection.external_ref.clone(),
            title: self.title.clone(),
            effects: self.effects.clone(),
            routing_token: Some(self.operation.protection.routing_token.clone()),
            active_grant_id: Some(self.operation.protection.grant_id.clone()),
            uncertain_grant_id: self.record.engram.uncertain_grant_id.clone(),
            rebind_required: self.record.engram.rebind_required,
            circuit_open: self.record.engram.circuit_open,
            next_bind_retry_at: self.record.engram.next_bind_retry_at,
            refreshed_work_binding: None,
        }
    }

    fn for_off_adoption(inner: &'a StateInner, session_id: &str) -> Option<EngramBindingTarget> {
        let view = Self::capture(inner, session_id)?;
        let settings = view.project.engram.as_ref()?;
        if !settings.same_admission_settings(&view.operation.original.settings)
            || view.record.engram.checkpoint_in_progress
                && view.record.engram.checkpoint_owner_generation
                    != view.operation.original.checkpoint_owner_generation
            || view.record.engram.checkpoint_status_owner.is_some()
        {
            return None;
        }
        Some(view.transport_target(inner))
    }

    fn for_certified_replay(
        inner: &'a StateInner,
        session_id: &str,
        owner: Option<&EngramQueuedAdmissionOwner>,
    ) -> Option<EngramBindingTarget> {
        let view = Self::capture(inner, session_id)?;
        let operation = view.operation;
        if operation.has_uncertain_checkpoint_history() {
            return None;
        }
        let Some(EngramCheckpointAttemptResult::CertifiedNotApplied { code }) =
            operation.attempts.last()?.result.as_ref()
        else {
            return None;
        };
        let retained = view.record.engram.retained_checkpoint.as_ref()?;
        let target = view.transport_target(inner);
        if !checkpoint_retry_configuration_matches(inner, session_id, &operation.original)
            || !operation.protection.authority_confirmed
            || view.project.engram.as_ref()?.authority_store_key.as_ref()
                != Some(&operation.protection.authority)
            || view.record.engram.checkpoint_in_progress
            || view.record.engram.checkpoint_status_owner.is_some()
            || inner.engram_project_resets.contains(&view.project.id)
            || owner.is_some_and(|owner| !owner.matches(view.record))
            || target.rebind_required
            || target.circuit_open
            || !retained.matches(&target)
            || retained.error.invocation_application()
                != EngramInvocationApplication::CertifiedNotApplied
            || retained.error.code.as_ref() != Some(code)
            || serde_json::to_value(&retained.request).ok()
                != serde_json::to_value(&operation.original.request).ok()
            || engram_abort_authority(&target) != operation.original.authority
            || inner
                .engram_session_obligations
                .all_protections()
                .any(|entry| {
                    engram_obligation_requires_exclusion(inner, entry)
                        && engram_obligation_matches_session(inner, entry, session_id)
                        && (entry.kind != EngramSessionObligationKind::ExistingCheckpoint
                            || entry.id != operation.original.id)
                })
        {
            return None;
        }
        Some(target)
    }

    fn for_original_status(
        inner: &'a StateInner,
        session_id: &str,
        owner: Option<&EngramQueuedAdmissionOwner>,
    ) -> Option<Self> {
        let view = Self::capture(inner, session_id)?;
        let retained = view.record.engram.retained_checkpoint.as_ref()?;
        let Some(EngramCheckpointAttemptResult::Unknown { code }) =
            view.operation.attempts.last()?.result.as_ref()
        else {
            return None;
        };
        let retained_code =
            retained.error.code.as_ref().map(|code| {
                engram_containment_error_code(Some(code), "checkpoint_failed").to_owned()
            });
        if code != &retained_code
            || retained.error.invocation_application() != EngramInvocationApplication::Unknown
            || !view.record.engram.rebind_required
            || view.record.engram.checkpoint_in_progress
            || view.record.runtime_stop_in_progress
            || view.record.engram.project_reset_in_progress
            || inner.engram_project_resets.contains(&view.project.id)
            || owner.is_some_and(|owner| !owner.matches(view.record))
            || !view
                .project
                .engram
                .as_ref()?
                .same_admission_settings(&view.operation.original.settings)
            || serde_json::to_value(&retained.request).ok()
                != serde_json::to_value(&view.operation.original.request).ok()
            || retained.authority != view.operation.original.authority
        {
            return None;
        }
        Some(view)
    }
}

struct EngramCheckpointStatusGuard {
    state: Arc<StateMutex<StateInner>>,
    session_id: String,
    owner: String,
}

impl Drop for EngramCheckpointStatusGuard {
    fn drop(&mut self) {
        let mut inner = self.state.lock().expect("state mutex poisoned");
        if let Some(index) = inner.find_session_index(&self.session_id)
            && inner.sessions[index]
                .engram
                .checkpoint_status_owner
                .as_ref()
                == Some(&self.owner)
        {
            inner.sessions[index].engram.checkpoint_status_owner = None;
        }
    }
}

impl AppState {
    fn diagnose_original_checkpoint_status(
        &self,
        session_id: &str,
        owner: Option<&EngramQueuedAdmissionOwner>,
        started_at: Option<std::time::Instant>,
    ) -> Option<EngramTransportError> {
        let (guard, operation, target) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let view =
                EngramCheckpointRecoveryView::for_original_status(&inner, session_id, owner)?;
            if view.record.engram.checkpoint_status_owner.is_some() {
                return Some(EngramTransportError::backoff(
                    "Original checkpoint status already has an owner",
                ));
            }
            let operation = serde_json::to_value(view.operation).ok()?;
            let target = view.transport_target(&inner);
            let id = Uuid::new_v4().to_string();
            let index = inner.find_session_index(session_id)?;
            inner.sessions[index].engram.checkpoint_status_owner = Some(id.clone());
            (
                EngramCheckpointStatusGuard {
                    state: self.inner.clone(),
                    session_id: session_id.to_owned(),
                    owner: id,
                },
                operation,
                target,
            )
        };
        let started = started_at
            .or_else(|| {
                self.checkpoint_preflight_budget(session_id)
                    .map(|budget| budget.0)
            })
            .unwrap_or_else(|| target.budget_clock.now());
        let deadline = target.dispatch_deadline(started);
        let still_owned = || {
            let inner = self.inner.lock().expect("state mutex poisoned");
            EngramCheckpointRecoveryView::for_original_status(&inner, session_id, owner)
                .is_some_and(|view| {
                    view.record.engram.checkpoint_status_owner.as_ref() == Some(&guard.owner)
                        && serde_json::to_value(view.operation).ok().as_ref() == Some(&operation)
                })
        };
        if !still_owned() {
            return Some(EngramTransportError::local_state(
                "Original checkpoint diagnostic owner changed",
            ));
        }
        let timeout = match target.rpc_timeout_until(deadline) {
            Ok(timeout) => timeout,
            Err(error) => return Some(error),
        };
        let request = EngramControlRequest::SessionStatus {
            routing_token: target.routing_token.clone()?,
        };
        let result = target
            .adapter
            .request(&target.connection, &request, timeout)
            .and_then(parse_engram_result::<EngramSessionStatusResponse>);
        if !still_owned() {
            return Some(EngramTransportError::local_state(
                "Original checkpoint diagnostic owner changed after status",
            ));
        }
        let mut held = EngramTransportError::local_state(
            "Status cannot settle the protected original checkpoint or discard its report",
        );
        if let Err(error) = result {
            held.causal_failure = Some(EngramCausalFailure::request(&error, &request));
        }
        Some(held)
    }
}
