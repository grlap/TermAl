// Passive historical metadata. Loading validates exact stored shapes and
// identities, but never restores exclusion, replay, accounting or admission.

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
    checkpoint_operations: Vec<EngramCheckpointOperation>,
}

impl<'de> Deserialize<'de> for EngramSessionObligationLedger {
    fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct RequiredLedger {
            schema_version: u32,
            entries: Vec<EngramSessionObligation>,
            checkpoint_operations: Vec<EngramCheckpointOperation>,
        }
        let raw = Value::deserialize(decoder)?;
        let image: RequiredLedger =
            serde_json::from_value(raw.clone()).map_err(serde::de::Error::custom)?;
        let ledger = Self {
            schema_version: image.schema_version,
            entries: image.entries,
            checkpoint_operations: image.checkpoint_operations,
        };
        if serde_json::to_value(&ledger).map_err(serde::de::Error::custom)? != raw {
            return Err(serde::de::Error::custom("unsupported historical obligation shape"));
        }
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

// Requests, queued control images and evidence stay JSON. They cannot be
// converted into current RPCs or overwrite the independently settled queue.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parked_admission_retry: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parked_bind_retry: Option<Value>,
    queued_prompts: Vec<Value>,
    begin_requested: Option<String>,
    dispatch_generation: u64,
}

