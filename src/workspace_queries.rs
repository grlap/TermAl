// Workspace-scoped read/write helpers that don't fit the turn or
// session lifecycles: saved multi-pane "workspace layout" CRUD and
// two content queries that walk the session's workdir on disk.
//
// Workspace layouts (list / get / put / delete) are user-curated
// bindings of pane trees that persist across restarts; they live in
// `StateInner.workspace_layouts` and flow through the same commit +
// broadcast pipeline as sessions (see `sse_broadcast.rs`). Serialized
// summaries are built by `collect_workspace_layout_summaries` so list
// responses are cheap.
//
// `list_agent_commands` and `search_instructions` are UI discovery
// helpers: the former enumerates agent-specific command / instruction
// files shipped with the session's workdir; the latter does a phrase
// search across instruction / system-prompt files so the UI can offer
// slash-command autocomplete. Both short-circuit to the matching
// `proxy_remote_*` helper in `remote_routes.rs` when the session is
// remote-backed — on remotes the on-disk content is owned by the
// remote host, so this backend forwards the query rather than
// answering locally.

impl AppState {
    /// Returns lightweight summaries of every saved workspace layout
    /// in this app (name + revision + updated_at — no per-pane trees).
    /// Used by the "workspaces" dropdown in the UI.
    fn list_workspace_layouts(&self) -> Result<WorkspaceLayoutsResponse, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        Ok(WorkspaceLayoutsResponse {
            workspaces: collect_workspace_layout_summaries(inner.workspace_layouts.values()),
        })
    }

    /// Returns the full pane-tree plus metadata for a single saved
    /// workspace layout by id. 404s if the id is unknown.
    fn get_workspace_layout(
        &self,
        workspace_id: &str,
    ) -> Result<WorkspaceLayoutResponse, ApiError> {
        let workspace_id = normalize_optional_identifier(Some(workspace_id))
            .ok_or_else(|| ApiError::bad_request("workspace id is required"))?;
        let inner = self.inner.lock().expect("state mutex poisoned");
        let layout = inner
            .workspace_layouts
            .get(workspace_id)
            .cloned()
            .ok_or_else(|| ApiError::not_found("workspace layout not found"))?;
        Ok(WorkspaceLayoutResponse { layout })
    }

    /// Stores workspace layout.
    /// Creates or updates a workspace layout (by id). Bumps its
    /// revision, persists the change, and broadcasts a full state
    /// snapshot so all clients see the new pane tree. The revision
    /// check in the request body lets the UI detect concurrent edits
    /// and reject stale writes with a 409.
    fn put_workspace_layout(
        &self,
        workspace_id: &str,
        request: PutWorkspaceLayoutRequest,
    ) -> Result<WorkspaceLayoutResponse, ApiError> {
        let workspace_id = normalize_optional_identifier(Some(workspace_id))
            .ok_or_else(|| ApiError::bad_request("workspace id is required"))?
            .to_owned();
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let existing_layout = inner.workspace_layouts.get(&workspace_id).cloned();
        let next_revision = existing_layout
            .as_ref()
            .map(|layout| layout.revision.saturating_add(1))
            .unwrap_or(1);
        let layout = WorkspaceLayoutDocument {
            id: workspace_id.clone(),
            revision: next_revision,
            updated_at: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            control_panel_side: request.control_panel_side,
            theme_id: request.theme_id,
            style_id: request.style_id,
            font_size_px: request.font_size_px,
            editor_font_size_px: request.editor_font_size_px,
            density_percent: request.density_percent,
            workspace: request.workspace,
        };
        inner.workspace_layouts.insert(workspace_id, layout.clone());
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!(
                "failed to persist workspace layout update: {err:#}"
            ))
        })?;
        Ok(WorkspaceLayoutResponse { layout })
    }

    /// Removes a saved workspace layout by id. No-op (not an error)
    /// if the id wasn't present — the UI treats "delete" as
    /// idempotent.
    fn delete_workspace_layout(
        &self,
        workspace_id: &str,
    ) -> Result<WorkspaceLayoutsResponse, ApiError> {
        let workspace_id = normalize_optional_identifier(Some(workspace_id))
            .ok_or_else(|| ApiError::bad_request("workspace id is required"))?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if inner.workspace_layouts.remove(workspace_id).is_none() {
            return Err(ApiError::not_found("workspace layout not found"));
        }
        let workspaces = collect_workspace_layout_summaries(inner.workspace_layouts.values());
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!(
                "failed to persist workspace layout deletion: {err:#}"
            ))
        })?;
        Ok(WorkspaceLayoutsResponse { workspaces })
    }

    /// Enumerates the agent-specific command / instruction files
    /// discoverable from the session's workdir, consulting the
    /// session's cached snapshot to avoid re-scanning the disk on
    /// every call. Remote-backed sessions forward to
    /// `proxy_remote_list_agent_commands` — the remote host owns the
    /// filesystem.
    fn list_agent_commands(
        &self,
        session_id: &str,
    ) -> std::result::Result<AgentCommandsResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_list_agent_commands(session_id);
        }

        let (session, cached_agent_commands) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            (
                inner.sessions[index].session.clone(),
                inner.sessions[index].agent_commands.clone(),
            )
        };

        let filesystem_commands = read_claude_agent_commands(FsPath::new(&session.workdir))?;
        let commands = if session.agent == Agent::Claude {
            merge_agent_commands(&cached_agent_commands, &filesystem_commands)
        } else {
            filesystem_commands
        };

        Ok(AgentCommandsResponse { commands })
    }

    /// Resolves a discovered agent command into the concrete payload used by
    /// regular sends and delegation sends. Remote-backed sessions forward the
    /// whole resolution request so command template behavior stays owned by
    /// the host that owns the session workdir.
    fn resolve_agent_command(
        &self,
        session_id: &str,
        command_name: &str,
        request: ResolveAgentCommandRequest,
    ) -> Result<ResolveAgentCommandResponse, ApiError> {
        let command_name = normalize_optional_identifier(Some(command_name))
            .ok_or_else(|| ApiError::bad_request("agent command name is required"))?;
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_resolve_agent_command(session_id, command_name, request);
        }

        let session = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            inner.sessions[index].session.clone()
        };
        let command = self
            .list_agent_commands(session_id)?
            .commands
            .into_iter()
            .find(|command| command.name.eq_ignore_ascii_case(command_name))
            .ok_or_else(|| ApiError::not_found("agent command not found"))?;
        let metadata =
            read_agent_command_resolver_metadata(FsPath::new(&session.workdir), &command)?;

        resolve_agent_command_payload(command, request, metadata)
    }

    /// Phrase-searches instruction / system-prompt files under the
    /// session's workdir and returns match locations for the UI's
    /// slash-command autocomplete. Remote-backed sessions forward to
    /// `proxy_remote_search_instructions`.
    fn search_instructions(
        &self,
        session_id: &str,
        query: &str,
    ) -> std::result::Result<InstructionSearchResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_search_instructions(session_id, query);
        }

        let session = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            inner.sessions[index].session.clone()
        };

        search_instruction_phrase(FsPath::new(&session.workdir), query)
    }
}

