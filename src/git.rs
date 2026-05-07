// Git operations — diff loading, status parsing, document readers,
// commit/push/sync orchestration, and worktree/index I/O.
//
// These are the helpers behind the `/api/git/...` endpoints; the HTTP
// handlers (`read_git_status`, `read_git_diff`, `apply_git_file_action`,
// `commit_git_changes`, `push_git_changes`, `sync_git_changes`) stay in
// api.rs, along with the low-level status-code parsing in the 7000s block.
//
// Extracted from api.rs into its own `include!()` fragment so HTTP handler
// code and git-command plumbing live in separate files.

/// Loads Git diff for request.
fn load_git_diff_for_request(
    workdir: &FsPath,
    request: &GitDiffRequest,
) -> Result<GitDiffResponse, ApiError> {
    load_git_diff_for_request_with_document_loader(
        workdir,
        request,
        load_git_diff_document_content,
    )
}

fn load_git_diff_for_request_with_document_loader<F>(
    workdir: &FsPath,
    request: &GitDiffRequest,
    load_document_content: F,
) -> Result<GitDiffResponse, ApiError>
where
    F: Fn(
        &FsPath,
        &str,
        Option<&str>,
        Option<&str>,
        GitDiffSection,
    ) -> Result<GitDiffDocumentContent, ApiError>,
{
    let workdir = normalize_git_workdir_path(workdir)?;
    let Some(repo_root) = resolve_git_repo_root(&workdir)? else {
        return Err(ApiError::bad_request("no git repository found"));
    };

    let current_path = normalize_git_repo_relative_path(&request.path)?;
    let original_path = request
        .original_path
        .as_deref()
        .map(normalize_git_repo_relative_path)
        .transpose()?;
    let status_code = request
        .status_code
        .as_deref()
        .and_then(|value| value.chars().next())
        .and_then(normalize_git_status_code);
    let diff = load_git_file_diff_text(
        &repo_root,
        &current_path,
        original_path.as_deref(),
        status_code.as_deref(),
        request.section_id,
    )?;

    if diff.trim().is_empty() {
        return Err(ApiError::bad_request(format!(
            "no diff available for {}",
            current_path
        )));
    }

    let file_path = repo_root.join(&current_path);
    let diff_identity = [
        repo_root.to_string_lossy().as_ref(),
        request.section_id.as_key(),
        current_path.as_str(),
        original_path.as_deref().unwrap_or(""),
        diff.as_str(),
    ]
    .join("\n");

    let diff_hash = stable_text_hash(&diff_identity);
    let language = infer_language_from_path(FsPath::new(&current_path)).map(str::to_owned);
    let mut document_enrichment_note = None;
    let document_content = if language.as_deref() == Some("markdown") {
        match load_document_content(
            &repo_root,
            &current_path,
            original_path.as_deref(),
            status_code.as_deref(),
            request.section_id,
        ) {
            Ok(content) => Some(content),
            Err(error) if should_degrade_git_diff_document_enrichment_error(&error) => {
                document_enrichment_note = git_diff_document_enrichment_note(&error);
                eprintln!(
                    "backend warning> git diff Markdown enrichment skipped for {}: {}",
                    current_path, error.message
                );
                None
            }
            Err(error) => return Err(error),
        }
    } else {
        None
    };

    Ok(GitDiffResponse {
        change_type: if matches!(status_code.as_deref(), Some("A" | "?")) {
            GitDiffChangeType::Create
        } else {
            GitDiffChangeType::Edit
        },
        change_set_id: format!("git-diff-{diff_hash}"),
        diff,
        diff_id: format!("git:{diff_hash}"),
        file_path: file_path
            .exists()
            .then(|| file_path.to_string_lossy().into_owned()),
        language,
        document_enrichment_note,
        document_content,
        summary: format!(
            "{} changes in {}",
            request.section_id.summary_label(),
            current_path
        ),
    })
}

