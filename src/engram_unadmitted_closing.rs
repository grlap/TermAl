// Read-only closing capture for observation reporting. Call on the observation
// worker after retaining the trace and previous checkpoint endpoint under the
// turn-result lock. No reconcile/admission/hold method is called here.

/// The test build's diagnostic line for one closing-capture attempt: the
/// attempt number, the capture budget, the time its source measurement took,
/// and whether it measured anything. It says how far a slow or failing
/// capture got, so it is printed only for an attempt that failed to measure or
/// took longer than its budget, and not for every capture in every test.
#[cfg(test)]
fn unadmitted_capture_diagnostic(
    session_id: &str,
    attempt: u32,
    budget: Duration,
    elapsed: Duration,
    measured: bool,
) -> Option<String> {
    if measured && elapsed <= budget {
        return None;
    }
    Some(format!(
        "UNADMITTED_CAPTURE_DIAGNOSTIC session={session_id} attempt={attempt} budget_us={} elapsed_us={} measured={measured}",
        budget.as_micros(),
        elapsed.as_micros(),
    ))
}

impl AppState {
    fn capture_runtime_observation_report(
        &self,
        session_id: &str,
        job: &EngramRuntimeClosingJob,
    ) -> Result<EngramRuntimeClosingOutcome, EngramTransportError> {
        let target = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if inner.find_session_index(session_id).is_some_and(|index| {
                Self::runtime_observation_capture_waits_for_binding(&inner.sessions[index])
            }) {
                return Ok(EngramRuntimeClosingOutcome::BindingRecoveryPending);
            }
            job.scope.require_current(&inner, session_id)?;
            Self::engram_binding_target_for_session_shape_locked(&inner, session_id, true)
                .map_err(EngramTransportError::local_state)?
        };
        let Some(target) = target else {
            return Err(runtime_observation_scope_changed(
                &job.scope,
                "connection or binding absent",
            ));
        };
        let Some(current) = target.work_binding.as_ref() else {
            return Err(runtime_observation_scope_changed(
                &job.scope,
                "connection or binding absent",
            ));
        };
        let store = target.settings.authority_store_key.clone().ok_or_else(|| {
            EngramTransportError::local_state("runtime closing capture has no verified store")
        })?;
        let routing = target.routing_token.as_deref().ok_or_else(|| {
            EngramTransportError::local_state("runtime closing capture has no route")
        })?;
        let deadline = target.budget_clock.now() + target.settings.call_timeout();
        let timeout = || target.rpc_timeout_until(deadline);
        let Some(policy) = job.policy.as_ref() else {
            return target
                .adapter
                .read_observation_policy(&target.connection, &store, timeout()?)
                .map(EngramRuntimeClosingOutcome::Policy);
        };
        let binding = target.adapter.read_work_binding(
            &target.connection,
            EngramBindingPreference {
                current: Some(current),
                refused: &[],
            },
            timeout()?,
        )?;
        let Some(binding) = binding else {
            return Err(runtime_observation_scope_changed(
                &job.scope,
                "connection or binding absent",
            ));
        };
        if binding != job.scope.binding {
            return Err(runtime_observation_scope_changed(
                &job.scope,
                &format!("canonical claim {} / {}", binding.work_id, binding.claim_id),
            ));
        }
        let root = target
            .adapter
            .request(
                &target.connection,
                &EngramControlRequest::NamedRootRead {
                    routing_token: routing.to_owned(),
                    run_id: binding.run_id.clone(),
                    claim_id: binding.claim_id.clone(),
                },
                timeout()?,
            )
            .and_then(parse_engram_result::<EngramNamedRootReadResponse>)?;
        if !runtime_observation_same_root(&root.named_root, &job.scope.root) {
            return Err(runtime_observation_scope_changed(
                &job.scope,
                &format!("canonical root {:?}", root.named_root),
            ));
        }
        root.validate_authority(&store, &binding)
            .map_err(|error| EngramTransportError::protocol(error.message))?;
        let root_basis: EngramObservationRootBasis = serde_json::from_value(json!({
            "capture_run_cut": root.read_cut.position,
            "latest_event": root.latest_event.as_ref().map(|event| &event.event),
            "state": root.named_root,
        }))
        .map_err(|error| {
            EngramTransportError::protocol(format!("invalid runtime closing root proof: {error}"))
        })?;
        let (place, provenance) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(session_id).ok_or_else(|| {
                EngramTransportError::local_state("runtime closing session disappeared")
            })?;
            let record = &inner.sessions[index];
            if Self::runtime_observation_capture_waits_for_binding(record) {
                return Ok(EngramRuntimeClosingOutcome::BindingRecoveryPending);
            }
            if !target
                .source_root_read_fence
                .still_current(&inner, &store, &binding)
                || !target
                    .source_root_read_fence
                    .same_store(&inner, &target.project_id)
                || record.engram.routing_token.as_deref() != Some(routing)
                || record.engram.work_binding.as_ref() != Some(&binding)
            {
                return Err(EngramTransportError::local_state(
                    "runtime closing basis changed during its read",
                ));
            }
            match &root.named_root {
                EngramNamedRootState::Bound {
                    generation,
                    workspace_id,
                    named_at,
                } => {
                    let selected = engram_work_source_root_for_claim(
                        &inner.engram_work_source_roots,
                        &store,
                        &binding.work_id,
                        &binding.claim_id,
                    )
                    .filter(|entry| {
                        entry.root == *workspace_id
                            && entry.generation == *generation as u64
                            && engram_named_at_matches(&entry.named_at, named_at)
                    })
                    .ok_or_else(|| {
                        EngramTransportError::local_state(
                            "runtime closing root has no matching local place",
                        )
                    })?;
                    (
                        EngramTurnSourceRoot::from_entry(selected).place(),
                        EngramRootCapture::Recorded {
                            generation: *generation,
                            state: EngramSourceRootState::Named,
                            workspace_id: workspace_id.clone(),
                        },
                    )
                }
                EngramNamedRootState::None => (
                    EngramBasisPlace::Workdir(record.session.workdir.clone()),
                    EngramRootCapture::Unnamed,
                ),
                EngramNamedRootState::UnboundByRelease { .. } => {
                    // The canonical end remains in root_basis. A workdir
                    // sighting is not relabelled as a measurement of a named
                    // workspace which was released.
                    (
                        EngramBasisPlace::Workdir(record.session.workdir.clone()),
                        EngramRootCapture::Unnamed,
                    )
                }
                EngramNamedRootState::Unknown => {
                    return Err(EngramTransportError::local_state(
                        "runtime closing root is unknown",
                    ));
                }
            }
        };
        let capture_budget = timeout()?;
        #[cfg(test)]
        let capture_started = std::time::Instant::now();
        let measured = self.engram_source_basis_within(&place, capture_budget);
        #[cfg(test)]
        if let Some(line) = unadmitted_capture_diagnostic(
            session_id,
            job.attempts,
            capture_budget,
            capture_started.elapsed(),
            measured.is_some(),
        ) {
            eprintln!("{line}");
        }
        let source_basis = provenance
            .stamp(measured)
            .ok_or_else(|| {
                EngramTransportError::local_state("runtime closing source could not be measured")
            })?;
        let observed_at = unadmitted_observation_timestamp();
        let status = target
            .adapter
            .request(
                &target.connection,
                &EngramControlRequest::SessionStatus {
                    routing_token: routing.to_owned(),
                },
                timeout()?,
            )
            .and_then(parse_engram_result::<EngramSessionStatusResponse>)?;
        let observer = status.session_id.ok_or_else(|| {
            EngramTransportError::protocol(
                "runtime closing status has no producer session identity",
            )
        })?;
        // SessionStatus is identity-only here. No phase is interpreted as
        // permission, and a leftover open grant is never borrowed.
        {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if inner.find_session_index(session_id).is_some_and(|index| {
                Self::runtime_observation_capture_waits_for_binding(&inner.sessions[index])
            }) {
                return Ok(EngramRuntimeClosingOutcome::BindingRecoveryPending);
            }
            job.scope.require_current(&inner, session_id)?;
            let current =
                Self::engram_binding_target_for_session_shape_locked(&inner, session_id, true)
                    .map_err(EngramTransportError::local_state)?;
            if current.as_ref().is_none_or(|current| {
                current.connection != target.connection
                    || current.work_binding.as_ref() != Some(&binding)
                    || current.routing_token.as_deref() != Some(routing)
            }) || !target
                .source_root_read_fence
                .same_store(&inner, &target.project_id)
                || !target
                    .source_root_read_fence
                    .still_current(&inner, &store, &binding)
            {
                return Err(EngramTransportError::local_state(
                    "runtime closing capture lost its exact scope",
                ));
            }
        }
        EngramRuntimeObservationReport::prepare(
            &job.trace,
            EngramRuntimeClosingCapture {
                binding,
                root_basis,
                sighting: EngramMeasuredSighting {
                    source_basis,
                    observed_at,
                },
                baseline: job.baseline.clone(),
                policy: policy.clone(),
            },
            target.connection.clone(),
            store,
            routing,
            observer,
            target.settings.call_timeout(),
        )
        .map(EngramRuntimeClosingOutcome::Report)
    }
}

fn runtime_observation_scope_changed(
    scope: &EngramRuntimeClosingScope,
    current: &str,
) -> EngramTransportError {
    let mut error = EngramTransportError::local_state(format!(
        "not reported: scope changed; pinned [{}], current [{}]",
        scope.label(),
        current
    ));
    error.code = Some("runtime_observation_scope_changed".to_owned());
    error
}

fn runtime_observation_same_root(
    left: &EngramNamedRootState,
    right: &EngramNamedRootState,
) -> bool {
    match (left, right) {
        (
            EngramNamedRootState::Bound {
                workspace_id: a,
                generation: ag,
                named_at: at,
            },
            EngramNamedRootState::Bound {
                workspace_id: b,
                generation: bg,
                named_at: bt,
            },
        ) => a == b && ag == bg && engram_named_at_matches(at, bt),
        _ => left == right,
    }
}
