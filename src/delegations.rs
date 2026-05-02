// Phase 1 delegation records: durable parent-child metadata plus read-only
// child sessions. The child remains a normal session; this layer owns parent
// cards, lifecycle/result metadata, compact `/api/state` summaries, result
// parsing, read-only guard integration, queue cleanup, and delegation SSE
// deltas. It does not remote-forward delegation creation or implement writable
// worker policies yet.

// Keep prompts bounded because they are embedded into child-agent startup input.
const MAX_DELEGATION_PROMPT_BYTES: usize = 64 * 1024;
// Public summaries ride in `/api/state`; full summaries stay behind result reads.
const MAX_DELEGATION_PUBLIC_SUMMARY_CHARS: usize = 1000;
// Result packets are expected near the end of long assistant output.
const DELEGATION_RESULT_PACKET_SEARCH_BYTES: usize = 32 * 1024;
// Phase 1 starts children immediately but still enforces simple fan-out limits.
const MAX_RUNNING_DELEGATIONS_PER_PARENT: usize = 4;
// Keep nesting shallow until delegation ownership/scheduling is explicit.
const MAX_DELEGATION_DEPTH: usize = 3;
// Shared conflict text for create/cancel races before child dispatch starts.
const DELEGATION_NO_LONGER_STARTABLE_MESSAGE: &str = "delegation is no longer running";
#[cfg(test)]
const TEST_FORCE_DELEGATION_START_FAILURE_PROMPT: &str =
    "TERMAL_TEST_FORCE_DELEGATION_START_FAILURE";
#[cfg(test)]
const TEST_CANCEL_DELEGATION_BEFORE_START_PROMPT: &str =
    "TERMAL_TEST_CANCEL_DELEGATION_BEFORE_START";

#[derive(Clone, Debug)]
enum ParentDelegationCardDelta {
    Created {
        session_id: String,
        message_id: String,
        message_index: usize,
        message_count: u32,
        message: Message,
        preview: String,
        status: SessionStatus,
        session_mutation_stamp: u64,
    },
    Updated {
        session_id: String,
        message_id: String,
        message_index: usize,
        message_count: u32,
        agents: Vec<ParallelAgentProgress>,
        preview: String,
        session_mutation_stamp: u64,
    },
}

#[derive(Clone, Debug)]
enum DelegationChildTranscriptDelta {
    MessageCreated {
        session_id: String,
        message_id: String,
        message_index: usize,
        message_count: u32,
        message: Message,
        preview: String,
        status: SessionStatus,
        session_mutation_stamp: u64,
    },
    MessageUpdated {
        session_id: String,
        message_id: String,
        message_index: usize,
        message_count: u32,
        message: Message,
        preview: String,
        status: SessionStatus,
        session_mutation_stamp: u64,
    },
}

#[derive(Clone, Debug)]
enum DelegationLifecycleDelta {
    Updated {
        delegation_id: String,
        status: DelegationStatus,
        updated_at: String,
        parent_card_delta: Option<ParentDelegationCardDelta>,
    },
    Completed {
        delegation_id: String,
        result: DelegationResult,
        completed_at: String,
        parent_card_delta: Option<ParentDelegationCardDelta>,
    },
    Failed {
        delegation_id: String,
        result: DelegationResult,
        failed_at: String,
        parent_card_delta: Option<ParentDelegationCardDelta>,
    },
    Canceled {
        delegation_id: String,
        canceled_at: String,
        reason: Option<String>,
        parent_card_delta: Option<ParentDelegationCardDelta>,
    },
}

struct RemovedSessionDelegationReconciliation {
    lifecycle_deltas: Vec<DelegationLifecycleDelta>,
    child_transcript_deltas: Vec<DelegationChildTranscriptDelta>,
    runtimes_to_kill: Vec<KillableRuntime>,
}

#[derive(Default)]
struct DetachedDelegationChildRuntime {
    runtime: Option<KillableRuntime>,
    transcript_deltas: Vec<DelegationChildTranscriptDelta>,
}

impl StateInner {
    fn rebuild_running_read_only_delegations(&mut self) {
        self.running_read_only_delegations = self
            .delegations
            .iter()
            .enumerate()
            .filter_map(|(index, delegation)| {
                running_read_only_delegation_index_entry(delegation, index)
            })
            .collect();
    }

    fn sync_running_read_only_delegation_index(&mut self, delegation_index: usize) {
        if let Some(delegation) = self.delegations.get(delegation_index) {
            if running_read_only_delegation_index_entry(delegation, delegation_index).is_some() {
                self.running_read_only_delegations.insert(delegation_index);
            } else {
                self.running_read_only_delegations.remove(&delegation_index);
            }
        } else {
            self.running_read_only_delegations.remove(&delegation_index);
        }
    }

    fn next_delegation_id(&self) -> String {
        format!("delegation-{}", Uuid::new_v4())
    }

    fn find_delegation_index(&self, delegation_id: &str) -> Option<usize> {
        self.delegations
            .iter()
            .position(|record| record.id == delegation_id)
    }

    fn find_delegation_index_by_child_session_id(&self, child_session_id: &str) -> Option<usize> {
        self.delegations
            .iter()
            .position(|record| record.child_session_id == child_session_id)
    }
}

fn running_read_only_delegation_index_entry(
    delegation: &DelegationRecord,
    index: usize,
) -> Option<usize> {
    (!delegation_is_terminal(delegation.status)
        && delegation.write_policy == DelegationWritePolicy::ReadOnly)
        .then_some(index)
}

fn find_parent_delegation_index_locked(
    inner: &StateInner,
    parent_session_id: &str,
    delegation_id: &str,
) -> Result<usize, ApiError> {
    let Some(parent_session_id) = normalize_optional_identifier(Some(parent_session_id)) else {
        return Err(ApiError::not_found("delegation not found"));
    };
    if inner
        .find_visible_session_index(parent_session_id)
        .is_none()
    {
        return Err(ApiError::not_found("delegation not found"));
    }
    let index = inner
        .find_delegation_index(delegation_id)
        .ok_or_else(|| ApiError::not_found("delegation not found"))?;
    if inner.delegations[index].parent_session_id != parent_session_id {
        return Err(ApiError::not_found("delegation not found"));
    }
    Ok(index)
}

impl AppState {
    fn create_read_only_delegation(
        &self,
        parent_session_id: &str,
        request: CreateDelegationRequest,
    ) -> Result<DelegationResponse, ApiError> {
        let parent_session_id = normalize_optional_identifier(Some(parent_session_id))
            .ok_or_else(|| ApiError::bad_request("parent session id is required"))?
            .to_owned();
        let prompt = request.prompt.trim();
        if prompt.is_empty() {
            return Err(ApiError::bad_request("delegation prompt cannot be empty"));
        }
        if prompt.len() > MAX_DELEGATION_PROMPT_BYTES {
            return Err(ApiError::bad_request(format!(
                "delegation prompt must be at most {} bytes",
                MAX_DELEGATION_PROMPT_BYTES
            )));
        }
        let mode = request.mode.unwrap_or(DelegationMode::Reviewer);
        if mode == DelegationMode::Worker {
            return Err(ApiError::from_status(
                StatusCode::NOT_IMPLEMENTED,
                "worker delegations are not implemented in Phase 1",
            ));
        }
        let write_policy = request
            .write_policy
            .unwrap_or(DelegationWritePolicy::ReadOnly);
        if write_policy != DelegationWritePolicy::ReadOnly {
            return Err(ApiError::from_status(
                StatusCode::NOT_IMPLEMENTED,
                "only readOnly delegation write policy is implemented in Phase 1",
            ));
        }

        let (parent_workdir, parent_project_id, parent_agent, parent_is_remote_backed) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let parent_index = inner
                .find_visible_session_index(&parent_session_id)
                .ok_or_else(ApiError::local_session_missing)?;
            let parent_record = &inner.sessions[parent_index];
            let parent = &parent_record.session;
            (
                parent.workdir.clone(),
                parent.project_id.clone(),
                parent.agent,
                parent_record.remote_id.is_some() || parent_record.remote_session_id.is_some(),
            )
        };
        if parent_is_remote_backed {
            return Err(ApiError::from_status(
                StatusCode::NOT_IMPLEMENTED,
                "delegations for remote-backed sessions are not implemented in Phase 1",
            ));
        }