/// Loads Git status for path.
fn load_git_status_for_path(path: &FsPath) -> Result<GitStatusResponse, ApiError> {
    let workdir = normalize_git_workdir_path(path)?;
    let Some(repo_root) = resolve_git_repo_root(&workdir)? else {
        return Ok(GitStatusResponse {
            ahead: 0,
            behind: 0,
            branch: None,
            files: Vec::new(),
            is_clean: true,
            repo_root: None,
            upstream: None,
            workdir: workdir.to_string_lossy().into_owned(),
        });
    };

    let output = git_command()
        .arg("-C")
        .arg(&repo_root)
        .args(["status", "--porcelain=v1", "--branch", "-uall"])
        .output()
        .map_err(|err| ApiError::internal(format!("failed to run git status: {err}")))?;

    if !output.status.success() {
        return Err(ApiError::internal(format!(
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut branch = None;
    let mut upstream = None;
    let mut ahead = 0;
    let mut behind = 0;
    let mut files = Vec::new();

    for line in stdout.lines() {
        if let Some(branch_info) = line.strip_prefix("## ") {
            let parsed = parse_git_branch_status(branch_info);
            branch = parsed.branch;
            upstream = parsed.upstream;
            ahead = parsed.ahead;
            behind = parsed.behind;
            continue;
        }

        if line.len() < 3 {
            continue;
        }

        let status = &line[..2];
        let path_payload = line[3..].trim();
        let (original_path, path) = parse_git_status_paths(path_payload);
        let index_status = status.chars().next().and_then(normalize_git_status_code);
        let worktree_status = status.chars().nth(1).and_then(normalize_git_status_code);

        files.push(GitStatusFile {
            index_status,
            original_path,
            path,
            worktree_status,
        });
    }

    let is_clean = files.is_empty();

    Ok(GitStatusResponse {
        ahead,
        behind,
        branch,
        files,
        is_clean,
        repo_root: Some(repo_root.to_string_lossy().into_owned()),
        upstream,
        workdir: workdir.to_string_lossy().into_owned(),
    })
}

/// Normalizes Git working directory path.
fn normalize_git_workdir_path(path: &FsPath) -> Result<PathBuf, ApiError> {
    if path.is_dir() {
        return Ok(path.to_path_buf());
    }

    path.parent()
        .map(FsPath::to_path_buf)
        .ok_or_else(|| ApiError::bad_request("cannot inspect git status for a root file path"))
}

/// Normalizes Git repo relative path.
fn normalize_git_repo_relative_path(path: &str) -> Result<String, ApiError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request("git file path cannot be empty"));
    }

    let is_rooted_or_prefixed = FsPath::new(trimmed).components().any(|component| {
        matches!(
            component,
            std::path::Component::Prefix(_) | std::path::Component::RootDir
        )
    });
    // `\foo` is a rooted Windows path. On Unix hosts `Path::components`
    // treats it as a normal component, so keep this explicit API-level guard.
    if is_rooted_or_prefixed || trimmed.starts_with('\\') {
        return Err(ApiError::bad_request(
            "git file actions require repository-relative paths",
        ));
    }

    if trimmed.contains('\0') {
        return Err(ApiError::bad_request(
            "git file path contains invalid characters",
        ));
    }

    if trimmed
        .split(['/', '\\'])
        .any(|component| component == "..")
    {
        return Err(ApiError::bad_request(
            "git file path cannot contain parent-directory traversal",
        ));
    }

    Ok(trimmed.to_owned())
}

/// Collects Git pathspecs.
fn collect_git_pathspecs(current_path: &str, original_path: Option<&str>) -> Vec<String> {
    let mut pathspecs = Vec::new();
    if let Some(original_path) = original_path.filter(|original| *original != current_path) {
        pathspecs.push(original_path.to_owned());
    }
    pathspecs.push(current_path.to_owned());
    pathspecs
}

/// Collects pathspecs for staging a Git file action.
fn collect_git_stage_pathspecs(
    current_path: &str,
    original_path: Option<&str>,
    status_code: Option<&str>,
) -> Vec<String> {
    match status_code.and_then(|code| code.trim().chars().next()) {
        Some('C' | 'R') => collect_git_pathspecs(current_path, original_path),
        _ => vec![current_path.to_owned()],
    }
}

/// Loads Git file diff text.
fn load_git_file_diff_text(
    repo_root: &FsPath,
    current_path: &str,
    original_path: Option<&str>,
    status_code: Option<&str>,
    section_id: GitDiffSection,
) -> Result<String, ApiError> {
    if matches!(section_id, GitDiffSection::Unstaged) && status_code == Some("?") {
        return build_untracked_git_diff(repo_root, current_path);
    }

    let pathspecs = collect_git_pathspecs(current_path, original_path);
    let mut command = git_command();
    command
        .arg("-C")
        .arg(repo_root)
        .arg("diff")
        .arg("--find-renames");

    if matches!(section_id, GitDiffSection::Staged) {
        command.arg("--cached");
    }

    let output = command
        .arg("--")
        .args(&pathspecs)
        .output()
        .map_err(|err| ApiError::internal(format!("failed to load git diff: {err}")))?;

    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.is_empty() {
        Err(ApiError::internal("failed to load git diff"))
    } else {
        Err(ApiError::internal(format!(
            "failed to load git diff: {stderr}"
        )))
    }
}

/// Builds untracked Git diff.
fn build_untracked_git_diff(repo_root: &FsPath, current_path: &str) -> Result<String, ApiError> {
    // Untracked diffs share the document read cap. This keeps a new large file
    // from turning the diff endpoint into an unbounded in-memory patch builder.
    let content = read_git_worktree_bytes(repo_root, current_path)?;
    let content = String::from_utf8_lossy(&content);
    let lines: Vec<&str> = content.lines().collect();
    let mut diff_lines = vec![
        format!("diff --git a/{current_path} b/{current_path}"),
        "new file mode 100644".to_owned(),
        "--- /dev/null".to_owned(),
        format!("+++ b/{current_path}"),
    ];

    if !lines.is_empty() {
        diff_lines.push(format!("@@ -0,0 +1,{} @@", lines.len()));
        diff_lines.extend(lines.into_iter().map(|line| format!("+{line}")));
    }

    if !content.is_empty() && !content.ends_with('\n') {
        diff_lines.push(r"\ No newline at end of file".to_owned());
    }

    Ok(diff_lines.join("\n"))
}

/// Loads full document sides for a Git diff.
fn load_git_diff_document_content(
    repo_root: &FsPath,
    current_path: &str,
    original_path: Option<&str>,
    status_code: Option<&str>,
    section_id: GitDiffSection,
) -> Result<GitDiffDocumentContent, ApiError> {
    let normalized_status = status_code.unwrap_or("M");
    let before = match section_id {
        GitDiffSection::Staged => {
            if normalized_status == "A" {
                read_git_diff_document_side(repo_root, GitDiffDocumentSideSpec::Empty)
            } else {
                let before_path = original_path.unwrap_or(current_path);
                read_git_diff_document_side(repo_root, GitDiffDocumentSideSpec::Head(before_path))
            }
        }
        GitDiffSection::Unstaged => {
            if normalized_status == "?" {
                read_git_diff_document_side(repo_root, GitDiffDocumentSideSpec::Empty)
            } else {
                read_git_diff_document_side(repo_root, GitDiffDocumentSideSpec::Index(current_path))
            }
        }
    }?;

    let after = match section_id {
        GitDiffSection::Staged => {
            if normalized_status == "D" {
                read_git_diff_document_side(repo_root, GitDiffDocumentSideSpec::Empty)
            } else {
                read_git_diff_document_side(repo_root, GitDiffDocumentSideSpec::Index(current_path))
            }
        }
        GitDiffSection::Unstaged => {
            if normalized_status == "D" {
                read_git_diff_document_side(repo_root, GitDiffDocumentSideSpec::Empty)
            } else {
                read_git_diff_document_side(repo_root, GitDiffDocumentSideSpec::Worktree(current_path))
            }
        }
    }?;
    let edit_blocked_reason = git_diff_document_edit_blocked_reason(
        repo_root,
        current_path,
        original_path,
        section_id,
    )?;

    Ok(GitDiffDocumentContent {
        before,
        after,
        can_edit: edit_blocked_reason.is_none(),
        edit_blocked_reason,
        is_complete_document: true,
        note: None,
    })
}

/// Explains why a rendered Git document diff cannot be edited.
fn git_diff_document_edit_blocked_reason(
    repo_root: &FsPath,
    current_path: &str,
    original_path: Option<&str>,
    section_id: GitDiffSection,
) -> Result<Option<String>, ApiError> {
    if !matches!(section_id, GitDiffSection::Staged) {
        return Ok(None);
    }

    if git_path_has_unstaged_worktree_changes(repo_root, current_path, original_path)? {
        return Ok(Some(
            "This staged Markdown diff is read-only because the worktree has unstaged changes for this file."
                .to_owned(),
        ));
    }

    Ok(None)
}

/// Checks whether the worktree differs from the index for a Git path.
fn git_path_has_unstaged_worktree_changes(
    repo_root: &FsPath,
    current_path: &str,
    original_path: Option<&str>,
) -> Result<bool, ApiError> {
    let pathspecs = collect_git_pathspecs(current_path, original_path);
    let output = git_command()
        .arg("-C")
        .arg(repo_root)
        .arg("diff")
        .arg("--quiet")
        .arg("--")
        .args(&pathspecs)
        .output()
        .map_err(|err| ApiError::internal(format!("failed to inspect unstaged git changes: {err}")))?;

    if output.status.success() {
        return Ok(false);
    }

    if output.status.code() == Some(1) {
        return Ok(true);
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.is_empty() {
        Err(ApiError::internal("failed to inspect unstaged git changes"))
    } else {
        Err(ApiError::internal(format!(
            "failed to inspect unstaged git changes: {stderr}"
        )))
    }
}

/// Represents the side of a Git document diff to read.
enum GitDiffDocumentSideSpec<'a> {
    Empty,
    Head(&'a str),
    Index(&'a str),
    Worktree(&'a str),
}

/// Reads one full document side for a Git diff.
fn read_git_diff_document_side(
    repo_root: &FsPath,
    spec: GitDiffDocumentSideSpec<'_>,
) -> Result<GitDiffDocumentSide, ApiError> {
    match spec {
        GitDiffDocumentSideSpec::Empty => Ok(GitDiffDocumentSide {
            content: String::new(),
            source: GitDiffDocumentSideSource::Empty,
        }),
        GitDiffDocumentSideSpec::Head(path) => Ok(GitDiffDocumentSide {
            content: read_git_object_text(repo_root, "HEAD", path)?,
            source: GitDiffDocumentSideSource::Head,
        }),
        GitDiffDocumentSideSpec::Index(path) => Ok(GitDiffDocumentSide {
            content: read_git_index_text(repo_root, path)?,
            source: GitDiffDocumentSideSource::Index,
        }),
        GitDiffDocumentSideSpec::Worktree(path) => Ok(GitDiffDocumentSide {
            content: read_git_worktree_text(repo_root, path)?,
            source: GitDiffDocumentSideSource::Worktree,
        }),
    }
}

/// Reads a UTF-8 Git object as text.
fn read_git_object_text(repo_root: &FsPath, revision: &str, path: &str) -> Result<String, ApiError> {
    let object_path = normalize_git_object_path(path);
    let spec = format!("{revision}:{object_path}");
    read_git_spec_text(repo_root, &spec, "git object")
}

/// Reads a UTF-8 Git index blob as text.
fn read_git_index_text(repo_root: &FsPath, path: &str) -> Result<String, ApiError> {
    let object_path = normalize_git_object_path(path);
    let spec = format!(":{object_path}");
    read_git_spec_text(repo_root, &spec, "git index object")
}

const GIT_STDERR_CAPTURE_MAX_BYTES: usize = 64 * 1024;

/// Reads a UTF-8 Git spec through a capped pipe.
fn read_git_spec_text(repo_root: &FsPath, spec: &str, label: &str) -> Result<String, ApiError> {
    use std::io::Read as _;

    let mut child = git_command()
        .arg("-C")
        .arg(repo_root)
        .arg("show")
        .arg(spec)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| ApiError::internal(format!("failed to read {label}: {err}")))?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| ApiError::internal(format!("failed to capture {label} output")))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| ApiError::internal(format!("failed to capture {label} errors")))?;
    let stderr_thread = std::thread::spawn(move || {
        let mut content = Vec::new();
        let result = stderr
            .by_ref()
            .take(GIT_STDERR_CAPTURE_MAX_BYTES as u64 + 1)
            .read_to_end(&mut content);
        let truncated = content.len() > GIT_STDERR_CAPTURE_MAX_BYTES;
        if truncated {
            content.truncate(GIT_STDERR_CAPTURE_MAX_BYTES);
        }
        (content, truncated, result.err())
    });

    let mut content = Vec::new();
    let read_result = stdout
        .by_ref()
        .take(MAX_FILE_CONTENT_BYTES as u64 + 1)
        .read_to_end(&mut content);
    if let Err(err) = read_result {
        let _ = child.kill();
        let _ = child.wait();
        let _ = stderr_thread.join();
        return Err(ApiError::internal(format!("failed to read {label}: {err}")));
    }

    if content.len() > MAX_FILE_CONTENT_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        let _ = stderr_thread.join();
        return Err(ApiError::bad_request(format!(
            "{label} exceeds the {} MB read limit",
            MAX_FILE_CONTENT_BYTES / (1024 * 1024)
        ))
        .with_kind(ApiErrorKind::GitDocumentTooLarge));
    }

    drop(stdout);
    let wait_result = child.wait();
    let (stderr, stderr_truncated, stderr_error) = stderr_thread
        .join()
        .map_err(|_| ApiError::internal(format!("failed to read {label}: stderr reader panicked")))?;
    let status =
        wait_result.map_err(|err| ApiError::internal(format!("failed to read {label}: {err}")))?;

    if status.success() {
        return decode_git_document_text(&content, label);
    }

    let stderr = String::from_utf8_lossy(&stderr).trim().to_owned();
    if is_missing_git_object_error(&stderr) {
        // Git document readers use NOT_FOUND as an internal "drop Markdown
        // enrichment" signal. The diff itself may still exist and should be
        // returned by the API boundary without document_content.
        return Err(ApiError::not_found(format!("{label} not found: {spec}"))
            .with_kind(ApiErrorKind::GitDocumentNotFound));
    }
    if let Some(err) = stderr_error {
        return Err(ApiError::internal(format!(
            "failed to read {label}: failed to drain git stderr: {err}"
        )));
    }
    if stderr.is_empty() {
        Err(ApiError::internal(format!("failed to read {label}")))
    } else if stderr_truncated {
        Err(ApiError::internal(format!(
            "failed to read {label}: {stderr}... [stderr truncated]"
        )))
    } else {
        Err(ApiError::internal(format!("failed to read {label}: {stderr}")))
    }
}

