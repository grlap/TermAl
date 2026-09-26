//! Tests the test-run card (docs/features/test-runs.md, slice 2 "The test-run
//! card"): when a card is created and for whom, updating it in place from the
//! index, `notIndexed`, the failure excerpt, the snapshot's byte budgets, the
//! retention exemption, the durable epoch, and the wire shape. Liveness is
//! injected so no test depends on a real process's pid.
//!
//! Owns these tests only. New module alongside src/test_run_cards.rs.

use super::*;

struct CardFixture {
    state: AppState,
    root: PathBuf,
    owner: String,
    events: std::rc::Rc<std::cell::RefCell<Vec<DeltaEvent>>>,
}

impl CardFixture {
    fn new(label: &str) -> Self {
        let fixture = Self::new_without_epoch(label);
        assert!(fixture.state.ensure_test_run_cards_epoch(), "the epoch is durable");
        fixture
    }

    fn new_without_epoch(label: &str) -> Self {
        let state = test_app_state();
        let root = state
            .test_temp_root
            .as_ref()
            .expect("test root should exist")
            .path()
            .join(format!("test-run-cards-{label}"));
        fs::create_dir_all(&root).unwrap();
        run_git_test_command(&root, &["init", "--quiet"]);
        create_test_project(&state, &root, "Cards");
        let owner = test_session_id(&state, Agent::Claude);
        Self {
            state,
            root,
            owner,
            events: Default::default(),
        }
    }

    fn runs_dir(&self) -> PathBuf {
        self.root.join(".git").join("review-runs")
    }

    fn refresh(&self, alive: &[u32]) {
        let alive = alive.to_vec();
        let events = self.events.clone();
        self.state
            .refresh_test_runs_with(&move |pid, _| alive.contains(&pid), &|event| {
                self.state.publish_delta(event);
                events.borrow_mut().push(event.clone());
            });
    }

    /// Card deltas published since the last call.
    fn card_events(&self) -> Vec<DeltaEvent> {
        self.events
            .borrow_mut()
            .drain(..)
            .filter(|event| match event {
                DeltaEvent::MessageCreated { message, .. } => {
                    matches!(message, Message::TestRun { .. })
                }
                DeltaEvent::TestRunCardUpdated { .. } => true,
                _ => false,
            })
            .collect()
    }

    fn owner_messages(&self) -> Vec<Message> {
        let inner = self.state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&self.owner).unwrap()]
            .session
            .messages
            .clone()
    }

    fn cards(&self) -> Vec<(String, TestRunCardSnapshot)> {
        self.owner_messages()
            .into_iter()
            .filter_map(|message| match message {
                Message::TestRun { id, run, .. } => Some((id, run)),
                _ => None,
            })
            .collect()
    }

    fn card(&self, run_id: &str) -> TestRunCardSnapshot {
        self.cards()
            .into_iter()
            .find(|(_, run)| run.run_id == run_id)
            .unwrap_or_else(|| panic!("{run_id} should have a card"))
            .1
    }

    fn card_ref(&self, run_id: &str) -> Option<TestRunCardRef> {
        self.state.inner.lock().unwrap().test_run_cards.get(run_id).cloned()
    }

    fn write(&self, run_id: &str, request: Value, results: Option<Value>) -> PathBuf {
        let run_dir = self.runs_dir().join(run_id);
        fs::create_dir_all(&run_dir).unwrap();
        let mut request = request;
        request["runId"] = json!(run_id);
        fs::write(run_dir.join("request.json"), request.to_string()).unwrap();
        if let Some(mut results) = results {
            results["runId"] = json!(run_id);
            fs::write(run_dir.join("results.json"), results.to_string()).unwrap();
        }
        run_dir
    }

    fn request(&self, started: &str) -> Value {
        json!({
            "root": self.root.to_string_lossy(),
            "full": true,
            "owner": self.owner,
            "started": started,
        })
    }
}

