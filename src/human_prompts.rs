// Owns explicit human intake and ordinary delivery beside retained control
// duties. No authority request, old-operation settlement, or tool grant lives
// here. Durable queue provenance survives reload; the live owner qualifies
// only one runtime turn and cannot authorize replay after a restart.
// Old control boot recovery and mailbox reconciliation remain with their own
// owners: neither is proof or admission for this unrelated human envelope.
// Real runtime, reset, Stop and delegation custody still qualify promotion.

#[cfg(test)]
thread_local! {
    static TEST_HUMAN_PROMOTION_CAPTURE_FAILURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static TEST_HUMAN_BEFORE_RUNTIME_RESET: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static TEST_HUMAN_BEFORE_PROMOTION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[derive(Debug)]
struct HumanPromptOwnerChanged;
impl std::fmt::Display for HumanPromptOwnerChanged {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the human runtime-reset owner changed or is fenced")
    }
}
impl std::error::Error for HumanPromptOwnerChanged {}

#[cfg(test)]
type TestHumanDrainHook = Box<dyn FnOnce() -> std::sync::mpsc::Sender<()> + Send>;
#[cfg(test)]
static TEST_HUMAN_DRAIN_CAPTURE_HOOKS: std::sync::LazyLock<
    std::sync::Mutex<HashMap<(usize, String), TestHumanDrainHook>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

#[cfg(test)]
struct TestHumanDrainHookOwner((usize, String));
#[cfg(test)]
impl Drop for TestHumanDrainHookOwner {
    fn drop(&mut self) {
        TEST_HUMAN_DRAIN_CAPTURE_HOOKS
            .lock()
            .unwrap()
            .remove(&self.0);
    }
}
#[cfg(test)]
struct TestHumanDrainCompletion(std::sync::mpsc::Sender<()>);
#[cfg(test)]
impl Drop for TestHumanDrainCompletion {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}
#[cfg(test)]
fn install_test_human_drain_capture_hook(
    state: &AppState,
    session_id: &str,
    hook: TestHumanDrainHook,
) -> TestHumanDrainHookOwner {
    let key = (Arc::as_ptr(&state.inner) as usize, session_id.to_owned());
    assert!(
        TEST_HUMAN_DRAIN_CAPTURE_HOOKS
            .lock()
            .unwrap()
            .insert(key.clone(), hook)
            .is_none()
    );
    TestHumanDrainHookOwner(key)
}
#[cfg(test)]
fn run_test_human_drain_capture_hook(
    state: &AppState,
    session_id: &str,
) -> Option<TestHumanDrainCompletion> {
    let key = (Arc::as_ptr(&state.inner) as usize, session_id.to_owned());
    let hook = TEST_HUMAN_DRAIN_CAPTURE_HOOKS.lock().unwrap().remove(&key);
    hook.map(|hook| TestHumanDrainCompletion(hook()))
}

#[derive(Clone, Debug)]
struct HumanPromptTurn {
    prompt_id: String,
    generation: u64,
    choice_id: Option<String>,
    control_predecessor: HumanControlPredecessor,
}

#[derive(Clone, Debug)]
struct HumanControlPredecessor {
    control_generation: u64,
    turn_generation: u64,
    runtime_episode: String,
}

// This only links a human successor to the original local episode. It is
// never a receipt or evidence of what that episode did remotely.
fn human_turn_descends_from_control_owner(
    record: &SessionRecord,
    control_generation: u64,
    turn_generation: u64,
    runtime_episode: &str,
) -> bool {
    human_prompt_turn_is_current(record)
        && record.human_prompt_turn.as_ref().is_some_and(|owner| {
            owner.control_predecessor.control_generation == control_generation
                && owner.control_predecessor.turn_generation == turn_generation
                && owner.control_predecessor.runtime_episode == runtime_episode
        })
}

/// An explicit new decision about an existing envelope. It does not claim
/// anything about the old provider attempt, and never changes its source.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct HumanQueuedChoice {
    decision_id: String,
    intent: String,
    selected_prompt: PendingPrompt,
    original_source: QueuedPromptSource,
    selected_at: String,
    server_instance_id: String,
    stop_generation: u64,
    pause_epoch: u64,
}

fn prune_human_queue_choices(record: &mut SessionRecord) {
    let queue = &record.queued_prompts;
    let receipts = &record.prompt_deliveries;
    record.human_queue_choices.retain(|id, _| {
        queue.iter().any(|queued| queued.pending_prompt.id == *id) || receipts.contains_key(id)
    });
}

