// Converts native cumulative counters into one bounded sample. No subprocesses,
// runtime control or application-state access belong in a collector.
use super::*;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;
#[cfg(target_os = "linux")]
use linux::Platform;
#[cfg(target_os = "macos")]
use macos::Platform;
#[cfg(windows)]
use windows::Platform;

pub(super) struct VolumePaths {
    pub(super) required: Vec<PathBuf>,
    pub(super) additional: Vec<PathBuf>,
    pub(super) omitted_at_least: u64,
}

struct NetworkCounter {
    identity: String,
    received: u64,
    sent: u64,
}
struct CpuCounter {
    total: u64,
    idle: u64,
}
struct Reading {
    logical: Metric<u64>,
    cpu: Option<CpuCounter>,
    busy: Option<(Metric<f64>, Metric<u64>)>,
    load: Metric<[f64; 3]>,
    memory: Memory,
    network: Metric<Vec<NetworkCounter>>,
    network_observed: std::time::Instant,
    network_at: String,
}
pub(super) struct Collector {
    platform: Platform,
    previous_cpu: Option<(CpuCounter, std::time::Instant)>,
    previous_network: HashMap<String, (u64, u64, std::time::Instant)>,
    previous_sample: Option<std::time::Instant>,
}
impl Collector {
    pub(super) fn new() -> Self {
        Self {
            platform: Platform::new(),
            previous_cpu: None,
            previous_network: HashMap::new(),
            previous_sample: None,
        }
    }
    pub(super) fn sample(
        &mut self,
        host: Option<FederationHostIdentity>,
        paths: &VolumePaths,
    ) -> Snapshot {
        let started = std::time::Instant::now();
        let at = utc_now();
        let reading = self.platform.read();
        let window = self
            .previous_sample
            .map(|previous| started.saturating_duration_since(previous));
        let calculated_busy = || match (&self.previous_cpu, &reading.cpu) {
            (Some((before, _)), Some(after)) => cpu_percent(before, after),
            (_, None) => Metric::unavailable(Reason::ProbeFailed),
            _ => Metric::unavailable(Reason::FirstSample),
        };
        let (busy, cpu_window) = reading.busy.unwrap_or_else(|| {
            (
                calculated_busy(),
                window
                    .map(|d| Metric::available(millis(d)))
                    .unwrap_or_else(|| Metric::unavailable(Reason::FirstSample)),
            )
        });
        self.previous_cpu = reading.cpu.map(|counter| (counter, started));
        self.previous_sample = Some(started);
        let mut next_network = HashMap::new();
        let network = Metric {
            state: reading.network.state,
            reason: reading.network.reason,
            value: reading.network.value.map(|rows| {
                rows.into_iter()
                    .map(|row| {
                        let prior = self.previous_network.get(&row.identity);
                        let (received_rate, sent_rate, rate_window) = match prior {
                            Some((rx, tx, observed)) => {
                                let elapsed = reading
                                    .network_observed
                                    .saturating_duration_since(*observed);
                                (
                                    counter_rate(*rx, row.received, elapsed),
                                    counter_rate(*tx, row.sent, elapsed),
                                    Metric::available(millis(elapsed)),
                                )
                            }
                            None => (
                                Metric::unavailable(Reason::FirstSample),
                                Metric::unavailable(Reason::FirstSample),
                                Metric::unavailable(Reason::FirstSample),
                            ),
                        };
                        next_network.insert(
                            row.identity.clone(),
                            (row.received, row.sent, reading.network_observed),
                        );
                        Interface {
                            identity: row.identity,
                            received_bytes: Metric::available(row.received),
                            sent_bytes: Metric::available(row.sent),
                            counter_measured_at: reading.network_at.clone(),
                            received_bytes_per_second: received_rate,
                            sent_bytes_per_second: sent_rate,
                            window_ms: rate_window,
                        }
                    })
                    .collect()
            }),
        };
        self.previous_network = next_network;
        let volumes = volumes(paths);
        Snapshot {
            schema_version: 1,
            host,
            os: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
            measured_at: at,
            source_age_ms: millis(started.elapsed()),
            stale_after_ms: millis(STALE_AFTER),
            cpu: Cpu {
                logical_count: reading.logical,
                busy_percent: busy,
                window_ms: cpu_window,
                load_average_1_5_15: reading.load,
            },
            memory: reading.memory,
            network,
            volumes,
        }
    }
}

