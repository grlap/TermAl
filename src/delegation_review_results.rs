/*
Structured delegated-review result protocol.

This layer owns request validation, durable mailbox projection, recovery, and
boot reconciliation. Generic mailbox transport remains in mailboxes.rs;
delegation lifecycle terminalization remains in delegations.rs.
*/

const DELEGATION_REVIEW_RESULT_TOPIC: &str = "delegation-review-result/v1";
const DELEGATION_REVIEW_RESULT_KIND: &str = "delegationReviewResult";
const DELEGATION_REVIEW_RESULT_SCHEMA_VERSION: u32 = 1;
const MAX_DELEGATION_REVIEW_RESULT_LIST_ITEMS: usize = 200;
const MAX_DELEGATION_REVIEW_RESULT_TEXT_CHARS: usize = 16 * 1024;

#[derive(Debug)]
struct ValidatedDelegationReviewMailboxSubmission {
    delegation_id: String,
    parent_session_id: String,
    child_session_id: String,
    submission_attempt: u32,
    sender_name: String,
    target_name: String,
    envelope: DelegationReviewMailboxResult,
}

impl AppState {
    /// Accepts the one mailbox message shape available to delegation children.
    /// The route derives every identity and delivery field from the durable
    /// delegation link; the model supplies only the bounded review payload.
    fn submit_delegation_review_result(
        &self,
        child_session_id: &str,
        request: SubmitDelegationReviewResultRequest,
    ) -> std::result::Result<MailboxAppendReceipt, ApiError> {
        let submission =
            self.validate_delegation_review_submission(child_session_id, request)?;
        self.persist_validated_delegation_review_submission(&submission)
    }

    fn persist_validated_delegation_review_submission(
        &self,
        submission: &ValidatedDelegationReviewMailboxSubmission,
    ) -> std::result::Result<MailboxAppendReceipt, ApiError> {
        let input = MailboxAppendInput {
            sender_session_id: submission.child_session_id.clone(),
            sender_name: submission.sender_name.clone(),
            target_session_id: submission.parent_session_id.clone(),
            target_name: submission.target_name.clone(),
            body: serde_json::to_string(&submission.envelope).map_err(|err| {
                ApiError::internal(format!(
                    "failed to encode validated delegation review result: {err}"
                ))
            })?,
            idempotency_key: delegation_review_result_idempotency_key(
                &submission.delegation_id,
                submission.submission_attempt,
            ),
            topic: Some(DELEGATION_REVIEW_RESULT_TOPIC.to_owned()),
            state_stamp: Some(format!(
                "{}:{}",
                submission.delegation_id, submission.submission_attempt
            )),
        };
        let appended = self
            .mailbox_store
            .append_delegation_review_result(&input)
            .map_err(mailbox_api_error)?;
        let MailboxAppendResult {
            mut receipt,
            finalization: _dispatch_finalization,
        } = appended;

        // Promote only after the child lifecycle independently reaches a
        // terminal state. Persisting this provisional value after the durable
        // mailbox append keeps normal parent fan-in free from Markdown parsing.
        // Mailbox idempotency accepts only this exact serialized envelope for
        // the attempt; delegation state is projected from that same envelope
        // so the two durable views cannot choose different review payloads.
        let record_result = self.record_delegation_review_submission(&submission);
        record_result?;

        if !receipt.duplicate {
            match self.mailbox_store.record_initial_dispatch_outcome(
                &receipt.message_id,
                "durableButNotWoken",
            ) {
                Ok(MailboxDispatchOutcomeRecord::Recorded { .. }) => {}
                Ok(MailboxDispatchOutcomeRecord::AlreadyFinalized {
                    dispatch_outcome,
                }) => receipt.notification_disposition = dispatch_outcome,
                Err(err) => {
                    eprintln!(
                        "mailbox> review result {} committed, but its non-waking disposition could not be finalized: {err:#}",
                        receipt.message_id
                    );
                }
            }
        }
        Ok(receipt)
    }

