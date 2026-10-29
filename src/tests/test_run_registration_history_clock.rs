// Deterministic witnesses for the registration durability budget running on
// the state's budget clock, included into test_run_registration_history.rs for
// its HistoryService (the real router and an actual SQLite fence writer).
// Owns: the real-time deadline still measured from the instant before the
// capture lock, a writer held past 5 s of real time still acknowledging while the
// scripted clock has not advanced, and an advanced scripted clock still giving
// the typed durability-pending reply. Does not own history reads, the
// structural history case, or the scheduled real-time measurement.
// New file, not split from another.

/// A served registration fixture whose budget clock is a scripted one.
fn clock_service(label: &str) -> (HistoryService, PathBuf, String, EngramBudgetClock) {
    let state = test_app_state();
    let root = state.test_temp_root.as_ref().unwrap().path().join(format!("clock-{label}-project"));
    fs::create_dir_all(&root).unwrap();
    run_git_test_command(&root, &["init", "--quiet"]);
    let project = create_test_project(&state, &root, "Registration clock");
    let owner = create_test_project_session(&state, Agent::Codex, &project, &root);
    let service = HistoryService::new(state);
    let clock = service.state.as_ref().unwrap().select_test_scripted_engram_budget_clock();
    (service, root, owner, clock)
}

/// Writes one launcher request; returns its run id and run directory.
fn clock_registration_request(root: &FsPath, owner: &str, case: &str) -> (String, PathBuf) {
    let id = format!("test-clock-{case}-{}", Uuid::new_v4());
    let directory = root.join(".git").join("review-runs").join(&id);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("request.json"), json!({
        "runId": id, "root": test_run_display_path(root), "owner": owner,
        "full": false, "stages": [{"name": "focused"}],
        "started": engram_command_started_at(), "creatorPid": std::process::id()
    }).to_string()).unwrap();
    (id, directory)
}

/// Sends the registration POST on its own thread; returns its status and body.
fn post_clock_registration(url: &str, owner: &str, id: &str, directory: &FsPath, case: &str)
    -> std::thread::JoinHandle<(reqwest::StatusCode, Value)>
{
    let (url, owner, case) = (url.to_owned(), owner.to_owned(), case.to_owned());
    let body = json!({"runId": id, "runDirectory": test_run_display_path(directory)});
    std::thread::spawn(move || {
        let client = reqwest::blocking::Client::builder()
            .timeout(phase_sync::DEADLOCK_GUARD).pool_max_idle_per_host(0).build().unwrap();
        let response = client.post(format!("{url}/api/sessions/{owner}/test-runs"))
            .header("x-termal-registration-operation", format!("clock-{case}"))
            .json(&body).send().unwrap();
        let status = response.status();
        (status, response.json().unwrap())
    })
}

fn finish_clock_service(mut service: HistoryService, outcome: std::thread::Result<()>) {
    let cleanup_errors = service.stop_all();
    if let Err(panic) = outcome {
        for error in cleanup_errors { eprintln!("{error}"); }
        std::panic::resume_unwind(panic);
    }
    assert!(cleanup_errors.is_empty(), "{cleanup_errors:?}");
}

#[test]
fn the_real_clock_deadline_is_measured_from_the_instant_before_the_capture_lock() {
    // Production: the deadline is exactly the pre-lock instant plus 5 s, so a
    // capture-lock wait still counts against the budget, as before.
    let requested = std::time::Instant::now() - Duration::from_secs(2);
    assert_eq!(test_run_registration_deadline(&EngramBudgetClock::Real, requested),
        requested + Duration::from_secs(5));
    // A scripted fixture clock measures from its own reading instead.
    let clock = EngramBudgetClock::scripted();
    clock.advance(Duration::from_secs(7));
    assert_eq!(test_run_registration_deadline(&clock, requested), clock.now() + Duration::from_secs(5));
}

#[test]
fn a_registration_whose_writer_is_held_past_five_real_seconds_acknowledges_on_the_unadvanced_scripted_clock() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (service, root, owner, clock) = clock_service("held-writer");
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (id, directory) = clock_registration_request(&root, &owner, "held");
        let (reached, release) = service.hold_next_writer_tick();
        let scripted_start = clock.now();
        let timed = std::time::Instant::now();
        let post = post_clock_registration(&service.url, &owner, &id, &directory, "held");
        reached.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
        // The schedule hook, not load, holds the writer. Real time passes the
        // production 5 s budget while nothing advances the scripted clock.
        std::thread::sleep(Duration::from_millis(5_200));
        assert_eq!(clock.now(), scripted_start, "nothing advanced the scripted budget clock");
        release.send(()).unwrap();
        let (status, ack) = post.join().unwrap();
        assert!(timed.elapsed() >= Duration::from_secs(5), "the writer was held past 5 s of real time");
        assert_eq!(status, reqwest::StatusCode::CREATED, "{ack}");
        assert_eq!(ack, json!({"contract":"termal-durable-test-run-registration",
            "version":1,"runId":id,"registered":true}));
        let client = reqwest::blocking::Client::builder()
            .timeout(phase_sync::DEADLOCK_GUARD).pool_max_idle_per_host(0).build().unwrap();
        let query: Value = client.get(format!("{}/api/test-runs/{id}", service.url)).send().unwrap().json().unwrap();
        assert_eq!(query["run"]["registrationState"], "registered");
        assert_eq!(query["run"]["trackingDurability"], "durable");
        assert!(service.commits.lock().unwrap().contains_key(&id), "the registration was committed to SQLite");
    }));
    finish_clock_service(service, outcome);
}

#[test]
fn advancing_the_scripted_clock_past_the_budget_still_gives_the_typed_durability_pending_reply() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (service, root, owner, clock) = clock_service("advanced-clock");
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (id, directory) = clock_registration_request(&root, &owner, "deadline");
        let (reached, release) = service.hold_next_writer_tick();
        let post = post_clock_registration(&service.url, &owner, &id, &directory, "deadline");
        reached.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
        // The fence is queued behind the held writer and its waiter is on the
        // scripted clock; only then does the budget elapse, by advancing it.
        clock.wait_for_scripted_waiter();
        clock.advance(Duration::from_secs(5) + Duration::from_millis(1));
        let (status, body) = post.join().unwrap();
        release.send(()).unwrap();
        assert_eq!(status, reqwest::StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(body["contract"], "termal-durability-pending");
        assert_eq!(body["version"], 1);
        assert_eq!(body["code"], "durability_pending");
        assert_eq!(body["outcome"], "pending");
        assert_eq!(body["operation"], "test_run_registration");
        assert_eq!(body["identity"]["runId"], id);
        assert_eq!(body["retry"]["mode"], "same_request");
        let message = body["error"].as_str().unwrap();
        for field in ["phase=durability_wait", "category=deadline", "fenceError=Deadline",
            "fenceBudgetMs=5000", "fenceRemainingMs=0", "durabilityConfirmed=false"] {
            assert!(message.contains(field), "{field} missing from {message}");
        }
    }));
    finish_clock_service(service, outcome);
}
