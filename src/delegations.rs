// Delegation records: durable parent-child metadata plus child-session launch
// policy. The child remains a normal session; this layer owns parent cards,
// lifecycle/result metadata, compact `/api/state` summaries, result parsing,
// read-only guard integration, queue cleanup, isolated worktree preparation,
// and delegation SSE deltas. It does not remote-forward delegation creation or
// implement shared-worktree/worker policies yet.

// Keep prompts bounded because they are embedded into child-agent startup input.
// Keep in sync with `MAX_DELEGATION_PROMPT_BYTES` in
// `ui/src/delegation-commands.ts`; if this changes, update the parity-pin test
// in `ui/src/delegation-commands.test.ts`.
const MAX_DELEGATION_PROMPT_BYTES: usize = 64 * 1024;
// Child session names are exposed through redacted delegation summaries; cap
// caller-supplied titles as metadata, not prompt-sized payloads. Keep in sync
// with `MAX_DELEGATION_TITLE_CHARS` in `ui/src/delegation-commands.ts`.
const MAX_DELEGATION_TITLE_CHARS: usize = 200;
// Explicit model names are metadata echoed in summaries and child cards, not
// prompt payloads. Keep in sync with `MAX_DELEGATION_MODEL_CHARS` in
// `ui/src/delegation-commands.ts`.
const MAX_DELEGATION_MODEL_CHARS: usize = 200;
// Public summaries ride in `/api/state`; full summaries stay behind result reads.
const MAX_DELEGATION_PUBLIC_SUMMARY_CHARS: usize = 1000;
// Result packets are expected near the end of long assistant output.
const DELEGATION_RESULT_PACKET_SEARCH_BYTES: usize = 32 * 1024;
// Phase 1 starts children immediately but still enforces simple fan-out limits.
const MAX_RUNNING_DELEGATIONS_PER_PARENT: usize = 4;
// Keep nesting shallow until delegation ownership/scheduling is explicit.
const MAX_DELEGATION_DEPTH: usize = 3;
// Parent resume prompts should only wait on a small fan-in set.
const MAX_DELEGATION_WAIT_IDS: usize = 10;
// The synthesized parent fan-in prompt is persisted and sent to a model.
const MAX_DELEGATION_WAIT_RESUME_PROMPT_BYTES: usize = 64 * 1024;
const DELEGATION_WAIT_RESUME_TRUNCATED_MARKER: &str =
    "\n\n[TermAl truncated this delegation fan-in prompt. Open the child sessions for full results.]";
// Shared conflict text for create/cancel races before child dispatch starts.
const DELEGATION_NO_LONGER_STARTABLE_MESSAGE: &str = "delegation is no longer running";
#[cfg(test)]
const TEST_FORCE_DELEGATION_START_FAILURE_PROMPT: &str =
    "TERMAL_TEST_FORCE_DELEGATION_START_FAILURE";
#[cfg(test)]
const TEST_CANCEL_DELEGATION_BEFORE_START_PROMPT: &str =
    "TERMAL_TEST_CANCEL_DELEGATION_BEFORE_START";

struct PreparedIsolatedWorktree {
    child_cwd: String,
    worktree_root: String,
}

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

#[derive(Clone, Debug, Default)]
struct DelegationWaitRefresh {
    dispatch_parents: Vec<String>,
    consumed_waits: Vec<ConsumedDelegationWait>,
    queue_results_by_wait_id: BTreeMap<String, DelegationWaitQueueResult>,
}

impl DelegationWaitRefresh {
    fn did_mutate(&self) -> bool {
        !self.consumed_waits.is_empty()
    }

    fn queue_result_for_wait(&self, wait_id: &str) -> DelegationWaitQueueResult {
        self.queue_results_by_wait_id
            .get(wait_id)
            .copied()
            .unwrap_or_default()
    }

    fn consume_wait(
        &mut self,
        wait: DelegationWaitRecord,
        reason: DelegationWaitConsumedReason,
    ) {
        self.consumed_waits.push(ConsumedDelegationWait { wait, reason });
    }

}

#[derive(Clone, Copy, Debug, Default)]
struct DelegationWaitQueueResult {
    prompt_queued: bool,
    dispatch_requested: bool,
}

