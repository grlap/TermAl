fn resolve_persistence_path(default_workdir: &str) -> PathBuf {
    resolve_termal_data_dir(default_workdir).join("sessions.json")
}

fn load_state(path: &FsPath) -> Result<Option<StateInner>> {
    if !path.exists() {
        return Ok(None);
    }

    let raw = fs::read(path).with_context(|| format!("failed to read `{}`", path.display()))?;
    let persisted: PersistedState = serde_json::from_slice(&raw)
        .with_context(|| format!("failed to parse `{}`", path.display()))?;
    Ok(Some(persisted.into_inner()))
}

fn persist_state(path: &FsPath, inner: &StateInner) -> Result<()> {
    let persisted = PersistedState::from_inner(inner);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }

    let encoded =
        serde_json::to_vec_pretty(&persisted).context("failed to serialize persisted state")?;
    fs::write(path, encoded).with_context(|| format!("failed to write `{}`", path.display()))
}

fn deliver_turn_dispatch(state: &AppState, dispatch: TurnDispatch) -> Result<(), ApiError> {
    match dispatch {
        TurnDispatch::PersistentClaude {
            command,
            sender,
            session_id,
        } => {
            if let Err(err) = sender.send(ClaudeRuntimeCommand::Prompt(command)) {
                let _ = state.clear_runtime(&session_id);
                let _ = state.fail_turn(
                    &session_id,
                    &format!("failed to queue prompt for Claude session: {err}"),
                );
                return Err(ApiError::internal(
                    "failed to queue prompt for Claude session",
                ));
            }
        }
        TurnDispatch::PersistentCodex {
            command,
            sender,
            session_id,
        } => {
            if let Err(err) = sender.send(CodexRuntimeCommand::Prompt {
                session_id: session_id.clone(),
                command,
            }) {
                let _ = state.clear_runtime(&session_id);
                let _ = state.fail_turn(
                    &session_id,
                    &format!("failed to queue prompt for Codex session: {err}"),
                );
                return Err(ApiError::internal(
                    "failed to queue prompt for Codex session",
                ));
            }
        }
        TurnDispatch::PersistentAcp {
            command,
            sender,
            session_id,
        } => {
            if let Err(err) = sender.send(AcpRuntimeCommand::Prompt(command)) {
                let _ = state.clear_runtime(&session_id);
                let _ = state.fail_turn(
                    &session_id,
                    &format!("failed to queue prompt for ACP session: {err}"),
                );
                return Err(ApiError::internal(
                    "failed to queue prompt for agent session",
                ));
            }
        }
    }

    Ok(())
}

fn get_string<'a>(value: &'a Value, path: &[&str]) -> Result<&'a str> {
    let mut current = value;
    for segment in path {
        current = current
            .get(segment)
            .with_context(|| format!("missing field `{}`", path.join(".")))?;
    }

    current
        .as_str()
        .with_context(|| format!("field `{}` is not a string", path.join(".")))
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { ok: true })
}

async fn get_state(State(state): State<AppState>) -> Json<StateResponse> {
    Json(state.snapshot())
}

async fn read_file(
    State(state): State<AppState>,
    Query(query): Query<FileQuery>,
) -> Result<Json<FileResponse>, ApiError> {
    let session_id = required_session_id(query.session_id.as_deref())?;
    let resolved_path =
        resolve_session_scoped_requested_path(&state, session_id, &query.path, ScopedPathMode::ExistingFile)?;
    let content = fs::read_to_string(&resolved_path).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound => {
            ApiError::bad_request(format!("file not found: {}", resolved_path.display()))
        }
        io::ErrorKind::InvalidData => ApiError::bad_request(format!(
            "file is not valid UTF-8: {}",
            resolved_path.display()
        )),
        _ => ApiError::internal(format!(
            "failed to read file {}: {err}",
            resolved_path.display()
        )),
    })?;

    Ok(Json(FileResponse {
        path: resolved_path.to_string_lossy().into_owned(),
        content,
        language: infer_language_from_path(&resolved_path).map(str::to_owned),
    }))
}

async fn write_file(
    State(state): State<AppState>,
    Json(request): Json<WriteFileRequest>,
) -> Result<Json<FileResponse>, ApiError> {
    let session_id = required_session_id(request.session_id.as_deref())?;
    let resolved_path = resolve_session_scoped_requested_path(
        &state,
        session_id,
        &request.path,
        ScopedPathMode::AllowMissingLeaf,
    )?;
    if let Some(parent) = resolved_path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            ApiError::internal(format!(
                "failed to create parent directory for {}: {err}",
                resolved_path.display()
            ))
        })?;
    }

    fs::write(&resolved_path, request.content.as_bytes()).map_err(|err| {
        ApiError::internal(format!(
            "failed to write file {}: {err}",
            resolved_path.display()
        ))
    })?;

    Ok(Json(FileResponse {
        path: resolved_path.to_string_lossy().into_owned(),
        content: request.content,
        language: infer_language_from_path(&resolved_path).map(str::to_owned),
    }))
}