    /// Records one recovery probe only if the delegation still represents the
    /// attempt observed before coordination-store I/O. Rearm increments the
    /// attempt, so a stale miss or quarantine can never suppress the next turn.
    fn record_delegation_review_recovery_probe(
        &self,
        child_session_id: &str,
        delegation_id: &str,
        parent_session_id: &str,
        submission_attempt: u32,
        recovery_error: Option<String>,
    ) -> std::result::Result<(), ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(delegation_index) = inner
            .find_delegation_index_by_child_session_id(child_session_id)
        else {
            return Ok(());
        };
        let delegation = &inner.delegations[delegation_index];
        if delegation.id != delegation_id
            || delegation.parent_session_id != parent_session_id
            || delegation.review_result_submission_attempt != submission_attempt
            || !delegation.review_result_required
            || delegation.review_result_schema_version.is_some()
            || delegation.submitted_review_result.is_some()
            || !matches!(
                delegation.status,
                DelegationStatus::Running | DelegationStatus::Failed
            )
        {
            return Ok(());
        }
        if delegation.review_result_recovery_probe_attempt == Some(submission_attempt)
            && delegation.review_result_recovery_error == recovery_error
        {
            return Ok(());
        }

        let diagnostic_note = recovery_error.as_ref().map(|reason| {
            format!("Durable structured review result was quarantined during recovery: {reason}")
        });
        let record = &mut inner.delegations[delegation_index];
        record.review_result_recovery_probe_attempt = Some(submission_attempt);
        record.review_result_recovery_error = recovery_error;
        if let (Some(result), Some(note)) = (record.result.as_mut(), diagnostic_note) {
            if result.status == DelegationStatus::Failed && !result.notes.contains(&note) {
                result.notes.push(note);
            }
        }
        inner.mark_delegation_mutated(delegation_index);
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!(
                "failed to persist structured review recovery diagnostic: {err:#}"
            ))
        })?;
        Ok(())
    }

    /// Restores a validated per-attempt review envelope that reached the
    /// durable mailbox but not the primary delegation row (for example after
    /// a process exit between those two independent SQLite commits).
    ///
    /// Mailbox I/O intentionally happens without the main state mutex. The
    /// final record step revalidates the live delegation link and attempt
    /// before applying the recovered envelope.
    fn recover_durable_delegation_review_submission(
        &self,
        child_session_id: &str,
    ) -> std::result::Result<(), ApiError> {
        #[cfg(test)]
        if self.mailbox_store.connection_if_enabled().is_none() {
            // Lightweight state-only tests intentionally disable the separate
            // coordination database; they exercise lifecycle behavior without
            // a durable mailbox to reconcile.
            return Ok(());
        }
        let recovery = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(delegation_index) = inner
                .find_delegation_index_by_child_session_id(child_session_id)
            else {
                return Ok(());
            };
            let delegation = &inner.delegations[delegation_index];
            if !delegation.review_result_required
                || delegation.review_result_schema_version.is_some()
                || delegation.submitted_review_result.is_some()
                || !matches!(
                    delegation.status,
                    DelegationStatus::Running | DelegationStatus::Failed
                )
            {
                return Ok(());
            }
            if delegation.status == DelegationStatus::Running
                && matches!(
                    delegation_child_outcome(&inner, child_session_id),
                    DelegationChildOutcome::Running
                )
            {
                return Ok(());
            }
            (
                delegation.id.clone(),
                delegation.parent_session_id.clone(),
                delegation.review_result_submission_attempt,
            )
        };
        let (delegation_id, parent_session_id, submission_attempt) = recovery;
        let idempotency_key =
            delegation_review_result_idempotency_key(&delegation_id, submission_attempt);
        {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(delegation_index) = inner
                .find_delegation_index_by_child_session_id(child_session_id)
            else {
                return Ok(());
            };
            if inner.delegations[delegation_index].review_result_recovery_probe_attempt
                == Some(submission_attempt)
            {
                return Ok(());
            }
        }
        let Some(stored) = self
            .mailbox_store
            .read_delegation_review_result_for_recovery(child_session_id, &idempotency_key)
            .map_err(mailbox_api_error)?
        else {
            return self.record_delegation_review_recovery_probe(
                child_session_id,
                &delegation_id,
                &parent_session_id,
                submission_attempt,
                None,
            );
        };
        let expected_state_stamp = format!("{delegation_id}:{submission_attempt}");
        let recovery_error = if stored.target_session_id != parent_session_id
            || stored.topic.as_deref() != Some(DELEGATION_REVIEW_RESULT_TOPIC)
            || stored.state_stamp.as_deref() != Some(expected_state_stamp.as_str())
        {
            Some(
                "envelope metadata does not match its delegation attempt".to_owned(),
            )
        } else {
            match serde_json::from_str::<DelegationReviewMailboxResult>(&stored.body) {
                Ok(envelope)
                    if envelope.schema_version == DELEGATION_REVIEW_RESULT_SCHEMA_VERSION
                        && envelope.kind == DELEGATION_REVIEW_RESULT_KIND
                        && envelope.delegation_id == delegation_id
                        && envelope.child_session_id == child_session_id
                        && envelope.submission_attempt == submission_attempt
                        && matches!(
                            envelope.status,
                            DelegationStatus::Completed | DelegationStatus::Failed
                        ) =>
                {
                    if let Err(err) = self.record_delegation_review_submission(
                        &ValidatedDelegationReviewMailboxSubmission {
                            delegation_id: delegation_id.clone(),
                            parent_session_id: parent_session_id.clone(),
                            child_session_id: child_session_id.to_owned(),
                            submission_attempt,
                            sender_name: stored.sender_name.clone(),
                            target_name: stored.target_name.clone(),
                            envelope,
                        },
                    ) {
                        // A conflict means concurrent lifecycle work made this
                        // attempt obsolete or already installed terminal truth.
                        // Internal failures mean the envelope is still
                        // applicable but could not be persisted; those must
                        // block fan-in so the next call can retry recovery.
                        if err.status == StatusCode::CONFLICT {
                            eprintln!(
                                "delegation review> durable result for child `{child_session_id}` became obsolete during recovery: {}",
                                err.message
                            );
                        } else {
                            return Err(err);
                        }
                    }
                    None
                }
                Ok(_) => Some("envelope identity is invalid".to_owned()),
                Err(err) => Some(format!("envelope JSON is invalid: {err}")),
            }
        };
        if let Some(reason) = recovery_error {
            eprintln!(
                "delegation review> quarantined durable result for child `{child_session_id}`: {reason}"
            );
            self.record_delegation_review_recovery_probe(
                child_session_id,
                &delegation_id,
                &parent_session_id,
                submission_attempt,
                Some(reason),
            )?;
        }
        match self.mailbox_store.record_initial_dispatch_outcome(
            &stored.message_id,
            "durableButNotWoken",
        ) {
            Ok(MailboxDispatchOutcomeRecord::Recorded { .. })
            | Ok(MailboxDispatchOutcomeRecord::AlreadyFinalized { .. }) => {}
            Err(err) => eprintln!(
                "mailbox> recovered review result {}, but its non-waking disposition could not be finalized: {err:#}",
                stored.message_id
            ),
        }
        Ok(())
    }

    fn reconcile_durable_delegation_review_submissions_after_boot(&self) {
        let child_session_ids = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .delegations
                .iter()
                .filter(|delegation| {
                    delegation.review_result_required
                        && delegation.review_result_schema_version.is_none()
                        && delegation.submitted_review_result.is_none()
                })
                .map(|delegation| delegation.child_session_id.clone())
                .collect::<Vec<_>>()
        };
        for child_session_id in child_session_ids {
            if let Err(err) =
                self.recover_durable_delegation_review_submission(&child_session_id)
            {
                eprintln!(
                    "delegation review> failed recovering durable result for child `{child_session_id}`: {}",
                    err.message
                );
                continue;
            }
            if let Err(err) = self.refresh_delegation_for_child_session(&child_session_id) {
                eprintln!(
                    "delegation review> failed refreshing child `{child_session_id}` after durable result recovery: {err:#}"
                );
            }
        }
    }

    fn validate_delegation_review_submission(
        &self,
        child_session_id: &str,
        request: SubmitDelegationReviewResultRequest,
    ) -> std::result::Result<ValidatedDelegationReviewMailboxSubmission, ApiError> {
        validate_delegation_review_result_request(&request)?;
        let inner = self.inner.lock().expect("state mutex poisoned");
        let delegation_index = inner
            .find_delegation_index_by_child_session_id(child_session_id)
            .ok_or_else(|| {
                ApiError::bad_request(
                    "structured review results require a linked delegation child",
                )
            })?;
        let delegation = &inner.delegations[delegation_index];
        if delegation.child_session_id != child_session_id
            || delegation.mode != DelegationMode::Reviewer
            || !delegation.review_result_required
        {
            return Err(ApiError::bad_request(
                "structured review results are accepted only from current reviewer children",
            ));
        }
        if delegation.status != DelegationStatus::Running
            && !((delegation.status == DelegationStatus::Completed
                || delegation.status == DelegationStatus::Failed)
                && delegation.review_result_schema_version
                    == Some(DELEGATION_REVIEW_RESULT_SCHEMA_VERSION))
        {
            return Err(ApiError::conflict(
                "delegation is no longer accepting a structured review result",
            ));
        }
        let child_index = inner
            .find_session_index(child_session_id)
            .ok_or_else(|| ApiError::not_found("delegation child session not found"))?;
        let parent_index = inner
            .find_visible_session_index(&delegation.parent_session_id)
            .ok_or_else(ApiError::local_session_missing)?;
        let child = &inner.sessions[child_index];
        let parent = &inner.sessions[parent_index];
        if child.hidden
            || !child.is_local_session()
            || child.session.parent_delegation_id.as_deref()
                != Some(delegation.id.as_str())
        {
            return Err(ApiError::bad_request(
                "review result sender is not the linked local delegation child",
            ));
        }
        if parent.hidden
            || !parent.is_local_session()
            || parent.session.parent_delegation_id.is_some()
            || inner
                .find_delegation_index_by_child_session_id(&parent.session.id)
                .is_some()
        {
            return Err(ApiError::bad_request(
                "review result target must be a local root session",
            ));
        }

        let findings = request
            .findings
            .iter()
            .map(|finding| DelegationFinding {
                severity: finding.severity.clone(),
                file: finding.file.clone(),
                line: finding.line,
                message: finding.message.clone(),
            })
            .collect::<Vec<_>>();
        let commands_run = request
            .commands_run
            .iter()
            .map(|command| DelegationCommandResult {
                command: command.command.clone(),
                status: command.status.as_str().to_owned(),
            })
            .collect::<Vec<_>>();
        let envelope = DelegationReviewMailboxResult {
            schema_version: DELEGATION_REVIEW_RESULT_SCHEMA_VERSION,
            kind: DELEGATION_REVIEW_RESULT_KIND.to_owned(),
            delegation_id: delegation.id.clone(),
            child_session_id: child_session_id.to_owned(),
            submission_attempt: delegation.review_result_submission_attempt,
            status: request.status,
            summary: request.summary.trim().to_owned(),
            findings,
            commands_run,
            files_inspected: request.files_inspected,
            notes: request.notes,
            suggested_tracker_updates: request.suggested_tracker_updates,
        };
        let envelope_bytes = serde_json::to_vec(&envelope).map_err(|err| {
            ApiError::internal(format!(
                "failed to measure validated delegation review result: {err}"
            ))
        })?;
        if envelope_bytes.len() > MAX_MAILBOX_BODY_BYTES {
            return Err(ApiError::bad_request(format!(
                "delegation review result exceeds the {MAX_MAILBOX_BODY_BYTES}-byte aggregate envelope limit; reduce findings, commands, files, notes, or suggested tracker updates and retry"
            )));
        }
        Ok(ValidatedDelegationReviewMailboxSubmission {
            delegation_id: delegation.id.clone(),
            parent_session_id: delegation.parent_session_id.clone(),
            child_session_id: child_session_id.to_owned(),
            submission_attempt: delegation.review_result_submission_attempt,
            sender_name: child.session.name.clone(),
            target_name: parent.session.name.clone(),
            envelope,
        })
    }

    fn record_delegation_review_submission(
        &self,
        submission: &ValidatedDelegationReviewMailboxSubmission,
    ) -> std::result::Result<(), ApiError> {
        let result = delegation_result_from_review_envelope(&submission.envelope);
        let (revision, lifecycle_delta, detached_child, wait_refresh) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let delegation_index = inner
                .find_delegation_index_by_child_session_id(&submission.child_session_id)
                .ok_or_else(|| {
                    ApiError::conflict("delegation disappeared during result submission")
                })?;
            let delegation = &inner.delegations[delegation_index];
            if delegation.id != submission.delegation_id
                || delegation.parent_session_id != submission.parent_session_id
                || !delegation.review_result_required
            {
                return Err(ApiError::conflict(
                    "delegation link changed during result submission",
                ));
            }
            if delegation.review_result_submission_attempt != submission.submission_attempt {
                return Err(ApiError::conflict(
                    "delegation review submission attempt changed while the mailbox result was being stored; submit a fresh structured result for the current attempt",
                ));
            }
            if delegation_is_terminal(delegation.status)
                && delegation.review_result_schema_version
                    == Some(DELEGATION_REVIEW_RESULT_SCHEMA_VERSION)
            {
                return Ok(());
            }
            if !matches!(
                delegation.status,
                DelegationStatus::Running | DelegationStatus::Failed
            ) {
                return Err(ApiError::conflict(
                    "delegation completed before its structured result could be recorded",
                ));
            }
            if let Some(existing) = delegation.submitted_review_result.as_ref() {
                if existing == &result {
                    return Ok(());
                }
                return Err(ApiError::conflict(
                    "delegation already submitted a different structured review result",
                ));
            }

            // A terminal observer may have acquired the state mutex after the
            // mailbox append committed but before this record step. Re-open
            // only a failed required review from the same attempt, then derive
            // its terminal state again while the envelope is present.
            let reopened_terminal = delegation.status == DelegationStatus::Failed;
            if reopened_terminal {
                let record = &mut inner.delegations[delegation_index];
                record.status = DelegationStatus::Running;
                record.completed_at = None;
                record.result = None;
                inner.sync_running_read_only_delegation_index(delegation_index);
            }
            let record = &mut inner.delegations[delegation_index];
            record.review_result_recovery_probe_attempt = None;
            record.review_result_recovery_error = None;
            record.submitted_review_result = Some(result);
            inner.mark_delegation_mutated(delegation_index);

            let (lifecycle_delta, detached_child, wait_refresh) = if reopened_terminal {
                let lifecycle_delta =
                    refresh_delegation_from_child_locked(&mut inner, delegation_index);
                let detached_child =
                    detach_terminal_delegation_child_runtime_locked(&mut inner, delegation_index);
                let wait_refresh = refresh_delegation_waits_locked(&mut inner);
                (lifecycle_delta, detached_child, wait_refresh)
            } else {
                (
                    None,
                    DetachedDelegationChildRuntime::default(),
                    DelegationWaitRefresh::default(),
                )
            };
            let revision = self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!(
                    "failed to persist structured delegation review result: {err:#}"
                ))
            })?;
            (revision, lifecycle_delta, detached_child, wait_refresh)
        };
        self.publish_delegation_refresh_side_effects(
            revision,
            lifecycle_delta,
            detached_child,
            wait_refresh,
        );
        Ok(())
    }
}