/// Reads a worktree file as text.
fn read_git_worktree_text(repo_root: &FsPath, path: &str) -> Result<String, ApiError> {
    let content = read_git_worktree_bytes(repo_root, path)?;
    decode_git_document_text(&content, "git worktree file")
}

/// Reads a worktree file or symlink without escaping the repository root.
fn read_git_worktree_bytes(repo_root: &FsPath, path: &str) -> Result<Vec<u8>, ApiError> {
    use std::io::Read as _;

    let file_path = repo_root.join(path);
    let canonical_repo_root = fs::canonicalize(repo_root).map_err(|err| {
        ApiError::internal(format!(
            "failed to canonicalize git repo root {}: {err}",
            repo_root.display()
        ))
    })?;
    ensure_worktree_parent_stays_in_repo(&canonical_repo_root, &file_path, path)?;
    let metadata = fs::symlink_metadata(&file_path)
        .map_err(|err| git_worktree_io_error("inspect", path, err))?;

    if metadata.file_type().is_symlink() {
        let canonical_target =
            canonicalize_worktree_path(&file_path, "worktree symlink target", path)?;
        ensure_canonical_path_starts_in_repo(
            &canonical_repo_root,
            &canonical_target,
            "worktree symlink target",
            path,
        )?;
        let target_metadata = fs::metadata(&canonical_target)
            .map_err(|err| git_worktree_io_error("inspect symlink target", path, err))?;
        if !target_metadata.is_file() {
            return Err(ApiError::not_found(format!(
                "git worktree symlink target is not a file: {path}"
            ))
            .with_kind(ApiErrorKind::GitDocumentNotFile));
        }
        ensure_git_document_bytes_within_limit(target_metadata.len(), "git worktree symlink target")?;
        return read_capped_worktree_file(
            &canonical_target,
            path,
            "git worktree symlink target",
        );
    }

    if !metadata.is_file() {
        // NOT_FOUND here means "this path is not an enrichable worktree
        // document"; load_git_diff_for_request maps that to document_content
        // fallback instead of treating the whole diff as missing.
        return Err(ApiError::not_found(format!(
            "git worktree path is not a file: {path}"
        ))
        .with_kind(ApiErrorKind::GitDocumentNotFile));
    }

    ensure_git_document_bytes_within_limit(metadata.len(), "git worktree file")?;
    ensure_worktree_path_stays_in_repo(&canonical_repo_root, &file_path, path)?;
    // Unix opens reject symlink swaps with O_NOFOLLOW. On Windows the remaining
    // swap window is accepted under the local single-user threat model.
    let file = open_worktree_file(&file_path, path, "git worktree file")?;
    let mut content = Vec::new();
    file.take(MAX_FILE_CONTENT_BYTES as u64 + 1)
        .read_to_end(&mut content)
        .map_err(|err| git_worktree_io_error("read", path, err))?;
    ensure_git_document_bytes_within_limit(content.len() as u64, "git worktree file")?;
    Ok(content)
}

