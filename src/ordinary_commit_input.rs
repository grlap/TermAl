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
/// Fails, leaving the caller without a basis, where the content revision
/// would fail to start (not a worktree root, configured Git filters), on a
/// repository with no commit, when HEAD moves while it is taken, and when the
/// index tells Git to skip a tracked file's worktree copy (assume-unchanged
/// or skip-worktree), since Git would then call an edited file clean, and when
/// the index holds a submodule gitlink, since status could run a filter the
/// submodule configures.
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

/// The commit the worktree at `root` is clean at, under the one rule every
/// committed-input reader shares: HEAD, when the index tells Git to skip no
/// tracked path's worktree copy, `status` with every untracked file listed,
/// paths the repository-scoped standard excludes ignore left out and
/// submodules never ignored reports nothing, and HEAD did not move while that
/// was read. The pinned runner points `core.excludesFile` at the null device,
/// as the launcher's `captureCommitInputStatus` does, so the ignore sources
/// are .gitignore and .git/info/exclude. None for a dirty
/// tree, such an index, an index with a submodule gitlink, an unborn HEAD, or
/// a tree Git cannot read. The
/// acceptance evaluation's committed-input admission
/// (src/acceptance_committed_input.rs) judges its named root with it, under
/// the request's deadline ([`committed_source_clean_head_within`]).
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

/// HEAD and the paths Git reports changed against it, read as
/// [`source_revision`] describes. Fails on an index that tells Git to skip a
/// tracked path's worktree copy or holds a submodule gitlink, on a HEAD that
/// is not a full commit id, and
/// when HEAD moves between the reads.
fn committed_source_status(
    git: &ReviewFreezeGit,
) -> Result<(String, Vec<SourceRevisionDirtyPath>)> {
    let head = committed_source_head(git)?;
    // `-v` tags an assume-unchanged entry with a lowercase letter and a
    // skip-worktree one with `S`. Status does not look at such a file's
    // worktree copy, so an edit there would read as clean.
    let tagged = git.run(&["ls-files", "-z", "-v"], false)?;
    for entry in tagged.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        if entry[0].is_ascii_lowercase() || entry[0] == b'S' {
            bail!(
                "a committed source revision does not cover a tracked path Git is told to \
                 skip (assume-unchanged or skip-worktree)"
            );
        }
    }
    // Status can run a clean or process filter from a submodule's own config
    // while it checks that submodule, outside the runner's filter check. As
    // `capture_review_freeze` does, refuse any gitlink first, from index
    // metadata alone (`ls-files --stage` runs no filter).
    let staged = git.run(&["ls-files", "--stage", "-z"], false)?;
    let gitlinks = staged
        .split(|b| *b == 0)
        .filter(|entry| entry.starts_with(b"160000 "))
        .map(|entry| {
            let path = entry.splitn(2, |b| *b == b'\t').nth(1).unwrap_or_default();
            String::from_utf8_lossy(path).into_owned()
        })
        .collect::<Vec<_>>();
    if !gitlinks.is_empty() {
        bail!(
            "a committed source revision does not inspect submodules (Git gitlinks are \
             unsupported): {}",
            gitlinks.join(", ")
        );
    }
    let status = git.run(
        &[
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

/// The full id of the commit HEAD names: 40 hex digits, or 64 in a SHA-256
/// repository.
fn committed_source_head(git: &ReviewFreezeGit) -> Result<String> {
    let output = git.run(&["rev-parse", "--verify", "HEAD^{commit}"], false)?;
    let head = String::from_utf8(output)
        .context("HEAD is not UTF-8")?
        .trim()
        .to_owned();
    if !(matches!(head.len(), 40 | 64) && is_lowercase_hex(&head)) {
        bail!("HEAD is not a full commit id: {head:?}");
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