async fn read_directory(
    State(state): State<AppState>,
    Query(query): Query<FileQuery>,
) -> Result<Json<DirectoryResponse>, ApiError> {
    let session_id = required_session_id(query.session_id.as_deref())?;
    let resolved_path =
        resolve_session_scoped_requested_path(&state, session_id, &query.path, ScopedPathMode::ExistingPath)?;
    let metadata = fs::metadata(&resolved_path).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound => {
            ApiError::bad_request(format!("path not found: {}", resolved_path.display()))
        }
        _ => ApiError::internal(format!(
            "failed to stat path {}: {err}",
            resolved_path.display()
        )),
    })?;

    if !metadata.is_dir() {
        return Err(ApiError::bad_request(format!(
            "path is not a directory: {}",
            resolved_path.display()
        )));
    }

    let mut entries = fs::read_dir(&resolved_path)
        .map_err(|err| {
            ApiError::internal(format!(
                "failed to read directory {}: {err}",
                resolved_path.display()
            ))
        })?
        .map(|entry| {
            let entry = entry.map_err(|err| {
                ApiError::internal(format!(
                    "failed to read directory entry in {}: {err}",
                    resolved_path.display()
                ))
            })?;
            let path = entry.path();
            let metadata = entry.metadata().map_err(|err| {
                ApiError::internal(format!(
                    "failed to stat directory entry {}: {err}",
                    path.display()
                ))
            })?;
            let name = entry.file_name().to_string_lossy().into_owned();

            Ok(DirectoryEntry {
                kind: if metadata.is_dir() {
                    FileSystemEntryKind::Directory
                } else {
                    FileSystemEntryKind::File
                },
                name,
                path: path.to_string_lossy().into_owned(),
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;

    entries.sort_by(|left, right| {
        let kind_order = match (&left.kind, &right.kind) {
            (FileSystemEntryKind::Directory, FileSystemEntryKind::File) => std::cmp::Ordering::Less,
            (FileSystemEntryKind::File, FileSystemEntryKind::Directory) => {
                std::cmp::Ordering::Greater
            }
            _ => std::cmp::Ordering::Equal,
        };

        if kind_order == std::cmp::Ordering::Equal {
            left.name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase())
        } else {
            kind_order
        }
    });

    Ok(Json(DirectoryResponse {
        name: resolved_path
            .file_name()
            .and_then(|value| value.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| resolved_path.to_string_lossy().into_owned()),
        path: resolved_path.to_string_lossy().into_owned(),
        entries,
    }))
}

async fn list_agent_commands(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<AgentCommandsResponse>, ApiError> {
    let response = state.list_agent_commands(&session_id)?;
    Ok(Json(response))
}

fn read_claude_agent_commands(workdir: &FsPath) -> Result<Vec<AgentCommand>, ApiError> {
    let commands_dir = workdir.join(".claude").join("commands");
    let entries = match fs::read_dir(&commands_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(ApiError::internal(format!(
                "failed to read agent commands in {}: {err}",
                commands_dir.display()
            )));
        }
    };

    let mut commands = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| {
            ApiError::internal(format!(
                "failed to read agent command entry in {}: {err}",
                commands_dir.display()
            ))
        })?;
        let path = entry.path();
        let metadata = entry.metadata().map_err(|err| {
            ApiError::internal(format!(
                "failed to stat agent command {}: {err}",
                path.display()
            ))
        })?;
        if !metadata.is_file() {
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }

        let content = fs::read_to_string(&path).map_err(|err| {
            ApiError::internal(format!(
                "failed to read agent command {}: {err}",
                path.display()
            ))
        })?;
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        let description = content
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("")
            .to_owned();

        commands.push(AgentCommand {
            name: stem.to_owned(),
            description,
            content,
            source: format!(".claude/commands/{}.md", stem),
        });
    }

    commands.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
    });
    Ok(commands)
}

async fn read_git_status(
    State(state): State<AppState>,
    Query(query): Query<FileQuery>,
) -> Result<Json<GitStatusResponse>, ApiError> {
    let workdir = resolve_project_scoped_requested_path(
        &state,
        query.session_id.as_deref(),
        query.project_id.as_deref(),
        &query.path,
        ScopedPathMode::ExistingPath,
    )?;
    Ok(Json(load_git_status_for_path(&workdir)?))
}

async fn read_git_diff(
    State(state): State<AppState>,
    Json(request): Json<GitDiffRequest>,
) -> Result<Json<GitDiffResponse>, ApiError> {
    let workdir = resolve_project_scoped_requested_path(
        &state,
        request.session_id.as_deref(),
        request.project_id.as_deref(),
        &request.workdir,
        ScopedPathMode::ExistingPath,
    )?;
    Ok(Json(load_git_diff_for_request(&workdir, &request)?))
}

async fn apply_git_file_action(
    State(state): State<AppState>,
    Json(request): Json<GitFileActionRequest>,
) -> Result<Json<GitStatusResponse>, ApiError> {
    let workdir = resolve_project_scoped_requested_path(
        &state,
        request.session_id.as_deref(),
        request.project_id.as_deref(),
        &request.workdir,
        ScopedPathMode::ExistingPath,
    )?;
    let workdir = normalize_git_workdir_path(&workdir)?;
    let Some(repo_root) = resolve_git_repo_root(&workdir)? else {
        return Err(ApiError::bad_request("no git repository found"));
    };

    let current_path = normalize_git_repo_relative_path(&request.path)?;
    let original_path = request
        .original_path
        .as_deref()
        .map(normalize_git_repo_relative_path)
        .transpose()?;

    match request.action {
        GitFileAction::Stage => {
            let pathspecs = collect_git_pathspecs(&current_path, original_path.as_deref());
            run_git_pathspec_command(
                &repo_root,
                &["add", "-A"],
                &pathspecs,
                "failed to stage git changes",
            )?;
        }
        GitFileAction::Unstage => {
            let pathspecs = collect_git_pathspecs(&current_path, original_path.as_deref());
            run_git_pathspec_command(
                &repo_root,
                &["restore", "--staged"],
                &pathspecs,
                "failed to unstage git changes",
            )?;
        }
        GitFileAction::Revert => {
            revert_git_file_action(
                &repo_root,
                &current_path,
                original_path.as_deref(),
                request.status_code.as_deref(),
            )?;
        }
    }

    Ok(Json(load_git_status_for_path(&workdir)?))
}

