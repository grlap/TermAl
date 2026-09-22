//! Durable queue disposition across settings reset, degradation and transcript eviction.
//! Exercises disposable host persistence and scripted control, never a live store.
use super::*;

struct BlockedAdmissionTransport {
    inner: Arc<ScriptedEngramControlTransport>,
    operation: &'static str,
    blocked_error: EngramTransportError,
    observed_tx: Mutex<Option<mpsc::Sender<Value>>>,
    release_rx: Mutex<mpsc::Receiver<()>>,
}

struct LostEvaluateReplyCachingTransport {
    inner: Arc<ScriptedEngramControlTransport>,
    cache: Mutex<std::collections::HashMap<String, Value>>,
    requests: Mutex<Vec<Value>>,
    first_evaluate: std::sync::atomic::AtomicBool,
    observed_tx: Mutex<Option<mpsc::Sender<Value>>>,
    release_rx: Mutex<mpsc::Receiver<()>>,
}

impl EngramControlTransport for LostEvaluateReplyCachingTransport {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        let encoded = serde_json::to_value(request).unwrap();
        self.requests.lock().unwrap().push(encoded.clone());
        let key = encoded["idempotency_key"].as_str().map(str::to_owned);
        if let Some(cached) = key
            .as_ref()
            .and_then(|key| self.cache.lock().unwrap().get(key).cloned())
        {
            return Ok(cached);
        }
        let response = self.inner.request(connection, request, timeout)?;
        if let Some(key) = key {
            self.cache.lock().unwrap().insert(key, response.clone());
        }
        if encoded["operation"] == "turn_evaluate"
            && self
                .first_evaluate
                .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            if let Some(observed) = self.observed_tx.lock().unwrap().take() {
                observed.send(encoded).unwrap();
            }
            self.release_rx.lock().unwrap().recv().unwrap();
            return Err(EngramTransportError::deadline(
                "evaluate reply was committed remotely but lost locally",
            ));
        }
        Ok(response)
    }

    fn read_work_binding(
        &self,
        connection: &EngramConnectionConfig,
        timeout: Duration,
    ) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.inner.read_work_binding(connection, timeout)
    }

    fn shutdown_session(&self, session_id: &str) {
        self.inner.shutdown_session(session_id);
    }
}

impl EngramControlTransport for BlockedAdmissionTransport {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        let encoded = serde_json::to_value(request).unwrap();
        let observer = if encoded["operation"] == self.operation {
            self.observed_tx.lock().unwrap().take()
        } else {
            None
        };
        if let Some(observer) = observer {
            self.inner
                .requests
                .lock()
                .unwrap()
                .push(RecordedEngramControlRequest {
                    connection: connection.clone(),
                    request: encoded.clone(),
                });
            observer.send(encoded).unwrap();
            self.release_rx.lock().unwrap().recv().unwrap();
            return Err(self.blocked_error.clone());
        }
        self.inner.request(connection, request, timeout)
    }

    fn read_work_binding(
        &self,
        connection: &EngramConnectionConfig,
        timeout: Duration,
    ) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.inner.read_work_binding(connection, timeout)
    }

    fn shutdown_session(&self, session_id: &str) {
        self.inner.shutdown_session(session_id);
    }
}