fn historical_control_code(code: &str) -> bool {
    !code.is_empty() && code.len() <= 64
        && code.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

impl EngramSessionObligationLedger {
    fn empty() -> Self {
        Self {
            schema_version: ENGRAM_SESSION_OBLIGATION_LEDGER_VERSION,
            entries: Vec::new(),
            checkpoint_operations: Vec::new(),
        }
    }

    fn validate(&self) -> Result<()> {
        #[cfg(test)]
        TEST_ENGRAM_FULL_LEDGER_VALIDATIONS.with(|count| count.set(count.get() + 1));
        if self.schema_version != ENGRAM_SESSION_OBLIGATION_LEDGER_VERSION
            || self.entries.len() + self.checkpoint_operations.len() > ENGRAM_SESSION_OBLIGATION_MAX_ENTRIES
            || serde_json::to_vec(self)?.len() > ENGRAM_SESSION_OBLIGATION_MAX_BYTES
        {
            bail!("unsupported historical obligation inventory");
        }
        let mut checkpoint_ids = BTreeSet::new();
        let mut checkpoint_keys = BTreeSet::new();
        let mut checkpoint_duties = BTreeSet::new();
        for operation in &self.checkpoint_operations {
            operation.validate()?;
            operation.protection.validate()?;
            let key = operation.original.request["idempotency_key"].as_str()
                .ok_or_else(|| anyhow!("checkpoint request has no key"))?;
            if !checkpoint_ids.insert(&operation.original.id)
                || !checkpoint_keys.insert((&operation.original.authority, key))
            {
                bail!("conflicting historical checkpoint identity or key");
            }
            if operation.protection.phase != EngramSessionObligationPhase::Confirmed
                && !checkpoint_duties.insert((
                    &operation.original.authority,
                    &operation.protection.external_ref,
                    &operation.protection.grant_id,
                ))
            {
                bail!("conflicting historical checkpoint producer duty");
            }
            if let Some(entry) = self.entries.iter().find(|entry| entry.id == operation.original.id) {
                let image: EngramUnsettledControlImage =
                    serde_json::from_value(entry.frozen_request.clone())?;
                if image.existing_checkpoint.as_ref().is_none_or(|adopted|
                    serde_json::to_value(&adopted.original).ok()
                        != serde_json::to_value(&operation.original).ok())
                {
                    bail!("historical checkpoint mirror differs from its canonical original");
                }
            }
        }
        let mut ids = BTreeSet::new();
        let mut identities = BTreeSet::new();
        let mut token_identities = BTreeSet::new();
        let mut outstanding_identities = BTreeSet::new();
        let mut outstanding_tokens = BTreeSet::new();
        let mut request_keys: BTreeMap<_, Vec<&EngramSessionObligation>> = BTreeMap::new();
        for entry in &self.entries {
            entry.validate()?;
            // Distinct, completed local admission records remain distinct data.
            // This does not reinstate the former live exclusion or replay owner.
            let cycle = historical_local_origin(entry).map(|image| {
                let head = &image.queued_prompts[0];
                serde_json::to_string(&json!({
                    "owner": entry.owner_generation,
                    "head": head["pending_prompt"]["id"],
                    "fingerprint": historical_head_fingerprint(head),
                    "bind": head["engram_bind"],
                    "evaluate": head["engram_evaluate"],
                }))
            }).transpose()?;
            let historical_confirmation = cycle.is_some()
                && entry.phase == EngramSessionObligationPhase::Confirmed
                && (historical_transfer_matches(entry) || historical_decision_matches(entry));
            if !historical_confirmation
                && (!outstanding_identities.insert((
                    &entry.authority, entry.external_ref.as_str(), entry.kind, entry.grant_id.as_str(),
                )) || (!entry.routing_token.is_empty() && !outstanding_tokens.insert((
                    &entry.authority, entry.routing_token.as_str(), entry.kind, entry.grant_id.as_str(),
                ))))
            {
                bail!("competing historical obligation producer duties");
            }
            if !ids.insert(entry.id.as_str()) || !identities.insert((
                &entry.authority, entry.external_ref.as_str(), entry.kind, entry.grant_id.as_str(), cycle.clone(),
            )) || (!entry.routing_token.is_empty() && !token_identities.insert((
                &entry.authority, entry.routing_token.as_str(), entry.kind, entry.grant_id.as_str(), cycle,
            )))
            {
                bail!("duplicate or conflicting historical obligation identity");
            }
            for key in historical_request_keys(entry)? {
                let owners = request_keys
                    .entry((&entry.authority, entry.connection.session_id.as_str(), key))
                    .or_default();
                if owners.iter().any(|previous|
                    !historical_retained_successor(previous, entry)
                        && !historical_retained_successor(entry, previous))
                {
                    bail!("conflicting historical obligation request key");
                }
                owners.push(entry);
            }
        }
        Ok(())
    }
}

impl EngramSessionObligation {
    fn validate(&self) -> Result<()> {
        if self.kind == EngramSessionObligationKind::ExistingCheckpoint {
            validate_existing_checkpoint_protection(self)?;
            let original: EngramExistingCheckpoint = serde_json::from_value(self.frozen_request.clone())?;
            if !historical_request_shape(&original.request)
                || (!self.settlement_evidence.is_null()
                    && !historical_receipt_matches(&self.settlement_evidence, &self.grant_id))
            {
                bail!("unsupported historical checkpoint request or receipt");
            }
            return Ok(());
        }
        if [
            self.id.as_str(), self.project_id.as_str(), self.local_session_id.as_str(),
            self.authority.project_id.as_str(), self.connection.session_id.as_str(),
            self.connection.actor_id.as_str(), self.external_ref.as_str(),
        ].iter().any(|value| value.trim().is_empty() || value.len() > 65_536)
            || self.owner_generation == 0
            || !historical_control_code(&self.reason_code)
            || self.routing_token.len() > 65_536 || self.grant_id.len() > 65_536
            || !self.authority.database_path.is_absolute()
            || !self.connection.project_file.is_absolute()
            || !self.connection.home.is_absolute() || !self.connection.project_root.is_absolute()
            || !(self.connection.binary_path.is_absolute()
                || matches!(self.connection.binary_path.components().next(),
                    Some(std::path::Component::Normal(_)))
                    && self.connection.binary_path.components().count() == 1)
            || serde_json::to_vec(&self.frozen_request)?.len() > ENGRAM_SESSION_OBLIGATION_REQUEST_MAX_BYTES
            || !self.frozen_request.is_object()
        {
            bail!("invalid historical obligation identity or request");
        }
        if self.kind == EngramSessionObligationKind::IssuedGrantRetirement
            && self.prompt_id.as_ref().is_none_or(|id| id.trim().is_empty() || id.len() > 65_536)
        {
            bail!("historical grant retirement lacks its owner prompt");
        }
        let local_confirmation = historical_transfer_matches(self) || historical_decision_matches(self);
        if self.phase == EngramSessionObligationPhase::Confirmed {
            if !self.authority_confirmed || (!local_confirmation
                && (self.routing_token.trim().is_empty() || self.grant_id.trim().is_empty()
                    || self.grant_id == "unknown" || !historical_terminal_evidence(self)))
            {
                bail!("confirmed historical obligation lacks matching stored evidence");
            }
        } else if !self.settlement_evidence.is_null()
            && !(self.phase == EngramSessionObligationPhase::Intended
                && (historical_bind_successor_matches(self)
                    || serde_json::from_value::<EngramUnsettledControlImage>(self.frozen_request.clone())
                        .ok().is_some_and(|image| historical_checkpoint_progress(self, &image))))
        {
            bail!("unconfirmed historical obligation carries settlement evidence");
        }
        match (self.kind, self.basis) {
            (EngramSessionObligationKind::CheckpointContainment,
                EngramSessionObligationBasis::ControlStateUnsettled) =>
            {
                let image: EngramUnsettledControlImage =
                    serde_json::from_value(self.frozen_request.clone())?;
                if image.operation != "unsettled_control_image"
                    || serde_json::to_value(&image)? != self.frozen_request
                    || image.routing_token.as_deref().unwrap_or_default() != self.routing_token
                    || image.active_grant_id.as_deref().or(image.uncertain_grant_id.as_deref())
                        .unwrap_or_default() != self.grant_id
                {
                    bail!("invalid historical unresolved control image");
                }
                for head in &image.queued_prompts {
                    if (historical_present(head, "engram_bind")
                        && !historical_queued_shape(&head["engram_bind"], "session_bind"))
                        || (historical_present(head, "engram_evaluate")
                            && !historical_queued_shape(&head["engram_evaluate"], "turn_evaluate"))
                    {
                        bail!("unsupported historical queued request shape");
                    }
                }
                if let Some(checkpoint) = &image.existing_checkpoint {
                    checkpoint.validate()?;
                    checkpoint.protection.validate()?;
                    let original = &checkpoint.original;
                    if original.connection != self.connection
                        || checkpoint.protection.authority != self.authority
                        || checkpoint.protection.authority_confirmed != self.authority_confirmed
                        || original.settings.get("authorityStoreKey").filter(|value| !value.is_null())
                            .is_some_and(|authority| serde_json::to_value(&self.authority).ok().as_ref() != Some(authority))
                        || checkpoint.attempts.last().is_none_or(|attempt|
                            attempt.dispatch_generation != image.dispatch_generation
                                || attempt.dispatch_generation.checked_add(1) != Some(self.owner_generation))
                        || original.request["operation"] != "turn_checkpoint"
                        || original.request["routing_token"].as_str() != Some(self.routing_token.as_str())
                        || original.request["grant_id"].as_str() != Some(self.grant_id.as_str())
                    {
                        bail!("historical checkpoint component has a different original owner");
                    }
                }
                if self.phase == EngramSessionObligationPhase::Confirmed && !local_confirmation
                    && (!image.begins_recorded || image.bind_in_progress || image.runtime_running
                        || image.begin_requested.is_some()
                        || (image.checkpoint_in_progress
                            && !(historical_checkpoint_progress(self, &image) && historical_checkpoint_only(&image)))
                        || image.queued_prompts.iter().any(historical_head_has_intent)
                        || (image.active_grant_id.is_some() && image.uncertain_grant_id.is_some()))
                {
                    bail!("historical confirmation contains additional unsettled ownership");
                }
            }
            (EngramSessionObligationKind::CheckpointContainment,
                EngramSessionObligationBasis::CheckpointUnsettled)
                if self.frozen_request["operation"] == "turn_checkpoint"
                    && self.frozen_request["routing_token"].as_str() == Some(self.routing_token.as_str())
                    && self.frozen_request["grant_id"].as_str() == Some(self.grant_id.as_str())
                    && !self.routing_token.trim().is_empty() && !self.grant_id.trim().is_empty()
                    && historical_request_shape(&self.frozen_request) => {}
            (EngramSessionObligationKind::IssuedGrantRetirement,
                EngramSessionObligationBasis::ProvenNeverSentBegin)
                if self.frozen_request["operation"] == "session_bind"
                    && self.frozen_request["external_ref"].as_str() == Some(self.external_ref.as_str())
                    && !self.routing_token.trim().is_empty() && !self.grant_id.trim().is_empty()
                    && historical_request_shape(&self.frozen_request) => {}
            _ => bail!("historical obligation kind, basis and image differ"),
        }
        Ok(())
    }
}


// Stored-data checks only. None of these functions constructs a current RPC,
// consults live settings/state, mutates history or returns an execution permit.
fn historical_fields(value: &Value, required: &[&str], optional: &[&str]) -> bool {
    value.as_object().is_some_and(|fields|
        required.iter().all(|key| fields.contains_key(*key))
            && fields.keys().all(|key| required.contains(&key.as_str()) || optional.contains(&key.as_str())))
}

fn historical_text(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| !text.trim().is_empty())
}

