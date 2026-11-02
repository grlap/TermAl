// Shared observation lifecycle after the existing runtime/turn ownership fence.
// Ordinary host-started turns and adopted runtime-started turns use the same
// immutable report owner; foreign and replaced frames never enter its trace.

fn ordinary_runtime_observation_enabled(inner: &StateInner, session_id: &str) -> bool {
    engram_source_root_base_is_enabled_locked(inner, session_id)
        && engram_project_for_session_locked(inner, session_id)
            .and_then(|project| project.engram.as_ref())
            .is_some_and(|settings| settings.execution_observability)
}

fn runtime_observation_opening_root(inner: &StateInner, session_id: &str) -> Option<EngramRuntimeOpeningRoot> {
    let index = inner.find_session_index(session_id)?;
    let binding = inner.sessions[index].engram.work_binding.clone()?;
    let store = engram_project_for_session_locked(inner, session_id)?
        .engram.as_ref()?.authority_store_key.clone()?;
    let source_root = engram_work_source_root_for_claim(
        &inner.engram_work_source_roots, &store, &binding.work_id, &binding.claim_id,
    ).map(EngramTurnSourceRoot::from_entry);
    Some(EngramRuntimeOpeningRoot {
        provenance: engram_root_capture_locked(inner, session_id),
        store, binding, source_root,
    })
}

fn runtime_observation_check_owner(record: &SessionRecord) -> Option<&str> {
    let trace = record.engram.unadmitted_active_turn.as_ref()
        .filter(|trace| trace.generation == record.active_turn_generation)?;
    let opening = trace.opening_root.as_ref()?;
    (record.engram.work_binding.as_ref() == Some(&opening.binding))
        .then_some(trace.host_turn_ref.as_str())
}

fn start_ordinary_runtime_observation(inner: &mut StateInner, index: usize) {
    let session_id = inner.sessions[index].session.id.clone();
    if !ordinary_runtime_observation_enabled(inner, &session_id) {
        return;
    }
    let verified_store = engram_project_for_session_locked(inner, &session_id)
        .and_then(|project| project.engram.as_ref())
        .is_some_and(|settings| settings.authority_store_key.is_some());
    if !verified_store && inner.sessions[index].engram.work_binding.is_none() {
        // No verified store and no attributed claim can own a report. Do not
        // turn a normal chat in an unverified project into failure custody.
        return;
    }
    let opening_root = runtime_observation_opening_root(inner, &session_id);
    let record = &mut inner.sessions[index];
    // A repeated start cannot replace the same trace, and an unfinished
    // predecessor remains owned by its terminal/recovery path rather than
    // silently losing its observed facts to a successor.
    if record.engram.unadmitted_active_turn.is_some() {
        return;
    }
    let Some(prompt) = record.provider_handoff_observations.prepared.as_ref()
        .filter(|prompt| prompt.turn_generation == record.active_turn_generation)
    else {
        return;
    };
    record.engram.unadmitted_active_turn = Some(EngramRuntimeObservedTurn::started(
        prompt.prompt_id.clone(), record.active_turn_generation,
    ));
    record.engram.unadmitted_active_turn.as_mut().expect("installed trace").opening_root = opening_root;
    if record.claude_outstanding.any() {
        mark_runtime_observation_source_uncertain(record);
    }
}

fn mark_runtime_observation_source_uncertain(record: &mut SessionRecord) -> bool {
    let generation = record.active_turn_generation;
    let Some(trace) = record.engram.unadmitted_active_turn.as_mut()
        .filter(|trace| trace.generation == generation)
    else {
        return false;
    };
    if trace.source_attribution_uncertain {
        return false;
    }
    trace.source_attribution_uncertain = true;
    true
}

fn adopt_runtime_observation(
    record: &mut SessionRecord, notice_message_id: &str, generation: u64,
    opening_root: Option<EngramRuntimeOpeningRoot>,
) {
    if record.engram.unadmitted_active_turn.is_some() {
        return;
    }
    record.engram.unadmitted_active_turn = Some(EngramRuntimeObservedTurn::adopted(
        notice_message_id.to_owned(),
        generation,
    ));
    record.engram.unadmitted_active_turn.as_mut().expect("installed trace").opening_root = opening_root;
}

fn observe_runtime_unadmitted_command(
    record: &mut SessionRecord,
    provenance: &EngramObservationProvenance,
    observation: &EngramRecorderObservation<'_>,
) {
    observe_runtime_unadmitted_recognized_command(record, provenance, observation, None, None);
}