#[test]
fn cancel_supersedes_the_exact_explicit_resume_owner_and_all_old_wakes() {
    for blocked_operation in ["session_bind", "turn_evaluate"] {
        let replies = if blocked_operation == "session_bind" {
            vec![
                rebind_reply("successor-token"),
                grant_reply("successor-grant"),
                begin_reply("successor-grant"),
            ]
        } else {
            vec![
                bind_reply("original-token"),
                status_reply("sync_required"),
                rebind_reply("successor-token"),
                grant_reply("successor-grant"),
                begin_reply("successor-grant"),
            ]
        };
        let (state, session, receiver, _) = root_fixture([]);
        let transport = ScriptedEngramControlTransport::new(replies);
        let (observed_tx, observed_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        state.install_control_test_transport(Arc::new(BlockedAdmissionTransport {
            inner: transport.clone(),
            operation: blocked_operation,
            blocked_error: if blocked_operation == "session_bind" {
                EngramTransportError::remote(EngramControlErrorBody {
                    code: "stale_fence".to_owned(),
                    message: "blocked bind became stale".to_owned(),
                })
            } else {
                EngramTransportError::deadline("lost turn_evaluate reply")
            },
            observed_tx: Mutex::new(Some(observed_tx)),
            release_rx: Mutex::new(release_rx),
        }));
        queue_test_engram_prompt(
            &state,
            &session,
            "canceled explicit-resume owner",
            QueuedPromptSource::User,
            None,
        );
        queue_test_engram_prompt(
            &state,
            &session,
            "successor requires a fresh Resume",
            QueuedPromptSource::User,
            None,
        );
        let canceled_prompt = {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            record.set_auto_dispatch_blocked(true);
            let prompt = record.queued_prompts[0].pending_prompt.id.clone();
            state.commit_locked(&mut inner).unwrap();
            prompt
        };

        std::thread::scope(|scope| {
            let first_resume = scope.spawn(|| state.resume_session_queue(&session));
            phase_sync::receive(&observed_rx, "explicit Resume reaches blocked admission");

            // A competing explicit drain records a wake on the same owner.
            // It must not authorize the successor after cancellation either.
            state.resume_session_queue(&session).unwrap();
            state.cancel_queued_prompt(&session, &canceled_prompt).unwrap();
            release_tx.send(()).unwrap();
            first_resume.join().unwrap().unwrap();
        });

        assert!(receiver.try_recv().is_err());
        {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(record.orchestrator_auto_dispatch_blocked);
            assert_eq!(record.queued_prompts.len(), 1);
            assert_eq!(
                record.queued_prompts[0].pending_prompt.text,
                "successor requires a fresh Resume"
            );
            assert!(record.queued_prompts[0].engram_bind.is_none());
            assert!(record.queued_prompts[0].engram_evaluate.is_none());
        }
        assert_eq!(
            operations(&transport)
                .iter()
                .filter(|operation| operation.as_str() == blocked_operation)
                .count(),
            1,
            "the old owner and its competing wake must not authorize the successor"
        );

        state.resume_session_queue(&session).unwrap();
        let CodexRuntimeCommand::Prompt { command, .. } = receiver.try_recv().unwrap() else {
            panic!("a fresh explicit Resume should deliver the successor");
        };
        assert_eq!(command.prompt, "successor requires a fresh Resume");
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn stale_bind_reply_cannot_reprepare_an_exact_retained_successor() {
    let (state, session, receiver, _) = root_fixture([]);
    let transport = ScriptedEngramControlTransport::new([
        rebind_reply("successor-token"),
        grant_reply("successor-grant"),
        begin_reply("successor-grant"),
    ]);
    let (observed_tx, observed_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    state.install_control_test_transport(Arc::new(BlockedAdmissionTransport {
        inner: transport.clone(),
        operation: "session_bind",
        blocked_error: EngramTransportError::remote(EngramControlErrorBody {
            code: "stale_fence".to_owned(),
            message: "original bind became stale".to_owned(),
        }),
        observed_tx: Mutex::new(Some(observed_tx)),
        release_rx: Mutex::new(release_rx),
    }));
    queue_test_engram_prompt(
        &state,
        &session,
        "original owner",
        QueuedPromptSource::User,
        None,
    );
    queue_test_engram_prompt(
        &state,
        &session,
        "exact retained successor",
        QueuedPromptSource::User,
        None,
    );
    let original_id = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.set_auto_dispatch_blocked(true);
        let id = record.queued_prompts[0].pending_prompt.id.clone();
        state.commit_locked(&mut inner).unwrap();
        id
    };

    std::thread::scope(|scope| {
        let first_resume = scope.spawn(|| state.resume_session_queue(&session));
        let original_bind = phase_sync::receive(&observed_rx, "original blocked bind");
        state.cancel_queued_prompt(&session, &original_id).unwrap();
        let retained_bind = {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let target = AppState::engram_binding_target_for_session_shape_locked(
                &inner,
                &session,
                true,
            )
            .unwrap()
            .unwrap();
            let generation = inner.sessions[index]
                .engram
                .dispatch_generation
                .saturating_add(1);
            let request = EngramControlRequest::SessionBind {
                external_ref: target.external_ref,
                title: target.title,
                assurance: ENGRAM_CONTROL_ASSURANCE.to_owned(),
                mediated_effects: target.effects,
                capability_map_revision: ENGRAM_CAPABILITY_MAP_REVISION,
                work_binding: None,
                idempotency_key: "retained-successor-bind-key".to_owned(),
            };
            let record = inner.session_mut_by_index(index).unwrap();
            let queued = record.queued_prompts.front_mut().unwrap();
            queued.engram_bind = Some(EngramQueuedBind {
                connection: target.connection,
                settings: target.settings,
                request: request.clone(),
                operation_generation: Some(generation),
            });
            sync_pending_prompts(record);
            state.commit_locked(&mut inner).unwrap();
            serde_json::to_value(request).unwrap()
        };
        assert_ne!(original_bind, retained_bind);
        state.resume_session_queue(&session).unwrap();
        release_tx.send(()).unwrap();
        first_resume.join().unwrap().unwrap();

        assert!(receiver.try_recv().is_err());
        let before_fresh_resume = {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(record.orchestrator_auto_dispatch_blocked);
            assert_eq!(record.queued_prompts.len(), 1);
            assert!(record.queued_prompts[0].engram_evaluate.is_none());
            serde_json::to_value(
                &record.queued_prompts[0]
                    .engram_bind
                    .as_ref()
                    .unwrap()
                    .request,
            )
            .unwrap()
        };
        assert_eq!(before_fresh_resume, retained_bind);
        assert_eq!(
            operations(&transport),
            ["session_bind"],
            "the stale owner must not bind, evaluate, or begin the successor"
        );

        state.resume_session_queue(&session).unwrap();
        let CodexRuntimeCommand::Prompt { command, .. } = receiver.try_recv().unwrap() else {
            panic!("fresh Resume should deliver the exact retained successor");
        };
        assert_eq!(command.prompt, "exact retained successor");
        assert!(receiver.try_recv().is_err());
        let requests = transport.requests();
        assert_eq!(requests[1].request, retained_bind);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.request["operation"] == "session_bind")
                .count(),
            2,
            "one old bind and one fresh-Resume replay are the only bind calls"
        );
    });
}

#[test]
fn restart_replays_cached_defer_then_resume_uses_a_fresh_evaluate_identity() {
    let (state, session, receiver, _) = root_fixture([]);
    let scripted = ScriptedEngramControlTransport::new([
        bind_reply("restart-token"),
        defer_reply("cached-defer"),
        status_reply("ready"),
        status_reply("sync_required"),
        rebind_reply("fresh-token"),
        grant_reply("fresh-grant"),
        begin_reply("fresh-grant"),
    ]);
    let (observed_tx, observed_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let transport = Arc::new(LostEvaluateReplyCachingTransport {
        inner: scripted,
        cache: Mutex::new(std::collections::HashMap::new()),
        requests: Mutex::new(Vec::new()),
        first_evaluate: std::sync::atomic::AtomicBool::new(true),
        observed_tx: Mutex::new(Some(observed_tx)),
        release_rx: Mutex::new(release_rx),
    });
    state.install_control_test_transport(transport.clone());
    queue_test_engram_prompt(
        &state,
        &session,
        "restart before promotion then defer",
        QueuedPromptSource::User,
        None,
    );

    let worker_state = state.clone();
    let worker_session = session.clone();
    let worker = std::thread::spawn(move || {
        worker_state.start_next_queued_turn_off_lock(&worker_session, false, false)
    });
    let first_evaluate = phase_sync::receive(
        &observed_rx,
        "the first evaluate should be cached before its reply is lost",
    );
    let mut cache = SqlitePersistConnectionCache::new();
    let delta = collect_persist_delta_from_shared_state(&state.inner, 0);
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &delta,
        &mut PersistFenceBatch::default(),
    )
    .unwrap();
    let mut loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    {
        let index = loaded.find_session_index(&session).unwrap();
        let record = loaded.session_mut_by_index(index).unwrap();
        assert!(record.engram.recovered_admission);
        assert!(record.queued_prompts[0].engram_evaluate.is_some());
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(!record.engram_boot_recovery_pending);
        record.engram.context_nudge_pending = false;
    }
    release_tx.send(()).unwrap();
    worker.join().unwrap().unwrap();
    *state.inner.lock().unwrap() = loaded;
    state.install_control_test_transport(transport.clone());
    assert!(AppState::engram_session_requires_dispatch_card_locked(
        &state.inner.lock().unwrap(),
        &session,
    ));

    state.resume_session_queue(&session).unwrap();
    assert!(receiver.try_recv().is_err(), "cached Defer must not deliver");
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.session.queue_paused);
        assert!(record.queued_prompts[0].engram_waiting);
        assert!(record.queued_prompts[0].engram_evaluate.is_none());
    }

    state.resume_session_queue(&session).unwrap();
    let CodexRuntimeCommand::Prompt { command, .. } = receiver.try_recv().unwrap() else {
        panic!("the fresh post-Defer operation should deliver once");
    };
    assert_eq!(command.prompt, "restart before promotion then defer");
    assert!(receiver.try_recv().is_err());
    let evaluate_requests = transport
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request["operation"] == "turn_evaluate")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(evaluate_requests.len(), 3);
    assert_eq!(
        evaluate_requests[0]["idempotency_key"],
        first_evaluate["idempotency_key"]
    );
    assert_eq!(
        evaluate_requests[1]["idempotency_key"],
        first_evaluate["idempotency_key"],
        "restart must replay the exact remotely cached operation"
    );
    assert_ne!(
        evaluate_requests[2]["idempotency_key"],
        first_evaluate["idempotency_key"],
        "terminal Defer retirement must give the later Resume a fresh identity"
    );
}

