// Owns terminal shared-Codex child resource release and the off-lock barrier
// before follow-up. Does not kill shared processes, sweep historical sessions,
// or change Claude/ACP cleanup. The reservation is installed with the terminal
// state; only its post-commit ticket may enqueue the external side effect.

const CODEX_CHILD_RESULT_FENCE_TIMEOUT: Duration = Duration::from_secs(5);
const CODEX_CHILD_ARCHIVE_RPC_TIMEOUT: Duration = Duration::from_secs(30);
const CODEX_CHILD_ARCHIVE_REPLY_TIMEOUT: Duration = Duration::from_secs(31);
const CODEX_THREAD_RECONCILIATION_TIMEOUT: Duration = Duration::from_secs(10);
const CODEX_THREAD_RECONCILIATION_REPLY_TIMEOUT: Duration =
    Duration::from_secs(CODEX_THREAD_RECONCILIATION_TIMEOUT.as_secs() + 1);
const CODEX_CHILD_UNARCHIVE_RPC_TIMEOUT: Duration = Duration::from_secs(30);
const CODEX_CHILD_UNARCHIVE_REPLY_TIMEOUT: Duration =
    Duration::from_secs(CODEX_CHILD_UNARCHIVE_RPC_TIMEOUT.as_secs() + 1);
// Serial fence + RPC receipt + archived/active probes + local detach/commit
// headroom. This bounds the caller's wait, not a blocked mutex or SQL commit.
const CODEX_CHILD_RELEASE_WAIT_TIMEOUT: Duration = Duration::from_secs(
    CODEX_CHILD_RESULT_FENCE_TIMEOUT.as_secs()
        + CODEX_CHILD_ARCHIVE_REPLY_TIMEOUT.as_secs()
        + 2 * CODEX_THREAD_RECONCILIATION_REPLY_TIMEOUT.as_secs()
        + 10,
);

#[derive(Clone, Debug, PartialEq, Eq)]
enum CodexReleaseOutcome {
    NotSent(String),
    Archived,
    NotArchived,
    Restored,
    // The server replied with an error, but inventory is not yet confirmed.
    Rejected(String),
    Ambiguous(String),
}

#[derive(Default)]
struct CodexDelegationRelease {
    outcome: Mutex<Option<CodexReleaseOutcome>>,
    resume: Mutex<()>,
    ready: std::sync::Condvar,
    // One owner, retained across spawn/commit failure. Never hand this slot to
    // an independent detach_async worker that can outlive the release barrier.
    local: Mutex<Option<SharedCodexSessionHandle>>,
    undurable_terminal: Mutex<Option<DelegationRecord>>,
    // Only automatic terminal release authorizes automatic unarchive.
    terminal_release: bool,
    // Retain process identity, not its writer sender (which would keep the
    // runtime channel alive). A replaced slot alone does not prove exit.
    archive_origin: Mutex<Option<(String, Arc<SharedChild>)>>,
}

impl CodexDelegationRelease {
    fn record_archive_origin(&self, runtime: &SharedCodexRuntime) {
        *self
            .archive_origin
            .lock()
            .expect("Codex archive origin mutex poisoned") =
            Some((runtime.runtime_id.clone(), runtime.process.clone()));
    }

    fn archive_origin_has_exited(&self, state: &AppState) -> bool {
        let origin = self
            .archive_origin
            .lock()
            .expect("Codex archive origin mutex poisoned")
            .clone();
        let Some((runtime_id, process)) = origin else {
            return false;
        };
        let replacement = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned")
            .as_ref()
            .is_some_and(|current| current.runtime_id != runtime_id);
        // Both facts precede the inventory probe. Never infer successful kill
        // from slot removal or signal delivery; errors remain ambiguous.
        replacement && matches!(process.try_wait(), Ok(Some(_)))
    }

    fn for_child(shared: SharedCodexSessionHandle, terminal: DelegationRecord) -> Self {
        Self {
            local: Mutex::new(Some(shared)),
            undurable_terminal: Mutex::new(Some(terminal)),
            terminal_release: true,
            ..Self::default()
        }
    }

    fn detach_on_worker(&self) {
        let mut local = self
            .local
            .lock()
            .expect("Codex release detach mutex poisoned");
        if let Some(shared) = local.as_ref() {
            shared.detach();
            *local = None;
        }
    }

    fn confirm_completed_outcome(&self, confirmed: CodexReleaseOutcome) {
        let mut outcome = self.outcome.lock().expect("Codex release mutex poisoned");
        // A late positive archive/restore must supersede an earlier unknown
        // outcome, but must not release a still-running archive operation.
        if outcome.is_some() {
            *outcome = Some(confirmed);
        }
    }

