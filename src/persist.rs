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

#[cfg(not(test))]
const SQLITE_SCHEMA_VERSION: &str = "1";
#[cfg(not(test))]
const SQLITE_LEGACY_STATE_KEY: &str = "persistedState";
#[cfg(not(test))]
const SQLITE_METADATA_KEY: &str = "metadataState";
#[cfg(not(test))]
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
// Test-only legacy JSON loaders support fixture round trips and migration
// regressions. Production `load_state` reads SQLite directly; this cap and
// escape hatch are fixture-safety tools, not a runtime import guardrail.
#[cfg(test)]
const MAX_LEGACY_JSON_STATE_BYTES: u64 = 100 * 1024 * 1024;
#[cfg(test)]
const LEGACY_JSON_STATE_MAX_BYTES_ENV: &str = "TERMAL_LEGACY_STATE_MAX_BYTES";
#[cfg(test)]
const BYTES_PER_MIB: u64 = 1024 * 1024;
#[cfg(test)]
static LEGACY_JSON_STATE_MAX_BYTES_OVERRIDE_WARNED: AtomicBool = AtomicBool::new(false);

#[cfg(not(test))]
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
fn harden_local_state_permissions(path: &FsPath, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if let Err(err) = fs::set_permissions(path, fs::Permissions::from_mode(mode)) {
        permission_hardening_failure(path, err)?;
    }

    let actual_mode = match fs::metadata(path) {
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
    reject_existing_state_directory_redirection_unix(path)?;
    harden_local_state_permissions(path, 0o700)
}

#[cfg(unix)]
fn reject_existing_state_directory_redirection(path: &FsPath) -> Result<()> {
    reject_existing_state_directory_redirection_unix(path)
}

#[cfg(all(not(test), windows))]
fn harden_local_state_directory_permissions(path: &FsPath) -> Result<()> {
    reject_existing_windows_state_path_redirection(path)
}

#[cfg(all(not(test), windows))]
fn reject_existing_state_directory_redirection(path: &FsPath) -> Result<()> {
    reject_existing_windows_state_path_redirection(path)
}

#[cfg(all(not(test), not(unix), not(windows)))]
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

#[cfg(not(test))]
fn harden_persist_commit_files(path: &FsPath) -> Result<()> {
    harden_sqlite_state_file_permissions(path).with_context(|| {
        format!(
            "committed persisted state to `{}` but failed to re-harden state files",
            path.display()
        )
    })
}

#[cfg(not(test))]
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

#[cfg(unix)]
fn reject_existing_sqlite_state_file_symlinks(path: &FsPath) -> Result<()> {
    reject_existing_state_file_symlink(path)?;
    reject_existing_state_file_symlink(&sqlite_sidecar_path(path, "-wal"))?;
    reject_existing_state_file_symlink(&sqlite_sidecar_path(path, "-shm"))?;
    reject_existing_state_file_symlink(&sqlite_sidecar_path(path, "-journal"))?;
    Ok(())
}

#[cfg(all(not(test), windows))]
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

/// Rejects Windows reparse points before SQLite can open the TermAl state
/// directory, database, or sidecars. A reparse point can redirect persisted
/// session history through a symlink, junction, or mount point; this is path
/// integrity, not Unix-style chmod hardening, so the insecure-permissions
/// escape hatch intentionally does not apply. `0x400` is the stable
/// `FILE_ATTRIBUTE_REPARSE_POINT` value; spelling it locally avoids adding a
/// Windows API crate only for this metadata bit.
#[cfg(all(not(test), windows))]
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

#[cfg(not(test))]
fn reject_existing_sqlite_state_path_redirection(path: &FsPath) -> Result<()> {
    if let Some(parent) = path.parent() {
        reject_existing_state_directory_redirection(parent)?;
    }
    reject_existing_sqlite_state_file_symlinks(path)
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

#[cfg(any(unix, all(not(test), windows)))]
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

#[cfg(test)]
fn persisted_state_size_limit_label(max_bytes: u64) -> String {
    if max_bytes % BYTES_PER_MIB == 0 {
        format!("{} MiB", max_bytes / BYTES_PER_MIB)
    } else {
        format!("{max_bytes} bytes")
    }
}

#[cfg(test)]
fn parse_legacy_json_state_max_bytes_override(raw: &str) -> Result<u64> {
    let trimmed = raw.trim();
    let max_bytes = trimmed.parse::<u64>().with_context(|| {
        format!("{LEGACY_JSON_STATE_MAX_BYTES_ENV} must be a positive integer byte count")
    })?;
    if max_bytes == 0 {
        bail!("{LEGACY_JSON_STATE_MAX_BYTES_ENV} must be greater than 0");
    }
    Ok(max_bytes)
}

#[cfg(test)]
fn legacy_json_state_max_bytes() -> Result<u64> {
    match std::env::var(LEGACY_JSON_STATE_MAX_BYTES_ENV) {
        Ok(value) => {
            let max_bytes = parse_legacy_json_state_max_bytes_override(&value)?;
            if !LEGACY_JSON_STATE_MAX_BYTES_OVERRIDE_WARNED.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "[termal] legacy state max bytes overridden via {LEGACY_JSON_STATE_MAX_BYTES_ENV} = {max_bytes}; default is {}",
                    persisted_state_size_limit_label(MAX_LEGACY_JSON_STATE_BYTES)
                );
            }
            Ok(max_bytes)
        }
        Err(std::env::VarError::NotPresent) => Ok(MAX_LEGACY_JSON_STATE_BYTES),
        Err(std::env::VarError::NotUnicode(_)) => bail!(
            "{LEGACY_JSON_STATE_MAX_BYTES_ENV} must be valid Unicode containing a positive integer byte count"
        ),
    }
}

