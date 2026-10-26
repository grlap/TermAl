// Host lifecycle journal beside persist_observer.rs. New file.
// Owns one bounded JSON-lines file in the TermAl state directory: a boot
// record per start, a flushed heartbeat each minute with numeric counters
// only, a panic record from the process panic hook and a shutdown record on a
// graceful or fatal exit. At boot it classifies how the previous instance
// ended. It owns no persistence, scheduling or session data: no message
// bodies, prompts, store contents or absolute paths are written, formatted
// panic text is withheld, and the heartbeat never takes the state lock.

const HOST_LIFECYCLE_FILE: &str = "host-lifecycle.jsonl";
const HOST_LIFECYCLE_PREVIOUS_FILE: &str = "host-lifecycle.1.jsonl";
// The current file rotates to one previous file past this size.
const HOST_LIFECYCLE_MAX_BYTES: u64 = 512 * 1024;
const HOST_LIFECYCLE_HEARTBEAT: Duration = Duration::from_secs(60);
const HOST_LIFECYCLE_PANIC_MESSAGE_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum HostShutdownOutcome {
    Graceful,
    Fatal,
}

/// Counters only: no identifiers, paths or text beyond a source location.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct HostHeartbeatCounters {
    persist_ticks: u64,
    persist_failed_ticks: u64,
    persisted_payload_bytes: u64,
    longest_persist_tick_us: u64,
    longest_state_hold_ms: Option<u64>,
    longest_state_hold_at: Option<String>,
    working_set_bytes: Option<u64>,
    committed_bytes: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
enum HostLifecycleRecord {
    Boot { at: String, instance_id: String, pid: u32, binary_sha256: Option<String> },
    Heartbeat { at: String, instance_id: String, counters: HostHeartbeatCounters },
    Panic { at: String, instance_id: String, thread: String, location: Option<String>, message: String },
    Shutdown { at: String, instance_id: String, outcome: HostShutdownOutcome },
}

impl HostLifecycleRecord {
    fn instance_id(&self) -> &str {
        match self {
            Self::Boot { instance_id, .. }
            | Self::Heartbeat { instance_id, .. }
            | Self::Panic { instance_id, .. }
            | Self::Shutdown { instance_id, .. } => instance_id,
        }
    }
}

/// How the instance before this one ended, from its own records.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "ended", rename_all = "camelCase", rename_all_fields = "camelCase")]
enum HostPreviousEnd {
    /// No earlier boot is recorded.
    Unknown,
    /// A shutdown record: graceful, or after a fatal server error.
    Shutdown { instance_id: String, at: String, outcome: HostShutdownOutcome, panics: usize },
    /// A panic was its last record: no heartbeat or shutdown followed it.
    Panic { instance_id: String, at: String, thread: String, location: Option<String>, message: String,
        boot_at: String, last_heartbeat_at: Option<String>, panics: usize },
    /// No shutdown, and it kept running after any panic: killed, crashed
    /// natively, or the machine reset.
    Unclean { instance_id: String, boot_at: String, last_heartbeat_at: Option<String>, panics: usize },
}

