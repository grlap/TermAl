// Owns immutable exact-revision metadata cuts and the ordered worker lifetime.
// Summary inputs are stamped SessionRecord fields, the local instance domain,
// and the separately advancing body sequence. Queue pause is also keyed so
// transient latch changes cannot hide behind a prior record stamp. Cache hits
// also compare every borrowed metadata field; stamps are an optimization, not
// sole correctness authority. New projection inputs require this audit again.
// Producers hold StateInner only to capture an owned cut: the header, the
// copy-on-write session rows and a handle to the session summary projector,
// whose capture queue records the cut in capture order. Cut consumers
// validate and project after StateInner is released, holding only the
// summary cache mutex, which no StateInner holder takes. The first consumer
// to project projects every live earlier capture first, in capture order, so
// the cache sees cuts in the order an under-lock projection did; a cut
// dropped unprojected was shown to no one and is skipped.
// Producers take StateInner before mailbox.pending. Closing accepts late
// work; Stopped is set under StateInner only after drain, before synchronous
// fallback.

use super::*;

#[cfg(test)]
thread_local! {
    /// Runs as a cut's rows are validated or projected, cache hit or miss.
    pub(crate) static TEST_BEFORE_SESSION_SUMMARY_VALIDATION: std::cell::RefCell<Option<Box<dyn FnMut()>>> = const { std::cell::RefCell::new(None) };
}

#[derive(PartialEq, Eq)]
pub(crate) struct StateProjectionKey {
    pub(crate) mutation_stamp: u64,
    pub(crate) body_seq: Option<u64>,
    pub(crate) queue_paused: bool,
}

