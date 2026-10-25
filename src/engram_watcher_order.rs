// Owns the order in which the workspace watcher's notifications, launcher
// gates' starts and launcher runs' registrations reached the host (a
// process-local counter that never runs backward), the decision whether a
// watcher-seen change may follow a gate's launch, and the times a watcher
// refusal names. Does not own the fences
// themselves, the settlement that re-judges an exemption, or the watcher
// thread (src/engram_carried_checks.rs, src/workspace_watch.rs). New fragment,
// split out of src/engram_carried_checks.rs so that file does not grow
// further.

/// The process-local order of watcher notifications and command starts. It
/// never runs backward, unlike the wall clock, so it alone decides whether a
/// notification reached the host before a check's launch.
static ENGRAM_WATCHER_ORDER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// The next point of `ENGRAM_WATCHER_ORDER`.
fn engram_watcher_order_tick() -> u64 {
    ENGRAM_WATCHER_ORDER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

/// How many launcher runs' registration orders are remembered; the oldest go
/// first, and a run whose order is gone takes no watcher exemption.
const ENGRAM_RUN_REGISTRATION_ORDERS_MAX: usize = 4096;

/// The watcher order at which the host accepted each launcher run's first
/// registration, by the run directory's path key. The launcher registers a
/// run before it fingerprints its input or runs any stage, so a watcher
/// notification ordered before it reports a write the run never saw begin.
/// Memory only: a host restart forgets it, so a run registered before a
/// restart takes no watcher exemption.
static ENGRAM_RUN_REGISTRATION_ORDERS: std::sync::Mutex<std::collections::VecDeque<(String, u64)>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());

/// Records that the host accepted the first registration of the launcher run
/// in `run_directory`, already canonical, now in the watcher order. A run
/// already recorded keeps its first point, the earliest and so the strictest.
/// Reads no file, so it may run under the state lock.
fn engram_note_run_registered(run_directory: &FsPath) {
    let key = engram_path_key(run_directory);
    let mut orders = ENGRAM_RUN_REGISTRATION_ORDERS
        .lock()
        .expect("run registration orders poisoned");
    if orders.iter().any(|(known, _)| *known == key) {
        return;
    }
    if orders.len() >= ENGRAM_RUN_REGISTRATION_ORDERS_MAX {
        orders.pop_front();
    }
    orders.push_back((key, engram_watcher_order_tick()));
}

/// The watcher order at which this process accepted the first registration
/// of the launcher run in `run_directory`, if it did and still remembers it.
/// Resolves the directory first, so call it off the state lock.
fn engram_run_registration_order(run_directory: &FsPath) -> Option<u64> {
    let key = engram_path_key(&fs::canonicalize(run_directory).ok()?);
    ENGRAM_RUN_REGISTRATION_ORDERS
        .lock()
        .expect("run registration orders poisoned")
        .iter()
        .find(|(known, _)| *known == key)
        .map(|(_, order)| *order)
}

/// Records in `exempted` a watcher change, with `cause`, that did not fence a
/// gate because its notification arrived at `arrival`, before the gate's
/// launch: the first cause is kept, and the latest such arrival, which the
/// run's registration must follow for the exemption to hold.
fn engram_note_watcher_exemption(
    exempted: &mut Option<(String, u64)>,
    cause: &str,
    arrival: Option<EngramWatcherArrival>,
) {
    // An exemption is only ever taken for a known arrival.
    let Some(arrival) = arrival else {
        return;
    };
    match exempted {
        Some((_, latest)) => *latest = (*latest).max(arrival.order),
        None => *exempted = Some((cause.to_owned(), arrival.order)),
    }
}

/// When a watcher notification reached the host: its place in the watcher
/// order, which decides, and its wall-clock time, which is only shown.
#[derive(Clone, Copy, Debug)]
struct EngramWatcherArrival {
    order: u64,
    at: std::time::SystemTime,
}

/// The arrival of a notification reaching the host now. The watcher's notify
/// callback takes it before the event waits behind earlier events.
fn engram_watcher_arrival_now() -> EngramWatcherArrival {
    EngramWatcherArrival {
        order: engram_watcher_order_tick(),
        at: std::time::SystemTime::now(),
    }
}

/// Whether a watcher notification that arrived at `arrival` may report a
/// write at or after the launch of a check whose start took the watcher order
/// point `started_order`. One that arrived strictly before that start reports
/// a write already done then, so in the launch's source snapshot (taken after
/// the start), however long it then waited before it was handled. The
/// changed file's own modification time is never trusted for this: tools can
/// set an old one, and a rename can bring in an old file, during the run. Nor
/// is the wall clock, which can step backward. A notification of unknown
/// arrival, or a check without a start point, may report a later write.
///
/// Residual, accepted: a notification the operating system itself delivers
/// late, after the launch, for a write before it, still fences the check.
fn engram_watcher_change_may_follow_launch(
    arrival: Option<EngramWatcherArrival>,
    started_order: Option<u64>,
) -> bool {
    match (arrival, started_order) {
        (Some(arrival), Some(started)) => arrival.order >= started,
        _ => true,
    }
}

/// Whether a change modified at `candidate` should be the one a worktree's
/// watcher cause names over one modified at `current`: a change without a
/// modification time names the least known, so it wins; otherwise the later.
fn engram_watcher_change_is_later(candidate: Option<u64>, current: Option<u64>) -> bool {
    match (candidate, current) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(candidate), Some(current)) => candidate > current,
    }
}

/// The modification time a batch of watcher changes too widespread to place
/// is named by: none when any change has none, else the latest.
fn engram_watcher_changes_latest_mtime(changes: &[WorkspaceFileChangeEvent]) -> Option<u64> {
    changes
        .iter()
        .map(|change| change.mtime_ms)
        .try_fold(0u64, |latest, mtime| mtime.map(|mtime| latest.max(mtime)))
}

/// The cause a watcher-seen change gives a check it fences. It names the
/// change's modification time and, when known, when its notification reached
/// the host, so the holder can tell a late report of an earlier change from
/// a write while the check ran.
fn engram_watcher_change_cause(
    shown: &str,
    mtime_ms: Option<u64>,
    arrival: Option<EngramWatcherArrival>,
) -> String {
    let shown_time = |time: chrono::DateTime<chrono::Utc>| {
        time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    };
    let modified = mtime_ms
        .and_then(|mtime| i64::try_from(mtime).ok())
        .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
        .map(|time| format!("modified at {}", shown_time(time)))
        .unwrap_or_else(|| "modification time unknown".to_owned());
    let seen = arrival
        .map(|arrival| arrival.at)
        .map(|time| {
            format!(
                ", seen at {}",
                shown_time(chrono::DateTime::<chrono::Utc>::from(time))
            )
        })
        .unwrap_or_default();
    format!("a file in its worktree changed: {shown}; {modified}{seen}")
}
