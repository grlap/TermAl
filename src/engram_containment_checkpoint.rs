// Passive historical checkpoint records. These codecs validate stored identity
// and mirror integrity; no transport, scheduling or admission owner uses them.

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngramExistingCheckpoint {
    id: String,
    authority: String,
    connection: EngramConnectionConfig,
    settings: Value,
    work_binding: Option<Value>,
    request: Value,
    dispatch_generation: u64,
    active_turn_generation: u64,
    runtime_episode: String,
    checkpoint_owner_generation: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum EngramExistingCheckpointProgress {
    Retained,
    PossiblySent,
    Accepted { response: Value },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
enum EngramCheckpointAttemptResult {
    CertifiedNotApplied { code: String },
    Unknown { code: Option<String> },
    Refused { response: Value },
    Accepted,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngramCheckpointAttempt {
    id: String,
    predecessor: Option<String>,
    dispatch_generation: u64,
    active_turn_generation: u64,
    runtime_episode: String,
    queued_prompt_id: Option<String>,
    queued_fingerprint: Option<String>,
    result: Option<EngramCheckpointAttemptResult>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramCheckpointOperation {
    original: EngramExistingCheckpoint,
    progress: EngramExistingCheckpointProgress,
    protection: EngramSessionObligation,
    attempts: Vec<EngramCheckpointAttempt>,
}

impl<'de> Deserialize<'de> for EngramCheckpointOperation {
    fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct RequiredImage {
            original: EngramExistingCheckpoint,
            progress: EngramExistingCheckpointProgress,
            protection: EngramSessionObligation,
            attempts: Vec<EngramCheckpointAttempt>,
        }
        let raw = Value::deserialize(decoder)?;
        let image: RequiredImage =
            serde_json::from_value(raw.clone()).map_err(serde::de::Error::custom)?;
        let operation = Self {
            original: image.original,
            progress: image.progress,
            protection: image.protection,
            attempts: image.attempts,
        };
        if serde_json::to_value(&operation).map_err(serde::de::Error::custom)? != raw {
            return Err(serde::de::Error::custom("unsupported checkpoint operation shape"));
        }
        operation.validate().map_err(serde::de::Error::custom)?;
        Ok(operation)
    }
}

fn historical_checkpoint_receipt_matches(response: &Value, grant_id: &str) -> bool {
    response["decision"] == "checkpointed"
        && response["receipt"]["grant_id"].as_str() == Some(grant_id)
        && response["receipt"]["cursor"].as_i64().is_some()
        && response["receipt"]["confirmed_cursor"].as_i64().is_some()
}

impl EngramCheckpointOperation {
    fn validate(&self) -> Result<()> {
        validate_existing_checkpoint_protection(&self.protection)?;
        if self.attempts.is_empty() || self.attempts.len() > ENGRAM_SESSION_OBLIGATION_MAX_ENTRIES {
            bail!("invalid checkpoint attempt inventory");
        }
        let mut ids = BTreeSet::new();
        for (index, attempt) in self.attempts.iter().enumerate() {
            if attempt.id.trim().is_empty() || !ids.insert(&attempt.id)
                || attempt.runtime_episode.is_empty()
                || attempt.queued_prompt_id.is_some() != attempt.queued_fingerprint.is_some()
                || attempt.predecessor.as_ref()
                    != index.checked_sub(1).map(|previous| &self.attempts[previous].id)
            {
                bail!("invalid checkpoint attempt lineage");
            }
            match &attempt.result {
                Some(EngramCheckpointAttemptResult::CertifiedNotApplied { code })
                    if !matches!(code.as_str(), "control_request_expired" | "control_store_busy") =>
                {
                    bail!("unsupported historical non-application guarantee");
                }
                Some(EngramCheckpointAttemptResult::Refused { response })
                    if response["decision"] != "refuse"
                        || response["code"].as_str().is_none_or(|code| !historical_control_code(code))
                        || response.as_object().is_none_or(|fields| fields.len() != 2) =>
                {
                    bail!("invalid historical checkpoint refusal");
                }
                _ => {}
            }
        }
        let first = &self.attempts[0];
        if first.id != self.original.id
            || first.dispatch_generation != self.original.dispatch_generation
            || first.active_turn_generation != self.original.active_turn_generation
            || first.runtime_episode != self.original.runtime_episode
            || self.protection.id != self.original.id
            || self.protection.frozen_request != serde_json::to_value(&self.original)?
        {
            bail!("checkpoint original/attempt/protection mismatch");
        }
        let receipt = match &self.progress {
            EngramExistingCheckpointProgress::Accepted { response } => response.clone(),
            _ => Value::Null,
        };
        if self.protection.settlement_evidence != receipt
            || (!receipt.is_null()
                && !historical_checkpoint_receipt_matches(&receipt, &self.protection.grant_id))
        {
            bail!("checkpoint receipt differs from its stored protection");
        }
        Ok(())
    }
}

fn validate_existing_checkpoint_protection(entry: &EngramSessionObligation) -> Result<()> {
    if entry.kind != EngramSessionObligationKind::ExistingCheckpoint
        || entry.basis != EngramSessionObligationBasis::ExistingCheckpoint
    {
        bail!("invalid existing checkpoint protection kind");
    }
    let original: EngramExistingCheckpoint = serde_json::from_value(entry.frozen_request.clone())?;
    let request = &original.request;
    if serde_json::to_value(&original)? != entry.frozen_request
        || original.id.trim().is_empty()
        || original.authority.trim().is_empty()
        || original.runtime_episode.is_empty()
        || original.id != entry.id
        || original.connection != entry.connection
        || original.dispatch_generation != entry.owner_generation
        || original.connection.session_id != entry.local_session_id
        || entry.project_id.trim().is_empty()
        || entry.external_ref.trim().is_empty()
        || !original.settings.is_object()
        || original.settings.get("authorityStoreKey").filter(|value| !value.is_null())
            .is_some_and(|authority| serde_json::to_value(&entry.authority).ok().as_ref() != Some(authority))
        || request["operation"] != "turn_checkpoint"
        || request["routing_token"].as_str() != Some(entry.routing_token.as_str())
        || request["grant_id"].as_str() != Some(entry.grant_id.as_str())
        || entry.routing_token.is_empty() || entry.grant_id.is_empty()
        || request["idempotency_key"].as_str().is_none_or(str::is_empty)
    {
        bail!("checkpoint protection does not name its exact stored original");
    }
    if (!entry.settlement_evidence.is_null()
        && !historical_checkpoint_receipt_matches(&entry.settlement_evidence, &entry.grant_id))
        || (entry.phase == EngramSessionObligationPhase::Confirmed
            && entry.settlement_evidence.is_null())
    {
        bail!("checkpoint protection lacks its matching stored receipt");
    }
    Ok(())
}
