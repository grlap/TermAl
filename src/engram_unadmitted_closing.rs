// Closing capture for observation reporting. Call on the observation
// worker after retaining the trace and previous checkpoint endpoint under the
// turn-result lock. A focus switch preserves an exact still-held opening;
// its prospective successor refresh follows installation of the old report.

include!("engram_unadmitted_focus.rs");

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
            Self::engram_source_root_target_locked(&inner, session_id)
                .map_err(EngramTransportError::local_state)?
        };
        let Some(target) = target else {
            return Err(runtime_observation_scope_changed(
                &job.scope,
                "connection or binding absent",
            ));
        };
        let store = target.settings.authority_store_key.clone().ok_or_else(|| {
            EngramTransportError::local_state("runtime closing capture has no verified store")
        })?;
        let deadline = target.budget_clock.now() + target.settings.call_timeout();
        let timeout = || target.rpc_timeout_until(deadline);
        if job.scope.binding.is_none() {
            target
                .adapter
                .read_work_session(&target.connection, &store, timeout()?)?;
            let binding = target.adapter.read_work_binding(
                &target.connection,
                EngramBindingPreference {
                    current: target.work_binding.as_ref(),
                },
                timeout()?,
            )?;
            let Some(binding) = binding else {
                if target.work_binding.is_some() {
                    return Err(runtime_observation_scope_changed(
                        &job.scope,
                        "the original claim association is no longer current",
                    ));
                }
                // An ordinary chat has no work owner to report to. A successful
                // canonical read proving no claim is a quiet terminal result,
                // not a transport failure or a retry/removal obligation.
                return Ok(EngramRuntimeClosingOutcome::Unclaimed);
            };
            if target
                .work_binding
                .as_ref()
                .is_some_and(|expected| expected != &binding)
            {
                return Err(runtime_observation_scope_changed(
                    &job.scope,
                    "the original claim association is no longer current",
                ));
            }
            let inner = self.inner.lock().expect("state mutex poisoned");
            job.scope.require_current(&inner, session_id)?;
            let mut scope =
                EngramRuntimeClosingScope::capture_with_binding(&inner, session_id, Some(&binding))
                    .map_err(EngramTransportError::local_state)?;
            scope.historical_fields = job.scope.historical_fields.clone();
            return Ok(EngramRuntimeClosingOutcome::Binding(scope));
        }
        let current = job
            .scope
            .binding
            .as_ref()
            .expect("ordinary binding read completed");
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
            },
            timeout()?,
        )?;
        let Some(mut binding) = binding else {
            return Err(runtime_observation_scope_changed(
                &job.scope,
                "connection or binding absent",
            ));
        };
        let mut prospective_focus = None;
        if job.scope.binding.as_ref() != Some(&binding) && current.work_id != binding.work_id {
            let held = target
                .adapter
                .read_held_claims(&target.connection, timeout()?)?;
            if select_engram_held_binding(
                held.items.clone(),
                held.omitted,
                EngramBindingPreference {
                    current: Some(current),
                },
            )
            .as_ref()
                != Some(&binding)
            {
                return Err(runtime_observation_scope_changed(
                    &job.scope,
                    &format!(
                        "canonical claim {} / {}; held claims changed during the closing focus read",
                        binding.work_id, binding.claim_id
                    ),
                ));
            }
            if runtime_observation_keeps_opening_scope(job, &held) {
                prospective_focus = Some(binding);
                binding = current.clone();
            }
        }
        if job.scope.binding.as_ref() != Some(&binding) {
            if job
                .scope
                .binding
                .as_ref()
                .is_some_and(|previous| previous.work_id != binding.work_id)
            {
                let refreshed = match self
                    .refresh_runtime_observation_focus(session_id, job, &target, &binding, deadline)
                {
                    Ok(()) => true,
                    Err(error) => {
                        eprintln!(
                            "engram> prospective observation focus refresh refused: {}",
                            error.message
                        );
                        false
                    }
                };
                return Ok(EngramRuntimeClosingOutcome::FocusChanged { binding, refreshed });
            }
            if job
                .scope
                .binding
                .as_ref()
                .is_some_and(|previous| runtime_observation_forward_revision(previous, &binding))
            {
                // Refresh only a later opening. This job still belongs to
                // the old revision and must keep its scope-change refusal.
                if let Err(error) = self
                    .refresh_runtime_observation_focus(session_id, job, &target, &binding, deadline)
                {
                    eprintln!(
                        "engram> prospective observation revision refresh refused: {}",
                        error.message
                    );
                }
            }
            // Other changes to the same work keep their existing refusal
            // and recovery requirements, including a new fence or run.
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
                || record
                    .engram
                    .work_binding
                    .as_ref()
                    .is_some_and(|current| current != &binding)
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
        let measured = self.engram_observation_source_capture(session_id, &place, capture_budget);
        #[cfg(test)]
        if let Some(line) = unadmitted_capture_diagnostic(
            session_id,
            job.attempts,
            capture_budget,
            capture_started.elapsed(),
            matches!(&measured, Ok(Some(_))),
        ) {
            eprintln!("{line}");
        }
        let observed_at = unadmitted_observation_timestamp();
        let (source, source_refusal) = match measured {
            Ok(measured) => {
                let source_basis = provenance.stamp(measured).ok_or_else(|| {
                    EngramTransportError::local_state(
                        "runtime closing source could not be measured",
                    )
                })?;
                (
                    EngramRuntimeClosingSource::Measured(EngramMeasuredSighting {
                        source_basis,
                        observed_at,
                    }),
                    None,
                )
            }
            Err(EngramObservationSourceRefusal::OwnerUnavailable) => (
                EngramRuntimeClosingSource::Unavailable { observed_at },
                None,
            ),
            Err(EngramObservationSourceRefusal::Legacy(
                SourceBasisRefusal::UnsupportedGitlinks,
            )) => (
                EngramRuntimeClosingSource::UnsupportedGitlinks { observed_at },
                Some(SourceBasisRefusal::UnsupportedGitlinks),
            ),
            Err(EngramObservationSourceRefusal::Legacy(refusal)) => {
                let mut error = EngramTransportError::local_state(refusal.to_string());
                error.code = Some("runtime_observation_source_refused".to_owned());
                return Err(error);
            }
        };
        let observer = target
            .adapter
            .read_work_session(&target.connection, &store, timeout()?)?;
        // The ordinary child's identity proves the observing session. No
        // control status, phase or leftover open grant enters this report.
        {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if inner.find_session_index(session_id).is_some_and(|index| {
                Self::runtime_observation_capture_waits_for_binding(&inner.sessions[index])
            }) {
                return Ok(EngramRuntimeClosingOutcome::BindingRecoveryPending);
            }
            job.scope.require_current(&inner, session_id)?;
            let current = Self::engram_source_root_target_locked(&inner, session_id)
                .map_err(EngramTransportError::local_state)?;
            if current.as_ref().is_none_or(|current| {
                current.connection != target.connection
                    || current
                        .work_binding
                        .as_ref()
                        .is_some_and(|current| current != &binding)
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
                source,
                baseline: job.baseline.clone(),
                registered_check_baselines: job.registered_check_baselines.clone(),
                policy: policy.clone(),
            },
            target.connection.clone(),
            store,
            observer,
            target.settings.call_timeout(),
        )
        .map(|report| {
            if let Some(binding) = prospective_focus {
                EngramRuntimeClosingOutcome::ReportWithFocus {
                    report,
                    source_refusal,
                    target: Box::new(target),
                    binding,
                    deadline,
                }
            } else {
                EngramRuntimeClosingOutcome::Report(report, source_refusal)
            }
        })
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