        let requested_cwd = request
            .cwd
            .as_deref()
            .map(|cwd| {
                validate_delegation_cwd_input(cwd)?;
                resolve_session_workdir(cwd)
            })
            .transpose()?;
        let cwd = requested_cwd.unwrap_or(parent_workdir);
        if let Some(project_id) = parent_project_id.as_deref() {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let project = inner
                .find_project(project_id)
                .ok_or_else(|| ApiError::bad_request(format!("unknown project `{project_id}`")))?;
            if project.remote_id != LOCAL_REMOTE_ID {
                return Err(ApiError::from_status(
                    StatusCode::NOT_IMPLEMENTED,
                    "delegations for remote-backed projects are not implemented in Phase 1",
                ));
            }
            if !path_contains(&project.root_path, FsPath::new(&cwd)) {
                return Err(ApiError::bad_request(format!(
                    "delegation cwd `{cwd}` must stay inside project `{}`",
                    project.name
                )));
            }
        }

        let agent = request.agent.unwrap_or(parent_agent);
        let has_test_runtime_override = {
            #[cfg(test)]
            {
                agent
                    .acp_runtime()
                    .is_some_and(|acp_agent| self.has_test_acp_runtime_override(acp_agent))
            }
            #[cfg(not(test))]
            {
                false
            }
        };
        if !has_test_runtime_override {
            validate_agent_session_setup(agent, &cwd).map_err(ApiError::bad_request)?;
        }
        self.invalidate_agent_readiness_cache();
        let _ = self.agent_readiness_snapshot();
        let title = request
            .title
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| "Delegated review".to_owned());
        let model = request
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        inner
            .find_visible_session_index(&parent_session_id)
            .ok_or_else(ApiError::local_session_missing)?;
        if active_delegation_count_for_parent(&inner, &parent_session_id)
            >= MAX_RUNNING_DELEGATIONS_PER_PARENT
        {
            return Err(ApiError::conflict(format!(
                "parent session already has {MAX_RUNNING_DELEGATIONS_PER_PARENT} active delegations"
            )));
        }
        if delegation_depth_for_parent(&inner, &parent_session_id) >= MAX_DELEGATION_DEPTH {
            return Err(ApiError::conflict(format!(
                "delegation nesting depth is limited to {MAX_DELEGATION_DEPTH}"
            )));
        }
        let delegation_id = inner.next_delegation_id();
        let now = stamp_now();
        let child_record = inner.create_session(
            agent,
            Some(title.clone()),
            cwd.clone(),
            parent_project_id.clone(),
            model,
        );
        let child_session_id = child_record.session.id.clone();
        let child_index = inner
            .find_session_index(&child_session_id)
            .expect("just-created child session should be indexed");
        {
            let child_record = inner
                .session_mut_by_index(child_index)
                .expect("child session index should be valid");
            if child_record.session.agent.supports_codex_prompt_settings() {
                child_record.codex_approval_policy = CodexApprovalPolicy::Never;
                child_record.codex_sandbox_mode = CodexSandboxMode::ReadOnly;
                child_record.session.approval_policy = Some(CodexApprovalPolicy::Never);
                child_record.session.sandbox_mode = Some(CodexSandboxMode::ReadOnly);
            } else if child_record.session.agent.supports_cursor_mode() {
                child_record.session.cursor_mode = Some(CursorMode::Plan);
            } else if child_record.session.agent.supports_claude_approval_mode() {
                child_record.session.claude_approval_mode = Some(ClaudeApprovalMode::Plan);
            } else if child_record.session.agent.supports_gemini_approval_mode() {
                child_record.session.gemini_approval_mode = Some(GeminiApprovalMode::Plan);
            }
            child_record.session.parent_delegation_id = Some(delegation_id.clone());
        }
        let child_session = Self::wire_session_from_record(&inner.sessions[child_index]);
        let child_delta_session =
            Self::wire_session_summary_from_record(&inner.sessions[child_index]);
        let record = DelegationRecord {
            id: delegation_id.clone(),
            parent_session_id,
            child_session_id: child_session_id.clone(),
            mode,
            status: DelegationStatus::Running,
            title,
            prompt: prompt.to_owned(),
            cwd,
            agent: child_session.agent,
            model: Some(child_session.model.clone()),
            write_policy,
            created_at: now.clone(),
            started_at: Some(now),
            completed_at: None,
            result: None,
        };
        let delegation_index = inner.delegations.len();
        inner.delegations.push(record.clone());
        inner.mark_delegations_mutated();
        inner.sync_running_read_only_delegation_index(delegation_index);
        let parent_card_delta = add_parent_delegation_card_locked(&mut inner, &record);
        let revision = self
            .commit_locked(&mut inner)
            .map_err(|err| ApiError::internal(format!("failed to persist delegation: {err:#}")))?;
        drop(inner);
        self.publish_delta(&DeltaEvent::SessionCreated {
            revision,
            session_id: child_session.id.clone(),
            session: child_delta_session,
        });
        if let Some(delta) = parent_card_delta {
            self.publish_parent_delegation_card_delta(revision, delta);
        }
        self.publish_delta(&DeltaEvent::DelegationCreated {
            revision,
            delegation: delegation_summary_from_record(&record),
        });

        #[cfg(test)]
        if record
            .prompt
            .contains(TEST_CANCEL_DELEGATION_BEFORE_START_PROMPT)
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if let Some(index) = inner.find_delegation_index(&record.id) {
                if let Some(delta) = mark_delegation_canceled_locked(
                    &mut inner,
                    index,
                    Some("test canceled delegation before child start".to_owned()),
                ) {
                    let revision = self.commit_locked(&mut inner).map_err(|err| {
                        ApiError::internal(format!(
                            "failed to persist test delegation cancelation: {err:#}"
                        ))
                    })?;
                    drop(inner);
                    self.publish_delegation_lifecycle_delta(revision, delta);
                }
            }
        }

        let runtime_prompt = build_read_only_delegation_prompt(&record);
        if let Err(err) =
            self.start_delegation_child_turn(&record.id, &record.child_session_id, runtime_prompt)
        {
            if err.status == StatusCode::CONFLICT
                && err.message == DELEGATION_NO_LONGER_STARTABLE_MESSAGE
            {
                return self.delegation_response_from_state(&record.id);
            }
            let detail = format!("failed to start child session: {}", err.message);
            self.mark_delegation_failed_after_start_error(
                &record.id,
                &record.child_session_id,
                &detail,
            )?;
            return self.delegation_response_from_state(&record.id);
        }

        let inner = self.inner.lock().expect("state mutex poisoned");
        let latest_delegation = inner
            .find_delegation_index(&record.id)
            .and_then(|index| inner.delegations.get(index))
            .cloned()
            .ok_or_else(|| ApiError::internal("created delegation disappeared"))?;
        let child_session = inner
            .find_session_index(&record.child_session_id)
            .and_then(|index| inner.sessions.get(index))
            .map(Self::wire_session_from_record)
            .ok_or_else(|| ApiError::internal("created child session disappeared"))?;
        let latest_revision = inner.revision;

        Ok(DelegationResponse {
            revision: latest_revision,
            delegation: latest_delegation,
            child_session,
            server_instance_id: self.server_instance_id.clone(),
        })
    }

    fn delegation_response_from_state(
        &self,
        delegation_id: &str,
    ) -> Result<DelegationResponse, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let delegation = inner
            .find_delegation_index(delegation_id)
            .and_then(|index| inner.delegations.get(index))
            .cloned()
            .ok_or_else(|| ApiError::not_found("delegation not found"))?;
        let child_session = inner
            .find_session_index(&delegation.child_session_id)
            .and_then(|index| inner.sessions.get(index))
            .map(Self::wire_session_from_record)
            .ok_or_else(|| ApiError::not_found("delegation child session not found"))?;
        Ok(DelegationResponse {
            revision: inner.revision,
            delegation,
            child_session,
            server_instance_id: self.server_instance_id.clone(),
        })
    }

    fn get_delegation(
        &self,
        parent_session_id: &str,
        delegation_id: &str,
    ) -> Result<DelegationStatusResponse, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = find_parent_delegation_index_locked(&inner, parent_session_id, delegation_id)?;
        let delegation = inner.delegations[index].clone();
        Ok(DelegationStatusResponse {
            revision: inner.revision,
            delegation,
            server_instance_id: self.server_instance_id.clone(),
        })
    }

    fn get_delegation_result(
        &self,
        parent_session_id: &str,
        delegation_id: &str,
    ) -> Result<DelegationResultResponse, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = find_parent_delegation_index_locked(&inner, parent_session_id, delegation_id)?;
        let delegation = &inner.delegations[index];
        let result = delegation
            .result
            .clone()
            .ok_or_else(|| ApiError::conflict("delegation result is not available yet"))?;
        Ok(DelegationResultResponse {
            revision: inner.revision,
            result,
            server_instance_id: self.server_instance_id.clone(),
        })
    }

    fn cancel_delegation(
        &self,
        parent_session_id: &str,
        delegation_id: &str,
    ) -> Result<DelegationStatusResponse, ApiError> {
        let child_session_id = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index =
                find_parent_delegation_index_locked(&inner, parent_session_id, delegation_id)?;
            let lifecycle_delta = refresh_delegation_from_child_locked(&mut inner, index);
            let revision = if lifecycle_delta.is_some() {
                self.commit_locked(&mut inner).map_err(|err| {
                    ApiError::internal(format!(
                        "failed to persist delegation status before cancel: {err:#}"
                    ))
                })?
            } else {
                inner.revision
            };
            let delegation = &inner.delegations[index];
            if delegation_is_terminal(delegation.status) {
                let response = DelegationStatusResponse {
                    revision,
                    delegation: delegation.clone(),
                    server_instance_id: self.server_instance_id.clone(),
                };
                drop(inner);
                if let Some(delta) = lifecycle_delta {
                    self.publish_delegation_lifecycle_delta(revision, delta);
                }
                return Ok(response);
            }
            let child_session_id = delegation.child_session_id.clone();
            drop(inner);
            if let Some(delta) = lifecycle_delta {
                self.publish_delegation_lifecycle_delta(revision, delta);
            }
            child_session_id
        };

        let stop_conflicted = match self.stop_session_with_options(
            &child_session_id,
            StopSessionOptions {
                dispatch_queued_prompts_on_success: false,
                orchestrator_stop_instance_id: None,
            },
        ) {
            Ok(_) => false,
            Err(err)
                if err.status == StatusCode::CONFLICT || err.status == StatusCode::NOT_FOUND =>
            {
                true
            }
            Err(err) => return Err(err),
        };

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = find_parent_delegation_index_locked(&inner, parent_session_id, delegation_id)?;
        let lifecycle_delta = if stop_conflicted {
            let refreshed_delta = refresh_delegation_from_child_locked(&mut inner, index);
            if delegation_is_terminal(inner.delegations[index].status) {
                refreshed_delta
            } else {
                mark_delegation_canceled_locked(&mut inner, index, None)
            }
        } else {
            mark_delegation_canceled_locked(&mut inner, index, None)
        };
        let revision = if lifecycle_delta.is_some() {
            self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist delegation cancelation: {err:#}"))
            })?
        } else {
            inner.revision
        };
        let delegation = inner.delegations[index].clone();
        drop(inner);
        if let Some(delta) = lifecycle_delta {
            self.publish_delegation_lifecycle_delta(revision, delta);
        }
        Ok(DelegationStatusResponse {
            revision,
            delegation,
            server_instance_id: self.server_instance_id.clone(),
        })
    }

    fn refresh_delegation_for_child_session(&self, child_session_id: &str) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_delegation_index_by_child_session_id(child_session_id) else {
            return Ok(());
        };
        let Some(lifecycle_delta) = refresh_delegation_from_child_locked(&mut inner, index) else {
            return Ok(());
        };
        let revision = self.commit_locked(&mut inner)?;
        drop(inner);
        self.publish_delegation_lifecycle_delta(revision, lifecycle_delta);
        Ok(())
    }

    fn ensure_read_only_delegation_allows_write_action(
        &self,
        session_id: Option<&str>,
        project_id: Option<&str>,
        workdir: Option<&str>,
        action: &str,
    ) -> Result<(), ApiError> {
        let session_id = normalize_optional_identifier(session_id);
        let project_id = normalize_optional_identifier(project_id);
        let workdir = workdir.map(str::trim).filter(|value| !value.is_empty());
        if session_id.is_none() && project_id.is_none() && workdir.is_none() {
            return Ok(());
        }
        let (
            blocked_by_session_delegation,
            mut write_targets,
            delegated_write_scopes,
            local_projects,
        ) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let blocked_by_session_delegation =
                read_only_session_delegation_block_locked(&inner, session_id);
            if blocked_by_session_delegation.is_some() {
                (
                    blocked_by_session_delegation,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                )
            } else {
                // Missing/non-visible sessions do not short-circuit here: explicit
                // project/workdir scope may still overlap a running delegation.
                let session_record = session_id
                    .and_then(|session_id| inner.find_visible_session_index(session_id))
                    .and_then(|session_index| inner.sessions.get(session_index));

                let mut write_targets = Vec::new();
                if project_id.is_some() || workdir.is_some() {
                    write_targets.push(DelegationWriteTarget {
                        project_id: project_id.map(str::to_owned),
                        workdir: workdir.map(str::to_owned),
                    });
                }
                if let Some(record) = session_record {
                    let session_workdir = record.session.workdir.trim();
                    write_targets.push(DelegationWriteTarget {
                        project_id: record.session.project_id.clone(),
                        workdir: if session_workdir.is_empty() {
                            None
                        } else {
                            Some(session_workdir.to_owned())
                        },
                    });
                }

                let delegated_write_scopes = if write_targets
                    .iter()
                    .any(|target| target.project_id.is_some() || target.workdir.is_some())
                {
                    inner
                        .running_read_only_delegations
                        .iter()
                        .filter_map(|delegation_index| inner.delegations.get(*delegation_index))
                        .filter_map(|delegation| {
                            let session_scope = inner
                                .find_session_index(&delegation.child_session_id)
                                .and_then(|index| inner.sessions.get(index));
                            let (project_id, workdir) = session_scope
                                .map(|record| {
                                    (
                                        record.session.project_id.clone(),
                                        record.session.workdir.trim().to_owned(),
                                    )
                                })
                                .unwrap_or_else(|| (None, delegation.cwd.trim().to_owned()));
                            Some(DelegationWriteScope {
                                title: delegation.title.clone(),
                                project_id,
                                workdir,
                            })
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                let needs_local_project_inference = write_targets.iter().any(|target| {
                    target.workdir.is_some()
                        || (target.project_id.is_some() && target.workdir.is_none())
                });
                let local_projects: Vec<LocalProjectWriteScope> = if needs_local_project_inference {
                    inner
                        .projects
                        .iter()
                        .filter(|project| project.remote_id == LOCAL_REMOTE_ID)
                        .filter_map(|project| {
                            let root_path = project.root_path.trim();
                            (!root_path.is_empty()).then(|| LocalProjectWriteScope {
                                project_id: project.id.clone(),
                                root_path: root_path.to_owned(),
                            })
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                (
                    blocked_by_session_delegation,
                    write_targets,
                    delegated_write_scopes,
                    local_projects,
                )
            }
        };
        append_local_project_write_targets(&mut write_targets, &local_projects);
        let blocked_by_scope_delegation = delegated_write_scopes.iter().find(|scope| {
            write_targets.iter().any(|target| {
                delegation_write_scope_matches(
                    scope,
                    target.project_id.as_deref(),
                    target.workdir.as_deref(),
                )
            })
        });
        if blocked_by_session_delegation.is_some() || blocked_by_scope_delegation.is_some() {
            let delegation_title = blocked_by_session_delegation
                .and_then(|block| block.title)
                .or_else(|| blocked_by_scope_delegation.map(|scope| scope.title.clone()));
            let detail = delegation_title
                .map(|title| format!(" while read-only delegation `{title}` is running"))
                .unwrap_or_else(|| {
                    " while an expired read-only delegation is still attached".to_owned()
                });
            return Err(ApiError::from_status(
                StatusCode::FORBIDDEN,
                format!("{action} is disabled for read-only delegated sessions{detail}"),
            ));
        }
        Ok(())
    }

    fn ensure_read_only_delegation_allows_session_write_action(
        &self,
        session_id: Option<&str>,
        action: &str,
    ) -> Result<(), ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        if let Some(block) = read_only_session_delegation_block_locked(&inner, session_id) {
            let detail = block
                .title
                .map(|title| format!(" while read-only delegation `{title}` is running"))
                .unwrap_or_else(|| {
                    " while an expired read-only delegation is still attached".to_owned()
                });
            return Err(ApiError::from_status(
                StatusCode::FORBIDDEN,
                format!("{action} is disabled for read-only delegated sessions{detail}"),
            ));
        }
        Ok(())
    }

    #[cfg_attr(test, allow(dead_code))]
    fn mark_delegation_failed_after_start_error(
        &self,
        delegation_id: &str,
        child_session_id: &str,
        detail: &str,
    ) -> Result<(), ApiError> {
        let (revision, lifecycle_delta, detached_child) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_delegation_index(delegation_id)
                .ok_or_else(|| ApiError::not_found("delegation not found"))?;
            let Some(lifecycle_delta) = mark_delegation_failed_locked(&mut inner, index, detail)
            else {
                return Ok(());
            };
            let detached_child =
                detach_delegation_child_runtime_locked(&mut inner, child_session_id, None);
            let revision = self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist delegation failure: {err:#}"))
            })?;
            (revision, lifecycle_delta, detached_child)
        };

        if let Some(runtime) = detached_child.runtime {
            if let Err(err) =
                shutdown_removed_runtime(runtime, &format!("failed delegation `{delegation_id}`"))
            {
                eprintln!("delegation cleanup warning> {err:#}");
            }
        }
        for delta in detached_child.transcript_deltas {
            self.publish_delegation_child_transcript_delta(revision, delta);
        }
        self.publish_delegation_lifecycle_delta(revision, lifecycle_delta);
        Ok(())
    }

    fn publish_delegation_lifecycle_delta(&self, revision: u64, delta: DelegationLifecycleDelta) {
        match delta {
            DelegationLifecycleDelta::Updated {
                delegation_id,
                status,
                updated_at,
                parent_card_delta,
            } => {
                self.publish_delta(&DeltaEvent::DelegationUpdated {
                    revision,
                    delegation_id,
                    status,
                    updated_at,
                });
                if let Some(delta) = parent_card_delta {
                    self.publish_parent_delegation_card_delta(revision, delta);
                }
            }
            DelegationLifecycleDelta::Completed {
                delegation_id,
                result,
                completed_at,
                parent_card_delta,
            } => {
                self.publish_delta(&DeltaEvent::DelegationCompleted {
                    revision,
                    delegation_id,
                    result: delegation_result_summary(&result),
                    completed_at,
                });
                if let Some(delta) = parent_card_delta {
                    self.publish_parent_delegation_card_delta(revision, delta);
                }
            }
            DelegationLifecycleDelta::Failed {
                delegation_id,
                result,
                failed_at,
                parent_card_delta,
            } => {
                self.publish_delta(&DeltaEvent::DelegationFailed {
                    revision,
                    delegation_id,
                    result: delegation_result_summary(&result),
                    failed_at,
                });
                if let Some(delta) = parent_card_delta {
                    self.publish_parent_delegation_card_delta(revision, delta);
                }
            }
            DelegationLifecycleDelta::Canceled {
                delegation_id,
                canceled_at,
                reason,
                parent_card_delta,
            } => {
                self.publish_delta(&DeltaEvent::DelegationCanceled {
                    revision,
                    delegation_id,
                    canceled_at,
                    reason,
                });
                if let Some(delta) = parent_card_delta {
                    self.publish_parent_delegation_card_delta(revision, delta);
                }
            }
        }
    }

    fn publish_parent_delegation_card_delta(
        &self,
        revision: u64,
        delta: ParentDelegationCardDelta,
    ) {
        match delta {
            ParentDelegationCardDelta::Created {
                session_id,
                message_id,
                message_index,
                message_count,
                message,
                preview,
                status,
                session_mutation_stamp,
            } => self.publish_delta(&DeltaEvent::MessageCreated {
                revision,
                session_id,
                message_id,
                message_index,
                message_count,
                message,
                preview,
                status,
                session_mutation_stamp: Some(session_mutation_stamp),
            }),
            ParentDelegationCardDelta::Updated {
                session_id,
                message_id,
                message_index,
                message_count,
                agents,
                preview,
                session_mutation_stamp,
            } => self.publish_delta(&DeltaEvent::ParallelAgentsUpdate {
                revision,
                session_id,
                message_id,
                message_index,
                message_count,
                agents,
                preview,
                session_mutation_stamp: Some(session_mutation_stamp),
            }),
        }
    }

    fn publish_delegation_child_transcript_delta(
        &self,
        revision: u64,
        delta: DelegationChildTranscriptDelta,
    ) {
        match delta {
            DelegationChildTranscriptDelta::MessageCreated {
                session_id,
                message_id,
                message_index,
                message_count,
                message,
                preview,
                status,
                session_mutation_stamp,
            } => self.publish_delta(&DeltaEvent::MessageCreated {
                revision,
                session_id,
                message_id,
                message_index,
                message_count,
                message,
                preview,
                status,
                session_mutation_stamp: Some(session_mutation_stamp),
            }),
            DelegationChildTranscriptDelta::MessageUpdated {
                session_id,
                message_id,
                message_index,
                message_count,
                message,
                preview,
                status,
                session_mutation_stamp,
            } => self.publish_delta(&DeltaEvent::MessageUpdated {
                revision,
                session_id,
                message_id,
                message_index,
                message_count,
                message,
                preview,
                status,
                session_mutation_stamp: Some(session_mutation_stamp),
            }),
        }
    }

    fn start_delegation_child_turn(
        &self,
        delegation_id: &str,
        child_session_id: &str,
        runtime_prompt: String,
    ) -> Result<(), ApiError> {
        #[cfg(test)]
        if runtime_prompt.contains(TEST_FORCE_DELEGATION_START_FAILURE_PROMPT) {
            return Err(ApiError::internal("forced delegation start failure"));
        }
        self.ensure_delegation_can_start_child_turn(delegation_id, child_session_id)?;
        let dispatch = self.dispatch_turn(
            child_session_id,
            SendMessageRequest {
                text: runtime_prompt,
                expanded_text: None,
                attachments: Vec::new(),
            },
        )?;
        if let DispatchTurnResult::Dispatched(dispatch) = dispatch {
            deliver_turn_dispatch(self, dispatch)?;
        }
        Ok(())
    }

    fn ensure_delegation_can_start_child_turn(
        &self,
        delegation_id: &str,
        child_session_id: &str,
    ) -> Result<(), ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let delegation = inner
            .find_delegation_index(delegation_id)
            .and_then(|index| inner.delegations.get(index))
            .ok_or_else(|| ApiError::not_found("delegation not found"))?;
        if delegation.child_session_id != child_session_id {
            return Err(ApiError::conflict("delegation child session mismatch"));
        }
        if delegation_is_terminal(delegation.status) {
            return Err(ApiError::conflict(DELEGATION_NO_LONGER_STARTABLE_MESSAGE));
        }
        Ok(())
    }
}