#[test]
fn writer_backed_first_defer_commit_is_already_a_visible_held_operation() {
    let (state, session, receiver, transport) = root_fixture([
        bind_reply("defer-token"),
        defer_reply("busy"),
        grant_reply("resumed-grant"),
        begin_reply("resumed-grant"),
    ]);
    queue_test_engram_prompt(
        &state,
        &session,
        "held Orchestrator root prompt",
        QueuedPromptSource::Orchestrator,
        None,
    );
    let revision_before_admission = state.snapshot().revision;
    let mut admission_snapshots = state.subscribe_events();
    let mut admission_deltas = state.subscribe_delta_events();
    let dispatch = state
        .start_next_queued_turn_off_lock(&session, false, false)
        .unwrap()
        .expect("Orchestrator root prompt should reach admission")
        .dispatch;
    let revision_after_admission = state.snapshot().revision;
    assert_eq!(
        revision_after_admission,
        revision_before_admission + 4,
        "bind retention, accepted-bind retirement, evaluate retention, and promotion are four visible transitions"
    );
    let retained_revisions = std::iter::from_fn(|| admission_snapshots.try_recv().ok())
        .filter_map(|payload| serde_json::from_str::<Value>(&payload).ok())
        .map(|payload| payload["revision"].as_u64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        retained_revisions,
        vec![
            revision_before_admission + 1,
            revision_before_admission + 2,
            revision_before_admission + 3,
        ],
        "each public retained projection transition must emit a newer metadata-first revision"
    );
    let started: Value = serde_json::from_str(
        &admission_deltas
            .try_recv()
            .expect("queue promotion should publish its one client revision"),
    )
    .unwrap();
    assert_eq!(started["revision"], revision_after_admission);
    assert!(admission_deltas.try_recv().is_err());
    let first_generation = dispatch.engram_dispatch_generation().unwrap();
    let prompt_id = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session).unwrap()].queued_prompts[0]
            .pending_prompt
            .id
            .clone()
    };
    let (persist_tx, persist_rx) = mpsc::channel();
    let state = AppState {
        persist_tx,
        ..state
    };
    let pre_defer = state.get_session(&session).unwrap();
    let pre_defer_revision = state.snapshot().revision;
    assert_eq!(pre_defer.session.status, SessionStatus::Active);
    assert!(!pre_defer.session.queue_paused);
    let mut delta_events = state.subscribe_delta_events();
    let mut cache = SqlitePersistConnectionCache::new();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| deliver_turn_dispatch(&state, dispatch));
        assert!(matches!(
            persist_rx
                .recv_timeout(phase_sync::DEADLOCK_GUARD)
                .expect("first Defer commit should reach the real writer boundary"),
            PersistRequest::Delta
        ));
        worker.join().unwrap().unwrap();
        let emitted: Value = serde_json::from_str(
            &delta_events
                .try_recv()
                .expect("the committed Defer transition should emit one transcript delta"),
        )
        .unwrap();
        assert_eq!(emitted["type"], "messageCreated");
        assert_eq!(emitted["sessionId"], session);
        assert_eq!(emitted["status"], "idle");
        assert_eq!(emitted["message"]["type"], "engramControl");
        assert_eq!(emitted["message"]["decision"], "defer");
        assert_eq!(emitted["revision"], pre_defer_revision + 1);
        assert_eq!(emitted["sessionQueue"]["queuePaused"], true);
        assert!(emitted["sessionQueue"]["queueProjectionHash"].is_string());
        assert_eq!(emitted["sessionQueue"]["pendingPrompts"][0]["id"], prompt_id);
        assert_eq!(
            emitted["sessionQueue"]["pendingPrompts"][0]["isEngramRetained"],
            true
        );
        assert!(
            emitted["sessionQueue"]["pendingPrompts"][0]["engramInterrupted"].is_null(),
            "false interrupted disposition is omitted on the wire"
        );
        assert!(
            persist_rx.try_recv().is_err(),
            "legacy parking must not write a second durable Defer transition"
        );
        let delta = collect_persist_delta_from_shared_state(&state.inner, 0);
        let committed = delta
            .changed_sessions
            .iter()
            .find(|record| record.session.id == session)
            .expect("Defer transition should include its session");
        assert_eq!(committed.session.status, SessionStatus::Idle);
        assert!(committed.orchestrator_auto_dispatch_blocked);
        assert!(committed.session.queue_paused);
        assert_eq!(committed.engram_dispatch_generation, first_generation + 1);
        assert!(committed.queued_prompts[0].engram_waiting);
        assert!(committed.queued_prompts[0].engram_evaluate.is_none());
        let (committed_revision, committed_mutation_stamp) = {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            (inner.revision, record.mutation_stamp)
        };
        assert_eq!(emitted["revision"], committed_revision);
        assert_eq!(
            emitted["sessionMutationStamp"],
            committed_mutation_stamp
        );
        persist_delta_with_fences(
            &mut cache,
            state.persistence_path.as_path(),
            &delta,
            &mut PersistFenceBatch::default(),
        )
        .unwrap();
    });
    assert!(receiver.try_recv().is_err());

    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let durable = &loaded.sessions[loaded.find_session_index(&session).unwrap()];
    assert_eq!(durable.session.status, SessionStatus::Idle);
    assert!(durable.orchestrator_auto_dispatch_blocked);
    assert!(durable.session.queue_paused);
    assert_eq!(durable.engram.dispatch_generation, first_generation + 1);
    assert!(durable.queued_prompts[0].engram_waiting);
    assert!(durable.queued_prompts[0].engram_evaluate.is_none());
    assert_eq!(
        durable
            .session
            .messages
            .iter()
            .filter(|message| message.id() == prompt_id)
            .count(),
        1,
        "the first Defer boundary retains one promoted transcript entry"
    );

    drop(persist_rx);
    state.resume_session_queue(&session).unwrap();
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    assert!(receiver.try_recv().is_err());
    let evaluations = transport
        .requests()
        .into_iter()
        .filter(|request| request.request["operation"] == "turn_evaluate")
        .map(|request| request.request)
        .collect::<Vec<_>>();
    assert_eq!(evaluations.len(), 2);
    assert_eq!(evaluations[0]["intent_fingerprint"], evaluations[1]["intent_fingerprint"]);
    assert_ne!(evaluations[0]["idempotency_key"], evaluations[1]["idempotency_key"]);
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(record.queued_prompts.is_empty());
    assert_eq!(
        record
            .session
            .messages
            .iter()
            .filter(|message| message.id() == prompt_id)
            .count(),
        1
    );
}

#[test]
fn failed_legacy_unknown_park_is_published_and_never_falls_through() {
    let (mut state, session, _receiver, _) = root_fixture([]);
    let (runtime, provider_receiver) = test_codex_runtime_handle("legacy-park-failure");
    queue_test_engram_prompt(
        &state,
        &session,
        "legacy held head",
        QueuedPromptSource::User,
        None,
    );
    queue_test_engram_prompt(
        &state,
        &session,
        "untouched successor",
        QueuedPromptSource::User,
        None,
    );
    let (generation, runtime_token, active_turn_generation) = {
        let mut inner = state.inner.lock().unwrap();
        let message_id = inner.next_message_id();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.runtime = SessionRuntime::Codex(runtime);
        record.session.status = SessionStatus::Active;
        push_message_on_record(
            record,
            Message::EngramControl {
                id: message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                card: EngramControlCard {
                    schema_version: ENGRAM_CONTROL_SCHEMA_VERSION,
                    stage: EngramControlStage::Dispatch,
                    assurance: ENGRAM_CONTROL_ASSURANCE.to_owned(),
                    decision: EngramControlCardDecision::Defer,
                    dispatch: EngramControlCardDispatch::Withheld,
                    refusal_code: None,
                    defer_code: Some("legacy-busy".to_owned()),
                    grant_id: None,
                    directives: Vec::new(),
                    delivered_range: None,
                    latency_ms: EngramControlLatencyCard {
                        evaluate: Some(0),
                        begin: None,
                        checkpoint: None,
                        total: 0,
                    },
                    fail_mode: EngramControlFailMode::Enforced,
                    repair_armed: false,
                    next_intent: None,
                },
            },
        );
        (
            record.engram.dispatch_generation,
            record.runtime.runtime_token().unwrap(),
            record.active_turn_generation,
        )
    };
    let durable_path = state.persistence_path.clone();
    {
        let inner = state.inner.lock().unwrap();
        persist_state_from_persisted(
            durable_path.as_path(),
            &PersistedState::from_inner(&inner),
        )
        .unwrap();
    }
    state.shutdown_persist_blocking();
    let failing_path = durable_path.with_extension("park-is-directory");
    fs::create_dir_all(&failing_path).unwrap();
    state.persistence_path = Arc::new(failing_path.clone());
    let mut events = state.subscribe_events();

    assert_eq!(
        state.park_unknown_engram_authorization(
            &session,
            generation,
            &runtime_token,
            active_turn_generation,
        ),
        EngramAuthorizationParkOutcome::PersistenceUnknown
    );
    let payload = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(phase_sync::DEADLOCK_GUARD, events.recv())
                .await
                .expect("park uncertainty publication timed out")
                .unwrap()
        });
    let published: Value = serde_json::from_str(&payload).unwrap();
    let published_session = published["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["id"] == session)
        .unwrap();
    assert_eq!(published_session["status"], "idle");
    assert_eq!(published_session["queuePaused"], true);
    assert!(published_session["preview"]
        .as_str()
        .unwrap()
        .contains("Waiting/Unknown"));
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.queued_prompts[0].engram_waiting);
        assert!(!record.queued_prompts[1].engram_waiting);
        assert!(!record.queued_prompts[1].engram_interrupted);
    }
    assert!(provider_receiver.try_recv().is_err());

    let durable = load_state(durable_path.as_path()).unwrap().unwrap();
    let durable_record = &durable.sessions[durable.find_session_index(&session).unwrap()];
    assert_eq!(
        durable_record.session.status,
        SessionStatus::Error,
        "boot recovery sees the prior durable Active record, not the failed Idle park"
    );
    assert!(!durable_record.session.queue_paused);
    assert!(!durable_record.queued_prompts[0].engram_waiting);
    assert!(!durable_record.queued_prompts[1].engram_waiting);

    fs::remove_dir_all(failing_path).unwrap();
}

