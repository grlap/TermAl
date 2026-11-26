// Evaluator-only evidence reads. Reuses the acceptance catalog and exact-record
// validators and the submission admission owner. The runtime cache retains the
// original attempt's cursors and actual-I/O quota; it is not tracker authority.
// Lost runtime state closes recovery rather than minting another allowance.

use super::*;

pub(crate) const TERMAL_READ_ACCEPTANCE_EVIDENCE_TOOL_NAME: &str =
    "termal_read_acceptance_evidence";
pub(crate) const TERMAL_READ_ACCEPTANCE_EVIDENCE_QUALIFIED_TOOL_NAME: &str =
    "mcp__termal-delegation__termal_read_acceptance_evidence";
pub(crate) const TERMAL_READ_ACCEPTANCE_EVIDENCE_TOOL_DESCRIPTION: &str = "When your brief enables evidence recovery, start with requester-selected proof. Use action records and only requester-selected locators to read their captured bodies without spending discovery allowance. Use action catalog for other older locators: omit cursor initially, then use only a cursor this tool returned. Read whole bodies with action records and known locators. Supply your exact attemptKey and a new readKey per logical read. Repeating the same key and arguments replays the result without tracker I/O; reusing the key with different arguments refuses. An exhausted result closes optional discovery and leaves submission open. Submit one verdict per criterion, with reasoned insufficient-evidence wherever unread proof prevents a judgment. A stale or unfinished result ends the attempt without a verdict: finish and report the reason so the parent can request a fresh evaluation.";

const ACCEPTANCE_DISCOVERY_EXHAUSTED_REMEDY: &str = "Submit one verdict per criterion on this attempt. Use insufficient-evidence with a reason where unread proof prevents a judgment.";