    fn finish(&self, outcome: CodexReleaseOutcome) {
        *self.outcome.lock().expect("Codex release mutex poisoned") = Some(outcome);
        self.ready.notify_all();
    }

    fn wait(&self) -> Result<CodexReleaseOutcome, ApiError> {
        let outcome = self.outcome.lock().expect("Codex release mutex poisoned");
        let (outcome, _) = self
            .ready
            .wait_timeout_while(outcome, CODEX_CHILD_RELEASE_WAIT_TIMEOUT, |value| {
                value.is_none()
            })
            .expect("Codex release mutex poisoned");
        let outcome = outcome.clone().ok_or_else(|| {
            ApiError::conflict("Codex child thread cleanup is still in progress; retry the prompt")
        })?;
        // Normally detached by the worker. A dropped ticket or failed spawn
        // retains ownership here, so retry cannot bypass local cleanup either.
        let mut local = self.local.try_lock().map_err(|_| {
            ApiError::conflict(
                "Codex child local detachment is still in progress; retry the prompt",
            )
        })?;
        if let Some(shared) = local.as_ref() {
            if !shared.try_detach() {
                return Err(ApiError::conflict(
                    "Codex child local detachment is still in progress; retry the prompt",
                ));
            }
            *local = None;
        }
        Ok(outcome)
    }

    fn confirm_durability(
        &self,
        state: &AppState,
        terminal: &DelegationRecord,
        connected: bool,
        waiter: PersistFenceWaiter,
    ) -> Result<(), ApiError> {
        if connected {
            waiter.wait().map_err(|error| {
                ApiError::internal(format!(
                    "terminal result durability was not confirmed: {error:?}"
                ))
            })?;
        } else {
            let inner = state.inner.lock().expect("state mutex poisoned");
            if !inner.delegations.iter().any(|record| record == terminal) {
                return Err(ApiError::conflict(
                    "terminal result changed before persistence",
                ));
            }
            state.persist_internal_locked(&inner).map_err(|error| {
                ApiError::internal(format!(
                    "failed persisting terminal child result: {error:#}"
                ))
            })?;
        }
        *self
            .undurable_terminal
            .lock()
            .expect("Codex release durability mutex poisoned") = None;
        Ok(())
    }

    fn retry_durability(&self, state: &AppState) -> Result<(), ApiError> {
        let terminal = self
            .undurable_terminal
            .lock()
            .expect("Codex release durability mutex poisoned")
            .clone();
        if let Some(terminal) = terminal {
            let terminal = {
                let inner = state.inner.lock().expect("state mutex poisoned");
                let current = inner
                    .find_delegation_index(&terminal.id)
                    .and_then(|index| inner.delegations.get(index))
                    .filter(|current| {
                        current.child_session_id == terminal.child_session_id
                            && current.review_result_submission_attempt
                                == terminal.review_result_submission_attempt
                            && matches!(
                                current.status,
                                DelegationStatus::Completed | DelegationStatus::Failed
                            )
                    })
                    .ok_or_else(|| {
                        ApiError::conflict(
                            "terminal delegation attempt changed before durability retry",
                        )
                    })?;
                current.clone()
            };
            let (fence, waiter) = PersistFence::new(
                PersistFenceTarget::Delegation(Box::new(terminal.clone())),
                std::time::Instant::now() + CODEX_CHILD_RESULT_FENCE_TIMEOUT,
            );
            let connected = state
                .persist_tx
                .send(PersistRequest::Fence(Box::new(fence)))
                .is_ok();
            self.confirm_durability(state, &terminal, connected, waiter)?;
        }
        Ok(())
    }
}

struct CodexDelegationReleaseTicket {
    release: Arc<CodexDelegationRelease>,
    session_id: String,
    thread_id: String,
    runtime: SharedCodexRuntime,
    terminal: DelegationRecord,
    armed: bool,
}

impl Drop for CodexDelegationReleaseTicket {
    fn drop(&mut self) {
        if self.armed {
            self.release.finish(CodexReleaseOutcome::NotSent(
                "terminal result commit did not publish thread cleanup".to_owned(),
            ));
        }
    }
}