#[test]
fn failed_defer_card_commit_keeps_the_durable_evaluation_recovery_anchor() {
    let (mut state, session, receiver, _) = root_fixture([]);
    queue_test_engram_prompt(
        &state,
        &session,
        "defer whose card persistence is ambiguous",
        QueuedPromptSource::User,
        None,
    );
    let target = {
        let inner = state.inner.lock().unwrap();
        AppState::engram_binding_target_for_session_shape_locked(&inner, &session, true)
            .unwrap()
            .unwrap()
    };
    let generation = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        let queued = record.queued_prompts.front_mut().unwrap();
        let fingerprint = engram_turn_intent_fingerprint(
            &queued.pending_prompt.text,
            queued.pending_prompt.expanded_text.as_deref(),
            &queued.attachments,
            queued.pending_prompt.source.as_ref(),
            queued.source,
        );
        queued.engram_evaluate = Some(EngramQueuedEvaluate {
            connection: target.connection,
            settings: target.settings,
            operation_generation: None,
            request: EngramControlRequest::TurnEvaluate {
                routing_token: "defer-persist-token".to_owned(),
                intent_fingerprint: fingerprint.clone(),
                requested_effects: target.effects,
                resource_intents: Vec::new(),
                purpose: "ordinary".to_owned(),
                idempotency_key: "defer-persist-old-key".to_owned(),
            },
            begun_grant_id: None,
        });
        record.engram.routing_token = Some("defer-persist-token".to_owned());
        record.session.status = SessionStatus::Active;
        record.engram.pending_dispatch = Some(EngramPendingDispatch {
            dispatch_generation: record.engram.dispatch_generation,
            intent_fingerprint: fingerprint,
            evaluated: EngramDispatchEvaluation::Defer {
                code: "busy".to_owned(),
                retry_after_ms: None,
                wake_condition: "operator resume".to_owned(),
            },
            evaluate_latency_ms: 0,
            started_at: std::time::Instant::now(),
            awaiting_runtime_stop_resolution: false,
        });
        record.engram.dispatch_generation
    };
    let durable_path = state.persistence_path.clone();
    {
        let inner = state.inner.lock().unwrap();
        persist_state_from_persisted(
            durable_path.as_path(),
            &PersistedState::from_inner(&inner),
        )
        .unwrap();
    }
    state.shutdown_persist_blocking();
    let failing_path = durable_path.with_extension("defer-card-is-directory");
    fs::create_dir_all(&failing_path).unwrap();
    state.persistence_path = Arc::new(failing_path.clone());
    let mut events = state.subscribe_events();

    assert_eq!(
        state.finish_engram_dispatch_record(
            &session,
            generation,
            None,
            EngramControlCard {
                schema_version: ENGRAM_CONTROL_SCHEMA_VERSION,
                stage: EngramControlStage::Dispatch,
                assurance: ENGRAM_CONTROL_ASSURANCE.to_owned(),
                decision: EngramControlCardDecision::Defer,
                dispatch: EngramControlCardDispatch::Withheld,
                refusal_code: None,
                defer_code: Some("busy".to_owned()),
                grant_id: None,
                directives: Vec::new(),
                delivered_range: None,
                latency_ms: EngramControlLatencyCard {
                    evaluate: Some(0),
                    begin: None,
                    checkpoint: None,
                    total: 0,
                },
                fail_mode: EngramControlFailMode::Enforced,
                repair_armed: false,
                next_intent: None,
            },
        ),
        EngramDispatchRecordFinish::PersistenceUnknown
    );
    let payload = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(phase_sync::DEADLOCK_GUARD, events.recv())
                .await
                .expect("Defer uncertainty publication timed out")
                .unwrap()
        });
    let published: Value = serde_json::from_str(&payload).unwrap();
    let published_session = published["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["id"] == session)
        .unwrap();
    assert_eq!(published_session["status"], "idle");
    assert_eq!(published_session["queuePaused"], true);
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.engram.dispatch_generation, generation);
        assert!(record.queued_prompts[0].engram_interrupted);
        assert!(record.queued_prompts[0].engram_waiting);
        assert!(record.queued_prompts[0]
            .engram_evaluate
            .as_ref()
            .is_some_and(|prepared| matches!(
                &prepared.request,
                EngramControlRequest::TurnEvaluate { idempotency_key, .. }
                    if idempotency_key == "defer-persist-old-key"
            )));
    }
    assert!(receiver.try_recv().is_err());

    let mut loaded = load_state(durable_path.as_path()).unwrap().unwrap();
    let loaded_record = &loaded.sessions[loaded.find_session_index(&session).unwrap()];
    assert_eq!(loaded_record.engram.dispatch_generation, generation);
    assert!(loaded_record.engram.recovered_admission);
    assert!(loaded_record.queued_prompts[0]
        .engram_evaluate
        .as_ref()
        .is_some_and(|prepared| matches!(
            &prepared.request,
            EngramControlRequest::TurnEvaluate { idempotency_key, .. }
                if idempotency_key == "defer-persist-old-key"
        )));

    fs::remove_dir_all(failing_path).unwrap();
    state.persistence_path = durable_path;
    let recovered = ScriptedEngramControlTransport::new([
        status_reply("ready"),
        grant_reply("defer-recovered-grant"),
        begin_reply("defer-recovered-grant"),
    ]);
    state.install_control_test_transport(recovered.clone());
    let (runtime, recovered_receiver) = test_codex_runtime_handle("defer-persist-recovery");
    {
        let loaded_index = loaded.find_session_index(&session).unwrap();
        let mut loaded_record = loaded.sessions.swap_remove(loaded_index);
        loaded_record.runtime = SessionRuntime::Codex(runtime);
        loaded_record.session.status = SessionStatus::Idle;
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index] = loaded_record;
    }
    state
        .resume_session_queue(&session)
        .expect("public Resume should recover the last durable evaluate key");
    assert!(matches!(
        recovered_receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    assert!(recovered_receiver.try_recv().is_err());
    let requests = recovered.requests();
    assert_eq!(
        requests
            .iter()
            .map(|request| request.request["operation"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["session_status", "turn_evaluate", "turn_begin"]
    );
    assert_eq!(
        requests[1].request["idempotency_key"],
        "defer-persist-old-key"
    );
}

#[test]
fn public_disable_after_restoring_begun_receipt_never_replays_prompt() {
    for disable_host in [false, true] {
        let (state, session, receiver, transport) = root_fixture([
            bind_reply("token"),
            grant_reply("grant"),
            begin_reply("grant"),
            checkpoint_reply("grant"),
        ]);
        {
            let mut inner = state.inner.lock().unwrap();
            let project = inner.sessions[inner.find_session_index(&session).unwrap()]
                .session
                .project_id
                .clone()
                .unwrap();
            fs::write(
                FsPath::new(&inner.find_project(&project).unwrap().root_path)
                    .join(".engram-project"),
                "fixture-doctor-advisory\n",
            )
            .unwrap();
            // The public API also invokes readiness. Use its existing fixture
            // executable while control traffic remains scripted.
            inner
                .projects
                .iter_mut()
                .find(|p| p.id == project)
                .unwrap()
                .engram
                .as_mut()
                .unwrap()
                .binary_path = Some(
                real_engram_control_fixture_path()
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        let dispatch = root_dispatch(&state, &session, false);
        assert!(matches!(
            state.prepare_engram_turn_delivery_off_lock(
                &session,
                dispatch.engram_dispatch_generation().unwrap()
            ),
            EngramTurnDeliveryPreparation::Ready
        ));
        let (project, mut settings, prompt) = {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let saved: PersistedSessionRecord =
                serde_json::from_str(&persisted_session_json(&state, &session)).unwrap();
            assert_eq!(
                saved.queued_prompts[0]
                    .engram_evaluate
                    .as_ref()
                    .unwrap()
                    .begun_grant_id
                    .as_deref(),
                Some("grant")
            );
            inner.sessions[index] = saved.into_record().unwrap();
            inner.recover_interrupted_sessions();
            let record = &inner.sessions[index];
            assert!(record.engram.recovered_admission);
            let project = record.session.project_id.clone().unwrap();
            (
                project.clone(),
                inner
                    .find_project(&project)
                    .unwrap()
                    .engram
                    .clone()
                    .unwrap(),
                record.queued_prompts[0].pending_prompt.id.clone(),
            )
        };
        if disable_host {
            settings.enabled = false;
        } else {
            settings.turn_gated_control = false;
        }
        state
            .update_project_engram_settings(&project, settings)
            .unwrap();
        state.resume_session_queue(&session).unwrap();
        assert!(matches!(
            state
                .dispatch_turn(
                    &session,
                    SendMessageRequest {
                        text: "successor after disable".into(),
                        expanded_text: None,
                        attachments: vec![],
                        source_session_id: None,
                        source_mailbox: None,
                    }
                )
                .unwrap(),
            DispatchTurnResult::Queued
        ));
        {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(record.session.queue_paused);
            assert!(record.session.pending_prompts[0].engram_interrupted);
            assert_eq!(record.queued_prompts[0].pending_prompt.id, prompt);
            assert_eq!(record.queued_prompts.len(), 2);
            assert_eq!(
                record.queued_prompts[1].pending_prompt.text,
                "successor after disable"
            );
            assert_eq!(
                record.queued_prompts[0]
                    .engram_evaluate
                    .as_ref()
                    .unwrap()
                    .begun_grant_id
                    .as_deref(),
                Some("grant")
            );
        }
        assert!(receiver.try_recv().is_err());
        assert_eq!(
            operations(&transport)
                .iter()
                .filter(|op| *op == "turn_evaluate")
                .count(),
            1
        );
        state.cancel_queued_prompt(&session, &prompt).unwrap();
        state.resume_session_queue(&session).unwrap();
        let CodexRuntimeCommand::Prompt { command, .. } = receiver.try_recv().unwrap() else {
            panic!("successor should reach provider");
        };
        assert!(command.prompt.contains("successor after disable"));
        assert!(!command.prompt.contains("Root controlled turn"));
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn degraded_unknown_and_terminal_store_faults_remain_cancelable_without_retry() {
    for child in [false, true] {
        for code in [
            "authorization_unknown",
            "control_binding_unavailable",
            "unknown_control_schema",
            "store_corrupt",
            "future_unknown_code",
        ] {
            let mut replies = Vec::new();
            if child {
                replies.push(bind_reply("parent"));
            }
            replies.push(bind_reply("token"));
            let mut error = EngramTransportError::transport("injected uncertain authorization");
            error.code = Some(code.into());
            replies.push(ScriptedEngramControlResponse::Reply(Err(error)));
            let (state, parent, receiver, transport) = root_fixture(replies);
            let (session, delegation) = if child {
                let created = state
                    .create_read_only_delegation(
                        &parent,
                        CreateDelegationRequest {
                            prompt: "held child".into(),
                            title: None,
                            cwd: None,
                            agent: Some(Agent::Codex),
                            model: None,
                            mode: Some(DelegationMode::Explorer),
                            write_policy: Some(DelegationWritePolicy::ReadOnly),
                        },
                    )
                    .unwrap();
                (
                    created.delegation.child_session_id,
                    Some(created.delegation.id),
                )
            } else {
                let dispatch = root_dispatch(&state, &parent, false);
                deliver_turn_dispatch(&state, dispatch).unwrap();
                (parent.clone(), None)
            };
            let (prompt, prepared) = {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                assert_eq!(record.session.status, SessionStatus::Idle, "{code}");
                assert!(record.session.queue_paused);
                assert!(record.session.pending_prompts[0].engram_interrupted);
                (
                    record.queued_prompts[0].pending_prompt.id.clone(),
                    serde_json::to_value(&record.queued_prompts[0]).unwrap(),
                )
            };
            let calls = transport.requests().len();
            state.resume_session_queue(&session).unwrap();
            if child {
                state
                    .refresh_delegation_for_child_session(&session)
                    .unwrap();
            }
            {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                assert!(record.session.queue_paused);
                assert_eq!(
                    serde_json::to_value(&record.queued_prompts[0]).unwrap(),
                    prepared
                );
                if let Some(id) = delegation {
                    assert_eq!(
                        inner.delegations[inner.find_delegation_index(&id).unwrap()].status,
                        DelegationStatus::Running
                    );
                }
            }
            assert_eq!(transport.requests().len(), calls);
            assert!(receiver.try_recv().is_err());
            state.cancel_queued_prompt(&session, &prompt).unwrap();
            assert!(
                state
                    .inner
                    .lock()
                    .unwrap()
                    .sessions
                    .iter()
                    .find(|r| r.session.id == session)
                    .unwrap()
                    .queued_prompts
                    .is_empty()
            );
        }
    }
}

#[test]
fn definite_refusal_retires_head_instead_of_hiding_it() {
    for with_successor in [false, true] {
        let (state, session, receiver, _) = root_fixture([
            bind_reply("token"),
            evaluation_refusal_reply("policy_denied"),
        ]);
        let mut deltas = state.subscribe_delta_events();
        let dispatch = root_dispatch(&state, &session, false);
        if with_successor {
            queue_test_engram_prompt(
                &state,
                &session,
                "successor remains queued",
                QueuedPromptSource::User,
                None,
            );
        }
        assert!(deliver_turn_dispatch(&state, dispatch).is_err());
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.queued_prompts.len(), usize::from(with_successor));
        assert_eq!(
            record.session.pending_prompts.len(),
            usize::from(with_successor)
        );
        if with_successor {
            assert_eq!(
                record.queued_prompts[0].pending_prompt.text,
                "successor remains queued"
            );
        }
        assert!(receiver.try_recv().is_err());
        drop(inner);
        let refusal_delta = std::iter::from_fn(|| deltas.try_recv().ok())
            .filter_map(|payload| serde_json::from_str::<Value>(&payload).ok())
            .filter(|event| event["type"] == "messageCreated")
            .find(|event| event.get("sessionQueue").is_some())
            .expect("terminal refusal must carry the authoritative queue projection");
        let projected = refusal_delta["sessionQueue"]["pendingPrompts"]
            .as_array()
            .unwrap();
        assert_eq!(projected.len(), usize::from(with_successor));
        if with_successor {
            assert_eq!(projected[0]["text"], "successor remains queued");
        }
        assert_eq!(refusal_delta["sessionQueue"]["queuePaused"], false);
        assert!(
            refusal_delta["sessionQueue"]["queueProjectionHash"].is_string()
        );
        assert_eq!(
            state
                .get_session(&session)
                .unwrap()
                .session
                .pending_prompts
                .len(),
            usize::from(with_successor),
            "last-head retirement and successor preservation must match targeted hydration"
        );
    }
}

#[test]
fn writer_backed_trimmed_promotion_retries_without_reinserting_transcript_or_history() {
    let (state, session, receiver, transport) = root_fixture([
        bind_reply("token"),
        defer_reply("busy"),
        grant_reply("grant"),
        begin_reply("grant"),
    ]);
    let dispatch = root_dispatch(&state, &session, false);
    deliver_turn_dispatch(&state, dispatch).unwrap();
    let (prompt, position, history) = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        let prompt = record.queued_prompts[0].pending_prompt.id.clone();
        let position = record.queued_prompts[0].promoted_message_index.unwrap();
        let history = record.session.prompt_history.clone();
        for sequence in 0..80 {
            push_message_on_record(
                record,
                Message::Text {
                    id: format!("trim-filler-{sequence}"),
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: "unrelated retained history".into(),
                    attachments: vec![],
                    expanded_text: None,
                    source: None,
                },
            );
        }
        (prompt, position, history)
    };
    let mut cache = SqlitePersistConnectionCache::new();
    let delta = collect_persist_delta_from_shared_state(&state.inner, 0);
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &delta,
        &mut PersistFenceBatch::default(),
    )
    .unwrap();
    let before = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        trim_retained_session_messages(record, SESSION_IN_MEMORY_MESSAGE_LIMIT);
        assert!(cached_message_index_on_record(record, &prompt).is_none());
        let roundtrip: QueuedPromptRecord =
            serde_json::from_value(serde_json::to_value(&record.queued_prompts[0]).unwrap())
                .unwrap();
        record.queued_prompts[0] = roundtrip;
        assert_eq!(
            record.queued_prompts[0].promoted_message_index,
            Some(position)
        );
        assert!(EngramQueuedAdmissionOwner::capture_promoted(record).is_some());
        session_message_count(record)
    };
    state.resume_session_queue(&session).unwrap();
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    assert!(receiver.try_recv().is_err());
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(
            session_message_count(record),
            before + 1,
            "only the new control card is appended"
        );
        assert_eq!(record.session.prompt_history, history);
        assert!(cached_message_index_on_record(record, &prompt).is_none());
    }
    let delta = collect_persist_delta_from_shared_state(&state.inner, 0);
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &delta,
        &mut PersistFenceBatch::default(),
    )
    .unwrap();
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    let (count, persisted_position): (usize, usize) = connection.query_row(
        "SELECT COUNT(*), MIN(position) FROM messages WHERE session_id = ?1 AND message_id = ?2",
        rusqlite::params![session, prompt], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!((count, persisted_position), (1, position));
    assert_eq!(
        operations(&transport),
        [
            "session_bind",
            "turn_evaluate",
            "turn_evaluate",
            "turn_begin"
        ]
    );
}

#[test]
fn writer_backed_trimmed_unpromoted_admission_replays_after_restart() {
    for lost_operation in ["session_bind", "turn_evaluate"] {
        let replies = if lost_operation == "session_bind" {
            vec![
                bind_reply("token"),
                grant_reply("grant"),
                begin_reply("grant"),
            ]
        } else {
            vec![
                bind_reply("token"),
                status_reply("ready"),
                grant_reply("grant"),
                begin_reply("grant"),
            ]
        };
        let (state, session, receiver, _) = root_fixture([]);
        let transport = ScriptedEngramControlTransport::new(replies);
        let (observed_tx, observed_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let blocking_transport = Arc::new(BlockedAdmissionTransport {
            inner: transport.clone(),
            operation: lost_operation,
            blocked_error: EngramTransportError::deadline(format!(
                "lost {lost_operation} reply"
            )),
            observed_tx: Mutex::new(Some(observed_tx)),
            release_rx: Mutex::new(release_rx),
        });
        state.install_control_test_transport(blocking_transport.clone());
        let prompt_id = {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            for sequence in 0..80 {
                push_message_on_record(
                    record,
                    Message::Text {
                        id: format!("unpromoted-trim-filler-{sequence}"),
                        timestamp: stamp_now(),
                        author: Author::Assistant,
                        text: "unrelated retained history".into(),
                        attachments: vec![],
                        expanded_text: None,
                        source: None,
                    },
                );
            }
            drop(inner);
            queue_test_engram_prompt(
                &state,
                &session,
                "exact unpromoted restart prompt",
                QueuedPromptSource::User,
                None,
            );
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            record.queued_prompts[0].pending_prompt.id.clone()
        };
        let (project_id, workdir) = {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            (
                record.session.project_id.clone().unwrap(),
                PathBuf::from(&record.session.workdir),
            )
        };

        let worker_state = state.clone();
        let worker_session = session.clone();
        let worker = std::thread::spawn(move || {
            worker_state.start_next_queued_turn_off_lock(&worker_session, false, false)
        });
        let issued = phase_sync::receive(
            &observed_rx,
            "prepared unpromoted admission reaches the wire",
        );
        assert!(receiver.try_recv().is_err());
        let fresh = state.get_session(&session).unwrap().session;
        assert_eq!(fresh.pending_prompts[0].id, prompt_id);
        assert!(
            fresh.pending_prompts[0].is_engram_retained,
            "a fresh client must see retained admission while the wire request is in flight"
        );
        create_test_project_session(&state, Agent::Claude, &project_id, &workdir);
        let after_unrelated_commit = state.get_session(&session).unwrap().session;
        assert_eq!(after_unrelated_commit.pending_prompts[0].id, prompt_id);
        assert!(
            after_unrelated_commit.pending_prompts[0].is_engram_retained,
            "an unrelated publication must not regress the prepared queue projection"
        );
        {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            let queued = &record.queued_prompts[0];
            assert_eq!(queued.promoted_message_index, None);
            assert!(queued.promotion_disposition_known);
            assert!(!queued.engram_interrupted);
            assert!(cached_message_index_on_record(record, &prompt_id).is_none());
        }

        let mut cache = SqlitePersistConnectionCache::new();
        let delta = collect_persist_delta_from_shared_state(&state.inner, 0);
        persist_delta_with_fences(
            &mut cache,
            state.persistence_path.as_path(),
            &delta,
            &mut PersistFenceBatch::default(),
        )
        .unwrap();
        let mut loaded = load_state(state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        {
            let index = loaded.find_session_index(&session).unwrap();
            let record = loaded.session_mut_by_index(index).unwrap();
            assert!(
                record.message_start_index > 0,
                "writer must trim the resident transcript"
            );
            assert!(cached_message_index_on_record(record, &prompt_id).is_none());
            assert_eq!(record.queued_prompts[0].promoted_message_index, None);
            assert!(record.queued_prompts[0].promotion_disposition_known);
            assert!(record.queued_prompts[0].has_engram_intent());
            assert!(!record.queued_prompts[0].engram_interrupted);
            assert!(record.engram.recovered_admission);
            record.engram.context_nudge_pending = false;
        }
        assert!(AppState::engram_session_requires_dispatch_card_locked(
            &loaded, &session
        ));
        {
            let record = &loaded.sessions[loaded.find_session_index(&session).unwrap()];
            assert!(record.orchestrator_auto_dispatch_blocked);
            assert!(record.session.queue_paused);
            assert_eq!(record.queued_prompts[0].pending_prompt.id, prompt_id);
        }
        release_tx.send(()).unwrap();
        worker.join().unwrap().unwrap();
        *state.inner.lock().unwrap() = loaded;
        state.install_control_test_transport(blocking_transport);

        state.resume_session_queue(&session).unwrap();
        let CodexRuntimeCommand::Prompt { command, .. } = receiver.try_recv().unwrap() else {
            panic!("recovered unpromoted admission should reach the provider once");
        };
        assert_eq!(command.prompt, "exact unpromoted restart prompt");
        assert!(receiver.try_recv().is_err());
        let retried = transport
            .requests()
            .into_iter()
            .filter(|request| request.request["operation"] == lost_operation)
            .map(|request| request.request)
            .collect::<Vec<_>>();
        assert_eq!(retried.len(), 2);
        assert_eq!(retried[0], issued);
        assert_eq!(
            retried[1], issued,
            "restart must replay the exact request and key"
        );
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(
            record
                .session
                .messages
                .iter()
                .filter(|message| message.id() == prompt_id)
                .count(),
            1,
            "promotion after restart must insert one transcript entry"
        );
    }
}

#[test]
fn promotion_persist_failure_preserves_exact_prepared_engram_intent_for_public_resume() {
    for lost_operation in ["session_bind", "turn_evaluate"] {
        let replies = if lost_operation == "session_bind" {
            vec![
                ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline(
                    "lost bind reply",
                ))),
                bind_reply("replayed-token"),
                grant_reply("replayed-grant"),
                begin_reply("replayed-grant"),
            ]
        } else {
            vec![
                bind_reply("initial-token"),
                ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline(
                    "lost evaluate reply",
                ))),
                status_reply("ready"),
                grant_reply("replayed-grant"),
                begin_reply("replayed-grant"),
            ]
        };
        let (state, session, receiver, transport) = root_fixture(replies);
        let attachment_metadata = MessageImageAttachment {
            byte_size: 1,
            file_name: "exact.png".to_owned(),
            media_type: "image/png".to_owned(),
        };
        let source = MessageSource::peer("peer-session".to_owned(), "Peer agent".to_owned());
        let (prompt_id, successor_id, baseline_history, baseline_message_count, baseline_generation) = {
            let mut inner = state.inner.lock().unwrap();
            let prompt_id = inner.next_message_id();
            let successor_id = inner.next_message_id();
            let index = inner.find_session_index(&session).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            queue_prompt_on_record_with_source(
                record,
                PendingPrompt {
                    engram_interrupted: false,
                    is_engram_retained: false,
                    attachments: vec![attachment_metadata.clone()],
                    id: prompt_id.clone(),
                    timestamp: stamp_now(),
                    text: "exact prepared rollback prompt".to_owned(),
                    expanded_text: Some("expanded exact prepared rollback prompt".to_owned()),
                    source: Some(source.clone()),
                },
                vec![PromptImageAttachment {
                    data: "AA==".to_owned(),
                    metadata: attachment_metadata,
                }],
                QueuedPromptSource::Orchestrator,
            );
            queue_prompt_on_record_with_source(
                record,
                PendingPrompt {
                    engram_interrupted: false,
                    is_engram_retained: false,
                    attachments: Vec::new(),
                    id: successor_id.clone(),
                    timestamp: stamp_now(),
                    text: "successor remains queued".to_owned(),
                    expanded_text: None,
                    source: None,
                },
                Vec::new(),
                QueuedPromptSource::Orchestrator,
            );
            let baseline = (
                prompt_id,
                successor_id,
                record.session.prompt_history.clone(),
                record.session.message_count,
                record.active_turn_generation,
            );
            state.commit_locked(&mut inner).unwrap();
            baseline
        };

        let connection = rusqlite::Connection::open(&*state.persistence_path).unwrap();
        connection
            .execute_batch(&format!(
                "CREATE TRIGGER reject_engram_promotion BEFORE INSERT ON messages
                 WHEN NEW.message_id = '{}'
                 BEGIN SELECT RAISE(ABORT, 'injected-promotion-commit-failure'); END;",
                prompt_id.replace('\'', "''"),
            ))
            .unwrap();

        let mut state_events = state.subscribe_events();
        let error = match state.start_next_queued_turn_off_lock(&session, false, false) {
            Ok(_) => panic!("the transcript promotion commit must fail"),
            Err(error) => error,
        };
        assert!(
            format!("{error:#}").contains("injected-promotion-commit-failure"),
            "{error:#}"
        );
        let rollback_revision = state.snapshot().revision;
        assert!(receiver.try_recv().is_err(), "no provider handoff is durable yet");

        let rollback_snapshot = std::iter::from_fn(|| state_events.try_recv().ok())
            .filter_map(|payload| serde_json::from_str::<Value>(&payload).ok())
            .find(|payload| {
                payload["revision"] == rollback_revision
                    && payload["sessions"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|candidate| {
                            candidate["id"] == session
                                && candidate["preview"]
                                    .as_str()
                                    .is_some_and(|preview| {
                                        preview.contains("promotion was not persisted")
                                    })
                        })
            })
            .expect("promotion rollback must publish its restored projection immediately");
        let published_session = rollback_snapshot["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|candidate| candidate["id"] == session)
            .unwrap();
        assert_eq!(published_session["status"], "idle");
        assert_eq!(published_session["queuePaused"], true);
        assert!(
            published_session.get("pendingPrompts").is_none(),
            "metadata-first SSE snapshots do not inline prompt bodies"
        );
        let hydrated = serde_json::to_value(state.get_session(&session).unwrap()).unwrap();
        let hydrated_prompt = &hydrated["session"]["pendingPrompts"][0];
        assert_eq!(hydrated_prompt["id"], prompt_id);
        assert_eq!(
            hydrated_prompt["isEngramRetained"],
            true
        );
        assert!(
            hydrated_prompt.get("engramBind").is_none()
                && hydrated_prompt.get("engramEvaluate").is_none(),
            "host-private prepared payloads must not enter the wire projection"
        );

        let issued = transport
            .requests()
            .into_iter()
            .find(|request| request.request["operation"] == lost_operation)
            .expect("the lost operation should have reached the wire")
            .request;
        let failed_queue = {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert_eq!(record.session.status, SessionStatus::Idle);
            assert!(record.orchestrator_auto_dispatch_blocked);
            assert!(record.session.queue_paused);
            assert!(record.session.preview.contains("promotion was not persisted"));
            assert_eq!(record.active_turn_generation, baseline_generation);
            assert_eq!(record.session.prompt_history, baseline_history);
            assert_eq!(record.session.message_count, baseline_message_count);
            assert_eq!(record.queued_prompts.len(), 2);
            assert_eq!(record.queued_prompts[0].pending_prompt.id, prompt_id);
            assert_eq!(record.queued_prompts[1].pending_prompt.id, successor_id);
            assert_eq!(record.queued_prompts[0].promoted_message_index, None);
            assert!(record.queued_prompts[0].promotion_disposition_known);
            assert!(!record.queued_prompts[0].engram_interrupted);
            assert!(cached_message_index_on_record(record, &prompt_id).is_none());
            let prepared = if lost_operation == "session_bind" {
                &record.queued_prompts[0]
                    .engram_bind
                    .as_ref()
                    .expect("lost bind must remain prepared")
                    .request
            } else {
                &record.queued_prompts[0]
                    .engram_evaluate
                    .as_ref()
                    .expect("lost evaluation must remain prepared")
                    .request
            };
            assert_eq!(serde_json::to_value(prepared).unwrap(), issued);
            serde_json::to_value(&record.queued_prompts).unwrap()
        };

        connection.execute_batch("DROP TRIGGER reject_engram_promotion;").unwrap();
        drop(connection);
        let mut loaded = load_state(state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        {
            let index = loaded.find_session_index(&session).unwrap();
            let record = loaded.session_mut_by_index(index).unwrap();
            assert_eq!(serde_json::to_value(&record.queued_prompts).unwrap(), failed_queue);
            assert!(record.orchestrator_auto_dispatch_blocked);
            assert!(record.session.queue_paused);
            assert_eq!(record.session.status, SessionStatus::Idle);
            assert!(record.engram.recovered_admission);
            assert!(!record.engram_boot_recovery_pending);
            assert_eq!(record.queued_prompts[0].pending_prompt.id, prompt_id);
            assert_eq!(record.queued_prompts[1].pending_prompt.id, successor_id);
            record.engram.context_nudge_pending = false;
        }
        assert!(AppState::engram_session_requires_dispatch_card_locked(
            &loaded, &session
        ));
        *state.inner.lock().unwrap() = loaded;
        state.install_control_test_transport(transport.clone());

        state.resume_session_queue(&session).unwrap();
        let observed_operations = operations(&transport);
        assert!(
            observed_operations
                .iter()
                .filter(|operation| **operation == lost_operation)
                .count()
                >= 2,
            "public Resume did not replay {lost_operation}; observed {observed_operations:?}"
        );
        let CodexRuntimeCommand::Prompt { command, .. } = receiver.try_recv().unwrap() else {
            panic!("public Resume should hand off the exact retained prompt once");
        };
        assert!(command.prompt.contains("expanded exact prepared rollback prompt"));
        assert!(command.prompt.contains("Peer agent"));
        assert!(command.prompt.contains("peer-session"));
        assert_eq!(command.attachments.len(), 1);
        assert!(receiver.try_recv().is_err());
        let replayed = transport
            .requests()
            .into_iter()
            .filter(|request| request.request["operation"] == lost_operation)
            .map(|request| request.request)
            .collect::<Vec<_>>();
        assert_eq!(replayed.len(), 2);
        assert_eq!(replayed[0], issued);
        assert_eq!(replayed[1], issued, "Resume must replay the exact request and key");
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(record.queued_prompts[0].pending_prompt.id, successor_id);
        assert_eq!(
            record
                .session
                .messages
                .iter()
                .filter(|message| message.id() == prompt_id)
                .count(),
            1
        );
        assert!(cached_message_index_on_record(record, &successor_id).is_none());
    }
}

#[test]
fn writer_backed_initial_explorer_admission_is_barriered_before_boot_settlement() {
    let (state, parent, receiver, first_transport) = root_fixture([
        bind_reply("parent"),
        bind_reply("child"),
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline(
            "lost child evaluation reply",
        ))),
    ]);
    let created = state
        .create_read_only_delegation(
            &parent,
            CreateDelegationRequest {
                prompt: "initial Explorer prompt retained across restart".into(),
                title: Some("restart explorer".into()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Explorer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .unwrap();
    let child = created.delegation.child_session_id;
    let delegation = created.delegation.id;
    let original_evaluate = first_transport
        .requests()
        .into_iter()
        .find(|request| request.request["operation"] == "turn_evaluate")
        .expect("initial child evaluation should reach the wire")
        .request;
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&child).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        assert!(record.queued_prompts[0].has_engram_intent());
        assert!(record.queued_prompts[0].promoted_message_index.is_some());
        // Model the durable window before the failing admission worker parks
        // the queue. Boot reconstruction must supply this barrier itself.
        record.set_auto_dispatch_blocked(false);
    }
    let mut cache = SqlitePersistConnectionCache::new();
    let delta = collect_persist_delta_from_shared_state(&state.inner, 0);
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &delta,
        &mut PersistFenceBatch::default(),
    )
    .unwrap();

    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    {
        let record = &loaded.sessions[loaded.find_session_index(&child).unwrap()];
        assert!(record.engram.recovered_admission);
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(record.session.queue_paused);
        assert_eq!(record.queued_prompts.len(), 1);
        assert!(record.queued_prompts[0]
            .pending_prompt
            .text
            .contains("initial Explorer prompt retained across restart"));
        assert_eq!(
            loaded.delegations[loaded.find_delegation_index(&delegation).unwrap()].status,
            DelegationStatus::Running,
            "boot settlement must not terminalize a child with retained authorization",
        );
    }

    *state.inner.lock().unwrap() = loaded;
    let retry_transport = ScriptedEngramControlTransport::new([
        status_reply("ready"),
        grant_reply("recovered-child-grant"),
        begin_reply("recovered-child-grant"),
    ]);
    state.install_control_test_transport(retry_transport.clone());
    state.resume_session_queue(&child).unwrap();
    let CodexRuntimeCommand::Prompt { command, .. } = receiver.try_recv().unwrap() else {
        panic!("public Resume should deliver the retained child prompt");
    };
    assert!(command
        .prompt
        .contains("initial Explorer prompt retained across restart"));
    assert!(receiver.try_recv().is_err());
    let replayed = retry_transport
        .requests()
        .into_iter()
        .find(|request| request.request["operation"] == "turn_evaluate")
        .expect("public Resume should replay the prepared evaluation")
        .request;
    assert_eq!(replayed, original_evaluate);
}

