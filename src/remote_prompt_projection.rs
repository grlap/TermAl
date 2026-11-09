// Owns bounded decoding of projections from the existing PromptDelivery owner.
// Does not synthesize provider facts or manage lifecycle. Recovered from projection ABI3.
// Bounded read-only projections of the existing executor/PromptDelivery owner.
// Decoding these values supplies no permission, runtime event or state change.
const REMOTE_PROMPT_STATUS_BYTES: usize = 16_384;

#[cfg_attr(not(test), allow(dead_code))]
fn remote_prompt_projection_uuid<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Uuid, D::Error> {
    let value = String::deserialize(deserializer)?;
    Uuid::parse_str(&value)
        .ok()
        .filter(|id| !id.is_nil() && id.to_string() == value)
        .ok_or_else(|| serde::de::Error::custom("canonical nonnil UUID required"))
}

#[cfg_attr(not(test), allow(dead_code))]
fn remote_prompt_projection_optional<'de, D, T>(
    deserializer: D,
) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(not(test), allow(dead_code))]
struct RemotePromptStatus {
    protocol_version: u16,
    #[serde(deserialize_with = "remote_prompt_projection_uuid")]
    operation_id: Uuid,
    intent_digest: String,
    duplicate: bool,
    local_intent_durable: bool,
    #[serde(deserialize_with = "remote_prompt_projection_optional")]
    receiver: Option<RemotePromptReceiverStatus>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(not(test), allow(dead_code))]
struct RemotePromptReceiverStatus {
    received: bool,
    #[serde(deserialize_with = "remote_prompt_projection_optional")]
    queue_entry_id: Option<String>,
    #[serde(deserialize_with = "remote_prompt_projection_optional")]
    queue_position: Option<u64>,
    root_paused: bool,
    root_stopping: bool,
    cancelled_unstarted: bool,
    #[serde(deserialize_with = "remote_prompt_projection_optional")]
    delivery: Option<RemotePromptDeliveryStatus>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(not(test), allow(dead_code))]
struct RemotePromptDeliveryStatus {
    disposition: PromptDeliveryDisposition,
    attempt: u32,
    #[serde(deserialize_with = "remote_prompt_projection_uuid")]
    write_ahead: Uuid,
    #[serde(deserialize_with = "remote_prompt_projection_optional")]
    turn_generation: Option<u64>,
    #[serde(deserialize_with = "remote_prompt_projection_optional")]
    accepted: Option<RemotePromptCorrelatedObservation>,
    #[serde(deserialize_with = "remote_prompt_projection_optional")]
    completed: Option<RemotePromptCorrelatedObservation>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(not(test), allow(dead_code))]
enum RemotePromptObservationKind {
    CodexTurnStartResponse,
    ClaudeOwnedCommand,
    AcpPromptResponse,
    NativeTurnCompleted,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(not(test), allow(dead_code))]
struct RemotePromptCorrelatedObservation {
    kind: RemotePromptObservationKind,
    prompt_id: String,
    attempt: u32,
    #[serde(deserialize_with = "remote_prompt_projection_uuid")]
    write_ahead: Uuid,
    runtime_id: String,
    turn_generation: u64,
    #[serde(deserialize_with = "remote_prompt_projection_optional")]
    correlation: Option<String>,
}

#[cfg_attr(not(test), allow(dead_code))]
fn remote_prompt_projection_error() -> ApiError {
    ApiError::from_status(StatusCode::SERVICE_UNAVAILABLE,
        "direct prompt status is invalid or unavailable; original outcome remains unknown; do not resend automatically")
}