fn build_read_only_delegation_prompt(record: &DelegationRecord) -> String {
    format!(
        "You are a delegated child session for TermAl delegation `{}`.\n\
\n\
Mode: {:?}\n\
Parent session: `{}`\n\
Child session: `{}`\n\
Working directory: `{}`\n\
Write policy: read-only. Do not edit, stage, commit, push, or otherwise mutate files.\n\
Other sessions may be active in the same workspace. Do not revert unrelated changes.\n\
\n\
Task:\n{}\n\
\n\
Final answer requirements:\n\
- Start with `## Result`.\n\
- Include `Status: completed` or `Status: failed`.\n\
- Include a concise `Summary:` section.\n\
- Include `Findings:`, `Commands Run:`, and `Files Inspected:` sections, using `- None` when empty.",
        record.id,
        record.mode,
        record.parent_session_id,
        record.child_session_id,
        record.cwd,
        record.prompt,
    )
}

fn add_parent_delegation_card_locked(
    inner: &mut StateInner,
    delegation: &DelegationRecord,
) -> Option<ParentDelegationCardDelta> {
    let Some(parent_index) = inner.find_session_index(&delegation.parent_session_id) else {
        return None;
    };
    let message_id = inner.next_message_id();
    let agent = ParallelAgentProgress {
        detail: Some(format!(
            "{} delegation in `{}` using {}{}",
            delegation_write_policy_label(&delegation.write_policy),
            delegation.cwd,
            delegation.agent.name(),
            delegation
                .model
                .as_deref()
                .map(|model| format!(" / {model}"))
                .unwrap_or_default()
        )),
        id: delegation.id.clone(),
        status: ParallelAgentStatus::Running,
        title: delegation.title.clone(),
    };
    let record = inner
        .session_mut_by_index(parent_index)
        .expect("parent session index should be valid");
    let message = Message::ParallelAgents {
        id: message_id.clone(),
        timestamp: stamp_now(),
        author: Author::Assistant,
        agents: vec![agent],
    };
    if let Some(preview) = message.preview_text() {
        record.session.preview = preview;
    }
    let message_index = push_message_on_record(record, message.clone());
    Some(ParentDelegationCardDelta::Created {
        session_id: record.session.id.clone(),
        message_id,
        message_index,
        message_count: session_message_count(record),
        message,
        preview: record.session.preview.clone(),
        status: record.session.status,
        session_mutation_stamp: record.mutation_stamp,
    })
}

