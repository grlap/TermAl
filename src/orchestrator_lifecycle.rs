// Orchestrator instance lifecycle — pause/resume/stop/pending-transition handling.

impl AppState {
    /// Pauses orchestrator instance.
    fn pause_orchestrator_instance(&self, instance_id: &str) -> Result<StateResponse, ApiError> {
        if let Some(target) = self.remote_orchestrator_target(instance_id)? {
            return self.proxy_remote_pause_orchestrator_instance(target);
        }
        let (state, orchestrators) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let instance = inner
                .orchestrator_instances
                .iter_mut()
                .find(|instance| instance.id == instance_id)
                .ok_or_else(|| ApiError::not_found("orchestrator instance not found"))?;
            match instance.status {
                OrchestratorInstanceStatus::Running => {
                    instance.status = OrchestratorInstanceStatus::Paused;
                }
                OrchestratorInstanceStatus::Paused => {
                    return Err(ApiError::conflict("orchestrator is already paused"));
                }
                OrchestratorInstanceStatus::Stopped => {
                    return Err(ApiError::conflict("stopped orchestrators cannot be paused"));
                }
            }
            self.commit_persisted_delta_locked(&mut inner)
                .map_err(|err| {
                    ApiError::internal(format!("failed to persist orchestrator state: {err:#}"))
                })?;
            (
                self.snapshot_from_inner(&inner),
                inner.orchestrator_instances.clone(),
            )
        };
        self.publish_orchestrators_updated(state.revision, orchestrators);
        Ok(state)
    }

    /// Resumes orchestrator instance.
    fn resume_orchestrator_instance(&self, instance_id: &str) -> Result<StateResponse, ApiError> {
        if let Some(target) = self.remote_orchestrator_target(instance_id)? {
            return self.proxy_remote_resume_orchestrator_instance(target);
        }
        let (state, orchestrators) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let instance = inner
                .orchestrator_instances
                .iter_mut()
                .find(|instance| instance.id == instance_id)
                .ok_or_else(|| ApiError::not_found("orchestrator instance not found"))?;
            match instance.status {
                OrchestratorInstanceStatus::Paused => {
                    instance.status = OrchestratorInstanceStatus::Running;
                }
                OrchestratorInstanceStatus::Running => {
                    return Err(ApiError::conflict("orchestrator is already running"));
                }
                OrchestratorInstanceStatus::Stopped => {
                    return Err(ApiError::conflict(
                        "stopped orchestrators cannot be resumed",
                    ));
                }
            }
            self.commit_persisted_delta_locked(&mut inner)
                .map_err(|err| {
                    ApiError::internal(format!("failed to persist orchestrator state: {err:#}"))
                })?;
            (
                self.snapshot_from_inner(&inner),
                inner.orchestrator_instances.clone(),
            )
        };
        self.publish_orchestrators_updated(state.revision, orchestrators);
        self.resume_pending_orchestrator_transitions()
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to resume orchestrator transitions: {err:#}"
                ))
            })?;
        let inner = self.inner.lock().expect("state mutex poisoned");
        Ok(self.snapshot_from_inner(&inner))
    }

    /// Begins orchestrator stop.
    fn begin_orchestrator_stop(&self, instance_id: &str) -> Result<(), ApiError> {
        let mut stopping = self
            .stopping_orchestrator_ids
            .lock()
            .expect("orchestrator stop mutex poisoned");
        if !stopping.insert(instance_id.to_owned()) {
            return Err(ApiError::conflict(
                "orchestrator stop is already in progress",
            ));
        }
        drop(stopping);
        self.stopping_orchestrator_session_ids
            .lock()
            .expect("orchestrator stop session mutex poisoned")
            .insert(instance_id.to_owned(), HashSet::new());

        let result = (|| {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let instance_index = inner
                .orchestrator_instances
                .iter()
                .position(|instance| instance.id == instance_id)
                .ok_or_else(|| ApiError::not_found("orchestrator instance not found"))?;
            if inner.orchestrator_instances[instance_index].status
                == OrchestratorInstanceStatus::Stopped
            {
                return Err(ApiError::conflict("orchestrator is already stopped"));
            }
            let mut active_session_ids = inner.orchestrator_instances[instance_index]
                .session_instances
                .iter()
                .filter_map(|session_instance| {
                    inner
                        .find_session_index(&session_instance.session_id)
                        .and_then(|session_index| {
                            let record = &inner.sessions[session_index];
                            if matches!(
                                record.session.status,
                                SessionStatus::Active | SessionStatus::Approval
                            ) && !matches!(record.runtime, SessionRuntime::None)
                            {
                                Some(session_instance.session_id.clone())
                            } else {
                                None
                            }
                        })
                })
                .collect::<Vec<_>>();
            active_session_ids.sort();
            active_session_ids.dedup();
            inner.orchestrator_instances[instance_index].stop_in_progress = true;
            inner.orchestrator_instances[instance_index].active_session_ids_during_stop =
                Some(active_session_ids);
            inner.orchestrator_instances[instance_index]
                .stopped_session_ids_during_stop
                .clear();
            if let Err(err) = self.persist_internal_locked(&inner) {
                inner.orchestrator_instances[instance_index].stop_in_progress = false;
                inner.orchestrator_instances[instance_index].active_session_ids_during_stop = None;
                return Err(ApiError::internal(format!(
                    "failed to persist orchestrator stop state: {err:#}"
                )));
            }
            Ok(())
        })();
        if let Err(err) = result {
            self.finish_orchestrator_stop(instance_id);
            return Err(err);
        }

        Ok(())
    }

    /// Finishes orchestrator stop.
    fn finish_orchestrator_stop(&self, instance_id: &str) {
        self.stopping_orchestrator_ids
            .lock()
            .expect("orchestrator stop mutex poisoned")
            .remove(instance_id);
        self.stopping_orchestrator_session_ids
            .lock()
            .expect("orchestrator stop session mutex poisoned")
            .remove(instance_id);
    }

    /// Records stopped orchestrator session.
    fn note_stopped_orchestrator_session(&self, instance_id: &str, session_id: &str) {
        self.stopping_orchestrator_session_ids
            .lock()
            .expect("orchestrator stop session mutex poisoned")
            .entry(instance_id.to_owned())
            .or_default()
            .insert(session_id.to_owned());
    }

    /// Returns the stopping orchestrator IDs snapshot.
    fn stopping_orchestrator_ids_snapshot(&self) -> HashSet<String> {
        self.stopping_orchestrator_ids
            .lock()
            .expect("orchestrator stop mutex poisoned")
            .clone()
    }

    /// Returns the stopping orchestrator session IDs snapshot.
    fn stopping_orchestrator_session_ids_snapshot(&self) -> HashMap<String, HashSet<String>> {
        self.stopping_orchestrator_session_ids
            .lock()
            .expect("orchestrator stop session mutex poisoned")
            .clone()
    }

    /// Prunes pending transitions for stopped orchestrator sessions.
    fn prune_pending_transitions_for_stopped_orchestrator_sessions(
        &self,
        instance_id: &str,
    ) -> Result<(), ApiError> {
        let mut stopping_sessions = self.stopping_orchestrator_session_ids_snapshot();
        let mut stopped_session_ids = stopping_sessions.remove(instance_id).unwrap_or_default();

        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let mut changed = false;
            if let Some(instance_index) = inner
                .orchestrator_instances
                .iter()
                .position(|instance| instance.id == instance_id)
            {
                let effective_stopped_session_ids = stopped_session_ids
                    .iter()
                    .cloned()
                    .chain(
                        inner.orchestrator_instances[instance_index]
                            .stopped_session_ids_during_stop
                            .iter()
                            .cloned(),
                    )
                    .collect::<HashSet<_>>();
                if inner.orchestrator_instances[instance_index].stop_in_progress {
                    inner.orchestrator_instances[instance_index].stop_in_progress = false;
                    changed = true;
                }
                if inner.orchestrator_instances[instance_index]
                    .active_session_ids_during_stop
                    .is_some()
                {
                    inner.orchestrator_instances[instance_index].active_session_ids_during_stop =
                        None;
                    changed = true;
                }
                if !inner.orchestrator_instances[instance_index]
                    .stopped_session_ids_during_stop
                    .is_empty()
                {
                    inner.orchestrator_instances[instance_index]
                        .stopped_session_ids_during_stop
                        .clear();
                    changed = true;
                }
                let dropped_pendings = inner.orchestrator_instances[instance_index]
                    .pending_transitions
                    .iter()
                    .filter(|pending| {
                        effective_stopped_session_ids.contains(&pending.destination_session_id)
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                if !dropped_pendings.is_empty() {
                    for pending in &dropped_pendings {
                        acknowledge_pending_orchestrator_transition(
                            &mut inner,
                            instance_index,
                            pending,
                        );
                    }
                    changed = true;
                }
                stopped_session_ids = effective_stopped_session_ids;
            }

            for session_id in &stopped_session_ids {
                let Some(session_index) = inner.find_session_index(session_id) else {
                    continue;
                };
                let queued_prompt_count = inner.sessions[session_index].queued_prompts.len();
                clear_stopped_orchestrator_queued_prompts(
                    inner
                        .session_mut_by_index(session_index)
                        .expect("session index should be valid"),
                );
                if inner.sessions[session_index].queued_prompts.len() != queued_prompt_count {
                    changed = true;
                }
            }

            if changed {
                self.commit_locked(&mut inner).map_err(|err| {
                    ApiError::internal(format!(
                        "failed to persist orchestrator stop cleanup: {err:#}"
                    ))
                })?;
            }
        }

        Ok(())
    }

    /// Returns whether session not running conflict.
    fn is_session_not_running_conflict(error: &ApiError) -> bool {
        error.status == StatusCode::CONFLICT
            && error.message == SESSION_NOT_RUNNING_CONFLICT_MESSAGE
    }

    /// Stops orchestrator instance.
    fn stop_orchestrator_instance(&self, instance_id: &str) -> Result<StateResponse, ApiError> {
        if let Some(target) = self.remote_orchestrator_target(instance_id)? {
            return self.proxy_remote_stop_orchestrator_instance(target);
        }
        self.begin_orchestrator_stop(instance_id)?;
        let mut resume_after_abort = false;
        let stop_result = (|| {
            let (session_ids, active_session_ids) = {
                let inner = self.inner.lock().expect("state mutex poisoned");
                let instance_index = inner
                    .orchestrator_instances
                    .iter()
                    .position(|instance| instance.id == instance_id)
                    .ok_or_else(|| ApiError::not_found("orchestrator instance not found"))?;
                if inner.orchestrator_instances[instance_index].status
                    == OrchestratorInstanceStatus::Stopped
                {
                    return Err(ApiError::conflict("orchestrator is already stopped"));
                }

                let session_ids = inner.orchestrator_instances[instance_index]
                    .session_instances
                    .iter()
                    .map(|session| session.session_id.clone())
                    .collect::<Vec<_>>();
                let active_session_ids = session_ids
                    .iter()
                    .filter(|session_id| {
                        inner
                            .find_session_index(session_id)
                            .is_some_and(|session_index| {
                                let record = &inner.sessions[session_index];
                                matches!(
                                    record.session.status,
                                    SessionStatus::Active | SessionStatus::Approval
                                ) && !matches!(record.runtime, SessionRuntime::None)
                            })
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                (session_ids, active_session_ids)
            };
            resume_after_abort = true;

            let mut stop_error = None;
            for session_id in active_session_ids {
                match self.stop_session_with_options(
                    &session_id,
                    StopSessionOptions {
                        dispatch_queued_prompts_on_success: false,
                        orchestrator_stop_instance_id: Some(instance_id.to_owned()),
                    },
                ) {
                    Ok(_) => {}
                    Err(err) => {
                        if Self::is_session_not_running_conflict(&err) {
                            continue;
                        }
                        if stop_error.is_none() {
                            stop_error = Some(err);
                        }
                    }
                }
            }
            if let Some(err) = stop_error {
                return Err(err);
            }

            let (state, orchestrators) = {
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                let instance_index = inner
                    .orchestrator_instances
                    .iter()
                    .position(|instance| instance.id == instance_id)
                    .ok_or_else(|| ApiError::not_found("orchestrator instance not found"))?;
                if inner.orchestrator_instances[instance_index].status
                    == OrchestratorInstanceStatus::Stopped
                {
                    return Err(ApiError::conflict("orchestrator is already stopped"));
                }

                {
                    let instance = &mut inner.orchestrator_instances[instance_index];
                    instance.status = OrchestratorInstanceStatus::Stopped;
                    instance.pending_transitions.clear();
                    instance.error_message = None;
                    instance.completed_at = Some(stamp_orchestrator_template_now());
                    instance.stop_in_progress = false;
                    instance.active_session_ids_during_stop = None;
                    instance.stopped_session_ids_during_stop.clear();
                }
                for session_id in &session_ids {
                    let Some(session_index) = inner.find_session_index(session_id) else {
                        continue;
                    };
                    clear_stopped_orchestrator_queued_prompts(
                    inner
                        .session_mut_by_index(session_index)
                        .expect("session index should be valid"),
                );
                }
                self.commit_persisted_delta_locked(&mut inner)
                    .map_err(|err| {
                        ApiError::internal(format!("failed to persist orchestrator state: {err:#}"))
                    })?;
                (
                    self.snapshot_from_inner(&inner),
                    inner.orchestrator_instances.clone(),
                )
            };
            self.publish_orchestrators_updated(state.revision, orchestrators);
            Ok(state)
        })();
        if stop_result.is_err() && resume_after_abort {
            if let Err(err) =
                self.prune_pending_transitions_for_stopped_orchestrator_sessions(instance_id)
            {
                eprintln!(
                    "orchestrator stop warning> failed pruning pending transitions after aborted stop `{}`: {err:#?}",
                    instance_id
                );
            }
        }
        self.finish_orchestrator_stop(instance_id);
        if stop_result.is_err() && resume_after_abort {
            if let Err(err) = self.resume_pending_orchestrator_transitions() {
                eprintln!(
                    "orchestrator stop warning> failed resuming pending transitions after aborted stop `{}`: {err:#}",
                    instance_id
                );
            }
        }
        stop_result
    }

    /// Resumes pending orchestrator transitions.
    fn resume_pending_orchestrator_transitions(&self) -> Result<()> {
        let mut changed = false;
        loop {
            if !self.accept_next_pending_orchestrator_transition()? {
                break;
            }
            changed = true;
        }

        let delta = {
            let stopping_orchestrator_ids = self.stopping_orchestrator_ids_snapshot();
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let deadlocked =
                mark_deadlocked_orchestrator_instances(&mut inner, &stopping_orchestrator_ids);
            if deadlocked {
                self.commit_locked(&mut inner)?;
            }
            if changed || deadlocked {
                Some((inner.revision, inner.orchestrator_instances.clone()))
            } else {
                None
            }
        };
        if let Some((revision, orchestrators)) = delta {
            self.publish_orchestrators_updated(revision, orchestrators);
        }

        Ok(())
    }

    /// Accepts next pending orchestrator transition.
    fn accept_next_pending_orchestrator_transition(&self) -> Result<bool> {
        let stopping_orchestrator_ids = self.stopping_orchestrator_ids_snapshot();
        let dispatch_destination_session_id = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");

            let Some(action) = next_pending_transition_action(&inner, &stopping_orchestrator_ids)
            else {
                return Ok(false);
            };

            match action {
                PendingTransitionAction::Acknowledge {
                    instance_index,
                    pendings,
                } => {
                    for pending in &pendings {
                        acknowledge_pending_orchestrator_transition(
                            &mut inner,
                            instance_index,
                            pending,
                        );
                    }
                    self.commit_locked(&mut inner)?;
                    return Ok(true);
                }
                PendingTransitionAction::Deliver {
                    destination_session_id,
                    destination_template,
                    instance_index,
                    pendings,
                    rendered_prompt,
                } => {
                    let Some(destination_session_index) =
                        inner.find_session_index(&destination_session_id)
                    else {
                        for pending in &pendings {
                            acknowledge_pending_orchestrator_transition(
                                &mut inner,
                                instance_index,
                                pending,
                            );
                        }
                        self.commit_locked(&mut inner)?;
                        return Ok(true);
                    };

                    let final_prompt = build_orchestrator_destination_prompt(
                        &inner.sessions[destination_session_index],
                        destination_template
                            .as_ref()
                            .map(|template| template.instructions.as_str())
                            .unwrap_or(""),
                        &rendered_prompt,
                    );

                    if final_prompt.trim().is_empty() {
                        for pending in &pendings {
                            acknowledge_pending_orchestrator_transition(
                                &mut inner,
                                instance_index,
                                pending,
                            );
                        }
                        self.commit_locked(&mut inner)?;
                        return Ok(true);
                    }

                    let should_dispatch_now = !matches!(
                        inner.sessions[destination_session_index].session.status,
                        SessionStatus::Active | SessionStatus::Approval
                    ) && inner.sessions[destination_session_index]
                        .queued_prompts
                        .is_empty()
                        && !record_has_archived_codex_thread(
                            &inner.sessions[destination_session_index],
                        );
                    let message_id = inner.next_message_id();
                    queue_orchestrator_prompt_on_record(
                        inner
                            .session_mut_by_index(destination_session_index)
                            .expect("session index should be valid"),
                        PendingPrompt {
                            attachments: Vec::new(),
                            id: message_id,
                            timestamp: stamp_now(),
                            text: final_prompt,
                            expanded_text: None,
                        },
                        Vec::new(),
                    );
                    for pending in &pendings {
                        acknowledge_pending_orchestrator_transition(
                            &mut inner,
                            instance_index,
                            pending,
                        );
                    }
                    self.commit_locked(&mut inner)?;

                    should_dispatch_now.then(|| destination_session_id)
                }
            }
        };

        if let Some(destination_session_id) = dispatch_destination_session_id {
            let dispatch = self
                .dispatch_next_queued_turn(&destination_session_id, false)?
                .ok_or_else(|| {
                    anyhow!("queued orchestrator transition prompt disappeared before dispatch")
                })?;
            if let Err(err) = deliver_turn_dispatch(self, dispatch) {
                eprintln!(
                    "orchestrator transition warning> failed to dispatch queued prompt for session `{}`: {}",
                    destination_session_id, err.message
                );
            }
        }

        Ok(true)
    }
}
