/*
SQLite-backed session state persistence.

Owns connection lifecycle, writer admission, path-aware schema setup,
global state loading and persistence transaction orchestration. Startup uses
`ensure_sqlite_state_schema_for_load` (including metadata authority checks),
while already boot-validated stores use bounded `ensure_sqlite_state_schema`
on write-connection reopens. Also owns connection
lifecycle (`open_sqlite_state_connection`, `SqlitePersistConnectionCache`),
load path (`load_state_for_boot`, `load_state_from_sqlite_with_connection`),
and the transaction entry points used by the background persist thread
(`persist_state_parts_via_connection`, `persist_delta_via_cache`,
`persist_created_session`, `persist_locked_state_to_sqlite`, `persist_state`).
Current-schema validation, initialization and authority checks live in
`persist_sqlite_schema.rs`; their unchanged regressions live in
`persist_sqlite_schema_tests.rs`. Normalized session quarantine and transcript
reads live in `persist_sqlite_transcript_read.rs`; session serialization and
transaction-local writes live in `persist_sqlite_transcript_write.rs`.
Overview-specific schema upgrades and backfill live in
`persist_sqlite_overview.rs`. Physical transcript layout, rowid conversion and
differential message writes live in `persist_sqlite_messages.rs`.

Extracted from `api.rs` so HTTP handler code and SQLite persistence live
in separate files. The crate still compiles as one `include!()`-assembled
module, so no visibility changes are required.
*/

/// Resolves persistence path.
fn resolve_persistence_path(default_workdir: &str) -> PathBuf {
    resolve_termal_data_dir(default_workdir).join("termal.sqlite")
}

// The joint floor inherits units-2 and ordinary handoff, and additionally
// requires canonical containment, retirement and checkpoint custody metadata.
const SQLITE_METADATA_UNITS_VERSION: &str = "2-metadata-units-2";
const SQLITE_OBSERVATION_PREDECESSOR: &str = "2-metadata-units-2-handoff-1";
const SQLITE_OBSERVATION_SCHEMA_VERSION: &str = "2-metadata-units-2-handoff-1-unadmitted-outbox-1";
const SQLITE_CHECKPOINT_RESULT_PREDECESSOR: &str = "2-metadata-units-2-handoff-1-unadmitted-outbox-1-obligations-1";
const SQLITE_SCHEMA_VERSION: &str = "2-metadata-units-2-handoff-1-unadmitted-outbox-1-obligations-1-checkpoint-result-1";
const SQLITE_METADATA_KEY: &str = "metadataState";
const SQLITE_PROMPT_HISTORY_STORAGE_KEY: &str = "prompt_history_storage_version";
const SQLITE_PROMPT_HISTORY_STORAGE_VERSION: &str = "1";
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const SQLITE_SESSION_TAIL_MESSAGES: usize = 64;

/// Per-database writer locks shared by every in-process SQLite write path.
///
/// WAL lets readers coexist, but SQLite still permits only one writer. The
/// state persist worker has its own database domain. Within the separate
/// coordination database, mailbox and board stores own independent
/// connections, so relying on SQLite's busy timeout alone can surface ordinary
/// in-process contention as `SQLITE_BUSY`. Serialize writers targeting the same
/// path before `BEGIN`; the timeout remains a boundary for external processes
/// or OS-level locks.
static SQLITE_STATE_WRITE_LOCKS: LazyLock<
    Mutex<HashMap<PathBuf, std::sync::Weak<SqliteStateWriterAdmission>>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Default)]
struct SqliteStateWriterAdmissionState {
    next_ticket: u64,
    serving_ticket: u64,
    canceled_tickets: BTreeSet<u64>,
}

#[derive(Default)]
struct SqliteStateWriterAdmission {
    state: Mutex<SqliteStateWriterAdmissionState>,
    changed: Condvar,
}

struct SqliteStateWriterGuard<'a> {
    admission: &'a SqliteStateWriterAdmission,
    ticket: u64,
}

static SQLITE_STATE_WRITER_POISON_WARNING_EMITTED: AtomicBool = AtomicBool::new(false);

fn lock_sqlite_state_writer_admission(
    admission: &SqliteStateWriterAdmission,
) -> std::sync::MutexGuard<'_, SqliteStateWriterAdmissionState> {
    admission.state.lock().unwrap_or_else(|poisoned| {
        if !SQLITE_STATE_WRITER_POISON_WARNING_EMITTED.swap(true, Ordering::Relaxed) {
            eprintln!("[termal] warning: recovered a poisoned SQLite state writer admission");
        }
        poisoned.into_inner()
    })
}

fn wait_sqlite_state_writer_admission<'a>(
    admission: &'a SqliteStateWriterAdmission,
    state: std::sync::MutexGuard<'a, SqliteStateWriterAdmissionState>,
) -> std::sync::MutexGuard<'a, SqliteStateWriterAdmissionState> {
    admission.changed.wait(state).unwrap_or_else(|poisoned| {
        if !SQLITE_STATE_WRITER_POISON_WARNING_EMITTED.swap(true, Ordering::Relaxed) {
            eprintln!("[termal] warning: recovered a poisoned SQLite state writer admission");
        }
        poisoned.into_inner()
    })
}

fn wait_timeout_sqlite_state_writer_admission<'a>(
    admission: &'a SqliteStateWriterAdmission,
    state: std::sync::MutexGuard<'a, SqliteStateWriterAdmissionState>,
    timeout: Duration,
) -> (
    std::sync::MutexGuard<'a, SqliteStateWriterAdmissionState>,
    std::sync::WaitTimeoutResult,
) {
    admission
        .changed
        .wait_timeout(state, timeout)
        .unwrap_or_else(|poisoned| {
            if !SQLITE_STATE_WRITER_POISON_WARNING_EMITTED.swap(true, Ordering::Relaxed) {
                eprintln!("[termal] warning: recovered a poisoned SQLite state writer admission");
            }
            poisoned.into_inner()
        })
}

fn issue_sqlite_state_writer_ticket(
    admission: &SqliteStateWriterAdmission,
    state: &mut SqliteStateWriterAdmissionState,
) -> u64 {
    let ticket = state.next_ticket;
    state.next_ticket = state
        .next_ticket
        .checked_add(1)
        .expect("SQLite state writer ticket space exhausted");
    admission.changed.notify_all();
    ticket
}

fn advance_past_canceled_sqlite_state_writer_tickets(state: &mut SqliteStateWriterAdmissionState) {
    while state.canceled_tickets.remove(&state.serving_ticket) {
        state.serving_ticket = state
            .serving_ticket
            .checked_add(1)
            .expect("SQLite state writer ticket space exhausted");
    }
}

impl Drop for SqliteStateWriterGuard<'_> {
    fn drop(&mut self) {
        let mut state = lock_sqlite_state_writer_admission(self.admission);
        debug_assert_eq!(
            state.serving_ticket, self.ticket,
            "SQLite state writer guard released out of FIFO order"
        );
        state.serving_ticket = state
            .serving_ticket
            .checked_add(1)
            .expect("SQLite state writer ticket space exhausted");
        advance_past_canceled_sqlite_state_writer_tickets(&mut state);
        self.admission.changed.notify_all();
    }
}

fn sqlite_state_write_lock(path: &FsPath) -> Arc<SqliteStateWriterAdmission> {
    let mut locks = SQLITE_STATE_WRITE_LOCKS
        .lock()
        .expect("SQLite state write-lock registry poisoned");
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(path).and_then(std::sync::Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(SqliteStateWriterAdmission::default());
    locks.insert(path.to_path_buf(), Arc::downgrade(&lock));
    lock
}

fn lock_sqlite_state_writer(lock: &SqliteStateWriterAdmission) -> SqliteStateWriterGuard<'_> {
    let mut state = lock_sqlite_state_writer_admission(lock);
    let ticket = issue_sqlite_state_writer_ticket(lock, &mut state);
    while state.serving_ticket != ticket {
        state = wait_sqlite_state_writer_admission(lock, state);
    }
    drop(state);
    SqliteStateWriterGuard {
        admission: lock,
        ticket,
    }
}

fn lock_sqlite_state_writer_for(
    lock: &SqliteStateWriterAdmission,
    timeout: Duration,
) -> Option<SqliteStateWriterGuard<'_>> {
    let deadline = std::time::Instant::now() + timeout;
    let mut state = lock_sqlite_state_writer_admission(lock);
    let ticket = issue_sqlite_state_writer_ticket(lock, &mut state);
    while state.serving_ticket != ticket {
        let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
            state.canceled_tickets.insert(ticket);
            advance_past_canceled_sqlite_state_writer_tickets(&mut state);
            lock.changed.notify_all();
            return None;
        };
        let (next_state, wait_result) =
            wait_timeout_sqlite_state_writer_admission(lock, state, remaining);
        state = next_state;
        if wait_result.timed_out() && state.serving_ticket != ticket {
            state.canceled_tickets.insert(ticket);
            advance_past_canceled_sqlite_state_writer_tickets(&mut state);
            lock.changed.notify_all();
            return None;
        }
    }
    drop(state);
    Some(SqliteStateWriterGuard {
        admission: lock,
        ticket,
    })
}

