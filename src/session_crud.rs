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

/// Spawn-visible Engram MCP inputs that can change after project creation.
/// `Project.root_path` also feeds the descriptor, but project roots are
/// immutable. Grant rotation and binary/home changes let the authorized
/// current turn finish and rebuild at the next boundary; grant removal,
/// disable, and project deletion revoke the live runtime immediately.
#[derive(Clone, PartialEq, Eq)]
struct ProjectEngramMcpRuntimeFingerprint {
    binary_path: String,
    home: String,
    work_authority_grant: Option<String>,
}

fn project_engram_mcp_runtime_fingerprint(
    project: &Project,
) -> Option<ProjectEngramMcpRuntimeFingerprint> {
    if project.remote_id != LOCAL_REMOTE_ID {
        return None;
    }
    let settings = project.engram.as_ref()?;
    if !settings.is_runtime_enabled() {
        return None;
    }
    Some(ProjectEngramMcpRuntimeFingerprint {
        binary_path: settings.binary_path.clone()?,
        home: settings.home.clone()?,
        work_authority_grant: settings.work_authority_grant.clone(),
    })
}

fn normalize_engram_work_authority_grant_update(
    update: Option<Option<String>>,
) -> Result<Option<Option<String>>, ApiError> {
    let Some(update) = update else {
        return Ok(None);
    };
    let Some(grant) = update else {
        return Ok(Some(None));
    };
    let grant = grant.trim();
    let is_lowercase_sha256 = grant.len() == 64
        && grant
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !is_lowercase_sha256 {
        return Err(ApiError::bad_request(
            "work_authority_grant must be a lowercase 64-character SHA-256 hash",
        ));
    }
    Ok(Some(Some(grant.to_owned())))
}

fn project_engram_authority_revocation_targets_locked(
    inner: &StateInner,
    project: &Project,
    session_ids: &[String],
) -> Vec<EngramAuthorityRevocationTarget> {
    let mut targets = BTreeSet::new();
    if let Some(settings) = project.engram.as_ref()
        && let (Some(binary_path), Some(home), Some(work_authority_grant)) = (
            settings.binary_path.as_ref(),
            settings.home.as_ref(),
            settings.work_authority_grant.as_ref(),
        )
    {
        targets.insert(EngramAuthorityRevocationTarget {
            binary_path: binary_path.clone(),
            home: home.clone(),
            project_root: project.root_path.clone(),
            work_authority_grant: work_authority_grant.clone(),
        });
    }
    for session_id in session_ids {
        let Some(record) = inner
            .find_session_index(session_id)
            .and_then(|index| inner.sessions.get(index))
        else {
            continue;
        };
        if matches!(record.runtime, SessionRuntime::None) {
            continue;
        }
        let Some(installed) = record.engram_mcp_installed.as_ref() else {
            continue;
        };
        let Some(work_authority_grant) = installed.work_authority_grant.as_ref() else {
            continue;
        };
        targets.insert(EngramAuthorityRevocationTarget {
            binary_path: installed.binary_path.clone(),
            home: installed.home.clone(),
            project_root: project.root_path.clone(),
            work_authority_grant: work_authority_grant.clone(),
        });
    }
    targets.into_iter().collect()
}

fn revoke_project_engram_authority_off_lock(
    targets: &[EngramAuthorityRevocationTarget],
    reason: Option<&str>,
) -> Option<String> {
    let reason = reason?;
    let failures = targets
        .iter()
        .enumerate()
        .filter_map(|(index, target)| {
            revoke_engram_project_work_authority(target, reason)
                .err()
                .map(|error| format!("revocation target {}: {error}", index + 1))
        })
        .collect::<Vec<_>>();
    if failures.is_empty() {
        None
    } else {
        Some(format!(
            "the old Engram work-authority grant could not be revoked for one or more runtime targets; a residual MCP child may still mutate until it exits: {}",
            failures.join("; ")
        ))
    }
}

fn mark_engram_mcp_runtime_resets_locked(
    inner: &mut StateInner,
    session_ids: &[String],
) -> Vec<(String, bool)> {
    let mut previous = Vec::new();
    for session_id in session_ids {
        let Some(index) = inner.find_session_index(session_id) else {
            continue;
        };
        if !inner.sessions[index].is_local_session() {
            continue;
        }
        previous.push((
            session_id.clone(),
            inner.sessions[index].runtime_reset_required,
        ));
        inner
            .session_mut_by_index(index)
            .expect("session index should be valid")
            .runtime_reset_required = true;
    }
    previous
}

fn restore_engram_mcp_runtime_resets_locked(
    inner: &mut StateInner,
    previous: Vec<(String, bool)>,
) {
    for (session_id, runtime_reset_required) in previous {
        if let Some(index) = inner.find_session_index(&session_id) {
            inner
                .session_mut_by_index(index)
                .expect("session index should be valid")
                .runtime_reset_required = runtime_reset_required;
        }
    }
}

fn project_matches_engram_reset_snapshot(project: &Project, snapshot: &Project) -> bool {
    project.id == snapshot.id
        && project.name == snapshot.name
        && project.root_path == snapshot.root_path
        && project.remote_id == snapshot.remote_id
        && project.remote_project_id == snapshot.remote_project_id
        && project.engram == snapshot.engram
}

fn project_engram_binding_target_locked(
    inner: &StateInner,
    session_id: &str,
) -> std::result::Result<Option<EngramBindingTarget>, String> {
    AppState::engram_binding_target_for_session_shape_locked(inner, session_id, false)
}

fn project_engram_binding_target_during_owned_reset_locked(
    inner: &StateInner,
    session_id: &str,
    project_id: &str,
    owner_generation: u64,
) -> std::result::Result<Option<EngramBindingTarget>, String> {
    if !inner
        .engram_project_resets
        .is_owned_by(project_id, owner_generation)
    {
        return Err("Engram project reset fence ownership was lost".to_owned());
    }
    AppState::engram_binding_target_for_session_shape_during_project_reset_locked(
        inner,
        session_id,
        false,
        owner_generation,
    )
}

#[cfg(test)]
static TEST_ENGRAM_PROJECT_RESET_FENCE_OBSERVERS: LazyLock<
    Mutex<HashMap<String, mpsc::SyncSender<()>>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(test)]
