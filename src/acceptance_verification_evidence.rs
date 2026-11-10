// Complete private verification metadata for the evaluator's source markers.
// This read enumerates recorded facts; it grants no eligibility or credit.

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceVerificationEvidencePage {
    basis: AcceptanceBindingEvidenceBasis,
    criterion: usize,
    requirement: Option<EngramAcceptanceRequirementForEvaluation>,
    total: usize,
    earlier: usize,
    shown: usize,
    omitted: usize,
    rows: Vec<AcceptanceBindingEvidenceVerification>,
    continuation: Option<String>,
}

impl AppState {
    fn read_acceptance_verification_evidence(
        &self,
        store: &EngramAuthorityStoreKey,
        task: &mut AcceptanceEvaluationTask,
        discovery: &AcceptanceEvidenceDiscovery,
        deadline: std::time::Instant,
        now: &impl Fn() -> std::time::Instant,
    ) -> Result<(), ApiError> {
        if task.bindings.is_empty() {
            return Ok(());
        }
        let (Some(target), Some(identity)) = (&discovery.target, &discovery.identity) else {
            return Ok(());
        };
        let Some(run_id) = &identity.active_run_id else {
            return Ok(());
        };
        let expected = AcceptanceBindingEvidenceBasis {
            project_id: store.project_id.clone(),
            work_id: identity.work_id.clone(),
            work_revision: task.acceptance_basis,
            run_id: run_id.clone(),
            run_cut: task.evidence_basis,
        };
        for binding in task.bindings.clone() {
            let mut after = None;
            let mut cursors = BTreeSet::new();
            let mut rows = Vec::new();
            let mut total = None;
            let mut last_position = 0;
            let mut completed = false;
            for _ in 0..MAX_ACCEPTANCE_BINDING_EVIDENCE_PAGES {
                let request = EngramControlRequest::AcceptanceVerificationRead {
                    work_id: expected.work_id.clone(),
                    expected_work_revision: expected.work_revision,
                    run_id: expected.run_id.clone(),
                    run_cut: expected.run_cut,
                    criterion: binding.criterion,
                    after: after.take(),
                };
                let value = match target.adapter.request(
                    &target.connection,
                    &request,
                    acceptance_criterion_evidence_read_timeout(deadline, now)?,
                ) {
                    Ok(value) => value,
                    Err(error)
                        if rows.is_empty()
                            && (acceptance_binding_read_unsupported(&error)
                                || error.kind == EngramTransportErrorKind::Remote
                                    && error.code.as_deref() == Some("invalid_request")
                                    && error.message.contains(
                                        "unknown variant `acceptance_verification_read`",
                                    )) =>
                    {
                        acceptance_canonical_index_unavailable(
                            task,
                            "complete private verification metadata is unavailable",
                        );
                        return Ok(());
                    }
                    Err(error) => return Err(acceptance_verification_read_error(error)),
                };
                if serde_json::to_vec(&value).map_or(true, |bytes| {
                    bytes.len() > ACCEPTANCE_PRIVATE_PAGE_MAX_BYTES
                }) {
                    return Err(ApiError::bad_gateway(
                        "private verification metadata exceeds its wire page bound",
                    ));
                }
                let page: AcceptanceVerificationEvidencePage = serde_json::from_value(value)
                    .map_err(|_| {
                        ApiError::bad_gateway("private verification metadata has an invalid page")
                    })?;
                let requirement_matches = page.requirement.as_ref().is_some_and(|requirement| {
                    requirement.check_kind == binding.check_kind
                        && requirement.check_fingerprint == binding.fingerprint
                });
                if page.basis != expected
                    || page.criterion != binding.criterion
                    || !requirement_matches
                    || total.is_some_and(|total| total != page.total)
                    || page.earlier != rows.len()
                    || page.shown != page.rows.len()
                    || page.shown > 8
                    || page
                        .earlier
                        .checked_add(page.shown)
                        .and_then(|read| page.total.checked_sub(read))
                        != Some(page.omitted)
                    || (page.omitted == 0) != page.continuation.is_none()
                    || page.shown == 0 && page.omitted > 0
                {
                    return Err(ApiError::conflict(
                        "private verification metadata changed its criterion, basis or page boundary; request a fresh evaluation",
                    ));
                }
                total = Some(page.total);
                for row in &page.rows {
                    if row.position <= last_position || row.position > expected.run_cut {
                        return Err(ApiError::bad_gateway(
                            "private verification metadata is not ordered within its pinned cut",
                        ));
                    }
                    last_position = row.position;
                }
                rows.extend(page.rows);
                match page.continuation {
                    None => {
                        completed = true;
                        break;
                    }
                    Some(cursor)
                        if !cursor.is_empty()
                            && cursor.len() <= 4096
                            && cursors.insert(cursor.clone()) =>
                    {
                        after = Some(cursor)
                    }
                    Some(_) => {
                        return Err(ApiError::bad_gateway(
                            "private verification metadata has an invalid or repeated continuation",
                        ));
                    }
                }
            }
            if !completed {
                return Err(ApiError::conflict(
                    "private verification metadata exceeded its complete-read page bound; no evaluation was started",
                ));
            }
            for row in rows {
                apply_acceptance_verification_metadata(task, row, &binding)?;
            }
        }
        Ok(())
    }
}

