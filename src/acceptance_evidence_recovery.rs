// Evaluator-only evidence reads. Reuses the acceptance catalog and exact-record
// validators and the submission admission owner. The runtime cache retains the
// original attempt's cursors and actual-I/O quota; it is not tracker authority.
// Lost runtime state closes recovery rather than minting another allowance.

const TERMAL_READ_ACCEPTANCE_EVIDENCE_TOOL_NAME: &str = "termal_read_acceptance_evidence";
const TERMAL_READ_ACCEPTANCE_EVIDENCE_QUALIFIED_TOOL_NAME: &str =
    "mcp__termal-delegation__termal_read_acceptance_evidence";
const TERMAL_READ_ACCEPTANCE_EVIDENCE_TOOL_DESCRIPTION: &str = "When your current brief enables evidence recovery, read omitted evidence before judging. An attempt without recovery keeps its missing-evidence verdict route; this tool refuses such a call without ending that attempt. Supply your exact attemptKey and a new readKey for each logical read. action catalog continues the captured notes catalog: omit cursor for its original boundary, then use only a cursor this tool returned. action records reads whole records named by your brief or a recovered catalog page; supply locators. The host derives the store, work, run and cut. Repeating the same readKey and arguments returns its original result without another read or budget charge; reusing it for different arguments refuses. All reads share one attempt allowance. An unfinished result recorded no verdict and ends this attempt: finish and report it so the parent can request a fresh evaluation. When recovery is enabled, do not submit insufficient-evidence merely because discovery stopped before older proof was read.";

#[derive(Clone, Debug, PartialEq, Eq)]
struct AcceptanceEvidenceRecoveryManifest {
    identity: AcceptanceEvidenceIdentity,
    basis: AcceptanceAgentEvidenceBasis,
    window: AcceptanceNotesWindow,
    seen: BTreeSet<String>,
    catalog: Vec<Value>,
    /// Exact selected records are readable before they appear in catalog paging.
    indexed: BTreeSet<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptanceUnfinishedEvidenceRead {
    attempt_key: String,
    read_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    intent_digest: Option<String>,
    reason: String,
    captured_cut: Option<Value>,
    /// This is local attempt history, never an Engram evaluation receipt.
    #[serde(skip_deserializing)]
    recorded: bool,
    /// Runtime acknowledgement only. A loaded row already came from storage.
    #[serde(skip, default = "acceptance_evidence_loaded_durable")]
    persistence_confirmed: bool,
}

fn acceptance_evidence_loaded_durable() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReadAcceptanceEvidenceRequest {
    attempt_key: String,
    read_key: String,
    action: String,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    locators: Vec<String>,
}

impl ReadAcceptanceEvidenceRequest {
    fn validate(&self) -> Result<(), ApiError> {
        if self.attempt_key.is_empty()
            || self.attempt_key.len() > 256
            || self.read_key.is_empty()
            || self.read_key.len() > 128
            || self.read_key.chars().any(char::is_control)
        {
            return Err(ApiError::bad_request("invalid evidence read key"));
        }
        match self.action.as_str() {
            "catalog"
                if self.locators.is_empty()
                    && self
                        .cursor
                        .as_ref()
                        .is_none_or(|s| !s.is_empty() && s.len() <= 128) =>
            {
                Ok(())
            }
            "records"
                if self.cursor.is_none()
                    && !self.locators.is_empty()
                    && self.locators.len() <= MAX_ACCEPTANCE_SELECTED_RECORDS
                    && self
                        .locators
                        .iter()
                        .all(|s| is_citable_acceptance_locator(s))
                    && self.locators.iter().collect::<BTreeSet<_>>().len()
                        == self.locators.len() =>
            {
                Ok(())
            }
            _ => Err(ApiError::bad_request(
                "use catalog with an optional host-issued cursor, or records with distinct known locators",
            )),
        }
    }

