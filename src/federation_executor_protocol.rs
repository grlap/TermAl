// Owns bounded application wake envelopes, not transport or binding installation.
// Link incarnations authorize an invocation; they are never application keys.

const FEDERATION_EXECUTOR_WAKE_ROUTE: &str = "/federation/v1/executor/wake";

// Only display copies are normalized. Mailbox/message/sender identifiers stay
// exact, and reads at the central host return the complete original metadata.
// One total function strips controls and truncates UTF-8 at a char boundary;
// the digest covers these deterministic copies, not the original display text.
fn federation_executor_display_copy(value: &str, limit: usize) -> String {
    let mut display = String::new();
    for character in value.chars().filter(|character| !character.is_control()) {
        if display.len() + character.len_utf8() > limit { break; }
        display.push(character);
    }
    display
}

impl FederationExecutorWake {
    fn with_display_copies(mut self) -> Self {
        self.sender_name = federation_executor_display_copy(&self.sender_name, 512);
        if self.sender_name.trim().is_empty() { self.sender_name = "Session".to_owned(); }
        self.topic = self.topic.map(|topic| federation_executor_display_copy(&topic, 2048));
        self
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationExecutorWake {
    mailbox_id: String,
    message_id: String,
    sequence: u64,
    unread_count: u64,
    sender_session_id: String,
    sender_name: String,
    topic: Option<String>,
}

impl FederationExecutorWake {
    fn validate(&self) -> Result<(), ApiError> {
        for (text, bound) in [(&self.mailbox_id, 256), (&self.message_id, 256),
            (&self.sender_session_id, 256), (&self.sender_name, 1024)]
        {
            if text.trim().is_empty() || text.len() > bound || text.contains('\0') {
                return Err(ApiError::bad_request("invalid remote mailbox wake metadata"));
            }
        }
        if self.sequence == 0 || self.unread_count == 0
            || self.sequence > i64::MAX as u64 || self.unread_count > i64::MAX as u64
            || self.topic.as_ref().is_some_and(|topic| topic.len() > 4096 || topic.contains('\0'))
        {
            return Err(ApiError::bad_request("invalid remote mailbox wake boundary"));
        }
        Ok(())
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationExecutorWakeRequest {
    protocol_version: u16,
    operation_key: String,
    target: FederationMailboxBinding,
    wake: FederationExecutorWake,
}

impl FederationExecutorWakeRequest {
    fn operation(&self, enrollment_generation: u64) -> Result<FederationExecutorOperation, ApiError> {
        self.target.validate()?;
        self.wake.validate()?;
        if self.protocol_version != FEDERATION_PROTOCOL_VERSION
            || self.operation_key != format!("mailbox-wake:{}", self.wake.message_id)
        {
            return Err(ApiError::bad_request("invalid remote wake protocol or operation key"));
        }
        // Value serialization is canonical for this small metadata envelope.
        // The mailbox body stays at the central host.
        let payload = serde_json::to_value(&self.wake)
            .and_then(|wake| serde_json::to_string(&wake))
            .map_err(|_| ApiError::bad_request("invalid remote wake encoding"))?;
        let operation = FederationExecutorOperation {
            central_host_id: self.target.central.host_id.clone(),
            central_epoch: self.target.central.host_generation,
            operation_key: self.operation_key.clone(),
            target_host_id: self.target.identity.host_id.clone(),
            enrollment_generation,
            session_id: self.target.identity.session_id.clone(),
            creation_id: self.target.identity.creation_id.clone(),
            placement_generation: self.target.placement_generation,
            command_kind: "wake".into(), payload,
        };
        operation.validate().map_err(mailbox_api_error)?;
        let receipt = FederationExecutorReceipt { protocol_version: FEDERATION_PROTOCOL_VERSION,
            payload_digest: operation.digest(), operation: operation.clone(), received: true };
        let fits = |bytes: serde_json::Result<Vec<u8>>| bytes.is_ok_and(|bytes| bytes.len() <= 32 * 1024);
        if !fits(serde_json::to_vec(self)) || !fits(serde_json::to_vec(&receipt)) {
            return Err(ApiError::bad_request("remote wake request or receipt exceeds the wire bound"));
        }
        Ok(operation)
    }
}

impl FederationExecutorOperation {
    fn mailbox_binding(&self) -> FederationMailboxBinding {
        FederationMailboxBinding {
            central: FederationHostIdentity {
                host_id: self.central_host_id.clone(), host_generation: self.central_epoch,
            },
            identity: FederationSessionIdentity {
                host_id: self.target_host_id.clone(), session_id: self.session_id.clone(),
                creation_id: self.creation_id.clone(),
            },
            placement_generation: self.placement_generation,
        }
    }

    fn queue_identity(&self) -> String {
        let identity = serde_json::to_vec(&(&self.central_host_id, &self.operation_key))
            .expect("operation key serializes");
        format!("federation-wake-{}", sha256_hex(&identity))
    }
}