fn human_choice_matches(
    record: &SessionRecord,
    queued: &QueuedPromptRecord,
    instance: &str,
) -> bool {
    queued.source == QueuedPromptSource::User
        && queued.pending_prompt.source.is_none()
        && record
            .human_queue_choices
            .get(&queued.pending_prompt.id)
            .is_some_and(|choice| {
                choice.original_source == queued.source
                    && choice.selected_prompt.id == queued.pending_prompt.id
                    && choice.server_instance_id == instance
                    && choice.stop_generation == record.runtime_stop_generation
                    && choice.pause_epoch == record.auto_dispatch_pause_epoch
                    && choice.intent == receipted_prompt_intent(queued)
            })
}

fn human_choice_control_intent_preserved(
    inner: &StateInner,
    session_id: &str,
    queued: &QueuedPromptRecord,
) -> bool {
    // Ordinary provider handoff is checked separately by the existing
    // receipt's Pending/acknowledged write-ahead states. It is not a prepared
    // control operation to reconcile against a frozen authority image.
    if queued.engram_bind.is_none() && queued.engram_evaluate.is_none() {
        return true;
    }
    inner
        .engram_session_obligations
        .entries
        .iter()
        .filter(|entry| entry.local_session_id == session_id)
        .any(|entry| {
            let Ok(image) =
                serde_json::from_value::<EngramUnsettledControlImage>(entry.frozen_request.clone())
            else {
                return false;
            };
            serde_json::to_value(&image).ok().as_ref() == Some(&entry.frozen_request)
                && image.operation == "unsettled_control_image"
                && image.queued_prompts.iter().any(|original| {
                    original.pending_prompt.id == queued.pending_prompt.id
                        && receipted_prompt_intent(original) == receipted_prompt_intent(queued)
                        && serde_json::to_value(&original.engram_bind).ok()
                            == serde_json::to_value(&queued.engram_bind).ok()
                        && serde_json::to_value(&original.engram_evaluate).ok()
                            == serde_json::to_value(&queued.engram_evaluate).ok()
                })
        })
}

fn explicit_human_request(request: &SendMessageRequest) -> bool {
    request.source_session_id.is_none() && request.source_mailbox.is_none()
}

fn human_prompt_turn_is_current(record: &SessionRecord) -> bool {
    record
        .human_prompt_turn
        .as_ref()
        .is_some_and(|owner| owner.generation == record.active_turn_generation)
}

fn human_prompt_handoff_allowed(record: &SessionRecord, generation: u64) -> bool {
    record.human_prompt_turn.as_ref().is_some_and(|owner| {
        owner.generation == generation
            && generation == record.active_turn_generation
            && record.queued_prompts.iter().any(|queued| {
                queued.pending_prompt.id == owner.prompt_id
                    && queued.pending_prompt.source.is_none()
                    && queued.engram_ordinary_handoff
                    && queued.promoted_message_index.is_some()
                    && match owner.choice_id.as_deref() {
                        None => {
                            queued.source == QueuedPromptSource::Human
                                && queued.engram_bind.is_none()
                                && queued.engram_evaluate.is_none()
                        }
                        Some(decision) => {
                            queued.source == QueuedPromptSource::User
                                && record
                                    .human_queue_choices
                                    .get(&owner.prompt_id)
                                    .is_some_and(|choice| {
                                        choice.decision_id == decision
                                            && choice.intent == receipted_prompt_intent(queued)
                                    })
                                && record.prompt_deliveries.get(&owner.prompt_id).is_some_and(
                                    |delivery| {
                                        delivery.disposition
                                            == PromptDeliveryDisposition::HandingOff
                                            && delivery.turn_generation == Some(generation)
                                            && delivery.write_ahead_acknowledged
                                            && delivery.federation_operation.is_none()
                                    },
                                )
                        }
                    }
            })
    })
}

enum HumanPromptDrain {
    Absent,
    Held,
    Started(StartedQueuedTurn),
}

fn human_queue_entry_is_eligible(
    inner: &StateInner,
    record: &SessionRecord,
    queued: &QueuedPromptRecord,
    instance: &str,
) -> bool {
    (queued.source == QueuedPromptSource::Human
        && queued.pending_prompt.source.is_none()
        && !queued.has_engram_intent()
        && queued.promoted_message_index.is_none())
        || (human_choice_matches(record, queued, instance)
            && human_choice_receipt_ready(record, &queued.pending_prompt.id)
            && human_choice_control_intent_preserved(inner, &record.session.id, queued))
}

