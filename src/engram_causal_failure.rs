// Causal diagnostics carried by the existing control card and transcript.
// Owns redaction and attribution, not retry policy or authority settlement.
// Extracted alongside the failure boundaries in engram_host_adapter.rs.

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramCausalFailureClass {
    LocalState,
    Deadline,
    Transport,
    Protocol,
    Remote,
    Backoff,
    ProducerRefusal,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramRemoteApplication {
    Unknown,
    NotStarted,
    Refused,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramCausalFailure {
    // Visibility is process-local; only the persistence clone clears this
    // before the exact message is acknowledged by the durable writer.
    #[serde(skip)]
    publication_pending: bool,
    operation: String,
    failure_class: EngramCausalFailureClass,
    original_code: Option<String>,
    message: String,
    boundary: String,
    attempt_id: Option<String>,
    remote_application: EngramRemoteApplication,
    continuation_reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    continuation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    turn_generation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    authority_fingerprint: Option<String>,
    #[serde(default)]
    control_process: EngramControlProcessObservation,
}

/// The control sidecar a failure was observed on. Explicitly unavailable when
/// the transport captured none, including every record saved before this.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramControlProcessObservation {
    identity: String,
    exit_state: String,
}

impl Default for EngramControlProcessObservation {
    fn default() -> Self {
        Self {
            identity: "unavailable".to_owned(),
            exit_state: "unavailable".to_owned(),
        }
    }
}

impl From<&EngramControlProcessEvidence> for EngramControlProcessObservation {
    fn from(evidence: &EngramControlProcessEvidence) -> Self {
        Self {
            identity: format!("pid {}", evidence.pid),
            exit_state: evidence.exit_state.clone(),
        }
    }
}

// Redact before persistence, not just during rendering. Known request secrets
// are removed exactly; credential-shaped suffixes are deliberately withheld
// rather than guessing the extent of a quoted/multiline credential value.
fn engram_causal_text(text: &str, secrets: &[&str]) -> String {
    let mut safe = text.to_owned();
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
        safe = safe.replace(secret, "[redacted]");
    }
    let lower = safe.to_ascii_lowercase();
    let token_start = lower
        .match_indices("token")
        .filter_map(|(start, _)| {
            let tail = lower[start + "token".len()..]
                .trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '"' || c == '\'');
            (tail.starts_with('=') || tail.starts_with(':')).then_some(start)
        })
        .min();
    let secret_start = [
        "bearer ",
        "bearer\t",
        "password",
        "routing_token",
        "routingtoken",
        "delivery_token",
        "deliverytoken",
        "api_key",
        "apikey",
        "authorization",
        "secret",
    ]
    .iter()
    .filter_map(|marker| lower.find(marker))
    .chain(token_start)
    .min();
    if let Some(start) = secret_start {
        safe.truncate(start);
        safe.push_str("[credential details redacted]");
    }
    let mut bounded = String::new();
    for character in safe.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if bounded.len() + character.len_utf8() > 1000 {
            bounded.push_str(" [truncated]");
            break;
        }
        bounded.push(character);
    }
    bounded
}

impl EngramCausalFailure {
    fn unsent_request(
        error: &EngramTransportError,
        request: &EngramControlRequest,
        boundary: &str,
    ) -> Self {
        let mut cause = Self::request(error, request);
        if error.causal_failure.is_none() {
            cause.boundary = boundary.to_owned();
            cause.remote_application = EngramRemoteApplication::NotStarted;
        }
        cause
    }

    fn local(error: &EngramTransportError, operation: &str, boundary: &str) -> Self {
        let mut cause = Self::error(error, operation, None, &[]);
        if error.causal_failure.is_none() {
            cause.boundary = boundary.to_owned();
            // This preparation did not send its request; a prior replay may
            // still have applied. This is not permission to retry it.
            cause.remote_application = EngramRemoteApplication::NotStarted;
        }
        cause
    }

    fn local_hold(operation: &str, code: &str, detail: &str, boundary: &str) -> Self {
        let mut error = if code == "dispatch_budget_exhausted" {
            EngramTransportError::deadline(detail)
        } else {
            EngramTransportError::local_state(detail)
        };
        error.code = Some(code.to_owned());
        Self::local(&error, operation, boundary)
    }

