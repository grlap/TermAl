// Owns the server instance lock of a TermAl data directory: the exclusive OS
// file lock a server-mode process takes on `<data dir>/termal-server.lock`
// before it loads, recovers or persists anything there, and the refusal a
// second server gets while another process holds that lock.
// Does not own the boot itself (`app_boot.rs`), the port bind (`main.rs`), or
// any stale-lock cleanup: the OS releases the lock when its process exits or
// crashes, so nothing is left to clean up. The owner file next to the lock
// only describes the holder for the refusal message and decides nothing.
// New module; the server entry point in `main.rs` previously called
// `AppState::new` (`app_boot.rs`) directly, which loaded, recovered and queued
// a persist of the store before a second process failed to bind its port.

/// File in the data directory whose exclusive OS lock marks a running server.
const SERVER_INSTANCE_LOCK_FILE_NAME: &str = "termal-server.lock";

/// Sibling file describing the current lock holder, read only to word the
/// refusal. The next holder overwrites a leftover one.
const SERVER_INSTANCE_OWNER_FILE_NAME: &str = "termal-server.owner";

/// The owner file's record of the lock holder.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerInstanceOwner {
    pid: u32,
    started_at: String,
    workdir: String,
}

/// Held for the life of a server process (`RUNNING_SERVER_INSTANCE_LOCK`),
/// which releases it by ending, in any way. Dropping it, as a test's holder
/// does, also releases it.
struct ServerInstanceLock {
    file: fs::File,
}

impl Drop for ServerInstanceLock {
    fn drop(&mut self) {
        // Windows frees a lock left on a closing handle only after an
        // unspecified delay, so a dropped lock unlocks first and can be taken
        // again at once. The running server never drops its lock; it relies
        // on the OS releasing it with the process.
        if let Err(error) = self.file.unlock() {
            eprintln!("instance lock> could not release the server lock: {error}");
        }
    }
}

impl ServerInstanceLock {
    /// Takes the data directory's exclusive instance lock, or refuses with a
    /// message naming the data directory, the lock file and the holder.
    fn acquire(data_dir: &FsPath) -> Result<Self> {
        fs::create_dir_all(data_dir).with_context(|| {
            format!(
                "failed to create the TermAl data directory `{}`",
                data_dir.display()
            )
        })?;
        let lock_path = data_dir.join(SERVER_INSTANCE_LOCK_FILE_NAME);
        let owner_path = data_dir.join(SERVER_INSTANCE_OWNER_FILE_NAME);
        // Files opened by std are not inherited by child processes, so agent
        // runtimes spawned by this server never keep the lock alive after it.
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| {
                format!(
                    "failed to open the TermAl server lock `{}`",
                    lock_path.display()
                )
            })?;
        match file.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                bail!(server_instance_refusal(data_dir, &lock_path, &owner_path));
            }
            Err(fs::TryLockError::Error(error)) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to lock `{}`; refusing to start before loading the TermAl data directory `{}`",
                        lock_path.display(),
                        data_dir.display()
                    )
                });
            }
        }
        let instance_lock = Self { file };
        // The lock alone protects the store; the owner file only words a later
        // refusal. Remove a previous holder's record first, so that unless the
        // removal fails too, a failed write leaves no record rather than one
        // naming a process that has exited. The record is written beside it
        // and renamed into place, so a refused start never reads half of it.
        if let Err(error) = fs::remove_file(&owner_path)
            && error.kind() != io::ErrorKind::NotFound
        {
            eprintln!(
                "instance lock> could not remove the previous holder's record `{}`: {error}",
                owner_path.display()
            );
        }
        let owner = ServerInstanceOwner {
            pid: std::process::id(),
            started_at: chrono::Utc::now().to_rfc3339(),
            workdir: std::env::current_dir()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
        };
        let pending_owner_path =
            data_dir.join(format!("{SERVER_INSTANCE_OWNER_FILE_NAME}.pending"));
        let written = serde_json::to_vec(&owner)
            .map_err(io::Error::other)
            .and_then(|bytes| fs::write(&pending_owner_path, bytes))
            .and_then(|()| fs::rename(&pending_owner_path, &owner_path));
        if let Err(error) = written {
            eprintln!(
                "instance lock> could not record this server in `{}`: {error}",
                owner_path.display()
            );
        }
        Ok(instance_lock)
    }
}