fn require_human_runtime_reset_owner(
    inner: &StateInner,
    session_id: &str,
    prompt_id: &str,
    intent: &str,
    instance: &str,
    explicit: bool,
) -> Result<()> {
    let index = inner
        .find_session_index(session_id)
        .ok_or_else(|| anyhow!(HumanPromptOwnerChanged))?;
    let record = &inner.sessions[index];
    let Some(position) = record
        .queued_prompts
        .iter()
        .position(|queued| queued.pending_prompt.id == prompt_id)
    else {
        return Err(anyhow!(HumanPromptOwnerChanged));
    };
    if !human_position_may_start(record, position, explicit)
        || human_start_is_held(inner, index, &record.queued_prompts[position], explicit)
        || record.engram.project_reset_in_progress
        || engram_project_for_session_locked(inner, session_id)
            .is_some_and(|project| inner.engram_project_resets.contains(&project.id))
        || !record.queued_prompts.iter().any(|queued| {
            queued.pending_prompt.id == prompt_id
                && receipted_prompt_intent(queued) == intent
                && human_queue_entry_is_eligible(inner, record, queued, instance)
        })
    {
        return Err(anyhow!(HumanPromptOwnerChanged));
    }
    Ok(())
}

#[derive(Clone)]
struct HumanResetResume {
    prompt_id: String,
    choice: HumanQueuedChoice,
    resume: PromptDeliveryResume,
}

fn capture_human_reset_resume(record: &SessionRecord, prompt_id: &str) -> Option<HumanResetResume> {
    Some(HumanResetResume {
        prompt_id: prompt_id.to_owned(),
        choice: record.human_queue_choices.get(prompt_id)?.clone(),
        resume: record
            .prompt_deliveries
            .get(prompt_id)?
            .resume
            .as_ref()?
            .clone(),
    })
}

fn transfer_human_reset_resume(
    record: &mut SessionRecord,
    saved: &HumanResetResume,
    generation: u64,
) -> bool {
    if record.runtime_stop_generation != generation
        || record.active_turn_generation != saved.resume.active_turn_generation
        || record.engram.dispatch_generation != saved.resume.dispatch_generation
        || record.auto_dispatch_pause_epoch != saved.resume.pause_epoch
        || record.queued_prompts.front().is_none_or(|head| {
            head.pending_prompt.id != saved.prompt_id
                || receipted_prompt_intent(head) != saved.resume.intent
        })
        || record
            .human_queue_choices
            .get(&saved.prompt_id)
            .is_none_or(|choice| {
                serde_json::to_value(choice).ok() != serde_json::to_value(&saved.choice).ok()
            })
        || record
            .prompt_deliveries
            .get(&saved.prompt_id)
            .is_none_or(|delivery| {
                delivery.disposition != PromptDeliveryDisposition::HandingOff
                    || delivery.turn_generation.is_some()
                    || !delivery.write_ahead_acknowledged
                    || delivery.write_ahead != saved.resume.write_ahead
                    || delivery.resume.as_ref() != Some(&saved.resume)
            })
    {
        return false;
    }
    // Only this successful exact reset advances the already accepted local
    // continuation. No external Stop, pause, edit, nonce or choice is replaced.
    record
        .human_queue_choices
        .get_mut(&saved.prompt_id)
        .unwrap()
        .stop_generation = generation;
    record
        .prompt_deliveries
        .get_mut(&saved.prompt_id)
        .unwrap()
        .resume
        .as_mut()
        .unwrap()
        .stop_generation = generation;
    true
}

fn human_position_may_start(record: &SessionRecord, position: usize, explicit: bool) -> bool {
    position == 0
        || record
            .queued_prompts
            .iter()
            .take(position)
            .all(QueuedPromptRecord::is_engram_retained)
        || (explicit
            && record.orchestrator_auto_dispatch_blocked
            && record.queued_prompts.iter().take(position).all(|queued| {
                matches!(
                    queued.source,
                    QueuedPromptSource::Mailbox | QueuedPromptSource::Orchestrator
                )
            }))
}

