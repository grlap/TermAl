// Owns the stall shape of a project settings change on a gated project: while
// the project's reset fence is held, an idle, unpaused session with a queued
// mailbox wake says why it is waiting, and the reason goes when the fence is
// released. A paused or busy session keeps its own preview. Does not own the
// reset transaction itself (src/tests/engram_host_adapter.rs), the order the
// release drains sessions in, or the teardown of control processes. New
// module, a child of src/tests/engram_root_dispatch.rs, whose root fixture it
// uses.

use super::*;

const PROJECT_RESET_WAIT_PREFIX: &str = "Waiting: Engram project settings are changing";

impl AppState {
    /// Replays the removed post-commit loops from the baseline for a controlled
    /// scale comparison. No production path can enable this test-only replay.
    pub(crate) fn replay_eager_engram_project_reset_bindings(
        &self,
        project: &str,
        generation: u64,
        sessions: &[String],
        timer: &EngramProjectResetTimer,
    ) {
        let targets = {
            let inner = self.inner.lock().unwrap();
            sessions
                .iter()
                .filter_map(|session| {
                    count_engram_reset_scaled_work("fresh target scan", 1);
                    if inner.find_session_index(session).is_some_and(|index| {
                        inner.sessions[index]
                            .queued_prompts
                            .front()
                            .is_some_and(QueuedPromptRecord::has_engram_intent)
                    }) {
                        return None;
                    }
                    project_engram_binding_target_during_owned_reset_locked(
                        &inner, session, project, generation,
                    )
                    .unwrap()
                })
                .collect::<Vec<_>>()
        };
        for (ordinal, target) in targets.into_iter().enumerate() {
            let _phase = timer.phase_work(
                "fresh bind",
                Some(&target.connection.session_id),
                ordinal,
                sessions.len(),
            );
            let _permit = match self.engram_retry_head_permit(&target.connection.session_id) {
                EngramRetryHeadPermit::Deferred => continue,
                permit => permit,
            };
            count_engram_reset_scaled_work("fresh bind", 1);
            let session_id = target.connection.session_id.clone();
            if let Err(error) = self.bind_engram_target_off_lock(target) {
                // The removed baseline loop continued after a degraded bind.
                // In particular, named-root reconciliation still sees its reset fence.
                self.record_engram_transport_failure(&session_id, &error);
                eprintln!("legacy replay session={session_id} post-reconfigure bind degraded: {error}");
            }
        }
    }
}

struct ResetScaleReplayGuard;

impl Drop for ResetScaleReplayGuard {
    fn drop(&mut self) {
        TEST_ENGRAM_RESET_LEGACY_SCALE.with(|enabled| enabled.set(false));
    }
}

struct ResetScaleTransport {
    inner: Arc<StatefulEngramControlTransport>,
    clock: EngramBudgetClock,
}

impl EngramControlTransport for ResetScaleTransport {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        self.clock.advance(Duration::from_millis(1));
        self.inner.request(connection, request, timeout)
    }

    fn shutdown_session(&self, session: &str) {
        self.inner.shutdown_session(session);
    }

    fn shutdown_session_until(
        &self,
        session: &str,
        clock: &EngramBudgetClock,
        deadline: std::time::Instant,
    ) -> Result<(), EngramTransportError> {
        self.inner.shutdown_session_until(session, clock, deadline)
    }
}

fn measured_idle_reset_scale(legacy_eager: bool) -> BTreeMap<&'static str, usize> {
    let (mut state, session, _receiver, _transport) = root_fixture([]);
    let (project, mut settings) = tier_reset_settings(&state, &session, false);
    // Match production's queued persistence path in both count-only arms.
    // The disconnected default fixture would rewrite all sessions per bind.
    let (persist_tx, persist_rx) = mpsc::channel();
    state.persist_tx = persist_tx;
    {
        let mut inner = state.inner.lock().unwrap();
        let template = inner.sessions[inner.find_session_index(&session).unwrap()].clone();
        for ordinal in 1..3_850 {
            let mut record = template.clone();
            record.session.id = format!("scale-history-{ordinal}");
            record.runtime = SessionRuntime::None;
            record.hidden = ordinal % 2 == 0;
            inner.sessions.push(record);
        }
    }
    let clock = state.engram_budget_clock();
    let transport = Arc::new(ResetScaleTransport {
        inner: StatefulEngramControlTransport::new(),
        clock: clock.clone(),
    });
    state.install_control_test_transport(transport.clone());
    TEST_ENGRAM_RESET_LEGACY_SCALE.with(|enabled| enabled.set(legacy_eager));
    let _guard = ResetScaleReplayGuard;
    TEST_ENGRAM_RESET_SCALED_WORK.with(|work| work.borrow_mut().clear());
    reset_engram_authority_build_counts();
    let started = clock.now();
    settings.turn_gated_control = true;
    state
        .update_project_engram_settings(&project, settings)
        .unwrap();
    let (captures, targets) = engram_authority_build_counts();
    let mut work = TEST_ENGRAM_RESET_SCALED_WORK.with(|work| work.borrow().clone());
    work.insert("root authority captures", captures);
    work.insert("binding targets", targets);
    work.insert("control RPCs", transport.inner.requests().len());
    work.insert(
        "controlled RPC milliseconds",
        clock.elapsed_since(started).as_millis() as usize,
    );
    let inner = state.inner.lock().unwrap();
    assert!(!inner.engram_project_resets.contains(&project));
    assert!(
        inner
            .sessions
            .iter()
            .all(|record| !record.engram.project_reset_in_progress)
    );
    eprintln!("controlled production-shaped queued-persistence replay legacy_eager={legacy_eager} sessions=3850 work={work:?}");
    let pending = persist_rx.try_iter().collect::<Vec<_>>();
    assert!(!pending.is_empty());
    assert!(
        pending.iter().all(|request| matches!(request, PersistRequest::Delta)),
        "the count replay must never acknowledge a persistence fence"
    );
    // Flush the real final state once, outside the measured operation counts.
    persist_locked_state_to_sqlite(state.persistence_path.as_path(), &inner).unwrap();
    work
}

#[test]
fn reset_phase_work_at_3850_sessions_distinguishes_legacy_fanout_from_retained_scans() {
    let before = measured_idle_reset_scale(true);
    let after = measured_idle_reset_scale(false);
    for phase in [
        "authority target scan",
        "old checkpoint candidate scan",
        "raise session reset fence",
        "quiescence scan",
        "commit session reset",
        "control shutdown",
        "queue registration",
        "release preview scan",
    ] {
        assert_eq!(before[phase], 3_850, "baseline {phase}");
        assert_eq!(after[phase], 3_850, "candidate {phase}");
    }
    assert_eq!(
        before["project membership scan"],
        after["project membership scan"]
    );
    assert_eq!(
        before["release membership scan"],
        after["release membership scan"]
    );
    assert_eq!(before["checkpoint claim arbitration"], 3_850);
    assert_eq!(
        after
            .get("checkpoint claim arbitration")
            .copied()
            .unwrap_or(0),
        0
    );
    assert_eq!(before["root authority captures"], 2 * 3_850);
    assert_eq!(before["binding targets"], 2 * 3_850);
    assert_eq!(before["fresh target scan"], 3_850);
    assert_eq!(before["fresh bind"], 3_850);
    assert_eq!(before["control RPCs"], 3_850);
    assert_eq!(before["controlled RPC milliseconds"], 3_850);
    for removed in [
        "root authority captures",
        "binding targets",
        "control RPCs",
        "controlled RPC milliseconds",
    ] {
        assert_eq!(after[removed], 0, "idle candidate removed {removed}");
    }
    assert!(!after.contains_key("fresh target scan"));
    assert!(!after.contains_key("fresh bind"));
}

#[test]
fn reset_progress_keeps_one_clock_and_rejects_stale_phase_completion() {
    let clock = EngramBudgetClock::scripted();
    let mut fences = EngramProjectResetFences::default();
    let first = fences
        .claim_with_clock("project", clock.clone(), true)
        .unwrap();
    let claim = fences.claimed_at["project"].clone();
    let timer = EngramProjectResetTimer::new("project", first, claim.clone());
    let old_phase = timer.phase("checkpoint", None);
    clock.advance(Duration::from_secs(3));
    let current_phase = timer.phase("commit", None);
    drop(old_phase);
    let progress = claim.progress();
    assert_eq!(progress.phase, "commit");
    assert_eq!(progress.completed, 0);
    assert_eq!(
        progress.total, 1,
        "counts describe this phase's one operation"
    );
    assert_eq!(progress.elapsed_ms, 3_000);
    assert_eq!(progress.remaining_ms, Some(18_000));
    drop(current_phase);
    assert_eq!(claim.progress().completed, 1);
    clock.advance(Duration::from_secs(18));
    assert!(claim.progress().deadline_expired);
    assert_eq!(claim.progress().remaining_ms, Some(0));
    assert!(fences.release("project", first));
    let second = fences.claim_with_clock("project", clock, true).unwrap();
    timer.committed();
    drop(timer);
    assert!(!fences.release("project", first));
    let current = fences.claimed_at["project"].progress();
    assert_eq!(current.generation, second);
    assert_eq!(current.phase, "claimed");
    assert!(!current.settings_committed);
    assert_eq!(current.elapsed_ms, 0);
}

