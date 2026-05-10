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

/// Resolves an agent command into the prompt payload used for send/delegate.
async fn resolve_agent_command(
    AxumPath((session_id, command_name)): AxumPath<(String, String)>,
    State(state): State<AppState>,
    payload: std::result::Result<Json<ResolveAgentCommandRequest>, JsonRejection>,
) -> Result<Json<ResolveAgentCommandResponse>, ApiError> {
    let Json(request) = payload.map_err(|rejection| api_json_rejection("request", rejection))?;
    let response =
        run_blocking_api(move || state.resolve_agent_command(&session_id, &command_name, request))
            .await?;
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

        let raw_content = fs::read_to_string(&path).map_err(|err| {
            ApiError::internal(format!(
                "failed to read agent command {}: {err}",
                path.display()
            ))
        })?;
        let command_content = strip_markdown_frontmatter(&raw_content);
        let content = command_content.content.to_owned();
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        let description = command_content
            .description
            .unwrap_or_else(|| fallback_agent_command_description(&content));

        commands.push(AgentCommand {
            kind: AgentCommandKind::PromptTemplate,
            name: stem.to_owned(),
            description,
            content,
            source: format!(".claude/commands/{}.md", stem),
            argument_hint: command_content.argument_hint,
        });
    }

    commands.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
    });
    Ok(commands)
}

struct MarkdownCommandContent<'a> {
    content: &'a str,
    description: Option<String>,
    argument_hint: Option<String>,
}

fn strip_markdown_frontmatter(content: &str) -> MarkdownCommandContent<'_> {
    let opening_len = if content.starts_with("---\r\n") {
        "---\r\n".len()
    } else if content.starts_with("---\n") {
        "---\n".len()
    } else {
        return MarkdownCommandContent {
            content,
            description: None,
            argument_hint: None,
        };
    };

    let mut offset = opening_len;
    for line in content[opening_len..].split_inclusive('\n') {
        let line_without_newline = line.trim_end_matches(['\r', '\n']);
        if line_without_newline.trim() == "---" {
            let frontmatter = &content[opening_len..offset];
            if looks_like_markdown_frontmatter(frontmatter) {
                let fields = markdown_frontmatter_fields(frontmatter);
                let body = strip_single_leading_blank_line(&content[offset + line.len()..]);
                return MarkdownCommandContent {
                    content: body,
                    description: fields.get("description").cloned(),
                    argument_hint: fields
                        .get("argument-hint")
                        .or_else(|| fields.get("argument_hint"))
                        .cloned(),
                };
            }
            return MarkdownCommandContent {
                content,
                description: None,
                argument_hint: None,
            };
        }
        offset += line.len();
    }

    MarkdownCommandContent {
        content,
        description: None,
        argument_hint: None,
    }
}

fn fallback_agent_command_description(content: &str) -> String {
    content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && *line != "---")
        .unwrap_or("")
        .to_owned()
}

fn strip_single_leading_blank_line(content: &str) -> &str {
    if let Some(rest) = content.strip_prefix("\r\n") {
        rest
    } else if let Some(rest) = content.strip_prefix('\n') {
        rest
    } else {
        content
    }
}

fn read_agent_command_resolver_metadata(
    workdir: &FsPath,
    command: &AgentCommand,
) -> Result<Option<AgentCommandResolverMetadata>, ApiError> {
    if command.kind != AgentCommandKind::PromptTemplate {
        return Ok(None);
    }

    let source = command.source.replace('\\', "/");
    let Some(source_stem) = source
        .strip_prefix(".claude/commands/")
        .and_then(|value| value.strip_suffix(".md"))
    else {
        return Ok(None);
    };
    if source_stem.contains('/') || !source_stem.eq_ignore_ascii_case(command.name.trim()) {
        return Ok(None);
    }

    let path = workdir
        .join(".claude")
        .join("commands")
        .join(format!("{source_stem}.md"));
    let raw_content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(ApiError::internal(format!(
            "failed to read agent command metadata {}: {err}",
            path.display()
            )));
        }
    };
    let Some(frontmatter) = markdown_frontmatter(&raw_content) else {
        return Ok(None);
    };

    parse_agent_command_resolver_metadata(frontmatter)
}

fn looks_like_markdown_frontmatter(frontmatter: &str) -> bool {
    frontmatter.lines().map(str::trim_end).any(|line| {
        if line.starts_with(' ') || line.starts_with('\t') {
            return false;
        }
        let Some((key, _)) = line.split_once(':') else {
            return false;
        };
        let key = key.trim();
        matches!(
            key,
            "name"
                | "description"
                | "metadata"
                | "argument-hint"
                | "argument_hint"
                | "allowed-tools"
                | "tools"
                | "model"
                | "disable-model-invocation"
                | "disable_model_invocation"
        )
    })
}

