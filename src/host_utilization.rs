// Owns the bounded observational snapshot and freshness cache. Transport and
// native probes remain in their existing federation and platform boundaries.
use super::*;

mod native;
mod runtime;
#[cfg(test)]
mod tests;
pub(super) use runtime::{Runtime, routes, serve_pair};

const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
const REFRESH_INTERVAL: Duration = Duration::from_secs(10);
const STALE_AFTER: Duration = Duration::from_secs(30);
pub(super) const PEER_DEADLINE: Duration = Duration::from_secs(2);
const MAX_INTERFACES: usize = 64;
const MAX_VOLUMES: usize = 32;
const MAX_VOLUME_PATHS: usize = 128;
const MAX_CONFIGURATION_INPUTS: usize = 4096;
const MAX_HOSTS: usize = 256;
const MAX_LABEL_BYTES: usize = 256;
pub(super) const RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum Status {
    Available,
    Stale,
    Unavailable,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum Reason {
    NotSampled,
    ProbeFailed,
    NotExposedByPlatform,
    FirstSample,
    CounterReset,
    InvalidCounters,
    CollectionLimit,
    PeerDisconnected,
    PeerUnavailable,
    IdentityChanged,
    ConfigurationStale,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Metric<T> {
    state: Status,
    value: Option<T>,
    reason: Option<Reason>,
}
impl<T> Metric<T> {
    fn available(value: T) -> Self {
        Self {
            state: Status::Available,
            value: Some(value),
            reason: None,
        }
    }
    fn unavailable(reason: Reason) -> Self {
        Self {
            state: Status::Unavailable,
            value: None,
            reason: Some(reason),
        }
    }
    fn unsupported() -> Self {
        Self {
            state: Status::Unsupported,
            value: None,
            reason: Some(Reason::NotExposedByPlatform),
        }
    }
    fn stale(&mut self) {
        if self.state == Status::Available {
            self.state = Status::Stale;
        }
    }
    fn valid(&self, check: impl FnOnce(&T) -> bool) -> bool {
        match (&self.state, &self.value, &self.reason) {
            (Status::Available | Status::Stale, Some(value), None) => check(value),
            (Status::Unavailable | Status::Unsupported, None, Some(_)) => true,
            _ => false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Cpu {
    logical_count: Metric<u64>,
    busy_percent: Metric<f64>,
    window_ms: Metric<u64>,
    load_average_1_5_15: Metric<[f64; 3]>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Memory {
    total_bytes: Metric<u64>,
    in_use_bytes: Metric<u64>,
    reclaimable_cache_bytes: Metric<u64>,
    free_bytes: Metric<u64>,
    available_bytes: Metric<u64>,
    commit_used_bytes: Metric<u64>,
    commit_limit_bytes: Metric<u64>,
    compressed_bytes: Metric<u64>,
    cache_definition: String,
}
impl Memory {
    fn unavailable() -> Self {
        Self {
            total_bytes: Metric::unavailable(Reason::ProbeFailed),
            in_use_bytes: Metric::unavailable(Reason::ProbeFailed),
            reclaimable_cache_bytes: Metric::unavailable(Reason::ProbeFailed),
            free_bytes: Metric::unavailable(Reason::ProbeFailed),
            available_bytes: Metric::unavailable(Reason::ProbeFailed),
            commit_used_bytes: Metric::unsupported(),
            commit_limit_bytes: Metric::unsupported(),
            compressed_bytes: Metric::unsupported(),
            cache_definition: String::new(),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Volume {
    identity: String,
    total_bytes: Metric<u64>,
    free_bytes: Metric<u64>,
    available_to_caller_bytes: Metric<u64>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VolumeCollection {
    data: Metric<Vec<Volume>>,
    complete: bool,
    failed_path_count: u64,
    unavailable_required_path_count: u64,
    omitted_path_count_at_least: u64,
    failure_reasons: Vec<Reason>,
}
impl VolumeCollection {
    fn valid(&self) -> bool {
        let has_rows = self
            .data
            .value
            .as_ref()
            .is_some_and(|rows| !rows.is_empty());
        self.data.valid(|rows| {
            !rows.is_empty()
                && rows.len() <= MAX_VOLUMES
                && rows.iter().all(|row| {
                    valid_label(&row.identity)
                        && row.total_bytes.valid(|_| true)
                        && row.free_bytes.valid(|_| true)
                        && row.available_to_caller_bytes.valid(|_| true)
                })
        }) && self.failed_path_count <= MAX_VOLUME_PATHS as u64
            && self.unavailable_required_path_count
                <= self.failed_path_count + self.omitted_path_count_at_least
            && self.unavailable_required_path_count <= 2
            && self.omitted_path_count_at_least <= (MAX_CONFIGURATION_INPUTS + 1) as u64
            && self.complete
                == (has_rows
                    && self.failed_path_count == 0
                    && self.omitted_path_count_at_least == 0)
            && self.failure_reasons.len() <= 2
            && self
                .failure_reasons
                .iter()
                .enumerate()
                .all(|(index, reason)| !self.failure_reasons[..index].contains(reason))
            && self
                .failure_reasons
                .iter()
                .all(|reason| matches!(reason, Reason::ProbeFailed | Reason::CollectionLimit))
            && self.failure_reasons.contains(&Reason::ProbeFailed)
                == (self.failed_path_count != 0
                    || (!has_rows && self.omitted_path_count_at_least == 0))
            && self.failure_reasons.contains(&Reason::CollectionLimit)
                == (self.omitted_path_count_at_least != 0)
            && (has_rows
                || (self.data.state == Status::Unavailable
                    && self.data.reason.as_ref() == self.failure_reasons.first()))
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Interface {
    identity: String,
    received_bytes: Metric<u64>,
    sent_bytes: Metric<u64>,
    counter_measured_at: String,
    received_bytes_per_second: Metric<f64>,
    sent_bytes_per_second: Metric<f64>,
    window_ms: Metric<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Snapshot {
    schema_version: u32,
    host: Option<FederationHostIdentity>,
    os: String,
    architecture: String,
    measured_at: String,
    source_age_ms: u64,
    stale_after_ms: u64,
    cpu: Cpu,
    memory: Memory,
    volumes: VolumeCollection,
    network: Metric<Vec<Interface>>,
}
impl Snapshot {
    fn is_stale(&self) -> bool {
        self.source_age_ms > millis(STALE_AFTER)
            || [
                self.cpu.logical_count.state,
                self.cpu.busy_percent.state,
                self.cpu.window_ms.state,
                self.cpu.load_average_1_5_15.state,
                self.memory.total_bytes.state,
                self.memory.in_use_bytes.state,
                self.memory.reclaimable_cache_bytes.state,
                self.memory.free_bytes.state,
                self.memory.available_bytes.state,
                self.memory.commit_used_bytes.state,
                self.memory.commit_limit_bytes.state,
                self.memory.compressed_bytes.state,
                self.volumes.data.state,
                self.network.state,
            ]
            .contains(&Status::Stale)
    }
    fn age(&mut self, elapsed: Duration, force_stale: bool) {
        self.source_age_ms = self.source_age_ms.saturating_add(millis(elapsed));
        if !force_stale && self.source_age_ms <= millis(STALE_AFTER) {
            return;
        }
        self.cpu.logical_count.stale();
        self.cpu.busy_percent.stale();
        self.cpu.window_ms.stale();
        self.cpu.load_average_1_5_15.stale();
        self.memory.total_bytes.stale();
        self.memory.in_use_bytes.stale();
        self.memory.reclaimable_cache_bytes.stale();
        self.memory.free_bytes.stale();
        self.memory.available_bytes.stale();
        self.memory.commit_used_bytes.stale();
        self.memory.commit_limit_bytes.stale();
        self.memory.compressed_bytes.stale();
        self.volumes.data.stale();
        self.network.stale();
        if let Some(volumes) = &mut self.volumes.data.value {
            for v in volumes {
                v.total_bytes.stale();
                v.free_bytes.stale();
                v.available_to_caller_bytes.stale();
            }
        }
        if let Some(network) = &mut self.network.value {
            for n in network {
                n.received_bytes.stale();
                n.sent_bytes.stale();
                n.received_bytes_per_second.stale();
                n.sent_bytes_per_second.stale();
                n.window_ms.stale();
            }
        }
    }

    fn validate(&self, host: &FederationHostIdentity) -> bool {
        self.schema_version == 1
            && self.host.as_ref() == Some(host)
            && self.stale_after_ms == millis(STALE_AFTER)
            && matches!(self.os.as_str(), "windows" | "linux" | "macos")
            && valid_label(&self.architecture)
            && chrono::DateTime::parse_from_rfc3339(&self.measured_at).is_ok()
            && self.memory.cache_definition.len() <= MAX_LABEL_BYTES
            && self.cpu.logical_count.valid(|n| *n > 0)
            && self
                .cpu
                .busy_percent
                .valid(|n| n.is_finite() && (0.0..=100.0).contains(n))
            && self.cpu.window_ms.valid(|n| *n > 0)
            && self
                .cpu
                .load_average_1_5_15
                .valid(|ns| ns.iter().all(|n| n.is_finite() && *n >= 0.0))
            && [
                &self.memory.total_bytes,
                &self.memory.in_use_bytes,
                &self.memory.reclaimable_cache_bytes,
                &self.memory.free_bytes,
                &self.memory.available_bytes,
                &self.memory.commit_used_bytes,
                &self.memory.commit_limit_bytes,
                &self.memory.compressed_bytes,
            ]
            .iter()
            .all(|m| m.valid(|_| true))
            && self.volumes.valid()
            && self.network.valid(|rows| {
                rows.len() <= MAX_INTERFACES
                    && rows.iter().all(|row| {
                        valid_label(&row.identity)
                            && chrono::DateTime::parse_from_rfc3339(&row.counter_measured_at)
                                .is_ok()
                            && row.received_bytes.valid(|_| true)
                            && row.sent_bytes.valid(|_| true)
                            && row
                                .received_bytes_per_second
                                .valid(|n| n.is_finite() && *n >= 0.0)
                            && row
                                .sent_bytes_per_second
                                .valid(|n| n.is_finite() && *n >= 0.0)
                            && row.window_ms.valid(|n| *n > 0)
                    })
            })
    }
}

fn valid_label(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_LABEL_BYTES && !value.chars().any(char::is_control)
}
fn millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}
fn utc_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[derive(Clone)]
struct Stored {
    snapshot: Snapshot,
    observed: std::time::Instant,
    received_at: String,
}
impl Stored {
    fn project(&self, now: std::time::Instant, stale: bool) -> Snapshot {
        let mut snapshot = self.snapshot.clone();
        snapshot.age(now.saturating_duration_since(self.observed), stale);
        snapshot
    }
}
#[derive(Clone)]
struct Peer {
    identity: FederationHostIdentity,
    os: String,
    architecture: String,
    enrollment_generation: u64,
    transport_generation: u64,
    connected: bool,
}
#[derive(Clone)]
struct PeerSample {
    peer: Peer,
    sample: Option<Stored>,
    reason: Option<Reason>,
}
#[derive(Default)]
struct Image {
    local: Option<Stored>,
    local_identity: Option<FederationHostIdentity>,
    peers: BTreeMap<String, PeerSample>,
    roster_observed: Option<std::time::Instant>,
    roster_total: usize,
    roster_complete: bool,
    central: Option<bool>,
}
#[derive(Default)]
pub(super) struct Cache {
    image: RwLock<Image>,
    // Held until the actual sampler exits or unwinds, even after bounded
    // shutdown detaches it. A failed thread spawn releases it separately.
    sampler_running: AtomicBool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HostView {
    host: Option<FederationHostIdentity>,
    os: String,
    architecture: String,
    state: Status,
    reason: Option<Reason>,
    received_at: Option<String>,
    snapshot: Option<Snapshot>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Aggregate {
    schema_version: u32,
    roster_state: Status,
    roster_age_ms: Option<u64>,
    roster_total: usize,
    complete: bool,
    hosts: Vec<HostView>,
}

impl Cache {
    fn local(&self, now: std::time::Instant) -> Result<Snapshot, ApiError> {
        let image = self
            .image
            .try_read()
            .map_err(|_| unavailable("utilization cache busy"))?;
        image
            .local
            .as_ref()
            .map(|stored| stored.project(now, false))
            .ok_or_else(|| unavailable("host utilization not sampled yet"))
    }
    fn aggregate(&self, now: std::time::Instant) -> Result<Aggregate, ApiError> {
        let image = self
            .image
            .try_read()
            .map_err(|_| unavailable("utilization cache busy"))?;
        if image.central.is_none() {
            return Err(unavailable(
                "host utilization configuration not sampled yet",
            ));
        }
        if image.central == Some(false) {
            return Err(ApiError::conflict(
                "host utilization aggregate requires Central",
            ));
        }
        let age = image
            .roster_observed
            .map(|time| now.saturating_duration_since(time));
        let roster_stale = age.is_none_or(|age| age > STALE_AFTER);
        let mut hosts = Vec::with_capacity(image.peers.len() + 1);
        if let Some(local) = &image.local {
            let snapshot = local.project(now, false);
            hosts.push(HostView {
                host: snapshot.host.clone(),
                os: snapshot.os.clone(),
                architecture: snapshot.architecture.clone(),
                state: if snapshot.is_stale() {
                    Status::Stale
                } else {
                    Status::Available
                },
                reason: None,
                received_at: Some(local.received_at.clone()),
                snapshot: Some(snapshot),
            });
        } else {
            hosts.push(HostView {
                host: image.local_identity.clone(),
                os: std::env::consts::OS.to_owned(),
                architecture: std::env::consts::ARCH.to_owned(),
                state: Status::Unavailable,
                reason: Some(Reason::NotSampled),
                received_at: None,
                snapshot: None,
            });
        }
        for row in image.peers.values() {
            let disconnected = !row.peer.connected || roster_stale;
            let unavailable = disconnected || row.reason.is_some();
            let snapshot = row
                .sample
                .as_ref()
                .map(|sample| sample.project(now, unavailable));
            hosts.push(HostView {
                host: Some(row.peer.identity.clone()),
                os: snapshot
                    .as_ref()
                    .map(|s| s.os.clone())
                    .unwrap_or_else(|| row.peer.os.clone()),
                architecture: snapshot
                    .as_ref()
                    .map(|s| s.architecture.clone())
                    .unwrap_or_else(|| row.peer.architecture.clone()),
                state: if unavailable || snapshot.is_none() {
                    Status::Unavailable
                } else if snapshot.as_ref().is_some_and(Snapshot::is_stale) {
                    Status::Stale
                } else {
                    Status::Available
                },
                reason: if roster_stale {
                    Some(Reason::ConfigurationStale)
                } else if disconnected {
                    Some(Reason::PeerDisconnected)
                } else {
                    row.reason.or(if snapshot.is_none() {
                        Some(Reason::NotSampled)
                    } else {
                        None
                    })
                },
                received_at: row.sample.as_ref().map(|s| s.received_at.clone()),
                snapshot,
            });
        }
        Ok(Aggregate {
            schema_version: 1,
            roster_state: if roster_stale {
                Status::Stale
            } else if !image.roster_complete {
                Status::Unavailable
            } else {
                Status::Available
            },
            roster_age_ms: age.map(millis),
            roster_total: image.roster_total,
            complete: image.roster_complete && image.local.is_some() && !roster_stale,
            hosts,
        })
    }
}
fn unavailable(message: &str) -> ApiError {
    ApiError::from_status(StatusCode::SERVICE_UNAVAILABLE, message)
}
