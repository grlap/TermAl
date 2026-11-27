// Disposition of retired checks and terminally refused records. Retired-check
// receipts stay with the original run; a rejected check's disclosure is only
// for the fresh request. Neither changes verification credit or root authority.

use super::*;

pub(crate) const ACCEPTANCE_RETIRED_CHECK_DISCLOSURE_MAX: usize = 16 * 1024;

// Build the required brief with room for a short notes pointer first. Use
// more detailed audit context only from the bytes left over. If the contract
// needs even the pointer's space, the attributed notes remain in the catalog.
pub(crate) fn acceptance_brief_with_retired_context<T>(
    budget: usize,
    notices: &[String],
    build: impl Fn(usize) -> Result<T, ApiError>,
    size: impl Fn(&T) -> usize,
) -> Result<(T, &str, usize), ApiError> {
    if let Some(pointer) = notices.last()
        && let Ok(brief) = build(budget.saturating_sub(pointer.len()))
        && pointer.len() <= budget.saturating_sub(size(&brief))
    {
        let remaining = budget - size(&brief);
        let notice = notices
            .iter()
            .find(|notice| notice.len() <= remaining)
            .map_or("", String::as_str);
        return Ok((brief, notice, budget - notice.len()));
    }
    build(budget).map(|brief| (brief, "", budget))
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AcceptanceRetiredCheckReceipt {
    body_digest: String,
    evidence_record: String,
}

pub(crate) fn acceptance_effective_retired_check_receipt<'a>(
    inner: &StateInner,
    row: &'a TestRunRegistration,
    store: &EngramAuthorityStoreKey,
    root: &AcceptanceEvaluationSourceRoot,
) -> Option<&'a AcceptanceRetiredCheckReceipt> {
    let receipt = acceptance_scoped_retired_check_receipt(row, store, root)?;
    (inner.pending_retired_check_receipts.get(&row.run_id) != Some(receipt)).then_some(receipt)
}

/// Effective exclusions at the tracker evidence bracket. The same set
/// supplies audit references and selects the registrations to skip.
#[derive(PartialEq, Eq)]
pub(crate) struct AcceptanceRetiredCheckSnapshot {
    store: EngramAuthorityStoreKey,
    pub(crate) root: AcceptanceEvaluationSourceRoot,
    receipts: BTreeMap<String, (u64, AcceptanceRetiredCheckReceipt)>,
}

impl AcceptanceRetiredCheckSnapshot {
    pub(crate) fn capture_locked(
        inner: &StateInner,
        store: &EngramAuthorityStoreKey,
        root: AcceptanceEvaluationSourceRoot,
    ) -> Self {
        let indexed = test_run_indexed_ids(&inner.test_runs);
        let receipts = acceptance_published_registrations(inner, &indexed)
            .filter_map(|row| {
                let receipt = acceptance_effective_retired_check_receipt(inner, row, store, &root)?;
                Some((
                    row.run_id.clone(),
                    (row.named_root.as_ref()?.generation, receipt.clone()),
                ))
            })
            .collect();
        Self {
            store: store.clone(),
            root,
            receipts,
        }
    }

    pub(crate) fn references(&self) -> Vec<Value> {
        self.receipts
            .iter()
            .map(|(run_id, (generation, receipt))| {
                json!({
                    "runId": run_id,
                    "generation": generation,
                    "evidenceRecord": receipt.evidence_record,
                })
            })
            .collect()
    }

    pub(crate) fn excludes(&self, run_id: &str) -> bool {
        self.receipts.contains_key(run_id)
    }

    pub(crate) fn require_unchanged_locked(
        &self,
        inner: &StateInner,
    ) -> Result<(), AcceptanceCommittedInputRefusal> {
        if self != &Self::capture_locked(inner, &self.store, self.root.clone()) {
            return Err(AcceptanceCommittedInputRefusal::RetiredChecksChanged);
        }
        Ok(())
    }
}