fn host_lifecycle_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The previous instance's end. The last boot names the instance; all of its
/// own records decide, wherever rotation left them.
fn host_lifecycle_classify(records: &[HostLifecycleRecord]) -> HostPreviousEnd {
    let Some(last_boot) = records.iter().rev().find(|record| matches!(record, HostLifecycleRecord::Boot { .. })) else {
        return HostPreviousEnd::Unknown;
    };
    let instance_id = last_boot.instance_id().to_owned();
    let own: Vec<_> = records.iter().filter(|record| record.instance_id() == instance_id).collect();
    // A rotation re-anchors the same boot; the first copy is the boot time.
    let boot_at = own.iter().find_map(|record| match record {
        HostLifecycleRecord::Boot { at, .. } => Some(at.clone()),
        _ => None,
    }).unwrap_or_default();
    let (mut panics, mut last_panic, mut last_heartbeat, mut ran_on) = (0, None, None, false);
    for record in own {
        match record {
            HostLifecycleRecord::Shutdown { at, outcome, .. } => {
                return HostPreviousEnd::Shutdown { instance_id, at: at.clone(), outcome: *outcome, panics };
            }
            HostLifecycleRecord::Panic { .. } => {
                panics += 1;
                last_panic = Some(record);
                ran_on = false;
            }
            HostLifecycleRecord::Heartbeat { at, .. } => {
                last_heartbeat = Some(at.clone());
                ran_on = true;
            }
            HostLifecycleRecord::Boot { .. } => {}
        }
    }
    match last_panic {
        Some(HostLifecycleRecord::Panic { at, thread, location, message, .. }) if !ran_on => HostPreviousEnd::Panic {
            instance_id, at: at.clone(), thread: thread.clone(), location: location.clone(),
            message: message.clone(), boot_at, last_heartbeat_at: last_heartbeat, panics,
        },
        _ => HostPreviousEnd::Unclean { instance_id, boot_at, last_heartbeat_at: last_heartbeat, panics },
    }
}

/// A literal panic message, cut at a character boundary within the bound.
/// Formatted messages can carry runtime data, so only their size is kept.
fn host_lifecycle_panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        let mut end = text.len().min(HOST_LIFECYCLE_PANIC_MESSAGE_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        return text[..end].to_owned();
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return format!("formatted message withheld ({} bytes)", text.len());
    }
    "non-string panic payload".to_owned()
}

/// A source location without any absolute path: this crate's own relative
/// `src/...` path, otherwise only the file name. Both separators and drive
/// or UNC prefixes are handled the same way on every host OS, since a panic
/// location is whatever path the compiler recorded.
fn host_lifecycle_location(file: &str, line: u32) -> String {
    let normalized = file.replace('\\', "/");
    let crate_relative = normalized.starts_with("src/")
        && !normalized.split('/').any(|step| step == ".." || step.contains(':'));
    let shown = if crate_relative {
        normalized.as_str()
    } else {
        normalized.rsplit('/').next().unwrap_or_default()
    };
    format!("{shown}:{line}")
}

/// The bounded journal. Appends are serialized; the panic hook never waits.
struct HostLifecycleJournal {
    directory: PathBuf,
    // This instance's boot record, written again at the top of each new file
    // after a rotation, so a long-running instance never loses its anchor.
    writing: Mutex<Option<HostLifecycleRecord>>,
}

impl HostLifecycleJournal {
    fn new(directory: PathBuf) -> Self {
        Self { directory, writing: Mutex::new(None) }
    }

    /// Records from the previous file, then the current one. Each line is
    /// parsed on its own: a torn line after a reset, invalid UTF-8 included,
    /// or a line this build cannot read, is skipped alone.
    fn read_records(&self) -> Vec<HostLifecycleRecord> {
        [HOST_LIFECYCLE_PREVIOUS_FILE, HOST_LIFECYCLE_FILE]
            .into_iter()
            .filter_map(|name| fs::read(self.directory.join(name)).ok())
            .flat_map(|bytes| bytes.split(|byte| *byte == b'\n')
                .filter_map(|line| serde_json::from_slice(line).ok())
                .collect::<Vec<_>>())
            .collect()
    }

