// Owns the host's switch for the ordinary committed-input flow
// (`ORDINARY_COMMIT_INPUT_DEFAULT`) and the source revision it selects: with
// the switch on, a clean worktree root has the revision `git-commit:<HEAD>`
// and one that is not clean has a typed not-clean outcome carrying the paths
// Git reported, never a revision and never a content revision instead.
// Does not own the content revision taken while the switch is off
// (`content_revision` in src/content_revision.rs), the launcher's side of the
// flow (`ORDINARY_COMMIT_INPUT_DEFAULT` and `commitInputOutcome` in
// scripts/test-launcher.mjs), the committed-input record a run writes
// (src/test_run_committed_input.rs), or when a basis is taken and what its
// absence means (src/engram_source_roots.rs, src/engram_turn_observations.rs).
// New module. Reuses review_freeze.rs's pinned Git runner.

/// Whether ordinary host source measurements use committed input. The
/// launcher's same-named switch selects run input independently; enabling it
/// does not enable this host default. While this is off, ordinary host source
/// measurements retain content revisions.
const ORDINARY_COMMIT_INPUT_DEFAULT: bool = false;

/// The scheme of a committed source revision. It never equals a content
/// revision's (`content-v1`), so a mix is visible in the string.
const COMMITTED_SOURCE_REVISION_SCHEME: &str = "git-commit";

/// At most this many dirty paths are named in a log line; the outcome itself
/// keeps them all.
const SOURCE_REVISION_NOT_CLEAN_SHOWN: usize = 8;

/// One entry `git status --porcelain=v1 -z` reported on a root that is not
/// clean: its two status letters and its path, and for a rename or copy the
/// path it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceRevisionDirtyPath {
    status: String,
    path: String,
    origin: Option<String>,
}

/// The source revision of a worktree root: a revision with the canonical root
/// it was taken on, or, with the switch on, the paths that keep it from one.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SourceRevision {
    /// A revision: a content revision with the switch off, `git-commit:<HEAD>`
    /// with it on.
    Taken { root: PathBuf, revision: String },
    /// With the switch on, a root Git reports changes in, tracked or
    /// untracked and not ignored. It has no revision.
    NotClean {
        dirty_paths: Vec<SourceRevisionDirtyPath>,
    },
}

/// The source revision of the worktree at `root`, which must be a worktree
/// root. With `ordinary_commit_input` off it is exactly [`content_revision`].
/// With it on, it is the commit HEAD names when Git reports the worktree clean
/// (`status` with every untracked file listed, paths the repository-scoped
/// standard excludes ignore left out, and submodules never ignored), else
/// [`SourceRevision::NotClean`].
/// Fails on a non-root checkout, filters, an unreadable or non-SHA-1 HEAD,
/// an uninitialized or off-pin submodule, and HEAD movement. Git status is
/// the dirt verdict: assume-unchanged and skip-worktree edits can be invisible.
fn source_revision(root: &FsPath, ordinary_commit_input: bool) -> Result<SourceRevision> {
    if !ordinary_commit_input {
        let (root, revision) = content_revision(root)?;
        return Ok(SourceRevision::Taken { root, revision });
    }
    let git = ReviewFreezeGit::new(root)?;
    capture_committed_source_revision(&git)
}

fn capture_committed_source_revision(git: &ReviewFreezeGit) -> Result<SourceRevision> {
    let (head, dirty_paths) = committed_source_status(git)?;
    if !dirty_paths.is_empty() {
        return Ok(SourceRevision::NotClean { dirty_paths });
    }
    Ok(SourceRevision::Taken {
        root: git.root.clone(),
        revision: format!("{COMMITTED_SOURCE_REVISION_SCHEME}:{head}"),
    })
}

/// HEAD when recursive submodules are initialized at pin, filters are absent,
/// root status reports no tracked or untracked nonignored dirt, and HEAD stays
/// unchanged. No full file or index listing is derived. Hidden index edits
/// remain Git status's documented visibility limitation.
/// Acceptance uses the same predicate under its existing shared deadline.
#[cfg(test)]
fn committed_source_clean_head(root: &FsPath) -> Option<String> {
    committed_source_clean_head_within(root, None)
}

/// [`committed_source_clean_head`] with every Git read under `deadline`
/// (none: Git's own default bound); None once it passes.
fn committed_source_clean_head_within(
    root: &FsPath,
    deadline: Option<std::time::Instant>,
) -> Option<String> {
    let git = ReviewFreezeGit::new_with_deadline(root, deadline).ok()?;
    let (head, dirty_paths) = committed_source_status(&git).ok()?;
    dirty_paths.is_empty().then_some(head)
}

