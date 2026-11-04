// The spawn profile of a shared Codex app-server.
//
// Owns: which profile a Codex session runs under (from its sandbox mode),
// the Codex home scope each profile uses, and the PATH a read-only-sandbox
// app-server is started with. On Windows, Codex's read-only sandbox cannot
// start the Store-packaged PowerShell 7 (process creation under the
// sandbox's restricted token is refused), and Codex picks its shell once per
// app-server process from PATH. So a read-only Codex session there runs on
// an app-server of its own whose PATH holds no `pwsh`, which makes Codex use
// Windows PowerShell 5.1; the read-only sandbox then reads and refuses
// writes. New delegated workspace-write threads use the same PATH filtering
// in their own app-server; their per-thread sandbox remains workspace-write.
// Root workspace-write sessions keep the default process, and existing threads
// keep their home.
//
// Does not own: the runtime slots and their lifecycle (shared_codex_mgr.rs),
// spawning the process (codex.rs), or the sandbox mode a delegation child
// is given (delegations.rs). New file.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum SharedCodexProfile {
    /// The app-server every session uses unless it needs another.
    Default,
    /// Windows only: an app-server for read-only-sandbox sessions, started
    /// with no PowerShell 7 on PATH.
    ReadOnlySandbox,
    /// Windows only: a separate process/home for delegated workspace-write
    /// threads. It uses the filtered PATH without sharing reviewer lifecycle.
    WorkspaceWriteSandbox,
}

impl SharedCodexProfile {
    const ALL: [Self; 3] = [
        Self::Default, Self::ReadOnlySandbox, Self::WorkspaceWriteSandbox,
    ];

    fn needs_filtered_shell(self) -> bool {
        matches!(self, Self::ReadOnlySandbox | Self::WorkspaceWriteSandbox)
    }

    /// The profile a session with this sandbox mode runs under.
    fn for_sandbox_mode(sandbox_mode: CodexSandboxMode) -> Self {
        if cfg!(windows) && sandbox_mode == CodexSandboxMode::ReadOnly {
            Self::ReadOnlySandbox
        } else {
            Self::Default
        }
    }

    /// The TermAl Codex home scope. Each profile keeps its threads apart, and
    /// startup discovery does not import threads from the dedicated homes
    /// (`codex_home_scope_is_importable`).
    fn codex_home_scope(self) -> &'static str {
        match self {
            Self::Default => "shared-app-server",
            Self::ReadOnlySandbox => "shared-app-server-read-only",
            Self::WorkspaceWriteSandbox => "shared-app-server-workspace-write",
        }
    }
}

impl SessionRecord {
    fn new_thread_codex_profile(&self, sandbox_mode: CodexSandboxMode) -> SharedCodexProfile {
        if cfg!(windows)
            && sandbox_mode == CodexSandboxMode::WorkspaceWrite
            && self.session.parent_delegation_id.is_some()
        {
            SharedCodexProfile::WorkspaceWriteSandbox
        } else {
            SharedCodexProfile::for_sandbox_mode(sandbox_mode)
        }
    }

    /// The app-server profile this Codex session's turns and thread
    /// operations use. A session with a thread uses the app-server whose Codex
    /// home holds that thread, whatever its sandbox mode is now; a thread
    /// created before profiles existed lives in the default home. A session
    /// without a thread uses the profile its sandbox mode needs.
    fn shared_codex_profile(&self) -> SharedCodexProfile {
        if self.external_session_id.is_some() {
            return self
                .codex_thread_profile
                .unwrap_or(SharedCodexProfile::Default);
        }
        self.new_thread_codex_profile(self.codex_sandbox_mode)
    }

    /// Records that this session attaches to the app-server of `profile`. A
    /// session without a thread will create it there, so the profile is fixed
    /// for the thread; a session with a thread keeps the profile it has.
    fn attach_to_shared_codex_profile(&mut self, profile: SharedCodexProfile) {
        if self.external_session_id.is_none() {
            self.codex_thread_profile = Some(profile);
        }
    }

    /// Why this Codex session cannot change its sandbox mode to
    /// `sandbox_mode` now, or `None` when it can. Re-sending the current mode
    /// is always accepted. An existing thread cannot move between Codex
    /// homes, so on Windows a thread on the default app-server cannot take
    /// a sandbox requiring the filtered process PATH. A session
    /// without a thread may change profile only while no turn runs: a running
    /// turn may be creating its first thread on the current app-server.
    fn codex_sandbox_change_refusal(&self, sandbox_mode: CodexSandboxMode) -> Option<&'static str> {
        if sandbox_mode == self.codex_sandbox_mode {
            return None;
        }
        let target = self.new_thread_codex_profile(sandbox_mode);
        if self.external_session_id.is_some() {
            return (target.needs_filtered_shell()
                && self.shared_codex_profile() == SharedCodexProfile::Default)
                .then_some(
                    "this Codex thread cannot change to that sandbox on its current Windows \
                     app-server: the requested mode needs the filtered PowerShell environment; \
                     start a new Codex session or delegation with that sandbox instead",
                );
        }
        let turn_running = self.runtime_stop_in_progress
            || matches!(
                self.session.status,
                SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
            );
        (target != self.shared_codex_profile() && turn_running).then_some(
            "wait for the current Codex turn to finish before changing app-server profiles: \
             the turn may be creating this session's thread",
        )
    }
}

/// `path` with every directory that holds a PowerShell 7 executable left
/// out. `holds_pwsh` is asked about each directory.
fn path_without_pwsh(
    path: &std::ffi::OsStr,
    holds_pwsh: impl Fn(&FsPath) -> bool,
) -> Option<std::ffi::OsString> {
    let kept = std::env::split_paths(path)
        .filter(|dir| !holds_pwsh(dir))
        .collect::<Vec<_>>();
    std::env::join_paths(kept).ok()
}

/// Whether `dir` holds PowerShell 7 (`pwsh.exe`, or a `pwsh.cmd` or
/// `pwsh.bat` shim). An app execution alias (the Store's
/// `WindowsApps` entries) is a reparse point that ordinary metadata cannot
/// follow, so the entry is checked without following it.
fn directory_holds_pwsh(dir: &FsPath) -> bool {
    ["pwsh.exe", "pwsh.cmd", "pwsh.bat"]
        .into_iter()
        .any(|name| fs::symlink_metadata(dir.join(name)).is_ok())
}

/// The PATH a read-only-sandbox app-server is started with, or `None` when
/// the host's PATH is unset or cannot be rebuilt.
fn shared_codex_read_only_sandbox_path() -> Option<std::ffi::OsString> {
    let path = std::env::var_os("PATH")?;
    path_without_pwsh(&path, directory_holds_pwsh)
}
