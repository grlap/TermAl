// Ordinary delegation queue state and exact parent-scoped Resume.
// Runtime, pause, durability and delivery receipts determine whether an attempt
// is waiting; stored control history supplies no admission or retry authority.

/// What a held child's queue head says about the delegation's attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DelegationChildHold {
    reason: DelegationHoldReason,
    retry_eligible: bool,
    next_retry_at: Option<String>,
    actions: Vec<DelegationHoldAction>,
    detail: String,
    /// The retained queue head the hold is for.
    prompt_id: String,
    /// Automatic provider-capacity retries of this delivery, if scheduled.
    attempts: u32,
    /// First observed pause of this prompt.
    held_since: Option<String>,
}

/// Whether the child's current attempt is held: the child is not executing,
/// its queue is paused and a prompt remains queued. Reads the
/// child only.
fn delegation_child_hold(child: &SessionRecord) -> Option<DelegationChildHold> {
    if matches!(child.session.status, SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping)
        || !child.orchestrator_auto_dispatch_blocked || delegation_child_admitting(child) {
        return None;
    }
    let head = ordinary_queue_position(child)
        .and_then(|position| child.queued_prompts.get(position))
        .or_else(|| child.queued_prompts.front())?;
    let runnable = ordinary_queue_entry_is_runnable(child, head);
    let durable = !queued_prompt_held_for_durability(child, head);
    Some(DelegationChildHold {
        reason: if !durable { DelegationHoldReason::PersistenceUnknown }
            else if !runnable { DelegationHoldReason::DeliveryUnknown }
            else { DelegationHoldReason::Stopped },
        retry_eligible: runnable && durable,
        next_retry_at: None,
        actions: if runnable && durable { vec![DelegationHoldAction::Resume, DelegationHoldAction::Cancel] }
            else { vec![DelegationHoldAction::Cancel] },
        detail: child.session.preview.clone(),
        prompt_id: head.pending_prompt.id.clone(),
        attempts: 0,
        held_since: None,
    })
}

fn delegation_child_admitting(child: &SessionRecord) -> bool {
    child.prompt_deliveries.values().any(|delivery| {
        delivery.disposition == PromptDeliveryDisposition::HandingOff
            && (delivery.turn_generation == Some(child.active_turn_generation)
                || (delivery.turn_generation.is_none() && delivery.resume.is_some()))
    })
}

/// The hold a record carries while its stored status is `running`; none once
/// it is terminal.
fn delegation_current_hold(record: &DelegationRecord) -> Option<&DelegationHold> {
    (record.status == DelegationStatus::Running)
        .then_some(record.attempt.hold.as_ref())
        .flatten()
}

/// Whether a held record's child still holds that same retained prompt, read
/// from the child under the caller's lock. A wait is judged on this, never on
/// the stored hold alone: an admission that took the prompt over (a resume, an
/// automatic retry), or a transition that has not refreshed the record yet,
/// leaves a stored hold the child no longer has.
fn delegation_hold_is_live(inner: &StateInner, record: &DelegationRecord) -> bool {
    let Some(hold) = delegation_current_hold(record) else {
        return false;
    };
    inner
        .find_session_index(&record.child_session_id)
        .and_then(|index| delegation_child_hold(&inner.sessions[index]))
        .is_some_and(|live| live.prompt_id == hold.prompt_id)
}