#[tokio::test]
async fn reset_progress_and_overlap_refusal_are_redacted_read_only_api_results() {
    let (state, session, _receiver, transport) = root_fixture([]);
    let (project, generation) = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let project = inner.sessions[index].session.project_id.clone().unwrap();
        let generation = inner.claim_engram_project_reset(&project, true).unwrap();
        (project, generation)
    };
    let timer = state.engram_project_reset_timer(&project, generation);
    let _phase = timer.phase("quiescence", None);
    state.engram_budget_clock().advance(Duration::from_secs(7));
    let app = Router::new()
        .route(
            "/settings/{id}",
            get(get_project_engram_reset).patch(update_project_engram_settings),
        )
        .with_state(state.clone())
        .layer(axum::Extension(EngramReadinessLimiter(Arc::new(
            tokio::sync::Semaphore::new(0),
        ))));
    for (method, body) in [
        ("GET", ""),
        ("PATCH", r#"{"enabled":true,"turnGatedControl":false}"#),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(format!("/settings/{project}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if method == "GET" {
                StatusCode::OK
            } else {
                StatusCode::CONFLICT
            }
        );
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["reset"]["generation"], generation);
        assert_eq!(value["reset"]["phase"], "quiescence");
        assert_eq!(value["reset"]["elapsedMs"], 7_000);
        assert_eq!(value["reset"]["remainingMs"], 14_000);
        assert_eq!(value["reset"]["settingsCommitted"], false);
        if method == "PATCH" {
            assert_eq!(value["code"], "engram_project_reset_in_progress");
        } else {
            assert_eq!(value["committed"]["turnGatedControl"], true);
        }
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        for private in ["routingToken", "grantId", "binaryPath", "home", "session-"] {
            assert!(
                !text.contains(private),
                "progress must not expose {private}"
            );
        }
    }
    assert!(transport.requests().is_empty());
    assert!(
        state
            .inner
            .lock()
            .unwrap()
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap()
            .turn_gated_control,
        "refused rollback did not change settings"
    );
}

#[test]
fn enabling_control_with_3850_idle_sessions_does_not_build_or_bind_historical_targets() {
    let (state, session, receiver, transport) = root_fixture([]);
    let (project, settings) = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let template = inner.sessions[index].clone();
        let project = template.session.project_id.clone().unwrap();
        let root = PathBuf::from(&inner.find_project(&project).unwrap().root_path);
        fs::write(root.join(".engram-project"), "fixture-ready\n").unwrap();
        let mut settings = real_fixture_engram_settings(&root);
        settings.turn_gated_control = false;
        inner
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .unwrap()
            .engram = Some(settings.clone());
        for ordinal in 1..3_850 {
            let mut record = template.clone();
            record.session.id = format!("historical-session-{ordinal}");
            record.runtime = SessionRuntime::None;
            record.hidden = ordinal % 2 == 0;
            inner.sessions.push(record);
        }
        settings.turn_gated_control = true;
        (project, settings)
    };
    reset_engram_authority_build_counts();
    state
        .update_project_engram_settings(&project, settings)
        .unwrap();
    let (captures, builds) = engram_authority_build_counts();
    assert_eq!(
        (captures, builds),
        (0, 0),
        "idle history must not clone root authority images"
    );
    assert!(
        transport.requests().is_empty(),
        "no historical session needs a fresh binding"
    );
    let inner = state.inner.lock().unwrap();
    assert!(!inner.engram_project_resets.contains(&project));
    assert!(
        inner
            .sessions
            .iter()
            .all(|record| !record.engram.project_reset_in_progress)
    );
    assert!(
        inner
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap()
            .turn_gated_control
    );
    drop(inner);

    // A late, debt-free root is not serialized behind thousands of paused
    // historical heads or a cleanup owner. Release only registers ordinary
    // service; the existing capped tick performs the admission.
    queue_test_engram_prompt(
        &state,
        &session,
        "late eligible root",
        QueuedPromptSource::User,
        None,
    );
    let generation = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let root_record = inner.sessions.remove(index);
        let head = root_record.queued_prompts.front().unwrap().clone();
        let mut cleanup_claimed = false;
        for ordinal in 0..inner.sessions.len() {
            let record = inner.session_mut_by_index(ordinal).unwrap();
            if record.session.project_id.as_deref() != Some(&project) {
                continue;
            }
            let mut queued = head.clone();
            queued.pending_prompt.id = format!("held-head-{ordinal}");
            record.queued_prompts.push_back(queued);
            record.set_auto_dispatch_blocked(true);
            record.engram.operator_paused = true;
            if !cleanup_claimed {
                record.set_auto_dispatch_blocked(false);
                record.engram.operator_paused = false;
                record.claim_missing_runtime_stop(RuntimeStopOwnerKind::UserStop);
                cleanup_claimed = true;
            }
            record.engram.project_reset_in_progress = true;
            sync_pending_prompts(record);
        }
        inner.sessions.push(root_record);
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram.project_reset_in_progress = true;
        let generation = inner.claim_engram_project_reset(&project, true).unwrap();
        state.commit_locked(&mut inner).unwrap();
        generation
    };
    let ordinary = StatefulEngramControlTransport::new();
    state.install_control_test_transport(ordinary.clone());
    state.release_engram_project_reset_fence(&project, generation, std::slice::from_ref(&session));
    {
        let inner = state.inner.lock().unwrap();
        assert!(!inner.engram_project_resets.contains(&project));
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.engram_queue_schedule.activation.is_some());
        assert_eq!(record.queued_prompts.len(), 1, "registered is not admitted");
    }
    assert!(ordinary.requests().is_empty());
    state.engram_abort_retry_tick(state.engram_budget_clock().wall_now());
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    let requests = ordinary.requests();
    assert_eq!(
        requests.len(),
        3,
        "only the eligible root binds, evaluates and begins"
    );
    assert!(
        requests
            .iter()
            .all(|request| request.connection.session_id == session)
    );
    let inner = state.inner.lock().unwrap();
    assert!(
        inner
            .sessions
            .iter()
            .filter(
                |record| record.session.project_id.as_deref() == Some(&project)
                    && record.session.id != session
            )
            .all(|record| {
                (record.orchestrator_auto_dispatch_blocked || record.runtime_stop_in_progress)
                    && record.queued_prompts.len() == 1
            }),
        "explicit pauses and cleanup ownership remain in force"
    );
}

#[test]
fn reset_activation_starts_deferred_boot_recovery_under_its_slot_without_an_unrelated_drain() {
    for unresolved_fast in [false, true] {
        let (state, session, receiver, _transport) = root_fixture([]);
        let (project, _) = tier_reset_settings(&state, &session, true);
        let transport = StatefulEngramControlTransport::new();
        state.install_control_test_transport(transport.clone());
        queue_test_engram_prompt(&state, &session, "after eager recovery expired", QueuedPromptSource::User, None);
        let generation = {
            let mut inner = state.inner.lock().unwrap();
            let generation = inner.claim_engram_project_reset(&project, true).unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            if unresolved_fast {
                record.session.model = "catalog-model".to_owned();
                record.session.codex_fast_mode = true;
                record.session.model_options.clear();
            }
            record.engram.project_reset_in_progress = true;
            // The eager boot budget has left this root pending, with no worker
            // and no admission/bind/abort retry to trigger its lazy reconciliation.
            record.engram_boot_recovery_pending = true;
            record.engram_boot_recovery_retry_in_progress = false;
            assert!(record.engram.admission_retry.is_none());
            assert!(record.engram.bind_retry.is_none());
            assert!(record.engram.abort_retry.is_none());
            generation
        };
        state.release_engram_project_reset_fence(&project, generation, std::slice::from_ref(&session));
        let gate = BindDispositionGate::new(&state, &session, "lazy_boot_recovery");
        let finished = unresolved_fast.then(||
            BindDispositionGate::new(&state, &session, "lazy_boot_recovery_finished"));
        state.engram_abort_retry_tick(state.engram_budget_clock().wall_now());
        gate.wait();
        assert_eq!(state.engram_retry_slots.in_flight(), 1, "recovery owns the selected slot");
        {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(record.engram_boot_recovery_pending);
            assert!(record.engram_boot_recovery_retry_in_progress);
        }
        assert!(receiver.try_recv().is_err(), "readiness still fences delivery");
        gate.release();
        if let Some(finished) = finished {
            let CodexRuntimeCommand::RefreshModelList { response_tx } =
                phase_sync::receive(&receiver, "recovered head's unresolved Fast discovery")
            else {
                panic!("Fast discovery must precede the provider prompt");
            };
            // The recovery scope has ended, including its local slot. The Fast
            // worker is still blocked on this actual discovery response.
            finished.wait();
            let recovery_finished = {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                !record.engram_boot_recovery_pending && !record.engram_boot_recovery_retry_in_progress
            };
            assert!(recovery_finished);
            assert_eq!(state.engram_retry_slots.in_flight(), 1,
                "Fast discovery must retain the activation slot after recovery finishes");
            assert!(receiver.try_recv().is_err(), "discovery still fences the provider prompt");
            finished.release();
            response_tx.send(Ok(codex_model_options(&json!({ "data": [{
                "model": "catalog-model",
                "serviceTiers": [{ "id": "Turbo-EXACT", "name": "Fast" }]
            }] })))).unwrap();
        }
        let deadline = std::time::Instant::now() + TEST_PHASE_DEADLOCK_GUARD;
        loop {
            let pending = {
                let inner = state.inner.lock().unwrap();
                inner.sessions[inner.find_session_index(&session).unwrap()].engram_boot_recovery_pending
            };
            if !pending && state.engram_retry_slots.in_flight() == 0 { break; }
            assert!(std::time::Instant::now() < deadline, "lazy recovery must finish");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(receiver.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap(), CodexRuntimeCommand::Prompt { .. }));
        assert!(receiver.try_recv().is_err(), "one recovered head is delivered once");
        assert_eq!(transport.requests().iter().filter(|request|
            request.request["operation"] == "turn_begin").count(), 1);
    }
}