fn historical_present(value: &Value, key: &str) -> bool {
    value.get(key).is_some_and(|value| !value.is_null())
}

fn historical_strings(value: &Value) -> bool {
    value.as_array().is_some_and(|values| values.iter().all(Value::is_string))
}

fn historical_effects(value: &Value) -> bool {
    value.as_array().is_some_and(|values| values.iter().all(|value|
        matches!(value.as_str(), Some("observe" | "communicate" | "mutate_local"))))
}

fn historical_request_shape(request: &Value) -> bool {
    let key = historical_text(&request["idempotency_key"]).is_some();
    match request["operation"].as_str() {
        Some("turn_checkpoint") => historical_fields(request,
            &["operation", "routing_token", "grant_id", "next_intent", "idempotency_key"],
            &["observations", "verification_evidence", "environment_evidence"])
            && key && historical_text(&request["routing_token"]).is_some()
            && historical_text(&request["grant_id"]).is_some()
            && ["observations", "verification_evidence", "environment_evidence"].iter().all(|field|
                request.get(*field).is_none_or(|value| value.as_array()
                    .is_some_and(|values| !values.is_empty() && values.iter().all(Value::is_object))))
            && matches!(request["next_intent"].as_str(), Some("continue" | "wait" | "exit")),
        Some("session_bind") => historical_fields(request,
            &["operation", "external_ref", "title", "assurance", "mediated_effects", "capability_map_revision", "idempotency_key"],
            &["work_binding"])
            && key && request["external_ref"].is_string() && request["title"].is_string()
            && request["assurance"].is_string() && historical_effects(&request["mediated_effects"])
            && request["capability_map_revision"].as_i64().is_some()
            && request.get("work_binding").is_none_or(|binding|
                historical_fields(binding,
                    &["root_execution_id", "work_id", "run_id", "work_revision", "claim_id", "claim_fence"], &[])
                    && ["root_execution_id", "work_id", "run_id", "claim_id"].iter().all(|key| binding[*key].is_string())
                    && binding["work_revision"].as_i64().is_some() && binding["claim_fence"].as_i64().is_some()),
        Some("turn_evaluate") => historical_fields(request,
            &["operation", "routing_token", "intent_fingerprint", "purpose", "requested_effects", "resource_intents", "idempotency_key"], &[])
            && key && historical_text(&request["routing_token"]).is_some()
            && request["intent_fingerprint"].is_string() && request["purpose"].is_string()
            && historical_effects(&request["requested_effects"]) && request["resource_intents"].is_array(),
        Some("turn_begin") => historical_fields(request,
            &["operation", "routing_token", "grant_id", "delivery_tokens", "idempotency_key"], &[])
            && key && historical_text(&request["routing_token"]).is_some()
            && historical_text(&request["grant_id"]).is_some() && historical_strings(&request["delivery_tokens"]),
        _ => false,
    }
}

fn historical_prepared_shape(value: &Value) -> bool {
    historical_fields(value, &["idempotency_key", "grant_id", "delivery_tokens", "issued_no_later_than", "phase"],
        &["delivered_range"])
        && historical_text(&value["idempotency_key"]).is_some()
        && historical_text(&value["grant_id"]).is_some()
        && historical_strings(&value["delivery_tokens"]) && value["phase"] == "prepared"
        && value["issued_no_later_than"].as_str().is_some_and(|time| chrono::DateTime::parse_from_rfc3339(time).is_ok())
        && value.get("delivered_range").is_none_or(historical_delivered_range)
}