async fn commit_git_changes(
    State(state): State<AppState>,
    Json(request): Json<GitCommitRequest>,
) -> Result<Json<GitCommitResponse>, ApiError> {
    let workdir = resolve_project_scoped_requested_path(
        &state,
        request.session_id.as_deref(),
        request.project_id.as_deref(),
        &request.workdir,
        ScopedPathMode::ExistingPath,
    )?;
    let workdir = normalize_git_workdir_path(&workdir)?;
    let Some(repo_root) = resolve_git_repo_root(&workdir)? else {
        return Err(ApiError::bad_request("no git repository found"));
    };

    let message = request.message.trim();
    if message.is_empty() {
        return Err(ApiError::bad_request("commit message cannot be empty"));
    }

    let status_before = load_git_status_for_path(&workdir)?;
    if !has_staged_git_changes(&status_before) {
        return Err(ApiError::bad_request("no staged changes to commit"));
    }

    let output = Command::new("git")
        .arg("-C")
        .arg(&repo_root)
        .args(["commit", "-m"])
        .arg(message)
        .output()
        .map_err(|err| ApiError::internal(format!("failed to create git commit: {err}")))?;

    if !output.status.success() {
        let detail = extract_git_command_error(&output);
        let message = if detail.is_empty() {
            "failed to create git commit".to_owned()
        } else {
            format!("failed to create git commit: {detail}")
        };
        return Err(ApiError::bad_request(message));
    }

    let status = load_git_status_for_path(&workdir)?;
    Ok(Json(GitCommitResponse {
        status,
        summary: build_git_commit_summary(message),
    }))
}

fn load_git_diff_for_request(
    workdir: &FsPath,
    request: &GitDiffRequest,
) -> Result<GitDiffResponse, ApiError> {
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

    Ok(GitDiffResponse {
        change_type: if matches!(status_code.as_deref(), Some("A" | "?")) {
            GitDiffChangeType::Create
        } else {
            GitDiffChangeType::Edit
        },
        diff,
        diff_id: format!("git:{}", stable_text_hash(&diff_identity)),
        file_path: file_path.exists().then(|| file_path.to_string_lossy().into_owned()),
        language: infer_language_from_path(FsPath::new(&current_path)).map(str::to_owned),
        summary: format!(
            "{} changes in {}",
            request.section_id.summary_label(),
            current_path
        ),
    })
}

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

    let output = Command::new("git")
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
        let (original_path, path) = match path_payload.split_once(" -> ") {
            Some((from, to)) => (Some(from.trim().to_owned()), to.trim().to_owned()),
            None => (None, path_payload.to_owned()),
        };
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

fn normalize_git_workdir_path(path: &FsPath) -> Result<PathBuf, ApiError> {
    if path.is_dir() {
        return Ok(path.to_path_buf());
    }

    path.parent()
        .map(FsPath::to_path_buf)
        .ok_or_else(|| ApiError::bad_request("cannot inspect git status for a root file path"))
}

fn normalize_git_repo_relative_path(path: &str) -> Result<String, ApiError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request("git file path cannot be empty"));
    }

    if FsPath::new(trimmed).is_absolute() {
        return Err(ApiError::bad_request(
            "git file actions require repository-relative paths",
        ));
    }

    if trimmed.contains('\0') {
        return Err(ApiError::bad_request(
            "git file path contains invalid characters",
        ));
    }

    Ok(trimmed.to_owned())
}

fn collect_git_pathspecs(current_path: &str, original_path: Option<&str>) -> Vec<String> {
    let mut pathspecs = Vec::new();
    if let Some(original_path) = original_path.filter(|original| *original != current_path) {
        pathspecs.push(original_path.to_owned());
    }
    pathspecs.push(current_path.to_owned());
    pathspecs
}

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
    let mut command = Command::new("git");
    command.arg("-C").arg(repo_root).arg("diff").arg("--find-renames");

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