fn cpu_percent(before: &CpuCounter, after: &CpuCounter) -> Metric<f64> {
    match (
        after.total.checked_sub(before.total),
        after.idle.checked_sub(before.idle),
    ) {
        (Some(total), Some(idle)) if total > 0 && idle <= total => {
            Metric::available(100.0 * (total - idle) as f64 / total as f64)
        }
        _ => Metric::unavailable(Reason::CounterReset),
    }
}
fn counter_rate(before: u64, after: u64, elapsed: Duration) -> Metric<f64> {
    match after.checked_sub(before) {
        Some(delta) if !elapsed.is_zero() => {
            Metric::available(delta as f64 / elapsed.as_secs_f64())
        }
        _ => Metric::unavailable(Reason::CounterReset),
    }
}

#[cfg(test)]
mod arithmetic_tests {
    use super::*;
    #[test]
    fn rates_use_bytes_per_second_and_counter_resets_never_become_zero() {
        assert_eq!(
            counter_rate(100, 400, Duration::from_millis(1500)).value,
            Some(200.0)
        );
        assert_eq!(
            counter_rate(400, 100, Duration::from_secs(5)).reason,
            Some(Reason::CounterReset)
        );
        assert_eq!(
            counter_rate(100, 400, Duration::ZERO).state,
            Status::Unavailable
        );
        assert_eq!(
            cpu_percent(
                &CpuCounter {
                    total: 100,
                    idle: 50
                },
                &CpuCounter {
                    total: 500,
                    idle: 350
                }
            )
            .value,
            Some(25.0)
        );
        assert_eq!(
            cpu_percent(
                &CpuCounter {
                    total: 100,
                    idle: 50
                },
                &CpuCounter {
                    total: 50,
                    idle: 40
                }
            )
            .state,
            Status::Unavailable
        );
    }
}

fn volumes(paths: &VolumePaths) -> VolumeCollection {
    collect_volumes(paths, Platform::volume)
}

fn collect_volumes(
    paths: &VolumePaths,
    mut probe: impl FnMut(&FsPath) -> Result<Volume, Reason>,
) -> VolumeCollection {
    let mut rows = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut failed = 0;
    let mut required_unavailable = 0;
    let mut omitted = paths.omitted_at_least;
    // Required paths are examined before optional projects or worktrees, so
    // an optional-path limit cannot hide the data/default checkout volumes.
    for (path, required) in paths
        .required
        .iter()
        .map(|path| (path, true))
        .chain(paths.additional.iter().map(|path| (path, false)))
    {
        if !seen.insert(path) {
            continue;
        }
        let result = if seen.len() > MAX_VOLUME_PATHS || path.as_os_str().len() > 32768 {
            Err(Reason::CollectionLimit)
        } else {
            probe(path)
        };
        let volume = match result {
            Ok(volume) if valid_label(&volume.identity) => volume,
            Ok(volume) if volume.identity.len() > MAX_LABEL_BYTES => {
                omitted += 1;
                required_unavailable += u64::from(required);
                continue;
            }
            Err(Reason::CollectionLimit) => {
                omitted += 1;
                required_unavailable += u64::from(required);
                continue;
            }
            _ => {
                failed += 1;
                required_unavailable += u64::from(required);
                continue;
            }
        };
        if rows.len() == MAX_VOLUMES && !rows.contains_key(&volume.identity) {
            omitted += 1;
            required_unavailable += u64::from(required);
            continue;
        }
        rows.insert(volume.identity.clone(), volume);
    }
    omitted = omitted.min((MAX_CONFIGURATION_INPUTS + 1) as u64);
    let complete = !rows.is_empty() && failed == 0 && omitted == 0;
    let mut failure_reasons = Vec::new();
    if failed != 0 || (rows.is_empty() && omitted == 0) {
        failure_reasons.push(Reason::ProbeFailed);
    }
    if omitted != 0 {
        failure_reasons.push(Reason::CollectionLimit);
    }
    let data = if rows.is_empty() {
        Metric::unavailable(failure_reasons[0])
    } else {
        Metric::available(rows.into_values().collect())
    };
    VolumeCollection {
        data,
        complete,
        failed_path_count: failed,
        unavailable_required_path_count: required_unavailable,
        omitted_path_count_at_least: omitted,
        failure_reasons,
    }
}

#[cfg(test)]
mod volume_tests {
    use super::*;

    fn measured(identity: &str) -> Volume {
        Volume {
            identity: identity.to_owned(),
            total_bytes: Metric::available(1000),
            free_bytes: Metric::available(400),
            available_to_caller_bytes: Metric::available(300),
        }
    }