fn historical_queued_shape(value: &Value, operation: &str) -> bool {
    let optional: &[&str] = if operation == "session_bind" { &["operation_generation"] }
        else { &["operation_generation", "begun_grant_id", "prepared_begin"] };
    historical_fields(value, &["connection", "settings", "request"], optional)
        && serde_json::from_value::<EngramConnectionConfig>(value["connection"].clone()).ok()
            .and_then(|connection| serde_json::to_value(connection).ok()).as_ref() == Some(&value["connection"])
        && value["settings"].is_object()
        && value["request"]["operation"] == operation && historical_request_shape(&value["request"])
        && value.get("operation_generation").is_none_or(|generation| generation.as_u64().is_some())
        && value.get("begun_grant_id").is_none_or(Value::is_string)
        && value.get("prepared_begin").is_none_or(historical_prepared_shape)
}

fn historical_head_has_intent(head: &Value) -> bool {
    head["engram_ordinary_handoff"] == true || historical_present(head, "engram_bind")
        || historical_present(head, "engram_evaluate")
}

fn historical_head_fingerprint(head: &Value) -> Option<String> {
    let prompt = &head["pending_prompt"];
    if historical_present(&prompt["source"], "mailbox") {
        let mailbox = &prompt["source"]["mailbox"];
        return Some(sha256_hex(format!("mailbox\0{}\0{}",
            mailbox["mailboxId"].as_str()?, mailbox["sequence"].as_u64()?).as_bytes()));
    }
    let text = prompt["text"].as_str()?;
    let expanded = match prompt.get("expandedText") {
        None | Some(Value::Null) => "", Some(value) => value.as_str()?,
    };
    let source = head["source"].as_str()?;
    if !matches!(source, "user" | "human" | "mailbox" | "orchestrator") { return None; }
    let attachments = head["attachments"].as_array()?.iter()
        .map(|attachment| Some(sha256_hex(attachment["data"].as_str()?.as_bytes())))
        .collect::<Option<Vec<_>>>()?.join("\0");
    Some(sha256_hex(format!("text\0{text}\0expanded\0{expanded}\0attachments\0{attachments}\0source\0{source}").as_bytes()))
}

fn historical_same_settings(left: &Value, right: &Value) -> bool {
    let (Some(mut left), Some(mut right)) = (left.as_object().cloned(), right.as_object().cloned()) else { return false };
    left.remove("acceptanceEvaluation");
    right.remove("acceptanceEvaluation");
    left == right
}

fn historical_local_origin(entry: &EngramSessionObligation) -> Option<EngramUnsettledControlImage> {
    if entry.kind != EngramSessionObligationKind::CheckpointContainment || !entry.authority_confirmed
        || entry.basis != EngramSessionObligationBasis::ControlStateUnsettled { return None; }
    let image: EngramUnsettledControlImage = serde_json::from_value(entry.frozen_request.clone()).ok()?;
    let head = image.queued_prompts.first()?;
    let bind = historical_present(head, "engram_bind");
    let evaluate = historical_present(head, "engram_evaluate");
    if serde_json::to_value(&image).ok()? != entry.frozen_request || image.operation != "unsettled_control_image"
        || image.active_grant_id.is_some() || image.uncertain_grant_id.is_some() || image.checkpoint_in_progress
        || image.runtime_running || image.bind_in_progress || image.begin_requested.is_some() || !entry.grant_id.is_empty()
        || image.dispatch_generation.checked_add(1)? != entry.owner_generation
        || image.routing_token.as_deref().unwrap_or_default() != entry.routing_token
        || head["engram_ordinary_handoff"] == true || bind == evaluate
        || historical_text(&head["pending_prompt"]["id"]).is_none()
        || historical_head_fingerprint(head).is_none()
        || entry.prompt_id.as_deref() != head["pending_prompt"]["id"].as_str()
        || image.queued_prompts.iter().skip(1).any(historical_head_has_intent) { return None; }
    let queued = if bind {
        let queued = &head["engram_bind"];
        if !historical_queued_shape(queued, "session_bind") || (!image.begins_recorded && image.routing_token.is_some())
            || queued["request"]["external_ref"].as_str() != Some(entry.external_ref.as_str()) { return None; }
        queued
    } else {
        let queued = &head["engram_evaluate"];
        if !historical_queued_shape(queued, "turn_evaluate") || !image.begins_recorded
            || historical_present(queued, "begun_grant_id") || historical_present(queued, "prepared_begin")
            || queued["request"]["intent_fingerprint"].as_str() != Some(historical_head_fingerprint(head)?.as_str())
            || image.routing_token.as_deref() != queued["request"]["routing_token"].as_str() { return None; }
        queued
    };
    // This compares recorded settings only; it never consults current settings.
    if serde_json::to_value(&entry.connection).ok()? != queued["connection"]
        || queued["settings"]["enabled"] != true || queued["settings"]["turnGatedControl"] != true
        || serde_json::to_value(&entry.authority).ok()? != queued["settings"]["authorityStoreKey"] { return None; }
    Some(image)
}