fn delegation_review_result_idempotency_key(
    delegation_id: &str,
    submission_attempt: u32,
) -> String {
    format!("delegation-review-result-v1-{delegation_id}-{submission_attempt}")
}

fn delegation_result_from_review_envelope(
    envelope: &DelegationReviewMailboxResult,
) -> DelegationResult {
    let mut notes = envelope.notes.clone();
    notes.extend(
        envelope
            .files_inspected
            .iter()
            .map(|path| format!("Inspected {path}")),
    );
    notes.extend(
        envelope
            .suggested_tracker_updates
            .iter()
            .map(|update| format!("Suggested tracker update: {update}")),
    );
    DelegationResult {
        delegation_id: envelope.delegation_id.clone(),
        child_session_id: envelope.child_session_id.clone(),
        status: envelope.status,
        summary: envelope.summary.clone(),
        findings: envelope.findings.clone(),
        changed_files: Vec::new(),
        commands_run: envelope.commands_run.clone(),
        notes,
    }
}

fn validate_delegation_review_result_request(
    request: &SubmitDelegationReviewResultRequest,
) -> std::result::Result<(), ApiError> {
    if request.schema_version != DELEGATION_REVIEW_RESULT_SCHEMA_VERSION {
        return Err(ApiError::bad_request(format!(
            "unsupported delegation review result schemaVersion {}; expected {}",
            request.schema_version, DELEGATION_REVIEW_RESULT_SCHEMA_VERSION
        )));
    }
    if !matches!(
        request.status,
        DelegationStatus::Completed | DelegationStatus::Failed
    ) {
        return Err(ApiError::bad_request(
            "delegation review result status must be `completed` or `failed`",
        ));
    }
    validate_delegation_review_text("summary", &request.summary, true)?;
    if request.findings.len() > MAX_DELEGATION_RESULT_FINDINGS {
        return Err(ApiError::bad_request(format!(
            "delegation review result exceeds {MAX_DELEGATION_RESULT_FINDINGS} findings"
        )));
    }
    for finding in &request.findings {
        if !matches!(
            finding.severity.as_str(),
            "Critical" | "High" | "Medium" | "Low" | "Note"
        ) {
            return Err(ApiError::bad_request(format!(
                "unsupported review finding severity `{}`",
                finding.severity
            )));
        }
        validate_delegation_review_text("finding message", &finding.message, true)?;
        if let Some(file) = finding.file.as_deref() {
            validate_delegation_review_text("finding file", file, true)?;
        }
        if finding.line == Some(0) {
            return Err(ApiError::bad_request(
                "delegation review result finding line must be at least 1",
            ));
        }
    }
    for (label, values) in [
        ("commandsRun", request.commands_run.len()),
        ("filesInspected", request.files_inspected.len()),
        ("notes", request.notes.len()),
        (
            "suggestedTrackerUpdates",
            request.suggested_tracker_updates.len(),
        ),
    ] {
        if values > MAX_DELEGATION_REVIEW_RESULT_LIST_ITEMS {
            return Err(ApiError::bad_request(format!(
                "delegation review result `{label}` exceeds {MAX_DELEGATION_REVIEW_RESULT_LIST_ITEMS} items"
            )));
        }
    }
    for command in &request.commands_run {
        validate_delegation_review_text("command", &command.command, true)?;
    }
    for (label, values) in [
        ("file inspected", &request.files_inspected),
        ("note", &request.notes),
        (
            "suggested tracker update",
            &request.suggested_tracker_updates,
        ),
    ] {
        for value in values {
            validate_delegation_review_text(label, value, true)?;
        }
    }
    Ok(())
}

