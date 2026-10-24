// Owns immutable exact-revision metadata cuts and the ordered worker lifetime.
// Summary inputs are stamped SessionRecord fields, the local instance domain,
// and the separately advancing body sequence. Queue pause is also keyed so
// transient latch changes cannot hide behind a prior record stamp. Cache hits
// also compare every borrowed metadata field; stamps are an optimization, not
// sole correctness authority. New projection inputs require this audit again.
// Producers hold StateInner before the cache or mailbox.pending; the worker
// reads only owned cuts, never the cache. Closing accepts late work; Stopped
// is set under StateInner only after drain, before synchronous fallback.

#[derive(PartialEq, Eq)]
struct StateProjectionKey {
    mutation_stamp: u64,
    body_seq: Option<u64>,
    queue_paused: bool,
}

impl StateProjectionKey {
    fn for_record(record: &SessionRecord) -> Self {
        Self {
            mutation_stamp: record.mutation_stamp,
            body_seq: if record.is_local_session() {
                record.wire_body_seq()
            } else {
                record.session.body_seq
            },
            queue_paused: record.orchestrator_auto_dispatch_blocked,
        }
    }
}

#[derive(Default)]
struct StateProjectionCache {
    server_instance_id: String,
    sessions: HashMap<String, (StateProjectionKey, Arc<StateSessionSummary>)>,
}

struct ImmutableStateSnapshot {
    header: StateResponse,
    sessions: Vec<Arc<StateSessionSummary>>,
}

impl ImmutableStateSnapshot {
    fn materialize(mut self) -> StateResponse {
        // No StateInner/cache guard survives the handoff to this worker.
        self.header.sessions = self
            .sessions
            .into_iter()
            .map(|summary| (*summary).clone())
            .collect();
        self.header
    }
}

impl AppState {
    // Borrowed metadata validation is the safety net for an unstamped writer.
    // No Session/StateSessionSummary or transcript is cloned on a cache hit.
    // Exhaustive destructuring makes a new wire field a compile-time audit.
    fn cached_state_summary_matches_record(
        local_instance_id: &str,
        record: &SessionRecord,
        summary: &StateSessionSummary,
    ) -> bool {
        let StateSessionSummary {
            mailbox_participant_id,
            body_seq,
            body_seq_epoch,
            kimi_effort,
            kimi_current_effort,
            kimi_effort_options,
            kimi_approval_mode,
            kimi_mode,
            kimi_current_mode,
            opencode_approval_mode,
            id,
            name,
            emoji,
            agent,
            workdir,
            project_id,
            remote_id,
            model,
            model_options,
            approval_policy,
            reasoning_effort,
            codex_fast_mode,
            sandbox_mode,
            cursor_mode,
            claude_effort,
            claude_approval_mode,
            gemini_approval_mode,
            opencode_model,
            opencode_effort,
            opencode_current_effort,
            opencode_effort_options,
            opencode_mode,
            opencode_current_mode,
            opencode_mode_options,
            external_session_id,
            agent_commands_revision,
            codex_thread_state,
            live_activity,
            engram_boot_recovery_pending,
            status,
            preview,
            message_count,
            markers,
            queue_paused,
            provider_capacity_waiting,
            source_tracking_recovery,
            queue_projection_hash,
            session_mutation_stamp,
            parent_delegation_id,
        } = summary;
        let session = &record.session;
        if !(kimi_effort == &session.kimi_effort
            && kimi_current_effort == &session.kimi_current_effort
            && kimi_effort_options == &session.kimi_effort_options
            && kimi_approval_mode == &session.kimi_approval_mode
            && kimi_mode == &session.kimi_mode
            && kimi_current_mode == &session.kimi_current_mode
            && opencode_approval_mode == &session.opencode_approval_mode
            && id == &session.id
            && name == &session.name
            && emoji == &session.emoji
            && agent == &session.agent
            && workdir == &session.workdir
            && project_id == &session.project_id
            && model == &session.model
            && model_options == &session.model_options
            && approval_policy == &session.approval_policy
            && reasoning_effort == &session.reasoning_effort
            && codex_fast_mode == &session.codex_fast_mode
            && sandbox_mode == &session.sandbox_mode
            && cursor_mode == &session.cursor_mode
            && claude_effort == &session.claude_effort
            && claude_approval_mode == &session.claude_approval_mode
            && gemini_approval_mode == &session.gemini_approval_mode
            && opencode_model == &session.opencode_model
            && opencode_effort == &session.opencode_effort
            && opencode_current_effort == &session.opencode_current_effort
            && opencode_effort_options == &session.opencode_effort_options
            && opencode_mode == &session.opencode_mode
            && opencode_current_mode == &session.opencode_current_mode
            && opencode_mode_options == &session.opencode_mode_options
            && external_session_id == &session.external_session_id
            && agent_commands_revision == &session.agent_commands_revision
            && codex_thread_state == &session.codex_thread_state
            && status == &session.status
            && preview == &session.preview
            && markers == &session.markers
            && provider_capacity_waiting == &session.provider_capacity_waiting
            && parent_delegation_id == &session.parent_delegation_id)
        {
            return false;
        }
        let local = record.is_local_session();
        let current_sequence = if local {
            record.wire_body_seq()
        } else {
            session.body_seq
        };
        let current_epoch = if local {
            Some(local_instance_id)
        } else {
            session.body_seq_epoch.as_deref()
        };
        let (current_sequence, current_epoch) =
            if current_sequence.is_some() && current_epoch.is_some() {
                (current_sequence, current_epoch)
            } else {
                (None, None)
            };
        remote_id == &record.remote_id
            && *session_mutation_stamp == Some(record.mutation_stamp)
            && *engram_boot_recovery_pending == record.engram_boot_recovery_pending
            && *message_count == session_message_count(record)
            && *queue_paused == record.orchestrator_auto_dispatch_blocked
            && *source_tracking_recovery
                == if local {
                    record.engram.source_observation_delete_requested
                } else {
                    session.source_tracking_recovery
                }
            && *body_seq == current_sequence
            && body_seq_epoch.as_deref() == current_epoch
            && *mailbox_participant_id
                == record
                    .federation_mailbox_binding
                    .as_ref()
                    .map(FederationMailboxBinding::participant_key)
            && *live_activity
                == (session.status == SessionStatus::Active)
                    .then(|| bounded_live_activity_summary(&session.live_activity))
                    .flatten()
            && *queue_projection_hash
                == if local {
                    Some(Self::queue_projection_hash(record))
                } else {
                    session.queue_projection_hash.clone()
                }
    }