    fn intent(&self) -> String {
        // Field order is fixed by this host type, not by caller JSON order.
        serde_json::to_string(self).expect("evidence read request is serializable")
    }
}

#[derive(Debug)]
struct AcceptanceEvidenceRecovery {
    manifest: AcceptanceEvidenceRecoveryManifest,
    target: DelegationAcceptanceEvaluation,
    remaining: Duration,
    initial_cursor: Option<String>,
    cursors: HashMap<String, AcceptanceNotesWindow>,
    known: BTreeMap<String, Value>,
    catalog_seen: BTreeSet<String>,
    seen: BTreeSet<String>,
    pages: HashMap<String, Value>,
    replies: HashMap<String, (String, Value)>,
}

impl AcceptanceEvidenceRecovery {
    fn new(
        manifest: AcceptanceEvidenceRecoveryManifest,
        target: DelegationAcceptanceEvaluation,
    ) -> Self {
        let initial_cursor = manifest
            .window
            .after
            .as_ref()
            .map(|_| Uuid::new_v4().to_string());
        let cursors = initial_cursor
            .iter()
            .map(|key| (key.clone(), manifest.window.clone()))
            .collect();
        let known =
            manifest
                .indexed
                .iter()
                .map(|locator| (locator.clone(), json!({"locator":locator})))
                .chain(manifest.catalog.iter().filter_map(|row| {
                    Some((row.get("locator")?.as_str()?.to_owned(), row.clone()))
                }))
                .collect();
        let catalog_seen = manifest
            .catalog
            .iter()
            .filter_map(|row| row.get("locator")?.as_str().map(str::to_owned))
            .collect();
        Self {
            seen: manifest.seen.clone(),
            manifest,
            target,
            remaining: acceptance_evaluation_evidence_budget(),
            initial_cursor,
            cursors,
            known,
            catalog_seen,
            pages: HashMap::new(),
            replies: HashMap::new(),
        }
    }

    fn same_scope(&self, target: &DelegationAcceptanceEvaluation) -> bool {
        let old = &self.target;
        let history_scope = |target: &DelegationAcceptanceEvaluation| {
            target.attempt_history.as_ref().map(|h| {
                (
                    h.parent_session_id.clone(),
                    h.child_session_id.clone(),
                    h.cwd.clone(),
                    h.ordinal,
                    h.identity.clone(),
                )
            })
        };
        old.attempt_key == target.attempt_key
            && old.work_ref == target.work_ref
            && old.store == target.store
            && old.mode == target.mode
            && old.parent_session == target.parent_session
            && old.execution_identity == target.execution_identity
            && old.acceptance_basis == target.acceptance_basis
            && old.evidence_basis == target.evidence_basis
            && old.criteria_count == target.criteria_count
            && old.bindings == target.bindings
            && old.source_id == target.source_id
            && old.source_fingerprint == target.source_fingerprint
            && old.source_root == target.source_root
            && old.source_claim == target.source_claim
            && old.naming_history == target.naming_history
            && history_scope(old) == history_scope(target)
    }

    fn call(
        &mut self,
        connection: &EngramConnectionConfig,
        args: &[String],
        read: &impl Fn(
            &EngramConnectionConfig,
            &[String],
            Duration,
        ) -> Result<Value, EngramTransportError>,
        now: &impl Fn() -> std::time::Instant,
    ) -> Result<Value, ApiError> {
        if self.remaining < ENGRAM_MIN_CALL_TIMEOUT {
            return Err(ApiError::conflict(
                "the attempt's evidence read allowance is spent",
            ));
        }
        let started = now();
        let result = read(connection, args, ENGRAM_MIN_CALL_TIMEOUT);
        self.remaining = self
            .remaining
            .saturating_sub(now().saturating_duration_since(started));
        result.map_err(|_| {
            ApiError::bad_gateway("the captured evidence read is unavailable or its cursor expired")
        })
    }

    fn inspect(
        &mut self,
        connection: &EngramConnectionConfig,
        read: &impl Fn(
            &EngramConnectionConfig,
            &[String],
            Duration,
        ) -> Result<Value, EngramTransportError>,
        now: &impl Fn() -> std::time::Instant,
    ) -> Result<(), ApiError> {
        let args = vec![
            "work".to_owned(),
            "core".to_owned(),
            "inspect".to_owned(),
            self.manifest.identity.work_id.clone(),
            "--json".to_owned(),
        ];
        let result = self.call(connection, &args, read, now)?;
        if acceptance_core_identity(&result)?.as_ref() != Some(&self.manifest.identity) {
            return Err(ApiError::conflict(
                "the captured work/run identity changed during evidence recovery",
            ));
        }
        Ok(())
    }