fn markdown_frontmatter(content: &str) -> Option<&str> {
    let opening_len = if content.starts_with("---\r\n") {
        "---\r\n".len()
    } else if content.starts_with("---\n") {
        "---\n".len()
    } else {
        return None;
    };

    let mut offset = opening_len;
    for line in content[opening_len..].split_inclusive('\n') {
        let line_without_newline = line.trim_end_matches(['\r', '\n']);
        if line_without_newline.trim() == "---" {
            let frontmatter = &content[opening_len..offset];
            return looks_like_markdown_frontmatter(frontmatter).then_some(frontmatter);
        }
        offset += line.len();
    }

    None
}

fn parse_agent_command_resolver_metadata(
    frontmatter: &str,
) -> Result<Option<AgentCommandResolverMetadata>, ApiError> {
    let fields = markdown_frontmatter_fields(frontmatter);
    if !fields
        .keys()
        .any(|key| key.starts_with("metadata.termal."))
    {
        return Ok(None);
    }

    let title = match fields.get("metadata.termal.title.strategy").map(String::as_str) {
        None | Some("default") => AgentCommandTitleStrategy::Default,
        Some("prefixFirstArgument") => {
            let prefix = required_frontmatter_field(
                &fields,
                "metadata.termal.title.prefix",
                "prefixFirstArgument title metadata requires metadata.termal.title.prefix",
            )?;
            AgentCommandTitleStrategy::PrefixFirstArgument { prefix }
        }
        Some(other) => {
            return Err(ApiError::bad_request(format!(
                "unsupported metadata.termal.title.strategy `{other}`"
            )));
        }
    };

    let delegation = match fields.get("metadata.termal.delegation.enabled") {
        None => None,
        Some(value) => {
            if !parse_frontmatter_bool(value, "metadata.termal.delegation.enabled")? {
                None
            } else {
                let mode = parse_agent_command_delegation_mode(required_frontmatter_field(
                    &fields,
                    "metadata.termal.delegation.mode",
                    "delegation metadata requires metadata.termal.delegation.mode",
                )?)?;
                let write_policy =
                    parse_agent_command_delegation_write_policy(required_frontmatter_field(
                        &fields,
                        "metadata.termal.delegation.writePolicy.kind",
                        "delegation metadata requires metadata.termal.delegation.writePolicy.kind",
                    )?)?;
                Some(AgentCommandDelegationMetadata { mode, write_policy })
            }
        }
    };

    Ok(Some(AgentCommandResolverMetadata { title, delegation }))
}

fn markdown_frontmatter_fields(frontmatter: &str) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    let mut path: Vec<(usize, String)> = Vec::new();
    for line in frontmatter.lines() {
        let line = line.trim_end();
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((raw_key, raw_value)) = trimmed.split_once(':') else {
            continue;
        };
        let indent = line.len().saturating_sub(trimmed.len());
        if line[..indent].contains('\t') {
            continue;
        }
        while path
            .last()
            .is_some_and(|(existing_indent, _)| *existing_indent >= indent)
        {
            path.pop();
        }

        let key = raw_key.trim();
        if key.is_empty() {
            continue;
        }

        let value = raw_value.trim();
        if value.is_empty() {
            path.push((indent, key.to_owned()));
            continue;
        }

        let mut field_path = path
            .iter()
            .map(|(_, key)| key.as_str())
            .collect::<Vec<_>>();
        field_path.push(key);
        fields.insert(
            field_path.join("."),
            unquote_markdown_frontmatter_string(value).to_owned(),
        );
    }
    fields
}

fn required_frontmatter_field(
    fields: &BTreeMap<String, String>,
    key: &str,
    message: &str,
) -> Result<String, ApiError> {
    fields
        .get(key)
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ApiError::bad_request(message))
}

fn parse_frontmatter_bool(value: &str, key: &str) -> Result<bool, ApiError> {
    match value.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(ApiError::bad_request(format!(
            "unsupported {key} value `{other}`"
        ))),
    }
}

fn parse_agent_command_delegation_mode(value: String) -> Result<DelegationMode, ApiError> {
    match value.as_str() {
        "reviewer" => Ok(DelegationMode::Reviewer),
        "explorer" => Ok(DelegationMode::Explorer),
        "worker" => Err(ApiError::bad_request(
            "metadata.termal.delegation.mode `worker` is not supported yet",
        )),
        other => Err(ApiError::bad_request(format!(
            "unsupported metadata.termal.delegation.mode `{other}`"
        ))),
    }
}

fn parse_agent_command_delegation_write_policy(
    value: String,
) -> Result<DelegationWritePolicy, ApiError> {
    match value.as_str() {
        "readOnly" => Ok(DelegationWritePolicy::ReadOnly),
        "isolatedWorktree" => Ok(DelegationWritePolicy::IsolatedWorktree {
            owned_paths: Vec::new(),
            worktree_path: None,
        }),
        "sharedWorktree" => Err(ApiError::bad_request(
            "metadata.termal.delegation.writePolicy.kind `sharedWorktree` is not supported yet",
        )),
        other => Err(ApiError::bad_request(format!(
            "unsupported metadata.termal.delegation.writePolicy.kind `{other}`"
        ))),
    }
}

fn unquote_markdown_frontmatter_string(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}
