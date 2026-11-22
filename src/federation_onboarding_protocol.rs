// Owns bounded onboarding probe payloads for the domain-only listener.
// Does not authenticate a caller or accept operator commands from a satellite.
// New O1 protocol beside federation_protocol.rs; not an executor-proxy split.

use super::*;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationOnboardingIdentity {
    pub(crate) host: FederationHostIdentity,
    pub(crate) central: FederationHostIdentity,
    pub(crate) enrollment_generation: u64,
    pub(crate) link_incarnation: String,
    pub(crate) platform: String,
    pub(crate) arch: String,
    pub(crate) profile: FederationExecutionProfile,
    #[serde(default)]
    pub(crate) running_image: FederationImageObservation,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationProbeRequest {
    // C allocates one fresh identity per explicit verification, never a path,
    // shell command, agent prompt, environment or session supplied by a peer.
    pub(crate) attempt_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationProbeSession {
    pub(crate) attempt_id: String,
    pub(crate) session_id: String,
    pub(crate) expected: FederationPeerSnapshot,
    // Empty only for a pre-upgrade record, retained as unknown evidence.
    #[serde(default)]
    pub(crate) basis: String,
    pub(crate) link_incarnation: String,
    pub(crate) wake: Option<FederationProbeWakeOutcome>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationProbeWakeRequest {
    pub(crate) attempt_id: String,
    pub(crate) session_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationProbeWakeOutcome {
    pub(crate) outcome_id: String,
    pub(crate) session_id: String,
    pub(crate) state: FederationProbeWakeState,
    // Observed in this isolated probe session by spawning and reaping a fixed
    // child. A durable acceptance alone is never a delivered wake result.
    pub(crate) platform: Option<FederationPlatformProbe>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum FederationProbeWakeState {
    Accepted,
    Unknown,
    Delivered,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationPlatformProbe {
    pub(crate) platform: String,
    pub(crate) arch: String,
    pub(crate) shell_dialect: String,
    pub(crate) paths_resolved: bool,
    pub(crate) child_spawned_and_reaped: bool,
    pub(crate) toolchain_identity: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FederationReverseProbe {
    pub(crate) identity: FederationOnboardingIdentity,
    pub(crate) attempt_id: String,
}

pub(crate) enum FederationOnboardingRpc {
    Identity,
    Reverse(FederationProbeRequest),
    Prepare(FederationProbeRequest),
    Wake(FederationProbeWakeRequest),
    Engram,
}

impl FederationOnboardingRpc {
    pub(crate) fn path(&self) -> &'static str {
        match self {
            Self::Identity => "/federation/v1/onboarding/identity",
            Self::Reverse(_) => "/federation/v1/onboarding/reverse",
            Self::Prepare(_) => "/federation/v1/onboarding/probe",
            Self::Wake(_) => "/federation/v1/onboarding/wake",
            Self::Engram => "/federation/v1/onboarding/engram",
        }
    }

    pub(crate) fn request(&self, client: &reqwest::Client, url: String) -> reqwest::RequestBuilder {
        match self {
            Self::Identity | Self::Engram => client.get(url),
            Self::Reverse(body) | Self::Prepare(body) => client.post(url).json(body),
            Self::Wake(body) => client.post(url).json(body),
        }
    }
}
