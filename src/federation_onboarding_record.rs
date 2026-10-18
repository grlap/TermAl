// Owns named onboarding outcomes and their captured verification basis.
// Does not run probes, provision hosts or activate membership. New O1 module,
// composed beside federation_registry.rs rather than split from another file.

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum FederationOnboardingCheck {
    Reachability,
    HostKey,
    CapabilityExchange,
    VersionSchema,
    QualifiedIdentity,
    ForwardRpc,
    ReverseRpc,
    PlatformPathsSupervisor,
    IsolatedWake,
}

const FEDERATION_ONBOARDING_CHECKS: [FederationOnboardingCheck; 9] = [
    FederationOnboardingCheck::Reachability,
    FederationOnboardingCheck::HostKey,
    FederationOnboardingCheck::CapabilityExchange,
    FederationOnboardingCheck::VersionSchema,
    FederationOnboardingCheck::QualifiedIdentity,
    FederationOnboardingCheck::ForwardRpc,
    FederationOnboardingCheck::ReverseRpc,
    FederationOnboardingCheck::PlatformPathsSupervisor,
    FederationOnboardingCheck::IsolatedWake,
];

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationOnboardingOutcome {
    check: FederationOnboardingCheck,
    passed: bool,
    // Fixed local reason, never an untrusted HTTP/SSH response or credential.
    reason: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationEngramOnboardingOutcome {
    status: FederationEngramProbeStatus,
    exit_code: Option<i32>,
    summary: Option<FederationEngramProbeSummary>,
    reason: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum FederationEngramProbeStatus {
    Pass,
    Fail,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationEngramProbeSummary {
    passed: bool,
    checks: Vec<String>,
}

impl FederationEngramOnboardingOutcome {
    fn unavailable() -> Self {
        Self {
            status: FederationEngramProbeStatus::Unavailable,
            exit_code: None,
            summary: None,
            reason: "Engram onboarding verification unavailable".to_owned(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationOnboardingReport {
    attempt_id: String,
    expected: FederationPeerSnapshot,
    basis: String,
    link_incarnation: String,
    outcomes: Vec<FederationOnboardingOutcome>,
    probe_session_id: Option<String>,
    wake_outcome_id: Option<String>,
    #[serde(default)]
    running_image: FederationImageObservation,
    // This observation affects Engram sync readiness only. It is deliberately
    // excluded from the TermAl checklist's complete predicate.
    engram: Option<FederationEngramOnboardingOutcome>,
}

impl FederationOnboardingReport {
    fn termal_complete(&self) -> bool {
        let Some((session_id, outcome_id)) = self
            .probe_session_id
            .as_deref()
            .zip(self.wake_outcome_id.as_deref())
        else {
            return false;
        };
        self.expected.validate().is_ok()
            && self.running_image.valid_identity()
            && self.expected.local_role == FederationRole::Central
            && self.basis.len() == 64
            && self.basis.bytes().all(|byte| byte.is_ascii_hexdigit())
            && [
                self.attempt_id.as_str(),
                self.link_incarnation.as_str(),
                session_id,
                outcome_id,
            ]
            .into_iter()
            .all(|id| validate_federation_probe_id(id).is_ok())
            && session_id != outcome_id
            && session_id != self.attempt_id
            && outcome_id != self.attempt_id
            && self.outcomes.len() == FEDERATION_ONBOARDING_CHECKS.len()
            && FEDERATION_ONBOARDING_CHECKS.iter().all(|check| {
                let mut outcomes = self
                    .outcomes
                    .iter()
                    .filter(|outcome| outcome.check == *check);
                outcomes.next().is_some_and(|outcome| outcome.passed) && outcomes.next().is_none()
            })
    }

    fn registry_checks(&self) -> Result<FederationVerificationChecks, ApiError> {
        if !self.termal_complete() {
            return Err(ApiError::conflict(
                "federation onboarding checklist incomplete",
            ));
        }
        Ok(FederationVerificationChecks {
            host_key: true,
            capability: true,
            protocol: true,
            bidirectional_rpc: true,
            isolated_wake: true,
            platform_supervisor: true,
        })
    }
}