static TEST_ENGRAM_PROJECT_RESET_FENCE_GATES: LazyLock<
    Mutex<HashMap<String, (mpsc::SyncSender<()>, mpsc::Receiver<()>)>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(test)]
struct TestEngramProjectResetFenceGate {
    project_id: String,
    entered_rx: mpsc::Receiver<()>,
    release_tx: mpsc::SyncSender<()>,
}

#[cfg(test)]
impl TestEngramProjectResetFenceGate {
    fn wait_until_entered(&self) {
        self.entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("Engram project reset should enter the test fence gate");
    }

    fn release(self) {
        let _ = self.release_tx.send(());
    }
}

#[cfg(test)]
impl Drop for TestEngramProjectResetFenceGate {
    fn drop(&mut self) {
        TEST_ENGRAM_PROJECT_RESET_FENCE_GATES
            .lock()
            .expect("Engram reset fence gate mutex poisoned")
            .remove(&self.project_id);
    }
}

#[cfg(test)]
fn gate_next_engram_project_reset_fence(project_id: &str) -> TestEngramProjectResetFenceGate {
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    TEST_ENGRAM_PROJECT_RESET_FENCE_GATES
        .lock()
        .expect("Engram reset fence gate mutex poisoned")
        .insert(project_id.to_owned(), (entered_tx, release_rx));
    TestEngramProjectResetFenceGate {
        project_id: project_id.to_owned(),
        entered_rx,
        release_tx,
    }
}

#[cfg(test)]
fn observe_next_engram_project_reset_fence(project_id: &str) -> mpsc::Receiver<()> {
    let (sender, receiver) = mpsc::sync_channel(1);
    TEST_ENGRAM_PROJECT_RESET_FENCE_OBSERVERS
        .lock()
        .expect("Engram reset fence observer mutex poisoned")
        .insert(project_id.to_owned(), sender);
    receiver
}

#[cfg(test)]
fn notify_engram_project_reset_fenced(project_id: &str) {
    let observer = TEST_ENGRAM_PROJECT_RESET_FENCE_OBSERVERS
        .lock()
        .expect("Engram reset fence observer mutex poisoned")
        .remove(project_id);
    if let Some(observer) = observer {
        let _ = observer.send(());
    }
}

#[cfg(test)]
fn wait_at_engram_project_reset_fence_gate(project_id: &str) {
    let gate = TEST_ENGRAM_PROJECT_RESET_FENCE_GATES
        .lock()
        .expect("Engram reset fence gate mutex poisoned")
        .remove(project_id);
    if let Some((entered_tx, release_rx)) = gate {
        let _ = entered_tx.send(());
        let _ = release_rx.recv_timeout(Duration::from_secs(5));
    }
}

impl AppState {
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
            // The stop fence is already held, so runtime callbacks cannot win
            // while the checkpoint and process teardown run off-lock. Passing
            // no token is intentional: token-scoped checkpoints decline to run
            // while a stop owns the runtime.
            self.checkpoint_engram_turn_off_lock(&session_id, None, EngramNextIntent::Exit);

