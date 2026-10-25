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
struct FederationQualifiedProbe {
    attempt_id: String,
    expected: FederationPeerSnapshot,
    basis: String,
    link_incarnation: String,
    session_id: String,
    outcome_id: String,
}

impl FederationQualifiedProbe {
    fn same_authority_as(&self, current: &FederationPeerSnapshot) -> bool {
        self.expected.local == current.local
            && self.expected.peer == current.peer
            && self.expected.central == current.central
            && self.expected.local_role == current.local_role
            && self.expected.enrollment_generation == current.enrollment_generation
            && self.expected.pair_capability_ref == current.pair_capability_ref
            && self.expected.endpoint == current.endpoint
    }

    fn from_report(report: &FederationOnboardingReport) -> Option<Self> {
        if !report.termal_complete() {
            return None;
        }
        Some(report.retained_probe.clone().unwrap_or_else(|| Self {
            attempt_id: report.attempt_id.clone(),
            expected: report.expected.clone(),
            basis: report.basis.clone(),
            link_incarnation: report.link_incarnation.clone(),
            session_id: report.probe_session_id.clone().expect("complete report"),
            outcome_id: report.wake_outcome_id.clone().expect("complete report"),
        }))
    }

    fn valid(&self) -> bool {
        self.expected.validate().is_ok()
            && self.expected.local_role == FederationRole::Central
            && self.basis.len() == 64
            && self.basis.bytes().all(|byte| byte.is_ascii_hexdigit())
            && [
                &self.attempt_id,
                &self.link_incarnation,
                &self.session_id,
                &self.outcome_id,
            ]
            .into_iter()
            .all(|id| validate_federation_probe_id(id).is_ok())
            && self.session_id != self.outcome_id
            && self.session_id != self.attempt_id
            && self.outcome_id != self.attempt_id
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
    // Reconnects re-attest the current link but do not launch another isolated
    // child. Preserve the original qualified probe's exact provenance instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retained_probe: Option<FederationQualifiedProbe>,
    #[serde(default)]
    running_image: FederationImageObservation,
    #[serde(
        default,
        skip_serializing_if = "FederationImageObservation::is_unknown"
    )]
    reverse_running_image: FederationImageObservation,
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
            && self.retained_probe.as_ref().is_none_or(|probe| {
                probe.valid()
                    && probe.same_authority_as(&self.expected)
                    && probe.session_id == session_id
                    && probe.outcome_id == outcome_id
            })
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
