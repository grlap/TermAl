// Top-level creation/configuration CRUD for `AppState`: sessions,
// projects, and the global `AppSettings`.
//
// Most runtime death transitions live in `session_lifecycle.rs` and
// `turn_lifecycle.rs`. This CRUD layer additionally coordinates immediate
// Engram MCP capability revocation because the stale runtime set must be
// captured and fenced in the same commit that changes or removes its project
// configuration; the terminal record mutation remains in `turn_lifecycle.rs`.
// A new session starts here at `create_session`,
// lands in `StateInner.sessions` with `SessionRuntime::None`, and
// spawns its real runtime lazily on the first prompt through
// `dispatch_turn` (`turn_dispatch.rs`). Session creation itself never
// starts an agent process.
//
// `create_session` flow: resolve the requested workdir (explicit →
// project default → global default); pick the agent (respecting
// cross-family runtime restrictions); pre-refresh the
// `cached_agent_readiness` snapshot *before* taking the state lock so
// `commit_session_created_locked`'s broadcast carries fresh data;
// under the lock, allocate the new session record; for remote-backed
// projects, forward to the remote `create_remote_session_proxy` path;
// publish via
// `commit_session_created_locked` (emits a SessionCreated delta +
// bumps the revision).
//
// `update_app_settings` updates the user's global defaults (default
// agent, model, approval policy, cursor mode, and a few bookmark /
// UI preferences). It invalidates the agent-readiness cache up front
// because the "allowed agents" set can shift and sessions following
// defaults must see the change. Settings are split into "sticky" vs
// "default": sticky values are the hard preference (applied now and
// followed going forward); default values are suggestions only
// consumed by future `create_session` calls.
//
// `create_project` + `delete_project`: Projects are named bundles of
// workdir + remote + per-project default settings. Creating a remote-
// backed project delegates to `create_remote_project_proxy`; local
// projects normalize the workdir path. Deleting a project does not remove
// its sessions — existing sessions keep their absolute workdirs and have
// their `project_id` cleared — but it does immediately stop any live runtime
// carrying the deleted Engram MCP descriptor. Project detachment still uses
// `session_mut_by_index` so `mutation_stamp` bumps persist the
// change. Orchestrator instances that reference the project also
// have their `project_id` cleared for the same reason.

const MAX_DEFAULT_MODEL_CHARS: usize = 200;

fn normalize_default_model_preference(model: String, agent: Agent) -> Result<String, ApiError> {
    let trimmed = model.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("default") {
        return Ok(default_model_preference());
    }
    if trimmed.chars().count() > MAX_DEFAULT_MODEL_CHARS {
        return Err(ApiError::bad_request(format!(
            "{} default model must be at most {} characters",
            agent.name(),
            MAX_DEFAULT_MODEL_CHARS
        )));
    }
    match agent {
        Agent::Claude => validate_claude_default_model_preference(trimmed)?,
        Agent::OpenCode => {
            return normalize_opencode_model(trimmed)
                .map_err(|err| ApiError::bad_request(err.to_string()));
        }
        _ => {}
    }

    Ok(trimmed.to_owned())
}

fn validate_claude_default_model_preference(model: &str) -> Result<(), ApiError> {
    if model.starts_with('-') {
        return Err(ApiError::bad_request(
            "Claude default model must not start with `-`",
        ));
    }
    if model.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "Claude default model must not contain control characters",
        ));
    }
    Ok(())
}

fn set_agent_default_model_if_present<F>(
    preferences: &mut AppPreferences,
    changed: &mut bool,
    requested_model: Option<String>,
    agent: Agent,
    field: F,
) -> Result<(), ApiError>
where
    F: for<'a> FnOnce(&'a mut AppPreferences) -> &'a mut String,
{
    let Some(requested_model) = requested_model else {
        return Ok(());
    };

    let normalized_model = normalize_default_model_preference(requested_model, agent)?;
    let target = field(preferences);
    if *target != normalized_model {
        *target = normalized_model;
        *changed = true;
    }

    Ok(())
}

fn project_engram_session_ids_locked(inner: &StateInner, project_id: &str) -> Vec<String> {
    let mut session_ids = inner
        .sessions
        .iter()
        .filter(|record| record.session.project_id.as_deref() == Some(project_id))
        .map(|record| record.session.id.clone())
        .collect::<BTreeSet<_>>();
    loop {
        let mut changed = false;
        for delegation in &inner.delegations {
            if session_ids.contains(&delegation.parent_session_id) {
                changed |= session_ids.insert(delegation.child_session_id.clone());
            }
        }
        if !changed {
            break;
        }
    }
    session_ids.into_iter().collect()
}

/// Why a carried check's holder is told its session's claim association ended.
const ENGRAM_SETTINGS_ASSOCIATION_RESET_REASON: &str =
    "its project's Engram settings changed before it settled";

/// Whether moving a project from `current` to `next` Engram settings ends
/// its sessions' ordinary claim associations: Engram turned off, another
/// executable, or another authority store. A claim belongs to the store that
/// made it, so none of these may leave it bound.
fn engram_settings_change_ends_session_associations(
    current: Option<&EngramProjectSettings>,
    next: &EngramProjectSettings,
) -> bool {
    let Some(current) = current else {
        return false;
    };
    let store_changed = current.home != next.home
        || current.authority_store_key != next.authority_store_key;
    store_changed
        || (current.enabled && (!next.enabled || current.binary_path != next.binary_path))
}

/// Ends the ordinary claim association of each of `session_ids`: its work
/// binding, named root, carried and running checks, and the host lines about
/// them. Holders of carried checks are told first. Observation custody (the
/// historical checkpoint, active trace, closing jobs, reports and a pending
/// Delete), in-flight worker counters, and ordinary queue facts (operator
/// pause, the Stop-held head) stay with the session. Returns each session's
/// previous state, so a failed commit can restore it.
fn reset_engram_session_associations_locked(
    inner: &mut StateInner,
    session_ids: &[String],
    why: &str,
) -> Vec<(String, EngramSessionState)> {
    let mut previous = Vec::new();
    for session_id in session_ids {
        let Some(index) = inner.find_session_index(session_id) else {
            continue;
        };
        let record = &mut inner.sessions[index];
        let lines = EngramHost::carried_checks_association_reset_lines(record, why);
        let old = record.engram.clone();
        record.engram = EngramSessionState {
            historical_control: old.historical_control.clone(),
            observation_delete_requested: old.observation_delete_requested,
            capture_workers: old.capture_workers.clone(),
            workdir_worktree: old.workdir_worktree.clone(),
            shell_directory: old.shell_directory.clone(),
            runtime_checkpoint: old.runtime_checkpoint.clone(),
            unadmitted_active_turn: old.unadmitted_active_turn.clone(),
            unadmitted_closing_jobs: old.unadmitted_closing_jobs.clone(),
            unadmitted_reports: old.unadmitted_reports.clone(),
            unadmitted_worker_running: old.unadmitted_worker_running,
            #[cfg(test)]
            test_source_root_lines: old.test_source_root_lines.clone(),
            // Context nudges belong to the MCP runtime reset below, which
            // snapshots and invalidates them for its sessions.
            context_nudge_pending: old.context_nudge_pending,
            pending_context_nudge: old.pending_context_nudge.clone(),
            context_refresh_needed: old.context_refresh_needed,
            context_nudge_generation: old.context_nudge_generation,
            context_nudge_in_progress: old.context_nudge_in_progress,
            context_nudge_in_progress_generation: old.context_nudge_in_progress_generation,
            context_nudge_delivery_generation: old.context_nudge_delivery_generation,
            context_nudge_delivery_turn_generation: old.context_nudge_delivery_turn_generation,
            signalled_compaction_item_ids: old.signalled_compaction_item_ids.clone(),
            operator_paused: old.operator_paused,
            stopped_prompt_id: old.stopped_prompt_id.clone(),
            ..EngramSessionState::default()
        };
        for line in lines {
            eprintln!("engram> session={session_id} {line}");
            record.engram.set_pending_source_root_line(line);
        }
        inner.stamp_session_at_index(index);
        previous.push((session_id.clone(), old));
    }
    previous
}

/// Puts back the session states `reset_engram_session_associations_locked`
/// replaced, after the commit that would have saved the reset failed.
fn restore_engram_session_associations_locked(
    inner: &mut StateInner,
    previous: Vec<(String, EngramSessionState)>,
) {
    for (session_id, engram) in previous {
        if let Some(index) = inner.find_session_index(&session_id) {
            inner.sessions[index].engram = engram;
            // The restored image must reach a delta writer past its old stamp.
            inner.stamp_session_at_index(index);
        }
    }
}

/// A runtime still needs explicit Engram cleanup when an installed descriptor
/// or a previous failed teardown proves that the live agent process may retain
/// an MCP child. Project deletion uses this broad predicate even when persisted
/// Engram settings are already disabled.
fn session_has_live_revocable_engram_mcp_runtime(record: &SessionRecord) -> bool {
    !matches!(record.runtime, SessionRuntime::None)
        && (record.engram_mcp_installed.is_some() || record.engram_mcp_runtime_quarantined)
}

