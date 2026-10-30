// Immutable terminal custody under the existing delegated-review mailbox owner.
// Submission envelopes remain untouched. No primary history vector or new store.
const REVIEW_ATTEMPT_TOPIC: &str = "delegation-review-terminal/v1";
const REVIEW_ATTEMPT_KEY_PREFIX: &str = "delegation-review-terminal-v1";
const REVIEW_ATTEMPT_AUTH_KEY: &str = "delegation_review_terminal_auth_v1";
const MAX_RETAINED_REVIEW_ATTEMPTS: u32 = 64;

#[cfg(test)]
thread_local! {
    static TEST_REVIEW_ATTEMPT_AFTER_APPEND: std::cell::RefCell<Option<Box<dyn FnOnce() -> bool>>> = const { std::cell::RefCell::new(None) };
    static TEST_REVIEW_BEFORE_FOLLOWUP_RESERVATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static TEST_REVIEW_CUSTODY_AFTER_CONFIRM_COMMIT: std::cell::RefCell<Option<Box<dyn FnOnce() -> bool>>> = const { std::cell::RefCell::new(None) };
    static TEST_REVIEW_CUSTODY_AFTER_NEUTRALIZE_COMMIT: std::cell::RefCell<Option<Box<dyn FnOnce() -> bool>>> = const { std::cell::RefCell::new(None) };
    static TEST_REVIEW_CUSTODY_AFTER_CUT_VALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static TEST_REVIEW_FOLLOWUP_AFTER_RECUT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static TEST_REVIEW_FOLLOWUP_AFTER_RECUT_ADOPTION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReviewAttemptProvenance {
    submission: String,
    submission_message_id: Option<String>,
    raw_output_available: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewAttemptReadState {
    original_project_id: Option<String>,
    current_submission_attempt: Option<u32>,
    /// Publication is historical truth, never certification of a later input.
    disposition: String,
}

#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReviewAttemptSnapshot {
    #[serde(default)]
    project_basis_recorded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    original_project_id: Option<String>,
    schema_version: u32,
    kind: String,
    parent_session_id: String,
    delegation_id: String,
    child_session_id: String,
    submission_attempt: u32,
    write_policy: DelegationWritePolicy,
    result: DelegationResult,
    provenance: ReviewAttemptProvenance,
    original_submission_body: Option<String>,
    raw_output: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SignedReviewAttempt {
    payload: String,
    authentication_tag: String,
}

fn review_attempt_key(id: &str, attempt: u32) -> String {
    format!("{REVIEW_ATTEMPT_KEY_PREFIX}-{id}-{attempt}")
}

fn review_attempt_corrupt() -> ApiError {
    ApiError::conflict("retained review attempt is corrupt or has unproven host provenance")
}

fn review_attempt_mac(key: &str, payload: &str) -> Result<Vec<u8>, ApiError> {
    // A fixed, NUL-terminated domain followed by one exact JSON payload.
    federation_mac(
        key.as_bytes(),
        &[REVIEW_ATTEMPT_TOPIC.as_bytes(), b"\0", payload.as_bytes()],
    )
    .map_err(|_| review_attempt_corrupt())
}

impl MailboxStore {
    // Opaque coordination metadata, preserved by existing migrations/readers.
    // Never returned through mailbox reads or created by a historical read.
    fn review_attempt_auth_key(&self, create: bool) -> Result<Option<zeroize::Zeroizing<String>>> {
        let _writer = if create {
            Some(self.lock_writer("retaining review result authentication")?)
        } else {
            None
        };
        let mut connection = self.connection()?;
        let read_key = |connection: &rusqlite::Connection| -> rusqlite::Result<Option<String>> {
            connection
                .query_row(
                    "SELECT value FROM meta WHERE key=?1",
                    rusqlite::params![REVIEW_ATTEMPT_AUTH_KEY],
                    |row| row.get(0),
                )
                .optional()
        };
        let key = if create {
            let transaction =
                begin_mailbox_write(&mut connection, "initializing review result authentication")?;
            if read_key(&transaction)?.is_none() {
                let history: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM mailbox_messages WHERE topic=?1)",
                    rusqlite::params![REVIEW_ATTEMPT_TOPIC],
                    |row| row.get(0),
                )?;
                if history {
                    bail!(
                        "review authentication key is missing over retained history; refusing regeneration"
                    )
                }
                let random =
                    zeroize::Zeroizing::new(federation_random::<32>().map_err(|_| {
                        anyhow!("OS randomness unavailable for review result custody")
                    })?);
                let generated = zeroize::Zeroizing::new(
                    random
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>(),
                );
                transaction.execute(
                    "INSERT OR IGNORE INTO meta(key,value) VALUES(?1,?2)",
                    rusqlite::params![REVIEW_ATTEMPT_AUTH_KEY, generated.as_str()],
                )?;
            }
            let winning = read_key(&transaction)?;
            transaction
                .commit()
                .context("failed to commit review authentication key")?;
            winning
        } else {
            read_key(&connection)?
        };
        if key
            .as_ref()
            .is_some_and(|k| k.len() != 64 || !k.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            bail!("review result authentication metadata is corrupt");
        }
        Ok(key.map(zeroize::Zeroizing::new))
    }
}

