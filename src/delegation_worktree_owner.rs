// Owns the record of which delegation created an isolated worktree, and the
// refusal of a follow-up whose worktree is gone or is no longer the one that
// delegation created. Does not own where a worktree may live
// (delegation_worktree_location.rs), worktree creation or the dirty-state
// copy (delegations.rs), or removing a worktree: TermAl removes none; a
// worktree is removed by hand once its work is integrated or discarded.
// Carved out of unlanded work on inspecting and removing ended worktrees
// automatically, which continues separately; only the owner record and the
// follow-up refusal were kept.

use super::*;

/// File in a linked worktree's Git administrative directory naming the
/// delegation that created it. Git keeps it out of status and deletes it with
/// the worktree, so a later worktree at the same path never carries it.
const DELEGATION_WORKTREE_OWNER_FILE: &str = "termal-delegation";

#[cfg(test)]
thread_local! {
    /// Makes the next owner record on this thread fail, as a full disk would.
    pub(crate) static TEST_FAIL_NEXT_DELEGATION_WORKTREE_OWNER_RECORD: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// The single line of a small regular file exactly as Git wrote it: only a
/// final `\n` or `\r\n` is removed, so a path ending in any other whitespace
/// keeps it. The same bounded, non-blocking, regular-file checks as the Git
/// boundary's metadata reader, so a FIFO or link left there cannot make the
/// caller wait or lead it elsewhere.
fn delegation_worktree_exact_line(path: &FsPath) -> io::Result<Option<String>> {
    use std::io::Read as _;
    let regular = |metadata: &fs::Metadata| {
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && !review_freeze_is_reparse(metadata)
    };
    if !regular(&fs::symlink_metadata(path)?) {
        return Ok(None);
    }
    let file = identity_open(path)?;
    if !regular(&file.metadata()?) {
        return Ok(None);
    }
    let mut text = String::new();
    file.take(8193).read_to_string(&mut text)?;
    if text.len() > 8192 {
        return Ok(None);
    }
    let line = text
        .strip_suffix('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .unwrap_or(&text);
    Ok((!line.is_empty() && !line.contains(['\r', '\n', '\0'])).then(|| line.to_owned()))
}

/// The worktree's Git administrative directory, named by its `.git` file and
/// read exactly, so a worktree whose name ends in whitespace names its own
/// administrative directory and never a sibling's.
fn delegation_worktree_admin_dir(worktree: &FsPath) -> io::Result<Option<PathBuf>> {
    let Some(line) = delegation_worktree_exact_line(&worktree.join(".git"))? else {
        return Ok(None);
    };
    Ok(line
        .strip_prefix("gitdir: ")
        .filter(|path| !path.trim().is_empty())
        .map(|path| worktree.join(path)))
}

/// Records which delegation created the worktree at this path.
pub(crate) fn record_delegation_worktree_owner(
    worktree: &FsPath,
    delegation_id: &str,
) -> Result<(), ApiError> {
    #[cfg(test)]
    if TEST_FAIL_NEXT_DELEGATION_WORKTREE_OWNER_RECORD.with(|fail| fail.replace(false)) {
        return Err(ApiError::internal(format!(
            "failed to record the owner of isolated worktree `{}`: injected test failure",
            worktree.display()
        )));
    }
    let admin = delegation_worktree_admin_dir(worktree)
        .map_err(|err| err.to_string())
        .and_then(|admin| admin.ok_or_else(|| "no gitdir line".to_owned()))
        .map_err(|cause| {
            ApiError::internal(format!(
                "isolated worktree `{}` has no readable Git administrative directory: {cause}",
                worktree.display()
            ))
        })?;
    // A new worktree's administrative directory holds no owner yet. Never
    // write over one that does: it is another worktree's record.
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(admin.join(DELEGATION_WORKTREE_OWNER_FILE))
        .and_then(|mut file| {
            use std::io::Write as _;
            file.write_all(delegation_id.as_bytes())
        });
    written.map_err(|err| {
        ApiError::internal(if err.kind() == io::ErrorKind::AlreadyExists {
            format!(
                "isolated worktree `{}` names an administrative directory that already records \
                 an owner; it was not overwritten",
                worktree.display()
            )
        } else {
            format!(
                "failed to record the owner of isolated worktree `{}`: {err}",
                worktree.display()
            )
        })
    })
}

/// What the owner record of the worktree at a path says.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DelegationWorktreeOwner {
    /// The delegation that created it. A worktree another delegation made at
    /// a reused path names that one.
    Recorded(String),
    /// None: no linked worktree there, one Git no longer links (a partial
    /// removal, or a `.git` file left at a path the worktree moved away
    /// from), something else at the path, or a worktree made before owners
    /// were recorded.
    Missing,
    /// It could not be read now (for example a sharing violation on
    /// Windows); a later attempt reads it again.
    Unreadable,
}

/// The administrative directory of the linked worktree at this path, when Git
/// still links it: the worktree's `.git` names it, it and the repository's
/// common directory each hold a HEAD, the common directory holds objects and
/// refs, and its backpointer names this `.git`. What a HEAD names is not
/// judged, so any branch name Git accepts passes. Every file is read as a
/// regular file, so a HEAD stored as a link (core.preferSymlinkRefs) is
/// refused.
fn delegation_worktree_link(worktree: &FsPath) -> io::Result<Option<PathBuf>> {
    use delegation_git_boundary::metadata_line;
    let marker = worktree.join(".git");
    let Some(admin) = delegation_worktree_admin_dir(worktree)? else {
        return Ok(None);
    };
    if !fs::metadata(&admin)?.is_dir() || metadata_line(&admin.join("HEAD"))?.is_none() {
        return Ok(None);
    }
    let (Some(common), Some(backpointer)) = (
        metadata_line(&admin.join("commondir"))?,
        metadata_line(&admin.join("gitdir"))?,
    ) else {
        return Ok(None);
    };
    if normalize_path_best_effort(&admin.join(backpointer)) != normalize_path_best_effort(&marker) {
        return Ok(None);
    }
    let common = admin.join(common);
    let linked = metadata_line(&common.join("HEAD"))?.is_some()
        && fs::metadata(common.join("objects"))?.is_dir()
        && fs::metadata(common.join("refs"))?.is_dir();
    Ok(linked.then_some(admin))
}

/// Reads the owner record of the worktree at this path, with the Git
/// boundary's bounded, non-blocking reads of regular files. It touches the
/// file system, so it never runs under the state lock: a path lookup on an
/// unreachable share could wait.
pub(crate) fn delegation_worktree_owner(worktree: &FsPath) -> DelegationWorktreeOwner {
    // Nothing there, a path under a file, or a file that is not text is no
    // worktree of ours; any other read error may pass, as a sharing violation
    // on Windows does.
    let classify = |err: io::Error| match err.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory | io::ErrorKind::InvalidData => {
            DelegationWorktreeOwner::Missing
        }
        _ => DelegationWorktreeOwner::Unreadable,
    };
    // A worktree Git no longer links is not one a follow-up may run in.
    let admin = match delegation_worktree_link(worktree) {
        Ok(Some(admin)) => admin,
        Ok(None) => return DelegationWorktreeOwner::Missing,
        Err(err) => return classify(err),
    };
    match delegation_git_boundary::metadata_line(&admin.join(DELEGATION_WORKTREE_OWNER_FILE)) {
        Ok(Some(owner)) => DelegationWorktreeOwner::Recorded(owner),
        Ok(None) => DelegationWorktreeOwner::Missing,
        Err(err) => classify(err),
    }
}