enum AcceptanceEvidenceReadError {
    AllowanceSpent(&'static str),
    Failed(ApiError),
}

impl From<ApiError> for AcceptanceEvidenceReadError {
    fn from(error: ApiError) -> Self {
        Self::Failed(error)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AcceptanceEvidenceRecoveryManifest {
    pub(crate) identity: AcceptanceEvidenceIdentity,
    pub(crate) basis: AcceptanceAgentEvidenceBasis,
    pub(crate) window: AcceptanceNotesWindow,
    pub(crate) reader: WorkReadTarget,
    /// The lookup path validated at admission, before canonicalization.
    pub(crate) lookup_path: PathBuf,
    pub(crate) seen: BTreeSet<String>,
    pub(crate) catalog: Vec<Value>,
    /// Exact selected records are readable before they appear in catalog paging.
    pub(crate) indexed: BTreeSet<String>,
    /// Whole requester-selected records validated inside the request's canonical bracket.
    pub(crate) selected_records: BTreeMap<String, Arc<Value>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AcceptanceUnfinishedEvidenceRead {
    pub(crate) attempt_key: String,
    pub(crate) read_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) intent_digest: Option<String>,
    pub(crate) reason: String,
    captured_cut: Option<Value>,
    /// The item's evidence changed after the captured cut, so Engram refused
    /// to continue the captured notes window. Retryable by a fresh evaluation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) stale_basis: bool,
    /// This is local attempt history, never an Engram evaluation receipt.
    #[serde(skip_deserializing)]
    pub(crate) recorded: bool,
    /// Runtime acknowledgement only. A loaded row already came from storage.
    #[serde(skip, default = "acceptance_evidence_loaded_durable")]
    pub(crate) persistence_confirmed: bool,
}

fn acceptance_evidence_loaded_durable() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ReadAcceptanceEvidenceRequest {
    pub(crate) attempt_key: String,
    pub(crate) read_key: String,
    pub(crate) action: String,
    #[serde(default)]
    pub(crate) cursor: Option<String>,
    #[serde(default)]
    pub(crate) locators: Vec<String>,
}

impl ReadAcceptanceEvidenceRequest {
    pub(crate) fn validate(&self) -> Result<(), ApiError> {
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

    pub(crate) fn intent(&self) -> String {
        // Field order is fixed by this host type, not by caller JSON order.
        serde_json::to_string(self).expect("evidence read request is serializable")
    }
}

#[derive(Debug)]
pub(crate) struct AcceptanceEvidenceRecovery {
    pub(crate) manifest: AcceptanceEvidenceRecoveryManifest,
    pub(crate) target: DelegationAcceptanceEvaluation,
    pub(crate) remaining: Duration,
    initial_cursor: Option<String>,
    pub(crate) cursors: HashMap<String, AcceptanceNotesWindow>,
    pub(crate) known: BTreeMap<String, Value>,
    catalog_seen: BTreeSet<String>,
    pub(crate) seen: BTreeSet<String>,
    pub(crate) pages: HashMap<String, Value>,
    replies: HashMap<String, (String, Value)>,
    /// Successful selected reads retain their intent and original budget display.
    /// The immutable bodies are shared in the manifest.
    selected_replies: HashMap<String, (String, u64)>,
    discovery_exhausted: bool,
    /// Set when a read failed because the captured window went stale.
    pub(crate) stale_basis: Option<String>,
}

/// Engram refused to continue the captured notes window. Judged on the
/// structured refusal code:
/// a failed CLI call carries it as `refusal_code`, a control reply as `code`.
fn acceptance_evidence_window_changed(error: &EngramTransportError) -> bool {
    error.refusal_code.as_deref().or_else(|| {
        (error.kind == EngramTransportErrorKind::Remote)
            .then_some(error.code.as_deref())
            .flatten()
    }) == Some("work_show_cursor_invalid")
}

impl AcceptanceEvidenceRecovery {
    pub(crate) fn new(
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
            selected_replies: HashMap::new(),
            discovery_exhausted: false,
            stale_basis: None,
        }
    }

    pub(crate) fn same_scope(&self, target: &DelegationAcceptanceEvaluation) -> bool {
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
    ) -> Result<Value, AcceptanceEvidenceReadError> {
        if self.remaining < ENGRAM_MIN_CALL_TIMEOUT {
            return Err(AcceptanceEvidenceReadError::AllowanceSpent(
                "the attempt's evidence read allowance is spent",
            ));
        }
        let started = now();
        let result = read(connection, args, ENGRAM_MIN_CALL_TIMEOUT);
        self.remaining = self
            .remaining
            .saturating_sub(now().saturating_duration_since(started));
        match result {
            Ok(value) => Ok(value),
            Err(error) if acceptance_evidence_window_changed(&error) => {
                let reason = format!(
                    "stale evidence basis: Engram can no longer continue the captured evidence window ({}); the item's evidence changed after the captured cut, typically by an appended record",
                    acceptance_brief_text(&error.message, 512)
                );
                self.stale_basis = Some(reason.clone());
                Err(ApiError::conflict(reason).into())
            }
            Err(error) => {
                let kind = match error.kind {
                    EngramTransportErrorKind::LocalState => "local_state",
                    EngramTransportErrorKind::Deadline => "deadline",
                    EngramTransportErrorKind::Transport => "transport",
                    EngramTransportErrorKind::Protocol => "protocol",
                    EngramTransportErrorKind::Remote => "remote",
                    #[cfg(test)]
                    EngramTransportErrorKind::Backoff => "backoff",
                };
                Err(ApiError::bad_gateway(format!(
                    "Engram evidence read failed ({kind}): {}",
                    acceptance_brief_text(&error.message, 512),
                ))
                .into())
            }
        }
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
    ) -> Result<(), AcceptanceEvidenceReadError> {
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
            )
            .into());
        }
        Ok(())
    }

    fn discover(
        &mut self,
        request: &ReadAcceptanceEvidenceRequest,
        connection: &EngramConnectionConfig,
        read: &impl Fn(
            &EngramConnectionConfig,
            &[String],
            Duration,
        ) -> Result<Value, EngramTransportError>,
        now: &impl Fn() -> std::time::Instant,
    ) -> Result<Value, AcceptanceEvidenceReadError> {
        self.inspect(connection, read, now)?;
        let result = match request.action.as_str() {
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
                            )
                            .into());
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
                                )
                                .into());
                            }
                        }
                        self.inspect(connection, read, now)?;
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
                        return Ok(response);
                    }
                }
            }
            "records" => self.read_records(&request.locators, connection, read, now)?,
            _ => unreachable!("validated read action"),
        };
        self.inspect(connection, read, now)?;
        Ok(result)
    }

    fn read_records(
        &mut self,
        locators: &[String],
        connection: &EngramConnectionConfig,
        read: &impl Fn(
            &EngramConnectionConfig,
            &[String],
            Duration,
        ) -> Result<Value, EngramTransportError>,
        now: &impl Fn() -> std::time::Instant,
    ) -> Result<Value, AcceptanceEvidenceReadError> {
        let mut records = Vec::with_capacity(locators.len());
        for locator in locators {
            let record = if let Some(record) = self.manifest.selected_records.get(locator) {
                record.as_ref().clone()
            } else {
                let args = [
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
                record
            };
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
                    )
                    .into());
                }
            }
            records.push(record);
        }
        Ok(json!({"records":records}))
    }

    fn complete(
        &self,
        request: &ReadAcceptanceEvidenceRequest,
        mut result: Value,
        remaining_read_millis: u64,
    ) -> Value {
        result["state"] = json!("complete");
        result["recorded"] = json!(false);
        result["attemptKey"] = json!(request.attempt_key);
        result["readKey"] = json!(request.read_key);
        result["capturedCut"] = json!(self.manifest.window.read_cut);
        result["remainingReadMillis"] = json!(remaining_read_millis);
        result
    }
}

