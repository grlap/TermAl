// Owns queue-only intake behind an ordinary checkpoint or an OFF retained head.
// Does not start admission, alter recovery ownership, or replay any RPC.
fn ordinary_checkpoint_queue_allowed(inner: &StateInner, session_id: &str) -> bool {
    let Some(index) = inner.find_visible_session_index(session_id) else {
        return false;
    };
    let record = &inner.sessions[index];
    if !record.is_local_session()
        || record.session.parent_delegation_id.is_some()
        || record.engram_boot_recovery_pending
        || record.engram.project_reset_in_progress
    {
        return false;
    }
    let Some(project) = engram_project_for_session_locked(inner, session_id) else {
        return false;
    };
    let Some(settings) = project
        .engram
        .as_ref()
        .filter(|settings| settings.is_control_enabled())
    else {
        return false;
    };
    if project.remote_id != LOCAL_REMOTE_ID || inner.engram_project_resets.contains(&project.id) {
        return false;
    }
    let Some(mirror) = record.engram.checkpoint_operation.as_ref() else {
        return false;
    };
    let Some(operation) = selected_checkpoint_operation(&inner.engram_session_obligations, mirror)
    else {
        return false;
    };
    let duty = &operation.protection;
    duty.local_session_id == session_id
        && duty.project_id == project.id
        && duty.kind == EngramSessionObligationKind::ExistingCheckpoint
        && settings.same_admission_settings(&operation.original.settings)
        && engram_obligation_requires_exclusion(inner, duty)
        && !inner
            .engram_session_obligations
            .entries
            .iter()
            .any(|entry| entry.id == duty.id)
        && !inner
            .engram_session_obligations
            .all_protections()
            .any(|entry| {
                entry.id != duty.id
                    && engram_obligation_requires_exclusion(inner, entry)
                    && engram_obligation_matches_session(inner, entry, session_id)
            })
        && certified_checkpoint_replay_target_locked(inner, session_id, None).is_none()
}

#[cfg(test)]
thread_local! {
    static TEST_CONTAINED_QUEUE_AFTER_PARSE: std::cell::RefCell<Option<Box<dyn FnOnce()>>>
        = const { std::cell::RefCell::new(None) };
}

