// File I/O HTTP routes + agent-command / instruction discovery.
//
// The session-workdir file editor hits three handlers here:
//
// - `read_file` — returns content + metadata (size, mtime, hash)
//   used by the UI editor's "open file" path.
// - `write_file` — optimistic-concurrency write that rejects the
//   save when the on-disk content hash differs from the
//   client-supplied `base_hash` (stale cache detection).
// - `read_directory` — lists a directory's entries for the file
//   tree panel.
//
// `validate_file_base_hash` is the concurrency check shared between
// `write_file` and the patch-file-action route in `api_git.rs`.
//
// The tail of this file owns the two **discovery** routes that scan
// the session workdir for agent-specific configuration:
//
// - `list_agent_commands` — enumerates `.claude/agents/*.md`,
//   `.claude/commands/*.md`, `.cursor/commands/*.md`, Codex
//   `~/.codex/prompts/*.md`, etc. and returns them in a unified
//   `AgentCommand` shape. `read_claude_agent_commands` is the inner
//   Claude-specific scanner.
// - `search_instructions` — phrase-search helper that feeds the UI's
//   slash-command autocomplete popover. Short-circuits to
//   `proxy_remote_search_instructions` for remote sessions.
//
// Both discovery routes short-circuit to the matching `proxy_remote_*`
// helper when the session is remote-backed.


/// Reads file.
async fn read_file(
    State(state): State<AppState>,
    Query(query): Query<FileQuery>,
) -> Result<Json<FileResponse>, ApiError> {
    // Step 1: resolve the path (needs brief mutex access). Use a small blocking
    // scope so we don't compete with streaming delta persists for pool time.
    let resolved_path = {
        let remote_scope = state
            .remote_scope_for_request(query.session_id.as_deref(), query.project_id.as_deref())?;
        if let Some(scope) = remote_scope {
            let response: FileResponse = run_blocking_api({
                let state = state.clone();
                let query_path = query.path.clone();
                move || {
                    state.remote_get_json(
                        &scope,
                        "/api/file",
                        vec![("path".to_owned(), query_path)],
                    )
                }
            })
            .await?;
            return Ok(Json(response));
        }

        resolve_project_scoped_requested_path(
            &state,
            query.session_id.as_deref(),
            query.project_id.as_deref(),
            &query.path,
            ScopedPathMode::ExistingFile,
        )?
    };

    // Step 2: read the file using tokio::fs (async, doesn't block the spawn_blocking pool).
    let metadata = tokio::fs::metadata(&resolved_path)
        .await
        .map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => {
                ApiError::not_found(format!("file not found: {}", resolved_path.display()))
            }
            _ => ApiError::internal(format!(
                "failed to stat file {}: {err}",
                resolved_path.display()
            )),
        })?;
    if metadata.len() > MAX_FILE_CONTENT_BYTES as u64 {
        return Err(ApiError::bad_request(format!(
            "file exceeds the {} MB read limit: {}",
            MAX_FILE_CONTENT_BYTES / (1024 * 1024),
            resolved_path.display()
        )));
    }
    let content = tokio::fs::read_to_string(&resolved_path)
        .await
        .map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => {
                ApiError::not_found(format!("file not found: {}", resolved_path.display()))
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

    let response_metadata = tokio::fs::metadata(&resolved_path).await.ok();
    let metadata_for_response = response_metadata.as_ref().unwrap_or(&metadata);
    let response = FileResponse {
        path: resolved_path.to_string_lossy().into_owned(),
        content_hash: Some(file_content_hash(content.as_bytes())),
        mtime_ms: file_metadata_mtime_ms(metadata_for_response),
        size_bytes: Some(metadata_for_response.len()),
        content,
        language: infer_language_from_path(&resolved_path).map(str::to_owned),
    };
    Ok(Json(response))
}

/// Writes file.
fn validate_file_base_hash(resolved_path: &FsPath, base_hash: &str) -> Result<(), ApiError> {
    // The editor sends the content hash it originally loaded. Refuse stale
    // saves by default so agent/user edits do not silently clobber each other;
    // explicit overwrite is the only bypass.
    let current_metadata = fs::metadata(resolved_path).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound => ApiError::conflict(format!(
            "file changed on disk before save: {} was deleted",
            resolved_path.display()
        )),
        _ => ApiError::internal(format!(
            "failed to stat file before save {}: {err}",
            resolved_path.display()
        )),
    })?;
    if current_metadata.len() > MAX_FILE_CONTENT_BYTES as u64 {
        return Err(ApiError::conflict(format!(
            "file changed on disk before save: {} exceeds the read limit",
            resolved_path.display()
        )));
    }

    let current_content = fs::read(resolved_path).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound => ApiError::conflict(format!(
            "file changed on disk before save: {} was deleted",
            resolved_path.display()
        )),
        _ => ApiError::internal(format!(
            "failed to read file before save {}: {err}",
            resolved_path.display()
        )),
    })?;
    let current_hash = file_content_hash(&current_content);
    if current_hash != base_hash {
        return Err(ApiError::conflict(format!(
            "file changed on disk before save: {}",
            resolved_path.display()
        )));
    }

    Ok(())
}