    /// Ends a partial last line left by a reset, so the next record starts on
    /// its own line instead of joining the torn one.
    fn terminate_torn_line(&self) -> std::io::Result<()> {
        use std::io::{Read, Seek, SeekFrom, Write};
        let _writing = self.writing.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Ok(mut file) = fs::OpenOptions::new().read(true).append(true)
            .open(self.directory.join(HOST_LIFECYCLE_FILE)) else { return Ok(()) };
        if file.metadata()?.len() == 0 {
            return Ok(());
        }
        let mut last = [0u8];
        file.seek(SeekFrom::End(-1))?;
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            file.write_all(b"\n")?;
            file.sync_data()?;
        }
        Ok(())
    }

    /// Appends this instance's boot record and keeps it as the rotation anchor.
    fn append_boot(&self, boot: HostLifecycleRecord) -> std::io::Result<()> {
        let mut writing = self.writing.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *writing = Some(boot.clone());
        Self::append_locked(&self.directory, &writing, &boot)
    }

    fn append(&self, record: &HostLifecycleRecord) -> std::io::Result<()> {
        let writing = self.writing.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::append_locked(&self.directory, &writing, record)
    }

    /// For the panic hook: skips rather than waits if an append is in flight,
    /// so a panic inside an append cannot deadlock.
    fn try_append(&self, record: &HostLifecycleRecord) -> std::io::Result<()> {
        match self.writing.try_lock() {
            Ok(writing) => Self::append_locked(&self.directory, &writing, record),
            Err(std::sync::TryLockError::Poisoned(writing)) => {
                Self::append_locked(&self.directory, &writing.into_inner(), record)
            }
            Err(std::sync::TryLockError::WouldBlock) => Ok(()),
        }
    }

    fn append_locked(directory: &FsPath, anchor: &Option<HostLifecycleRecord>, record: &HostLifecycleRecord)
        -> std::io::Result<()>
    {
        use std::io::Write;
        let current = directory.join(HOST_LIFECYCLE_FILE);
        let mut lines = Vec::new();
        if fs::metadata(&current).is_ok_and(|metadata| metadata.len() >= HOST_LIFECYCLE_MAX_BYTES) {
            match fs::rename(&current, directory.join(HOST_LIFECYCLE_PREVIOUS_FILE)) {
                // The new file starts with this instance's anchor.
                Ok(()) => lines.extend(anchor.iter().filter(|anchor| *anchor != record)),
                // A held file must not stop lifecycle records: overshoot the
                // bound until a later rotation succeeds.
                Err(error) => eprintln!("host lifecycle> rotation deferred: {}", error.kind()),
            }
        }
        lines.push(record);
        let mut bytes = Vec::new();
        for line in lines {
            serde_json::to_writer(&mut bytes, line).map_err(std::io::Error::other)?;
            bytes.push(b'\n');
        }
        let mut file = fs::OpenOptions::new().create(true).append(true).open(&current)?;
        file.write_all(&bytes)?;
        // One small durable append per record, at most once a minute when idle.
        file.sync_data()
    }
}

/// The process's working set and commit, from one cheap OS query.
fn host_process_memory() -> (Option<u64>, Option<u64>) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let mut counters: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        counters.cb = size;
        // SAFETY: the pseudo-handle needs no closing; counters is a sized,
        // writable PROCESS_MEMORY_COUNTERS.
        if unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) } != 0 {
            return (Some(counters.WorkingSetSize as u64), Some(counters.PagefileUsage as u64));
        }
        (None, None)
    }
    #[cfg(target_os = "linux")]
    {
        // statm: total and resident pages; Linux has no per-process commit.
        let resident = fs::read_to_string("/proc/self/statm").ok().and_then(|text| {
            text.split_whitespace().nth(1)?.parse::<u64>().ok()
        });
        // SAFETY: sysconf has no preconditions.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        (resident.filter(|_| page > 0).map(|pages| pages * page as u64), None)
    }
    #[cfg(target_os = "macos")]
    {
        let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
        // SAFETY: info is a writable proc_taskinfo of the size passed.
        let written = unsafe {
            libc::proc_pidinfo(std::process::id() as i32, libc::PROC_PIDTASKINFO, 0,
                (&mut info as *mut libc::proc_taskinfo).cast(), size)
        };
        if written == size { (Some(info.pti_resident_size), None) } else { (None, None) }
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        (None, None)
    }
}

