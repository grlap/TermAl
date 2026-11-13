// Wire adapter for Engram's execution-observability remote verification
// contract. Native completion custody and delivery remain their existing
// owners. This module neither qualifies evidence nor creates local credit.
use super::*;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Purpose {
    Verification,
    LandingGate,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum IntendedUse {
    CommittedInput {
        expected_commit: String,
        purpose: Purpose,
    },
    AuthorizedTestRef {
        ref_name: String,
        expected_tree: String,
        purpose: Purpose,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Association {
    pub(super) binding: EngramControlWorkBinding,
    pub(super) root: EngramVerificationRecordRoot,
    #[serde(rename = "use")]
    pub(super) intended_use: IntendedUse,
}

// Reuse the retained completion without copying its fields or converting its
// result. Historical Legacy inputs remain readable by that owner, but cannot
// enter a NEW request, including one decoded after persistence/reload.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    try_from = "native_test_completion::Completion",
    into = "native_test_completion::Completion"
)]
struct SourceCompletion(native_test_completion::Completion);

impl TryFrom<native_test_completion::Completion> for SourceCompletion {
    type Error = &'static str;

    fn try_from(completion: native_test_completion::Completion) -> Result<Self, Self::Error> {
        if !matches!(completion.input, native_test_completion::Input::SourceId(_)) {
            return Err(
                "source_id_missing_or_changed: new remote deliveries require SourceId input",
            );
        }
        completion
            .validate()
            .map_err(|_| "invalid_request: malformed native completion")?;
        Ok(Self(completion))
    }
}

impl From<SourceCompletion> for native_test_completion::Completion {
    fn from(completion: SourceCompletion) -> Self {
        completion.0
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    operation_key: String,
    completion: SourceCompletion,
    association: Association,
}

impl Request {
    // The causal delivery owner supplies these already retained facts and
    // association. No source capture, current-time substitution, authority
    // lookup or eligibility test belongs here. Missing endpoints stay null;
    // changed endpoints, failed/provisional results and missing finish times
    // reach Engram unchanged for its single admission decision.
    #[allow(dead_code)] // Consumed by the subsequent delivery/sender slice.
    pub(super) fn new(
        operation_key: String,
        completion: &native_test_completion::Completion,
        association: Association,
    ) -> Result<Self, ApiError> {
        Ok(Self {
            operation_key,
            completion: SourceCompletion::try_from(completion.clone())
                .map_err(ApiError::bad_request)?,
            association,
        })
    }

    #[allow(dead_code)] // No automatic sender/activation is introduced here.
    pub(super) fn into_control_request(self) -> EngramControlRequest {
        EngramControlRequest::RemoteVerificationRecord { record: self }
    }

    #[allow(dead_code)] // The sender must retain this receipt before ACK.
    pub(super) fn receipt(&self, value: Value) -> Result<Receipt, EngramTransportError> {
        let receipt: Receipt = serde_json::from_value(value)
            .map_err(|_| EngramTransportError::protocol("malformed remote verification receipt"))?;
        receipt.check(self.completion.0.mode)?;
        Ok(receipt)
    }
}

// These are Engram's qualification reasons, not a local policy or fallback.
// invalid_request is a transport refusal and never a recorded qualification.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Reason {
    ProducerUnsupported,
    AssociationStale,
    ResultConflict,
    NotFullMode,
    UntrackedDebrisPendingDisposition,
    ResultNotPass,
    CommitMismatch,
    TreeMismatch,
    SourceIdMissingOrChanged,
}

fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Qualification {
    pub(super) credited: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub(super) reason: Option<Reason>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub(super) detail: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Receipt {
    pub(super) operation: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub(super) origin: Option<String>,
    pub(super) verifications: Vec<EngramVerificationRecordVerification>,
    pub(super) qualification: Qualification,
}

impl Receipt {
    fn check(&self, mode: native_test_completion::Mode) -> Result<(), EngramTransportError> {
        let stage = match mode {
            native_test_completion::Mode::Full => "full",
            native_test_completion::Mode::Focused => "focused",
        };
        if !is_engram_record_id(&self.operation)
            || self
                .origin
                .as_ref()
                .is_some_and(|id| !is_engram_record_id(id))
            || self.qualification.credited != self.qualification.reason.is_none()
            || self.verifications.len() != usize::from(self.qualification.credited)
            || (self.qualification.credited && self.origin.is_none())
            || self
                .verifications
                .iter()
                .any(|record| record.stage != stage || !is_engram_record_id(&record.record_id))
        {
            return Err(EngramTransportError::protocol(
                "inconsistent remote verification receipt",
            ));
        }
        Ok(())
    }
}
