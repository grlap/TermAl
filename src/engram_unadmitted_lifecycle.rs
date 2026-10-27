// Narrow hooks called only after the Claude ownership router has established
// that a frame belongs to the adopted current generation. Foreign, replaced,
// ordinary mediated and unadopted segments do not enter this collection.

fn adopt_runtime_observation(record: &mut SessionRecord, notice_message_id: &str, generation: u64) {
    record.engram.unadmitted_active_turn = Some(EngramRuntimeObservedTurn::adopted(
        notice_message_id.to_owned(),
        generation,
    ));
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
    let EngramObservationProvenance::Claude(provenance) = provenance else {
        return None;
    };
    let ClaudeWorkOrigin::Adopted { turn_generation } = provenance.origin else {
        return None;
    };
    if !record.runtime.matches_runtime_token(&provenance.token) {
        return None;
    }
    let trace = record.engram.unadmitted_active_turn.as_ref()?;
    if turn_generation != trace.generation
        || record.adopted_claude_turn_generation != Some(trace.generation)
        || record.active_turn_generation != trace.generation
    {
        return None;
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
    if let Some(trace) = record.engram.unadmitted_active_turn.as_mut() {
        trace.observe_recognized(observation, recognized, declared);
    }
}

fn observe_runtime_unadmitted_declared_end(
    record: &mut SessionRecord,
    provenance: &EngramObservationProvenance,
    observation: &EngramRecorderObservation<'_>,
    expected_turn: &str,
    binding: &EngramDeclaredBinding,
    outcome: Option<EngramExecutionOutcome>,
) {
    if runtime_unadmitted_trace(record, provenance)
        .is_none_or(|trace| trace.host_turn_ref != expected_turn) { return; }
    if let Some(trace) = record.engram.unadmitted_active_turn.as_mut() {
        trace.observe_with_facts(observation, None, Some(binding), outcome);
    }
}

fn runtime_previous_checkpoint(record: &SessionRecord) -> Option<EngramRuntimeCheckpointBasis> {
    record.engram.runtime_checkpoint.clone()
}

impl AppState {
    // Called only past a terminal path's runtime/generation/stop-owner fence,
    // before it clears those markers. Facts and the job share that path's
    // persistence commit; no RPC or source measurement occurs here.
    fn queue_runtime_observation_close_locked(&self, inner: &mut StateInner, index: usize) {
        let record = &inner.sessions[index];
        let Some(generation) = record
            .adopted_claude_turn_generation
            .filter(|generation| *generation == record.active_turn_generation)
        else {
            return;
        };
        let Some(trace) = record
            .engram
            .unadmitted_active_turn
            .as_ref()
            .filter(|trace| trace.generation == generation)
            .cloned()
        else {
            return;
        };
        let session_id = record.session.id.clone();
        let baseline = runtime_previous_checkpoint(record);
        let scope = EngramRuntimeClosingScope::capture(inner, &session_id);
        let record = &mut inner.sessions[index];
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
                    expanded_text: None,
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
            self.commit_locked(&mut inner)?;
        } else {
            self.persist_internal_locked(&inner)?;
        }
        Ok(())
    }
}