fn historical_transfer_matches(entry: &EngramSessionObligation) -> bool {
    let Some(image) = historical_local_origin(entry) else { return false };
    let head = &image.queued_prompts[0];
    let proof = &entry.settlement_evidence;
    let evaluate = &proof["evaluate"];
    let begin = &proof["begin"];
    let prepared = &evaluate["prepared_begin"];
    let settings = if historical_present(head, "engram_evaluate") { &head["engram_evaluate"]["settings"] }
        else { &head["engram_bind"]["settings"] };
    let fingerprint = historical_head_fingerprint(head);
    historical_fields(proof, &["operation", "promptId", "intentFingerprint", "evaluate", "begin"], &["progress"])
        && proof["operation"] == "retained_admission_transfer"
        && proof["promptId"] == head["pending_prompt"]["id"]
        && fingerprint.as_deref() == proof["intentFingerprint"].as_str()
        && fingerprint.as_deref() == evaluate["request"]["intent_fingerprint"].as_str()
        && historical_queued_shape(evaluate, "turn_evaluate")
        && serde_json::to_value(&entry.connection).ok().as_ref() == Some(&evaluate["connection"])
        && historical_same_settings(settings, &evaluate["settings"])
        && !historical_present(evaluate, "begun_grant_id") && historical_prepared_shape(prepared)
        && historical_request_shape(begin) && begin["operation"] == "turn_begin"
        && evaluate["request"]["routing_token"] == begin["routing_token"]
        && begin["grant_id"] != "unknown" && prepared["grant_id"] == begin["grant_id"]
        && prepared["idempotency_key"] == begin["idempotency_key"]
        && prepared["delivery_tokens"] == begin["delivery_tokens"]
        && head.get("engram_evaluate").filter(|value| !value.is_null()).is_none_or(|original|
            original["request"] == evaluate["request"])
        && proof.get("progress").is_none_or(|progress|
            historical_fields(progress, &["operation", "promptId", "intentFingerprint", "grantId", "beginKey"], &[])
                && progress["operation"] == "begun_successor" && progress["promptId"] == proof["promptId"]
                && progress["intentFingerprint"] == proof["intentFingerprint"]
                && progress["grantId"] == begin["grant_id"] && progress["beginKey"] == begin["idempotency_key"])
}

fn historical_decision_matches(entry: &EngramSessionObligation) -> bool {
    let Some(image) = historical_local_origin(entry) else { return false };
    let head = &image.queued_prompts[0];
    let proof = &entry.settlement_evidence;
    let evaluate = &proof["evaluate"];
    let card = &proof["card"];
    let settings = if historical_present(head, "engram_evaluate") { &head["engram_evaluate"]["settings"] }
        else { &head["engram_bind"]["settings"] };
    historical_fields(proof, &["operation", "promptId", "evaluate", "card"], &[])
        && proof["operation"] == "retained_admission_decision" && proof["promptId"] == head["pending_prompt"]["id"]
        && historical_queued_shape(evaluate, "turn_evaluate")
        && serde_json::to_value(&entry.connection).ok().as_ref() == Some(&evaluate["connection"])
        && historical_same_settings(settings, &evaluate["settings"])
        && !historical_present(evaluate, "prepared_begin") && !historical_present(evaluate, "begun_grant_id")
        && evaluate["request"]["intent_fingerprint"].as_str() == historical_head_fingerprint(head).as_deref()
        && head.get("engram_evaluate").filter(|value| !value.is_null()).is_none_or(|original|
            original["request"] == evaluate["request"])
        && matches!(card["decision"].as_str(), Some("refuse" | "defer")) && card["stage"] == "dispatch"
        && card["schemaVersion"] == 1 && card["dispatch"] == "withheld" && !historical_present(card, "grantId")
        && card["repairArmed"] == false && card["failMode"] == "enforced"
        && historical_decision_card_shape(card)
}

fn historical_bind_successor_matches(entry: &EngramSessionObligation) -> bool {
    let Some(image) = historical_local_origin(entry) else { return false };
    let head = &image.queued_prompts[0];
    let bind = &head["engram_bind"];
    let proof = &entry.settlement_evidence;
    let evaluate = &proof["evaluate"];
    let Some(fingerprint) = historical_head_fingerprint(head) else { return false };
    let generation = bind["operation_generation"].as_u64().unwrap_or_else(|| entry.owner_generation.saturating_add(1));
    historical_fields(proof, &["operation", "promptId", "evaluate"], &[])
        && proof["operation"] == "retained_bind_evaluate" && proof["promptId"] == head["pending_prompt"]["id"]
        && historical_queued_shape(bind, "session_bind") && historical_queued_shape(evaluate, "turn_evaluate")
        && evaluate["connection"] == bind["connection"] && historical_same_settings(&evaluate["settings"], &bind["settings"])
        && evaluate["operation_generation"].as_u64() == Some(generation)
        && !historical_present(evaluate, "prepared_begin") && !historical_present(evaluate, "begun_grant_id")
        && evaluate["request"]["intent_fingerprint"] == fingerprint
        && evaluate["request"]["idempotency_key"] == format!("termal-evaluate:{}:{generation}:{fingerprint}", entry.local_session_id)
        && evaluate["request"]["purpose"] == "ordinary"
        && evaluate["request"]["requested_effects"] == bind["request"]["mediated_effects"]
        && evaluate["request"]["resource_intents"].as_array().is_some_and(Vec::is_empty)
}

fn historical_decision_card_shape(card: &Value) -> bool {
    historical_fields(card,
        &["schemaVersion", "stage", "assurance", "decision", "dispatch", "latencyMs", "failMode", "repairArmed"],
        &["causalFailure", "refusalCode", "deferCode", "directives", "deliveredRange", "nextIntent", "sourceRoot"])
        && card["assurance"].is_string()
        && ["refusalCode", "deferCode"].iter().all(|key| card.get(*key).is_none_or(Value::is_string))
        && card.get("directives").is_none_or(|value| value.as_array().is_some_and(|directives|
            !directives.is_empty() && directives.iter().all(|directive|
                historical_fields(directive, &["directiveId", "kind", "audience", "satisfaction"], &[])
                    && ["directiveId", "kind", "audience", "satisfaction"].iter().all(|key| directive[*key].is_string()))))
        && historical_fields(&card["latencyMs"], &["total"],
            &["evaluate", "begin", "checkpoint", "admissionFence", "preparationFence"])
        && card["latencyMs"].as_object().is_some_and(|fields| fields.values().all(|value| value.as_u64().is_some()))
        && card.get("deliveredRange").is_none_or(historical_delivered_range)
        && card.get("nextIntent").is_none_or(|value| matches!(value.as_str(), Some("continue" | "wait" | "exit")))
        // These bodies are retained verbatim, not converted into current cards.
        && ["causalFailure", "sourceRoot"].iter().all(|key| card.get(*key).is_none_or(Value::is_object))
}

