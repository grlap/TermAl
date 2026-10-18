// Owns bounded onboarding probe payloads for the domain-only listener.
// Does not authenticate a caller or accept operator commands from a satellite.
// New O1 protocol beside federation_protocol.rs; not an executor-proxy split.

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationOnboardingIdentity {
    host: FederationHostIdentity,
    central: FederationHostIdentity,
    enrollment_generation: u64,
    link_incarnation: String,
    platform: String,
    arch: String,
    profile: FederationExecutionProfile,
    #[serde(default)]
    running_image: FederationImageObservation,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationProbeRequest {
    // C allocates one fresh identity per explicit verification, never a path,
    // shell command, agent prompt, environment or session supplied by a peer.
    attempt_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationProbeSession {
    attempt_id: String,
    session_id: String,
    expected: FederationPeerSnapshot,
    // Empty only for a pre-upgrade record, retained as unknown evidence.
    #[serde(default)]
    basis: String,
    link_incarnation: String,
    wake: Option<FederationProbeWakeOutcome>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationProbeWakeRequest {
    attempt_id: String,
    session_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationProbeWakeOutcome {
    outcome_id: String,
    session_id: String,
    state: FederationProbeWakeState,
    // Observed in this isolated probe session by spawning and reaping a fixed
    // child. A durable acceptance alone is never a delivered wake result.
    platform: Option<FederationPlatformProbe>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum FederationProbeWakeState {
    Accepted,
    Unknown,
    Delivered,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationPlatformProbe {
    platform: String,
    arch: String,
    shell_dialect: String,
    paths_resolved: bool,
    child_spawned_and_reaped: bool,
    toolchain_identity: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationReverseProbe {
    identity: FederationOnboardingIdentity,
    attempt_id: String,
}

enum FederationOnboardingRpc {
    Identity,
    Reverse(FederationProbeRequest),
    Prepare(FederationProbeRequest),
    Wake(FederationProbeWakeRequest),
    Engram,
}

impl FederationOnboardingRpc {
    fn path(&self) -> &'static str {
        match self {
            Self::Identity => "/federation/v1/onboarding/identity",
            Self::Reverse(_) => "/federation/v1/onboarding/reverse",
            Self::Prepare(_) => "/federation/v1/onboarding/probe",
            Self::Wake(_) => "/federation/v1/onboarding/wake",
            Self::Engram => "/federation/v1/onboarding/engram",
        }
    }

    fn request(&self, client: &reqwest::Client, url: String) -> reqwest::RequestBuilder {
        match self {
            Self::Identity | Self::Engram => client.get(url),
            Self::Reverse(body) | Self::Prepare(body) => client.post(url).json(body),
            Self::Wake(body) => client.post(url).json(body),
        }
    }
}