fn resolve_agent_command_payload(
    command: AgentCommand,
    request: ResolveAgentCommandRequest,
    metadata: Option<AgentCommandResolverMetadata>,
) -> Result<ResolveAgentCommandResponse, ApiError> {
    let arguments = request
        .arguments
        .as_deref()
        .map(str::trim)
        .unwrap_or_default();
    let note = request
        .note
        .as_deref()
        .map(str::trim)
        .unwrap_or_default();
    let visible_prompt = resolved_agent_command_visible_prompt(&command, arguments);
    let title = resolved_agent_command_title(&command, metadata.as_ref(), arguments, &visible_prompt);

    let expanded_prompt = match command.kind {
        AgentCommandKind::PromptTemplate => {
            let expanded = command.content.split("$ARGUMENTS").collect::<Vec<_>>().join(arguments);
            Some(append_resolved_agent_command_note(expanded, note))
        }
        AgentCommandKind::NativeSlash => {
            if !note.is_empty() {
                return Err(ApiError::bad_request(
                    "native slash commands do not support additional notes",
                ));
            }
            None
        }
    };

    let delegation = (request.intent == AgentCommandResolveIntent::Delegate)
        .then(|| resolved_agent_command_delegation_defaults(metadata.as_ref(), &title))
        .flatten();

    Ok(ResolveAgentCommandResponse {
        name: command.name,
        source: command.source,
        kind: command.kind,
        visible_prompt,
        expanded_prompt,
        title: Some(title),
        delegation,
    })
}