    fn execute(
        &mut self,
        request: &ReadAcceptanceEvidenceRequest,
        connection: &EngramConnectionConfig,
        read: &impl Fn(
            &EngramConnectionConfig,
            &[String],
            Duration,
        ) -> Result<Value, EngramTransportError>,
        now: &impl Fn() -> std::time::Instant,
    ) -> Result<Value, ApiError> {
        self.inspect(connection, read, now)?;
        let mut result = match request.action.as_str() {
            "catalog" => {
                let handle = request
                    .cursor
                    .as_ref()
                    .or(self.initial_cursor.as_ref())
                    .cloned();
                match handle {
                    None => json!({"catalog":[], "cursor":null, "remainingUnread":0}),
                    Some(handle) if self.pages.contains_key(&handle) => self.pages[&handle].clone(),
                    Some(handle) => {
                        let previous = self.cursors.get(&handle).cloned().ok_or_else(|| {
                            ApiError::conflict("the catalog cursor was not issued for this attempt")
                        })?;
                        let token = previous.after.as_ref().ok_or_else(|| {
                            ApiError::conflict("captured catalog has no continuation")
                        })?;
                        let args = vec![
                            "work".to_owned(),
                            "show".to_owned(),
                            self.manifest.identity.work_ref.clone(),
                            "--notes".to_owned(),
                            "--gates".to_owned(),
                            "--after".to_owned(),
                            token.clone(),
                            "--json".to_owned(),
                        ];
                        let page = self.call(connection, &args, read, now)?;
                        let window = acceptance_compact_notes_carrier(
                            &page,
                            &self.manifest.identity,
                            &self.manifest.basis,
                            &previous,
                            &self.seen,
                        )?;
                        if window.read_cut.project_position < previous.read_cut.project_position {
                            return Err(ApiError::conflict(
                                "the recovered catalog moved before its captured cut",
                            ));
                        }
                        let rows = page["notes"]
                            .as_array()
                            .ok_or_else(|| ApiError::bad_gateway("catalog has no records"))?;
                        let mut catalog_seen = self.catalog_seen.clone();
                        for row in rows {
                            let locator = row["locator"]
                                .as_str()
                                .filter(|s| is_citable_acceptance_locator(s))
                                .ok_or_else(|| {
                                    ApiError::bad_gateway("catalog has an invalid record locator")
                                })?;
                            if !catalog_seen.insert(locator.to_owned()) {
                                return Err(ApiError::conflict(
                                    "catalog repeated an already discovered record",
                                ));
                            }
                        }
                        self.catalog_seen = catalog_seen;
                        let next = window.after.as_ref().map(|_| Uuid::new_v4().to_string());
                        if let Some(token) = &window.after {
                            self.seen.insert(token.clone());
                        }
                        if let Some(next) = &next {
                            self.cursors.insert(next.clone(), window.clone());
                        }
                        for row in rows {
                            self.known
                                .insert(row["locator"].as_str().unwrap().to_owned(), row.clone());
                        }
                        let response = json!({"catalog":rows, "cursor":next, "remainingUnread":window.older,
                            "pageReadCut":window.read_cut});
                        self.pages.insert(handle, response.clone());
                        response
                    }
                }
            }
            "records" => {
                let mut records = Vec::new();
                for locator in &request.locators {
                    let args = vec![
                        "work".to_owned(),
                        "show".to_owned(),
                        self.manifest.identity.work_ref.clone(),
                        "--note".to_owned(),
                        locator.clone(),
                        "--json".to_owned(),
                    ];
                    let record = self.call(connection, &args, read, now)?;
                    validate_acceptance_selected_record(
                        &record,
                        &self.manifest.identity.work_ref,
                        locator,
                    )?;
                    let original = &self.known[locator];
                    // Whole records are immutable. Any provenance or size assertion
                    // already exposed by the captured catalog must still agree.
                    for field in [
                        "family",
                        "kind",
                        "by",
                        "created_at",
                        "non_holder",
                        "body_bytes",
                    ] {
                        if original
                            .get(field)
                            .is_some_and(|v| record["note"].get(field) != Some(v))
                        {
                            return Err(ApiError::conflict(
                                "the exact evidence record disagrees with its captured catalog",
                            ));
                        }
                    }
                    records.push(record);
                }
                json!({"records":records})
            }
            _ => unreachable!("validated read action"),
        };
        self.inspect(connection, read, now)?;
        result["state"] = json!("complete");
        result["recorded"] = json!(false);
        result["attemptKey"] = json!(request.attempt_key);
        result["readKey"] = json!(request.read_key);
        result["capturedCut"] = json!(self.manifest.window.read_cut);
        result["remainingReadMillis"] = json!(self.remaining.as_millis() as u64);
        Ok(result)
    }
}

fn acceptance_evidence_unfinished(
    target: &DelegationAcceptanceEvaluation,
) -> Option<&AcceptanceUnfinishedEvidenceRead> {
    target
        .attempt_history
        .as_ref()?
        .unfinished_evidence
        .as_ref()
}

fn acceptance_unfinished_evidence_notice_locked(
    inner: &StateInner,
    store: &EngramAuthorityStoreKey,
    work: &str,
) -> String {
    let Some(target) = inner
        .delegations
        .iter()
        .filter_map(|d| d.acceptance_evaluation.as_ref().map(|t| (d, t)))
        .filter(|(_, t)| t.judges(store, work))
        .max_by_key(|(d, t)| {
            (
                t.attempt_history.as_ref().map_or(0, |h| h.attempt_sequence),
                &d.created_at,
            )
        })
        .map(|(_, t)| t)
    else {
        return String::new();
    };
    acceptance_evidence_unfinished(target).map(|read|
        format!("Earlier evidence recovery ended unfinished without an Engram verdict. Attempt {} at evidence cut {}: {}. This new attempt must read the proof and judge afresh.\n",
            acceptance_brief_text(&read.attempt_key, 128), target.evidence_basis, acceptance_brief_text(&read.reason, 512)))
        .unwrap_or_default()
}

fn acceptance_evidence_read_tool_definition() -> Value {
    json!({"name":TERMAL_READ_ACCEPTANCE_EVIDENCE_TOOL_NAME,
        "description":TERMAL_READ_ACCEPTANCE_EVIDENCE_TOOL_DESCRIPTION,
        "annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false},
        "inputSchema":{"type":"object","additionalProperties":false,"required":["attemptKey","readKey","action"],
            "properties":{"attemptKey":{"type":"string","minLength":1,"maxLength":256},
                "readKey":{"type":"string","minLength":1,"maxLength":128},
                "action":{"type":"string","enum":["catalog","records"]},
                "cursor":{"type":"string","maxLength":128},
                "locators":{"type":"array","maxItems":MAX_ACCEPTANCE_SELECTED_RECORDS,
                    "items":{"type":"string"}}}}})
}

impl AppState {
    // Caller/attempt identity is independent of source freshness. A valid
    // original attempt can retain an unfinished reason after its scope moves.
    fn acceptance_evidence_caller_locked(
        &self,
        inner: &StateInner,
        child: &str,
        key: &str,
    ) -> Result<AcceptanceEvaluationSubmitAuthority, ApiError> {
        let authority = acceptance_evaluation_submit_authority_locked(inner, child)?;
        if authority.target.attempt_key != key {
            return Err(ApiError::conflict(
                "the evidence read must echo this evaluator's current attemptKey",
            ));
        }
        let delegation = &inner.delegations[inner
            .find_delegation_index(&authority.delegation_id)
            .ok_or_else(|| ApiError::conflict("the evaluator delegation disappeared"))?];
        if authority.target.attempt_history.as_ref().is_some_and(|h| {
            h.child_session_id != child
                || h.parent_session_id != delegation.parent_session_id
                || h.cwd != delegation.cwd
        }) {
            return Err(ApiError::conflict(
                "the stored evidence attempt no longer matches its evaluator parent and child",
            ));
        }
        if !authority
            .target
            .attempt_history
            .as_ref()
            .is_some_and(|h| h.evidence_recovery_enabled)
        {
            return Err(ApiError::conflict(
                "this attempt was briefed without evidence recovery; judge only the evidence shown and identify missing proof as insufficient-evidence",
            ));
        }
        if !matches!(
            authority.target.submission,
            AcceptanceEvaluationSubmission::None
        ) {
            return Err(ApiError::conflict(
                "evidence cannot be read after submission began",
            ));
        }
        Ok(authority)
    }

