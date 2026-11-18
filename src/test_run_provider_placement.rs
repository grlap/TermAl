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

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct TestRunProviderGitPlacement {
    root: PathBuf,
    common: PathBuf,
    admin: PathBuf,
}

struct TestRunProviderPlacement {
    worktree: String,
    git: Option<TestRunProviderGitPlacement>,
}

impl TestRunProviderPlacement {
    fn for_candidate(
        root: &FsPath,
        session_root: &FsPath,
    ) -> Result<Self, TestRunPlacementRefusal> {
        let git = if root == session_root {
            // Preserve visibility for ordinary session roots without Git.
            test_run_provider_git_placement(root, None, None).ok()
        } else {
            let session = test_run_provider_git_placement(session_root, None, None)?;
            Some(test_run_provider_git_placement(
                root,
                Some(&session.common),
                None,
            )?)
        };
        Ok(Self {
            worktree: test_run_display_path(root),
            git,
        })
    }
}

// Refuse a nested unusable Git marker without changing the shared writer's
// classification of a directory inside an enclosing checkout.
fn test_run_provider_root(directory: &FsPath) -> &FsPath {
    directory
        .ancestors()
        .find(|ancestor| fs::symlink_metadata(ancestor.join(".git")).is_ok())
        .unwrap_or(directory)
}

fn test_run_provider_git_placement(
    root: &FsPath,
    expected_common: Option<&FsPath>,
    expected_admin: Option<&FsPath>,
) -> Result<TestRunProviderGitPlacement, TestRunPlacementRefusal> {
    use TestRunPlacementRefusal as Refusal;
    if engram_network_path(&root.to_string_lossy()) {
        return Err(Refusal::NetworkDirectory);
    }
    identity_no_links(root).map_err(|_| Refusal::RepositoryUnavailable)?;
    let marker = root.join(".git");
    let marker_kind = fs::symlink_metadata(&marker).map_err(|_| Refusal::RepositoryUnavailable)?;
    identity_plain_metadata(&marker_kind).map_err(|_| Refusal::RepositoryUnavailable)?;
    if marker_kind.is_dir() {
        if expected_common.is_some_and(|expected| !test_run_same_git_path(&marker, expected)) {
            return Err(Refusal::ForeignRepository);
        }
        if expected_admin.is_some_and(|expected| !test_run_same_git_path(&marker, expected)) {
            return Err(Refusal::UnregisteredWorktree);
        }
        return Ok(TestRunProviderGitPlacement {
            root: root.to_path_buf(),
            common: marker.clone(),
            admin: marker,
        });
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
    if expected_admin.is_some_and(|expected| !test_run_same_git_path(&admin, expected)) {
        return Err(Refusal::UnregisteredWorktree);
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
    Ok(TestRunProviderGitPlacement {
        root: root.to_path_buf(),
        common: common.to_path_buf(),
        admin,
    })
}

fn test_run_provider_worktree(
    check: &EngramCheckCommand,
    workdir: &FsPath,
    directories: Option<&[Option<String>]>,
) -> Result<TestRunProviderPlacement, TestRunPlacementRefusal> {
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
        let candidate = test_run_provider_root(&directory).to_path_buf();
        let placement = TestRunProviderPlacement::for_candidate(&candidate, &session_root)?;
        if root
            .as_ref()
            .is_some_and(|previous: &TestRunProviderPlacement| {
                previous.worktree != placement.worktree || previous.git != placement.git
            })
        {
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
        root = Some(placement);
    }
    root.ok_or(Refusal::DirectoryUnknown)
}

fn test_run_provider_artifact_directory(
    root: &FsPath,
    start: Option<&TestRunProviderGitPlacement>,
) -> Option<PathBuf> {
    // Older rows without a captured boundary may resolve an ordinary .git
    // directory in their own root. They cannot follow a linked marker.
    let root = start.map_or(root, |start| start.root.as_path());
    let marker = root.join(".git");
    let common = start.map_or(marker.as_path(), |start| &start.common);
    let admin = start.map_or(marker.as_path(), |start| &start.admin);
    let placement = test_run_provider_git_placement(root, Some(common), Some(admin)).ok()?;
    Some(placement.admin.join("review-runs"))
}