fn historical_delivered_range(value: &Value) -> bool {
    historical_fields(value, &["from", "to", "head"], &[])
        && ["from", "to", "head"].iter().all(|key| value[*key].as_i64().is_some())
}

fn historical_receipt_matches(response: &Value, grant_id: &str) -> bool {
    historical_checkpoint_receipt_matches(response, grant_id)
        && response["receipt"].get("checkpoint").is_none_or(|value| value.is_null() || value.is_string())
        && response["receipt"].get("verification_evidence").is_none_or(historical_strings)
}


fn historical_checkpoint_progress(entry: &EngramSessionObligation, image: &EngramUnsettledControlImage) -> bool {
    let Some(checkpoint) = &image.existing_checkpoint else { return false };
    let proof = &entry.settlement_evidence;
    historical_fields(proof, &["operation", "checkpointId", "ownerGeneration", "response"], &[])
        && proof["operation"] == "existing_checkpoint_progress" && proof["checkpointId"] == checkpoint.original.id
        && proof["ownerGeneration"].as_u64() == Some(entry.owner_generation)
        && historical_receipt_matches(&proof["response"], &entry.grant_id)
}

fn historical_checkpoint_only(image: &EngramUnsettledControlImage) -> bool {
    image.checkpoint_in_progress && image.existing_checkpoint.is_some() && image.begins_recorded
        && !image.bind_in_progress && !image.runtime_running && image.uncertain_grant_id.is_none()
        && image.begin_requested.is_none() && image.parked_admission_retry.is_none() && image.parked_bind_retry.is_none()
        && !image.queued_prompts.iter().any(historical_head_has_intent)
}

fn historical_terminal_evidence(entry: &EngramSessionObligation) -> bool {
    let proof = &entry.settlement_evidence;
    if proof["operation"] == "existing_checkpoint_progress" {
        return serde_json::from_value::<EngramUnsettledControlImage>(entry.frozen_request.clone())
            .ok().is_some_and(|image| historical_checkpoint_progress(entry, &image));
    }
    if !historical_fields(proof, &["operation", "response"], &[]) { return false; }
    let response = &proof["response"];
    match proof["operation"].as_str() {
        Some("turn_checkpoint") if entry.kind == EngramSessionObligationKind::CheckpointContainment =>
            historical_receipt_matches(response, &entry.grant_id),
        Some("turn_grant_read") if entry.kind == EngramSessionObligationKind::CheckpointContainment => {
            if !historical_fields(response,
                &["control_schema_version", "session_id", "grant_id", "status", "state", "begun_at", "completed_at"], &[])
                || response["control_schema_version"] != 1 || response["status"] != "found"
                || response["session_id"] != entry.connection.session_id || response["grant_id"] != entry.grant_id
            { return false; }
            match response["state"].as_str() {
                Some("expired" | "superseded") => response["begun_at"].is_null() && response["completed_at"].is_null(),
                Some("completed") => {
                    let Some((begun, completed)) = response["begun_at"].as_str().zip(response["completed_at"].as_str()) else { return false };
                    match (chrono::DateTime::parse_from_rfc3339(begun), chrono::DateTime::parse_from_rfc3339(completed)) {
                        (Ok(begun), Ok(completed)) => completed >= begun, _ => false,
                    }
                }
                _ => false,
            }
        }
        Some("session_bind") if entry.kind == EngramSessionObligationKind::IssuedGrantRetirement =>
            historical_text(&response["routing_token"]).is_some()
                && response["status"]["session_id"] == entry.connection.session_id
                && !historical_present(&response["status"], "open_grant_id")
                && matches!(response["status"]["phase"].as_str(), Some("ready" | "sync_required"))
                && response["status"].get("named_root").is_none_or(|value| value.is_null() || value.is_object())
                && response["status"].get("open_grant_state").is_none_or(|value| value.is_null() || value.is_string())
                && response["status"].get("confirmed_cursor").is_none_or(|value| value.is_null() || value.as_i64().is_some()),
        _ => false,
    }
}

// A shared historical key is valid only across the exact recorded local
// transfer/successor link. This comparison cannot activate that successor.
fn historical_retained_successor(history: &EngramSessionObligation, successor: &EngramSessionObligation) -> bool {
    if history.id == successor.id || history.phase != EngramSessionObligationPhase::Confirmed
        || !historical_transfer_matches(history) || successor.kind != EngramSessionObligationKind::CheckpointContainment
        || successor.basis != EngramSessionObligationBasis::ControlStateUnsettled
        || history.authority != successor.authority || history.connection != successor.connection
        || history.local_session_id != successor.local_session_id || history.external_ref != successor.external_ref { return false; }
    let Ok(image) = serde_json::from_value::<EngramUnsettledControlImage>(successor.frozen_request.clone()) else { return false };
    let Some(head) = image.queued_prompts.first() else { return false };
    let transfer = &history.settlement_evidence;
    let original = &transfer["evaluate"];
    let prepared = &original["prepared_begin"];
    let evaluate = &head["engram_evaluate"];
    if !historical_queued_shape(evaluate, "turn_evaluate")
        || evaluate.get("begun_grant_id").is_some_and(|grant| grant != &prepared["grant_id"]) { return false; }
    let mut normalized = evaluate.clone();
    if let Some(fields) = normalized.as_object_mut() { fields.remove("begun_grant_id"); }
    image.dispatch_generation.checked_add(1) == Some(successor.owner_generation)
        && (if historical_present(head, "promoted_message_index") {
            original["operation_generation"].as_u64() == Some(image.dispatch_generation)
        } else { image.dispatch_generation == history.owner_generation })
        && successor.prompt_id.as_deref() == head["pending_prompt"]["id"].as_str()
        && head["pending_prompt"]["id"] == transfer["promptId"]
        && historical_head_fingerprint(head).as_deref() == transfer["intentFingerprint"].as_str()
        && !historical_present(head, "engram_bind")
        && image.queued_prompts.iter().skip(1).all(|head| !historical_head_has_intent(head))
        && normalized == *original && image.routing_token.as_deref() == transfer["begin"]["routing_token"].as_str()
        && [image.active_grant_id.as_deref(), image.uncertain_grant_id.as_deref(), image.begin_requested.as_deref()]
            .iter().any(|grant| grant.is_some() && *grant == prepared["grant_id"].as_str())
}

