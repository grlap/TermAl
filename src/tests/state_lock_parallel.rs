//! Agreed disposable parallel model: real router, mailbox SQLite and production
//! publication/reclamation/persist owners. These dimensions are not host census.
use super::*;
use std::sync::atomic::AtomicUsize;

const MODEL_SESSIONS: usize = 6_793;
const MODEL_LANES: usize = 8;
const MODEL_PAIRS: usize = 512;
const MODEL_WARM_ROUNDS: usize = 90;
/// One plan lock and one per candidate session, in at most two collection passes.
const MODEL_MAX_COLLECT_LOCKS: usize = 2 * (1 + MODEL_LANES);
/// One 16 KiB message with its encoding.
const MODEL_MAX_RETAINED_MESSAGE_BYTES: usize = 17 * 1024;
/// Session metadata with no transcript in it, before its checkout path.
const MODEL_MAX_SESSION_METADATA_BYTES: usize = 4 * 1024;

#[derive(Clone, Serialize)]
struct OperationSample {
    phase: &'static str,
    lane: usize,
    iteration: usize,
    kind: &'static str,
    requested_us: u64,
    elapsed_us: u64,
    status: Option<u16>,
    error: Option<String>,
}

#[derive(Clone, Serialize)]
struct HoldSample {
    phase: &'static str,
    #[serde(skip)]
    reported_at: std::time::Instant,
    released_us: u64,
    held_us: u64,
    waited_us: u64,
    file: &'static str,
    line: u32,
    thread_id: String,
    snapshot_cut_count: usize,
    snapshot_cut_us: u64,
}

fn micros(duration: Duration) -> u64 {
    duration.as_micros().min(u64::MAX as u128) as u64
}

fn nonempty_committed_tick(tick: &PersistObservedTick) -> bool {
    tick.success
        && tick.counts.metadata_transaction_committed
        && tick
            .counts
            .metadata_fields
            .iter()
            .any(|field| field.committed_upserts > 0 || field.committed_deletes > 0)
}

/// Waits, with every client and writer finished, for a tick that started after
/// them. Nothing changes under such a tick, so no session is deferred and every
/// edit made so far has been written once it ends. It has started after them
/// because it follows a tick that finished after them.
fn model_persist_quiet_point(
    state: &AppState,
    receiver: &mpsc::Receiver<(std::time::Instant, PersistObservedTick)>,
) -> Vec<(std::time::Instant, PersistObservedTick)> {
    let quiet_from = std::time::Instant::now();
    let mut samples = Vec::new();
    let mut one_finished_after = false;
    loop {
        state.persist_tx.send(PersistRequest::Delta).unwrap();
        let (at, tick) = receiver
            .recv_timeout(phase_sync::DEADLOCK_GUARD)
            .expect("the production writer answers a delta with a tick");
        samples.push((at, tick));
        if one_finished_after {
            return samples;
        }
        one_finished_after = at > quiet_from;
    }
}