fn iso(time: chrono::DateTime<chrono::Utc>) -> String {
    time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn after_epoch() -> String {
    iso(chrono::Utc::now() + chrono::TimeDelta::seconds(1))
}

fn running(pid: u32, current: &str) -> Value {
    json!({
        "state": "running",
        "pid": pid,
        "stages": [
            { "name": "cargo-check", "state": if current == "cargo-check" { "running" } else { "passed" }, "code": 0 },
            { "name": "rust-tests", "state": if current == "rust-tests" { "running" } else { "unrun" } },
        ],
    })
}

fn passed() -> Value {
    json!({
        "state": "passed", "exitCode": 0, "ended": "2026-09-26T10:10:00.000Z",
        "stages": [
            { "name": "cargo-check", "state": "passed", "code": 0 },
            { "name": "rust-tests", "state": "passed", "code": 0 },
        ],
    })
}

fn failed_with(diagnostics: &str, truncated: bool) -> Value {
    json!({
        "state": "failed", "exitCode": 1, "ended": "2026-09-26T10:10:00.000Z",
        "error": "rust-tests failed",
        "stages": [
            { "name": "cargo-check", "state": "passed", "code": 0 },
            { "name": "rust-tests", "state": "failed", "code": 101,
              "diagnostics": { "text": diagnostics, "truncated": truncated } },
        ],
    })
}

#[test]
fn an_owned_run_gets_one_card_updated_in_place() {
    let fixture = CardFixture::new("lifecycle");
    let run_dir = fixture.write(
        "test-life",
        fixture.request(&after_epoch()),
        Some(running(7, "cargo-check")),
    );

    fixture.refresh(&[7]);
    let created = fixture.card_events();
    assert!(
        matches!(created.as_slice(), [DeltaEvent::MessageCreated { .. }]),
        "one card is created"
    );
    let cards = fixture.cards();
    assert_eq!(cards.len(), 1);
    let (message_id, card) = cards[0].clone();
    assert_eq!(card.state, TestRunState::Running);
    assert_eq!(card.current_stage.as_deref(), Some("cargo-check"));
    // The session preview follows the card, as it follows a delegation card.
    assert_eq!(
        {
            let inner = fixture.state.inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&fixture.owner).unwrap()]
                .session
                .preview
                .clone()
        },
        "Test run (full): running"
    );
    let card_ref = fixture.card_ref("test-life").expect("the map records the card");
    assert_eq!((card_ref.session_id.as_str(), card_ref.message_id.as_str()), (fixture.owner.as_str(), message_id.as_str()));
    assert!(!card_ref.terminal);
    let messages_after_create = fixture.owner_messages().len();

    // An unchanged run writes nothing, and neither does a change that only
    // moves the detail version: the snapshot has no volatile field.
    fixture.refresh(&[7]);
    assert!(fixture.card_events().is_empty(), "unchanged run");
    let mut results = running(7, "cargo-check");
    results["runId"] = json!("test-life");
    results["limitations"] = json!("detail only");
    fs::write(run_dir.join("results.json"), results.to_string()).unwrap();
    fixture.refresh(&[7]);
    assert!(
        fixture.events.borrow().iter().any(|event| matches!(event, DeltaEvent::TestRunChanged { .. })),
        "the summary's detail version moved"
    );
    assert!(fixture.card_events().is_empty(), "detail-only change rewrites no card");

    // Progress updates the same message in place.
    let mut results = running(7, "rust-tests");
    results["runId"] = json!("test-life");
    fs::write(run_dir.join("results.json"), results.to_string()).unwrap();
    fixture.refresh(&[7]);
    let updated = fixture.card_events();
    assert!(
        matches!(updated.as_slice(), [DeltaEvent::TestRunCardUpdated { message_id: id, run, .. }]
            if *id == message_id && run.current_stage.as_deref() == Some("rust-tests")),
        "{}",
        updated.len()
    );
    assert_eq!(fixture.owner_messages().len(), messages_after_create, "never a second card");

    // The verdict lands in the same card, which is then terminal.
    let mut results = passed();
    results["runId"] = json!("test-life");
    fs::write(run_dir.join("results.json"), results.to_string()).unwrap();
    fixture.refresh(&[]);
    assert_eq!(fixture.card("test-life").state, TestRunState::Passed);
    assert!(fixture.card_ref("test-life").unwrap().terminal);
    assert_eq!(fixture.cards().len(), 1);
}

#[test]
fn only_an_owned_run_started_after_the_epoch_gets_a_card() {
    let fixture = CardFixture::new("eligibility");
    let before_epoch = iso(chrono::Utc::now() - chrono::TimeDelta::hours(1));
    fixture.write("test-old", fixture.request(&before_epoch), Some(running(7, "cargo-check")));
    let mut no_owner = fixture.request(&after_epoch());
    no_owner.as_object_mut().unwrap().remove("owner");
    fixture.write("test-no-owner", no_owner, Some(running(7, "cargo-check")));
    let mut notify_only = fixture.request(&after_epoch());
    notify_only.as_object_mut().unwrap().remove("owner");
    notify_only["notifyTo"] = json!(fixture.owner);
    fixture.write("test-notify-only", notify_only, Some(running(7, "cargo-check")));
    let mut unknown_owner = fixture.request(&after_epoch());
    unknown_owner["owner"] = json!("session-does-not-exist");
    fixture.write("test-unknown-owner", unknown_owner, Some(running(7, "cargo-check")));

    fixture.refresh(&[7]);

    assert!(fixture.cards().is_empty(), "{:?}", fixture.cards());
    assert!(fixture.state.inner.lock().unwrap().test_run_cards.is_empty());
}