fn historical_request_keys(entry: &EngramSessionObligation) -> Result<BTreeSet<String>> {
    fn retain(keys: &mut BTreeSet<String>, request: &Value) {
        if let Some(key) = request["idempotency_key"].as_str() { keys.insert(key.to_owned()); }
    }
    let mut keys = BTreeSet::new();
    retain(&mut keys, &entry.frozen_request);
    if entry.basis == EngramSessionObligationBasis::ControlStateUnsettled {
        let image: EngramUnsettledControlImage = serde_json::from_value(entry.frozen_request.clone())?;
        if let Some(checkpoint) = &image.existing_checkpoint { retain(&mut keys, &checkpoint.original.request); }
        for head in &image.queued_prompts {
            retain(&mut keys, &head["engram_bind"]["request"]);
            retain(&mut keys, &head["engram_evaluate"]["request"]);
            retain(&mut keys, &head["engram_evaluate"]["prepared_begin"]);
        }
    }
    retain(&mut keys, &entry.settlement_evidence["evaluate"]["request"]);
    retain(&mut keys, &entry.settlement_evidence["begin"]);
    Ok(keys)
}

#[cfg(test)]
mod historical_obligation_validation_tests {
    use super::*;

    fn terminal(grant: &str) -> EngramSessionObligation {
        let root = std::env::current_dir().unwrap();
        EngramSessionObligation {
            id: "terminal".into(), project_id: "project".into(),
            authority: EngramAuthorityStoreKey { database_path: root.join("historical.db"), project_id: "store-project".into() },
            authority_confirmed: true,
            connection: EngramConnectionConfig {
                binary_path: PathBuf::from("engram"), project_file: root.join(".engram-project"),
                home: root.clone(), project_root: root, actor_id: "actor".into(), actor_context: None,
                session_id: "session".into(),
            },
            external_ref: "termal:session:session".into(), local_session_id: "session".into(),
            owner_generation: 2, prompt_id: None, routing_token: "token".into(), grant_id: grant.into(),
            kind: EngramSessionObligationKind::CheckpointContainment,
            phase: EngramSessionObligationPhase::Confirmed,
            basis: EngramSessionObligationBasis::ControlStateUnsettled,
            reason_code: "GrantScopeMismatch".into(),
            frozen_request: json!({
                "operation":"unsettled_control_image", "routingToken":"token", "activeGrantId":grant,
                "uncertainGrantId":null, "beginsRecorded":true, "bindInProgress":false,
                "checkpointInProgress":false, "runtimeRunning":false, "queuedPrompts":[],
                "beginRequested":null, "dispatchGeneration":1
            }),
            settlement_evidence: json!({"operation":"turn_checkpoint", "response":{
                "decision":"checkpointed", "receipt":{"grant_id":grant,"cursor":7,"confirmed_cursor":6}}}),
        }
    }

    fn ledger(entries: Vec<EngramSessionObligation>) -> EngramSessionObligationLedger {
        EngramSessionObligationLedger { schema_version: 1, entries, checkpoint_operations: Vec::new() }
    }

    fn local_cycle(label: &str, transfer: bool) -> EngramSessionObligation {
        let mut entry = terminal("");
        entry.id = label.into();
        entry.prompt_id = Some(format!("prompt-{label}"));
        entry.routing_token.clear();
        let connection = serde_json::to_value(&entry.connection).unwrap();
        let settings = json!({"enabled":true,"turnGatedControl":true,"authorityStoreKey":entry.authority});
        let bind = json!({"connection":connection,"settings":settings,"request":{
            "operation":"session_bind","external_ref":entry.external_ref,"title":"stored title",
            "assurance":"turn_gated","mediated_effects":["observe"],"capability_map_revision":1,
            "idempotency_key":format!("bind-{label}")}});
        let head = json!({"pending_prompt":{"id":entry.prompt_id,"text":format!("stored-{label}")},
            "source":"user","attachments":[],"engram_bind":bind});
        let fingerprint = historical_head_fingerprint(&head).unwrap();
        entry.frozen_request["routingToken"] = Value::Null;
        entry.frozen_request["activeGrantId"] = Value::Null;
        entry.frozen_request["beginsRecorded"] = json!(false);
        entry.frozen_request["queuedPrompts"] = json!([head]);
        let mut evaluate = json!({"connection":connection,"settings":settings,"operation_generation":2,
            "request":{"operation":"turn_evaluate","routing_token":"new-token","purpose":"ordinary",
                "intent_fingerprint":fingerprint,"requested_effects":["observe"],"resource_intents":[],
                "idempotency_key":format!("evaluate-{label}")}});
        entry.settlement_evidence = if transfer {
            evaluate["prepared_begin"] = json!({"idempotency_key":format!("begin-{label}"),
                "grant_id":format!("grant-{label}"),"delivery_tokens":[],
                "issued_no_later_than":"2026-01-01T00:00:00Z","phase":"prepared"});
            json!({"operation":"retained_admission_transfer","promptId":entry.prompt_id,
                "intentFingerprint":fingerprint,"evaluate":evaluate,"begin":{
                    "operation":"turn_begin","routing_token":"new-token","grant_id":format!("grant-{label}"),
                    "delivery_tokens":[],"idempotency_key":format!("begin-{label}")}})
        } else {
            json!({"operation":"retained_admission_decision","promptId":entry.prompt_id,"evaluate":evaluate,
                "card":{"schemaVersion":1,"stage":"dispatch","assurance":"turn_gated","decision":"refuse",
                    "dispatch":"withheld","latencyMs":{"total":0},"repairArmed":false,"failMode":"enforced"}})
        };
        entry
    }

