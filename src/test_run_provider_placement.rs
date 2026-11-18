// Factual provider placement, split from the observation start callers.
// A transient target validates arguments only; it is never published as a
// check, source snapshot or credit authority.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TestRunPlacementRefusal {
    DirectoryUnknown,
    NetworkDirectory,
    DirectoryUnavailable,
    RepositoryUnavailable,
    ForeignRepository,
    UnregisteredWorktree,
    AmbiguousWorktree,
    CommandTargetUnconfirmed,
}

impl TestRunPlacementRefusal {
    fn message(self) -> &'static str {
        match self {
            Self::DirectoryUnknown => {
                "Provider placement refused (directory_known): command cwd is unknown; report an explicit cwd inside the session repository."
            }
            Self::NetworkDirectory => {
                "Provider placement refused (local_directory): command cwd or session workdir is a network path; use a local registered worktree."
            }
            Self::DirectoryUnavailable => {
                "Provider placement refused (directory_exists): command cwd cannot be resolved as a directory; restore it or use an existing registered worktree."
            }
            Self::RepositoryUnavailable => {
                "Provider placement refused (git_common_directory): the session or command root has no readable Git common directory; restore its Git metadata."
            }
            Self::ForeignRepository => {
                "Provider placement refused (same_repository): command and session have different Git common directories; use a registered worktree of the session repository."
            }
            Self::UnregisteredWorktree => {
                "Provider placement refused (registered_worktree): the command root has no matching Git worktree registration; recreate it with git worktree add."
            }
            Self::AmbiguousWorktree => {
                "Provider placement refused (unique_worktree): possible command directories name different worktrees; report one explicit cwd."
            }
            Self::CommandTargetUnconfirmed => {
                "Provider association refused (command_target): command arguments or shell syntax do not confirm a test of the command worktree; use a literal test command whose inputs stay in that worktree."
            }
        }
    }
}

// The metadata tells us which local path to inspect, but it may not redirect
// a read. Resolve leading relative climbs from the known directory only.
fn test_run_git_metadata_target(base: &FsPath, raw: &str) -> Option<PathBuf> {
    use std::path::Component;
    if engram_network_path(raw) {
        return None;
    }
    let path = FsPath::new(raw);
    if !path.is_absolute() && matches!(path.components().next(), Some(Component::Prefix(_))) {
        return None;
    }
    let mut named = path.has_root();
    for component in path.components() {
        match component {
            Component::Normal(_) => named = true,
            Component::ParentDir if named => return None,
            _ => {}
        }
    }
    #[cfg(windows)]
    let base = normalize_user_facing_path(base);
    let target = engram_lexical_path(&base.join(path));
    (target.is_absolute() && !engram_network_path(target.to_str()?)).then_some(target)
}