#[cfg(test)]
fn sqlite_state_writer_issued_tickets(lock: &SqliteStateWriterAdmission) -> u64 {
    lock_sqlite_state_writer_admission(lock).next_ticket
}

#[cfg(test)]
fn wait_for_sqlite_state_writer_issued_tickets(lock: &SqliteStateWriterAdmission, expected: u64) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut state = lock_sqlite_state_writer_admission(lock);
    while state.next_ticket < expected {
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .expect("SQLite writer ticket should be issued before diagnostic deadline");
        let (next_state, wait_result) =
            wait_timeout_sqlite_state_writer_admission(lock, state, remaining);
        state = next_state;
        assert!(
            !wait_result.timed_out() || state.next_ticket >= expected,
            "SQLite writer ticket should be issued before diagnostic deadline"
        );
    }
}

/// Opens and hardens the main file and any existing sidecars before persistent
/// PRAGMAs or schema maintenance. This handle is not ready for writes: callers
/// must complete the applicable path-aware schema setup (including foreign-key
/// enforcement and post-WAL hardening), or explicitly configure it themselves.
fn open_sqlite_state_connection_unconfigured(path: &FsPath) -> Result<rusqlite::Connection> {
    if let Some(parent) = path.parent() {
        harden_local_state_directory_permissions(parent)?;
    }
    reject_existing_sqlite_state_file_symlinks(path)?;
    let connection = rusqlite::Connection::open(path)
        .with_context(|| format!("failed to open `{}`", path.display()))?;
    connection
        .busy_timeout(SQLITE_BUSY_TIMEOUT)
        .with_context(|| format!("failed to set SQLite busy timeout for `{}`", path.display()))?;
    harden_sqlite_state_file_permissions(path)?;
    Ok(connection)
}

fn configure_sqlite_state_connection(connection: &rusqlite::Connection) -> Result<()> {
    // WAL lets readers coexist with the background persistence writer. NORMAL
    // sync is the common local-app tradeoff: durable enough for TermAl state,
    // with much lower fsync cost than FULL on every small create-session write.
    connection
        .execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA foreign_keys = ON;
            ",
        )
        .context("failed to configure SQLite state pragmas")
}

fn open_sqlite_state_connection(path: &FsPath) -> Result<rusqlite::Connection> {
    let connection = open_sqlite_state_connection_unconfigured(path)?;
    let setup = {
        let write_lock = sqlite_state_write_lock(path);
        let _write_guard = lock_sqlite_state_writer(&write_lock);
        let setup = configure_sqlite_state_connection(&connection).with_context(|| {
            format!(
                "failed to configure SQLite pragmas for `{}`",
                path.display()
            )
        });
        finish_sqlite_state_file_setup(path, setup)
    };
    setup?;
    Ok(connection)
}

/// The opener hardens existing files before PRAGMAs, but enabling WAL or doing
/// maintenance can create new WAL/SHM/journal files. Always perform this second
/// pass, including after a setup error while the connection still owns them.
/// Preserve the original setup error verbatim; report any additional hardening
/// failure separately. Successful setup must never hide a hardening failure.
/// Call while holding the path's writer guard, before releasing the connection,
/// so another in-process writer cannot create sidecars between setup and chmod.
fn finish_sqlite_state_file_setup<T>(path: &FsPath, setup: Result<T>) -> Result<T> {
    let hardening = harden_sqlite_state_file_permissions(path);
    resolve_sqlite_state_setup_result(path, setup, hardening)
}

/// Keep error precedence independent of the platform-specific permission pass.
/// Rewrapping a setup failure would change existing fail-closed error chains;
/// the secondary diagnostic includes both failures so it stands on its own.
fn resolve_sqlite_state_setup_result<T>(
    path: &FsPath,
    setup: Result<T>,
    hardening: Result<()>,
) -> Result<T> {
    match setup {
        Ok(value) => hardening.map(|()| value),
        Err(error) => {
            if let Err(hardening_error) = hardening {
                eprintln!(
                    "persist> setup failed for `{}`: {error:#}; additionally failed to harden SQLite files: {hardening_error:#}",
                    path.display()
                );
            }
            Err(error)
        }
    }
}

fn open_sqlite_state_read_connection(path: &FsPath) -> Result<rusqlite::Connection> {
    reject_existing_sqlite_state_path_redirection(path)?;
    let connection = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("failed to open `{}` for transcript paging", path.display()))?;
    connection
        .busy_timeout(SQLITE_BUSY_TIMEOUT)
        .with_context(|| format!("failed to set SQLite read timeout for `{}`", path.display()))?;
    connection
        .execute_batch("PRAGMA query_only = ON;")
        .with_context(|| {
            format!(
                "failed to configure SQLite transcript reader for `{}`",
                path.display()
            )
        })?;
    Ok(connection)
}

fn open_sqlite_history_snapshot(path: &FsPath) -> Result<rusqlite::Connection> {
    let connection = open_sqlite_state_read_connection(path)?;
    // Cursor resolution and range loading are separate statements. Keep them
    // in one read transaction so a concurrent writer cannot move the cursor
    // between those statements.
    connection
        .execute_batch("BEGIN DEFERRED TRANSACTION;")
        .with_context(|| {
            format!(
                "failed to start SQLite transcript read snapshot for `{}`",
                path.display()
            )
        })?;
    Ok(connection)
}

#[cfg(unix)]
fn allow_insecure_state_permissions() -> bool {
    std::env::var("TERMAL_ALLOW_INSECURE_STATE_PERMISSIONS")
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

#[cfg(unix)]
fn permission_hardening_failure(path: &FsPath, detail: impl std::fmt::Display) -> Result<()> {
    let message = format!(
        "failed to restrict permissions on `{}`: {detail}",
        path.display()
    );
    if allow_insecure_state_permissions() {
        eprintln!("[termal] warning: {message}");
        Ok(())
    } else {
        Err(anyhow!(message))
    }
}

#[cfg(unix)]
fn harden_local_state_file_permissions(path: &FsPath) -> Result<()> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::fs::PermissionsExt;

    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(err) => return permission_hardening_failure(path, err),
    };
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
        permission_hardening_failure(path, io::Error::last_os_error())?;
    }

    let actual_mode = match file.metadata() {
        Ok(metadata) => metadata.permissions().mode() & 0o777,
        Err(err) => return permission_hardening_failure(path, err),
    };
    if actual_mode & 0o077 != 0 {
        permission_hardening_failure(
            path,
            format!("mode {actual_mode:o} still grants group or other access"),
        )?;
    }
    Ok(())
}

#[cfg(unix)]
fn harden_local_state_directory_permissions(path: &FsPath) -> Result<()> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::fs::PermissionsExt;

    reject_existing_state_directory_redirection_unix(path)?;
    let directory = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(directory) => directory,
        Err(err) => return permission_hardening_failure(path, err),
    };
    if unsafe { libc::fchmod(directory.as_raw_fd(), 0o700) } != 0 {
        permission_hardening_failure(path, io::Error::last_os_error())?;
    }

    let actual_mode = match directory.metadata() {
        Ok(metadata) => metadata.permissions().mode() & 0o777,
        Err(err) => return permission_hardening_failure(path, err),
    };
    if actual_mode & 0o077 != 0 {
        permission_hardening_failure(
            path,
            format!("mode {actual_mode:o} still grants group or other access"),
        )?;
    }
    Ok(())
}

#[cfg(unix)]
fn reject_existing_state_directory_redirection(path: &FsPath) -> Result<()> {
    reject_existing_state_directory_redirection_unix(path)
}

#[cfg(windows)]
fn harden_local_state_directory_permissions(path: &FsPath) -> Result<()> {
    reject_existing_windows_state_path_redirection(path)
}

#[cfg(windows)]
fn reject_existing_state_directory_redirection(path: &FsPath) -> Result<()> {
    reject_existing_windows_state_path_redirection(path)
}

#[cfg(all(not(test), not(unix), not(windows)))]
fn harden_local_state_directory_permissions(_path: &FsPath) -> Result<()> {
    Ok(())
}

#[cfg(all(test, not(unix), not(windows)))]
fn harden_local_state_directory_permissions(_path: &FsPath) -> Result<()> {
    Ok(())
}

#[cfg(all(not(test), not(unix), not(windows)))]
fn reject_existing_state_directory_redirection(_path: &FsPath) -> Result<()> {
    Ok(())
}

#[cfg(all(not(test), unix))]
fn create_local_state_directory(path: &FsPath) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .with_context(|| format!("failed to create `{}`", path.display()))?;
    harden_local_state_directory_permissions(path)?;
    Ok(())
}

#[cfg(all(not(test), not(unix)))]
fn create_local_state_directory(path: &FsPath) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("failed to create `{}`", path.display()))
}

#[cfg(unix)]
fn harden_sqlite_state_file_permissions(path: &FsPath) -> Result<()> {
    harden_existing_state_file_permissions(path)?;
    harden_existing_state_file_permissions(&sqlite_sidecar_path(path, "-wal"))?;
    harden_existing_state_file_permissions(&sqlite_sidecar_path(path, "-shm"))?;
    harden_existing_state_file_permissions(&sqlite_sidecar_path(path, "-journal"))?;
    Ok(())
}