#[test]
fn a_run_terminal_at_first_sight_gets_a_card_only_if_it_started_recently() {
    let fixture = CardFixture::new("late-sight");
    // An epoch an hour back, so all three runs started after it.
    fixture.state.inner.lock().unwrap().test_run_cards_epoch =
        Some(iso(chrono::Utc::now() - chrono::TimeDelta::hours(1)));
    let recently = iso(chrono::Utc::now() - chrono::TimeDelta::minutes(5));
    let long_ago = iso(chrono::Utc::now() - chrono::TimeDelta::minutes(20));
    fixture.write("test-short", fixture.request(&recently), Some(passed()));
    fixture.write("test-long-done", fixture.request(&long_ago), Some(passed()));
    fixture.write("test-long-live", fixture.request(&long_ago), Some(running(7, "rust-tests")));

    fixture.refresh(&[7]);

    let carded: HashSet<String> = fixture.cards().into_iter().map(|(_, run)| run.run_id).collect();
    assert_eq!(
        carded,
        HashSet::from(["test-short".to_owned(), "test-long-live".to_owned()]),
        "a short run that ended between rescans, and any run still going"
    );
}

#[test]
fn a_card_that_is_not_terminal_reads_not_indexed_when_its_run_leaves() {
    let fixture = CardFixture::new("not-indexed");
    let run_dir = fixture.write("test-gone", fixture.request(&after_epoch()), Some(running(7, "rust-tests")));
    fixture.refresh(&[7]);
    assert_eq!(fixture.card("test-gone").state, TestRunState::Running);

    fs::remove_dir_all(&run_dir).unwrap();
    fixture.refresh(&[7]);
    let card = fixture.card("test-gone");
    assert_eq!(card.state, TestRunState::Unknown);
    assert_eq!(card.unknown_reason, Some(TestRunCardUnknownReason::NotIndexed));
    assert_eq!(card.current_stage.as_deref(), Some("rust-tests"), "the last observation stays");
    assert!(!fixture.card_ref("test-gone").unwrap().terminal);

    // Indexed again, its summary updates the card again.
    fixture.write("test-gone", fixture.request(&after_epoch()), Some(passed()));
    fixture.refresh(&[]);
    assert_eq!(fixture.card("test-gone").state, TestRunState::Passed);
    assert_eq!(fixture.cards().len(), 1);
}

#[test]
fn a_failed_card_carries_its_first_failure_cut_to_four_kib() {
    let fixture = CardFixture::new("failure");
    let long = "é".repeat(3000); // 6000 bytes of two-byte characters
    fixture.write("test-failed", fixture.request(&after_epoch()), Some(failed_with(&long, false)));
    fixture.refresh(&[]);
    let card = fixture.card("test-failed");
    assert_eq!(card.state, TestRunState::Failed);
    let failure = card.failure.expect("a failed card names its failure");
    assert_eq!(failure.phase, TestRunCardFailurePhase::Stage);
    assert_eq!(failure.name, "rust-tests");
    assert!(failure.truncated);
    assert!(failure.excerpt.len() <= 4 * 1024);
    assert!(long.starts_with(&failure.excerpt), "cut at a character boundary");
    assert_eq!(card.error.as_deref(), Some("rust-tests failed"));
    assert_eq!(
        fixture.card_ref("test-failed").unwrap().failure_detail_version,
        fixture.state.test_run_summaries(None)[0].detail_version,
        "the excerpt's source version is recorded"
    );

    // A preflight failure keeps its check name.
    let preflight = CardFixture::new("preflight");
    preflight.write(
        "test-preflight",
        preflight.request(&after_epoch()),
        Some(json!({
            "state": "failed", "exitCode": 1, "ended": "2026-09-26T10:00:00.000Z",
            "error": "preflight failed",
            "preflight": [{ "name": "cargo", "code": 127, "diagnostics": { "text": "not found" } }],
            "stages": [],
        })),
    );
    preflight.refresh(&[]);
    let failure = preflight.card("test-preflight").failure.unwrap();
    assert_eq!(
        (failure.phase, failure.name.as_str(), failure.excerpt.as_str(), failure.truncated),
        (TestRunCardFailurePhase::Preflight, "cargo", "not found", false)
    );
}