fn acceptance_scoped_retired_check_receipt<'a>(
    row: &'a TestRunRegistration,
    store: &EngramAuthorityStoreKey,
    root: &AcceptanceEvaluationSourceRoot,
) -> Option<&'a AcceptanceRetiredCheckReceipt> {
    let receipt = row.retired_check_disposition.as_ref()?;
    (row.authority_store.as_ref() == Some(store)
        && row.work_binding.as_ref().is_some_and(|binding| {
            binding.work_id == root.work_id && binding.claim_id == root.claim_id
        })
        && row
            .named_root
            .as_ref()
            .is_some_and(|named| named.generation < root.generation)
        && row
            .verification_record
            .as_ref()
            .and_then(TestRunVerificationAck::body_digest)
            == Some(receipt.body_digest.as_str()))
    .then_some(receipt)
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AcceptanceRetiredCheckRequest {
    pub(crate) run_id: String,
    pub(crate) reason: String,
}

pub(crate) struct AcceptanceRetiredCheckDisposition {
    pub(crate) registration: TestRunRegistration,
    pub(crate) current: AcceptanceEvaluationSourceRoot,
    pub(crate) identity: AcceptanceEvidenceIdentity,
    pub(crate) disclosure: Value,
    // A rejected run remains in the committed selection. Only retired runs
    // are excluded. This source binds the disclosure to the fresh evidence cut.
    pub(crate) rejected_source: Option<EngramSourceId>,
}

pub(crate) struct RecordedAcceptanceCheckDispositions {
    pub(crate) disclosures: Vec<AcceptanceRetiredCheckDisposition>,
    root: AcceptanceEvaluationSourceRoot,
    identity: AcceptanceEvidenceIdentity,
    retained_receipts: Vec<TestRunRegistration>,
}

impl RecordedAcceptanceCheckDispositions {
    pub(crate) fn require_unchanged(
        &self,
        task: &AcceptanceEvaluationTask,
        root: Option<&AcceptanceEvaluationSourceRoot>,
        snapshot: Option<&AcceptanceRetiredCheckSnapshot>,
    ) -> Result<(), ApiError> {
        if root != Some(&self.root) || task.canonical_identity.as_ref() != Some(&self.identity) {
            return Err(ApiError::conflict(
                "check disposition source authority changed",
            ));
        }
        for expected in &self.retained_receipts {
            let actual = snapshot.and_then(|snapshot| snapshot.receipts.get(&expected.run_id));
            if !actual.is_some_and(|(generation, receipt)| {
                expected.retired_check_disposition.as_ref() == Some(receipt)
                    && expected.named_root.as_ref().map(|named| named.generation)
                        == Some(*generation)
            }) {
                return Err(AcceptanceCommittedInputRefusal::RetiredChecksChanged
                    .into_api_error(&task.work_ref));
            }
        }
        Ok(())
    }
}

fn require_check_disposition_reader_locked(
    inner: &StateInner,
    parent: &str,
    connection: &EngramConnectionConfig,
    store: &EngramAuthorityStoreKey,
) -> Result<(), ApiError> {
    let current = acceptance_evaluation_host_target_locked(inner, parent)?;
    if current.store != *store {
        return Err(ApiError::conflict(
            ACCEPTANCE_EVALUATION_STORE_CHANGED_ERROR,
        ));
    }
    if current.connection != *connection {
        return Err(ApiError::conflict(
            "check disposition source authority changed",
        ));
    }
    Ok(())
}

struct RetiredCheckNoteWrites<'a> {
    inner: &'a StateMutex<StateInner>,
    run_ids: Vec<String>,
}

impl Drop for RetiredCheckNoteWrites<'_> {
    fn drop(&mut self) {
        if !self.run_ids.is_empty() {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            for run_id in &self.run_ids {
                inner.retired_check_note_writes.remove(run_id);
            }
        }
    }
}