/// Root HEAD and Git's root dirt diagnostics, guarded by recursive submodule
/// pin status and root/child filter configuration.
fn committed_source_status(
    git: &ReviewFreezeGit,
) -> Result<(String, Vec<SourceRevisionDirtyPath>)> {
    let head = committed_source_head(git)?;
    committed_source_submodules(git)?;
    let status = git.run(
        &[
            // Unlike -u, this override reaches recursive child status too.
            "-c",
            "status.showUntrackedFiles=all",
            "-c",
            "diff.ignoreSubmodules=none",
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignore-submodules=none",
            "-z",
        ],
        false,
    )?;
    let dirty_paths = source_revision_dirty_paths(&status)?;
    // A commit, checkout or reset between the two reads would pair one
    // HEAD with another HEAD's status.
    if committed_source_head(git)? != head {
        bail!("HEAD moved while the committed source revision was taken");
    }
    Ok((head, dirty_paths))
}

/// Git owns initialization and pin judgment, including nested children. Only
/// child paths from its bounded status output are retained, never a file list.
fn committed_source_submodules(git: &ReviewFreezeGit) -> Result<()> {
    let status = git.run(&["submodule", "status", "--recursive"], false)
        .context("submodule initialization/pin or .gitmodules mapping check failed")?;
    let status = String::from_utf8(status)?;
    for row in status.lines() {
        if row.is_empty() { continue; }
        if !row.starts_with(' ') {
            bail!("submodule is not initialized at its pin (git submodule status --recursive)");
        }
        if row.len() < 43 || row.as_bytes()[41] != b' '
            || !row.get(1..41).is_some_and(is_lowercase_hex) {
            bail!("malformed git submodule status row");
        }
        let tail = &row[42..];
        // Git appends an optional ` (describe)` suffix after the path.
        let path = if tail.ends_with(')') {
            tail.rsplit_once(" (").map_or(tail, |(path, _)| path)
        } else { tail };
        let root = fs::canonicalize(git.root.join(path))
            .with_context(|| format!("submodule {path} is not initialized"))?;
        if root == git.root || !root.starts_with(&git.root) {
            bail!("submodule {path} is outside its worktree root");
        }
        let child = ReviewFreezeGit {
            root,
            binary: git.binary.clone(),
            deadline: git.deadline,
        };
        let filters = child.run(
            &["config", "--null", "--get-regexp", r"^filter\..*\.(clean|smudge|process)$"],
            true,
        )?;
        if !filters.is_empty() {
            bail!("submodule {path} has a configured Git filter");
        }
    }
    Ok(())
}

/// The full SHA-1 commit id supported by the registered Commit wire.
fn committed_source_head(git: &ReviewFreezeGit) -> Result<String> {
    let output = git.run(&["rev-parse", "--verify", "HEAD^{commit}"], false)?;
    let head = String::from_utf8(output)
        .context("HEAD is not UTF-8")?
        .trim()
        .to_owned();
    if !(head.len() == 40 && is_lowercase_hex(&head)) {
        bail!("HEAD is not a full SHA-1 commit id (unsupported object format): {head:?}");
    }
    Ok(head)
}

/// The entries of `git status --porcelain=v1 -z`: "XY PATH", and after a
/// rename or copy one more field holding the path it came from.
fn source_revision_dirty_paths(status: &[u8]) -> Result<Vec<SourceRevisionDirtyPath>> {
    let mut fields = status.split(|b| *b == 0).filter(|f| !f.is_empty());
    let mut paths = Vec::new();
    while let Some(entry) = fields.next() {
        if entry.len() < 4 || entry[2] != b' ' {
            bail!("malformed status entry in a committed source revision");
        }
        let letters = &entry[..2];
        let origin = if letters.iter().any(|b| matches!(b, b'R' | b'C')) {
            let origin = fields
                .next()
                .context("a renamed or copied status entry without its origin")?;
            Some(String::from_utf8_lossy(origin).into_owned())
        } else {
            None
        };
        paths.push(SourceRevisionDirtyPath {
            status: String::from_utf8_lossy(letters).into_owned(),
            path: String::from_utf8_lossy(&entry[3..]).into_owned(),
            origin,
        });
    }
    Ok(paths)
}

/// Why a not-clean root has no revision, for a log line: the first few paths
/// and how many more there are.
fn source_revision_not_clean_reason(dirty_paths: &[SourceRevisionDirtyPath]) -> String {
    let shown = dirty_paths
        .iter()
        .take(SOURCE_REVISION_NOT_CLEAN_SHOWN)
        .map(|entry| match &entry.origin {
            Some(origin) => format!("{} {} from {origin}", entry.status, entry.path),
            None => format!("{} {}", entry.status, entry.path),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let more = dirty_paths
        .len()
        .saturating_sub(SOURCE_REVISION_NOT_CLEAN_SHOWN);
    if more > 0 {
        format!("the worktree is not clean ({shown}, and {more} more)")
    } else {
        format!("the worktree is not clean ({shown})")
    }
}
