// Git status reads and submodule file summaries. Replaces the v1 parser
// formerly in git.rs with NUL-delimited porcelain v2. Diff previews and
// mutations stay in git.rs.

const GIT_SUBMODULE_FILE_LIMIT: usize = 100;

#[derive(Default)]
pub(crate) struct ParsedGitBranchStatus {
    pub(crate) ahead: usize,
    pub(crate) behind: usize,
    pub(crate) branch: Option<String>,
    pub(crate) upstream: Option<String>,
    pub(crate) initial: bool,
}

pub(crate) struct ParsedGitStatusEntry {
    pub(crate) file: GitStatusFile,
    pub(crate) gitlink: Option<GitlinkChange>,
    path_is_utf8: bool,
}

#[derive(Default)]
pub(crate) struct GitlinkChange {
    pub(crate) head: Option<String>,
    pub(crate) index: Option<String>,
    pub(crate) staged: bool,
    pub(crate) unstaged: bool,
    staged_pointer: bool,
    worktree_pointer: bool,
    pub(crate) dirt: GitlinkDirt,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct GitlinkDirt {
    pub(crate) modified: bool,
    pub(crate) untracked: bool,
}

impl GitlinkDirt {
    pub(crate) fn any(self) -> bool {
        self.modified || self.untracked
    }
}

pub(crate) fn load_git_status_for_path(path: &FsPath) -> Result<GitStatusResponse, ApiError> {
    let workdir = normalize_git_workdir_path(path)?;
    let repo_root = resolve_git_repo_root(&workdir)?;
    let mut branch = ParsedGitBranchStatus::default();
    let mut files = Vec::new();
    if let Some(repo_root) = repo_root.as_deref() {
        let output = git_command()
            .arg("--no-optional-locks")
            .arg("-C")
            .arg(repo_root)
            .args(["status", "--porcelain=v2", "-z", "--branch", "-uall"])
            .output()
            .map_err(|err| ApiError::internal(format!("failed to run git status: {err}")))?;
        if !output.status.success() {
            return Err(ApiError::internal(format!(
                "git status failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }

        let (parsed_branch, entries) = parse_git_status(&output.stdout);
        branch = parsed_branch;
        // The porcelain sentinel is also a legal literal branch name.
        if branch.branch.as_deref() == Some("(detached)")
            && git_command()
                .arg("-C")
                .arg(repo_root)
                .args(["symbolic-ref", "--quiet", "HEAD"])
                .output()
                .is_ok_and(|output| output.status.code() == Some(1))
        {
            branch.branch = Some("HEAD (no branch)".to_owned());
        }
        files = entries
            .into_iter()
            .map(
                |ParsedGitStatusEntry {
                     mut file,
                     gitlink,
                     path_is_utf8,
                 }| {
                    if let Some(gitlink) = gitlink {
                        let path = path_is_utf8.then_some(file.path.as_str());
                        file.submodule = Some(read_git_submodule_status(repo_root, path, gitlink));
                    }
                    file
                },
            )
            .collect();
    }

    Ok(GitStatusResponse {
        ahead: branch.ahead,
        behind: branch.behind,
        branch: branch.branch,
        is_clean: files.is_empty(),
        files,
        repo_root: repo_root.map(|root| root.to_string_lossy().into_owned()),
        upstream: branch.upstream,
        workdir: workdir.to_string_lossy().into_owned(),
    })
}

pub(crate) fn parse_git_status(output: &[u8]) -> (ParsedGitBranchStatus, Vec<ParsedGitStatusEntry>) {
    let mut branch = ParsedGitBranchStatus::default();
    let mut entries = Vec::new();
    let mut records = output.split(|byte| *byte == 0);
    while let Some(raw) = records.next() {
        let record = String::from_utf8_lossy(raw);
        if let Some(header) = record.strip_prefix("# ") {
            branch.read_header(header);
            continue;
        }

        // In porcelain v2 -z, a rename/copy gives the destination followed
        // by a separate NUL-delimited source path.
        let original_path = if raw.starts_with(b"2 ") {
            records
                .next()
                .map(|path| String::from_utf8_lossy(path).into_owned())
        } else {
            None
        };
        if let Some(mut entry) = parse_git_status_entry(&record, original_path) {
            // A lossy display name cannot safely address a checkout.
            entry.path_is_utf8 = matches!(record, std::borrow::Cow::Borrowed(_));
            entries.push(entry);
        }
    }
    if branch.initial {
        branch.branch = branch
            .branch
            .map(|name| format!("No commits yet on {name}"));
    }
    (branch, entries)
}

impl ParsedGitBranchStatus {
    fn read_header(&mut self, header: &str) {
        let Some((key, value)) = header.split_once(' ') else {
            return;
        };
        match key {
            "branch.oid" => self.initial = value == "(initial)",
            "branch.head" => self.branch = Some(value.to_owned()),
            "branch.upstream" => self.upstream = Some(value.to_owned()),
            "branch.ab" => {
                if let Some((ahead, behind)) = value.split_once(' ') {
                    self.ahead = ahead
                        .strip_prefix('+')
                        .and_then(|count| count.parse().ok())
                        .unwrap_or(0);
                    self.behind = behind
                        .strip_prefix('-')
                        .and_then(|count| count.parse().ok())
                        .unwrap_or(0);
                }
            }
            _ => {}
        }
    }
}

fn parse_git_status_entry(
    record: &str,
    original_path: Option<String>,
) -> Option<ParsedGitStatusEntry> {
    let (kind, rest) = record.split_once(' ')?;
    let field_count = match kind {
        "1" => 8,
        "2" => 9,
        "u" => 10,
        "?" => return Some(git_status_entry("??", rest, None, None)),
        _ => return None,
    };
    let fields: Vec<_> = rest.splitn(field_count, ' ').collect();
    match fields.as_slice() {
        [
            xy,
            sub,
            head_mode,
            index_mode,
            worktree_mode,
            head,
            index,
            ..,
            path,
        ] if kind != "u" && fields.len() == field_count => {
            let gitlink =
                git_status_has_gitlink(&[head_mode, index_mode, worktree_mode]).then(|| {
                    let old = git_mode_is_submodule(head_mode).then_some(*head);
                    let new = git_mode_is_submodule(index_mode).then_some(*index);
                    GitlinkChange {
                        head: old.map(str::to_owned),
                        index: new.map(str::to_owned),
                        staged: old.is_some() || new.is_some(),
                        unstaged: new.is_some() || git_mode_is_submodule(worktree_mode),
                        staged_pointer: old != new,
                        ..gitlink_dirty_state(sub)
                    }
                });
            Some(git_status_entry(xy, path, original_path, gitlink))
        }
        [xy, sub, mode1, mode2, mode3, worktree_mode, _, _, _, path] if kind == "u" => {
            let gitlink =
                git_status_has_gitlink(&[mode1, mode2, mode3, worktree_mode]).then(|| {
                    GitlinkChange {
                        staged: true,
                        unstaged: true,
                        staged_pointer: true,
                        ..gitlink_dirty_state(sub)
                    }
                });
            Some(git_status_entry(xy, path, original_path, gitlink))
        }
        _ => None,
    }
}

fn git_status_has_gitlink(modes: &[&str]) -> bool {
    modes.iter().any(|mode| git_mode_is_submodule(mode))
}

fn gitlink_dirty_state(sub: &str) -> GitlinkChange {
    let bytes = sub.as_bytes();
    GitlinkChange {
        worktree_pointer: bytes.get(1) == Some(&b'C'),
        dirt: GitlinkDirt {
            modified: bytes.get(2) == Some(&b'M'),
            untracked: bytes.get(3) == Some(&b'U'),
        },
        ..GitlinkChange::default()
    }
}

fn git_status_entry(
    xy: &str,
    path: &str,
    original_path: Option<String>,
    gitlink: Option<GitlinkChange>,
) -> ParsedGitStatusEntry {
    let mut codes = xy.chars();
    ParsedGitStatusEntry {
        file: GitStatusFile {
            index_status: codes.next().and_then(normalize_git_status_code),
            worktree_status: codes.next().and_then(normalize_git_status_code),
            original_path,
            path: path.to_owned(),
            submodule: None,
        },
        gitlink,
        path_is_utf8: true,
    }
}

fn pending_git_submodule_status(gitlink: &GitlinkChange) -> GitSubmoduleStatus {
    GitSubmoduleStatus {
        staged: gitlink.staged,
        unstaged: gitlink.unstaged,
        staged_commits: gitlink
            .staged_pointer
            .then_some(GitSubmoduleFiles::Unavailable),
        unstaged_commits: gitlink
            .worktree_pointer
            .then_some(GitSubmoduleFiles::Unavailable),
        modified: gitlink
            .dirt
            .modified
            .then_some(GitSubmoduleFiles::Unavailable),
        untracked: gitlink
            .dirt
            .untracked
            .then_some(GitSubmoduleFiles::Unavailable),
        ..GitSubmoduleStatus::default()
    }
}

fn read_git_submodule_status(
    parent: &FsPath,
    path: Option<&str>,
    gitlink: GitlinkChange,
) -> GitSubmoduleStatus {
    let mut status = pending_git_submodule_status(&gitlink);
    let Some(path) = path else {
        status.checkout = GitSubmoduleCheckout::Unavailable;
        return status;
    };
    let child = match resolve_git_submodule_checkout(parent, FsPath::new(path)) {
        Ok(child) => child,
        Err(issue) => {
            status.checkout = issue;
            return status;
        }
    };
    if gitlink.staged_pointer {
        status.staged_commits = Some(read_git_submodule_commit_files(
            &child,
            gitlink.head.as_deref(),
            gitlink.index.as_deref(),
        ));
    }
    if gitlink.worktree_pointer {
        if let Some(head) = git_submodule_checkout_head(&child) {
            status.unstaged_commits = Some(read_git_submodule_commit_files(
                &child,
                gitlink.index.as_deref(),
                Some(&head),
            ));
        }
    }
    if gitlink.dirt.any() {
        let mut files = GitSubmoduleLocalFiles::default();
        files.read(&child, "", gitlink.dirt);
        status.modified = gitlink.dirt.modified.then(|| files.modified.into_group());
        status.untracked = gitlink.dirt.untracked.then(|| files.untracked.into_group());
    }
    status
}

#[derive(Default)]
struct GitSubmoduleLocalFiles {
    modified: GitSubmoduleFileList,
    untracked: GitSubmoduleFileList,
}

impl GitSubmoduleLocalFiles {
    fn mark_incomplete(&mut self, dirt: GitlinkDirt) {
        self.modified.incomplete |= dirt.modified;
        self.untracked.incomplete |= dirt.untracked;
    }

    fn read(&mut self, child: &FsPath, prefix: &str, dirt: GitlinkDirt) {
        let output = git_submodule_read_command(child)
            .args(["status", "--porcelain=v2", "-z", "-uall"])
            .output();
        let Ok(output) = output else {
            self.mark_incomplete(dirt);
            return;
        };
        if !output.status.success() {
            self.mark_incomplete(dirt);
            return;
        }

        let (_, entries) = parse_git_status(&output.stdout);
        for ParsedGitStatusEntry {
            file,
            gitlink,
            path_is_utf8,
        } in entries
        {
            let path = format!("{prefix}{}", file.path);
            if let Some(gitlink) = gitlink {
                // An unstaged M may be Git propagating dirt from the
                // nested checkout. Report the gitlink itself only when
                // the entry changed independently of that dirt.
                if dirt.modified
                    && (file.index_status.is_some()
                        || file.worktree_status.as_deref() != Some("M")
                        || gitlink.worktree_pointer
                        || !gitlink.dirt.any())
                {
                    self.modified.push(path.clone());
                }

                let nested_dirt = GitlinkDirt {
                    modified: dirt.modified && gitlink.dirt.modified,
                    untracked: dirt.untracked && gitlink.dirt.untracked,
                };
                if nested_dirt.any() {
                    if path_is_utf8
                        && let Ok(nested) =
                            resolve_git_submodule_checkout(child, FsPath::new(&file.path))
                    {
                        self.read(&nested, &format!("{path}/"), nested_dirt);
                    } else {
                        self.mark_incomplete(nested_dirt);
                    }
                }
            } else if file.index_status.as_deref() == Some("?") {
                if dirt.untracked {
                    self.untracked.push(path);
                }
            } else if dirt.modified {
                self.modified.push(path);
            }
        }
    }
}

#[derive(Default)]
struct GitSubmoduleFileList {
    paths: Vec<String>,
    remaining: usize,
    incomplete: bool,
}

impl GitSubmoduleFileList {
    fn push(&mut self, path: String) {
        if self.paths.len() < GIT_SUBMODULE_FILE_LIMIT {
            self.paths.push(path);
        } else {
            self.remaining += 1;
        }
    }

    fn into_group(self) -> GitSubmoduleFiles {
        if self.incomplete && self.paths.is_empty() {
            return GitSubmoduleFiles::Unavailable;
        }
        GitSubmoduleFiles::Listed {
            paths: self.paths,
            remaining: self.remaining,
            incomplete: self.incomplete,
        }
    }
}

fn resolve_git_submodule_checkout(
    parent: &FsPath,
    path: &FsPath,
) -> Result<PathBuf, GitSubmoduleCheckout> {
    if path.as_os_str().is_empty()
        || !path
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
    {
        return Err(GitSubmoduleCheckout::Unavailable);
    }
    let parent = fs::canonicalize(parent).map_err(|_| GitSubmoduleCheckout::Unavailable)?;
    let child = match fs::canonicalize(parent.join(path)) {
        Ok(child) if child != parent && child.starts_with(&parent) => child,
        Ok(_) => return Err(GitSubmoduleCheckout::Unavailable),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(GitSubmoduleCheckout::NotCheckedOut);
        }
        Err(_) => return Err(GitSubmoduleCheckout::Unavailable),
    };
    // An uninitialized path may exist as an empty directory. Without its
    // own Git metadata, rev-parse could discover the superproject.
    if !child.join(".git").exists() {
        return Err(GitSubmoduleCheckout::NotCheckedOut);
    }
    let root = resolve_git_repo_root(&child)
        .map_err(|_| GitSubmoduleCheckout::Unavailable)?
        .and_then(|root| fs::canonicalize(root).ok());
    if root.as_deref() != Some(child.as_path()) {
        return Err(GitSubmoduleCheckout::NotCheckedOut);
    }
    Ok(child)
}

pub(crate) fn git_submodule_read_command(child: &FsPath) -> Command {
    let mut command = git_command();
    command
        .env("GIT_NO_LAZY_FETCH", "1")
        .args(["--no-optional-locks", "-c", "core.fsmonitor=false", "-C"])
        .arg(child);
    command
}

pub(crate) fn git_submodule_checkout_head(child: &FsPath) -> Option<String> {
    let output = git_submodule_read_command(child)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .ok()?;
    let oid = std::str::from_utf8(&output.stdout).ok()?.trim();
    (output.status.success()
        && matches!(oid.len(), 40 | 64)
        && oid.bytes().all(|byte| byte.is_ascii_hexdigit()))
    .then(|| oid.to_owned())
}

pub(crate) fn read_git_submodule_commit_files(
    child: &FsPath,
    old: Option<&str>,
    new: Option<&str>,
) -> GitSubmoduleFiles {
    let mut command = git_submodule_read_command(child);
    let comparing = match (old, new) {
        (Some(old), Some(new)) => {
            command.args([
                "diff",
                "--name-status",
                "-z",
                "--no-renames",
                "--no-ext-diff",
                "--no-textconv",
                "--ignore-submodules=none",
                old,
                new,
                "--",
            ]);
            true
        }
        // On a gitlink add or delete there is one endpoint. Every file
        // from that endpoint was added or removed with the gitlink.
        (Some(commit), None) | (None, Some(commit)) => {
            command.args(["ls-tree", "-r", "--name-only", "-z", commit, "--"]);
            false
        }
        (None, None) => return GitSubmoduleFiles::Unavailable,
    };
    if let Ok(output) = command.output()
        && output.status.success()
    {
        let paths = output.stdout.split(|byte| *byte == 0);
        // --name-status -z emits (status, path) pairs with renames disabled.
        return if comparing {
            git_submodule_file_list(
                paths
                    .skip(1)
                    .step_by(2)
                    .filter(|path| !path.is_empty())
                    .map(|path| String::from_utf8_lossy(path).into_owned()),
            )
        } else {
            git_submodule_file_list(
                paths
                    .filter(|path| !path.is_empty())
                    .map(|path| String::from_utf8_lossy(path).into_owned()),
            )
        };
    }

    match find_missing_gitlink_commit(child, old, new) {
        Some(commit) => GitSubmoduleFiles::MissingCommit { commit },
        None => GitSubmoduleFiles::Unavailable,
    }
}

fn find_missing_gitlink_commit(
    child: &FsPath,
    old: Option<&str>,
    new: Option<&str>,
) -> Option<String> {
    // A failed diff could mean an unreadable tree or an I/O error.
    // Only a successful object query with an explicit missing reply
    // identifies a missing endpoint.
    let commits: Vec<_> = [old, new].into_iter().flatten().collect();
    let input = format!("{}\n", commits.join("\n"));
    let mut command = git_submodule_read_command(child)
        .args(["cat-file", "--batch-check=%(objecttype)"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let sent = command
        .stdin
        .take()
        .is_some_and(|mut stdin| std::io::Write::write_all(&mut stdin, input.as_bytes()).is_ok());
    let output = command.wait_with_output().ok()?;
    if !sent || !output.status.success() || !output.stderr.is_empty() {
        return None;
    }
    let stdout = std::str::from_utf8(&output.stdout).ok()?;
    commits
        .into_iter()
        .zip(stdout.lines())
        .find(|(commit, reply)| reply.strip_suffix(" missing") == Some(*commit))
        .map(|(commit, _)| commit.to_owned())
}

fn git_submodule_file_list(paths: impl Iterator<Item = String>) -> GitSubmoduleFiles {
    let mut files = GitSubmoduleFileList::default();
    paths.for_each(|path| files.push(path));
    files.into_group()
}