#[test]
fn reset_activation_lazy_recovery_keeps_the_original_owner_after_queue_changes() {
    for change in ["cancel", "replacement", "generation"] {
        let (state, session, receiver, _transport) = root_fixture([]);
        let (project, _) = tier_reset_settings(&state, &session, true);
        let transport = StatefulEngramControlTransport::new();
        state.install_control_test_transport(transport.clone());
        queue_test_engram_prompt(&state, &session, "original A", QueuedPromptSource::User, None);
        if change == "cancel" {
            queue_test_engram_prompt(&state, &session, "dormant B", QueuedPromptSource::User, None);
        }
        let (generation, original) = {
            let mut inner = state.inner.lock().unwrap();
            let generation = inner.claim_engram_project_reset(&project, true).unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            record.engram.project_reset_in_progress = true;
            record.engram_boot_recovery_pending = true;
            let original = EngramQueuedAdmissionOwner::capture(record).unwrap();
            assert!(!record.queued_prompts.front().unwrap().is_engram_retained());
            (generation, original)
        };
        state.release_engram_project_reset_fence(&project, generation, std::slice::from_ref(&session));
        let gate = BindDispositionGate::new(&state, &session, "lazy_boot_recovery");
        let finished = BindDispositionGate::new(&state, &session, "lazy_boot_recovery_finished");
        state.engram_abort_retry_tick(state.engram_budget_clock().wall_now());
        gate.wait();
        assert_eq!(state.engram_retry_slots.in_flight(), 1);
        if change == "cancel" {
            state.cancel_queued_prompt(&session, &original.prompt_id).unwrap();
        } else {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            if change == "replacement" {
                let head = record.queued_prompts.front_mut().unwrap();
                head.pending_prompt.id = "replacement-B".to_owned();
                head.pending_prompt.text = "replacement B".to_owned();
                sync_pending_prompts(record);
            } else {
                record.engram.dispatch_generation += 1;
            }
            state.commit_locked(&mut inner).unwrap();
        }
        // A separate registration may arrive while old A still owns recovery.
        // Its existence is not permission for A's completion to dispatch it.
        let independently_registered = if change == "generation" {
            let mut inner = state.inner.lock().unwrap();
            state.register_engram_project_reset_sessions_locked(&mut inner, std::slice::from_ref(&session));
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].engram_queue_schedule.activation.clone()
        } else {
            None
        };
        gate.release();
        finished.wait();
        let (ready, still_queued, activation, current_owner) = {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            (!record.engram_boot_recovery_pending && !record.engram_boot_recovery_retry_in_progress,
                record.queued_prompts.len(), record.engram_queue_schedule.activation.clone(),
                EngramQueuedAdmissionOwner::capture(record))
        };
        assert!(ready, "readiness settles independently of the old head");
        assert_eq!(state.engram_retry_slots.in_flight(), 0);
        assert_eq!(transport.requests().iter().filter(|request|
            matches!(request.request["operation"].as_str(), Some("turn_evaluate" | "turn_begin"))).count(), 0,
            "stale recovery activation must not evaluate or begin the successor");
        assert!(receiver.try_recv().is_err(), "stale recovery must not deliver the successor");
        assert_eq!(still_queued, 1, "the current head stays dormant");
        assert_ne!(current_owner, Some(original));
        if independently_registered.is_some() {
            assert_eq!(activation, independently_registered, "old completion must not erase a newer activation");
        }
        finished.release();
        {
            let mut inner = state.inner.lock().unwrap();
            state.register_engram_project_reset_sessions_locked(&mut inner, std::slice::from_ref(&session));
            let index = inner.find_session_index(&session).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            record.set_auto_dispatch_blocked(true);
            record.engram.operator_paused = true;
            record.runtime_stop_in_progress = true;
            state.commit_locked(&mut inner).unwrap();
        }
        state.engram_abort_retry_tick(state.engram_budget_clock().wall_now());
        assert!(receiver.try_recv().is_err(), "a new activation still respects pause and Stop custody");
        assert!(transport.requests().iter().all(|request|
            !matches!(request.request["operation"].as_str(), Some("turn_evaluate" | "turn_begin"))));
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            record.set_auto_dispatch_blocked(false);
            record.engram.operator_paused = false;
            record.runtime_stop_in_progress = false;
            state.commit_locked(&mut inner).unwrap();
        }
        state.engram_abort_retry_tick(state.engram_budget_clock().wall_now());
        assert!(matches!(receiver.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap(), CodexRuntimeCommand::Prompt { .. }));
        assert!(receiver.try_recv().is_err());
        assert_eq!(transport.requests().iter().filter(|request| request.request["operation"] == "turn_begin").count(), 1,
            "only the independently activated current head begins");
        assert_eq!(state.engram_retry_slots.in_flight(), 0);
    }
}

#[test]
fn ordinary_wake_during_activation_recovery_retains_its_own_head_until_service() {
    for after_clear in [false, true] {
        for replace in [false, true] {
            let (state, session, receiver, _transport) = root_fixture([]);
            let (project, _) = tier_reset_settings(&state, &session, true);
            let transport = StatefulEngramControlTransport::new();
            state.install_control_test_transport(transport.clone());
            queue_test_engram_prompt(&state, &session, "A", QueuedPromptSource::User, None);
            queue_test_engram_prompt(&state, &session, "B", QueuedPromptSource::User, None);
            let (generation, original) = {
                let mut inner = state.inner.lock().unwrap();
                let generation = inner.claim_engram_project_reset(&project, true).unwrap();
                let index = inner.find_session_index(&session).unwrap();
                let record = &mut inner.sessions[index];
                record.engram.project_reset_in_progress = true;
                record.engram_boot_recovery_pending = true;
                (generation, EngramQueuedAdmissionOwner::capture(record).unwrap())
            };
            state.release_engram_project_reset_fence(&project, generation, std::slice::from_ref(&session));
            let gate = BindDispositionGate::new(&state, &session, "lazy_boot_recovery");
            let finished = BindDispositionGate::new(&state, &session, "lazy_boot_recovery_finished");
            state.engram_abort_retry_tick(state.engram_budget_clock().wall_now());
            gate.wait();
            state.cancel_queued_prompt(&session, &original.prompt_id).unwrap();
            if replace {
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&session).unwrap();
                inner.sessions[index].engram.dispatch_generation += 1;
            }
            if after_clear {
                gate.release();
                finished.wait();
                assert!(receiver.try_recv().is_err(), "cancellation alone grants no successor wake");
                let dispatch = state.dispatch_next_queued_turn(&session, false).unwrap().unwrap();
                deliver_turn_dispatch(&state, dispatch).expect("independent ordinary drain after readiness");
                finished.release();
            } else {
                assert!(state.dispatch_next_queued_turn(&session, false).unwrap().is_none());
                let registered = {
                    let inner = state.inner.lock().unwrap();
                    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                    let current = EngramQueuedAdmissionOwner::capture(record).unwrap();
                    (current, record.engram_queue_schedule.activation.clone())
                };
                assert_eq!(registered.1.as_ref().map(|(owner, _)| owner), Some(&registered.0),
                    "ordinary readiness-deferred wake must retain its exact current owner at arrival");
                // A repeated independent event must not reset the same head's age.
                assert!(state.dispatch_next_queued_turn(&session, false).unwrap().is_none());
                {
                    let inner = state.inner.lock().unwrap();
                    assert_eq!(inner.sessions[inner.find_session_index(&session).unwrap()]
                        .engram_queue_schedule.activation, registered.1);
                }
                gate.release();
                finished.wait();
                assert_eq!(state.engram_retry_slots.in_flight(), 0);
                assert!(receiver.try_recv().is_err(), "A cannot dispatch independently woken B");
                assert!(transport.requests().iter().all(|r|
                    !matches!(r.request["operation"].as_str(), Some("turn_evaluate" | "turn_begin"))));
                {
                    let mut inner = state.inner.lock().unwrap();
                    let index = inner.find_session_index(&session).unwrap();
                    assert_eq!(inner.sessions[index].engram_queue_schedule.activation, registered.1);
                    assert!(!inner.sessions[index].engram_boot_recovery_pending);
                    inner.sessions[index].set_auto_dispatch_blocked(true);
                    inner.sessions[index].runtime_stop_in_progress = true;
                }
                finished.release();
                state.engram_abort_retry_tick(state.engram_budget_clock().wall_now());
                assert!(receiver.try_recv().is_err(), "independent B still respects pause and Stop");
                {
                    let mut inner = state.inner.lock().unwrap();
                    let index = inner.find_session_index(&session).unwrap();
                    inner.sessions[index].set_auto_dispatch_blocked(false);
                    inner.sessions[index].runtime_stop_in_progress = false;
                }
                state.engram_abort_retry_tick(state.engram_budget_clock().wall_now());
            }
            assert!(matches!(receiver.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap(), CodexRuntimeCommand::Prompt { .. }));
            assert!(receiver.try_recv().is_err(), "B delivers once");
            assert_eq!(transport.requests().iter().filter(|r| r.request["operation"] == "turn_begin").count(), 1);
            state.engram_abort_retry_tick(state.engram_budget_clock().wall_now());
            assert!(receiver.try_recv().is_err(), "a consumed or stale registration cannot duplicate B");
            assert_eq!(state.engram_retry_slots.in_flight(), 0);
        }
    }
}