impl AppState {
    // Intake is not admission: this branch accepts only an ordinary local root
    // envelope while its pre-OFF checkpoint remains the sole excluding duty.
    fn queue_ordinary_behind_checkpoint(
        &self,
        session_id: &str,
        request: &SendMessageRequest,
        followup: Option<&DelegationRecord>,
    ) -> Result<bool, ApiError> {
        if followup.is_some() {
            return Ok(false);
        }
        {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if !ordinary_checkpoint_queue_allowed(&inner, session_id) {
                return Ok(false);
            }
        }
        let mut prompt = request.text.trim().to_owned();
        let mut expanded = request
            .expanded_text
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != prompt)
            .map(str::to_owned);
        let attachments = parse_prompt_image_attachments(&request.attachments)?;
        if prompt.is_empty() && attachments.is_empty() {
            return Err(ApiError::bad_request("prompt cannot be empty"));
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        // A completed close falls back to the normal dispatch path, which
        // performs fresh eligibility and runtime preparation. Nothing mutated.
        if !ordinary_checkpoint_queue_allowed(&inner, session_id) {
            return Ok(false);
        }
        let index = inner
            .find_visible_session_index(session_id)
            .expect("eligible root");
        if inner.sessions[index].codex_terminal_identity_unresolved {
            return Err(ApiError::conflict(
                "Codex terminal identity is unresolved; Stop before sending another prompt",
            ));
        }
        if record_has_archived_codex_thread(&inner.sessions[index]) {
            return Err(ApiError::conflict(
                "the current Codex thread is archived; unarchive it before sending another prompt",
            ));
        }
        if inner.sessions[index]
            .codex_delegation_release
            .as_ref()
            .is_some_and(|release| {
                release
                    .outcome
                    .lock()
                    .expect("Codex release mutex poisoned")
                    .is_none()
            })
        {
            return Err(ApiError::conflict(
                "Codex thread cleanup started during prompt admission; retry the prompt",
            ));
        }
        let source = request.source_session_id.as_deref().and_then(|source_id| {
            inner
                .find_session_index(source_id)
                .map(|source_index| {
                    let name = inner.sessions[source_index].session.name.clone();
                    match request.source_mailbox.clone() {
                        Some(mailbox) => {
                            MessageSource::mailbox(source_id.to_owned(), name, mailbox)
                        }
                        None => MessageSource::peer(source_id.to_owned(), name),
                    }
                })
                .or_else(|| {
                    let mailbox = request.source_mailbox.clone()?;
                    let participant = inner.federation.mailbox_directory.get(source_id)?;
                    Some(MessageSource::mailbox(
                        source_id.to_owned(),
                        participant.name.clone(),
                        mailbox,
                    ))
                })
        });
        if let Some(template) =
            orchestrator_template_session_for_runtime_session(&inner, session_id)
        {
            let rendered = build_orchestrator_destination_prompt(
                &inner.sessions[index],
                &template.instructions,
                expanded.as_deref().unwrap_or(&prompt),
            );
            if expanded.is_some() {
                expanded = Some(rendered.clone());
            }
            prompt = rendered;
        }
        let queued_source = if source.as_ref().is_some_and(MessageSource::is_mailbox) {
            QueuedPromptSource::Mailbox
        } else {
            QueuedPromptSource::User
        };
        let mailbox_id = source
            .as_ref()
            .and_then(|source| source.mailbox.as_ref())
            .map(|mailbox| mailbox.mailbox_id.clone());
        let protected_head = inner.sessions[index]
            .engram
            .checkpoint_operation
            .as_ref()
            .and_then(|operation| operation.attempts.last())
            .and_then(|attempt| attempt.queued_prompt_id.clone());
        if let Some(mailbox_id) = mailbox_id {
            let record = inner.session_mut_by_index(index).expect("eligible root");
            let holds = &record.queued_prompt_durability_holds;
            if let Some(existing) = record.queued_prompts.iter_mut().find(|queued| {
                !queued.is_engram_retained()
                    && protected_head.as_ref() != Some(&queued.pending_prompt.id)
                    && !holds.contains_key(&queued.pending_prompt.id)
                    && queued
                        .pending_prompt
                        .source
                        .as_ref()
                        .and_then(|source| source.mailbox.as_ref())
                        .is_some_and(|mailbox| mailbox.mailbox_id == mailbox_id)
            }) {
                let previous = existing
                    .pending_prompt
                    .source
                    .as_ref()
                    .and_then(|source| source.mailbox.as_ref())
                    .map(|mailbox| mailbox.sequence)
                    .unwrap_or_default();
                let incoming = source
                    .as_ref()
                    .and_then(|source| source.mailbox.as_ref())
                    .map(|mailbox| mailbox.sequence)
                    .unwrap_or_default();
                if previous <= incoming {
                    existing.pending_prompt.timestamp = stamp_now();
                    existing.pending_prompt.text = prompt;
                    existing.pending_prompt.expanded_text = expanded;
                    existing.pending_prompt.source = source;
                    existing.source = QueuedPromptSource::Mailbox;
                }
                let id = existing.pending_prompt.id.clone();
                sync_pending_prompts(record);
                self.commit_followup_prompt_locked(&mut inner, None, &id, true)
                    .map_err(|error| {
                        ApiError::internal(format!(
                            "failed to persist mailbox notification: {error:#}"
                        ))
                    })?;
                return Ok(true);
            }
        }
        let id = inner.next_message_id();
        queue_prompt_on_record_with_source(
            inner.session_mut_by_index(index).expect("eligible root"),
            PendingPrompt {
                id: id.clone(),
                timestamp: stamp_now(),
                text: prompt,
                expanded_text: expanded,
                source,
                attachments: attachments.iter().map(|a| a.metadata.clone()).collect(),
                engram_interrupted: false,
                is_engram_retained: false,
            },
            attachments,
            queued_source,
        );
        self.commit_followup_prompt_locked(&mut inner, None, &id, true)
            .map_err(|error| {
                ApiError::internal(format!("failed to persist queued prompt: {error:#}"))
            })?;
        Ok(true)
    }

