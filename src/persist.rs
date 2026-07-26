/*
SQLite-backed session state persistence.

Owns the on-disk schema (`ensure_sqlite_state_schema`), connection lifecycle
(`open_sqlite_state_connection`, `SqlitePersistConnectionCache`), load path
(`load_state`, `load_state_from_sqlite`), and the per-transaction write helpers
used by the background persist thread
(`persist_state_parts_via_connection`, `persist_delta_via_cache`,
`persist_created_session`, `persist_state_from_persisted`, `persist_state`).

Extracted from `api.rs` so HTTP handler code and SQLite persistence live
in separate files. The crate still compiles as one `include!()`-assembled
module, so no visibility changes are required.
*/

/// Resolves persistence path.
fn resolve_persistence_path(default_workdir: &str) -> PathBuf {
    resolve_termal_data_dir(default_workdir).join("termal.sqlite")
}

const SQLITE_SCHEMA_VERSION: &str = "1";
const SQLITE_METADATA_KEY: &str = "metadataState";
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Per-database writer locks shared by every in-process SQLite write path.
///
/// WAL lets readers coexist, but SQLite still permits only one writer. The
/// state persist worker and durable mailbox store own separate connections, so
/// relying on SQLite's busy timeout alone can surface ordinary in-process
/// contention as `SQLITE_BUSY`. Serialize those writers before `BEGIN`; the
/// timeout remains a boundary for external processes or OS-level locks.
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
    if let Some(stored_schema_version) = stored_schema_version.as_deref() {
        if stored_schema_version != SQLITE_SCHEMA_VERSION {
            bail!(
                "unsupported SQLite state schema version `{}`; this binary supports `{}`",
                stored_schema_version,
                SQLITE_SCHEMA_VERSION
            );
        }
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

            CREATE TABLE IF NOT EXISTS delegations (
              id TEXT PRIMARY KEY,
              value_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS mailboxes (
              id TEXT PRIMARY KEY,
              participant_key TEXT NOT NULL UNIQUE,
              created_at TEXT NOT NULL,
              next_sequence INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS mailbox_participants (
              mailbox_id TEXT NOT NULL,
              session_id TEXT NOT NULL,
              display_name TEXT NOT NULL,
              processed_through INTEGER NOT NULL DEFAULT 0,
              joined_at TEXT NOT NULL,
              left_at TEXT,
              PRIMARY KEY (mailbox_id, session_id),
              FOREIGN KEY (mailbox_id) REFERENCES mailboxes(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS mailbox_messages (
              id TEXT PRIMARY KEY,
              mailbox_id TEXT NOT NULL,
              sequence INTEGER NOT NULL,
              sender_session_id TEXT NOT NULL,
              sender_name TEXT NOT NULL,
              target_session_id TEXT NOT NULL,
              target_name TEXT NOT NULL,
              created_at TEXT NOT NULL,
              class TEXT NOT NULL CHECK (class = 'routine'),
              topic TEXT,
              state_stamp TEXT,
              body TEXT NOT NULL,
              idempotency_key TEXT NOT NULL,
              unread_depth_at_append INTEGER NOT NULL,
              notification_disposition TEXT NOT NULL,
              dispatch_outcome TEXT,
              UNIQUE (mailbox_id, sequence),
              UNIQUE (sender_session_id, idempotency_key),
              FOREIGN KEY (mailbox_id) REFERENCES mailboxes(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS mailbox_participants_session
              ON mailbox_participants(session_id, left_at);
            ",
        )
        .context("failed to initialize SQLite state schema")?;
    if !mailbox_messages_table_has_column(connection, "dispatch_outcome")? {
        connection
            .execute(
                "ALTER TABLE mailbox_messages ADD COLUMN dispatch_outcome TEXT",
                [],
            )
            .context("failed to add immutable mailbox dispatch outcome")?;
    }
    ensure_mailbox_dispatch_outcome_backfill(connection)?;
    connection
        .execute(
            "INSERT INTO meta(key, value) VALUES('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![SQLITE_SCHEMA_VERSION],
        )
        .context("failed to record SQLite state schema version")?;
    Ok(())
}

fn mailbox_dispatch_outcome_backfill_complete(
    connection: &rusqlite::Connection,
) -> Result<bool> {
    connection
        .query_row(
            "SELECT EXISTS(
               SELECT 1 FROM meta
               WHERE key = 'mailbox_dispatch_outcome_backfill_v1'
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .context("failed to inspect mailbox dispatch-outcome migration state")
}

fn ensure_mailbox_dispatch_outcome_backfill(
    connection: &rusqlite::Connection,
) -> Result<()> {
    // The completed path is read-only. Avoid taking SQLite's writer slot on
    // every connection schema check after this one-time migration has landed.
    if mailbox_dispatch_outcome_backfill_complete(connection)? {
        return Ok(());
    }

    // Re-check after acquiring IMMEDIATE: another connection may have completed
    // the migration between the read-only probe above and this transaction.
    // Check, backfill, and mark completion atomically. Production callers also
    // hold the path-scoped SQLite writer lock in
    // `ensure_sqlite_state_schema_for_path`, so separate connections cannot
    // race this one-time migration. The transaction prevents a crash between
    // the UPDATE and marker write from re-arming it on the next boot.
    let migration = rusqlite::Transaction::new_unchecked(
        connection,
        rusqlite::TransactionBehavior::Immediate,
    )
        .context("failed to begin mailbox dispatch-outcome migration")?;
    let dispatch_outcome_backfilled =
        mailbox_dispatch_outcome_backfill_complete(&migration)?;
    if !dispatch_outcome_backfilled {
        // The legacy lifecycle column cannot distinguish a direct delivery from a
        // delivery reached after recovery. Preserve deliveredToIdleSession as the
        // pragmatic immutable approximation, but normalize recovered/unknown
        // values to the accurate never-woken fallback. Pre-migration rows must not
        // be mined for recovery statistics.
        //
        // This is deliberately a one-time migration. Fresh appends use NULL as an
        // in-flight finalization marker, so repeating the backfill during ordinary
        // schema checks would fabricate a provisional immutable receipt.
        migration
            .execute(
                "UPDATE mailbox_messages
                 SET dispatch_outcome = CASE
                   WHEN notification_disposition = 'queuedBehindActiveTurn'
                     THEN 'queuedBehindActiveTurn'
                   WHEN notification_disposition = 'deliveredToIdleSession'
                     THEN 'deliveredToIdleSession'
                   ELSE 'durableButNotWoken'
                 END
                 WHERE dispatch_outcome IS NULL
                    OR dispatch_outcome NOT IN (
                      'durableButNotWoken',
                      'queuedBehindActiveTurn',
                      'deliveredToIdleSession'
                    )",
                [],
            )
            .context("failed to backfill immutable mailbox dispatch outcomes")?;
        migration
            .execute(
                "INSERT INTO meta(key, value)
                 VALUES('mailbox_dispatch_outcome_backfill_v1', 'complete')
                 ON CONFLICT(key) DO NOTHING",
                [],
            )
            .context("failed to record mailbox dispatch-outcome migration")?;
    }
    migration
        .commit()
        .context("failed to commit mailbox dispatch-outcome migration")?;
    Ok(())
}

fn mailbox_messages_table_has_column(
    connection: &rusqlite::Connection,
    column_name: &str,
) -> Result<bool> {
    let mut statement = connection.prepare("PRAGMA table_info(mailbox_messages)")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
    for row in rows {
        if row? == column_name {
            return Ok(true);
        }
    }
    Ok(false)
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
    fn sqlite_schema_adds_and_backfills_immutable_mailbox_dispatch_outcome() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        connection
            .execute_batch(
                "
                CREATE TABLE meta (
                  key TEXT PRIMARY KEY,
                  value TEXT NOT NULL
                );
                CREATE TABLE mailbox_messages (
                  id TEXT PRIMARY KEY,
                  notification_disposition TEXT NOT NULL
                );
                INSERT INTO mailbox_messages(id, notification_disposition)
                VALUES
                  ('mailbox-message-durable', 'durableButNotWoken'),
                  ('mailbox-message-queued', 'queuedBehindActiveTurn'),
                  ('mailbox-message-recovered', 'recoveredWake'),
                  ('mailbox-message-delivered', 'deliveredToIdleSession'),
                  ('mailbox-message-unknown', 'legacyUnknownState');
                ",
            )
            .expect("seed pre-dispatch-outcome mailbox schema");
        connection
            .execute(
                "INSERT INTO meta(key, value) VALUES('schema_version', ?1)",
                rusqlite::params![SQLITE_SCHEMA_VERSION],
            )
            .expect("seed supported schema version");

        ensure_sqlite_state_schema(&connection).expect("schema migration should succeed");

        assert!(
            mailbox_messages_table_has_column(&connection, "dispatch_outcome")
                .expect("mailbox column should be inspectable")
        );
        let mut statement = connection
            .prepare(
                "SELECT id, dispatch_outcome
                 FROM mailbox_messages
                 ORDER BY id",
            )
            .expect("backfilled outcomes should prepare");
        let outcomes = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .expect("backfilled outcomes should query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("backfilled outcomes should decode");
        assert_eq!(
            outcomes,
            vec![
                (
                    "mailbox-message-delivered".to_owned(),
                    "deliveredToIdleSession".to_owned(),
                ),
                (
                    "mailbox-message-durable".to_owned(),
                    "durableButNotWoken".to_owned(),
                ),
                (
                    "mailbox-message-queued".to_owned(),
                    "queuedBehindActiveTurn".to_owned(),
                ),
                (
                    "mailbox-message-recovered".to_owned(),
                    "durableButNotWoken".to_owned(),
                ),
                (
                    "mailbox-message-unknown".to_owned(),
                    "durableButNotWoken".to_owned(),
                ),
            ]
        );
        drop(statement);

        connection
            .execute_batch(
                "
                UPDATE mailbox_messages
                SET dispatch_outcome = NULL,
                    notification_disposition = 'queuedBehindActiveTurn'
                WHERE id = 'mailbox-message-durable';
                ",
            )
            .expect("seed a fresh provisional outcome after migration");
        ensure_sqlite_state_schema(&connection).expect("ordinary schema check should repeat");
        let provisional_outcome: Option<String> = connection
            .query_row(
                "SELECT dispatch_outcome
                 FROM mailbox_messages
                 WHERE id = 'mailbox-message-durable'",
                [],
                |row| row.get(0),
            )
            .expect("provisional outcome should read");
        assert_eq!(
            provisional_outcome, None,
            "one-time migration must not rewrite a fresh in-flight receipt marker"
        );
    }

    #[test]
    fn completed_dispatch_outcome_backfill_fast_path_is_read_only() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        ensure_sqlite_state_schema(&connection).expect("initial schema migration should succeed");
        connection
            .execute_batch("PRAGMA query_only = ON;")
            .expect("test connection should enter query-only mode");

        ensure_mailbox_dispatch_outcome_backfill(&connection)
            .expect("completed migration should use the read-only fast path");
    }

    #[test]
    fn sqlite_schema_dispatch_outcome_migration_is_safe_across_connections() {
        let root = std::env::temp_dir().join(format!(
            "termal-schema-concurrency-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("test directory should exist");
        let path = root.join("termal.sqlite");
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let threads = (0..2)
            .map(|_| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let connection = open_sqlite_state_connection(&path)
                        .expect("concurrent SQLite connection should open");
                    barrier.wait();
                    ensure_sqlite_state_schema_for_path(&connection, &path)
                        .expect("concurrent schema ensure should succeed");
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().expect("schema ensure thread should join");
        }

        let connection =
            rusqlite::Connection::open(&path).expect("schema database should reopen");
        let marker_count: u32 = connection
            .query_row(
                "SELECT COUNT(*) FROM meta
                 WHERE key = 'mailbox_dispatch_outcome_backfill_v1'",
                [],
                |row| row.get(0),
            )
            .expect("migration marker should read");
        assert_eq!(marker_count, 1);
        drop(connection);
        fs::remove_dir_all(root).expect("test directory should clean up");
    }
}

fn load_state_from_sqlite(path: &FsPath) -> Result<Option<StateInner>> {
    let connection = open_sqlite_state_connection(path)?;
    ensure_sqlite_state_schema_for_path(&connection, path)?;
    // `open_sqlite_state_connection` already hardens the fresh handle, but
    // schema initialization can create or recreate SQLite sidecars, so the
    // startup read path deliberately re-runs the full main/sidecar pass.
    harden_sqlite_state_file_permissions(path)?;
    let session_records = load_session_records_from_sqlite(&connection, path)?;
    let delegation_records = load_delegation_records_from_sqlite(&connection, path)?;
    let Some(encoded) = sqlite_app_state_value(&connection, SQLITE_METADATA_KEY, path)? else {
        return Ok(None);
    };
    let mut persisted: PersistedState = serde_json::from_str(&encoded)
        .with_context(|| format!("failed to parse persisted state from `{}`", path.display()))?;
    if !session_records.is_empty() {
        persisted.sessions = session_records;
    }
    let loaded_delegations_from_table =
        apply_sqlite_delegation_records(&mut persisted, delegation_records);
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
) -> bool {
    if delegation_records.is_empty() {
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

fn load_session_records_from_sqlite(
    connection: &rusqlite::Connection,
    path: &FsPath,
) -> Result<Vec<PersistedSessionRecord>> {
    let mut statement = connection
        .prepare("SELECT value_json FROM sessions ORDER BY rowid")
        .with_context(|| format!("failed to prepare session load from `{}`", path.display()))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .with_context(|| {
            format!(
                "failed to query persisted sessions from `{}`",
                path.display()
            )
        })?;
    let mut records = Vec::new();
    for row in rows {
        let encoded =
            row.with_context(|| format!("failed to read session row from `{}`", path.display()))?;
        let record = serde_json::from_str(&encoded).with_context(|| {
            format!(
                "failed to parse persisted session row from `{}`",
                path.display()
            )
        })?;
        records.push(record);
    }
    Ok(records)
}

fn load_delegation_records_from_sqlite(
    connection: &rusqlite::Connection,
    path: &FsPath,
) -> Result<Vec<DelegationRecord>> {
    let mut statement = connection
        .prepare("SELECT value_json FROM delegations ORDER BY rowid")
        .with_context(|| format!("failed to prepare delegation load from `{}`", path.display()))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .with_context(|| {
            format!(
                "failed to query persisted delegations from `{}`",
                path.display()
            )
        })?;
    let mut records = Vec::new();
    for row in rows {
        let encoded = row
            .with_context(|| format!("failed to read delegation row from `{}`", path.display()))?;
        let record = serde_json::from_str(&encoded).with_context(|| {
            format!(
                "failed to parse persisted delegation row from `{}`",
                path.display()
            )
        })?;
        records.push(record);
    }
    Ok(records)
}

fn persist_persisted_state_to_sqlite(path: &FsPath, persisted: &PersistedState) -> Result<()> {
    let metadata = persisted.metadata_only();
    persist_state_parts_to_sqlite(
        path,
        &metadata,
        &persisted.sessions,
        true,
        &persisted.delegations,
        true,
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
    delegations: &[DelegationRecord],
    replace_delegations: bool,
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
        delegations,
        replace_delegations,
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
    delegations: &[DelegationRecord],
    replace_delegations: bool,
) -> Result<()> {
    let metadata_json =
        serde_json::to_string(metadata).context("failed to serialize persisted state metadata")?;
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
        tx.execute("DELETE FROM sessions", [])
            .with_context(|| format!("failed to replace sessions in `{}`", path.display()))?;
    }
    for session in sessions {
        let session_json =
            serde_json::to_string(session).context("failed to serialize persisted session")?;
        tx.execute(
            "INSERT INTO sessions(id, value_json) VALUES(?1, ?2)
             ON CONFLICT(id) DO UPDATE SET value_json = excluded.value_json",
            rusqlite::params![&session.session.id, session_json],
        )
        .with_context(|| format!("failed to write persisted session to `{}`", path.display()))?;
    }
    if replace_delegations {
        tx.execute("DELETE FROM delegations", [])
            .with_context(|| format!("failed to replace delegations in `{}`", path.display()))?;
    }
    for delegation in delegations {
        let delegation_json = serde_json::to_string(delegation)
            .context("failed to serialize persisted delegation")?;
        tx.execute(
            "INSERT INTO delegations(id, value_json) VALUES(?1, ?2)
             ON CONFLICT(id) DO UPDATE SET value_json = excluded.value_json",
            rusqlite::params![&delegation.id, delegation_json],
        )
        .with_context(|| {
            format!(
                "failed to write persisted delegation `{}` to `{}`",
                delegation.id,
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
) -> Result<()> {
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
) -> Result<()> {
    let metadata_json = serde_json::to_string(&delta.metadata)
        .context("failed to serialize persisted state metadata")?;
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
    for session in &delta.changed_sessions {
        let session_json =
            serde_json::to_string(session).context("failed to serialize persisted session")?;
        tx.execute(
            "INSERT INTO sessions(id, value_json) VALUES(?1, ?2)
             ON CONFLICT(id) DO UPDATE SET value_json = excluded.value_json",
            rusqlite::params![&session.session.id, session_json],
        )
        .with_context(|| {
            format!(
                "failed to write persisted session `{}` to `{}`",
                session.session.id,
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
    if let Some(delegations) = &delta.changed_delegations {
        for delegation in delegations {
            let delegation_json = serde_json::to_string(delegation)
                .context("failed to serialize persisted delegation")?;
            tx.execute(
                "INSERT INTO delegations(id, value_json) VALUES(?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET value_json = excluded.value_json",
                rusqlite::params![&delegation.id, delegation_json],
            )
            .with_context(|| {
                format!(
                    "failed to write persisted delegation `{}` to `{}`",
                    delegation.id,
                    path.display()
                )
            })?;
        }
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
    Ok(())
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
