// Owns structural and separately scheduled timing evidence under real history.
// Does not change launcher budgets, fake durability, or measure the old host.
use super::*;

include!("test_run_registration_history_seed.rs");
include!("test_run_registration_history_service.rs");

#[derive(Default)]
struct HistoryReads {
    parsed: HashMap<String, String>,
    excerpts: HashMap<String, (String, String)>,
    published_failures: HashSet<String>,
    handshake: Duration,
    scan_work: Duration,
}

#[test]
fn candidate_registration_is_independent_of_real_history_reads() {
    history_registration_cases(320, false);
}

#[test]
#[ignore = "scheduled exclusive history measurement; select explicitly with --ignored --exact"]
fn candidate_registration_latency_with_3200_real_history_runs() {
    history_registration_cases(3200, true);
}

fn history_registration_cases(count: usize, enforce_budget: bool) {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let state = test_app_state();
    let root = state.test_temp_root.as_ref().unwrap().path().join("history-project");
    fs::create_dir_all(&root).unwrap();
    run_git_test_command(&root, &["init", "--quiet"]);
    let project = create_test_project(&state, &root, "History registration");
    let owner = create_test_project_session(&state, Agent::Codex, &project, &root);
    let mut service = HistoryService::new(state);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut seed = HistorySeed::new(&root, &owner, count);
        history_registration_case(&mut service, &root, &owner, &seed, "cold", enforce_budget);
        seed.write_results(1);
        history_registration_case(&mut service, &root, &owner, &seed, "changed", enforce_budget);
    }));
    let cleanup_errors = service.stop_all();
    if let Err(panic) = outcome {
        for error in cleanup_errors { eprintln!("{error}"); }
        std::panic::resume_unwind(panic);
    }
    assert!(cleanup_errors.is_empty(), "{cleanup_errors:?}");
}

