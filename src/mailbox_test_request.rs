// Preparatory custody shapes beside mailbox and native registration records.
// Existing producers leave these fields absent. Until the durable custody
// owner is installed, every serde ingress rejects a supplied non-null value.
// These types neither capture an association nor qualify a test result.

const TEST_REQUEST_CUSTODY_UNAVAILABLE: &str = "test_request_custody_unavailable";

fn reject_inactive_test_request_custody<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    if value.is_null() {
        Ok(None)
    } else {
        Err(serde::de::Error::custom(TEST_REQUEST_CUSTODY_UNAVAILABLE))
    }
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
#[serde(deny_unknown_fields)]
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

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MailboxTestPayload {
    schema_version: u16,
    requester: MailboxRelayPrincipal,
    mode: MailboxTestMode,
    targets: Vec<MailboxTestCapturedTarget>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TestRunCausalRequest {
    request_message_id: String,
    canonical_message_id: String,
    request_send_key: String,
    runner: MailboxRelayPrincipal,
    payload: MailboxTestPayload,
}

#[cfg(test)]
#[path = "tests/mailbox_test_request_shapes.rs"]
mod mailbox_test_request_shapes;
