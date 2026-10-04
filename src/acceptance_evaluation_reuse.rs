// Explicit evaluator reuse: reserves the existing submission/follow-up boundary
// before reads, then transfers a fully prepared target after its durable ACK.
struct AcceptanceEvaluationReuseReservation {
    previous: DelegationRecord,
    _guard: AcceptanceEvaluationSubmissionInFlight,
    automatic: bool,
}

impl AppState {
    fn reserve_acceptance_evaluation_reuse(
        &self,
        parent: &str,
        request: &RequestAcceptanceEvaluationRequest,
        automatic: bool,
    ) -> Result<Option<AcceptanceEvaluationReuseReservation>, ApiError> {
        let Some(id) = request.reuse_delegation_id.as_deref() else {
            return Ok(None);
        };
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let store = acceptance_evaluation_host_target_locked(&inner, parent)?.store;
        let index = inner.find_delegation_index(id).ok_or_else(|| {
            ApiError::bad_request("reuseDelegationId is not an evaluator of this parent and task")
        })?;
        let previous = inner.delegations[index].clone();
        let target = previous
            .acceptance_evaluation
            .as_ref()
            .filter(|t| t.judges(&store, request.work_ref.trim()));
        if previous.parent_session_id != parent
            || previous.mode != DelegationMode::Evaluator
            || target.is_none()
        {
            return Err(ApiError::bad_request(
                "reuseDelegationId must name an Evaluator child of this parent for the same store and item",
            ));
        }
        let target = target.unwrap();
        if inner
            .acceptance_evaluation_submissions_in_flight
            .contains(id)
            || inner.delegation_followup_admissions.contains_key(id)
            || matches!(
                target.submission,
                AcceptanceEvaluationSubmission::Pending { .. }
                    | AcceptanceEvaluationSubmission::Unconfirmed { .. }
            )
            || (matches!(
                previous.status,
                DelegationStatus::Queued | DelegationStatus::Running
            ) && !automatic)
        {
            return Err(ApiError::conflict(
                "the preceding evaluation or brief delivery is unsettled or still active; settle its exact stored submission before requesting another judgment",
            ));
        }
        if automatic
            && !target
                .attempt_history
                .as_ref()
                .and_then(|h| h.refusal.as_ref())
                .is_some_and(|r| r.code == "acceptance_evaluation_resubmit")
        {
            return Err(ApiError::conflict(
                "only a confirmed typed resubmit refusal admits automatic refresh",
            ));
        }
        let guard = AcceptanceEvaluationSubmissionInFlight::admit_locked(self, &mut inner, id)
            .ok_or_else(|| {
                ApiError::conflict("evaluation attempt admission is already in progress")
            })?;
        Ok(Some(AcceptanceEvaluationReuseReservation {
            previous,
            _guard: guard,
            automatic,
        }))
    }

    fn acceptance_evaluation_reuse_reason(
        &self,
        reservation: &AcceptanceEvaluationReuseReservation,
        seed: &AcceptanceEvaluationTargetSeed,
        cwd: &str,
        policy_known: bool,
    ) -> Option<String> {
        let previous = &reservation.previous;
        let old = previous.acceptance_evaluation.as_ref()?;
        let Some(history) = old
            .attempt_history
            .as_ref()
            .filter(|h| h.schema_version == 1)
        else {
            return Some("legacy or unreadable spawn-time evaluator evidence".to_owned());
        };
        if history.ordinal >= MAX_ACCEPTANCE_EVALUATOR_ATTEMPTS {
            return Some("the cap of three attempt keys was reached".to_owned());
        }
        if history.requester_text_tainted {
            return Some(
                "requester text was delivered or its delivery cannot be excluded".to_owned(),
            );
        }
        if history.identity_refused {
            return Some("Engram refused this evaluator's independence".to_owned());
        }
        if !policy_known || seed.mode != AcceptanceEvaluationMode::IndependentSession {
            return Some(
                "current policy or task pin does not establish independent_session admission"
                    .to_owned(),
            );
        }
        if previous.write_policy != DelegationWritePolicy::ReadOnly
            || history.parent_session_id != previous.parent_session_id
            || history.child_session_id != previous.child_session_id
            || history.cwd != previous.cwd
            || cwd != history.cwd
        {
            return Some("the persisted spawn-time evaluator scope no longer matches".to_owned());
        }
        let Some(identity) = seed.reuse_identity.as_ref() else {
            return Some("canonical identity is unavailable".to_owned());
        };
        if identity.work_id != history.identity.work_id
            || identity.active_run_id != history.identity.active_run_id
            || identity.root_execution_id != history.identity.root_execution_id
            || identity.run_generation != history.identity.run_generation
        {
            return Some("the canonical work or run changed".to_owned());
        }
        let inner = self.inner.lock().expect("state mutex poisoned");
        let child = inner
            .find_session_index(&previous.child_session_id)
            .map(|i| &inner.sessions[i]);
        if child.is_none_or(|c| {
            c.hidden
                || !c.is_local_session()
                || c.session.parent_delegation_id.as_deref() != Some(previous.id.as_str())
                || !c.queued_prompts.is_empty()
        }) {
            return Some("the local evaluator is missing or has pending delivery".to_owned());
        }
        let child = child.unwrap();
        if child.session.agent != previous.agent
            || previous.model.as_deref() != Some(child.session.model.as_str())
        {
            return Some("the live evaluator agent or model differs from its persisted spawn configuration".to_owned());
        }
        None
    }