fn acceptance_retired_check_disclosure(
    registration: &TestRunRegistration,
    request: &AcceptanceRetiredCheckRequest,
    root: &AcceptanceEvaluationSourceRoot,
    parent: &str,
    author: &(String, Option<String>),
) -> Result<Value, ApiError> {
    use std::io::Read as _;

    let checked_root = registration
        .named_root
        .as_ref()
        .expect("validated retired root");
    let ack = registration
        .verification_record
        .as_ref()
        .expect("validated receipt");
    let digest = ack.body_digest().expect("validated acknowledged digest");
    let kind = "retired_root_check_excluded_from_evaluation_admission";
    let scope = "retired check admission on this claim only; no verification credit, pass or waiver of criteria";
    let body_path =
        FsPath::new(&registration.run_directory).join(ENGRAM_VERIFICATION_RECORD_BODY_FILE);
    let unreadable = |_| {
        ApiError::conflict(
            "retired check disposition cannot read its original acknowledged verification body",
        )
    };
    // Opening a dangling symlink also returns NotFound. Check the entry itself.
    let file = match fs::File::open(&body_path) {
        Ok(file) => file,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && checked_root.workspace_id != root.root
                && digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                && fs::symlink_metadata(&body_path)
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(json!({
                "kind": kind,
                "runId": request.run_id, "reason": request.reason,
                "author": {"sessionId": parent, "actorId": author.0, "actorContext": author.1},
                "workId": root.work_id, "claimId": root.claim_id,
                "checkedRoot": checked_root, "currentRoot": root,
                "requestIdentity": registration.request_identity,
                "verificationBodyDigest": digest,
                "originalVerificationArtifact": "unavailable",
                "artifactOutcome": "unknown",
                "scope": scope
            }));
        }
        Err(error) => return Err(unreadable(error)),
    };
    // The frozen body is contained in the producer's bounded frame.
    // The smaller disclosure budget only governs the note and brief.
    let mut text = String::new();
    file.take((ACCEPTANCE_VERIFICATION_FRAME_MAX_BYTES + 1) as u64)
        .read_to_string(&mut text)
        .map_err(unreadable)?;
    if text.len() > ACCEPTANCE_VERIFICATION_FRAME_MAX_BYTES {
        return Err(ApiError::conflict(
            "retired check disposition original body exceeds the verification frame bound",
        ));
    }
    let (_, body) = engram_verification_record_parse(&text).ok_or_else(|| {
        ApiError::conflict("retired check disposition original body is malformed")
    })?;
    let receipt = match ack {
        TestRunVerificationAck::Received { receipt, .. }
        | TestRunVerificationAck::Settled { receipt, .. } => receipt,
        _ => unreachable!("validated receipt"),
    };
    // The acknowledged immutable digest remains authoritative even
    // when old mutable artifacts have disappeared. Rebuilding from
    // those files would make an honest unknown result undisposable.
    if body.registration.host_run_id != registration.run_id
        || body.registration.request_identity != registration.request_identity
        || registration.work_binding.as_ref() != Some(&body.registration.binding)
        || checked_root.workspace_id != body.registration.root.workspace_id
        || checked_root.generation != body.registration.root.generation as u64
        || !engram_verification_record_digest_answers(digest, &body)
    {
        return Err(ApiError::conflict(
            "retired check disposition cannot authenticate the original verification body",
        ));
    }
    let artifact =
        match test_run_read_json(&FsPath::new(&registration.run_directory).join("results.json")) {
            TestRunJson::Parsed(result, _, _) => result
                .get("state")
                .cloned()
                .unwrap_or_else(|| json!("unknown")),
            _ => json!("unknown"),
        };
    // Preserve a historical receipt disagreement without treating it
    // as passed evidence or silently repairing the receipt.
    let receipt_error = engram_verification_record_receipt_check(&body, receipt).err();
    let inline = (text.len() <= ACCEPTANCE_RETIRED_CHECK_DISCLOSURE_MAX).then(|| {
        json!({
            "kind": kind,
            "runId": request.run_id, "reason": request.reason,
            "author": {"sessionId": parent, "actorId": author.0, "actorContext": author.1},
            "workId": root.work_id, "claimId": root.claim_id,
            "checkedRoot": registration.named_root, "currentRoot": root,
            "requestIdentity": registration.request_identity,
            "verificationBodyDigest": ack.body_digest(),
            "originalVerification": body,
            "originalAcknowledgement": ack,
            "receiptValidationError": receipt_error,
            "artifactOutcome": artifact,
            "scope": scope
        })
    });
    let inline =
        inline.filter(|value| value.to_string().len() <= ACCEPTANCE_RETIRED_CHECK_DISCLOSURE_MAX);
    Ok(if let Some(inline) = inline {
        inline
    } else {
        let verified_receipt = receipt_error.is_none().then_some(receipt);
        json!({
            "kind": kind,
            "runId": request.run_id,
            "reason": request.reason,
            "checkedRootGeneration": body.registration.root.generation,
            "currentRootGeneration": root.generation,
            "verificationBodyDigest": ack.body_digest(),
            "originalVerificationReference": {
                "artifact": ENGRAM_VERIFICATION_RECORD_BODY_FILE,
                "producerRecord": verified_receipt.map(|receipt| &receipt.producer),
                "verificationRecord": verified_receipt
                    .and_then(|receipt| receipt.verifications.first())
                    .map(|verification| &verification.record_id)
            },
            "originalRunOutcome": body.run.outcome,
            "receiptValidationError": receipt_error,
            "artifactOutcome": artifact.as_str()
                .filter(|state| matches!(*state, "running" | "passed" | "failed" | "unknown"))
                .unwrap_or("unknown"),
            "scope": scope
        })
    })
}