    fn queue_user_behind_contained_head(
        &self,
        session_id: &str,
        request: &SendMessageRequest,
        followup: Option<&DelegationRecord>,
    ) -> Result<bool, ApiError> {
        if followup.is_some()
            || request.source_session_id.is_some()
            || request.source_mailbox.is_some()
            || ENGRAM_CONTAINMENT_ADMISSION.with(|permits| !permits.borrow().is_empty())
        {
            return Ok(false);
        }
        let (entry_image, owner, fingerprint, project, workdir) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(entry) = engram_session_obligation_locked(&inner, session_id) else {
                return Ok(false);
            };
            let Some(index) = inner.find_visible_session_index(session_id) else {
                return Ok(false);
            };
            let record = &inner.sessions[index];
            let off = engram_project_for_session_locked(&inner, session_id)
                .and_then(|p| p.engram.as_ref())
                .is_some_and(|s| s.enabled && !s.turn_gated_control);
            let Some(head) = record
                .queued_prompts
                .front()
                .filter(|head| head.is_engram_retained())
            else {
                return Ok(false);
            };
            if !off
                || !record.orchestrator_auto_dispatch_blocked
                || !record.is_local_session()
                || record.session.parent_delegation_id.is_some()
                || record.engram.project_reset_in_progress
                || engram_project_for_session_locked(&inner, session_id)
                    .is_some_and(|p| inner.engram_project_resets.contains(&p.id))
                || record.codex_terminal_identity_unresolved
                || record_has_archived_codex_thread(record)
            {
                return Ok(false);
            }
            (
                serde_json::to_value(entry).expect("entry"),
                EngramQueuedAdmissionOwner::capture(record).expect("head"),
                containment_prompt_fingerprint(head),
                engram_project_for_session_locked(&inner, session_id)
                    .expect("project")
                    .clone(),
                record.session.workdir.clone(),
            )
        };
        let mut prompt = request.text.trim().to_owned();
        let mut expanded = request
            .expanded_text
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != prompt)
            .map(str::to_owned);
        let attachments = parse_prompt_image_attachments(&request.attachments)?;
        if prompt.is_empty() && attachments.is_empty() {
            return Err(ApiError::bad_request("prompt cannot be empty"));
        }
        #[cfg(test)]
        TEST_CONTAINED_QUEUE_AFTER_PARSE.with(|hook| {
            let hook = hook.borrow_mut().take();
            if let Some(hook) = hook {
                hook();
            }
        });
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        if !engram_session_obligation_locked(&inner, session_id)
            .is_some_and(|e| serde_json::to_value(e).ok().as_ref() == Some(&entry_image))
            || !owner.matches(&inner.sessions[index])
            || !inner.sessions[index].is_local_session()
            || inner.sessions[index].session.parent_delegation_id.is_some()
            || inner.sessions[index].engram.project_reset_in_progress
            || inner.engram_project_resets.contains(&project.id)
            || inner.sessions[index].session.workdir != workdir
            || inner.sessions[index].session.project_id.as_deref() != Some(project.id.as_str())
            || inner.sessions[index].codex_terminal_identity_unresolved
            || record_has_archived_codex_thread(&inner.sessions[index])
            || !engram_project_for_session_locked(&inner, session_id)
                .is_some_and(|p| project_matches_engram_reset_snapshot(p, &project))
            || !inner.sessions[index]
                .queued_prompts
                .front()
                .is_some_and(|head| {
                    containment_prompt_fingerprint(head) == fingerprint && head.is_engram_retained()
                })
            || !inner.sessions[index].orchestrator_auto_dispatch_blocked
            || !engram_project_for_session_locked(&inner, session_id)
                .and_then(|p| p.engram.as_ref())
                .is_some_and(|s| s.enabled && !s.turn_gated_control)
        {
            return Err(ApiError::conflict(
                "contained queue owner changed before append",
            ));
        }
        if let Some(template) =
            orchestrator_template_session_for_runtime_session(&inner, session_id)
        {
            let rendered = build_orchestrator_destination_prompt(
                &inner.sessions[index],
                &template.instructions,
                expanded.as_deref().unwrap_or(&prompt),
            );
            if expanded.is_some() {
                prompt = rendered.clone();
                expanded = Some(rendered);
            } else {
                prompt = rendered;
            }
        }
        let id = inner.next_message_id();
        queue_prompt_on_record_with_source(
            inner.session_mut_by_index(index).expect("visible session"),
            PendingPrompt {
                engram_interrupted: false,
                is_engram_retained: false,
                attachments: attachments.iter().map(|a| a.metadata.clone()).collect(),
                id: id.clone(),
                timestamp: stamp_now(),
                text: prompt,
                expanded_text: expanded,
                source: None,
            },
            attachments,
            QueuedPromptSource::User,
        );
        self.commit_followup_prompt_locked(&mut inner, None, &id, true)
            .map_err(|error| {
                ApiError::internal(format!("failed persisting the contained queue: {error:#}"))
            })?;
        let image = EngramProjectContainmentFence::capture(
            &inner,
            &project.id,
            &[session_id.to_owned()],
        )
        .map_err(|_| ApiError::conflict("contained queue persistence image is unavailable"))?;
        drop(inner);
        self.confirm_engram_containment(image)?;
        let inner = self.inner.lock().expect("state mutex poisoned");
        if !engram_session_obligation_locked(&inner, session_id)
            .is_some_and(|e| serde_json::to_value(e).ok().as_ref() == Some(&entry_image))
            || !inner
                .find_visible_session_index(session_id)
                .is_some_and(|i| {
                    owner.matches(&inner.sessions[i])
                        && inner.sessions[i].orchestrator_auto_dispatch_blocked
                        && inner.sessions[i]
                            .queued_prompts
                            .iter()
                            .any(|q| q.pending_prompt.id == id)
                })
        {
            return Err(ApiError::conflict(
                "contained queue ownership changed before acknowledgement",
            ));
        }
        Ok(true)
    }
}