#[test]
fn writer_backed_begun_prehandoff_restart_never_resends_provider_prompt() {
    let (state, session, receiver, _) = root_fixture([
        bind_reply("token"),
        grant_reply("begun-grant"),
        begin_reply("begun-grant"),
    ]);
    let dispatch = root_dispatch(&state, &session, false);
    assert!(matches!(
        state.prepare_engram_turn_delivery_off_lock(
            &session,
            dispatch.engram_dispatch_generation().unwrap(),
        ),
        EngramTurnDeliveryPreparation::Ready
    ));
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.queued_prompts[0].promoted_message_index.is_some());
        assert_eq!(
            record.queued_prompts[0]
                .engram_evaluate
                .as_ref()
                .unwrap()
                .begun_grant_id
                .as_deref(),
            Some("begun-grant"),
        );
        assert!(!record.orchestrator_auto_dispatch_blocked);
    }
    let mut cache = SqlitePersistConnectionCache::new();
    let delta = collect_persist_delta_from_shared_state(&state.inner, 0);
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &delta,
        &mut PersistFenceBatch::default(),
    )
    .unwrap();

    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    {
        let record = &loaded.sessions[loaded.find_session_index(&session).unwrap()];
        assert!(record.engram.recovered_admission);
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(record.session.queue_paused);
        assert!(record.queued_prompts[0].promoted_message_index.is_some());
    }
    *state.inner.lock().unwrap() = loaded;
    let recovery_transport = ScriptedEngramControlTransport::new([status_reply("ready")]);
    state.install_control_test_transport(recovery_transport.clone());
    state.resume_session_queue(&session).unwrap();
    assert!(receiver.try_recv().is_err());
    assert_eq!(operations(&recovery_transport), ["session_status"]);
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(record.queued_prompts[0].engram_interrupted);
}