fn history_registration_case(
    service: &mut HistoryService, root: &FsPath, owner: &str,
    seed: &HistorySeed, case: &str, enforce_budget: bool,
) {
    let id = format!("test-history-registration-{case}-{}", Uuid::new_v4());
    let directory = root.join(".git").join("review-runs").join(&id);
    fs::create_dir_all(&directory).unwrap();
    let started_at = engram_command_started_at();
    fs::write(directory.join("request.json"), json!({
        "runId":id,"root":test_run_display_path(root),"owner":owner,
        "full":false,"stages":[{"name":"focused"}],
        "started":started_at,"creatorPid":std::process::id()
    }).to_string()).unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    assert!(test_history_received_hooks().lock().unwrap().insert(id.clone(), release_tx).is_none());
    service.release_id = Some(id.clone());
    let state = service.state.as_ref().unwrap().clone();
    service.scan = Some(std::thread::spawn(move || {
        let reads = std::rc::Rc::new(std::cell::RefCell::new(HistoryReads::default()));
        let observed = reads.clone();
        TEST_HISTORY_PARSED.with(|slot| *slot.borrow_mut() = Some(Box::new(move |directory, digest| {
            let id = directory.file_name().unwrap().to_str().unwrap();
            if id.starts_with("test-history-") && !id.starts_with("test-history-registration-") {
                assert!(observed.borrow_mut().parsed.insert(id.to_owned(), digest.to_owned()).is_none());
            }
        })));
        let observed = reads.clone();
        TEST_HISTORY_EXCERPT.with(|slot| *slot.borrow_mut() = Some(Box::new(move |directory, failure| {
            let id = directory.file_name().unwrap().to_str().unwrap();
            if let TestRunCardFailureRead::Read { digest, failure: Some(failure) } = failure {
                observed.borrow_mut().excerpts.insert(id.to_owned(), (digest.clone(), failure.excerpt.clone()));
            }
        })));
        let observed = reads.clone();
        let inner = state.inner.clone();
        TEST_HISTORY_SCAN_READY.with(|slot| *slot.borrow_mut() = Some(Box::new(move || {
            assert!(inner.is_not_held_by_current_thread_for_test(),
                "scan seam must not hold StateInner");
            // The production serialization guard stays held. No SQL connection
            // has been acquired by this scanner. POST's actual received point
            // releases this handshake before any validation or durability work.
            let handshake = std::time::Instant::now();
            ready_tx.send(()).unwrap();
            let release = release_rx.recv_timeout(phase_sync::DEADLOCK_GUARD);
            observed.borrow_mut().handshake = handshake.elapsed();
            release.expect("POST must record received and release the real scan");
        })));
        let observed = reads.clone();
        let started = std::time::Instant::now();
        state.refresh_test_runs_with(&test_run_process_writer_may_be_alive, &|inner, event| {
            state.publish_delta_locked(inner, event.clone());
            // Inspect actual published card events, not a copied card builder.
            let run = match event {
                DeltaEvent::MessageCreated { message: Message::TestRun { run, .. }, .. } => Some(run),
                DeltaEvent::TestRunCardUpdated { run, .. } => Some(run),
                _ => None,
            };
            if let Some(run) = run {
                if run.failure.as_ref().is_some_and(|failure| failure.excerpt.contains("history-failure-")) {
                    observed.borrow_mut().published_failures.insert(run.run_id.clone());
                }
            }
        });
        drop(observed);
        TEST_HISTORY_PARSED.with(|slot| slot.borrow_mut().take());
        TEST_HISTORY_EXCERPT.with(|slot| slot.borrow_mut().take());
        let mut reads = std::rc::Rc::try_unwrap(reads).ok().unwrap().into_inner();
        reads.scan_work = started.elapsed().saturating_sub(reads.handshake);
        reads
    }));
    ready_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
    let client = reqwest::blocking::Client::builder()
        .timeout(phase_sync::DEADLOCK_GUARD).pool_max_idle_per_host(0).build().unwrap();
    let timed = std::time::Instant::now();
    let response = client.post(format!("{}/api/sessions/{owner}/test-runs", service.url))
        .header("x-termal-registration-operation", format!("history-{case}"))
        .json(&json!({"runId":id,"runDirectory":test_run_display_path(&directory)}))
        .send().unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CREATED);
    let ack: Value = response.json().unwrap();
    let acknowledged = std::time::Instant::now();
    assert_eq!(ack, json!({"contract":"termal-durable-test-run-registration",
        "version":1,"runId":id,"registered":true}));
    let response = client.get(format!("{}/api/test-runs/{id}", service.url)).send().unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let query: Value = response.json().unwrap();
    let elapsed = timed.elapsed();
    drop(client);
    eprintln!("candidate history {case} HTTP: postPlusGet={elapsed:?} tracesAtReply={}",
        serde_json::to_string(&service.state.as_ref().unwrap()
            .test_run_phase_traces.selected(Some(&id), None)).unwrap());
    assert_eq!(query["run"]["runId"], id);
    assert_eq!(query["run"]["runDir"], test_run_display_path(&directory));
    assert_eq!(query["run"]["ownerSessionId"], owner);
    assert_eq!(query["run"]["startedAt"], started_at);
    assert_eq!(query["run"]["registrationState"], "registered");
    assert_eq!(query["run"]["trackingDurability"], "durable");
    assert!(service.commits.lock().unwrap()[&id] <= acknowledged, "SQL COMMIT precedes HTTP ACK");
    let state = service.state.as_ref().unwrap();
    let registered = state.inner.lock().unwrap().test_run_registrations[&id].clone();
    assert!(registered.monitoring_interrupted_at.is_none());
    let stored: TestRunRegistration = {
        // Read actual committed bytes before the boot projection, and close
        // this SQLite reader before teardown (also required on Windows).
        let connection = rusqlite::Connection::open_with_flags(
            state.persistence_path.as_path(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ).unwrap();
        let metadata = read_persisted_metadata_json(&connection).unwrap().unwrap();
        let metadata: Value = serde_json::from_str(&metadata).unwrap();
        serde_json::from_value(metadata["testRunRegistrations"][id.as_str()].clone()).unwrap()
    };
    assert_eq!(stored, registered, "every durable registration field matches the ACKed identity");
    // load_state deliberately follows real boot recovery. A live launch with
    // no final observation gains monitoringInterruptedAt; compare every other
    // field exactly and assert that recovery fact rather than ignoring it.
    let reload_started_millis = chrono::Utc::now().timestamp_millis();
    let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    let mut recovered = loaded.test_run_registrations[&id].clone();
    let interrupted_at = recovered.monitoring_interrupted_at.take()
        .expect("boot recovery must mark this still-live registration interrupted");
    let interrupted_millis = chrono::DateTime::parse_from_rfc3339(&interrupted_at)
        .unwrap().timestamp_millis();
    assert!(interrupted_millis >= reload_started_millis,
        "recovery timestamp must be at or after reload, at the wire's millisecond precision");
    assert_eq!(recovered, stored, "boot recovery must preserve the complete durable identity");
    drop(loaded);
    let reads = service.scan.take().unwrap().join().unwrap();
    service.release_id.take();
    let serialized = service.serialized_failure_events();
    for specimen in ["test-history-0000", "test-history-0002"] {
        assert!(serialized.contains(&seed.failures[specimen]),
            "actual ordered SSE publication must carry the current failure excerpt");
    }
    assert_eq!(reads.parsed, seed.digests, "every real history result parsed");
    assert_eq!(reads.excerpts.len(), seed.failures.len());
    for (id, marker) in &seed.failures {
        let (digest, excerpt) = &reads.excerpts[id];
        assert_eq!(digest, &seed.digests[id]);
        assert!(excerpt.starts_with(marker) && excerpt.len() <= TEST_RUN_CARD_EXCERPT_MAX_BYTES);
    }
    assert!(reads.published_failures.contains("test-history-0000"));
    assert!(reads.published_failures.contains("test-history-0002"));
    let traces = service.state.as_ref().unwrap().test_run_phase_traces.selected(Some(&id), None);
    let registration = traces.iter().find(|trace| trace.kind == "registration").unwrap();
    let scans: Vec<_> = traces.iter().filter(|trace| trace.kind == "rescan").collect();
    assert_eq!(scans.len(), 1, "POST and GET must never attempt another rescan mutex");
    let point = |trace: &TestRunPhaseSnapshot, phase| trace.phases.iter()
        .find(|point| point.phase == phase).unwrap().monotonic_micros;
    for phase in ["validated", "durable", "response_ready"] { point(registration, phase); }
    let received = point(registration, "received");
    assert!(point(scans[0], "roots_taken") <= received);
    assert!(received <= point(scans[0], "reads_done"));
    for phase in ["commit_acquired", "published"] { point(scans[0], phase); }
    eprintln!("candidate history {case}: realParsed={} realExcerpts={} postPlusGet={elapsed:?} handshake={:?} scanWork={:?} traces={}",
        reads.parsed.len(), reads.excerpts.len(), reads.handshake, reads.scan_work,
        serde_json::to_string(&traces).unwrap());
    if enforce_budget { assert!(elapsed <= Duration::from_secs(2), "POST plus immediate GET took {elapsed:?}"); }
}
