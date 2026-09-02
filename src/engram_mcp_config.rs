// Engram's per-session MCP stdio descriptor for local agent runtimes.
//
// Owns descriptor selection plus the base-tier context refresh lifecycle and
// its bounded `engram work next` child process. Runtime-specific JSON
// composition remains in `delegation_mcp.rs`; premium turn-gated control and
// project settings validation remain in `engram_host_adapter.rs`.

const ENGRAM_MCP_SERVER_NAME: &str = "engram";
const ENGRAM_HOME_ENV: &str = "ENGRAM_HOME";
const ENGRAM_ACTOR_ID_ENV: &str = "ENGRAM_ACTOR_ID";
const ENGRAM_SESSION_ID_ENV: &str = "ENGRAM_SESSION_ID";
const ENGRAM_CONTEXT_NUDGE_MAX_BYTES: usize = 32 * 1024;

struct EngramMcpRuntimeConfig {
    stdio: TermalDelegationMcpStdioConfig,
    installed: EngramMcpInstalledDescriptor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngramContextNudgePreparation {
    NotApplicable,
    Ready,
    Failed,
}

#[derive(Clone)]
struct EngramContextNudgeTarget {
    command: PathBuf,
    home: String,
    project_file: PathBuf,
    project_root: PathBuf,
    actor_id: String,
    session_id: String,
    generation: u64,
    timeout: Duration,
}

impl AppState {
    #[allow(dead_code)]
    fn engram_mcp_stdio_config_for_session(
        &self,
        session_id: &str,
    ) -> Option<TermalDelegationMcpStdioConfig> {
        self.refresh_engram_project_declaration_for_session_off_lock(session_id);
        let inner = self.inner.lock().expect("state mutex poisoned");
        engram_mcp_stdio_config_for_session_locked(&inner, session_id)
    }

    fn engram_mcp_stdio_config_for_runtime(
        &self,
        session_id: &str,
        runtime_token: &RuntimeToken,
    ) -> Option<TermalDelegationMcpStdioConfig> {
        self.refresh_engram_project_declaration_for_session_off_lock(session_id);
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let runtime_config = engram_mcp_runtime_config_for_session_locked(&inner, session_id)?;
        let index = inner.find_session_index(session_id)?;
        if !inner.sessions[index]
            .runtime
            .matches_runtime_token(runtime_token)
        {
            return None;
        }
        inner
            .session_mut_by_index(index)
            .expect("session index should be valid")
            .engram_mcp_installed = Some(runtime_config.installed);
        Some(runtime_config.stdio)
    }

    fn mark_engram_context_nudge_pending(&self, session_id: &str) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        record.engram.invalidate_context_nudge();
    }