    fn install_reused_acceptance_evaluation(
        &self,
        reservation: &AcceptanceEvaluationReuseReservation,
        seed: AcceptanceEvaluationTargetSeed,
        prompt: String,
    ) -> Result<String, ApiError> {
        let previous = &reservation.previous;
        let old = previous.acceptance_evaluation.as_ref().unwrap();
        // Re-prove the settled prior record before replacing it, including a
        // retry after an acknowledgement was lost in the previous call.
        let old_authority = AcceptanceEvaluationSubmitAuthority {
            delegation_id: previous.id.clone(),
            target: old.clone(),
        };
        self.confirm_acceptance_evaluation_submission_durable(&old_authority, previous)
            .map_err(|e| {
                ApiError::internal(format!(
                    "the previous attempt is not acknowledged durable: {e}"
                ))
            })?;
        let mut history = old
            .attempt_history
            .clone()
            .ok_or_else(|| ApiError::conflict("legacy evaluator cannot be reused"))?;
        if history.schema_version != 1 || history.ordinal >= MAX_ACCEPTANCE_EVALUATOR_ATTEMPTS {
            return Err(ApiError::conflict(
                "this evaluator cannot mint another attempt key",
            ));
        }
        history.previous.push(AcceptanceEvaluationPreviousAttempt {
            ordinal: history.ordinal,
            key: old.attempt_key.clone(),
            acceptance_basis: old.acceptance_basis,
            evidence_basis: old.evidence_basis,
            source_fingerprint: old.source_fingerprint.clone(),
            submission: old.submission.clone(),
            refusal: history.refusal.take(),
        });
        history.ordinal += 1;
        let key = acceptance_evaluation_ordinal_key(&previous.id, history.ordinal);
        let prompt =
            acceptance_evaluation_attempt_prompt(&prompt, &key, history.ordinal, Some(old), &seed);
        if prompt.len() > MAX_DELEGATION_PROMPT_BYTES {
            return Err(ApiError::bad_request(
                "host-authored re-evaluation brief is too large",
            ));
        }
        history.prepared_brief = Some(AcceptanceEvaluationPreparedBrief {
            attempt_key: key.clone(),
            digest: acceptance_evaluation_payload_digest(&[prompt.clone()]),
            prompt: prompt.clone(),
            offered: false,
        });
        let (written, dispatch) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_delegation_index(&previous.id)
                .ok_or_else(|| ApiError::conflict("evaluator delegation disappeared"))?;
            if inner.delegations[index] != *previous
                || inner
                    .delegation_followup_admissions
                    .contains_key(&previous.id)
            {
                return Err(ApiError::conflict(
                    "evaluator changed during fresh-attempt preparation",
                ));
            }
            let child = inner.find_visible_session_index(&previous.child_session_id)
                .map(|i| &inner.sessions[i])
                .ok_or_else(|| ApiError::conflict("the evaluator disappeared during preparation"))?;
            if !child.is_local_session()
                || child.session.parent_delegation_id.as_deref() != Some(previous.id.as_str())
                || child.session.agent != previous.agent
                || previous.model.as_deref() != Some(child.session.model.as_str())
                || !child.queued_prompts.is_empty()
            {
                return Err(ApiError::conflict("the evaluator configuration or delivery changed during preparation"));
            }
            acceptance_evaluation_target_admission_locked(
                &inner,
                &previous.parent_session_id,
                &seed,
                Some(&previous.id),
            )?;
            let mut target = seed.into_target(key);
            target.attempt_history = Some(history);
            inner.delegations[index].acceptance_evaluation = Some(target);
            inner.mark_delegation_mutated(index);
            let (_, dispatch) = self.commit_locked_with_persist_dispatch(&mut inner)
                .map_err(|e| ApiError::internal(format!("new attempt retained but not acknowledged; inspect this delegation before retrying: {e:#}")))?;
            (inner.delegations[index].clone(), dispatch)
        };
        let authority = AcceptanceEvaluationSubmitAuthority {
            delegation_id: written.id.clone(),
            target: written.acceptance_evaluation.clone().unwrap(),
        };
        if dispatch != PersistDispatch::Synchronous {
            self.confirm_acceptance_evaluation_submission_durable(&authority, &written)
                .map_err(|e| ApiError::internal(format!("new attempt retained but its brief remains withheld until durable acknowledgement: {e}")))?;
        }
        Ok(prompt)
    }
}