/// Ensures the worktree entry's parent resolves inside the Git repository root.
fn ensure_worktree_parent_stays_in_repo(
    canonical_repo_root: &FsPath,
    file_path: &FsPath,
    relative_path: &str,
) -> Result<(), ApiError> {
    let Some(parent_path) = file_path.parent() else {
        return Err(ApiError::internal(format!(
            "failed to resolve parent for git worktree file {relative_path}"
        )));
    };
    let canonical_parent = canonicalize_worktree_path(parent_path, "worktree parent", relative_path)?;
    ensure_canonical_path_starts_in_repo(
        canonical_repo_root,
        &canonical_parent,
        "worktree parent",
        relative_path,
    )
}

/// Ensures a regular worktree path resolves inside the Git repository root.
fn ensure_worktree_path_stays_in_repo(
    canonical_repo_root: &FsPath,
    file_path: &FsPath,
    relative_path: &str,
) -> Result<(), ApiError> {
    let canonical_file = canonicalize_worktree_path(file_path, "worktree file", relative_path)?;
    ensure_canonical_path_starts_in_repo(
        canonical_repo_root,
        &canonical_file,
        "worktree file",
        relative_path,
    )
}

/// Canonicalizes a worktree path for repository containment checks.
fn canonicalize_worktree_path(
    path: &FsPath,
    label: &str,
    relative_path: &str,
) -> Result<PathBuf, ApiError> {
    fs::canonicalize(path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            ApiError::not_found(format!("git {label} not found: {relative_path}"))
                .with_kind(ApiErrorKind::GitDocumentNotFound)
        } else {
            ApiError::internal(format!(
                "failed to canonicalize git {label} {relative_path}: {err}"
            ))
        }
    })
}

