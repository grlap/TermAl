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
    /// This invocation committed no changes; earlier same-key attempts may have.
    NotApplied,
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
    /// The control transport proved this request never reached Engram
    /// (`EngramTransportError::request_never_sent`). It tells a request the
    /// transport could not hand over, whose possibly begun Begin keeps its
    /// exact replay, from a host-local hold that never handed it over
    /// (`EngramTransportError::stopped_before_transport`). Saved with the
    /// cause, so a restored card keeps the distinction.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    transport_unsent: bool,
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
    #[cfg(test)]
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
            #[cfg(test)]
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
            remote_application: if error.invocation_application()
                == EngramInvocationApplication::CertifiedNotApplied
            {
                EngramRemoteApplication::NotApplied
            } else if error.process_never_started || error.request_never_sent {
                // `request_never_sent` proves only that this request did not
                // reach Engram; control availability above stays tied to a
                // process that never started.
                EngramRemoteApplication::NotStarted
            } else {
                EngramRemoteApplication::Unknown
            },
            continuation_reason:
                "The Engram work-service request did not finish; its original facts remain retained.".to_owned(),
            continuation_id: None,
            turn_generation: None,
            authority_fingerprint: None,
            control_process: error
                .control_process
                .as_ref()
                .map(EngramControlProcessObservation::from)
                .unwrap_or_default(),
            transport_unsent: error.request_never_sent,
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
        let mut cause = Self::error(
            error,
            wire["operation"].as_str().unwrap_or("unknown"),
            wire["idempotency_key"].as_str(),
            &[],
        );
        // An unsent protocol failure (an oversize or unencodable request) had
        // no response to validate; it keeps the request boundary.
        if error.causal_failure.is_none()
            && error.kind == EngramTransportErrorKind::Protocol
            && !error.request_never_sent
        {
            cause.boundary = "Engram control response validation".to_owned();
        }
        cause
    }

}
// Historical details that never acquired a durable publication proof stay
// hidden. No retired admission or card-publication operation is resumed.
fn engram_causal_unpublished(cause: &Option<EngramCausalFailure>) -> bool {
    cause.as_ref().is_none_or(|cause| cause.publication_pending)
}