/// Clears what a terminal record no longer has: a hold and a pending start.
/// The hold generation stays, so a later attempt never reuses one.
fn clear_delegation_attempt_presentation(record: &mut DelegationRecord) {
    record.attempt.hold = None;
    record.attempt.pending_start = false;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DelegationAttemptChange {
    Unchanged,
    /// Only the guidance text changed; saved with the next commit, announced
    /// with nothing.
    DetailOnly,
    /// The public status or the hold changed.
    Changed,
}

/// Brings the attempt state of `delegation_index` up to date with its child.
/// `followup_awaits_first_turn` is the caller's reading of
/// `delegation_followup_awaits_first_turn` for the same record.
fn sync_delegation_attempt_state_locked(
    inner: &mut StateInner,
    delegation_index: usize,
    followup_awaits_first_turn: bool,
) -> DelegationAttemptChange {
    let Some(record) = inner.delegations.get(delegation_index) else {
        return DelegationAttemptChange::Unchanged;
    };
    if record.status != DelegationStatus::Running {
        return DelegationAttemptChange::Unchanged;
    }
    let child = inner
        .find_session_index(&record.child_session_id)
        .map(|index| &inner.sessions[index]);
    let child_hold = child.and_then(delegation_child_hold);
    let pending_start = child_hold.is_none()
        && (followup_awaits_first_turn || child.is_some_and(delegation_child_admitting));
    let previous_status = public_delegation_status(record);
    let previous_hold = record.attempt.hold.clone();
    let previous_generation = record.attempt.hold_generation;

    let next_hold = child_hold.map(|held| {
        let same_hold = previous_hold.as_ref().filter(|previous| {
            previous.prompt_id == held.prompt_id
                && previous.reason == held.reason
                && previous.retry_eligible == held.retry_eligible
                && previous.next_retry_at == held.next_retry_at
                && previous.actions == held.actions
                && previous.attempts == held.attempts
        });
        if let Some(previous) = same_hold {
            return DelegationHold {
                detail: held.detail,
                ..previous.clone()
            };
        }
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let held_since = previous_hold
            .as_ref()
            .filter(|previous| previous.prompt_id == held.prompt_id)
            .map(|previous| previous.held_since.clone())
            .or(held.held_since)
            .unwrap_or_else(|| now.clone());
        DelegationHold {
            reason: held.reason,
            held_since,
            last_activity_at: now,
            generation: previous_generation.saturating_add(1),
            retry_eligible: held.retry_eligible,
            next_retry_at: held.next_retry_at,
            actions: held.actions,
            detail: held.detail,
            prompt_id: held.prompt_id,
            attempts: held.attempts,
        }
    });

    let generation = next_hold
        .as_ref()
        .map_or(previous_generation, |hold| hold.generation);
    let hold_changed = match (&previous_hold, &next_hold) {
        (None, None) => false,
        (Some(previous), Some(next)) => previous.generation != next.generation,
        _ => true,
    };
    let detail_changed = !hold_changed && previous_hold != next_hold;
    let record = &mut inner.delegations[delegation_index];
    let unchanged = !hold_changed && !detail_changed && record.attempt.pending_start == pending_start;
    if unchanged {
        return DelegationAttemptChange::Unchanged;
    }
    record.attempt.hold = next_hold;
    record.attempt.pending_start = pending_start;
    record.attempt.hold_generation = generation;
    let status_changed = public_delegation_status(record) != previous_status;
    inner.mark_delegation_mutated(delegation_index);
    if hold_changed || status_changed {
        DelegationAttemptChange::Changed
    } else {
        DelegationAttemptChange::DetailOnly
    }
}

/// The detail a parent card shows for a held attempt.
fn delegation_hold_card_detail(hold: &DelegationHold) -> String {
    let actions = delegation_hold_actions_label(&hold.actions);
    format!(
        "Held ({}): {} Actions: {actions}.",
        delegation_hold_reason_label(hold.reason),
        hold.detail.trim()
    )
}

fn delegation_hold_reason_label(reason: DelegationHoldReason) -> &'static str {
    match reason {
        DelegationHoldReason::AdmissionDeferred => "admissionDeferred",
        DelegationHoldReason::PersistenceUnknown => "persistenceUnknown",
        DelegationHoldReason::RetryScheduled => "retryScheduled",
        DelegationHoldReason::DeliveryUnknown => "deliveryUnknown",
        DelegationHoldReason::Stopped => "stopped",
    }
}