#[cfg(test)]
fn read_json_persisted_state_with_limit(
    path: &FsPath,
    max_bytes: u64,
) -> Result<PersistedState> {
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to inspect `{}`", path.display()))?;
    let actual_bytes = metadata.len();
    if actual_bytes > max_bytes {
        let state_path = path.display();
        let max_bytes_label = persisted_state_size_limit_label(max_bytes);
        bail!(
            "persisted state `{state_path}` is too large ({actual_bytes} bytes, max {max_bytes_label}). To import this trusted legacy state once, set `{LEGACY_JSON_STATE_MAX_BYTES_ENV}` to a byte value of at least {actual_bytes} and restart TermAl; after a successful import the JSON file is migrated to SQLite and renamed."
        );
    }

    let raw = fs::read(path).with_context(|| format!("failed to read `{}`", path.display()))?;
    let encoded: Value = serde_json::from_slice(&raw)
        .with_context(|| format!("failed to parse `{}`", path.display()))?;
    serde_json::from_value(encoded)
        .with_context(|| format!("failed to deserialize state from `{}`", path.display()))
}

#[cfg(test)]
fn read_json_persisted_state(path: &FsPath) -> Result<PersistedState> {
    let max_bytes = legacy_json_state_max_bytes()?;
    read_json_persisted_state_with_limit(path, max_bytes)
}

/// Loads state.
#[cfg(test)]
fn load_state(path: &FsPath) -> Result<Option<StateInner>> {
    if !path.exists() {
        return Ok(None);
    }

    let persisted = read_json_persisted_state(path)?;
    Ok(Some(persisted.into_inner().with_context(|| {
        format!("failed to validate state from `{}`", path.display())
    })?))
}

/// Loads state from SQLite in production.
#[cfg(not(test))]
fn load_state(path: &FsPath) -> Result<Option<StateInner>> {
    if !path.exists() {
        return Ok(None);
    }
    load_state_from_sqlite(path)
}

#[cfg(not(test))]
fn ensure_sqlite_state_schema(connection: &rusqlite::Connection) -> Result<()> {
    connection
        .execute_batch(
            "
            CREATE TABLE IF NOT EXISTS meta (
              key TEXT PRIMARY KEY,
              value TEXT NOT NULL
            );

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
            ",
        )
        .context("failed to initialize SQLite state schema")?;
    connection
        .execute(
            "INSERT INTO meta(key, value) VALUES('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![SQLITE_SCHEMA_VERSION],
        )
        .context("failed to record SQLite state schema version")?;
    Ok(())
}

