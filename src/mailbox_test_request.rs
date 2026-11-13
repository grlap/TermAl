// Closed request and custody shapes beside existing mailbox/registration owners.
// Decoding validates shape only; the host capture owner supplies authority.

const MAILBOX_TEST_REQUEST_MAX_BYTES: usize = 64 * 1024;
const MAILBOX_TEST_REQUEST_MAX_TARGETS: usize = 16;

fn mailbox_test_request_input_schema() -> Value {
    json!({
        "type": "object", "additionalProperties": false,
        "description": "Optional native test request. The host captures target associations; message delivery alone neither starts a test nor grants verification credit.",
        "required": ["mode", "targets"],
        "properties": {
            "mode": {"type": "string", "enum": ["full", "focused"]},
            "targets": {"type": "array", "minItems": 1, "maxItems": MAILBOX_TEST_REQUEST_MAX_TARGETS,
                "items": {"type": "object", "additionalProperties": false,
                    "required": ["workRef", "use"], "properties": {
                        "workRef": {"type": "string", "minLength": 1, "maxLength": 256},
                        "use": {"oneOf": [
                            {"type": "object", "additionalProperties": false,
                                "required": ["kind", "expected_commit", "purpose"], "properties": {
                                    "kind": {"const": "committed_input"},
                                    "expected_commit": {"type": "string", "pattern": "^[0-9a-f]{40}$"},
                                    "purpose": {"type": "string", "enum": ["verification", "landing_gate"]}}},
                            {"type": "object", "additionalProperties": false,
                                "required": ["kind", "ref_name", "expected_tree", "purpose"], "properties": {
                                    "kind": {"const": "authorized_test_ref"},
                                    "ref_name": {"type": "string", "pattern": "^refs/heads/test/"},
                                    "expected_tree": {"type": "string", "pattern": "^[0-9a-f]{40}$"},
                                    "purpose": {"const": "landing_gate"}}}
                        ]}
                    }}}
        }
    })
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum MailboxTestMode {
    Full,
    Focused,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum MailboxTestPurpose {
    Verification,
    LandingGate,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum MailboxTestLandingPurpose {
    LandingGate,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum MailboxTestInputUse {
    CommittedInput {
        expected_commit: String,
        purpose: MailboxTestPurpose,
    },
    AuthorizedTestRef {
        ref_name: String,
        expected_tree: String,
        purpose: MailboxTestLandingPurpose,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MailboxTestTarget {
    work_ref: String,
    r#use: MailboxTestInputUse,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MailboxTestRequest {
    mode: MailboxTestMode,
    targets: Vec<MailboxTestTarget>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(try_from = "MailboxTestAssociationWire")]
struct MailboxTestAssociation {
    binding: EngramControlWorkBinding,
    root: EngramVerificationRecordRoot,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MailboxTestCapturedTarget {
    target: MailboxTestTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    association: Option<MailboxTestAssociation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    uncredited_reason: Option<String>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", try_from = "MailboxTestPayloadWire")]
struct MailboxTestPayload {
    schema_version: u16,
    request_send_key: String,
    requester: MailboxRelayPrincipal,
    // None exists only in the host's in-memory capture draft. Wire decoding
    // and every publication boundary require the resolved native recipient.
    #[serde(skip_serializing_if = "Option::is_none")]
    recipient: Option<MailboxRelayPrincipal>,
    mode: MailboxTestMode,
    targets: Vec<MailboxTestCapturedTarget>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", try_from = "TestRunCausalRequestWire")]
struct TestRunCausalRequest {
    request_message_id: String,
    canonical_message_id: String,
    request_send_key: String,
    runner: MailboxRelayPrincipal,
    payload: MailboxTestPayload,
}

// Local closed projections avoid widening the decoding contract of every
// shared Engram binding/root user merely to close this mailbox ingress.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MailboxTestBindingWire {
    root_execution_id: String,
    work_id: String,
    run_id: String,
    work_revision: i64,
    claim_id: String,
    claim_fence: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MailboxTestRootWire {
    workspace_id: String,
    generation: i64,
    state: EngramVerificationRecordRootState,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MailboxTestAssociationWire {
    binding: MailboxTestBindingWire,
    root: MailboxTestRootWire,
}

impl TryFrom<MailboxTestAssociationWire> for MailboxTestAssociation {
    type Error = String;
    fn try_from(value: MailboxTestAssociationWire) -> std::result::Result<Self, Self::Error> {
        let b = value.binding;
        let root = value.root;
        if [&b.root_execution_id, &b.work_id, &b.run_id, &b.claim_id]
            .into_iter()
            .any(|id| !mailbox_relay_uuid(id))
            || b.work_revision <= 0
            || b.claim_fence <= 0
            || root.generation <= 0
            || root.workspace_id.is_empty()
            || root.workspace_id.len() > 4096
            || root.workspace_id.chars().any(char::is_control)
        {
            return Err("invalid_request: malformed captured association".into());
        }
        Ok(Self {
            binding: EngramControlWorkBinding {
                root_execution_id: b.root_execution_id,
                work_id: b.work_id,
                run_id: b.run_id,
                work_revision: b.work_revision,
                claim_id: b.claim_id,
                claim_fence: b.claim_fence,
            },
            root: EngramVerificationRecordRoot {
                workspace_id: root.workspace_id,
                generation: root.generation,
                state: root.state,
            },
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MailboxTestPayloadWire {
    schema_version: u16,
    request_send_key: String,
    requester: MailboxRelayPrincipal,
    recipient: MailboxRelayPrincipal,
    mode: MailboxTestMode,
    targets: Vec<MailboxTestCapturedTarget>,
}

impl TryFrom<MailboxTestPayloadWire> for MailboxTestPayload {
    type Error = String;
    fn try_from(value: MailboxTestPayloadWire) -> std::result::Result<Self, Self::Error> {
        let payload = Self {
            schema_version: value.schema_version,
            request_send_key: value.request_send_key,
            requester: value.requester,
            recipient: Some(value.recipient),
            mode: value.mode,
            targets: value.targets,
        };
        validate_mailbox_test_payload(&payload).map_err(|error| error.message)?;
        Ok(payload)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TestRunCausalRequestWire {
    request_message_id: String,
    canonical_message_id: String,
    request_send_key: String,
    runner: MailboxRelayPrincipal,
    payload: MailboxTestPayload,
}

impl TryFrom<TestRunCausalRequestWire> for TestRunCausalRequest {
    type Error = String;
    fn try_from(value: TestRunCausalRequestWire) -> std::result::Result<Self, Self::Error> {
        if !mailbox_test_message_selector_valid(&value.request_message_id)
            || !value
                .canonical_message_id
                .strip_prefix("mailbox-message-")
                .is_some_and(mailbox_relay_uuid)
            || value.request_send_key.is_empty()
            || value.request_send_key.len() > 256
        {
            return Err("invalid_request: malformed causal message identity".into());
        }
        value.runner.validate().map_err(|error| error.message)?;
        if value.payload.recipient.as_ref() != Some(&value.runner) {
            return Err("invalid_request: causal runner differs from retained recipient".into());
        }
        if value.request_send_key != value.payload.request_send_key {
            return Err("invalid_request: causal send key differs from retained request".into());
        }
        Ok(Self {
            request_message_id: value.request_message_id,
            canonical_message_id: value.canonical_message_id,
            request_send_key: value.request_send_key,
            runner: value.runner,
            payload: value.payload,
        })
    }
}

fn mailbox_test_message_selector_valid(value: &str) -> bool {
    if let Some(uuid) = value.strip_prefix("mailbox-message-") {
        return Uuid::parse_str(uuid).is_ok_and(|id| id.to_string() == uuid);
    }
    value.strip_prefix("relay-message-").is_some_and(|token| {
        !token.is_empty()
            && token.len() <= 1024
            && token
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
    })
}

fn mailbox_test_hex_id(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn validate_mailbox_test_target(target: &MailboxTestTarget) -> Result<(), ApiError> {
    let invalid = || ApiError::bad_request("invalid_request: malformed test request target");
    if target.work_ref.is_empty()
        || target.work_ref.len() > 256
        || target.work_ref.chars().any(char::is_control)
    {
        return Err(invalid());
    }
    match &target.r#use {
        MailboxTestInputUse::CommittedInput {
            expected_commit, ..
        } => {
            if !mailbox_test_hex_id(expected_commit) {
                return Err(invalid());
            }
        }
        MailboxTestInputUse::AuthorizedTestRef {
            ref_name,
            expected_tree,
            ..
        } => {
            let suffix = ref_name
                .strip_prefix("refs/heads/test/")
                .filter(|suffix| !suffix.is_empty());
            if !mailbox_test_hex_id(expected_tree)
                || !suffix.is_some_and(|suffix| {
                    suffix.len() <= 256
                        && suffix
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
                        && !suffix.contains("..")
                        && !suffix.ends_with('.')
                        && !suffix.ends_with(".lock")
                })
            {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

fn validate_mailbox_test_request(request: &MailboxTestRequest) -> Result<(), ApiError> {
    if request.targets.is_empty() || request.targets.len() > MAILBOX_TEST_REQUEST_MAX_TARGETS {
        return Err(ApiError::bad_request(
            "invalid_request: test request needs 1 to 16 targets",
        ));
    }
    let mut work = BTreeSet::new();
    for target in &request.targets {
        validate_mailbox_test_target(target)?;
        if !work.insert(&target.work_ref) {
            return Err(ApiError::bad_request(
                "invalid_request: duplicate test request target",
            ));
        }
    }
    Ok(())
}

fn deserialize_mailbox_test_request<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<MailboxTestRequest>, D::Error> {
    let request = MailboxTestRequest::deserialize(deserializer)?;
    validate_mailbox_test_request(&request)
        .map_err(|error| serde::de::Error::custom(error.message))?;
    Ok(Some(request))
}

fn validate_mailbox_test_payload(payload: &MailboxTestPayload) -> Result<(), ApiError> {
    validate_mailbox_test_payload_draft(payload)?;
    let recipient = payload.recipient.as_ref().ok_or_else(|| {
        ApiError::bad_request("invalid_request: test-request recipient is not pinned")
    })?;
    recipient.validate()?;
    if recipient == &payload.requester {
        return Err(ApiError::bad_request(
            "invalid_request: test request cannot target its sender",
        ));
    }
    Ok(())
}

fn validate_mailbox_test_payload_draft(payload: &MailboxTestPayload) -> Result<(), ApiError> {
    if payload.schema_version != 1
        || payload.request_send_key.is_empty()
        || payload.request_send_key.len() > 256
        || payload.targets.is_empty()
        || payload.targets.len() > MAILBOX_TEST_REQUEST_MAX_TARGETS
    {
        return Err(ApiError::bad_request(
            "invalid_request: invalid test-request custody schema or targets",
        ));
    }
    payload.requester.validate()?;
    let mut work = BTreeSet::new();
    for captured in &payload.targets {
        validate_mailbox_test_target(&captured.target)?;
        if !work.insert(&captured.target.work_ref) {
            return Err(ApiError::bad_request(
                "invalid_request: duplicate captured test target",
            ));
        }
        match (&captured.association, &captured.uncredited_reason) {
            (Some(association), None) => {
                let work_id = &association.binding.work_id;
                if captured.target.work_ref != *work_id
                    && !work_id
                        .rsplit('-')
                        .next()
                        .is_some_and(|suffix| captured.target.work_ref == format!("w-{suffix}"))
                {
                    return Err(ApiError::bad_request(
                        "invalid_request: captured work identity differs",
                    ));
                }
                // Validate programmatic construction through the same closed
                // projection used on persisted and authenticated relay input.
                serde_json::from_value::<MailboxTestAssociation>(
                    serde_json::to_value(association).map_err(|_| {
                        ApiError::bad_request("invalid_request: association encoding")
                    })?,
                )
                .map_err(|_| ApiError::bad_request("invalid_request: captured association"))?;
            }
            (None, Some(reason)) if !reason.is_empty() && reason.len() <= 2048 => {}
            _ => {
                return Err(ApiError::bad_request(
                    "invalid_request: captured target needs an association or uncredited reason",
                ));
            }
        }
    }
    if serde_json::to_vec(payload)
        .map_or(true, |bytes| bytes.len() > MAILBOX_TEST_REQUEST_MAX_BYTES)
    {
        return Err(ApiError::bad_request(
            "invalid_request: test-request custody exceeds its bound",
        ));
    }
    Ok(())
}

impl std::fmt::Debug for MailboxTestPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MailboxTestPayload")
            .field("schema_version", &self.schema_version)
            .field("request_send_key", &"<redacted>")
            .field("requester", &self.requester)
            .field("recipient", &self.recipient)
            .field("mode", &self.mode)
            .field("targets", &self.targets)
            .finish()
    }
}

impl std::fmt::Debug for TestRunCausalRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestRunCausalRequest")
            .field("request_message_id", &self.request_message_id)
            .field("canonical_message_id", &self.canonical_message_id)
            .field("request_send_key", &"<redacted>")
            .field("runner", &self.runner)
            .field("payload", &self.payload)
            .finish()
    }
}

#[cfg(test)]
#[path = "tests/mailbox_test_request_shapes.rs"]
mod mailbox_test_request_shapes;