struct AgentCommandResolverMetadata {
    title: AgentCommandTitleStrategy,
    delegation: Option<AgentCommandDelegationMetadata>,
}

enum AgentCommandTitleStrategy {
    Default,
    PrefixFirstArgument { prefix: String },
}

struct AgentCommandDelegationMetadata {
    mode: DelegationMode,
    write_policy: DelegationWritePolicy,
}

fn resolved_agent_command_visible_prompt(command: &AgentCommand, arguments: &str) -> String {
    let slash_command = if command.kind == AgentCommandKind::NativeSlash {
        let content = command.content.trim();
        if content.starts_with('/') && !content.contains('\n') {
            content.to_owned()
        } else {
            format!("/{}", command.name.trim())
        }
    } else {
        format!("/{}", command.name.trim())
    };

    if arguments.is_empty() {
        slash_command
    } else {
        format!("{slash_command} {arguments}")
    }
}

fn append_resolved_agent_command_note(mut prompt: String, note: &str) -> String {
    if note.is_empty() {
        return prompt;
    }

    if !prompt.ends_with('\n') {
        prompt.push_str("\n\n");
    } else if !prompt.ends_with("\n\n") {
        prompt.push('\n');
    }
    prompt.push_str("## Additional User Note\n\n");
    prompt.push_str(note);
    prompt
}

fn resolved_agent_command_title(
    command: &AgentCommand,
    metadata: Option<&AgentCommandResolverMetadata>,
    arguments: &str,
    visible_prompt: &str,
) -> String {
    let title_strategy = metadata
        .map(|metadata| &metadata.title)
        .unwrap_or(&AgentCommandTitleStrategy::Default);
    match title_strategy {
        AgentCommandTitleStrategy::Default => {}
        AgentCommandTitleStrategy::PrefixFirstArgument { prefix } => {
            if let Some(first_argument) = arguments.split_whitespace().next() {
                return truncate_resolved_agent_command_title(&format!(
                    "{prefix} {first_argument}"
                ));
            }
        }
    }

    let title = command
        .description
        .trim()
        .split('\n')
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or(visible_prompt);
    truncate_resolved_agent_command_title(title)
}

fn truncate_resolved_agent_command_title(value: &str) -> String {
    const MAX_AGENT_COMMAND_TITLE_CHARS: usize = 200;
    if value.chars().count() <= MAX_AGENT_COMMAND_TITLE_CHARS {
        return value.to_owned();
    }

    value
        .chars()
        .take(MAX_AGENT_COMMAND_TITLE_CHARS.saturating_sub(3))
        .chain("...".chars())
        .collect()
}

fn resolved_agent_command_delegation_defaults(
    metadata: Option<&AgentCommandResolverMetadata>,
    title: &str,
) -> Option<ResolvedAgentCommandDelegationDefaults> {
    let delegation = metadata?.delegation.as_ref()?;

    Some(ResolvedAgentCommandDelegationDefaults {
        mode: Some(delegation.mode),
        title: Some(title.to_owned()),
        write_policy: Some(delegation.write_policy.clone()),
    })
}
