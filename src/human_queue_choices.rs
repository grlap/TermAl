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
    queue: QueuedTurnOwner,
    selection: HumanPromptSelection,
    intent: String,
    stop_generation: u64,
    pause_epoch: u64,
    source: QueuedPromptSource,
    write_ahead: Option<String>,
}

impl HumanResumeOwner {
    fn capture(record: &SessionRecord, queue: QueuedTurnOwner) -> Option<Self> {
        if !queue.matches(record) {
            return None;
        }
        let position = ordinary_queue_position(record)?;
        let head = record.queued_prompts.get(position)?;
        Some(Self {
            queue,
            selection: HumanPromptSelection {
                prompt: projected_pending_prompts(record).into_iter().nth(position)?,
                session_mutation_stamp: record.mutation_stamp,
            },
            intent: receipted_prompt_intent(head),
            stop_generation: record.runtime_stop_generation,
            pause_epoch: record.auto_dispatch_pause_epoch,
            source: head.source,
            write_ahead: record
                .prompt_deliveries
                .get(&head.pending_prompt.id)
                .map(|receipt| receipt.write_ahead.clone()),
        })
    }

    fn matches_with_stop(&self, record: &SessionRecord, stop_generation: u64) -> bool {
        let mut queue = self.queue.clone();
        queue.stop_generation = stop_generation;
        let position = ordinary_queue_position(record);
        queue.matches(record)
            && record.runtime_stop_generation == stop_generation
            && record.auto_dispatch_pause_epoch == self.pause_epoch
            && position.and_then(|position| record.queued_prompts.get(position)).is_some_and(|head| {
                head.source == self.source
                    && receipted_prompt_intent(head) == self.intent
                    && record
                        .prompt_deliveries
                        .get(&head.pending_prompt.id)
                        .map(|receipt| &receipt.write_ahead)
                        == self.write_ahead.as_ref()
            })
            && projected_pending_prompts(record)
                .get(position.unwrap_or(usize::MAX))
                .is_some_and(|head| {
                    serde_json::to_value(head).ok()
                        == serde_json::to_value(&self.selection.prompt).ok()
                })
    }

    fn matches(&self, record: &SessionRecord) -> bool {
        self.matches_with_stop(record, self.stop_generation)
    }
}

#[cfg(test)]
fn ordinary_known_unpromoted_head(record: &SessionRecord, head: &QueuedPromptRecord) -> bool {
    head.source == QueuedPromptSource::User
        && head.pending_prompt.source.is_none()
        && ordinary_queue_entry_is_runnable(record, head)
        && !record.prompt_deliveries.contains_key(&head.pending_prompt.id)
}

fn selected_ordinary_resume_matches(
    _inner: &StateInner,
    record: &SessionRecord,
    selection: &HumanPromptSelection,
) -> bool {
    ordinary_queue_position(record) == Some(0)
        && projected_pending_prompts(record).first().is_some_and(|head| {
            serde_json::to_value(head).ok() == serde_json::to_value(&selection.prompt).ok()
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
        self.cancel_observation_delete_for_resume(session_id)?;
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
        let (owner, human) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner.find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let record = &inner.sessions[index];
            if record.mutation_stamp != selection.session_mutation_stamp
                || !selected_ordinary_resume_matches(&inner, record, &selection)
            {
                return Err(ApiError::conflict(
                    "the selected queued prompt changed; refresh before resuming",
                ));
            }
            let owner = QueuedTurnOwner::capture(record).ok_or_else(|| ApiError::conflict(
                "the selected prompt has no proven unsent delivery; no replacement was resumed",
            ))?;
            let human = record.queued_prompts.front()
                .filter(|head| head.source == QueuedPromptSource::Human)
                .and_then(|_| HumanResumeOwner::capture(record, owner.clone()));
            (owner, human)
        };
        if let Some(human) = human {
            #[cfg(test)]
            if let Some(hook) = TEST_CURRENT_HUMAN_SELECTED_BEFORE_RESUME.with(|slot| slot.borrow_mut().take()) {
                hook();
            }
            return self.resume_current_selected_human_prompt(session_id, human);
        }
        #[cfg(test)]
        if let Some(hook) = TEST_ORDINARY_SELECTED_BEFORE_RESUME.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
        if let Some(outcome) = self.resume_local_session_queue_inner_with_selection(
            session_id, Some(owner), None, Some(&selection),
        )? {
            outcome.into_public_result()?;
        }
        Ok(self.snapshot())
    }
}