fn update_parent_delegation_card_locked(
    inner: &mut StateInner,
    delegation: &DelegationRecord,
    status: ParallelAgentStatus,
    detail: String,
) -> Option<ParentDelegationCardDelta> {
    let Some(parent_index) = inner.find_session_index(&delegation.parent_session_id) else {
        return None;
    };
    let record = inner
        .session_mut_by_index(parent_index)
        .expect("parent session index should be valid");
    for (message_index, message) in record.session.messages.iter_mut().enumerate().rev() {
        let Message::ParallelAgents { id, agents, .. } = message else {
            continue;
        };
        let Some(agent) = agents.iter_mut().find(|agent| agent.id == delegation.id) else {
            continue;
        };
        agent.status = status;
        agent.detail = Some(detail);
        let agents = agents.clone();
        let preview = parallel_agents_preview_text(&agents);
        record.session.preview = preview.clone();
        return Some(ParentDelegationCardDelta::Updated {
            session_id: record.session.id.clone(),
            message_id: id.clone(),
            message_index,
            message_count: session_message_count(record),
            agents,
            preview,
            session_mutation_stamp: record.mutation_stamp,
        });
    }
    None
}

fn refresh_delegation_from_child_locked(
    inner: &mut StateInner,
    delegation_index: usize,
) -> Option<DelegationLifecycleDelta> {
    let delegation = inner.delegations.get(delegation_index)?.clone();
    if delegation_is_terminal(delegation.status) {
        return None;
    }

    let child_outcome = delegation_child_outcome(inner, &delegation.child_session_id);
    match child_outcome {
        DelegationChildOutcome::Running => {
            if delegation.status == DelegationStatus::Running {
                None
            } else {
                let updated_at = stamp_now();
                let record = inner.delegations.get_mut(delegation_index)?;
                record.status = DelegationStatus::Running;
                record.started_at.get_or_insert_with(|| updated_at.clone());
                inner.sync_running_read_only_delegation_index(delegation_index);
                inner.mark_delegations_mutated();
                let parent_card_delta = update_parent_delegation_card_locked(
                    inner,
                    &delegation,
                    ParallelAgentStatus::Running,
                    "Delegated session is running.".to_owned(),
                );
                Some(DelegationLifecycleDelta::Updated {
                    delegation_id: delegation.id,
                    status: DelegationStatus::Running,
                    updated_at,
                    parent_card_delta,
                })
            }
        }
        DelegationChildOutcome::Completed {
            summary,
            changed_files,
            commands_run,
        } => {
            let completed_at = stamp_now();
            let result = DelegationResult {
                delegation_id: delegation.id.clone(),
                child_session_id: delegation.child_session_id.clone(),
                status: DelegationStatus::Completed,
                summary,
                findings: Vec::new(),
                changed_files,
                commands_run,
                notes: Vec::new(),
            };
            let record = inner.delegations.get_mut(delegation_index)?;
            record.status = DelegationStatus::Completed;
            record.completed_at = Some(completed_at.clone());
            record.result = Some(result.clone());
            inner.sync_running_read_only_delegation_index(delegation_index);
            inner.mark_delegations_mutated();
            clear_delegation_child_queue_locked(inner, &delegation.child_session_id);
            let parent_card_delta = update_parent_delegation_card_locked(
                inner,
                &delegation,
                ParallelAgentStatus::Completed,
                compact_delegation_public_summary(&result.summary),
            );
            Some(DelegationLifecycleDelta::Completed {
                delegation_id: delegation.id,
                result,
                completed_at,
                parent_card_delta,
            })
        }
        DelegationChildOutcome::Failed { summary } => {
            mark_delegation_failed_locked(inner, delegation_index, &summary)
        }
        DelegationChildOutcome::IdleWithoutResult => mark_delegation_failed_locked(
            inner,
            delegation_index,
            "child finished without a result packet",
        ),
        DelegationChildOutcome::Missing => mark_delegation_failed_locked(
            inner,
            delegation_index,
            "delegation child session no longer exists",
        ),
    }
}