pub(crate) fn acceptance_evidence_unfinished(
    target: &DelegationAcceptanceEvaluation,
) -> Option<&AcceptanceUnfinishedEvidenceRead> {
    target
        .attempt_history
        .as_ref()?
        .unfinished_evidence
        .as_ref()
}

pub(crate) fn acceptance_unfinished_evidence_notice_locked(
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

pub(crate) fn acceptance_evidence_read_tool_definition() -> Value {
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

    fn validate_acceptance_evidence_authority_locked(
        &self,
        inner: &StateInner,
        child: &str,
        authority: &AcceptanceEvaluationSubmitAuthority,
        reader: &WorkReadTarget,
    ) -> Result<(), ApiError> {
        let delegation = &inner.delegations[inner
            .find_delegation_index(&authority.delegation_id)
            .ok_or_else(|| ApiError::conflict("the evaluator delegation disappeared"))?];
        let project = acceptance_evaluation_project_locked(inner, child)?;
        let (connection, store) =
            work_host_reader_configuration(inner, project).map_err(ApiError::conflict)?;
        if authority.target.store.as_ref() != Some(&store) || reader.store != store {
            return Err(ApiError::conflict(
                ACCEPTANCE_EVALUATION_STORE_CHANGED_ERROR,
            ));
        }
        if reader.connection != connection {
            return Err(ApiError::conflict(
                "the Work reader changed during the evidence attempt; request a fresh evaluation",
            ));
        }
        let naming = authority
            .target
            .naming_history
            .as_ref()
            .ok_or_else(|| ApiError::conflict("the attempt has no naming history"))?;
        if engram_work_naming_change(inner, &store, naming).is_some()
            || authority
                .target
                .source_root
                .as_ref()
                .is_some_and(|root| !root.still_named(&inner.engram_work_source_roots, &store))
            || authority.target.source_claim.as_ref().is_some_and(|claim| {
                acceptance_evaluation_claim_named_since(
                    &inner.engram_work_source_roots,
                    &store,
                    claim,
                    &authority.target.work_ref,
                ) || acceptance_evaluation_other_claim_named_since(inner, &store, claim)
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
            &store,
            claim.as_ref().or(authority.target.source_claim.as_ref()),
        ) {
            return Err(ApiError::conflict(
                "the evidence attempt's source claim is unconfirmed",
            ));
        }
        Ok(())
    }

    fn finish_acceptance_evidence_read_locked(
        &self,
        inner: &mut StateInner,
        authority: &AcceptanceEvaluationSubmitAuthority,
        request: &ReadAcceptanceEvidenceRequest,
        reason: String,
        cut: Option<Value>,
        stale_basis: bool,
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
                stale_basis,
                recorded: false,
                persistence_confirmed: false,
            })
            .clone();
        inner.mark_delegation_mutated(index);
        if unfinished.stale_basis {
            return Ok(json!({"state":"stale", "retryable":true, "recorded":false,
                "attemptKey":request.attempt_key, "readKey":request.read_key,
                "unfinishedEvidence":unfinished,
                "remedy":"The item's evidence changed after the captured cut, so this attempt cannot judge it. Finish and report this stale basis; the parent should request a fresh evaluation, which captures the current evidence."}));
        }
        Ok(
            json!({"state":"unfinished", "failureClass":"evaluator_infrastructure",
            "recorded":false, "attemptKey":request.attempt_key,
            "readKey":request.read_key, "unfinishedEvidence":unfinished,
            "remedy":"No verdict was sent. Finish and report this reason; the parent may request a fresh evaluation."}),
        )
    }

    pub(crate) fn read_acceptance_evidence_with_io(
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
        let result = self.read_acceptance_evidence_with_io_inner(
            child,
            request.clone(),
            read,
            now,
            validate_work_read_target_at,
        )?;
        if !matches!(result["state"].as_str(), Some("unfinished" | "stale")) {
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

    #[cfg(test)]
    pub(crate) fn replay_acceptance_evidence_with_probe(
        &self,
        child: &str,
        request: ReadAcceptanceEvidenceRequest,
        probe: impl Fn(&WorkReadTarget, &FsPath) -> Result<(), ApiError>,
    ) -> Result<Value, ApiError> {
        self.read_acceptance_evidence_with_io_inner(
            child,
            request,
            |_, _, _| panic!("a cached replay must not read the tracker"),
            std::time::Instant::now,
            probe,
        )
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
        probe: impl Fn(&WorkReadTarget, &FsPath) -> Result<(), ApiError>,
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
                false,
            );
        };
        // One attempt owns one serialized logical-read stream. Identical
        // concurrent calls wait here and get the first result, not another IO.
        let mut recovery = entry.lock().expect("evidence recovery mutex poisoned");
        let intent = request.intent();
        let validate_reader = |manifest: &AcceptanceEvidenceRecoveryManifest| {
            probe(&manifest.reader, &manifest.lookup_path)
        };
        let (authority, authority_check, _in_flight) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let authority =
                self.acceptance_evidence_caller_locked(&inner, child, &request.attempt_key)?;
            if !recovery.same_scope(&authority.target) {
                return Err(ApiError::conflict(
                    "the evidence recovery belongs to another attempt scope",
                ));
            }
            let selected_reply = recovery.selected_replies.get(&request.read_key);
            let saved_reply = recovery.replies.get(&request.read_key);
            if let Some(saved_intent) = selected_reply
                .map(|(intent, _)| intent)
                .or_else(|| saved_reply.map(|(intent, _)| intent))
            {
                if *saved_intent != intent {
                    return Err(ApiError::conflict(
                        "readKey is already bound to another evidence read intent",
                    ));
                }
                // A reason-only failure may be replayed after a scope move
                // so an earlier unfinished read can be acknowledged.
                let _in_flight = if saved_reply.is_some_and(|(_, result)| {
                    matches!(result["state"].as_str(), Some("unfinished" | "stale"))
                }) {
                    drop(inner);
                    None
                } else {
                    if acceptance_evidence_unfinished(&authority.target).is_some() {
                        return Err(ApiError::conflict(
                            "this evidence read attempt already ended without a verdict; request a fresh evaluation",
                        ));
                    }
                    self.validate_acceptance_evidence_authority_locked(
                        &inner,
                        child,
                        &authority,
                        &recovery.manifest.reader,
                    )?;
                    let guard = AcceptanceEvaluationSubmissionInFlight::admit_locked(
                        self,
                        &mut inner,
                        &authority.delegation_id,
                    )
                    .ok_or_else(|| {
                        ApiError::conflict("an evaluation operation is already in progress")
                    })?;
                    drop(inner);
                    validate_reader(&recovery.manifest)?;
                    {
                        let inner = self.inner.lock().expect("state mutex poisoned");
                        let current = self.acceptance_evidence_caller_locked(
                            &inner,
                            child,
                            &request.attempt_key,
                        )?;
                        if current.delegation_id != authority.delegation_id
                            || current.target != authority.target
                        {
                            return Err(ApiError::conflict(
                                "the evaluator changed while evidence was being read; nothing was exposed",
                            ));
                        }
                        self.validate_acceptance_evidence_authority_locked(
                            &inner,
                            child,
                            &current,
                            &recovery.manifest.reader,
                        )?;
                    }
                    Some(guard)
                };
                if let Some((_, remaining_read_millis)) = selected_reply {
                    let records: Vec<_> = request
                        .locators
                        .iter()
                        .map(|locator| recovery.manifest.selected_records[locator].as_ref())
                        .collect();
                    return Ok(recovery.complete(
                        &request,
                        json!({"records":records}),
                        *remaining_read_millis,
                    ));
                }
                return Ok(saved_reply.expect("known reply").1.clone());
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
            let authority_check = self.validate_acceptance_evidence_authority_locked(
                &inner,
                child,
                &authority,
                &recovery.manifest.reader,
            );
            let guard = AcceptanceEvaluationSubmissionInFlight::admit_locked(
                self,
                &mut inner,
                &authority.delegation_id,
            )
            .ok_or_else(|| ApiError::conflict("an evaluation operation is already in progress"))?;
            (authority, authority_check, guard)
        };
        let selected_only = request.action == "records"
            && request
                .locators
                .iter()
                .all(|id| recovery.manifest.selected_records.contains_key(id));
        let connection = recovery.manifest.reader.connection.clone();
        let result = match authority_check.and_then(|()| validate_reader(&recovery.manifest)) {
            Err(error) => Err(error.into()),
            // Selected bodies already passed the opening and closing canonical
            // checks before spawn. Reuse them under the current local scope.
            Ok(())
                if selected_only
                    && recovery.selected_replies.len() >= MAX_ACCEPTANCE_BRIEF_EVIDENCE_ENTRIES =>
            {
                return Err(ApiError::conflict(
                    "the selected evidence replay limit is reached; reuse a readKey with its original arguments",
                ));
            }
            Ok(()) if selected_only => {
                recovery.read_records(&request.locators, &connection, &read, &now)
            }
            Ok(()) if recovery.discovery_exhausted => {
                return Err(ApiError::conflict(ACCEPTANCE_DISCOVERY_EXHAUSTED_REMEDY));
            }
            Ok(()) if recovery.replies.len() >= MAX_ACCEPTANCE_BRIEF_EVIDENCE_ENTRIES => {
                Err(AcceptanceEvidenceReadError::AllowanceSpent(
                    "the attempt's bounded logical evidence reads are exhausted",
                ))
            }
            Ok(()) => recovery.discover(&request, &connection, &read, &now),
        };
        let result = if matches!(&result, Err(AcceptanceEvidenceReadError::Failed(_))) {
            result
        } else {
            validate_reader(&recovery.manifest)
                .map_err(AcceptanceEvidenceReadError::from)
                .and(result)
        };
        // Taken at once, so no early return below can leave it for a later read.
        let stale_basis = recovery.stale_basis.take();
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let current =
            self.acceptance_evidence_caller_locked(&inner, child, &request.attempt_key)?;
        if current.delegation_id != authority.delegation_id || current.target != authority.target {
            return Err(ApiError::conflict(
                "the evaluator changed while evidence was being read; nothing was exposed",
            ));
        }
        let result = if matches!(&result, Err(AcceptanceEvidenceReadError::Failed(_))) {
            result
        } else {
            self.validate_acceptance_evidence_authority_locked(
                &inner,
                child,
                &current,
                &recovery.manifest.reader,
            )
            .map_err(AcceptanceEvidenceReadError::from)
            .and(result)
        };
        let complete = result.is_ok();
        let result = match result {
            Ok(result) => result,
            Err(AcceptanceEvidenceReadError::AllowanceSpent(reason)) => {
                // Closing the optional read stream prevents a partial result
                // from escaping on retry. Ordinary submission stays available.
                recovery.discovery_exhausted = true;
                json!({"state":"exhausted", "recorded":false, "attemptKey":request.attempt_key,
                    "readKey":request.read_key, "reason":reason, "capturedCut":recovery.manifest.window.read_cut,
                    "remedy":ACCEPTANCE_DISCOVERY_EXHAUSTED_REMEDY})
            }
            Err(AcceptanceEvidenceReadError::Failed(error)) => self
                .finish_acceptance_evidence_read_locked(
                    &mut inner,
                    &authority,
                    &request,
                    stale_basis.clone().unwrap_or(error.message),
                    Some(json!(recovery.manifest.window.read_cut)),
                    stale_basis.is_some(),
                )?,
        };
        // Serialize and cache response data after releasing app state.
        // The in-flight guard also relocks state when dropped.
        drop(inner);
        let remaining_read_millis = recovery.remaining.as_millis() as u64;
        let result = if complete {
            recovery.complete(&request, result, remaining_read_millis)
        } else {
            result
        };
        if complete && selected_only {
            recovery
                .selected_replies
                .insert(request.read_key, (intent, remaining_read_millis));
        } else {
            recovery
                .replies
                .insert(request.read_key, (intent, result.clone()));
        }
        Ok(result)
    }
}

pub(crate) async fn read_session_acceptance_evidence(
    AxumPath(child): AxumPath<String>,
    State(state): State<AppState>,
    request: Result<Json<ReadAcceptanceEvidenceRequest>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(request) = request.map_err(|e| api_json_rejection("acceptance evidence read", e))?;
    let permit =
        acquire_acceptance_evaluation_permit(state.acceptance_evaluation_submit_permits.clone())?;
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