    /// Refreshes the repository-declaration cache without holding the global
    /// state mutex across the filesystem probe.
    fn refresh_engram_project_declaration_for_session_off_lock(&self, session_id: &str) -> bool {
        let snapshot = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return false;
            };
            if !inner.sessions[index].is_local_session() {
                inner.sessions[index].engram.context_nudge_pending = false;
                return false;
            }
            let Some(project) = engram_project_for_session_locked(&inner, session_id) else {
                inner.sessions[index].engram.context_nudge_pending = false;
                return false;
            };
            let project_id = project.id.clone();
            let project_root = project.root_path.clone();
            let settings = project.engram.clone();
            let base_enabled = project.remote_id == LOCAL_REMOTE_ID
                && settings
                    .as_ref()
                    .is_some_and(EngramProjectSettings::is_base_enabled);
            if !base_enabled {
                inner.engram_declared_project_ids.remove(&project_id);
                inner
                    .engram_declaration_checked_project_ids
                    .remove(&project_id);
                inner.sessions[index].engram.context_nudge_pending = false;
                return false;
            }
            if inner.engram_project_resets.contains(&project_id) {
                return inner.engram_declared_project_ids.contains(&project_id);
            }
            (project_id, project_root, settings)
        };
        let (project_id, project_root, settings) = snapshot;
        let project_file = PathBuf::from(&project_root).join(".engram-project");
        let declared = fs::metadata(&project_file)
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0);

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let current = engram_project_for_session_locked(&inner, session_id).is_some_and(|project| {
            project.id == project_id
                && project.root_path == project_root
                && project.remote_id == LOCAL_REMOTE_ID
                && project.engram == settings
                && !inner.engram_project_resets.contains(&project_id)
        });
        if !current {
            return false;
        }
        let was_declared = inner.engram_declared_project_ids.contains(&project_id);
        let already_checked = !inner
            .engram_declaration_checked_project_ids
            .insert(project_id.clone());
        if declared {
            inner.engram_declared_project_ids.insert(project_id.clone());
        } else {
            inner.engram_declared_project_ids.remove(&project_id);
        }
        if already_checked && was_declared != declared {
            let affected = inner
                .sessions
                .iter()
                .enumerate()
                .filter(|(_, record)| {
                    record.is_local_session()
                        && engram_project_for_session_locked(&inner, &record.session.id)
                            .is_some_and(|project| project.id == project_id)
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            for index in affected {
                let record = inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                record.runtime_reset_required = true;
                record.engram.invalidate_context_nudge();
            }
        }
        declared
    }

    /// Refreshes the base-tier work context without holding the global state
    /// mutex. Failure is advisory: the prompt still runs and the pending bit
    /// remains set so a later prompt can retry.
    fn prepare_engram_context_nudge_off_lock(
        &self,
        session_id: &str,
    ) -> EngramContextNudgePreparation {
        let mut project_declared =
            self.refresh_engram_project_declaration_for_session_off_lock(session_id);
        let wait_deadline = std::time::Instant::now()
            + ENGRAM_WORK_BINDING_COMMAND_TIMEOUT
            + Duration::from_secs(1);
        let mut waited_for_generation = None;
        loop {
            let target = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return EngramContextNudgePreparation::NotApplicable;
            };
            let record = &inner.sessions[index];
            if !record.is_local_session() {
                return EngramContextNudgePreparation::NotApplicable;
            }
            if !record.engram.context_nudge_pending {
                return EngramContextNudgePreparation::Ready;
            }
            if record.engram.context_nudge_in_progress {
                waited_for_generation = record.engram.context_nudge_in_progress_generation;
                None
            } else {
                if waited_for_generation == Some(record.engram.context_nudge_generation) {
                    return EngramContextNudgePreparation::Failed;
                }
                let Some(project) = engram_project_for_session_locked(&inner, session_id) else {
                    return EngramContextNudgePreparation::NotApplicable;
                };
                let Some(settings) = project.engram.as_ref() else {
                    return EngramContextNudgePreparation::NotApplicable;
                };
                if project.remote_id != LOCAL_REMOTE_ID
                    || inner.engram_project_resets.contains(&project.id)
                    || !settings.is_base_enabled()
                {
                    return EngramContextNudgePreparation::NotApplicable;
                }
                let (Some(command), Some(home)) =
                    (settings.binary_path.as_deref(), settings.home.as_deref())
                else {
                    return EngramContextNudgePreparation::NotApplicable;
                };
                let generation = record.engram.context_nudge_generation.max(1);
                let actor_id = engram_actor_id(record.session.agent);
                let project_root = PathBuf::from(&project.root_path);
                let project_file = project_root.join(".engram-project");
                if !project_declared {
                    return EngramContextNudgePreparation::NotApplicable;
                }
                let target = EngramContextNudgeTarget {
                    command: PathBuf::from(command),
                    home: home.to_owned(),
                    project_file,
                    project_root,
                    actor_id,
                    session_id: session_id.to_owned(),
                    generation,
                    timeout: ENGRAM_WORK_BINDING_COMMAND_TIMEOUT,
                };
                let record = inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                record.engram.context_nudge_in_progress = true;
                record.engram.context_nudge_in_progress_generation = Some(generation);
                record.engram.context_nudge_generation = generation;
                Some(target)
            }
            };

            let Some(target) = target else {
                if std::time::Instant::now() >= wait_deadline {
                    eprintln!(
                        "engram> session={session_id} timed out waiting for context refresh ownership"
                    );
                    return EngramContextNudgePreparation::Failed;
                }
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };

            let result = run_engram_context_nudge(&target);
            project_declared =
                self.refresh_engram_project_declaration_for_session_off_lock(session_id);
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return EngramContextNudgePreparation::NotApplicable;
            };
            let target_still_current = engram_project_for_session_locked(&inner, session_id)
                .filter(|project| {
                    project.remote_id == LOCAL_REMOTE_ID
                        && !inner.engram_project_resets.contains(&project.id)
                })
                .and_then(|project| project.engram.as_ref().map(|settings| (project, settings)))
                .is_some_and(|(project, settings)| {
                    settings.is_base_enabled()
                        && PathBuf::from(&project.root_path) == target.project_root
                        && settings.binary_path.as_deref()
                            == Some(target.command.to_string_lossy().as_ref())
                        && settings.home.as_deref() == Some(target.home.as_str())
                        && project_declared
                });
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            if record.engram.context_nudge_in_progress_generation == Some(target.generation) {
                record.engram.context_nudge_in_progress = false;
                record.engram.context_nudge_in_progress_generation = None;
            }
            if record.engram.context_nudge_generation != target.generation {
                drop(inner);
                continue;
            }
            if !target_still_current {
                record.engram.context_nudge_pending = false;
                record.engram.pending_context_nudge = None;
                return EngramContextNudgePreparation::NotApplicable;
            }
            match result {
                Ok(context) => {
                    record.engram.context_nudge_pending = false;
                    record.engram.pending_context_nudge = (!context.is_empty()).then_some(context);
                    return EngramContextNudgePreparation::Ready;
                }
                Err(error) => {
                    eprintln!(
                        "engram> session={} context nudge failed: {error}",
                        target.session_id
                    );
                    return EngramContextNudgePreparation::Failed;
                }
            }
        }
    }

    fn acknowledge_engram_context_nudge_delivery(
        &self,
        session_id: &str,
        active_turn_generation: u64,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if record.engram.context_nudge_delivery_turn_generation
            != Some(active_turn_generation)
        {
            return;
        }
        if record.engram.context_nudge_delivery_generation
            == Some(record.engram.context_nudge_generation)
        {
            record.engram.pending_context_nudge = None;
        }
        record.engram.context_nudge_delivery_generation = None;
        record.engram.context_nudge_delivery_turn_generation = None;
    }
}