fn seed_model(root: &FsPath) -> (PathBuf, Vec<String>) {
    let workdir = root.to_string_lossy().into_owned();
    let mut inner = bootstrap_default_local_state(&workdir);
    inner.sessions.clear();
    let project_id = inner.projects.first().map(|p| p.id.clone());
    let mut ids = Vec::with_capacity(MODEL_SESSIONS);
    for n in 0..MODEL_SESSIONS {
        let id = inner
            .create_session(
                Agent::Codex,
                Some(format!("Modeled {n}")),
                workdir.clone(),
                project_id.clone(),
                None,
            )
            .session
            .id;
        if n < MODEL_LANES {
            let index = inner.find_session_index(&id).unwrap();
            let record = &mut inner.sessions[index];
            record.session.messages = (0..64)
                .map(|m| Message::Text {
                    attachments: Vec::new(),
                    id: format!("{id}-message-{m}"),
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: "x".repeat(16 * 1024),
                    expanded_text: None,
                    source: None,
                })
                .collect();
            record.session.message_count = 64;
            record.message_positions = build_message_positions(&record.session.messages);
            record.set_auto_dispatch_blocked(true);
        }
        ids.push(id);
    }
    let store = EngramAuthorityStoreKey {
        database_path: root.join("modeled-authority.sqlite"),
        project_id: project_id
            .clone()
            .unwrap_or_else(|| "modeled-project".into()),
    };
    // Metadata volume only: no fixture executes authority calls or names a
    // live source root. Disabled control keeps the real reclamation plan local.
    for n in 0..MODEL_PAIRS {
        let claim = format!("modeled-claim-{n}");
        inner.engram_work_source_roots.push(EngramWorkSourceRoot {
            store: store.clone(),
            work_id: format!("modeled-work-{n}"),
            short_ref: format!("modeled-{n}"),
            claim_id: claim.clone(),
            claim_fence: 1,
            root: workdir.clone(),
            common_dir_key: root
                .join("modeled-common.git")
                .to_string_lossy()
                .into_owned(),
            named_by_session: ids[n].clone(),
            named_at: stamp_now(),
            generation: n as u64 + 1,
        });
        inner
            .engram_named_root_journal
            .push(EngramNamedRootJournal {
                store: store.clone(),
                claim_id: claim,
                read_binding: Some(EngramControlWorkBinding {
                    root_execution_id: format!("modeled-root-{n}"),
                    work_id: format!("modeled-work-{n}"),
                    run_id: format!("modeled-run-{n}"),
                    work_revision: 1,
                    claim_id: format!("modeled-claim-{n}"),
                    claim_fence: 1,
                }),
                confirmed: None,
                pending: None,
                obsolete: false,
                retirement: None,
                reconciliation: None,
            });
        let run_id = format!("modeled-test-{n}");
        inner.test_run_registrations.insert(
            run_id.clone(),
            TestRunRegistration {
                run_id,
                native_origin: None,
                native_completion: None,
                source_owner: None,
                project_id: project_id.clone(),
                run_directory: root
                    .join(format!("modeled-runs/{n}"))
                    .to_string_lossy()
                    .into_owned(),
                locator_identity: None,
                worktree: workdir.clone(),
                request_identity: format!("modeled-request-{n}"),
                initiating_session_id: ids[n].clone(),
                owning_session_id: ids[n].clone(),
                creator_pid: None,
                registered_at: test_run_timestamp(),
                started_at: None,
                preset: TestRunPreset::Focused,
                detached: Some(false),
                stage_names: vec!["focused".into()],
                declared: None,
                declared_native: None,
                declared_terminal: None,
                work_binding: None,
                named_root: None,
                authority_store: None,
                validated_unbound: true,
                monitoring_interrupted_at: None,
                verification_record: None,
            },
        );
    }
    inner.engram_source_root_generation = MODEL_PAIRS as u64;
    let path = root.join("termal.sqlite");
    persist_locked_state_to_sqlite(&path, &inner).unwrap();
    (path, ids)
}

struct BootObservationScope;
impl Drop for BootObservationScope {
    fn drop(&mut self) {
        TEST_BOOT_STATE_MUTEX.with(|hook| hook.borrow_mut().take());
        TEST_BOOT_PERSIST_TICK_SINK.with(|hook| hook.borrow_mut().take());
        TEST_BOOT_SKIP_CODEX_DISCOVERY.with(|skip| skip.set(false));
    }
}

struct Workers(AppState);
impl Drop for Workers {
    fn drop(&mut self) {
        self.0.shutdown_persist_blocking();
        self.0.shutdown_state_broadcast_blocking();
    }
}