#[test]
fn the_snapshot_fills_its_optional_part_in_the_fixed_order_within_eight_kib() {
    let fixture = CardFixture::new("budget");
    // 64 stages with long timestamps exceed 8 KiB on their own.
    let stamp = format!("2026-09-26T10:00:00.000Z{}", "x".repeat(80));
    let stages: Vec<Value> = (0..64)
        .map(|index| {
            let state = match index {
                40 => "failed",
                _ if index < 40 => "passed",
                _ => "unrun",
            };
            json!({ "name": format!("stage-{index:02}"), "state": state, "code": 0,
                    "started": stamp, "ended": stamp })
        })
        .collect();
    let mut results = failed_with(&"d".repeat(6000), false);
    results["stages"] = json!(stages);
    results["stages"][40]["diagnostics"] = json!({ "text": "d".repeat(6000), "truncated": false });
    fixture.write("test-budget", fixture.request(&after_epoch()), Some(results));
    fixture.refresh(&[]);

    let card = fixture.card("test-budget");
    let full = serde_json::to_vec(&card).unwrap().len();
    let core = serde_json::to_vec(&TestRunCardSnapshot {
        stages: vec![],
        stages_omitted: 0,
        error: None,
        error_truncated: false,
        failure: None,
        command: None,
        command_truncated: false,
        ..card.clone()
    })
    .unwrap()
    .len();
    assert!(full - core <= 8 * 1024, "optional part {} bytes", full - core);
    assert!(card.stages.iter().any(|stage| stage.name == "stage-40"), "the failed stage first");
    assert!(card.failure.is_some(), "the failure outranks passed stage rows");
    assert!(card.stages_omitted > 0);
    assert_eq!(card.stages.len() + card.stages_omitted, 64);
    let names: Vec<&str> = card.stages.iter().map(|stage| stage.name.as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "stages stay in plan order");
    // The same run always gives the same snapshot.
    let entry = fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_runs
        .find("test-budget")
        .cloned()
        .unwrap();
    assert_eq!(test_run_card_snapshot(&entry, card.failure.clone()), Some(card));
}

#[test]
fn a_run_whose_core_exceeds_ten_kib_gets_no_card() {
    let fixture = CardFixture::new("core-budget");
    let mut request = fixture.request(&after_epoch());
    // Control characters escape to six bytes each in JSON.
    request["root"] = json!("\u{1}".repeat(4000));
    fixture.write("test-escaped", request, Some(running(7, "rust-tests")));
    fixture.refresh(&[7]);
    assert!(fixture.cards().is_empty());
}

#[test]
fn a_resident_card_without_its_verdict_holds_its_run_until_it_has_it() {
    for cold in [false, true] {
        let fixture = CardFixture::new(if cold { "retention-cold" } else { "retention" });
        fixture.state.inner.lock().unwrap().test_run_cards_epoch =
            Some(iso(chrono::Utc::now() - chrono::TimeDelta::days(2)));
        let long_ago = iso(chrono::Utc::now() - chrono::TimeDelta::days(1));
        let run_dir = fixture.write("test-carded", fixture.request(&long_ago), Some(running(7, "rust-tests")));
        fixture.refresh(&[7]);
        assert!(fixture.card_ref("test-carded").is_some());
        if cold {
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.owner).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            push_message_on_record(
                record,
                Message::Text {
                    attachments: Vec::new(),
                    id: "message-later".to_owned(),
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: "later".to_owned(),
                    expanded_text: None,
                    source: None,
                },
            );
            trim_retained_session_messages(record, 1);
        }
        // The run passes behind 50 newer terminal runs.
        let mut results = passed();
        results["runId"] = json!("test-carded");
        fs::write(run_dir.join("results.json"), results.to_string()).unwrap();
        for index in 0..TEST_RUN_TERMINAL_RUNS_PER_PROJECT {
            fixture.write(
                &format!("test-newer-{index:02}"),
                fixture.request(&iso(chrono::Utc::now() - chrono::TimeDelta::minutes(index as i64 + 30))),
                Some(passed()),
            );
        }
        fixture.refresh(&[]);
        let indexed = |fixture: &CardFixture| {
            fixture
                .state
                .test_run_summaries(None)
                .iter()
                .any(|run| run.run_id == "test-carded")
        };
        if cold {
            assert!(!indexed(&fixture), "a card out of the window does not hold its run");
            continue;
        }
        assert!(indexed(&fixture), "held until its card has the verdict");
        assert_eq!(fixture.card("test-carded").state, TestRunState::Passed);
        fixture.refresh(&[]);
        assert!(!indexed(&fixture), "then it ages out");
        assert!(fixture.card_ref("test-carded").is_none(), "and its terminal entry is pruned");
        assert_eq!(fixture.card("test-carded").state, TestRunState::Passed, "the card stays");
    }
}