#[test]
fn ordinary_lazy_recovery_still_wakes_its_queue_without_a_reset_activation() {
    let (state, session, receiver, _transport) = root_fixture([]);
    let _ = tier_reset_settings(&state, &session, true);
    let transport = StatefulEngramControlTransport::new();
    state.install_control_test_transport(transport.clone());
    queue_test_engram_prompt(&state, &session, "ordinary recovery", QueuedPromptSource::User, None);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram_boot_recovery_pending = true;
        assert!(inner.sessions[index].engram_queue_schedule.activation.is_none());
    }
    let finished = BindDispositionGate::new(&state, &session, "lazy_boot_recovery_finished");
    state.request_engram_boot_recovery_retry(&session);
    finished.wait();
    assert!(matches!(receiver.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap(), CodexRuntimeCommand::Prompt { .. }));
    assert!(receiver.try_recv().is_err());
    assert_eq!(transport.requests().iter().filter(|request| request.request["operation"] == "turn_begin").count(), 1);
    assert_eq!(state.engram_retry_slots.in_flight(), 0);
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(!record.engram_boot_recovery_pending && !record.engram_boot_recovery_retry_in_progress);
    drop(inner);
    finished.release();
}

#[test]
fn reset_activation_aging_preserves_deferred_heads_ahead_of_rejoining_attempts_and_arrivals() {
    let (state, session, _receiver, _transport) = root_fixture([]);
    queue_test_engram_prompt(
        &state,
        &session,
        "fair queue head",
        QueuedPromptSource::User,
        None,
    );
    let ids = {
        let mut inner = state.inner.lock().unwrap();
        let template = inner.sessions[inner.find_session_index(&session).unwrap()].clone();
        (0..9)
            .map(|ordinal| {
                let mut record = template.clone();
                record.session.id = format!("fair-session-{ordinal}");
                record.queued_prompts.front_mut().unwrap().pending_prompt.id =
                    format!("fair-head-{ordinal}");
                let id = record.session.id.clone();
                inner.sessions.push(record);
                id
            })
            .collect::<Vec<_>>()
    };
    let due = |ids: &[String], timestamp: &str| {
        let inner = state.inner.lock().unwrap();
        ids.iter()
            .map(|id| EngramDueRetry {
                session_id: id.clone(),
                kind: EngramRetryKind::Activation,
                due_at: timestamp.to_owned(),
                owner: EngramQueuedAdmissionOwner::capture(
                    &inner.sessions[inner.find_session_index(id).unwrap()],
                )
                .unwrap().into(),
            })
            .collect::<Vec<_>>()
    };
    let mut first = due(&ids[..8], "2026-01-01T00:00:00.000Z");
    state.order_engram_due_retries(&mut first);
    assert_eq!(
        first
            .iter()
            .map(|head| &head.session_id)
            .collect::<Vec<_>>(),
        ids[..8].iter().collect::<Vec<_>>()
    );
    {
        let mut inner = state.inner.lock().unwrap();
        // The first batch used the four slots. Its failures/no-ops become new
        // attempts; cap-deferred heads keep their original eligibility.
        for id in &ids[..ENGRAM_RETRY_MAX_IN_FLIGHT] {
            let index = inner.find_session_index(id).unwrap();
            inner.sessions[index].engram_queue_schedule.eligibility = None;
        }
    }
    let mut next = due(&ids, "2025-01-01T00:00:00.000Z");
    state.order_engram_due_retries(&mut next);
    assert_eq!(
        next[..4]
            .iter()
            .map(|head| &head.session_id)
            .collect::<Vec<_>>(),
        ids[4..8].iter().collect::<Vec<_>>()
    );
    // A fixed eight-head backlog therefore reaches the late head in two
    // admitted batches. With complete attempts bounded by T and tick interval
    // I, including scoped-worker fallback and the current batch, the documented
    // conservative service bound is (1 + ceil(B / 4)) * (4*T + I).
    assert_eq!(next[3].session_id, ids[7]);
}

struct BoundedActivationTransport {
    inner: Arc<StatefulEngramControlTransport>,
    clock: EngramBudgetClock,
    spans: Mutex<HashMap<String, (std::time::Instant, Option<std::time::Instant>)>>,
}

impl EngramControlTransport for BoundedActivationTransport {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        self.spans
            .lock()
            .unwrap()
            .entry(connection.session_id.clone())
            .or_insert((self.clock.now(), None));
        self.clock.advance(Duration::from_millis(1));
        let result = self.inner.request(connection, request, timeout);
        if matches!(request, EngramControlRequest::TurnBegin { .. }) && result.is_ok() {
            self.spans
                .lock()
                .unwrap()
                .get_mut(&connection.session_id)
                .unwrap()
                .1 = Some(self.clock.now());
        }
        result
    }

    fn shutdown_session(&self, session_id: &str) {
        self.inner.shutdown_session(session_id);
    }
}

#[test]
fn a_fixed_activation_backlog_receives_bounded_capped_service_including_the_late_root() {
    const HEADS: usize = 9;
    let (state, session, receiver, _transport) = root_fixture([]);
    queue_test_engram_prompt(
        &state,
        &session,
        "bounded service",
        QueuedPromptSource::User,
        None,
    );
    let (project, generation, ids) = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let template = inner.sessions[index].clone();
        let project = template.session.project_id.clone().unwrap();
        let mut ids = vec![session.clone()];
        for ordinal in 1..HEADS {
            let mut record = template.clone();
            record.session.id = format!("bounded-service-{ordinal}");
            record.queued_prompts.front_mut().unwrap().pending_prompt.id =
                format!("bounded-head-{ordinal}");
            ids.push(record.session.id.clone());
            inner.sessions.push(record);
        }
        let generation = inner.claim_engram_project_reset(&project, true).unwrap();
        for id in &ids {
            let index = inner.find_session_index(id).unwrap();
            inner
                .session_mut_by_index(index)
                .unwrap()
                .engram
                .project_reset_in_progress = true;
        }
        state.commit_locked(&mut inner).unwrap();
        (project, generation, ids)
    };
    let clock = state.engram_budget_clock();
    let transport = Arc::new(BoundedActivationTransport {
        inner: StatefulEngramControlTransport::new(),
        clock: clock.clone(),
        spans: Mutex::new(HashMap::new()),
    });
    state.install_control_test_transport(transport.clone());
    let started = clock.now();
    state.release_engram_project_reset_fence(&project, generation, &ids);
    assert!(transport.inner.requests().is_empty());
    let tick_interval = Duration::from_secs(2);
    // Four concurrent attempts can advance this shared controlled clock by at
    // most twelve one-ms RPC steps. All local storage/provider calls in this
    // fixture complete synchronously; there is no unbounded external worker.
    let complete_attempt_bound = Duration::from_millis(12);
    for batch in 0..HEADS.div_ceil(ENGRAM_RETRY_MAX_IN_FLIGHT) {
        state.engram_abort_retry_tick(clock.wall_now());
        let inner = state.inner.lock().unwrap();
        let served = ids
            .iter()
            .filter(|id| {
                inner.sessions[inner.find_session_index(id).unwrap()]
                    .session
                    .status
                    == SessionStatus::Active
            })
            .count();
        assert_eq!(
            served,
            ((batch + 1) * ENGRAM_RETRY_MAX_IN_FLIGHT).min(HEADS)
        );
        drop(inner);
        assert_eq!(state.engram_retry_slots.in_flight(), 0);
        if served != HEADS {
            clock.advance(tick_interval);
        }
    }
    let service_bound = (complete_attempt_bound * ENGRAM_RETRY_MAX_IN_FLIGHT as u32
        + tick_interval)
        * (1 + HEADS.div_ceil(ENGRAM_RETRY_MAX_IN_FLIGHT)) as u32;
    assert!(clock.elapsed_since(started) <= service_bound);
    let spans = transport.spans.lock().unwrap();
    assert_eq!(spans.len(), HEADS);
    assert!(
        spans
            .values()
            .all(|(first, last)| last.unwrap().duration_since(*first) <= complete_attempt_bound)
    );
    assert!(
        spans[ids.last().unwrap()].1.is_some(),
        "late root received actual Begin service"
    );
    assert_eq!(
        receiver.try_iter().count(),
        HEADS,
        "each head reached the provider once"
    );
}

