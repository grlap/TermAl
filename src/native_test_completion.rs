// Shared remote_verification_record v1 completion shape. Native custody and
// authenticated delivery supply provenance; parsing never grants credit.
// Destination qualification belongs to the Engram admission adapter.
use super::*;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Mode { Full, Focused }

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub(super) enum Verdict { Pass, Fail, Interrupted, Running, Unknown }

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Origin {
    pub(super) host_id: String,
    pub(super) runner_session_id: String,
}

fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where D: serde::Deserializer<'de>, T: Deserialize<'de> {
    Option::<T>::deserialize(deserializer)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct NativeResult {
    pub(super) verdict: Verdict,
    pub(super) terminal: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub(super) exit: Option<i64>,
    pub(super) reason: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct LegacyInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) tree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) before_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) after_fingerprint: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(super) enum SourceInputKind {
    #[serde(rename = "source_id")]
    SourceId,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceInput {
    pub(super) kind: SourceInputKind,
    #[serde(deserialize_with = "required_nullable")]
    pub(super) source_id_before: Option<EngramSourceId>,
    #[serde(deserialize_with = "required_nullable")]
    pub(super) source_id_after: Option<EngramSourceId>,
}

/// Historical records retain their exact legacy representation. The closed
/// alternatives reject a malformed tagged value rather than trying digests.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub(super) enum Input {
    SourceId(SourceInput),
    Legacy(LegacyInput),
}

impl Default for Input {
    fn default() -> Self { Self::Legacy(LegacyInput::default()) }
}

impl Input {
    fn valid(&self) -> bool {
        match self {
            Self::SourceId(input) => {
                let deadline = std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET;
                [&input.source_id_before, &input.source_id_after].iter()
                    .filter_map(|value| value.as_ref())
                    .all(|source| serde_json::to_vec(source).is_ok_and(|bytes| bytes.len() <= ENGRAM_SOURCE_JSON_LIMIT)
                        && source.validate(deadline).is_ok())
            }
            Self::Legacy(input) => {
                [&input.commit, &input.tree].iter().filter_map(|value| value.as_ref())
                    .all(|id| test_run_commit_id_valid(id))
                    && [&input.before_fingerprint, &input.after_fingerprint].iter()
                        .filter_map(|value| value.as_ref()).all(|value|
                            value.len() == 64 && value.bytes().all(|byte|
                                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Completion {
    pub(super) schema_version: u32,
    pub(super) origin: Origin,
    pub(super) run_id: String,
    pub(super) mode: Mode,
    pub(super) result: NativeResult,
    pub(super) input: Input,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) finished_at: Option<String>,
}

impl Completion {
    pub(super) fn authoritative_terminal(&self) -> bool {
        self.result.terminal && matches!(self.result.verdict,
            Verdict::Pass | Verdict::Fail | Verdict::Interrupted)
    }

    pub(super) fn validate(&self) -> Result<(), ApiError> {
        let host = Uuid::parse_str(&self.origin.host_id).ok()
            .filter(|id| !id.is_nil()).map(|id| id.to_string());
        if self.schema_version != 1 || host.as_deref() != Some(self.origin.host_id.as_str())
            || self.run_id.is_empty() || self.run_id.len() > TEST_RUN_TEXT_MAX_BYTES
            || !self.run_id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || validate_federation_reference(&self.origin.runner_session_id).is_err()
            || self.result.reason.is_empty() || self.result.reason.len() > 1024
            || self.finished_at.as_ref().is_some_and(|at| at.len() > 128)
            || self.finished_at.as_deref().is_some_and(|at| chrono::DateTime::parse_from_rfc3339(at).is_err())
            || !self.input.valid()
        {
            return Err(ApiError::bad_request("invalid_request: malformed native test completion"));
        }
        Ok(())
    }
}