    #[test]
    fn confirmation_never_resolves_an_empty_or_unknown_original_grant() {
        for grant in ["", "unknown"] {
            let raw = serde_json::to_value(ledger(vec![terminal(grant)])).unwrap();
            assert!(serde_json::from_value::<EngramSessionObligationLedger>(raw.clone()).is_err());
            assert_eq!(raw["entries"][0]["grantId"], grant);
        }
    }

    #[test]
    fn terminal_evidence_requires_the_exact_receipt_and_confirmed_phase() {
        let original = terminal("grant");
        ledger(vec![original.clone()]).validate().unwrap();
        for changed in ["grant", "cursor", "decision", "authority", "phase", "extra-duty"] {
            let mut entry = original.clone();
            match changed {
                "grant" => entry.settlement_evidence["response"]["receipt"]["grant_id"] = json!("other"),
                "cursor" => entry.settlement_evidence["response"]["receipt"]["cursor"] = json!("7"),
                "decision" => entry.settlement_evidence["response"]["decision"] = json!("refuse"),
                "authority" => entry.authority_confirmed = false,
                "phase" => entry.phase = EngramSessionObligationPhase::Sent,
                "extra-duty" => entry.frozen_request["bindInProgress"] = json!(true),
                _ => unreachable!(),
            }
            assert!(ledger(vec![entry]).validate().is_err(), "{changed}");
        }
    }

    #[test]
    fn completed_local_transfer_and_decision_cycles_remain_distinct_raw_history() {
        let transfer = local_cycle("transfer", true);
        let decision = local_cycle("decision", false);
        let original = serde_json::to_value(ledger(vec![transfer.clone(), decision])).unwrap();
        let decoded: EngramSessionObligationLedger = serde_json::from_value(original.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), original);
        for changed in ["owner", "prompt", "begin-key", "fingerprint"] {
            let mut entry = transfer.clone();
            match changed {
                "owner" => entry.owner_generation += 1,
                "prompt" => entry.settlement_evidence["promptId"] = json!("other"),
                "begin-key" => entry.settlement_evidence["begin"]["idempotency_key"] = json!("other"),
                "fingerprint" => entry.settlement_evidence["intentFingerprint"] = json!("other"),
                _ => unreachable!(),
            }
            assert!(ledger(vec![entry]).validate().is_err(), "{changed}");
        }
    }

    #[test]
    fn competing_producer_token_and_request_keys_are_rejected() {
        let mut first = terminal("grant");
        first.basis = EngramSessionObligationBasis::CheckpointUnsettled;
        first.frozen_request = json!({"operation":"turn_checkpoint","routing_token":"token","grant_id":"grant",
            "next_intent":"wait","idempotency_key":"key"});
        let mut second = first.clone();
        second.id = "second".into();
        second.frozen_request["idempotency_key"] = json!("different-key");
        assert!(ledger(vec![first.clone(), second.clone()]).validate().is_err(), "same producer duty");
        second.external_ref = "other-producer".into();
        assert!(ledger(vec![first.clone(), second.clone()]).validate().is_err(), "same token duty");
        second.grant_id = "other-grant".into();
        second.frozen_request["grant_id"] = json!("other-grant");
        second.settlement_evidence["response"]["receipt"]["grant_id"] = json!("other-grant");
        ledger(vec![first.clone(), second.clone()]).validate().unwrap();
        second.frozen_request["idempotency_key"] = json!("key");
        assert!(ledger(vec![first, second]).validate().is_err(), "same wire key with different grant");
    }

    #[test]
    fn shared_transfer_keys_require_the_exact_recorded_successor() {
        let history = local_cycle("transfer", true);
        let mut successor = history.clone();
        successor.id = "successor".into();
        successor.owner_generation += 1;
        successor.phase = EngramSessionObligationPhase::Intended;
        successor.routing_token = "new-token".into();
        successor.grant_id = "grant-transfer".into();
        successor.frozen_request["routingToken"] = json!(successor.routing_token);
        successor.frozen_request["activeGrantId"] = json!(successor.grant_id);
        successor.frozen_request["beginsRecorded"] = json!(true);
        successor.frozen_request["dispatchGeneration"] = json!(history.owner_generation);
        successor.frozen_request["queuedPrompts"][0].as_object_mut().unwrap().remove("engram_bind");
        successor.frozen_request["queuedPrompts"][0]["engram_evaluate"] = history.settlement_evidence["evaluate"].clone();
        successor.settlement_evidence = Value::Null;
        ledger(vec![history.clone(), successor.clone()]).validate().unwrap();
        successor.frozen_request["queuedPrompts"][0]["engram_evaluate"]["prepared_begin"]["idempotency_key"] = json!("other");
        assert!(ledger(vec![history, successor]).validate().is_err());
    }
}