    #[test]
    fn failed_optional_path_keeps_the_required_volume_without_exposing_the_path() {
        let paths = VolumePaths {
            required: vec![PathBuf::from("data-root")],
            additional: vec![
                PathBuf::from("deleted-private-project"),
                PathBuf::from("data-root"),
            ],
            omitted_at_least: 0,
        };
        let mut probes = 0;
        let collection = collect_volumes(&paths, |path| {
            probes += 1;
            if path == FsPath::new("data-root") {
                Ok(measured("data-volume"))
            } else {
                Err(Reason::ProbeFailed)
            }
        });
        assert_eq!(probes, 2, "duplicate configured paths share one probe");
        assert!(collection.valid());
        assert_eq!(collection.data.state, Status::Available);
        assert_eq!(
            collection.data.value.as_ref().unwrap()[0].identity,
            "data-volume"
        );
        assert!(!collection.complete);
        assert_eq!(collection.failed_path_count, 1);
        assert_eq!(collection.unavailable_required_path_count, 0);
        assert_eq!(collection.failure_reasons, [Reason::ProbeFailed]);
        assert!(
            !serde_json::to_string(&collection)
                .unwrap()
                .contains("deleted-private-project")
        );
    }

    #[test]
    fn failed_required_paths_and_all_failed_collection_remain_explicitly_unavailable() {
        let paths = VolumePaths {
            required: vec![PathBuf::from("missing-data")],
            additional: vec![PathBuf::from("optional")],
            omitted_at_least: 0,
        };
        for all_failed in [false, true] {
            let collection = collect_volumes(&paths, |path| {
                if !all_failed && path == FsPath::new("optional") {
                    Ok(measured("optional-volume"))
                } else {
                    Err(Reason::ProbeFailed)
                }
            });
            assert!(collection.valid());
            assert!(!collection.complete);
            assert_eq!(collection.unavailable_required_path_count, 1);
            assert_eq!(collection.failed_path_count, if all_failed { 2 } else { 1 });
            if all_failed {
                assert_eq!(collection.data.state, Status::Unavailable);
                assert!(
                    collection.data.value.is_none(),
                    "failure cannot invent zero capacities"
                );
            } else {
                assert_eq!(collection.data.value.as_ref().unwrap().len(), 1);
            }
        }
    }

    #[test]
    fn collection_limits_preserve_required_rows_and_report_a_bounded_lower_bound() {
        let paths = VolumePaths {
            required: vec![PathBuf::from("data-root")],
            additional: (0..MAX_VOLUME_PATHS + 1)
                .map(|n| PathBuf::from(format!("volume-{n}")))
                .collect(),
            omitted_at_least: 1,
        };
        let mut probes = 0;
        let collection = collect_volumes(&paths, |path| {
            probes += 1;
            Ok(measured(path.to_str().unwrap()))
        });
        assert_eq!(probes, MAX_VOLUME_PATHS);
        assert!(collection.valid());
        assert_eq!(collection.data.value.as_ref().unwrap().len(), MAX_VOLUMES);
        assert!(
            collection
                .data
                .value
                .as_ref()
                .unwrap()
                .iter()
                .any(|row| row.identity == "data-root")
        );
        assert!(!collection.complete);
        assert_eq!(collection.failed_path_count, 0);
        assert_eq!(collection.unavailable_required_path_count, 0);
        assert_eq!(
            collection.omitted_path_count_at_least,
            (MAX_VOLUME_PATHS + 3 - MAX_VOLUMES) as u64
        );
        assert_eq!(collection.failure_reasons, [Reason::CollectionLimit]);
    }
}

#[cfg(unix)]
fn load_average() -> Metric<[f64; 3]> {
    let mut load = [0.0; 3];
    // SAFETY: the native function receives exactly three writable doubles.
    if unsafe { libc::getloadavg(load.as_mut_ptr(), 3) } == 3
        && load.iter().all(|v| v.is_finite() && *v >= 0.0)
    {
        Metric::available(load)
    } else {
        Metric::unavailable(Reason::ProbeFailed)
    }
}

#[cfg(unix)]
fn bounded_read(path: &FsPath, limit: usize) -> Result<String, Reason> {
    use std::io::Read;
    let file = fs::File::open(path).map_err(|_| Reason::ProbeFailed)?;
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| Reason::ProbeFailed)?;
    if bytes.len() > limit {
        return Err(Reason::CollectionLimit);
    }
    String::from_utf8(bytes).map_err(|_| Reason::InvalidCounters)
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
struct Platform;
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
impl Platform {
    fn new() -> Self {
        Self
    }
    fn read(&mut self) -> Reading {
        Reading {
            logical: Metric::unsupported(),
            cpu: None,
            busy: Some((Metric::unsupported(), Metric::unsupported())),
            load: Metric::unsupported(),
            memory: Memory::unavailable(),
            network: Metric::unsupported(),
            network_observed: std::time::Instant::now(),
            network_at: utc_now(),
        }
    }
    fn volume(_: &FsPath) -> Result<Volume, Reason> {
        Err(Reason::NotExposedByPlatform)
    }
}