fn build_untracked_git_diff(repo_root: &FsPath, current_path: &str) -> Result<String, ApiError> {
    let file_path = repo_root.join(current_path);
    let content = fs::read(&file_path).map_err(|err| {
        ApiError::internal(format!(
            "failed to read untracked file {}: {err}",
            file_path.display()
        ))
    })?;
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

fn run_git_pathspec_command(
    repo_root: &FsPath,
    args: &[&str],
    pathspecs: &[String],
    error_context: &str,
) -> Result<(), ApiError> {
    let output = Command::new("git")
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

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let detail = if stderr.is_empty() { stdout } else { stderr };

    if detail.is_empty() {
        Err(ApiError::internal(error_context))
    } else {
        Err(ApiError::internal(format!("{error_context}: {detail}")))
    }
}

fn has_staged_git_changes(status: &GitStatusResponse) -> bool {
    status.files.iter().any(|file| file.index_status.is_some())
}

fn extract_git_command_error(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !stderr.is_empty() {
        return stderr;
    }

    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn build_git_commit_summary(message: &str) -> String {
    let headline = message.lines().next().unwrap_or("").trim();
    if headline.is_empty() {
        "Created commit.".to_owned()
    } else {
        format!("Created commit: {headline}")
    }
}

fn stable_text_hash(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

async fn state_events(
    State(state): State<AppState>,
) -> Sse<impl futures_core::Stream<Item = std::result::Result<Event, Infallible>>> {
    let mut state_receiver = state.subscribe_events();
    let mut delta_receiver = state.subscribe_delta_events();
    let initial_payload = serde_json::to_string(&state.snapshot())
        .unwrap_or_else(|_| "{\"revision\":0,\"projects\":[],\"sessions\":[]}".to_owned());

    let stream = async_stream::stream! {
        yield Ok(Event::default().event("state").data(initial_payload));

        loop {
            tokio::select! {
                biased;

                result = state_receiver.recv() => {
                    match result {
                        Ok(payload) => yield Ok(Event::default().event("state").data(payload)),
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            let payload = serde_json::to_string(&state.snapshot())
                                .unwrap_or_else(|_| "{\"revision\":0,\"projects\":[],\"sessions\":[]}".to_owned());
                            yield Ok(Event::default().event("state").data(payload));
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }

                result = delta_receiver.recv() => {
                    match result {
                        Ok(payload) => yield Ok(Event::default().event("delta").data(payload)),
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            let payload = serde_json::to_string(&state.snapshot())
                                .unwrap_or_else(|_| "{\"revision\":0,\"projects\":[],\"sessions\":[]}".to_owned());
                            yield Ok(Event::default().event("state").data(payload));
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn create_session(
    State(state): State<AppState>,
    Json(request): Json<CreateSessionRequest>,
) -> Result<(StatusCode, Json<CreateSessionResponse>), ApiError> {
    let response = state.create_session(request)?;
    Ok((StatusCode::CREATED, Json(response)))
}

async fn create_project(
    State(state): State<AppState>,
    Json(request): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<CreateProjectResponse>), ApiError> {
    let response = state.create_project(request)?;
    Ok((StatusCode::CREATED, Json(response)))
}

async fn update_app_settings(
    State(state): State<AppState>,
    Json(request): Json<UpdateAppSettingsRequest>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = state.update_app_settings(request)?;
    Ok(Json(response))
}

async fn pick_project_root(
    State(state): State<AppState>,
) -> Result<Json<PickProjectRootResponse>, ApiError> {
    let default_workdir = state.default_workdir.clone();
    let path = tokio::task::spawn_blocking(move || pick_project_root_path(&default_workdir))
        .await
        .map_err(|err| ApiError::internal(format!("folder picker task failed: {err}")))??;
    Ok(Json(PickProjectRootResponse { path }))
}

async fn update_session_settings(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
    Json(request): Json<UpdateSessionSettingsRequest>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = state.update_session_settings(&session_id, request)?;
    Ok(Json(response))
}

async fn refresh_session_model_options(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = state.refresh_session_model_options(&session_id)?;
    Ok(Json(response))
}

async fn send_message(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
    Json(request): Json<SendMessageRequest>,
) -> Result<(StatusCode, Json<StateResponse>), ApiError> {
    let dispatch = state.dispatch_turn(&session_id, request)?;

    if let DispatchTurnResult::Dispatched(dispatch) = dispatch {
        deliver_turn_dispatch(&state, dispatch)?;
    }

    let snapshot = state.snapshot();

    Ok((StatusCode::ACCEPTED, Json(snapshot)))
}

async fn cancel_queued_prompt(
    AxumPath((session_id, prompt_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = state.cancel_queued_prompt(&session_id, &prompt_id)?;
    Ok(Json(response))
}

async fn stop_session(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = state.stop_session(&session_id)?;
    Ok(Json(response))
}

async fn kill_session(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = state.kill_session(&session_id)?;
    Ok(Json(response))
}

async fn submit_approval(
    AxumPath((session_id, message_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<ApprovalRequest>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = state.update_approval(&session_id, &message_id, request.decision)?;
    Ok(Json(response))
}

#[derive(Debug)]
struct ApiError {
    message: String,
    status: StatusCode,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::BAD_REQUEST,
        }
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::CONFLICT,
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::NOT_FOUND,
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(ErrorResponse {
            error: self.message,
        });
        (self.status, body).into_response()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
enum Agent {
    Codex,
    Claude,
    Cursor,
    Gemini,
}

impl Agent {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self> {
        let mut args = args;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--agent" => {
                    let value = args.next().context("missing value after `--agent`")?;
                    return Self::from_str(&value);
                }
                "codex" => return Ok(Self::Codex),
                "claude" => return Ok(Self::Claude),
                "cursor" | "cursor-agent" => return Ok(Self::Cursor),
                "gemini" | "gemini-cli" => return Ok(Self::Gemini),
                other => bail!("unknown argument `{other}`"),
            }
        }

        Ok(Self::Codex)
    }

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "codex" => Ok(Self::Codex),
            "claude" => Ok(Self::Claude),
            "cursor" | "cursor-agent" => Ok(Self::Cursor),
            "gemini" | "gemini-cli" => Ok(Self::Gemini),
            other => {
                bail!("unknown agent `{other}`; expected `codex`, `claude`, `cursor`, or `gemini`")
            }
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
            Self::Cursor => "Cursor",
            Self::Gemini => "Gemini",
        }
    }

    fn avatar(self) -> &'static str {
        match self {
            Self::Codex => "CX",
            Self::Claude => "CL",
            Self::Cursor => "CR",
            Self::Gemini => "GM",
        }
    }

    fn default_model(self) -> &'static str {
        match self {
            Self::Codex => "gpt-5.4",
            Self::Claude => "sonnet",
            Self::Cursor => "auto",
            Self::Gemini => "auto",
        }
    }

    fn supports_codex_prompt_settings(self) -> bool {
        matches!(self, Self::Codex)
    }

    fn supports_claude_approval_mode(self) -> bool {
        matches!(self, Self::Claude)
    }

    fn supports_cursor_mode(self) -> bool {
        matches!(self, Self::Cursor)
    }

    fn supports_gemini_approval_mode(self) -> bool {
        matches!(self, Self::Gemini)
    }

    fn acp_runtime(self) -> Option<AcpAgent> {
        match self {
            Self::Cursor => Some(AcpAgent::Cursor),
            Self::Gemini => Some(AcpAgent::Gemini),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Project {
    id: String,
    name: String,
    root_path: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Session {
    id: String,
    name: String,
    emoji: String,
    agent: Agent,
    workdir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project_id: Option<String>,
    model: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    model_options: Vec<SessionModelOption>,
    approval_policy: Option<CodexApprovalPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<CodexReasoningEffort>,
    sandbox_mode: Option<CodexSandboxMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor_mode: Option<CursorMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claude_effort: Option<ClaudeEffortLevel>,
    claude_approval_mode: Option<ClaudeApprovalMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gemini_approval_mode: Option<GeminiApprovalMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    external_session_id: Option<String>,
    status: SessionStatus,
    preview: String,
    messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending_prompts: Vec<PendingPrompt>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CodexApprovalPolicy {
    Untrusted,
    OnFailure,
    OnRequest,
    Never,
}

impl CodexApprovalPolicy {
    fn as_cli_value(self) -> &'static str {
        match self {
            Self::Untrusted => "untrusted",
            Self::OnFailure => "on-failure",
            Self::OnRequest => "on-request",
            Self::Never => "never",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CodexSandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl CodexSandboxMode {
    fn as_cli_value(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CodexReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
}

impl CodexReasoningEffort {
    fn as_api_value(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ClaudeApprovalMode {
    Ask,
    AutoApprove,
    Plan,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ClaudeEffortLevel {
    Default,
    Low,
    Medium,
    High,
    Max,
}

impl ClaudeEffortLevel {
    fn as_cli_value(self) -> Option<&'static str> {
        match self {
            Self::Default => None,
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::Max => Some("max"),
        }
    }
}

impl ClaudeApprovalMode {
    fn initial_cli_permission_mode(self) -> Option<&'static str> {
        match self {
            Self::Plan => Some("plan"),
            Self::Ask | Self::AutoApprove => None,
        }
    }

    fn session_cli_permission_mode(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Ask | Self::AutoApprove => "default",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum CursorMode {
    Agent,
    Plan,
    Ask,
}

impl CursorMode {
    fn as_acp_value(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Plan => "plan",
            Self::Ask => "ask",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum GeminiApprovalMode {
    Default,
    AutoEdit,
    Yolo,
    Plan,
}

impl GeminiApprovalMode {
    fn as_cli_value(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AutoEdit => "auto_edit",
            Self::Yolo => "yolo",
            Self::Plan => "plan",
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum SessionStatus {
    Active,
    Idle,
    Approval,
    Error,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Author {
    You,
    Assistant,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum CommandStatus {
    Running,
    Success,
    Error,
}

impl CommandStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Success => "success",
            Self::Error => "error",
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum ChangeType {
    Edit,
    Create,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum ApprovalDecision {
    Pending,
    Interrupted,
    Canceled,
    Accepted,
    AcceptedForSession,
    Rejected,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct MessageImageAttachment {
    byte_size: usize,
    file_name: String,
    media_type: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PendingPrompt {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<MessageImageAttachment>,
    id: String,
    timestamp: String,
    text: String,
    #[serde(
        default,
        rename = "expandedText",
        skip_serializing_if = "Option::is_none"
    )]
    expanded_text: Option<String>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Message {
    Text {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<MessageImageAttachment>,
        id: String,
        timestamp: String,
        author: Author,
        text: String,
        #[serde(
            default,
            rename = "expandedText",
            skip_serializing_if = "Option::is_none"
        )]
        expanded_text: Option<String>,
    },
    Thinking {
        id: String,
        timestamp: String,
        author: Author,
        title: String,
        lines: Vec<String>,
    },
    Command {
        id: String,
        timestamp: String,
        author: Author,
        command: String,
        #[serde(
            default,
            rename = "commandLanguage",
            skip_serializing_if = "Option::is_none"
        )]
        command_language: Option<String>,
        output: String,
        #[serde(
            default,
            rename = "outputLanguage",
            skip_serializing_if = "Option::is_none"
        )]
        output_language: Option<String>,
        status: CommandStatus,
    },
    Diff {
        id: String,
        timestamp: String,
        author: Author,
        #[serde(rename = "filePath")]
        file_path: String,
        summary: String,
        diff: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        #[serde(rename = "changeType")]
        change_type: ChangeType,
    },
    Markdown {
        id: String,
        timestamp: String,
        author: Author,
        title: String,
        markdown: String,
    },
    #[serde(rename = "subagentResult")]
    SubagentResult {
        id: String,
        timestamp: String,
        author: Author,
        title: String,
        summary: String,
        #[serde(
            default,
            rename = "conversationId",
            skip_serializing_if = "Option::is_none"
        )]
        conversation_id: Option<String>,
        #[serde(default, rename = "turnId", skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
    },
    Approval {
        id: String,
        timestamp: String,
        author: Author,
        title: String,
        command: String,
        #[serde(
            default,
            rename = "commandLanguage",
            skip_serializing_if = "Option::is_none"
        )]
        command_language: Option<String>,
        detail: String,
        decision: ApprovalDecision,
    },
}

impl Message {
    fn id(&self) -> &str {
        match self {
            Self::Text { id, .. }
            | Self::Thinking { id, .. }
            | Self::Command { id, .. }
            | Self::Diff { id, .. }
            | Self::Markdown { id, .. }
            | Self::SubagentResult { id, .. }
            | Self::Approval { id, .. } => id,
        }
    }

    fn preview_text(&self) -> Option<String> {
        match self {
            Self::Text {
                text, attachments, ..
            } => Some(prompt_preview_text(text, attachments)),
            Self::Thinking { title, .. } => Some(make_preview(title)),
            Self::Markdown { title, .. } => Some(make_preview(title)),
            Self::Approval { title, .. } => Some(make_preview(title)),
            Self::Diff { summary, .. } => Some(make_preview(summary)),
            Self::SubagentResult { .. } => None,
            Self::Command { .. } => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalRequest {
    decision: ApprovalDecision,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionRequest {
    agent: Option<Agent>,
    name: Option<String>,
    workdir: Option<String>,
    project_id: Option<String>,
    model: Option<String>,
    approval_policy: Option<CodexApprovalPolicy>,
    reasoning_effort: Option<CodexReasoningEffort>,
    sandbox_mode: Option<CodexSandboxMode>,
    cursor_mode: Option<CursorMode>,
    claude_approval_mode: Option<ClaudeApprovalMode>,
    claude_effort: Option<ClaudeEffortLevel>,
    gemini_approval_mode: Option<GeminiApprovalMode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateProjectRequest {
    name: Option<String>,
    root_path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateAppSettingsRequest {
    default_codex_reasoning_effort: Option<CodexReasoningEffort>,
    default_claude_effort: Option<ClaudeEffortLevel>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileQuery {
    path: String,
    session_id: Option<String>,
    project_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriteFileRequest {
    path: String,
    content: String,
    session_id: Option<String>,
}

#[derive(Serialize)]
struct FileResponse {
    path: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
enum FileSystemEntryKind {
    Directory,
    File,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DirectoryEntry {
    kind: FileSystemEntryKind,
    name: String,
    path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DirectoryResponse {
    entries: Vec<DirectoryEntry>,
    name: String,
    path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GitStatusFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    index_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_path: Option<String>,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    worktree_status: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GitStatusResponse {
    ahead: usize,
    behind: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    files: Vec<GitStatusFile>,
    is_clean: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    upstream: Option<String>,
    workdir: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum GitDiffSection {
    Staged,
    Unstaged,
}

impl GitDiffSection {
    fn as_key(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Unstaged => "unstaged",
        }
    }

    fn summary_label(self) -> &'static str {
        match self {
            Self::Staged => "Staged",
            Self::Unstaged => "Unstaged",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum GitFileAction {
    Stage,
    Unstage,
    Revert,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitDiffRequest {
    original_path: Option<String>,
    path: String,
    section_id: GitDiffSection,
    session_id: Option<String>,
    project_id: Option<String>,
    #[serde(default)]
    status_code: Option<String>,
    workdir: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum GitDiffChangeType {
    Edit,
    Create,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GitDiffResponse {
    change_type: GitDiffChangeType,
    diff: String,
    diff_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    summary: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GitCommitResponse {
    status: GitStatusResponse,
    summary: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitFileActionRequest {
    action: GitFileAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_path: Option<String>,
    path: String,
    session_id: Option<String>,
    project_id: Option<String>,
    #[serde(default)]
    status_code: Option<String>,
    workdir: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitCommitRequest {
    message: String,
    session_id: Option<String>,
    project_id: Option<String>,
    workdir: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorResponse {
    error: String,
}

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
}

#[derive(Deserialize)]
struct SendMessageRequest {
    text: String,
    #[serde(default, rename = "expandedText")]
    expanded_text: Option<String>,
    #[serde(default)]
    attachments: Vec<SendMessageAttachmentRequest>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendMessageAttachmentRequest {
    data: String,
    file_name: Option<String>,
    media_type: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateSessionSettingsRequest {
    name: Option<String>,
    model: Option<String>,
    approval_policy: Option<CodexApprovalPolicy>,
    reasoning_effort: Option<CodexReasoningEffort>,
    sandbox_mode: Option<CodexSandboxMode>,
    cursor_mode: Option<CursorMode>,
    claude_approval_mode: Option<ClaudeApprovalMode>,
    claude_effort: Option<ClaudeEffortLevel>,
    gemini_approval_mode: Option<GeminiApprovalMode>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionModelOption {
    label: String,
    value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    badges: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    supported_claude_effort_levels: Vec<ClaudeEffortLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_reasoning_effort: Option<CodexReasoningEffort>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    supported_reasoning_efforts: Vec<CodexReasoningEffort>,
}

impl SessionModelOption {
    #[cfg(test)]
    fn plain(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            description: None,
            badges: Vec::new(),
            supported_claude_effort_levels: Vec::new(),
            default_reasoning_effort: None,
            supported_reasoning_efforts: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rate_limits: Option<CodexRateLimits>,
}

impl CodexState {
    fn is_empty(&self) -> bool {
        self.rate_limits.is_none()
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexRateLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    credits: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plan_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    primary: Option<CodexRateLimitWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secondary: Option<CodexRateLimitWindow>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexRateLimitWindow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resets_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    used_percent: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window_duration_mins: Option<u64>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
enum AgentReadinessStatus {
    Ready,
    Missing,
    NeedsSetup,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentReadiness {
    agent: Agent,
    status: AgentReadinessStatus,
    blocking: bool,
    detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command_path: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StateResponse {
    revision: u64,
    #[serde(default, skip_serializing_if = "CodexState::is_empty")]
    codex: CodexState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    agent_readiness: Vec<AgentReadiness>,
    preferences: AppPreferences,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    projects: Vec<Project>,
    sessions: Vec<Session>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionResponse {
    session_id: String,
    state: StateResponse,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateProjectResponse {
    project_id: String,
    state: StateResponse,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentCommand {
    name: String,
    description: String,
    content: String,
    source: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentCommandsResponse {
    commands: Vec<AgentCommand>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PickProjectRootResponse {
    path: Option<String>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum DeltaEvent {
    MessageCreated {
        revision: u64,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "messageIndex")]
        message_index: usize,
        message: Message,
        preview: String,
        status: SessionStatus,
    },
    TextDelta {
        revision: u64,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "messageIndex")]
        message_index: usize,
        delta: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        preview: Option<String>,
    },
    CommandUpdate {
        revision: u64,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "messageIndex")]
        message_index: usize,
        command: String,
        #[serde(rename = "commandLanguage", skip_serializing_if = "Option::is_none")]
        command_language: Option<String>,
        output: String,
        #[serde(rename = "outputLanguage", skip_serializing_if = "Option::is_none")]
        output_language: Option<String>,
        status: CommandStatus,
        preview: String,
    },
}

fn resolve_requested_path(path: &str) -> Result<PathBuf, ApiError> {
    let raw_path = FsPath::new(path);
    let resolved = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|err| ApiError::internal(format!("failed to resolve cwd: {err}")))?
            .join(raw_path)
    };

    Ok(resolved)
}

#[derive(Clone, Copy)]
enum ScopedPathMode {
    ExistingFile,
    ExistingPath,
    AllowMissingLeaf,
}

fn required_session_id(session_id: Option<&str>) -> Result<&str, ApiError> {
    let Some(session_id) = session_id.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err(ApiError::bad_request("sessionId is required"));
    };
    Ok(session_id)
}

fn normalize_optional_identifier(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|candidate| !candidate.is_empty())
}

fn resolve_session_project_root_path(
    state: &AppState,
    session_id: &str,
) -> Result<PathBuf, ApiError> {
    let root_path = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = &inner.sessions[index];
        record
            .session
            .project_id
            .as_deref()
            .and_then(|project_id| inner.find_project(project_id))
            .map(|project| project.root_path.clone())
            .or_else(|| {
                inner
                    .find_project_for_workdir(&record.session.workdir)
                    .map(|project| project.root_path.clone())
            })
            .unwrap_or_else(|| record.session.workdir.clone())
    };

    fs::canonicalize(&root_path).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound => {
            ApiError::bad_request(format!("project root not found: {root_path}"))
        }
        _ => ApiError::internal(format!(
            "failed to resolve project root {}: {err}",
            root_path
        )),
    })
}

fn resolve_project_root_path_by_id(
    state: &AppState,
    project_id: &str,
) -> Result<PathBuf, ApiError> {
    let root_path = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .find_project(project_id)
            .ok_or_else(|| ApiError::not_found("project not found"))?
            .root_path
            .clone()
    };

    fs::canonicalize(&root_path).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound => {
            ApiError::bad_request(format!("project root not found: {root_path}"))
        }
        _ => ApiError::internal(format!(
            "failed to resolve project root {}: {err}",
            root_path
        )),
    })
}

fn resolve_request_project_root_path(
    state: &AppState,
    session_id: Option<&str>,
    project_id: Option<&str>,
) -> Result<PathBuf, ApiError> {
    if let Some(session_id) = normalize_optional_identifier(session_id) {
        return resolve_session_project_root_path(state, session_id);
    }

    if let Some(project_id) = normalize_optional_identifier(project_id) {
        return resolve_project_root_path_by_id(state, project_id);
    }

    Err(ApiError::bad_request("sessionId or projectId is required"))
}

fn resolve_project_scoped_requested_path(
    state: &AppState,
    session_id: Option<&str>,
    project_id: Option<&str>,
    path: &str,
    mode: ScopedPathMode,
) -> Result<PathBuf, ApiError> {
    let project_root = resolve_request_project_root_path(state, session_id, project_id)?;
    let requested_path = resolve_requested_path(path)?;
    let resolved_path = match mode {
        ScopedPathMode::ExistingFile => canonicalize_existing_path(&requested_path, "file")?,
        ScopedPathMode::ExistingPath => canonicalize_existing_path(&requested_path, "path")?,
        ScopedPathMode::AllowMissingLeaf => canonicalize_path_with_existing_ancestor(&requested_path)?,
    };

    if resolved_path != project_root && !resolved_path.starts_with(&project_root) {
        return Err(ApiError::bad_request(format!(
            "path `{}` must stay inside project `{}`",
            requested_path.display(),
            project_root.display()
        )));
    }

    Ok(normalize_user_facing_path(&resolved_path))
}

fn resolve_session_scoped_requested_path(
    state: &AppState,
    session_id: &str,
    path: &str,
    mode: ScopedPathMode,
) -> Result<PathBuf, ApiError> {
    resolve_project_scoped_requested_path(state, Some(session_id), None, path, mode)
}

fn canonicalize_existing_path(path: &FsPath, label: &str) -> Result<PathBuf, ApiError> {
    fs::canonicalize(path).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound => ApiError::bad_request(format!("{label} not found: {}", path.display())),
        _ => ApiError::internal(format!("failed to resolve {label} {}: {err}", path.display())),
    })
}

fn canonicalize_path_with_existing_ancestor(path: &FsPath) -> Result<PathBuf, ApiError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|err| ApiError::internal(format!("failed to resolve cwd: {err}")))?
            .join(path)
    };

    let mut suffix = Vec::new();
    let mut probe = absolute.as_path();

    loop {
        match fs::metadata(probe) {
            Ok(_) => {
                let mut canonical = fs::canonicalize(probe).map_err(|err| {
                    ApiError::internal(format!(
                        "failed to resolve path {}: {err}",
                        probe.display()
                    ))
                })?;
                for component in suffix.iter().rev() {
                    canonical.push(component);
                }
                return Ok(canonical);
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let Some(name) = probe.file_name().map(|value| value.to_os_string()) else {
                    return Err(ApiError::bad_request(format!(
                        "path not found: {}",
                        absolute.display()
                    )));
                };
                suffix.push(name);
                let Some(parent) = probe.parent() else {
                    return Err(ApiError::bad_request(format!(
                        "path not found: {}",
                        absolute.display()
                    )));
                };
                probe = parent;
            }
            Err(err) => {
                return Err(ApiError::internal(format!(
                    "failed to resolve path {}: {err}",
                    probe.display()
                )));
            }
        }
    }
}

fn normalize_user_facing_path(path: &FsPath) -> PathBuf {
    #[cfg(windows)]
    {
        let raw = path.to_string_lossy();
        if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = raw.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }

    path.to_path_buf()
}

fn resolve_directory_path(path: &str, label: &str) -> Result<String, ApiError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request(format!("{label} cannot be empty")));
    }

    let resolved = resolve_requested_path(trimmed)?;
    let directory = if resolved.is_dir() {
        resolved
    } else {
        return Err(ApiError::bad_request(format!(
            "`{}` is not a directory",
            trimmed
        )));
    };
    let canonical = fs::canonicalize(&directory).unwrap_or(directory);
    Ok(canonical.to_string_lossy().into_owned())
}

fn resolve_project_root_path(path: &str) -> Result<String, ApiError> {
    resolve_directory_path(path, "project root path")
}

fn resolve_session_workdir(path: &str) -> Result<String, ApiError> {
    resolve_directory_path(path, "session workdir")
}

fn pick_project_root_path(default_workdir: &str) -> Result<Option<String>, ApiError> {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("osascript")
            .arg("-e")
            .arg("on run argv")
            .arg("-e")
            .arg("set defaultLocation to POSIX file (item 1 of argv)")
            .arg("-e")
            .arg("try")
            .arg("-e")
            .arg(
                "set chosenFolder to choose folder with prompt \"Choose a folder for this project\" default location defaultLocation",
            )
            .arg("-e")
            .arg("return POSIX path of chosenFolder")
            .arg("-e")
            .arg("on error number -128")
            .arg("-e")
            .arg("return \"\"")
            .arg("-e")
            .arg("end try")
            .arg("-e")
            .arg("end run")
            .arg(default_workdir)
            .output()
            .map_err(|err| ApiError::internal(format!("failed to open folder picker: {err}")))?;

        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            let message = if detail.is_empty() {
                "folder picker failed".to_owned()
            } else {
                format!("folder picker failed: {detail}")
            };
            return Err(ApiError::internal(message));
        }

        let selected = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if selected.is_empty() {
            return Ok(None);
        }

        return resolve_project_root_path(&selected).map(Some);
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = default_workdir;
        Err(ApiError::bad_request(
            "Folder picker is unavailable on this platform. Enter the path manually.",
        ))
    }
}

fn default_project_name(root_path: &str) -> String {
    let path = FsPath::new(root_path);
    path.file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| root_path.to_owned())
}

fn dedupe_project_name(existing: &[Project], base_name: &str) -> String {
    let existing_names = existing
        .iter()
        .map(|project| project.name.as_str())
        .collect::<HashSet<_>>();
    if !existing_names.contains(base_name) {
        return base_name.to_owned();
    }

    let mut suffix = 2usize;
    loop {
        let candidate = format!("{base_name} {suffix}");
        if !existing_names.contains(candidate.as_str()) {
            return candidate;
        }
        suffix += 1;
    }
}

fn path_contains(root_path: &str, candidate_path: &FsPath) -> bool {
    let root = normalize_path_best_effort(FsPath::new(root_path));
    let candidate = normalize_path_best_effort(candidate_path);
    candidate == root || candidate.starts_with(root)
}

fn normalize_path_best_effort(path: &FsPath) -> PathBuf {
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    fs::canonicalize(&resolved).unwrap_or(resolved)
}

struct ParsedGitBranchStatus {
    ahead: usize,
    behind: usize,
    branch: Option<String>,
    upstream: Option<String>,
}

fn resolve_git_repo_root(workdir: &FsPath) -> Result<Option<PathBuf>, ApiError> {
    let output = Command::new("git")
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

fn normalize_git_status_code(code: char) -> Option<String> {
    match code {
        ' ' => None,
        other => Some(other.to_string()),
    }
}