fn runtime_unadmitted_trace<'a>(
    record: &'a SessionRecord, provenance: &EngramObservationProvenance,
) -> Option<&'a EngramRuntimeObservedTurn> {
    let trace = record.engram.unadmitted_active_turn.as_ref()?;
    if record.active_turn_generation != trace.generation {
        return None;
    }
    match provenance {
        EngramObservationProvenance::Runtime { token, turn_generation } => {
            if !record.runtime.matches_runtime_token(token)
                || *turn_generation != trace.generation {
                return None;
            }
        }
        // ACP readers carry explicit runtime/turn ownership. Ambient events
        // remain available to the separately fenced Codex adapter only.
        EngramObservationProvenance::Ambient => {
            if !matches!(record.runtime, SessionRuntime::Codex(_)) {
                return None;
            }
        }
        EngramObservationProvenance::Claude(provenance) => {
            if !record.runtime.matches_runtime_token(&provenance.token) {
                return None;
            }
            match provenance.origin {
                ClaudeWorkOrigin::Attempt { turn_generation }
                    if turn_generation == trace.generation => {}
                ClaudeWorkOrigin::Adopted { turn_generation }
                    if turn_generation == trace.generation
                        && record.adopted_claude_turn_generation == Some(trace.generation) => {}
                _ => return None,
            }
        }
    }
    Some(trace)
}

fn observe_runtime_unadmitted_recognized_command(
    record: &mut SessionRecord,
    provenance: &EngramObservationProvenance,
    observation: &EngramRecorderObservation<'_>,
    recognized: Option<&EngramCheckCommand>,
    declared: Option<&EngramDeclaredBinding>,
) {
    if runtime_unadmitted_trace(record, provenance).is_none() { return; }
    let fresh_start = matches!(observation, EngramRecorderObservation::CommandStarted { key, .. }
        if !record.engram.running_command_keys.contains_key(*key));
    if let Some(trace) = record.engram.unadmitted_active_turn.as_mut() {
        trace.observe_recognized(observation, recognized, declared, fresh_start);
    }
}

fn observe_runtime_unadmitted_declared_end(
    record: &mut SessionRecord,
    provenance: &EngramObservationProvenance,
    observation: &EngramRecorderObservation<'_>,
    expected_turn: &str,
    expected_check: &str,
    binding: &EngramDeclaredBinding,
    outcome: Option<EngramExecutionOutcome>,
) {
    observe_runtime_unadmitted_captured_end(record, provenance, observation,
        expected_turn, expected_check, Some(binding), outcome);
}

fn observe_runtime_unadmitted_captured_end(
    record: &mut SessionRecord,
    provenance: &EngramObservationProvenance,
    observation: &EngramRecorderObservation<'_>,
    expected_turn: &str,
    expected_check: &str,
    binding: Option<&EngramDeclaredBinding>,
    outcome: Option<EngramExecutionOutcome>,
) {
    let EngramRecorderObservation::CommandFinished { key, .. } = observation else { return; };
    if runtime_unadmitted_trace(record, provenance).is_none_or(|trace| {
        trace.host_turn_ref != expected_turn || trace.commands.iter().rev()
            .find(|entry| entry.tool_key == *key)
            .is_none_or(|entry| entry.check.host_check_id != expected_check)
    }) { return; }
    if let Some(trace) = record.engram.unadmitted_active_turn.as_mut() {
        trace.observe_with_facts(observation, None, binding, outcome, false);
    }
}

fn runtime_previous_checkpoint(record: &SessionRecord) -> Option<EngramRuntimeCheckpointBasis> {
    record.engram.runtime_checkpoint.clone()
}

impl AppState {
    // Provider handoff uses only the opening captured at turn start. Remote
    // confirmation belongs to the observation owner, never a runtime reader.
    fn prepare_ordinary_opening_root_before_handoff(
        &self, session_id: &str, token: &RuntimeToken, generation: u64,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else { return; };
        let record = &inner.sessions[index];
        if !record.runtime.matches_runtime_token(token)
            || record.active_turn_generation != generation
            || record.session.status != SessionStatus::Active
            || record.runtime_stop_in_progress
            || record.engram.observation_delete_requested
            || session_resource_reset_in_progress(&inner, record)
            || !ordinary_runtime_observation_enabled(&inner, session_id)
            || record.engram.work_binding.is_none()
        { return; }
        if !record.engram.unadmitted_active_turn.as_ref().is_some_and(|trace|
            trace.generation == generation && trace.commands.is_empty() && !trace.opening_prepared
                && record.provider_handoff_observations.prepared.as_ref().is_some_and(|prompt|
                    prompt.turn_generation == generation && prompt.prompt_id == trace.notice_message_id))
        { return; }
        inner.sessions[index].engram.unadmitted_active_turn.as_mut()
            .expect("checked trace").opening_prepared = true;
        if engram_turn_root_capture_locked(&inner, session_id) == EngramRootCapture::Unconfirmed {
            mark_runtime_observation_source_uncertain(&mut inner.sessions[index]);
        }
        // Turn-start's first sweep preceded installation of the captured root.
        engram_mark_checks_overlapped_by(&mut inner, index, EngramWriterAct::Presence);
    }