fn tier_reset_settings(
    state: &AppState,
    session: &str,
    gated: bool,
) -> (String, EngramProjectSettings) {
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(session).unwrap();
    let project = inner.sessions[index].session.project_id.clone().unwrap();
    let root = PathBuf::from(&inner.find_project(&project).unwrap().root_path);
    fs::write(root.join(".engram-project"), "fixture-ready\n").unwrap();
    let mut settings = real_fixture_engram_settings(&root);
    settings.turn_gated_control = gated;
    inner
        .projects
        .iter_mut()
        .find(|p| p.id == project)
        .unwrap()
        .engram = Some(settings.clone());
    state.commit_locked(&mut inner).unwrap();
    (project, settings)
}

#[test]
fn tier_reset_deadline_refuses_before_commit_and_registers_the_held_head() {
    let (state, session, _receiver, transport) = root_fixture([]);
    let (project, mut settings) = tier_reset_settings(&state, &session, false);
    settings.turn_gated_control = true;
    let gate = gate_next_engram_project_reset_fence(&project);
    std::thread::scope(|scope| {
        let reset = scope.spawn(|| {
            let result = state.update_project_engram_settings(&project, settings);
            abort_engram_project_reset_fence_gate(&project, &fence_gate_detail(&result));
            result
        });
        gate.wait_until_entered();
        queue_test_engram_prompt(
            &state,
            &session,
            "held at deadline",
            QueuedPromptSource::User,
            None,
        );
        state
            .engram_budget_clock()
            .advance(ENGRAM_CONTROL_SETTLE_TIMEOUT);
        gate.release();
        let error = reset
            .join()
            .unwrap()
            .err()
            .expect("an expired reset must refuse the settings replacement");
        assert_eq!(error.kind, Some(ApiErrorKind::EngramProjectResetDeadline));
        assert!(error.project_reset.as_ref().unwrap().deadline_expired);
    });
    assert!(
        transport.requests().is_empty(),
        "expired budget sends no fresh control operation"
    );
    let inner = state.inner.lock().unwrap();
    assert!(
        !inner
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap()
            .turn_gated_control
    );
    assert!(!inner.engram_project_resets.contains(&project));
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(!record.engram.project_reset_in_progress);
    assert!(record.engram_queue_schedule.activation.is_some());
    assert_eq!(record.queued_prompts.len(), 1);
}

#[test]
fn an_activation_cannot_admit_a_replacement_head_or_bypass_a_pause() {
    for replace in [true, false] {
        let (state, session, receiver, transport) = root_fixture([]);
        queue_test_engram_prompt(
            &state,
            &session,
            "original head",
            QueuedPromptSource::User,
            None,
        );
        let owner = {
            let inner = state.inner.lock().unwrap();
            EngramQueuedAdmissionOwner::capture(
                &inner.sessions[inner.find_session_index(&session).unwrap()],
            )
            .unwrap()
        };
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            if replace {
                record.queued_prompts.clear();
                sync_pending_prompts(record);
            } else {
                record.set_auto_dispatch_blocked(true);
                record.engram.operator_paused = true;
            }
            state.commit_locked(&mut inner).unwrap();
        }
        if replace {
            queue_test_engram_prompt(
                &state,
                &session,
                "replacement head",
                QueuedPromptSource::User,
                None,
            );
        }
        assert!(
            state
                .dispatch_next_queued_turn_for_activation(&session, owner)
                .unwrap()
                .is_none()
        );
        assert!(transport.requests().is_empty());
        assert!(receiver.try_recv().is_err());
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(
            record.queued_prompts.front().unwrap().pending_prompt.text,
            if replace {
                "replacement head"
            } else {
                "original head"
            }
        );
    }
}

#[test]
fn a_pause_during_activation_evaluation_still_prevents_promotion_and_begin() {
    pause_during_activation_evaluation(false);
}

#[test]
fn scripted_clock_activation_recapture_completes_without_reentering_state() {
    pause_during_activation_evaluation(true);
}

fn pause_during_activation_evaluation(pin_scripted_wall: bool) {
    let (state, session, receiver, transport) =
        root_fixture([bind_reply("pause-token"), grant_reply("pause-grant")]);
    let clock = state.engram_budget_clock();
    assert!(matches!(&clock, EngramBudgetClock::Scripted(_)));
    if pin_scripted_wall {
        clock.pin_wall();
    }
    queue_test_engram_prompt(
        &state,
        &session,
        "pause while evaluating",
        QueuedPromptSource::User,
        None,
    );
    let owner = {
        let inner = state.inner.lock().unwrap();
        EngramQueuedAdmissionOwner::capture(
            &inner.sessions[inner.find_session_index(&session).unwrap()],
        )
        .unwrap()
    };
    let paused_state = state.clone();
    let paused_session = session.clone();
    let paused_clock = clock.clone();
    TEST_MAILBOX_RELAY_AFTER_EVALUATE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            if pin_scripted_wall {
                paused_clock.advance(Duration::from_secs(7));
            }
            let mut inner = paused_state.inner.lock().unwrap();
            let index = inner.find_session_index(&paused_session).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            record.set_auto_dispatch_blocked(true);
            record.engram.operator_paused = true;
            paused_state.commit_locked(&mut inner).unwrap();
        }));
    });
    state.run_engram_retry_attempt(
        EngramDueRetry {
            session_id: session.clone(),
            owner: owner.clone().into(),
            kind: EngramRetryKind::Activation,
            due_at: state.engram_budget_clock().wall_now().to_rfc3339(),
        },
        state.engram_budget_clock().wall_now(),
        state.engram_retry_slots.try_acquire().unwrap(),
    );
    assert!(receiver.try_recv().is_err());
    assert!(
        transport
            .requests()
            .iter()
            .all(|request| request.request["operation"] != "turn_begin")
    );
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(record.orchestrator_auto_dispatch_blocked && record.engram.operator_paused);
    assert_eq!(record.queued_prompts.len(), 1);
    let next_owner = EngramQueuedAdmissionOwner::capture(record).unwrap();
    assert_eq!(next_owner.prompt_id, owner.prompt_id);
    assert_eq!(next_owner.active_turn_generation, owner.active_turn_generation);
    assert_eq!(next_owner.generation, owner.generation + 1);
    assert_eq!(
        record
            .engram_queue_schedule
            .activation
            .as_ref()
            .map(|(retained, _)| retained),
        Some(&next_owner)
    );
    if pin_scripted_wall {
        let due_at = &record.engram_queue_schedule.activation.as_ref().unwrap().1;
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339(due_at).unwrap(),
            clock.wall_now()
        );
    }
    assert_eq!(state.engram_retry_slots.in_flight(), 0);
}

#[test]
fn an_activation_superseded_during_evaluation_cannot_register_a_successor_or_new_reset() {
    for new_reset in [false, true] {
        let (state, session, receiver, transport) =
            root_fixture([bind_reply("superseded-token"), grant_reply("superseded-grant")]);
        queue_test_engram_prompt(
            &state, &session, "original head", QueuedPromptSource::User, None,
        );
        let owner = {
            let inner = state.inner.lock().unwrap();
            EngramQueuedAdmissionOwner::capture(
                &inner.sessions[inner.find_session_index(&session).unwrap()],
            ).unwrap()
        };
        let changed_state = state.clone();
        let changed_session = session.clone();
        TEST_MAILBOX_RELAY_AFTER_EVALUATE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                let mut inner = changed_state.inner.lock().unwrap();
                let index = inner.find_session_index(&changed_session).unwrap();
                if new_reset {
                    let project = inner.sessions[index].session.project_id.clone().unwrap();
                    inner.claim_engram_project_reset(&project, true).unwrap();
                    let record = inner.session_mut_by_index(index).unwrap();
                    record.engram.project_reset_in_progress = true;
                    record.engram.dispatch_generation += 1;
                } else {
                    let record = inner.session_mut_by_index(index).unwrap();
                    let head = record.queued_prompts.front_mut().unwrap();
                    head.pending_prompt.id = "replacement-prompt".to_owned();
                    head.pending_prompt.text = "replacement head".to_owned();
                    sync_pending_prompts(record);
                }
                changed_state.commit_locked(&mut inner).unwrap();
            }));
        });
        state.run_engram_retry_attempt(
            EngramDueRetry {
                session_id: session.clone(), owner: owner.into(), kind: EngramRetryKind::Activation,
                due_at: state.engram_budget_clock().wall_now().to_rfc3339(),
            },
            state.engram_budget_clock().wall_now(),
            state.engram_retry_slots.try_acquire().unwrap(),
        );
        assert!(receiver.try_recv().is_err());
        assert!(transport.requests().iter()
            .all(|request| request.request["operation"] != "turn_begin"));
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.engram_queue_schedule.activation.is_none());
        assert_eq!(record.queued_prompts.len(), 1);
        if new_reset {
            assert!(record.engram.project_reset_in_progress);
            assert_eq!(record.queued_prompts.front().unwrap().pending_prompt.text, "original head");
        } else {
            assert_eq!(record.queued_prompts.front().unwrap().pending_prompt.id, "replacement-prompt");
        }
        assert_eq!(state.engram_retry_slots.in_flight(), 0);
    }
}