    fn immutable_state_snapshot_locked(&self, inner: &StateInner) -> ImmutableStateSnapshot {
        #[cfg(test)]
        assert!(!self.inner.is_not_held_by_current_thread_for_test());
        let mut cache = inner
            .state_projection_cache
            .lock()
            .expect("projection cache mutex poisoned");
        // The only projection input outside the stamped record is the local
        // body-sequence epoch. A new instance cannot reuse the old domain.
        if cache.server_instance_id != self.server_instance_id {
            cache.sessions.clear();
            cache
                .server_instance_id
                .clone_from(&self.server_instance_id);
        }
        let mut live = HashSet::new();
        let sessions = inner
            .sessions
            .iter()
            .filter(|record| !record.hidden)
            .map(|record| {
                let id = record.session.id.as_str();
                live.insert(id);
                let key = StateProjectionKey::for_record(record);
                if let Some((cached_key, summary)) = cache.sessions.get(id) {
                    if *cached_key == key
                        && Self::cached_state_summary_matches_record(
                            &self.server_instance_id,
                            record,
                            summary,
                        )
                    {
                        return summary.clone();
                    }
                }
                let summary = Arc::new(Self::wire_session_summary_from_record(
                    &self.server_instance_id,
                    record,
                ));
                cache.sessions.insert(id.to_owned(), (key, summary.clone()));
                summary
            })
            .collect();
        cache.sessions.retain(|id, _| live.contains(id.as_str()));
        drop(cache);
        ImmutableStateSnapshot {
            header: self.snapshot_header_from_inner(
                inner,
                self.cached_agent_readiness(),
                Vec::new(),
            ),
            sessions,
        }
    }

    fn warm_state_projection_cache(&self) {
        let inner = self.inner.lock().expect("state mutex poisoned");
        // One-time startup cost precedes requests; the shared projections stay.
        self.immutable_state_snapshot_locked(&inner);
    }

    fn finish_state_broadcast_if_drained(&self) -> bool {
        let mailbox = self
            .state_broadcast_mailbox
            .as_ref()
            .expect("broadcast mailbox");
        // Match producer lock order; later fallback cannot overtake old work.
        let _inner = self.inner.lock().expect("state mutex poisoned");
        let mut pending = mailbox
            .pending
            .lock()
            .expect("broadcast mailbox mutex poisoned");
        if pending.closing && pending.work.is_empty() && !pending.lagged {
            pending.stopped = true;
            true
        } else {
            false
        }
    }

    fn shutdown_state_broadcast_blocking(&self) {
        if let Some(worker) = &self.state_broadcast_worker {
            worker.shutdown();
        }
    }
}

struct StateBroadcastWorker {
    mailbox: Arc<StateBroadcastMailbox>,
    handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl StateBroadcastWorker {
    fn start(mut state: AppState) -> Self {
        // The worker must not retain its own join owner through AppState.
        state.state_broadcast_worker = None;
        let mailbox = state
            .state_broadcast_mailbox
            .as_ref()
            .expect("broadcast mailbox")
            .clone();
        let worker_mailbox = mailbox.clone();
        let handle = std::thread::Builder::new()
            .name("termal-state-broadcast".to_owned())
            .spawn(move || {
                loop {
                    match worker_mailbox.recv_next() {
                        StateBroadcastWork::Drain => {
                            if state.finish_state_broadcast_if_drained() {
                                break;
                            }
                        }
                        work => {
                            #[cfg(test)]
                            {
                                let hook = worker_mailbox.before_forward.lock().unwrap().take();
                                if let Some(hook) = hook {
                                    hook();
                                }
                            }
                            forward_state_broadcast_work(work, &state.state_broadcast_senders);
                        }
                    }
                }
            })
            .expect("failed to spawn state broadcast thread");
        Self {
            mailbox,
            handle: Mutex::new(Some(handle)),
        }
    }

    fn shutdown(&self) {
        // Serialize joiners: returning means the final drain has finished.
        let mut handle = self.handle.lock().expect("broadcast worker mutex poisoned");
        self.mailbox.begin_shutdown();
        if let Some(handle) = handle.take() {
            if let Err(error) = handle.join() {
                eprintln!("state broadcast worker join failed: {error:?}");
            }
        }
    }
}

impl Drop for StateBroadcastWorker {
    fn drop(&mut self) {
        // The final owner must be dropped off-lock; this joins a native thread.
        // Production explicitly drains before dropping its async runtime.
        self.shutdown();
    }
}

impl StateBroadcastMailbox {
    fn is_stopped(&self) -> bool {
        self.pending
            .lock()
            .expect("state broadcast mailbox mutex poisoned")
            .stopped
    }

    fn begin_shutdown(&self) {
        self.pending
            .lock()
            .expect("state broadcast mailbox mutex poisoned")
            .closing = true;
        self.work_available.notify_all();
    }
}