/// The worktree a follow-up of this delegation runs in, if it has one.
fn delegation_followup_worktree(delegation: &DelegationRecord) -> Option<&str> {
    match &delegation.write_policy {
        DelegationWritePolicy::IsolatedWorktree {
            worktree_path: Some(worktree),
            ..
        } => Some(worktree),
        _ => None,
    }
}

impl AppState {
    /// A follow-up needs the delegation's worktree. Refused once it is gone or
    /// is no longer the one this delegation created: never run in a
    /// recreated, reused, half-removed or missing directory. Checked off the
    /// state lock, before restoration and again just before the lock that
    /// admits the follow-up; returns the worktree it checked, for that lock
    /// to confirm unchanged.
    pub(crate) fn check_followup_worktree(
        &self,
        parent_session_id: &str,
        delegation_id: &str,
    ) -> Result<Option<String>, ApiError> {
        let worktree = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index =
                find_parent_delegation_index_locked(&inner, parent_session_id, delegation_id)?;
            delegation_followup_worktree(&inner.delegations[index]).map(str::to_owned)
        };
        let Some(worktree) = worktree else {
            return Ok(None);
        };
        let worktree_text = truncate_chars(&worktree, DELEGATION_WORKTREE_DETAIL_MAX_CHARS);
        // Only the linked worktree this delegation created qualifies: anything
        // else at the path (a worktree another delegation or a person made, a
        // plain directory, a partly removed worktree, or one from before owners
        // were recorded) is not run in.
        match delegation_worktree_owner(FsPath::new(&worktree)) {
            DelegationWorktreeOwner::Recorded(owner) if owner == delegation_id => {
                Ok(Some(worktree))
            }
            DelegationWorktreeOwner::Unreadable => Err(ApiError::conflict(format!(
                "this delegation's worktree `{worktree_text}` could not be read just now; retry \
                 the follow-up shortly"
            ))),
            _ => Err(ApiError::conflict(format!(
                "this delegation's worktree `{worktree_text}` is gone, is no longer the one it \
                 created, or was created before TermAl recorded owners; start a new delegation \
                 instead"
            ))),
        }
    }
}

/// Under the state lock, with no file access: the delegation's worktree is
/// still the one its follow-up checked just before.
pub(crate) fn confirm_followup_worktree_locked(
    inner: &StateInner,
    delegation_index: usize,
    checked: Option<&str>,
) -> Result<(), ApiError> {
    if delegation_followup_worktree(&inner.delegations[delegation_index]) == checked {
        Ok(())
    } else {
        Err(ApiError::conflict(
            "this delegation's worktree changed while its follow-up was checked; retry the \
             follow-up",
        ))
    }
}
