// Owns the explicit exact-content choice on the existing queue Resume action.
// The choice preserves original provenance and is not historical delivery
// evidence. Only prompt_delivery.rs can establish and fence an unsent attempt.

#[cfg(test)]
thread_local! {
    static TEST_ORDINARY_SELECTED_BEFORE_RESUME: std::cell::RefCell<Option<Box<dyn FnOnce()>>>
        = const { std::cell::RefCell::new(None) };
    static TEST_CURRENT_HUMAN_SELECTED_BEFORE_RESUME: std::cell::RefCell<Option<Box<dyn FnOnce()>>>
        = const { std::cell::RefCell::new(None) };
}

struct HumanResumeOwner {
    queue: EngramQueuedAdmissionOwner,
    selection: HumanPromptSelection,
    intent: String,
    stop_generation: u64,
    pause_epoch: u64,
    source: QueuedPromptSource,
    choice_id: Option<String>,
    write_ahead: Option<String>,
}

impl HumanResumeOwner {
    fn capture(record: &SessionRecord, queue: EngramQueuedAdmissionOwner) -> Option<Self> {
        if !queue.matches(record) {
            return None;
        }
        let head = record.queued_prompts.front()?;
        Some(Self {
            queue,
            selection: HumanPromptSelection {
                prompt: projected_pending_prompts(record).into_iter().next()?,
                session_mutation_stamp: record.mutation_stamp,
            },
            intent: receipted_prompt_intent(head),
            stop_generation: record.runtime_stop_generation,
            pause_epoch: record.auto_dispatch_pause_epoch,
            source: head.source,
            choice_id: record
                .human_queue_choices
                .get(&head.pending_prompt.id)
                .map(|choice| choice.decision_id.clone()),
            write_ahead: record
                .prompt_deliveries
                .get(&head.pending_prompt.id)
                .map(|receipt| receipt.write_ahead.clone()),
        })
    }

    fn matches_with_stop(&self, record: &SessionRecord, stop_generation: u64) -> bool {
        self.queue.matches(record)
            && record.runtime_stop_generation == stop_generation
            && record.auto_dispatch_pause_epoch == self.pause_epoch
            && record.queued_prompts.front().is_some_and(|head| {
                head.source == self.source
                    && receipted_prompt_intent(head) == self.intent
                    && record
                        .human_queue_choices
                        .get(&head.pending_prompt.id)
                        .map(|choice| &choice.decision_id)
                        == self.choice_id.as_ref()
                    && record
                        .prompt_deliveries
                        .get(&head.pending_prompt.id)
                        .map(|receipt| &receipt.write_ahead)
                        == self.write_ahead.as_ref()
            })
            && projected_pending_prompts(record)
                .first()
                .is_some_and(|head| {
                    serde_json::to_value(head).ok()
                        == serde_json::to_value(&self.selection.prompt).ok()
                })
    }

    fn matches(&self, record: &SessionRecord) -> bool {
        self.matches_with_stop(record, self.stop_generation)
    }
}

fn ordinary_known_unpromoted_head(record: &SessionRecord, head: &QueuedPromptRecord) -> bool {
    head.source == QueuedPromptSource::User
        && head.pending_prompt.source.is_none()
        && head.promotion_disposition_known
        && head.promoted_message_index.is_none()
        && !head.has_engram_intent()
        && !head.is_engram_retained()
        && !head.engram_waiting
        && !head.engram_interrupted
        && !head.engram_ordinary_handoff
        && !record
            .prompt_deliveries
            .contains_key(&head.pending_prompt.id)
        && !record
            .human_queue_choices
            .contains_key(&head.pending_prompt.id)
}

fn selected_control_recovery_head(
    inner: &StateInner,
    record: &SessionRecord,
    head: &QueuedPromptRecord,
) -> bool {
    head.source == QueuedPromptSource::User
        && head.pending_prompt.source.is_none()
        && !head.engram_ordinary_handoff
        && !record
            .prompt_deliveries
            .contains_key(&head.pending_prompt.id)
        && (head.engram_bind.is_some()
            || matches!(
                engram_begin_preparation_resume(record),
                EngramBeginPreparationResume::Releasable
            )
            || engram_abort_retry_holds_head(record)
            || head.engram_evaluate.is_some()
            || head.engram_waiting
            || engram_admission_obligation_locked(inner, &record.session.id)
                .is_some_and(|entry| containment_original_admission(entry).is_some())
            || record
                .engram
                .source_observation_gate
                .as_ref()
                .is_some_and(|gate| gate.owns(record)))
}