fn delegation_hold_actions_label(actions: &[DelegationHoldAction]) -> String {
    if actions.is_empty() {
        return "none".to_owned();
    }
    actions
        .iter()
        .map(|action| match action {
            DelegationHoldAction::Resume => "resume",
            DelegationHoldAction::Cancel => "cancel",
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The lifecycle delta for an attempt change of a running record: its public
/// status, and the parent card moved to the matching detail.
fn delegation_attempt_delta_locked(
    inner: &mut StateInner,
    delegation_index: usize,
) -> Option<DelegationLifecycleDelta> {
    let delegation = inner.delegations.get(delegation_index)?.clone();
    let detail = match delegation_current_hold(&delegation) {
        Some(hold) => delegation_hold_card_detail(hold),
        None => delegation_running_detail_locked(inner, &delegation),
    };
    let parent_card_delta = if parent_delegation_card_matches_locked(
        inner,
        &delegation,
        ParallelAgentStatus::Running,
        &detail,
    ) {
        None
    } else {
        update_parent_delegation_card_locked(
            inner,
            &delegation,
            ParallelAgentStatus::Running,
            detail,
        )
    };
    Some(DelegationLifecycleDelta::Updated {
        status: public_delegation_status(&delegation),
        hold: delegation_current_hold(&delegation).cloned(),
        delegation_id: delegation.id,
        updated_at: stamp_now(),
        parent_card_delta,
    })
}

/// The section a held child contributes to a wait's fan-in. It says what the
/// child needs, never that it completed.
fn delegation_wait_hold_section(delegation: &DelegationRecord, hold: &DelegationHold) -> String {
    let next_retry = hold
        .next_retry_at
        .as_deref()
        .map_or_else(String::new, |at| format!("\nNext retry: {at}"));
    format!(
        "### {} (`{}`)\n\nStatus: held (not completed)\nChild session: `{}`\nReason: {}\nHeld since: {}\nLast activity: {}\nHold generation: {}\nRetry eligible: {}{next_retry}\nSupported actions: {}\nDetail: {}\n\nThis child has not produced a result. Resume it with `termal_resume_session` or cancel it with `termal_cancel_session` as its actions allow, then register a new wait with `termal_resume_after_delegations` to be woken by its result.",
        delegation.title,
        delegation.id,
        delegation.child_session_id,
        delegation_hold_reason_label(hold.reason),
        hold.held_since,
        hold.last_activity_at,
        hold.generation,
        hold.retry_eligible,
        delegation_hold_actions_label(&hold.actions),
        hold.detail.trim(),
    )
}

/// How a started turn was left, as a create or follow-up response reports it:
/// the delivery outcome, unless the child holds the attempt now, which wins.
/// A rejected turn is the caller's error.
fn delegation_turn_delivery(
    delivery: TurnDispatchDeliveryOutcome,
    hold: Option<DelegationHold>,
) -> Result<DelegationTurnDelivery, ApiError> {
    let (state, detail) = match delivery {
        TurnDispatchDeliveryOutcome::Delivered => (DelegationTurnDeliveryState::Delivered, None),
        TurnDispatchDeliveryOutcome::Scheduled => (DelegationTurnDeliveryState::Scheduled, None),
        TurnDispatchDeliveryOutcome::Superseded => (DelegationTurnDeliveryState::Superseded, None),
        // An uncertainty the host reported holds the turn even before the
        // child's hold is read; an outcome without one only queued it.
        TurnDispatchDeliveryOutcome::Held { error: Some(error) } => {
            (DelegationTurnDeliveryState::Held, Some(error.message))
        }
        TurnDispatchDeliveryOutcome::Held { error: None } => {
            (DelegationTurnDeliveryState::Queued, None)
        }
        TurnDispatchDeliveryOutcome::Rejected(error) => return Err(error),
    };
    Ok(DelegationTurnDelivery {
        state: if hold.is_some() {
            DelegationTurnDeliveryState::Held
        } else {
            state
        },
        hold,
        detail,
    })
}

impl AppState {
    /// The response of a create that started a delegation's first turn: the
    /// delegation as it stands now, with how that turn was left.
    fn delegation_response_after_turn(
        &self,
        delegation_id: &str,
        delivery: TurnDispatchDeliveryOutcome,
    ) -> Result<DelegationResponse, ApiError> {
        let child_session_id = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .find_delegation_index(delegation_id)
                .map(|index| inner.delegations[index].child_session_id.clone())
                .ok_or_else(|| ApiError::internal("created delegation disappeared"))?
        };
        // A turn queued behind the child never reached the dispatch path that
        // refreshes the attempt state; refresh it here either way.
        self.sync_delegation_attempt_for_child_session(&child_session_id);
        let mut response = self.delegation_response_from_state(delegation_id)?;
        let hold = delegation_current_hold(&response.delegation).cloned();
        response.first_turn = Some(delegation_turn_delivery(delivery, hold)?);
        Ok(response)
    }

    /// The response of a follow-up: the delegation as it stands after its
    /// turn's delivery (never the admission's earlier snapshot), with how that
    /// turn was left and the hold its child reports.
    fn delegation_status_after_followup_turn(
        &self,
        parent_session_id: &str,
        delegation_id: &str,
        delivery: TurnDispatchDeliveryOutcome,
    ) -> Result<DelegationStatusResponse, ApiError> {
        let child_session_id = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index =
                find_parent_delegation_index_locked(&inner, parent_session_id, delegation_id)?;
            inner.delegations[index].child_session_id.clone()
        };
        self.sync_delegation_attempt_for_child_session(&child_session_id);
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = find_parent_delegation_index_locked(&inner, parent_session_id, delegation_id)?;
        let delegation = inner.delegations[index].clone();
        let hold = delegation_current_hold(&delegation).cloned();
        let turn = delegation_turn_delivery(delivery, hold)?;
        Ok(DelegationStatusResponse {
            revision: inner.revision,
            delegation,
            turn: Some(turn),
            server_instance_id: self.server_instance_id.clone(),
        })
    }

    /// Retries a held delegation's retained prompt, when its hold offers
    /// Resume: the child's paused queue resumes exactly that prompt through a
    /// fresh admission. Refused for a hold that offers no Resume (an unknown
    /// delivery, a stop, an unconfirmed settlement) and for a delegation that
    /// is not held. Afterwards the delegation reports `queued` while it
    /// admits, `running` once its provider runs, or `held` again.
    fn resume_delegation(
        &self,
        parent_session_id: &str,
        delegation_id: &str,
    ) -> Result<DelegationStatusResponse, ApiError> {
        let child_session_id = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index =
                find_parent_delegation_index_locked(&inner, parent_session_id, delegation_id)?;
            inner.delegations[index].child_session_id.clone()
        };
        self.sync_delegation_attempt_for_child_session(&child_session_id);
        // The resume is bound to the exact retained head checked here, under
        // the same lock: a successor a concurrent cancellation exposes before
        // the drain starts is never admitted in its place.
        let owner = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index =
                find_parent_delegation_index_locked(&inner, parent_session_id, delegation_id)?;
            let delegation = &inner.delegations[index];
            let Some(hold) = delegation_current_hold(delegation) else {
                return Err(ApiError::conflict(format!(
                    "delegation is {}, not held; there is nothing to resume",
                    delegation_status_label(public_delegation_status(delegation))
                )));
            };
            if !hold.actions.contains(&DelegationHoldAction::Resume) {
                return Err(ApiError::conflict(format!(
                    "this hold ({}) does not offer resume; supported actions: {}. {}",
                    delegation_hold_reason_label(hold.reason),
                    delegation_hold_actions_label(&hold.actions),
                    hold.detail.trim()
                )));
            }
            inner
                .find_session_index(&child_session_id)
                .and_then(|child| QueuedTurnOwner::capture(&inner.sessions[child]))
                .filter(|owner| owner.prompt_id == hold.prompt_id)
                .ok_or_else(|| {
                    ApiError::conflict(
                        "the held prompt is no longer the child's queue head; read the delegation again",
                    )
                })?
        };
        // The resume's own delivery outcome is reported, never one inferred
        // from the session: a Fast-discovery turn is only scheduled until its
        // worker hands it over, and another owner may have taken the head.
        let resumed = self.resume_local_session_queue(&child_session_id, Some(owner));
        self.sync_delegation_attempt_for_child_session(&child_session_id);
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = find_parent_delegation_index_locked(&inner, parent_session_id, delegation_id)?;
        let delegation = inner.delegations[index].clone();
        let hold = delegation_current_hold(&delegation).cloned();
        let delivery = match resumed {
            Ok(Some(delivery)) => delivery,
            // Nothing was promoted: the drain refused the head or another
            // owner holds it now.
            Ok(None) => TurnDispatchDeliveryOutcome::Superseded,
            // The resumed admission held the prompt again: report that hold.
            Err(error) if hold.is_some() => TurnDispatchDeliveryOutcome::Held { error: Some(error) },
            Err(error) => return Err(error),
        };
        let turn = delegation_turn_delivery(delivery, hold)?;
        Ok(DelegationStatusResponse {
            revision: inner.revision,
            delegation,
            turn: Some(turn),
            server_instance_id: self.server_instance_id.clone(),
        })
    }

    /// Brings the delegation whose child is `child_session_id` up to date with
    /// a held or not-yet-started attempt, refreshes the waits on it, and
    /// announces the change. Called off the state lock after the paths that
    /// can hold or release a child's retained prompt: a dispatch, a Stop, a
    /// queued-prompt cancellation, and the abort-retry tick. Never terminalizes
    /// a delegation; the ordinary refresh owns that.
    fn sync_delegation_attempt_for_child_session(&self, child_session_id: &str) {
        let (revision, wait_refresh) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(delegation_id) = inner
                .find_session_index(child_session_id)
                .and_then(|index| inner.sessions[index].session.parent_delegation_id.clone())
            else {
                return;
            };
            let Some(index) = inner.find_delegation_index(&delegation_id) else {
                return;
            };
            if inner.delegations[index].child_session_id != child_session_id {
                return;
            }
            let followup_awaits_first_turn =
                delegation_followup_awaits_first_turn(&inner, &inner.delegations[index]);
            let change =
                sync_delegation_attempt_state_locked(&mut inner, index, followup_awaits_first_turn);
            // A wait on this delegation is judged again even when the attempt
            // did not change: that is the retry of a wake whose commit failed.
            let watched = inner
                .delegation_waits
                .iter()
                .any(|wait| wait.delegation_ids.iter().any(|id| *id == delegation_id));
            if change == DelegationAttemptChange::Unchanged && !watched {
                return;
            }
            let lifecycle_delta = if change != DelegationAttemptChange::Unchanged {
                delegation_attempt_delta_locked(&mut inner, index)
            } else {
                None
            };
            // The waits it satisfies and their parents' queued wakes are one
            // transaction: a failed commit restores them
            // (`commit_followup_wait_refresh_locked`), so a later refresh
            // retries the wake, its dispatch included. The attempt state is
            // not rolled back: memory keeps what the child truly is, as for
            // any failed commit, and the next successful commit saves it.
            let committed =
                match self.commit_followup_wait_refresh_locked(&mut inner, Some(&delegation_id)) {
                    Ok((revision, refresh)) if refresh.did_mutate() => Ok((revision, refresh)),
                    Ok((revision, refresh)) if change == DelegationAttemptChange::Unchanged => {
                        Ok((revision, refresh))
                    }
                    Ok((_, refresh)) => self
                        .commit_locked(&mut inner)
                        .map(|revision| (revision, refresh)),
                    Err(error) => Err(error),
                };
            match committed {
                Ok((revision, wait_refresh)) => {
                    // Events are enqueued under the lock that committed them,
                    // with that revision; delivery to the parent runs after.
                    self.enqueue_delegation_refresh_locked(
                        &inner,
                        revision,
                        lifecycle_delta.as_ref(),
                        &DetachedDelegationChildRuntime::default(),
                        &wait_refresh,
                    );
                    (revision, wait_refresh)
                }
                Err(error) => {
                    eprintln!(
                        "delegation> failed persisting the attempt state of `{delegation_id}`: {error:#}"
                    );
                    self.publish_state_locked(&inner);
                    return;
                }
            }
        };
        self.dispatch_delegation_wait_resumes(revision, wait_refresh.dispatch_parents);
    }
}