#[cfg_attr(not(test), allow(dead_code))]
fn remote_prompt_projection_string(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

#[cfg_attr(not(test), allow(dead_code))]
impl RemotePromptCorrelatedObservation {
    fn validate(&self, queue: &str, delivery: &RemotePromptDeliveryStatus) -> Result<(), ApiError> {
        if !remote_prompt_projection_string(&self.prompt_id, 256)
            || self.prompt_id != queue
            || !remote_prompt_projection_string(&self.runtime_id, 256)
            || self.attempt == 0
            || self.attempt != delivery.attempt
            || self.write_ahead.is_nil()
            || self.write_ahead != delivery.write_ahead
            || self.turn_generation == 0
            || delivery.turn_generation != Some(self.turn_generation)
            || self
                .correlation
                .as_ref()
                .is_some_and(|c| !remote_prompt_projection_string(c, 1024))
        {
            return Err(remote_prompt_projection_error());
        }
        Ok(())
    }
}

#[cfg_attr(not(test), allow(dead_code))]
impl RemotePromptReceiverStatus {
    fn validate(&self) -> Result<(), ApiError> {
        if !self.received
            || self
                .queue_entry_id
                .as_ref()
                .is_some_and(|id| !remote_prompt_projection_string(id, 256))
            || (self.queue_position.is_some() && self.queue_entry_id.is_none())
            || (self.cancelled_unstarted && self.queue_position.is_some())
        {
            return Err(remote_prompt_projection_error());
        }
        if let Some(delivery) = &self.delivery {
            let queue = self
                .queue_entry_id
                .as_deref()
                .ok_or_else(remote_prompt_projection_error)?;
            if delivery.write_ahead.is_nil()
                || delivery.turn_generation == Some(0)
                || (delivery.attempt == 0
                    && delivery.disposition != PromptDeliveryDisposition::Pending)
                || (self.cancelled_unstarted
                    && matches!(
                        delivery.disposition,
                        PromptDeliveryDisposition::HandingOff
                            | PromptDeliveryDisposition::Accepted
                            | PromptDeliveryDisposition::Ambiguous
                    ))
            {
                return Err(remote_prompt_projection_error());
            }
            for observation in [&delivery.accepted, &delivery.completed]
                .into_iter()
                .flatten()
            {
                observation.validate(queue, delivery)?;
                if delivery.disposition != PromptDeliveryDisposition::Accepted
                    || self.cancelled_unstarted
                {
                    return Err(remote_prompt_projection_error());
                }
            }
            if delivery.accepted.as_ref().is_some_and(|accepted| {
                accepted.kind == RemotePromptObservationKind::NativeTurnCompleted
            }) {
                return Err(remote_prompt_projection_error());
            }
            if let Some(completed) = &delivery.completed {
                if !matches!(
                    completed.kind,
                    RemotePromptObservationKind::NativeTurnCompleted
                        | RemotePromptObservationKind::AcpPromptResponse
                ) {
                    return Err(remote_prompt_projection_error());
                }
                if delivery
                    .accepted
                    .as_ref()
                    .is_some_and(|accepted| accepted.runtime_id != completed.runtime_id)
                {
                    return Err(remote_prompt_projection_error());
                }
            }
        }
        Ok(())
    }
}

#[cfg_attr(not(test), allow(dead_code))]
impl RemotePromptStatus {
    fn validate(&self) -> Result<(), ApiError> {
        if self.protocol_version != 1
            || self.operation_id.is_nil()
            || self.intent_digest.len() != 64
            || !self
                .intent_digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || (!self.local_intent_durable && self.receiver.is_some())
        {
            return Err(remote_prompt_projection_error());
        }
        if let Some(receiver) = &self.receiver {
            receiver.validate()?;
        }
        Ok(())
    }

    fn decode_bounded(bytes: &[u8]) -> Result<Self, ApiError> {
        if bytes.len() > REMOTE_PROMPT_STATUS_BYTES {
            return Err(remote_prompt_projection_error());
        }
        let status: Self =
            serde_json::from_slice(bytes).map_err(|_| remote_prompt_projection_error())?;
        status.validate()?;
        Ok(status)
    }

    // Reconciliation observes a retained source row. JSON null reports no
    // observation; it never establishes remote non-entry or permits a resend.
    fn decode_optional_bounded(bytes: &[u8]) -> Result<Option<Self>, ApiError> {
        if bytes.len() > REMOTE_PROMPT_STATUS_BYTES {
            return Err(remote_prompt_projection_error());
        }
        let status: Option<Self> =
            serde_json::from_slice(bytes).map_err(|_| remote_prompt_projection_error())?;
        if let Some(status) = &status {
            status.validate()?;
        }
        Ok(status)
    }

    fn encode_bounded(&self) -> Result<Vec<u8>, ApiError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| remote_prompt_projection_error())?;
        if bytes.len() > REMOTE_PROMPT_STATUS_BYTES {
            return Err(remote_prompt_projection_error());
        }
        Ok(bytes)
    }

    // The caller obtains these expected values from the retained source row,
    // never a newly resolved reference or a new permission generation.
    fn validate_for_operation(
        &self,
        operation_id: Uuid,
        intent_digest: &str,
        queue_entry_id: &str,
    ) -> Result<(), ApiError> {
        self.validate()?;
        if self.operation_id != operation_id
            || self.intent_digest != intent_digest
            || self
                .receiver
                .as_ref()
                .and_then(|r| r.queue_entry_id.as_deref())
                .is_some_and(|id| id != queue_entry_id)
        {
            return Err(remote_prompt_projection_error());
        }
        Ok(())
    }
}
