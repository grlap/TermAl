// An unsolicited adopted successor cannot be paused. Preserve the predecessor's
// already observed facts under its own opening, with no source sighting or credit.
// This path never authorizes measurement across a scope transition.
impl AppState {
    fn capture_adopted_predecessor_unadmitted(
        &self, session: &str, job: &EngramRuntimeClosingJob,
    ) -> Result<EngramRuntimeClosingOutcome, EngramTransportError> {
        let binding = job.scope.binding.as_ref().ok_or_else(||
            EngramTransportError::local_state("adopted predecessor has no opening association"))?;
        let current = |inner: &StateInner| {
            let Some(index) = inner.find_session_index(session) else { return false; };
            let record = &inner.sessions[index];
            !record.runtime_stop_in_progress && !record.engram.observation_delete_requested
                && !session_resource_reset_in_progress(inner, record)
                && !Self::runtime_observation_capture_waits_for_binding(record)
                && job.trace.opening_root.as_ref().is_some_and(|opening|
                    opening.store == job.scope.store && &opening.binding == binding)
                && record.engram.unadmitted_closing_jobs.iter().any(|stored| stored == job)
                && !engram_authority_work_unresolved(inner, &job.scope.store, &binding.work_id)
                && Self::engram_source_root_target_locked(inner, session).ok().flatten()
                    .is_some_and(|target| target.connection == job.scope.connection
                        && target.settings.authority_store_key.as_ref() == Some(&job.scope.store))
                && record.session.workdir == job.scope.workdir
        };
        let target = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if !current(&inner) { return Err(runtime_observation_scope_changed(&job.scope, "predecessor report custody changed")); }
            Self::engram_source_root_target_locked(&inner, session)
                .map_err(EngramTransportError::local_state)?
                .ok_or_else(|| EngramTransportError::local_state("predecessor connection absent"))?
        };
        let deadline = target.budget_clock.now() + target.settings.call_timeout();
        let held = target.adapter.read_held_claims(&job.scope.connection, target.rpc_timeout_until(deadline)?)?;
        if !held.items.iter().any(|claim| claim.work_id == binding.work_id
            && claim.claim_id == binding.claim_id && claim.claim_fence == binding.claim_fence
            && claim.control_binding.as_ref() == Some(binding)) {
            return Err(runtime_observation_scope_changed(&job.scope, "original opening is no longer held"));
        }
        let root = target.adapter.request(&job.scope.connection, &EngramControlRequest::NamedRootRead {
            run_id: binding.run_id.clone(), claim_id: binding.claim_id.clone(),
        }, target.rpc_timeout_until(deadline)?)
            .and_then(parse_engram_result::<EngramNamedRootReadResponse>)?;
        root.validate_authority(&job.scope.store, binding)
            .map_err(|error| EngramTransportError::protocol(error.message))?;
        if !runtime_observation_same_root(&root.named_root, &job.scope.root) {
            return Err(runtime_observation_scope_changed(&job.scope, "original root changed"));
        }
        let root_basis = serde_json::from_value(json!({
            "capture_run_cut": root.read_cut.position,
            "latest_event": root.latest_event.as_ref().map(|event| &event.event),
            "state": root.named_root,
        })).map_err(|error| EngramTransportError::protocol(format!("invalid predecessor root proof: {error}")))?;
        let policy = target.adapter.read_observation_policy(&job.scope.connection, &job.scope.store, target.rpc_timeout_until(deadline)?)?;
        let observer = target.adapter.read_work_session(&job.scope.connection, &job.scope.store, target.rpc_timeout_until(deadline)?)?;
        if !current(&self.inner.lock().expect("state mutex poisoned")) {
            return Err(runtime_observation_scope_changed(&job.scope, "predecessor report custody changed during read"));
        }
        EngramRuntimeObservationReport::prepare(&job.trace, EngramRuntimeClosingCapture {
            binding: binding.clone(), root_basis,
            source: EngramRuntimeClosingSource::Unavailable { observed_at: unadmitted_observation_timestamp() },
            baseline: None, registered_check_baselines: Vec::new(), policy,
        }, job.scope.connection.clone(), job.scope.store.clone(), observer, target.settings.call_timeout())
            .map(|report| EngramRuntimeClosingOutcome::Report(report, None))
    }
}