fn retained_registration<'a>(
    inner: &'a StateInner,
    run_id: &str,
) -> Option<&'a TestRunRegistration> {
    inner
        .test_run_registrations
        .get(run_id)
        .filter(|_| inner.test_runs.registered.contains(run_id))
        .or_else(|| {
            inner
                .test_run_compacted
                .get(run_id)
                .filter(|row| row.compacted && test_run_compact_is_installed(inner, row))
                .and_then(|row| row.registration.as_ref())
        })
}

pub(crate) fn acceptance_retired_context_notices(
    inline: &[&AcceptanceRetiredCheckDisposition],
    references: &[Value],
) -> Result<Vec<String>, ApiError> {
    if inline.is_empty() && references.is_empty() {
        return Ok(Vec::new());
    }
    let reference_notice = |rows: &[&Value]| -> Result<String, ApiError> {
        if rows.is_empty() {
            return Ok(String::new());
        }
        let json =
            serde_json::to_string(rows).map_err(|error| ApiError::internal(error.to_string()))?;
        Ok(format!(
            "Audited retired check exclusions on this claim (context only, no verification credit). The notes record the reasons and available results, with unavailable outcomes marked unknown:\n{json}\n\n"
        ))
    };
    let mut notices = Vec::new();
    if !inline.is_empty() {
        let inline_ids: HashSet<_> = inline
            .iter()
            .map(|row| row.registration.run_id.as_str())
            .collect();
        let inherited: Vec<_> = references
            .iter()
            .filter(|row| {
                row["runId"]
                    .as_str()
                    .is_some_and(|id| !inline_ids.contains(id))
            })
            .collect();
        let disclosures: Vec<_> = inline.iter().map(|row| &row.disclosure).collect();
        let json = serde_json::to_string(&disclosures)
            .map_err(|error| ApiError::internal(error.to_string()))?;
        let full = format!(
            "Host-recorded retired check dispositions (context only, no verification credit):\n{json}\n\n{}",
            reference_notice(&inherited)?,
        );
        if full.len() <= ACCEPTANCE_RETIRED_CHECK_DISCLOSURE_MAX {
            notices.push(full);
        }
    }
    let summary = reference_notice(&references.iter().collect::<Vec<_>>())?;
    if !summary.is_empty() && summary.len() <= ACCEPTANCE_RETIRED_CHECK_DISCLOSURE_MAX {
        notices.push(summary);
    }
    notices.push("See the tracker notes for audited retired check exclusions on this claim (context only, no verification credit).\n\n".to_owned());
    Ok(notices)
}

pub(crate) fn validate_acceptance_check_requests(
    requests: &[AcceptanceRetiredCheckRequest],
    kind: &str,
) -> Result<(), ApiError> {
    let mut seen = BTreeSet::new();
    for request in requests {
        if request.run_id.is_empty()
            || request.run_id.len() > 256
            || request.run_id.chars().any(char::is_control)
            || request.reason.trim().is_empty()
            || request.reason.len() > 1024
            || request
                .reason
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
            || !seen.insert(request.run_id.as_str())
        {
            return Err(ApiError::bad_request(format!(
                "{kind} check dispositions require distinct exact run IDs and a nonempty reason of at most 1024 bytes"
            )));
        }
    }
    Ok(())
}