#[test]
fn tier_reset_unknown_checkpoint_keeps_original_custody_across_abort_and_reload() {
    let (state, session, _receiver, transport) = root_fixture([
        bind_reply("original-token"),
        grant_reply("original-grant"),
        begin_reply("original-grant"),
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline(
            "original reply lost",
        ))),
    ]);
    let (project, mut settings) = tier_reset_settings(&state, &session, true);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    // A historical begun grant may remain while Base is selected. Enabling
    // enforcement must settle it under its original identity, not erase it.
    {
        let mut inner = state.inner.lock().unwrap();
        inner
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .unwrap()
            .engram
            .as_mut()
            .unwrap()
            .turn_gated_control = false;
        state.commit_locked(&mut inner).unwrap();
    }
    settings.turn_gated_control = true;
    let error = state
        .update_project_engram_settings(&project, settings)
        .err()
        .expect("an uncertain checkpoint must refuse the settings replacement");
    assert_eq!(error.kind, Some(ApiErrorKind::EngramProjectResetRefused));
    let original = {
        let inner = state.inner.lock().unwrap();
        assert!(!inner.engram_project_resets.contains(&project));
        assert!(
            !inner
                .find_project(&project)
                .unwrap()
                .engram
                .as_ref()
                .unwrap()
                .turn_gated_control
        );
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(
            record.engram.active_grant_id.as_deref(),
            Some("original-grant")
        );
        assert!(engram_session_obligation_locked(&inner, &session).is_some());
        let operation = inner
            .engram_session_obligations
            .checkpoint_operations
            .last()
            .unwrap();
        assert!(operation.has_uncertain_checkpoint_history());
        serde_json::to_value(&operation.original).unwrap()
    };
    let restored = load_state(&state.persistence_path).unwrap().unwrap();
    let operation = restored
        .engram_session_obligations
        .checkpoint_operations
        .last()
        .unwrap();
    assert_eq!(serde_json::to_value(&operation.original).unwrap(), original);
    assert!(operation.has_uncertain_checkpoint_history());
    assert!(engram_session_obligation_locked(&restored, &session).is_some());
    assert_eq!(
        transport
            .requests()
            .iter()
            .filter(|r| r.request["operation"] == "turn_checkpoint")
            .count(),
        1
    );
}

#[test]
fn releasing_a_reset_preserves_its_live_checkpoint_and_late_completion_cannot_release_the_next_generation()
 {
    let (state, session, _receiver, _transport) = root_fixture([
        bind_reply("live-token"),
        grant_reply("live-grant"),
        begin_reply("live-grant"),
    ]);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    let (project, generation, claim, target) = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let project = inner.sessions[index].session.project_id.clone().unwrap();
        let mut target = project_engram_binding_target_locked(&inner, &session)
            .unwrap()
            .unwrap();
        let generation = inner.claim_engram_project_reset(&project, true).unwrap();
        target.project_reset_owner_generation = Some(generation);
        let record = inner.session_mut_by_index(index).unwrap();
        record.engram.project_reset_in_progress = true;
        assert!(record.engram.begin_checkpoint(Some(generation)));
        (
            project.clone(),
            generation,
            inner
                .engram_project_reset_claim(&project, generation)
                .unwrap(),
            target,
        )
    };
    let (step, gate) = gated_engram_step("turn_checkpoint", checkpoint_reply("live-grant"));
    state.install_control_test_transport(GatedEngramControlTransport::new([step]));
    std::thread::scope(|scope| {
        let checkpoint =
            scope.spawn(|| state.checkpoint_engram_project_reset_target(&target, &claim, &mut None));
        gate.wait();
        state.release_engram_project_reset_fence(
            &project,
            generation,
            std::slice::from_ref(&session),
        );
        let next = {
            let mut inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(
                record.engram.checkpoint_in_progress,
                "live receipt custody keeps its owner"
            );
            assert_eq!(record.engram.checkpoint_owner_generation, Some(generation));
            inner.claim_engram_project_reset(&project, true).unwrap()
        };
        gate.release();
        let _result = checkpoint.join().unwrap();
        state.release_engram_project_reset_fence(
            &project,
            generation,
            std::slice::from_ref(&session),
        );
        assert!(
            state
                .inner
                .lock()
                .unwrap()
                .engram_project_resets
                .is_owned_by(&project, next)
        );
        state.release_engram_project_reset_fence(&project, next, std::slice::from_ref(&session));
    });
}

/// Whether `preview` names the fence's claim time as `since HH:MM:SSZ`.
fn names_since_time(preview: &str) -> bool {
    preview.split("since ").nth(1).is_some_and(|rest| {
        let time = rest.as_bytes();
        time.len() >= 9
            && [0, 1, 3, 4, 6, 7]
                .iter()
                .all(|&at| time[at].is_ascii_digit())
            && time[2] == b':'
            && time[5] == b':'
            && time[8] == b'Z'
    })
}

#[test]
fn a_held_project_settings_change_tells_an_idle_session_with_a_queued_wake_why_it_waits() {
    let (mut state, session, receiver, _transport) = root_fixture([
        bind_reply("reset-bind"),
        grant_reply("released-grant"),
        begin_reply("released-grant"),
    ]);
    let (project, mut settings, root) = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let project = inner.sessions[index].session.project_id.clone().unwrap();
        let project_record = inner.projects.iter_mut().find(|p| p.id == project).unwrap();
        let root = PathBuf::from(&project_record.root_path);
        fs::write(root.join(".engram-project"), "fixture-ok\n").unwrap();
        let settings = project_record.engram.as_mut().unwrap();
        settings.binary_path = Some(
            real_engram_control_fixture_path()
                .to_string_lossy()
                .into_owned(),
        );
        (project, settings.clone(), root)
    };
    state.mailbox_store = Arc::new(
        MailboxStore::open(&resolve_coordination_persistence_path(
            state.persistence_path.as_ref(),
        ))
        .unwrap(),
    );
    let sender = test_session_id(&state, Agent::Claude);
    // A new home is a reset-required change: the settings transaction holds
    // the project's reset fence while it runs.
    let new_home = root.join("reset-wait-home");
    fs::create_dir_all(&new_home).unwrap();
    settings.home = Some(new_home.to_string_lossy().into_owned());
    let reset_gate = gate_next_engram_project_reset_fence(&project);
    std::thread::scope(|scope| {
        let reset = scope.spawn(|| {
            let result = state.update_project_engram_settings(&project, settings);
            abort_engram_project_reset_fence_gate(&project, &fence_gate_detail(&result));
            result
        });
        let observed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            reset_gate.wait_until_entered();
            // A mailbox wake arrives while the settings change holds the fence.
            state
                .append_mailbox_message_and_notify(
                    &sender,
                    SendMailboxMessageRequest {
                        target_session_id: session.clone(),
                        message: "a wake during the settings change".to_owned(),
                        idempotency_key: "reset-wait-wake".to_owned(),
                        topic: Some("reset wait".to_owned()),
                        state_stamp: None,
                        class: Some("routine".to_owned()),
                    },
                )
                .unwrap();
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            (
                record.session.status,
                record.orchestrator_auto_dispatch_blocked,
                record
                    .queued_prompts
                    .iter()
                    .map(|queued| queued.source)
                    .collect::<Vec<_>>(),
                record.session.preview.clone(),
            )
        }));
        let public_preview = state
            .summary_snapshot()
            .sessions
            .into_iter()
            .find(|record| record.id == session)
            .map(|record| record.preview);
        let no_provider_while_fenced = receiver.try_recv().is_err();
        reset_gate.release();
        let reset_result = reset.join();
        let (status, paused, queued, preview) =
            observed.unwrap_or_else(|error| std::panic::resume_unwind(error));
        reset_result
            .expect("reset worker should settle")
            .expect("the settings change should finish");
        // The stall shape: idle, unpaused, one queued mailbox wake, nothing
        // sent to the provider.
        assert_eq!(status, SessionStatus::Idle);
        assert!(!paused, "the session is not paused");
        assert_eq!(queued, vec![QueuedPromptSource::Mailbox]);
        assert!(no_provider_while_fenced);
        assert!(
            preview.starts_with(PROJECT_RESET_WAIT_PREFIX),
            "an idle, unpaused session held by a project settings change must say why it waits: {preview:?}"
        );
        assert!(
            names_since_time(&preview),
            "the reason names when the settings change began: {preview:?}"
        );
        assert_eq!(
            public_preview.as_deref(),
            Some(preview.as_str()),
            "the public session shows the same reason"
        );
    });
    // Released: the reason is gone.
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(!record.engram.project_reset_in_progress);
        assert!(
            !record
                .session
                .preview
                .starts_with(PROJECT_RESET_WAIT_PREFIX),
            "the release replaces the waiting reason: {:?}",
            record.session.preview
        );
    }
    // Every step the fence was held for was written with its duration, so a
    // reset that keeps its fence names the step it is in.
    let lines = engram_project_reset_phase_lines(&project);
    let prefix = format!("engram> project={project} reset generation ");
    assert!(
        lines.iter().all(|line| line.starts_with(&prefix)),
        "{lines:#?}"
    );
    let written = |event: &str| lines.iter().any(|line| line.contains(event));
    assert!(written(" fence claimed"), "{lines:#?}");
    assert!(
        written(" fence released and eligible queues registered (fence held at most "),
        "{lines:#?}"
    );
    for phase in [
        "quiescence".to_owned(),
        "checkpoint claims".to_owned(),
        "timed-out checkpoints".to_owned(),
        "commit".to_owned(),
        "authority revocation".to_owned(),
        format!("control shutdown session={session}"),
        "MCP runtime shutdown".to_owned(),
        "MCP runtime finalize".to_owned(),
        "release and register".to_owned(),
    ] {
        assert!(
            written(&format!(" phase {phase} started (fence held ")),
            "{phase} start: {lines:#?}"
        );
        assert!(
            written(&format!(" phase {phase} took ")),
            "{phase} end: {lines:#?}"
        );
    }
    assert!(
        lines
            .last()
            .is_some_and(|line| line.contains(" finished (fence held ")),
        "{lines:#?}"
    );
}

