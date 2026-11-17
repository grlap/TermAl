// Owns where an isolated delegation's worktree may live: strictly inside its
// source repository, outside `.git`, in a directory Git ignores, by default
// `<source repo>/.worktrees/d-<first twelve hex digits of the id>`, and short
// enough for Git's administrative directory limit on Windows.
// Does not own worktree creation or the dirty-state copy (delegations.rs),
// worker base resolution (delegation_worker.rs), or removing a worktree once
// its delegation has ended: ended worktrees stay where they are and the
// spawning parent removes them. New module; the default location formerly
// lived under the TermAl data dir in delegations.rs.

/// The Git-ignored directory, relative to a source repository's root, that
/// holds its delegation worktrees by default.
const DELEGATION_WORKTREES_DIR: &str = ".worktrees";

/// The default folder, under `DELEGATION_WORKTREES_DIR`, for a delegation's
/// worktree: `d-` and the first twelve hex digits of its id. Git also names
/// the worktree's administrative directory after this folder, so a short name
/// keeps both paths clear of Git's limit on Windows.
fn delegation_worktree_folder_name(delegation_id: &str) -> String {
    let digits = delegation_id
        .strip_prefix("delegation-")
        .unwrap_or(delegation_id)
        .chars()
        .filter(char::is_ascii_hexdigit)
        .take(12)
        .collect::<String>();
    if digits.len() == 12 {
        format!("d-{digits}")
    } else {
        delegation_id.to_owned()
    }
}

/// Longest `$GIT_DIR` Git for Windows accepts: it refuses one longer than its
/// PATH_MAX (260) less 40 with "'$GIT_DIR' too big".
#[cfg(windows)]
const GIT_WINDOWS_GIT_DIR_MAX_CHARS: usize = 220;

/// The administrative directory `git worktree add` will create for a worktree
/// whose folder is `name`: `<common dir>/worktrees/<name>`, or, when that is
/// taken, the same with the first free numeric suffix (`name1`, `name2`, ...).
#[cfg(windows)]
fn git_worktree_admin_dir(common: &str, name: &str) -> String {
    let base = format!("{common}/worktrees/{name}");
    if !FsPath::new(&base).exists() {
        return base;
    }
    (1u32..)
        .map(|counter| format!("{base}{counter}"))
        .find(|candidate| !FsPath::new(candidate).exists())
        .unwrap_or(base)
}

/// Refuses, with the reason, a worktree Git would refuse on Windows, instead
/// of letting `git worktree add` fail with "'$GIT_DIR' too big" after it has
/// begun. Git checks both the worktree's `.git` file and the administrative
/// directory it creates, so the longer of the two must fit. Other platforms
/// allow paths far longer than any worktree TermAl creates.
#[cfg(windows)]
fn refuse_worktree_beyond_git_path_limit(
    source_repo_root: &FsPath,
    worktree_root: &FsPath,
) -> Result<(), ApiError> {
    let Some(folder) = worktree_root.file_name() else {
        return Ok(());
    };
    let common = git_repo_output(
        source_repo_root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        "failed to read the source repository's Git directory",
    )?;
    let common = String::from_utf8_lossy(&common).trim().to_owned();
    let admin = git_worktree_admin_dir(&common, &folder.to_string_lossy());
    let git_file = format!("{}/.git", worktree_root.to_string_lossy());
    let (longest, length) = [admin, git_file]
        .into_iter()
        .map(|path| {
            let length = path.len();
            (path, length)
        })
        .max_by_key(|(_, length)| *length)
        .expect("two candidate paths");
    if length <= GIT_WINDOWS_GIT_DIR_MAX_CHARS {
        return Ok(());
    }
    Err(ApiError::bad_request(format!(
        "isolated worktree path `{}` is too deep for Git on Windows: Git would use \
         `{longest}`, {length} characters, and refuses more than \
         {GIT_WINDOWS_GIT_DIR_MAX_CHARS}; move the repository to a shorter path or \
         give a shorter worktree path",
        worktree_root.display()
    )))
}

#[cfg(not(windows))]
fn refuse_worktree_beyond_git_path_limit(
    _source_repo_root: &FsPath,
    _worktree_root: &FsPath,
) -> Result<(), ApiError> {
    Ok(())
}

/// Longest path or Git detail kept in a refusal.
const DELEGATION_WORKTREE_DETAIL_MAX_CHARS: usize = 400;

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
            "isolated worktree path `{}` must be inside the source repository `{}`, outside its \
             `.git`, in a directory Git ignores, such as `{DELEGATION_WORKTREES_DIR}/` (the \
             default when no path is given)",
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
    // A backslash separates components only on Windows; elsewhere it is part
    // of a file name, and Git must check that name, not a different path.
    // `check-ignore` takes the path as given (it rejects literal-pathspec
    // magic), and the leading `./` keeps it from being read as magic.
    let mut request = relative.to_string_lossy().into_owned();
    if cfg!(windows) {
        request = request.replace('\\', "/");
    }
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