            let runtime_context = format!("{context} for session `{session_id}`");
            let (shutdown_error, retain_runtime_for_retry, suppress_codex_thread_resume) =
                match shutdown_stopped_runtime(target.runtime.clone(), &runtime_context) {
                Ok(()) => (None, false, false),
                Err(first_error) if target.runtime.stop_failure_is_best_effort() => {
                    // The project grant is revoked independently before this
                    // teardown, so an old MCP child cannot mutate Engram even
                    // if the thread interrupt is unconfirmed. Preserve the
                    // shared app-server for unrelated sessions and surface the
                    // residual read-only process window as degraded cleanup.
                    (
                        Some(format!(
                            "shared Codex interrupt failed after detach; the revoked grant blocks further mutations, but the old thread may still read until Codex unloads it: {first_error:#}"
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

    fn resume_revoked_engram_mcp_sessions(
        &self,
        outcome: &mut EngramMcpRuntimeRevocationOutcome,
    ) {
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
                        if let Err(error) = deliver_turn_dispatch(self, dispatch) {
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
                outcome
                    .failures
                    .push(format!("failed to resume orchestrator transitions: {error:#}"));
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
                "Engram MCP configuration was revoked, but runtime cleanup was degraded: {}{}",
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

    /// Updates the optional Engram adapter for one local project. Filesystem
    /// checks and `engram doctor` run off-lock; the project identity/root are
    /// fenced before the validated settings are committed.
    #[cfg(test)]
    fn update_project_engram_settings(
        &self,
        project_id: &str,
        settings: EngramProjectSettings,
    ) -> Result<StateResponse, ApiError> {
        let work_authority_grant_update = Some(settings.work_authority_grant.clone());
        self.update_project_engram_settings_with_grant(
            project_id,
            settings,
            work_authority_grant_update,
        )
    }

    fn patch_project_engram_settings(
        &self,
        project_id: &str,
        request: UpdateProjectEngramSettingsRequest,
    ) -> Result<StateResponse, ApiError> {
        let (mut settings, work_authority_grant_update) = request.into_settings();
        let work_authority_grant_update =
            normalize_engram_work_authority_grant_update(work_authority_grant_update)?;
        settings.work_authority_grant = work_authority_grant_update.clone().flatten();
        self.update_project_engram_settings_with_grant(
            project_id,
            settings,
            work_authority_grant_update,
        )
    }

    fn update_project_engram_settings_with_grant(
        &self,
        project_id: &str,
        mut settings: EngramProjectSettings,
        work_authority_grant_update: Option<Option<String>>,
    ) -> Result<StateResponse, ApiError> {
        let project_id = normalize_optional_identifier(Some(project_id))
            .ok_or_else(|| ApiError::bad_request("project id is required"))?
            .to_owned();
        if settings
            .deadline_ms
            .is_some_and(|deadline_ms| deadline_ms == 0 || deadline_ms > 10_000)
        {
            return Err(ApiError::bad_request(
                "Engram deadline must be between 1 and 10000 ms",
            ));
        }
        let project_snapshot = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .find_project(&project_id)
                .cloned()
                .ok_or_else(|| ApiError::not_found("project not found"))?
        };
        if project_snapshot.remote_id != LOCAL_REMOTE_ID {
            return Err(ApiError::bad_request(
                "Engram host control is available only for local projects",
            ));
        }
        if work_authority_grant_update.is_none() {
            settings.work_authority_grant = project_snapshot
                .engram
                .as_ref()
                .and_then(|current| current.work_authority_grant.clone());
        }
        if !settings.enabled
            && let Some(current) = project_snapshot.engram.as_ref()
        {
            // A failed disable checkpoint leaves durable authority that must
            // later be addressed through the connection that owns it. Keep
            // omitted connection fields while disabled so a same-store
            // re-enable can recover, and a home change can checkpoint the old
            // store before the session state is wiped.
            if settings.binary_path.is_none() {
                settings.binary_path.clone_from(&current.binary_path);
            }
            if settings.home.is_none() {
                settings.home.clone_from(&current.home);
            }
            // Disable irreversibly revokes the old project grant below. Never
            // persist that revoked hash into a disabled configuration where a
            // later re-enable could accidentally reuse it.
            settings.work_authority_grant = None;
        }
        if settings.enabled {
            validate_engram_project_enablement(&project_snapshot, &settings)?;
        }

        let mut updated_project_snapshot = project_snapshot.clone();
        updated_project_snapshot.engram = Some(settings.clone());
        let previous_mcp_runtime_fingerprint =
            project_engram_mcp_runtime_fingerprint(&project_snapshot);
        let next_mcp_runtime_fingerprint =
            project_engram_mcp_runtime_fingerprint(&updated_project_snapshot);
        let mcp_runtime_reset_required =
            previous_mcp_runtime_fingerprint != next_mcp_runtime_fingerprint;
        let immediate_mcp_runtime_teardown = previous_mcp_runtime_fingerprint
            .as_ref()
            .is_some_and(|previous| {
                next_mcp_runtime_fingerprint.is_none()
                    || (previous.work_authority_grant.is_some()
                        && next_mcp_runtime_fingerprint
                            .as_ref()
                            .is_some_and(|next| next.work_authority_grant.is_none()))
            });
        let authority_revocation_reason = project_snapshot
            .engram
            .as_ref()
            .and_then(|current| current.work_authority_grant.as_ref())
            .and_then(|_| {
                if !settings.enabled {
                    Some("TermAl project Engram integration disabled")
                } else if settings.work_authority_grant.is_none() {
                    Some("TermAl project Engram work-authority grant removed")
                } else {
                    None
                }
            });

        let reset_required = project_snapshot.engram.as_ref().is_some_and(|current| {
            if current.enabled {
                !settings.enabled
                    || current.binary_path != settings.binary_path
                    || current.home != settings.home
            } else {
                // Routing tokens belong to the Engram store under `home`.
                // A different executable can still address that same store,
                // but a home change must drain the old connection first.
                current.home != settings.home
            }
        });
        if !reset_required {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if inner.engram_project_resets.contains(&project_id) {
                return Err(ApiError::conflict(
                    "Engram project settings are already being reset",
                ));
            }
            let project_index = inner
                .projects
                .iter()
                .position(|project| project.id == project_id)
                .ok_or_else(|| ApiError::not_found("project not found"))?;
            let project = &inner.projects[project_index];
            if project.root_path != project_snapshot.root_path
                || project.remote_id != project_snapshot.remote_id
                || project.engram != project_snapshot.engram
            {
                return Err(ApiError::conflict(
                    "project changed while Engram settings were being validated",
                ));
            }
            let affected_session_ids = mcp_runtime_reset_required
                .then(|| project_engram_session_ids_locked(&inner, &project_id))
                .unwrap_or_default();
            let authority_revocation_targets =
                project_engram_authority_revocation_targets_locked(
                    &inner,
                    &project_snapshot,
                    &affected_session_ids,
                );
            let project_reset_generation = if immediate_mcp_runtime_teardown {
                Some(
                    inner
                        .engram_project_resets
                        .claim(&project_id)
                        .ok_or_else(|| {
                            ApiError::conflict(
                                "Engram project settings are already being reset",
                            )
                        })?,
                )
            } else {
                None
            };
            inner.projects[project_index].engram = Some(settings);
            let previous_runtime_reset_flags = mark_engram_mcp_runtime_resets_locked(
                &mut inner,
                &affected_session_ids,
            );
            let revocation_batch = if immediate_mcp_runtime_teardown {
                claim_engram_mcp_runtime_revocations_locked(
                    &mut inner,
                    &affected_session_ids,
                )
            } else {
                EngramMcpRuntimeRevocationBatch::default()
            };
            if let Err(err) = self.commit_locked(&mut inner) {
                inner.projects[project_index].engram = project_snapshot.engram.clone();
                restore_engram_mcp_runtime_resets_locked(
                    &mut inner,
                    previous_runtime_reset_flags,
                );
                rollback_engram_mcp_runtime_revocations_locked(
                    &mut inner,
                    &revocation_batch,
                );
                if let Some(project_reset_generation) = project_reset_generation {
                    inner
                        .engram_project_resets
                        .release(&project_id, project_reset_generation);
                }
                return Err(ApiError::internal(format!(
                    "failed to persist Engram project settings: {err:#}"
                )));
            }
            drop(inner);
            #[cfg(test)]
            if project_reset_generation.is_some() {
                notify_engram_project_reset_fenced(&project_id);
            }
            #[cfg(test)]
            if project_reset_generation.is_some() {
                wait_at_engram_project_reset_fence_gate(&project_id);
            }
            let authority_revocation_failure = revoke_project_engram_authority_off_lock(
                &authority_revocation_targets,
                authority_revocation_reason,
            );
            let cleanup_result = if immediate_mcp_runtime_teardown {
                let shutdowns = self.shutdown_revoked_engram_mcp_runtimes(
                    revocation_batch,
                    "revoked Engram MCP configuration",
                );
                let mut outcome = self.finalize_revoked_engram_mcp_runtimes(shutdowns);
                if let Some(failure) = authority_revocation_failure {
                    outcome.failures.push(failure);
                }
                self.resume_revoked_engram_mcp_sessions(&mut outcome);
                self.finish_revoked_engram_mcp_runtime_outcome(outcome)
                    .map(|_| ())
            } else if let Some(failure) = authority_revocation_failure {
                Err(ApiError::internal(format!(
                    "Engram project settings changed, but capability revocation was degraded: {failure}"
                )))
            } else {
                Ok(())
            };
            if let Some(project_reset_generation) = project_reset_generation {
                self.release_engram_project_reset_fence(
                    &project_id,
                    project_reset_generation,
                    &affected_session_ids,
                );
            }
            cleanup_result?;
            return Ok(self.snapshot());
        }

        // Fence every session before releasing the lock. The generation bump
        // rejects an already-evaluating dispatch; the ephemeral flag keeps new
        // dispatches on the Phase 0 shadow path while the old connection drains.
        let (
            session_ids,
            reset_target_candidates,
            project_reset_generation,
            authority_revocation_targets,
        ) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let project = inner
                .find_project(&project_id)
                .ok_or_else(|| ApiError::not_found("project not found"))?;
            if project.root_path != project_snapshot.root_path
                || project.remote_id != project_snapshot.remote_id
                || project.engram != project_snapshot.engram
            {
                return Err(ApiError::conflict(
                    "project changed while Engram settings were being validated",
                ));
            }
            let session_ids = project_engram_session_ids_locked(&inner, &project_id);
            let authority_revocation_targets =
                project_engram_authority_revocation_targets_locked(
                    &inner,
                    &project_snapshot,
                    &session_ids,
                );
            let reset_target_candidates = session_ids
                .iter()
                .map(|session_id| {
                    project_engram_binding_target_locked(&inner, session_id).map_err(|error| {
                        ApiError::internal(format!(
                            "failed snapshotting old Engram binding: {error}"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            let project_reset_generation = inner
                .engram_project_resets
                .claim(&project_id)
                .ok_or_else(|| {
                    ApiError::conflict("Engram project settings are already being reset")
                })?;
            for session_id in &session_ids {
                if let Some(index) = inner.find_session_index(session_id) {
                    let record = inner
                        .session_mut_by_index(index)
                        .expect("session index should be valid");
                    record.engram.project_reset_in_progress = true;
                    record.engram.dispatch_generation =
                        record.engram.dispatch_generation.saturating_add(1);
                }
            }
            (
                session_ids,
                reset_target_candidates,
                project_reset_generation,
                authority_revocation_targets,
            )
        };
        #[cfg(test)]
        notify_engram_project_reset_fenced(&project_id);
        #[cfg(test)]
        wait_at_engram_project_reset_fence_gate(&project_id);
        if let Err(error) = self.wait_for_engram_project_reset_quiescence(&session_ids) {
            self.release_engram_project_reset_fence(
                &project_id,
                project_reset_generation,
                &session_ids,
            );
            return Err(error);
        }

        let reset_targets_result = (|| -> Result<Vec<EngramBindingTarget>, ApiError> {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let project = inner
                .find_project(&project_id)
                .ok_or_else(|| ApiError::not_found("project not found"))?;
            if project.root_path != project_snapshot.root_path
                || project.remote_id != project_snapshot.remote_id
                || project.engram != project_snapshot.engram
                || !inner
                    .engram_project_resets
                    .is_owned_by(&project_id, project_reset_generation)
            {
                return Err(ApiError::conflict(
                    "project changed while Engram settings were being reset",
                ));
            }
            let mut targets = Vec::new();
            for mut target in reset_target_candidates {
                let Some(record) = inner
                    .find_session_index(&target.connection.session_id)
                    .and_then(|index| inner.sessions.get(index))
                else {
                    continue;
                };
                target.routing_token = record.engram.routing_token.clone();
                target.active_grant_id = record.engram.active_grant_id.clone();
                if target.active_grant_id.is_none() {
                    continue;
                }
                targets.push(target);
            }
            for target in &targets {
                if let Some(index) = inner.find_session_index(&target.connection.session_id) {
                    inner
                        .session_mut_by_index(index)
                        .expect("session index should be valid")
                        .engram
                        .checkpoint_in_progress = true;
                }
            }
            Ok(targets)
        })();
        let reset_targets = match reset_targets_result {
            Ok(targets) => targets,
            Err(error) => {
                self.release_engram_project_reset_fence(
                    &project_id,
                    project_reset_generation,
                    &session_ids,
                );
                return Err(error);
            }
        };

        let disabling = !settings.enabled;
        let preserve_checkpoint_recovery = disabling
            && project_snapshot
                .engram
                .as_ref()
                .is_some_and(|current| current.home == settings.home);
        let mut checkpoint_failures = Vec::new();
        let mut checkpoint_recovery = Vec::new();
        let mut checkpointed = Vec::new();
        for target in &reset_targets {
            let checkpoint_started_at = std::time::Instant::now();
            match target.checkpoint_for_project_reset_off_lock() {
                Ok(()) => checkpointed.push((
                    target.connection.session_id.clone(),
                    target
                        .active_grant_id
                        .clone()
                        .expect("reset target should carry an active grant"),
                )),
                Err(error) => {
                    let failure = format!("session {}: {}", target.connection.session_id, error);
                    if disabling {
                        // Disable is the operator escape hatch. Preserve a
                        // visible record that the begun grant could not be
                        // closed, but do not let an unreachable control plane
                        // veto the setting change or reaping its process. When
                        // the disabled settings still name the same store,
                        // keep the durable authority identity so a later
                        // re-enable can ask Engram what remains open,
                        // checkpoint it with `wait`, and only then replace the
                        // binding. A home change cannot carry that identity
                        // into the new store; the degraded card is the durable
                        // record of the failed cleanup instead.
                        self.record_engram_project_disable_checkpoint_failure(
                            target,
                            &error,
                            checkpoint_started_at.elapsed(),
                        );
                        if preserve_checkpoint_recovery {
                            checkpoint_recovery.push((
                                target.connection.session_id.clone(),
                                target.routing_token.clone(),
                                target.active_grant_id.clone(),
                            ));
                        }
                        eprintln!(
                            "engram> project={} disable checkpoint degraded: {failure}",
                            project_id
                        );
                    }
                    checkpoint_failures.push(failure);
                }
            }
        }

        if !checkpoint_failures.is_empty() && !disabling {
            self.abort_engram_project_reset(
                &project_id,
                project_reset_generation,
                &session_ids,
                &checkpointed,
            )?;
            return Err(ApiError::conflict(format!(
                "Engram project settings were not changed because checkpointing failed: {}",
                checkpoint_failures.join("; ")
            )));
        }

        let (adapter, final_session_ids, revocation_batch) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let reset_still_in_progress = inner
                .engram_project_resets
                .is_owned_by(&project_id, project_reset_generation);
            let Some(project_index) = inner
                .projects
                .iter()
                .position(|project| project.id == project_id)
            else {
                drop(inner);
                self.abort_engram_project_reset(
                    &project_id,
                    project_reset_generation,
                    &session_ids,
                    &checkpointed,
                )?;
                return Err(ApiError::not_found("project not found"));
            };
            let project = &inner.projects[project_index];
            if project.root_path != project_snapshot.root_path
                || project.remote_id != project_snapshot.remote_id
                || project.engram != project_snapshot.engram
                || !reset_still_in_progress
            {
                drop(inner);
                self.abort_engram_project_reset(
                    &project_id,
                    project_reset_generation,
                    &session_ids,
                    &checkpointed,
                )?;
                return Err(ApiError::conflict(
                    "project changed while Engram settings were being reset",
                ));
            }
            let final_session_ids = project_engram_session_ids_locked(&inner, &project_id);
            let previous_session_engram = final_session_ids
                .iter()
                .filter_map(|session_id| {
                    inner
                        .find_session_index(session_id)
                        .and_then(|index| inner.sessions.get(index))
                        .map(|record| (session_id.clone(), record.engram.clone()))
                })
                .collect::<Vec<_>>();
            inner.projects[project_index].engram = Some(settings.clone());
            for session_id in &final_session_ids {
                if let Some(index) = inner.find_session_index(session_id) {
                    let dispatch_generation = inner.sessions[index].engram.dispatch_generation;
                    let record = inner
                        .session_mut_by_index(index)
                        .expect("session index should be valid");
                    record.engram = EngramSessionState::default();
                    record.engram.dispatch_generation = dispatch_generation;
                    if let Some((_, routing_token, active_grant_id)) = checkpoint_recovery
                        .iter()
                        .find(|(failed_session_id, _, _)| failed_session_id == session_id)
                    {
                        record.engram.routing_token = routing_token.clone();
                        record.engram.active_grant_id = active_grant_id.clone();
                        record.engram.rebind_required = true;
                    }
                }
            }
            let previous_runtime_reset_flags = if mcp_runtime_reset_required {
                mark_engram_mcp_runtime_resets_locked(&mut inner, &final_session_ids)
            } else {
                Vec::new()
            };
            let revocation_batch = if immediate_mcp_runtime_teardown {
                claim_engram_mcp_runtime_revocations_locked(
                    &mut inner,
                    &final_session_ids,
                )
            } else {
                EngramMcpRuntimeRevocationBatch::default()
            };
            if let Err(err) = self.commit_locked(&mut inner) {
                // The old connection processes are deliberately still alive at
                // this point. Restore their matching in-memory configuration
                // before reporting the persistence failure so a failed PATCH
                // cannot leave memory describing the new settings while the
                // sidecars still use the old binary/home. Grants successfully
                // checkpointed with `exit` cannot be resurrected; clear only
                // those grants and require a fresh bind on the next dispatch.
                inner.projects[project_index].engram = project_snapshot.engram.clone();
                for (session_id, previous_engram) in previous_session_engram {
                    if let Some(index) = inner.find_session_index(&session_id) {
                        let record = inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid");
                        record.engram = previous_engram;
                        record.engram.project_reset_in_progress = false;
                        record.engram.checkpoint_in_progress = false;
                    }
                }
                restore_engram_mcp_runtime_resets_locked(
                    &mut inner,
                    previous_runtime_reset_flags,
                );
                rollback_engram_mcp_runtime_revocations_locked(
                    &mut inner,
                    &revocation_batch,
                );
                // A session may have left the project while the off-lock
                // checkpoint loop was running. It was not part of the final
                // wipe/snapshot above, but it still carries the initial fence.
                for session_id in &session_ids {
                    if let Some(index) = inner.find_session_index(session_id) {
                        let record = inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid");
                        record.engram.project_reset_in_progress = false;
                        record.engram.checkpoint_in_progress = false;
                    }
                }
                for (session_id, grant_id) in &checkpointed {
                    if let Some(index) = inner.find_session_index(session_id) {
                        let record = inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid");
                        if record.engram.active_grant_id.as_deref() == Some(grant_id) {
                            record.engram.active_grant_id = None;
                            record.engram.rebind_required = true;
                        }
                    }
                }
                inner
                    .engram_project_resets
                    .release(&project_id, project_reset_generation);
                return Err(ApiError::internal(format!(
                    "failed to persist Engram project settings: {err:#}"
                )));
            }
            (
                inner.engram_host_adapter.clone(),
                final_session_ids,
                revocation_batch,
            )
        };

        // Capability revocation and process teardown are deliberately off-lock.
        // Revoke the grant first: Engram revalidates it on every mutation, so
        // even an unconfirmed agent interruption cannot keep mutating work.
        let authority_revocation_failure = revoke_project_engram_authority_off_lock(
            &authority_revocation_targets,
            authority_revocation_reason,
        );
        for session_id in &final_session_ids {
            adapter.shutdown_session(session_id);
        }
        let runtime_shutdowns = immediate_mcp_runtime_teardown.then(|| {
            self.shutdown_revoked_engram_mcp_runtimes(
                revocation_batch,
                "revoked Engram MCP configuration",
            )
        });
        let fresh_targets = if settings.enabled {
            let inner = self.inner.lock().expect("state mutex poisoned");
            final_session_ids
                .iter()
                .filter_map(|session_id| {
                    match project_engram_binding_target_during_owned_reset_locked(
                        &inner,
                        session_id,
                        &project_id,
                        project_reset_generation,
                    ) {
                        Ok(target) => target,
                        Err(error) => {
                            eprintln!(
                                "engram> session={session_id} fresh binding snapshot degraded: {error}"
                            );
                            None
                        }
                    }
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        for target in fresh_targets {
            let session_id = target.connection.session_id.clone();
            if let Err(error) = self.bind_engram_target_off_lock(target) {
                self.record_engram_transport_failure(&session_id, &error);
                eprintln!("engram> session={session_id} post-reconfigure bind degraded: {error}");
            }
        }
        let cleanup_result = if let Some(runtime_shutdowns) = runtime_shutdowns {
            let mut outcome = self.finalize_revoked_engram_mcp_runtimes(runtime_shutdowns);
            if let Some(failure) = authority_revocation_failure {
                outcome.failures.push(failure);
            }
            self.resume_revoked_engram_mcp_sessions(&mut outcome);
            self.finish_revoked_engram_mcp_runtime_outcome(outcome)
                .map(|_| ())
        } else if let Some(failure) = authority_revocation_failure {
            Err(ApiError::internal(format!(
                "Engram project settings changed, but capability revocation was degraded: {failure}"
            )))
        } else {
            Ok(())
        };
        self.release_engram_project_reset_fence(
            &project_id,
            project_reset_generation,
            &session_ids,
        );
        cleanup_result?;
        let inner = self.inner.lock().expect("state mutex poisoned");
        Ok(self.snapshot_from_inner(&inner))
    }

    fn record_engram_project_disable_checkpoint_failure(
        &self,
        target: &EngramBindingTarget,
        error: &EngramTransportError,
        elapsed: Duration,
    ) {
        let Some(grant_id) = target.active_grant_id.as_deref() else {
            return;
        };
        let latency_ms = duration_millis(elapsed);
        self.finish_engram_checkpoint_record(
            &target.connection.session_id,
            grant_id,
            EngramControlCardDecision::Degraded,
            EngramControlCard {
                schema_version: ENGRAM_CONTROL_SCHEMA_VERSION,
                stage: EngramControlStage::Checkpoint,
                assurance: "advisory".to_owned(),
                decision: EngramControlCardDecision::Degraded,
                dispatch: EngramControlCardDispatch::SentOnGrant,
                refusal_code: Some(
                    error
                        .code
                        .clone()
                        .unwrap_or_else(|| "checkpoint_failed".to_owned()),
                ),
                defer_code: None,
                grant_id: Some(grant_id.to_owned()),
                directives: Vec::new(),
                delivered_range: None,
                latency_ms: EngramControlLatencyCard {
                    evaluate: None,
                    begin: None,
                    checkpoint: Some(latency_ms),
                    total: latency_ms,
                },
                fail_mode: EngramControlFailMode::Degraded,
                next_intent: Some(EngramNextIntent::Exit),
                repair_armed: true,
            },
        );
    }

    fn wait_for_engram_project_reset_quiescence(
        &self,
        session_ids: &[String],
    ) -> Result<(), ApiError> {
        let deadline = std::time::Instant::now() + ENGRAM_CONTROL_SETTLE_TIMEOUT;
        loop {
            let busy = {
                let inner = self.inner.lock().expect("state mutex poisoned");
                session_ids.iter().any(|session_id| {
                    inner
                        .find_session_index(session_id)
                        .and_then(|index| inner.sessions.get(index))
                        .is_some_and(|record| {
                            record.engram.bind_in_progress
                                || record.engram.checkpoint_in_progress
                                || record.engram.pending_dispatch.is_some()
                        })
                })
            };
            if !busy {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(ApiError::conflict(
                    "Engram project settings are busy with an in-flight control operation",
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn release_engram_project_reset_fence(
        &self,
        project_id: &str,
        owner_generation: u64,
        session_ids: &[String],
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if !inner
            .engram_project_resets
            .release(project_id, owner_generation)
        {
            return;
        }
        for session_id in session_ids {
            if let Some(index) = inner.find_session_index(session_id) {
                let record = inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                record.engram.project_reset_in_progress = false;
                record.engram.checkpoint_in_progress = false;
            }
        }
    }

    fn abort_engram_project_reset(
        &self,
        project_id: &str,
        owner_generation: u64,
        session_ids: &[String],
        checkpointed: &[(String, String)],
    ) -> Result<(), ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if !inner
            .engram_project_resets
            .release(project_id, owner_generation)
        {
            return Ok(());
        }
        for session_id in session_ids {
            if let Some(index) = inner.find_session_index(session_id) {
                let record = inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                record.engram.project_reset_in_progress = false;
                record.engram.checkpoint_in_progress = false;
            }
        }
        for (session_id, grant_id) in checkpointed {
            if let Some(index) = inner.find_session_index(session_id) {
                let record = inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                if record.engram.active_grant_id.as_deref() == Some(grant_id) {
                    record.engram.active_grant_id = None;
                    record.engram.rebind_required = true;
                }
            }
        }
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!(
                "failed persisting partial Engram project reset: {err:#}"
            ))
        })?;
        Ok(())
    }

    /// Creates a new session (local or remote-backed) from a
    /// `CreateSessionRequest`, persists it, and broadcasts the
    /// SessionCreated delta.
    ///
    /// Workdir resolution order is: explicit `request.workdir` →
    /// the project's default workdir (if `project_id` was given) →
    /// the global `AppSettings.default_workdir`. The selected agent
    /// defaults to `Agent::Codex` but can be overridden by the
    /// request; cross-family guards reject combinations like "ACP
    /// agent with Codex-only reasoning effort" before we commit.
    ///
    /// For remote-backed projects we short-circuit to
    /// [`Self::create_remote_session_proxy`] so the real record lives
    /// on the remote and this host only stores a proxy shell. For
    /// local projects we refresh the agent-readiness cache before
    /// taking the state lock (so the broadcast carries fresh data
    /// without doing filesystem I/O under the lock), then create a
    /// record with `SessionRuntime::None`. The real runtime is spawned
    /// lazily on the first prompt through
    /// `turn_dispatch.rs::dispatch_turn`.
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
                        "OpenCode sessions only support model settings at creation; choose the dynamic mode after OpenCode reports its live options",
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
        let session = AppState::wire_session_from_record(created_record);
        let delta_session = AppState::wire_session_summary_from_record(created_record);
        drop(inner);
        self.publish_delta(&DeltaEvent::SessionCreated {
            revision,
            session_id: session.id.clone(),
            session: delta_session,
        });
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
            request.default_opencode_model,
            Agent::OpenCode,
            |preferences| &mut preferences.default_opencode_model,
        )?;

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
        let project_snapshot = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .find_project(&project_id)
                .cloned()
                .ok_or_else(|| ApiError::not_found("project not found"))?
        };
        if project_snapshot.remote_id == LOCAL_REMOTE_ID && project_snapshot.engram.is_some() {
            return self.delete_project_with_engram_reset(project_id, project_snapshot);
        }
        self.delete_project_without_engram_reset(&project_id)
    }

    fn delete_project_without_engram_reset(
        &self,
        project_id: &str,
    ) -> Result<StateResponse, ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
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

    fn delete_project_with_engram_reset(
        &self,
        project_id: String,
        project_snapshot: Project,
    ) -> Result<StateResponse, ApiError> {
        let mcp_runtime_reset_required =
            project_engram_mcp_runtime_fingerprint(&project_snapshot).is_some();
        let (
            session_ids,
            reset_target_candidates,
            project_reset_generation,
            authority_revocation_targets,
        ) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let project = inner
                .find_project(&project_id)
                .ok_or_else(|| ApiError::not_found("project not found"))?;
            if !project_matches_engram_reset_snapshot(project, &project_snapshot) {
                return Err(ApiError::conflict(
                    "project changed while Engram deletion was being prepared",
                ));
            }
            let session_ids = project_engram_session_ids_locked(&inner, &project_id);
            let authority_revocation_targets =
                project_engram_authority_revocation_targets_locked(
                    &inner,
                    &project_snapshot,
                    &session_ids,
                );
            let reset_target_candidates = session_ids
                .iter()
                .map(|session_id| {
                    let has_durable_control_authority = inner
                        .find_session_index(session_id)
                        .and_then(|index| inner.sessions.get(index))
                        .is_some_and(|record| {
                            record.engram.routing_token.is_some()
                                || record.engram.active_grant_id.is_some()
                        });
                    if !has_durable_control_authority {
                        // Project deletion has nothing to drain for this
                        // session. In particular, a never-enabled project may
                        // intentionally omit Engram binary and home settings.
                        return Ok(None);
                    }
                    project_engram_binding_target_locked(&inner, session_id).map_err(|error| {
                        ApiError::internal(format!(
                            "failed snapshotting deleted-project Engram binding: {error}"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            let project_reset_generation = inner
                .engram_project_resets
                .claim(&project_id)
                .ok_or_else(|| {
                    ApiError::conflict("Engram project settings are already being reset")
                })?;
            for session_id in &session_ids {
                if let Some(index) = inner.find_session_index(session_id) {
                    let record = inner
                        .session_mut_by_index(index)
                        .expect("session index should be valid");
                    record.engram.project_reset_in_progress = true;
                    record.engram.dispatch_generation =
                        record.engram.dispatch_generation.saturating_add(1);
                }
            }
            (
                session_ids,
                reset_target_candidates,
                project_reset_generation,
                authority_revocation_targets,
            )
        };
        #[cfg(test)]
        notify_engram_project_reset_fenced(&project_id);
        #[cfg(test)]
        wait_at_engram_project_reset_fence_gate(&project_id);
        if let Err(error) = self.wait_for_engram_project_reset_quiescence(&session_ids) {
            self.release_engram_project_reset_fence(
                &project_id,
                project_reset_generation,
                &session_ids,
            );
            return Err(error);
        }

        let reset_targets_result = (|| -> Result<Vec<EngramBindingTarget>, ApiError> {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let project = inner
                .find_project(&project_id)
                .ok_or_else(|| ApiError::not_found("project not found"))?;
            if !project_matches_engram_reset_snapshot(project, &project_snapshot)
                || !inner
                    .engram_project_resets
                    .is_owned_by(&project_id, project_reset_generation)
            {
                return Err(ApiError::conflict(
                    "project changed while Engram deletion was being fenced",
                ));
            }
            let mut targets = Vec::new();
            for mut target in reset_target_candidates {
                let Some(record) = inner
                    .find_session_index(&target.connection.session_id)
                    .and_then(|index| inner.sessions.get(index))
                else {
                    continue;
                };
                target.routing_token = record.engram.routing_token.clone();
                target.active_grant_id = record.engram.active_grant_id.clone();
                if target.active_grant_id.is_none() {
                    continue;
                }
                targets.push(target);
            }
            for target in &targets {
                if let Some(index) = inner.find_session_index(&target.connection.session_id) {
                    inner
                        .session_mut_by_index(index)
                        .expect("session index should be valid")
                        .engram
                        .checkpoint_in_progress = true;
                }
            }
            Ok(targets)
        })();
        let reset_targets = match reset_targets_result {
            Ok(targets) => targets,
            Err(error) => {
                self.release_engram_project_reset_fence(
                    &project_id,
                    project_reset_generation,
                    &session_ids,
                );
                return Err(error);
            }
        };

        let mut checkpointed = Vec::new();
        let mut checkpoint_failures = Vec::new();
        for target in &reset_targets {
            match target.checkpoint_for_project_reset_off_lock() {
                Ok(()) => checkpointed.push((
                    target.connection.session_id.clone(),
                    target
                        .active_grant_id
                        .clone()
                        .expect("reset target should carry an active grant"),
                )),
                Err(error) => checkpoint_failures.push(format!(
                    "session {}: {}",
                    target.connection.session_id, error
                )),
            }
        }
        if !checkpoint_failures.is_empty() {
            self.abort_engram_project_reset(
                &project_id,
                project_reset_generation,
                &session_ids,
                &checkpointed,
            )?;
            return Err(ApiError::conflict(format!(
                "project was not deleted because Engram checkpointing failed: {}",
                checkpoint_failures.join("; ")
            )));
        }

        let (adapter, final_session_ids, persist_dispatch, revocation_batch) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(project_index) = inner
                .projects
                .iter()
                .position(|project| project.id == project_id)
            else {
                drop(inner);
                self.abort_engram_project_reset(
                    &project_id,
                    project_reset_generation,
                    &session_ids,
                    &checkpointed,
                )?;
                return Err(ApiError::not_found("project not found"));
            };
            if !project_matches_engram_reset_snapshot(
                &inner.projects[project_index],
                &project_snapshot,
            )
                || !inner
                    .engram_project_resets
                    .is_owned_by(&project_id, project_reset_generation)
            {
                drop(inner);
                self.abort_engram_project_reset(
                    &project_id,
                    project_reset_generation,
                    &session_ids,
                    &checkpointed,
                )?;
                return Err(ApiError::conflict(
                    "project changed while Engram deletion was being committed",
                ));
            }

            let final_session_ids = project_engram_session_ids_locked(&inner, &project_id);
            let previous_session_states = final_session_ids
                .iter()
                .filter_map(|session_id| {
                    inner
                        .find_session_index(session_id)
                        .and_then(|index| inner.sessions.get(index))
                        .map(|record| {
                            (
                                session_id.clone(),
                                record.session.project_id.clone(),
                                record.engram.clone(),
                            )
                        })
                })
                .collect::<Vec<_>>();
            let previous_orchestrator_projects = inner
                .orchestrator_instances
                .iter()
                .enumerate()
                .filter(|(_, instance)| instance.project_id == project_id)
                .map(|(index, instance)| (index, instance.project_id.clone()))
                .collect::<Vec<_>>();
            let coordination_scope_was_pending = inner
                .pending_coordination_scope_deletions
                .contains(&project_id);
            let previous_response_detachment = inner
                .pending_response_board_project_detachments
                .get(&project_id)
                .cloned();
            let removed_project = inner.projects.remove(project_index);
            for session_id in &final_session_ids {
                if let Some(index) = inner.find_session_index(session_id) {
                    let dispatch_generation = inner.sessions[index].engram.dispatch_generation;
                    let record = inner
                        .session_mut_by_index(index)
                        .expect("session index should be valid");
                    if record.session.project_id.as_deref() == Some(project_id.as_str()) {
                        record.session.project_id = None;
                    }
                    record.engram = EngramSessionState::default();
                    record.engram.dispatch_generation = dispatch_generation;
                }
            }
            for instance in &mut inner.orchestrator_instances {
                if instance.project_id == project_id {
                    instance.project_id.clear();
                }
            }
            inner
                .pending_coordination_scope_deletions
                .insert(project_id.clone());
            inner.pending_response_board_project_detachments.insert(
                project_id.clone(),
                removed_project.name.clone(),
            );
            let previous_runtime_reset_flags = if mcp_runtime_reset_required {
                mark_engram_mcp_runtime_resets_locked(&mut inner, &final_session_ids)
            } else {
                Vec::new()
            };
            let revocation_batch = if mcp_runtime_reset_required {
                claim_engram_mcp_runtime_revocations_locked(
                    &mut inner,
                    &final_session_ids,
                )
            } else {
                EngramMcpRuntimeRevocationBatch::default()
            };
            let persist_dispatch = match self.commit_locked_with_persist_dispatch(&mut inner) {
                Ok((_, persist_dispatch)) => persist_dispatch,
                Err(error) => {
                    inner.projects.insert(project_index, removed_project);
                    for (session_id, previous_project_id, mut previous_engram) in
                        previous_session_states
                    {
                        if let Some((_, checkpointed_grant_id)) = checkpointed
                            .iter()
                            .find(|(checkpointed_session_id, _)| {
                                checkpointed_session_id == &session_id
                            })
                            && previous_engram.active_grant_id.as_deref()
                                == Some(checkpointed_grant_id.as_str())
                        {
                            previous_engram.active_grant_id = None;
                            previous_engram.rebind_required = true;
                        }
                        previous_engram.project_reset_in_progress = false;
                        previous_engram.checkpoint_in_progress = false;
                        if let Some(index) = inner.find_session_index(&session_id) {
                            let record = inner
                                .session_mut_by_index(index)
                                .expect("session index should be valid");
                            record.session.project_id = previous_project_id;
                            record.engram = previous_engram;
                        }
                    }
                    restore_engram_mcp_runtime_resets_locked(
                        &mut inner,
                        previous_runtime_reset_flags,
                    );
                    rollback_engram_mcp_runtime_revocations_locked(
                        &mut inner,
                        &revocation_batch,
                    );
                    for (index, previous_project_id) in previous_orchestrator_projects {
                        if let Some(instance) = inner.orchestrator_instances.get_mut(index) {
                            instance.project_id = previous_project_id;
                        }
                    }
                    if !coordination_scope_was_pending {
                        inner.pending_coordination_scope_deletions.remove(&project_id);
                    }
                    match previous_response_detachment {
                        Some(previous) => {
                            inner
                                .pending_response_board_project_detachments
                                .insert(project_id.clone(), previous);
                        }
                        None => {
                            inner
                                .pending_response_board_project_detachments
                                .remove(&project_id);
                        }
                    }
                    inner
                        .engram_project_resets
                        .release(&project_id, project_reset_generation);
                    return Err(ApiError::internal(format!(
                        "failed to remove project after draining Engram: {error:#}"
                    )));
                }
            };
            (
                inner.engram_host_adapter.clone(),
                final_session_ids,
                persist_dispatch,
                revocation_batch,
            )
        };

        let authority_revocation_reason = project_snapshot
            .engram
            .as_ref()
            .and_then(|settings| settings.work_authority_grant.as_ref())
            .map(|_| "TermAl project deleted");
        let authority_revocation_failure = revoke_project_engram_authority_off_lock(
            &authority_revocation_targets,
            authority_revocation_reason,
        );
        for session_id in &final_session_ids {
            adapter.shutdown_session(session_id);
        }
        let runtime_teardown_result = if mcp_runtime_reset_required {
            let shutdowns = self.shutdown_revoked_engram_mcp_runtimes(
                revocation_batch,
                "deleted project Engram MCP configuration",
            );
            let mut outcome = self.finalize_revoked_engram_mcp_runtimes(shutdowns);
            if let Some(failure) = authority_revocation_failure {
                outcome.failures.push(failure);
            }
            self.resume_revoked_engram_mcp_sessions(&mut outcome);
            self.finish_revoked_engram_mcp_runtime_outcome(outcome)
        } else if let Some(failure) = authority_revocation_failure {
            Err(ApiError::internal(format!(
                "project was deleted, but capability revocation was degraded: {failure}"
            )))
        } else {
            Ok(Vec::new())
        };
        self.release_engram_project_reset_fence(
            &project_id,
            project_reset_generation,
            &final_session_ids,
        );
        let deletion_result = self.finish_project_deletion(&project_id, persist_dispatch);
        match (deletion_result, runtime_teardown_result) {
            (Ok(response), Ok(_)) => Ok(response),
            (Err(deletion_error), Ok(_)) => Err(deletion_error),
            (Ok(_), Err(runtime_error)) => Err(runtime_error),
            (Err(deletion_error), Err(runtime_error)) => Err(ApiError::internal(format!(
                "{}; Engram MCP runtime cleanup also degraded: {}",
                deletion_error.message, runtime_error.message
            ))),
        }
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