fn selected_ordinary_resume_matches(
    inner: &StateInner,
    record: &SessionRecord,
    selection: &HumanPromptSelection,
) -> bool {
    record.queued_prompts.front().is_some_and(|head| {
        ordinary_known_unpromoted_head(record, head)
            || selected_control_recovery_head(inner, record, head)
            || head.pending_prompt.source.is_some()
            || matches!(
                head.source,
                QueuedPromptSource::Mailbox | QueuedPromptSource::Orchestrator
            )
    }) && projected_pending_prompts(record)
        .first()
        .is_some_and(|head| {
            let mut current = serde_json::to_value(head).unwrap_or(Value::Null);
            let mut selected = serde_json::to_value(&selection.prompt).unwrap_or(Value::Null);
            // Recovery may clear these presentation flags through its
            // own canonical owner; the immutable selected payload stays.
            for field in ["engramInterrupted", "isEngramRetained"] {
                if let Some(value) = current.as_object_mut() {
                    value.remove(field);
                }
                if let Some(value) = selected.as_object_mut() {
                    value.remove(field);
                }
            }
            current == selected
        })
}

impl AppState {
    fn resume_current_selected_human_prompt(
        &self,
        session_id: &str,
        mut owner: HumanResumeOwner,
    ) -> Result<StateResponse, ApiError> {
        {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if !inner
                .find_visible_session_index(session_id)
                .is_some_and(|index| {
                    owner.matches(&inner.sessions[index])
                        && inner.sessions[index].mutation_stamp
                            == owner.selection.session_mutation_stamp
                })
            {
                return Err(ApiError::conflict(
                    "the selected Human queue owner changed before Resume",
                ));
            }
        }
        if self.resume_off_checkpoint_receipt(session_id)? {
            return Ok(self.snapshot());
        }
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .filter(|index| owner.matches(&inner.sessions[*index]))
                .ok_or_else(|| {
                    ApiError::conflict("the selected Human queue owner changed during Resume")
                })?;
            if clear_provider_capacity_retry(&mut inner.sessions[index]) {
                inner.stamp_session_at_index(index);
                self.commit_locked(&mut inner).map_err(|error| {
                    ApiError::internal(format!(
                        "failed persisting selected Resume capacity cancellation: {error:#}"
                    ))
                })?;
            }
        }
        match self.start_human_queued_turn_with_selection(session_id, true, Some(&mut owner)) {
            Ok(HumanPromptDrain::Started(started)) => {
                deliver_turn_dispatch(self, started.dispatch).into_public_result()?;
                Ok(self.snapshot())
            }
            Ok(HumanPromptDrain::Absent | HumanPromptDrain::Held) => Err(ApiError::conflict(
                "the selected Human prompt changed or is held; Resume did not start a replacement",
            )),
            Err(error) if error.downcast_ref::<HumanPromptOwnerChanged>().is_some() => Err(
                ApiError::conflict("the selected Human queue owner changed during Resume"),
            ),
            Err(error) => Err(ApiError::internal(format!(
                "failed to resume selected Human prompt: {error:#}"
            ))),
        }
    }

    fn resume_selected_human_prompt(
        &self,
        session_id: &str,
        selection: HumanPromptSelection,
    ) -> Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_resume_session_queue_with_request(
                session_id,
                Some(ResumeQueueRequest {
                    origin: Some(ExplicitPromptOrigin::Human),
                    human_choice: Some(selection),
                }),
            );
        }
        let (owner, accepted, decision_id, fence) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let record = &inner.sessions[index];
            let head = record
                .queued_prompts
                .front()
                .ok_or_else(|| {
                    ApiError::conflict("the selected queued prompt is no longer present")
                })?
                .clone();
            let projected = projected_pending_prompts(record)
                .into_iter()
                .next()
                .expect("selected queue head");
            if record.mutation_stamp != selection.session_mutation_stamp
                || serde_json::to_value(&projected).ok()
                    != serde_json::to_value(&selection.prompt).ok()
            {
                return Err(ApiError::conflict(
                    "the selected queued prompt changed; refresh before resuming",
                ));
            }
            if head.source == QueuedPromptSource::Human {
                let owner = HumanResumeOwner::capture(
                    record,
                    EngramQueuedAdmissionOwner::capture(record).expect("selected Human head"),
                )
                .expect("validated selected Human owner");
                drop(inner);
                #[cfg(test)]
                if let Some(hook) =
                    TEST_CURRENT_HUMAN_SELECTED_BEFORE_RESUME.with(|slot| slot.borrow_mut().take())
                {
                    hook();
                }
                return self.resume_current_selected_human_prompt(session_id, owner);
            }
            if ordinary_known_unpromoted_head(record, &head)
                || selected_control_recovery_head(&inner, record, &head)
                || head.pending_prompt.source.is_some()
                || matches!(
                    head.source,
                    QueuedPromptSource::Mailbox | QueuedPromptSource::Orchestrator
                )
            {
                let owner =
                    EngramQueuedAdmissionOwner::capture(record).expect("selected ordinary head");
                drop(inner);
                #[cfg(test)]
                if let Some(hook) =
                    TEST_ORDINARY_SELECTED_BEFORE_RESUME.with(|slot| slot.borrow_mut().take())
                {
                    hook();
                }
                let outcome = self.resume_local_session_queue_inner_with_selection(
                    session_id,
                    Some(owner),
                    None,
                    Some(&selection),
                )?;
                if let Some(outcome) = outcome {
                    outcome.into_public_result()?;
                }
                return Ok(self.snapshot());
            }
            let record = inner.session_mut_by_index(index).expect("selected session");
            let intent = receipted_prompt_intent(&head);
            let reuse = record
                .human_queue_choices
                .get(&head.pending_prompt.id)
                .filter(|choice| {
                    choice.server_instance_id == self.server_instance_id
                        && choice.intent == intent
                        && choice.stop_generation == record.runtime_stop_generation
                        && choice.pause_epoch == record.auto_dispatch_pause_epoch
                })
                .cloned();
            let choice = reuse.unwrap_or_else(|| HumanQueuedChoice {
                decision_id: Uuid::new_v4().to_string(),
                intent: intent.clone(),
                selected_prompt: projected,
                original_source: head.source,
                selected_at: stamp_now(),
                server_instance_id: self.server_instance_id.clone(),
                stop_generation: record.runtime_stop_generation,
                pause_epoch: record.auto_dispatch_pause_epoch,
            });
            let decision_id = choice.decision_id.clone();
            record
                .human_queue_choices
                .insert(head.pending_prompt.id.clone(), choice);
            let owner = EngramQueuedAdmissionOwner::capture(record).expect("selected head");
            let accepted = ReceiptedResumeIdentity {
                prompt_id: head.pending_prompt.id.clone(),
                intent,
                stop_generation: record.runtime_stop_generation,
                pause_epoch: record.auto_dispatch_pause_epoch,
            };
            self.commit_locked(&mut inner).map_err(|error| {
                ApiError::internal(format!(
                    "failed to persist the human queue choice: {error:#}"
                ))
            })?;
            let fence = human_prompt_persistence_fence(&inner, session_id)?;
            (owner, accepted, decision_id, fence)
        };
        if let Some(fence) = fence {
            self.confirm_engram_containment(fence)?;
        }
        let reusable = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let record = &inner.sessions[index];
            let head = record
                .queued_prompts
                .front()
                .filter(|_| owner.matches(record))
                .ok_or_else(|| {
                    ApiError::conflict("the selected queue owner changed before acknowledgement")
                })?;
            if !human_choice_matches(record, head, &self.server_instance_id)
                || !record
                    .human_queue_choices
                    .get(&accepted.prompt_id)
                    .is_some_and(|choice| choice.decision_id == decision_id)
            {
                return Err(ApiError::conflict(
                    "the human queue choice was superseded before acknowledgement",
                ));
            }
            let preparing = record
                .prompt_deliveries
                .get(&accepted.prompt_id)
                .is_some_and(|delivery| {
                    delivery.disposition == PromptDeliveryDisposition::HandingOff
                        && delivery.turn_generation.is_none()
                        && prompt_delivery_resume_holds(record, &accepted.prompt_id)
                });
            human_choice_control_intent_preserved(&inner, session_id, head)
                && (human_choice_receipt_pending(record, &accepted.prompt_id) || preparing)
        };
        if self.resume_off_checkpoint_receipt(session_id)? {
            return Ok(self.snapshot());
        }
        self.cancel_provider_capacity_retry(session_id);
        if !reusable {
            // Record the choice, but do not turn missing historical evidence
            // into permission. A distinct proven Human successor still owns
            // its normal Resume path, independently of this old envelope.
            if let HumanPromptDrain::Started(started) = self
                .start_human_queued_turn(session_id, true)
                .map_err(|error| {
                    ApiError::internal(format!("failed to resume human prompt: {error:#}"))
                })?
            {
                deliver_turn_dispatch(self, started.dispatch).into_public_result()?;
                return Ok(self.snapshot());
            }
            return Err(ApiError::conflict(
                "the human choice was saved, but no matching delivery receipt proves the earlier attempt unsent; the original prompt and unknown delivery are retained",
            ));
        }
        match self.resume_selected_human_receipted_head(session_id, Some(&owner), &accepted) {
            ReceiptedResume::Refused(reason) => Err(ApiError::conflict(reason)),
            ReceiptedResume::Preparing => Ok(self.snapshot()),
            ReceiptedResume::NotReceipted => Err(ApiError::conflict(
                "the selected receipt could not start because its runtime or Stop owner changed; no continuation was admitted",
            )),
        }
    }
}