fn harden_persist_commit_files(path: &FsPath) -> Result<()> {
    harden_sqlite_state_file_permissions(path).with_context(|| {
        format!(
            "committed persisted state to `{}` but failed to re-harden state files",
            path.display()
        )
    })
}

#[cfg(test)]
thread_local! {
    static PERSIST_MESSAGE_COMMIT_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
    static METADATA_COMMIT_INTEGRITY_FAULT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn verify_persist_commit_integrity(path: &FsPath) -> Result<()> {
    #[cfg(test)]
    if METADATA_COMMIT_INTEGRITY_FAULT.with(|fault| fault.replace(false)) {
        return Err(anyhow!("injected metadata post-commit integrity failure"));
    }
    let hardening_result = harden_persist_commit_files(path);
    if let Err(redirection_err) = reject_existing_sqlite_state_path_redirection(path) {
        if let Err(err) = &hardening_result {
            eprintln!(
                "backend warning> committed persisted state to `{}` but failed to re-harden \
                 state files before post-commit redirection check failed: {err:#}",
                path.display()
            );
        }
        return Err(redirection_err).with_context(|| {
            if let Err(err) = &hardening_result {
                format!("post-commit redirection check failed after hardening error: {err}")
            } else {
                format!(
                    "post-commit redirection check failed after hardening `{}`",
                    path.display()
                )
            }
        });
    }
    hardening_result
}

#[cfg(all(not(test), not(unix)))]
fn harden_sqlite_state_file_permissions(_path: &FsPath) -> Result<()> {
    Ok(())
}

#[cfg(all(test, not(unix)))]
fn harden_sqlite_state_file_permissions(_path: &FsPath) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn reject_existing_sqlite_state_file_symlinks(path: &FsPath) -> Result<()> {
    reject_existing_state_file_symlink(path)?;
    reject_existing_state_file_symlink(&sqlite_sidecar_path(path, "-wal"))?;
    reject_existing_state_file_symlink(&sqlite_sidecar_path(path, "-shm"))?;
    reject_existing_state_file_symlink(&sqlite_sidecar_path(path, "-journal"))?;
    Ok(())
}

#[cfg(windows)]
fn reject_existing_sqlite_state_file_symlinks(path: &FsPath) -> Result<()> {
    reject_existing_windows_state_path_redirection(path)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let sidecar = sqlite_sidecar_path(path, suffix);
        reject_windows_state_path_metadata_redirection(
            &sidecar,
            windows_sqlite_sidecar_symlink_metadata(&sidecar),
        )?;
    }
    Ok(())
}

#[cfg(all(not(test), not(unix), not(windows)))]
fn reject_existing_sqlite_state_file_symlinks(_path: &FsPath) -> Result<()> {
    Ok(())
}

#[cfg(all(test, not(unix), not(windows)))]
fn reject_existing_sqlite_state_file_symlinks(_path: &FsPath) -> Result<()> {
    Ok(())
}

#[cfg(all(test, windows))]
thread_local! {
    static WINDOWS_STATE_PATH_METADATA_FAILURES: std::cell::RefCell<std::collections::VecDeque<(PathBuf, i32)>> = const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
    static WINDOWS_STATE_PATH_METADATA_PAUSES: std::cell::RefCell<Vec<Duration>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(windows)]
fn windows_state_path_symlink_metadata(path: &FsPath) -> io::Result<fs::Metadata> {
    #[cfg(test)]
    if let Some(code) = WINDOWS_STATE_PATH_METADATA_FAILURES.with(|slot| {
        let mut failures = slot.borrow_mut();
        if failures.front().is_some_and(|(target, _)| target == path) {
            failures.pop_front().map(|(_, code)| code)
        } else {
            None
        }
    }) {
        return Err(io::Error::from_raw_os_error(code));
    }
    fs::symlink_metadata(path)
}

#[cfg(windows)]
const WINDOWS_SQLITE_SIDECAR_INSPECTION_BACKOFF_MS: [u64; 4] = [1, 4, 16, 64];

#[cfg(windows)]
fn windows_sqlite_sidecar_symlink_metadata(path: &FsPath) -> io::Result<fs::Metadata> {
    // Sidecar inspection can briefly fail before writer admission. A denial
    // is never proof of absence or a safe path.
    // Reinspect only these transient Windows errors, with a fixed attempt cap;
    // main-file and directory checks keep their original one-shot behavior.
    let is_contention = |err: &io::Error| matches!(err.raw_os_error(), Some(5 | 32));
    let first_error = match windows_state_path_symlink_metadata(path) {
        Err(err) if is_contention(&err) => err,
        result => return result,
    };
    for delay_ms in WINDOWS_SQLITE_SIDECAR_INSPECTION_BACKOFF_MS {
        let delay = Duration::from_millis(delay_ms);
        #[cfg(test)]
        WINDOWS_STATE_PATH_METADATA_PAUSES.with(|slot| slot.borrow_mut().push(delay));
        std::thread::sleep(delay);
        match windows_state_path_symlink_metadata(path) {
            Err(err) if is_contention(&err) => {}
            result => return result,
        }
    }
    Err(first_error)
}

/// Rejects Windows reparse points before SQLite can open the TermAl state
/// directory, database, or sidecars. A reparse point can redirect persisted
/// session history through a symlink, junction, or mount point; this is path
/// integrity, not Unix-style chmod hardening, so the insecure-permissions
/// escape hatch intentionally does not apply. `0x400` is the stable
/// `FILE_ATTRIBUTE_REPARSE_POINT` value; spelling it locally avoids adding a
/// Windows API crate only for this metadata bit.
#[cfg(windows)]
fn reject_existing_windows_state_path_redirection(path: &FsPath) -> Result<()> {
    reject_windows_state_path_metadata_redirection(path, windows_state_path_symlink_metadata(path))
}

#[cfg(windows)]
fn reject_windows_state_path_metadata_redirection(
    path: &FsPath,
    metadata: io::Result<fs::Metadata>,
) -> Result<()> {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

    match metadata {
        Ok(metadata) if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 => {
            Err(anyhow!(
                "refusing to follow redirected state path `{}`",
                path.display()
            ))
        }
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => {
            Err(err).with_context(|| format!("failed to inspect state path `{}`", path.display()))
        }
    }
}

fn reject_existing_sqlite_state_path_redirection(path: &FsPath) -> Result<()> {
    if let Some(parent) = path.parent() {
        reject_existing_state_directory_redirection(parent)?;
    }
    reject_existing_sqlite_state_file_symlinks(path)
}

#[cfg(test)]
fn create_local_state_directory(path: &FsPath) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("failed to create `{}`", path.display()))?;
    #[cfg(unix)]
    harden_local_state_directory_permissions(path)?;
    Ok(())
}

#[cfg(all(test, not(unix), not(windows)))]
fn reject_existing_state_directory_redirection(_path: &FsPath) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn reject_existing_state_file_symlink(path: &FsPath) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(anyhow!(
            "refusing to follow symlinked state path `{}`",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => permission_hardening_failure(path, err),
    }
}

#[cfg(unix)]
fn reject_existing_state_directory_redirection_unix(path: &FsPath) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(anyhow!(
            "refusing to use symlinked state directory `{}`",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => permission_hardening_failure(path, err),
    }
}

#[cfg(unix)]
fn harden_existing_state_file_permissions(path: &FsPath) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            reject_existing_state_file_symlink(path)
        }
        Ok(metadata) if metadata.is_file() => harden_local_state_file_permissions(path),
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => permission_hardening_failure(path, err),
    }
}

