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
            let common = engram_source_root_common_dir(&session_root)
                .ok_or(Refusal::RepositoryUnavailable)?;
            let candidate_common =
                engram_source_root_common_dir(&candidate).ok_or(Refusal::RepositoryUnavailable)?;
            if candidate_common != common {
                return Err(Refusal::ForeignRepository);
            }
            if !engram_source_root_registered(&candidate, &common) {
                return Err(Refusal::UnregisteredWorktree);
            }
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