    fn acceptance_evidence_read_target_locked(
        &self,
        inner: &StateInner,
        child: &str,
        authority: &AcceptanceEvaluationSubmitAuthority,
    ) -> Result<WorkReadTarget, ApiError> {
        let delegation = &inner.delegations[inner
            .find_delegation_index(&authority.delegation_id)
            .ok_or_else(|| ApiError::conflict("the evaluator delegation disappeared"))?];
        let target = acceptance_evaluation_host_target_locked(inner, child)?;
        if authority.target.store.as_ref() != Some(&target.store) {
            return Err(ApiError::conflict(
                ACCEPTANCE_EVALUATION_STORE_CHANGED_ERROR,
            ));
        }
        let naming = authority
            .target
            .naming_history
            .as_ref()
            .ok_or_else(|| ApiError::conflict("the attempt has no naming history"))?;
        if engram_work_naming_change(inner, &target.store, naming).is_some()
            || authority.target.source_root.as_ref().is_some_and(|root| {
                !root.still_named(&inner.engram_work_source_roots, &target.store)
            })
            || authority.target.source_claim.as_ref().is_some_and(|claim| {
                acceptance_evaluation_claim_named_since(
                    &inner.engram_work_source_roots,
                    &target.store,
                    claim,
                    &authority.target.work_ref,
                ) || acceptance_evaluation_other_claim_named_since(inner, &target.store, claim)
            })
        {
            return Err(ApiError::conflict(
                "the attempt's source naming scope changed before evidence recovery",
            ));
        }
        let claim =
            authority
                .target
                .source_root
                .as_ref()
                .map(|root| AcceptanceEvaluationSourceClaim {
                    work_id: root.work_id.clone(),
                    claim_id: root.claim_id.clone(),
                    named_generation_at_request: None,
                });
        if acceptance_evaluation_claim_unconfirmed_locked(
            inner,
            &delegation.parent_session_id,
            &target.store,
            claim.as_ref().or(authority.target.source_claim.as_ref()),
        ) {
            return Err(ApiError::conflict(
                "the evidence attempt's source claim is unconfirmed",
            ));
        }
        validate_work_read_target(&target)?;
        Ok(target)
    }

