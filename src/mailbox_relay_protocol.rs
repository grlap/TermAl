// Native cross-host routine-mail identities and immutable conversation ownership.
// Does not enroll sessions, dispatch prompts, perform I/O, or store message bodies.

use super::*;

pub(crate) const MAILBOX_RELAY_VERSION: u16 = 1;
pub(crate) const MAILBOX_RELAY_RPC_ROUTE: &str = "/federation/v1/mailbox/relay";

// Reconnection itself advances transport_generation. The logical request stays
// pinned to enrollment material, while every attempt separately requires its
// exact current authenticated lease. Never weaken the shared transport check.
pub(crate) fn same_mailbox_relay_enrollment(
    a: &FederationPeerSnapshot,
    b: &FederationPeerSnapshot,
) -> bool {
    a.validate().is_ok()
        && b.validate().is_ok()
        && a.local == b.local
        && a.local_role == b.local_role
        && a.peer == b.peer
        && a.central == b.central
        && a.enrollment_generation == b.enrollment_generation
        && a.pair_capability_ref == b.pair_capability_ref
        && a.endpoint == b.endpoint
}

pub(crate) fn valid_mailbox_relay_id(value: &str) -> bool {
    value
        .strip_prefix("mailbox-")
        .is_some_and(mailbox_relay_uuid)
}

