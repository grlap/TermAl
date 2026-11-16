// Owns where an isolated delegation's worktree may live inside its source
// repository (strictly inside, outside `.git`, ignored by Git) and the sweep
// that removes it once the delegation has ended with nothing to lose: a clean
// worktree whose HEAD is its base or reachable from a ref, holding no other
// worktree. A worktree with uncommitted work, commits no ref holds or a
// nested worktree is retained, with the reason on the delegation result.
// Does not own worktree creation or the dirty-state copy (delegations.rs) or
// worker base resolution (delegation_worker.rs). New module; the default
// location formerly lived under the TermAl data dir in delegations.rs.

/// The Git-ignored directory, relative to a source repository's root, that
/// holds its delegation worktrees by default.
const DELEGATION_WORKTREES_DIR: &str = ".worktrees";

/// Longest Git detail kept in a refusal or a retention note.
const DELEGATION_WORKTREE_DETAIL_MAX_CHARS: usize = 400;

/// Start of every retention note; a delegation whose result carries one is
/// not inspected again until a follow-up clears that result.
const DELEGATION_WORKTREE_RETAINED_PREFIX: &str = "Worktree retained at `";

/// Where sweep requests run. The host runs them on one background thread,
/// which coalesces requests that arrive while a pass runs and ends when none
/// is left; test states only count them and run `sweep_delegation_worktrees`
/// themselves, so no pass outlives its fixture.
#[derive(Clone)]
struct DelegationWorktreeSweeper {
    background: bool,
    state: Arc<Mutex<DelegationWorktreeSweepState>>,
}

#[derive(Default)]
struct DelegationWorktreeSweepState {
    running: bool,
    again: bool,
    requests: u64,
}

impl DelegationWorktreeSweeper {
    fn new(background: bool) -> Self {
        Self {
            background,
            state: Arc::default(),
        }
    }
}

/// The worktree's path relative to the source repository's root, refused
/// unless it lies strictly inside that root and outside its `.git`.
fn isolated_worktree_relative_path(
    source_repo_root: &FsPath,
    worktree_root: &FsPath,
) -> Result<PathBuf, ApiError> {
    let root = normalize_path_best_effort(source_repo_root);
    let worktree = normalize_path_best_effort(worktree_root);
    let relative = worktree
        .strip_prefix(&root)
        .ok()
        .filter(|relative| {
            relative.components().next().is_some_and(|first| {
                !first
                    .as_os_str()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(".git")
            })
        })
        .map(FsPath::to_path_buf);
    relative.ok_or_else(|| {
        ApiError::bad_request(format!(
            "isolated worktree path `{}` must be inside the source repository `{}`, under its \
             Git-ignored `{DELEGATION_WORKTREES_DIR}/` directory (the default when no path is given)",
            truncate_chars(
                &worktree_root.to_string_lossy(),
                DELEGATION_WORKTREE_DETAIL_MAX_CHARS
            ),
            truncate_chars(
                &source_repo_root.to_string_lossy(),
                DELEGATION_WORKTREE_DETAIL_MAX_CHARS
            ),
        ))
    })
}

/// Refuses a worktree location Git does not ignore: an unignored nested
/// worktree would show in the parent's status and block its next spawn.
fn ensure_isolated_worktree_ignored(
    source_repo_root: &FsPath,
    relative: &FsPath,
) -> Result<(), ApiError> {
    let mut request = relative.to_string_lossy().replace('\\', "/");
    request.insert_str(0, "./");
    request.push('/');
    let output = git_command()
        .arg("-C")
        .arg(source_repo_root)
        .args(["check-ignore", "-q", "--"])
        .arg(&request)
        .output()
        .map_err(|err| ApiError::internal(format!("failed to run git check-ignore: {err}")))?;
    match output.status.code() {
        Some(0) => Ok(()),
        Some(1) => Err(ApiError::bad_request(format!(
            "isolated worktree path `{}` is not ignored by Git in `{}`; add \
             `{DELEGATION_WORKTREES_DIR}/` to that repository's .gitignore",
            truncate_chars(
                &relative.to_string_lossy(),
                DELEGATION_WORKTREE_DETAIL_MAX_CHARS
            ),
            truncate_chars(
                &source_repo_root.to_string_lossy(),
                DELEGATION_WORKTREE_DETAIL_MAX_CHARS
            ),
        ))),
        _ => Err(ApiError::internal(format!(
            "git check-ignore failed for the isolated worktree path: {}",
            truncate_chars(
                &extract_git_command_error(&output),
                DELEGATION_WORKTREE_DETAIL_MAX_CHARS
            )
        ))),
    }
}