    fn finish_acceptance_evidence_read_locked(
        &self,
        inner: &mut StateInner,
        authority: &AcceptanceEvaluationSubmitAuthority,
        request: &ReadAcceptanceEvidenceRequest,
        reason: String,
        cut: Option<Value>,
    ) -> Result<Value, ApiError> {
        let index = inner
            .find_delegation_index(&authority.delegation_id)
            .ok_or_else(|| {
                ApiError::conflict("the evaluator disappeared during evidence recovery")
            })?;
        let target = inner.delegations[index]
            .acceptance_evaluation
            .as_mut()
            .filter(|target| **target == authority.target)
            .ok_or_else(|| {
                ApiError::conflict("the evaluator target changed during evidence recovery")
            })?;
        let history = target.attempt_history.as_mut().ok_or_else(||
            ApiError::conflict("this legacy attempt cannot recover evidence; finish and request a fresh evaluation"))?;
        let unfinished = history
            .unfinished_evidence
            .get_or_insert(AcceptanceUnfinishedEvidenceRead {
                attempt_key: request.attempt_key.clone(),
                read_key: request.read_key.clone(),
                intent_digest: Some(acceptance_evaluation_payload_digest(&[request.intent()])),
                reason: acceptance_brief_text(&reason, 1024),
                captured_cut: cut,
                recorded: false,
                persistence_confirmed: false,
            })
            .clone();
        inner.mark_delegation_mutated(index);
        Ok(
            json!({"state":"unfinished", "recorded":false, "attemptKey":request.attempt_key,
            "readKey":request.read_key, "unfinishedEvidence":unfinished,
            "remedy":"No verdict was sent. Finish and report this reason; the parent may request a fresh evaluation."}),
        )
    }