async fn operation(
    app: &Router,
    path: String,
    body: Value,
    phase: &'static str,
    lane: usize,
    iteration: usize,
    kind: &'static str,
    origin: std::time::Instant,
) -> (OperationSample, Value) {
    let started = std::time::Instant::now();
    let request = Request::post(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let result = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        let response = app
            .clone()
            .oneshot(request)
            .await
            .map_err(|e| format!("router: {e}"))?;
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .map_err(|e| format!("body: {e}"))?;
        let body: Value = serde_json::from_slice(&bytes).map_err(|e| format!("JSON: {e}"))?;
        Ok::<_, String>((status, body))
    })
    .await;
    let (status, value, error) = match result {
        Ok(Ok((status, value))) => (Some(status), value, None),
        Ok(Err(error)) => (None, Value::Null, Some(error)),
        Err(_) => (
            None,
            Value::Null,
            Some("fixture deadlock guard expired".into()),
        ),
    };
    (
        OperationSample {
            phase,
            lane,
            iteration,
            kind,
            requested_us: micros(started.duration_since(origin)),
            elapsed_us: micros(started.elapsed()),
            status,
            error,
        },
        value,
    )
}

fn distribution(mut values: Vec<u64>) -> Value {
    if values.is_empty() {
        return json!({"count":0,"p99Us":null,"maxUs":null});
    }
    values.sort_unstable();
    let rank = (values.len() * 99).div_ceil(100).saturating_sub(1);
    json!({"count":values.len(),"p99Us":values[rank],"maxUs":values.last().unwrap()})
}

