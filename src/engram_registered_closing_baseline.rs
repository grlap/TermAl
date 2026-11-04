// Closing fallback from this turn's original registered Git checks. Reuses
// terminal preparation; never captures source, sends evidence or waits for it.

// Off-lock only. Rebuild the ordinary native record from the same bytes the
// terminal disk read identified, without freezing or sending another record.
fn registered_closing_clean_input(registration: &TestRunRegistration, disk: &TestRunDisk) -> bool {
    let directory = FsPath::new(&registration.run_directory);
    let TestRunJson::Parsed(request, _, _) = test_run_read_json(&directory.join("request.json"))
    else {
        return false;
    };
    let TestRunJson::Parsed(results, digest, _) =
        test_run_read_json(&directory.join("results.json"))
    else {
        return false;
    };
    if test_run_registration_identity(&request).ok().as_ref()
        != Some(&registration.request_identity)
        || request.get("runId").and_then(Value::as_str) != Some(registration.run_id.as_str())
        || results.get("runId").and_then(Value::as_str) != Some(registration.run_id.as_str())
        || disk
            .results
            .as_ref()
            .is_none_or(|read| read.digest != digest)
        || !matches!(
            disk.results.as_ref().and_then(|read| read.terminal_state),
            Some(TestRunState::Passed | TestRunState::Failed)
        )
        || results.get("interrupted").and_then(Value::as_bool) == Some(true)
        || registration.monitoring_interrupted_at.is_some()
        || engram_verification_record_producer_refusal(registration, &results).is_some()
        || !engram_verification_record_unclassified_stages(&request, &results).is_empty()
    {
        return false;
    }
    let Some(input) = TestRunCommittedInput::read(&results) else {
        return false;
    };
    if !input.clean_before
        || input.eligible != Some(true)
        || input.outcome != Some(TestRunCommittedInputOutcome::Clean)
        || input.after.as_ref().is_none_or(|after| !after.clean)
    {
        return false;
    }
    let Ok(body) = engram_verification_record_body(registration, &request, &results) else {
        return false;
    };
    match &registration.verification_record {
        None => true,
        Some(TestRunVerificationAck::Rejected { .. }) => false,
        Some(ack) => {
            ack.body_digest() == Some(engram_verification_record_body_digest(&body).as_str())
        }
    }
}