/// Words the refusal of a second server, naming the data directory, the lock
/// file and, when its owner file is readable, the holding process.
fn server_instance_refusal(data_dir: &FsPath, lock_path: &FsPath, owner_path: &FsPath) -> String {
    let holder = match fs::read(owner_path)
        .map_err(|error| error.to_string())
        .and_then(|bytes| {
            serde_json::from_slice::<ServerInstanceOwner>(&bytes).map_err(|error| error.to_string())
        }) {
        Ok(owner) => format!(
            "held by pid {}, started {}, workdir `{}`",
            owner.pid, owner.started_at, owner.workdir
        ),
        Err(error) => format!(
            "its holder is not described in `{}`: {error}",
            owner_path.display()
        ),
    };
    format!(
        "another TermAl server is already using the data directory `{}`: it holds the lock `{}` ({holder}). This process stopped before loading, recovering or persisting any state there. Stop that server first. A second server needs its own data directory: it is HOME/.termal (USERPROFILE/.termal when HOME is unset), and a different HOME also changes it for the agents that server starts.",
        data_dir.display(),
        lock_path.display(),
    )
}

/// Keeps the running server's lock until the process exits. A static is never
/// dropped, so the lock outlives `run_server` and every background writer that
/// still holds an `AppState` clone: a detached boot worker, or a blocking task
/// the runtime waits for at teardown. The OS releases it only once the process,
/// with all its threads, is gone.
static RUNNING_SERVER_INSTANCE_LOCK: OnceLock<ServerInstanceLock> = OnceLock::new();

impl ServerInstanceLock {
    /// Moves the lock into `holder`, which keeps it for as long as the holder
    /// lives, independently of any `AppState`.
    fn keep_in(self, holder: &OnceLock<Self>) -> Result<()> {
        holder
            .set(self)
            .map_err(|_| anyhow!("this process already holds a TermAl server lock"))
    }
}

impl AppState {
    /// Server-mode boot from the default paths of `default_workdir`. The data
    /// directory stays locked until the process exits.
    fn new_server(default_workdir: String) -> Result<Self> {
        let default_workdir = normalize_local_user_facing_path(&default_workdir);
        let persistence_path = resolve_persistence_path(&default_workdir);
        let orchestrator_templates_path = resolve_orchestrator_templates_path(&default_workdir);
        Self::new_server_with_paths(
            default_workdir,
            persistence_path,
            orchestrator_templates_path,
            &RUNNING_SERVER_INSTANCE_LOCK,
        )
    }

    /// Server-mode boot from explicit paths: takes the instance lock of the
    /// state database's directory and moves it into `lock_holder` before it
    /// boots, so the lock lives as long as the holder, not this state.
    fn new_server_with_paths(
        default_workdir: String,
        persistence_path: PathBuf,
        orchestrator_templates_path: PathBuf,
        lock_holder: &OnceLock<ServerInstanceLock>,
    ) -> Result<Self> {
        let data_dir = std::path::absolute(&persistence_path)
            .with_context(|| {
                format!(
                    "failed to resolve the TermAl state database path `{}`",
                    persistence_path.display()
                )
            })?
            .parent()
            .map(FsPath::to_path_buf)
            .context("the TermAl state database path has no parent directory")?;
        ServerInstanceLock::acquire(&data_dir)?.keep_in(lock_holder)?;
        Self::new_with_paths(
            default_workdir,
            persistence_path,
            orchestrator_templates_path,
        )
    }
}