fn human_start_is_held(
    inner: &StateInner,
    index: usize,
    queued: &QueuedPromptRecord,
    explicit: bool,
) -> bool {
    let record = &inner.sessions[index];
    if let Some(wake) = &record.engram.admission_in_progress {
        wake.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    !record.is_local_session()
        || delegated_child_dispatch_is_blocked_locked(inner, index)
        || matches!(
            record.session.status,
            SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
        )
        || record.runtime_stop_in_progress
        || record.engram.project_reset_in_progress
        || engram_project_for_session_locked(inner, &record.session.id)
            .is_some_and(|project| inner.engram_project_resets.contains(&project.id))
        || record.codex_terminal_identity_unresolved
        || record_has_archived_codex_thread(record)
        || record
            .codex_delegation_release
            .as_ref()
            .is_some_and(|release| {
                release
                    .outcome
                    .lock()
                    .expect("Codex release mutex poisoned")
                    .is_none()
            })
        || record.engram.admission_in_progress.is_some()
        || record.engram.bind_in_progress
        || record.engram.checkpoint_in_progress
        || record.engram.pending_dispatch.is_some()
        || record.engram.source_observation_preparations != 0
        || (record.orchestrator_auto_dispatch_blocked && !explicit)
        || queued_prompt_held_for_durability(record, queued)
}

fn human_control_predecessor(record: &SessionRecord) -> HumanControlPredecessor {
    record
        .human_prompt_turn
        .as_ref()
        .filter(|_| human_prompt_turn_is_current(record))
        .map(|owner| owner.control_predecessor.clone())
        .filter(|prior| prior.control_generation == record.engram.dispatch_generation)
        .unwrap_or_else(|| HumanControlPredecessor {
            control_generation: record.engram.dispatch_generation,
            turn_generation: record.active_turn_generation,
            runtime_episode: format!("{:?}", record.runtime.runtime_token()),
        })
}

impl AppState {
    // Called by explicit user-facing request handlers only. Internal agent,
    // delegation and mailbox dispatch continue through dispatch_turn.
    fn dispatch_human_turn(
        &self,
        session_id: &str,
        request: SendMessageRequest,
    ) -> Result<DispatchTurnResult, ApiError> {
        self.dispatch_human_turn_with_acceptance(session_id, request, &mut None)
    }

    fn dispatch_human_turn_with_acceptance(
        &self,
        session_id: &str,
        request: SendMessageRequest,
        accepted_prompt_id: &mut Option<String>,
    ) -> Result<DispatchTurnResult, ApiError> {
        let _acceptance = self.reserve_direct_requester_acceptance_prompt(session_id, &request)?;
        self.dispatch_human_turn_with_followup(session_id, request, None, accepted_prompt_id)
    }

    fn dispatch_human_turn_with_followup(
        &self,
        session_id: &str,
        request: SendMessageRequest,
        followup: Option<&DelegationRecord>,
        accepted_prompt_id: &mut Option<String>,
    ) -> Result<DispatchTurnResult, ApiError> {
        // Check the original request, never the result of display-name lookup:
        // an unknown/deleted peer must not become an explicit human.
        if !explicit_human_request(&request) {
            return self.dispatch_turn_with_followup(session_id, request, followup);
        }
        if self.remote_session_target(session_id)?.is_some() {
            if followup.is_some() {
                return Err(ApiError::conflict(
                    "remote delegation follow-up is not supported",
                ));
            }
            self.proxy_remote_turn_dispatch_with_origin(
                session_id,
                request,
                Some(ExplicitPromptOrigin::Human),
            )?;
            return Ok(DispatchTurnResult::Queued);
        }
        // A refused relay carrier remains intact, but the existing explicit
        // user-priority policy may activate independent work ahead of it.
        let refused_relay = self
            .revalidate_queued_mailbox_wakeups_before_dispatch(session_id)
            .err()
            .and_then(|_| {
                let inner = self.inner.lock().expect("state mutex poisoned");
                let index = inner.find_session_index(session_id)?;
                let record = &inner.sessions[index];
                record
                    .queued_prompts
                    .front()?
                    .pending_prompt
                    .source
                    .as_ref()?
                    .mailbox
                    .as_ref()
                    .filter(|source| source.mailbox_id.starts_with("relay-mailbox-"))?;
                EngramQueuedAdmissionOwner::capture(record)
            });
        let mut prompt = request.text.trim().to_owned();
        let mut expanded = request
            .expanded_text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty() && *text != prompt)
            .map(str::to_owned);
        let attachments = parse_prompt_image_attachments(&request.attachments)?;
        if prompt.is_empty() && attachments.is_empty() {
            return Err(ApiError::bad_request("prompt cannot be empty"));
        }
        {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if inner
                .find_visible_session_index(session_id)
                .is_some_and(|index| delegated_child_dispatch_is_blocked_locked(&inner, index))
                && !reserved_followup_is_admissible_locked(&inner, session_id, followup)
            {
                return Err(ApiError::conflict(DELEGATION_NO_LONGER_STARTABLE_MESSAGE));
            }
        }
        let review_recut = self.capture_followup_review_recut(followup)?;
        self.prepare_codex_child_followup(session_id)?;
        self.refresh_followup_review_custody(review_recut.as_ref())?;
        let (accepted_id, fence) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            if delegated_child_dispatch_is_blocked_locked(&inner, index)
                && !reserved_followup_is_admissible_locked(&inner, session_id, followup)
            {
                return Err(ApiError::conflict(DELEGATION_NO_LONGER_STARTABLE_MESSAGE));
            }
            if let Some(previous) = followup {
                validate_delegation_followup_child_locked(
                    &inner,
                    session_id,
                    !attachments.is_empty(),
                )?;
                if !reserved_followup_is_admissible_locked(&inner, session_id, followup) {
                    return Err(ApiError::conflict(
                        "delegation changed during human follow-up admission",
                    ));
                }
                self.validate_followup_review_custody_locked(&inner, previous)?;
                acceptance_evaluation_followup_admission_locked(&inner, previous)?;
            }
            let record = &inner.sessions[index];
            if record.codex_terminal_identity_unresolved || record_has_archived_codex_thread(record)
            {
                return Err(ApiError::conflict(
                    "the runtime's terminal identity must be resolved before sending",
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
                    expanded = Some(rendered.clone());
                }
                prompt = rendered;
            }
            let reset_fenced = inner.sessions[index].engram.project_reset_in_progress
                || engram_project_for_session_locked(&inner, session_id)
                    .is_some_and(|project| inner.engram_project_resets.contains(&project.id));
            let intake_before = inner.sessions[index].clone();
            let id = inner.next_message_id();
            let record = &mut inner.sessions[index];
            clear_provider_capacity_retry(record);
            // Preserve ordinary explicit-Send semantics across a settings
            // reset: queue while fenced, then drain without a second click.
            if reset_fenced {
                record.set_auto_dispatch_blocked(false);
            }
            queue_prompt_on_record_with_source(
                record,
                PendingPrompt {
                    id: id.clone(),
                    timestamp: stamp_now(),
                    text: prompt,
                    expanded_text: expanded,
                    source: None,
                    attachments: attachments
                        .iter()
                        .map(|item| item.metadata.clone())
                        .collect(),
                    engram_interrupted: false,
                    is_engram_retained: false,
                },
                attachments,
                QueuedPromptSource::Human,
            );
            if refused_relay
                .as_ref()
                .is_some_and(|owner| owner.matches(record))
                && !queued_head_held_for_durability(record)
                && record.engram.pending_dispatch.is_none()
                && record.engram.admission_in_progress.is_none()
                && record.engram.source_observation_preparations == 0
                && record.engram.active_grant_id.is_none()
                && record.engram.uncertain_grant_id.is_none()
                && record.queued_prompts.front().is_some_and(|head| {
                    !head.is_engram_retained() && head.promoted_message_index.is_none()
                })
            {
                prioritize_user_queued_prompts(record);
            }
            inner.stamp_session_at_index(index);
            if let Err(error) = self.commit_followup_prompt_locked(&mut inner, followup, &id, true)
            {
                // Nothing has been promoted or handed off. An error response
                // leaves the draft with the caller, never an executable copy.
                // The follow-up helper owns its delegation/runtime rollback;
                // only restore the queue there, keeping that cleanup intact.
                if followup.is_none() {
                    inner.sessions[index] = intake_before;
                } else {
                    inner.sessions[index].queued_prompts = intake_before.queued_prompts;
                    sync_pending_prompts(&mut inner.sessions[index]);
                }
                inner.stamp_session_at_index(index);
                let restored = self.persist_internal_locked(&inner);
                self.publish_state_locked(&inner);
                let detail = match restored {
                    Ok(()) => format!("failed to persist human prompt: {error:#}"),
                    Err(restore) => format!(
                        "failed to persist human prompt: {error:#}; failed persisting intake rollback: {restore:#}"
                    ),
                };
                return Err(ApiError::internal(detail));
            }
            // This invocation owns a committed envelope, even if final provider
            // delivery instead attempts an older ordinary FIFO head.
            *accepted_prompt_id = Some(id.clone());
            (id, human_prompt_persistence_fence(&inner, session_id))
        };
        if let Err(error) = fence.and_then(|fence| match fence {
            Some(fence) => self.confirm_engram_containment(fence),
            None => Ok(()),
        }) {
            return Ok(self.retain_accepted_human_start_failure(
                session_id,
                &accepted_id,
                &error.message,
            ));
        }
        let started = match self.start_human_queued_turn(session_id, true) {
            Ok(started) => started,
            Err(error) => {
                return Ok(self.retain_accepted_human_start_failure(
                    session_id,
                    &accepted_id,
                    &format!("{error:#}"),
                ));
            }
        };
        match started {
            HumanPromptDrain::Started(started) => {
                Ok(DispatchTurnResult::DispatchedAfterQueue(started.dispatch))
            }
            HumanPromptDrain::Absent => {
                // A new explicit Send also activates an older dormant FIFO
                // head. It keeps its original gated admission and exact owner;
                // missing historical delivery disposition is never inferred.
                let owner = {
                    let inner = self.inner.lock().expect("state mutex poisoned");
                    inner.find_session_index(session_id).and_then(|index| {
                        let record = &inner.sessions[index];
                        record
                            .queued_prompts
                            .front()
                            .filter(|head| {
                                ordinary_known_unpromoted_head(record, head)
                                    || matches!(
                                        head.source,
                                        QueuedPromptSource::Mailbox
                                            | QueuedPromptSource::Orchestrator
                                    )
                            })
                            .and_then(|_| EngramQueuedAdmissionOwner::capture(record))
                    })
                };
                let started = owner
                    .map(|owner| {
                        self.dispatch_next_queued_turn_for_resume_with_prompt(session_id, owner)
                    })
                    .transpose();
                let started = match started {
                    Ok(started) => started.flatten(),
                    Err(error) => {
                        return Ok(self.retain_accepted_human_start_failure(
                            session_id,
                            &accepted_id,
                            &format!("{error:#}"),
                        ));
                    }
                };
                Ok(match started {
                    Some(started) => DispatchTurnResult::DispatchedAfterQueue(started.dispatch),
                    None => DispatchTurnResult::Queued,
                })
            }
            HumanPromptDrain::Held => Ok(DispatchTurnResult::Queued),
        }
    }

    // Intake has already committed this exact new envelope. Reporting it as
    // unsent would make the UI restore a draft that can be submitted twice.
    // Keep its original id/content and surface a retryable session failure;
    // never disturb a concurrent runtime/Stop owner or old control evidence.
    fn retain_accepted_human_start_failure(
        &self,
        session_id: &str,
        prompt_id: &str,
        reason: &str,
    ) -> DispatchTurnResult {
        eprintln!(
            "human prompt> session={session_id} prompt={prompt_id} retained after start failure: {reason}"
        );
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if let Some(index) = inner.find_visible_session_index(session_id) {
            let record = &mut inner.sessions[index];
            if record.queued_prompts.iter().any(|queued| {
                queued.source == QueuedPromptSource::Human && queued.pending_prompt.id == prompt_id
            }) && !record.runtime_stop_in_progress
                && !matches!(
                    record.session.status,
                    SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
                )
            {
                record.set_auto_dispatch_blocked(true);
                record.session.status = SessionStatus::Error;
                let unsent = record
                    .queued_prompts
                    .iter()
                    .find(|queued| queued.pending_prompt.id == prompt_id)
                    .is_some_and(|queued| {
                        !queued.has_engram_intent() && queued.promoted_message_index.is_none()
                    });
                let recovery = if unsent {
                    "Resume retries the retained prompt."
                } else {
                    "Content retained; delivery confirmation requires recovery before replay."
                };
                record.session.preview =
                    format!("Human prompt queued; start failed: {reason}. {recovery}");
                inner.stamp_session_at_index(index);
                if let Err(error) = self.commit_locked(&mut inner) {
                    eprintln!(
                        "human prompt> session={session_id} prompt={prompt_id} start failed: {reason}; failed recording the retained queue failure: {error:#}"
                    );
                }
            }
        }
        DispatchTurnResult::Queued
    }

    fn start_human_queued_turn(
        &self,
        session_id: &str,
        explicit_resume: bool,
    ) -> Result<HumanPromptDrain> {
        self.start_human_queued_turn_with_selection(session_id, explicit_resume, None)
    }

    fn start_human_queued_turn_with_selection(
        &self,
        session_id: &str,
        explicit_resume: bool,
        mut selection: Option<&mut HumanResumeOwner>,
    ) -> Result<HumanPromptDrain> {
        let selected = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_visible_session_index(session_id) else {
                return Ok(HumanPromptDrain::Absent);
            };
            let record = &inner.sessions[index];
            if selection
                .as_ref()
                .is_some_and(|owner| !owner.matches(record))
            {
                return Err(anyhow!(HumanPromptOwnerChanged));
            }
            let Some(position) = record.queued_prompts.iter().position(|queued| {
                selection
                    .as_ref()
                    .is_none_or(|owner| queued.pending_prompt.id == owner.queue.prompt_id)
                    && human_queue_entry_is_eligible(
                        &inner,
                        record,
                        queued,
                        &self.server_instance_id,
                    )
            }) else {
                return Ok(HumanPromptDrain::Absent);
            };
            let queued = &record.queued_prompts[position];
            if !human_position_may_start(record, position, explicit_resume) {
                return Ok(HumanPromptDrain::Absent);
            }
            if self.host_is_shutting_down()
                || human_start_is_held(&inner, index, queued, explicit_resume)
            {
                return Ok(HumanPromptDrain::Held);
            }
            (
                queued.pending_prompt.id.clone(),
                receipted_prompt_intent(queued),
                human_control_predecessor(record),
            )
        };
        #[cfg(test)]
        if let Some(hook) = TEST_HUMAN_BEFORE_RUNTIME_RESET.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
        if let Err(error) = self.prepare_dedicated_runtime_reset_for_human_off_lock(
            session_id,
            &selected.0,
            &selected.1,
            explicit_resume,
            selection.as_deref_mut(),
        ) {
            if error.downcast_ref::<HumanPromptOwnerChanged>().is_some() {
                return Ok(HumanPromptDrain::Held);
            }
            return Err(error);
        }
        #[cfg(test)]
        if let Some(hook) = TEST_HUMAN_BEFORE_PROMOTION.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
        self.engram_host().turn_starting(session_id);
        let (started, fence) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_visible_session_index(session_id) else {
                return Ok(HumanPromptDrain::Absent);
            };
            let record = &inner.sessions[index];
            if selection
                .as_ref()
                .is_some_and(|owner| !owner.matches(record))
            {
                return Err(anyhow!(HumanPromptOwnerChanged));
            }
            let Some(position) = record.queued_prompts.iter().position(|queued| {
                queued.pending_prompt.id == selected.0
                    && receipted_prompt_intent(queued) == selected.1
                    && human_queue_entry_is_eligible(
                        &inner,
                        record,
                        queued,
                        &self.server_instance_id,
                    )
            }) else {
                // A retained unknown handoff or a choice without delivery
                // evidence owns no executable human turn. It must not hide
                // an independent ordinary head or old-operation recovery.
                return Ok(HumanPromptDrain::Absent);
            };
            // Ordinary FIFO remains intact. A separately retained control
            // owner may not strand the human envelope behind it.
            if position != 0
                && !(explicit_resume
                    && record.orchestrator_auto_dispatch_blocked
                    && record.queued_prompts.iter().take(position).all(|queued| {
                        matches!(
                            queued.source,
                            QueuedPromptSource::Mailbox | QueuedPromptSource::Orchestrator
                        )
                    }))
                && !record
                    .queued_prompts
                    .iter()
                    .take(position)
                    .all(QueuedPromptRecord::is_engram_retained)
            {
                return Ok(HumanPromptDrain::Absent);
            }
            let queued = record.queued_prompts[position].clone();
            let choice_id = (queued.source == QueuedPromptSource::User).then(|| {
                record.human_queue_choices[&queued.pending_prompt.id]
                    .decision_id
                    .clone()
            });
            if self.host_is_shutting_down()
                || human_start_is_held(&inner, index, &queued, explicit_resume)
                || (choice_id.is_none()
                    && (queued.has_engram_intent() || queued.promoted_message_index.is_some()))
            {
                return Ok(HumanPromptDrain::Held);
            }
            if !self.prompt_delivery_gate_locked(&mut inner, index, &queued.pending_prompt.id, true)
            {
                return Ok(HumanPromptDrain::Held);
            }
            let snapshot = inner.sessions[index].capture_queue_promotion_snapshot();
            let control_generation = inner.sessions[index].engram.dispatch_generation;
            // Carry the original episode across this request's successful
            // dedicated reset, never reconstruct it from the cleared runtime.
            if selected.2.control_generation != control_generation {
                return Ok(HumanPromptDrain::Absent);
            }
            let control_predecessor = selected.2;
            let mcp = engram_mcp_runtime_config_for_session_locked(&inner, session_id);
            let host_only = session_is_acceptance_evaluator_child_locked(&inner, session_id);
            // Place only this envelope at the promotion boundary. Every older
            // retained record returns to the same relative position afterwards.
            inner.sessions[index].queued_prompts.remove(position);
            inner.sessions[index]
                .queued_prompts
                .push_front(queued.clone());
            let started = self.start_turn_on_record(
                &mut inner.sessions[index],
                queued.pending_prompt.id.clone(),
                queued.pending_prompt.text.clone(),
                queued.attachments.clone(),
                queued.pending_prompt.expanded_text.clone(),
                None,
                None,
                mcp,
                host_only,
            );
            let mut started = match started {
                Ok(started) => started,
                Err(error) => {
                    inner.sessions[index].queued_prompts.pop_front();
                    let record = &mut inner.sessions[index];
                    record.restore_queue_promotion(
                        snapshot,
                        queued.clone(),
                        &queued.pending_prompt.id,
                    );
                    let restored = record
                        .queued_prompts
                        .pop_front()
                        .expect("restored human prompt");
                    record.queued_prompts.insert(position, restored);
                    sync_pending_prompts(record);
                    return Err(anyhow!(error.message));
                }
            };
            let record = &mut inner.sessions[index];
            // A human opening creates no new control operation or generation.
            // Earlier immutable requests retain their original control owner.
            record.engram.dispatch_generation = control_generation;
            record.human_prompt_turn = Some(HumanPromptTurn {
                prompt_id: queued.pending_prompt.id.clone(),
                generation: record.active_turn_generation,
                choice_id,
                control_predecessor,
            });
            started
                .dispatch
                .set_human_prompt_id(queued.pending_prompt.id.clone());
            // Before any provider enqueue, persist an unknown-handoff marker.
            // A reload must never infer that the human prompt was not sent.
            record
                .queued_prompts
                .front_mut()
                .expect("selected human prompt")
                .engram_ordinary_handoff = true;
            mark_prompt_delivery_promoted(
                record,
                &queued,
                &mut started.dispatch,
                &queued.pending_prompt.id,
                &self.server_instance_id,
            );
            let promoted = record
                .queued_prompts
                .pop_front()
                .expect("selected human prompt");
            record.queued_prompts.insert(position, promoted);
            sync_pending_prompts(record);
            EngramHost::turn_started(&mut inner, index);
            inner.stamp_session_at_index(index);
            let revision = match self.commit_followup_prompt_locked(
                &mut inner,
                None,
                &queued.pending_prompt.id,
                false,
            ) {
                Ok(revision) => revision,
                Err(error) => {
                    let record = &mut inner.sessions[index];
                    record.queued_prompts.remove(position);
                    record.restore_queue_promotion(
                        snapshot,
                        queued.clone(),
                        &queued.pending_prompt.id,
                    );
                    // restore_queue_promotion restores at the front; retain the
                    // original ordering of the independently owned old heads.
                    let restored = record
                        .queued_prompts
                        .pop_front()
                        .expect("restored human prompt");
                    record.queued_prompts.insert(position, restored);
                    sync_pending_prompts(record);
                    inner.stamp_session_at_index(index);
                    let restore_error = self.persist_internal_locked(&inner).err();
                    self.publish_state_locked(&inner);
                    return Err(match restore_error {
                        Some(restore) => anyhow!(
                            "{error:#}; failed to persist restored human queue: {restore:#}"
                        ),
                        None => error,
                    });
                }
            };
            // Keep the sole unsent dispatch even if capture itself fails.
            let fence = human_prompt_persistence_fence(&inner, session_id);
            #[cfg(test)]
            let fence = if TEST_HUMAN_PROMOTION_CAPTURE_FAILURE.with(|flag| flag.replace(false)) {
                Err(ApiError::internal(
                    "scripted human promotion image capture failure",
                ))
            } else {
                fence
            };
            self.publish_started_turn_message_delta(&inner, revision, started.message_delta);
            (
                StartedQueuedTurn {
                    dispatch: started.dispatch,
                    queued,
                },
                fence,
            )
        };
        let confirmation = fence.and_then(|fence| match fence {
            Some(fence) => self.confirm_engram_containment(fence),
            None => Ok(()),
        });
        if let Err(error) = confirmation {
            // This stack still owns dispatch; it has never reached a provider.
            // Use the existing exact runtime/generation terminal owner so a
            // late ACK cannot strand Active or terminalize a successor. The
            // conservative queue marker and old control duty are retained.
            let settlement = self.fail_rejected_turn_delivery(
                session_id,
                started.dispatch.runtime_token(),
                started.dispatch.active_turn_generation(),
                None,
                &error.message,
            );
            // Rejected-delivery terminalization deliberately leaves receipt
            // settlement to its sole unsent dispatch owner, as delivery does.
            self.note_prompt_handoff_outcome(
                session_id,
                Some(started.dispatch.active_turn_generation()),
                PromptDeliveryOutcome::Rejected(error.message.clone()),
                None,
            );
            return Err(match settlement {
                Ok(_) => anyhow!(error.message),
                Err(settlement) => anyhow!(
                    "{}; failed to record unsent human turn: {settlement:#}",
                    error.message
                ),
            });
        }
        Ok(HumanPromptDrain::Started(started))
    }
}

fn human_prompt_persistence_fence(
    inner: &StateInner,
    session_id: &str,
) -> Result<Option<EngramProjectContainmentFence>, ApiError> {
    let Some(project) = engram_project_for_session_locked(inner, session_id)
        .filter(|project| project.engram.is_some())
    else {
        return Ok(None);
    };
    EngramProjectContainmentFence::capture(inner, &project.id, &[session_id.to_owned()])
        .map(Some)
        .map_err(|error| {
            ApiError::internal(format!(
                "human prompt persistence image unavailable: {error:#}"
            ))
        })
}