#[test]
fn a_recent_terminal_card_keeps_its_entry_after_its_run_leaves() {
    let fixture = CardFixture::new("recent-prune");
    let run_dir = fixture.write("test-recent", fixture.request(&after_epoch()), Some(passed()));
    fixture.refresh(&[]);
    assert!(fixture.card_ref("test-recent").unwrap().terminal);
    fs::remove_dir_all(&run_dir).unwrap();
    fixture.refresh(&[]);
    assert!(
        fixture.card_ref("test-recent").is_some(),
        "a returning run could still qualify within 10 minutes of its start"
    );
    fixture.write("test-recent", fixture.request(&after_epoch()), Some(passed()));
    fixture.refresh(&[]);
    assert_eq!(fixture.cards().len(), 1, "never a second card");
}

#[test]
fn an_unreadable_failure_read_keeps_the_excerpt_and_is_retried() {
    let fixture = CardFixture::new("failure-retry");
    let flaky_dir = fixture.write(
        "test-a-flaky",
        fixture.request(&after_epoch()),
        Some(failed_with("first diagnostics", false)),
    );
    // A live run sorted after it: its liveness check runs between the index
    // read of the failed run and the excerpt read.
    fixture.write("test-b-live", fixture.request(&after_epoch()), Some(running(7, "rust-tests")));
    let results_path = flaky_dir.join("results.json");
    let armed = std::rc::Rc::new(std::cell::Cell::new(false));
    let refresh = |armed_now: bool| {
        armed.set(armed_now);
        let armed = armed.clone();
        let path = results_path.clone();
        let events = fixture.events.clone();
        fixture.state.refresh_test_runs_with(
            &move |_, _| {
                if armed.replace(false) {
                    fs::write(&path, "{ mid-replacement").unwrap();
                }
                true
            },
            &|event| {
                fixture.state.publish_delta(event);
                events.borrow_mut().push(event.clone());
            },
        );
    };
    refresh(false);
    let first_version = fixture.card_ref("test-a-flaky").unwrap().failure_detail_version;
    assert_eq!(
        fixture.card("test-a-flaky").failure.unwrap().excerpt,
        "first diagnostics"
    );

    // New results; their excerpt read fails this time.
    let mut second = failed_with("second diagnostics", false);
    second["runId"] = json!("test-a-flaky");
    fs::write(&results_path, second.to_string()).unwrap();
    refresh(true);
    assert_eq!(
        fixture.card("test-a-flaky").failure.unwrap().excerpt,
        "first diagnostics",
        "an unreadable read never empties the excerpt"
    );
    assert_eq!(
        fixture.card_ref("test-a-flaky").unwrap().failure_detail_version,
        first_version,
        "and is never recorded as read"
    );

    // Readable again with the same content: the summary does not change, but
    // the pending read is retried and lands.
    fs::write(&results_path, second.to_string()).unwrap();
    refresh(false);
    assert_eq!(
        fixture.card("test-a-flaky").failure.unwrap().excerpt,
        "second diagnostics"
    );
    let summary_version = fixture
        .state
        .test_run_summaries(None)
        .into_iter()
        .find(|run| run.run_id == "test-a-flaky")
        .unwrap()
        .detail_version;
    assert_eq!(
        fixture.card_ref("test-a-flaky").unwrap().failure_detail_version,
        summary_version,
        "the version of the bytes read"
    );
}

#[test]
fn a_detail_only_change_of_a_failed_run_is_recorded_without_a_card_update() {
    let fixture = CardFixture::new("failed-detail-only");
    let run_dir = fixture.write("test-failed", fixture.request(&after_epoch()), Some(failed_with("boom", false)));
    fixture.refresh(&[]);
    fixture.card_events();
    let mut results = failed_with("boom", false);
    results["runId"] = json!("test-failed");
    results["limitations"] = json!("detail only");
    fs::write(run_dir.join("results.json"), results.to_string()).unwrap();
    fixture.refresh(&[]);
    assert!(fixture.card_events().is_empty(), "the card is unchanged");
    let summary_version = fixture.state.test_run_summaries(None)[0].detail_version.clone();
    assert_eq!(
        fixture.card_ref("test-failed").unwrap().failure_detail_version,
        summary_version,
        "the read is recorded, so it is not repeated on every rescan"
    );
}

#[test]
fn a_run_whose_delta_was_not_committed_gets_no_card() {
    let mut fixture = CardFixture::new("unpublished");
    let persistence_path = fixture.state.persistence_path.clone();
    let failing = test_temp_dir().join(format!("termal-cards-persist-failure-{}", Uuid::new_v4()));
    fs::create_dir_all(&failing).unwrap();
    fixture.state.shutdown_persist_blocking();
    fixture.state.persistence_path = Arc::new(failing.clone());
    fixture.write("test-unsent", fixture.request(&after_epoch()), Some(running(7, "rust-tests")));
    fixture.refresh(&[7]);
    assert!(fixture.cards().is_empty(), "cards follow published summaries only");

    fixture.state.persistence_path = persistence_path;
    fixture.refresh(&[7]);
    assert_eq!(fixture.cards().len(), 1);
    let _ = fs::remove_dir_all(&failing);
}