fn validate_delegation_review_text(
    label: &str,
    value: &str,
    required: bool,
) -> std::result::Result<(), ApiError> {
    let trimmed = value.trim();
    if required && trimmed.is_empty() {
        return Err(ApiError::bad_request(format!(
            "delegation review result {label} cannot be empty"
        )));
    }
    if value.chars().count() > MAX_DELEGATION_REVIEW_RESULT_TEXT_CHARS {
        return Err(ApiError::bad_request(format!(
            "delegation review result {label} exceeds {MAX_DELEGATION_REVIEW_RESULT_TEXT_CHARS} characters"
        )));
    }
    Ok(())
}

fn post_submission_transport_error(outcome: &DelegationChildOutcome) -> Option<String> {
    match outcome {
        DelegationChildOutcome::Running | DelegationChildOutcome::Completed { .. } => None,
        DelegationChildOutcome::Failed { summary } => {
            let detail = summary.trim();
            Some(if detail.is_empty() {
                "Child runtime failed after structured result submission.".to_owned()
            } else {
                format!("Child runtime failed after structured result submission: {detail}")
            })
        }
        DelegationChildOutcome::IdleWithoutResult => Some(
            "Child became idle without a final assistant packet after structured result submission."
                .to_owned(),
        ),
        DelegationChildOutcome::Missing => Some(
            "Delegation child session disappeared after structured result submission.".to_owned(),
        ),
    }
}