impl CodexDelegationReleaseTicket {
    fn start(mut self, state: &AppState) {
        self.armed = false;
        // commit_locked normally only queues persistence. Ask the existing
        // writer to acknowledge this exact terminal result after SQL COMMIT.
        // Never wait here: completion may own the shared stdout/session lock.
        let (fence, waiter) = PersistFence::new(
            PersistFenceTarget::Delegation(Box::new(self.terminal.clone())),
            std::time::Instant::now() + CODEX_CHILD_RESULT_FENCE_TIMEOUT,
        );
        let connected = state
            .persist_tx
            .send(PersistRequest::Fence(Box::new(fence)))
            .is_ok();
        let release = self.release.clone();
        let state = state.clone();
        let session_id = self.session_id.clone();
        let thread_id = self.thread_id.clone();
        let runtime = self.runtime.clone();
        let terminal = self.terminal.clone();
        let spawned = std::thread::Builder::new().name("termal-child-thread-release".to_owned()).spawn(move || {
            // turn/completed may still own runtime.sessions on the reader.
            // Finish local detach off-reader before archive or follow-up.
            release.detach_on_worker();
            let mut sent = false;
            let mut rejected = false;
            let mut confirmed_not_archived = false;
            let outcome = (|| -> Result<(), ApiError> {
                release.confirm_durability(&state, &terminal, connected, waiter)?;
                let current = state.shared_codex_runtime.lock().expect("shared Codex runtime mutex poisoned")
                    .as_ref().is_some_and(|current| current.runtime_id == runtime.runtime_id);
                if !current { return Err(ApiError::conflict("Codex runtime changed before child thread archive")); }
                release.record_archive_origin(&runtime);
                let (response_tx, response_rx) = mpsc::channel();
                runtime.input_tx.send(CodexRuntimeCommand::JsonRpcRequest {
                    method: "thread/archive".to_owned(), params: json!({"threadId":thread_id}),
                    timeout: CODEX_CHILD_ARCHIVE_RPC_TIMEOUT, response_tx,
                }).map_err(|error| ApiError::internal(format!("failed to queue child thread archive: {error}")))?;
                sent = true; // Enqueued: loss of a reply is now ambiguous.
                let reply = response_rx.recv_timeout(CODEX_CHILD_ARCHIVE_REPLY_TIMEOUT)
                    .map_err(|error| match error {
                        mpsc::RecvTimeoutError::Timeout => CodexResponseError::Timeout(error.to_string()),
                        mpsc::RecvTimeoutError::Disconnected => CodexResponseError::Transport(error.to_string()),
                    }).and_then(|reply| reply);
                let current = state.shared_codex_runtime.lock().expect("shared Codex runtime mutex poisoned")
                    .as_ref().is_some_and(|current| current.runtime_id == runtime.runtime_id);
                if !current { return Err(ApiError::conflict("Codex runtime changed during child thread archive")); }
                if let Err(error) = reply {
                    rejected = matches!(error, CodexResponseError::JsonRpc(_));
                    eprintln!("codex child archive> session={session_id} thread={thread_id} error={error}");
                    if !state.probe_codex_archive_state(&thread_id, true) {
                        confirmed_not_archived = rejected && state.probe_codex_archive_state(&thread_id, false);
                        return Err(ApiError::internal(format!("child thread archive is unconfirmed: {error}")));
                    }
                }
                let mut inner = state.inner.lock().expect("state mutex poisoned");
                let Some(index) = inner.find_session_index(&session_id) else { return Ok(()); };
                let record = &inner.sessions[index];
                if record.external_session_id.as_deref() != Some(&thread_id)
                    || !record.codex_delegation_release.as_ref().is_some_and(|current| Arc::ptr_eq(current, &release)) {
                    return Err(ApiError::conflict("Codex child thread changed during archive"));
                }
                set_record_codex_thread_state(inner.session_mut_by_index(index).expect("validated child session index"), CodexThreadState::Archived);
                state.commit_locked(&mut inner).map_err(|error| ApiError::internal(format!("failed persisting released child thread: {error:#}")))?;
                Ok(())
            })().map_err(|error| error.message);
            if let Err(error) = &outcome { eprintln!("codex child release> session={session_id} error={error}"); }
            release.finish(match outcome {
                Ok(()) => CodexReleaseOutcome::Archived,
                Err(_) if confirmed_not_archived => CodexReleaseOutcome::NotArchived,
                Err(error) if rejected => CodexReleaseOutcome::Rejected(error),
                Err(error) if sent => CodexReleaseOutcome::Ambiguous(error),
                Err(error) => CodexReleaseOutcome::NotSent(error),
            });
        });
        if let Err(error) = spawned {
            self.release.finish(CodexReleaseOutcome::NotSent(format!(
                "failed to start child archive reconciliation: {error}"
            )));
        }
    }
}