/// What the inspection of a terminal delegation's worktree decided.
enum DelegationWorktreeReclaim {
    /// Nothing to lose: remove it with `git worktree remove` run against this
    /// source repository root.
    Remove(PathBuf),
    /// Kept, with the bounded note that says why.
    Retained(String),
    /// Not a linked worktree at that path inside its repository: left alone.
    Skipped,
}

fn delegation_worktree_retained_note(worktree: &FsPath, reason: &str) -> String {
    format!(
        "{DELEGATION_WORKTREE_RETAINED_PREFIX}{}`: {reason}. Integrate or discard it, then remove it with `git worktree remove`.",
        worktree.to_string_lossy()
    )
}

/// Runs one Git command in `directory` and returns its trimmed stdout, or the
/// bounded detail of its failure.
fn delegation_worktree_git(directory: &FsPath, args: &[&str]) -> Result<String, String> {
    let output = git_command()
        .arg("-C")
        .arg(directory)
        .args(args)
        .output()
        .map_err(|err| format!("failed to run git {}: {err}", args.join(" ")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(truncate_chars(
            &extract_git_command_error(&output),
            DELEGATION_WORKTREE_DETAIL_MAX_CHARS,
        ))
    }
}

/// Decides off the state lock whether a terminal delegation's worktree holds
/// anything to lose. Only a linked worktree at exactly this path, strictly
/// inside the repository that contains its parent directory, is considered;
/// one elsewhere (made before worktrees lived in their repository) is left as
/// it is.
fn inspect_isolated_worktree(
    worktree: &FsPath,
    base_revision: Option<&str>,
) -> DelegationWorktreeReclaim {
    if !worktree.join(".git").is_file() {
        return DelegationWorktreeReclaim::Skipped;
    }
    let worktree_key = normalize_path_best_effort(worktree);
    match delegation_worktree_git(worktree, &["rev-parse", "--show-toplevel"]) {
        Ok(toplevel) if normalize_path_best_effort(FsPath::new(&toplevel)) == worktree_key => {}
        _ => return DelegationWorktreeReclaim::Skipped,
    }
    let Some(container) = worktree.parent() else {
        return DelegationWorktreeReclaim::Skipped;
    };
    let Ok(source_root) = delegation_worktree_git(container, &["rev-parse", "--show-toplevel"])
    else {
        return DelegationWorktreeReclaim::Skipped;
    };
    let source_root = PathBuf::from(source_root);
    if isolated_worktree_relative_path(&source_root, worktree).is_err() {
        return DelegationWorktreeReclaim::Skipped;
    }
    let retained = |reason: String| {
        DelegationWorktreeReclaim::Retained(delegation_worktree_retained_note(worktree, &reason))
    };
    match delegation_worktree_git(
        worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    ) {
        Ok(status) if status.is_empty() => {}
        Ok(_) => return retained("it holds uncommitted or untracked changes".to_owned()),
        Err(detail) => return retained(format!("its status could not be read ({detail})")),
    }
    let head = match delegation_worktree_git(worktree, &["rev-parse", "HEAD"]) {
        Ok(head) => head,
        Err(detail) => return retained(format!("its HEAD could not be read ({detail})")),
    };
    if base_revision != Some(head.as_str()) {
        match delegation_worktree_git(
            worktree,
            &[
                "for-each-ref",
                "--contains",
                head.as_str(),
                "--count=1",
                "--format=%(refname)",
            ],
        ) {
            Ok(refs) if !refs.is_empty() => {}
            Ok(_) => {
                return retained(format!(
                    "its HEAD commit {head} is on no branch or tag, so removing it would lose that commit"
                ));
            }
            Err(detail) => {
                return retained(format!("its commits could not be checked ({detail})"));
            }
        }
    }
    // A nested delegation's worktree lives in this one's ignored
    // `.worktrees/`, invisible to its status; removing this directory would
    // delete it with everything it holds.
    match delegation_worktree_git(&source_root, &["worktree", "list", "--porcelain"]) {
        Ok(listing) => {
            let nested = listing
                .lines()
                .filter_map(|line| line.strip_prefix("worktree "))
                .map(|path| normalize_path_best_effort(FsPath::new(path)))
                .any(|path| path != worktree_key && path.starts_with(&worktree_key));
            if nested {
                return retained(
                    "it contains another worktree, which removing it would delete".to_owned(),
                );
            }
        }
        Err(detail) => {
            return retained(format!(
                "its nested worktrees could not be listed ({detail})"
            ));
        }
    }
    DelegationWorktreeReclaim::Remove(source_root)
}

/// Removes a worktree the inspection found has nothing to lose. Without
/// `--force` Git itself refuses one that changed since that inspection;
/// ignored build output goes with the worktree.
fn remove_isolated_worktree(source_root: &FsPath, worktree: &FsPath) -> Result<(), String> {
    let path = worktree.to_string_lossy();
    delegation_worktree_git(source_root, &["worktree", "remove", "--", path.as_ref()]).map(|_| ())
}

/// Terminal isolated delegations the sweep has not settled: their recorded
/// worktree may still need inspecting.
fn delegation_worktree_sweep_candidates_locked(inner: &StateInner) -> Vec<(String, PathBuf)> {
    inner
        .delegations
        .iter()
        .filter(|delegation| {
            delegation_is_terminal(delegation.status)
                && !inner.settled_delegation_worktrees.contains(&delegation.id)
                && !inner
                    .reclaiming_delegation_worktrees
                    .contains(&delegation.id)
                && !delegation.result.as_ref().is_some_and(|result| {
                    result
                        .notes
                        .iter()
                        .any(|note| note.starts_with(DELEGATION_WORKTREE_RETAINED_PREFIX))
                })
        })
        .filter_map(|delegation| match &delegation.write_policy {
            DelegationWritePolicy::IsolatedWorktree {
                worktree_path: Some(worktree),
                ..
            } => Some((delegation.id.clone(), PathBuf::from(worktree))),
            _ => None,
        })
        .collect()
}

/// Reserves a delegation's worktree for one inspection, in the same lock that
/// admits a follow-up: only while the delegation is still terminal, no
/// follow-up is being admitted, the child's runtime is gone and its shutdown
/// finished, and no closing source observation of the child still reads the
/// worktree. Anything else waits for a later sweep.
fn reserve_delegation_worktree_locked(
    inner: &mut StateInner,
    delegation_id: &str,
) -> DelegationWorktreeReservation {
    let Some(index) = inner.find_delegation_index(delegation_id) else {
        return DelegationWorktreeReservation::Wait;
    };
    let delegation = &inner.delegations[index];
    if !delegation_is_terminal(delegation.status)
        || inner
            .delegation_followup_admissions
            .contains_key(delegation_id)
        || inner
            .reclaiming_delegation_worktrees
            .contains(delegation_id)
        || inner
            .delegation_child_shutdowns
            .contains(&delegation.child_session_id)
    {
        return DelegationWorktreeReservation::Wait;
    }
    let DelegationWritePolicy::IsolatedWorktree {
        worktree_path: Some(worktree),
        ..
    } = &delegation.write_policy
    else {
        return DelegationWorktreeReservation::Wait;
    };
    let worktree = PathBuf::from(worktree);
    if let Some(child) = inner.find_session_index(&delegation.child_session_id) {
        let record = &inner.sessions[child];
        if runtime_observation_terminal_removal_notice(record).is_some() {
            // That observation will never settle and still reads this
            // worktree; keep it rather than wait forever.
            return DelegationWorktreeReservation::Keep(delegation_worktree_retained_note(
                &worktree,
                "its child's closing source observation could not be delivered",
            ));
        }
        if !matches!(record.runtime, SessionRuntime::None)
            || runtime_observation_has_removal_duty(record)
            || record.engram.source_root_bind_owner.is_some()
        {
            return DelegationWorktreeReservation::Wait;
        }
    }
    let base_revision = delegation.base_revision.clone();
    inner
        .reclaiming_delegation_worktrees
        .insert(delegation_id.to_owned());
    DelegationWorktreeReservation::Reserved(worktree, base_revision)
}

/// What the sweep may do with one delegation's worktree right now.
enum DelegationWorktreeReservation {
    /// Held for inspection: the worktree and the delegation's base revision.
    Reserved(PathBuf, Option<String>),
    /// Not yet: a later pass looks again.
    Wait,
    /// Kept for good, with this note.
    Keep(String),
}

impl AppState {
    /// One pass of the worktree reconciler: every terminal isolated
    /// delegation whose worktree is still there and reservable is inspected
    /// and, with nothing to lose, removed. Requested at boot, after each
    /// terminal transition and when a delegation child's source observation
    /// settles; a later pass retries whatever was not yet reservable, so
    /// nothing depends on an in-memory queue, and a missed request is caught
    /// at the next boot.
    fn sweep_delegation_worktrees(&self) {
        let candidates = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            delegation_worktree_sweep_candidates_locked(&inner)
        };
        for (delegation_id, worktree) in candidates {
            if !worktree.join(".git").is_file() {
                // Already removed, or never a linked worktree: nothing for
                // this process to do (a removed worktree is never recreated).
                self.inner
                    .lock()
                    .expect("state mutex poisoned")
                    .settled_delegation_worktrees
                    .insert(delegation_id);
                continue;
            }
            let reserved = {
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                reserve_delegation_worktree_locked(&mut inner, &delegation_id)
            };
            let (worktree, base_revision) = match reserved {
                DelegationWorktreeReservation::Wait => continue,
                DelegationWorktreeReservation::Keep(note) => {
                    self.note_retained_delegation_worktree(delegation_id, note);
                    continue;
                }
                DelegationWorktreeReservation::Reserved(worktree, base_revision) => {
                    (worktree, base_revision)
                }
            };
            let (note, settled) =
                match inspect_isolated_worktree(&worktree, base_revision.as_deref()) {
                    DelegationWorktreeReclaim::Skipped => (None, true),
                    DelegationWorktreeReclaim::Retained(note) => (Some(note), false),
                    DelegationWorktreeReclaim::Remove(source_root) => {
                        match remove_isolated_worktree(&source_root, &worktree) {
                            Ok(()) => (None, false),
                            Err(detail) => (
                                Some(delegation_worktree_retained_note(
                                    &worktree,
                                    &format!("git worktree remove refused ({detail})"),
                                )),
                                false,
                            ),
                        }
                    }
                };
            {
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                inner.reclaiming_delegation_worktrees.remove(&delegation_id);
                if settled {
                    inner
                        .settled_delegation_worktrees
                        .insert(delegation_id.clone());
                }
            }
            if let Some(note) = note {
                self.note_retained_delegation_worktree(delegation_id, note);
            }
        }
    }

    /// Records why a worktree is kept on the delegation's result. The note
    /// also keeps later passes from inspecting it again.
    fn note_retained_delegation_worktree(&self, delegation_id: String, note: String) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_delegation_index(&delegation_id) else {
            return;
        };
        let Some(result) = inner.delegations[index].result.as_mut() else {
            // Nowhere to keep the reason: do not inspect it again.
            inner.settled_delegation_worktrees.insert(delegation_id);
            return;
        };
        if !result.notes.contains(&note) {
            result.notes.push(note);
            inner.mark_delegation_mutated(index);
            let _ = self.commit_locked(&mut inner);
        }
    }

    /// Asks for a sweep pass. Event-driven only: nothing runs while nothing
    /// has happened. Requests that arrive during a pass coalesce into one
    /// more pass.
    fn request_delegation_worktree_sweep(&self) {
        let sweeper = &self.delegation_worktree_sweeper;
        {
            let mut sweep = sweeper.state.lock().expect("worktree sweep poisoned");
            sweep.requests += 1;
            if !sweeper.background {
                return;
            }
            if sweep.running {
                sweep.again = true;
                return;
            }
            sweep.running = true;
        }
        let state = self.clone();
        let spawned = std::thread::Builder::new()
            .name("termal-worktree-sweep".to_owned())
            .spawn(move || {
                loop {
                    state.sweep_delegation_worktrees();
                    let mut sweep = state
                        .delegation_worktree_sweeper
                        .state
                        .lock()
                        .expect("worktree sweep poisoned");
                    if !std::mem::take(&mut sweep.again) {
                        sweep.running = false;
                        return;
                    }
                }
            });
        if spawned.is_err() {
            sweeper
                .state
                .lock()
                .expect("worktree sweep poisoned")
                .running = false;
        }
    }

    /// How many sweep passes were requested so far.
    #[cfg(test)]
    fn delegation_worktree_sweep_requests(&self) -> u64 {
        self.delegation_worktree_sweeper
            .state
            .lock()
            .expect("worktree sweep poisoned")
            .requests
    }

    /// Lifts the fence a detached child runtime set, once its shutdown has
    /// finished; its worktree may then be reclaimed. A failed shutdown keeps
    /// the fence for the life of the process.
    fn release_delegation_child_shutdown(&self, child_session_id: &str) {
        self.inner
            .lock()
            .expect("state mutex poisoned")
            .delegation_child_shutdowns
            .remove(child_session_id);
    }
}

/// A follow-up needs the delegation's worktree. Refused while the sweep holds
/// it, or once it was removed; never run in a recreated or missing directory.
/// Checked again in the lock that admits the follow-up.
fn refuse_followup_without_worktree_locked(
    inner: &StateInner,
    delegation_index: usize,
) -> Result<(), ApiError> {
    let delegation = &inner.delegations[delegation_index];
    let DelegationWritePolicy::IsolatedWorktree {
        worktree_path: Some(worktree),
        ..
    } = &delegation.write_policy
    else {
        return Ok(());
    };
    let worktree_text = truncate_chars(worktree, DELEGATION_WORKTREE_DETAIL_MAX_CHARS);
    if inner
        .reclaiming_delegation_worktrees
        .contains(&delegation.id)
    {
        return Err(ApiError::conflict(format!(
            "this delegation's worktree `{worktree_text}` is being checked for removal after \
             the delegation ended; retry the follow-up shortly, or start a new delegation"
        )));
    }
    if !FsPath::new(worktree).is_dir() {
        return Err(ApiError::conflict(format!(
            "this delegation's worktree `{worktree_text}` was removed after it ended with \
             nothing to keep; start a new delegation instead"
        )));
    }
    Ok(())
}