/// Status codes that qualify for Markdown-enrichment degradation even when
/// the originating `ApiError` carries no `ApiErrorKind`. Both
/// [`should_degrade_git_diff_document_enrichment_error`] and
/// [`git_diff_document_enrichment_note`] consult this list, so adding a new
/// degraded status updates both sites in lockstep. Server errors (`5xx`)
/// are handled separately by `StatusCode::is_server_error`.
const DEGRADED_UNTAGGED_STATUSES: &[StatusCode] = &[StatusCode::BAD_REQUEST, StatusCode::NOT_FOUND];

fn is_untagged_degradable_status(status: StatusCode) -> bool {
    DEGRADED_UNTAGGED_STATUSES.contains(&status)
}

fn git_diff_document_enrichment_note(error: &ApiError) -> Option<String> {
    match error.kind {
        Some(ApiErrorKind::GitDocumentTooLarge) => Some(
            "Rendered Markdown is unavailable because the document exceeds the 10 MB read limit."
                .to_owned(),
        ),
        Some(ApiErrorKind::GitDocumentBecameSymlink) => Some(
            "Rendered Markdown is unavailable because the file changed to a symlink while loading."
                .to_owned(),
        ),
        Some(ApiErrorKind::GitDocumentInvalidUtf8) => Some(
            "Rendered Markdown is unavailable because the document is not valid UTF-8."
                .to_owned(),
        ),
        Some(ApiErrorKind::GitDocumentNotFile) => Some(
            "Rendered Markdown is unavailable because the path is not a regular file.".to_owned(),
        ),
        Some(ApiErrorKind::GitDocumentNotFound) => Some(
            "Rendered Markdown is unavailable because the document could not be found.".to_owned(),
        ),
        Some(
            ApiErrorKind::LocalSessionMissing
            | ApiErrorKind::RemoteConnectionUnavailable
            | ApiErrorKind::RemoteSessionHydrationFreshnessRace
            | ApiErrorKind::RemoteSessionMissingFullTranscript,
        ) => None,
        None if error.status.is_server_error() => Some(
            "Rendered Markdown is unavailable due to a read error."
                .to_owned(),
        ),
        None if is_untagged_degradable_status(error.status) => {
            Some("Rendered Markdown is unavailable.".to_owned())
        }
        None => None,
    }
}