impl AppState {
    fn wait_for_codex_child_release(
        &self,
        session_id: &str,
    ) -> Result<Option<CodexReleaseOutcome>, ApiError> {
        let release = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .find_session_index(session_id)
                .and_then(|index| inner.sessions[index].codex_delegation_release.clone())
        };
        release.map(|release| release.wait()).transpose()
    }

    fn prepare_codex_child_followup(&self, session_id: &str) -> Result<(), ApiError> {
        // Ordinary/remote sessions use normal admission, not a child-release
        // wait on an in-flight manual Archive operation.
        let eligible = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner.find_session_index(session_id).is_some_and(|index| {
                let record = &inner.sessions[index];
                record.session.agent == Agent::Codex
                    && record.session.parent_delegation_id.is_some()
                    && record.remote_id.is_none()
            })
        };
        if !eligible {
            return Ok(());
        }
        let barrier = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .find_session_index(session_id)
                .and_then(|index| inner.sessions[index].codex_delegation_release.clone())
        };
        let _resume = barrier
            .as_ref()
            .map(|barrier| {
                barrier.resume.try_lock().map_err(|_| {
                    ApiError::conflict(
                        "Codex child recovery is already in progress; retry the prompt",
                    )
                })
            })
            .transpose()?;
        let release = self.wait_for_codex_child_release(session_id)?;
        let mut archived = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .find_session_index(session_id)
                .is_some_and(|index| record_has_archived_codex_thread(&inner.sessions[index]))
        };
        // A late positive notification supersedes a failed RPC/probe. Without
        // it, unknown release is not absence and must remain retryable.
        if !archived {
            if let Some(
                CodexReleaseOutcome::Ambiguous(ref error)
                | CodexReleaseOutcome::Rejected(ref error),
            ) = release
            {
                let thread_id = {
                    let inner = self.inner.lock().expect("state mutex poisoned");
                    inner
                        .find_session_index(session_id)
                        .and_then(|index| inner.sessions[index].external_session_id.clone())
                };
                // Always retry archived evidence, including after a lost reply.
                // A timed-out archive can finish after an active sample while
                // its process lives. A confirmed exit plus a replacement lets
                // a new active sample settle that ambiguity safely.
                archived = thread_id
                    .as_deref()
                    .is_some_and(|thread_id| self.probe_codex_archive_state(thread_id, true));
                let confirmed_active = !archived
                    && (matches!(release, Some(CodexReleaseOutcome::Rejected(_)))
                        || barrier
                            .as_ref()
                            .is_some_and(|barrier| barrier.archive_origin_has_exited(self)))
                    && thread_id
                        .as_deref()
                        .is_some_and(|thread_id| self.probe_codex_archive_state(thread_id, false));
                if !archived && !confirmed_active {
                    return Err(ApiError::conflict(format!(
                        "Codex child cleanup was not confirmed; retry after cleanup completes or the old runtime exits. If recovering with manual Archive, choose Unarchive afterwards before continuing: {error}"
                    )));
                }
                if let Some(barrier) = &barrier {
                    barrier.finish(if archived {
                        CodexReleaseOutcome::Archived
                    } else {
                        CodexReleaseOutcome::NotArchived
                    });
                }
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                if let Some(index) = inner.find_session_index(session_id) {
                    if inner.sessions[index].external_session_id != thread_id {
                        return Err(ApiError::conflict(
                            "Codex child thread changed during reconciliation",
                        ));
                    }
                    if archived {
                        set_record_codex_thread_state(
                            inner
                                .session_mut_by_index(index)
                                .expect("validated child session index"),
                            CodexThreadState::Archived,
                        );
                        self.commit_locked(&mut inner).map_err(|error| {
                            ApiError::internal(format!(
                                "failed persisting confirmed archive: {error:#}"
                            ))
                        })?;
                    }
                    archived = record_has_archived_codex_thread(&inner.sessions[index]);
                }
            }
        }
        // NotSent is resumable, not proof of durability. Repeat the exact fence
        // before re-arm can clear the result; storage faults stay retryable.
        if let Some(barrier) = &barrier {
            barrier.retry_durability(self)?;
        }
        if archived {
            if !barrier
                .as_ref()
                .is_some_and(|barrier| barrier.terminal_release)
            {
                return Err(ApiError::conflict(
                    "the current Codex thread was archived manually; unarchive it before sending another prompt",
                ));
            }
            self.unarchive_codex_thread(session_id)?;
        }
        Ok(())
    }
}
