// Request-scoped exclusion of acknowledged checks on a retired source root.
// Owns validation and the attributed tracker note, not verification credit or
// root authority. The API records it before taking a fresh canonical evidence
// bracket; only that attempt's complete persisted brief carries the exclusion.

const ACCEPTANCE_RETIRED_CHECKS_MAX: usize = 4;
const ACCEPTANCE_RETIRED_CHECK_DISCLOSURE_MAX: usize = 16 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AcceptanceRetiredCheckRequest {
    run_id: String,
    reason: String,
}

struct AcceptanceRetiredCheckDisposition {
    registration: TestRunRegistration,
    current: AcceptanceEvaluationSourceRoot,
    identity: AcceptanceEvidenceIdentity,
    disclosure: Value,
}

fn validate_acceptance_retired_check_requests(
    requests: &[AcceptanceRetiredCheckRequest],
) -> Result<(), ApiError> {
    let mut seen = BTreeSet::new();
    if requests.len() > ACCEPTANCE_RETIRED_CHECKS_MAX {
        return Err(ApiError::bad_request(
            "at most four retired check dispositions may be requested",
        ));
    }
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
            return Err(ApiError::bad_request(
                "retired check dispositions require distinct exact run IDs and a nonempty reason of at most 1024 bytes",
            ));
        }
    }
    Ok(())
}

impl AppState {
    fn acceptance_retired_check_registration(
        &self,
        parent: &str,
        connection: &EngramConnectionConfig,
        store: &EngramAuthorityStoreKey,
        task: &AcceptanceEvaluationTask,
        root: &AcceptanceEvaluationSourceRoot,
        run_id: &str,
    ) -> Result<TestRunRegistration, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let target = acceptance_evaluation_host_target_locked(&inner, parent)?;
        if target.store != *store
            || target.connection != *connection
            || !root.still_named(&inner.engram_work_source_roots, store)
            || task.canonical_work_id() != Some(root.work_id.as_str())
        {
            return Err(ApiError::conflict(
                "retired check disposition source authority changed",
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
                ApiError::bad_request(format!(
                    "retired check {run_id} is not a retained registered run"
                ))
            })?;
        if row.authority_store.as_ref() != Some(store)
            || !row
                .work_binding
                .as_ref()
                .is_some_and(|b| b.work_id == root.work_id && b.claim_id == root.claim_id)
            || !row.named_root.as_ref().is_some_and(|named| {
                named.generation < root.generation
                    && (named.workspace_id != root.root || named.generation != root.generation)
            })
            || !row.verification_record.as_ref().is_some_and(|ack| {
                matches!(
                    ack,
                    TestRunVerificationAck::Received { .. }
                        | TestRunVerificationAck::Settled { .. }
                ) && ack.body_digest().is_some()
            })
        {
            return Err(ApiError::bad_request(format!(
                "check {run_id} is not an acknowledged check on this claim's retired root; current-root checks cannot be excluded"
            )));
        }
        Ok(row.clone())
    }

    fn validate_acceptance_retired_dispositions(
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
                ApiError::conflict("retired check disposition lost its current source root")
            })?;
            if root != &disposition.current
                || task.canonical_identity.as_ref() != Some(&disposition.identity)
            {
                return Err(ApiError::conflict(
                    "retired check disposition names a different current source root",
                ));
            }
            let row = self.acceptance_retired_check_registration(
                parent,
                connection,
                store,
                task,
                root,
                &disposition.registration.run_id,
            )?;
            if !acceptance_committed_input_registration_unchanged(&disposition.registration, &row) {
                return Err(ApiError::conflict(
                    "retired check disposition registration changed",
                ));
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn record_acceptance_retired_dispositions(
        &self,
        parent: &str,
        connection: &EngramConnectionConfig,
        store: &EngramAuthorityStoreKey,
        task: &AcceptanceEvaluationTask,
        root: Option<&AcceptanceEvaluationSourceRoot>,
        requests: &[AcceptanceRetiredCheckRequest],
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
            ApiError::bad_request("a retired check disposition requires the current named root")
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
            let registration = self.acceptance_retired_check_registration(
                parent,
                connection,
                store,
                task,
                root,
                &request.run_id,
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
                | TestRunVerificationAck::Settled { receipt, .. } => receipt,
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
            });
        }
        for disposition in &mut selected {
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
            let result = read(
                &writer,
                &args,
                acceptance_criterion_evidence_read_timeout(deadline, now)?,
            )
            .map_err(|e| {
                acceptance_evaluation_transport_error(
                    "record retired check disposition (no evaluator started)",
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
                || result["work"]["revision"].as_i64() != Some(task.acceptance_basis)
                || result["work"]["lifecycle"] != "open"
                || record.is_none()
            {
                return Err(ApiError::bad_gateway(
                    "retired check disposition write supplied no acknowledgement for this item and revision; no evaluator started",
                ));
            }
            disposition.disclosure["evidenceRecord"] = json!(record.unwrap());
        }
        self.validate_acceptance_retired_dispositions(
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