fn run_engram_context_nudge(
    target: &EngramContextNudgeTarget,
) -> std::result::Result<String, String> {
    let context_generation = format!("termal-{}", target.generation);
    let mut command = engram_command(&target.command);
    configure_terminal_process_tree(&mut command);
    let mut child = command
        .arg("--project-file")
        .arg(&target.project_file)
        .arg("--home")
        .arg(&target.home)
        .arg("work")
        .arg("next")
        .arg("--context-generation")
        .arg(context_generation)
        .env(ENGRAM_HOME_ENV, &target.home)
        .env(ENGRAM_ACTOR_ID_ENV, &target.actor_id)
        .env(ENGRAM_SESSION_ID_ENV, &target.session_id)
        .current_dir(&target.project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to start `engram work next`: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "`engram work next` stdout is unavailable".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "`engram work next` stderr is unavailable".to_owned())?;
    let process = Arc::new(
        SharedChild::new(child)
            .map_err(|error| format!("failed sharing `engram work next`: {error}"))?,
    );
    let process_tree = EngramProcessTree::attach(&process).map_err(|error| {
        let _ = kill_child_process(&process, "Engram context nudge");
        let _ = process.wait();
        format!("failed preparing `engram work next` process tree: {error:#}")
    })?;
    process_tree.resume_after_attach(&process).map_err(|error| {
        let _ = process_tree.terminate(&process);
        let _ = process.wait();
        format!("failed resuming `engram work next`: {error:#}")
    })?;
    let stdout_reader = std::thread::spawn(move || read_engram_cli_output(stdout));
    let stderr_reader = std::thread::spawn(move || read_engram_cli_output(stderr));
    let timeout = target.timeout;
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match process.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) => {
                let _ = process_tree.terminate(&process);
                let _ = process.wait();
                break Err(format!(
                    "`engram work next` exceeded {} ms",
                    timeout.as_millis()
                ));
            }
            Err(error) => {
                let _ = process_tree.terminate(&process);
                let _ = process.wait();
                break Err(format!("failed waiting for `engram work next`: {error}"));
            }
        }
    };
    let stdout = join_engram_cli_output(stdout_reader, "stdout")
        .map_err(|error| error.message)?;
    let stderr = join_engram_cli_output(stderr_reader, "stderr")
        .map_err(|error| error.message)?;
    let status = status?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr).trim().to_owned();
        return Err(if stderr.is_empty() {
            format!("`engram work next` exited with {status}")
        } else {
            format!("`engram work next` failed: {stderr}")
        });
    }
    let mut value = String::from_utf8(stdout)
        .map_err(|error| format!("`engram work next` returned non-UTF-8 text: {error}"))?;
    if truncate_engram_context_nudge(&mut value) {
        eprintln!(
            "engram> session={} truncated work context to {} bytes",
            target.session_id, ENGRAM_CONTEXT_NUDGE_MAX_BYTES
        );
    }
    Ok(value.trim().to_owned())
}