#[cfg(not(test))]
fn load_state_from_sqlite(path: &FsPath) -> Result<Option<StateInner>> {
    let connection = open_sqlite_state_connection(path)?;
    ensure_sqlite_state_schema(&connection)?;
    // `open_sqlite_state_connection` already hardens the fresh handle, but
    // schema initialization can create or recreate SQLite sidecars, so the
    // startup read path deliberately re-runs the full main/sidecar pass.
    harden_sqlite_state_file_permissions(path)?;
    let session_records = load_session_records_from_sqlite(&connection, path)?;
    let delegation_records = load_delegation_records_from_sqlite(&connection, path)?;
    // The legacy lookup is only consulted when the primary key is
    // missing. `.or(...)` would eagerly run both queries (both sides
    // of `.or` must be evaluated before the combinator sees them),
    // which pays for a second `SELECT ... FROM app_state WHERE key = ?`
    // round-trip against the connection on every startup — silent
    // but wasteful on the happy path where `SQLITE_METADATA_KEY` is
    // always present post-migration. Structure as `if let` chains so
    // the legacy query only runs when the primary returns `None`.
    let encoded =
        if let Some(encoded) = sqlite_app_state_value(&connection, SQLITE_METADATA_KEY, path)? {
            encoded
        } else if let Some(encoded) =
            sqlite_app_state_value(&connection, SQLITE_LEGACY_STATE_KEY, path)?
        {
            encoded
        } else {
            return Ok(None);
        };
    let mut persisted: PersistedState = serde_json::from_str(&encoded)
        .with_context(|| format!("failed to parse persisted state from `{}`", path.display()))?;
    if !session_records.is_empty() {
        persisted.sessions = session_records;
    }
    persisted.delegations = delegation_records;
    Ok(Some(persisted.into_inner().with_context(|| {
        format!("failed to validate state from `{}`", path.display())
    })?))
}

#[cfg(not(test))]
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

#[cfg(not(test))]
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

#[cfg(not(test))]
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

#[cfg(not(test))]
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

#[cfg(not(test))]
fn persist_created_session(
    path: &FsPath,
    inner: &StateInner,
    _record: &SessionRecord,
) -> Result<()> {
    let persisted = PersistedState::from_inner(inner);
    persist_persisted_state_to_sqlite(path, &persisted)
}

#[cfg(test)]
fn persist_created_session(
    path: &FsPath,
    inner: &StateInner,
    _record: &SessionRecord,
) -> Result<()> {
    let persisted = PersistedState::from_inner(inner);
    persist_state_from_persisted(path, &persisted)
}

#[cfg(not(test))]
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
    ensure_sqlite_state_schema(&connection)?;
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
#[cfg(not(test))]
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
#[cfg(not(test))]
struct SqlitePersistConnectionCache {
    path: Option<PathBuf>,
    connection: Option<rusqlite::Connection>,
}

#[cfg(not(test))]
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
            ensure_sqlite_state_schema(&connection)?;
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
/// row `INSERT OR UPDATE`s and `DELETE`s, and delegation-table rewrites
/// when delegation state changed — via the shared connection cache.
///
/// This is the sole production write path. It writes only the rows in
/// `delta.changed_sessions` and removes only `delta.removed_session_ids`;
/// unchanged session rows are left untouched so a mutation on one
/// session no longer rewrites every other session row every commit.
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
#[cfg(not(test))]
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

#[cfg(not(test))]
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
    if let Some(delegations) = &delta.changed_delegations {
        tx.execute("DELETE FROM delegations", [])
            .with_context(|| format!("failed to replace delegations in `{}`", path.display()))?;
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
    // Keep post-commit redirection and owner-only permission verification
    // fatal. The chmod helper itself honors
    // TERMAL_ALLOW_INSECURE_STATE_PERMISSIONS when the operator explicitly
    // accepts insecure state-file modes.
    verify_persist_commit_integrity(path)?;
    Ok(())
}

/// Persists state from a pre-built `PersistedState` snapshot.
#[cfg(not(test))]
fn persist_state_from_persisted(path: &FsPath, persisted: &PersistedState) -> Result<()> {
    persist_persisted_state_to_sqlite(path, persisted)
}

#[cfg(test)]
fn persist_state_from_persisted(path: &FsPath, persisted: &PersistedState) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }

    let encoded =
        serde_json::to_vec_pretty(persisted).context("failed to serialize persisted state")?;
    fs::write(path, encoded).with_context(|| format!("failed to write `{}`", path.display()))
}

/// Persists state directly from `StateInner` (used in tests for synchronous
/// setup of persisted state files).
#[cfg(test)]
fn persist_state(path: &FsPath, inner: &StateInner) -> Result<()> {
    let persisted = PersistedState::from_inner(inner);
    persist_state_from_persisted(path, &persisted)
}