fn should_degrade_git_diff_document_enrichment_error(error: &ApiError) -> bool {
    is_untagged_degradable_status(error.status) || error.status.is_server_error()
}

/// Ensures a canonicalized path starts inside the canonical Git repository root.
fn ensure_canonical_path_starts_in_repo(
    canonical_repo_root: &FsPath,
    canonical_path: &FsPath,
    label: &str,
    relative_path: &str,
) -> Result<(), ApiError> {
    if !canonical_path.starts_with(&canonical_repo_root) {
        return Err(ApiError::bad_request(format!(
            "git {label} escapes repository root: {relative_path}"
        )));
    }

    Ok(())
}

/// Opens a regular worktree file for reading.
#[cfg(unix)]
fn open_worktree_file(file_path: &FsPath, relative_path: &str, label: &str) -> Result<fs::File, ApiError> {
    use std::os::unix::fs::OpenOptionsExt as _;

    match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(file_path)
    {
        Ok(file) => Ok(file),
        Err(err) if err.raw_os_error() == Some(libc::ELOOP) => {
            Err(ApiError::bad_request(format!("{label} changed to a symlink: {relative_path}"))
                .with_kind(ApiErrorKind::GitDocumentBecameSymlink))
        }
        Err(err) => Err(git_worktree_io_error("open", relative_path, err)),
    }
}

/// Opens a regular worktree file for reading.
#[cfg(not(unix))]
fn open_worktree_file(file_path: &FsPath, relative_path: &str, _label: &str) -> Result<fs::File, ApiError> {
    fs::File::open(file_path).map_err(|err| git_worktree_io_error("open", relative_path, err))
}

/// Reads a worktree file through the shared document byte cap.
fn read_capped_worktree_file(file_path: &FsPath, relative_path: &str, label: &str) -> Result<Vec<u8>, ApiError> {
    use std::io::Read as _;

    let file = open_worktree_file(file_path, relative_path, label)?;
    let mut content = Vec::new();
    file.take(MAX_FILE_CONTENT_BYTES as u64 + 1)
        .read_to_end(&mut content)
        .map_err(|err| git_worktree_io_error("read", relative_path, err))?;
    ensure_git_document_bytes_within_limit(content.len() as u64, label)?;
    Ok(content)
}

/// Maps worktree I/O errors to API-safe, repo-relative messages.
fn git_worktree_io_error(action: &str, relative_path: &str, err: std::io::Error) -> ApiError {
    if err.kind() == std::io::ErrorKind::NotFound {
        ApiError::not_found(format!("git worktree path not found: {relative_path}"))
            .with_kind(ApiErrorKind::GitDocumentNotFound)
    } else {
        ApiError::internal(format!(
            "failed to {action} git worktree file {relative_path}: {err}"
        ))
    }
}

/// Enforces the file read ceiling on Git document enrichment.
fn ensure_git_document_bytes_within_limit(size: u64, label: &str) -> Result<(), ApiError> {
    if size > MAX_FILE_CONTENT_BYTES as u64 {
        return Err(ApiError::bad_request(format!(
            "{label} exceeds the {} MB read limit",
            MAX_FILE_CONTENT_BYTES / (1024 * 1024)
        ))
        .with_kind(ApiErrorKind::GitDocumentTooLarge));
    }

    Ok(())
}

/// Decodes Git document bytes only when they are valid UTF-8.
fn decode_git_document_text(content: &[u8], label: &str) -> Result<String, ApiError> {
    std::str::from_utf8(content)
        .map(|text| text.to_owned())
        .map_err(|_| {
            ApiError::bad_request(format!("{label} is not valid UTF-8"))
                .with_kind(ApiErrorKind::GitDocumentInvalidUtf8)
        })
}

/// Checks whether git reported a missing object/path.
fn is_missing_git_object_error(stderr: &str) -> bool {
    let normalized = stderr.to_ascii_lowercase();
    normalized.contains("not a valid object name")
        || normalized.contains("invalid object name")
        || normalized.contains("unknown revision")
        || normalized.contains("does not exist (neither on disk nor in the index)")
        || normalized.contains("does not exist in")
        || normalized.contains("path")
            && normalized.contains("exists on disk")
            && normalized.contains("but not in")
}

/// Builds a Git command with stable stderr wording for parsed error paths.
fn git_command() -> Command {
    let mut command = Command::new("git");
    command.env("LC_ALL", "C").env("LANG", "C");
    command
}

/// Normalizes a repository-relative path for Git object lookups.
fn normalize_git_object_path(path: &str) -> String {
    path.replace('\\', "/")
}