impl StateProjectionKey {
    pub(crate) fn for_record(record: &SessionRecord) -> Self {
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
pub(crate) struct StateProjectionCache {
    // A producer only enqueues a weak capture handle here; the cache mutex is
    // taken by cut consumers after StateInner is released, so a cold
    // projection never stalls StateInner.
    pub(crate) session_summaries: Arc<SessionSummaryProjector>,
    pub(crate) test_runs: Option<Arc<TestRunHistoryProjection>>,
    pub(crate) test_run_version: u64,
    #[cfg(test)]
    pub(crate) before_materialize: Option<Box<dyn FnOnce() + Send>>,
    #[cfg(test)]
    pub(crate) capture_sink:
        Option<mpsc::Sender<(std::time::Instant, Duration, std::thread::ThreadId)>>,
}

pub(crate) struct CachedStateSessionSummary {
    pub(crate) key: StateProjectionKey,
    pub(crate) summary: Arc<StateSessionSummary>,
    // Only the local public hash's inputs, with no queued text/attachments.
    // They are projected with summary from the same captured row and
    // compared even when a producer forgot to advance its session's mutation
    // stamp. None also distinguishes a nonlocal record from an empty local
    // queue.
    queue_identities: Option<Vec<(String, bool, bool)>>,
}

#[derive(Default)]
pub(crate) struct SessionSummaryCache {
    pub(crate) server_instance_id: String,
    pub(crate) sessions: HashMap<String, CachedStateSessionSummary>,
}

/// Session rows captured under StateInner, and the slot a consumer of a later
/// cut fills when it projects them first. The rows are copy-on-write handles,
/// so later writers detach instead of changing them.
pub(crate) struct CapturedSessionRows {
    rows: SessionSequence,
    local_instance_id: String,
    projected: Mutex<Option<Vec<Arc<StateSessionSummary>>>>,
}

/// Captures not yet projected, oldest first, and the length at which dead
/// entries behind a live one are next compacted out.
#[derive(Default)]
struct CaptureQueue {
    entries: std::collections::VecDeque<std::sync::Weak<CapturedSessionRows>>,
    compact_at: usize,
}

/// The fewest queued captures that trigger a compaction.
pub(crate) const CAPTURE_QUEUE_COMPACT_FLOOR: usize = 32;

/// The summary cache and the captures not yet projected, oldest first.
#[derive(Default)]
pub(crate) struct SessionSummaryProjector {
    pub(crate) cache: Mutex<SessionSummaryCache>,
    // Taken under StateInner, briefly, and by projectors only to pop.
    captured: Mutex<CaptureQueue>,
}

impl SessionSummaryProjector {
    /// Under StateInner: queues a capture in capture order. Only a weak
    /// handle is kept, so a cut dropped unprojected frees its rows. Dead
    /// entries leave from the front at once, and from behind a live front
    /// (a cut held before projection) when the queue doubles past its live
    /// length, so the queue stays proportional to the live cuts at an
    /// amortized constant cost per capture. Live entries keep their order.
    fn register(&self, capture: &Arc<CapturedSessionRows>) {
        let mut captured = self
            .captured
            .lock()
            .expect("session summary capture queue poisoned");
        while captured
            .entries
            .front()
            .is_some_and(|front| front.strong_count() == 0)
        {
            captured.entries.pop_front();
        }
        if captured.entries.len() >= captured.compact_at.max(CAPTURE_QUEUE_COMPACT_FLOOR) {
            captured.entries.retain(|entry| entry.strong_count() > 0);
            captured.compact_at = captured.entries.len() * 2;
        }
        captured.entries.push_back(Arc::downgrade(capture));
    }

    #[cfg(test)]
    pub(crate) fn queued_captures_for_test(&self) -> usize {
        self.captured
            .lock()
            .expect("session summary capture queue poisoned")
            .entries
            .len()
    }

    /// After StateInner release: projects every live capture queued before
    /// `own` in capture order, leaving each result for its cut, then `own`.
    fn project(&self, own: &Arc<CapturedSessionRows>) -> Vec<Arc<StateSessionSummary>> {
        let mut cache = self
            .cache
            .lock()
            .expect("session summary cache mutex poisoned");
        if let Some(projected) = own
            .projected
            .lock()
            .expect("session summary slot poisoned")
            .take()
        {
            return projected;
        }
        loop {
            let next = self
                .captured
                .lock()
                .expect("session summary capture queue poisoned")
                .entries
                .pop_front();
            // A capture is queued before its cut exists, only a projector
            // holding the cache pops it, and compaction drops only dead
            // entries, so `own` is still queued here.
            let Some(next) = next else {
                break;
            };
            let Some(next) = next.upgrade() else {
                continue;
            };
            let projected = AppState::project_session_summaries(
                &next.local_instance_id,
                &next.rows,
                &mut cache,
            );
            if Arc::ptr_eq(&next, own) {
                return projected;
            }
            *next
                .projected
                .lock()
                .expect("session summary slot poisoned") = Some(projected);
        }
        AppState::project_session_summaries(&own.local_instance_id, &own.rows, &mut cache)
    }
}

/// A cut's captured rows, not yet projected.
pub(crate) struct PendingSessionProjection {
    captured: Arc<CapturedSessionRows>,
    projector: Arc<SessionSummaryProjector>,
}

pub(crate) struct ImmutableStateSnapshot {
    pub(crate) header: StateResponse,
    /// Empty until `project_pending_sessions` runs, after StateInner release.
    pub(crate) sessions: Vec<Arc<StateSessionSummary>>,
    pub(crate) pending_sessions: Option<PendingSessionProjection>,
    /// The full-state request generation read under StateInner when this cut
    /// was captured (`state_broadcast_diff.rs`).
    pub(crate) request_generation: u64,
    #[cfg(test)]
    pub(crate) before_materialize: Option<Box<dyn FnOnce() + Send>>,
}

impl ImmutableStateSnapshot {
    /// Validates and projects the captured rows, and fills the header's
    /// session-derived fields. Every consumer calls this before reading
    /// `sessions`; it does nothing on a cut that is already projected.
    pub(crate) fn project_pending_sessions(&mut self) {
        let Some(PendingSessionProjection {
            captured,
            projector,
        }) = self.pending_sessions.take()
        else {
            return;
        };
        self.sessions = projector.project(&captured);
        let rows = &captured.rows;
        self.header.queue_pause_reasons = AppState::queue_pause_reasons_from(rows.iter());
        self.header.pending_engram_mcp_revocation_session_ids =
            AppState::pending_engram_mcp_revocation_session_ids_from(rows.iter());
    }

    pub(crate) fn materialize(mut self) -> StateResponse {
        #[cfg(test)]
        if let Some(hook) = self.before_materialize.take() {
            hook();
        }
        #[cfg(test)]
        observe_state_snapshot_projection_for_test();
        self.project_pending_sessions();
        self.prepare_test_run_activity();
        // No StateInner/cache guard survives the handoff to this consumer.
        self.header.sessions = self
            .sessions
            .into_iter()
            .map(|summary| (*summary).clone())
            .collect();
        self.header.mirrored_delegation_child_ids =
            mirrored_delegation_child_ids(&self.header.sessions)
                .map(str::to_owned)
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
        queue_identities: Option<&[(String, bool, bool)]>,
    ) -> bool {
        let StateSessionSummary {
            mailbox_participant_id,
            body_seq,
            body_seq_epoch,
            kimi_effort,
            kimi_current_effort,
            kimi_effort_options,
            gemini_reasoning,
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
            mirrored_delegation_child,
            // This is attached from the separate exact test-run cut after
            // metadata cache lookup; it is never a SessionRecord input.
            test_run_activity: _,
        } = summary;
        let session = &record.session;
        if !(kimi_effort == &session.kimi_effort
            && kimi_current_effort == &session.kimi_current_effort
            && kimi_effort_options == &session.kimi_effort_options
            && gemini_reasoning == &session.gemini_reasoning
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
            && parent_delegation_id == &session.parent_delegation_id
            && mirrored_delegation_child == &session.mirrored_delegation_child)
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
            && *message_count == session_message_count(record)
            && *queue_paused == record.orchestrator_auto_dispatch_blocked
            && *source_tracking_recovery
                == if local {
                    record.engram.observation_delete_requested
                } else {
                    session.source_tracking_recovery
                }
            && *body_seq == current_sequence
            && body_seq_epoch.as_deref() == current_epoch
            && *mailbox_participant_id
                == record
                    .mailbox_binding()
                    .map(FederationMailboxBinding::participant_key)
            && *live_activity
                == (session.status == SessionStatus::Active)
                    .then(|| bounded_live_activity_summary(&session.live_activity))
                    .flatten()
            && match (local, queue_identities) {
                (true, Some(identities)) => {
                    projected_pending_prompt_identities(record).eq(identities
                        .iter()
                        .map(|(id, interrupted, retained)| (id.as_str(), *interrupted, *retained)))
                }
                (false, None) => queue_projection_hash == &session.queue_projection_hash,
                _ => false,
            }
    }

    pub(crate) fn immutable_state_snapshot_locked(
        &self,
        inner: &StateInner,
    ) -> ImmutableStateSnapshot {
        #[cfg(test)]
        let capture_started = std::time::Instant::now();
        #[cfg(test)]
        assert!(!self.inner.is_not_held_by_current_thread_for_test());
        // Only the test hooks below take from the cache.
        #[cfg_attr(not(test), allow(unused_mut))]
        let mut cache = inner
            .state_projection_cache
            .lock()
            .expect("projection cache mutex poisoned");
        let projector = cache.session_summaries.clone();
        #[cfg(test)]
        let before_materialize = cache.before_materialize.take();
        #[cfg(test)]
        let capture_sink = cache.capture_sink.clone();
        drop(cache);
        let header = self.snapshot_header_without_session_scans(
            inner,
            self.cached_agent_readiness(),
            Vec::new(),
        );
        // Only row handles are copied here; per-session validation and
        // projection run in `project_pending_sessions`, after release.
        let captured = Arc::new(CapturedSessionRows {
            rows: inner.sessions.clone(),
            local_instance_id: header.server_instance_id.clone(),
            projected: Mutex::new(None),
        });
        // Queued under StateInner, so the queue follows capture order.
        projector.register(&captured);
        let snapshot = ImmutableStateSnapshot {
            header,
            sessions: Vec::new(),
            pending_sessions: Some(PendingSessionProjection {
                captured,
                projector,
            }),
            // Read under StateInner, which every request increment that must
            // bind to a cut (the full-state route) also holds.
            request_generation: self
                .state_broadcast_senders
                .full_state_request_generation
                .load(Ordering::SeqCst),
            #[cfg(test)]
            before_materialize,
        };
        #[cfg(test)]
        if let Some(sink) = capture_sink {
            let _ = sink.send((
                capture_started,
                capture_started.elapsed(),
                std::thread::current().id(),
            ));
        }
        snapshot
    }

    /// Projects one capture's rows through the summary cache, which the
    /// caller holds. No StateInner guard is held: the rows are immutable
    /// handles and the validator decides every reuse. Captures reach this in
    /// capture order (`SessionSummaryProjector::project`).
    pub(crate) fn project_session_summaries(
        local_instance_id: &str,
        rows: &SessionSequence,
        cache: &mut SessionSummaryCache,
    ) -> Vec<Arc<StateSessionSummary>> {
        #[cfg(test)]
        TEST_BEFORE_SESSION_SUMMARY_VALIDATION.with(|hook| {
            if let Some(hook) = hook.borrow_mut().as_mut() {
                hook();
            }
        });
        // The only projection input outside the stamped record is the local
        // body-sequence epoch. A new instance cannot reuse the old domain.
        if cache.server_instance_id != local_instance_id {
            cache.sessions.clear();
            local_instance_id.clone_into(&mut cache.server_instance_id);
        }
        let mut live = HashSet::new();
        let sessions = rows
            .iter()
            .filter(|record| !record.hidden)
            .map(|record| {
                let id = record.session.id.as_str();
                live.insert(id);
                let key = StateProjectionKey::for_record(record);
                if let Some(cached) = cache.sessions.get(id) {
                    if cached.key == key
                        && Self::cached_state_summary_matches_record(
                            local_instance_id,
                            record,
                            &cached.summary,
                            cached.queue_identities.as_deref(),
                        )
                    {
                        return cached.summary.clone();
                    }
                }
                let summary = Arc::new(Self::wire_session_summary_from_record(
                    local_instance_id,
                    record,
                ));
                let queue_identities = if record.is_local_session() {
                    Some(
                        projected_pending_prompt_identities(record)
                            .map(|(id, interrupted, retained)| {
                                (id.to_owned(), interrupted, retained)
                            })
                            .collect(),
                    )
                } else {
                    None
                };
                cache.sessions.insert(
                    id.to_owned(),
                    CachedStateSessionSummary {
                        key,
                        summary: summary.clone(),
                        queue_identities,
                    },
                );
                summary
            })
            .collect();
        cache.sessions.retain(|id, _| live.contains(id.as_str()));
        sessions
    }

    pub(crate) fn warm_state_projection_cache(&self) {
        let mut cut = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            self.immutable_state_snapshot_locked(&inner)
        };
        // One-time startup cost precedes requests; the shared projections stay.
        cut.project_pending_sessions();
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

    pub(crate) fn shutdown_state_broadcast_blocking(&self) {
        if let Some(worker) = &self.state_broadcast_worker {
            worker.shutdown();
        }
    }
}

pub(crate) struct StateBroadcastWorker {
    pub(crate) mailbox: Arc<StateBroadcastMailbox>,
    pub(crate) handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl StateBroadcastWorker {
    pub(crate) fn start(mut state: AppState) -> Self {
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
                // Worker-local: closing keeps the base for late work.
                let mut diffs = StateDiffProducer::default();
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
                            diffs.forward(work, &state.state_broadcast_senders);
                        }
                    }
                }
                // Stopped: later commits publish full snapshots synchronously,
                // so the base no longer describes what clients will see.
                drop(diffs);
            })
            .expect("failed to spawn state broadcast thread");
        Self {
            mailbox,
            handle: Mutex::new(Some(handle)),
        }
    }

    pub(crate) fn shutdown(&self) {
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
    pub(crate) fn is_stopped(&self) -> bool {
        self.pending
            .lock()
            .expect("state broadcast mailbox mutex poisoned")
            .stopped
    }

    pub(crate) fn begin_shutdown(&self) {
        self.pending
            .lock()
            .expect("state broadcast mailbox mutex poisoned")
            .closing = true;
        self.work_available.notify_all();
    }
}
