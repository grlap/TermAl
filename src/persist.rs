/*
SQLite-backed session state persistence.

Owns the on-disk schema entry point (`ensure_sqlite_state_schema`), connection
lifecycle (`open_sqlite_state_connection`, `SqlitePersistConnectionCache`),
load path (`load_state`, `load_state_from_sqlite`), and the per-transaction
write helpers used by the background persist thread
(`persist_state_parts_via_connection`, `persist_delta_via_cache`,
`persist_created_session`, `persist_state_from_persisted`, `persist_state`).
Overview-specific schema upgrades and backfill live in
`persist_sqlite_overview.rs`.

Extracted from `api.rs` so HTTP handler code and SQLite persistence live
in separate files. The crate still compiles as one `include!()`-assembled
module, so no visibility changes are required.
*/

/// Resolves persistence path.
fn resolve_persistence_path(default_workdir: &str) -> PathBuf {
    resolve_termal_data_dir(default_workdir).join("termal.sqlite")
}

const SQLITE_SCHEMA_VERSION: &str = "2";
const SQLITE_PREVIOUS_SCHEMA_VERSION: &str = "1";
const SQLITE_METADATA_KEY: &str = "metadataState";
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const SQLITE_SESSION_TAIL_MESSAGES: usize = 64;
const SQLITE_TRANSCRIPT_MIGRATION_SESSION_BATCH: usize = 16;
const SQLITE_TRANSCRIPT_MIGRATION_MESSAGE_BATCH: usize = 64;
const SQLITE_PROMPT_HISTORY_STORAGE_KEY: &str = "prompt_history_storage_version";
const SQLITE_PROMPT_HISTORY_STORAGE_VERSION: &str = "1";

/// Per-database writer locks shared by every in-process SQLite write path.
///
/// WAL lets readers coexist, but SQLite still permits only one writer. The
/// The state persist worker has its own database domain. Within the separate
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
                eprintln!(
                    "[termal] warning: recovered a poisoned SQLite state writer admission"
                );
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