    fn error(
        error: &EngramTransportError,
        operation: &str,
        attempt: Option<&str>,
        secrets: &[&str],
    ) -> Self {
        if let Some(cause) = &error.causal_failure {
            return cause.clone();
        }
        let failure_class = match error.kind {
            EngramTransportErrorKind::LocalState => EngramCausalFailureClass::LocalState,
            EngramTransportErrorKind::Deadline => EngramCausalFailureClass::Deadline,
            EngramTransportErrorKind::Transport => EngramCausalFailureClass::Transport,
            EngramTransportErrorKind::Protocol => EngramCausalFailureClass::Protocol,
            EngramTransportErrorKind::Remote => EngramCausalFailureClass::Remote,
            EngramTransportErrorKind::Backoff => EngramCausalFailureClass::Backoff,
        };
        Self {
            publication_pending: false,
            operation: operation.to_owned(),
            failure_class,
            original_code: error
                .code
                .as_deref()
                .map(|code| engram_causal_text(code, secrets)),
            message: engram_causal_text(&error.message, secrets),
            boundary: "TermAl host → Engram control".to_owned(),
            attempt_id: attempt.map(|key| engram_causal_text(key, secrets)),
            remote_application: if error.process_never_started {
                EngramRemoteApplication::NotStarted
            } else {
                EngramRemoteApplication::Unknown
            },
            continuation_reason:
                "Provider handoff withheld; owning control operation is not settled.".to_owned(),
            continuation_id: None,
            turn_generation: None,
            authority_fingerprint: None,
            control_process: error
                .control_process
                .as_ref()
                .map(EngramControlProcessObservation::from)
                .unwrap_or_default(),
        }
    }

    fn request(error: &EngramTransportError, request: &EngramControlRequest) -> Self {
        // This temporary value is never persisted. Do not attach a raw request
        // (which can contain prompts, reports or capabilities) to diagnostics.
        let Ok(wire) = serde_json::to_value(request) else {
            return Self::error(
                &EngramTransportError::protocol("Causal request metadata unavailable"),
                "unknown",
                None,
                &[],
            );
        };
        let mut secrets = Vec::new();
        if let Some(token) = wire["routing_token"].as_str() {
            secrets.push(token);
        }
        if let Some(tokens) = wire["delivery_tokens"].as_array() {
            secrets.extend(tokens.iter().filter_map(Value::as_str));
        }
        let mut cause = Self::error(
            error,
            wire["operation"].as_str().unwrap_or("unknown"),
            wire["idempotency_key"].as_str(),
            &secrets,
        );
        if error.causal_failure.is_none() && error.kind == EngramTransportErrorKind::Protocol {
            cause.boundary = "Engram control response validation".to_owned();
        }
        cause
    }

    fn request_refusal(request: &EngramControlRequest, code: &str) -> Self {
        let mut error = EngramTransportError::local_state(
            "Engram explicitly refused this request; prior application is not determined",
        );
        error.code = Some(code.to_owned());
        let mut cause = Self::request(&error, request);
        cause.failure_class = EngramCausalFailureClass::ProducerRefusal;
        cause.remote_application = EngramRemoteApplication::Refused;
        cause.continuation_reason =
            "Provider handoff withheld by explicit producer refusal.".to_owned();
        cause
    }
}

// Called only from binding's existing backoff branch. A checkpoint failure
// can explain that hold only for the exact still-open grant and queued head.
// The newest checkpoint wins, so a later success cannot resurrect old failure.
fn engram_checkpoint_cause_for_owner(
    record: &SessionRecord,
    target: &EngramBindingTarget,
    owner: &EngramQueuedAdmissionOwner,
) -> Option<EngramCausalFailure> {
    if !owner.matches(record)
        || record.runtime_stop_in_progress
        || record.engram.project_reset_in_progress
    {
        return None;
    }
    let grant = record.engram.active_grant_id.as_deref()?;
    let card = record
        .session
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::EngramControl { card, .. }
                if card.stage == EngramControlStage::Checkpoint
                    && card.grant_id.as_deref() == Some(grant) =>
            {
                Some(card)
            }
            _ => None,
        })?;
    let cause = card.causal_failure.as_ref()?;
    if card.decision != EngramControlCardDecision::Degraded
        || cause.operation != "turn_checkpoint"
        || cause.continuation_id.as_deref() != Some(owner.prompt_id.as_str())
        || cause.turn_generation != Some(owner.active_turn_generation)
        || cause.authority_fingerprint.as_deref() != Some(engram_abort_authority(target).as_str())
    {
        return None;
    }
    let mut cause = cause.clone();
    cause.continuation_reason = "Admission held by binding backoff after this unsettled closing checkpoint; no new Evaluate was sent.".to_owned();
    Some(cause)
}