#[test]
fn writer_backed_ordinary_queue_restart_does_not_gain_engram_barrier() {
    let (state, _) = test_app_state_with_delegation_codex_runtime("ordinary-restart-runtime");
    let root = state.test_temp_root.as_ref().unwrap().path().join("ordinary-project");
    fs::create_dir_all(&root).unwrap();
    let project = create_test_project(&state, &root, "Ordinary project");
    let session = create_test_project_session(&state, Agent::Codex, &project, &root);
    queue_test_engram_prompt(
        &state,
        &session,
        "ordinary queued prompt",
        QueuedPromptSource::User,
        None,
    );
    let mut cache = SqlitePersistConnectionCache::new();
    let delta = collect_persist_delta_from_shared_state(&state.inner, 0);
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &delta,
        &mut PersistFenceBatch::default(),
    )
    .unwrap();
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&session).unwrap()];
    assert!(!record.engram.recovered_admission);
    assert!(!record.orchestrator_auto_dispatch_blocked);
    assert!(!record.session.queue_paused);
    assert_eq!(record.queued_prompts[0].pending_prompt.text, "ordinary queued prompt");
}

#[test]
fn trimmed_legacy_unknown_promotion_disposition_still_fails_closed() {
    let (state, session, _, _) = root_fixture([]);
    let prompt_id = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        for sequence in 0..80 {
            push_message_on_record(
                record,
                Message::Text {
                    id: format!("legacy-trim-filler-{sequence}"),
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: "legacy retained history".into(),
                    attachments: vec![],
                    expanded_text: None,
                    source: None,
                },
            );
        }
        drop(inner);
        queue_test_engram_prompt(
            &state,
            &session,
            "legacy disposition unknown",
            QueuedPromptSource::User,
            None,
        );
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.queued_prompts[0].engram_waiting = true;
        sync_pending_prompts(record);
        record.queued_prompts[0].pending_prompt.id.clone()
    };
    let mut cache = SqlitePersistConnectionCache::new();
    let delta = collect_persist_delta_from_shared_state(&state.inner, 0);
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &delta,
        &mut PersistFenceBatch::default(),
    )
    .unwrap();
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    let encoded: String = connection
        .query_row(
            "SELECT value_json FROM sessions WHERE id = ?1",
            rusqlite::params![session],
            |row| row.get(0),
        )
        .unwrap();
    let mut legacy: Value = serde_json::from_str(&encoded).unwrap();
    legacy["queuedPrompts"][0]
        .as_object_mut()
        .unwrap()
        .remove("promotion_disposition_known");
    connection
        .execute(
            "UPDATE sessions SET value_json = ?1 WHERE id = ?2",
            rusqlite::params![legacy.to_string(), session],
        )
        .unwrap();

    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&session).unwrap()];
    assert!(record.message_start_index > 0);
    assert!(cached_message_index_on_record(record, &prompt_id).is_none());
    assert!(record.queued_prompts[0].engram_interrupted);
    assert!(!record.engram.recovered_admission);
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(record.session.queue_paused);
}