fn validate_review_snapshot(
    snapshot: &ReviewAttemptSnapshot,
    parent: &str,
    delegation: &str,
    child: &str,
    attempt: u32,
    policy: &DelegationWritePolicy,
) -> Result<(), ApiError> {
    if snapshot.schema_version != 1
        || snapshot.kind != REVIEW_ATTEMPT_TOPIC
        || snapshot.parent_session_id != parent
        || snapshot.delegation_id != delegation
        || snapshot.child_session_id != child
        || snapshot.submission_attempt != attempt
        || snapshot.write_policy != *policy
        || snapshot.result.delegation_id != delegation
        || snapshot.result.child_session_id != child
        || !delegation_is_terminal(snapshot.result.status)
        || snapshot.provenance.raw_output_available != snapshot.raw_output.is_some()
        || !matches!(
            snapshot.provenance.submission.as_str(),
            "validated" | "missing" | "corrupt"
        )
    {
        return Err(review_attempt_corrupt());
    }
    Ok(())
}

fn review_legacy_identity_error() -> ApiError {
    ApiError::conflict(
        "legacy review attempt identity is unavailable; the current evidence remains accessible, but continuation requires a new delegation",
    )
}

// Legacy project recovery uses only consistent native host facts. The new
// commissioning field and authenticated snapshot remain authoritative even if
// the parent later moves, disappears, pauses, or loses its runtime.
fn review_original_project_locked<'a>(
    inner: &'a StateInner,
    record: &'a DelegationRecord,
) -> Option<&'a str> {
    if record.project_basis_recorded || record.original_project_id.is_some() {
        return record.original_project_id.as_deref().filter(|project| !project.trim().is_empty());
    }
    let parent = &inner.sessions[inner.find_session_index(&record.parent_session_id)?];
    let child = &inner.sessions[inner.find_session_index(&record.child_session_id)?];
    if !parent.is_local_session()
        || !child.is_local_session()
        || parent.session.parent_delegation_id.is_some()
        || child.session.parent_delegation_id.as_deref() != Some(record.id.as_str())
    {
        return None;
    }
    let project = parent.session.project_id.as_deref()?;
    (!project.trim().is_empty() && child.session.project_id.as_deref() == Some(project))
        .then_some(project)
}

fn review_requester_project_locked(inner: &StateInner, requester: &str) -> Result<String, ApiError> {
    let index = inner.find_session_index(requester)
        .ok_or_else(|| ApiError::not_found("review result is unavailable"))?;
    let root = &inner.sessions[index];
    if root.hidden || !root.is_local_session()
        || root.engram.source_observation_delete_requested
        || root.session.parent_delegation_id.is_some()
        || inner.find_delegation_index_by_child_session_id(requester).is_some()
    {
        return Err(ApiError::not_found("review result is unavailable"));
    }
    root.session.project_id.as_ref().filter(|project| !project.trim().is_empty())
        .cloned().ok_or_else(|| ApiError::not_found("review result is unavailable"))
}

fn review_snapshot_matches_inner(inner: &StateInner, snapshot: &ReviewAttemptSnapshot) -> bool {
    review_snapshot_matches_before_prompt(inner, snapshot, None)
}

fn review_snapshot_matches_before_prompt(
    inner: &StateInner,
    snapshot: &ReviewAttemptSnapshot,
    admitted_prompt: Option<&str>,
) -> bool {
    let Some(index) = inner.find_delegation_index(&snapshot.delegation_id) else {
        return false;
    };
    let current = &inner.delegations[index];
    current.parent_session_id == snapshot.parent_session_id
        && review_original_project_locked(inner, current) == snapshot.original_project_id.as_deref()
        && current.child_session_id == snapshot.child_session_id
        && current.review_result_submission_attempt == snapshot.submission_attempt
        && current.write_policy == snapshot.write_policy
        && current.result.as_ref() == Some(&snapshot.result)
        && current.status == snapshot.result.status
        && delegation_is_terminal(current.status)
        && inner
            .find_session_index(&current.child_session_id)
            .and_then(|i| {
                delegation_child_output_before_prompt(
                    &inner.sessions[i].session,
                    current.status,
                    admitted_prompt,
                )
            })
            == snapshot.raw_output.as_deref()
}

