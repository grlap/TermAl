// Pinned, bounded Git reads for source admission, declared runs and ignore queries.
// The transport, timing and diagnostics come from the former schema-1 checker.

use super::*;

/// Shared deadline for pinned source reads. Git calls can take seconds under load.
pub(crate) const SOURCE_READ_TIMEOUT: Duration = Duration::from_secs(40);
const PINNED_GIT_OUTPUT_LIMIT: usize = 128 * 1024 * 1024;

// Optional per-capture diagnostics. Names are host constants, never paths or
// arguments. This observes the caller's existing deadline; it cannot renew it.
#[derive(Debug, Default)]
pub(crate) struct SourceReadTimings(pub(crate) Mutex<SourceReadTimingState>);

#[derive(Debug, Default)]
pub(crate) struct SourceReadTimingState {
    pub(crate) finished: Vec<(&'static str, Duration)>,
    pub(crate) active: Option<(u64, &'static str, std::time::Instant)>,
    pub(crate) sequence: u64,
    pub(crate) omitted: bool,
}

pub(crate) struct SourceReadTimingGuard {
    pub(crate) timings: Arc<SourceReadTimings>,
    pub(crate) sequence: u64,
}

impl SourceReadTimings {
    pub(crate) fn begin(self: &Arc<Self>, name: &'static str) -> SourceReadTimingGuard {
        let mut state = self.0.lock().expect("source timing mutex poisoned");
        state.sequence += 1;
        let sequence = state.sequence;
        state.active = Some((sequence, name, std::time::Instant::now()));
        SourceReadTimingGuard {
            timings: self.clone(),
            sequence,
        }
    }

    pub(crate) fn summary(&self) -> String {
        let state = self.0.lock().expect("source timing mutex poisoned");
        let mut stages: Vec<_> = state
            .finished
            .iter()
            .map(|(name, elapsed)| format!("{name}:{}ms", elapsed.as_millis()))
            .collect();
        if let Some((_, name, started)) = state.active {
            stages.push(format!(
                "{name}:running:{}ms",
                started.elapsed().as_millis()
            ));
        }
        if state.omitted {
            stages.push("earlier_stages_omitted".to_owned());
        }
        if stages.is_empty() {
            stages.push("not_started".to_owned());
        }
        stages.join(",").chars().take(2048).collect()
    }
}

impl Drop for SourceReadTimingGuard {
    fn drop(&mut self) {
        let mut state = self.timings.0.lock().expect("source timing mutex poisoned");
        if let Some((sequence, name, started)) = state.active {
            if sequence != self.sequence {
                return;
            }
            state.active = None;
            if state.finished.len() == 32 {
                state.finished.remove(0);
                state.omitted = true;
            }
            state.finished.push((name, started.elapsed()));
        }
    }
}

pub(crate) struct PinnedGitRead {
    pub(crate) root: PathBuf,
    pub(crate) binary: PathBuf,
    pub(crate) deadline: std::time::Instant,
    pub(crate) timings: Option<Arc<SourceReadTimings>>,
}

impl PinnedGitRead {
    pub(crate) fn new(cwd: &FsPath) -> Result<Self> {
        Self::new_with_deadline(cwd, None)
    }

    pub(crate) fn new_with_deadline(
        cwd: &FsPath,
        deadline: Option<std::time::Instant>,
    ) -> Result<Self> {
        Self::new_with_deadline_and_timings(cwd, deadline, None)
    }

    pub(crate) fn new_with_deadline_and_timings(
        cwd: &FsPath,
        deadline: Option<std::time::Instant>,
        timings: Option<Arc<SourceReadTimings>>,
    ) -> Result<Self> {
        let git = Self::for_ignore_query(cwd, deadline, timings)?;
        let top = git.run(&["rev-parse", "--show-toplevel"], false)?;
        if fs::canonicalize(String::from_utf8(top)?.trim())? != git.root {
            bail!("review cwd must be the worktree root");
        }
        // Git diff can invoke filters. Ignore classification does not run
        // diff, filters or hooks and needs only the shared pinned transport.
        let filters = git.run(
            &[
                "config",
                "--null",
                "--get-regexp",
                r"^filter\..*\.(clean|smudge|process)$",
            ],
            true,
        )?;
        if !filters.is_empty() {
            return Err(SourceBasisRefusal::ConfiguredGitFilters.into());
        }
        Ok(git)
    }

    // Transport setup only; never a source-identity admission. This is used
    // solely for bounded check-ignore queries on supplied notification paths.
    pub(crate) fn for_ignore_query(
        cwd: &FsPath,
        deadline: Option<std::time::Instant>,
        timings: Option<Arc<SourceReadTimings>>,
    ) -> Result<Self> {
        check_capture_cancellation()?;
        let root = fs::canonicalize(cwd)?;
        // Never allow Windows' current-directory executable search, a relative
        // PATH entry, or a git shim supplied by the reviewed repository.
        let executable = if cfg!(windows) { "git.exe" } else { "git" };
        let binary = std::env::split_paths(&std::env::var_os("PATH").context("PATH missing")?)
            .filter(|dir| dir.is_absolute())
            .filter_map(|dir| fs::canonicalize(dir.join(executable)).ok())
            .find(|path| path.is_file() && !path.starts_with(&root))
            .context("trusted Git executable outside the reviewed tree not found")?;
        let git = Self {
            root,
            binary,
            deadline: capture_deadline(
                deadline.unwrap_or_else(|| std::time::Instant::now() + SOURCE_READ_TIMEOUT),
            ),
            timings,
        };
        Ok(git)
    }

    pub(crate) fn command(&self, args: &[&str]) -> BoundedReadCommand {
        let mut command = BoundedReadCommand::new(&self.binary);
        for (name, _) in std::env::vars_os() {
            if name
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("GIT_")
            {
                command.env_remove(name);
            }
        }
        let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
        command
            .current_dir(&self.root)
            .env("GIT_CONFIG_GLOBAL", null)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            // Missing promisor objects must fail locally. Also deny all
            // transports, independently of repository protocol/helper config.
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_ALLOW_PROTOCOL", "")
            .env("LC_ALL", "C")
            .env("LANG", "C")
            .args([
                "--no-pager",
                "-c",
                "core.fsmonitor=false",
                "-c",
                &format!("core.hooksPath={null}"),
                "-c",
                "diff.submodule=short",
                // Conversion warnings are not source changes. Suppress them
                // at Git's configuration boundary; every remaining diagnostic
                // makes the read incomplete, even when Git exits successfully.
                "-c",
                "core.safecrlf=false",
            ])
            // Unset config still falls back to XDG/HOME ignore/attributes.
            // Pin these paths as well; repository .gitignore/.gitattributes
            // remain inputs.
            .args([
                "-c",
                &format!("core.excludesFile={null}"),
                "-c",
                &format!("core.attributesFile={null}"),
            ]);
        // Command-scoped configuration also reaches Git's submodule children.
        // Never change repository, user or system configuration to measure it.
        #[cfg(windows)]
        command.args(["-c", "core.longpaths=true"]);
        command.args(args);
        command
    }

    pub(crate) fn run(&self, args: &[&str], allow_absent: bool) -> Result<Vec<u8>> {
        let name = match args.first().copied() {
            Some("rev-parse") => "git_rev_parse",
            Some("config") => "git_config",
            Some("submodule") => "git_submodule",
            Some("status" | "-c") => "git_status",
            Some("cat-file") => "git_cat_file",
            _ => "git_other",
        };
        let _timing = self.timings.as_ref().map(|timings| timings.begin(name));
        check_capture_cancellation()?;
        let mut command = self.command(args);
        let started = std::time::Instant::now();
        let remaining_at_start = self.deadline.saturating_duration_since(started);
        #[cfg(test)]
        if BOUNDED_READ_TIMING_DIAGNOSTIC.load(std::sync::atomic::Ordering::SeqCst) {
            eprintln!(
                "Git timing thread={:?} args={args:?} remaining_ms={}",
                std::thread::current().id(),
                remaining_at_start.as_millis()
            );
        }
        let output =
            run_bounded_read_command(&mut command, self.deadline, PINNED_GIT_OUTPUT_LIMIT, false)
                .map_err(|error| {
                pinned_git_failure(error, args, started.elapsed(), remaining_at_start)
            })?;
        if !output.stderr.is_empty()
            || (!output.status.success() && !(allow_absent && output.status.code() == Some(1)))
        {
            return Err(pinned_git_failure(
                anyhow!(
                    "Git read refused (exit {:?}): {}",
                    output.status.code(),
                    pinned_git_diagnostic(&output.stderr)
                ),
                args,
                started.elapsed(),
                remaining_at_start,
            ));
        }
        Ok(output.stdout)
    }
}

pub(crate) fn pinned_git_diagnostic(stderr: &[u8]) -> String {
    if [
        "filename too long",
        "file name too long",
        "path too long",
        "filename or extension is too long",
    ]
    .iter()
    .any(|cause| {
        stderr
            .windows(cause.len())
            .any(|part| part.eq_ignore_ascii_case(cause.as_bytes()))
    }) {
        return "long-path read failed; shorten the worktree or child path, or check Git's Windows long-path support; no source revision was taken".to_owned();
    }
    let diagnostic = String::from_utf8_lossy(&stderr[..stderr.len().min(4096)]);
    if diagnostic.is_empty() {
        return "Git exited unsuccessfully without a diagnostic".to_owned();
    }
    let safe: String = diagnostic
        .chars()
        .take(512)
        .map(|c| {
            if c.is_ascii() && !c.is_ascii_control() {
                c
            } else {
                '?'
            }
        })
        .collect();
    format!("Git emitted stderr; source measurement refused: {safe}")
}

pub(crate) fn pinned_git_failure(
    error: anyhow::Error,
    args: &[&str],
    elapsed: Duration,
    remaining_at_start: Duration,
) -> anyhow::Error {
    let operation = args.first().copied().unwrap_or("<missing>");
    let context = format!(
        "Git verification operation {operation:?} with arguments {args:?} failed after {elapsed:?} \
         (remaining shared budget at call start: {remaining_at_start:?}; total shared budget: \
         {SOURCE_READ_TIMEOUT:?}): {error}"
    );
    error.context(context)
}