#[test]
fn a_paused_or_busy_session_keeps_its_own_preview_while_the_fence_is_held() {
    for case in ["paused", "busy"] {
        let (state, session, _receiver, transport) = root_fixture([]);
        let (project, generation, original) = {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let project = inner.sessions[index].session.project_id.clone().unwrap();
            let generation = inner.engram_project_resets.claim(&project).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            record.engram.project_reset_in_progress = true;
            let original = if case == "paused" {
                record.set_auto_dispatch_blocked(true);
                "Paused by the test.".to_owned()
            } else {
                record.session.status = SessionStatus::Active;
                "Working on the test turn.".to_owned()
            };
            record.session.preview = original.clone();
            (project, generation, original)
        };
        queue_test_engram_prompt(
            &state,
            &session,
            "a wake during the settings change",
            QueuedPromptSource::Mailbox,
            None,
        );
        assert!(
            state
                .dispatch_next_queued_turn(&session, false)
                .unwrap()
                .is_none(),
            "{case}: the fence holds the queue"
        );
        {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert_eq!(
                record.session.preview, original,
                "{case}: its own preview stays"
            );
        }
        assert!(
            transport.requests().is_empty(),
            "{case}: nothing reached Engram"
        );
        let mut inner = state.inner.lock().unwrap();
        assert!(inner.engram_project_resets.release(&project, generation));
    }
}

/// An idle, unpaused session behind a held fence, with the given preview
/// before it, a queued mailbox wake, and the waiting reason the drain showed.
/// Returns the project, the fence generation, and the revision the reason
/// was committed at.
fn fenced_session_showing_the_reason(
    state: &AppState,
    session: &str,
    original: &str,
) -> (String, u64, u64) {
    let (project, generation) = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(session).unwrap();
        let project = inner.sessions[index].session.project_id.clone().unwrap();
        let generation = inner.engram_project_resets.claim(&project).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.engram.project_reset_in_progress = true;
        record.session.preview = original.to_owned();
        (project, generation)
    };
    queue_test_engram_prompt(
        state,
        session,
        "a wake during the settings change",
        QueuedPromptSource::Mailbox,
        None,
    );
    let queued_revision = state.inner.lock().unwrap().revision;
    assert!(
        state
            .dispatch_next_queued_turn(session, false)
            .unwrap()
            .is_none()
    );
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    assert!(
        record
            .session
            .preview
            .starts_with(PROJECT_RESET_WAIT_PREFIX),
        "{:?}",
        record.session.preview
    );
    // A client that adopted the queued wake's revision adopts only a newer
    // one: the reason is committed at a revision past it.
    assert!(
        inner.revision > queued_revision,
        "the waiting reason must be committed at a newer revision than {queued_revision}, got {}",
        inner.revision
    );
    (project, generation, inner.revision)
}

/// Keeps the release's drain from starting a turn, so the preview it leaves
/// is the restored one.
fn hold_the_drain_with_a_pending_stop(state: &AppState, session: &str) {
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(session).unwrap();
    inner
        .session_mut_by_index(index)
        .unwrap()
        .runtime_stop_in_progress = true;
}

#[test]
fn the_release_restores_the_replaced_preview_at_a_newer_revision() {
    let (state, session, _receiver, transport) = root_fixture([]);
    let original = "The preview before the settings change.";
    let (project, generation, shown_revision) =
        fenced_session_showing_the_reason(&state, &session, original);
    hold_the_drain_with_a_pending_stop(&state, &session);
    state.release_engram_project_reset_fence(&project, generation, std::slice::from_ref(&session));
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.session.preview, original);
    assert!(record.engram.project_reset_wait_preview.is_none());
    assert!(
        inner.revision > shown_revision,
        "the restored preview must be committed at a newer revision than {shown_revision}, got {}",
        inner.revision
    );
    assert_eq!(record.queued_prompts.len(), 1, "no turn started");
    assert!(transport.requests().is_empty());
}

#[test]
fn an_aborted_settings_change_drains_what_it_held_and_restores_the_preview() {
    // The aborted change's wake starts its turn, as after a release.
    let (state, session, _receiver, transport) = root_fixture([
        bind_reply("abort-bind"),
        grant_reply("abort-grant"),
        begin_reply("abort-grant"),
    ]);
    let original = "The preview before the aborted settings change.";
    let (project, generation, shown_revision) =
        fenced_session_showing_the_reason(&state, &session, original);
    // A session that joined the project while the fence was held is not in
    // the list the settings change took at its claim, yet shows the reason.
    let root = {
        let inner = state.inner.lock().unwrap();
        let project_record = inner.projects.iter().find(|p| p.id == project).unwrap();
        PathBuf::from(&project_record.root_path)
    };
    let joined = create_test_project_session(&state, Agent::Codex, &project, &root);
    let joined_original = "The joined session's own preview.";
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&joined).unwrap();
        inner.session_mut_by_index(index).unwrap().session.preview = joined_original.to_owned();
    }
    queue_test_engram_prompt(
        &state,
        &joined,
        "a wake for the joined session",
        QueuedPromptSource::Mailbox,
        None,
    );
    assert!(
        state
            .dispatch_next_queued_turn(&joined, false)
            .unwrap()
            .is_none()
    );
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&joined).unwrap()];
        assert!(
            record
                .session
                .preview
                .starts_with(PROJECT_RESET_WAIT_PREFIX),
            "{:?}",
            record.session.preview
        );
    }
    // The joined session's drain starts no turn, so its restored preview is
    // the one left.
    hold_the_drain_with_a_pending_stop(&state, &joined);
    state
        .abort_engram_project_reset(&project, generation, std::slice::from_ref(&session), &[])
        .unwrap();
    state.engram_abort_retry_tick(state.engram_budget_clock().wall_now());
    let inner = state.inner.lock().unwrap();
    assert!(!inner.engram_project_resets.contains(&project));
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(
        record.queued_prompts.is_empty(),
        "the wake the aborted change held starts its turn: {:?}",
        record.queued_prompts
    );
    assert!(
        !record
            .session
            .preview
            .starts_with(PROJECT_RESET_WAIT_PREFIX),
        "{:?}",
        record.session.preview
    );
    let joined_record = &inner.sessions[inner.find_session_index(&joined).unwrap()];
    assert_eq!(
        joined_record.session.preview, joined_original,
        "the abort clears the reason of every session the fence held"
    );
    assert!(
        inner.revision > shown_revision,
        "the restored preview must be committed at a newer revision than {shown_revision}, got {}",
        inner.revision
    );
    assert!(
        transport
            .requests()
            .iter()
            .any(|request| request.request["operation"] == "turn_begin"),
        "the drained wake was admitted"
    );
}