    fn read_acceptance_evidence_with_io(
        &self,
        child: &str,
        request: ReadAcceptanceEvidenceRequest,
        read: impl Fn(
            &EngramConnectionConfig,
            &[String],
            Duration,
        ) -> Result<Value, EngramTransportError>,
        now: impl Fn() -> std::time::Instant,
    ) -> Result<Value, ApiError> {
        let result =
            self.read_acceptance_evidence_with_io_inner(child, request.clone(), read, now)?;
        if result["state"] != "unfinished" {
            return Ok(result);
        }
        // The cache may hold this reply while its first persistence attempt is
        // unconfirmed. Every replay retries acknowledgement, never tracker IO.
        let (authority, written, dispatch) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let authority =
                self.acceptance_evidence_caller_locked(&inner, child, &request.attempt_key)?;
            let unfinished =
                acceptance_evidence_unfinished(&authority.target).ok_or_else(|| {
                    ApiError::conflict("the unfinished attempt changed before acknowledgement")
                })?;
            if unfinished.persistence_confirmed {
                return Ok(result);
            }
            let index = inner
                .find_delegation_index(&authority.delegation_id)
                .unwrap();
            // A failed dispatch remains a retryable local unfinished state.
            inner.mark_delegation_mutated(index);
            let (_, dispatch) = self.commit_locked_with_persist_dispatch(&mut inner)
                .map_err(|e| ApiError::internal(format!("unfinished recovery is not acknowledged durable; repeat the same readKey: {e:#}")))?;
            (authority, inner.delegations[index].clone(), dispatch)
        };
        if dispatch != PersistDispatch::Synchronous {
            self.confirm_acceptance_evaluation_submission_durable(&authority, &written)
                .map_err(|e| ApiError::internal(format!("unfinished recovery is not acknowledged durable; repeat the same readKey: {e}")))?;
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_delegation_index(&authority.delegation_id)
            .ok_or_else(|| {
                ApiError::conflict("the evaluator disappeared before acknowledgement")
            })?;
        let target = inner.delegations[index]
            .acceptance_evaluation
            .as_mut()
            .filter(|t| **t == authority.target)
            .ok_or_else(|| ApiError::conflict("the evaluator changed before acknowledgement"))?;
        target
            .attempt_history
            .as_mut()
            .unwrap()
            .unfinished_evidence
            .as_mut()
            .unwrap()
            .persistence_confirmed = true;
        Ok(result)
    }