    // Called only past a terminal path's runtime/generation/stop-owner fence,
    // before it clears those markers. Facts and the job share that path's
    // persistence commit; no RPC or source measurement occurs here.
    fn queue_runtime_observation_close_locked(&self, inner: &mut StateInner, index: usize) {
        self.queue_runtime_observation_close_with_outcome_locked(inner, index, EngramObservedTurnOutcome::Unknown);
    }

    fn queue_runtime_observation_close_with_outcome_locked(
        &self, inner: &mut StateInner, index: usize, outcome: EngramObservedTurnOutcome,
    ) {
        // Retained work can become hazardous without a fresh frame callback.
        // Freeze the source-attribution decision under this same close lock.
        engram_reconcile_claude_consumer(inner, index);
        let record = &inner.sessions[index];
        let Some(mut trace) = record
            .engram
            .unadmitted_active_turn
            .as_ref()
            .filter(|trace| trace.generation == record.active_turn_generation)
            .cloned()
        else {
            return;
        };
        trace.observed_outcome = Some(outcome);
        let session_id = record.session.id.clone();
        let baseline = runtime_previous_checkpoint(record);
        let scope = EngramRuntimeClosingScope::capture(inner, &session_id);
        let record = &mut inner.sessions[index];
        let mut notices = engram_carry_running_codex_checks(record, &trace.host_turn_ref);
        notices.extend(engram_close_unended_checks(
            &mut record.engram, Some(&trace.host_turn_ref), "the ordinary turn's end",
        ));
        record.engram.active_turn_checks.retain(|check| check.end.is_some());
        for notice in notices {
            engram_publish_check_notice(&session_id, &mut record.engram, notice, &mut std::io::stderr());
        }
        record.engram.unadmitted_active_turn = None;
        match scope {
            Ok(scope) => {
                if !record
                    .engram
                    .unadmitted_closing_jobs
                    .iter()
                    .any(|job| job.trace.host_turn_ref == trace.host_turn_ref)
                    && !record
                        .engram
                        .unadmitted_reports
                        .iter()
                        .any(|report| report.host_turn_ref == trace.host_turn_ref)
                {
                    record
                        .engram
                        .unadmitted_closing_jobs
                        .push(EngramRuntimeClosingJob::new(trace, baseline, scope));
                }
                // The kick waits for this lock to be released. Delivery still
                // requires exact SQLite row read-back, even if commit fails.
                let state = self.clone();
                if let Err(error) = std::thread::Builder::new()
                    .name("termal-observation-kick".to_owned())
                    .spawn(move || state.start_runtime_observation_worker(&session_id))
                {
                    eprintln!("engram> observation worker kick failed: {error}");
                }
            }
            Err(reason) => {
                let message = Message::Text {
                    attachments: Vec::new(),
                    id: inner.next_message_id(),
                    timestamp: stamp_now(),
                    author: Author::System,
                    text: format!(
                        "Runtime-started turn not reported to Engram: {reason}. Its checks remain uncredited."
                    ),
                    expanded_text: Some(serde_json::to_string_pretty(&trace)
                        .unwrap_or_else(|_| format!("{trace:#?}"))),
                    source: None,
                };
                push_message_on_record(&mut inner.sessions[index], message);
            }
        }
    }

    fn close_runtime_observation_logged(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        adopted_generation: Option<u64>,
    ) {
        if let Err(error) = self.close_runtime_observation(session_id, token, adopted_generation) {
            eprintln!(
                "engram> runtime observation for {session_id} could not be queued: {error:#}"
            );
        }
    }

    fn close_runtime_observation(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        adopted_generation: Option<u64>,
    ) -> Result<()> {
        let Some(_install) = self.runtime_observation_lifetime.enter("close") else {
            return Ok(());
        };
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return Ok(());
        };
        let record = &inner.sessions[index];
        if !record.runtime.matches_runtime_token(token)
            || record.runtime_stop_in_progress
            || record.adopted_claude_turn_generation != adopted_generation
            || adopted_generation != Some(record.active_turn_generation)
        {
            return Ok(());
        }
        let messages_before = session_message_count(&inner.sessions[index]);
        self.queue_runtime_observation_close_locked(&mut inner, index);
        inner.stamp_session_at_index(index);
        if session_message_count(&inner.sessions[index]) != messages_before {
            self.commit_session_locked(&mut inner, session_id)?;
        } else {
            self.persist_session_internal_locked(&inner, session_id)?;
        }
        Ok(())
    }
}