#[test]
fn a_failed_settings_commit_restores_the_replaced_preview() {
    let (mut state, session, receiver, _transport) = root_fixture([]);
    let (project, mut settings, root) = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let project = inner.sessions[index].session.project_id.clone().unwrap();
        let project_record = inner.projects.iter_mut().find(|p| p.id == project).unwrap();
        let root = PathBuf::from(&project_record.root_path);
        fs::write(root.join(".engram-project"), "fixture-ok\n").unwrap();
        let settings = project_record.engram.as_mut().unwrap();
        settings.binary_path = Some(
            real_engram_control_fixture_path()
                .to_string_lossy()
                .into_owned(),
        );
        (project, settings.clone(), root)
    };
    state.mailbox_store = Arc::new(
        MailboxStore::open(&resolve_coordination_persistence_path(
            state.persistence_path.as_ref(),
        ))
        .unwrap(),
    );
    let sender = test_session_id(&state, Agent::Claude);
    let before = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session).unwrap()]
            .session
            .preview
            .clone()
    };
    // The release after the rollback drains the session; a pending stop keeps
    // that drain from starting a turn, so the restored preview is the one left.
    hold_the_drain_with_a_pending_stop(&state, &session);
    // The settings change commits to a store that rejects it; the wake keeps
    // the working store.
    state.shutdown_persist_blocking();
    let mut reset_state = state.clone();
    let failed_path = root.join("failed-reset-wait-store.sqlite");
    fs::create_dir_all(&failed_path).unwrap();
    reset_state.persistence_path = Arc::new(failed_path);
    let new_home = root.join("failed-reset-wait-home");
    fs::create_dir_all(&new_home).unwrap();
    settings.home = Some(new_home.to_string_lossy().into_owned());
    let reset_gate = gate_next_engram_project_reset_fence(&project);
    std::thread::scope(|scope| {
        let reset = scope.spawn(|| {
            let result = reset_state.update_project_engram_settings(&project, settings);
            abort_engram_project_reset_fence_gate(&project, &fence_gate_detail(&result));
            result
        });
        let shown = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            reset_gate.wait_until_entered();
            state
                .append_mailbox_message_and_notify(
                    &sender,
                    SendMailboxMessageRequest {
                        target_session_id: session.clone(),
                        message: "a wake during the failing settings change".to_owned(),
                        idempotency_key: "failed-reset-wait-wake".to_owned(),
                        topic: Some("failed reset wait".to_owned()),
                        state_stamp: None,
                        class: Some("routine".to_owned()),
                    },
                )
                .unwrap();
            let inner = state.inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&session).unwrap()]
                .session
                .preview
                .clone()
        }));
        reset_gate.release();
        let result = reset.join();
        let shown = shown.unwrap_or_else(|error| std::panic::resume_unwind(error));
        assert!(
            shown.starts_with(PROJECT_RESET_WAIT_PREFIX),
            "the wake showed the reason while the fence was held: {shown:?}"
        );
        let error = match result.expect("reset worker should settle") {
            Err(error) => error,
            Ok(_) => panic!("the rejecting store must fail the settings commit"),
        };
        assert!(
            error
                .message
                .contains("failed to persist Engram project settings"),
            "{}",
            error.message
        );
    });
    let inner = state.inner.lock().unwrap();
    assert!(!inner.engram_project_resets.contains(&project));
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(
        record.session.preview, before,
        "the rolled-back settings change restores the replaced preview"
    );
    assert!(receiver.try_recv().is_err(), "nothing reached the provider");
}

#[test]
fn a_direct_prompt_queued_behind_the_fence_shows_the_reason() {
    let (state, session, _receiver, transport) = root_fixture([]);
    let generation = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let project = inner.sessions[index].session.project_id.clone().unwrap();
        let generation = inner.engram_project_resets.claim(&project).unwrap();
        inner
            .session_mut_by_index(index)
            .unwrap()
            .engram
            .project_reset_in_progress = true;
        (project, generation)
    };
    let result = state
        .dispatch_turn(
            &session,
            SendMessageRequest {
                text: "a direct prompt during the settings change".to_owned(),
                expanded_text: None,
                attachments: vec![],
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .unwrap();
    assert!(matches!(result, DispatchTurnResult::Queued));
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.queued_prompts.len(), 1);
        assert!(
            record
                .session
                .preview
                .starts_with(PROJECT_RESET_WAIT_PREFIX),
            "a prompt the fence queues says why it waits: {:?}",
            record.session.preview
        );
    }
    assert!(transport.requests().is_empty());
    let (project, generation) = generation;
    let mut inner = state.inner.lock().unwrap();
    assert!(inner.engram_project_resets.release(&project, generation));
}

#[test]
fn canceling_the_last_queued_prompt_behind_the_fence_restores_the_preview() {
    let (state, session, _receiver, transport) = root_fixture([]);
    let original = "The preview before the canceled wake.";
    let (project, generation, shown_revision) =
        fenced_session_showing_the_reason(&state, &session, original);
    let prompt = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session).unwrap()].queued_prompts[0]
            .pending_prompt
            .id
            .clone()
    };
    state.cancel_queued_prompt(&session, &prompt).unwrap();
    let mut inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(record.queued_prompts.is_empty());
    assert_eq!(
        record.session.preview, original,
        "no queued prompt is left to wait for"
    );
    assert!(
        inner.revision > shown_revision,
        "the restored preview must be committed at a newer revision than {shown_revision}, got {}",
        inner.revision
    );
    assert!(transport.requests().is_empty());
    assert!(inner.engram_project_resets.release(&project, generation));
}

#[test]
fn the_release_with_runtime_revocation_restores_the_preview_unless_it_lost_the_fence() {
    // Released by its own generation: the reason goes, at a newer revision.
    let (state, session, _receiver, transport) = root_fixture([]);
    let original = "The preview before the authority rotation.";
    let (project, generation, shown_revision) =
        fenced_session_showing_the_reason(&state, &session, original);
    hold_the_drain_with_a_pending_stop(&state, &session);
    let release = state.release_engram_project_and_runtime_revocation_fences(
        &project,
        generation,
        std::slice::from_ref(&session),
        EngramMcpRuntimeRevocationBatch::default(),
    );
    assert!(!release.project_fence_release_failed);
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.session.preview, original);
        assert!(
            inner.revision > shown_revision,
            "the restored preview must be committed at a newer revision than {shown_revision}, got {}",
            inner.revision
        );
    }
    assert!(transport.requests().is_empty());

    // Its generation was lost to a newer one: that fence still holds the
    // session, so the reason stays.
    let (state, session, _receiver, _transport) = root_fixture([]);
    let (project, generation, _) =
        fenced_session_showing_the_reason(&state, &session, "The preview before the lost fence.");
    let newer = {
        let mut inner = state.inner.lock().unwrap();
        assert!(inner.engram_project_resets.release(&project, generation));
        inner.engram_project_resets.claim(&project).unwrap()
    };
    let release = state.release_engram_project_and_runtime_revocation_fences(
        &project,
        generation,
        std::slice::from_ref(&session),
        EngramMcpRuntimeRevocationBatch::default(),
    );
    assert!(release.project_fence_release_failed);
    let mut inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(
        record
            .session
            .preview
            .starts_with(PROJECT_RESET_WAIT_PREFIX),
        "the newer fence still holds the session: {:?}",
        record.session.preview
    );
    assert!(inner.engram_project_resets.release(&project, newer));
}

#[test]
fn an_authority_only_settings_change_writes_its_phases_while_it_holds_the_fence() {
    // Rotating only the work-authority grant holds the project fence in the
    // authority branch, not the reset below it; its steps are written too.
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("reset-wait-authority-phases");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join(".engram-project"), "fixture-ready\n").unwrap();
    let project = create_test_project(&state, &root, "Reset wait authority phases");
    let session = create_test_project_session(&state, Agent::Claude, &project, &root);
    let mut enabled = real_fixture_engram_settings(&root);
    enabled.work_authority_grant = Some("grant-old".to_owned());
    state
        .update_project_engram_settings(&project, enabled.clone())
        .expect("fixture settings should enable Engram");
    attach_engram_mcp_test_runtime(&state, &session);
    // The enabling change is a reset of its own; only the rotation is read.
    engram_project_reset_phase_lines(&project);
    let mut rotated = enabled;
    rotated.work_authority_grant = Some("grant-new".to_owned());
    state
        .update_project_engram_settings(&project, rotated)
        .expect("the rotation should finish");
    let lines = engram_project_reset_phase_lines(&project);
    let written = |event: &str| lines.iter().any(|line| line.contains(event));
    assert!(written(" fence claimed"), "{lines:#?}");
    for phase in [
        "authority preflight",
        "commit",
        "authority revocation",
        "release and register",
    ] {
        assert!(
            written(&format!(" phase {phase} started (fence held ")),
            "{phase} start: {lines:#?}"
        );
        assert!(
            written(&format!(" phase {phase} took ")),
            "{phase} end: {lines:#?}"
        );
    }
    assert!(
        written(" fence released and eligible queues registered (fence held at most "),
        "{lines:#?}"
    );
    assert!(
        lines
            .last()
            .is_some_and(|line| line.contains(" finished (fence held ")),
        "{lines:#?}"
    );
}

#[test]
fn a_waiting_reason_saved_before_a_restart_loads_as_the_idle_preview() {
    // The fence lives in memory only, so no reason survives a restart.
    let saved = engram_project_reset_wait_preview(Some(chrono::Utc::now()));
    assert_eq!(
        engram_project_reset_wait_preview_after_restart(saved),
        "Ready for a prompt."
    );
    assert_eq!(
        engram_project_reset_wait_preview_after_restart("Last reply.".to_owned()),
        "Last reply."
    );
}