    fn read_acceptance_evidence_with_io_inner(
        &self,
        child: &str,
        request: ReadAcceptanceEvidenceRequest,
        read: impl Fn(
            &EngramConnectionConfig,
            &[String],
            Duration,
        ) -> Result<Value, EngramTransportError>,
        now: impl Fn() -> std::time::Instant,
    ) -> Result<Value, ApiError> {
        request.validate()?;
        let entry = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            self.acceptance_evidence_caller_locked(&inner, child, &request.attempt_key)?;
            inner
                .acceptance_evidence_recovery
                .get(&request.attempt_key)
                .cloned()
        };
        let Some(entry) = entry else {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let authority =
                self.acceptance_evidence_caller_locked(&inner, child, &request.attempt_key)?;
            if inner
                .acceptance_evaluation_submissions_in_flight
                .contains_key(&authority.delegation_id)
            {
                return Err(ApiError::conflict(
                    "an evaluation operation is already in progress",
                ));
            }
            if let Some(unfinished) = acceptance_evidence_unfinished(&authority.target) {
                if unfinished.read_key != request.read_key {
                    return Err(ApiError::conflict(
                        "this evidence read attempt already ended without a verdict",
                    ));
                }
                if unfinished.intent_digest.as_deref()
                    != Some(acceptance_evaluation_payload_digest(&[request.intent()]).as_str())
                {
                    return Err(ApiError::conflict(
                        "readKey cannot be replayed with a different or unavailable original intent",
                    ));
                }
            }
            return self.finish_acceptance_evidence_read_locked(
                &mut inner,
                &authority,
                &request,
                "the original evidence recovery state is unavailable; its allowance was not reset"
                    .to_owned(),
                None,
            );
        };
        // One attempt owns one serialized logical-read stream. Identical
        // concurrent calls wait here and get the first result, not another IO.
        let mut recovery = entry.lock().expect("evidence recovery mutex poisoned");
        let (authority, target, _in_flight) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let authority =
                self.acceptance_evidence_caller_locked(&inner, child, &request.attempt_key)?;
            if !recovery.same_scope(&authority.target) {
                return Err(ApiError::conflict(
                    "the evidence recovery belongs to another attempt scope",
                ));
            }
            if let Some((intent, result)) = recovery.replies.get(&request.read_key) {
                if *intent != request.intent() {
                    return Err(ApiError::conflict(
                        "readKey is already bound to another evidence read intent",
                    ));
                }
                return Ok(result.clone());
            }
            if acceptance_evidence_unfinished(&authority.target).is_some() {
                return Err(ApiError::conflict(
                    "this evidence read attempt already ended without a verdict; request a fresh evaluation",
                ));
            }
            // A fabricated cursor/locator refuses only that call, never the
            // legitimate attempt. No raw continuation is accepted from callers.
            if request.action == "catalog"
                && request
                    .cursor
                    .as_ref()
                    .is_some_and(|key| !recovery.cursors.contains_key(key))
                || request.action == "records"
                    && request
                        .locators
                        .iter()
                        .any(|id| !recovery.known.contains_key(id))
            {
                return Err(ApiError::conflict(
                    "the cursor or record was not issued to this evaluator attempt",
                ));
            }
            let target = self.acceptance_evidence_read_target_locked(&inner, child, &authority);
            let guard = AcceptanceEvaluationSubmissionInFlight::admit_locked(
                self,
                &mut inner,
                &authority.delegation_id,
            )
            .ok_or_else(|| ApiError::conflict("an evaluation operation is already in progress"))?;
            (authority, target, guard)
        };
        let result = match target {
            Err(error) => Err(error),
            Ok(_) if recovery.replies.len() >= MAX_ACCEPTANCE_BRIEF_EVIDENCE_ENTRIES => Err(
                ApiError::conflict("the attempt's bounded logical evidence reads are exhausted"),
            ),
            Ok(target) => recovery.execute(&request, &target.connection, &read, &now),
        };
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let current =
            self.acceptance_evidence_caller_locked(&inner, child, &request.attempt_key)?;
        if current.target != authority.target {
            return Err(ApiError::conflict(
                "the evaluator changed while evidence was being read; nothing was exposed",
            ));
        }
        let result = result.and_then(|value| {
            self.acceptance_evidence_read_target_locked(&inner, child, &current)
                .map(|_| value)
        });
        let result = match result {
            Ok(result) => result,
            Err(error) => self.finish_acceptance_evidence_read_locked(
                &mut inner,
                &authority,
                &request,
                error.message,
                Some(json!(recovery.manifest.window.read_cut)),
            )?,
        };
        recovery
            .replies
            .insert(request.read_key.clone(), (request.intent(), result.clone()));
        // The in-flight guard relocks state when dropped.
        drop(inner);
        Ok(result)
    }
}

async fn read_session_acceptance_evidence(
    AxumPath(child): AxumPath<String>,
    State(state): State<AppState>,
    request: Result<Json<ReadAcceptanceEvidenceRequest>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(request) = request.map_err(|e| api_json_rejection("acceptance evidence read", e))?;
    let permit =
        acquire_acceptance_evaluation_permit(ACCEPTANCE_EVALUATION_SUBMIT_PERMITS.clone())?;
    run_blocking_api(move || {
        let _permit = permit;
        let cwd = state.engram_host_launch_workdir();
        let result = state
            .read_acceptance_evidence_with_io(
                &child,
                request,
                |connection, args, timeout| {
                    run_acceptance_evaluation_read(connection, args, timeout, &cwd)
                },
                std::time::Instant::now,
            )
            .map(Json);
        state.refresh_acceptance_evaluation_card(&child);
        result
    })
    .await
}