async fn write_file(
    State(state): State<AppState>,
    Json(request): Json<WriteFileRequest>,
) -> Result<Json<FileResponse>, ApiError> {
    let response = run_blocking_api(move || {
        state.ensure_read_only_delegation_allows_session_write_action(
            request.session_id.as_deref(),
            "file writes",
        )?;
        if let Some(scope) = state.remote_scope_for_request(
            request.session_id.as_deref(),
            request.project_id.as_deref(),
        )? {
            return state.remote_put_json(
                &scope,
                "/api/file",
                json!({
                    "path": request.path.clone(),
                    "content": request.content.clone(),
                    "baseHash": request.base_hash.clone(),
                    "overwrite": request.overwrite,
                }),
            );
        }

        state.ensure_read_only_delegation_allows_write_action(
            request.session_id.as_deref(),
            request.project_id.as_deref(),
            None,
            "file writes",
        )?;

        if request.content.as_bytes().len() > MAX_FILE_CONTENT_BYTES {
            return Err(ApiError::bad_request(format!(
                "file content exceeds the {} MB write limit",
                MAX_FILE_CONTENT_BYTES / (1024 * 1024)
            )));
        }

        let mut resolved_path = resolve_project_scoped_requested_path(
            &state,
            request.session_id.as_deref(),
            request.project_id.as_deref(),
            &request.path,
            ScopedPathMode::AllowMissingLeaf,
        )?;
        let should_check_base = request
            .base_hash
            .as_deref()
            .map(str::trim)
            .filter(|hash| !hash.is_empty())
            .filter(|_| !request.overwrite);
        if let Some(base_hash) = should_check_base {
            validate_file_base_hash(&resolved_path, base_hash)?;
        }
        if let Some(parent) = resolved_path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                ApiError::internal(format!(
                    "failed to create parent directory for {}: {err}",
                    resolved_path.display()
                ))
            })?;
        }
        resolved_path = verify_scoped_write_path_after_parent_creation(
            &state,
            request.session_id.as_deref(),
            request.project_id.as_deref(),
            &resolved_path,
        )?;

        let content_hash = file_content_hash(request.content.as_bytes());
        if let Some(base_hash) = should_check_base {
            validate_file_base_hash(&resolved_path, base_hash)?;
        }
        fs::write(&resolved_path, request.content.as_bytes()).map_err(|err| {
            ApiError::internal(format!(
                "failed to write file {}: {err}",
                resolved_path.display()
            ))
        })?;
        let metadata = fs::metadata(&resolved_path).ok();

        Ok(FileResponse {
            path: resolved_path.to_string_lossy().into_owned(),
            content_hash: Some(content_hash),
            mtime_ms: metadata.as_ref().and_then(file_metadata_mtime_ms),
            size_bytes: metadata.map(|metadata| metadata.len()),
            content: request.content,
            language: infer_language_from_path(&resolved_path).map(str::to_owned),
        })
    })
    .await?;
    Ok(Json(response))
}

/// Reads directory.
async fn read_directory(
    State(state): State<AppState>,
    Query(query): Query<FileQuery>,
) -> Result<Json<DirectoryResponse>, ApiError> {
    let response = run_blocking_api(move || {
        if let Some(scope) = state
            .remote_scope_for_request(query.session_id.as_deref(), query.project_id.as_deref())?
        {
            return state.remote_get_json(
                &scope,
                "/api/fs",
                vec![("path".to_owned(), query.path.clone())],
            );
        }

        let resolved_path = resolve_project_scoped_requested_path(
            &state,
            query.session_id.as_deref(),
            query.project_id.as_deref(),
            &query.path,
            ScopedPathMode::ExistingPath,
        )?;
        let metadata = fs::metadata(&resolved_path).map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => {
                ApiError::not_found(format!("path not found: {}", resolved_path.display()))
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
                (FileSystemEntryKind::Directory, FileSystemEntryKind::File) => {
                    std::cmp::Ordering::Less
                }
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

        Ok(DirectoryResponse {
            name: resolved_path
                .file_name()
                .and_then(|value| value.to_str())
                .map(str::to_owned)
                .unwrap_or_else(|| resolved_path.to_string_lossy().into_owned()),
            path: resolved_path.to_string_lossy().into_owned(),
            entries,
        })
    })
    .await?;
    Ok(Json(response))
}

/// Lists agent commands.
async fn list_agent_commands(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<AgentCommandsResponse>, ApiError> {
    let response = run_blocking_api(move || state.list_agent_commands(&session_id)).await?;
    Ok(Json(response))
}

/// Searches instructions.
async fn search_instructions(
    Query(query): Query<InstructionSearchQuery>,
    State(state): State<AppState>,
) -> Result<Json<InstructionSearchResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.search_instructions(&query.session_id, &query.q)).await?;
    Ok(Json(response))
}

/// Reads Claude agent commands.
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
            kind: AgentCommandKind::PromptTemplate,
            name: stem.to_owned(),
            description,
            content,
            source: format!(".claude/commands/{}.md", stem),
            argument_hint: None,
        });
    }

    commands.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
    });
    Ok(commands)
}