fn advance_past_canceled_sqlite_state_writer_tickets(
    state: &mut SqliteStateWriterAdmissionState,
) {
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
fn wait_for_sqlite_state_writer_issued_tickets(
    lock: &SqliteStateWriterAdmission,
    expected: u64,
) {
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

fn open_sqlite_state_connection(path: &FsPath) -> Result<rusqlite::Connection> {
    if let Some(parent) = path.parent() {
        harden_local_state_directory_permissions(parent)?;
    }
    reject_existing_sqlite_state_file_symlinks(path)?;
    let connection = rusqlite::Connection::open(path)
        .with_context(|| format!("failed to open `{}`", path.display()))?;
    connection
        .busy_timeout(SQLITE_BUSY_TIMEOUT)
        .with_context(|| format!("failed to set SQLite busy timeout for `{}`", path.display()))?;
    // WAL lets readers coexist with the background persistence writer. NORMAL
    // sync is the common local-app tradeoff: durable enough for TermAl state,
    // with much lower fsync cost than FULL on every small create-session write.
    {
        let write_lock = sqlite_state_write_lock(path);
        let _write_guard = lock_sqlite_state_writer(&write_lock);
        connection
            .execute_batch(
                "
                PRAGMA journal_mode = WAL;
                PRAGMA synchronous = NORMAL;
                PRAGMA foreign_keys = ON;
                ",
            )
            .with_context(|| {
                format!(
                    "failed to configure SQLite pragmas for `{}`",
                    path.display()
                )
            })?;
    }
    harden_sqlite_state_file_permissions(path)?;
    Ok(connection)
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
        .with_context(|| format!("failed to configure SQLite transcript reader for `{}`", path.display()))?;
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

fn verify_persist_commit_integrity(path: &FsPath) -> Result<()> {
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
    reject_existing_windows_state_path_redirection(&sqlite_sidecar_path(path, "-wal"))?;
    reject_existing_windows_state_path_redirection(&sqlite_sidecar_path(path, "-shm"))?;
    reject_existing_windows_state_path_redirection(&sqlite_sidecar_path(path, "-journal"))?;
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

/// Rejects Windows reparse points before SQLite can open the TermAl state
/// directory, database, or sidecars. A reparse point can redirect persisted
/// session history through a symlink, junction, or mount point; this is path
/// integrity, not Unix-style chmod hardening, so the insecure-permissions
/// escape hatch intentionally does not apply. `0x400` is the stable
/// `FILE_ATTRIBUTE_REPARSE_POINT` value; spelling it locally avoids adding a
/// Windows API crate only for this metadata bit.
#[cfg(windows)]
fn reject_existing_windows_state_path_redirection(path: &FsPath) -> Result<()> {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 => {
            Err(anyhow!(
                "refusing to follow redirected state path `{}`",
                path.display()
            ))
        }
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err)
            .with_context(|| format!("failed to inspect state path `{}`", path.display())),
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
        let root = std::env::temp_dir().join(format!(
            "termal-state-permissions-{}",
            Uuid::new_v4()
        ));
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
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .expect("set broad test mode");
    }

    #[test]
    fn state_file_hardening_sets_owner_only_file_mode() {
        let root = temp_permission_root();
        let file = root.join("termal.sqlite");
        fs::write(&file, b"state").expect("write temp file");
        set_mode(&file, 0o666);

        harden_local_state_file_permissions(&file).expect("harden state file");

        assert_eq!(mode(&file), 0o600);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn state_directory_hardening_sets_owner_only_directory_mode() {
        let root = temp_permission_root();
        let dir = root.join("state-dir");
        fs::create_dir(&dir).expect("create temp dir");
        set_mode(&dir, 0o777);

        harden_local_state_directory_permissions(&dir).expect("harden state dir");

        assert_eq!(mode(&dir), 0o700);
        let _ = fs::remove_dir_all(root);
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
        let _ = fs::remove_dir_all(root);
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
        let _ = fs::remove_dir_all(root);
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
        let _ = fs::remove_dir_all(root);
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
        symlink(&sidecar_target, sqlite_sidecar_path(&db, "-wal"))
            .expect("create sidecar symlink");

        let sidecar_error = harden_sqlite_state_file_permissions(&db)
            .expect_err("symlinked sidecar should be rejected");
        assert!(format!("{sidecar_error:#}").contains("symlinked state path"));
        assert_eq!(mode(&main_target), 0o644);
        assert_eq!(mode(&sidecar_target), 0o644);
        let _ = fs::remove_dir_all(root);
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
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn persist_commit_integrity_rejects_post_commit_redirection() {
        let root = temp_permission_root();
        let db = root.join("termal.sqlite");
        let sidecar_target = root.join("outside-wal");
        fs::write(&db, b"state").expect("write sqlite state file");
        fs::write(&sidecar_target, b"wal").expect("write sidecar target");
        symlink(&sidecar_target, sqlite_sidecar_path(&db, "-wal"))
            .expect("create sidecar symlink");

        let error = verify_persist_commit_integrity(&db)
            .expect_err("post-commit symlink should be fatal");

        assert!(format!("{error:#}").contains("post-commit redirection check failed"));
        assert!(format!("{error:#}").contains("symlinked state path"));
        let _ = fs::remove_dir_all(root);
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
        let _ = fs::remove_dir_all(root);
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
fn load_state(path: &FsPath) -> Result<Option<StateInner>> {
    if !path.exists() {
        return Ok(None);
    }
    load_state_from_sqlite(path)
}

fn ensure_sqlite_state_schema(connection: &rusqlite::Connection) -> Result<()> {
    connection
        .execute_batch(
            "
            CREATE TABLE IF NOT EXISTS meta (
              key TEXT PRIMARY KEY,
              value TEXT NOT NULL
            );
            ",
        )
        .context("failed to initialize SQLite meta schema")?;
    let stored_schema_version = match connection.query_row(
        "SELECT value FROM meta WHERE key = 'schema_version'",
        [],
        |row| row.get::<_, String>(0),
    ) {
        Ok(value) => Some(value),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(err) => return Err(err).context("failed to read SQLite state schema version"),
    };
    if let Some(stored_schema_version) = stored_schema_version.as_deref()
        && !matches!(
            stored_schema_version,
            SQLITE_SCHEMA_VERSION | SQLITE_PREVIOUS_SCHEMA_VERSION
        )
    {
        bail!(
            "unsupported SQLite state schema version `{}`; this binary supports `{}`",
            stored_schema_version,
            SQLITE_SCHEMA_VERSION
        );
    }
    connection
        .execute_batch(
            "
            CREATE TABLE IF NOT EXISTS app_state (
              key TEXT PRIMARY KEY,
              value_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sessions (
              id TEXT PRIMARY KEY,
              value_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS messages (
              session_id TEXT NOT NULL,
              position INTEGER NOT NULL CHECK(position >= 0),
              message_id TEXT NOT NULL,
              value_json TEXT NOT NULL,
              overview_kind INTEGER NOT NULL DEFAULT 0,
              is_user INTEGER NOT NULL DEFAULT 0,
              PRIMARY KEY(session_id, position),
              UNIQUE(session_id, message_id),
              FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
            ) WITHOUT ROWID;

            CREATE TABLE IF NOT EXISTS session_overviews (
              session_id TEXT PRIMARY KEY,
              value_blob BLOB NOT NULL,
              FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
            ) WITHOUT ROWID;

            CREATE TABLE IF NOT EXISTS session_prompt_histories (
              session_id TEXT PRIMARY KEY,
              value_json TEXT NOT NULL,
              FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
            ) WITHOUT ROWID;

            CREATE TABLE IF NOT EXISTS delegations (
              id TEXT PRIMARY KEY,
              value_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS board_cards (
              id TEXT PRIMARY KEY,
              x REAL NOT NULL,
              y REAL NOT NULL,
              w REAL NOT NULL,
              h REAL NOT NULL,
              snapshot_json TEXT NOT NULL,
              source_session_id TEXT NOT NULL,
              source_message_id TEXT NOT NULL,
              created_at TEXT NOT NULL
            );
            ",
        )
        .context("failed to initialize SQLite state schema")?;
    ensure_sqlite_message_overview_columns(connection)?;
    if stored_schema_version.as_deref() == Some(SQLITE_PREVIOUS_SCHEMA_VERSION) {
        migrate_sqlite_state_schema_v1_to_v2(connection)?;
    }
    migrate_embedded_sqlite_prompt_histories(connection)?;
    backfill_missing_sqlite_session_overviews(connection)?;
    connection
        .execute(
            "INSERT INTO meta(key, value) VALUES('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![SQLITE_SCHEMA_VERSION],
        )
        .context("failed to record SQLite state schema version")?;
    Ok(())
}

/// Moves the legacy embedded `session.promptHistory` projection into its own
/// row. The marker makes this a one-time upgrade while the transaction keeps a
/// failed migration retryable on the next startup. Invalid session JSON stays
/// untouched so the normal row-isolation loader can quarantine it.
fn migrate_embedded_sqlite_prompt_histories(
    connection: &rusqlite::Connection,
) -> Result<()> {
    let migration_complete = connection
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            rusqlite::params![SQLITE_PROMPT_HISTORY_STORAGE_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .context("failed to read SQLite prompt-history storage version")?
        .as_deref()
        == Some(SQLITE_PROMPT_HISTORY_STORAGE_VERSION);
    if migration_complete {
        return Ok(());
    }

    let tx = connection
        .unchecked_transaction()
        .context("failed to start SQLite prompt-history migration")?;
    let encoded_sessions = {
        let mut statement = tx
            .prepare(
                "SELECT id, value_json
                 FROM sessions
                 WHERE typeof(id) = 'text' AND typeof(value_json) = 'text'",
            )
            .context("failed to prepare embedded prompt-history migration")?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .context("failed to query embedded prompt histories")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to read embedded prompt histories")?
    };
    for (session_id, encoded) in encoded_sessions {
        let Ok(mut value) = serde_json::from_str::<Value>(&encoded) else {
            continue;
        };
        let Some(session) = value.get_mut("session").and_then(Value::as_object_mut) else {
            continue;
        };
        let Some(prompt_history) = session.remove("promptHistory") else {
            continue;
        };
        let Some(prompt_history) = prompt_history.as_array() else {
            // Leave malformed current-schema rows unchanged so strict session
            // validation, rather than this compatibility migration, owns the
            // resulting quarantine decision.
            session.insert("promptHistory".to_owned(), prompt_history);
            continue;
        };
        let history_json = serde_json::to_string(prompt_history)
            .context("failed to serialize migrated prompt history")?;
        let metadata_json = serde_json::to_string(&value)
            .context("failed to serialize migrated session metadata")?;
        tx.execute(
            "INSERT INTO session_prompt_histories(session_id, value_json)
             VALUES(?1, ?2)
             ON CONFLICT(session_id) DO NOTHING",
            rusqlite::params![session_id, history_json],
        )
        .with_context(|| {
            format!("failed to migrate prompt history for session `{session_id}`")
        })?;
        tx.execute(
            "UPDATE sessions SET value_json = ?2 WHERE id = ?1",
            rusqlite::params![session_id, metadata_json],
        )
        .with_context(|| {
            format!("failed to remove embedded prompt history for session `{session_id}`")
        })?;
    }
    tx.execute(
        "INSERT INTO meta(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![
            SQLITE_PROMPT_HISTORY_STORAGE_KEY,
            SQLITE_PROMPT_HISTORY_STORAGE_VERSION
        ],
    )
    .context("failed to record SQLite prompt-history storage version")?;
    tx.commit()
        .context("failed to commit SQLite prompt-history migration")
}

include!("persist_sqlite_overview.rs");
#[cfg(test)]
include!("persist_sqlite_overview_tests.rs");

fn migrate_sqlite_state_schema_v1_to_v2(connection: &rusqlite::Connection) -> Result<()> {
    let tx = connection
        .unchecked_transaction()
        .context("failed to start SQLite transcript migration")?;
    let mut last_rowid = 0_i64;
    let mut skipped_sessions = 0_usize;
    loop {
        // Bound migration memory by session batches instead of collecting every
        // legacy row id before decoding. Message payloads are streamed in a
        // separate bounded loop below, so Rust never materializes one session's
        // complete embedded transcript either.
        let encoded_sessions = {
            let mut statement = tx
                .prepare(
                    "SELECT rowid, id
                     FROM sessions
                     WHERE rowid > ?1
                     ORDER BY rowid
                     LIMIT ?2",
                )
                .context("failed to prepare persisted transcript migration batch")?;
            let rows = statement
                .query_map(
                    rusqlite::params![
                        last_rowid,
                        i64::try_from(SQLITE_TRANSCRIPT_MIGRATION_SESSION_BATCH)
                            .context("transcript migration batch size exceeds SQLite range")?
                    ],
                    |row| {
                        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                    },
                )
                .context("failed to query sessions for transcript migration")?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .context("failed to read a session batch for transcript migration")?
        };
        if encoded_sessions.is_empty() {
            break;
        }
        for (rowid, session_id) in encoded_sessions {
            last_rowid = rowid;
            tx.execute_batch("SAVEPOINT transcript_session_migration")
                .context("failed to start per-session transcript migration")?;
            match migrate_sqlite_v1_session_transcript(&tx, rowid, &session_id) {
                Ok(()) => {
                    tx.execute_batch("RELEASE transcript_session_migration")
                        .context("failed to commit per-session transcript migration")?;
                }
                Err(err) => {
                    tx.execute_batch(
                        "ROLLBACK TO transcript_session_migration;
                         RELEASE transcript_session_migration;",
                    )
                    .context("failed to roll back invalid per-session transcript migration")?;
                    skipped_sessions += 1;
                    eprintln!(
                        "persist> skipping invalid legacy session `{session_id}` during transcript migration: {err:#}"
                    );
                }
            }
        }
    }
    if skipped_sessions > 0 {
        eprintln!(
            "persist> skipped {skipped_sessions} invalid legacy session record(s) during transcript migration"
        );
    }

    tx.execute(
        "INSERT INTO meta(key, value) VALUES('schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![SQLITE_SCHEMA_VERSION],
    )
    .context("failed to advance SQLite state schema version")?;
    tx.commit()
        .context("failed to commit SQLite transcript migration")?;
    Ok(())
}

fn migrate_sqlite_v1_session_transcript(
    tx: &rusqlite::Transaction<'_>,
    rowid: i64,
    session_id: &str,
) -> Result<()> {
    // SQLite removes the embedded array before the value crosses into Rust.
    // `json_each` below then exposes only one explicit message batch at a time.
    let metadata_json: String = tx
        .query_row(
            "SELECT json_set(
                       value_json,
                       '$.session.messages',
                       json('[]'),
                       '$.session.messageCount',
                       json_array_length(value_json, '$.session.messages')
                     )
             FROM sessions
             WHERE rowid = ?1",
            rusqlite::params![rowid],
            |row| row.get(0),
        )
        .with_context(|| format!("failed to normalize legacy session `{session_id}` metadata"))?;
    let mut record: PersistedSessionRecord = serde_json::from_str(&metadata_json)
        .with_context(|| format!("failed to parse session `{session_id}` metadata"))?;
    if record.session.id != session_id {
        bail!(
            "row id `{session_id}` does not match embedded id `{}`",
            record.session.id
        );
    }
    backfill_persisted_session_defaults(&mut record.session);
    validate_persisted_session_fields(&record.session, record.external_session_id.as_deref())
        .with_context(|| format!("persisted session `{session_id}` failed validation"))?;
    validate_remote_proxy_identity(
        record.remote_id.as_deref(),
        record.remote_session_id.as_deref(),
    )
    .with_context(|| format!("persisted session `{session_id}` has invalid remote proxy identity"))?;
    let expected_message_count = usize::try_from(record.session.message_count)
        .context("legacy transcript count does not fit this platform")?;
    let mut next_position = 0_usize;
    loop {
        let message_rows = {
            let mut statement = tx
                .prepare(
                    "SELECT CAST(entry.key AS INTEGER),
                            json_extract(entry.value, '$.id'),
                            entry.value
                     FROM sessions AS session,
                          json_each(session.value_json, '$.session.messages') AS entry
                     WHERE session.rowid = ?1
                       AND CAST(entry.key AS INTEGER) >= ?2
                     ORDER BY CAST(entry.key AS INTEGER)
                     LIMIT ?3",
                )
                .with_context(|| {
                    format!("failed to prepare legacy transcript batch for `{session_id}`")
                })?;
            statement
                .query_map(
                    rusqlite::params![
                        rowid,
                        i64::try_from(next_position)
                            .context("legacy transcript position exceeds SQLite range")?,
                        i64::try_from(SQLITE_TRANSCRIPT_MIGRATION_MESSAGE_BATCH)
                            .context("transcript migration message batch exceeds SQLite range")?,
                    ],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .with_context(|| {
                    format!("failed to query legacy transcript batch for `{session_id}`")
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
                .with_context(|| {
                    format!("failed to read legacy transcript batch for `{session_id}`")
                })?
        };
        if message_rows.is_empty() {
            break;
        }
        for (position, message_id, encoded) in message_rows {
            let position = usize::try_from(position)
                .context("legacy transcript position is negative or too large")?;
            if position != next_position {
                bail!("legacy transcript has a gap at position {next_position}");
            }
            let message: Message = serde_json::from_str(&encoded).with_context(|| {
                format!("failed to parse legacy transcript position {position}")
            })?;
            if message.id() != message_id {
                bail!(
                    "legacy transcript position {position} id `{message_id}` does not match embedded id `{}`",
                    message.id()
                );
            }
            let (overview_kind, is_user) = conversation_overview_message_metadata(&message);
            tx.execute(
                "INSERT INTO messages(
                     session_id,
                     position,
                     message_id,
                     value_json,
                     overview_kind,
                     is_user
                 )
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    session_id,
                    i64::try_from(position)
                        .context("legacy transcript position exceeds SQLite range")?,
                    message_id,
                    encoded,
                    i64::try_from(conversation_overview_kind_index(overview_kind))
                        .context("overview kind exceeds SQLite integer range")?,
                    i64::from(is_user),
                ],
            )
            .with_context(|| {
                format!("failed to migrate transcript position {position} for `{session_id}`")
            })?;
            next_position = next_position.saturating_add(1);
        }
    }
    if next_position != expected_message_count {
        bail!(
            "legacy transcript expected {expected_message_count} messages but migrated {next_position}"
        );
    }
    let updated = tx
        .execute(
            "UPDATE sessions SET value_json = ?2 WHERE rowid = ?1",
            rusqlite::params![rowid, metadata_json],
        )
        .with_context(|| format!("failed to store migrated metadata for `{session_id}`"))?;
    if updated != 1 {
        bail!("legacy session row disappeared during transcript migration");
    }
    Ok(())
}

fn ensure_sqlite_state_schema_for_path(
    connection: &rusqlite::Connection,
    path: &FsPath,
) -> Result<()> {
    let write_lock = sqlite_state_write_lock(path);
    let _write_guard = lock_sqlite_state_writer(&write_lock);
    ensure_sqlite_state_schema(connection)
}

#[cfg(test)]
mod sqlite_schema_tests {
    use super::*;

    #[test]
    fn sqlite_schema_guard_rejects_unsupported_version_before_creating_state_tables() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        connection
            .execute_batch(
                "
                CREATE TABLE meta (
                  key TEXT PRIMARY KEY,
                  value TEXT NOT NULL
                );
                INSERT INTO meta(key, value) VALUES('schema_version', '0');
                ",
            )
            .expect("seed unsupported schema version");

        let error = ensure_sqlite_state_schema(&connection)
            .expect_err("unsupported schema version should be rejected");

        assert!(
            format!("{error:#}").contains("unsupported SQLite state schema version `0`"),
            "{error:#}"
        );
        let state_table_count: u32 = connection
            .query_row(
                "
                SELECT COUNT(*)
                FROM sqlite_master
                WHERE type = 'table'
                  AND name IN ('app_state', 'sessions', 'delegations')
                ",
                [],
                |row| row.get(0),
            )
            .expect("state table count should be queryable");
        assert_eq!(state_table_count, 0);
    }

    #[test]
    fn schema_v2_backfills_compact_session_overview_metadata() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        connection
            .execute_batch(
                "
                PRAGMA foreign_keys = ON;
                CREATE TABLE meta (
                  key TEXT PRIMARY KEY,
                  value TEXT NOT NULL
                );
                INSERT INTO meta(key, value) VALUES('schema_version', '2');
                CREATE TABLE sessions (
                  id TEXT PRIMARY KEY,
                  value_json TEXT NOT NULL
                );
                CREATE TABLE messages (
                  session_id TEXT NOT NULL,
                  position INTEGER NOT NULL CHECK(position >= 0),
                  message_id TEXT NOT NULL,
                  value_json TEXT NOT NULL,
                  PRIMARY KEY(session_id, position),
                  UNIQUE(session_id, message_id),
                  FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
                ) WITHOUT ROWID;
                CREATE TABLE session_overviews (
                  session_id TEXT PRIMARY KEY,
                  value_blob BLOB NOT NULL,
                  FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
                ) WITHOUT ROWID;
                INSERT INTO sessions(id, value_json) VALUES('session-1', '{}');
                INSERT INTO messages(session_id, position, message_id, value_json)
                VALUES(
                  'session-1',
                  0,
                  'message-1',
                  '{\"type\":\"command\",\"id\":\"message-1\",\"author\":\"you\",\"status\":\"error\"}'
                ), (
                  'session-1',
                  1,
                  'message-2',
                  '{\"type\":\"text\",\"id\":\"message-2\",\"author\":\"you\",\"text\":\"Human prompt\"}'
                ), (
                  'session-1',
                  2,
                  'message-3',
                  '{\"type\":\"text\",\"id\":\"message-3\",\"author\":\"you\",\"text\":\"Peer prompt\",\"source\":{\"sessionId\":\"peer-session\",\"name\":\"Peer\"}}'
                );
                ",
            )
            .expect("legacy v2 fixture should initialize");

        ensure_sqlite_state_schema(&connection)
            .expect("v2 overview metadata should backfill");

        let value_blob: Vec<u8> = connection
            .query_row(
                "SELECT value_blob
                 FROM session_overviews
                 WHERE session_id = 'session-1'",
                [],
                |row| row.get(0),
            )
            .expect("backfilled overview blob should exist");
        assert_eq!(
            value_blob,
            vec![
                encode_conversation_overview_message(
                    ConversationOverviewKind::Error,
                    true,
                ),
                encode_conversation_overview_message(
                    ConversationOverviewKind::Text,
                    true,
                ),
                encode_conversation_overview_message(
                    ConversationOverviewKind::Text,
                    true,
                ),
            ]
        );
    }

    #[test]
    fn schema_v2_isolates_malformed_overview_backfill_per_session() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        connection
            .execute_batch(
                "
                PRAGMA foreign_keys = ON;
                CREATE TABLE meta (
                  key TEXT PRIMARY KEY,
                  value TEXT NOT NULL
                );
                INSERT INTO meta(key, value) VALUES('schema_version', '2');
                CREATE TABLE sessions (
                  id TEXT PRIMARY KEY,
                  value_json TEXT NOT NULL
                );
                CREATE TABLE messages (
                  session_id TEXT NOT NULL,
                  position INTEGER NOT NULL CHECK(position >= 0),
                  message_id TEXT NOT NULL,
                  value_json TEXT NOT NULL,
                  PRIMARY KEY(session_id, position),
                  UNIQUE(session_id, message_id),
                  FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
                ) WITHOUT ROWID;
                CREATE TABLE session_overviews (
                  session_id TEXT PRIMARY KEY,
                  value_blob BLOB NOT NULL,
                  FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
                ) WITHOUT ROWID;
                INSERT INTO sessions(id, value_json)
                VALUES('healthy-local', '{}'), ('gapped-local', '{}');
                INSERT INTO messages(session_id, position, message_id, value_json)
                VALUES(
                  'healthy-local',
                  0,
                  'healthy-message',
                  '{\"type\":\"text\",\"id\":\"healthy-message\",\"author\":\"agent\",\"text\":\"ok\"}'
                ), (
                  'gapped-local',
                  4,
                  'gapped-message',
                  '{\"type\":\"text\",\"id\":\"gapped-message\",\"author\":\"agent\",\"text\":\"bad\"}'
                );
                ",
            )
            .expect("mixed local fixture should initialize");

        ensure_sqlite_state_schema(&connection)
            .expect("one malformed session must not abort global schema startup");

        let healthy_blob: Vec<u8> = connection
            .query_row(
                "SELECT value_blob
                 FROM session_overviews
                 WHERE session_id = 'healthy-local'",
                [],
                |row| row.get(0),
            )
            .expect("healthy local session should still be backfilled");
        assert_eq!(
            healthy_blob,
            vec![encode_conversation_overview_message(
                ConversationOverviewKind::Text,
                false,
            )]
        );
        let malformed_overview_count: u32 = connection
            .query_row(
                "SELECT COUNT(*)
                 FROM session_overviews
                 WHERE session_id = 'gapped-local'",
                [],
                |row| row.get(0),
            )
            .expect("malformed overview count should be queryable");
        assert_eq!(malformed_overview_count, 0);
    }

    #[test]
    fn fresh_state_schema_does_not_create_coordination_tables() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        ensure_sqlite_state_schema(&connection).expect("state schema should initialize");

        let coordination_table_count: u32 = connection
            .query_row(
                "
                SELECT COUNT(*)
                FROM sqlite_master
                WHERE type = 'table'
                  AND (
                    name LIKE 'mailbox%'
                    OR name LIKE 'coordination_board_%'
                  )
                ",
                [],
                |row| row.get(0),
            )
            .expect("coordination table count should be queryable");
        assert_eq!(coordination_table_count, 0);
    }

    #[test]
    fn transcript_schema_uses_indexed_range_and_cursor_queries() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        ensure_sqlite_state_schema(&connection).expect("state schema should initialize");

        let range_plan: Vec<String> = connection
            .prepare(
                "EXPLAIN QUERY PLAN
                 SELECT position, value_json
                 FROM messages
                 WHERE session_id = ?1 AND position >= ?2 AND position < ?3
                 ORDER BY position ASC",
            )
            .expect("range query plan should prepare")
            .query_map(rusqlite::params!["session-1", 10_i64, 20_i64], |row| {
                row.get(3)
            })
            .expect("range query plan should execute")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("range query plan rows should decode");
        assert!(
            range_plan
                .iter()
                .any(|detail| detail.contains("SEARCH messages USING PRIMARY KEY")),
            "range query must use the (session_id, position) primary key: {range_plan:?}"
        );
        assert!(
            range_plan.iter().all(|detail| !detail.contains("SCAN messages")),
            "range query must not scan messages: {range_plan:?}"
        );

        let cursor_plan: Vec<String> = connection
            .prepare(
                "EXPLAIN QUERY PLAN
                 SELECT position
                 FROM messages
                 WHERE session_id = ?1 AND message_id = ?2",
            )
            .expect("cursor query plan should prepare")
            .query_map(rusqlite::params!["session-1", "message-1"], |row| {
                row.get(3)
            })
            .expect("cursor query plan should execute")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("cursor query plan rows should decode");
        assert!(
            cursor_plan.iter().any(|detail| {
                detail.contains("SEARCH messages")
                    && detail.contains("session_id=?")
                    && detail.contains("message_id=?")
            }),
            "cursor query must use the unique (session_id, message_id) index: {cursor_plan:?}"
        );
        assert!(
            cursor_plan.iter().all(|detail| !detail.contains("SCAN messages")),
            "cursor query must not scan messages: {cursor_plan:?}"
        );
    }

    #[test]
    fn schema_v1_migration_moves_embedded_transcript_without_loss() {
        let expected_message_count = SQLITE_TRANSCRIPT_MIGRATION_MESSAGE_BATCH * 2 + 3;
        let mut inner = StateInner::new();
        let record = inner.create_session(
            Agent::Claude,
            Some("Migration".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        );
        let index = inner
            .find_session_index(&record.session.id)
            .expect("created session should exist");
        for sequence in 0..expected_message_count {
            let message = Message::Text {
                attachments: Vec::new(),
                id: format!("message-{sequence}"),
                timestamp: stamp_now(),
                author: Author::You,
                text: format!("body-{sequence}"),
                expanded_text: None,
                source: None,
            };
            let record = inner
                .session_mut_by_index(index)
                .expect("created session index should stay valid");
            let insert_at = record.session.messages.len();
            insert_message_on_record(record, insert_at, message);
        }
        let persisted = PersistedState::from_inner(&inner);
        let old_record = persisted
            .sessions
            .first()
            .expect("persisted session should exist");
        let old_encoded =
            serde_json::to_string(old_record).expect("v1 session should serialize");

        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        connection
            .execute_batch(
                "
                PRAGMA foreign_keys = ON;
                CREATE TABLE meta (
                  key TEXT PRIMARY KEY,
                  value TEXT NOT NULL
                );
                INSERT INTO meta(key, value) VALUES('schema_version', '1');
                CREATE TABLE sessions (
                  id TEXT PRIMARY KEY,
                  value_json TEXT NOT NULL
                );
                ",
            )
            .expect("v1 schema should initialize");
        connection
            .execute(
                "INSERT INTO sessions(id, value_json) VALUES(?1, ?2)",
                rusqlite::params![old_record.session.id, old_encoded],
            )
            .expect("v1 session should insert");

        ensure_sqlite_state_schema(&connection).expect("v1 schema should migrate");

        let schema_version: String = connection
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .expect("schema version should load");
        assert_eq!(schema_version, SQLITE_SCHEMA_VERSION);
        let migrated_metadata: String = connection
            .query_row(
                "SELECT value_json FROM sessions WHERE id = ?1",
                rusqlite::params![old_record.session.id],
                |row| row.get(0),
            )
            .expect("migrated metadata should load");
        let migrated_record: PersistedSessionRecord =
            serde_json::from_str(&migrated_metadata).expect("migrated metadata should parse");
        assert!(migrated_record.session.messages.is_empty());
        assert_eq!(
            usize::try_from(migrated_record.session.message_count).unwrap(),
            expected_message_count
        );

        let migrated_messages: Vec<(i64, String, String)> = connection
            .prepare(
                "SELECT position, message_id, value_json
                 FROM messages
                 WHERE session_id = ?1
                 ORDER BY position",
            )
            .expect("migrated transcript query should prepare")
            .query_map(rusqlite::params![old_record.session.id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .expect("migrated transcript query should execute")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("migrated transcript rows should decode");
        assert_eq!(migrated_messages.len(), expected_message_count);
        for (sequence, (position, message_id, encoded)) in
            migrated_messages.into_iter().enumerate()
        {
            assert_eq!(position, i64::try_from(sequence).unwrap());
            assert_eq!(message_id, format!("message-{sequence}"));
            let message: Message =
                serde_json::from_str(&encoded).expect("migrated message should parse");
            assert_eq!(message.id(), message_id);
            assert!(matches!(
                message,
                Message::Text { text, .. } if text == format!("body-{sequence}")
            ));
        }
    }

    #[test]
    fn schema_v1_migration_skips_one_invalid_session_without_blocking_valid_rows() {
        let mut inner = StateInner::new();
        inner.create_session(
            Agent::Claude,
            Some("Migration sibling".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        );
        let valid_record = PersistedState::from_inner(&inner)
            .sessions
            .into_iter()
            .next()
            .expect("persisted session should exist");
        let valid_session_id = valid_record.session.id.clone();
        let valid_encoded =
            serde_json::to_string(&valid_record).expect("valid v1 session should serialize");
        let mut invalid_value =
            serde_json::to_value(&valid_record).expect("invalid v1 fixture should encode");
        invalid_value["session"]["id"] = Value::String("embedded-other-session".to_owned());
        let invalid_encoded =
            serde_json::to_string(&invalid_value).expect("invalid v1 fixture should serialize");

        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        connection
            .execute_batch(
                "
                PRAGMA foreign_keys = ON;
                CREATE TABLE meta (
                  key TEXT PRIMARY KEY,
                  value TEXT NOT NULL
                );
                INSERT INTO meta(key, value) VALUES('schema_version', '1');
                CREATE TABLE sessions (
                  id TEXT PRIMARY KEY,
                  value_json TEXT NOT NULL
                );
                ",
            )
            .expect("v1 schema should initialize");
        connection
            .execute(
                "INSERT INTO sessions(id, value_json) VALUES(?1, ?2)",
                rusqlite::params![valid_session_id, valid_encoded],
            )
            .expect("valid v1 session should insert");
        connection
            .execute(
                "INSERT INTO sessions(id, value_json) VALUES('invalid-row', ?1)",
                rusqlite::params![invalid_encoded],
            )
            .expect("invalid sibling v1 session should insert");

        ensure_sqlite_state_schema(&connection)
            .expect("one invalid sibling must not abort the v1 migration");

        let schema_version: String = connection
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .expect("schema version should load");
        assert_eq!(schema_version, SQLITE_SCHEMA_VERSION);
        let valid_metadata: String = connection
            .query_row(
                "SELECT value_json FROM sessions WHERE id = ?1",
                rusqlite::params![valid_session_id],
                |row| row.get(0),
            )
            .expect("valid migrated sibling should remain readable");
        let valid_migrated: PersistedSessionRecord =
            serde_json::from_str(&valid_metadata).expect("valid migrated metadata should parse");
        assert_eq!(valid_migrated.session.id, valid_session_id);
    }
}

fn load_state_from_sqlite(path: &FsPath) -> Result<Option<StateInner>> {
    let connection = open_sqlite_state_connection(path)?;
    ensure_sqlite_state_schema_for_path(&connection, path)?;
    // `open_sqlite_state_connection` already hardens the fresh handle, but
    // schema initialization can create or recreate SQLite sidecars, so the
    // startup read path deliberately re-runs the full main/sidecar pass.
    harden_sqlite_state_file_permissions(path)?;
    let (
        mut session_records,
        mut quarantined_session_ids,
        mut skipped_session_records,
    ) =
        load_session_records_from_sqlite_with_skipped(&connection, path)?;
    let (
        delegation_records,
        quarantined_delegation_ids,
        delegation_table_row_count,
    ) = load_delegation_records_from_sqlite(&connection, path)?;
    let Some(encoded) = sqlite_app_state_value(&connection, SQLITE_METADATA_KEY, path)? else {
        return Ok(None);
    };
    let mut persisted: PersistedState = serde_json::from_str(&encoded)
        .with_context(|| format!("failed to parse persisted state from `{}`", path.display()))?;
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
    let loaded_delegations_from_table = apply_sqlite_delegation_records(
        &mut persisted,
        delegation_records,
        delegation_table_row_count > 0,
    );
    let mut inner = persisted.into_inner().with_context(|| {
        format!("failed to validate state from `{}`", path.display())
    })?;
    if !loaded_delegations_from_table && !inner.delegations.is_empty() {
        inner.mark_loaded_delegations_for_sqlite_migration();
    }
    Ok(Some(inner))
}

fn apply_sqlite_delegation_records(
    persisted: &mut PersistedState,
    delegation_records: Vec<DelegationRecord>,
    table_had_rows: bool,
) -> bool {
    if !table_had_rows {
        return false;
    }
    persisted.delegations = delegation_records;
    true
}

fn sqlite_app_state_value(
    connection: &rusqlite::Connection,
    key: &str,
    path: &FsPath,
) -> Result<Option<String>> {
    match connection.query_row(
        "SELECT value_json FROM app_state WHERE key = ?1",
        rusqlite::params![key],
        |row| row.get::<_, String>(0),
    ) {
        Ok(encoded) => Ok(Some(encoded)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(err) => Err(err)
            .with_context(|| format!("failed to read persisted state from `{}`", path.display())),
    }
}

#[cfg(test)]
fn load_session_records_from_sqlite(
    connection: &rusqlite::Connection,
    path: &FsPath,
) -> Result<Vec<PersistedSessionRecord>> {
    load_session_records_from_sqlite_with_skipped(connection, path)
        .map(|(records, _, _)| records)
}

fn load_session_records_from_sqlite_with_skipped(
    connection: &rusqlite::Connection,
    path: &FsPath,
) -> Result<(Vec<PersistedSessionRecord>, BTreeSet<String>, usize)> {
    let mut statement = connection
        .prepare("SELECT id, value_json FROM sessions ORDER BY rowid")
        .with_context(|| format!("failed to prepare session load from `{}`", path.display()))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .with_context(|| {
            format!(
                "failed to query persisted sessions from `{}`",
                path.display()
            )
        })?;
    let mut records = Vec::new();
    let mut quarantined_ids = BTreeSet::new();
    let mut skipped = 0;
    for row in rows {
        let (session_id, encoded) = match row {
            Ok(row) => row,
            Err(err) => {
                skipped += 1;
                eprintln!(
                    "persist> skipping unreadable session row from `{}`: {err:#}",
                    path.display()
                );
                continue;
            }
        };
        let loaded_record = (|| -> Result<PersistedSessionRecord> {
            let mut record: PersistedSessionRecord = serde_json::from_str(&encoded)
                .with_context(|| format!("failed to parse persisted session `{session_id}`"))?;
            if record.session.id != session_id {
                bail!(
                    "row id `{session_id}` does not match embedded id `{}`",
                    record.session.id
                );
            }
            backfill_persisted_session_defaults(&mut record.session);
            validate_persisted_session_fields(
                &record.session,
                record.external_session_id.as_deref(),
            )
            .with_context(|| format!("persisted session `{session_id}` failed validation"))?;
            validate_remote_proxy_identity(
                record.remote_id.as_deref(),
                record.remote_session_id.as_deref(),
            )
            .with_context(|| {
                format!("persisted session `{session_id}` has invalid remote proxy identity")
            })?;
            load_persisted_session_tail(connection, path, &mut record)?;
            load_persisted_prompt_history(connection, path, &mut record)?;
            Ok(record)
        })();
        match loaded_record {
            Ok(record) => records.push(record),
            Err(err) => {
                skipped += 1;
                quarantined_ids.insert(session_id.clone());
                eprintln!("persist> skipping invalid session `{session_id}`: {err:#}");
            }
        }
    }
    Ok((records, quarantined_ids, skipped))
}

fn load_persisted_prompt_history(
    connection: &rusqlite::Connection,
    path: &FsPath,
    record: &mut PersistedSessionRecord,
) -> Result<()> {
    let stored_history = connection
        .query_row(
            "SELECT value_json
             FROM session_prompt_histories
             WHERE session_id = ?1",
            rusqlite::params![record.session.id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .with_context(|| {
            format!(
                "failed to load persisted prompt history for `{}` from `{}`",
                record.session.id,
                path.display()
            )
        })?;
    if let Some(encoded) = stored_history {
        let prompts = serde_json::from_str::<Vec<String>>(&encoded).with_context(|| {
            format!(
                "failed to parse persisted prompt history for `{}`",
                record.session.id
            )
        })?;
        record.session.prompt_history = normalize_prompt_history(prompts);
        return Ok(());
    }

    // Compatibility fallback for a database created before the separate
    // history row was introduced. The schema migration normally moves this
    // value first, but keeping the fallback makes isolated row loads robust.
    if !record.session.prompt_history.is_empty() {
        record.session.prompt_history =
            normalize_prompt_history(std::mem::take(&mut record.session.prompt_history));
        return Ok(());
    }

    let mut statement = connection
        .prepare(
            "SELECT value_json
             FROM messages
             WHERE session_id = ?1 AND is_user = 1
             ORDER BY position DESC
             LIMIT ?2",
        )
        .with_context(|| {
            format!(
                "failed to prepare persisted prompt history for `{}`",
                record.session.id
            )
        })?;
    let rows = statement
        .query_map(
            rusqlite::params![
                record.session.id,
                i64::try_from(SESSION_PROMPT_HISTORY_LIMIT)
                    .context("prompt history limit exceeds SQLite integer range")?
            ],
            |row| row.get::<_, String>(0),
        )
        .with_context(|| {
            format!(
                "failed to query persisted prompt history for `{}`",
                record.session.id
            )
        })?;

    let mut prompts = Vec::new();
    for row in rows {
        let encoded = row.with_context(|| {
            format!(
                "failed to read persisted prompt history for `{}` from `{}`",
                record.session.id,
                path.display()
            )
        })?;
        match serde_json::from_str::<Message>(&encoded) {
            Ok(message) => {
                if let Some(prompt) = message.user_prompt_text() {
                    prompts.push(prompt.to_owned());
                }
            }
            Err(err) => {
                // Prompt history is advisory. A malformed older row outside
                // the validated startup tail must not quarantine an otherwise
                // usable session; normal transcript paging will still surface
                // the row-level failure if the user reaches that history.
                eprintln!(
                    "persist> skipping unreadable prompt-history message for `{}` from `{}`: {err:#}",
                    record.session.id,
                    path.display()
                );
            }
        }
    }
    prompts.reverse();
    record.session.prompt_history = normalize_prompt_history(prompts);
    Ok(())
}

fn load_persisted_session_tail(
    connection: &rusqlite::Connection,
    path: &FsPath,
    record: &mut PersistedSessionRecord,
) -> Result<()> {
    let total_message_count = usize::try_from(record.session.message_count)
        .context("persisted transcript count does not fit this platform")?;
    let start_index = total_message_count.saturating_sub(SQLITE_SESSION_TAIL_MESSAGES);
    let mut statement = connection
        .prepare(
            "SELECT position, message_id, value_json
             FROM messages
             WHERE session_id = ?1 AND position >= ?2 AND position < ?3
             ORDER BY position ASC",
        )
        .with_context(|| {
            format!(
                "failed to prepare persisted transcript tail for `{}`",
                record.session.id
            )
        })?;
    let rows = statement
        .query_map(
            rusqlite::params![
                record.session.id,
                i64::try_from(start_index)
                    .context("persisted transcript position exceeds SQLite integer range")?,
                i64::try_from(total_message_count)
                    .context("persisted transcript count exceeds SQLite integer range")?
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .with_context(|| {
            format!(
                "failed to query persisted transcript tail for `{}`",
                record.session.id
            )
        })?;
    let mut messages = Vec::new();
    let mut unusable_tail: Option<String> = None;
    for (expected_position, row) in (start_index..total_message_count).zip(rows) {
        let (position, message_id, encoded) = row.with_context(|| {
            format!(
                "failed to read persisted transcript tail for `{}` from `{}`",
                record.session.id,
                path.display()
            )
        })?;
        let actual_position = usize::try_from(position)
            .context("persisted transcript position is negative or too large")?;
        if actual_position != expected_position {
            unusable_tail = Some(format!("gap at position {expected_position}"));
            break;
        }
        let message: Message = serde_json::from_str(&encoded).with_context(|| {
            format!(
                "failed to parse transcript position {actual_position} for `{}`",
                record.session.id
            )
        })?;
        if message.id() != message_id {
            bail!(
                "transcript position {actual_position} row id `{message_id}` does not match embedded id `{}` for `{}`",
                message.id(),
                record.session.id
            );
        }
        messages.push(message);
    }
    let expected_tail_len = total_message_count.saturating_sub(start_index);
    if unusable_tail.is_none() && messages.len() != expected_tail_len {
        unusable_tail = Some(format!(
            "expected {expected_tail_len} retained messages but loaded {}",
            messages.len()
        ));
    }
    // A REMOTE-PROXY transcript whose local rows do not cover the range
    // implied by `message_count` is a hydration state, not corruption: the
    // transcript lives on the remote host, so its metadata can legitimately
    // know a count while zero `messages` rows exist locally.
    //
    // A LOCAL session in the same shape must be quarantined instead. In
    // particular, a v1 transcript migration can roll back one session while
    // the outer schema migration advances to v2. Its legacy row still holds
    // the only embedded transcript copy, but there are no normalized rows.
    // Treating that as ordinary hydration clears the embedded messages in
    // memory and lets the next persist overwrite the recovery copy. Return an
    // error before mutating `record`; the row-level loader records the session
    // in the runtime quarantine, and full persistence preserves its untouched
    // row. The same local/remote distinction also fails safe for normalized
    // local rows lost or damaged after migration.
    if let Some(reason) = unusable_tail {
        let is_remote_proxy = validate_remote_proxy_identity(
            record.remote_id.as_deref(),
            record.remote_session_id.as_deref(),
        )?
        .is_some();
        if !is_remote_proxy {
            bail!(
                "local session `{}` transcript tail is inconsistent ({reason}); preserving its persisted row for recovery",
                record.session.id
            );
        }
        if !messages.is_empty() {
            // A partial tail means the rows themselves disagree with the
            // metadata, which is worth surfacing. Zero rows is the ordinary
            // unhydrated/proxy case and stays quiet to avoid boot-time noise
            // proportional to the number of proxy sessions.
            eprintln!(
                "persist> session `{}` transcript tail is inconsistent ({reason}); starting it unhydrated",
                record.session.id
            );
        }
        record.message_start_index = total_message_count;
        record.session.messages = Vec::new();
        record.session.messages_loaded = total_message_count == 0;
        return Ok(());
    }
    record.message_start_index = start_index;
    record.session.messages = messages;
    record.session.messages_loaded = start_index == 0;
    Ok(())
}

fn persisted_message_position(
    path: &FsPath,
    session_id: &str,
    message_id: &str,
) -> Result<Option<usize>> {
    let connection = open_sqlite_state_read_connection(path)?;
    persisted_message_position_with_connection(&connection, session_id, message_id)
}

fn persisted_message_position_with_connection(
    connection: &rusqlite::Connection,
    session_id: &str,
    message_id: &str,
) -> Result<Option<usize>> {
    match connection.query_row(
        "SELECT position
         FROM messages
         WHERE session_id = ?1 AND message_id = ?2",
        rusqlite::params![session_id, message_id],
        |row| row.get::<_, i64>(0),
    ) {
        Ok(position) => Ok(Some(
            usize::try_from(position)
                .context("persisted transcript cursor position is negative or too large")?,
        )),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(err) => Err(err).with_context(|| {
            format!("failed to resolve transcript cursor for session `{session_id}`")
        }),
    }
}

#[cfg(test)]
fn load_persisted_message_range(
    path: &FsPath,
    session_id: &str,
    start_index: usize,
    end_index: usize,
) -> Result<Vec<(usize, Message)>> {
    if start_index >= end_index {
        return Ok(Vec::new());
    }
    let connection = open_sqlite_state_read_connection(path)?;
    load_persisted_message_range_with_connection(
        &connection,
        session_id,
        start_index,
        end_index,
    )
}

fn load_persisted_message_range_with_connection(
    connection: &rusqlite::Connection,
    session_id: &str,
    start_index: usize,
    end_index: usize,
) -> Result<Vec<(usize, Message)>> {
    if start_index >= end_index {
        return Ok(Vec::new());
    }
    let mut statement = connection
        .prepare(
            "SELECT position, message_id, value_json
             FROM messages
             WHERE session_id = ?1 AND position >= ?2 AND position < ?3
             ORDER BY position ASC",
        )
        .with_context(|| {
            format!("failed to prepare transcript page for session `{session_id}`")
        })?;
    let rows = statement
        .query_map(
            rusqlite::params![
                session_id,
                i64::try_from(start_index)
                    .context("transcript page start exceeds SQLite integer range")?,
                i64::try_from(end_index)
                    .context("transcript page end exceeds SQLite integer range")?,
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .with_context(|| format!("failed to query transcript page for session `{session_id}`"))?;
    let mut messages = Vec::new();
    for row in rows {
        let (position, message_id, encoded) = row
            .with_context(|| format!("failed to read transcript page for session `{session_id}`"))?;
        let position = usize::try_from(position)
            .context("persisted transcript position is negative or too large")?;
        let message: Message = serde_json::from_str(&encoded).with_context(|| {
            format!("failed to parse transcript position {position} for session `{session_id}`")
        })?;
        if message.id() != message_id {
            bail!(
                "transcript position {position} row id `{message_id}` does not match embedded id `{}` for session `{session_id}`",
                message.id()
            );
        }
        messages.push((position, message));
    }
    Ok(messages)
}

/// Loads the persisted prefix from one compact byte per message.
///
/// Persisted transcripts can be much larger than the retained in-memory tail.
/// The blob is maintained transactionally with transcript rows, so a rail read
/// never allocates full message payloads or steps through 25k SQLite rows.
fn load_persisted_message_overview_with_connection(
    connection: &rusqlite::Connection,
    session_id: &str,
    end_index: usize,
    message_count: usize,
    bucket_count: usize,
) -> Result<Vec<(usize, ConversationOverviewKind, u32, u32)>> {
    if end_index == 0 {
        return Ok(Vec::new());
    }
    let value_blob = connection
        .query_row(
            "SELECT value_blob
             FROM session_overviews
             WHERE session_id = ?1",
            rusqlite::params![session_id],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .with_context(|| {
            format!("failed to load transcript overview blob for session `{session_id}`")
        })?;
    if value_blob.len() < end_index {
        bail!(
            "persisted transcript overview for `{session_id}` has {} positions but needs {end_index}",
            value_blob.len()
        );
    }
    let mut kind_counts = vec![[0_u32; 4]; bucket_count];
    let mut user_counts = vec![0_u32; bucket_count];
    for (position, encoded) in value_blob.into_iter().take(end_index).enumerate() {
        let (kind, is_user) = decode_conversation_overview_message(encoded)
            .with_context(|| format!("invalid overview position {position} for `{session_id}`"))?;
        let bucket_index =
            conversation_overview_bucket_index(position, message_count, bucket_count);
        let kind_index = conversation_overview_kind_index(kind);
        kind_counts[bucket_index][kind_index] =
            kind_counts[bucket_index][kind_index].saturating_add(1);
        user_counts[bucket_index] =
            user_counts[bucket_index].saturating_add(u32::from(is_user));
    }
    let mut overview = Vec::with_capacity(bucket_count.saturating_mul(2));
    for bucket_index in 0..bucket_count {
        for kind_index in 0..4 {
            let count = kind_counts[bucket_index][kind_index];
            if count == 0 {
                continue;
            }
            let kind = match kind_index {
                0 => ConversationOverviewKind::Text,
                1 => ConversationOverviewKind::Command,
                2 => ConversationOverviewKind::Diff,
                3 => ConversationOverviewKind::Error,
                _ => unreachable!("overview kind index is bounded"),
            };
            // Author counts are bucket-wide, so attach them to the first
            // nonempty kind group and leave the remaining groups at zero.
            let user_count = std::mem::take(&mut user_counts[bucket_index]);
            overview.push((bucket_index, kind, count, user_count));
        }
    }
    Ok(overview)
}

fn load_delegation_records_from_sqlite(
    connection: &rusqlite::Connection,
    path: &FsPath,
) -> Result<(Vec<DelegationRecord>, BTreeSet<String>, usize)> {
    let mut statement = connection
        .prepare("SELECT id, value_json FROM delegations ORDER BY rowid")
        .with_context(|| format!("failed to prepare delegation load from `{}`", path.display()))?;
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
    let mut row_count = 0_usize;
    for row in rows {
        let (delegation_id, encoded) = match row {
            Ok(row) => row,
            Err(err) => {
                skipped += 1;
                eprintln!(
                    "persist> skipping unreadable delegation row from `{}`: {err:#}",
                    path.display()
                );
                continue;
            }
        };
        row_count += 1;
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
    Ok((records, quarantined_ids, row_count))
}

struct SerializedPersistedMessage {
    position: usize,
    message_id: String,
    value_json: String,
    overview_kind: ConversationOverviewKind,
    is_user: bool,
}

struct SerializedPersistedSession {
    session_id: String,
    message_start_index: usize,
    message_count: usize,
    write_overview: bool,
    prompt_history_value_json: Option<String>,
    value_json: String,
    messages: Vec<SerializedPersistedMessage>,
}

fn serialize_persisted_session(
    record: &PersistedSessionRecord,
) -> Result<SerializedPersistedSession> {
    let remote_proxy_identity = validate_remote_proxy_identity(
        record.remote_id.as_deref(),
        record.remote_session_id.as_deref(),
    )
    .with_context(|| {
        format!(
            "persisted session `{}` has invalid remote proxy identity",
            record.session.id
        )
    })?;
    let mut metadata = record.clone();
    let retained_end = record
        .message_start_index
        .checked_add(record.session.messages.len())
        .context("persisted transcript position overflow")?;
    let total_message_count = if record.session.messages_loaded {
        retained_end
    } else {
        retained_end.max(
            usize::try_from(record.session.message_count)
                .context("persisted transcript count does not fit this platform")?,
        )
    };
    let prompt_history_value_json = record
        .persist_prompt_history
        .then(|| {
            serde_json::to_string(&record.session.prompt_history)
                .context("failed to serialize persisted prompt history")
        })
        .transpose()?;
    metadata.session.messages.clear();
    // Prompt history has an independent mutation watermark and SQLite row. It
    // must not inflate the metadata JSON rewritten by every streaming commit.
    metadata.session.prompt_history.clear();
    metadata.session.message_count =
        u32::try_from(total_message_count).context("persisted transcript exceeds wire limit")?;
    metadata.session.messages_loaded = total_message_count == 0;
    metadata.message_start_index = 0;

    let value_json =
        serde_json::to_string(&metadata).context("failed to serialize persisted session metadata")?;
    let messages = record
        .session
        .messages
        .iter()
        .enumerate()
        .map(|(local_index, message)| {
            let position = record
                .message_start_index
                .checked_add(local_index)
                .context("persisted transcript position overflow")?;
            let value_json = serde_json::to_string(message)
                .context("failed to serialize persisted transcript message")?;
            let (overview_kind, is_user) = conversation_overview_message_metadata(message);
            Ok(SerializedPersistedMessage {
                position,
                message_id: message.id().to_owned(),
                value_json,
                overview_kind,
                is_user,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(SerializedPersistedSession {
        session_id: record.session.id.clone(),
        message_start_index: record.message_start_index,
        message_count: total_message_count,
        write_overview: remote_proxy_identity.is_none(),
        prompt_history_value_json,
        value_json,
        messages,
    })
}

fn serialize_persisted_sessions_with_isolation(
    sessions: &[PersistedSessionRecord],
) -> Vec<SerializedPersistedSession> {
    let mut serialized_sessions = Vec::with_capacity(sessions.len());
    let mut skipped = 0_usize;
    for record in sessions {
        match serialize_persisted_session(record) {
            Ok(session) => serialized_sessions.push(session),
            Err(err) => {
                skipped += 1;
                eprintln!(
                    "persist> preserving the last good row for invalid in-memory session `{}`: {err:#}",
                    record.session.id
                );
            }
        }
    }
    if skipped > 0 {
        eprintln!(
            "persist> skipped {skipped} invalid in-memory session record(s) while persisting"
        );
    }
    serialized_sessions
}

fn write_serialized_persisted_session(
    tx: &rusqlite::Transaction<'_>,
    session: &SerializedPersistedSession,
) -> Result<()> {
    tx.execute(
        "INSERT INTO sessions(id, value_json) VALUES(?1, ?2)
         ON CONFLICT(id) DO UPDATE SET value_json = excluded.value_json",
        rusqlite::params![session.session_id, session.value_json],
    )
    .with_context(|| {
        format!(
            "failed to write persisted session metadata for `{}`",
            session.session_id
        )
    })?;
    if let Some(prompt_history_value_json) = &session.prompt_history_value_json {
        tx.execute(
            "INSERT INTO session_prompt_histories(session_id, value_json)
             VALUES(?1, ?2)
             ON CONFLICT(session_id) DO UPDATE SET value_json = excluded.value_json",
            rusqlite::params![session.session_id, prompt_history_value_json],
        )
        .with_context(|| {
            format!(
                "failed to write persisted prompt history for `{}`",
                session.session_id
            )
        })?;
    }
    tx.execute(
        "DELETE FROM messages WHERE session_id = ?1 AND position >= ?2",
        rusqlite::params![
            session.session_id,
            i64::try_from(session.message_start_index)
                .context("persisted transcript position exceeds SQLite integer range")?
        ],
    )
    .with_context(|| {
        format!(
            "failed to replace persisted transcript tail for `{}`",
            session.session_id
        )
    })?;
    let mut insert = tx
        .prepare_cached(
            "INSERT INTO messages(
                 session_id,
                 position,
                 message_id,
                 value_json,
                 overview_kind,
                 is_user
             )
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .context("failed to prepare persisted transcript write")?;
    for message in &session.messages {
        insert
            .execute(rusqlite::params![
                session.session_id,
                i64::try_from(message.position)
                    .context("persisted transcript position exceeds SQLite integer range")?,
                message.message_id,
                message.value_json,
                i64::try_from(conversation_overview_kind_index(message.overview_kind))
                    .context("overview kind exceeds SQLite integer range")?,
                i64::from(message.is_user),
            ])
            .with_context(|| {
                format!(
                    "failed to write transcript position {} for `{}`",
                    message.position, session.session_id
                )
            })?;
    }
    if !session.write_overview {
        tx.execute(
            "DELETE FROM session_overviews WHERE session_id = ?1",
            rusqlite::params![session.session_id],
        )
        .with_context(|| {
            format!(
                "failed to clear local transcript overview for remote proxy `{}`",
                session.session_id
            )
        })?;
        return Ok(());
    }
    let mut overview_blob = if session.message_start_index == 0 {
        Vec::with_capacity(session.message_count)
    } else {
        let existing = match tx.query_row(
            "SELECT value_blob
             FROM session_overviews
             WHERE session_id = ?1",
            rusqlite::params![session.session_id],
            |row| row.get::<_, Vec<u8>>(0),
        ) {
            Ok(existing) => existing,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                let mut statement = tx
                    .prepare(
                        "SELECT overview_kind, is_user
                         FROM messages
                         WHERE session_id = ?1 AND position < ?2
                         ORDER BY position",
                    )
                    .context("failed to prepare transcript overview prefix recovery")?;
                statement
                    .query_map(
                        rusqlite::params![
                            session.session_id,
                            i64::try_from(session.message_start_index).context(
                                "persisted transcript position exceeds SQLite integer range"
                            )?,
                        ],
                        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?)),
                    )
                    .context("failed to query transcript overview prefix recovery")?
                    .map(|row| {
                        let (kind_index, is_user) =
                            row.context("failed to read transcript overview prefix")?;
                        let kind = match kind_index {
                            0 => ConversationOverviewKind::Text,
                            1 => ConversationOverviewKind::Command,
                            2 => ConversationOverviewKind::Diff,
                            3 => ConversationOverviewKind::Error,
                            _ => bail!("invalid persisted transcript overview kind {kind_index}"),
                        };
                        Ok(encode_conversation_overview_message(kind, is_user))
                    })
                    .collect::<Result<Vec<_>>>()?
            }
            Err(err) => {
                return Err(err).context("failed to load persisted transcript overview prefix");
            }
        };
        if existing.len() < session.message_start_index {
            bail!(
                "persisted transcript overview for `{}` has {} positions but tail starts at {}",
                session.session_id,
                existing.len(),
                session.message_start_index
            );
        }
        let mut prefix = existing;
        prefix.truncate(session.message_start_index);
        prefix.reserve(session.messages.len());
        prefix
    };
    overview_blob.extend(
        session
            .messages
            .iter()
            .map(|message| encode_conversation_overview_message(message.overview_kind, message.is_user)),
    );
    if overview_blob.len() != session.message_count {
        bail!(
            "persisted transcript overview for `{}` has {} positions but metadata expects {}",
            session.session_id,
            overview_blob.len(),
            session.message_count
        );
    }
    tx.execute(
        "INSERT INTO session_overviews(session_id, value_blob)
         VALUES(?1, ?2)
         ON CONFLICT(session_id) DO UPDATE SET value_blob = excluded.value_blob",
        rusqlite::params![session.session_id, overview_blob],
    )
    .with_context(|| {
        format!(
            "failed to write persisted transcript overview for `{}`",
            session.session_id
        )
    })?;
    Ok(())
}

fn remove_missing_persisted_sessions(
    tx: &rusqlite::Transaction<'_>,
    retained_session_ids: &HashSet<&str>,
    quarantined_session_ids: &BTreeSet<String>,
) -> Result<()> {
    let stored_session_ids = {
        let mut statement = tx
            .prepare("SELECT id FROM sessions")
            .context("failed to prepare stored-session replacement")?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .context("failed to query stored sessions for replacement")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to read stored sessions for replacement")?
    };
    for session_id in stored_session_ids {
        if retained_session_ids.contains(session_id.as_str())
            || quarantined_session_ids.contains(&session_id)
        {
            continue;
        }
        tx.execute(
            "DELETE FROM sessions WHERE id = ?1",
            rusqlite::params![session_id],
        )
        .context("failed to remove stale persisted session")?;
    }
    Ok(())
}

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

fn persist_persisted_state_to_sqlite(path: &FsPath, persisted: &PersistedState) -> Result<()> {
    let metadata = persisted.metadata_only();
    persist_state_parts_to_sqlite(
        path,
        &metadata,
        &persisted.sessions,
        true,
        &persisted.quarantined_persisted_session_ids,
        &persisted.delegations,
        true,
        &persisted.quarantined_persisted_delegation_ids,
    )
}

fn persist_created_session(
    path: &FsPath,
    inner: &StateInner,
    _record: &SessionRecord,
) -> Result<()> {
    let persisted = PersistedState::from_inner(inner);
    persist_persisted_state_to_sqlite(path, &persisted)
}

fn persist_state_parts_to_sqlite(
    path: &FsPath,
    metadata: &PersistedState,
    sessions: &[PersistedSessionRecord],
    replace_sessions: bool,
    quarantined_session_ids: &BTreeSet<String>,
    delegations: &[DelegationRecord],
    replace_delegations: bool,
    quarantined_delegation_ids: &BTreeSet<String>,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        create_local_state_directory(parent)?;
    }

    let mut connection = open_sqlite_state_connection(path)?;
    ensure_sqlite_state_schema_for_path(&connection, path)?;
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
    )
}

/// Applies one persist transaction to an already-open SQLite connection.
///
/// Assumes the caller has run [`ensure_sqlite_state_schema`] at least once
/// for this connection. Used by the background persist thread so the
/// per-persist hot path does not pay for opening a fresh connection or
/// re-running the schema-version upsert on every commit.
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
) -> Result<()> {
    let metadata_json =
        serde_json::to_string(metadata).context("failed to serialize persisted state metadata")?;
    let serialized_sessions = serialize_persisted_sessions_with_isolation(sessions);
    let serialized_delegations = delegations
        .iter()
        .map(|delegation| {
            serde_json::to_string(delegation)
                .context("failed to serialize persisted delegation")
                .map(|json| (delegation.id.as_str(), json))
        })
        .collect::<Result<Vec<_>>>()?;
    let write_lock = sqlite_state_write_lock(path);
    let write_guard = lock_sqlite_state_writer(&write_lock);
    let tx = connection.transaction().with_context(|| {
        format!(
            "failed to start SQLite transaction for `{}`",
            path.display()
        )
    })?;
    tx.execute(
        "INSERT INTO app_state(key, value_json) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
        rusqlite::params![SQLITE_METADATA_KEY, metadata_json],
    )
    .with_context(|| format!("failed to write state metadata to `{}`", path.display()))?;
    if replace_sessions {
        // Retain every session from the snapshot, including any session whose
        // current in-memory value failed validation. Such a session is skipped
        // above so its last known-good SQLite row remains recoverable.
        let retained_session_ids = sessions
            .iter()
            .map(|session| session.session.id.as_str())
            .collect::<HashSet<_>>();
        remove_missing_persisted_sessions(
            &tx,
            &retained_session_ids,
            quarantined_session_ids,
        )
            .with_context(|| format!("failed to replace sessions in `{}`", path.display()))?;
    }
    for session in &serialized_sessions {
        write_serialized_persisted_session(&tx, session)
            .with_context(|| format!("failed to write persisted session to `{}`", path.display()))?;
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
    tx.commit()
        .with_context(|| format!("failed to commit persisted state to `{}`", path.display()))?;
    drop(write_guard);
    // Keep post-commit redirection and owner-only permission verification
    // fatal. The chmod helper itself honors
    // TERMAL_ALLOW_INSECURE_STATE_PERMISSIONS when the operator explicitly
    // accepts insecure state-file modes.
    verify_persist_commit_integrity(path)?;
    Ok(())
}

/// Thread-local SQLite connection cache for the background persist thread.
///
/// Every queued persist previously opened a fresh SQLite connection and
/// re-ran `ensure_sqlite_state_schema`, which writes `schema_version`
/// every call. The persist thread writes many times during an active
/// session, so amortizing that fixed cost to one open-and-validate per
/// thread lifetime removes the biggest per-persist overhead.
struct SqlitePersistConnectionCache {
    path: Option<PathBuf>,
    connection: Option<rusqlite::Connection>,
}

impl SqlitePersistConnectionCache {
    fn new() -> Self {
        Self {
            path: None,
            connection: None,
        }
    }

    /// Returns a mutable reference to a SQLite connection opened for
    /// `path`, reusing the cached connection when the path matches.
    /// Runs schema validation only when a fresh connection is opened.
    fn connection_for(&mut self, path: &FsPath) -> Result<&mut rusqlite::Connection> {
        let matches_cache = self.path.as_deref() == Some(path);
        if !matches_cache {
            // Path changed (or first open): open+validate the replacement
            // first so a transient failure does not speculatively discard a
            // still-working cached connection.
            if let Some(parent) = path.parent() {
                create_local_state_directory(parent)?;
            }
            let connection = open_sqlite_state_connection(path)?;
            ensure_sqlite_state_schema_for_path(&connection, path)?;
            // Deliberately repeat the open-time hardening after schema
            // validation because SQLite may create sidecars between the two
            // points; cached reuses skip this until the next successful commit.
            harden_sqlite_state_file_permissions(path)?;
            self.path = Some(path.to_path_buf());
            self.connection = Some(connection);
        }
        Ok(self
            .connection
            .as_mut()
            .expect("connection was just cached for the requested path"))
    }

    /// Drops the cached connection so the next `connection_for` call
    /// reopens fresh and re-runs `ensure_sqlite_state_schema`.
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
fn persist_delta_via_cache(
    cache: &mut SqlitePersistConnectionCache,
    path: &FsPath,
    delta: &PersistDelta,
) -> Result<Vec<String>> {
    let result = persist_delta_via_cache_inner(cache, path, delta);
    if result.is_err() {
        cache.invalidate();
    }
    result
}

fn persist_delta_via_cache_inner(
    cache: &mut SqlitePersistConnectionCache,
    path: &FsPath,
    delta: &PersistDelta,
) -> Result<Vec<String>> {
    let metadata_json = serde_json::to_string(&delta.metadata)
        .context("failed to serialize persisted state metadata")?;
    let serialized_sessions =
        serialize_persisted_sessions_with_isolation(&delta.changed_sessions);
    let persisted_session_ids = serialized_sessions
        .iter()
        .map(|session| session.session_id.clone())
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
    let connection = cache.connection_for(path)?;
    // Keep state-path redirection failures fatal on cached writes too. Directory
    // chmod hardening runs when the cached connection is opened; the hot path
    // intentionally repeats only symlink/reparse checks before each transaction
    // so path swaps are caught without chmoding the state directory every tick.
    reject_existing_sqlite_state_path_redirection(path)?;
    let write_lock = sqlite_state_write_lock(path);
    let write_guard = lock_sqlite_state_writer(&write_lock);
    let tx = connection.transaction().with_context(|| {
        format!(
            "failed to start SQLite transaction for `{}`",
            path.display()
        )
    })?;
    tx.execute(
        "INSERT INTO app_state(key, value_json) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
        rusqlite::params![SQLITE_METADATA_KEY, metadata_json],
    )
    .with_context(|| {
        format!(
            "failed to write state metadata to `{}`",
            path.display()
        )
    })?;
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
    for session in &serialized_sessions {
        write_serialized_persisted_session(&tx, session).with_context(|| {
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
    tx.commit().with_context(|| {
        format!(
            "failed to commit persisted state to `{}`",
            path.display()
        )
    })?;
    drop(write_guard);
    // Keep post-commit redirection and owner-only permission verification
    // fatal. The chmod helper itself honors
    // TERMAL_ALLOW_INSECURE_STATE_PERMISSIONS when the operator explicitly
    // accepts insecure state-file modes.
    verify_persist_commit_integrity(path)?;
    Ok(persisted_session_ids)
}

/// Persists state from a pre-built `PersistedState` snapshot.
fn persist_state_from_persisted(path: &FsPath, persisted: &PersistedState) -> Result<()> {
    persist_persisted_state_to_sqlite(path, persisted)
}

/// Persists state directly from `StateInner` (used in tests for synchronous
/// setup of persisted state files).
#[cfg(test)]
fn persist_state(path: &FsPath, inner: &StateInner) -> Result<()> {
    let persisted = PersistedState::from_inner(inner);
    persist_state_from_persisted(path, &persisted)
}
