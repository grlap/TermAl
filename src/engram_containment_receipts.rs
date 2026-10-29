// Owns bounded custody and explicit local accounting for OFF-created checkpoints.
// Shares the ordinary receipt pool; never replays RPCs or admits provider work.

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EngramContainmentReceiptOwner {
    obligation: EngramSessionObligation,
    project: Project,
    host: EngramHostSettings,
    // This compatibility-only in-process field is skipped by Project's serde.
    // Retain it explicitly so runtime identity comparison never defaults it.
    project_work_authority_grant: Option<String>,
    reset_generation: u64,
    active_turn_generation: u64,
    runtime_episode: String,
    work_binding: Option<EngramControlWorkBinding>,
}

fn off_checkpoint_original_matches(
    current: &EngramSessionObligation,
    expected: &EngramSessionObligation,
) -> Result<bool> {
    let mut original = current.clone();
    // These three fields describe local accounting progress, not wire identity.
    original.phase = expected.phase;
    original.reason_code = expected.reason_code.clone();
    original.settlement_evidence = expected.settlement_evidence.clone();
    Ok(serde_json::to_value(original)? == serde_json::to_value(expected)?)
}

fn off_checkpoint_receipt_owner_matches(
    inner: &StateInner,
    owner: &EngramContainmentReceiptOwner,
    response: Option<&Value>,
    initial_flight: bool,
) -> Result<bool> {
    let expected = &owner.obligation;
    let mut project_snapshot = owner.project.clone();
    if let Some(settings) = &mut project_snapshot.engram {
        settings.work_authority_grant = owner.project_work_authority_grant.clone();
    }
    let Some(current) = inner
        .engram_session_obligations
        .entries
        .iter()
        .find(|entry| entry.id == expected.id)
    else {
        return Ok(false);
    };
    if !off_checkpoint_original_matches(current, expected)?
        || inner.preferences.engram != owner.host
        || !inner
            .find_project(&expected.project_id)
            .is_some_and(|project| {
                project_matches_engram_reset_snapshot(project, &project_snapshot)
            })
    {
        return Ok(false);
    }
    let Some(index) = inner.find_session_index(&expected.local_session_id) else {
        return Ok(false);
    };
    let record = &inner.sessions[index];
    let recorded = response.is_some_and(|response| {
        current.phase == EngramSessionObligationPhase::Confirmed
            && current.settlement_evidence
                == json!({"operation":"turn_checkpoint", "response":response})
    });
    Ok(
        record.engram.dispatch_generation == expected.owner_generation
            && record.active_turn_generation == owner.active_turn_generation
            && format!("{:?}", record.runtime.runtime_token()) == owner.runtime_episode
            && record.engram.work_binding == owner.work_binding
            && record.engram.routing_token.as_deref() == Some(expected.routing_token.as_str())
            && (record.engram.active_grant_id.as_deref() == Some(expected.grant_id.as_str())
                || recorded && record.engram.active_grant_id.is_none())
            && record.engram.uncertain_grant_id.is_none()
            && !record.runtime_stop_in_progress
            && !matches!(
                record.session.status,
                SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
            )
            && !record.engram.bind_in_progress
            && record.engram.pending_dispatch.is_none()
            && !record
                .queued_prompts
                .iter()
                .any(QueuedPromptRecord::has_engram_intent)
            && if initial_flight {
                inner
                    .engram_project_resets
                    .is_owned_by(&expected.project_id, owner.reset_generation)
                    && record.engram.project_reset_in_progress
                    && (record.engram.checkpoint_in_progress
                        && record.engram.checkpoint_owner_generation
                            == Some(owner.reset_generation)
                        || recorded && !record.engram.checkpoint_in_progress)
            } else {
                !inner.engram_project_resets.contains(&expected.project_id)
                    && !record.engram.project_reset_in_progress
                    && !record.engram.checkpoint_in_progress
            },
    )
}

fn off_checkpoint_cut_matches(inner: &StateInner, image: &EngramProjectContainmentFence) -> bool {
    EngramProjectContainmentFence::capture(
        inner,
        &image.project_id,
        &image
            .sessions
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
    )
    .is_ok_and(|current| {
        current.settings == image.settings
            && current.obligations == image.obligations
            && current.sessions == image.sessions
    })
}