// The caller is already under the terminal transition lock. Every command
// must have one original, current association; truncated rings or ambiguous
// keys therefore leave the prefix unknown rather than selecting a last check.
fn registered_closing_baselines_locked(
    inner: &StateInner,
    index: usize,
    prepared: &[RegisteredCheckNotice],
    scope: &EngramRuntimeClosingScope,
) -> Vec<EngramRuntimeCheckpointBasis> {
    let record = &inner.sessions[index];
    let Some(trace) = record.engram.unadmitted_active_turn.as_ref() else {
        return Vec::new();
    };
    let Some(binding) = scope.binding.as_ref() else {
        return Vec::new();
    };
    let EngramNamedRootState::Bound {
        workspace_id,
        generation,
        named_at,
    } = &scope.root
    else {
        return Vec::new();
    };
    if trace.generation != record.active_turn_generation
        || trace.source_attribution_uncertain
        || trace.commands.is_empty()
        || trace.commands.len() > ENGRAM_TURN_CHECK_LIMIT
        // An unresolved predecessor can still publish a source change after
        // this check. Terminal failures are retained diagnostics, not pending
        // observations; they must not disable later turns' own comparisons.
        || record.engram.unadmitted_closing_jobs.iter().any(|job| job.terminal_failure.is_none())
    {
        return Vec::new();
    }
    let runtime = test_run_runtime_identity(record, &EngramObservationProvenance::Ambient);
    if runtime.is_none() {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for command in &trace.commands {
        if !seen.insert(command.tool_key.as_str())
            || !command.invocation_ended
            || command.launch_only
        {
            return Vec::new();
        }
        let mut checks = record.engram.active_turn_checks.iter().filter(|check| {
            check.host_turn_ref == trace.host_turn_ref
                && check.key == command.tool_key
                && command.recognized_command.as_ref() == Some(&check.command)
        });
        let (Some(check), None) = (checks.next(), checks.next()) else {
            return Vec::new();
        };
        let Some(end) = check.end.as_ref() else {
            return Vec::new();
        };
        if check.interference_end().is_none()
            || check.overlapped
            || check.watcher_fence.is_some()
            || check.fenced_by_outstanding.is_some()
            || check.runtime_output_cut
        {
            return Vec::new();
        }
        let mut notices = prepared.iter().filter(|notice| {
            notice.generation == trace.generation
                && notice.runtime == runtime
                && notice.check.host_turn_ref == check.host_turn_ref
                && notice.check.sequence == check.sequence
                && notice.check.key == check.key
                && notice.check.started_at == check.started_at
                && notice.check.command == check.command
                && notice.check.target.root == check.target.root
                && Arc::ptr_eq(&notice.check.start_basis, &check.start_basis)
                && notice
                    .check
                    .end
                    .as_ref()
                    .is_some_and(|old| Arc::ptr_eq(&old.end_basis, &end.end_basis))
        });
        let (Some(notice), None) = (notices.next(), notices.next()) else {
            return Vec::new();
        };
        let Some(row) = notice.observation.as_ref().filter(|row| {
            inner.test_run_observations.get(&row.execution_id) == Some(*row)
                && row.work_binding.as_ref() == Some(binding)
        }) else {
            return Vec::new();
        };
        let Some(artifact) = row.artifact.as_ref() else {
            return Vec::new();
        };
        let Some(_) = notice.runs.iter().find(|(run, state)| {
            run.run_id == artifact.run_id
                && notice.clean_commit_runs.contains(&run.run_id)
                && matches!(state, Some(TestRunState::Passed | TestRunState::Failed))
                && inner.test_runs.registered.contains(&run.run_id)
                && inner.test_run_registrations.get(&run.run_id) == Some(run)
                && engram_verification_record_row_matches(row, run)
                && run.owning_session_id == record.session.id
                && run.work_binding.as_ref() == Some(binding)
                && run.authority_store.as_ref() == Some(&scope.store)
                && run.named_root.as_ref().is_some_and(|root| {
                    root.workspace_id == *workspace_id
                        && i64::try_from(root.generation).ok() == Some(*generation)
                })
        }) else {
            return Vec::new();
        };
        // Registration identity was checked off-lock and again above. Only
        // existing, ready content measurements supply endpoints, never the
        // registered body's Git revision or a new read at close.
        let Some((Some(before), before_at)) = check.start_basis.measured_value() else {
            return Vec::new();
        };
        let Some((Some(after), after_at)) = end.end_basis.measured_value() else {
            return Vec::new();
        };
        let valid_source = |source: &EngramExecutionSourceBasis| {
            source.workspace_id == *workspace_id
                && source.source_root_generation == Some(*generation)
                && source.source_root_state == Some(EngramSourceRootState::Named)
                && source.source_revision.starts_with("content-v1:")
        };
        let ordered = || -> Option<bool> {
            let start = EngramHost::check_timestamp(&check.started_at)?;
            let before = EngramHost::check_timestamp(&before_at)?;
            let after = EngramHost::check_timestamp(&after_at)?;
            let ended = EngramHost::check_timestamp(row.summary.ended_at.as_deref()?)?;
            let adopted = EngramHost::check_timestamp(&trace.adopted_at)?;
            let named = EngramHost::check_timestamp(named_at)?;
            Some(
                adopted <= before
                    && named <= before
                    && start <= before
                    && before <= after
                    && ended <= after,
            )
        };
        if !valid_source(&before) || !valid_source(&after) || ordered() != Some(true) {
            return Vec::new();
        }
        result.push(EngramRuntimeCheckpointBasis::of_sighting(
            binding.clone(),
            &before,
            &before_at,
        ));
        result.push(EngramRuntimeCheckpointBasis::of_sighting(
            binding.clone(),
            &after,
            &after_at,
        ));
    }
    result
}