/// Settings updates escalate to immediate project-wide cleanup only for a live
/// runtime whose earlier teardown was quarantined. A merely installed healthy
/// descriptor can keep the documented deferred-rotation behavior.
fn project_has_live_quarantined_engram_mcp_runtime_locked(
    inner: &StateInner,
    project_id: &str,
) -> bool {
    project_engram_session_ids_locked(inner, project_id)
        .iter()
        .any(|session_id| {
            inner
                .find_session_index(session_id)
                .and_then(|index| inner.sessions.get(index))
                .is_some_and(|record| {
                    record.engram_mcp_runtime_quarantined
                        && !matches!(record.runtime, SessionRuntime::None)
                })
        })
}

/// Connection inputs visible to an Engram MCP runtime. Observation preferences
/// are deliberately excluded: changing them never rebuilds a provider runtime.
#[derive(Clone, PartialEq, Eq)]
struct ProjectEngramMcpRuntimeFingerprint {
    binary_path: String,
    home: String,
    store_key: Option<EngramAuthorityStoreKey>,
}

fn project_engram_mcp_runtime_fingerprint(
    project: &Project,
) -> Option<ProjectEngramMcpRuntimeFingerprint> {
    if project.remote_id != LOCAL_REMOTE_ID {
        return None;
    }
    let settings = project.engram.as_ref()?;
    if !settings.is_base_enabled() {
        return None;
    }
    Some(ProjectEngramMcpRuntimeFingerprint {
        binary_path: settings.binary_path.clone()?,
        home: settings.home.clone()?,
        store_key: settings.authority_store_key.clone(),
    })
}

fn validate_engram_project_home_for_update(
    settings: &EngramProjectSettings,
) -> Result<(), ApiError> {
    let Some(raw_home) = settings.home.as_deref() else {
        return Ok(());
    };
    let home = raw_home.trim();
    if home.is_empty() || !FsPath::new(home).is_absolute() {
        return Err(ApiError::bad_request(
            "Engram home must be a non-empty absolute path",
        ));
    }
    if settings.enabled && !FsPath::new(home).is_dir() {
        return Err(ApiError::bad_request(
            "Engram home must be an existing absolute directory when enabled",
        ));
    }
    Ok(())
}

fn default_engram_home_path() -> Result<PathBuf, ApiError> {
    let base = std::env::var_os("USERPROFILE")
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var_os("HOME").filter(|value| !value.is_empty()))
        .map(PathBuf::from)
        .ok_or_else(|| {
            ApiError::bad_request(
                "cannot resolve the default Engram home because USERPROFILE/HOME is unavailable",
            )
        })?;
    Ok(base.join(".engram"))
}

fn expand_engram_home_path(raw_home: &str) -> Result<PathBuf, ApiError> {
    let trimmed = raw_home.trim();
    let user_profile_prefix = "%USERPROFILE%";
    if trimmed
        .get(..user_profile_prefix.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(user_profile_prefix))
    {
        let base = std::env::var_os("USERPROFILE")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| {
                ApiError::bad_request(
                    "cannot expand %USERPROFILE% because USERPROFILE is unavailable",
                )
            })?;
        let suffix = trimmed[user_profile_prefix.len()..].trim_start_matches(['/', '\\']);
        return Ok(if suffix.is_empty() {
            base
        } else {
            base.join(suffix)
        });
    }
    if trimmed == "~" || trimmed.starts_with("~/") || trimmed.starts_with("~\\") {
        let base = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .or_else(|| std::env::var_os("USERPROFILE").filter(|value| !value.is_empty()))
            .map(PathBuf::from)
            .ok_or_else(|| {
                ApiError::bad_request("cannot expand ~ because HOME/USERPROFILE is unavailable")
            })?;
        let suffix = trimmed[1..].trim_start_matches(['/', '\\']);
        return Ok(if suffix.is_empty() {
            base
        } else {
            base.join(suffix)
        });
    }
    Ok(PathBuf::from(trimmed))
}

fn normalize_engram_project_paths(settings: &mut EngramProjectSettings) -> Result<(), ApiError> {
    let requires_runtime_paths = settings.enabled;
    if requires_runtime_paths && settings.binary_path.is_none() {
        settings.binary_path = Some("engram".to_owned());
    }
    if requires_runtime_paths && settings.home.is_none() {
        settings.home = Some(default_engram_home_path()?.to_string_lossy().into_owned());
    }
    if let Some(home) = settings.home.as_deref() {
        settings.home = Some(
            expand_engram_home_path(home)?
                .to_string_lossy()
                .into_owned(),
        );
    }
    Ok(())
}

fn normalize_engram_host_settings(
    settings: EngramHostSettings,
) -> Result<EngramHostSettings, ApiError> {
    let developer_name = settings.developer_name.trim().to_ascii_lowercase();
    if developer_name.is_empty() {
        return Err(ApiError::bad_request(
            "developerName must contain at least one character",
        ));
    }
    if developer_name.len() > 64 {
        return Err(ApiError::bad_request(
            "developerName must contain at most 64 characters",
        ));
    }
    if !developer_name
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte))
    {
        return Err(ApiError::bad_request(
            "developerName may contain only lowercase ASCII letters, digits, `.`, `_`, and `-`",
        ));
    }
    let binary_path = settings.binary_path.trim();
    let binary_path = if binary_path.is_empty() {
        default_engram_binary_path()
    } else {
        binary_path.to_owned()
    };
    let raw_home = settings.home.trim();
    let home = if raw_home.is_empty() {
        default_engram_home_path()?
    } else {
        expand_engram_home_path(raw_home)?
    };
    let project_settings = EngramProjectSettings {
        acceptance_evaluation: None,
        enabled: true,
        execution_observability: true,
        binary_path: Some(binary_path.clone()),
        home: Some(home.to_string_lossy().into_owned()),
        authority_store_key: None,
        deadline_ms: None,
    };
    let (binary_path, home) = validate_engram_host_connection_paths(&project_settings)?;
    Ok(EngramHostSettings {
        developer_name,
        binary_path: binary_path.to_string_lossy().into_owned(),
        home: home.to_string_lossy().into_owned(),
    })
}

fn append_engram_mcp_notices_locked(
    inner: &mut StateInner,
    session_ids: &[String],
    notice: &str,
) -> Vec<(String, usize)> {
    let mut previous_message_lengths = Vec::new();
    for session_id in session_ids {
        let Some(index) = inner.find_session_index(session_id) else {
            continue;
        };
        if !inner.sessions[index].is_local_session()
            || matches!(inner.sessions[index].runtime, SessionRuntime::None)
        {
            continue;
        }
        previous_message_lengths.push((
            session_id.clone(),
            inner.sessions[index].session.messages.len(),
        ));
        let message_id = inner.next_message_id();
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        push_message_on_record(
            record,
            Message::Text {
                attachments: Vec::new(),
                id: message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: notice.to_owned(),
                expanded_text: None,
                source: None,
            },
        );
    }
    previous_message_lengths
}

fn append_engram_mcp_rotation_notices_locked(
    inner: &mut StateInner,
    session_ids: &[String],
) -> Vec<(String, usize)> {
    const NOTICE: &str = "Engram connection changed. This runtime will use the current configuration on the next send.";
    append_engram_mcp_notices_locked(inner, session_ids, NOTICE)
}

#[cfg(test)]
const ENGRAM_CLEANUP_WARNING_SEPARATOR: &str = "\n\n";

#[cfg(test)]
fn engram_cleanup_warning_contains(warning: Option<&str>, notice: &str) -> bool {
    warning.is_some_and(|warning| {
        warning
            .split(ENGRAM_CLEANUP_WARNING_SEPARATOR)
            .any(|candidate| candidate == notice)
    })
}

#[cfg(test)]
fn add_engram_cleanup_warning(warning: Option<&str>, notice: &str) -> Option<String> {
    match warning {
        Some(existing) if engram_cleanup_warning_contains(Some(existing), notice) => {
            Some(existing.to_owned())
        }
        Some(existing) => Some(format!(
            "{existing}{ENGRAM_CLEANUP_WARNING_SEPARATOR}{notice}"
        )),
        None => Some(notice.to_owned()),
    }
}

#[cfg(test)]
fn remove_engram_cleanup_warning(warning: Option<&str>, notice: &str) -> Option<String> {
    let remaining = warning?
        .split(ENGRAM_CLEANUP_WARNING_SEPARATOR)
        .filter(|candidate| *candidate != notice)
        .collect::<Vec<_>>();
    (!remaining.is_empty()).then(|| remaining.join(ENGRAM_CLEANUP_WARNING_SEPARATOR))
}