#[test]
fn after_a_restart_the_transcript_decides_whether_a_card_is_terminal() {
    // The map reached disk claiming a terminal card while the transcript row
    // still holds the running snapshot (a row deferred to a later persist
    // tick, then a crash). The first scan after the restart trusts the
    // transcript: the run is gone, so the card reads notIndexed.
    let (_temp_root, project_root, persistence_path, templates_path) =
        super::delegation_support::temp_delegation_state_paths();
    run_git_test_command(&project_root, &["init", "--quiet"]);
    let boot = || {
        AppState::new_with_paths(
            project_root.to_string_lossy().into_owned(),
            persistence_path.clone(),
            templates_path.clone(),
        )
        .expect("state should boot")
    };
    let run_dir = project_root.join(".git").join("review-runs").join("test-crash");
    let owner = {
        let state = boot();
        assert!(state.ensure_test_run_cards_epoch());
        let owner = test_session_id(&state, Agent::Claude);
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(
            run_dir.join("request.json"),
            json!({ "runId": "test-crash", "root": project_root.to_string_lossy(), "full": true,
                    "owner": owner, "started": after_epoch() })
            .to_string(),
        )
        .unwrap();
        let mut results = running(7, "rust-tests");
        results["runId"] = json!("test-crash");
        fs::write(run_dir.join("results.json"), results.to_string()).unwrap();
        state.refresh_test_runs_with(&|pid, _| pid == 7, &|event| state.publish_delta(event));
        {
            let mut inner = state.inner.lock().unwrap();
            inner.test_run_cards.get_mut("test-crash").unwrap().terminal = true;
            state.commit_locked(&mut inner).unwrap();
        }
        state.shutdown_persist_blocking();
        owner
    };
    fs::remove_dir_all(&run_dir).unwrap();

    let restarted = boot();
    restarted.refresh_test_runs_with(&|_, _| false, &|event| restarted.publish_delta(event));
    let inner = restarted.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&owner).unwrap()];
    let card = record
        .session
        .messages
        .iter()
        .find_map(|message| match message {
            Message::TestRun { run, .. } => Some(run.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(card.state, TestRunState::Unknown);
    assert_eq!(card.unknown_reason, Some(TestRunCardUnknownReason::NotIndexed));
    drop(inner);
    restarted.shutdown_persist_blocking();
}

#[test]
fn the_epoch_is_stored_once_and_reloads_as_durable() {
    let fixture = CardFixture::new("epoch");
    let epoch = fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_cards_epoch
        .clone()
        .expect("the first boot stores an epoch");
    assert!(fixture.state.ensure_test_run_cards_epoch());
    assert_eq!(
        fixture.state.inner.lock().unwrap().test_run_cards_epoch.as_deref(),
        Some(epoch.as_str()),
        "never derived from now again"
    );
    let reloaded = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(reloaded.test_run_cards_epoch.as_deref(), Some(epoch.as_str()));
    assert!(reloaded.test_runs.cards_enabled, "a loaded epoch is durable");

    // A fresh state has no epoch and creates no card until it is durable.
    let fresh = CardFixture::new("no-epoch-yet");
    {
        let mut inner = fresh.state.inner.lock().unwrap();
        inner.test_runs.cards_enabled = false;
        inner.test_run_cards_epoch = Some(iso(chrono::Utc::now() - chrono::TimeDelta::hours(1)));
    }
    fresh.write("test-early", fresh.request(&after_epoch()), Some(running(7, "rust-tests")));
    fresh.refresh(&[7]);
    assert!(fresh.cards().is_empty());
}

#[test]
fn the_epoch_never_blocks_rescans_and_cards_follow_once_it_is_durable() {
    // The persist worker is ours: the epoch's fence resolves only when this
    // test says so, after two rescans.
    let mut fixture = CardFixture::new_without_epoch("epoch-step");
    let (persist_tx, persist_rx) = mpsc::channel();
    fixture.state.persist_tx = persist_tx;
    let next_fence = |rx: &mpsc::Receiver<PersistRequest>| {
        rx.try_iter()
            .find_map(|request| match request {
                PersistRequest::Fence(fence) => Some(fence),
                _ => None,
            })
            .expect("the epoch's fence was sent")
    };
    let mut pending = None;
    assert!(!fixture.state.step_test_run_cards_epoch(&mut pending));
    let epoch = fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_cards_epoch
        .clone()
        .expect("the epoch is set at once");

    // A run that starts after the epoch, published while it is not durable.
    let run_dir = fixture.write("test-early", fixture.request(&after_epoch()), Some(running(7, "rust-tests")));
    fixture.refresh(&[7]);
    assert_eq!(fixture.state.test_run_summaries(None).len(), 1, "rescans publish runs");
    assert!(!fixture.state.step_test_run_cards_epoch(&mut pending), "still pending, never waits");
    // It ends before the epoch is durable, so its summary changes no more.
    let mut results = passed();
    results["runId"] = json!("test-early");
    fs::write(run_dir.join("results.json"), results.to_string()).unwrap();
    // A second run fails in the same window.
    fixture.write(
        "test-early-failed",
        fixture.request(&after_epoch()),
        Some(failed_with("early failure", false)),
    );
    fixture.refresh(&[]);
    assert!(fixture.cards().is_empty(), "no card before the epoch is durable");

    // A failed fence is retried with the same epoch.
    next_fence(&persist_rx).finish(Err(PersistFenceError::WriteFailed("disk".to_owned())));
    assert!(!fixture.state.step_test_run_cards_epoch(&mut pending));
    assert_eq!(
        fixture.state.inner.lock().unwrap().test_run_cards_epoch.as_deref(),
        Some(epoch.as_str())
    );
    // Durable now: the next scan re-evaluates the run it already published.
    next_fence(&persist_rx).finish(Ok(()));
    assert!(fixture.state.step_test_run_cards_epoch(&mut pending));
    fixture.refresh(&[]);
    assert_eq!(fixture.card("test-early").state, TestRunState::Passed);
    assert_eq!(
        fixture.card("test-early-failed").failure.map(|failure| failure.excerpt).as_deref(),
        Some("early failure"),
        "the re-evaluating scan reads the excerpt too"
    );
}

#[test]
fn a_hidden_or_remote_proxy_owner_gets_no_card() {
    for (label, hidden) in [("hidden-owner", true), ("remote-owner", false)] {
        let fixture = CardFixture::new(label);
        {
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.owner).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            if hidden {
                record.hidden = true;
            } else {
                record.remote_id = Some("remote-a".to_owned());
                record.remote_session_id = Some("remote-session-1".to_owned());
            }
        }
        fixture.write("test-owned", fixture.request(&after_epoch()), Some(running(7, "rust-tests")));
        fixture.refresh(&[7]);
        assert!(fixture.cards().is_empty(), "{label}");
        assert!(fixture.card_ref("test-owned").is_none(), "{label}");
    }
}

#[test]
fn cards_are_reconciled_by_the_first_scan_after_a_restart() {
    let (_temp_root, project_root, persistence_path, templates_path) =
        super::delegation_support::temp_delegation_state_paths();
    run_git_test_command(&project_root, &["init", "--quiet"]);
    let boot = || {
        AppState::new_with_paths(
            project_root.to_string_lossy().into_owned(),
            persistence_path.clone(),
            templates_path.clone(),
        )
        .expect("state should boot")
    };
    let runs_dir = project_root.join(".git").join("review-runs");
    let write = |run_id: &str, owner: &str, results: Value| {
        let run_dir = runs_dir.join(run_id);
        fs::create_dir_all(&run_dir).unwrap();
        let request = json!({ "runId": run_id, "root": project_root.to_string_lossy(),
            "full": true, "owner": owner, "started": after_epoch() });
        fs::write(run_dir.join("request.json"), request.to_string()).unwrap();
        let mut results = results;
        results["runId"] = json!(run_id);
        fs::write(run_dir.join("results.json"), results.to_string()).unwrap();
    };
    let refresh = |state: &AppState, alive: &'static [u32]| {
        state.refresh_test_runs_with(&move |pid, _| alive.contains(&pid), &|event| {
            state.publish_delta(event)
        });
    };
    let (resident_owner, cold_owner) = {
        // Booting creates the default project at the project root.
        let state = boot();
        assert!(state.ensure_test_run_cards_epoch());
        let resident_owner = test_session_id(&state, Agent::Claude);
        let cold_owner = test_session_id(&state, Agent::Codex);
        write("test-resident", &resident_owner, running(7, "rust-tests"));
        write("test-cold", &cold_owner, running(7, "rust-tests"));
        refresh(&state, &[7]);
        // Enough later messages that the cold card is outside the tail a
        // reload keeps in memory.
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&cold_owner).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            for number in 0..SESSION_IN_MEMORY_MESSAGE_LIMIT + 4 {
                push_message_on_record(
                    record,
                    Message::Text {
                        attachments: Vec::new(),
                        id: format!("message-later-{number}"),
                        timestamp: stamp_now(),
                        author: Author::Assistant,
                        text: "later".to_owned(),
                        expanded_text: None,
                        source: None,
                    },
                );
            }
            state.commit_locked(&mut inner).unwrap();
        }
        state.shutdown_persist_blocking();
        (resident_owner, cold_owner)
    };
    // Both runs pass while TermAl is down.
    write("test-resident", &resident_owner, passed());
    write("test-cold", &cold_owner, passed());

    let restarted = boot();
    assert!(restarted.inner.lock().unwrap().test_runs.cards_enabled, "a loaded epoch is durable");
    refresh(&restarted, &[]);
    let inner = restarted.inner.lock().unwrap();
    let card = |owner: &str| {
        let record = &inner.sessions[inner.find_session_index(owner).unwrap()];
        record.session.messages.iter().find_map(|message| match message {
            Message::TestRun { run, .. } => Some(run.clone()),
            _ => None,
        })
    };
    assert_eq!(card(&resident_owner).map(|run| run.state), Some(TestRunState::Passed));
    assert!(inner.test_run_cards["test-resident"].terminal);
    assert!(card(&cold_owner).is_none(), "outside the reloaded tail");
    assert!(!inner.test_run_cards["test-cold"].terminal, "kept for a later pass");
    drop(inner);
    restarted.shutdown_persist_blocking();
}