impl AppState {
    fn reserve_retired_check_note_writes(
        &self,
        store: &EngramAuthorityStoreKey,
        root: &AcceptanceEvaluationSourceRoot,
        selected: &[AcceptanceRetiredCheckDisposition],
    ) -> Result<RetiredCheckNoteWrites<'_>, ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if !root.still_named(&inner.engram_work_source_roots, store) {
            return Err(ApiError::conflict(
                "check disposition source authority changed",
            ));
        }
        let mut run_ids = Vec::new();
        for disposition in selected.iter().filter(|row| row.rejected_source.is_none()) {
            let run_id = &disposition.registration.run_id;
            if inner.retired_check_note_writes.contains(run_id)
                || !retained_registration(&inner, run_id).is_some_and(|row| {
                    acceptance_committed_input_registration_unchanged(
                        &disposition.registration,
                        row,
                    ) && acceptance_scoped_retired_check_receipt(row, store, root).is_none()
                })
            {
                return Err(ApiError::conflict(format!(
                    "retired check {run_id} changed or has a disposition being recorded; request again"
                )));
            }
            run_ids.push(run_id.clone());
        }
        inner
            .retired_check_note_writes
            .extend(run_ids.iter().cloned());
        Ok(RetiredCheckNoteWrites {
            inner: &self.inner,
            run_ids,
        })
    }

    fn acceptance_check_disposition_registration(
        &self,
        parent: &str,
        connection: &EngramConnectionConfig,
        store: &EngramAuthorityStoreKey,
        task: &AcceptanceEvaluationTask,
        root: &AcceptanceEvaluationSourceRoot,
        run_id: &str,
        rejected: bool,
    ) -> Result<TestRunRegistration, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let target = acceptance_evaluation_host_target_locked(&inner, parent)?;
        if target.store != *store
            || target.connection != *connection
            || !root.still_named(&inner.engram_work_source_roots, store)
            || task.canonical_work_id() != Some(root.work_id.as_str())
        {
            return Err(ApiError::conflict(
                "check disposition source authority changed",
            ));
        }
        let row = retained_registration(&inner, run_id).ok_or_else(|| {
            ApiError::bad_request(format!("check {run_id} is not a retained registered run"))
        })?;
        let same_claim = row.authority_store.as_ref() == Some(store)
            && row.work_binding.as_ref().is_some_and(|binding| {
                binding.work_id == root.work_id && binding.claim_id == root.claim_id
            });
        if rejected {
            if !same_claim
                || !row.named_root.as_ref().is_some_and(|named| {
                    named.workspace_id == root.root && named.generation == root.generation
                })
                || !engram_verification_rejected_by_producer(row.verification_record.as_ref())
            {
                return Err(ApiError::bad_request(format!(
                    "check {run_id} is not a current-root run of this claim with a host-held terminal verification rejection"
                )));
            }
        } else if !same_claim
            || !row
                .named_root
                .as_ref()
                .is_some_and(|named| named.generation < root.generation)
            || !row
                .verification_record
                .as_ref()
                .is_some_and(|ack| ack.body_digest().is_some())
        {
            return Err(ApiError::bad_request(format!(
                "check {run_id} is not an acknowledged check on this claim's retired root; current-root checks cannot be excluded"
            )));
        }
        Ok(row.clone())
    }

    #[allow(clippy::too_many_arguments)]
    fn confirm_acceptance_retired_check_receipt(
        &self,
        parent: &str,
        connection: &EngramConnectionConfig,
        store: &EngramAuthorityStoreKey,
        expected: &TestRunRegistration,
        root: &AcceptanceEvaluationSourceRoot,
        evidence_record: Option<&str>,
        deadline: std::time::Instant,
    ) -> Result<(), ApiError> {
        let run_id = &expected.run_id;
        let receipt = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let target = acceptance_evaluation_host_target_locked(&inner, parent)?;
            // A validated note applies to the retired registration on this
            // claim. Keep its receipt if naming moved while the note was
            // in flight, then refuse the evaluation on the stale root.
            let acknowledged_during_rebind = evidence_record.is_some()
                && engram_work_source_root_for_work(
                    &inner.engram_work_source_roots,
                    store,
                    &root.work_id,
                )
                .is_none_or(|entry| entry.claim_id == root.claim_id);
            if target.store != *store
                || target.connection != *connection
                || (!root.still_named(&inner.engram_work_source_roots, store)
                    && !acknowledged_during_rebind)
            {
                return Err(ApiError::conflict(
                    "check disposition source authority changed",
                ));
            }
            let store_receipt = |row: &mut TestRunRegistration| {
                if !acceptance_committed_input_registration_unchanged(expected, row) {
                    return Err(ApiError::conflict("check disposition registration changed"));
                }
                if let Some(existing) = acceptance_scoped_retired_check_receipt(row, store, root) {
                    return Ok((existing.clone(), false));
                }
                let evidence_record = evidence_record.ok_or_else(|| {
                    ApiError::conflict("the acknowledged retired check disposition changed")
                })?;
                let new_receipt = AcceptanceRetiredCheckReceipt {
                    body_digest: expected
                        .verification_record
                        .as_ref()
                        .and_then(TestRunVerificationAck::body_digest)
                        .expect("validated retired check receipt")
                        .to_owned(),
                    evidence_record: evidence_record.to_owned(),
                };
                row.retired_check_disposition = Some(new_receipt.clone());
                Ok((new_receipt, true))
            };
            let (receipt, installed) = if inner.test_runs.registered.contains(run_id)
                && inner.test_run_registrations.contains_key(run_id)
            {
                inner
                    .test_run_registrations
                    .update_unindexed_fields(run_id, store_receipt)
            } else if inner
                .test_run_compacted
                .get(run_id)
                .is_some_and(|row| row.compacted && test_run_compact_is_installed(&*inner, row))
            {
                inner
                    .test_run_compacted
                    .update_unindexed_fields(run_id, |row| {
                        row.registration.as_mut().map(store_receipt)
                    })
                    .flatten()
            } else {
                None
            }
            .ok_or_else(|| {
                ApiError::conflict("check disposition registration is no longer retained")
            })??;
            if installed {
                inner
                    .pending_retired_check_receipts
                    .insert(run_id.clone(), receipt.clone());
            }
            (inner.pending_retired_check_receipts.get(run_id) == Some(&receipt)).then_some(receipt)
        };
        if let Some(receipt) = &receipt {
            self.confirm_engram_fence_target_until(
                PersistFenceTarget::RetiredCheckDisposition {
                    run_id: expected.run_id.clone(),
                    receipt: receipt.clone(),
                },
                self.engram_budget_clock(),
                deadline,
            )
            .map_err(|_| {
                ApiError::internal("could not confirm retired check disposition persistence")
            })?;
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if let Some(receipt) = &receipt
            && inner.pending_retired_check_receipts.get(run_id) == Some(receipt)
        {
            inner.pending_retired_check_receipts.remove(run_id);
        }
        // A confirmed receipt remains effective for its original registration
        // even when the caller can no longer write to that tracker.
        require_check_disposition_reader_locked(&inner, parent, connection, store)?;
        if !root.still_named(&inner.engram_work_source_roots, store) {
            return Err(ApiError::conflict(
                "check disposition source authority changed",
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_acceptance_check_dispositions(
        &self,
        parent: &str,
        connection: &EngramConnectionConfig,
        store: &EngramAuthorityStoreKey,
        task: &AcceptanceEvaluationTask,
        root: Option<&AcceptanceEvaluationSourceRoot>,
        dispositions: &[AcceptanceRetiredCheckDisposition],
    ) -> Result<(), ApiError> {
        for disposition in dispositions {
            let root = root.ok_or_else(|| {
                ApiError::conflict("check disposition lost its current source root")
            })?;
            if root != &disposition.current
                || task.canonical_identity.as_ref() != Some(&disposition.identity)
            {
                return Err(ApiError::conflict(
                    "check disposition names a different current source root",
                ));
            }
            let rejected = disposition.rejected_source.is_some();
            let row = self.acceptance_check_disposition_registration(
                parent,
                connection,
                store,
                task,
                root,
                &disposition.registration.run_id,
                rejected,
            )?;
            let unchanged = if rejected {
                row.matches_registration_snapshot(&disposition.registration)
            } else {
                acceptance_committed_input_registration_unchanged(&disposition.registration, &row)
            };
            if !unchanged {
                return Err(ApiError::conflict("check disposition registration changed"));
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_acceptance_check_dispositions(
        &self,
        parent: &str,
        connection: &EngramConnectionConfig,
        store: &EngramAuthorityStoreKey,
        task: &AcceptanceEvaluationTask,
        root: Option<&AcceptanceEvaluationSourceRoot>,
        requests: &[AcceptanceRetiredCheckRequest],
        rejected: Option<&AcceptanceRetiredCheckRequest>,
        read: &impl Fn(
            &EngramConnectionConfig,
            &[String],
            Duration,
        ) -> Result<Value, EngramTransportError>,
        deadline: std::time::Instant,
        now: &impl Fn() -> std::time::Instant,
    ) -> Result<RecordedAcceptanceCheckDispositions, ApiError> {
        let root = root.ok_or_else(|| {
            ApiError::bad_request("a check disposition requires the current named root")
        })?;
        let identity = task.canonical_identity.clone().ok_or_else(|| {
            ApiError::conflict("check disposition requires canonical work/run identity")
        })?;
        // Validate the entire selection before issuing any write.
        let mut selected = Vec::new();
        let mut retained_receipts = Vec::new();
        let mut same_path_blockers = None;
        let author = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(parent)
                .ok_or_else(ApiError::local_session_missing)?;
            engram_runtime_actor_identity(
                &inner.preferences.engram.developer_name,
                &inner.sessions[index],
            )
        };
        for request in requests {
            if now() >= deadline {
                return Err(
                    AcceptanceCommittedInputRefusal::ScanIncomplete.into_api_error(&task.work_ref)
                );
            }
            let registration = self.acceptance_check_disposition_registration(
                parent,
                connection,
                store,
                task,
                root,
                &request.run_id,
                false,
            )?;
            if acceptance_scoped_retired_check_receipt(&registration, store, root).is_some() {
                retained_receipts.push(registration);
                continue;
            }
            if registration
                .named_root
                .as_ref()
                .is_some_and(|named| named.workspace_id == root.root)
            {
                // Only the holder's live binding can decide whether a
                // same-path predecessor remains eligible for this claim.
                if same_path_blockers.is_none() {
                    let held = {
                        let inner = self.inner.lock().expect("state mutex poisoned");
                        let naming = engram_work_source_root_for_claim(
                            &inner.engram_work_source_roots,
                            store,
                            &root.work_id,
                            &root.claim_id,
                        );
                        task.current_claim
                            .as_ref()
                            .zip(naming)
                            .is_some_and(|(claim, entry)| claim.can_dispose_same_path(entry))
                    };
                    if !held {
                        return Err(ApiError::conflict(format!(
                            "only the current holder with a confirmed claim binding can exclude same-path retired check {}",
                            request.run_id
                        )));
                    }
                    same_path_blockers = Some(
                        match self.acceptance_committed_input_decision(
                            parent,
                            connection,
                            store,
                            task,
                            Some(root),
                            deadline,
                            now,
                        ) {
                            Err(AcceptanceCommittedInputRefusal::RootRebound {
                                checks, ..
                            }) => checks
                                .into_iter()
                                .map(|(run_id, _)| run_id)
                                .collect::<HashSet<_>>(),
                            Err(refusal @ AcceptanceCommittedInputRefusal::ScanIncomplete) => {
                                return Err(refusal.into_api_error(&task.work_ref));
                            }
                            _ => HashSet::new(),
                        },
                    );
                }
                if !same_path_blockers
                    .as_ref()
                    .is_some_and(|checks| checks.contains(&request.run_id))
                {
                    return Err(ApiError::bad_request(format!(
                        "check {} is eligible for predecessor admission or has another refusal; it cannot be excluded as a retired-root rebound",
                        request.run_id
                    )));
                }
            }
            let disclosure =
                acceptance_retired_check_disclosure(&registration, request, root, parent, &author)?;
            selected.push(AcceptanceRetiredCheckDisposition {
                registration,
                current: root.clone(),
                identity: identity.clone(),
                disclosure,
                rejected_source: None,
            });
        }
        for registration in &retained_receipts {
            self.confirm_acceptance_retired_check_receipt(
                parent,
                connection,
                store,
                registration,
                root,
                None,
                deadline,
            )?;
        }
        let rejected_selection = if let Some(request) = rejected {
            let registration = self.acceptance_check_disposition_registration(
                parent,
                connection,
                store,
                task,
                root,
                &request.run_id,
                true,
            )?;
            let decision = self
                .acceptance_committed_input_decision_with_dispositions(
                    parent,
                    connection,
                    store,
                    task,
                    Some(root),
                    deadline,
                    now,
                    &selected,
                    None,
                    Some(&registration),
                    true, // Recheck the source at the rejected note write.
                )
                .map_err(|refusal| refusal.into_api_error(&task.work_ref))?;
            let (AcceptanceCommittedInputDecision::Admitted(admission), Some(selection)) = decision
            else {
                return Err(AcceptanceCommittedInputRefusal::RejectedCheckNotSelected {
                    run_id: request.run_id.clone(),
                }
                .into_api_error(&task.work_ref));
            };
            if task.current_claim.is_none() {
                self.require_acceptance_named_root_sighting(
                    parent,
                    connection,
                    store,
                    task,
                    Some(root),
                    deadline,
                    now,
                )?;
            }
            let disclosure = json!({
                "kind": "current_root_verification_rejected",
                "runId": request.run_id, "reason": request.reason,
                "author": {"sessionId": parent, "actorId": author.0, "actorContext": author.1},
                "workId": root.work_id, "claimId": root.claim_id,
                "currentRoot": root, "requestIdentity": registration.request_identity,
                "sourceId": admission.recorded_source,
                "artifactOutcome": "passed",
                "verificationRecord": registration.verification_record,
                "scope": "this evaluation request only; context only, no verification credit, no satisfaction of bound criteria, and no standing disposition"
            });
            if disclosure.to_string().len() > ACCEPTANCE_RETIRED_CHECK_DISCLOSURE_MAX {
                return Err(ApiError::bad_request(
                    "check disposition disclosure exceeds the complete brief bound",
                ));
            }
            selected.push(AcceptanceRetiredCheckDisposition {
                registration,
                current: root.clone(),
                identity: identity.clone(),
                disclosure,
                rejected_source: admission.recorded_source,
            });
            Some(selection)
        } else {
            None
        };
        let _note_writes = self.reserve_retired_check_note_writes(store, root, &selected)?;
        for (index, disposition) in selected.iter_mut().enumerate() {
            let mut writer = connection.clone();
            writer.actor_id = author.0.clone();
            writer.actor_context = author.1.clone();
            writer.session_id = parent.to_owned();
            let mut args = vec![
                "work".to_owned(),
                "--actor-id".to_owned(),
                writer.actor_id.clone(),
                "--session-id".to_owned(),
                writer.session_id.clone(),
            ];
            if let Some(context) = &writer.actor_context {
                args.extend(["--actor-context".to_owned(), context.clone()]);
            }
            args.extend([
                "note".to_owned(),
                task.work_ref.clone(),
                disposition.disclosure.to_string(),
                "--json".to_owned(),
            ]);
            if let Some(source) = &disposition.rejected_source {
                self.require_acceptance_committed_source_unchanged(
                    parent,
                    FsPath::new(&root.root),
                    &source.commit,
                    Some(source),
                    deadline,
                    now,
                )
                .map_err(|refusal| refusal.into_api_error(&task.work_ref))?;
            }
            let timeout = acceptance_criterion_evidence_read_timeout(deadline, now)?;
            {
                let inner = self.inner.lock().expect("state mutex poisoned");
                require_check_disposition_reader_locked(&inner, parent, connection, store)?;
                if (index == 0 || disposition.rejected_source.is_some())
                    && let Some(selection) = &rejected_selection
                {
                    // Recheck the rejected selection after off-lock sighting
                    // and after any preceding retired notes.
                    selection.require_unchanged_locked(&inner, &task.work_ref)?;
                }
            }
            let result = read(&writer, &args, timeout).map_err(|e| {
                acceptance_evaluation_transport_error(
                    "record check disposition (no evaluator started)",
                    e,
                )
            })?;
            let record = result.get("evidence").and_then(Value::as_str).filter(|s| {
                matches!(s.len(), 32 | 64)
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            });
            if result["operation"] != "note"
                || result["work"]["short_ref"] != task.work_ref
                || result["work"]
                    .get("work_id")
                    .is_some_and(|id| id.as_str() != task.canonical_work_id())
                || result["work"]["revision"].as_i64() != Some(disposition.identity.revision)
                || result["work"]["lifecycle"] != "open"
                || record.is_none()
            {
                return Err(ApiError::bad_gateway(
                    "check disposition write supplied no acknowledgement for this item and revision; no evaluator started",
                ));
            }
            let record = record.unwrap();
            if disposition.rejected_source.is_none() {
                self.confirm_acceptance_retired_check_receipt(
                    parent,
                    connection,
                    store,
                    &disposition.registration,
                    &disposition.current,
                    Some(record),
                    deadline,
                )?;
            }
            disposition.disclosure["evidenceRecord"] = json!(record);
        }
        self.validate_acceptance_check_dispositions(
            parent,
            connection,
            store,
            task,
            Some(root),
            &selected,
        )?;
        Ok(RecordedAcceptanceCheckDispositions {
            disclosures: selected,
            root: root.clone(),
            identity,
            retained_receipts,
        })
    }
}