#[cfg(test)]
fn append_engram_cleanup_degraded_notices_locked(
    inner: &mut StateInner,
    session_ids: &[String],
    notice: &str,
) -> Vec<(String, usize)> {
    let mut previous_message_lengths = Vec::new();
    for session_id in session_ids {
        let Some(index) = inner.find_session_index(session_id) else {
            continue;
        };
        if !inner.sessions[index].is_local_session()
            || inner.sessions[index]
                .session
                .messages
                .iter()
                .any(|message| matches!(message, Message::Text { text, .. } if text == notice))
        {
            continue;
        }
        previous_message_lengths.push((
            session_id.clone(),
            inner.sessions[index].session.messages.len(),
        ));
        let message_id = inner.next_message_id();
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        push_message_on_record(
            record,
            Message::Text {
                attachments: Vec::new(),
                id: message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: notice.to_owned(),
                expanded_text: None,
                source: None,
            },
        );
    }
    previous_message_lengths
}

fn rollback_engram_mcp_rotation_notices_locked(
    inner: &mut StateInner,
    previous_message_lengths: &[(String, usize)],
) {
    for (session_id, previous_len) in previous_message_lengths {
        let Some(index) = inner.find_session_index(session_id) else {
            continue;
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        for message in &record.session.messages.window(*previous_len..) {
            record
                .body_sequence
                .unrepresented_messages
                .remove(message.id());
        }
        record.session.messages.truncate(*previous_len);
        record.message_positions = build_message_positions(&record.session.messages);
        sync_retained_transcript_metadata(record);
    }
}

/// Runtime reset changes only these fields. Preserve a deferred compaction
/// refresh as well as its undelivered page when persisting settings fails.
struct EngramMcpRuntimeResetSnapshot {
    session_id: String,
    runtime_reset_required: bool,
    context_nudge_generation: u64,
    context_nudge_pending: bool,
    pending_context_nudge: Option<String>,
    context_refresh_needed: bool,
}

fn mark_engram_mcp_runtime_resets_locked(
    inner: &mut StateInner,
    session_ids: &[String],
) -> Vec<EngramMcpRuntimeResetSnapshot> {
    let mut previous = Vec::new();
    for session_id in session_ids {
        let Some(index) = inner.find_session_index(session_id) else {
            continue;
        };
        if !inner.sessions[index].is_local_session() {
            continue;
        }
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        previous.push(EngramMcpRuntimeResetSnapshot {
            session_id: session_id.clone(),
            runtime_reset_required: record.runtime_reset_required,
            context_nudge_generation: record.engram.context_nudge_generation,
            context_nudge_pending: record.engram.context_nudge_pending,
            pending_context_nudge: record.engram.pending_context_nudge.clone(),
            context_refresh_needed: record.engram.context_refresh_needed,
        });
        record.runtime_reset_required = true;
        record.engram.invalidate_context_nudge();
    }
    previous
}

fn restore_engram_mcp_runtime_resets_locked(
    inner: &mut StateInner,
    previous: Vec<EngramMcpRuntimeResetSnapshot>,
) {
    for snapshot in previous {
        if let Some(index) = inner.find_session_index(&snapshot.session_id) {
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            record.runtime_reset_required = snapshot.runtime_reset_required;
            record.engram.context_nudge_generation = snapshot.context_nudge_generation;
            record.engram.context_nudge_pending = snapshot.context_nudge_pending;
            record.engram.pending_context_nudge = snapshot.pending_context_nudge;
            record.engram.context_refresh_needed = snapshot.context_refresh_needed;
        }
    }
}

fn project_matches_engram_connection_snapshot(project: &Project, snapshot: &Project) -> bool {
    project.id == snapshot.id
        && project.name == snapshot.name
        && project.root_path == snapshot.root_path
        && project.remote_id == snapshot.remote_id
        && project.remote_project_id == snapshot.remote_project_id
        && project.engram == snapshot.engram
}

#[cfg(test)]
thread_local! {
    static TEST_ENGRAM_SETTINGS_MUTATION_BOUNDARY: std::cell::RefCell<
        Option<(&'static str, Box<dyn FnOnce()>)>
    > = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn run_engram_settings_mutation_boundary(boundary: &'static str) {
    TEST_ENGRAM_SETTINGS_MUTATION_BOUNDARY.with(|slot| {
        let callback = {
            let mut slot = slot.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|(expected, _)| *expected == boundary)
            {
                slot.take().map(|(_, callback)| callback)
            } else {
                None
            }
        };
        if let Some(callback) = callback {
            callback();
        }
    });
}

impl AppState {
    #[cfg(test)]
    fn note_engram_cleanup_degraded(&self, project_id: &str, notice: &str, context: &str) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(project_index) = inner
            .projects
            .iter()
            .position(|project| project.id == project_id)
        else {
            return;
        };
        let previous_project_warning = inner.projects[project_index].engram_cleanup_warning.clone();
        let next_project_warning =
            add_engram_cleanup_warning(previous_project_warning.as_deref(), notice);
        let project_warning_changed = previous_project_warning != next_project_warning;
        if project_warning_changed {
            inner.projects[project_index].engram_cleanup_warning = next_project_warning;
        }
        let session_ids = project_engram_session_ids_locked(&inner, project_id);
        let rollbacks =
            append_engram_cleanup_degraded_notices_locked(&mut inner, &session_ids, notice);
        if !project_warning_changed && rollbacks.is_empty() {
            return;
        }
        if let Err(error) = self.commit_locked(&mut inner) {
            inner.projects[project_index].engram_cleanup_warning = previous_project_warning;
            rollback_engram_mcp_rotation_notices_locked(&mut inner, &rollbacks);
            eprintln!(
                "engram> project={project_id} failed to persist {context} warning: {error:#}"
            );
        }
    }

    #[cfg(test)]
    fn clear_engram_cleanup_warning_if_matches(
        &self,
        project_id: &str,
        notice: &str,
        context: &str,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(project_index) = inner
            .projects
            .iter()
            .position(|project| project.id == project_id)
        else {
            return;
        };
        let previous_project_warning = inner.projects[project_index].engram_cleanup_warning.clone();
        let next_project_warning =
            remove_engram_cleanup_warning(previous_project_warning.as_deref(), notice);
        if previous_project_warning == next_project_warning {
            return;
        }
        inner.projects[project_index].engram_cleanup_warning = next_project_warning;
        if let Err(error) = self.commit_locked(&mut inner) {
            inner.projects[project_index].engram_cleanup_warning = previous_project_warning;
            eprintln!(
                "engram> project={project_id} failed to persist resolved {context} warning: {error:#}"
            );
        }
    }

    fn shutdown_revoked_engram_mcp_runtimes(
        &self,
        batch: EngramMcpRuntimeRevocationBatch,
        context: &str,
    ) -> EngramMcpRuntimeRevocationShutdownBatch {
        let mut result = EngramMcpRuntimeRevocationShutdownBatch {
            pending_session_ids: batch.pending_session_ids,
            ..EngramMcpRuntimeRevocationShutdownBatch::default()
        };

        for target in batch.targets {
            let session_id = target.session_id.clone();
            // The exact runtime stop owner fences callbacks during teardown.
            let runtime_context = format!("{context} for session `{session_id}`");
            let (shutdown_error, retain_runtime_for_retry, suppress_codex_thread_resume) =
                match shutdown_stopped_runtime(target.runtime.clone(), &runtime_context) {
                    Ok(()) => (None, false, false),
                    Err(first_error) if target.runtime.stop_failure_is_best_effort() => {
                        // Preserve the shared app-server for unrelated sessions
                        // and surface the residual descriptor window. When the
                        // authority command itself failed, that old descriptor
                        // may still carry mutation authority until Codex unloads
                        // the detached thread.
                        (
                            Some(format!(
                                "shared Codex interrupt failed after detach; the old thread may remain alive with its prior MCP capabilities until Codex unloads it: {first_error:#}"
                            )),
                            false,
                            true,
                        )
                    }
                    Err(first_error) => {
                        match shutdown_stopped_runtime(target.runtime.clone(), &runtime_context) {
                            Ok(()) => (None, false, false),
                            Err(retry_error) => match target.runtime.process_has_exited() {
                                Ok(true) => (None, false, false),
                                Ok(false) => (
                                    Some(format!(
                                        "initial shutdown failed: {first_error:#}; retry failed: {retry_error:#}"
                                    )),
                                    true,
                                    false,
                                ),
                                Err(status_error) => (
                                    Some(format!(
                                        "initial shutdown failed: {first_error:#}; retry failed: {retry_error:#}; process exit could not be confirmed: {status_error:#}"
                                    )),
                                    true,
                                    false,
                                ),
                            },
                        }
                    }
                };
            result.shutdowns.push(EngramMcpRuntimeRevocationShutdown {
                target,
                shutdown_error,
                retain_runtime_for_retry,
                suppress_codex_thread_resume,
            });
        }

        result
    }

    fn finalize_revoked_engram_mcp_runtimes(
        &self,
        batch: EngramMcpRuntimeRevocationShutdownBatch,
    ) -> EngramMcpRuntimeRevocationOutcome {
        let mut outcome = EngramMcpRuntimeRevocationOutcome {
            pending_session_ids: batch.pending_session_ids,
            ..EngramMcpRuntimeRevocationOutcome::default()
        };
        for shutdown in batch.shutdowns {
            let target = shutdown.target;
            let finalization = self.finish_revoked_engram_mcp_runtime_if_matches(
                &target.session_id,
                &target.token,
                target.owner_generation,
                target.stop_options.as_ref(),
                shutdown.suppress_codex_thread_resume,
                shutdown.retain_runtime_for_retry,
                shutdown.shutdown_error.as_deref(),
            );
            if let Some(completion) = finalization.completion {
                outcome.completions.push(completion);
            }
            for failure in finalization.failures {
                outcome
                    .failures
                    .push(format!("session `{}`: {failure}", target.session_id));
            }
        }
        outcome
    }

    fn resume_revoked_engram_mcp_sessions(&self, outcome: &mut EngramMcpRuntimeRevocationOutcome) {
        let should_resume_orchestrators = outcome
            .completions
            .iter()
            .any(|completion| completion.should_resume_orchestrator_transitions);
        for completion in &outcome.completions {
            if let Some(orchestrator_instance_id) =
                completion.orchestrator_stop_instance_id.as_deref()
            {
                self.note_stopped_orchestrator_session(
                    orchestrator_instance_id,
                    &completion.session_id,
                );
            }
            if completion.should_refresh_delegation {
                if let Err(error) =
                    self.refresh_delegation_for_child_session(&completion.session_id)
                {
                    outcome.failures.push(format!(
                        "session `{}`: failed to refresh delegation lifecycle: {error:#}",
                        completion.session_id
                    ));
                }
            }
            if completion.should_dispatch_next {
                match self.dispatch_next_queued_turn(&completion.session_id, false) {
                    Ok(Some(dispatch)) => {
                        if let Err(error) = deliver_turn_dispatch(self, dispatch)
                            .into_background_result("session restore queue drain")
                        {
                            outcome.failures.push(format!(
                                "session `{}`: failed to deliver queued turn dispatch: {}",
                                completion.session_id, error.message
                            ));
                        }
                    }
                    Ok(None) => {}
                    Err(error) => outcome.failures.push(format!(
                        "session `{}`: failed to dispatch queued turn after revocation: {error:#}",
                        completion.session_id
                    )),
                }
            }
        }
        if should_resume_orchestrators {
            if let Err(error) = self.resume_pending_orchestrator_transitions() {
                outcome.failures.push(format!(
                    "failed to resume orchestrator transitions: {error:#}"
                ));
            }
        }
    }

    fn finish_revoked_engram_mcp_runtime_outcome(
        &self,
        outcome: EngramMcpRuntimeRevocationOutcome,
    ) -> Result<Vec<String>, ApiError> {
        if outcome.failures.is_empty() {
            Ok(outcome.pending_session_ids)
        } else {
            let pending_detail = if outcome.pending_session_ids.is_empty() {
                String::new()
            } else {
                format!(
                    "; pending behind existing Stop owners: {}",
                    outcome.pending_session_ids.join(", ")
                )
            };
            Err(ApiError::internal(format!(
                "Engram capability revocation or runtime cleanup was degraded: {}{}",
                outcome.failures.join("; "),
                pending_detail
            )))
        }
    }

    fn teardown_revoked_engram_mcp_runtimes(
        &self,
        batch: EngramMcpRuntimeRevocationBatch,
        context: &str,
    ) -> Result<Vec<String>, ApiError> {
        let shutdowns = self.shutdown_revoked_engram_mcp_runtimes(batch, context);
        let mut outcome = self.finalize_revoked_engram_mcp_runtimes(shutdowns);
        self.resume_revoked_engram_mcp_sessions(&mut outcome);
        self.finish_revoked_engram_mcp_runtime_outcome(outcome)
    }

    /// Verifies a proposed project Engram configuration without mutating
    /// project state. Save repeats the same checks, so this response is UI
    /// guidance rather than an authorization token.
    fn verify_project_engram_settings(
        &self,
        project_id: &str,
        request: UpdateProjectEngramSettingsRequest,
    ) -> Result<VerifyProjectEngramSettingsResponse, ApiError> {
        let project_id = normalize_optional_identifier(Some(project_id))
            .ok_or_else(|| ApiError::bad_request("project id is required"))?
            .to_owned();
        let (project, host_settings) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let project = inner
                .find_project(&project_id)
                .cloned()
                .ok_or_else(|| ApiError::not_found("project not found"))?;
            (project, inner.preferences.engram.clone())
        };
        if project.remote_id != LOCAL_REMOTE_ID {
            return Err(ApiError::bad_request(
                "Engram settings are available only for local projects",
            ));
        }

        let mut settings = request.into_settings();
        if let Some(defaults) = &mut settings.acceptance_evaluation {
            defaults.normalize()?;
        }
        settings.binary_path = Some(host_settings.binary_path.clone());
        settings.home = Some(host_settings.home.clone());
        settings.deadline_ms = None;
        // Verify always probes the proposed runtime, even when the Enabled
        // toggle is currently off. Save skips readiness when disabled.
        // A successful Verify proves the fields are ready for a later enable.
        settings.enabled = true;
        normalize_engram_project_paths(&mut settings)?;
        validate_engram_project_home_for_update(&settings)?;
        let (binary_path, project_file, home) =
            validate_engram_project_connection_paths(&project, &settings)?;
        let project_root = PathBuf::from(&project.root_path);
        self.validate_engram_diagnostic_snapshot(&project, &host_settings)?;
        let started = std::time::Instant::now();
        let readiness = run_engram_readiness(
            &binary_path,
            &project_file,
            &home,
            &project_root,
            &self.engram_host_launch_workdir(),
        )?;
        let mut errors = Vec::new();
        let store_key = match validate_engram_readiness(&readiness, &project_file, &home) {
            Ok(store_key) => Some(store_key),
            Err(error) => {
                errors.push(error.message);
                None
            }
        };
        settings.authority_store_key = store_key;
        self.validate_engram_diagnostic_snapshot(&project, &host_settings)?;
        let verified = errors.is_empty();
        Ok(VerifyProjectEngramSettingsResponse {
            verified,
            binary_path: settings.binary_path.unwrap_or_default(),
            home: settings.home.unwrap_or_default(),
            project_id: readiness.project_id,
            database: normalize_user_facing_path(&readiness.database)
                .to_string_lossy()
                .into_owned(),
            ready: verified,
            full_audit: "not_run",
            host_path_status: readiness.host_path_policy.status,
            elapsed_ms: started.elapsed().as_millis() as u64,
            errors,
        })
    }

    /// Updates the optional Engram adapter for one local project. Filesystem
    /// checks and `engram readiness` run off-lock; the project identity/root are
    /// fenced before the validated settings are committed.
    #[cfg(test)]
    fn update_project_engram_settings(
        &self,
        project_id: &str,
        settings: EngramProjectSettings,
    ) -> Result<StateResponse, ApiError> {
        self.update_project_engram_settings_checked(project_id, settings, None, false)
    }

    fn patch_project_engram_settings(
        &self,
        project_id: &str,
        request: UpdateProjectEngramSettingsRequest,
    ) -> Result<StateResponse, ApiError> {
        let keep_stored_observability = request.keeps_stored_observability();
        let mut settings = request.into_settings();
        let host_settings = self
            .inner
            .lock()
            .expect("state mutex poisoned")
            .preferences
            .engram
            .clone();
        settings.binary_path = Some(host_settings.binary_path.clone());
        settings.home = Some(host_settings.home.clone());
        settings.deadline_ms = None;
        self.update_project_engram_settings_checked(
            project_id, settings, Some(host_settings), keep_stored_observability,
        )
    }

    /// `keep_stored_observability`: the request did not name Execution
    /// Observability, so a project with stored settings keeps its own choice
    /// rather than the request's default.
    fn update_project_engram_settings_checked(
        &self,
        project_id: &str,
        mut settings: EngramProjectSettings,
        expected_host_settings: Option<EngramHostSettings>,
        keep_stored_observability: bool,
    ) -> Result<StateResponse, ApiError> {
        let project_id = normalize_optional_identifier(Some(project_id))
            .ok_or_else(|| ApiError::bad_request("project id is required"))?
            .to_owned();
        if let Some(defaults) = &mut settings.acceptance_evaluation {
            defaults.normalize()?;
        }
        settings.binary_path = settings.binary_path.and_then(|value| {
            let value = value.trim().to_owned();
            (!value.is_empty()).then_some(value)
        });
        settings.home = settings.home.and_then(|value| {
            let value = value.trim().to_owned();
            (!value.is_empty()).then_some(value)
        });
        if settings
            .deadline_ms
            .is_some_and(|value| value == 0 || value > ENGRAM_MAX_CALL_TIMEOUT_MS)
        {
            return Err(ApiError::bad_request(format!(
                "Engram deadline must be between 1 and {ENGRAM_MAX_CALL_TIMEOUT_MS} ms",
            )));
        }
        let project_snapshot = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if inner.engram_project_resets.contains(&project_id) {
                return Err(ApiError::conflict(
                    "Engram connection change is already in progress",
                ));
            }
            inner
                .find_project(&project_id)
                .cloned()
                .ok_or_else(|| ApiError::not_found("project not found"))?
        };
        if project_snapshot.remote_id != LOCAL_REMOTE_ID {
            return Err(ApiError::bad_request(
                "Engram settings are available only for local projects",
            ));
        }
        if let Some(current) = project_snapshot.engram.as_ref() {
            // Older clients that omit evaluator defaults do not reset them.
            if settings.acceptance_evaluation.is_none() {
                settings.acceptance_evaluation = current.acceptance_evaluation.clone();
            }
            // A partial update never changes observations it does not name.
            if keep_stored_observability {
                settings.execution_observability = current.execution_observability;
            }
            if !settings.enabled {
                if settings.binary_path.is_none() {
                    settings.binary_path.clone_from(&current.binary_path);
                }
                if settings.home.is_none() {
                    settings.home.clone_from(&current.home);
                }
            }
            // The HTTP settings form does not edit a connection's call deadline.
            if expected_host_settings.is_some()
                && current.binary_path == settings.binary_path
                && current.home == settings.home
            {
                settings.deadline_ms = current.deadline_ms;
            }
            settings.authority_store_key = current.authority_store_key.clone();
        }
        normalize_engram_project_paths(&mut settings)?;
        let metadata_only = project_snapshot.engram.as_ref().is_some_and(|current| {
            let mut expected = current.clone();
            expected.execution_observability = settings.execution_observability;
            expected.acceptance_evaluation = settings.acceptance_evaluation.clone();
            expected == settings
        });
        if !metadata_only {
            validate_engram_project_home_for_update(&settings)?;
            if settings.enabled {
                settings.authority_store_key = Some(validate_engram_project_enablement(
                    &project_snapshot,
                    &settings,
                    &self.engram_host_launch_workdir(),
                )?);
            }
        }
        let ends_session_associations = engram_settings_change_ends_session_associations(
            project_snapshot.engram.as_ref(),
            &settings,
        );
        #[cfg(test)]
        run_engram_settings_mutation_boundary("before_commit");
        let (session_ids, connection_claim, adapter, revocations, mut persistence_failures) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let project_index = inner
                .projects
                .iter()
                .position(|project| project.id == project_id)
                .ok_or_else(|| ApiError::not_found("project not found"))?;
            if !project_matches_engram_connection_snapshot(
                &inner.projects[project_index],
                &project_snapshot,
            ) || expected_host_settings
                .as_ref()
                .is_some_and(|host| inner.preferences.engram != *host)
                || inner.engram_project_resets.contains(&project_id)
            {
                return Err(ApiError::conflict(
                    "project changed while Engram settings were being validated",
                ));
            }
            let mut next_project = project_snapshot.clone();
            next_project.engram = Some(settings.clone());
            let connection_changed = project_engram_mcp_runtime_fingerprint(&project_snapshot)
                != project_engram_mcp_runtime_fingerprint(&next_project);
            let unchanged_request = project_snapshot.engram.as_ref() == Some(&settings);
            let retry_cleanup = (!metadata_only || unchanged_request)
                && (project_has_live_quarantined_engram_mcp_runtime_locked(&inner, &project_id)
                    || (!settings.is_base_enabled()
                        && project_engram_session_ids_locked(&inner, &project_id)
                            .iter()
                            .any(|id| {
                                inner.find_session_index(id).is_some_and(|index| {
                                    session_has_live_revocable_engram_mcp_runtime(
                                        &inner.sessions[index],
                                    )
                                })
                            })));
            let session_ids = if connection_changed || retry_cleanup {
                project_engram_session_ids_locked(&inner, &project_id)
            } else {
                Vec::new()
            };
            let connection_generation = if connection_changed || retry_cleanup {
                Some(
                    inner
                        .claim_engram_project_reset(&project_id)
                        .ok_or_else(|| {
                            ApiError::conflict("Engram connection change is already in progress")
                        })?,
                )
            } else {
                None
            };
            inner.projects[project_index].engram = Some(settings.clone());
            // Every session of the project, connection change or not: a claim
            // made in the old store must not stay bound under the new settings.
            let association_resets = if ends_session_associations {
                let affected = project_engram_session_ids_locked(&inner, &project_id);
                reset_engram_session_associations_locked(
                    &mut inner,
                    &affected,
                    ENGRAM_SETTINGS_ASSOCIATION_RESET_REASON,
                )
            } else {
                Vec::new()
            };
            let resets = mark_engram_mcp_runtime_resets_locked(&mut inner, &session_ids);
            let notices = if connection_changed && settings.is_base_enabled() {
                append_engram_mcp_rotation_notices_locked(&mut inner, &session_ids)
            } else {
                Vec::new()
            };
            // Disabling an installed MCP descriptor stops that actual runtime.
            // A preference-only edit never claims a Stop or changes queue state.
            let cleanup_ids = session_ids
                .iter()
                .filter(|id| {
                    inner.find_session_index(id).is_some_and(|index| {
                        let record = &inner.sessions[index];
                        session_has_live_revocable_engram_mcp_runtime(record)
                            && (!settings.is_base_enabled()
                                || record.engram_mcp_runtime_quarantined)
                    })
                })
                .cloned()
                .collect::<Vec<_>>();
            let revocations = claim_engram_mcp_runtime_revocations_locked(&mut inner, &cleanup_ids);
            let committed = self.commit_locked_with_persist_outcome(&mut inner);
            if let Err(error) = committed.as_ref() {
                inner.projects[project_index].engram = project_snapshot.engram.clone();
                restore_engram_mcp_runtime_resets_locked(&mut inner, resets);
                rollback_engram_mcp_rotation_notices_locked(&mut inner, &notices);
                rollback_engram_mcp_runtime_revocations_locked(&mut inner, &revocations);
                // Applied first, so restored last: its whole previous session
                // state wins over any snapshot taken after the reset.
                restore_engram_session_associations_locked(&mut inner, association_resets);
                if let Some(generation) = connection_generation {
                    inner.engram_project_resets.release(&project_id, generation);
                }
                return Err(ApiError::internal(format!(
                    "failed to persist Engram project settings: {error:#}"
                )));
            }
            // Metadata success is independent of a rejected sibling row. The
            // runtime owner still reports rejection of one of its own rows;
            // do not roll back already-durable metadata after partial success.
            let (_, persisted) = committed.expect("metadata commit checked above");
            let persistence_failures = session_ids
                .iter()
                .filter_map(|id| {
                    persisted
                        .require_session_saved(id)
                        .err()
                        .map(|error| format!("{error:#}"))
                })
                .collect::<Vec<_>>();
            let connection_claim = connection_generation.map(|generation| {
                inner
                    .engram_project_reset_claim(&project_id, generation)
                    .expect("connection transition still owns its generation")
            });
            (
                session_ids,
                connection_claim,
                inner.engram_host_adapter.clone(),
                revocations,
                persistence_failures,
            )
        };
        // Connection children are closed under the project generation fence.
        // Existing observation records/outboxes and ordinary work claims stay
        // intact; no remote checkpoint or settlement participates in this path.
        if let Some(claim) = connection_claim.as_ref() {
            for session_id in &session_ids {
                if let Err(error) = adapter.shutdown_session_until(session_id, claim) {
                    persistence_failures.push(format!(
                        "session `{session_id}`: Engram connection cleanup failed: {error:?}"
                    ));
                }
            }
        }
        let shutdowns =
            self.shutdown_revoked_engram_mcp_runtimes(revocations, "Engram connection change");
        let mut outcome = self.finalize_revoked_engram_mcp_runtimes(shutdowns);
        outcome.failures.append(&mut persistence_failures);
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if let Some(claim) = connection_claim.as_ref() {
                if !inner
                    .engram_project_resets
                    .release(&project_id, claim.generation)
                {
                    outcome.failures.push(
                        "Engram connection transition ownership changed during cleanup".to_owned(),
                    );
                }
            }
        }
        self.resume_revoked_engram_mcp_sessions(&mut outcome);
        let pending = self.finish_revoked_engram_mcp_runtime_outcome(outcome)?;
        if !pending.is_empty() {
            return Err(ApiError::conflict(format!(
                "Engram settings were saved; runtime cleanup is pending behind an existing Stop: {}",
                pending.join(", "),
            )));
        }
        for session_id in &session_ids {
            // Normal dispatch honors operator pauses, Stop owners and each
            // prompt's persistence hold. No force-resume is granted here.
            if let Some(dispatch) =
                self.dispatch_next_queued_turn(session_id, false)
                    .map_err(|error| {
                        ApiError::internal(format!(
                            "failed to resume the ordinary queue: {error:#}"
                        ))
                    })?
            {
                deliver_turn_dispatch(self, dispatch)
                    .into_background_result("Engram connection queue drain")?;
            }
        }
        let inner = self.inner.lock().expect("state mutex poisoned");
        Ok(self.snapshot_from_inner(&inner))
    }

    fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<CreateSessionResponse, ApiError> {
        self.create_session_with_agent_setup_validator(request, |agent, workdir| {
            self.validate_agent_session_setup_for_state(agent, workdir)
        })
    }

    /// Creates a session with an injectable unlocked readiness preflight.
    ///
    /// The explicit interleaving point lets lifecycle tests remove a project
    /// during validation without introducing test-only behavior into the
    /// production readiness resolver.
    fn create_session_with_agent_setup_validator(
        &self,
        request: CreateSessionRequest,
        mut validate_agent_session_setup: impl FnMut(Agent, &str) -> Result<(), String>,
    ) -> Result<CreateSessionResponse, ApiError> {
        let agent = request.agent.unwrap_or(Agent::Codex);
        if request.opencode_approval_mode.is_some() && agent != Agent::OpenCode {
            return Err(ApiError::bad_request(
                "opencodeApprovalMode is only supported by OpenCode",
            ));
        }
        if (request.kimi_approval_mode.is_some() || request.kimi_mode.is_some())
            && agent != Agent::Kimi
        {
            return Err(ApiError::bad_request(
                "kimiApprovalMode and kimiMode are only supported by Kimi",
            ));
        }
        if agent == Agent::Kimi
            && (request.sandbox_mode.is_some()
                || request.approval_policy.is_some()
                || request.reasoning_effort.is_some()
                || request.claude_approval_mode.is_some()
                || request.claude_effort.is_some()
                || request.cursor_mode.is_some()
                || request.gemini_approval_mode.is_some())
        {
            return Err(ApiError::bad_request(
                "Kimi sessions support model, kimiApprovalMode and kimiMode settings only",
            ));
        }
        let has_explicit_project = request.project_id.is_some();
        let requested_workdir = request
            .workdir
            .as_deref()
            .map(resolve_session_workdir)
            .transpose()?;
        let requested_model = request
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| {
                if agent.supports_opencode_settings() {
                    normalize_opencode_model(value)
                        .map_err(|err| ApiError::bad_request(err.to_string()))
                } else {
                    Ok(value.to_owned())
                }
            })
            .transpose()?;
        let requested_name = request
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let project = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if let Some(project_id) = request.project_id.as_deref() {
                Some(inner.find_project(project_id).cloned().ok_or_else(|| {
                    ApiError::bad_request(format!("unknown project `{project_id}`"))
                })?)
            } else {
                requested_workdir
                    .as_deref()
                    .and_then(|workdir| inner.find_project_for_workdir(workdir).cloned())
            }
        };
        let workdir = requested_workdir.unwrap_or_else(|| {
            project
                .as_ref()
                .map(|entry| entry.root_path.clone())
                .unwrap_or_else(|| self.default_workdir.clone())
        });
        if let Some(project) = project.as_ref() {
            if project.remote_id != LOCAL_REMOTE_ID {
                let mut remote_request = request;
                remote_request.model = Some(requested_model.clone().unwrap_or_else(|| {
                    let inner = self.inner.lock().expect("state mutex poisoned");
                    inner.preferences.default_model_for_agent(agent)
                }));
                return self.create_remote_session_proxy(remote_request, project.clone());
            }
            if !path_contains(&project.root_path, FsPath::new(&workdir)) {
                return Err(ApiError::bad_request(format!(
                    "session workdir `{workdir}` must stay inside project `{}`",
                    project.name
                )));
            }
        }
        validate_agent_session_setup(agent, &workdir).map_err(ApiError::bad_request)?;
        // Refresh the agent readiness cache before the critical section so that
        // commit_locked's SSE publish and the API response snapshot both carry
        // up-to-date readiness without filesystem I/O under the inner mutex.
        self.invalidate_agent_readiness_cache();
        let _ = self.agent_readiness_snapshot();
        match agent {
            agent if agent.supports_opencode_settings() => {
                if request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.claude_approval_mode.is_some()
                    || request.claude_effort.is_some()
                    || request.cursor_mode.is_some()
                    || request.gemini_approval_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "OpenCode sessions only support model and OpenCode approval settings at creation; choose the dynamic mode after OpenCode reports its live options",
                    ));
                }
            }
            agent if agent.supports_codex_prompt_settings() => {
                if request.claude_approval_mode.is_some()
                    || request.claude_effort.is_some()
                    || request.cursor_mode.is_some()
                    || request.gemini_approval_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Codex sessions only support model, sandbox, approval policy, and reasoning effort settings",
                    ));
                }
            }
            agent if agent.supports_claude_approval_mode() => {
                if request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.cursor_mode.is_some()
                    || request.gemini_approval_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Claude sessions only support model, mode, and effort settings",
                    ));
                }
            }
            agent if agent.supports_cursor_mode() => {
                if request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.claude_approval_mode.is_some()
                    || request.claude_effort.is_some()
                    || request.gemini_approval_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Cursor sessions only support mode settings",
                    ));
                }
            }
            agent if agent.supports_gemini_approval_mode() => {
                if request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.claude_approval_mode.is_some()
                    || request.claude_effort.is_some()
                    || request.cursor_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Gemini sessions only support approval mode settings",
                    ));
                }
            }
            _ => {}
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let project_id = if let Some(preflight_project) = project.as_ref() {
            match inner.find_project(&preflight_project.id).cloned() {
                None if has_explicit_project => {
                    return Err(ApiError::bad_request(format!(
                        "unknown project `{}`",
                        preflight_project.id
                    )));
                }
                None => {
                    // Workdir-derived project attachment is best-effort. If
                    // that inferred owner disappears during readiness
                    // preflight, keep the explicit workdir and create
                    // projectless.
                    None
                }
                Some(current_project)
                    if current_project.remote_id != LOCAL_REMOTE_ID
                        || current_project.root_path != preflight_project.root_path =>
                {
                    if has_explicit_project {
                        return Err(ApiError::conflict(
                            "project changed while creating the session",
                        ));
                    }
                    // The caller did not select this project, so authority
                    // drift invalidates only the inferred attachment.
                    None
                }
                Some(current_project) => Some(current_project.id),
            }
        } else {
            None
        };
        let mut record =
            inner.create_session(agent, requested_name, workdir, project_id, requested_model);
        if agent == Agent::OpenCode {
            if let Some(mode) = request.opencode_approval_mode {
                record.session.opencode_approval_mode = Some(mode);
            }
        }
        if agent == Agent::Kimi {
            // The creation dialog edits a draft of the app default for this
            // one session; `create_session` already applied the default.
            if let Some(mode) = request.kimi_approval_mode {
                record.session.kimi_approval_mode = Some(mode);
            }
            if let Some(mode) = request.kimi_mode {
                record.session.kimi_mode = Some(mode);
            }
        }
        if record.session.agent.supports_codex_prompt_settings() {
            if let Some(sandbox_mode) = request.sandbox_mode {
                record.codex_sandbox_mode = sandbox_mode;
                record.session.sandbox_mode = Some(sandbox_mode);
            }
            if let Some(approval_policy) = request.approval_policy {
                record.codex_approval_policy = approval_policy;
                record.session.approval_policy = Some(approval_policy);
            }
            if let Some(reasoning_effort) = request.reasoning_effort {
                record.codex_reasoning_effort = reasoning_effort;
                record.session.reasoning_effort = Some(reasoning_effort);
            }
        } else if record.session.agent.supports_claude_approval_mode() {
            if let Some(claude_approval_mode) = request.claude_approval_mode {
                record.session.claude_approval_mode = Some(claude_approval_mode);
            }
            if let Some(claude_effort) = request.claude_effort {
                record.session.claude_effort = Some(claude_effort);
            }
        } else if record.session.agent.supports_cursor_mode() {
            if let Some(cursor_mode) = request.cursor_mode {
                record.session.cursor_mode = Some(cursor_mode);
            }
        } else if record.session.agent.supports_gemini_approval_mode() {
            if let Some(gemini_approval_mode) = request.gemini_approval_mode {
                record.session.gemini_approval_mode = Some(gemini_approval_mode);
            }
        }
        if let Some(index) = inner.find_session_index(&record.session.id) {
            if let Some(slot) = inner.sessions.get_mut(index) {
                *slot = record.clone();
            }
            // The whole-struct replace above clobbered the stamp that
            // `push_session` assigned; re-stamp via `session_mut_by_index`
            // so `collect_persist_delta` picks up this rewrite on the
            // next persist tick. The local `record` carries
            // `mutation_stamp: 0` from construction, so skipping this
            // call would leave the row below the persist watermark.
            let _ = inner.session_mut_by_index(index);
        }
        let revision = self
            .commit_session_created_locked(&mut inner, &record)
            .map_err(|err| ApiError::internal(format!("failed to persist session: {err:#}")))?;
        let created_record = inner
            .find_session_index(&record.session.id)
            .and_then(|index| inner.sessions.get(index))
            .expect("just-created session must be present in the index");
        let session = AppState::wire_session_from_record(&self.server_instance_id, created_record);
        let delta_session =
            AppState::wire_session_summary_from_record(&self.server_instance_id, created_record);
        self.publish_delta_locked(
            &inner,
            DeltaEvent::SessionCreated {
                revision,
                session_id: session.id.clone(),
                session: delta_session,
            },
        );
        drop(inner);
        Ok(CreateSessionResponse {
            session_id: session.id.clone(),
            session,
            revision,
            server_instance_id: self.server_instance_id.clone(),
        })
    }

    /// Updates app settings.
    /// Updates the user's global `AppSettings` (default agent/model,
    /// approval policy, cursor mode, bookmarks, etc.) and broadcasts.
    ///
    /// Some of these fields feed the per-session defaults used by
    /// future `create_session` calls; others flip hard behaviour
    /// immediately (for example: which agents to probe during the
    /// readiness scan). The agent-readiness cache is invalidated up
    /// front before taking the state lock so subsequent commits
    /// pick up the new scan shape. Sticky fields (values applied to
    /// existing sessions) vs default fields (values consumed only by
    /// future `create_session` calls) are distinguished by
    /// `persisted_state::AppSettings` field semantics — this method
    /// only writes the settings bag; session propagation happens
    /// lazily as individual sessions pull defaults.
    ///
    /// Settings mutations commit through [`Self::commit_locked`] so
    /// the SSE channel gets a full state snapshot — settings changes
    /// touch many UI surfaces at once and a delta event would be
    /// awkward to fan out reliably.
    fn update_app_settings(
        &self,
        request: UpdateAppSettingsRequest,
    ) -> Result<StateResponse, ApiError> {
        // Normalize remotes outside the lock — pure validation on request data.
        let normalized_remotes = request.remotes.map(normalize_remote_configs).transpose()?;
        // Also before any preference changes, so a rejected effort cannot
        // leave an earlier field of the same request applied.
        let default_kimi_effort = request
            .default_kimi_effort
            .as_deref()
            .map(normalize_default_kimi_effort)
            .transpose()?;

        // Refresh the agent readiness cache before the critical section so that
        // commit_locked's SSE publish and the API response snapshot both carry
        // up-to-date readiness without filesystem I/O under the inner mutex.
        self.invalidate_agent_readiness_cache();
        let _ = self.agent_readiness_snapshot();

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let mut changed = false;

        set_agent_default_model_if_present(
            &mut inner.preferences,
            &mut changed,
            request.default_codex_model,
            Agent::Codex,
            |preferences| &mut preferences.default_codex_model,
        )?;
        if let Some(default_codex_sandbox_mode) = request.default_codex_sandbox_mode {
            if inner.preferences.default_codex_sandbox_mode != default_codex_sandbox_mode {
                inner.preferences.default_codex_sandbox_mode = default_codex_sandbox_mode;
                changed = true;
            }
        }
        if let Some(default_codex_approval_policy) = request.default_codex_approval_policy {
            if inner.preferences.default_codex_approval_policy != default_codex_approval_policy {
                inner.preferences.default_codex_approval_policy = default_codex_approval_policy;
                changed = true;
            }
        }
        set_agent_default_model_if_present(
            &mut inner.preferences,
            &mut changed,
            request.default_claude_model,
            Agent::Claude,
            |preferences| &mut preferences.default_claude_model,
        )?;
        set_agent_default_model_if_present(
            &mut inner.preferences,
            &mut changed,
            request.default_cursor_model,
            Agent::Cursor,
            |preferences| &mut preferences.default_cursor_model,
        )?;
        set_agent_default_model_if_present(
            &mut inner.preferences,
            &mut changed,
            request.default_gemini_model,
            Agent::Gemini,
            |preferences| &mut preferences.default_gemini_model,
        )?;
        set_agent_default_model_if_present(
            &mut inner.preferences,
            &mut changed,
            request.default_kimi_model,
            Agent::Kimi,
            |preferences| &mut preferences.default_kimi_model,
        )?;
        set_agent_default_model_if_present(
            &mut inner.preferences,
            &mut changed,
            request.default_opencode_model,
            Agent::OpenCode,
            |preferences| &mut preferences.default_opencode_model,
        )?;

        if let Some(mode) = request.default_opencode_approval_mode {
            if inner.preferences.default_opencode_approval_mode != mode {
                inner.preferences.default_opencode_approval_mode = mode;
                changed = true;
            }
        }

        if let Some(mode) = request.default_kimi_approval_mode {
            if inner.preferences.default_kimi_approval_mode != mode {
                inner.preferences.default_kimi_approval_mode = mode;
                changed = true;
            }
        }

        if let Some(effort) = default_kimi_effort {
            if inner.preferences.default_kimi_effort != effort {
                inner.preferences.default_kimi_effort = effort;
                changed = true;
            }
        }

        if let Some(default_codex_reasoning_effort) = request.default_codex_reasoning_effort {
            if inner.preferences.default_codex_reasoning_effort != default_codex_reasoning_effort {
                inner.preferences.default_codex_reasoning_effort = default_codex_reasoning_effort;
                changed = true;
            }
        }

        if let Some(default_claude_approval_mode) = request.default_claude_approval_mode {
            if inner.preferences.default_claude_approval_mode != default_claude_approval_mode {
                inner.preferences.default_claude_approval_mode = default_claude_approval_mode;
                changed = true;
            }
        }

        if let Some(default_claude_effort) = request.default_claude_effort {
            if inner.preferences.default_claude_effort != default_claude_effort {
                inner.preferences.default_claude_effort = default_claude_effort;
                changed = true;
            }
        }

        let mut remote_config_publication = None;
        if let Some(normalized_remotes) = normalized_remotes {
            let next_remote_ids: HashSet<&str> = normalized_remotes
                .iter()
                .map(|remote| remote.id.as_str())
                .collect();
            if let Some(project) = inner
                .projects
                .iter()
                .find(|project| !next_remote_ids.contains(project.remote_id.as_str()))
            {
                return Err(ApiError::bad_request(format!(
                    "cannot remove remote `{}` because project `{}` still uses it",
                    project.remote_id, project.name
                )));
            }
            if inner.preferences.remotes != normalized_remotes {
                let previous_remote_ids = inner
                    .preferences
                    .remotes
                    .iter()
                    .map(|remote| remote.id.as_str())
                    .collect::<HashSet<_>>();
                let event_bridge_rearms = normalized_remotes
                    .iter()
                    .filter(|remote| !previous_remote_ids.contains(remote.id.as_str()))
                    .filter(|remote| {
                        inner
                            .sessions
                            .iter()
                            .any(|record| record.remote_id.as_deref() == Some(remote.id.as_str()))
                    })
                    .map(|remote| remote.id.clone())
                    .collect::<Vec<_>>();
                inner.preferences.remotes = normalized_remotes.clone();
                // Publish registry authority before this state guard is
                // released. Live connection teardown happens below, after the
                // guard is dropped, but requests in that interval already fail
                // closed against the new authoritative map.
                let publication = self
                    .remote_registry
                    .publish_configs_with_event_bridge_rearms(
                        &normalized_remotes,
                        &event_bridge_rearms,
                    );
                for remote_id in &publication.changed_ids {
                    let _ = self.clear_remote_applied_revision_locked(&mut inner, remote_id);
                    let _ = self.clear_remote_sse_fallback_resync(remote_id);
                }
                remote_config_publication = Some(publication);
                changed = true;
            }
        }

        let remote_routing_dirty =
            inner.remote_settings_persist_dirty || remote_config_publication.is_some();
        let commit_result = if changed || inner.settings_persist_dirty {
            self.commit_locked(&mut inner)
                .map_err(|err| {
                    ApiError::internal(format!("failed to persist app settings: {err:#}"))
                })
                .map(|_| ())
        } else {
            Ok(())
        };
        if commit_result.is_ok() {
            inner.settings_persist_dirty = false;
            inner.remote_settings_persist_dirty = false;
        } else {
            inner.settings_persist_dirty = true;
            inner.remote_settings_persist_dirty = remote_routing_dirty;
            // The settings mutation and registry publication remain
            // authoritative in memory even though their synchronous write
            // failed. Publish the already-advanced revision so peer SSE
            // clients cannot keep displaying the pre-change route forever.
            self.publish_state_locked(&inner);
        }

        let snapshot = self.snapshot_from_inner(&inner);
        drop(inner);
        if let Some(publication) = remote_config_publication {
            let bridges_to_restart = self.remote_registry.finish_config_publication(publication);
            for remote_id in bridges_to_restart {
                self.start_remote_event_bridge_by_id(&remote_id);
            }
        }
        // Registry publication already retired the old route while the state
        // mutex was held. Teardown and latest-route restart must finish even
        // when persistence fails, otherwise the old process/reader leaks and
        // the in-memory settings authority is left without a bridge.
        if remote_routing_dirty {
            if let Err(error) = &commit_result {
                eprintln!(
                    "app settings warning> remote routing changes are active in memory but failed to persist; a restart may restore the previous routing settings: {}",
                    error.message
                );
            }
        }
        commit_result?;
        Ok(snapshot)
    }

    /// Updates machine-scoped Engram process settings. Enabled projects must
    /// be vetoed first so a host-path rotation cannot silently strand or reuse
    /// project authority against a different store.
    fn update_engram_host_settings(
        &self,
        request: UpdateEngramHostSettingsRequest,
    ) -> Result<StateResponse, ApiError> {
        let normalized = normalize_engram_host_settings(EngramHostSettings {
            developer_name: request.developer_name,
            binary_path: request.binary_path,
            home: request.home,
        })?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if inner.preferences.engram == normalized {
            return Ok(self.snapshot_from_inner(&inner));
        }
        let host_identity_or_paths_changed = inner.preferences.engram.developer_name
            != normalized.developer_name
            || inner.preferences.engram.binary_path != normalized.binary_path
            || inner.preferences.engram.home != normalized.home;
        if host_identity_or_paths_changed
            && inner.projects.iter().any(|project| {
                project
                    .engram
                    .as_ref()
                    .is_some_and(EngramProjectSettings::is_base_enabled)
            })
        {
            return Err(ApiError::bad_request(
                "disable every enabled Engram project before changing the host developer name, binary path, or home",
            ));
        }
        inner.preferences.engram = normalized;
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist Engram host settings: {err:#}"))
        })?;
        Ok(self.snapshot_from_inner(&inner))
    }

    /// Creates a new Project entry (a named bundle of workdir + remote
    /// + per-project default settings).
    ///
    /// Remote-backed projects (those with a resolvable `remote_id` on
    /// the request) delegate to [`Self::create_remote_project_proxy`] so
    /// the real project record lives on the remote and this host only
    /// stores a proxy shell. Local projects normalize the workdir path
    /// through `resolve_session_workdir` and commit only if the
    /// `projects` vec actually grew — idempotent re-creates with the
    /// same id no-op rather than broadcasting redundant state.
    fn create_project(
        &self,
        request: CreateProjectRequest,
    ) -> Result<CreateProjectResponse, ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let remote_id = if request.remote_id.trim().is_empty() {
            default_local_remote_id()
        } else {
            request.remote_id.trim().to_owned()
        };
        let remote = inner
            .find_remote(&remote_id)
            .cloned()
            .ok_or_else(|| ApiError::bad_request(format!("unknown remote `{remote_id}`")))?;
        let trimmed_root_path = request.root_path.trim();
        if trimmed_root_path.is_empty() {
            return Err(ApiError::bad_request("project root path cannot be empty"));
        }
        let root_path = if matches!(remote.transport, RemoteTransport::Local) {
            resolve_project_root_path(trimmed_root_path)?
        } else {
            trimmed_root_path.to_owned()
        };
        if !remote.enabled {
            return Err(ApiError::bad_request(format!(
                "remote `{}` is disabled",
                remote.name
            )));
        }
        if remote_id != LOCAL_REMOTE_ID {
            drop(inner);
            return self.create_remote_project_proxy(request, remote, root_path);
        }
        let existing_len = inner.projects.len();
        let project = inner.create_project(request.name, root_path, remote_id);
        if inner.projects.len() != existing_len {
            self.commit_locked(&mut inner)
                .map_err(|err| ApiError::internal(format!("failed to persist project: {err:#}")))?;
        }
        Ok(CreateProjectResponse {
            project_id: project.id,
            state: self.snapshot_from_inner(&inner),
        })
    }

    /// Deletes the local project reference and keeps its sessions visible
    /// outside project scope. Remote-backed projects are intentionally removed
    /// only from this local state; TermAl does not delete remote project data
    /// from a local project-list action.
    fn delete_project(&self, project_id: &str) -> Result<StateResponse, ApiError> {
        let project_id = normalize_optional_identifier(Some(project_id))
            .ok_or_else(|| ApiError::bad_request("project id is required"))?
            .to_owned();
        let mut snapshot = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if inner.engram_project_resets.contains(&project_id) {
                return Err(ApiError::conflict(
                    "Engram connection change is already in progress",
                ));
            }
            inner
                .find_project(&project_id)
                .cloned()
                .ok_or_else(|| ApiError::not_found("project not found"))?
        };
        if snapshot.remote_id == LOCAL_REMOTE_ID
            && let Some(settings) = snapshot.engram.as_mut()
        {
            settings.enabled = false;
            // Use the ordinary connection owner to stop installed descriptors.
            // No project is detached while that cleanup is failed or pending.
            self.update_project_engram_settings_checked(&project_id, settings.clone(), None, false)?;
        }
        self.delete_project_reference(&project_id, &snapshot)
    }

    fn delete_project_reference(
        &self,
        project_id: &str,
        expected: &Project,
    ) -> Result<StateResponse, ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if inner.engram_project_resets.contains(project_id)
            || !inner.find_project(project_id).is_some_and(|project| {
                project_matches_engram_connection_snapshot(project, expected)
            })
        {
            return Err(ApiError::conflict("project changed before deletion"));
        }
        let Some(project_index) = inner
            .projects
            .iter()
            .position(|project| project.id == project_id)
        else {
            return Err(ApiError::not_found("project not found"));
        };

        let removed_project = inner.projects.remove(project_index);
        // Collect affected indices first so the mutating pass can go
        // through `session_mut_by_index` (which bumps `mutation_stamp`).
        // Iterating `&mut inner.sessions` directly would clear the
        // `project_id` in memory but skip the stamp, causing
        // `collect_persist_delta` to drop these changes — the deleted
        // project would reappear attached to those sessions on restart.
        let affected_session_indices: Vec<usize> = inner
            .sessions
            .iter()
            .enumerate()
            .filter_map(|(idx, record)| {
                if record.session.project_id.as_deref() == Some(project_id) {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect();
        for idx in affected_session_indices {
            if let Some(record) = inner.session_mut_by_index(idx) {
                record.session.project_id = None;
            }
        }
        for instance in &mut inner.orchestrator_instances {
            if instance.project_id == project_id {
                instance.project_id.clear();
            }
        }
        // Boards are local-authoritative in v1. Remote projects can never own
        // a local coordination scope, so fencing their ids would only grow the
        // permanent deleted-scope table and couple remote deletion to an
        // irrelevant coordination.sqlite write.
        if removed_project.remote_id == default_local_remote_id() {
            inner
                .pending_coordination_scope_deletions
                .insert(project_id.to_owned());
        }
        inner
            .pending_response_board_project_detachments
            .insert(project_id.to_owned(), removed_project.name.clone());

        let (_, persist_dispatch) = self
            .commit_locked_with_persist_dispatch(&mut inner)
            .map_err(|err| ApiError::internal(format!("failed to remove project: {err:#}")))?;
        drop(inner);

        // A queued mutation is owned by the persist worker: it first commits
        // the project removal plus the outbox item to termal.sqlite, then
        // signals the dedicated cleanup worker to atomically fence/delete the
        // board scope in coordination.sqlite and queue persistence of the
        // cleared outbox. A disconnected channel makes
        // commit_locked_with_persist_dispatch synchronously persist the
        // primary deletion; only that explicit outcome authorizes synchronous
        // cleanup here. Thread-handle presence is not a durability signal
        // (test and shutdown states can keep a connected sender without a
        // running handle).
        self.finish_project_deletion(project_id, persist_dispatch)
    }

    fn finish_project_deletion(
        &self,
        project_id: &str,
        persist_dispatch: PersistDispatch,
    ) -> Result<StateResponse, ApiError> {
        if persist_dispatch == PersistDispatch::Synchronous {
            self.replay_pending_coordination_scope_deletions()
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to finish project coordination cleanup: {err:#}"
                    ))
                })?;
            self.replay_pending_response_board_project_detachments();
        }

        self.prune_telegram_config_for_deleted_project(project_id)?;

        Ok(self.snapshot())
    }

    fn replay_pending_coordination_scope_deletions(&self) -> Result<()> {
        // This path is used only after the primary persist worker has stopped
        // (or by worker-less test AppStates). Match the dedicated cleanup
        // worker's semantics exactly: every cleanup failure remains in the
        // durable outbox, successful scopes are removed, and an already-durable
        // project deletion is never turned into an HTTP 500 by secondary
        // coordination storage. A later synchronous call or process boot can
        // retry any retained item.
        let pass = process_pending_coordination_scope_deletions(&self.inner, |scope_project_id| {
            self.coordination_board_store
                .delete_scope_for_project_lifecycle(scope_project_id)
        });
        if pass.completed == 0 {
            return Ok(());
        }
        let inner = self.inner.lock().expect("state mutex poisoned");
        self.persist_internal_locked(&inner)
    }

    fn replay_pending_response_board_project_detachments(&self) {
        // The project removal and this outbox entry are already durable. A
        // secondary SQLite failure must not turn that irreversible success
        // into an HTTP 500: retain the entry and retry on the next worker pass
        // or process boot. Conversion and bookkeeping removal are idempotent.
        let pass = process_pending_response_board_project_detachments(
            &self.inner,
            |project_id, last_project_name| {
                convert_deleted_project_response_board_tab(
                    self.persistence_path.as_path(),
                    project_id,
                    last_project_name,
                )
                .map_err(|err| err.message)
            },
        );
        if pass.completed == 0 {
            return;
        }
        let inner = self.inner.lock().expect("state mutex poisoned");
        if let Err(err) = self.persist_internal_locked(&inner) {
            // The durable copy still contains the outbox item, so restart will
            // safely repeat the already-applied conversion.
            eprintln!(
                "[termal] failed to persist completed response-board project-tab \
                 detachment bookkeeping: {err:#}"
            );
        }
    }
}