fn reconcile_delegations_for_removed_session_locked(
    inner: &mut StateInner,
    removed_session_id: &str,
) -> RemovedSessionDelegationReconciliation {
    let impacted = inner
        .delegations
        .iter()
        .enumerate()
        .filter_map(|(index, delegation)| {
            if delegation_is_terminal(delegation.status) {
                None
            } else if delegation.child_session_id == removed_session_id {
                Some((index, "delegation child session was removed", None, false))
            } else if delegation.parent_session_id == removed_session_id {
                Some((
                    index,
                    "delegation parent session was removed",
                    Some(delegation.child_session_id.clone()),
                    true,
                ))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    let mut deltas = Vec::new();
    let mut child_transcript_deltas = Vec::new();
    let mut runtimes_to_kill = Vec::new();
    for (index, detail, child_session_id_to_unlink, removed_parent_session) in impacted {
        if let Some(delta) = mark_delegation_failed_locked(inner, index, detail) {
            deltas.push(if removed_parent_session {
                strip_parent_card_delta(delta)
            } else {
                delta
            });
        }
        if let Some(child_session_id) = child_session_id_to_unlink {
            let detached_child = detach_delegation_child_runtime_locked(
                inner,
                &child_session_id,
                Some("Delegation halted: parent session was removed."),
            );
            if let Some(runtime) = detached_child.runtime {
                runtimes_to_kill.push(runtime);
            }
            child_transcript_deltas.extend(detached_child.transcript_deltas);
            if let Some(child_index) = inner.find_session_index(&child_session_id) {
                let child = inner
                    .session_mut_by_index(child_index)
                    .expect("child session index should be valid");
                child.session.parent_delegation_id = None;
            }
        }
    }
    RemovedSessionDelegationReconciliation {
        lifecycle_deltas: deltas,
        child_transcript_deltas,
        runtimes_to_kill,
    }
}

fn mark_delegation_failed_locked(
    inner: &mut StateInner,
    delegation_index: usize,
    detail: &str,
) -> Option<DelegationLifecycleDelta> {
    let delegation = inner.delegations.get(delegation_index)?.clone();
    if delegation_is_terminal(delegation.status) {
        return None;
    }
    let completed_at = stamp_now();
    let summary = detail.trim();
    let summary = if summary.is_empty() {
        "Delegation failed."
    } else {
        summary
    };
    let public_summary = compact_delegation_public_summary(summary);
    let result = DelegationResult {
        delegation_id: delegation.id.clone(),
        child_session_id: delegation.child_session_id.clone(),
        status: DelegationStatus::Failed,
        summary: summary.to_owned(),
        findings: Vec::new(),
        changed_files: Vec::new(),
        commands_run: Vec::new(),
        notes: Vec::new(),
    };
    let record = inner.delegations.get_mut(delegation_index)?;
    record.status = DelegationStatus::Failed;
    record.completed_at = Some(completed_at.clone());
    record.result = Some(result.clone());
    inner.sync_running_read_only_delegation_index(delegation_index);
    inner.mark_delegations_mutated();
    clear_delegation_child_queue_locked(inner, &delegation.child_session_id);
    if let Some(child_index) = inner.find_session_index(&delegation.child_session_id) {
        let child = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");
        child.session.status = SessionStatus::Error;
        child.session.preview = public_summary.clone();
    }
    let parent_card_delta = update_parent_delegation_card_locked(
        inner,
        &delegation,
        ParallelAgentStatus::Error,
        public_summary,
    );
    Some(DelegationLifecycleDelta::Failed {
        delegation_id: delegation.id,
        result,
        failed_at: completed_at,
        parent_card_delta,
    })
}

fn mark_delegation_canceled_locked(
    inner: &mut StateInner,
    delegation_index: usize,
    reason: Option<String>,
) -> Option<DelegationLifecycleDelta> {
    let delegation = inner.delegations.get(delegation_index)?.clone();
    if delegation_is_terminal(delegation.status) {
        return None;
    }
    let canceled_at = stamp_now();
    let raw_summary = reason.as_deref().unwrap_or("Delegation canceled.");
    let public_summary = compact_delegation_public_summary(raw_summary);
    let result = DelegationResult {
        delegation_id: delegation.id.clone(),
        child_session_id: delegation.child_session_id.clone(),
        status: DelegationStatus::Canceled,
        summary: raw_summary.to_owned(),
        findings: Vec::new(),
        changed_files: Vec::new(),
        commands_run: Vec::new(),
        notes: Vec::new(),
    };
    let record = inner.delegations.get_mut(delegation_index)?;
    record.status = DelegationStatus::Canceled;
    record.completed_at = Some(canceled_at.clone());
    record.result = Some(result);
    inner.sync_running_read_only_delegation_index(delegation_index);
    inner.mark_delegations_mutated();
    clear_delegation_child_queue_locked(inner, &delegation.child_session_id);
    if let Some(child_index) = inner.find_session_index(&delegation.child_session_id) {
        let child = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");
        child.session.status = SessionStatus::Idle;
        child.session.preview = public_summary.clone();
    }
    let parent_card_delta = update_parent_delegation_card_locked(
        inner,
        &delegation,
        ParallelAgentStatus::Error,
        public_summary,
    );
    Some(DelegationLifecycleDelta::Canceled {
        delegation_id: delegation.id,
        canceled_at,
        reason,
        parent_card_delta,
    })
}

fn clear_delegation_child_queue_locked(inner: &mut StateInner, child_session_id: &str) {
    let Some(child_index) = inner.find_session_index(child_session_id) else {
        return;
    };
    let child = inner
        .session_mut_by_index(child_index)
        .expect("child session index should be valid");
    child.queued_prompts.clear();
    sync_pending_prompts(child);
}

fn detach_delegation_child_runtime_locked(
    inner: &mut StateInner,
    child_session_id: &str,
    halted_marker_text: Option<&str>,
) -> DetachedDelegationChildRuntime {
    let halted_marker = halted_marker_text.map(|text| (inner.next_message_id(), text.to_owned()));
    let Some(child_index) = inner.find_session_index(child_session_id) else {
        return DetachedDelegationChildRuntime::default();
    };
    let child = inner
        .session_mut_by_index(child_index)
        .expect("child session index should be valid");
    let runtime = match &child.runtime {
        SessionRuntime::Claude(handle) => Some(KillableRuntime::Claude(handle.clone())),
        SessionRuntime::Codex(handle) => Some(KillableRuntime::Codex(handle.clone())),
        SessionRuntime::Acp(handle) => Some(KillableRuntime::Acp(handle.clone())),
        SessionRuntime::None => None,
    };
    child.runtime = SessionRuntime::None;
    child.runtime_reset_required = false;
    child.runtime_stop_in_progress = false;
    child.deferred_stop_callbacks.clear();
    child.active_turn_start_message_count = None;
    child.active_turn_file_changes.clear();
    child.active_turn_file_change_grace_deadline = None;
    child.queued_prompts.clear();
    sync_pending_prompts(child);
    clear_all_pending_requests(child);
    let changed_message_indices = cancel_pending_interaction_messages(&mut child.session.messages);
    let created_marker = halted_marker.map(|(message_id, text)| {
        let message = Message::Text {
            attachments: Vec::new(),
            id: message_id,
            timestamp: stamp_now(),
            author: Author::Assistant,
            text,
            expanded_text: None,
        };
        if let Some(preview) = message.preview_text() {
            child.session.preview = preview;
        }
        let message_index = push_message_on_record(child, message.clone());
        (message_index, message)
    });
    let session_id = child.session.id.clone();
    let message_count = session_message_count(child);
    let preview = child.session.preview.clone();
    let status = child.session.status;
    let session_mutation_stamp = child.mutation_stamp;
    let mut transcript_deltas = changed_message_indices
        .into_iter()
        .filter_map(|message_index| {
            let message = child.session.messages.get(message_index)?.clone();
            Some(DelegationChildTranscriptDelta::MessageUpdated {
                session_id: session_id.clone(),
                message_id: message.id().to_owned(),
                message_index,
                message_count,
                message,
                preview: preview.clone(),
                status,
                session_mutation_stamp,
            })
        })
        .collect::<Vec<_>>();
    if let Some((message_index, message)) = created_marker {
        transcript_deltas.push(DelegationChildTranscriptDelta::MessageCreated {
            session_id,
            message_id: message.id().to_owned(),
            message_index,
            message_count,
            message,
            preview,
            status,
            session_mutation_stamp,
        });
    }
    DetachedDelegationChildRuntime {
        runtime,
        transcript_deltas,
    }
}

fn strip_parent_card_delta(delta: DelegationLifecycleDelta) -> DelegationLifecycleDelta {
    match delta {
        DelegationLifecycleDelta::Updated {
            delegation_id,
            status,
            updated_at,
            parent_card_delta: _,
        } => DelegationLifecycleDelta::Updated {
            delegation_id,
            status,
            updated_at,
            parent_card_delta: None,
        },
        DelegationLifecycleDelta::Completed {
            delegation_id,
            result,
            completed_at,
            parent_card_delta: _,
        } => DelegationLifecycleDelta::Completed {
            delegation_id,
            result,
            completed_at,
            parent_card_delta: None,
        },
        DelegationLifecycleDelta::Failed {
            delegation_id,
            result,
            failed_at,
            parent_card_delta: _,
        } => DelegationLifecycleDelta::Failed {
            delegation_id,
            result,
            failed_at,
            parent_card_delta: None,
        },
        DelegationLifecycleDelta::Canceled {
            delegation_id,
            canceled_at,
            reason,
            parent_card_delta: _,
        } => DelegationLifecycleDelta::Canceled {
            delegation_id,
            canceled_at,
            reason,
            parent_card_delta: None,
        },
    }
}

fn delegation_is_terminal(status: DelegationStatus) -> bool {
    matches!(
        status,
        DelegationStatus::Completed | DelegationStatus::Failed | DelegationStatus::Canceled
    )
}

fn validate_delegation_cwd_input(cwd: &str) -> Result<(), ApiError> {
    #[cfg(windows)]
    {
        let trimmed = cwd.trim();
        let path = FsPath::new(trimmed);
        if let Some(std::path::Component::Prefix(prefix)) = path.components().next() {
            match prefix.kind() {
                std::path::Prefix::Disk(_) if !path.is_absolute() => {
                    return Err(ApiError::bad_request(
                        "delegation cwd cannot be a drive-relative Windows path",
                    ));
                }
                std::path::Prefix::VerbatimDisk(_) if !path.is_absolute() => {
                    return Err(ApiError::bad_request(
                        "delegation cwd cannot be a drive-relative Windows path",
                    ));
                }
                std::path::Prefix::UNC(_, _) | std::path::Prefix::VerbatimUNC(_, _) => {
                    return Err(ApiError::bad_request("delegation cwd cannot be a UNC path"));
                }
                std::path::Prefix::DeviceNS(_) => {
                    return Err(ApiError::bad_request(
                        "delegation cwd cannot be a Windows device namespace path",
                    ));
                }
                std::path::Prefix::Verbatim(prefix) => {
                    let prefix = prefix.to_string_lossy();
                    if prefix.eq_ignore_ascii_case("GLOBALROOT")
                        || prefix.eq_ignore_ascii_case("Mup")
                    {
                        return Err(ApiError::bad_request(
                            "delegation cwd cannot be a Windows device namespace path",
                        ));
                    }
                }
                _ => {}
            }
        }
    }
    let _ = cwd;
    Ok(())
}

enum DelegationChildOutcome {
    Running,
    Completed {
        summary: String,
        changed_files: Vec<String>,
        commands_run: Vec<DelegationCommandResult>,
    },
    Failed {
        summary: String,
    },
    IdleWithoutResult,
    Missing,
}

struct ParsedDelegationResult {
    status: DelegationStatus,
    summary: String,
}

fn delegation_child_outcome(inner: &StateInner, child_session_id: &str) -> DelegationChildOutcome {
    let Some(child_index) = inner.find_session_index(child_session_id) else {
        return DelegationChildOutcome::Missing;
    };
    let child = &inner.sessions[child_index];
    match child.session.status {
        SessionStatus::Active | SessionStatus::Approval => DelegationChildOutcome::Running,
        SessionStatus::Error => DelegationChildOutcome::Failed {
            summary: child.session.preview.clone(),
        },
        SessionStatus::Idle => {
            if let Some(result) = latest_assistant_delegation_result(&child.session) {
                if result.status == DelegationStatus::Failed {
                    DelegationChildOutcome::Failed {
                        summary: result.summary,
                    }
                } else {
                    DelegationChildOutcome::Completed {
                        summary: result.summary,
                        changed_files: child_changed_files(&child.session),
                        commands_run: child_commands_run(&child.session),
                    }
                }
            } else {
                DelegationChildOutcome::IdleWithoutResult
            }
        }
    }
}

struct DelegationWriteScope {
    title: String,
    project_id: Option<String>,
    workdir: String,
}

struct DelegationWriteTarget {
    project_id: Option<String>,
    workdir: Option<String>,
}

struct LocalProjectWriteScope {
    project_id: String,
    root_path: String,
}

struct DelegationWriteBlock {
    title: Option<String>,
}

fn append_local_project_write_targets(
    write_targets: &mut Vec<DelegationWriteTarget>,
    local_projects: &[LocalProjectWriteScope],
) {
    let mut inferred_targets = Vec::new();
    for target in write_targets.iter() {
        if let Some(project_id) = target.project_id.as_deref() {
            if let Some(project) = local_projects
                .iter()
                .find(|project| project.project_id == project_id)
            {
                inferred_targets.push(DelegationWriteTarget {
                    project_id: Some(project.project_id.clone()),
                    workdir: Some(project.root_path.clone()),
                });
            }
        }

        let Some(workdir) = target.workdir.as_deref() else {
            continue;
        };
        for project in local_projects
            .iter()
            .filter(|project| path_contains(&project.root_path, FsPath::new(workdir)))
        {
            inferred_targets.push(DelegationWriteTarget {
                project_id: Some(project.project_id.clone()),
                workdir: Some(project.root_path.clone()),
            });
        }
    }
    write_targets.extend(inferred_targets);
}

fn read_only_session_delegation_block_locked(
    inner: &StateInner,
    session_id: Option<&str>,
) -> Option<DelegationWriteBlock> {
    let session_id = normalize_optional_identifier(session_id)?;
    let session_index = inner.find_visible_session_index(session_id)?;
    let record = inner.sessions.get(session_index)?;
    let delegation_id = record.session.parent_delegation_id.as_deref()?;
    match inner
        .delegations
        .iter()
        .find(|delegation| delegation.id == delegation_id)
    {
        Some(delegation)
            if !delegation_is_terminal(delegation.status)
                && delegation.write_policy == DelegationWritePolicy::ReadOnly =>
        {
            Some(DelegationWriteBlock {
                title: Some(delegation.title.clone()),
            })
        }
        Some(_) => None,
        None => {
            // Production paths should not leave a child session pointing at a
            // missing delegation. If that anomaly appears, keep writes blocked
            // and use fallback wording so the stale link is visible.
            Some(DelegationWriteBlock { title: None })
        }
    }
}

fn delegation_write_scope_matches(
    scope: &DelegationWriteScope,
    project_id: Option<&str>,
    workdir: Option<&str>,
) -> bool {
    if project_id.is_some_and(|project_id| scope.project_id.as_deref() == Some(project_id)) {
        return true;
    }
    let scope_workdir = scope.workdir.trim();
    if scope_workdir.is_empty() {
        return false;
    }
    workdir.is_some_and(|workdir| {
        // Match both directions: callers writing inside the delegated tree and
        // callers targeting a broader ancestor of the delegated tree are blocked.
        path_contains(scope_workdir, FsPath::new(workdir))
            || path_contains(workdir, FsPath::new(scope_workdir))
    })
}

fn delegated_child_dispatch_is_blocked_locked(inner: &StateInner, session_index: usize) -> bool {
    let Some(record) = inner.sessions.get(session_index) else {
        return false;
    };
    let Some(delegation_id) = record.session.parent_delegation_id.as_deref() else {
        return false;
    };
    inner
        .delegations
        .iter()
        .find(|delegation| {
            delegation.id == delegation_id && delegation.child_session_id == record.session.id
        })
        .is_some_and(|delegation| delegation_is_terminal(delegation.status))
}

fn latest_assistant_delegation_result(session: &Session) -> Option<ParsedDelegationResult> {
    // Walk back through assistant messages and parse each candidate. The agent
    // is asked to put `## Result` last, but realistic runtimes can append a
    // trailing chunk after the result packet (status text, diff summary, etc.).
    // Stopping at the latest assistant message would misclassify those runs as
    // `IdleWithoutResult`; instead, keep scanning until we find a parseable
    // packet.
    session.messages.iter().rev().find_map(|message| {
        let text = match message {
            Message::Text {
                author: Author::Assistant,
                text,
                ..
            } => non_empty_trimmed(text),
            Message::Markdown {
                author: Author::Assistant,
                markdown,
                title,
                ..
            } => non_empty_trimmed(markdown).or_else(|| non_empty_trimmed(title)),
            Message::SubagentResult {
                author: Author::Assistant,
                summary,
                title,
                ..
            } => non_empty_trimmed(summary).or_else(|| non_empty_trimmed(title)),
            Message::Diff {
                author: Author::Assistant,
                summary,
                ..
            } => non_empty_trimmed(summary),
            _ => None,
        };
        text.and_then(|t| parse_delegation_result_packet(&t))
    })
}

fn parse_delegation_result_packet(text: &str) -> Option<ParsedDelegationResult> {
    let search_window = delegation_result_search_window(text);
    let mut lines = search_window
        .lines()
        .skip_while(|line| !line.trim().eq_ignore_ascii_case("## Result"));
    lines.next()?;

    let mut status = None;
    let mut summary_lines = Vec::new();
    let mut in_summary = false;
    for line in lines {
        let cleaned = line.trim();
        if !in_summary {
            if let Some((label, value)) = cleaned.split_once(':') {
                if label.trim().eq_ignore_ascii_case("status") {
                    status = match value.trim().to_ascii_lowercase().as_str() {
                        "completed" => Some(DelegationStatus::Completed),
                        "failed" => Some(DelegationStatus::Failed),
                        _ => None,
                    };
                    continue;
                }
            }
        } else if is_delegation_result_section_heading(cleaned) {
            in_summary = false;
        }

        if cleaned.eq_ignore_ascii_case("Summary:") {
            in_summary = true;
            continue;
        }

        if in_summary {
            summary_lines.push(line);
        }
    }

    let status = status?;
    let summary = summary_lines.join("\n").trim().to_owned();
    let summary = if summary.is_empty() {
        match status {
            DelegationStatus::Completed => "Delegation completed.".to_owned(),
            DelegationStatus::Failed => "Delegation failed.".to_owned(),
            _ => String::new(),
        }
    } else {
        summary
    };

    Some(ParsedDelegationResult { status, summary })
}

fn is_delegation_result_section_heading(cleaned: &str) -> bool {
    let Some(label) = cleaned.strip_suffix(':') else {
        return false;
    };
    matches!(
        label.trim().to_ascii_lowercase().as_str(),
        "findings" | "commands run" | "files inspected" | "notes"
    )
}

fn delegation_result_search_window(text: &str) -> &str {
    if text.len() <= DELEGATION_RESULT_PACKET_SEARCH_BYTES {
        return text;
    }
    let mut start = text.len() - DELEGATION_RESULT_PACKET_SEARCH_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

fn compact_delegation_public_summary(summary: &str) -> String {
    let compact = summary
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let compact = if compact.is_empty() {
        "Delegation updated.".to_owned()
    } else {
        compact
    };
    truncate_chars(&compact, MAX_DELEGATION_PUBLIC_SUMMARY_CHARS)
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut end = 0;
    for (count, (index, ch)) in value.char_indices().enumerate() {
        if count == max_chars {
            return format!("{}...", &value[..end]);
        }
        end = index + ch.len_utf8();
    }
    value.to_owned()
}

fn child_changed_files(session: &Session) -> Vec<String> {
    let mut files = BTreeSet::new();
    for message in &session.messages {
        if let Message::FileChanges { files: entries, .. } = message {
            for entry in entries {
                files.insert(entry.path.clone());
            }
        }
    }
    files.into_iter().collect()
}

fn child_commands_run(session: &Session) -> Vec<DelegationCommandResult> {
    session
        .messages
        .iter()
        .filter_map(|message| {
            if let Message::Command {
                command, status, ..
            } = message
            {
                Some(DelegationCommandResult {
                    command: command.clone(),
                    status: status.label().to_owned(),
                })
            } else {
                None
            }
        })
        .collect()
}

fn non_empty_trimmed(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn active_delegation_count_for_parent(inner: &StateInner, parent_session_id: &str) -> usize {
    inner
        .delegations
        .iter()
        .filter(|delegation| {
            delegation.parent_session_id == parent_session_id
                && !delegation_is_terminal(delegation.status)
        })
        .count()
}

fn delegation_depth_for_parent(inner: &StateInner, parent_session_id: &str) -> usize {
    let mut depth = 0;
    let mut current_session_id = parent_session_id;
    let mut seen = HashSet::new();
    while let Some(parent_delegation_id) = inner
        .sessions
        .iter()
        .find(|record| record.session.id == current_session_id)
        .and_then(|record| record.session.parent_delegation_id.as_deref())
    {
        if !seen.insert(parent_delegation_id.to_owned()) {
            break;
        }
        let Some(parent_delegation) = inner
            .delegations
            .iter()
            .find(|delegation| delegation.id == parent_delegation_id)
        else {
            break;
        };
        depth += 1;
        current_session_id = &parent_delegation.parent_session_id;
    }
    depth
}

fn delegation_result_summary(result: &DelegationResult) -> DelegationResultSummary {
    DelegationResultSummary {
        delegation_id: result.delegation_id.clone(),
        child_session_id: result.child_session_id.clone(),
        status: result.status,
        summary: compact_delegation_public_summary(&result.summary),
    }
}

fn delegation_summary_from_record(record: &DelegationRecord) -> DelegationSummary {
    DelegationSummary {
        id: record.id.clone(),
        parent_session_id: record.parent_session_id.clone(),
        child_session_id: record.child_session_id.clone(),
        mode: record.mode,
        status: record.status,
        title: record.title.clone(),
        agent: record.agent,
        model: record.model.clone(),
        write_policy: record.write_policy.clone(),
        created_at: record.created_at.clone(),
        started_at: record.started_at.clone(),
        completed_at: record.completed_at.clone(),
        result: record.result.as_ref().map(delegation_result_summary),
    }
}

fn delegation_write_policy_label(write_policy: &DelegationWritePolicy) -> &'static str {
    match write_policy {
        DelegationWritePolicy::ReadOnly => "Read-only",
        DelegationWritePolicy::SharedWorktree { .. } => "Shared-worktree",
        DelegationWritePolicy::IsolatedWorktree { .. } => "Isolated-worktree",
    }
}