#[test]
fn a_card_whose_session_is_gone_leaves_the_map() {
    let fixture = CardFixture::new("session-gone");
    let run_dir = fixture.write("test-orphan", fixture.request(&after_epoch()), Some(running(7, "rust-tests")));
    fixture.refresh(&[7]);
    assert!(fixture.card_ref("test-orphan").is_some());
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.owner).unwrap();
        inner.sessions.remove(index);
    }
    let mut results = passed();
    results["runId"] = json!("test-orphan");
    fs::write(run_dir.join("results.json"), results.to_string()).unwrap();
    fixture.refresh(&[]);
    assert!(fixture.card_ref("test-orphan").is_none());
}

#[test]
fn a_card_outside_the_in_memory_window_is_not_updated_yet() {
    // Documented limit of this changeset: a card whose message has left the
    // session's in-memory transcript window keeps its entry and is not
    // updated, and nothing else is touched.
    let fixture = CardFixture::new("cold");
    let run_dir = fixture.write("test-cold", fixture.request(&after_epoch()), Some(running(7, "rust-tests")));
    fixture.refresh(&[7]);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.owner).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        push_message_on_record(
            record,
            Message::Text {
                attachments: Vec::new(),
                id: "message-later".to_owned(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "later".to_owned(),
                expanded_text: None,
                source: None,
            },
        );
        trim_retained_session_messages(record, 1);
    }
    let mut results = passed();
    results["runId"] = json!("test-cold");
    fs::write(run_dir.join("results.json"), results.to_string()).unwrap();
    fixture.refresh(&[]);
    assert!(fixture.card_events().iter().all(|event| !matches!(event, DeltaEvent::TestRunCardUpdated { .. })));
    let card_ref = fixture.card_ref("test-cold").expect("kept for a later pass");
    assert!(!card_ref.terminal);
}