#[cfg(any(unix, windows))]
fn sqlite_sidecar_path(path: &FsPath, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

#[cfg(all(test, unix))]
mod state_permission_hardening_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::fs::symlink;

    static ENV_MUTEX: std::sync::LazyLock<std::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

    fn temp_permission_root() -> PathBuf {
        let root = test_temp_dir().join(format!("termal-state-permissions-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create temp permission root");
        root
    }

    fn mode(path: &FsPath) -> u32 {
        fs::metadata(path)
            .expect("inspect mode")
            .permissions()
            .mode()
            & 0o777
    }

    fn set_mode(path: &FsPath, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("set broad test mode");
    }

    #[test]
    fn state_file_hardening_sets_owner_only_file_mode() {
        let root = temp_permission_root();
        let file = root.join("termal.sqlite");
        fs::write(&file, b"state").expect("write temp file");
        set_mode(&file, 0o666);

        harden_local_state_file_permissions(&file).expect("harden state file");

        assert_eq!(mode(&file), 0o600);
        remove_test_directory(root);
    }

    #[test]
    fn state_directory_hardening_sets_owner_only_directory_mode() {
        let root = temp_permission_root();
        let dir = root.join("state-dir");
        fs::create_dir(&dir).expect("create temp dir");
        set_mode(&dir, 0o777);

        harden_local_state_directory_permissions(&dir).expect("harden state dir");

        assert_eq!(mode(&dir), 0o700);
        remove_test_directory(root);
    }

    #[test]
    fn state_directory_hardening_rejects_symlinked_directories() {
        let root = temp_permission_root();
        let target = root.join("outside-state-dir");
        let link = root.join("state-dir-link");
        fs::create_dir(&target).expect("create state directory target");
        set_mode(&target, 0o777);
        symlink(&target, &link).expect("create state directory symlink");

        let error = harden_local_state_directory_permissions(&link)
            .expect_err("symlinked state directory should be rejected");

        assert!(format!("{error:#}").contains("symlinked state directory"));
        assert_eq!(mode(&target), 0o777);
        remove_test_directory(root);
    }

    #[test]
    fn sqlite_state_hardening_covers_main_file_and_sidecars() {
        let root = temp_permission_root();
        let db = root.join("termal.sqlite");
        let paths = [
            db.clone(),
            sqlite_sidecar_path(&db, "-wal"),
            sqlite_sidecar_path(&db, "-shm"),
            sqlite_sidecar_path(&db, "-journal"),
        ];
        for path in &paths {
            fs::write(path, b"state").expect("write sqlite state file");
            set_mode(path, 0o666);
        }

        harden_sqlite_state_file_permissions(&db).expect("harden sqlite state files");

        for path in &paths {
            assert_eq!(mode(path), 0o600, "{}", path.display());
        }
        remove_test_directory(root);
    }

    #[test]
    fn existing_state_file_hardening_rejects_symlinks() {
        let root = temp_permission_root();
        let target = root.join("outside-target");
        let link = root.join("termal.sqlite-wal");
        fs::write(&target, b"target").expect("write symlink target");
        set_mode(&target, 0o644);
        symlink(&target, &link).expect("create state-file sidecar symlink");

        let error = harden_existing_state_file_permissions(&link)
            .expect_err("symlink sidecar should be rejected");

        assert!(format!("{error:#}").contains("symlinked state path"));
        assert_eq!(mode(&target), 0o644);
        remove_test_directory(root);
    }

    #[test]
    fn sqlite_state_hardening_rejects_symlinked_main_and_sidecar_paths() {
        let root = temp_permission_root();
        let main_target = root.join("outside-main");
        let sidecar_target = root.join("outside-wal");
        let db = root.join("termal.sqlite");
        fs::write(&main_target, b"main").expect("write main target");
        fs::write(&sidecar_target, b"wal").expect("write sidecar target");
        set_mode(&main_target, 0o644);
        set_mode(&sidecar_target, 0o644);
        symlink(&main_target, &db).expect("create main symlink");

        let main_error = harden_sqlite_state_file_permissions(&db)
            .expect_err("symlinked main database should be rejected");
        assert!(format!("{main_error:#}").contains("symlinked state path"));

        fs::remove_file(&db).expect("remove main symlink");
        fs::write(&db, b"state").expect("write real main database");
        symlink(&sidecar_target, sqlite_sidecar_path(&db, "-wal")).expect("create sidecar symlink");

        let sidecar_error = harden_sqlite_state_file_permissions(&db)
            .expect_err("symlinked sidecar should be rejected");
        assert!(format!("{sidecar_error:#}").contains("symlinked state path"));
        assert_eq!(mode(&main_target), 0o644);
        assert_eq!(mode(&sidecar_target), 0o644);
        remove_test_directory(root);
    }

    #[test]
    fn persist_commit_integrity_rehardens_sqlite_files_after_commit() {
        let root = temp_permission_root();
        let db = root.join("termal.sqlite");
        let paths = [
            db.clone(),
            sqlite_sidecar_path(&db, "-wal"),
            sqlite_sidecar_path(&db, "-shm"),
            sqlite_sidecar_path(&db, "-journal"),
        ];
        for path in &paths {
            fs::write(path, b"state").expect("write sqlite state file");
            set_mode(path, 0o666);
        }

        verify_persist_commit_integrity(&db).expect("post-commit integrity should pass");

        for path in &paths {
            assert_eq!(mode(path), 0o600, "{}", path.display());
        }
        remove_test_directory(root);
    }

    #[test]
    fn persist_commit_integrity_rejects_post_commit_redirection() {
        let root = temp_permission_root();
        let db = root.join("termal.sqlite");
        let sidecar_target = root.join("outside-wal");
        fs::write(&db, b"state").expect("write sqlite state file");
        fs::write(&sidecar_target, b"wal").expect("write sidecar target");
        symlink(&sidecar_target, sqlite_sidecar_path(&db, "-wal")).expect("create sidecar symlink");

        let error =
            verify_persist_commit_integrity(&db).expect_err("post-commit symlink should be fatal");

        assert!(format!("{error:#}").contains("post-commit redirection check failed"));
        assert!(format!("{error:#}").contains("symlinked state path"));
        remove_test_directory(root);
    }

    #[test]
    fn insecure_state_permission_override_does_not_allow_symlinks() {
        let _guard = ENV_MUTEX
            .lock()
            .expect("state permission env mutex poisoned");
        let original = std::env::var_os("TERMAL_ALLOW_INSECURE_STATE_PERMISSIONS");
        unsafe {
            std::env::set_var("TERMAL_ALLOW_INSECURE_STATE_PERMISSIONS", "true");
        }
        let root = temp_permission_root();
        let target = root.join("outside-target");
        let link = root.join("termal.sqlite");
        fs::write(&target, b"target").expect("write symlink target");
        symlink(&target, &link).expect("create state-file symlink");

        let error = reject_existing_sqlite_state_file_symlinks(&link)
            .expect_err("symlink refusal should ignore insecure-permission override");

        assert!(format!("{error:#}").contains("symlinked state path"));
        remove_test_directory(root);
        unsafe {
            if let Some(value) = original {
                std::env::set_var("TERMAL_ALLOW_INSECURE_STATE_PERMISSIONS", value);
            } else {
                std::env::remove_var("TERMAL_ALLOW_INSECURE_STATE_PERMISSIONS");
            }
        }
    }

    #[test]
    fn insecure_state_permission_override_converts_failure_to_warning() {
        let _guard = ENV_MUTEX
            .lock()
            .expect("state permission env mutex poisoned");
        let original = std::env::var_os("TERMAL_ALLOW_INSECURE_STATE_PERMISSIONS");
        unsafe {
            std::env::remove_var("TERMAL_ALLOW_INSECURE_STATE_PERMISSIONS");
        }
        let path = FsPath::new("/tmp/termal-permission-test");

        assert!(permission_hardening_failure(path, "forced failure").is_err());

        unsafe {
            std::env::set_var("TERMAL_ALLOW_INSECURE_STATE_PERMISSIONS", "true");
        }
        assert!(permission_hardening_failure(path, "forced failure").is_ok());

        unsafe {
            if let Some(value) = original {
                std::env::set_var("TERMAL_ALLOW_INSECURE_STATE_PERMISSIONS", value);
            } else {
                std::env::remove_var("TERMAL_ALLOW_INSECURE_STATE_PERMISSIONS");
            }
        }
    }
}

/// Loads state from SQLite.
#[cfg(test)]
fn load_state(path: &FsPath) -> Result<Option<StateInner>> {
    if !path.exists() {
        return Ok(None);
    }
    let (state, _connection) = load_state_from_sqlite_with_connection(path, false)?;
    Ok(state)
}

/// Loads state while retaining the validated SQLite connection for the
/// production boot path. Handing this connection directly to the background
/// persistence worker prevents a last-connection close between loading and
/// serving. On Windows, that close can synchronously checkpoint and remove a
/// large WAL/SHM pair and make startup appear hung in `DeleteFileW`.
fn load_state_for_boot(
    path: &FsPath,
) -> Result<(Option<StateInner>, Option<rusqlite::Connection>)> {
    if !path.exists() {
        return Ok((None, None));
    }
    let (state, connection) = load_state_from_sqlite_with_connection(path, true)?;
    Ok((state, Some(connection)))
}

include!("persist_metadata_units.rs");
include!("persist_sqlite_schema.rs");
include!("persist_sqlite_obligations.rs");

include!("persist_sqlite_overview.rs");
include!("persist_sqlite_messages.rs");
#[cfg(test)]
include!("persist_sqlite_overview_tests.rs");
#[cfg(test)]
include!("persist_sqlite_maintenance_tests.rs");
#[cfg(test)]
include!("persist_sqlite_messages_tests.rs");

fn ensure_sqlite_state_schema_for_path(
    connection: &rusqlite::Connection,
    path: &FsPath,
) -> Result<()> {
    ensure_sqlite_state_schema_for_path_with_initialization(connection, path).map(|_| ())
}

fn ensure_sqlite_state_schema_for_path_with_initialization(
    connection: &rusqlite::Connection,
    path: &FsPath,
) -> Result<bool> {
    let write_lock = sqlite_state_write_lock(path);
    let _write_guard = lock_sqlite_state_writer(&write_lock);
    let setup = ensure_sqlite_state_schema_with_initialization(connection).with_context(|| {
        format!(
            "failed to validate or initialize state database `{}`",
            path.display()
        )
    });
    finish_sqlite_state_file_setup(path, setup)
}

fn ensure_sqlite_state_schema_for_load_path(
    connection: &rusqlite::Connection,
    path: &FsPath,
) -> Result<Option<PersistedState>> {
    let write_lock = sqlite_state_write_lock(path);
    let _write_guard = lock_sqlite_state_writer(&write_lock);
    let setup = ensure_sqlite_state_schema_for_load(connection).with_context(|| {
        format!(
            "failed to validate or initialize state database `{}`",
            path.display()
        )
    });
    finish_sqlite_state_file_setup(path, setup)
}

include!("persist_sqlite_schema_tests.rs");
#[cfg(test)]
include!("persist_sqlite_reader_floor_tests.rs");
include!("persist_sqlite_obligations_tests.rs");

fn load_state_from_sqlite_with_connection(
    path: &FsPath,
    defer_delegations: bool,
) -> Result<(Option<StateInner>, rusqlite::Connection)> {
    let connection = open_sqlite_state_connection_unconfigured(path)?;
    let persisted = ensure_sqlite_state_schema_for_load_path(&connection, path)?;
    let (mut session_records, mut quarantined_session_ids, mut skipped_session_records) =
        load_session_records_from_sqlite_with_skipped(&connection, path)?;
    let (delegation_records, quarantined_delegation_ids) = if defer_delegations {
        (Vec::new(), BTreeSet::new())
    } else {
        load_delegation_records_from_sqlite(&connection, path)?
    };
    let Some(mut persisted) = persisted else {
        return Ok((None, connection));
    };
    let project_ids = persisted
        .projects
        .iter()
        .map(|project| project.id.as_str())
        .collect::<HashSet<_>>();
    session_records.retain(|record| {
        let Some(project_id) = record.session.project_id.as_deref() else {
            return true;
        };
        if project_ids.contains(project_id) {
            return true;
        }
        skipped_session_records += 1;
        quarantined_session_ids.insert(record.session.id.clone());
        eprintln!(
            "persist> skipping session `{}` because it references unknown project `{project_id}`",
            record.session.id
        );
        false
    });
    if skipped_session_records > 0 {
        eprintln!(
            "persist> skipped {skipped_session_records} invalid session record(s) while loading `{}`",
            path.display()
        );
    }
    // The normalized `sessions` table is authoritative even when every row was
    // skipped. Falling back to the metadata blob here could resurrect stale or
    // structurally invalid sessions after the row-level isolation above.
    persisted.sessions = session_records;
    persisted.quarantined_persisted_session_ids = quarantined_session_ids;
    persisted.quarantined_persisted_delegation_ids = quarantined_delegation_ids;
    persisted.delegations = delegation_records;
    if defer_delegations {
        persisted.delegation_inventory = DelegationInventoryState::Loading {
            token: Uuid::new_v4().to_string(),
            source: delegation_inventory_file_identity(path)?,
            worker_started: false,
        };
    }
    let mut inner = persisted
        .into_inner()
        .with_context(|| format!("failed to validate state from `{}`", path.display()))?;
    bind_loaded_federation(&mut inner, &connection, path)?;
    Ok((Some(inner), connection))
}

include!("persist_sqlite_transcript_read.rs");

#[cfg(test)]
thread_local! {
    static DELEGATION_STARTUP_PAYLOAD_WORK: std::cell::Cell<(usize, usize)> =
        const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
fn take_delegation_startup_payload_work() -> (usize, usize) {
    DELEGATION_STARTUP_PAYLOAD_WORK.with(|work| work.replace((0, 0)))
}

fn load_delegation_records_from_sqlite(
    connection: &rusqlite::Connection,
    path: &FsPath,
) -> Result<(Vec<DelegationRecord>, BTreeSet<String>)> {
    let (records, quarantined, _) = load_delegation_records_with_fingerprint(connection, path, || false)?;
    Ok((records, quarantined))
}

impl AppState {
    /// Fences the final inventory validation and install with existing writer
    /// admission plus SQLite's cross-connection reservation. SQL stays off
    /// StateInner. Never wait for StateInner while holding either writer lock:
    /// synchronous persistence already acquires them in the other order.
    fn lock_delegation_inventory_publication<'state, 'sql, 'writer>(
        &'state self,
        connection: &'sql rusqlite::Connection,
        writer: &'writer SqliteStateWriterAdmission,
        version: i64,
        fingerprint: &str,
        source: &FederationImageFileIdentity,
        observe: &mut impl FnMut(&str),
    ) -> Result<(
        StateMutexGuard<'state, StateInner>,
        rusqlite::Transaction<'sql>,
        SqliteStateWriterGuard<'writer>,
    )> {
        let mut observed = false;
        loop {
            // A StateInner wait is ordinary lock admission, not SQLite busy
            // time. Start a fresh SQL budget after each released state wait.
            let deadline = std::time::Instant::now() + SQLITE_BUSY_TIMEOUT;
            let remaining = deadline.checked_duration_since(std::time::Instant::now())
                .context("delegation inventory publication admission timed out")?;
            let admission = lock_sqlite_state_writer_for(writer, remaining)
                .context("delegation inventory writer admission timed out")?;
            let remaining = deadline.checked_duration_since(std::time::Instant::now())
                .context("delegation inventory publication admission timed out")?;
            connection.busy_timeout(remaining)?;
            let transaction = rusqlite::Transaction::new_unchecked(
                connection, rusqlite::TransactionBehavior::Immediate,
            )?;
            let current_version: i64 =
                transaction.query_row("PRAGMA data_version", [], |row| row.get(0))?;
            if version != current_version {
                anyhow::ensure!(
                    delegation_inventory_fingerprint(&transaction)? == fingerprint,
                    "delegation inventory changed while loading; refusing stale publication"
                );
            }
            if !observed {
                observed = true;
                observe("publication_reserved");
            }
            if let Some(inner) = self.inner.try_lock_now() {
                // Revalidate the pathname at the publication boundary as well
                // as the open connection's SQL generation. No SQL runs here.
                verify_delegation_inventory_source(connection, &self.persistence_path, source)?;
                observe("publication_state_acquired");
                return Ok((inner, transaction, admission));
            }
            drop(transaction);
            drop(admission);
            // Wait on the existing state mutex without reserving SQLite. This
            // is lock admission, not a retry of loading or recovery effects.
            drop(self.inner.lock().expect("state mutex poisoned"));
        }
    }
}

fn load_delegation_records_with_fingerprint(
    connection: &rusqlite::Connection,
    path: &FsPath,
    cancelled: impl Fn() -> bool,
) -> Result<(Vec<DelegationRecord>, BTreeSet<String>, String)> {
    let mut statement = connection
        .prepare("SELECT id, value_json FROM delegations ORDER BY rowid")
        .with_context(|| {
            format!(
                "failed to prepare delegation load from `{}`",
                path.display()
            )
        })?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .with_context(|| {
            format!(
                "failed to query persisted delegations from `{}`",
                path.display()
            )
        })?;
    let mut records = Vec::new();
    let mut quarantined_ids = BTreeSet::new();
    let mut skipped = 0_usize;
    let mut fingerprint = sha2::Sha256::new();
    for row in rows {
        anyhow::ensure!(!cancelled(), "delegation inventory load interrupted by shutdown");
        // A row conversion/iteration failure cannot identify a complete set of
        // records or quarantine IDs. Refuse rather than certify an omission.
        let (delegation_id, encoded) = row.with_context(|| {
            format!("failed to read complete delegation inventory from `{}`", path.display())
        })?;
        delegation_inventory_hash_row(&mut fingerprint, &delegation_id, &encoded);
        #[cfg(test)]
        DELEGATION_STARTUP_PAYLOAD_WORK.with(|work| {
            let (rows, bytes) = work.get();
            work.set((rows + 1, bytes + encoded.len()));
        });
        match serde_json::from_str::<DelegationRecord>(&encoded) {
            Ok(record) if record.id == delegation_id => records.push(record),
            Ok(record) => {
                skipped += 1;
                quarantined_ids.insert(delegation_id.clone());
                eprintln!(
                    "persist> skipping invalid delegation `{delegation_id}` because its embedded id is `{}`",
                    record.id
                );
            }
            Err(err) => {
                skipped += 1;
                quarantined_ids.insert(delegation_id.clone());
                eprintln!(
                    "persist> skipping invalid delegation `{delegation_id}` from `{}`: {err:#}",
                    path.display(),
                );
            }
        }
    }
    if skipped > 0 {
        eprintln!(
            "persist> skipped {skipped} invalid delegation record(s) while loading `{}`",
            path.display()
        );
    }
    Ok((records, quarantined_ids, format!("{:x}", fingerprint.finalize())))
}

include!("persist_sqlite_transcript_write.rs");

fn remove_missing_persisted_delegations(
    tx: &rusqlite::Transaction<'_>,
    retained_delegation_ids: &HashSet<&str>,
    quarantined_delegation_ids: &BTreeSet<String>,
) -> Result<()> {
    let stored_delegation_ids = {
        let mut statement = tx
            .prepare("SELECT id FROM delegations")
            .context("failed to prepare stored-delegation replacement")?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .context("failed to query stored delegations for replacement")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to read stored delegations for replacement")?
    };
    for delegation_id in stored_delegation_ids {
        if retained_delegation_ids.contains(delegation_id.as_str())
            || quarantined_delegation_ids.contains(&delegation_id)
        {
            continue;
        }
        tx.execute(
            "DELETE FROM delegations WHERE id = ?1",
            rusqlite::params![delegation_id],
        )
        .context("failed to remove stale persisted delegation")?;
    }
    Ok(())
}

#[cfg(test)]
fn persist_persisted_state_to_sqlite(path: &FsPath, persisted: &PersistedState) -> Result<u64> {
    let metadata = persisted.metadata_only();
    persist_state_parts_to_sqlite(
        path,
        &metadata,
        &persisted.sessions,
        matches!(persisted.delegation_inventory, DelegationInventoryState::Ready),
        &persisted.quarantined_persisted_session_ids,
        &persisted.delegations,
        matches!(persisted.delegation_inventory, DelegationInventoryState::Ready),
        &persisted.quarantined_persisted_delegation_ids,
    )
}

fn persist_created_session(
    path: &FsPath,
    inner: &StateInner,
    _record: &SessionRecord,
) -> Result<()> {
    persist_locked_state_to_sqlite(path, inner)?;
    Ok(())
}

// The caller holds StateInner throughout this operation. Admit this writer
// before capturing its predecessor: an earlier background commit can publish
// that predecessor while we wait, without needing to acquire StateInner.
fn persist_locked_state_to_sqlite(path: &FsPath, inner: &StateInner) -> Result<u64> {
    if let Some(parent) = path.parent() {
        create_local_state_directory(parent)?;
    }
    let mut connection = open_sqlite_state_connection_unconfigured(path)?;
    let initialized = ensure_sqlite_state_schema_for_path_with_initialization(&connection, path)?;
    let writer = sqlite_state_write_lock(path);
    let guard = lock_sqlite_state_writer(&writer);
    if initialized && inner.metadata_predecessor.load(std::sync::atomic::Ordering::Acquire) == 0 {
        let generation = metadata_unit_generation(&connection)?;
        if generation != 1 {
            bail!("fresh metadata predecessor changed before source capture");
        }
        // This source is still held, and no snapshot has been made here yet.
        // Never apply this adoption to an already captured delta.
        inner.metadata_predecessor.store(generation, std::sync::atomic::Ordering::Release);
    }
    let persisted = PersistedState::from_inner(inner);
    let metadata = persisted.metadata_only();
    persist_state_parts_via_connection(
        &mut connection,
        path,
        &metadata,
        &persisted.sessions,
        inner.delegation_inventory_complete(),
        &persisted.quarantined_persisted_session_ids,
        &persisted.delegations,
        inner.delegation_inventory_complete(),
        &persisted.quarantined_persisted_delegation_ids,
        guard,
        true,
    )
}

#[cfg(test)]
fn persist_state_parts_to_sqlite(
    path: &FsPath,
    metadata: &PersistedState,
    sessions: &[PersistedSessionRecord],
    replace_sessions: bool,
    quarantined_session_ids: &BTreeSet<String>,
    delegations: &[DelegationRecord],
    replace_delegations: bool,
    quarantined_delegation_ids: &BTreeSet<String>,
) -> Result<u64> {
    if let Some(parent) = path.parent() {
        create_local_state_directory(parent)?;
    }

    let mut connection = open_sqlite_state_connection_unconfigured(path)?;
    ensure_sqlite_state_schema_for_path(&connection, path)?;
    let writer = sqlite_state_write_lock(path);
    let guard = lock_sqlite_state_writer(&writer);
    persist_state_parts_via_connection(
        &mut connection,
        path,
        metadata,
        sessions,
        replace_sessions,
        quarantined_session_ids,
        delegations,
        replace_delegations,
        quarantined_delegation_ids,
        guard,
        false,
    )
}

/// Applies one persist transaction to an already-open SQLite connection.
///
/// Requires successful path-aware schema setup: validate before maintenance,
/// enable foreign keys, and harden newly created sidecars before writes. The
/// background persist thread reuses that validated connection; its hot path
/// does not reopen or repeat schema validation and maintenance on every commit.
fn persist_state_parts_via_connection(
    connection: &mut rusqlite::Connection,
    path: &FsPath,
    metadata: &PersistedState,
    sessions: &[PersistedSessionRecord],
    replace_sessions: bool,
    quarantined_session_ids: &BTreeSet<String>,
    delegations: &[DelegationRecord],
    replace_delegations: bool,
    quarantined_delegation_ids: &BTreeSet<String>,
    write_guard: SqliteStateWriterGuard<'_>,
    adopt_locked_source: bool,
) -> Result<u64> {
    let metadata_units = project_metadata_units(metadata)?;
    let serialized_sessions = serialize_persisted_sessions_with_isolation(sessions);
    let serialized_delegations = delegations
        .iter()
        .map(|delegation| {
            serde_json::to_string(delegation)
                .context("failed to serialize persisted delegation")
                .map(|json| (delegation.id.as_str(), json))
        })
        .collect::<Result<Vec<_>>>()?;
    let tx = connection.transaction().with_context(|| {
        format!(
            "failed to start SQLite transaction for `{}`",
            path.display()
        )
    })?;
    write_federation_host_generation(&tx, metadata)?;
    let committed_metadata = write_metadata_units(
        &tx,
        metadata_units,
        None,
        &serialized_sessions,
        Some(&metadata.metadata_capture),
        true,
    )?;
    if replace_sessions {
        // Retain every session from the snapshot, including any session whose
        // current in-memory value failed validation. Such a session is skipped
        // above so its last known-good SQLite row remains recoverable.
        let retained_session_ids = sessions
            .iter()
            .map(|session| session.session.id.as_str())
            .collect::<HashSet<_>>();
        remove_missing_persisted_sessions(&tx, &retained_session_ids, quarantined_session_ids)
            .with_context(|| format!("failed to replace sessions in `{}`", path.display()))?;
    }
    for session in &serialized_sessions {
        write_serialized_persisted_session(&tx, session).with_context(|| {
            format!("failed to write persisted session to `{}`", path.display())
        })?;
    }
    if replace_delegations {
        let retained_delegation_ids = serialized_delegations
            .iter()
            .map(|(delegation_id, _)| *delegation_id)
            .collect::<HashSet<_>>();
        remove_missing_persisted_delegations(
            &tx,
            &retained_delegation_ids,
            quarantined_delegation_ids,
        )
        .with_context(|| format!("failed to replace delegations in `{}`", path.display()))?;
    }
    for (delegation_id, delegation_json) in serialized_delegations {
        tx.execute(
            "INSERT INTO delegations(id, value_json) VALUES(?1, ?2)
             ON CONFLICT(id) DO UPDATE SET value_json = excluded.value_json",
            rusqlite::params![delegation_id, delegation_json],
        )
        .with_context(|| {
            format!(
                "failed to write persisted delegation `{}` to `{}`",
                delegation_id,
                path.display()
            )
        })?;
    }
    validate_sqlite_checkpoint_write_cut(&tx, &committed_metadata, &serialized_sessions)
        .context("refusing incoherent checkpoint persistence cut")?;
    tx.commit()
        .with_context(|| format!("failed to commit persisted state to `{}`", path.display()))?;
    metadata
        .metadata_capture
        .committed(committed_metadata.generation);
    if adopt_locked_source {
        // Publish while writer admission is still held, even if the following
        // integrity checks fail. The receipt still fails; only a fresh coherent
        // source capture can follow this committed transaction.
        metadata.metadata_capture.adopt_own_committed(metadata);
    }
    persist_observe_metadata_commit();
    verify_federation_write(connection, metadata)?;
    drop(write_guard);
    // Keep post-commit redirection and owner-only permission verification
    // fatal. The chmod helper itself honors
    // TERMAL_ALLOW_INSECURE_STATE_PERMISSIONS when the operator explicitly
    // accepts insecure state-file modes.
    verify_persist_commit_integrity(path)?;
    Ok(committed_metadata.generation)
}

/// Body identities at absolute transcript positions after a verified COMMIT.
/// Weak references pin allocations against address reuse without owning bodies;
/// an evicted prefix can therefore release both payloads and cached identities.
struct PersistTranscriptBaseline {
    start: usize,
    identities: Vec<std::sync::Weak<Message>>,
}

/// Thread-local SQLite connection cache for the background persist thread.
///
/// Every queued persist previously opened a fresh SQLite connection and
/// re-ran `ensure_sqlite_state_schema`. The persist thread writes many times
/// during an active session, so amortizing that cost to one open-and-validate per
/// thread lifetime removes the biggest per-persist overhead.
///
/// Production seeds this cache from the connection validated during process
/// startup. Reopens after invalidation assume the same store path (including a
/// path observed as absent at boot) was startup-validated by this process; they
/// deliberately repeat only bounded write-path schema setup.
/// `AppState::new_with_paths` captures that path once for the persist worker and
/// reuses it for every queued write; production does not switch stores at
/// runtime. The cache's path-change support alone does not establish startup
/// validation for a different store.
struct SqlitePersistConnectionCache {
    path: Option<PathBuf>,
    connection: Option<rusqlite::Connection>,
    metadata_units: Option<PersistMetadataUnitCache>,
    // Adopted only after COMMIT and post-commit integrity verification.
    transcripts: HashMap<String, PersistTranscriptBaseline>,
    // Same-connection data_version sampled inside that committed transaction.
    // Any different predecessor invalidates all sparse transcript assumptions.
    transcript_data_version: Option<i64>,
}

impl SqlitePersistConnectionCache {
    fn new() -> Self {
        Self {
            path: None,
            connection: None,
            metadata_units: None,
            transcripts: HashMap::new(),
            transcript_data_version: None,
        }
    }

    /// Seeds the cache with the connection that already loaded and validated
    /// startup state. Ownership moves into the persistence thread without a
    /// zero-connection interval, so SQLite never performs last-close sidecar
    /// cleanup on the startup thread.
    fn from_validated_connection(validated: Option<(PathBuf, rusqlite::Connection)>) -> Self {
        match validated {
            Some((path, connection)) => Self {
                path: Some(path),
                connection: Some(connection),
                metadata_units: None,
                transcripts: HashMap::new(),
                transcript_data_version: None,
            },
            None => Self::new(),
        }
    }

    /// Returns a mutable reference to a SQLite connection opened for
    /// `path`, reusing the cached connection when the path matches.
    /// Runs bounded schema validation only when a fresh connection is opened;
    /// startup-only metadata and legacy-authority validation is a precondition
    /// already established for this path by the current process boot.
    fn connection_for(&mut self, path: &FsPath) -> Result<&mut rusqlite::Connection> {
        let matches_cache = self.path.as_deref() == Some(path);
        if !matches_cache {
            // Path changed (or first open): open+validate the replacement
            // first so a transient failure does not speculatively discard a
            // still-working cached connection.
            if let Some(parent) = path.parent() {
                create_local_state_directory(parent)?;
            }
            let connection = open_sqlite_state_connection_unconfigured(path)?;
            ensure_sqlite_state_schema_for_path(&connection, path)?;
            // Path-aware setup already hardened the files created by WAL and
            // maintenance. Cached reuse hardens them again after each commit.
            self.path = Some(path.to_path_buf());
            self.connection = Some(connection);
            self.metadata_units = None;
            self.transcripts.clear();
            self.transcript_data_version = None;
        }
        Ok(self
            .connection
            .as_mut()
            .expect("connection was just cached for the requested path"))
    }

    // Called after live-tail trimming and dropping the captured delta, with
    // no StateInner guard. Drop dead weak prefixes too, so an imported large
    // transcript does not leave its old identity/control-block window cached.
    fn release_evicted_transcript_prefixes(&mut self, persisted_ids: &[String]) {
        for id in persisted_ids {
            let Some(baseline) = self.transcripts.get_mut(id) else {
                continue;
            };
            let expired = baseline
                .identities
                .iter()
                .take_while(|body| body.strong_count() == 0)
                .count();
            if expired != 0 {
                drop(baseline.identities.drain(..expired));
                baseline.identities.shrink_to_fit();
                baseline.start += expired;
            }
        }
    }

    /// Drops the cached connection so the next `connection_for` call
    /// reopens fresh and repeats bounded validate-before-mutate schema setup
    /// and both permission passes (not the startup-only authority scan).
    ///
    /// Invoked when a persist operation fails. The cached connection
    /// may be in a poisoned or transaction-stuck state
    /// (`SQLITE_BUSY`, `SQLITE_CORRUPT`, the backing file unlinked
    /// by a manual reset, a Windows-side handle glitch after an OS
    /// sleep, etc.). Without invalidation every subsequent tick
    /// would reuse the broken handle and log the same error
    /// forever — a "permanent persist broken" state that a backend
    /// restart would otherwise repair. The next tick pays the cost
    /// of one open-plus-schema-ensure; the happy path still reuses
    /// one connection per process lifetime.
    fn invalidate(&mut self) {
        self.connection = None;
        self.path = None;
        self.metadata_units = None;
        self.transcripts.clear();
        self.transcript_data_version = None;
    }
}

/// Applies a `PersistDelta` — metadata upsert, targeted session
/// row `INSERT OR UPDATE`s and `DELETE`s, and targeted delegation row
/// `INSERT OR UPDATE`s and `DELETE`s via the shared connection cache.
///
/// This is the sole production write path. It writes only the rows in
/// `delta.changed_sessions` / `delta.changed_delegations` and removes only
/// `delta.removed_session_ids` / `delta.removed_delegation_ids`; unchanged rows
/// are left untouched so a mutation on one record no longer rewrites every
/// other row every commit.
/// See `state.rs::PersistDelta` and `StateInner::collect_persist_delta`
/// for the authoritative description of how the delta is assembled.
///
/// Error-driven invalidation: on ANY error returned from
/// [`persist_delta_via_cache_inner`] the cached connection is
/// dropped via [`SqlitePersistConnectionCache::invalidate`]
/// before the error propagates. The next persist tick reopens
/// fresh and re-runs `ensure_sqlite_state_schema`. Without this,
/// a connection poisoned by `SQLITE_BUSY` / `SQLITE_CORRUPT` /
/// an unlinked backing file / a Windows handle glitch would be
/// reused tick after tick, logging the same error forever — a
/// permanent persist-broken state that a backend restart would
/// otherwise repair.
///
/// Invalidation is deliberately wide: it fires on transaction-
/// path errors (`transaction()` / `execute` / `commit`) AND on
/// pre-connection failures in the inner helper (metadata JSON
/// serialization, the `fs::create_dir_all` inside
/// `connection_for`, or the open+schema-ensure itself). The
/// reopen cost is bounded — a single open + `ensure_sqlite_state_schema`
/// on the next tick — and the stuck-handle case we actually
/// care about is covered. Narrowing the window to only the
/// transaction calls would require splitting the inner helper
/// into "pre-connection / transaction / post-connection" phases
/// with extra plumbing; not worth it for this severity.
///
/// Test-only: production writes go through `persist_delta_via_cache_timed`.
#[cfg(test)]
fn persist_delta_via_cache(
    cache: &mut SqlitePersistConnectionCache,
    path: &FsPath,
    delta: &PersistDelta,
) -> Result<Vec<String>> {
    persist_delta_via_cache_timed(cache, path, delta, &mut PersistTickTimings::default())
}

/// `persist_delta_via_cache`, also recording how long each write phase took.
fn persist_delta_via_cache_timed(
    cache: &mut SqlitePersistConnectionCache,
    path: &FsPath,
    delta: &PersistDelta,
    timings: &mut PersistTickTimings,
) -> Result<Vec<String>> {
    let result = persist_delta_via_cache_inner(cache, path, delta, timings);
    if result.is_err() {
        cache.invalidate();
    }
    result
}

/// A different connection committed after transcript preparation. The write
/// must be retried from a fresh baseline without resolving its content fences.
#[derive(Debug)]
struct PersistTranscriptPredecessorChanged;

impl std::fmt::Display for PersistTranscriptPredecessorChanged {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("transcript predecessor changed while preparing persistence")
    }
}