fn terminalize_submitted_review_result_locked(
    inner: &mut StateInner,
    delegation_index: usize,
    delegation: &DelegationRecord,
    result: DelegationResult,
    post_submission_transport_error: Option<String>,
) -> Option<DelegationLifecycleDelta> {
    let terminal_at = stamp_now();
    let public_summary = compact_delegation_public_summary(&result.summary);
    let (card_status, lifecycle_status) = match result.status {
        DelegationStatus::Completed => (
            ParallelAgentStatus::Completed,
            DelegationStatus::Completed,
        ),
        DelegationStatus::Failed => (ParallelAgentStatus::Error, DelegationStatus::Failed),
        _ => return None,
    };
    {
        let record = inner.delegations.get_mut(delegation_index)?;
        record.status = lifecycle_status;
        record.completed_at = Some(terminal_at.clone());
        record.result = Some(result.clone());
        record.submitted_review_result = None;
        record.post_submission_transport_error = post_submission_transport_error;
        record.review_result_recovery_probe_attempt = None;
        record.review_result_recovery_error = None;
        record.review_result_schema_version = Some(DELEGATION_REVIEW_RESULT_SCHEMA_VERSION);
        record.result_parser_version = DELEGATION_RESULT_PARSER_VERSION;
    }
    inner.sync_running_read_only_delegation_index(delegation_index);
    inner.mark_delegation_mutated(delegation_index);
    settle_terminal_delegation_child_locked(
        inner,
        &delegation.child_session_id,
        SessionStatus::Idle,
        &public_summary,
    );
    let parent_card_delta = update_parent_delegation_card_locked(
        inner,
        delegation,
        card_status,
        public_summary,
    );
    Some(match lifecycle_status {
        DelegationStatus::Completed => DelegationLifecycleDelta::Completed {
            delegation_id: delegation.id.clone(),
            result,
            completed_at: terminal_at,
            parent_card_delta,
        },
        DelegationStatus::Failed => DelegationLifecycleDelta::Failed {
            delegation_id: delegation.id.clone(),
            result,
            failed_at: terminal_at,
            parent_card_delta,
        },
        _ => return None,
    })
}