#[test]
fn the_card_and_its_update_have_the_contract_wire_shape() {
    let fixture = CardFixture::new("wire");
    fixture.write("test-wire", fixture.request(&after_epoch()), Some(running(7, "rust-tests")));
    fixture.refresh(&[7]);
    let message = fixture
        .owner_messages()
        .into_iter()
        .find(|message| matches!(message, Message::TestRun { .. }))
        .unwrap();
    let wire = serde_json::to_value(&message).unwrap();
    assert_eq!(wire["type"], "testRun");
    assert_eq!(wire["author"], "system");
    assert_eq!(wire["schemaVersion"], 1);
    for key in [
        "runId", "worktree", "runDir", "preset", "detached", "state", "interrupted",
        "currentStage", "startedAt", "endedAt", "exitCode", "stages", "stagesOmitted",
        "error", "errorTruncated", "failure", "command", "commandTruncated",
    ] {
        assert!(wire["run"].get(key).is_some(), "{key}: {wire}");
    }
    assert!(wire["run"].get("unknownReason").is_none(), "only when unknown");
    let round_trip: Message = serde_json::from_value(wire).unwrap();
    assert_eq!(round_trip.id(), message.id());

    let Message::TestRun { run, .. } = message else { unreachable!() };
    let update = serde_json::to_value(DeltaEvent::TestRunCardUpdated {
        revision: 9,
        session_id: "session-1".to_owned(),
        message_id: "message-1".to_owned(),
        message_index: 3,
        message_count: 4,
        preview: "Test run (full): running".to_owned(),
        session_mutation_stamp: None,
        run,
    })
    .unwrap();
    assert_eq!(update["type"], "testRunCardUpdated");
    for key in ["revision", "sessionId", "messageId", "messageIndex", "messageCount", "preview", "run"] {
        assert!(update.get(key).is_some(), "{key}: {update}");
    }
    assert!(update.get("sessionMutationStamp").is_none());
}