fn test_run_same_git_path(left: &FsPath, right: &FsPath) -> bool {
    #[cfg(windows)]
    {
        // Compare native components with equivalent verbatim-drive spelling.
        // Do not use the lossy path keys used by UI discovery.
        left.to_str().is_some()
            && right.to_str().is_some()
            && normalize_user_facing_path(left) == normalize_user_facing_path(right)
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn test_run_provider_git_common_dir(
    root: &FsPath,
    expected_common: Option<&FsPath>,
) -> Result<PathBuf, TestRunPlacementRefusal> {
    use TestRunPlacementRefusal as Refusal;
    if engram_network_path(&root.to_string_lossy()) {
        return Err(Refusal::NetworkDirectory);
    }
    let marker = root.join(".git");
    let marker_kind = fs::symlink_metadata(&marker).map_err(|_| Refusal::RepositoryUnavailable)?;
    identity_plain_metadata(&marker_kind).map_err(|_| Refusal::RepositoryUnavailable)?;
    if marker_kind.is_dir() {
        if expected_common.is_some_and(|expected| !test_run_same_git_path(&marker, expected)) {
            return Err(Refusal::ForeignRepository);
        }
        return Ok(marker);
    }
    let read = |path: &FsPath| {
        delegation_git_boundary::metadata_line(path)
            .ok()
            .flatten()
            .ok_or(Refusal::RepositoryUnavailable)
    };
    let pointer = read(&marker)?;
    let gitdir = pointer
        .strip_prefix("gitdir:")
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or(Refusal::RepositoryUnavailable)?;
    let admin = test_run_git_metadata_target(root, gitdir).ok_or(Refusal::RepositoryUnavailable)?;
    let Some(common) = admin
        .parent()
        .filter(|parent| parent.file_name().is_some_and(|name| name == "worktrees"))
        .and_then(FsPath::parent)
    else {
        return Err(Refusal::UnregisteredWorktree);
    };
    // Reject a foreign admin directory before issuing any I/O against it.
    if expected_common.is_some_and(|expected| !test_run_same_git_path(common, expected)) {
        return Err(Refusal::ForeignRepository);
    }
    identity_no_links(&admin).map_err(|_| Refusal::UnregisteredWorktree)?;
    if !fs::symlink_metadata(&admin).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(Refusal::UnregisteredWorktree);
    }
    let resolved_common = test_run_git_metadata_target(&admin, &read(&admin.join("commondir"))?)
        .ok_or(Refusal::RepositoryUnavailable)?;
    if !test_run_same_git_path(&resolved_common, common) {
        return Err(Refusal::ForeignRepository);
    }
    let backlink = test_run_git_metadata_target(&admin, &read(&admin.join("gitdir"))?)
        .ok_or(Refusal::UnregisteredWorktree)?;
    if !test_run_same_git_path(&backlink, &marker) {
        return Err(Refusal::UnregisteredWorktree);
    }
    Ok(common.to_path_buf())
}

fn test_run_provider_worktree(
    check: &EngramCheckCommand,
    workdir: &FsPath,
    directories: Option<&[Option<String>]>,
) -> Result<String, TestRunPlacementRefusal> {
    use TestRunPlacementRefusal as Refusal;
    if engram_network_path(&workdir.to_string_lossy()) {
        return Err(Refusal::NetworkDirectory);
    }
    let session_root = engram_worktree_root_path(workdir);
    let explicit = check.directory.as_deref().map(|directory| {
        engram_resolve_shell_move(None, directory).map(|directory| vec![Some(directory)])
    });
    let explicit = match explicit {
        Some(Some(explicit)) => Some(explicit),
        Some(None) => return Err(Refusal::DirectoryUnknown),
        None => None,
    };
    let directories = explicit
        .as_deref()
        .or(directories)
        .ok_or(Refusal::DirectoryUnknown)?;
    let mut root = None;
    for directory in directories {
        let directory = directory.as_deref().map_or_else(
            || workdir.to_path_buf(),
            |directory| workdir.join(directory),
        );
        if engram_network_path(&directory.to_string_lossy()) {
            return Err(Refusal::NetworkDirectory);
        }
        let directory = fs::canonicalize(directory).map_err(|_| Refusal::DirectoryUnavailable)?;
        if !directory.is_dir() {
            return Err(Refusal::DirectoryUnavailable);
        }
        let candidate = engram_worktree_root_path(&directory);
        // Preserve ordinary session-root visibility, including a project
        // without Git. A different root needs positive repository membership.
        if candidate != session_root {
            let common = test_run_provider_git_common_dir(&session_root, None)?;
            test_run_provider_git_common_dir(&candidate, Some(&common))?;
        }
        if root.as_ref().is_some_and(|previous| previous != &candidate) {
            return Err(Refusal::AmbiguousWorktree);
        }
        // Preserve all original argument/shell/path guards. Factual placement
        // cannot make a command testing another input publishable.
        if engram_check_worktree_in(
            check,
            &candidate,
            Some(&test_run_display_path(&directory)),
            None,
        )
        .is_none()
        {
            return Err(Refusal::CommandTargetUnconfirmed);
        }
        root = Some(candidate);
    }
    root.map(|root| test_run_display_path(&root))
        .ok_or(Refusal::DirectoryUnknown)
}