/// Reverts Git file action.
fn revert_git_file_action(
    repo_root: &FsPath,
    current_path: &str,
    original_path: Option<&str>,
    status_code: Option<&str>,
) -> Result<(), ApiError> {
    if let Some(original_path) = original_path.filter(|original| *original != current_path) {
        run_git_pathspec_command(
            repo_root,
            &["restore", "--worktree", "--source=HEAD"],
            &[original_path.to_owned()],
            "failed to restore the original git path",
        )?;
    }

    if status_code.is_some_and(|status| status.trim() == "?") {
        run_git_pathspec_command(
            repo_root,
            &["clean", "-f"],
            &[current_path.to_owned()],
            "failed to remove untracked git path",
        )?;
    } else {
        run_git_pathspec_command(
            repo_root,
            &["restore", "--worktree", "--source=HEAD"],
            &[current_path.to_owned()],
            "failed to revert git changes",
        )?;
    }

    Ok(())
}

/// Runs Git pathspec command.
fn run_git_pathspec_command(
    repo_root: &FsPath,
    args: &[&str],
    pathspecs: &[String],
    error_context: &str,
) -> Result<(), ApiError> {
    let output = git_command()
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .arg("--")
        .args(pathspecs)
        .output()
        .map_err(|err| ApiError::internal(format!("{error_context}: {err}")))?;

    if output.status.success() {
        return Ok(());
    }

    let detail = extract_git_command_error(&output);

    if detail.is_empty() {
        Err(ApiError::internal(error_context))
    } else {
        Err(ApiError::internal(format!("{error_context}: {detail}")))
    }
}

/// Returns whether staged Git changes.
fn has_staged_git_changes(status: &GitStatusResponse) -> bool {
    status.files.iter().any(|file| file.index_status.is_some())
}