impl std::error::Error for PersistTranscriptPredecessorChanged {}

/// A well-formed checkpoint mirror on its own owner's row disagrees with the
/// ledger entry the same cut selects. In the delta writer this is the bounded
/// collector deferring that owner row while the cut still carried its changed
/// ledger entry: nothing is committed, the watermark stays where it was, and a
/// later collection is coherent, so, like a changed transcript predecessor,
/// the write is retried without resolving its content fences
/// (`persist_fence.rs`). The full-snapshot writer shares the validator and so
/// reports the same type, but it holds no fences and treats it as any other
/// refusal. Malformed, noncanonical or misowned mirrors, and every other
/// validation failure, stay ordinary errors.
#[derive(Debug)]
struct PersistCheckpointMirrorLedgerMismatch;

impl std::fmt::Display for PersistCheckpointMirrorLedgerMismatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("existing checkpoint mirror disagrees with its selected ledger entry")
    }
}

impl std::error::Error for PersistCheckpointMirrorLedgerMismatch {}

fn persist_delta_via_cache_inner(
    cache: &mut SqlitePersistConnectionCache,
    path: &FsPath,
    delta: &PersistDelta,
    timings: &mut PersistTickTimings,
) -> Result<Vec<String>> {
    // Every phase records its elapsed time before its result is checked, so a
    // failed tick still accounts for the time it spent.
    persist_observe_phase("serialize");
    let serialize_started = std::time::Instant::now();
    let predecessor_version = if cache.path.as_deref() == Some(path) {
        cache
            .connection
            .as_ref()
            .map(|connection| {
                connection.query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
            })
            .transpose()?
    } else {
        None
    };
    if predecessor_version != cache.transcript_data_version {
        cache.transcripts.clear();
    }
    let used_baseline = !cache.transcripts.is_empty();
    let serialized = (|| -> Result<_> {
        let metadata_units = project_metadata_units(&delta.metadata)?;
        // A baseline belongs to the exact cached store connection. A reopen
        // or path switch must send the complete captured coverage again.
        let baseline = (cache.path.as_deref() == Some(path)).then_some(&cache.transcripts);
        let serialized_sessions = delta
            .changed_sessions
            .iter()
            .filter_map(|record| {
                let previous = baseline.and_then(|rows| rows.get(&record.session.id));
                match serialize_persisted_session_since(record, previous) {
                    Ok(row) => Some(row),
                    Err(error) => {
                        eprintln!(
                            "persist> preserving invalid session `{}`: {error:#}",
                            record.session.id
                        );
                        None
                    }
                }
            })
            .collect::<Vec<_>>();
        let serialized_delegations = delta
            .changed_delegations
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|delegation| {
                serde_json::to_string(delegation)
                    .context("failed to serialize persisted delegation")
                    .map(|json| (delegation.id.as_str(), json))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok((metadata_units, serialized_sessions, serialized_delegations))
    })();
    timings.serialize = serialize_started.elapsed();
    let (metadata_units, serialized_sessions, serialized_delegations) = serialized?;
    persist_observe_payload("", &serialized_sessions, &serialized_delegations);
    let persisted_session_ids = serialized_sessions
        .iter()
        .map(|session| session.session_id.clone())
        .collect::<Vec<_>>();
    // Opening covers a reopen after an invalidated cache (schema setup and
    // hardening, under the same writer lock) and the redirection check.
    persist_observe_phase("open");
    let open_started = std::time::Instant::now();
    #[cfg(test)]
    let reused_connection = cache.path.as_deref() == Some(path) && cache.connection.is_some();
    let connected = cache.connection_for(path);
    #[cfg(test)]
    let connection_elapsed = open_started.elapsed();
    #[cfg(test)]
    persist_observe(|observer| {
        observer.update(|tick| {
            tick.phases.test_open_connection_us = Some(persist_observed_us(connection_elapsed));
            tick.phases.test_open_reused_connection = Some(reused_connection);
        })
    });
    let opened = connected.and_then(|connection| {
        // Keep state-path redirection failures fatal on cached writes too.
        // Directory chmod hardening runs when the cached connection is opened;
        // the hot path intentionally repeats only symlink/reparse checks
        // before each transaction so path swaps are caught without chmoding
        // the state directory every tick.
        #[cfg(test)]
        let checks_started = std::time::Instant::now();
        let checked = reject_existing_sqlite_state_path_redirection(path);
        #[cfg(test)]
        let checks_elapsed = checks_started.elapsed();
        #[cfg(test)]
        persist_observe(|observer| {
            observer.update(|tick| {
                tick.phases.test_open_path_checks_us = Some(persist_observed_us(checks_elapsed));
            })
        });
        checked.map(|()| connection)
    });
    timings.open = open_started.elapsed();
    let _ = opened?;
    let previous_metadata_units = cache.metadata_units.take();
    let connection = cache.connection_for(path)?;
    let write_lock = sqlite_state_write_lock(path);
    persist_observe_phase("writer_ticket");
    let ticket_started = std::time::Instant::now();
    let write_guard = lock_sqlite_state_writer(&write_lock);
    timings.ticket_wait = ticket_started.elapsed();
    persist_observe_phase("statements");
    let statements_started = std::time::Instant::now();
    let written = connection
        .transaction()
        .with_context(|| {
            format!(
                "failed to start SQLite transaction for `{}`",
                path.display()
            )
        })
        .and_then(|tx| {
            // Another connection may commit after preparation. Never apply a
            // sparse list against that different predecessor; invalidate and
            // let the normal retry submit complete captured coverage.
            if used_baseline
                && Some(tx.query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))?)
                    != predecessor_version
            {
                return Err(PersistTranscriptPredecessorChanged.into());
            }
            write_federation_host_generation(&tx, &delta.metadata)?;
            let committed_metadata = write_metadata_units(
                &tx,
                metadata_units,
                previous_metadata_units.as_ref(),
                &serialized_sessions,
                Some(&delta.metadata.metadata_capture),
                !delta.changed_sessions.is_empty()
                    || !delta.removed_session_ids.is_empty()
                    || delta
                        .changed_delegations
                        .as_ref()
                        .is_some_and(|rows| !rows.is_empty())
                    || !delta.removed_delegation_ids.is_empty(),
            )?;
            write_persist_delta_statements(
                &tx,
                path,
                delta,
                &serialized_sessions,
                serialized_delegations,
            )?;
            validate_sqlite_checkpoint_write_cut(&tx, &committed_metadata, &serialized_sessions)
                .context("refusing incoherent checkpoint persistence cut")?;
            Ok((tx, committed_metadata))
        });
    timings.statements = statements_started.elapsed();
    let (tx, committed_metadata) = written?;
    persist_observe_phase("commit");
    let commit_started = std::time::Instant::now();
    // Anchor the body snapshot while this transaction still owns SQLite's
    // write lock. Sampling after COMMIT could adopt an intervening external
    // writer's version alongside our older immutable bodies. Our own commit
    // does not change PRAGMA data_version on this connection.
    let committed_transcript_version =
        tx.query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))?;
    let committed = persist_commit_transaction_observed(tx)
        .with_context(|| format!("failed to commit persisted state to `{}`", path.display()));
    timings.commit = commit_started.elapsed();
    committed?;
    #[cfg(test)]
    if let Some(hook) = PERSIST_MESSAGE_COMMIT_HOOK.with(|slot| slot.take()) {
        hook();
    }
    delta
        .metadata
        .metadata_capture
        .committed(committed_metadata.generation);
    persist_observe_metadata_commit();
    // This source produced the committed transaction. Cache and fence success
    // still wait for integrity verification; failure requires a fresh capture.
    delta
        .metadata
        .metadata_capture
        .adopt(committed_metadata.generation, &delta.metadata);
    drop(write_guard);
    // Keep post-commit redirection and owner-only permission verification
    // fatal. The chmod helper itself honors
    // TERMAL_ALLOW_INSECURE_STATE_PERMISSIONS when the operator explicitly
    // accepts insecure state-file modes.
    persist_observe_phase("post_commit");
    let post_commit_started = std::time::Instant::now();
    let verified = verify_persist_commit_integrity(path);
    timings.post_commit = post_commit_started.elapsed();
    verified?;
    cache.metadata_units = Some(committed_metadata);
    cache.transcript_data_version = Some(committed_transcript_version);
    // Publish only snapshots whose transaction and integrity checks succeeded.
    // Weak identities prevent allocation-address ABA without retaining bodies.
    for id in &delta.removed_session_ids {
        cache.transcripts.remove(id);
    }
    for row in &serialized_sessions {
        cache.transcripts.insert(
            row.session_id.clone(),
            PersistTranscriptBaseline {
                start: row.message_start_index,
                identities: row.snapshot.identities(),
            },
        );
    }
    Ok(persisted_session_ids)
}