fn host_binary_sha256() -> Option<String> {
    use sha2::Digest;
    let mut file = fs::File::open(std::env::current_exe().ok()?).ok()?;
    let mut hasher = sha2::Sha256::new();
    std::io::copy(&mut file, &mut hasher).ok()?;
    Some(format!("{:x}", hasher.finalize()))
}

/// This instance's journal and its previous instance's end.
struct HostLifecycle {
    journal: HostLifecycleJournal,
    instance_id: String,
    previous: HostPreviousEnd,
}

/// Asks the lifecycle thread to record a shutdown and stop.
#[derive(Default)]
struct HostLifecycleStop {
    requested: Mutex<Option<HostShutdownOutcome>>,
    wake: std::sync::Condvar,
}

impl HostLifecycleStop {
    fn request(&self, outcome: HostShutdownOutcome) {
        *self.requested.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(outcome);
        self.wake.notify_all();
    }
}

/// The running journal thread, for run_server to stop at shutdown.
struct HostLifecycleRuntime {
    stop: Arc<HostLifecycleStop>,
    thread: std::thread::JoinHandle<()>,
}

impl HostLifecycleRuntime {
    /// Records how the server ended and waits for the journal thread.
    fn finish(self, outcome: HostShutdownOutcome) {
        self.stop.request(outcome);
        let _ = self.thread.join();
    }
}

static HOST_LIFECYCLE: std::sync::OnceLock<Arc<HostLifecycle>> = std::sync::OnceLock::new();

impl HostLifecycle {
    /// Classifies the previous end, then records this boot.
    fn open(directory: PathBuf, instance_id: String, binary_sha256: Option<String>) -> Self {
        let journal = HostLifecycleJournal::new(directory);
        let previous = host_lifecycle_classify(&journal.read_records());
        if let Err(error) = journal.terminate_torn_line() {
            eprintln!("host lifecycle> torn last line not terminated: {}", error.kind());
        }
        let boot = HostLifecycleRecord::Boot { at: host_lifecycle_now(), instance_id: instance_id.clone(),
            pid: std::process::id(), binary_sha256 };
        if let Err(error) = journal.append_boot(boot) {
            eprintln!("host lifecycle> boot record not written: {}", error.kind());
        }
        Self { journal, instance_id, previous }
    }

    /// Starts the server's journal on its own thread, so hashing the binary
    /// and the boot, heartbeat and shutdown appends stay off the async startup
    /// path. The thread writes the boot record, installs the panic hook, then
    /// a heartbeat each minute until run_server asks it to record the shutdown.
    /// The panic record is the one exception: see record_panic.
    fn start(state: &AppState) -> Option<HostLifecycleRuntime> {
        let directory = state.persistence_path.parent()?.to_path_buf();
        let stop = Arc::new(HostLifecycleStop::default());
        let thread_stop = Arc::clone(&stop);
        let state = state.clone();
        let thread = std::thread::Builder::new()
            .name("termal-host-lifecycle".to_owned())
            .spawn(move || {
                let lifecycle = Arc::new(Self::open(directory, state.server_instance_id.clone(), host_binary_sha256()));
                if HOST_LIFECYCLE.set(Arc::clone(&lifecycle)).is_ok() {
                    let previous_hook = std::panic::take_hook();
                    std::panic::set_hook(Box::new(move |info| {
                        if let Some(lifecycle) = HOST_LIFECYCLE.get() {
                            lifecycle.record_panic(
                                std::thread::current().name().unwrap_or("unnamed"),
                                info.location().map(|location| host_lifecycle_location(location.file(), location.line())),
                                host_lifecycle_panic_message(info.payload()),
                            );
                        }
                        previous_hook(info);
                    }));
                }
                lifecycle.run(&state, &thread_stop);
            })
            // Nothing was written: the next boot sees no record of this one.
            .map_err(|error| eprintln!("host lifecycle> journal not started: {}", error.kind()))
            .ok()?;
        Some(HostLifecycleRuntime { stop, thread })
    }