/// Extracts Git command error.
fn extract_git_command_error(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !stderr.is_empty() {
        return stderr;
    }

    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// Builds Git commit summary.
fn build_git_commit_summary(message: &str) -> String {
    let headline = message.lines().next().unwrap_or("").trim();
    if headline.is_empty() {
        "Created commit.".to_owned()
    } else {
        format!("Created commit: {headline}")
    }
}

/// Pushes Git repo.
fn push_git_repo(workdir: &FsPath) -> Result<GitRepoActionResponse, ApiError> {
    let workdir = normalize_git_workdir_path(workdir)?;
    let Some(repo_root) = resolve_git_repo_root(&workdir)? else {
        return Err(ApiError::bad_request("no git repository found"));
    };

    let status_before = load_git_status_for_path(&workdir)?;
    run_git_repo_command(&repo_root, &["push"], "failed to push git changes")?;
    let status = load_git_status_for_path(&workdir)?;
    Ok(GitRepoActionResponse {
        summary: build_git_push_summary(&status_before, &status),
        status,
    })
}

/// Syncs Git repo.
fn sync_git_repo(workdir: &FsPath) -> Result<GitRepoActionResponse, ApiError> {
    let workdir = normalize_git_workdir_path(workdir)?;
    let Some(repo_root) = resolve_git_repo_root(&workdir)? else {
        return Err(ApiError::bad_request("no git repository found"));
    };

    let status_before = load_git_status_for_path(&workdir)?;
    if status_before.upstream.is_none() {
        return Err(ApiError::bad_request(
            "git sync requires a tracking upstream branch",
        ));
    }

    run_git_repo_command(
        &repo_root,
        &["pull", "--ff-only"],
        "failed to pull git changes",
    )?;
    run_git_repo_command(&repo_root, &["push"], "failed to push git changes")?;
    let status = load_git_status_for_path(&workdir)?;
    Ok(GitRepoActionResponse {
        summary: build_git_sync_summary(&status_before, &status),
        status,
    })
}

/// Runs Git repo command.
fn run_git_repo_command(
    repo_root: &FsPath,
    args: &[&str],
    error_context: &str,
) -> Result<(), ApiError> {
    let output = git_command()
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .map_err(|err| ApiError::internal(format!("{error_context}: {err}")))?;

    if output.status.success() {
        return Ok(());
    }

    let detail = extract_git_command_error(&output);
    if detail.is_empty() {
        Err(ApiError::bad_request(error_context))
    } else {
        Err(ApiError::bad_request(format!("{error_context}: {detail}")))
    }
}

/// Builds Git push summary.
fn build_git_push_summary(
    status_before: &GitStatusResponse,
    status_after: &GitStatusResponse,
) -> String {
    let branch = status_after
        .branch
        .as_deref()
        .or(status_before.branch.as_deref());
    let upstream = status_after
        .upstream
        .as_deref()
        .or(status_before.upstream.as_deref());
    build_git_repo_action_summary("Pushed", branch, upstream)
}

/// Builds Git sync summary.
fn build_git_sync_summary(
    status_before: &GitStatusResponse,
    status_after: &GitStatusResponse,
) -> String {
    let branch = status_after
        .branch
        .as_deref()
        .or(status_before.branch.as_deref());
    let upstream = status_after
        .upstream
        .as_deref()
        .or(status_before.upstream.as_deref());
    build_git_repo_action_summary("Synced", branch, upstream)
}

/// Builds Git repo action summary.
fn build_git_repo_action_summary(
    verb: &str,
    branch: Option<&str>,
    upstream: Option<&str>,
) -> String {
    match (branch, upstream) {
        (Some(branch), Some(upstream)) => format!("{verb} {branch} with {upstream}."),
        (Some(branch), None) => format!("{verb} {branch}."),
        _ => format!("{verb} git repository."),
    }
}

/// Enumerates parsed Git branch states.
struct ParsedGitBranchStatus {
    ahead: usize,
    behind: usize,
    branch: Option<String>,
    upstream: Option<String>,
}

/// Resolves Git repo root.
fn resolve_git_repo_root(workdir: &FsPath) -> Result<Option<PathBuf>, ApiError> {
    let output = git_command()
        .arg("-C")
        .arg(workdir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|err| ApiError::internal(format!("failed to run git rev-parse: {err}")))?;

    if output.status.success() {
        let repo_root = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        return Ok((!repo_root.is_empty()).then(|| PathBuf::from(repo_root)));
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let trimmed = stderr.trim();
    if trimmed.contains("not a git repository") {
        return Ok(None);
    }

    Err(ApiError::internal(format!(
        "git rev-parse failed: {trimmed}"
    )))
}

/// Parses Git branch status.
fn parse_git_branch_status(line: &str) -> ParsedGitBranchStatus {
    let mut branch = None;
    let mut upstream = None;
    let mut ahead = 0;
    let mut behind = 0;

    let (head_segment, counts_segment) = match line.split_once(" [") {
        Some((head, counts)) => (head, Some(counts.trim_end_matches(']'))),
        None => (line, None),
    };

    if let Some((local_branch, upstream_branch)) = head_segment.split_once("...") {
        branch = Some(local_branch.trim().to_owned());
        let upstream_name = upstream_branch.trim();
        if !upstream_name.is_empty() {
            upstream = Some(upstream_name.to_owned());
        }
    } else {
        let trimmed = head_segment.trim();
        if !trimmed.is_empty() {
            branch = Some(trimmed.to_owned());
        }
    }

    if let Some(counts_segment) = counts_segment {
        for item in counts_segment.split(',') {
            let trimmed = item.trim();
            if let Some(value) = trimmed.strip_prefix("ahead ") {
                ahead = value.parse::<usize>().unwrap_or(0);
            } else if let Some(value) = trimmed.strip_prefix("behind ") {
                behind = value.parse::<usize>().unwrap_or(0);
            }
        }
    }

    ParsedGitBranchStatus {
        ahead,
        behind,
        branch,
        upstream,
    }
}

/// Parses Git status paths.
fn parse_git_status_paths(path_payload: &str) -> (Option<String>, String) {
    if let Some(separator_index) = find_git_status_rename_separator(path_payload) {
        let original_path = decode_git_status_path(&path_payload[..separator_index]);
        let path = decode_git_status_path(&path_payload[separator_index + 4..]);
        return (Some(original_path), path);
    }

    (None, decode_git_status_path(path_payload))
}

/// Finds Git status rename separator.
fn find_git_status_rename_separator(path_payload: &str) -> Option<usize> {
    let bytes = path_payload.as_bytes();
    let mut index = 0;
    let mut in_quotes = false;

    while index < bytes.len() {
        match bytes[index] {
            b'\\' if in_quotes => {
                index += 2;
            }
            b'"' => {
                in_quotes = !in_quotes;
                index += 1;
            }
            b' ' if !in_quotes && bytes[index..].starts_with(b" -> ") => {
                return Some(index);
            }
            _ => {
                index += 1;
            }
        }
    }

    None
}

/// Decodes Git status path.
fn decode_git_status_path(path: &str) -> String {
    let trimmed = path.trim();
    decode_git_status_quoted_path(trimmed).unwrap_or_else(|| trimmed.to_owned())
}

/// Decodes Git status quoted path.
fn decode_git_status_quoted_path(path: &str) -> Option<String> {
    if !path.starts_with('"') || !path.ends_with('"') || path.len() < 2 {
        return None;
    }

    let inner = &path[1..path.len() - 1];
    let bytes = inner.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] != b'\\' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }

        index += 1;
        if index >= bytes.len() {
            return None;
        }

        match bytes[index] {
            b'"' | b'\\' => {
                decoded.push(bytes[index]);
                index += 1;
            }
            b'a' => {
                decoded.push(0x07);
                index += 1;
            }
            b'b' => {
                decoded.push(0x08);
                index += 1;
            }
            b'f' => {
                decoded.push(0x0c);
                index += 1;
            }
            b'n' => {
                decoded.push(b'\n');
                index += 1;
            }
            b'r' => {
                decoded.push(b'\r');
                index += 1;
            }
            b't' => {
                decoded.push(b'\t');
                index += 1;
            }
            b'v' => {
                decoded.push(0x0b);
                index += 1;
            }
            b'0'..=b'7' => {
                let mut value = bytes[index] - b'0';
                index += 1;

                for _ in 0..2 {
                    if index >= bytes.len() {
                        break;
                    }
                    let next = bytes[index];
                    if !matches!(next, b'0'..=b'7') {
                        break;
                    }
                    value = value.saturating_mul(8).saturating_add(next - b'0');
                    index += 1;
                }

                decoded.push(value);
            }
            other => {
                decoded.push(other);
                index += 1;
            }
        }
    }

    Some(String::from_utf8_lossy(&decoded).into_owned())
}

/// Normalizes Git status code.
fn normalize_git_status_code(code: char) -> Option<String> {
    match code {
        ' ' => None,
        other => Some(other.to_string()),
    }
}