#[derive(Clone, Debug)]
struct ConsumedDelegationWait {
    wait: DelegationWaitRecord,
    reason: DelegationWaitConsumedReason,
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
        let title = request
            .title
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| "Delegated review".to_owned());
        if title.chars().count() > MAX_DELEGATION_TITLE_CHARS {
            return Err(ApiError::bad_request(format!(
                "delegation title must be at most {} characters",
                MAX_DELEGATION_TITLE_CHARS
            )));
        }
        let model = request
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        if model
            .as_deref()
            .is_some_and(|value| value.chars().count() > MAX_DELEGATION_MODEL_CHARS)
        {
            return Err(ApiError::bad_request(format!(
                "delegation model must be at most {} characters",
                MAX_DELEGATION_MODEL_CHARS
            )));
        }
        let mode = request.mode.unwrap_or(DelegationMode::Reviewer);
        if mode == DelegationMode::Worker {
            return Err(ApiError::from_status(
                StatusCode::NOT_IMPLEMENTED,
                "worker delegations are not implemented in Phase 1",
            ));
        }
        let requested_write_policy = request
            .write_policy
            .unwrap_or(DelegationWritePolicy::ReadOnly);
        if matches!(
            requested_write_policy,
            DelegationWritePolicy::SharedWorktree { .. }
        ) {
            return Err(ApiError::from_status(
                StatusCode::NOT_IMPLEMENTED,
                "sharedWorktree delegation write policy is not implemented yet",
            ));
        }
        let delegation_id = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner.next_delegation_id()
        };

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
        let source_cwd = requested_cwd.unwrap_or(parent_workdir);
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
            if !path_contains(&project.root_path, FsPath::new(&source_cwd)) {
                return Err(ApiError::bad_request(format!(
                    "delegation cwd `{source_cwd}` must stay inside project `{}`",
                    project.name
                )));
            }
        }

        let (cwd, child_project_id, write_policy) = match requested_write_policy {
            DelegationWritePolicy::ReadOnly => (
                source_cwd.clone(),
                parent_project_id.clone(),
                DelegationWritePolicy::ReadOnly,
            ),
            DelegationWritePolicy::SharedWorktree { .. } => unreachable!(
                "sharedWorktree policy should be rejected before workspace preparation"
            ),
            DelegationWritePolicy::IsolatedWorktree {
                owned_paths,
                worktree_path,
            } => {
                let prepared = prepare_isolated_delegation_worktree(
                    &source_cwd,
                    worktree_path.as_deref(),
                    &self.default_workdir,
                    &delegation_id,
                )?;
                (
                    prepared.child_cwd,
                    None,
                    DelegationWritePolicy::IsolatedWorktree {
                        owned_paths,
                        worktree_path: Some(prepared.worktree_root),
                    },
                )
            }
        };

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
            #[cfg(test)]
            if let Some(detail) = self.test_agent_setup_failure(agent) {
                return Err(ApiError::bad_request(detail));
            }
            validate_agent_session_setup(agent, &cwd).map_err(ApiError::bad_request)?;
        }
        self.invalidate_agent_readiness_cache();
        let _ = self.agent_readiness_snapshot();

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
        let now = stamp_now();
        let child_record = inner.create_session(
            agent,
            Some(title.clone()),
            cwd.clone(),
            child_project_id,
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
            configure_delegation_child_prompt_settings(child_record, &write_policy);
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
        inner.mark_delegation_mutated(delegation_index);
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

        let runtime_prompt = build_delegation_prompt(&record);
        if let Err(err) =
            self.start_delegation_child_turn(&record.id, &record.child_session_id, runtime_prompt)
        {
            if err.status == StatusCode::CONFLICT
                && err.message == DELEGATION_NO_LONGER_STARTABLE_MESSAGE
            {
                return self.delegation_response_from_state(&record.id);
            }
            self.mark_delegation_failed_after_start_error(
                &record.id,
                &record.child_session_id,
                "child session failed to start",
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

    #[cfg(test)]
    fn test_agent_setup_failure(&self, agent: Agent) -> Option<String> {
        self.test_agent_setup_failures
            .lock()
            .expect("test agent setup failures mutex poisoned")
            .iter()
            .find(|(candidate, _)| *candidate == agent)
            .map(|(_, detail)| detail.clone())
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

    fn create_delegation_wait(
        &self,
        parent_session_id: &str,
        request: CreateDelegationWaitRequest,
    ) -> Result<DelegationWaitResponse, ApiError> {
        let parent_session_id = normalize_optional_identifier(Some(parent_session_id))
            .ok_or_else(|| ApiError::bad_request("parent session id is required"))?
            .to_owned();
        let delegation_ids = normalize_delegation_wait_ids(request.delegation_ids)?;
        let title = request
            .title
            .as_deref()
            .and_then(non_empty_trimmed);
        if title
            .as_ref()
            .is_some_and(|value| value.chars().count() > MAX_DELEGATION_TITLE_CHARS)
        {
            return Err(ApiError::bad_request(format!(
                "delegation wait title must be at most {MAX_DELEGATION_TITLE_CHARS} characters"
            )));
        }
        let wait_id = format!("delegation-wait-{}", Uuid::new_v4());
        let wait = DelegationWaitRecord {
            id: wait_id.clone(),
            parent_session_id: parent_session_id.clone(),
            delegation_ids,
            mode: request.mode,
            created_at: stamp_now(),
            title,
        };

        let created_revision = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            validate_delegation_wait_parent_locked(&inner, &parent_session_id)?;
            validate_delegation_wait_targets_locked(&inner, &wait)?;
            inner.delegation_waits.push(wait.clone());
            self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist delegation wait: {err:#}"))
            })?
        };

        self.publish_delegation_wait_created(created_revision, wait.clone());

        let (revision, wait_queue_result, refresh) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let refresh = refresh_delegation_waits_locked(&mut inner);
            if refresh.did_mutate() {
                let revision = self.commit_locked(&mut inner).map_err(|err| {
                    ApiError::internal(format!(
                        "failed to persist delegation wait refresh: {err:#}"
                    ))
                })?;
                (revision, refresh.queue_result_for_wait(&wait_id), refresh)
            } else {
                (
                    created_revision,
                    DelegationWaitQueueResult::default(),
                    refresh,
                )
            }
        };
        let resume_prompt_queued = wait_queue_result.prompt_queued;
        let resume_dispatch_requested = wait_queue_result.dispatch_requested;
        if refresh.did_mutate() {
            self.publish_delegation_wait_consumed_deltas(revision, &refresh.consumed_waits);
        }
        self.dispatch_delegation_wait_resumes(refresh.dispatch_parents.clone());

        Ok(DelegationWaitResponse {
            revision,
            wait,
            resume_prompt_queued,
            resume_dispatch_requested,
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
            let wait_refresh = refresh_delegation_waits_locked(&mut inner);
            let revision = if lifecycle_delta.is_some() || wait_refresh.did_mutate() {
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
                if wait_refresh.did_mutate() {
                    self.publish_delegation_wait_consumed_deltas(
                        revision,
                        &wait_refresh.consumed_waits,
                    );
                }
                self.dispatch_delegation_wait_resumes(wait_refresh.dispatch_parents);
                return Ok(response);
            }
            let child_session_id = delegation.child_session_id.clone();
            drop(inner);
            if let Some(delta) = lifecycle_delta {
                self.publish_delegation_lifecycle_delta(revision, delta);
            }
            if wait_refresh.did_mutate() {
                self.publish_delegation_wait_consumed_deltas(revision, &wait_refresh.consumed_waits);
            }
            self.dispatch_delegation_wait_resumes(wait_refresh.dispatch_parents);
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
        let wait_refresh = refresh_delegation_waits_locked(&mut inner);
        let revision = if lifecycle_delta.is_some() || wait_refresh.did_mutate() {
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
        if wait_refresh.did_mutate() {
            self.publish_delegation_wait_consumed_deltas(revision, &wait_refresh.consumed_waits);
        }
        self.dispatch_delegation_wait_resumes(wait_refresh.dispatch_parents);
        Ok(DelegationStatusResponse {
            revision,
            delegation,
            server_instance_id: self.server_instance_id.clone(),
        })
    }

    fn refresh_delegation_for_child_session(&self, child_session_id: &str) -> Result<()> {
        let (revision, lifecycle_delta, wait_refresh) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_delegation_index_by_child_session_id(child_session_id)
            else {
                return Ok(());
            };
            let Some(lifecycle_delta) = refresh_delegation_from_child_locked(&mut inner, index)
            else {
                return Ok(());
            };
            let wait_refresh = refresh_delegation_waits_locked(&mut inner);
            let revision = self.commit_locked(&mut inner)?;
            (revision, lifecycle_delta, wait_refresh)
        };
        self.publish_delegation_lifecycle_delta(revision, lifecycle_delta);
        if wait_refresh.did_mutate() {
            self.publish_delegation_wait_consumed_deltas(revision, &wait_refresh.consumed_waits);
        }
        self.dispatch_delegation_wait_resumes(wait_refresh.dispatch_parents);
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
        let (revision, lifecycle_delta, detached_child, wait_refresh) = {
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
            let wait_refresh = refresh_delegation_waits_locked(&mut inner);
            let revision = self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist delegation failure: {err:#}"))
            })?;
            (revision, lifecycle_delta, detached_child, wait_refresh)
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
        if wait_refresh.did_mutate() {
            self.publish_delegation_wait_consumed_deltas(revision, &wait_refresh.consumed_waits);
        }
        self.dispatch_delegation_wait_resumes(wait_refresh.dispatch_parents);
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

    fn publish_delegation_wait_created(&self, revision: u64, wait: DelegationWaitRecord) {
        self.publish_delta(&DeltaEvent::DelegationWaitCreated { revision, wait });
    }

    fn publish_delegation_wait_consumed_deltas(
        &self,
        revision: u64,
        waits: &[ConsumedDelegationWait],
    ) {
        for consumed in waits {
            self.publish_delta(&DeltaEvent::DelegationWaitConsumed {
                revision,
                wait_id: consumed.wait.id.clone(),
                parent_session_id: consumed.wait.parent_session_id.clone(),
                reason: consumed.reason,
            });
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

    fn dispatch_delegation_wait_resumes(&self, parent_session_ids: Vec<String>) {
        let mut seen = BTreeSet::new();
        for parent_session_id in parent_session_ids {
            if !seen.insert(parent_session_id.clone()) {
                continue;
            }
            match self.dispatch_next_queued_turn(&parent_session_id, false) {
                Ok(Some(dispatch)) => {
                    if let Err(err) = deliver_turn_dispatch(self, dispatch) {
                        eprintln!(
                            "delegation wait warning> failed to dispatch queued resume for session `{}`: {}",
                            parent_session_id, err.message
                        );
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    eprintln!(
                        "delegation wait warning> failed to inspect queued resume for session `{parent_session_id}`: {err:#}"
                    );
                }
            }
        }
    }

    fn reconcile_delegation_waits_after_boot(&self) -> Result<()> {
        let (revision, wait_refresh) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let wait_refresh = refresh_delegation_waits_locked(&mut inner);
            if !wait_refresh.did_mutate() {
                return Ok(());
            }
            let revision = self.commit_locked(&mut inner)?;
            (revision, wait_refresh)
        };
        self.publish_delegation_wait_consumed_deltas(revision, &wait_refresh.consumed_waits);
        self.dispatch_delegation_wait_resumes(wait_refresh.dispatch_parents);
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

fn build_delegation_prompt(record: &DelegationRecord) -> String {
    let write_policy = delegation_prompt_write_policy(&record.write_policy);
    format!(
        "You are a delegated child session for TermAl delegation `{}`.\n\
\n\
Mode: {:?}\n\
Parent session: `{}`\n\
Child session: `{}`\n\
Working directory: `{}`\n\
{}\n\
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
        write_policy,
        record.prompt,
    )
}

fn configure_delegation_child_prompt_settings(
    child_record: &mut SessionRecord,
    write_policy: &DelegationWritePolicy,
) {
    if child_record.session.agent.supports_codex_prompt_settings() {
        child_record.codex_approval_policy = CodexApprovalPolicy::Never;
        child_record.codex_sandbox_mode = delegation_codex_sandbox_mode(write_policy);
        child_record.session.approval_policy = Some(CodexApprovalPolicy::Never);
        child_record.session.sandbox_mode = Some(child_record.codex_sandbox_mode);
    } else if child_record.session.agent.supports_cursor_mode() {
        child_record.session.cursor_mode = Some(CursorMode::Plan);
    } else if child_record.session.agent.supports_gemini_approval_mode() {
        child_record.session.gemini_approval_mode = Some(GeminiApprovalMode::Plan);
    }
}

fn normalize_delegation_wait_ids(ids: Vec<String>) -> Result<Vec<String>, ApiError> {
    let mut normalized = Vec::with_capacity(ids.len());
    let mut seen = BTreeSet::new();
    for id in ids {
        let Some(id) = non_empty_trimmed(&id) else {
            return Err(ApiError::bad_request(
                "delegation wait ids cannot be empty",
            ));
        };
        if seen.insert(id.clone()) {
            normalized.push(id);
        }
    }
    if normalized.is_empty() {
        return Err(ApiError::bad_request(
            "delegation wait requires at least one delegation id",
        ));
    }
    if normalized.len() > MAX_DELEGATION_WAIT_IDS {
        return Err(ApiError::bad_request(format!(
            "delegation wait accepts at most {MAX_DELEGATION_WAIT_IDS} delegation ids"
        )));
    }
    Ok(normalized)
}

fn validate_delegation_wait_targets_locked(
    inner: &StateInner,
    wait: &DelegationWaitRecord,
) -> Result<(), ApiError> {
    for delegation_id in &wait.delegation_ids {
        let index = inner
            .find_delegation_index(delegation_id)
            .ok_or_else(|| ApiError::not_found("delegation not found"))?;
        let delegation = &inner.delegations[index];
        if delegation.parent_session_id != wait.parent_session_id {
            return Err(ApiError::bad_request(format!(
                "delegation `{delegation_id}` does not belong to parent session `{}`",
                wait.parent_session_id
            )));
        }
    }
    Ok(())
}

fn validate_delegation_wait_parent_locked(
    inner: &StateInner,
    parent_session_id: &str,
) -> Result<(), ApiError> {
    match delegation_wait_parent_eligibility_locked(inner, parent_session_id) {
        DelegationWaitParentEligibility::Eligible => Ok(()),
        DelegationWaitParentEligibility::Missing => Err(ApiError::local_session_missing()),
        DelegationWaitParentEligibility::Unavailable => Err(ApiError::conflict(
            "delegation wait parent session is archived; unarchive it before scheduling a wait",
        )),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DelegationWaitParentEligibility {
    Eligible,
    Missing,
    Unavailable,
}

fn delegation_wait_parent_eligibility_locked(
    inner: &StateInner,
    parent_session_id: &str,
) -> DelegationWaitParentEligibility {
    let Some(index) = inner.find_visible_session_index(parent_session_id) else {
        return DelegationWaitParentEligibility::Missing;
    };
    let parent = &inner.sessions[index];
    if record_has_archived_codex_thread(parent) {
        return DelegationWaitParentEligibility::Unavailable;
    }
    DelegationWaitParentEligibility::Eligible
}

// Delegation wait lifecycle:
// 1. Mutate `inner.delegation_waits` only while the state lock is held.
// 2. Treat consumed waits as state mutations even when the parent resume cannot
//    be queued, so callers commit deletions and do not resurrect waits on boot.
// 3. Drop the lock before dispatching queued parent turns. Dispatch failures are
//    currently best-effort and logged to stderr; the consumed wait has already
//    been persisted and announced through `delegationWaitConsumed`.
fn refresh_delegation_waits_locked(inner: &mut StateInner) -> DelegationWaitRefresh {
    if inner.delegation_waits.is_empty() {
        return DelegationWaitRefresh::default();
    }

    let waits = std::mem::take(&mut inner.delegation_waits);
    let mut remaining = Vec::new();
    let mut refresh = DelegationWaitRefresh::default();
    for wait in waits {
        match delegation_wait_parent_eligibility_locked(inner, &wait.parent_session_id) {
            DelegationWaitParentEligibility::Eligible => {}
            DelegationWaitParentEligibility::Missing => {
                refresh.consume_wait(wait, DelegationWaitConsumedReason::ParentSessionRemoved);
                continue;
            }
            DelegationWaitParentEligibility::Unavailable => {
                refresh.consume_wait(wait, DelegationWaitConsumedReason::ParentSessionUnavailable);
                continue;
            }
        }
        let Some(resume_prompt) = delegation_wait_resume_prompt_locked(inner, &wait) else {
            remaining.push(wait);
            continue;
        };
        let queue_result =
            queue_delegation_wait_resume_locked(inner, &wait.parent_session_id, resume_prompt);
        if queue_result.dispatch_requested {
            refresh.dispatch_parents.push(wait.parent_session_id.clone());
        }
        refresh
            .queue_results_by_wait_id
            .insert(wait.id.clone(), queue_result);
        refresh.consume_wait(wait, DelegationWaitConsumedReason::Completed);
    }
    inner.delegation_waits = remaining;
    refresh
}

fn delegation_wait_resume_prompt_locked(
    inner: &StateInner,
    wait: &DelegationWaitRecord,
) -> Option<String> {
    let records = wait
        .delegation_ids
        .iter()
        .filter_map(|id| inner.delegations.iter().find(|delegation| delegation.id == *id))
        .collect::<Vec<_>>();
    if records.len() != wait.delegation_ids.len() {
        return Some(limit_delegation_wait_resume_prompt(format!(
            "Delegation wait `{}` ended because one or more delegation records disappeared.\n\nRequested delegations:\n{}",
            wait.id,
            wait.delegation_ids
                .iter()
                .map(|id| format!("- `{id}`"))
                .collect::<Vec<_>>()
                .join("\n")
        )));
    }

    let terminal_records = records
        .iter()
        .copied()
        .filter(|delegation| delegation_is_terminal(delegation.status))
        .collect::<Vec<_>>();
    let satisfied = match wait.mode {
        DelegationWaitMode::Any => !terminal_records.is_empty(),
        DelegationWaitMode::All => terminal_records.len() == records.len(),
    };
    if !satisfied {
        return None;
    }

    Some(limit_delegation_wait_resume_prompt(
        build_delegation_wait_resume_prompt(wait, &records, &terminal_records),
    ))
}

fn queue_delegation_wait_resume_locked(
    inner: &mut StateInner,
    parent_session_id: &str,
    prompt: String,
) -> DelegationWaitQueueResult {
    let Some(parent_index) = inner.find_visible_session_index(parent_session_id) else {
        return DelegationWaitQueueResult::default();
    };
    let should_dispatch_now = {
        let record = &inner.sessions[parent_index];
        !matches!(
            record.session.status,
            SessionStatus::Active | SessionStatus::Approval
        ) && !record.orchestrator_auto_dispatch_blocked
            && record.queued_prompts.is_empty()
            && !record_has_archived_codex_thread(record)
    };
    let message_id = inner.next_message_id();
    let record = inner
        .session_mut_by_index(parent_index)
        .expect("parent session index should be valid");
    queue_orchestrator_prompt_on_record(
        record,
        PendingPrompt {
            attachments: Vec::new(),
            id: message_id,
            timestamp: stamp_now(),
            text: prompt,
            expanded_text: None,
        },
        Vec::new(),
    );
    DelegationWaitQueueResult {
        prompt_queued: true,
        dispatch_requested: should_dispatch_now,
    }
}

fn build_delegation_wait_resume_prompt(
    wait: &DelegationWaitRecord,
    records: &[&DelegationRecord],
    terminal_records: &[&DelegationRecord],
) -> String {
    let title = wait.title.as_deref().unwrap_or("Delegation wait completed");
    let mode = match wait.mode {
        DelegationWaitMode::Any => "any",
        DelegationWaitMode::All => "all",
    };
    let overview = records
        .iter()
        .map(|delegation| {
            format!(
                "- `{}`: {} - {}",
                delegation.id,
                delegation_status_label(delegation.status),
                delegation.title
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let results = terminal_records
        .iter()
        .map(|delegation| delegation_wait_result_section(delegation))
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");

    format!(
        "{title}\n\nWait id: `{}`\nMode: `{mode}`\nParent session: `{}`\n\nDelegations:\n{}\n\nResults:\n{}",
        wait.id, wait.parent_session_id, overview, results
    )
}

fn limit_delegation_wait_resume_prompt(prompt: String) -> String {
    truncate_to_byte_limit_with_marker(
        prompt,
        MAX_DELEGATION_WAIT_RESUME_PROMPT_BYTES,
        DELEGATION_WAIT_RESUME_TRUNCATED_MARKER,
    )
}

fn delegation_wait_result_section(delegation: &DelegationRecord) -> String {
    let result = delegation.result.as_ref();
    let summary = result
        .map(|result| result.summary.trim())
        .filter(|summary| !summary.is_empty())
        .unwrap_or("No result summary was recorded.");
    let commands = result
        .map(|result| {
            result
                .commands_run
                .iter()
                .map(|command| format!("- `{}`: {}", command.command, command.status))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let changed_files = result
        .map(|result| {
            result
                .changed_files
                .iter()
                .map(|path| format!("- `{path}`"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let findings = result
        .map(|result| {
            result
                .findings
                .iter()
                .map(format_delegation_finding)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let notes = result
        .map(|result| {
            result
                .notes
                .iter()
                .map(|note| format!("- {note}"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let commands = if commands.is_empty() {
        "- None".to_owned()
    } else {
        commands.join("\n")
    };
    let findings = if findings.is_empty() {
        "- None".to_owned()
    } else {
        findings.join("\n")
    };
    let changed_files = if changed_files.is_empty() {
        "- None".to_owned()
    } else {
        changed_files.join("\n")
    };
    let notes = if notes.is_empty() {
        "- None".to_owned()
    } else {
        notes.join("\n")
    };

    format!(
        "### {} (`{}`)\n\nStatus: {}\nChild session: `{}`\n\nSummary:\n{}\n\nFindings:\n{}\n\nChanged files:\n{}\n\nCommands run:\n{}\n\nNotes:\n{}",
        delegation.title,
        delegation.id,
        delegation_status_label(delegation.status),
        delegation.child_session_id,
        summary,
        findings,
        changed_files,
        commands,
        notes
    )
}

fn format_delegation_finding(finding: &DelegationFinding) -> String {
    let location = match (finding.file.as_deref(), finding.line) {
        (Some(file), Some(line)) => format!(" `{file}:{line}`"),
        (Some(file), None) => format!(" `{file}`"),
        (None, Some(line)) => format!(" line {line}"),
        (None, None) => String::new(),
    };
    format!(
        "- {}{} - {}",
        finding.severity, location, finding.message
    )
}

fn delegation_status_label(status: DelegationStatus) -> &'static str {
    match status {
        DelegationStatus::Queued => "queued",
        DelegationStatus::Running => "running",
        DelegationStatus::Completed => "completed",
        DelegationStatus::Failed => "failed",
        DelegationStatus::Canceled => "canceled",
    }
}

fn delegation_prompt_write_policy(write_policy: &DelegationWritePolicy) -> String {
    match write_policy {
        DelegationWritePolicy::ReadOnly => {
            "Write policy: read-only. Do not edit, stage, commit, push, or otherwise mutate files."
                .to_owned()
        }
        DelegationWritePolicy::IsolatedWorktree {
            owned_paths,
            worktree_path,
        } => {
            let worktree_path = worktree_path.as_deref().unwrap_or("(pending)");
            let owned_paths = if owned_paths.is_empty() {
                "No owned paths were declared.".to_owned()
            } else {
                format!("Owned paths: {}.", owned_paths.join(", "))
            };
            format!(
                "Write policy: isolated worktree. You may write only inside the isolated worktree `{worktree_path}`. Do not edit, stage, commit, push, or otherwise mutate the parent workspace. {owned_paths}"
            )
        }
        DelegationWritePolicy::SharedWorktree { owned_paths } => {
            let owned_paths = if owned_paths.is_empty() {
                "No owned paths were declared.".to_owned()
            } else {
                format!("Owned paths: {}.", owned_paths.join(", "))
            };
            format!(
                "Write policy: shared worktree. Do not write outside declared ownership. {owned_paths}"
            )
        }
    }
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
        source: ParallelAgentSource::Delegation,
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

/// Updates the parent parallel-agent card for a delegation lifecycle change.
///
/// The parent-card lookup key is `(parent_session, agent_id, source)`.
/// Tool-sourced rows can share the same visible id shape, but delegation
/// lifecycle updates only own rows whose source is `ParallelAgentSource::Delegation`.
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
        let Some(agent) = agents.iter_mut().find(|agent| {
            agent.id == delegation.id && agent.source == ParallelAgentSource::Delegation
        }) else {
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
                inner.mark_delegation_mutated(delegation_index);
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
            findings,
            changed_files,
            commands_run,
            notes,
        } => {
            let completed_at = stamp_now();
            let result = DelegationResult {
                delegation_id: delegation.id.clone(),
                child_session_id: delegation.child_session_id.clone(),
                status: DelegationStatus::Completed,
                summary,
                findings,
                changed_files,
                commands_run,
                notes,
            };
            let record = inner.delegations.get_mut(delegation_index)?;
            record.status = DelegationStatus::Completed;
            record.completed_at = Some(completed_at.clone());
            record.result = Some(result.clone());
            inner.sync_running_read_only_delegation_index(delegation_index);
            inner.mark_delegation_mutated(delegation_index);
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
    inner.mark_delegation_mutated(delegation_index);
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
    inner.mark_delegation_mutated(delegation_index);
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

fn delegation_codex_sandbox_mode(write_policy: &DelegationWritePolicy) -> CodexSandboxMode {
    match write_policy {
        DelegationWritePolicy::ReadOnly => CodexSandboxMode::ReadOnly,
        DelegationWritePolicy::IsolatedWorktree { .. } => CodexSandboxMode::WorkspaceWrite,
        DelegationWritePolicy::SharedWorktree { .. } => CodexSandboxMode::WorkspaceWrite,
    }
}

fn prepare_isolated_delegation_worktree(
    source_cwd: &str,
    requested_worktree_path: Option<&str>,
    default_workdir: &str,
    delegation_id: &str,
) -> Result<PreparedIsolatedWorktree, ApiError> {
    if let Some(path) = requested_worktree_path {
        validate_delegation_cwd_input(path)?;
    }
    let source_cwd_path = fs::canonicalize(FsPath::new(source_cwd))
        .map(|path| normalize_user_facing_path(&path))
        .map_err(|err| {
            ApiError::bad_request(format!(
                "delegation cwd `{source_cwd}` could not be resolved: {err}"
            ))
        })?;
    let source_repo_root = resolve_git_repo_root(&source_cwd_path)?.ok_or_else(|| {
        ApiError::bad_request("isolatedWorktree delegation requires a git repository")
    })?;
    let source_repo_root = fs::canonicalize(&source_repo_root)
        .map(|path| normalize_user_facing_path(&path))
        .map_err(|err| {
            ApiError::internal(format!(
                "failed to canonicalize git repo root {}: {err}",
                source_repo_root.display()
            ))
        })?;
    let relative_cwd = source_cwd_path
        .strip_prefix(&source_repo_root)
        .map_err(|_| {
            ApiError::bad_request(format!(
                "delegation cwd `{}` must stay inside git repository `{}`",
                source_cwd_path.display(),
                source_repo_root.display()
            ))
        })?
        .to_path_buf();

    let worktree_root =
        resolve_isolated_worktree_root(requested_worktree_path, default_workdir, delegation_id)?;
    let source_repo_root_text = source_repo_root.to_string_lossy();
    let worktree_root_text = worktree_root.to_string_lossy();
    if path_contains(source_repo_root_text.as_ref(), &worktree_root)
        || path_contains(worktree_root_text.as_ref(), &source_repo_root)
    {
        return Err(ApiError::bad_request(
            "isolated worktree path must be outside the source repository",
        ));
    }

    ensure_isolated_worktree_target_available(&worktree_root)?;
    let staged_patch = git_repo_output(
        &source_repo_root,
        &["diff", "--cached", "--binary"],
        "failed to collect staged git diff for isolated worktree",
    )?;
    let unstaged_patch = git_repo_output(
        &source_repo_root,
        &["diff", "--binary"],
        "failed to collect unstaged git diff for isolated worktree",
    )?;

    create_detached_git_worktree(&source_repo_root, &worktree_root)?;
    if git_patch_has_content(&staged_patch) {
        run_git_repo_command_with_input(
            &worktree_root,
            &["apply", "--index", "--binary"],
            &staged_patch,
            "failed to apply staged git diff to isolated worktree",
        )?;
    }
    if git_patch_has_content(&unstaged_patch) {
        run_git_repo_command_with_input(
            &worktree_root,
            &["apply", "--binary"],
            &unstaged_patch,
            "failed to apply unstaged git diff to isolated worktree",
        )?;
    }

    let child_cwd = normalize_user_facing_path(&worktree_root.join(relative_cwd));
    Ok(PreparedIsolatedWorktree {
        child_cwd: child_cwd.to_string_lossy().into_owned(),
        worktree_root: worktree_root.to_string_lossy().into_owned(),
    })
}

fn resolve_isolated_worktree_root(
    path: Option<&str>,
    default_workdir: &str,
    delegation_id: &str,
) -> Result<PathBuf, ApiError> {
    let requested_path = match path.map(str::trim).filter(|value| !value.is_empty()) {
        Some(path) => resolve_requested_path(path)?,
        None => resolve_termal_data_dir(default_workdir)
            .join("delegations")
            .join(delegation_id)
            .join("worktree"),
    };
    canonicalize_path_with_existing_ancestor(&requested_path)
}

fn ensure_isolated_worktree_target_available(worktree_root: &FsPath) -> Result<(), ApiError> {
    match fs::metadata(worktree_root) {
        Ok(metadata) if !metadata.is_dir() => Err(ApiError::bad_request(format!(
            "isolated worktree path `{}` must be a directory",
            worktree_root.display()
        ))),
        Ok(_) => {
            let mut entries = fs::read_dir(worktree_root).map_err(|err| {
                ApiError::internal(format!(
                    "failed to inspect isolated worktree path {}: {err}",
                    worktree_root.display()
                ))
            })?;
            if entries.next().is_some() {
                return Err(ApiError::bad_request(format!(
                    "isolated worktree path `{}` must be empty or not exist",
                    worktree_root.display()
                )));
            }
            Ok(())
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(ApiError::internal(format!(
            "failed to inspect isolated worktree path {}: {err}",
            worktree_root.display()
        ))),
    }
}

fn create_detached_git_worktree(
    source_repo_root: &FsPath,
    worktree_root: &FsPath,
) -> Result<(), ApiError> {
    let parent = worktree_root.parent().ok_or_else(|| {
        ApiError::bad_request(format!(
            "isolated worktree path `{}` is invalid",
            worktree_root.display()
        ))
    })?;
    fs::create_dir_all(parent).map_err(|err| {
        ApiError::internal(format!(
            "failed to create isolated worktree parent {}: {err}",
            parent.display()
        ))
    })?;

    let output = git_command()
        .arg("-C")
        .arg(source_repo_root)
        .args(["worktree", "add", "--detach"])
        .arg(worktree_root)
        .arg("HEAD")
        .output()
        .map_err(|err| ApiError::internal(format!("failed to create git worktree: {err}")))?;
    if output.status.success() {
        return Ok(());
    }
    let detail = extract_git_command_error(&output);
    if detail.is_empty() {
        Err(ApiError::bad_request(
            "failed to create isolated git worktree",
        ))
    } else {
        Err(ApiError::bad_request(format!(
            "failed to create isolated git worktree: {detail}"
        )))
    }
}

fn git_repo_output(
    repo_root: &FsPath,
    args: &[&str],
    error_context: &str,
) -> Result<Vec<u8>, ApiError> {
    let output = git_command()
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .map_err(|err| ApiError::internal(format!("{error_context}: {err}")))?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    let detail = extract_git_command_error(&output);
    if detail.is_empty() {
        Err(ApiError::bad_request(error_context))
    } else {
        Err(ApiError::bad_request(format!("{error_context}: {detail}")))
    }
}

fn run_git_repo_command_with_input(
    repo_root: &FsPath,
    args: &[&str],
    input: &[u8],
    error_context: &str,
) -> Result<(), ApiError> {
    let mut child = git_command()
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| ApiError::internal(format!("{error_context}: {err}")))?;
    let Some(mut stdin) = child.stdin.take() else {
        return Err(ApiError::internal(format!(
            "{error_context}: failed to open git stdin"
        )));
    };
    stdin
        .write_all(input)
        .map_err(|err| ApiError::internal(format!("{error_context}: {err}")))?;
    drop(stdin);

    let output = child
        .wait_with_output()
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

fn git_patch_has_content(patch: &[u8]) -> bool {
    patch.iter().any(|byte| !byte.is_ascii_whitespace())
}

enum DelegationChildOutcome {
    Running,
    Completed {
        summary: String,
        findings: Vec<DelegationFinding>,
        changed_files: Vec<String>,
        commands_run: Vec<DelegationCommandResult>,
        notes: Vec<String>,
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
    findings: Vec<DelegationFinding>,
    notes: Vec<String>,
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
                        findings: result.findings,
                        changed_files: child_changed_files(&child.session),
                        commands_run: child_commands_run(&child.session),
                        notes: result.notes,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DelegationResultSection {
    Summary,
    Findings,
    Notes,
    FilesInspected,
    Ignored,
}

fn parse_delegation_result_packet(text: &str) -> Option<ParsedDelegationResult> {
    let search_window = delegation_result_search_window(text);
    let mut lines = search_window
        .lines()
        .skip_while(|line| !line.trim().eq_ignore_ascii_case("## Result"));
    lines.next()?;

    let mut status = None;
    let mut summary_lines = Vec::new();
    let mut finding_lines = Vec::new();
    let mut note_lines: Vec<String> = Vec::new();
    let mut section: Option<DelegationResultSection> = None;
    for line in lines {
        let cleaned = line.trim();
        if section.is_none() {
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
        }

        if let Some(next_section) = delegation_result_section_heading(cleaned) {
            section = Some(next_section);
            continue;
        }

        match section {
            Some(DelegationResultSection::Summary) => summary_lines.push(line),
            Some(DelegationResultSection::Findings) => finding_lines.push(line),
            Some(DelegationResultSection::Notes) => note_lines.push(line.to_owned()),
            Some(DelegationResultSection::FilesInspected) => {
                if let Some(note) = parse_delegation_note_line(line) {
                    note_lines.push(format!("Inspected {note}"));
                }
            }
            Some(DelegationResultSection::Ignored) | None => {}
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

    Some(ParsedDelegationResult {
        status,
        summary,
        findings: finding_lines
            .iter()
            .filter_map(|line| parse_delegation_finding_line(line))
            .collect(),
        notes: note_lines
            .iter()
            .filter_map(|line| parse_delegation_note_line(line))
            .collect(),
    })
}

fn delegation_result_section_heading(cleaned: &str) -> Option<DelegationResultSection> {
    let Some(label) = cleaned.strip_suffix(':') else {
        return None;
    };
    match label.trim().to_ascii_lowercase().as_str() {
        "summary" => Some(DelegationResultSection::Summary),
        "findings" => Some(DelegationResultSection::Findings),
        "notes" => Some(DelegationResultSection::Notes),
        "files inspected" => Some(DelegationResultSection::FilesInspected),
        "commands run" => Some(DelegationResultSection::Ignored),
        _ => None,
    }
}

fn parse_delegation_note_line(line: &str) -> Option<String> {
    let text = normalize_delegation_result_list_item(line);
    if text.is_empty() || text.eq_ignore_ascii_case("none") {
        return None;
    }
    Some(text.to_owned())
}

fn parse_delegation_finding_line(line: &str) -> Option<DelegationFinding> {
    let text = normalize_delegation_result_list_item(line);
    if text.is_empty() || text.eq_ignore_ascii_case("none") {
        return None;
    }
    let (head, message) = text
        .split_once(" - ")
        .map(|(head, message)| (head.trim(), message.trim()))
        .unwrap_or(("", text));
    let (severity, location) = parse_delegation_finding_head(head);
    let severity = if severity.is_empty() {
        "Note"
    } else {
        severity
    };
    let (file, line) = parse_delegation_finding_location(location);
    Some(DelegationFinding {
        severity: severity.to_owned(),
        file,
        line,
        message: message.to_owned(),
    })
}

fn parse_delegation_finding_head(head: &str) -> (&str, &str) {
    let head = head.trim();
    if let Some((severity, location)) = head.rsplit_once(char::is_whitespace) {
        if !severity.trim().is_empty()
            && looks_like_delegation_finding_location(location)
        {
            return (severity.trim(), location.trim());
        }
    }
    head.split_once(char::is_whitespace)
        .map(|(severity, location)| (severity.trim(), location.trim()))
        .unwrap_or((head, ""))
}

fn looks_like_delegation_finding_location(location: &str) -> bool {
    let location = location.trim().trim_matches('`');
    if location.is_empty() {
        return false;
    }
    if location.contains('/') || location.contains('\\') {
        return true;
    }
    location
        .rsplit_once(':')
        .is_some_and(|(file, line)| !file.trim().is_empty() && line.parse::<u32>().is_ok())
}

fn parse_delegation_finding_location(location: &str) -> (Option<String>, Option<u32>) {
    let location = location.trim().trim_matches('`');
    if location.is_empty() {
        return (None, None);
    }
    if let Some((file, line)) = location.rsplit_once(':') {
        let file = file.trim().trim_matches('`');
        if let Ok(line) = line.parse::<u32>() {
            if !file.is_empty() {
                return (Some(file.to_owned()), Some(line));
            }
        }
        if !file.is_empty() {
            return (Some(file.to_owned()), None);
        }
    }
    (Some(location.to_owned()), None)
}

fn normalize_delegation_result_list_item(line: &str) -> &str {
    line.trim()
        .strip_prefix("- ")
        .or_else(|| line.trim().strip_prefix("* "))
        .unwrap_or_else(|| line.trim())
        .trim()
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

fn truncate_to_byte_limit_with_marker(
    mut value: String,
    max_bytes: usize,
    marker: &str,
) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    if marker.len() >= max_bytes {
        let mut end = max_bytes;
        while end > 0 && !marker.is_char_boundary(end) {
            end -= 1;
        }
        return marker[..end].to_owned();
    }

    let mut end = max_bytes - marker.len();
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value.push_str(marker);
    value
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