pub(crate) fn valid_mailbox_relay_name(value: &str) -> bool {
    !value.is_empty() && value.chars().count() <= 160 && !value.chars().any(char::is_control)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MailboxRelayPrincipal {
    pub(crate) host: FederationHostIdentity,
    pub(crate) store_id: String,
    pub(crate) native_session_id: String,
    pub(crate) native_creation_id: String,
}

pub(crate) fn mailbox_relay_uuid(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
}

impl MailboxRelayPrincipal {
    pub(crate) fn validate(&self) -> Result<(), ApiError> {
        self.host.validate()?;
        validate_federation_reference(&self.native_session_id)?;
        if !mailbox_relay_uuid(&self.native_creation_id) || !mailbox_relay_uuid(&self.store_id) {
            return Err(ApiError::bad_request(
                "invalid native mailbox creation identity",
            ));
        }
        Ok(())
    }

    pub(crate) fn participant_key(&self) -> String {
        format!(
            "relay-principal-{}",
            sha256_hex(&serde_json::to_vec(self).expect("native mailbox identity serializes"),)
        )
    }

    // The authenticated originating host attests its own native caller. A
    // locator, display name, or request body cannot stand in for that channel.
    pub(crate) fn require_origin(
        &self,
        authenticated_host: &FederationHostIdentity,
    ) -> Result<(), ApiError> {
        self.validate()?;
        if &self.host != authenticated_host {
            return Err(ApiError::conflict("mailbox relay origin identity changed"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MailboxRelayHome {
    pub(crate) host: FederationHostIdentity,
    pub(crate) store_id: String,
}

impl MailboxRelayHome {
    pub(crate) fn validate(&self) -> Result<(), ApiError> {
        self.host.validate()?;
        if !mailbox_relay_uuid(&self.store_id) {
            return Err(ApiError::bad_request("invalid mailbox home store identity"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MailboxRelayConversation {
    pub(crate) version: u16,
    pub(crate) home: MailboxRelayHome,
    pub(crate) participants: [MailboxRelayPrincipal; 2],
}

impl MailboxRelayConversation {
    pub(crate) fn new(
        home: MailboxRelayHome,
        a: MailboxRelayPrincipal,
        b: MailboxRelayPrincipal,
    ) -> Result<Self, ApiError> {
        let mut participants = [a, b];
        participants.sort_by_key(MailboxRelayPrincipal::participant_key);
        let conversation = Self {
            version: MAILBOX_RELAY_VERSION,
            home,
            participants,
        };
        conversation.validate()?;
        Ok(conversation)
    }

    pub(crate) fn validate(&self) -> Result<(), ApiError> {
        self.home.validate()?;
        for participant in &self.participants {
            participant.validate()?;
        }
        if self.version != MAILBOX_RELAY_VERSION {
            return Err(ApiError::conflict("mailbox relay upgrade required"));
        }
        if self.participants[0].host.host_id == self.participants[1].host.host_id
            || !self
                .participants
                .iter()
                .any(|p| p.host == self.home.host && p.store_id == self.home.store_id)
            || self.participants[0].participant_key() >= self.participants[1].participant_key()
        {
            return Err(ApiError::bad_request(
                "invalid cross-host mailbox conversation",
            ));
        }
        Ok(())
    }

    pub(crate) fn require_home(&self, central: &MailboxRelayHome) -> Result<(), ApiError> {
        self.validate()?;
        if &self.home != central {
            return Err(ApiError::conflict(
                "mailbox relay home changed; reconcile the original request",
            ));
        }
        Ok(())
    }

    pub(crate) fn require_member(&self, actor: &MailboxRelayPrincipal) -> Result<(), ApiError> {
        self.validate()?;
        actor.validate()?;
        if !self.participants.contains(actor) {
            return Err(ApiError::not_found("mailbox relay membership not found"));
        }
        Ok(())
    }

    pub(crate) fn pair_key(&self) -> String {
        sha256_hex(&serde_json::to_vec(self).expect("mailbox conversation serializes"))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MailboxRelayWake {
    pub(crate) version: u16,
    pub(crate) conversation: MailboxRelayConversation,
    pub(crate) sender: MailboxRelayPrincipal,
    pub(crate) target: MailboxRelayPrincipal,
    pub(crate) mailbox_id: String,
    pub(crate) message_id: String,
    pub(crate) sequence: u64,
    pub(crate) unread_count: u64,
    pub(crate) sender_name: String,
    pub(crate) topic: Option<String>,
}

impl MailboxRelayWake {
    pub(crate) fn validate(&self) -> Result<(), ApiError> {
        self.conversation.require_member(&self.sender)?;
        self.conversation.require_member(&self.target)?;
        if self.version != MAILBOX_RELAY_VERSION
            || self.sender == self.target
            || !valid_mailbox_relay_id(&self.mailbox_id)
            || !self
                .message_id
                .strip_prefix("mailbox-message-")
                .is_some_and(mailbox_relay_uuid)
            || self.sequence == 0
            || !valid_mailbox_relay_name(&self.sender_name)
            || self
                .topic
                .as_ref()
                .is_some_and(|topic| topic.len() > MAX_MAILBOX_METADATA_BYTES)
        {
            return Err(ApiError::bad_request("invalid native mailbox relay wake"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MailboxRelayAppend {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) test_request: Option<MailboxTestPayload>,
    pub(crate) requested_target: String,
    pub(crate) conversation: MailboxRelayConversation,
    pub(crate) sender: MailboxRelayPrincipal,
    pub(crate) target: MailboxRelayPrincipal,
    pub(crate) sender_name: String,
    pub(crate) target_name: String,
    pub(crate) message: String,
    pub(crate) idempotency_key: String,
    pub(crate) topic: Option<String>,
    pub(crate) state_stamp: Option<String>,
}

impl MailboxRelayAppend {
    pub(crate) fn input(&self) -> Result<MailboxAppendInput, ApiError> {
        if let Some(payload) = &self.test_request {
            validate_mailbox_test_payload(payload)?;
            if payload.requester != self.sender {
                return Err(ApiError::conflict(
                    "invalid_request: test-request sender does not match authenticated append",
                ));
            }
            if payload.recipient.as_ref() != Some(&self.target) {
                return Err(ApiError::conflict(
                    "invalid_request: test-request recipient does not match append",
                ));
            }
            if payload.request_send_key != self.idempotency_key {
                return Err(ApiError::conflict(
                    "invalid_request: test-request send key differs from append",
                ));
            }
        }
        self.conversation.require_member(&self.sender)?;
        self.conversation.require_member(&self.target)?;
        if self.requested_target.trim().is_empty()
            || self.requested_target.len() > MAX_MAILBOX_METADATA_BYTES
            || self.sender == self.target
            || !valid_mailbox_relay_name(&self.sender_name)
            || !valid_mailbox_relay_name(&self.target_name)
        {
            return Err(ApiError::bad_request(
                "invalid native mailbox relay participants",
            ));
        }
        if reserved_delegation_review_message(self.topic.as_deref(), &self.idempotency_key) {
            return Err(ApiError::bad_request(
                "the review-result topic is reserved for internal delegation results, not routine mail",
            ));
        }
        let input = MailboxAppendInput {
            sender_session_id: self.sender.participant_key(),
            sender_name: self.sender_name.clone(),
            target_session_id: self.target.participant_key(),
            target_name: self.target_name.clone(),
            body: self.message.clone(),
            idempotency_key: self.idempotency_key.clone(),
            topic: self.topic.clone(),
            state_stamp: self.state_stamp.clone(),
        };
        validate_mailbox_append_input(&input).map_err(mailbox_api_error)?;
        Ok(input)
    }

    pub(crate) fn digest(&self) -> Result<String, ApiError> {
        self.input()?;
        // Display names can refresh without changing a previously admitted send.
        let mut value = json!({
            "version": MAILBOX_RELAY_VERSION, "conversation": self.conversation,
            "sender": self.sender, "target": self.target, "message": self.message,
            "requestedTarget": self.requested_target,
            "idempotencyKey": self.idempotency_key, "topic": self.topic, "stateStamp": self.state_stamp,
        });
        // The optional key changes selected sends only. Legacy None retains
        // its exact digest input, including the historical null fields.
        if let Some(payload) = &self.test_request {
            value["testRequest"] =
                serde_json::to_value(payload).expect("native test-request custody serializes");
        }
        Ok(sha256_hex(
            &serde_json::to_vec(&value).expect("native relay request serializes"),
        ))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MailboxRelayDescriptor {
    pub(crate) principal: MailboxRelayPrincipal,
    pub(crate) name: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MailboxRelaySummary {
    pub(crate) conversation: MailboxRelayConversation,
    pub(crate) summary: MailboxSummary,
}

impl MailboxRelaySummary {
    pub(crate) fn participant_names(&self) -> [String; 2] {
        self.conversation.participants.clone().map(|principal| {
            self.summary
                .participants
                .iter()
                .find(|participant| participant.session_id == principal.participant_key())
                .map(|participant| participant.display_name.clone())
                .unwrap_or_else(|| principal.native_session_id.clone())
        })
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MailboxRelayRequest {
    pub(crate) version: u16,
    pub(crate) request_id: String,
    pub(crate) operation: MailboxRelayOperation,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(
    tag = "operation",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(crate) enum MailboxRelayOperation {
    /// Scheduling hint only; original authenticated completion must be pulled.
    CausalTestCompletionReady {
        hint: CausalTestCompletionReadyHint,
    },
    /// Original typed request only; no observer adoption or verification credit.
    ReadCausalTestCompletion {
        selector: CausalTestCompletionSelector,
    },
    /// Native observed completion only; no remote verification admission.
    ObserveTestRun {
        actor: MailboxRelayPrincipal,
        project_scope: String,
        run_id: String,
    },
    Describe {
        native_session_id: String,
    },
    Send {
        append: MailboxRelayAppend,
    },
    List {
        actor: MailboxRelayPrincipal,
        home: MailboxRelayHome,
    },
    Read {
        actor: MailboxRelayPrincipal,
        conversation: MailboxRelayConversation,
        mailbox_id: String,
        after_sequence: Option<u64>,
        limit: u64,
        issue_receipt: bool,
    },
    ReadMessage {
        actor: MailboxRelayPrincipal,
        home: MailboxRelayHome,
        message_id: String,
    },
    Acknowledge {
        actor: MailboxRelayPrincipal,
        conversation: MailboxRelayConversation,
        mailbox_id: String,
        receipt: String,
    },
    Wake {
        wake: MailboxRelayWake,
    },
    InspectWake {
        actor: MailboxRelayPrincipal,
        conversation: MailboxRelayConversation,
        target: MailboxRelayPrincipal,
        mailbox_id: String,
        sequence: u64,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MailboxRelayResponse {
    pub(crate) version: u16,
    pub(crate) request_id: String,
    pub(crate) host: FederationHostIdentity,
    pub(crate) value: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MailboxRelayLocator {
    pub(crate) version: u16,
    pub(crate) home: MailboxRelayHome,
    pub(crate) id: String,
}

impl MailboxRelayLocator {
    pub(crate) fn encode(
        home: &MailboxRelayHome,
        id: &str,
        message: bool,
    ) -> Result<String, ApiError> {
        use base64::Engine;
        let locator = Self {
            version: MAILBOX_RELAY_VERSION,
            home: home.clone(),
            id: id.to_owned(),
        };
        locator.validate(message)?;
        let prefix = if message {
            "relay-message-"
        } else {
            "relay-mailbox-"
        };
        Ok(format!(
            "{prefix}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&locator).expect("relay locator serializes"))
        ))
    }

    pub(crate) fn decode(value: &str, message: bool) -> Result<Option<Self>, ApiError> {
        use base64::Engine;
        let prefix = if message {
            "relay-message-"
        } else {
            "relay-mailbox-"
        };
        let Some(encoded) = value.strip_prefix(prefix) else {
            return Ok(None);
        };
        if encoded.len() > 1024 {
            return Err(ApiError::bad_request("invalid mailbox relay locator"));
        }
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| ApiError::bad_request("invalid mailbox relay locator"))?;
        let locator: Self = serde_json::from_slice(&bytes)
            .map_err(|_| ApiError::bad_request("invalid mailbox relay locator"))?;
        locator.validate(message)?;
        if Self::encode(&locator.home, &locator.id, message)? != value {
            return Err(ApiError::bad_request("noncanonical mailbox relay locator"));
        }
        Ok(Some(locator))
    }

    pub(crate) fn validate(&self, message: bool) -> Result<(), ApiError> {
        self.home.validate()?;
        let valid = if message {
            self.id
                .strip_prefix("mailbox-message-")
                .is_some_and(mailbox_relay_uuid)
        } else {
            valid_mailbox_relay_id(&self.id)
        };
        if self.version != MAILBOX_RELAY_VERSION || !valid {
            return Err(ApiError::bad_request(
                "unsupported or invalid mailbox relay locator",
            ));
        }
        Ok(())
    }
}