fn acceptance_verification_read_error(error: EngramTransportError) -> ApiError {
    if error.kind == EngramTransportErrorKind::Remote
        && matches!(
            error.code.as_deref(),
            Some(
                "acceptance_verification_read_wrong_revision"
                    | "acceptance_verification_read_wrong_run"
                    | "acceptance_verification_read_stale_cut"
                    | "acceptance_verification_read_cursor_basis_mismatch"
                    | "control_session_not_bound"
                    | "control_session_token_mismatch"
                    | "control_connection_superseded"
                    | "invalid_control_session"
            )
        )
    {
        return ApiError::conflict(
            "private verification metadata basis changed; request a fresh evaluation",
        );
    }
    acceptance_evaluation_transport_error("acceptance_verification_read", error)
}

fn apply_acceptance_verification_metadata(
    task: &mut AcceptanceEvaluationTask,
    row: AcceptanceBindingEvidenceVerification,
    binding: &AcceptanceCriterionBinding,
) -> Result<(), ApiError> {
    let invalid = || {
        ApiError::bad_gateway(
            "private verification metadata contains an invalid recorded verification",
        )
    };
    let label = acceptance_binding_source_revision(&row.source_basis).ok_or_else(invalid)?;
    let record = row.record.clone();
    let producer_valid = if let Some(bound) = &row.bound {
        row.producer.position > 0
            && row.producer.position < bound.original_position
            && is_engram_record_id(&bound.verification)
            && is_engram_record_id(&bound.sighting)
            && acceptance_verification_basis_agrees(&bound.original_basis, &row.source_basis)
    } else {
        row.producer.position > 0 && row.producer.position < row.position
    };
    if !is_engram_record_id(&row.record)
        || !is_lowercase_sha256(&row.check_fingerprint)
        || !is_engram_record_id(&row.producer.record)
        || !producer_valid
        || row.check_kind != binding.check_kind
        || !is_acceptance_tracker_word(&row.result)
        || !is_acceptance_tracker_word(&row.producer.outcome)
        || acceptance_source_workspace(&row.source_basis.workspace_id).is_none()
    {
        return Err(invalid());
    }
    let metadata = AcceptanceEvidenceVerification {
        applicability_tree: row.applicability.map(|restriction| restriction.tree),
        source_basis: Some(row.source_basis.identity),
        check_kind: row.check_kind,
        result: row.result,
        source_revision: Some(label.clone()),
        source_workspace: Some(row.source_basis.workspace_id),
    };
    let mut found = false;
    for entry in task
        .evidence
        .iter_mut()
        .chain(task.indexed_evidence.iter_mut())
        .filter(|entry| entry.locator == row.record)
    {
        if entry
            .verification
            .as_ref()
            .and_then(|verification| verification.source_basis.as_ref())
            .is_some_and(|basis| Some(basis) != metadata.source_basis.as_ref())
        {
            return Err(invalid());
        }
        entry.verification = Some(metadata.clone());
        found = true;
    }
    if !found {
        task.indexed_evidence.push(AcceptanceEvaluationEvidence {
            locator: row.record, kind: "verification".into(), by: None, created_at: None,
            summary: Some(format!("Host projection of recorded verification metadata at run cut {}: command fingerprint {}; source {}; producer {} {}. This is not a stored record body or a freshness verdict.",
                task.evidence_basis, row.check_fingerprint, label, row.producer.record, row.producer.outcome)),
            non_holder: false, body_bytes: None, cut_by_tracker: false, verification: Some(metadata),
        });
    }
    if !task.criterion_evidence.iter().any(|association| {
        association.criterion == binding.criterion && association.locators.contains(&record)
    }) {
        task.criterion_evidence.push(AcceptanceCriterionEvidence {
            criterion: binding.criterion,
            locators: vec![record],
            association: "recorded verification metadata; inspect command, source and result"
                .to_owned(),
        });
    }
    Ok(())
}