impl AppState {
    fn review_attempt_read_state(&self, snapshot: &ReviewAttemptSnapshot, selected_id: &str) -> ReviewAttemptReadState {
        let (current_attempt, original_project_id) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let current = inner.find_delegation_index(&snapshot.delegation_id)
                .map(|i| &inner.delegations[i]);
            (current.map(|record| record.review_result_submission_attempt),
                snapshot.original_project_id.clone().or_else(|| (!snapshot.project_basis_recorded).then_some(current).flatten()
                    .and_then(|record| review_original_project_locked(&inner, record)).map(str::to_owned)))
        };
        // This is a live observation, separate from immutable result bytes.
        // Validate through the existing archive owner. A later damaged row
        // must neither revoke the selected id nor certify it as the head.
        let head = (|| -> Result<(Option<String>, bool, bool), ApiError> {
            let rows = self.review_custody_rows_read_only(Some(&snapshot.child_session_id), &snapshot.delegation_id)?;
            let key = self.mailbox_store.review_attempt_auth_key(false)
                .map_err(mailbox_api_error)?.ok_or_else(review_attempt_corrupt)?;
            let mut verified = Vec::new();
            let mut uncertain = false;
            for row in &rows {
                match decode_review_slots_with_chain(std::slice::from_ref(row),
                    &snapshot.parent_session_id, &snapshot.delegation_id,
                    &snapshot.child_session_id, &snapshot.write_policy, &key, false)
                {
                    Ok(slots) => verified.extend(slots),
                    Err(_) => uncertain = true,
                }
            }
            let later_attempt = verified.iter().any(|slot| slot.submission_attempt > snapshot.submission_attempt
                && slot.publication.is_some());
            if !verified.iter().any(|slot| slot.submission_attempt == snapshot.submission_attempt
                && slot.publication.as_ref().is_some_and(|proof| proof.result_id == selected_id)) {
                return Ok((None, true, later_attempt));
            }
            let latest = verified.iter().filter(|slot| slot.submission_attempt == snapshot.submission_attempt)
                .filter_map(|slot| slot.publication.as_ref()).max_by_key(|proof| proof.ordinal)
                .map(|proof| proof.result_id.clone());
            Ok((latest, uncertain, later_attempt))
        })();
        let superseded_publication = head.as_ref().is_ok_and(|(latest, _, _)|
            latest.as_deref().is_some_and(|latest| latest != selected_id));
        ReviewAttemptReadState {
            original_project_id,
            current_submission_attempt: current_attempt,
            disposition: if superseded_publication || head.as_ref().is_ok_and(|(_, _, later_attempt)| *later_attempt)
                || current_attempt.is_some_and(|attempt| attempt > snapshot.submission_attempt) {
                "superseded"
            } else if !head.as_ref().is_ok_and(|(latest, uncertain, _)|
                !uncertain && latest.as_deref() == Some(selected_id)) {
                "published_head_unavailable"
            } else if current_attempt == Some(snapshot.submission_attempt) {
                "published"
            } else {
                "published_current_unavailable"
            }.to_owned(),
        }
    }
    fn review_attempt_storage_enabled(&self) -> bool {
        #[cfg(test)]
        if self.mailbox_store.connection_if_enabled().is_none() {
            return false;
        }
        true
    }

    /// Exact-id archive projection. The requester stays itself; no parent API,
    /// recovery, publication, receipt finalization, or lifecycle refresh runs.
    fn read_exact_review_as_root(
        &self,
        requester: &str,
        delegation: &str,
        attempt: u32,
        result_id: &str,
    ) -> Result<(ReviewAttemptSnapshot, String), ApiError> {
        if attempt == 0 || result_id.trim().is_empty() {
            return Err(ApiError::bad_request(
                "exact review reads require a positive submissionAttempt and resultId",
            ));
        }
        let (requester_project, current) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let project = review_requester_project_locked(&inner, requester)?;
            let current = inner.find_delegation_index(delegation)
                .map(|i| inner.delegations[i].clone());
            if let Some(record) = current.as_ref() {
                if record.mode != DelegationMode::Reviewer {
                    return Err(ApiError::not_found("review result is unavailable"));
                }
                // Mutable legacy facts cannot override the selected archive's
                // stamped original project. Only immutable commissioning data
                // can reject the caller before the snapshot is authenticated.
                if let Some(original) = record.original_project_id.as_deref() {
                    if project != original {
                        return Err(ApiError::not_found("review result is unavailable"));
                    }
                }
            }
            (project, current)
        };
        let child = current.as_ref().map(|record| record.child_session_id.clone()).or_else(|| {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let mut children = inner.sessions.iter().filter(|record| record.is_local_session()
                && record.session.parent_delegation_id.as_deref() == Some(delegation));
            let first = children.next().map(|record| record.session.id.clone());
            if children.next().is_none() { first } else { None }
        });
        let records = self.review_custody_rows_read_only(child.as_deref(), delegation)?;
        if records.is_empty() {
            if let Some(record) = current.as_ref() {
                let inner = self.inner.lock().expect("state mutex poisoned");
                if review_original_project_locked(&inner, record).is_none() {
                    return Err(ApiError::conflict("project_basis_missing"));
                }
                if !delegation_is_terminal(record.status) {
                    return Err(ApiError::conflict(
                        "review_result_pending: attempt is pending, held, or unpromoted; no confirmed publication",
                    ));
                }
            }
            return Err(ApiError::not_found("review attempt has no confirmed publication"));
        }
        let key = self.mailbox_store.review_attempt_auth_key(false)
            .map_err(mailbox_api_error)?.ok_or_else(review_attempt_corrupt)?;
        for row in &records {
            // A damaged later publication cannot revoke an earlier exact id.
            let Ok(slot) = decode_review_value::<ReviewCustodySlot>(&row.body, &key) else {
                continue;
            };
            let Some(candidate) = slot.candidate.as_ref() else { continue; };
            if review_candidate_id(candidate)? != result_id { continue; }
            if slot.delegation_id != delegation || slot.submission_attempt != attempt
                || row.sender_session_id != slot.child_session_id
            {
                return Err(ApiError::not_found("review result is unavailable for this attempt"));
            }
            let body = serde_json::to_string(candidate).map_err(|_| review_attempt_corrupt())?;
            let snapshot: ReviewAttemptSnapshot = decode_review_value(&body, &key)?;
            let original_project = snapshot.original_project_id.clone().or_else(|| {
                if snapshot.project_basis_recorded { return None; }
                let inner = self.inner.lock().expect("state mutex poisoned");
                current.as_ref().filter(|record| {
                    record.parent_session_id == snapshot.parent_session_id
                        && record.child_session_id == snapshot.child_session_id
                }).and_then(|record| review_original_project_locked(&inner, record))
                    .map(str::to_owned)
            }).ok_or_else(|| ApiError::conflict("project_basis_missing"))?;
            if original_project != requester_project {
                return Err(ApiError::not_found("review result is unavailable"));
            }
            if current.as_ref().and_then(|record| record.original_project_id.as_deref())
                .is_some_and(|project| project != original_project)
                || current.as_ref().is_some_and(|record| record.project_basis_recorded
                    && record.original_project_id != snapshot.original_project_id)
            {
                return Err(review_attempt_corrupt());
            }
            decode_review_slots_with_chain(
                std::slice::from_ref(row), &snapshot.parent_session_id,
                delegation, &snapshot.child_session_id, &snapshot.write_policy, &key, false,
            )?;
            if !slot.publication.as_ref().is_some_and(|proof| proof.result_id == result_id) {
                return Err(ApiError::conflict(
                    "review_result_pending: requested candidate has no confirmed publication",
                ));
            }
            if snapshot.provenance.submission != "validated" {
                return Err(ApiError::conflict(
                    "review_result_unvalidated: original structured submission is missing or quarantined",
                ));
            }
            let inner = self.inner.lock().expect("state mutex poisoned");
            if review_requester_project_locked(&inner, requester)? != requester_project {
                return Err(ApiError::not_found("review result is unavailable"));
            }
            return Ok((snapshot, result_id.to_owned()));
        }
        Err(ApiError::conflict(
            "retained review result identity does not match a confirmed publication; history may be missing or quarantined",
        ))
    }

    fn review_attempt_authority(
        &self,
        parent: &str,
        delegation: &str,
        attempt: u32,
    ) -> Result<(String, DelegationWritePolicy, u32), ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = find_parent_delegation_index_locked(&inner, parent, delegation)?;
        let record = &inner.delegations[index];
        if attempt == 0 {
            return Err(review_legacy_identity_error());
        }
        if record.mode != DelegationMode::Reviewer
            || attempt > record.review_result_submission_attempt
        {
            return Err(ApiError::not_found(
                "review submission attempt is unavailable",
            ));
        }
        Ok((
            record.child_session_id.clone(),
            record.write_policy.clone(),
            record.review_result_submission_attempt,
        ))
    }

    // Caller owns the existing recovery guard. No StateInner lock spans I/O.
    fn review_custody_rows(
        &self,
        child: &str,
        delegation: &str,
    ) -> Result<Vec<MailboxIdempotencyRecord>, ApiError> {
        let records = self.review_custody_rows_read_only(Some(child), delegation)?;
        for record in &records {
            if record.dispatch_outcome.is_none() {
                self.mailbox_store
                    .record_initial_dispatch_outcome(&record.message_id, "durableButNotWoken")
                    .map_err(mailbox_api_error)?;
            }
        }
        Ok(records)
    }

    fn review_custody_rows_read_only(
        &self,
        child: Option<&str>,
        delegation: &str,
    ) -> Result<Vec<MailboxIdempotencyRecord>, ApiError> {
        let connection = self.mailbox_store.connection().map_err(mailbox_api_error)?;
        review_slot_rows_on(&connection, child, delegation).map_err(mailbox_api_error)
    }

    fn read_retained_review_attempt(
        &self,
        parent: &str,
        delegation: &str,
        attempt: u32,
    ) -> Result<Option<(ReviewAttemptSnapshot, String)>, ApiError> {
        self.read_review_publication(parent, delegation, attempt, None)
    }

    fn read_review_publication(
        &self,
        parent: &str,
        delegation: &str,
        attempt: u32,
        expected_id: Option<&str>,
    ) -> Result<Option<(ReviewAttemptSnapshot, String)>, ApiError> {
        let (child, policy, _) = self.review_attempt_authority(parent, delegation, attempt)?;
        // Pinned immutable reads do not join retention/recovery ownership.
        // Unpinned internal selection retains its existing serialization.
        let _owner = expected_id.is_none().then(|| self
            .mailbox_store
            .delegation_review_recovery_guard
            .lock()
            .expect("delegation review recovery guard mutex poisoned"));
        let records = self.review_custody_rows_read_only(Some(&child), delegation)?;
        if records.is_empty() {
            return Ok(None);
        }
        let key = self
            .mailbox_store
            .review_attempt_auth_key(false)
            .map_err(mailbox_api_error)?
            .ok_or_else(review_attempt_corrupt)?;
        let slots = if expected_id.is_some() {
            // A later damaged row cannot revoke an earlier independently
            // authenticated publication. Exact-id reads never substitute a
            // different version; unpinned selection still validates the chain.
            let mut verified = Vec::new();
            for record in &records {
                if let Ok(mut slot) = decode_review_slots_with_chain(
                    std::slice::from_ref(record),
                    parent,
                    delegation,
                    &child,
                    &policy,
                    &key,
                    false,
                ) {
                    verified.append(&mut slot);
                }
            }
            verified
        } else {
            decode_review_slots(&records, parent, delegation, &child, &policy, &key)?
        };
        let selected = slots
            .iter()
            .filter(|slot| {
                slot.submission_attempt == attempt
                    && slot
                        .publication
                        .as_ref()
                        .is_some_and(|proof| expected_id.is_none_or(|id| proof.result_id == id))
            })
            .max_by_key(|slot| {
                slot.publication
                    .as_ref()
                    .expect("confirmed selection")
                    .ordinal
            });
        let Some(slot) = selected else {
            return Ok(None);
        };
        let snapshot = decode_review_value(
            &serde_json::to_string(slot.candidate.as_ref().expect("confirmed candidate"))
                .map_err(|_| review_attempt_corrupt())?,
            &key,
        )?;
        Ok(Some((
            snapshot,
            slot.publication
                .as_ref()
                .expect("confirmed selection")
                .result_id
                .clone(),
        )))
    }

    fn capture_review_semantic_cut(
        &self,
        parent: &str,
        delegation: &str,
        attempt: u32,
    ) -> Result<ReviewAttemptSnapshot, ApiError> {
        let (cut, raw_output) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = find_parent_delegation_index_locked(&inner, parent, delegation)?;
            let record = &inner.delegations[index];
            if record.mode != DelegationMode::Reviewer
                || record.review_result_submission_attempt != attempt
                || !delegation_is_terminal(record.status)
                || record.result.is_none()
            {
                return Err(ApiError::not_found(
                    "review attempt was not retained; no historical transcript reconstruction is available",
                ));
            }
            if attempt == 0 {
                return Err(review_legacy_identity_error());
            }
            if attempt > MAX_RETAINED_REVIEW_ATTEMPTS {
                return Err(review_capacity_error());
            }
            let raw = inner
                .find_session_index(&record.child_session_id)
                .and_then(|i| {
                    delegation_child_authoritative_output(&inner.sessions[i].session, record.status)
                })
                .map(str::to_owned);
            (record.clone(), raw)
        };
        let submission = self
            .mailbox_store
            .read_idempotent_message(
                &cut.child_session_id,
                &delegation_review_result_idempotency_key(delegation, attempt),
            )
            .map_err(mailbox_api_error)?;
        let valid = submission.as_ref().is_some_and(|record| {
            validated_review_submission_envelope(
                record,
                parent,
                &cut.child_session_id,
                delegation,
                attempt,
            )
            .is_ok()
        });
        Ok(ReviewAttemptSnapshot {
            project_basis_recorded: true,
            original_project_id: {
                let inner = self.inner.lock().expect("state mutex poisoned");
                review_original_project_locked(&inner, &cut).map(str::to_owned)
            },
            schema_version: 1,
            kind: REVIEW_ATTEMPT_TOPIC.to_owned(),
            parent_session_id: parent.to_owned(),
            delegation_id: delegation.to_owned(),
            child_session_id: cut.child_session_id,
            submission_attempt: attempt,
            write_policy: cut.write_policy,
            result: cut.result.expect("checked terminal result"),
            provenance: ReviewAttemptProvenance {
                submission: if valid {
                    "validated"
                } else if submission.is_some() {
                    "corrupt"
                } else {
                    "missing"
                }
                .to_owned(),
                submission_message_id: submission.as_ref().map(|record| record.message_id.clone()),
                raw_output_available: raw_output.is_some(),
            },
            original_submission_body: submission.map(|record| record.body),
            raw_output,
        })
    }

    fn write_new_review_slot(&self, slot: &ReviewCustodySlot, key: &str) -> Result<(), ApiError> {
        let body = encode_review_value(slot, key)?;
        if body.len() > MAX_MAILBOX_BODY_BYTES {
            return Err(ApiError::conflict(
                "complete review candidate plus publication proof exceeds 256 KiB",
            ));
        }
        self.mailbox_store
            .append_review_custody_slot(
                &MailboxAppendInput {
                    sender_session_id: slot.child_session_id.clone(),
                    sender_name: slot.child_session_id.clone(),
                    target_session_id: slot.parent_session_id.clone(),
                    target_name: slot.parent_session_id.clone(),
                    body,
                    idempotency_key: review_attempt_key(&slot.delegation_id, slot.slot),
                    topic: Some(REVIEW_ATTEMPT_TOPIC.to_owned()),
                    state_stamp: Some(format!("{}:{}", slot.delegation_id, slot.slot)),
                },
                &slot.delegation_id,
            )
            .map_err(mailbox_api_error)
    }

    fn retain_review_attempt(
        &self,
        parent: &str,
        delegation: &str,
        attempt: u32,
    ) -> Result<(ReviewAttemptSnapshot, String), ApiError> {
        let (child, policy, current_attempt) =
            self.review_attempt_authority(parent, delegation, attempt)?;
        if attempt < current_attempt {
            return self.read_retained_review_attempt(parent, delegation, attempt)?
                .ok_or_else(|| ApiError::not_found("review attempt was not retained; no historical transcript reconstruction is available"));
        }
        // Recovery and the existing terminal refresh precede any current cut.
        // Pinned historical reads take read_review_publication instead.
        self.recover_durable_delegation_review_submission(&child)?;
        self.get_delegation(parent, delegation)?;
        let _owner = self
            .mailbox_store
            .delegation_review_recovery_guard
            .lock()
            .expect("delegation review recovery guard mutex poisoned");
        let snapshot = self.capture_review_semantic_cut(parent, delegation, attempt)?;
        let key = self
            .mailbox_store
            .review_attempt_auth_key(true)
            .map_err(mailbox_api_error)?
            .ok_or_else(review_attempt_corrupt)?;
        let candidate: SignedReviewAttempt =
            serde_json::from_str(&encode_review_value(&snapshot, &key)?)
                .map_err(|_| review_attempt_corrupt())?;
        let id = review_candidate_id(&candidate)?;
        let mut records = self.review_custody_rows(&child, delegation)?;
        let mut slots = decode_review_slots(&records, parent, delegation, &child, &policy, &key)?;
        let already_confirmed = slots.iter().any(|slot| {
            slot.publication
                .as_ref()
                .is_some_and(|proof| proof.result_id == id)
        });
        if let Some(index) = slots
            .iter()
            .position(|slot| slot.candidate.is_some() && slot.publication.is_none())
        {
            let old = &slots[index];
            if old.submission_attempt > attempt {
                return Err(ApiError::conflict(
                    "a future review attempt owns the unpublished candidate; no custody was changed",
                ));
            }
            if old.submission_attempt < attempt || already_confirmed {
                if review_candidate_id(old.candidate.as_ref().expect("pending candidate"))? == id {
                    return Err(review_attempt_corrupt());
                }
                // Failed recuts stay charged to their original attempt. Clearing
                // only their unpublished payload keeps them from blocking the
                // newer attempt's own protected first-capture reservation.
                let mut cleared = old.clone();
                cleared.candidate = None;
                cleared.publication = None;
                self.mailbox_store.replace_review_custody_slot(
                    &records[index],
                    old,
                    &cleared,
                    &policy,
                    &key,
                    review_publication_head(&slots).map(|head| head.result_id.as_str()),
                )?;
                records = self.review_custody_rows(&child, delegation)?;
                slots = decode_review_slots(&records, parent, delegation, &child, &policy, &key)?;
            }
        }
        if already_confirmed {
            return Ok((snapshot, id));
        }
        let head = review_publication_head(&slots);
        let predecessor = head.map(|head| head.result_id.clone());
        let ordinal = head.map_or(1, |head| head.ordinal + 1);
        let pending = slots
            .iter()
            .position(|slot| slot.candidate.is_some() && slot.publication.is_none());
        let reserved = slots
            .iter()
            .position(|slot| slot.candidate.is_none() && slot.submission_attempt == attempt);
        let index = if let Some(index) = pending.or(reserved) {
            if slots[index].submission_attempt != attempt {
                return Err(ApiError::conflict(
                    "another review attempt owns the unpublished candidate; no custody was changed",
                ));
            }
            index
        } else {
            if slots.len() >= MAX_RETAINED_REVIEW_ATTEMPTS as usize {
                return Err(review_capacity_error());
            }
            let slot = ReviewCustodySlot {
                schema_version: 1,
                kind: REVIEW_CUSTODY_SLOT_KIND.to_owned(),
                parent_session_id: parent.to_owned(),
                delegation_id: delegation.to_owned(),
                child_session_id: child.clone(),
                slot: slots.len() as u32 + 1,
                submission_attempt: attempt,
                candidate: None,
                publication: None,
            };
            self.write_new_review_slot(&slot, &key)?;
            records = self.review_custody_rows(&child, delegation)?;
            slots = decode_review_slots(&records, parent, delegation, &child, &policy, &key)?;
            slots
                .iter()
                .position(|saved| saved.slot == slot.slot)
                .expect("new reservation read back")
        };
        let old = slots[index].clone();
        let mut pending = old.clone();
        pending.candidate = Some(candidate);
        pending.publication = None;
        let mut confirmed = pending.clone();
        confirmed.publication = Some(ReviewPublicationProof {
            result_id: id.clone(),
            predecessor_result_id: predecessor.clone(),
            ordinal,
        });
        if encode_review_value(&confirmed, &key)?.len() > MAX_MAILBOX_BODY_BYTES {
            return Err(ApiError::conflict(
                "complete review candidate plus publication proof exceeds 256 KiB",
            ));
        }
        let pending_body = self.mailbox_store.replace_review_custody_slot(
            &records[index],
            &old,
            &pending,
            &policy,
            &key,
            predecessor.as_deref(),
        )?;
        #[cfg(test)]
        let lost_reply = TEST_REVIEW_ATTEMPT_AFTER_APPEND
            .with(|slot| slot.borrow_mut().take())
            .is_some_and(|hook| hook());
        {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if !review_snapshot_matches_inner(&inner, &snapshot) {
                return Err(ApiError::conflict(
                    "review terminal cut changed while retention was being committed; no follow-up was admitted",
                ));
            }
        }
        #[cfg(test)]
        if let Some(hook) =
            TEST_REVIEW_CUSTODY_AFTER_CUT_VALIDATION.with(|slot| slot.borrow_mut().take())
        {
            hook();
        }
        records[index].body = pending_body;
        self.mailbox_store.replace_review_custody_slot(
            &records[index],
            &pending,
            &confirmed,
            &policy,
            &key,
            predecessor.as_deref(),
        )?;
        // The exact authenticated publication is now durable even if a reply is lost.
        #[cfg(test)]
        if lost_reply {
            let rows = self.review_custody_rows(&child, delegation)?;
            let saved = decode_review_slots(&rows, parent, delegation, &child, &policy, &key)?;
            if !saved.iter().any(|slot| {
                slot.publication
                    .as_ref()
                    .is_some_and(|proof| proof.result_id == id)
            }) {
                return Err(ApiError::internal(
                    "review publication commit is unconfirmed",
                ));
            }
        }
        Ok((snapshot, id))
    }

    fn reserve_next_review_capture(
        &self,
        snapshot: &ReviewAttemptSnapshot,
    ) -> Result<String, ApiError> {
        let next = snapshot
            .submission_attempt
            .checked_add(1)
            .filter(|next| *next <= MAX_RETAINED_REVIEW_ATTEMPTS)
            .ok_or_else(review_capacity_error)?;
        let _owner = self
            .mailbox_store
            .delegation_review_recovery_guard
            .lock()
            .expect("delegation review recovery guard mutex poisoned");
        {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if !review_snapshot_matches_inner(&inner, snapshot) {
                return Err(ApiError::conflict(
                    "review terminal cut changed before reserving the next capture",
                ));
            }
        }
        let key = self
            .mailbox_store
            .review_attempt_auth_key(false)
            .map_err(mailbox_api_error)?
            .ok_or_else(review_attempt_corrupt)?;
        let rows = self.review_custody_rows(&snapshot.child_session_id, &snapshot.delegation_id)?;
        let slots = decode_review_slots(
            &rows,
            &snapshot.parent_session_id,
            &snapshot.delegation_id,
            &snapshot.child_session_id,
            &snapshot.write_policy,
            &key,
        )?;
        if let Some(slot) = slots.iter().find(|slot| slot.submission_attempt == next) {
            if slot.candidate.is_some() {
                return Err(ApiError::conflict(
                    "the next attempt already owns a captured slot; stale follow-up reservation was refused",
                ));
            }
            return Ok(review_attempt_key(&snapshot.delegation_id, slot.slot));
        }
        if slots.len() >= MAX_RETAINED_REVIEW_ATTEMPTS as usize {
            return Err(review_capacity_error());
        }
        let slot = ReviewCustodySlot {
            schema_version: 1,
            kind: REVIEW_CUSTODY_SLOT_KIND.to_owned(),
            parent_session_id: snapshot.parent_session_id.clone(),
            delegation_id: snapshot.delegation_id.clone(),
            child_session_id: snapshot.child_session_id.clone(),
            slot: slots.len() as u32 + 1,
            submission_attempt: next,
            candidate: None,
            publication: None,
        };
        self.write_new_review_slot(&slot, &key)?;
        Ok(review_attempt_key(&snapshot.delegation_id, slot.slot))
    }

    fn resolve_review_selection(
        &self,
        parent: &str,
        delegation: &str,
        attempt: u32,
        id: Option<&str>,
    ) -> Result<(ReviewAttemptSnapshot, String), ApiError> {
        if let Some(id) = id {
            let is_parent = {
                let inner = self.inner.lock().expect("state mutex poisoned");
                inner.find_delegation_index(delegation)
                    .is_some_and(|i| inner.delegations[i].parent_session_id == parent)
            };
            if !is_parent {
                return self.read_exact_review_as_root(parent, delegation, attempt, id);
            }
            return self.read_review_publication(parent, delegation, attempt, Some(id))?
                .ok_or_else(|| ApiError::conflict(
                    "retained review result identity does not match a confirmed publication",
                ));
        }
        // Internal custody must match the actual cut being cleared. Explicit
        // API selection instead returns the latest confirmed version, even if
        // the current semantic cut has returned to an older published value.
        self.retain_review_attempt(parent, delegation, attempt)?;
        self.read_retained_review_attempt(parent, delegation, attempt)?
            .ok_or_else(|| ApiError::not_found("review attempt has no confirmed publication"))
    }

    fn get_delegation_result_selected(
        &self,
        parent: &str,
        delegation: &str,
        selection: &DelegationResultSelection,
    ) -> Result<DelegationResultResponse, ApiError> {
        let Some(attempt) = selection.submission_attempt else {
            if selection.result_id.is_some() {
                return Err(ApiError::bad_request("resultId requires submissionAttempt"));
            }
            return self.get_delegation_result(parent, delegation);
        };
        let (snapshot, id) = self.resolve_review_selection(
            parent,
            delegation,
            attempt,
            selection.result_id.as_deref(),
        )?;
        if selection
            .result_id
            .as_ref()
            .is_some_and(|expected| expected != &id)
        {
            return Err(ApiError::conflict(
                "retained review result identity does not match",
            ));
        }
        let revision = self.inner.lock().expect("state mutex poisoned").revision;
        Ok(DelegationResultResponse {
            review_attempt_state: Some(self.review_attempt_read_state(&snapshot, &id)),
            revision,
            result: delegation_result_for_policy(&snapshot.result, &snapshot.write_policy)
                .into_owned(),
            submission_attempt: Some(attempt),
            result_id: Some(id),
            review_attempt_provenance: Some(snapshot.provenance),
            acceptance_evaluation: None,
            server_instance_id: self.server_instance_id.clone(),
        })
    }

    fn get_delegation_result_output_selected(
        &self,
        parent: &str,
        delegation: &str,
        offset: usize,
        limit: usize,
        selection: &DelegationResultSelection,
    ) -> Result<DelegationResultOutputResponse, ApiError> {
        validate_delegation_result_output_page(limit)?;
        if selection.submission_attempt.is_none() {
            if selection.result_id.is_some() {
                return Err(ApiError::bad_request("resultId requires submissionAttempt"));
            }
            // Unpinned default preserves its documented current-attempt view.
            // It does not advertise immutable output or a result identity.
            return self.get_delegation_result_output(parent, delegation, offset, limit);
        }
        let attempt = selection
            .submission_attempt
            .expect("explicit selector checked");
        let (snapshot, id) = self.resolve_review_selection(
            parent,
            delegation,
            attempt,
            selection.result_id.as_deref(),
        )?;
        if selection
            .result_id
            .as_ref()
            .is_some_and(|expected| expected != &id)
        {
            return Err(ApiError::conflict(
                "retained review result identity does not match",
            ));
        }
        let output = snapshot.raw_output.as_deref().ok_or_else(|| {
            ApiError::not_found(
                "raw output was unavailable at the retained terminal cut; no summary fallback",
            )
        })?;
        let page = delegation_result_output_page(output, offset, limit)?;
        let revision = self.inner.lock().expect("state mutex poisoned").revision;
        Ok(DelegationResultOutputResponse {
            review_attempt_state: Some(self.review_attempt_read_state(&snapshot, &id)),
            revision,
            delegation_id: delegation.to_owned(),
            child_session_id: snapshot.child_session_id,
            submission_attempt: Some(attempt),
            result_id: Some(id),
            output: page.output,
            offset_bytes: offset,
            next_offset_bytes: page.next_offset_bytes,
            total_bytes: output.len(),
            complete: page.next_offset_bytes.is_none(),
            summary_fallback: false,
            server_instance_id: self.server_instance_id.clone(),
        })
    }
}