    /// Written synchronously on the panicking thread, before the default hook
    /// runs. A panic can be the process's last act, so a hand-off to the
    /// journal thread could be lost. The cost is one small flushed append on a
    /// rare path, possibly while that thread still holds a lock; try_append
    /// skips rather than waits if another append is in flight.
    fn record_panic(&self, thread: &str, location: Option<String>, message: String) {
        let _ = self.journal.try_append(&HostLifecycleRecord::Panic { at: host_lifecycle_now(),
            instance_id: self.instance_id.clone(), thread: thread.to_owned(), location, message });
    }

    /// Counters taken without the state lock: the observer mutex, the state
    /// mutex's own long-hold slot and one OS memory query.
    fn heartbeat_counters(state: &AppState) -> HostHeartbeatCounters {
        let persist = state.persist_observer.take_heartbeat_counters();
        let hold = state.inner.take_longest_hold();
        let (working_set_bytes, committed_bytes) = host_process_memory();
        HostHeartbeatCounters {
            persist_ticks: persist.completed_ticks,
            persist_failed_ticks: persist.failed_ticks,
            persisted_payload_bytes: persist.payload_bytes,
            longest_persist_tick_us: persist.longest_tick_us,
            longest_state_hold_ms: hold.map(|hold| hold.held.as_millis().min(u64::MAX as u128) as u64),
            longest_state_hold_at: hold.map(|hold| host_lifecycle_location(hold.location.file(), hold.location.line())),
            working_set_bytes,
            committed_bytes,
        }
    }

    fn record_heartbeat(&self, state: &AppState) -> std::io::Result<()> {
        let counters = Self::heartbeat_counters(state);
        self.journal.append(&HostLifecycleRecord::Heartbeat { at: host_lifecycle_now(),
            instance_id: self.instance_id.clone(), counters })
    }

    fn record_shutdown(&self, outcome: HostShutdownOutcome) {
        if let Err(error) = self.journal.append(&HostLifecycleRecord::Shutdown { at: host_lifecycle_now(),
            instance_id: self.instance_id.clone(), outcome })
        {
            eprintln!("host lifecycle> shutdown record not written: {}", error.kind());
        }
    }

    /// Heartbeats each minute until a stop is requested, then records it.
    fn run(&self, state: &AppState, stop: &HostLifecycleStop) {
        let mut requested = stop.requested.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            // Checked before every wait: a stop requested before this thread
            // first waits must not cost a whole interval.
            if let Some(outcome) = *requested {
                drop(requested);
                self.record_shutdown(outcome);
                return;
            }
            let (guard, wait) = stop.wake.wait_timeout(requested, HOST_LIFECYCLE_HEARTBEAT)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            requested = guard;
            if requested.is_some() || !wait.timed_out() {
                continue;
            }
            // Never hold the stop flag across the append.
            drop(requested);
            if let Err(error) = self.record_heartbeat(state) {
                eprintln!("host lifecycle> heartbeat not written: {}", error.kind());
            }
            requested = stop.requested.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

async fn get_host_lifecycle_diagnostics(State(state): State<AppState>) -> Json<Value> {
    // No file, state or SQLite reads: the end classified at boot, once the
    // journal thread has read it.
    Json(host_lifecycle_diagnostics_body(&state.server_instance_id,
        HOST_LIFECYCLE.get().map(|lifecycle| &lifecycle.previous)))
}

/// The diagnostics response: `previousInstance` is null until the journal's
/// thread has classified the previous end.
fn host_lifecycle_diagnostics_body(server_instance_id: &str, previous: Option<&HostPreviousEnd>) -> Value {
    json!({ "serverInstanceId": server_instance_id,
        "journal": HOST_LIFECYCLE_FILE,
        "previousInstance": previous })
}