async fn model_round_boundary(barrier: &tokio::sync::Barrier) {
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, barrier.wait())
        .await
        .expect("all modeled lanes and the writer reach the round boundary");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn modeled_parallel_mailbox_resume_and_writer_keep_their_counts() {
    let timings = FixtureTimings::start("parallel_model");
    timings.phase("fixture_setup");
    let _temp_scope = TestTempRootDirectoryScope::repository_local();
    let root = TestTempRoot::create("modeled-state-parallel");
    timings.phase("seed_model");
    let (path, ids) = seed_model(root.path());
    timings.phase("boot_and_mailbox_setup");
    let origin = std::time::Instant::now();
    let phase_number = Arc::new(AtomicUsize::new(1));
    let holds = Arc::new(Mutex::new(Vec::<HoldSample>::new()));
    let (cut_tx, cut_rx) = mpsc::channel::<(std::time::Instant, Duration, std::thread::ThreadId)>();
    let (tick_tx, tick_rx) = mpsc::channel::<(std::time::Instant, PersistObservedTick)>();
    let hold_rows = holds.clone();
    let hold_phase = phase_number.clone();
    TEST_BOOT_STATE_MUTEX.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |inner| {
            inner.state_projection_cache.lock().unwrap().capture_sink = Some(cut_tx);
            StateMutex::new_with_diagnostic_reporter(
                inner,
                Duration::ZERO,
                Arc::new(move |diagnostic| {
                    if let StateMutexDiagnostic::Held {
                        held,
                        waited,
                        file,
                        line,
                        ..
                    } = diagnostic
                    {
                        let reported_at = std::time::Instant::now();
                        let sample = HoldSample {
                            phase: match hold_phase.load(Ordering::SeqCst) {
                                1 => "cold",
                                2 => "warm",
                                _ => "teardown",
                            },
                            reported_at,
                            released_us: micros(reported_at.duration_since(origin)),
                            held_us: micros(held),
                            waited_us: micros(waited),
                            file,
                            line,
                            thread_id: format!("{:?}", std::thread::current().id()),
                            snapshot_cut_count: 0,
                            snapshot_cut_us: 0,
                        };
                        hold_rows.lock().unwrap().push(sample);
                    }
                }),
            )
        }))
    });
    TEST_BOOT_SKIP_CODEX_DISCOVERY.with(|skip| skip.set(true));
    TEST_BOOT_PERSIST_TICK_SINK.with(|sink| *sink.borrow_mut() = Some(tick_tx));
    let boot_scope = BootObservationScope;
    let mut state = AppState::new_with_paths(
        root.path().to_string_lossy().into_owned(),
        path,
        root.path().join("orchestrators.json"),
    )
    .unwrap();
    state.agent_runtime_spawning_enabled = false;
    drop(boot_scope);
    let workers = Workers(state.clone());
    {
        let inner = state.inner.lock().unwrap();
        assert_eq!(inner.sessions.len(), MODEL_SESSIONS);
        assert_eq!(inner.engram_work_source_roots.len(), MODEL_PAIRS);
        assert_eq!(inner.engram_named_root_journal.len(), MODEL_PAIRS);
        assert_eq!(inner.test_run_registrations.len(), MODEL_PAIRS);
        for id in &ids[..MODEL_LANES] {
            let record = &inner.sessions[inner.find_session_index(id).unwrap()];
            assert_eq!(record.session.messages.len(), 64);
        }
    }
    let app = app_router(state.clone());
    let (api_phase_tx, api_phase_rx) = mpsc::channel();
    state.agent_readiness_cache.write().unwrap().phase_sink = Some(api_phase_tx);
    let mut mailboxes = Vec::new();
    let mut setup_operations = Vec::new();
    for lane in 0..MODEL_LANES {
        // Durable setup goes through the real mailbox store but creates no
        // wake/prompt: the modeled Resume queue must be paused and empty.
        let appended = state
            .mailbox_store
            .append(&MailboxAppendInput {
                sender_session_id: ids[MODEL_LANES].clone(),
                sender_name: "Modeled sender".into(),
                target_session_id: ids[lane].clone(),
                target_name: format!("Lane {lane}"),
                body: "modeled mailbox body".into(),
                idempotency_key: format!("modeled-mail-{lane}"),
                topic: None,
                state_stamp: None,
            })
            .unwrap();
        let (sample, page) = operation(
            &app,
            format!(
                "/api/sessions/{}/mailboxes/{}/read",
                ids[lane], appended.mailbox_id
            ),
            json!({"issueReceipt":true,"afterSequence":0}),
            "setup",
            lane,
            0,
            "read",
            origin,
        )
        .await;
        assert_eq!(sample.status, Some(200), "setup read: {page}");
        setup_operations.push(sample);
        let receipt = page["receipt"]
            .as_str()
            .expect("setup issued receipt")
            .to_owned();
        mailboxes.push((appended.mailbox_id.clone(), receipt));
    }
    let mut all_operations = Vec::new();
    let mut all_ticks = Vec::new();
    let mut receiver = tick_rx;
    let mut warm_started = None;
    for (phase_index, phase, iterations) in [(1, "cold", 10), (2, "warm", MODEL_WARM_ROUNDS)] {
        timings.phase(if phase_index == 1 {
            "cold_operations"
        } else {
            "warm_operations"
        });
        phase_number.store(phase_index, Ordering::SeqCst);
        if phase_index == 2 {
            warm_started = Some(std::time::Instant::now());
        }
        let barrier = Arc::new(tokio::sync::Barrier::new(MODEL_LANES + 1));
        let mut clients = Vec::new();
        for lane in 0..MODEL_LANES {
            let app = app.clone();
            let state = state.clone();
            let session = ids[lane].clone();
            let (mailbox, receipt) = mailboxes[lane].clone();
            let barrier = barrier.clone();
            clients.push(tokio::spawn(async move {
                let mut rows = Vec::new();
                model_round_boundary(&barrier).await;
                for iteration in 0..iterations {
                    let (row, _) = operation(
                        &app,
                        format!("/api/sessions/{session}/mailboxes/{mailbox}/read"),
                        json!({"issueReceipt":true,"afterSequence":0}),
                        phase,
                        lane,
                        iteration,
                        "read",
                        origin,
                    )
                    .await;
                    rows.push(row);
                    let (row, _) = operation(
                        &app,
                        format!("/api/sessions/{session}/mailboxes/{mailbox}/acknowledge"),
                        json!({"receipt":receipt}),
                        phase,
                        lane,
                        iteration,
                        "ack",
                        origin,
                    )
                    .await;
                    rows.push(row);
                    {
                        let mut inner = state.inner.lock().unwrap();
                        let index = inner.find_session_index(&session).unwrap();
                        let record = inner.session_mut_by_index(index).unwrap();
                        assert!(record.queued_prompts.is_empty());
                        record.set_auto_dispatch_blocked(true);
                    }
                    let (row, _) = operation(
                        &app,
                        format!("/api/sessions/{session}/queue/resume"),
                        json!({}),
                        phase,
                        lane,
                        iteration,
                        "resume",
                        origin,
                    )
                    .await;
                    rows.push(row);
                    model_round_boundary(&barrier).await;
                }
                rows
            }));
        }
        let writer_state = state.clone();
        let changing = ids[..MODEL_LANES].to_vec();
        let writer = tokio::spawn(async move {
            model_round_boundary(&barrier).await;
            let runtime = tokio::runtime::Handle::current();
            tokio::task::spawn_blocking(move || {
                let mut samples = Vec::new();
                for round in 0..iterations {
                    {
                        let mut inner = writer_state.inner.lock().unwrap();
                        for id in &changing {
                            let index = inner.find_session_index(id).unwrap();
                            let record = inner.session_mut_by_index(index).unwrap();
                            if let Message::Text { text, .. } =
                                &mut record.session.messages[round % 64]
                            {
                                text.replace_range(..1, if round % 2 == 0 { "y" } else { "x" });
                            }
                        }
                        writer_state.commit_locked(&mut inner).unwrap();
                    }
                    writer_state.schedule_engram_root_reclamation(true);
                    loop {
                        let (at, tick) = receiver
                            .recv_timeout(phase_sync::DEADLOCK_GUARD)
                            .expect("production writer completes a nonempty tick");
                        let committed = nonempty_committed_tick(&tick);
                        samples.push((at, tick));
                        if committed {
                            break;
                        }
                    }
                    // Keep all eight client lanes active across every writer
                    // round, instead of measuring most ticks after they leave.
                    runtime.block_on(model_round_boundary(&barrier));
                }
                (receiver, samples)
            })
            .await
            .unwrap()
        });
        for client in clients {
            all_operations.extend(client.await.unwrap());
        }
        let (next_receiver, samples) = writer.await.unwrap();
        receiver = next_receiver;
        all_ticks.extend(samples);
        // Cold's writes end before warm starts, its first whole transcripts among
        // them, and warm's writes end while it's still warm: which tick carries an
        // edit is up to the host, which phase it counts in no longer is.
        all_ticks.extend(tokio::task::block_in_place(|| {
            model_persist_quiet_point(&state, &receiver)
        }));
    }
    // Teardown begins after the last client and modeled writer round. Its
    // final synchronous full-store write is reported separately from the
    // affected paths exercised during the cold and warm workload.
    let teardown_started = std::time::Instant::now();
    phase_number.store(3, Ordering::SeqCst);
    timings.phase("worker_shutdown");
    drop(workers);
    timings.phase("report_and_assertions");
    state.persist_observer.test_tick_sink.lock().unwrap().take();
    all_ticks.extend(receiver.try_iter());
    let mut holds = holds.lock().unwrap().clone();
    holds.sort_by_key(|row| row.reported_at);
    let warm_started = warm_started.unwrap();
    let sample_phase = |at: std::time::Instant| {
        if at >= teardown_started {
            "teardown"
        } else if at >= warm_started {
            "warm"
        } else {
            "cold"
        }
    };
    let mut holds_by_thread = HashMap::<String, Vec<usize>>::new();
    for (index, row) in holds.iter().enumerate() {
        holds_by_thread
            .entry(row.thread_id.clone())
            .or_default()
            .push(index);
    }
    let cuts = cut_rx
        .try_iter()
        .map(|(at, elapsed, thread)| {
            let thread_id = format!("{thread:?}");
            let ended_at = at + elapsed;
            let indexes = holds_by_thread
                .get(&thread_id)
                .expect("cut owner was observed");
            // The cut runs while this thread owns StateInner. Its next Held
            // report is synchronous after unlocking, before another acquisition
            // on that thread. Classify the entire acquisition, never a residual.
            let position = indexes.partition_point(|index| holds[*index].reported_at < ended_at);
            let index = *indexes
                .get(position)
                .expect("cut owner released StateInner");
            let hold = &mut holds[index];
            hold.snapshot_cut_count += 1;
            hold.snapshot_cut_us += micros(elapsed);
            assert!(
                hold.snapshot_cut_us <= hold.held_us,
                "cut duration is inside its whole hold"
            );
            json!({
                "phase":sample_phase(at),
                "requestedUs":micros(at.duration_since(origin)),"elapsedUs":micros(elapsed),
                "threadId":thread_id,"holdIndex":index,
            })
        })
        .collect::<Vec<_>>();
    state
        .agent_readiness_cache
        .write()
        .unwrap()
        .phase_sink
        .take();
    let api_phases = api_phase_rx
        .try_iter()
        .map(|row| {
            json!({
                "phase":sample_phase(row.started),"stage":row.phase,
                "sessionId":row.session_id,"threadId":format!("{:?}",row.thread_id),
                "requestedUs":micros(row.started.duration_since(origin)),
                "elapsedUs":micros(row.elapsed),
            })
        })
        .collect::<Vec<_>>();
    let ticks = all_ticks
        .iter()
        .map(|(at, tick)| {
            json!({
                "phase":sample_phase(*at),
                "endedUs":micros(at.duration_since(origin)),"tick":tick,
            })
        })
        .collect::<Vec<_>>();
    let mut summaries = Vec::new();
    for phase in ["cold", "warm", "teardown"] {
        for kind in ["read", "ack", "resume"] {
            let values = all_operations
                .iter()
                .filter(|row| row.phase == phase && row.kind == kind)
                .map(|row| row.elapsed_us)
                .collect::<Vec<_>>();
            let duration = distribution(values.clone());
            let reference = (phase == "warm").then(|| json!({
                "p99Us":1_000_000,"maxUs":5_000_000,
                "p99Exceeded":duration["p99Us"].as_u64().is_some_and(|value| value > 1_000_000),
                "maxExceeded":duration["maxUs"].as_u64().is_some_and(|value| value > 5_000_000),
                "samplesAboveP99Reference":values.iter().filter(|value| **value > 1_000_000).count(),
                "samplesAboveMaxReference":values.iter().filter(|value| **value > 5_000_000).count(),
            }));
            summaries.push(json!({"phase":phase,"kind":kind,"duration":duration,
                "latencyGate":false,"warmReference":reference}));
        }
        summaries.push(
            json!({"phase":phase,"kind":"stateHold","duration":distribution(
            holds.iter().filter(|r| r.phase == phase).map(|r| r.held_us).collect())}),
        );
        for (kind, projection) in [("stateHoldProjection", true), ("stateHoldOther", false)] {
            let rows = holds
                .iter()
                .filter(|row| row.phase == phase && (row.snapshot_cut_count > 0) == projection)
                .collect::<Vec<_>>();
            let mut sites = std::collections::BTreeMap::<String, Vec<&HoldSample>>::new();
            for row in &rows {
                sites
                    .entry(format!("{}:{}", row.file, row.line))
                    .or_default()
                    .push(row);
            }
            let sites = sites.into_iter().map(|(site, rows)| json!({
                "site":site,
                "duration":distribution(rows.iter().map(|row| row.held_us).collect()),
                "snapshotCutOverlapUs":rows.iter().map(|row| row.snapshot_cut_us).sum::<u64>(),
            })).collect::<Vec<_>>();
            summaries.push(json!({"phase":phase,"kind":kind,
                "duration":distribution(rows.iter().map(|row| row.held_us).collect()),
                "totalHoldUs":rows.iter().map(|row| row.held_us).sum::<u64>(),
                "snapshotCutOverlapUs":rows.iter().map(|row| row.snapshot_cut_us).sum::<u64>(),
                "sites":sites}));
        }
        summaries.push(json!({"phase":phase,"kind":"writerTick","duration":distribution(
            all_ticks.iter().filter(|(at,_)| sample_phase(*at) == phase).map(|(_,t)| t.loop_wall_us).collect())}));
    }
    println!(
        "MODELED_PARALLEL_STATE {}",
        json!({
            "dimensions":{"sessions":MODEL_SESSIONS,"changingSessions":MODEL_LANES,"messagesEach":64,
                "messageBytes":16384,"namedRootJournalPairs":MODEL_PAIRS,"registrations":MODEL_PAIRS,
                "lanes":MODEL_LANES,"tripletsEach":100},
            "clock":"real monotonic; requested wait includes scheduling; no retries/outlier removal",
            "apiLatencyPolicy":"report-only for this trim slice; complete elapsed samples include projection, contention, readiness, response handling and fixture decoding; warm 1s p99/5s max references and overruns remain visible; end-to-end bounds belong to the separate snapshot/cold-start work",
            "projectionWaitAttribution":"unavailable: aggregate StateInner waits do not identify each intervening holder; no inferred wait, overlap or scheduler time is subtracted and no adjusted API latency is gated",
            "holdCategories":"snapshot cut belongs to the next synchronous Held report on its owning thread; complete holds are retained, cut duration is reported without subtraction; released_us is reporter-entry time after unlock",
            "rounds":"each eight-lane triplet round waits for one nonempty committed tick, which need not carry that round's edit",
            "coldIncludesBoot":true,"setupOperations":setup_operations,
            "operations":all_operations,"holds":holds,"snapshotCuts":cuts,
            "writerTicks":ticks,"summaries":summaries,"apiPhases":api_phases,
            "teardown":{
                "startedUs":micros(teardown_started.duration_since(origin)),
                "stateHolds":distribution(holds.iter().filter(|r| r.phase == "teardown").map(|r| r.held_us).collect()),
                "writerTicks":distribution(all_ticks.iter().filter(|(at,_)| sample_phase(*at) == "teardown").map(|(_,t)| t.loop_wall_us).collect()),
                "boundScope":"all elapsed times are report-only, with warm writer tick and non-projection hold limits in MODELED_PARALLEL_TIME_REPORT; functional and count checks decide the test",
            },
        })
    );
    assert_eq!(
        all_operations.len(),
        MODEL_LANES * 100 * 3,
        "every valid triplet completed"
    );
    assert!(
        all_operations
            .iter()
            .all(|r| r.status == Some(200) && r.error.is_none())
    );
    assert!(
        all_ticks.iter().all(|(_, t)| t.success),
        "no failed writer tick hidden"
    );
    assert!(
        all_ticks
            .iter()
            .filter(|(_, t)| nonempty_committed_tick(t))
            .count()
            >= 100
    );
    // Elapsed time on a shared host says how busy the host was, so these limits
    // are reported and never decide the test. Written straight to stderr so that
    // a passing run's log keeps them, which libtest's capture of println wouldn't.
    let time_report = summaries
        .iter()
        .filter(|summary| summary["phase"] == "warm")
        .filter_map(|summary| {
            let (p99, maximum) = match summary["kind"].as_str()? {
                "stateHoldOther" => (50_000u64, 250_000u64),
                "writerTick" => (2_000_000, 5_000_000),
                _ => return None,
            };
            let duration = &summary["duration"];
            assert!(duration["count"].as_u64().unwrap() > 0, "{summary}");
            Some(json!({
                "kind":summary["kind"],"p99Us":duration["p99Us"],"maxUs":duration["maxUs"],
                "p99LimitUs":p99,"maxLimitUs":maximum,
                "p99Exceeded":duration["p99Us"].as_u64().unwrap() > p99,
                "maxExceeded":duration["maxUs"].as_u64().unwrap() > maximum,
            }))
        })
        .collect::<Vec<_>>();
    let _ = std::io::Write::write_all(
        &mut std::io::stderr(),
        format!(
            "MODELED_PARALLEL_TIME_REPORT {}\n",
            Value::from(time_report)
        )
        .as_bytes(),
    );
    // What decides the test is the work counted under and around the state lock,
    // so no host's speed changes the result. It catches a tick that collects more
    // sessions than the lanes, a collection that copies its sessions under fewer
    // locks than sessions or retries without bound, whole transcripts rewritten,
    // a transcript carried in session metadata, and a request that builds extra
    // snapshot cuts. It doesn't catch a scan under the lock that takes no extra
    // lock and adds no row, a cut that only gets more expensive, one hold copying
    // a few sessions on a tick where a retry added locks, one whole rewrite when
    // enough edits coalesced, or rewritten metadata and prompt history rows. The
    // time report may show some of these as slower ticks and holds, but it leaves
    // out projection holds.
    let warm_cuts = holds
        .iter()
        .filter(|row| row.phase == "warm")
        .map(|row| {
            assert!(
                row.snapshot_cut_count <= 1,
                "one cut per state hold: {}:{}",
                row.file,
                row.line
            );
            row.snapshot_cut_count
        })
        .sum::<usize>();
    // One for each writer commit, and two for a resume: its commit and its response.
    let cuts_allowed = MODEL_WARM_ROUNDS + 2 * MODEL_LANES * MODEL_WARM_ROUNDS;
    assert!(warm_cuts > 0, "warm holds report their snapshot cuts");
    assert!(
        warm_cuts <= cuts_allowed,
        "{warm_cuts} warm snapshot cuts, {cuts_allowed} allowed"
    );
    // The checkout path is in every session row, and may be at any depth.
    let session_metadata_allowed = MODEL_MAX_SESSION_METADATA_BYTES
        + 4 * Value::from(root.path().to_string_lossy().as_ref())
            .to_string()
            .len();
    let mut warm_messages = 0;
    for (at, tick) in &all_ticks {
        if sample_phase(*at) != "warm" {
            continue;
        }
        let (counts, locks) = (&tick.counts, tick.phases.collect_lock_count);
        let changed = counts.changed_sessions;
        assert!(
            changed <= MODEL_LANES,
            "tick {} collected {changed} sessions",
            tick.tick
        );
        assert!(
            changed == 0 || locks > changed,
            "tick {} held the lock across sessions: {locks} locks for {changed}",
            tick.tick,
        );
        assert!(
            locks <= MODEL_MAX_COLLECT_LOCKS,
            "tick {} took {locks} collection locks",
            tick.tick,
        );
        assert!(
            counts.message_bytes <= counts.retained_messages * MODEL_MAX_RETAINED_MESSAGE_BYTES,
            "tick {} wrote {} messages, {} bytes",
            tick.tick,
            counts.retained_messages,
            counts.message_bytes,
        );
        assert!(
            counts.session_metadata_bytes <= changed * session_metadata_allowed,
            "tick {} wrote {} session metadata bytes for {changed} sessions",
            tick.tick,
            counts.session_metadata_bytes,
        );
        warm_messages += counts.retained_messages;
    }
    // A total, not a limit per tick: a tick that runs late can legitimately carry
    // several rounds' edits. The quiet points leave warm exactly its own edits, so
    // each is written at most once, and every one of the 64 messages it edited has
    // to be written even if all of a message's edits coalesced.
    let edits = MODEL_LANES * MODEL_WARM_ROUNDS;
    let edited_messages = MODEL_LANES * 64;
    assert!(
        (edited_messages..=edits).contains(&warm_messages),
        "{warm_messages} warm messages written for {edits} edits of {edited_messages} messages",
    );
    timings.phase("scope_cleanup");
}