impl AppState {
    fn reserve_off_checkpoint_receipt(
        &self,
        attempt: &EngramContainmentAttempt,
        generation: u64,
    ) -> std::result::Result<EngramCheckpointReceiptReservation, &'static str> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let entry = inner
            .engram_session_obligations
            .entries
            .iter()
            .find(|entry| entry.id == attempt.obligation_id)
            .ok_or("containment_owner_changed")?
            .clone();
        let project = inner
            .find_project(&entry.project_id)
            .ok_or("containment_owner_changed")?
            .clone();
        let index = inner
            .find_session_index(&entry.local_session_id)
            .ok_or("containment_owner_changed")?;
        let record = &inner.sessions[index];
        if entry.kind != EngramSessionObligationKind::CheckpointContainment
            || entry.basis != EngramSessionObligationBasis::CheckpointUnsettled
            || !entry.authority_confirmed
            || entry.phase != EngramSessionObligationPhase::Sent
            || project
                .engram
                .as_ref()
                .is_none_or(|settings| settings.turn_gated_control)
            || entry.connection != attempt.target.connection
            || attempt
                .target
                .project_reset_checkpoint_request()
                .ok()
                .and_then(|request| serde_json::to_value(request).ok())
                != Some(entry.frozen_request.clone())
        {
            return Err("containment_owner_changed");
        }
        let owner = EngramContainmentReceiptOwner {
            project_work_authority_grant: project
                .engram
                .as_ref()
                .and_then(|settings| settings.work_authority_grant.clone()),
            obligation: entry,
            project,
            host: inner.preferences.engram.clone(),
            reset_generation: generation,
            active_turn_generation: record.active_turn_generation,
            runtime_episode: format!("{:?}", record.runtime.runtime_token()),
            work_binding: record.engram.work_binding.clone(),
        };
        if !off_checkpoint_receipt_owner_matches(&inner, &owner, None, true).unwrap_or(false) {
            return Err("containment_owner_changed");
        }
        let identity =
            bounded_checkpoint_json(&owner, 2 * ENGRAM_SESSION_OBLIGATION_REQUEST_MAX_BYTES)
                .map_err(|_| "checkpoint_receipt_capacity_unavailable")?;
        let key = (owner.obligation.id, format!("containment:{generation}"));
        inner
            .engram_pending_checkpoint_receipts
            .reserve_identity(
                key.clone(),
                identity,
                EngramCheckpointReceiptOrigin::OffContainment,
            )
            .map_err(|_| "checkpoint_receipt_capacity_unavailable")?;
        Ok(EngramCheckpointReceiptReservation {
            state: self.clone(),
            key,
        })
    }

    fn retain_off_checkpoint_response(
        &self,
        key: &(String, String),
        response: &Value,
    ) -> Result<()> {
        let encoded = bounded_checkpoint_json(response, ENGRAM_CHECKPOINT_REPLY_MAX_BYTES)?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let identity = inner
            .engram_pending_checkpoint_receipts
            .entries
            .get(key)
            .filter(|entry| entry.origin == EngramCheckpointReceiptOrigin::OffContainment)
            .ok_or_else(|| anyhow!("OFF checkpoint reservation disappeared"))?
            .identity
            .clone();
        inner
            .engram_pending_checkpoint_receipts
            .receive_identity(key, &identity, encoded, true)
    }

    fn note_off_checkpoint_accounting_pending(
        &self,
        attempt: &EngramContainmentAttempt,
        code: &str,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let mut ledger = inner.engram_session_obligations.clone();
        let Some(entry) = ledger.entries.iter_mut().find(|entry| {
            entry.id == attempt.obligation_id
                && entry.owner_generation == attempt.owner_generation
                && entry.connection == attempt.target.connection
                && Some(entry.routing_token.as_str()) == attempt.target.routing_token.as_deref()
                && Some(entry.grant_id.as_str()) == attempt.target.active_grant_id.as_deref()
        }) else {
            return;
        };
        entry.reason_code = code.to_owned();
        if ledger.validate().is_err() {
            return;
        }
        inner.engram_session_obligations = ledger;
        let _ = self.commit_locked(&mut inner);
    }

    fn finish_off_checkpoint_receipt(
        &self,
        key: &(String, String),
        deadline: std::time::Instant,
        initial_flight: bool,
    ) -> Result<(), ApiError> {
        let clock = self.engram_budget_clock();
        let pending_error = || {
            ApiError::conflict(
                "OFF checkpoint receipt accounting remains pending; the original duty stays excluded",
            )
        };
        let (owner, response) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let pending = inner
                .engram_pending_checkpoint_receipts
                .entries
                .get(key)
                .filter(|pending| {
                    pending.origin == EngramCheckpointReceiptOrigin::OffContainment
                        && pending.accepted
                        && pending.accounting
                })
                .ok_or_else(pending_error)?;
            let owner: EngramContainmentReceiptOwner =
                serde_json::from_slice(&pending.identity).map_err(|_| pending_error())?;
            let response: Value =
                serde_json::from_slice(pending.response.as_ref().ok_or_else(pending_error)?)
                    .map_err(|_| pending_error())?;
            (owner, response)
        };
        let session_id = &owner.obligation.local_session_id;
        let project_id = &owner.obligation.project_id;
        let sessions = vec![session_id.clone()];
        // First acknowledge the exact receipt while the live grant is retained.
        // A Confirmed ledger image alone does not lift the effective exclusion.
        let receipt_image = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if !off_checkpoint_receipt_owner_matches(
                &inner,
                &owner,
                Some(&response),
                initial_flight,
            )
            .map_err(|_| pending_error())?
                || clock.now() >= deadline
            {
                return Err(pending_error());
            }
            let mut ledger = inner.engram_session_obligations.clone();
            let entry = ledger
                .entries
                .iter_mut()
                .find(|entry| entry.id == owner.obligation.id)
                .ok_or_else(pending_error)?;
            let evidence = json!({"operation":"turn_checkpoint", "response":response});
            if entry.phase == EngramSessionObligationPhase::Confirmed
                && entry.settlement_evidence != evidence
            {
                return Err(pending_error());
            }
            let changed = entry.phase != EngramSessionObligationPhase::Confirmed
                || entry.settlement_evidence != evidence;
            entry.phase = EngramSessionObligationPhase::Confirmed;
            entry.settlement_evidence = evidence;
            ledger.validate().map_err(|_| pending_error())?;
            if changed {
                inner.engram_session_obligations = ledger;
                self.commit_locked(&mut inner)
                    .map_err(|_| pending_error())?;
            }
            EngramProjectContainmentFence::capture(&inner, project_id, &sessions)
                .map_err(|_| pending_error())?
        };
        self.confirm_engram_containment_until(receipt_image.clone(), deadline)?;
        let cleared_image = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if !off_checkpoint_cut_matches(&inner, &receipt_image)
                || !off_checkpoint_receipt_owner_matches(
                    &inner,
                    &owner,
                    Some(&response),
                    initial_flight,
                )
                .map_err(|_| pending_error())?
                || clock.now() >= deadline
            {
                return Err(pending_error());
            }
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(pending_error)?;
            if inner.sessions[index].engram.active_grant_id.is_some() {
                let record = inner.session_mut_by_index(index).expect("matched owner");
                record.engram.active_grant_id = None;
                record
                    .engram
                    .clear_checkpoint_if_owned_by(Some(owner.reset_generation));
                self.commit_locked(&mut inner)
                    .map_err(|_| pending_error())?;
            }
            EngramProjectContainmentFence::capture(&inner, project_id, &sessions)
                .map_err(|_| pending_error())?
        };
        // Then acknowledge the coupled cleared row before publishing release.
        self.confirm_engram_containment_until(cleared_image.clone(), deadline)?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if !off_checkpoint_cut_matches(&inner, &cleared_image)
            || !off_checkpoint_receipt_owner_matches(
                &inner,
                &owner,
                Some(&response),
                initial_flight,
            )
            .map_err(|_| pending_error())?
            || clock.now() >= deadline
        {
            return Err(pending_error());
        }
        let added = inner
            .engram_confirmed_containment_ids
            .insert(owner.obligation.id.clone());
        if added && self.commit_locked(&mut inner).is_err() {
            inner
                .engram_confirmed_containment_ids
                .remove(&owner.obligation.id);
            return Err(pending_error());
        }
        inner.engram_pending_checkpoint_receipts.remove(key);
        Ok(())
    }

    // Called only by the explicit public Resume route. Automatic admission and
    // capacity callers cannot select this producer through the ordinary helper.
    fn resume_off_checkpoint_receipt(&self, session_id: &str) -> Result<bool, ApiError> {
        let clock = self.engram_budget_clock();
        let (key, deadline) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let mut selected = None;
            for (key, pending) in &inner.engram_pending_checkpoint_receipts.entries {
                if pending.origin != EngramCheckpointReceiptOrigin::OffContainment
                    || !pending.accepted
                {
                    continue;
                }
                let owner: EngramContainmentReceiptOwner =
                    serde_json::from_slice(&pending.identity).map_err(|_| {
                        ApiError::conflict("OFF checkpoint receipt identity is unavailable")
                    })?;
                if owner.obligation.local_session_id == session_id {
                    if selected.is_some() {
                        return Err(ApiError::conflict(
                            "OFF checkpoint receipt identity is ambiguous",
                        ));
                    }
                    selected = Some(key.clone());
                }
            }
            let Some(key) = selected else {
                return Ok(false);
            };
            let pending = inner
                .engram_pending_checkpoint_receipts
                .entries
                .get_mut(&key)
                .expect("selected");
            if pending.accounting {
                return Err(ApiError::conflict(
                    "OFF checkpoint local accounting is already active",
                ));
            }
            let deadline = clock.now() + ENGRAM_CONTROL_SETTLE_TIMEOUT;
            pending.accounting = true;
            (key, deadline)
        };
        let _reservation = EngramCheckpointReceiptReservation {
            state: self.clone(),
            key: key.clone(),
        };
        self.finish_off_checkpoint_receipt(&key, deadline, false)?;
        Ok(true)
    }
}
