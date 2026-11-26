// Request-scoped disposition of retired checks and terminally refused records.
// Owns validation and the attributed tracker note, not verification credit or
// root authority. The API records it before taking a fresh canonical evidence
// bracket; only that attempt's complete persisted brief carries the exclusion.

use super::*;

pub(crate) const ACCEPTANCE_RETIRED_CHECKS_MAX: usize = 4;
const ACCEPTANCE_RETIRED_CHECK_DISCLOSURE_MAX: usize = 16 * 1024;

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

pub(crate) fn validate_acceptance_retired_check_requests(
    requests: &[AcceptanceRetiredCheckRequest],
) -> Result<(), ApiError> {
    if requests.len() > ACCEPTANCE_RETIRED_CHECKS_MAX {
        return Err(ApiError::bad_request(
            "at most four retired check dispositions may be requested",
        ));
    }
    validate_acceptance_check_requests(requests, "retired")
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
        let row = inner
            .test_run_registrations
            .get(run_id)
            .filter(|_| inner.test_runs.registered.contains(run_id))
            .or_else(|| {
                inner
                    .test_run_compacted
                    .get(run_id)
                    .filter(|row| row.compacted && test_run_compact_is_installed(&*inner, row))
                    .and_then(|row| row.registration.as_ref())
            })
            .ok_or_else(|| {
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
    ) -> Result<Vec<AcceptanceRetiredCheckDisposition>, ApiError> {
        use std::io::Read as _;
        let root = root.ok_or_else(|| {
            ApiError::bad_request("a check disposition requires the current named root")
        })?;
        // Validate the entire selection before issuing any write.
        let mut selected = Vec::new();
        let mut total = 0;
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
            let registration = self.acceptance_check_disposition_registration(
                parent,
                connection,
                store,
                task,
                root,
                &request.run_id,
                false,
            )?;
            if registration
                .named_root
                .as_ref()
                .is_some_and(|named| named.workspace_id == root.root)
                && !matches!(self.acceptance_committed_input_decision(parent, connection, store, task, Some(root), deadline, now),
                    Err(AcceptanceCommittedInputRefusal::RootRebound { run_id, .. }) if run_id == request.run_id)
            {
                return Err(ApiError::bad_request(format!(
                    "check {} is eligible for predecessor admission or has another refusal; it cannot be excluded as a retired-root rebound",
                    request.run_id
                )));
            }
            let mut text = String::new();
            fs::File::open(FsPath::new(&registration.run_directory).join(ENGRAM_VERIFICATION_RECORD_BODY_FILE))
                .and_then(|file| file.take((ACCEPTANCE_RETIRED_CHECK_DISCLOSURE_MAX + 1) as u64).read_to_string(&mut text))
                .map_err(|_| ApiError::conflict("retired check disposition cannot read its original acknowledged verification body"))?;
            if text.len() > ACCEPTANCE_RETIRED_CHECK_DISCLOSURE_MAX {
                return Err(ApiError::conflict(
                    "retired check disposition original body exceeds the complete disclosure bound",
                ));
            }
            let (_, body) = engram_verification_record_parse(&text).ok_or_else(|| {
                ApiError::conflict("retired check disposition original body is malformed")
            })?;
            let ack = registration
                .verification_record
                .as_ref()
                .expect("validated receipt");
            let receipt = match ack {
                TestRunVerificationAck::Received { receipt, .. }
                | TestRunVerificationAck::Settled { receipt, .. }
                | TestRunVerificationAck::Unassociated { receipt, .. } => receipt,
                _ => unreachable!("validated receipt"),
            };
            // The acknowledged immutable digest remains authoritative even
            // when old mutable artifacts have disappeared. Rebuilding from
            // those files would make an honest unknown result undisposable.
            if body.registration.host_run_id != registration.run_id
                || body.registration.request_identity != registration.request_identity
                || registration.work_binding.as_ref() != Some(&body.registration.binding)
                || !registration.named_root.as_ref().is_some_and(|named| {
                    named.workspace_id == body.registration.root.workspace_id
                        && named.generation == body.registration.root.generation as u64
                })
                || !engram_verification_record_digest_answers(ack.body_digest().unwrap(), &body)
            {
                return Err(ApiError::conflict(
                    "retired check disposition cannot authenticate the original verification body",
                ));
            }
            let artifact = match test_run_read_json(
                &FsPath::new(&registration.run_directory).join("results.json"),
            ) {
                TestRunJson::Parsed(result, _, _) => result
                    .get("state")
                    .cloned()
                    .unwrap_or_else(|| json!("unknown")),
                _ => json!("unknown"),
            };
            let disclosure = json!({
                "kind": "retired_root_check_excluded_from_evaluation_admission",
                "runId": request.run_id, "reason": request.reason,
                "author": {"sessionId": parent, "actorId": author.0, "actorContext": author.1},
                "workId": root.work_id, "claimId": root.claim_id,
                "checkedRoot": registration.named_root, "currentRoot": root,
                "requestIdentity": registration.request_identity,
                "verificationBodyDigest": ack.body_digest(),
                "originalVerification": body,
                "originalAcknowledgement": ack,
                // A disposition may explicitly set aside an invalid old
                // check. Preserve a receipt disagreement, never treat that
                // receipt as proof of a passed check or silently repair it.
                "receiptValidationError": engram_verification_record_receipt_check(&body, receipt).err(),
                "artifactOutcome": artifact,
                "scope": "this evaluation request only; no verification credit, pass, waiver of criteria, or standing exclusion"
            });
            total += disclosure.to_string().len();
            if total > ACCEPTANCE_RETIRED_CHECK_DISCLOSURE_MAX {
                return Err(ApiError::bad_request(
                    "retired check disposition disclosure exceeds the complete brief bound",
                ));
            }
            let identity = task.canonical_identity.clone().ok_or_else(|| {
                ApiError::conflict("retired check disposition requires canonical work/run identity")
            })?;
            selected.push(AcceptanceRetiredCheckDisposition {
                registration,
                current: root.clone(),
                identity,
                disclosure,
                rejected_source: None,
            });
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
            if total + disclosure.to_string().len() > ACCEPTANCE_RETIRED_CHECK_DISCLOSURE_MAX {
                return Err(ApiError::bad_request(
                    "check disposition disclosure exceeds the complete brief bound",
                ));
            }
            let identity = task.canonical_identity.clone().ok_or_else(|| {
                ApiError::conflict("check disposition requires canonical work/run identity")
            })?;
            selected.push(AcceptanceRetiredCheckDisposition {
                registration,
                current: root.clone(),
                identity,
                disclosure,
                rejected_source: admission.recorded_source,
            });
            Some(selection)
        } else {
            None
        };
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
            if (index == 0 || disposition.rejected_source.is_some())
                && let Some(selection) = &rejected_selection
            {
                // Sighting and the note writes are off-lock. Check before
                // the first write and again if retired notes preceded the
                // rejected disclosure.
                let inner = self.inner.lock().expect("state mutex poisoned");
                selection.require_unchanged_locked(&inner, &task.work_ref)?;
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
            disposition.disclosure["evidenceRecord"] = json!(record.unwrap());
        }
        self.validate_acceptance_check_dispositions(
            parent,
            connection,
            store,
            task,
            Some(root),
            &selected,
        )?;
        Ok(selected)
    }
}
