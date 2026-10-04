// Typed host-brief delivery and ordinary requester contamination share the
// evaluation single-flight boundary. Caller text cannot select the host origin.
impl AppState {
    fn recover_retained_acceptance_brief(
        &self,
        child: &str,
        request: &SubmitAcceptanceEvaluationRequest,
    ) -> Result<Option<String>, ApiError> {
        let reserved = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_delegation_index_by_child_session_id(child) else {
                return Ok(None);
            };
            let previous = inner.delegations[index].clone();
            let Some(target) = previous.acceptance_evaluation.as_ref() else {
                return Ok(None);
            };
            let retained = target
                .attempt_history
                .as_ref()
                .filter(|h| h.schema_version == 1)
                .and_then(|h| Some((h.prepared_brief.as_ref()?, h.previous.last()?)))
                .filter(|(_, old)| {
                    request.attempt_key.as_deref() == Some(old.key.as_str())
                        && old
                            .refusal
                            .as_ref()
                            .is_some_and(|r| r.code == "acceptance_evaluation_resubmit")
                });
            let Some((brief, _)) = retained else {
                return Ok(None);
            };
            if previous.mode != DelegationMode::Evaluator
                || previous.write_policy != DelegationWritePolicy::ReadOnly
                || !matches!(target.submission, AcceptanceEvaluationSubmission::None)
                || inner
                    .delegation_followup_admissions
                    .contains_key(&previous.id)
            {
                return Err(ApiError::conflict(
                    "the retained brief cannot be offered while another responsibility is active",
                ));
            }
            let prompt = brief.prompt.clone();
            let guard = AcceptanceEvaluationSubmissionInFlight::admit_locked(
                self,
                &mut inner,
                &previous.id,
            )
            .ok_or_else(|| {
                ApiError::conflict("acceptance attempt admission is already in progress")
            })?;
            (
                AcceptanceEvaluationReuseReservation {
                    previous,
                    _guard: guard,
                    automatic: true,
                },
                prompt,
            )
        };
        let (reservation, prompt) = reserved;
        let authority = AcceptanceEvaluationSubmitAuthority {
            delegation_id: reservation.previous.id.clone(),
            target: reservation.previous.acceptance_evaluation.clone().unwrap(),
        };
        self.confirm_acceptance_evaluation_submission_durable(&authority, &reservation.previous)
            .map_err(|e| {
                ApiError::internal(format!(
                    "retained refresh target remains withheld until acknowledged: {e}"
                ))
            })?;
        self.offer_acceptance_evaluation_brief(&reservation, &prompt)?;
        Ok(Some(prompt))
    }

    fn reserve_requester_acceptance_followup(
        &self,
        parent: &str,
        id: &str,
        request: &SendMessageRequest,
    ) -> Result<Option<AcceptanceEvaluationSubmissionInFlight>, ApiError> {
        self.reserve_requester_acceptance_prompt(parent, id, request, false)
    }

    fn reserve_direct_requester_acceptance_prompt(
        &self,
        child: &str,
        request: &SendMessageRequest,
    ) -> Result<Option<AcceptanceEvaluationSubmissionInFlight>, ApiError> {
        let delegation = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .find_delegation_index_by_child_session_id(child)
                .filter(|i| inner.delegations[*i].mode == DelegationMode::Evaluator)
                .map(|i| {
                    (
                        inner.delegations[i].parent_session_id.clone(),
                        inner.delegations[i].id.clone(),
                    )
                })
        };
        let Some((parent, id)) = delegation else {
            return Ok(None);
        };
        self.reserve_requester_acceptance_prompt(&parent, &id, request, true)
    }

    fn reserve_requester_acceptance_prompt(
        &self,
        parent: &str,
        id: &str,
        request: &SendMessageRequest,
        direct: bool,
    ) -> Result<Option<AcceptanceEvaluationSubmissionInFlight>, ApiError> {
        parse_prompt_image_attachments(&request.attachments)?;
        if request.text.trim().is_empty() && request.attachments.is_empty() {
            return Err(ApiError::bad_request("follow-up message cannot be empty"));
        }
        let (guard, written, dispatch) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = find_parent_delegation_index_locked(&inner, parent, id)?;
            if inner.delegations[index].mode != DelegationMode::Evaluator {
                return Ok(None);
            }
            if inner
                .acceptance_evaluation_submissions_in_flight
                .contains(id)
                || inner.delegation_followup_admissions.contains_key(id)
                || inner.delegations[index]
                    .acceptance_evaluation
                    .as_ref()
                    .is_some_and(|t| {
                        matches!(
                            t.submission,
                            AcceptanceEvaluationSubmission::Pending { .. }
                                | AcceptanceEvaluationSubmission::Unconfirmed { .. }
                        ) || t
                            .attempt_history
                            .as_ref()
                            .and_then(|h| h.prepared_brief.as_ref())
                            .is_some()
                    })
            {
                return Err(ApiError::conflict(
                    "an evaluator submission or host brief is unsettled; requester follow-up cannot overlap it",
                ));
            }
            if (!direct
                && !matches!(
                    inner.delegations[index].status,
                    DelegationStatus::Completed | DelegationStatus::Failed
                ))
                || (direct && delegation_is_terminal(inner.delegations[index].status))
            {
                return Err(ApiError::conflict(
                    "the evaluator must finish before requester follow-up",
                ));
            }
            if let Some(history) = inner.delegations[index]
                .acceptance_evaluation
                .as_mut()
                .and_then(|t| t.attempt_history.as_mut())
            {
                // Set before possible delivery, including an ambiguous failure.
                history.requester_text_tainted = true;
            }
            inner.mark_delegation_mutated(index);
            let (_, dispatch) = self
                .commit_locked_with_persist_dispatch(&mut inner)
                .map_err(|e| {
                    ApiError::internal(format!(
                        "requester follow-up taint retained but not acknowledged: {e:#}"
                    ))
                })?;
            let written = inner.delegations[index].clone();
            let guard =
                AcceptanceEvaluationSubmissionInFlight::admit_locked(self, &mut inner, id).unwrap();
            (guard, written, dispatch)
        };
        if dispatch != PersistDispatch::Synchronous {
            if let Some(target) = written.acceptance_evaluation.as_ref() {
                self.confirm_acceptance_evaluation_submission_durable(
                    &AcceptanceEvaluationSubmitAuthority {
                        delegation_id: id.to_owned(),
                        target: target.clone(),
                    },
                    &written,
                )
                .map_err(|e| {
                    ApiError::internal(format!(
                        "requester follow-up withheld until its taint is durable: {e}"
                    ))
                })?;
            }
        }
        Ok(Some(guard))
    }

    fn offer_acceptance_evaluation_brief(
        &self,
        reservation: &AcceptanceEvaluationReuseReservation,
        prompt: &str,
    ) -> Result<(), ApiError> {
        let (written, dispatch) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_delegation_index(&reservation.previous.id)
                .ok_or_else(|| {
                    ApiError::conflict("evaluator disappeared before host-brief delivery")
                })?;
            let record = &inner.delegations[index];
            let child = inner.find_visible_session_index(&record.child_session_id)
                .map(|i| &inner.sessions[i])
                .ok_or_else(|| ApiError::conflict("retained brief evaluator is unavailable"))?;
            if !child.is_local_session()
                || child.session.parent_delegation_id.as_deref() != Some(record.id.as_str())
                || child.session.agent != record.agent
                || record.model.as_deref() != Some(child.session.model.as_str())
            {
                return Err(ApiError::conflict("retained host brief remains withheld: the evaluator configuration changed"));
            }
            let target = inner.delegations[index]
                .acceptance_evaluation
                .as_mut()
                .unwrap();
            let brief = target
                .attempt_history
                .as_mut()
                .and_then(|h| h.prepared_brief.as_mut())
                .ok_or_else(|| ApiError::conflict("no retained host brief is available"))?;
            if brief.attempt_key != target.attempt_key
                || brief.prompt != prompt
                || brief.digest != acceptance_evaluation_payload_digest(&[prompt.to_owned()])
            {
                return Err(ApiError::conflict(
                    "host brief does not match its exact persisted attempt",
                ));
            }
            brief.offered = true;
            inner.mark_delegation_mutated(index);
            let (_, dispatch) = self
                .commit_locked_with_persist_dispatch(&mut inner)
                .map_err(|e| {
                    ApiError::internal(format!(
                        "host brief remains retained after persistence failure: {e:#}"
                    ))
                })?;
            (inner.delegations[index].clone(), dispatch)
        };
        if dispatch != PersistDispatch::Synchronous {
            self.confirm_acceptance_evaluation_submission_durable(
                &AcceptanceEvaluationSubmitAuthority {
                    delegation_id: written.id.clone(),
                    target: written.acceptance_evaluation.clone().unwrap(),
                },
                &written,
            )
            .map_err(|e| {
                ApiError::internal(format!(
                    "host brief remains withheld until durable acknowledgement: {e}"
                ))
            })?;
        }
        // A running evaluator receives the refreshed brief in its submission
        // response. A finished evaluator is rearmed with this exact host text.
        if !reservation.automatic {
            self.followup_delegation_request_with_origin(
                &written.parent_session_id,
                &written.id,
                SendMessageRequest {
                    text: prompt.to_owned(),
                    expanded_text: None,
                    attachments: Vec::new(),
                    source_session_id: None,
                    source_mailbox: None,
                },
                true,
            )?;
        }
        Ok(())
    }
}