fn truncate_engram_context_nudge(value: &mut String) -> bool {
    if value.len() <= ENGRAM_CONTEXT_NUDGE_MAX_BYTES {
        return false;
    }
    let mut boundary = ENGRAM_CONTEXT_NUDGE_MAX_BYTES;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    true
}

#[allow(dead_code)]
fn engram_mcp_stdio_config_for_session_locked(
    inner: &StateInner,
    session_id: &str,
) -> Option<TermalDelegationMcpStdioConfig> {
    engram_mcp_runtime_config_for_session_locked(inner, session_id).map(|config| config.stdio)
}

fn engram_mcp_runtime_config_for_session_locked(
    inner: &StateInner,
    session_id: &str,
) -> Option<EngramMcpRuntimeConfig> {
    let session = inner
        .find_session_index(session_id)
        .and_then(|index| inner.sessions.get(index))?;
    if !session.is_local_session() {
        return None;
    }
    let project = engram_project_for_session_locked(inner, session_id)?;
    if project.remote_id != LOCAL_REMOTE_ID || inner.engram_project_resets.contains(&project.id) {
        return None;
    }
    let settings = project.engram.as_ref()?;
    if !settings.is_base_enabled() {
        return None;
    }
    let command = settings.binary_path.as_deref()?.to_owned();
    let home = settings.home.as_deref()?;
    if !inner.engram_declared_project_ids.contains(&project.id) {
        return None;
    }
    let project_file = PathBuf::from(&project.root_path).join(".engram-project");
    let args = vec![
        "--project-file".to_owned(),
        project_file.to_string_lossy().into_owned(),
        "--home".to_owned(),
        home.to_owned(),
        "mcp".to_owned(),
        "--actor-id".to_owned(),
        engram_actor_id(session.session.agent),
        "--session-id".to_owned(),
        session_id.to_owned(),
    ];
    let mut env = BTreeMap::new();
    env.insert(ENGRAM_HOME_ENV.to_owned(), home.to_owned());
    env.insert(
        ENGRAM_ACTOR_ID_ENV.to_owned(),
        engram_actor_id(session.session.agent),
    );
    env.insert(ENGRAM_SESSION_ID_ENV.to_owned(), session_id.to_owned());
    let installed = EngramMcpInstalledDescriptor {
        binary_path: command.clone(),
        home: home.to_owned(),
        store_key: settings.authority_store_key.clone(),
        work_authority_grant: settings.work_authority_grant.clone(),
    };
    Some(EngramMcpRuntimeConfig {
        stdio: TermalDelegationMcpStdioConfig { command, args, env },
        installed,
    })
}