/// The statements of one delta write, inside its open transaction.
fn write_persist_delta_statements(
    tx: &rusqlite::Transaction<'_>,
    path: &FsPath,
    delta: &PersistDelta,
    serialized_sessions: &[SerializedPersistedSession],
    serialized_delegations: Vec<(&str, String)>,
) -> Result<()> {
    for session_id in &delta.removed_session_ids {
        tx.execute(
            "DELETE FROM sessions WHERE id = ?1",
            rusqlite::params![session_id],
        )
        .with_context(|| {
            format!(
                "failed to remove session `{}` from `{}`",
                session_id,
                path.display()
            )
        })?;
    }
    for session in serialized_sessions {
        write_serialized_persisted_session(tx, session).with_context(|| {
            format!(
                "failed to write persisted session `{}` to `{}`",
                session.session_id,
                path.display()
            )
        })?;
    }
    for delegation_id in &delta.removed_delegation_ids {
        tx.execute(
            "DELETE FROM delegations WHERE id = ?1",
            rusqlite::params![delegation_id],
        )
        .with_context(|| {
            format!(
                "failed to remove delegation `{}` from `{}`",
                delegation_id,
                path.display()
            )
        })?;
    }
    for (delegation_id, delegation_json) in serialized_delegations {
        tx.execute(
            "INSERT INTO delegations(id, value_json) VALUES(?1, ?2)
             ON CONFLICT(id) DO UPDATE SET value_json = excluded.value_json",
            rusqlite::params![delegation_id, delegation_json],
        )
        .with_context(|| {
            format!(
                "failed to write persisted delegation `{}` to `{}`",
                delegation_id,
                path.display()
            )
        })?;
    }
    Ok(())
}

/// Persists state from a pre-built `PersistedState` snapshot.
#[cfg(test)]
fn persist_state_from_persisted(path: &FsPath, persisted: &PersistedState) -> Result<u64> {
    persist_persisted_state_to_sqlite(path, persisted)
}

/// Persists state directly from `StateInner` (used in tests for synchronous
/// setup of persisted state files).
#[cfg(test)]
fn persist_state(path: &FsPath, inner: &StateInner) -> Result<()> {
    persist_locked_state_to_sqlite(path, inner)?;
    Ok(())
}
