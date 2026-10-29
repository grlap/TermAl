// Owns the strict shared metadata representation and identity-based exclusion
// for unresolved control-session duties. Does not send recovery requests,
// settle grants, or activate a persistence format without its reader floor.

const ENGRAM_SESSION_OBLIGATION_LEDGER_VERSION: u32 = 1;
const ENGRAM_SESSION_OBLIGATION_MAX_ENTRIES: usize = 65_536;
const ENGRAM_SESSION_OBLIGATION_MAX_BYTES: usize = 64 * 1024 * 1024;
const ENGRAM_SESSION_OBLIGATION_REQUEST_MAX_BYTES: usize = 1024 * 1024;

#[cfg(test)]
thread_local! {
    static TEST_ENGRAM_FULL_LEDGER_VALIDATIONS: std::cell::Cell<usize>
        = const { std::cell::Cell::new(0) };
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramSessionObligationLedger {
    schema_version: u32,
    entries: Vec<EngramSessionObligation>,
    // Mandatory even when empty: unresolved checkpoint protection must survive
    // the loader discarding an unrelated malformed session row.
    checkpoint_operations: Vec<EngramCheckpointOperation>,
}

impl<'de> Deserialize<'de> for EngramSessionObligationLedger {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct RequiredLedger {
            schema_version: u32,
            entries: Vec<Value>,
            checkpoint_operations: Vec<EngramCheckpointOperation>,
        }
        let raw = RequiredLedger::deserialize(deserializer)?;
        let mut entries = Vec::with_capacity(raw.entries.len());
        for value in raw.entries {
            let entry: EngramSessionObligation =
                serde_json::from_value(value.clone()).map_err(serde::de::Error::custom)?;
            // Nested authoritative types also must not silently discard a new
            // connection/ownership field, or supply a missing recovery value.
            if serde_json::to_value(&entry).map_err(serde::de::Error::custom)? != value {
                return Err(serde::de::Error::custom(
                    "unsupported or incomplete Engram session obligation shape",
                ));
            }
            entries.push(entry);
        }
        let ledger = Self {
            schema_version: raw.schema_version,
            entries,
            checkpoint_operations: raw.checkpoint_operations,
        };
        ledger.validate().map_err(serde::de::Error::custom)?;
        Ok(ledger)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngramSessionObligation {
    id: String,
    project_id: String,
    authority: EngramAuthorityStoreKey,
    authority_confirmed: bool,
    connection: EngramConnectionConfig,
    external_ref: String,
    local_session_id: String,
    owner_generation: u64,
    prompt_id: Option<String>,
    routing_token: String,
    grant_id: String,
    kind: EngramSessionObligationKind,
    phase: EngramSessionObligationPhase,
    basis: EngramSessionObligationBasis,
    reason_code: String,
    // Preserve the exact wire value, including a future field, until the
    // strict decoder rejects it. Deserializing directly into today's request
    // enum could silently discard an unknown field and alter a replay.
    frozen_request: Value,
    settlement_evidence: Value,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramSessionObligationKind {
    ExistingCheckpoint,
    CheckpointContainment,
    IssuedGrantRetirement,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramSessionObligationPhase {
    Intended,
    Sent,
    Confirmed,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramSessionObligationBasis {
    ExistingCheckpoint,
    CheckpointUnsettled,
    ControlStateUnsettled,
    ProvenNeverSentBegin,
}

// A recovery image is not a replayable RPC. It keeps the original uncertainty
// even when no exact grant or request can safely be reconstructed. Empty token
// and grant strings in its enclosing entry mean explicitly unknown identities.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngramUnsettledControlImage {
    operation: String,
    routing_token: Option<String>,
    active_grant_id: Option<String>,
    uncertain_grant_id: Option<String>,
    begins_recorded: bool,
    bind_in_progress: bool,
    checkpoint_in_progress: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    existing_checkpoint: Option<EngramCheckpointOperation>,
    runtime_running: bool,
    // The parked schedule transferred to this exclusion before its owner
    // generation changed. History only; it is never a new retry permission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parked_admission_retry: Option<EngramAdmissionRetry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parked_bind_retry: Option<EngramBindRetry>,
    queued_prompts: Vec<QueuedPromptRecord>,
    begin_requested: Option<String>,
    dispatch_generation: u64,
}

impl EngramSessionObligationLedger {
    fn all_protections(&self) -> impl Iterator<Item = &EngramSessionObligation> {
        self.entries.iter().chain(
            self.checkpoint_operations
                .iter()
                .filter(|operation| {
                    !self
                        .entries
                        .iter()
                        .any(|entry| entry.id == operation.original.id)
                })
                .map(|operation| &operation.protection),
        )
    }
    fn empty() -> Self {
        Self {
            schema_version: ENGRAM_SESSION_OBLIGATION_LEDGER_VERSION,
            entries: Vec::new(),
            checkpoint_operations: Vec::new(),
        }
    }

    // The primary metadata loader must call this before loading recoverable
    // sessions or starting any effects. There is deliberately no Default or
    // serde(default): absence cannot stand in for an acknowledged empty ledger.
    fn validate(&self) -> Result<()> {
        #[cfg(test)]
        TEST_ENGRAM_FULL_LEDGER_VALIDATIONS.with(|count| count.set(count.get() + 1));
        let mut checkpoint_ids = BTreeSet::new();
        let mut checkpoint_keys = BTreeSet::new();
        let mut checkpoint_duties = BTreeSet::new();
        for operation in &self.checkpoint_operations {
            operation.validate()?;
            if !checkpoint_ids.insert(operation.original.id.clone()) {
                bail!("duplicate existing checkpoint operation identity");
            }
            let key = serde_json::to_value(&operation.original.request)?["idempotency_key"]
                .as_str()
                .ok_or_else(|| anyhow!("checkpoint request has no key"))?
                .to_owned();
            if !checkpoint_keys.insert((operation.original.authority.clone(), key)) {
                bail!("conflicting existing checkpoint mutation key");
            }
            if operation.protection.phase != EngramSessionObligationPhase::Confirmed
                && !checkpoint_duties.insert((
                    operation.original.authority.clone(),
                    operation.protection.external_ref.clone(),
                    operation.protection.grant_id.clone(),
                ))
            {
                bail!("conflicting existing checkpoint producer duty");
            }
            if let Some(entry) = self
                .entries
                .iter()
                .find(|entry| entry.id == operation.original.id)
            {
                let image: EngramUnsettledControlImage =
                    serde_json::from_value(entry.frozen_request.clone())?;
                if image.existing_checkpoint.as_ref().is_none_or(|adopted| {
                    serde_json::to_value(&adopted.original).ok()
                        != serde_json::to_value(&operation.original).ok()
                }) {
                    bail!("containment adoption refers to a different checkpoint operation");
                }
            }
        }
        if self.schema_version != ENGRAM_SESSION_OBLIGATION_LEDGER_VERSION {
            bail!("unsupported Engram session obligation ledger version");
        }
        if self.entries.len() + self.checkpoint_operations.len()
            > ENGRAM_SESSION_OBLIGATION_MAX_ENTRIES
            || serde_json::to_vec(self)?.len() > ENGRAM_SESSION_OBLIGATION_MAX_BYTES
        {
            bail!("Engram session obligation ledger capacity exceeded");
        }
        let mut ids = BTreeSet::new();
        let mut identities = BTreeSet::new();
        let mut token_identities = BTreeSet::new();
        let mut request_keys: BTreeMap<_, Vec<&EngramSessionObligation>> = BTreeMap::new();
        let mut outstanding_identities = BTreeSet::new();
        let mut outstanding_tokens = BTreeSet::new();
        for entry in &self.entries {
            entry.validate()?;
            // Completed admission cycles share a producer but not a wire
            // operation, queued head or owner. Keep their history distinct
            // without allowing two outstanding duties for that producer.
            let cycle = containment_original_admission(entry)
                .map(|image| {
                    let head = &image.queued_prompts[0];
                    serde_json::to_string(&json!({
                        "owner": entry.owner_generation,
                        "head": head.pending_prompt.id,
                        "fingerprint": containment_prompt_fingerprint(head),
                        "bind": head.engram_bind,
                        "evaluate": head.engram_evaluate,
                    }))
                })
                .transpose()?;
            let historical_admission = cycle.is_some()
                && entry.phase == EngramSessionObligationPhase::Confirmed
                && containment_admission_recovery_evidence_matches(entry);
            // This is structural validation only. An unacknowledged Confirmed
            // image still excludes its producer through the live/boot proof
            // boundary and cannot authorize creation of a competing cycle.
            if !historical_admission {
                if !outstanding_identities.insert((
                    &entry.authority,
                    entry.external_ref.as_str(),
                    entry.kind,
                    entry.grant_id.as_str(),
                )) || (!entry.routing_token.is_empty()
                    && !outstanding_tokens.insert((
                        &entry.authority,
                        entry.routing_token.as_str(),
                        entry.kind,
                        entry.grant_id.as_str(),
                    )))
                {
                    bail!("competing outstanding Engram session obligations");
                }
            }
            if !ids.insert(entry.id.as_str())
                || !identities.insert((
                    &entry.authority,
                    entry.external_ref.as_str(),
                    entry.kind,
                    entry.grant_id.as_str(),
                    cycle.clone(),
                ))
            {
                bail!("duplicate or conflicting Engram session obligation identity");
            }
            if !entry.routing_token.is_empty()
                && !token_identities.insert((
                    &entry.authority,
                    entry.routing_token.as_str(),
                    entry.kind,
                    entry.grant_id.as_str(),
                    cycle,
                ))
            {
                bail!("duplicate or conflicting Engram producer-token identity");
            }
            for key in engram_obligation_wire_keys(entry)? {
                let owners = request_keys
                    .entry((&entry.authority, entry.connection.session_id.as_str(), key))
                    .or_default();
                if owners.iter().any(|previous| {
                    !containment_admission_retained_duty_successor(previous, entry)
                        && !containment_admission_retained_duty_successor(entry, previous)
                }) {
                    bail!("conflicting Engram session obligation request key");
                }
                owners.push(entry);
            }
        }
        Ok(())
    }

    // Full successful-turn reports are recovery custody, not an append-only
    // audit log. Retire acknowledged superseded history with its successor,
    // or an absent session's history with the removal transaction. The same
    // custody checks also let later publications reclaim a removed session's
    // history after a volatile receipt reservation has finally been released.
    fn compact_checkpoint_history(
        &mut self,
        inner: &StateInner,
        successor: Option<&EngramCheckpointOperation>,
    ) -> Vec<String> {
        let entries = &self.entries;
        let mut retired = Vec::new();
        self.checkpoint_operations.retain(|operation| {
            let id = &operation.original.id;
            let successor_allows_retirement = successor.is_none_or(|next| {
                id != &next.original.id
                    && matches!((&operation.original.request, &next.original.request),
                        (EngramControlRequest::TurnCheckpoint { grant_id, idempotency_key, .. },
                         EngramControlRequest::TurnCheckpoint { grant_id: next_grant, idempotency_key: next_key, .. })
                            if grant_id != next_grant && idempotency_key != next_key)
            });
            let owner_absent_or_superseded = inner
                .find_session_index(&operation.protection.local_session_id)
                .is_none()
                || successor.is_some_and(|next| {
                    operation.protection.local_session_id == next.protection.local_session_id
                });
            let linked = entries.iter().any(|entry| {
                entry.id == *id
                    || entry
                        .settlement_evidence
                        .get("checkpointId")
                        .and_then(Value::as_str)
                        == Some(id.as_str())
                    || entry
                        .frozen_request
                        .get("existingCheckpoint")
                        .and_then(|image| image.get("original"))
                        .and_then(|original| original.get("id"))
                        .and_then(Value::as_str)
                        == Some(id.as_str())
            });
            let active = inner
                .engram_pending_checkpoint_receipts
                .entries
                .keys()
                .any(|(original, _)| original == id)
                || inner.sessions.iter().any(|record| {
                    record
                        .engram
                        .checkpoint_operation
                        .as_ref()
                        .is_some_and(|mirror| mirror.original.id == *id)
                        && (record.engram.checkpoint_in_progress
                            || record.engram.checkpoint_status_owner.is_some())
                });
            let removable = successor_allows_retirement
                && owner_absent_or_superseded
                // Quarantine omits the live row but keeps its SQLite mirror.
                // Its exact ledger protection must survive either caller.
                && !inner.quarantined_persisted_session_ids.contains(
                    &operation.protection.local_session_id,
                )
                && operation.protection.phase == EngramSessionObligationPhase::Confirmed
                && matches!(
                    operation.progress,
                    EngramExistingCheckpointProgress::Accepted { .. }
                )
                && operation.attempts.last().is_some_and(|attempt| {
                    matches!(attempt.result, Some(EngramCheckpointAttemptResult::Accepted))
                })
                && inner.engram_confirmed_containment_ids.contains(id)
                && !linked
                && !active
                && operation.validate().is_ok();
            if removable {
                retired.push(id.clone());
            }
            !removable
        });
        retired
    }

    // Exclusion uses producer identity, not the old local generation. A
    // recreated local row cannot make the same unresolved producer session safe.
    fn excludes(
        &self,
        authority: &EngramAuthorityStoreKey,
        connection: &EngramConnectionConfig,
        external_ref: &str,
        routing_token: Option<&str>,
    ) -> bool {
        self.entries.iter().any(|entry| {
            entry.authority == *authority
                && (entry.connection.session_id == connection.session_id
                    || entry.external_ref == external_ref
                    || routing_token.is_some_and(|token| token == entry.routing_token))
        })
    }

    fn with_entry(&self, entry: EngramSessionObligation) -> Result<Self> {
        self.validate()?;
        let mut next = self.clone();
        if let Some(previous) = next.entries.iter().find(|old| old.id == entry.id) {
            if serde_json::to_value(previous)? != serde_json::to_value(&entry)? {
                bail!("Engram session obligation identity changed");
            }
            return Ok(next);
        }
        next.entries.push(entry);
        next.validate()?;
        Ok(next)
    }
}

impl StateInner {
    // Called by the existing removal/tombstone owner, before its caller's
    // commit. Unresolved or unacknowledged duties outlive the removed row.
    fn compact_removed_checkpoint_history(&mut self) {
        if self
            .engram_session_obligations
            .checkpoint_operations
            .is_empty()
        {
            return;
        }
        let mut prospective = self.engram_session_obligations.clone();
        let retired = prospective.compact_checkpoint_history(self, None);
        if retired.is_empty() {
            return;
        }
        if let Err(error) = prospective.validate() {
            eprintln!("engram> removed checkpoint history retained: {error:#}");
            return;
        }
        self.engram_session_obligations = prospective;
        for id in retired {
            self.engram_confirmed_containment_ids.remove(&id);
        }
    }
}

// Typed admission images contain original wire operations below the top-level
// control image. Never let a new ledger UUID disguise reuse of one of their
// authoritative keys, including a specifically linked Evaluate or Begin.
fn engram_obligation_wire_keys(entry: &EngramSessionObligation) -> Result<BTreeSet<String>> {
    let mut keys = BTreeSet::new();
    fn retain(keys: &mut BTreeSet<String>, request: &EngramControlRequest) -> Result<()> {
        if let Some(key) = serde_json::to_value(request)?
            .get("idempotency_key")
            .and_then(Value::as_str)
        {
            keys.insert(key.to_owned());
        }
        Ok(())
    }
    if let Ok(request) =
        serde_json::from_value::<EngramControlRequest>(entry.frozen_request.clone())
    {
        retain(&mut keys, &request)?;
    } else if let Ok(image) =
        serde_json::from_value::<EngramUnsettledControlImage>(entry.frozen_request.clone())
    {
        if let Some(checkpoint) = &image.existing_checkpoint {
            retain(&mut keys, &checkpoint.original.request)?;
        }
        for head in &image.queued_prompts {
            if let Some(bind) = &head.engram_bind {
                retain(&mut keys, &bind.request)?;
            }
            if let Some(evaluate) = &head.engram_evaluate {
                retain(&mut keys, &evaluate.request)?;
                if let Some(prepared) = &evaluate.prepared_begin {
                    keys.insert(prepared.idempotency_key.clone());
                }
            }
        }
    }
    if let Ok(proof) = serde_json::from_value::<EngramContainmentAdmissionTransfer>(
        entry.settlement_evidence.clone(),
    ) {
        retain(&mut keys, &proof.evaluate.request)?;
        retain(&mut keys, &proof.begin)?;
    } else if let Ok(proof) =
        serde_json::from_value::<EngramContainmentBindSuccessor>(entry.settlement_evidence.clone())
    {
        retain(&mut keys, &proof.evaluate.request)?;
    } else if let Ok(proof) = serde_json::from_value::<EngramContainmentAdmissionDecision>(
        entry.settlement_evidence.clone(),
    ) {
        retain(&mut keys, &proof.evaluate.request)?;
    }
    Ok(keys)
}

impl EngramSessionObligation {
    fn validate(&self) -> Result<()> {
        if self.kind == EngramSessionObligationKind::ExistingCheckpoint {
            return validate_existing_checkpoint_protection(self);
        }
        if self.phase == EngramSessionObligationPhase::Confirmed && !self.authority_confirmed {
            bail!("unconfirmed authority provenance cannot settle a control obligation");
        }
        if self.kind == EngramSessionObligationKind::IssuedGrantRetirement
            && self
                .prompt_id
                .as_ref()
                .is_none_or(|id| id.trim().is_empty() || id.len() > 65_536)
        {
            bail!("issued-grant retirement requires its exact owner prompt");
        }
        let required = [
            self.id.as_str(),
            self.project_id.as_str(),
            self.local_session_id.as_str(),
            self.reason_code.as_str(),
            self.authority.project_id.as_str(),
            self.connection.session_id.as_str(),
            self.connection.actor_id.as_str(),
            self.external_ref.as_str(),
        ];
        if required
            .iter()
            .any(|value| value.trim().is_empty() || value.len() > 65_536)
            || self.owner_generation == 0
            || engram_containment_error_code(Some(&self.reason_code), "") != self.reason_code
            || self.routing_token.len() > 65_536
            || self.grant_id.len() > 65_536
            || !self.authority.database_path.is_absolute()
            || !self.connection.project_file.is_absolute()
            || !self.connection.home.is_absolute()
            || !self.connection.project_root.is_absolute()
            || !(self.connection.binary_path.is_absolute()
                || matches!(
                    self.connection.binary_path.components().next(),
                    Some(std::path::Component::Normal(_))
                ) && self.connection.binary_path.components().count() == 1)
        {
            bail!("invalid mandatory Engram session obligation identity");
        }
        if serde_json::to_vec(&self.frozen_request)?.len()
            > ENGRAM_SESSION_OBLIGATION_REQUEST_MAX_BYTES
        {
            bail!("Engram session obligation request capacity exceeded");
        }
        let admission_transfer = containment_admission_recovery_evidence_matches(self);
        if self.phase == EngramSessionObligationPhase::Confirmed && !admission_transfer {
            if self.routing_token.trim().is_empty()
                || self.grant_id.trim().is_empty()
                || self.grant_id == ENGRAM_UNCERTAIN_GRANT_UNKNOWN
                || !engram_containment_settlement_evidence_matches(self)
            {
                bail!("Engram session obligation confirmation lacks matching evidence");
            }
        } else if self.phase != EngramSessionObligationPhase::Confirmed
            && !self.settlement_evidence.is_null()
            && !(self.phase == EngramSessionObligationPhase::Intended
                && (containment_bind_successor(self).is_some()
                    || contained_checkpoint_progress_matches(self)))
        {
            bail!("unconfirmed Engram session obligation carries settlement evidence");
        }
        if self.kind == EngramSessionObligationKind::CheckpointContainment
            && self.basis == EngramSessionObligationBasis::ControlStateUnsettled
        {
            let image: EngramUnsettledControlImage =
                serde_json::from_value(self.frozen_request.clone())?;
            if image.operation != "unsettled_control_image"
                || serde_json::to_value(&image)? != self.frozen_request
                || image.routing_token.as_deref().unwrap_or_default() != self.routing_token
                || image
                    .active_grant_id
                    .as_deref()
                    .or(image.uncertain_grant_id.as_deref())
                    .unwrap_or_default()
                    != self.grant_id
            {
                bail!("invalid unresolved control recovery image");
            }
            if let Some(checkpoint) = &image.existing_checkpoint {
                checkpoint.validate()?;
                let original = &checkpoint.original;
                if original.connection != self.connection
                    || checkpoint.protection.authority != self.authority
                    || checkpoint.protection.authority_confirmed != self.authority_confirmed
                    || original
                        .settings
                        .authority_store_key
                        .as_ref()
                        .is_some_and(|authority| authority != &self.authority)
                    || checkpoint.attempts.last().is_none_or(|attempt| {
                        attempt.dispatch_generation != image.dispatch_generation
                            || attempt.dispatch_generation.checked_add(1)
                                != Some(self.owner_generation)
                    })
                    || !matches!(&original.request, EngramControlRequest::TurnCheckpoint { routing_token, grant_id, .. }
                        if routing_token == &self.routing_token && grant_id == &self.grant_id)
                {
                    bail!("existing checkpoint component has a different original owner");
                }
            }
            if self.phase == EngramSessionObligationPhase::Confirmed
                && !admission_transfer
                && (!image.begins_recorded
                    || image.bind_in_progress
                    || image.checkpoint_in_progress
                        && !(contained_checkpoint_progress_matches(self)
                            && contained_checkpoint_is_only_duty(&image))
                    || image.runtime_running
                    || image.begin_requested.is_some()
                    || image
                        .queued_prompts
                        .iter()
                        .any(QueuedPromptRecord::has_engram_intent)
                    || image.active_grant_id.is_some() && image.uncertain_grant_id.is_some())
            {
                bail!("unresolved control image contains additional unsettled ownership");
            }
            return Ok(());
        }
        if self.routing_token.trim().is_empty() || self.grant_id.trim().is_empty() {
            bail!("missing exact Engram session obligation grant identity");
        }
        let request: EngramControlRequest = serde_json::from_value(self.frozen_request.clone())
            .context("unsupported Engram session obligation request")?;
        if serde_json::to_value(&request)? != self.frozen_request {
            bail!("Engram session obligation request contains unsupported fields");
        }
        match (self.kind, self.basis, &request) {
            (
                EngramSessionObligationKind::CheckpointContainment,
                EngramSessionObligationBasis::CheckpointUnsettled,
                EngramControlRequest::TurnCheckpoint {
                    routing_token,
                    grant_id,
                    idempotency_key,
                    ..
                },
            ) if routing_token == &self.routing_token
                && grant_id == &self.grant_id
                && !idempotency_key.trim().is_empty() => {}
            (
                EngramSessionObligationKind::IssuedGrantRetirement,
                EngramSessionObligationBasis::ProvenNeverSentBegin,
                EngramControlRequest::SessionBind {
                    idempotency_key,
                    external_ref,
                    ..
                },
            ) if !idempotency_key.trim().is_empty() && external_ref == &self.external_ref => {}
            _ => bail!("Engram session obligation kind, basis and request do not match"),
        }
        Ok(())
    }
}

fn engram_session_obligation_locked<'a>(
    inner: &'a StateInner,
    session_id: &str,
) -> Option<&'a EngramSessionObligation> {
    if inner.engram_session_obligations.entries.is_empty()
        && inner
            .engram_session_obligations
            .checkpoint_operations
            .is_empty()
    {
        return None;
    }
    inner
        .engram_session_obligations
        .all_protections()
        .find(|entry| {
            engram_obligation_requires_exclusion(inner, entry)
                && engram_obligation_matches_session(inner, entry, session_id)
        })
}

fn engram_obligation_matches_session(
    inner: &StateInner,
    entry: &EngramSessionObligation,
    session_id: &str,
) -> bool {
    let record = inner
        .find_session_index(session_id)
        .map(|index| &inner.sessions[index]);
    let external_ref = record.map(|record| match &record.session.parent_delegation_id {
        Some(delegation) => format!("termal:delegation:{delegation}"),
        None => format!("termal:session:{session_id}"),
    });
    let current_authority = engram_project_for_session_locked(inner, session_id)
        .and_then(|project| project.engram.as_ref())
        .and_then(|settings| settings.authority_store_key.as_ref());
    // The original local identity remains excluded through settings edits,
    // even if its current connection cannot be reconstructed yet.
    entry.local_session_id == session_id
        || entry.connection.session_id == session_id
        || (current_authority == Some(&entry.authority)
            && (external_ref
                .as_ref()
                .is_some_and(|reference| *reference == entry.external_ref)
                || record
                    .and_then(|record| record.engram.routing_token.as_deref())
                    .is_some_and(|token| token == entry.routing_token)))
}

fn engram_containment_confirmation_materialized(
    inner: &StateInner,
    entry: &EngramSessionObligation,
) -> bool {
    // An adopted enclosure may share this original's release id. It cannot
    // hide receipt inputs that the canonical component still needs to owe.
    if inner.find_session_index(&entry.local_session_id).is_some_and(|index| {
        inner.sessions[index].engram.pending_batches.iter().any(|batch|
            pending_checkpoint_batch_owner(inner, &entry.local_session_id, batch)
                .is_some_and(|operation| operation.original.id == entry.id))
    }) {
        return false;
    }
    if entry.kind == EngramSessionObligationKind::ExistingCheckpoint {
        return entry.phase == EngramSessionObligationPhase::Confirmed
            && validate_existing_checkpoint_protection(entry).is_ok();
    }
    if entry.kind != EngramSessionObligationKind::CheckpointContainment
        || !entry.authority_confirmed
        || entry.phase != EngramSessionObligationPhase::Confirmed
        || !engram_containment_settlement_evidence_matches(entry)
    {
        return false;
    }
    // This confirmation is a transfer into the durable prepared-Begin owner,
    // not terminal-grant evidence. Its exact successor was committed together
    // with this proof; subsequent protocol progress cannot undo that transfer.
    if containment_admission_decision_matches(entry) {
        return true;
    }
    if containment_admission_transfer_matches(entry) {
        return containment_admission_transfer_materialized(inner, entry);
    }
    // A confirmed original grant remains terminal after its local row is
    // removed. Missing rows never release an unconfirmed obligation above.
    let Some(index) = inner.find_session_index(&entry.local_session_id) else {
        return true;
    };
    let record = &inner.sessions[index];
    record.engram.dispatch_generation >= entry.owner_generation
        && record.engram.active_grant_id.as_deref() != Some(entry.grant_id.as_str())
        && record.engram.uncertain_grant_id.as_deref() != Some(entry.grant_id.as_str())
    // A later binding may rotate its token without starting a new turn.
    // Exact terminal evidence remains valid for the original, absent grant.
}

fn engram_obligation_requires_exclusion(
    inner: &StateInner,
    entry: &EngramSessionObligation,
) -> bool {
    // A live transition needs the writer receipt as well as exact evidence.
    // Loaded confirmations get that receipt from their committed metadata.
    !inner.engram_confirmed_containment_ids.contains(&entry.id)
        // A prepared transfer's receipt does not authorize it under later
        // settings. Begun progress and terminal/no-grant settlement have
        // their separate monotonic proof rules.
        || (containment_admission_transfer_matches(entry)
            && !containment_admission_transfer_materialized(inner, entry))
}

fn engram_project_has_unresolved_obligations(inner: &StateInner, project_id: &str) -> bool {
    inner
        .engram_session_obligations
        .all_protections()
        .any(|entry| {
            entry.project_id == project_id && engram_obligation_requires_exclusion(inner, entry)
        })
}

// Explicit settings changes/removal do not settle an original report. An idle
// unresolved checkpoint can outlive its local owner in mandatory metadata;
// admission still uses the unconditional exclusion above.
fn engram_checkpoint_debt_can_outlive_owner(
    inner: &StateInner,
    entry: &EngramSessionObligation,
) -> bool {
    engram_checkpoint_report_is_inactive(inner, entry, None)
}

fn engram_checkpoint_report_is_inactive(
    inner: &StateInner,
    entry: &EngramSessionObligation,
    promoted_local_turn: Option<u64>,
) -> bool {
    let needs_original = match (entry.kind, entry.basis) {
        (EngramSessionObligationKind::ExistingCheckpoint,
            EngramSessionObligationBasis::ExistingCheckpoint) => true,
        (EngramSessionObligationKind::CheckpointContainment,
            EngramSessionObligationBasis::CheckpointUnsettled) => false,
        (EngramSessionObligationKind::CheckpointContainment,
            EngramSessionObligationBasis::ControlStateUnsettled) => {
            let Ok(image) = serde_json::from_value::<EngramUnsettledControlImage>(
                entry.frozen_request.clone(),
            ) else {
                return false;
            };
            if image.existing_checkpoint.is_none()
                || !image.begins_recorded
                || image.active_grant_id.as_deref() != Some(entry.grant_id.as_str())
                || image.routing_token.as_deref() != Some(entry.routing_token.as_str())
                || image.bind_in_progress
                || image.runtime_running
                || image.uncertain_grant_id.is_some()
                || image.begin_requested.is_some()
                || image.parked_admission_retry.is_some()
                || image.parked_bind_retry.is_some()
                || image.queued_prompts.iter().any(QueuedPromptRecord::has_engram_intent)
            {
                return false;
            }
            true
        }
        _ => return false,
    };
    if (needs_original && !inner.engram_session_obligations.checkpoint_operations.iter()
        .any(|operation| operation.original.id == entry.id))
        || checkpoint_receipt_has_live_owner(inner, &entry.id)
    {
        return false;
    }
    inner.find_session_index(&entry.local_session_id).is_none_or(|index| {
        let record = &inner.sessions[index];
        !record.engram.checkpoint_in_progress
            && record.engram.active_grant_id.as_ref().is_none_or(|grant| {
                grant == &entry.grant_id
                    && record.engram.routing_token.as_deref() == Some(entry.routing_token.as_str())
                    && record.engram.begins_recorded
            })
            && record.engram.checkpoint_status_owner.is_none()
            && !record.runtime_stop_in_progress
            && (!matches!(record.session.status,
                SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping)
                || (record.session.status == SessionStatus::Active
                    && promoted_local_turn == Some(record.active_turn_generation)))
            && !record.engram.bind_in_progress
            && record.engram.admission_in_progress.is_none()
            && record.engram.pending_dispatch.is_none()
            && record.engram.uncertain_grant_id.is_none()
            && (record.engram.begins_recorded || record.engram.routing_token.is_none())
            && !record.queued_prompts.iter().any(QueuedPromptRecord::has_engram_intent)
    })
}

// This permits only local provider dispatch. Generic Engram targets, checkpoint
// selection and recovery continue to use the unconditional obligation guard.
// The final handoff supplies the existing promoted turn's exact generation;
// promotion itself requires the ordinary inactive/quiescent check above.
fn engram_unmediated_checkpoint_dispatch_allowed(
    inner: &StateInner,
    session_id: &str,
    promoted_local_turn: Option<u64>,
) -> bool {
    let Some(index) = inner.find_session_index(session_id) else { return false; };
    let record = &inner.sessions[index];
    let project = engram_project_for_session_locked(inner, session_id);
    if !record.is_local_session()
        || (promoted_local_turn.is_none() && !record.queued_prompts.is_empty())
        || record.engram.project_reset_in_progress
        || record.engram_boot_recovery_pending
        || project.is_some_and(|project| inner.engram_project_resets.contains(&project.id))
        || project.and_then(|project| project.engram.as_ref())
            .is_some_and(EngramProjectSettings::is_control_enabled)
    {
        return false;
    }
    let mut found = false;
    for entry in inner.engram_session_obligations.all_protections().filter(|entry| {
        engram_obligation_requires_exclusion(inner, entry)
            && engram_obligation_matches_session(inner, entry, session_id)
    }) {
        if !engram_checkpoint_report_is_inactive(inner, entry, promoted_local_turn) {
            return false;
        }
        found = true;
    }
    found
}

fn require_no_engram_local_dispatch_obligation(
    inner: &StateInner,
    session_id: &str,
) -> Result<(), ApiError> {
    if engram_unmediated_checkpoint_dispatch_allowed(inner, session_id, None) {
        Ok(())
    } else {
        require_no_engram_session_obligation(inner, session_id)
    }
}

fn require_no_engram_local_admission_obligation(
    inner: &StateInner,
    session_id: &str,
) -> Result<(), ApiError> {
    if engram_unmediated_checkpoint_dispatch_allowed(inner, session_id, None) {
        Ok(())
    } else {
        require_no_engram_admission_obligation(inner, session_id)
    }
}

fn engram_project_has_administrative_blocker(inner: &StateInner, project_id: &str) -> bool {
    inner.engram_session_obligations.all_protections().any(|entry| {
        entry.project_id == project_id
            && engram_obligation_requires_exclusion(inner, entry)
            && !engram_checkpoint_debt_can_outlive_owner(inner, entry)
    })
}

fn engram_session_has_administrative_checkpoint_debt(inner: &StateInner, session_id: &str) -> bool {
    inner.engram_session_obligations.all_protections().any(|entry| {
        engram_obligation_requires_exclusion(inner, entry)
            && engram_obligation_matches_session(inner, entry, session_id)
            && engram_checkpoint_debt_can_outlive_owner(inner, entry)
    })
}

fn preserve_checkpoint_debt_after_local_reset(
    mut next: EngramSessionState,
    previous: &EngramSessionState,
    has_retained_checkpoint_debt: bool,
    pending_batches: Vec<EngramPendingBatch>,
    preserve_owed: bool,
) -> EngramSessionState {
    next.pending_batches = pending_batches;
    if preserve_owed { next.owed_binds = previous.owed_binds.clone(); }
    if has_retained_checkpoint_debt {
        next.checkpoint_operation = previous.checkpoint_operation.clone();
        next.routing_token = previous.routing_token.clone();
        next.active_grant_id = previous.active_grant_id.clone();
        next.uncertain_grant_id = previous.uncertain_grant_id.clone();
        next.begins_recorded = previous.begins_recorded;
        next.rebind_required = true;
    }
    next
}

fn require_no_engram_session_obligation(
    inner: &StateInner,
    session_id: &str,
) -> Result<(), ApiError> {
    if let Some(entry) = engram_session_obligation_locked(inner, session_id) {
        return Err(ApiError::conflict(format!(
            "Engram control recovery remains unresolved for session {session_id} (code={}); \
             this session is excluded until its original authority is settled",
            entry.reason_code,
        )));
    }
    Ok(())
}

// Deletion retains immutable checkpoint debt. Live receipt/flight ownership
// and every independent duty still keep their rows for supported accounting.
fn require_no_engram_removal_obligation(
    inner: &StateInner,
    sessions: &[String],
    teardown_checkpoints: &BTreeMap<String, (String, String)>,
) -> Result<(), ApiError> {
    for session_id in sessions {
        if engram_session_has_checkpoint_batch_custody(inner, session_id) {
            return Err(ApiError::conflict(format!(
                "Engram checkpoint batch accounting prevents removing session {session_id}; retain it until the pending credit is settled",
            )));
        }
        let blocker = inner.engram_session_obligations.all_protections().find(|entry| {
            if !engram_obligation_requires_exclusion(inner, entry)
                || !engram_obligation_matches_session(inner, entry, session_id)
                || engram_checkpoint_debt_can_outlive_owner(inner, entry) {
                return false;
            }
            let own_teardown = teardown_checkpoints.get(session_id).is_some_and(|key| {
                entry.kind == EngramSessionObligationKind::ExistingCheckpoint
                    && entry.local_session_id == *session_id
                    && entry.id == key.0
                    && inner.engram_session_obligations.checkpoint_operations.iter()
                        .any(|operation| operation.original.id == key.0
                            && operation.attempts.last().is_some_and(|attempt| attempt.id == key.1))
            });
            !own_teardown
        });
        if let Some(entry) = blocker {
            return Err(ApiError::conflict(format!(
                "Engram control recovery prevents removing session {session_id} (code={})",
                entry.reason_code,
            )));
        }
    }
    Ok(())
}

// Public recovery information is a separate projection: none of the retained
// connection credentials, requests or authority-store paths leave metadata.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramControlRecoverySummary {
    project_id: String,
    session_id: String,
    operation: String,
    code: String,
    excluded: bool,
    settlement_confirmed: bool,
    next_actions: Vec<String>,
}

fn engram_control_recovery_summaries(inner: &StateInner) -> Vec<EngramControlRecoverySummary> {
    inner
        .engram_session_obligations
        .all_protections()
        .filter(|entry| engram_obligation_requires_exclusion(inner, entry))
        .map(|entry| EngramControlRecoverySummary {
            project_id: entry.project_id.clone(),
            session_id: entry.local_session_id.clone(),
            operation: match entry.kind {
                EngramSessionObligationKind::ExistingCheckpoint => "turn_checkpoint",
                EngramSessionObligationKind::CheckpointContainment => "turn_checkpoint",
                EngramSessionObligationKind::IssuedGrantRetirement => "session_bind_retirement",
            }
            .to_owned(),
            code: entry.reason_code.clone(),
            excluded: true,
            settlement_confirmed: false,
            next_actions: {
                let target = inner.find_visible_session_index(&entry.local_session_id)
                    .map(|index| &inner.sessions[index]);
                let mut actions = Vec::new();
                if let Some(record) = target {
                    actions.push("inspect_session".to_owned());
                    if !matches!(record.runtime, SessionRuntime::None) {
                        actions.push("stop_session_if_running".to_owned());
                    }
                }
                if target.is_some() && inner.engram_pending_checkpoint_receipts.entries.iter().any(
                    |((id, _), pending)| {
                        id == &entry.id
                            && pending.origin == EngramCheckpointReceiptOrigin::OffContainment
                            && pending.accepted
                    },
                ) {
                    actions.push("resume_local_accounting".to_owned());
                }
                if target.is_some() && inner.engram_session_obligations.checkpoint_operations.iter()
                    .any(|operation| operation.original.id == entry.id
                        && matches!(operation.progress, EngramExistingCheckpointProgress::Accepted { .. })
                        && EngramCheckpointResultOwner::capture(inner, &entry.id).is_some()) {
                    actions.push("resume_local_accounting".to_owned());
                }
                actions.sort();
                actions.dedup();
                actions
            },
        })
        .collect()
}
