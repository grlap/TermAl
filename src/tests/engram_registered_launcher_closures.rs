// Registered launcher notices use real recorder replies and independent run
// artifacts. Ordinary observations never confer native verification credit.
use super::*;

const COMMAND: &str = "node scripts/test-launcher.mjs focused -- node --test scripts/test-launcher.test.mjs";
const KEY: &str = "live-launcher";
const COMMITTED_COMMAND: &str = "node scripts/test-launcher.mjs focused --input commit -- node --test scripts/test-launcher.test.mjs";

fn clean_owner_source(root: &FsPath) -> Value {
    assert!(run_git_test_command_output(root, &["status", "--porcelain"]).is_empty(),
        "the receipt fixture measures an actual clean checkout");
    json!({
        "commit": run_git_test_command_output(root, &["rev-parse", "HEAD"]).trim(),
        "tree": run_git_test_command_output(root, &["rev-parse", "HEAD^{tree}"]).trim(),
        "submodules": [], "changes": [],
    })
}

// Scoped protocol input, not a source cache. Real pinned-Git controls below
// bypass it. Dropping a fixture removes its unique run's reply on every exit.
struct NoticeSourceReply(String);

impl Drop for NoticeSourceReply {
    fn drop(&mut self) {
        TEST_RECEIVED_NOTICE_SOURCE_RESULTS.lock().unwrap().remove(&self.0);
        TEST_RECEIVED_NOTICE_SOURCE_READ.lock().unwrap().remove(&self.0);
    }
}

struct RegisteredLauncherTurn {
    turn: EOCheckedTurn,
    recorder: SessionRecorder,
    run_id: String,
    directory: PathBuf,
    started: String,
    command: String,
    source_id: Option<Value>,
    notice_source: std::cell::RefCell<Option<NoticeSourceReply>>,
}

impl RegisteredLauncherTurn {
    fn start() -> Self {
        Self::with_turn(EOCheckedTurn::start("registered-launcher-close", true))
    }

    fn with_turn(turn: EOCheckedTurn) -> Self {
        let mut recorder = turn.recorder();
        let cwd = turn.record(|record| record.engram.observed_source_root()
            .map(|root| root.root.clone()));
        recorder.command_started_in(KEY, COMMAND, Some(COMMAND), cwd.as_deref()).unwrap();
        Self::after_start(turn, recorder)
    }

    fn after_start(turn: EOCheckedTurn, recorder: SessionRecorder) -> Self {
        Self::after_start_with_input(turn, recorder, false)
    }

    fn committed() -> Self {
        let turn = Self::committed_turn("received-launcher-notice");
        let cwd = turn.record(|record| record.engram.observed_source_root().unwrap().root.clone());
        let mut recorder = turn.recorder();
        recorder.command_started_in(KEY, COMMITTED_COMMAND, Some(COMMITTED_COMMAND), Some(&cwd)).unwrap();
        Self::after_start_with_input(turn, recorder, true)
    }

    fn committed_turn(label: &str) -> EOCheckedTurn {
        let turn = EOCheckedTurn::idle(label, true);
        // The production reader ignores global/system Git configuration.
        // Pin fixture checkout conversion locally before creating the linked
        // worktree, so a Windows system autocrlf setting cannot dirty its bytes
        // when the pinned verifier reads them. Never change global config.
        run_git_test_command(&turn.root, &["config", "core.autocrlf", "false"]);
        let worktree = turn.root.join(".worktrees").join("wt");
        run_git_test_command(&turn.root, &["worktree", "add", "--quiet", "-b", "wt", worktree.to_str().unwrap()]);
        let binding = turn.record(|record| record.engram.work_binding.clone().unwrap());
        turn.state.name_engram_source_root(&turn.session_id, EngramSourceRootRequest {
            work:binding.work_id, path:Some(Some(worktree.to_string_lossy().into_owned())),
        }).unwrap();
        turn.begin("Run the committed checks.");
        turn
    }

    fn after_start_with_input(turn: EOCheckedTurn, recorder: SessionRecorder, committed: bool) -> Self {
        turn.wait_for_snapshots();
        let open = turn.record(|record| record.engram.active_turn_checks.iter()
            .filter(|check| check.end.is_none()).count());
        assert_eq!(open, 1);
        let started = engram_command_started_at();
        let run_id = format!("test-{}", Uuid::new_v4());
        let root = turn.record(|record| record.engram.active_turn_checks.iter()
            .find(|check| check.key == KEY).unwrap().target.root.clone());
        let directory = EngramHost::run_artifact_directory(&root).unwrap().join(&run_id);
        fs::create_dir_all(&directory).unwrap();
        let mut request = json!({
            "runId": run_id, "root": root.to_string_lossy(),
            "owner": turn.session_id, "creatorPid": std::process::id(),
            "started": started, "full": false, "detached": false,
            "stages": [{ "name": "focused", "command": "node",
                "args": ["--test", "scripts/test-launcher.test.mjs"] }],
        });
        let source_id = committed.then(|| clean_owner_source(&root));
        if let Some(source) = &source_id {
            request["input"] = json!("commit");
            request["sourceId"] = source.clone();
        }
        fs::write(directory.join("request.json"), serde_json::to_vec(&request).unwrap()).unwrap();
        let command = if committed { COMMITTED_COMMAND } else { COMMAND }.to_owned();
        let fixture = Self { turn, recorder, run_id, directory, started, command, source_id,
            notice_source: std::cell::RefCell::new(None) };
        fixture.write_results("running");
        assert_eq!(TestRunDisk::read(&fixture.directory, None).unwrap().state(&|_, _| false),
            TestRunState::Running);
        let ack = fixture.turn.state.register_test_run(&fixture.turn.session_id, RegisterTestRunRequest {
            run_id: fixture.run_id.clone(), run_directory: test_run_display_path(&fixture.directory),
        }).expect("durable registration precedes terminal publication");
        assert!(ack.registered);
        fixture.assert_unindexed();
        fixture
    }

    fn write_results(&self, state: &str) {
        let results = if state == "running" {
            json!({ "runId": self.run_id, "state": state, "started": self.started,
                "pid": std::process::id(), "heartbeat": { "at": test_run_timestamp(), "everyMs": 60_000 },
                "stages": [{ "name": "focused", "state": state }] })
        } else {
            json!({ "runId": self.run_id, "state": state, "started": self.started,
                "ended": test_run_timestamp(), "exitCode": if state == "passed" { 0 } else { 1 },
                "stages": [{ "name": "focused", "state": state, "code": if state == "passed" { 0 } else { 1 } }] })
        };
        fs::write(self.directory.join("results.json"), serde_json::to_vec(&results).unwrap()).unwrap();
    }

    fn background_reply(&mut self) {
        // The provider's real reply carries a locator and explicitly says the
        // process has not ended. No direct artifact-owner injection.
        self.recorder.command_completed_with_exit(KEY, &self.command,
            &format!("RUN {}", test_run_display_path(&self.directory)),
            CommandStatus::Running, EngramCommandExit::NotFinished).unwrap();
        let (associated, open) = {
            let mut inner = self.turn.state.inner.lock().unwrap();
            let associated = inner.test_run_observations.values().any(|row|
                row.artifact.as_ref().is_some_and(|artifact| artifact.run_id == self.run_id));
            let record = &inner.sessions[inner.find_session_index(&self.turn.session_id).unwrap()];
            let open = record.engram.active_turn_checks.iter().any(|check| check.key == KEY && check.end.is_none());
            // Only the process-local projection is absent at the publication
            // boundary; durable registration and original observation remain.
            inner.test_runs.entries.clear();
            (associated, open)
        };
        assert!(associated);
        assert!(open, "a NotFinished launcher reply cannot end its check");
        self.assert_unindexed();
    }

    // Synthetic launcher/producer facts through the production body builder
    // and receipt owner; no executed consumer credit is asserted by the fixture.
    fn receive(&self, passed: bool) -> EngramVerificationRecordReceipt {
        self.receive_until(passed, None).0
    }

    fn receive_until(&self, passed: bool, deadline: Option<std::time::Instant>)
        -> (EngramVerificationRecordReceipt, EngramVerificationRecordSettlement) {
        let registration = self.turn.state.inner.lock().unwrap()
            .test_run_registrations[&self.run_id].clone();
        let git = ReviewFreezeGit::new(FsPath::new(&registration.worktree))
            .expect("fixture root must satisfy the pinned Git reader");
        let (head, dirty) = committed_source_status(&git).expect("fixture Git reads must be complete");
        assert!(dirty.is_empty(), "receipt fixture must be clean: {dirty:?}");
        self.set_notice_source(TestReceivedNoticeSourceResult::Status(head.clone(), dirty));
        let source_before = self.source_id.as_ref().expect("a native receipt has an admitted owner identity");
        let source_after = clean_owner_source(FsPath::new(&registration.worktree));
        let at = test_run_timestamp();
        fs::write(self.directory.join("results.json"), serde_json::to_vec(&json!({
            "runId":self.run_id, "state":if passed {"passed"} else {"failed"},
            "started":self.started, "ended":at, "exitCode":if passed {0} else {1},
            "sourceIdBefore":source_before, "sourceIdAfter":source_after,
            "stages":[{"name":"focused", "state":if passed {"passed"} else {"failed"},
                "code":if passed {0} else {1}, "started":at, "ended":at}],
            "input":{"kind":"commit", "commit":head, "cleanBefore":true,
                "after":{"commit":head,"clean":true,"paths":[]}, "outcome":"clean","eligible":true}
        })).unwrap()).unwrap();
        let EngramVerificationRecordPrepared::Ready(frozen) = engram_verification_record_prepare(&registration)
            else { panic!("clean committed fixture is ready"); };
        let receipt = EngramVerificationRecordReceipt {
            operation:"0123456789abcdef0123456789abcdef".into(),
            producer:"0123456789abcdef0123456789abcdef".into(),
            verifications:vec![EngramVerificationRecordVerification {
                stage:"focused".into(), record_id:"fedcba9876543210fedcba9876543210".into() }],
            run_outcome:frozen.body.run.outcome,
        };
        let settlement = self.turn.state.complete_engram_verification_record_until(
            &registration, &frozen.body, &receipt,
            deadline.unwrap_or_else(|| std::time::Instant::now() + DEADLOCK_GUARD));
        self.wait_for_received_notices();
        assert!(matches!(self.turn.state.inner.lock().unwrap().test_run_registrations[&self.run_id]
            .verification_record, Some(TestRunVerificationAck::Received { .. } | TestRunVerificationAck::Settled { .. })));
        (receipt, settlement)
    }

    fn set_notice_source(&self, result: TestReceivedNoticeSourceResult) {
        TEST_RECEIVED_NOTICE_SOURCE_RESULTS.lock().unwrap().insert(self.run_id.clone(), result);
        if self.notice_source.borrow().is_none() {
            *self.notice_source.borrow_mut() = Some(NoticeSourceReply(self.run_id.clone()));
        }
    }

    fn receive_not_durable(&mut self) -> EngramVerificationRecordReceipt {
        let original_writer = self.turn.state.persist_tx.clone();
        let (held_tx, held_writer) = mpsc::channel();
        self.turn.state.persist_tx = held_tx;
        let expired = self.turn.state.engram_budget_clock().now() - Duration::from_secs(1);
        let (receipt, settlement) = self.receive_until(true, Some(expired));
        assert!(matches!(settlement, EngramVerificationRecordSettlement::NotDurable { .. }));
        self.turn.state.persist_tx = original_writer;
        drop(held_writer);
        let registration = self.turn.state.inner.lock().unwrap().test_run_registrations[&self.run_id].clone();
        assert!(matches!(registration.verification_record, Some(TestRunVerificationAck::Received { .. })));
        assert!(ReceivedCheckNotice::read(&registration,
            std::time::Instant::now() + RECEIVED_NOTICE_CAPTURE_BUDGET).is_none(),
            "a held receipt cannot start presentation or renew its failed association fence");
        receipt
    }

    fn capture_dirty_notice_source(&self) {
        let registration = self.turn.state.inner.lock().unwrap()
            .test_run_registrations[&self.run_id].clone();
        // Predicate correctness uses the real pinned reader's default bound.
        // Presentation then consumes this known Dirty result deterministically.
        let git = ReviewFreezeGit::new(FsPath::new(&registration.worktree)).unwrap();
        let (head, dirty) = committed_source_status(&git).unwrap();
        assert!(dirty.iter().any(|path| path.path == "README.md"), "{dirty:?}");
        self.set_notice_source(TestReceivedNoticeSourceResult::Status(head, dirty));
    }

    fn assert_unindexed(&self) {
        let indexed = self.turn.state.inner.lock().unwrap().test_runs.find(&self.run_id).is_some();
        assert!(!indexed);
    }

    fn line(&self) -> (String, bool) {
        self.turn.record(|record| (
            record.engram.pending_source_root_line.clone().unwrap_or_default(),
            record.engram.unended_check_closures.iter().any(|closed| closed.key == KEY),
        ))
    }

    fn finish(&self) -> (String, bool) {
        let observation = self.turn.finish();
        self.wait_for_received_notices();
        assert_ordinary_observation_has_no_native_credit(&self.turn, &observation);
        let checks = observation["occurrence"]["observed_checks"].as_array().unwrap();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0]["observed_result"], "indeterminate");
        assert!(checks[0].get("finished_at").is_none());
        self.line()
    }

    fn wait_for_received_notices(&self) {
        let captures = &self.turn.state.capture_lifetime;
        let deadline = std::time::Instant::now() + DEADLOCK_GUARD;
        let mut workers = captures.state.lock().unwrap();
        while workers.workers.iter().any(|worker|
            worker.thread.thread().name() == Some("termal-received-launcher-notice")
                && !worker.done.load(std::sync::atomic::Ordering::Acquire)) {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            assert!(!remaining.is_zero(), "owned received notice did not finish");
            workers = captures.changed.wait_timeout(workers, remaining).unwrap().0;
        }
    }

    fn assert_advice(&self, line: &str) {
        assert!(line.contains(&self.run_id), "{line}");
        assert!(!line.to_ascii_lowercase().contains("run it again"), "{line}");
        assert!(!line.to_ascii_lowercase().contains("run the check again"), "{line}");
    }
}

#[test]
fn a_registered_launcher_locator_in_a_codex_output_delta_keeps_its_exact_notice() {
    let (associated, closed, line) = codex_delta_locator_notice("matching");
    assert!(associated && !closed && line.contains("correlated with")
        && line.contains("registered launcher run") && !line.contains("closed with outcome Unknown"),
        "an exact item's delta locator must survive turn close: associated={associated}, closed={closed}, {line}");
}

#[test]
fn a_codex_delta_locator_keeps_native_identity_and_output_boundaries() {
    for case in ["wrong-thread", "wrong-turn", "wrong-item", "missing-item", "partial", "oversized", "cleared", "conflicting", "generation", "runtime"] {
        let (associated, closed, line) = codex_delta_locator_notice(case);
        assert!(closed && !line.contains("correlated with") && line.contains("closed with outcome Unknown"),
            "{case}: associated={associated}, closed={closed}, {line}");
        assert_eq!(associated, case == "conflicting", "{case}: only an earlier valid locator remains historical");
    }
    let (associated, closed, line) = codex_delta_locator_notice("split");
    assert!(associated && !closed && line.contains("correlated with"), "{line}");
}

fn codex_delta_locator_notice(case: &str) -> (bool, bool, String) {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let committed = case == "received-delta";
    let turn = if committed { RegisteredLauncherTurn::committed_turn("received-launcher-output-delta") }
        else { EOCheckedTurn::start("registered-launcher-output-delta", true) };
    let command = if committed { COMMITTED_COMMAND } else { COMMAND };
    let cwd = turn.record(|record| record.engram.observed_source_root()
        .map(|root| root.root.clone()).unwrap_or_else(|| record.session.workdir.clone()));
    let (runtime, generation) = turn.record(|record| {
        let SessionRuntime::Codex(handle) = &record.runtime else { panic!("Codex fixture") };
        (handle.shared_session.as_ref().unwrap().runtime.clone(), record.active_turn_generation)
    });
    let thread = "registered-launcher-delta-thread";
    let native_turn = "registered-launcher-delta-turn";
    runtime.sessions.lock().unwrap().insert(turn.session_id.clone(), SharedCodexSessionState {
        thread_id: Some(thread.to_owned()),
        turn_id: Some(native_turn.to_owned()),
        turn_started: true,
        active_turn_generation: Some(generation),
        ..SharedCodexSessionState::default()
    });
    runtime.thread_sessions.lock().unwrap().insert(thread.to_owned(), turn.session_id.clone());
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let dispatch = |turn: &EOCheckedTurn, message: Value| {
        handle_shared_codex_app_server_message(
            &message, &turn.state, &runtime.runtime_id, &pending,
            &runtime.sessions, &runtime.thread_sessions, &runtime.input_tx,
        ).unwrap();
    };
    dispatch(&turn, json!({"method":"item/started", "params":{
        "threadId":thread, "turnId":native_turn,
        "item":{"type":"commandExecution", "id":KEY, "command":command,
            "cwd":cwd, "status":"inProgress"}
    }}));
    let recorder = turn.recorder();
    let fixture = RegisteredLauncherTurn::after_start_with_input(turn, recorder, committed);
    let original_runtime = fixture.turn.record(|record| record.runtime.clone());
    assert!(fixture.turn.state.inner.lock().unwrap().test_run_observations.values()
        .all(|row| row.artifact.is_none()), "registration alone cannot associate a command");
    let mut delta = json!({"method":"item/commandExecution/outputDelta", "params":{
        "threadId":thread, "turnId":native_turn, "itemId":KEY,
        "delta":format!("RUN {}\n", test_run_display_path(&fixture.directory))
    }});
    match case {
        "wrong-thread" => delta["params"]["threadId"] = json!("another-thread"),
        "wrong-turn" => delta["params"]["turnId"] = json!("another-turn"),
        "wrong-item" => delta["params"]["itemId"] = json!("another-item"),
        "missing-item" => { delta["params"].as_object_mut().unwrap().remove("itemId"); }
        "partial" => delta["params"]["delta"] = json!("RUN "),
        "oversized" => delta["params"]["delta"] = json!("x".repeat(256 * 1024 + 1)),
        "cleared" => clear_codex_turn_state(&mut runtime.sessions.lock().unwrap()
            .get_mut(&fixture.turn.session_id).unwrap().turn_state),
        "generation" => fixture.turn.record_mut(|record| record.active_turn_generation += 1),
        "runtime" => fixture.turn.record_mut(|record| record.runtime = SessionRuntime::None),
        "split" => {
            let rest = delta["params"]["delta"].as_str().unwrap()[2..].to_owned();
            delta["params"]["delta"] = json!("RU");
            dispatch(&fixture.turn, delta.clone());
            delta["params"]["delta"] = json!(rest);
        }
        "matching" | "conflicting" | "running" | "received-delta" => {}
        _ => panic!("unknown case {case}"),
    }
    dispatch(&fixture.turn, delta.clone());
    if case == "generation" { fixture.turn.record_mut(|record| record.active_turn_generation = generation); }
    if case == "runtime" { fixture.turn.record_mut(|record| record.runtime = original_runtime); }
    if case == "conflicting" {
        delta["params"]["delta"] = json!(format!("RUN {}\n", fixture.directory.with_file_name(format!("test-{}", Uuid::new_v4())).display()));
        dispatch(&fixture.turn, delta);
    }
    let associated = fixture.turn.state.inner.lock().unwrap().test_run_observations.values().any(|row|
        row.artifact.as_ref().is_some_and(|artifact| artifact.run_id == fixture.run_id));
    if committed { fixture.receive(true); }
    else if case != "running" { fixture.write_results("passed"); }
    let request = fixture.turn.finish();
    fixture.wait_for_received_notices();
    assert_ordinary_observation_has_no_native_credit(&fixture.turn, &request);
    let (line, closed) = fixture.line();
    if !closed { assert!(line.contains(&fixture.run_id), "{line}"); }
    if case == "running" {
        assert!(fixture.turn.record(|record| record.engram.running_codex_tests.iter().any(|test| test.key == KEY)));
        fixture.turn.begin("Run a successor check.");
        let mut recorder = fixture.turn.recorder();
        recorder.command_started("delta-successor", SIZE_TEST).unwrap();
        assert!(fixture.turn.record(|record| record.engram.active_turn_checks.iter()
            .any(|check| check.key == "delta-successor" && check.overlap_cause.is_some())),
            "a locator cannot end a still-running writer");
    }
    (associated, closed, line)
}

#[test]
fn a_codex_delta_locator_preserves_running_writer_custody() {
    let (associated, closed, line) = codex_delta_locator_notice("running");
    assert!(associated && !closed && line.contains("still running"), "{line}");
}

#[test]
fn a_codex_delta_locator_names_the_same_commands_received_verification() {
    let (associated, closed, line) = codex_delta_locator_notice("received-delta");
    assert!(associated && !closed && line.contains("Engram received")
        && line.contains("fedcba9876543210fedcba9876543210") && !line.contains("outcome Unknown"), "{line}");
}

#[test]
fn registered_launcher_closures_discriminate_checkout_conversion_from_clean_source() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let turn = RegisteredLauncherTurn::committed_turn("receipt-checkout-conversion");
    let clean = turn.record(|record| record.engram.observed_source_root().unwrap().root.clone());
    assert!(committed_source_clean_head_within(FsPath::new(&clean), None).is_some());
    let converted = turn.root.join(".worktrees").join("converted");
    // Models the inherited Windows autocrlf=true checkout of the failed
    // fixture, independently of the machine running this diagnostic.
    run_git_test_command(&turn.root, &["-c", "core.autocrlf=true", "worktree", "add", "--quiet", "-b", "converted", converted.to_str().unwrap()]);
    assert!(fs::read(converted.join("README.md")).unwrap().windows(2).any(|pair| pair == b"\r\n"));
    assert!(run_git_test_command_output(&converted, &["-c", "core.autocrlf=true", "status", "--porcelain"]).is_empty(),
        "conversion-aware fixture Git sees a clean checkout");
    // Force a content reread instead of depending on Git's stat-cache age.
    fs::OpenOptions::new().write(true).open(converted.join("README.md")).unwrap()
        .set_modified(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1)).unwrap();
    let git = ReviewFreezeGit::new(&converted).unwrap();
    let (_, dirty) = committed_source_status(&git).unwrap();
    assert!(dirty.iter().any(|path| path.path == "README.md"), "{dirty:?}");
    assert!(committed_source_clean_head_within(&converted, None).is_none(),
        "the unchanged production predicate must reject conversion-dependent bytes");
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn a_received_registered_launcher_names_its_record_before_or_after_turn_close() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for late in [false, true] {
        for passed in [false, true] {
            let mut fixture = RegisteredLauncherTurn::committed();
            fixture.background_reply();
            if late { fixture.finish(); }
            let receipt = fixture.receive(passed);
            if !late { fixture.finish(); }
            let (line, closed) = fixture.line();
            assert!(!closed && line.contains(&fixture.run_id)
                && line.contains(&receipt.verifications[0].record_id)
                && line.contains("Engram received") && !line.contains("outcome Unknown"), "late={late} passed={passed}: {line}");
            assert!(line.contains(if passed { "passed" } else { "failed" }), "{line}");
            let prompt = fixture.turn.begin_prompt("Report the registered outcome.").prompt;
            assert!(prompt.contains(&receipt.verifications[0].record_id) && !prompt.contains("outcome Unknown"), "{prompt}");
        }
    }
}

#[test]
fn a_received_record_does_not_associate_an_unrelated_launcher_or_hide_dirty_source() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for correlated in [false, true] {
        let mut fixture = RegisteredLauncherTurn::committed();
        if correlated { fixture.background_reply(); }
        let receipt = fixture.receive(true);
        if correlated {
            let root = fixture.turn.state.inner.lock().unwrap().test_run_registrations[&fixture.run_id].worktree.clone();
            fs::write(FsPath::new(&root).join("README.md"), "changed after verification\n").unwrap();
            fixture.capture_dirty_notice_source();
        }
        let (line, closed) = fixture.finish();
        // This named-root fixture has no provider end. The pre-existing
        // carry owner must keep its original writer/end-by-report custody;
        // a native receipt (correlated or otherwise) cannot end that process.
        assert!(!closed && fixture.turn.record(|record|
            record.engram.running_codex_tests.iter().any(|test| test.key == KEY)
                && record.engram.carried_checks.iter().any(|carry|
                    carry.check.key == KEY && carry.ends_by_report && carry.check.end.is_none())), "{line}");
        assert!(!line.contains(&receipt.verifications[0].record_id), "{line}");
        if !correlated {
            assert!(line.contains("relationship to this command is unconfirmed")
                && !line.contains("correlated with this command") && !line.contains("Engram received"), "{line}");
        }
    }
}

#[test]
fn a_late_received_notice_preserves_other_warnings_and_an_in_flight_delivery() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::committed();
    fixture.background_reply();
    fixture.finish();
    fixture.turn.record_mut(|record| {
        record.engram.set_pending_source_root_line(format!("{ENGRAM_CHECK_CREDIT_LINE_PREFIX} unrelated check remains Unknown"));
        record.engram.source_root_line_delivery = Some((record.engram.pending_source_root_line.clone().unwrap(), record.active_turn_generation));
    });
    let generation = fixture.turn.record(|record| record.active_turn_generation);
    let feedback_revision = fixture.turn.record(|record| record.engram.uncredited_feedback_revision);
    let receipt = fixture.receive(true);
    assert_eq!(fixture.turn.record(|record| record.engram.uncredited_feedback_revision), feedback_revision,
        "receipt presentation must not invalidate an in-flight other-command notice");
    fixture.turn.state.acknowledge_engram_source_root_line_delivery(&fixture.turn.session_id, generation);
    let (line, closed) = fixture.line();
    assert!(!closed && line.contains(&receipt.verifications[0].record_id)
        && line.contains("unrelated check remains Unknown"), "{line}");
}

#[test]
fn a_received_notice_waits_for_durability_and_can_upgrade_on_the_next_settlement() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::committed();
    fixture.background_reply();
    fixture.finish();
    let before = fixture.line().0;
    let (tx, held_writer) = mpsc::channel();
    fixture.turn.state.persist_tx = tx;
    // This fence uses the fixture's frozen Engram clock, not the physical
    // clock advanced by the preceding Git work. Expire it in its own domain.
    let expired = fixture.turn.state.engram_budget_clock().now() - Duration::from_secs(1);
    let (receipt, settlement) = fixture.receive_until(true, Some(expired));
    assert!(matches!(settlement, EngramVerificationRecordSettlement::NotDurable { .. }));
    assert_eq!(fixture.line().0, before, "a failed durability wait cannot consume the one-shot notice upgrade");
    assert!(!before.contains(&receipt.verifications[0].record_id));
    drop(held_writer);
    let registration = fixture.turn.state.inner.lock().unwrap().test_run_registrations[&fixture.run_id].clone();
    let frozen = engram_verification_record_frozen(&fixture.directory, &fixture.run_id).unwrap();
    assert_eq!(fixture.turn.state.complete_engram_verification_record_until(&registration,
        &frozen.body, &receipt, std::time::Instant::now() + DEADLOCK_GUARD),
        EngramVerificationRecordSettlement::Settled);
    fixture.wait_for_received_notices();
    assert!(fixture.line().0.contains(&receipt.verifications[0].record_id));
    let prompt = fixture.turn.begin_prompt("Report the settled registered outcome.").prompt;
    assert!(prompt.contains(&receipt.verifications[0].record_id), "{prompt}");
}

#[test]
fn a_received_but_unsettled_record_cannot_upgrade_at_turn_close() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::committed();
    fixture.background_reply();
    let receipt = fixture.receive_not_durable();
    let (line, closed) = fixture.finish();
    assert!(!closed && line.contains("correlated with")
        && !line.contains(&receipt.verifications[0].record_id), "{line}");
    let registration = fixture.turn.state.inner.lock().unwrap().test_run_registrations[&fixture.run_id].clone();
    assert!(matches!(registration.verification_record, Some(TestRunVerificationAck::Received { .. })),
        "turn close must not perform or renew receipt settlement");
    let frozen = engram_verification_record_frozen(&fixture.directory, &fixture.run_id).unwrap();
    assert_eq!(fixture.turn.state.complete_engram_verification_record_until(&registration,
        &frozen.body, &receipt, std::time::Instant::now() + DEADLOCK_GUARD),
        EngramVerificationRecordSettlement::Settled);
    fixture.wait_for_received_notices();
    let prompt = fixture.turn.begin_prompt("Report the now settled outcome.").prompt;
    assert!(prompt.contains(&receipt.verifications[0].record_id), "{prompt}");
}

#[test]
fn failed_optional_claim_reads_explain_only_existing_receipt_candidates() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for received in [false, true] {
        let mut fixture = RegisteredLauncherTurn::committed();
        fixture.background_reply();
        fixture.receive(true);
        fixture.finish();
        let (primary, binding) = {
            let inner = fixture.turn.state.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.turn.session_id).unwrap();
            (inner.engram_work_source_roots[0].clone(), inner.sessions[index].engram.work_binding.clone().unwrap())
        };
        let other = test_control_work_binding("failed-optional-receipt", 1);
        let held = EngramHeldClaims { items: [&binding, &other].into_iter().map(|binding| EngramHeldClaim {
            work_id: binding.work_id.clone(), short_ref: binding.work_id.clone(), claim_id: binding.claim_id.clone(),
            claim_fence: binding.claim_fence, focused: false, control_binding: Some(binding.clone()),
        }).collect(), omitted: 0 };
        fixture.turn.transport.register_named_root_run(&other);
        fixture.turn.transport.replace_held_claims([Ok(held)]);
        fixture.turn.state.name_engram_source_root(&fixture.turn.session_id, EngramSourceRootRequest {
            work: other.work_id.clone(), path: Some(Some(primary.root.clone())),
        }).unwrap();
        fixture.turn.transport.enable_named_roots(&fixture.turn.session_id, &other);
        fixture.turn.transport.set_current_work_binding(Some(other.clone()));
        fixture.turn.begin("Observe the independent current claim.");
        fixture.turn.record_mut(|record| record.engram.pending_source_root_line = None);
        if !received {
            fixture.turn.state.inner.lock().unwrap().test_run_registrations.update_unindexed_fields(
                &fixture.run_id, |run| run.verification_record = None);
        }
        fixture.turn.transport.replace_held_claims([Err(EngramTransportError::transport("optional read unavailable"))]);
        let mut recorder = fixture.turn.recorder();
        recorder.command_started_in("optional-receipt-check", SIZE_TEST, Some(SIZE_TEST), Some(&primary.root)).unwrap();
        fixture.turn.wait_for_other_root_notices();
        let line = fixture.turn.record(|record| record.engram.pending_source_root_line.clone());
        if received {
            let line = line.as_deref().expect("an existing receipt gets a truthful unavailable explanation");
            assert!(line.contains("Named-root claim confirmation is unavailable")
                && line.contains("held-claim read unavailable")
                && line.contains("confirms no current-input or ordinary-check credit")
                && !line.contains("Make that item") && !line.contains("run it again"), "{line}");
        } else {
            assert_eq!(line, None, "the original failed optional-read silence contract is preserved");
        }
        assert_eq!(fixture.turn.record(|record| record.engram.active_turn_checks.len()), 1);
        recorder.command_completed_with_exit("optional-receipt-check", SIZE_TEST,
            "running 1 test\ntest result: ok. 1 passed", CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
        fixture.turn.wait_for_snapshots();
        assert_ordinary_observation_has_no_native_credit(&fixture.turn, &fixture.turn.finish());
        let prompt = fixture.turn.begin_prompt("Report the current check.").prompt;
        assert_eq!(prompt.contains("Named-root claim confirmation is unavailable"), received, "{prompt}");
        assert!(!prompt.contains("Make that item"), "{prompt}");
    }
}

#[test]
fn received_notice_batch_expiry_is_typed_and_visible_without_rerun() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::committed();
    fixture.background_reply();
    let receipt = fixture.receive(true);
    let expired = std::time::Instant::now() - Duration::from_secs(1);
    let state = fixture.turn.state.clone();
    TEST_RECEIVED_NOTICE_SOURCE_READ.lock().unwrap().insert(fixture.run_id.clone(), Box::new(move |deadline| {
        assert_eq!(deadline, expired, "the batch deadline cannot be renewed per candidate");
        assert!(state.inner.is_not_held_by_current_thread_for_test(), "source reads cannot hold the shared state lock");
    }));
    let prepared = fixture.turn.state.prepare_registered_check_notices_until(&fixture.turn.session_id, expired);
    assert_eq!(prepared.len(), 1);
    assert_eq!(prepared[0].received.as_ref().unwrap().source(),
        ReceivedNoticeSource::SourceUnavailable(ReceivedNoticeSourceUnavailable::Deadline));
    let (lines, correlated) = {
        let mut inner = fixture.turn.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.turn.session_id).unwrap();
        registered_check_notices_locked(&mut inner, index, &prepared)
    };
    assert_eq!(correlated.len(), 1, "expiry cannot erase native correlation");
    assert_eq!(lines.len(), 1);
    let line = &lines[0];
    assert!(line.contains(&receipt.verifications[0].record_id)
        && line.contains("confirmation is unavailable") && line.contains("deadline elapsed")
        && line.contains("confirms no current-input or ordinary-check credit")
        && !line.contains("unchanged input") && !line.contains("run it again")
        && !line.contains("Make that item"), "{line}");
    assert_ordinary_observation_has_no_native_credit(&fixture.turn, &fixture.turn.finish());
}

#[test]
fn received_source_confirmation_is_owned_and_never_blocks_the_terminal_path() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for cancelled in [false, true] {
        let mut fixture = RegisteredLauncherTurn::committed();
        fixture.background_reply();
        let receipt = fixture.receive(true);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        *fixture.turn.state.capture_lifetime.before_capture.lock().unwrap() = Some(Arc::new(move || {
            if std::thread::current().name() != Some("termal-received-launcher-notice") { return; }
            assert!(current_capture_cancellation().is_some(), "the proof inherits host cancellation");
            entered_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv_timeout(DEADLOCK_GUARD).unwrap();
        }));
        // This is the real terminal close, intentionally without the fixture's
        // presentation wait. It must return while its optional worker is parked.
        let observation = fixture.turn.finish();
        assert_ordinary_observation_has_no_native_credit(&fixture.turn, &observation);
        entered_rx.recv_timeout(DEADLOCK_GUARD).unwrap();
        let before = fixture.line().0;
        assert!(before.contains("correlated with")
            && !before.contains(&receipt.verifications[0].record_id), "{before}");
        assert!(fixture.turn.state.inner.is_not_held_by_current_thread_for_test());
        assert!(fixture.turn.state.inner.lock().unwrap().find_session_index(&fixture.turn.session_id).is_some(),
            "other runtime frames retain state access while source proof is parked");
        if cancelled { fixture.turn.state.capture_lifetime.stop(); }
        release_tx.send(()).unwrap();
        fixture.wait_for_received_notices();
        *fixture.turn.state.capture_lifetime.before_capture.lock().unwrap() = None;
        if cancelled {
            assert_eq!(fixture.line().0, before, "shutdown cannot publish a queued proof");
        } else {
            assert!(fixture.line().0.contains(&receipt.verifications[0].record_id));
            let prompt = fixture.turn.begin_prompt("Report the received outcome.").prompt;
            assert!(prompt.contains(&receipt.verifications[0].record_id), "{prompt}");
        }
    }
}

#[test]
fn an_unavailable_notice_source_confirms_only_the_historical_receipt() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::committed();
    fixture.background_reply();
    let receipt = fixture.receive(true);
    fixture.set_notice_source(TestReceivedNoticeSourceResult::Unavailable);
    let (line, closed) = fixture.finish();
    assert!(!closed && line.contains(&fixture.run_id)
        && line.contains(&receipt.verifications[0].record_id)
        && line.contains("source read unavailable")
        && line.contains("confirms no current-input or ordinary-check credit")
        && line.contains("No rerun is requested")
        && !line.contains("unchanged input") && !line.contains("outcome Unknown"), "{line}");
    let prompt = fixture.turn.begin_prompt("Report only the historical registered outcome.").prompt;
    assert!(prompt.contains(&receipt.verifications[0].record_id)
        && prompt.contains("source read unavailable")
        && prompt.contains("confirms no current-input or ordinary-check credit")
        && !prompt.contains("unchanged input") && !prompt.contains("Make that item"), "{prompt}");
}

#[test]
fn received_notice_real_clean_reader_confirms_current_without_the_protocol_seam() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::committed();
    fixture.background_reply();
    let receipt = fixture.receive(true);
    fixture.notice_source.borrow_mut().take();
    let registration = fixture.turn.state.inner.lock().unwrap().test_run_registrations[&fixture.run_id].clone();
    let notice = ReceivedCheckNotice::read(&registration,
        std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT).expect("real pinned Git clean proof");
    assert_eq!(notice.source(), ReceivedNoticeSource::Current);
    assert!(notice.line().contains(&receipt.verifications[0].record_id)
        && notice.line().contains("unchanged input"), "{}", notice.line());
    assert_ordinary_observation_has_no_native_credit(&fixture.turn, &fixture.turn.finish());
    fixture.wait_for_received_notices();
}

#[test]
fn received_notice_source_change_and_read_unavailability_stay_distinct() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for changed in [false, true] {
        let mut fixture = RegisteredLauncherTurn::committed();
        fixture.background_reply();
        fixture.receive(true);
        let registration = fixture.turn.state.inner.lock().unwrap().test_run_registrations[&fixture.run_id].clone();
        // Independent real-reader control: no scripted protocol result.
        fixture.notice_source.borrow_mut().take();
        let root = FsPath::new(&registration.worktree);
        if changed {
            fs::write(root.join("README.md"), "changed after receipt\n").unwrap();
        } else {
            // This reader refuses configured filters before any source read.
            // It cannot establish clean input or silently call that dirt.
            run_git_test_command(root, &["config", "filter.notice.clean", "unreachable-filter"]);
        }
        // This is a predicate integration control, not the protocol's timed
        // availability test: use the pinned reader's existing default bound.
        let notice = ReceivedCheckNotice::read(&registration,
            std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT);
        if changed {
            assert!(notice.is_none(), "changed source must not produce a received-current notice");
        } else {
            let notice = notice.expect("a valid historical receipt survives an unavailable source read");
            assert_eq!(notice.source(), ReceivedNoticeSource::SourceUnavailable(ReceivedNoticeSourceUnavailable::Read));
            assert!(notice.line().contains("source read unavailable") && !notice.line().contains("unchanged input"));
            run_git_test_command(root, &["config", "--unset", "filter.notice.clean"]);
        }
        assert!(ReviewFreezeGit::new_with_deadline(root,
            Some(std::time::Instant::now() - Duration::from_secs(1))).is_err(),
            "the real pinned reader must refuse an expired deadline");
        assert_ordinary_observation_has_no_native_credit(&fixture.turn, &fixture.turn.finish());
    }
}

#[test]
fn received_notice_publication_never_renews_an_expired_source_proof() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::committed();
    fixture.background_reply();
    fixture.receive(true);
    let mut prepared = fixture.turn.state.prepare_registered_check_notices_until(&fixture.turn.session_id,
        std::time::Instant::now() + RECEIVED_NOTICE_CAPTURE_BUDGET);
    let notice = prepared[0].received.as_mut().unwrap();
    assert_eq!(notice.source(), ReceivedNoticeSource::Current);
    // Deterministically model the interval between off-lock read and locked
    // publication, without a wall-clock sleep or a larger production bound.
    notice.deadline = std::time::Instant::now() - Duration::from_secs(1);
    let (lines, _) = {
        let mut inner = fixture.turn.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.turn.session_id).unwrap();
        registered_check_notices_locked(&mut inner, index, &prepared)
    };
    assert!(lines[0].contains("confirmation is unavailable") && !lines[0].contains("unchanged input"));
    assert_ordinary_observation_has_no_native_credit(&fixture.turn, &fixture.turn.finish());
}

#[test]
fn received_notice_authority_steps_share_the_remaining_batch_time() {
    let maximum = ENGRAM_WORK_BINDING_COMMAND_TIMEOUT;
    let short = std::time::Instant::now() + Duration::from_secs(1);
    let remaining = received_notice_step_timeout(short, maximum);
    assert!(remaining > Duration::ZERO && remaining <= Duration::from_secs(1));
    assert_eq!(received_notice_step_timeout(std::time::Instant::now() - Duration::from_secs(1), maximum), Duration::ZERO);
    assert_eq!(received_notice_step_timeout(std::time::Instant::now() + maximum + Duration::from_secs(20), maximum), maximum);
}

#[test]
fn received_notice_other_root_read_cannot_restart_its_authority_deadline() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = RegisteredLauncherTurn::committed();
    let read = {
        let mut inner = fixture.turn.state.inner.lock().unwrap();
        let mut other = inner.engram_work_source_roots[0].clone();
        other.work_id.push_str("-other");
        other.claim_id.push_str("-other");
        inner.engram_work_source_roots.push(other);
        let index = inner.find_session_index(&fixture.turn.session_id).unwrap();
        EngramOtherRootNoticeRead::capture(&inner, index).unwrap()
    };
    let before = fixture.turn.transport.held_claim_reads();
    assert!(matches!(read.read(std::time::Instant::now() - Duration::from_secs(1), false),
        Err(ReceivedNoticeSourceUnavailable::Deadline)));
    assert_eq!(fixture.turn.transport.held_claim_reads(), before, "no authority process starts after expiry");
    fixture.turn.transport.replace_held_claims([Ok(EngramHeldClaims { items: Vec::new(), omitted: 0 })]);
    let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = observed.clone();
    let state = fixture.turn.state.clone();
    fixture.turn.transport.on_next_held_claim_read(move |timeout| {
        assert!(timeout > Duration::ZERO && timeout <= Duration::from_secs(1), "{timeout:?}");
        assert!(state.inner.is_not_held_by_current_thread_for_test(), "authority reads cannot hold the shared state lock");
        seen.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    assert!(read.read(std::time::Instant::now() + Duration::from_secs(1), false).is_ok());
    assert!(observed.load(std::sync::atomic::Ordering::SeqCst));
    fixture.turn.state.inner.lock().unwrap().engram_work_source_roots.pop();
    assert_ordinary_observation_has_no_native_credit(&fixture.turn, &fixture.turn.finish());
}

#[test]
fn a_late_received_notice_reads_off_lock_after_durable_settlement() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::committed();
    fixture.background_reply();
    fixture.finish();
    let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = observed.clone();
    let state = fixture.turn.state.clone();
    let run_id = fixture.run_id.clone();
    let directory = fixture.directory.clone();
    TEST_RECEIVED_NOTICE_SOURCE_READ.lock().unwrap().insert(fixture.run_id.clone(), Box::new(move |deadline| {
        assert!(deadline > std::time::Instant::now());
        assert!(state.inner.is_not_held_by_current_thread_for_test(), "other sessions must retain access during a notice capture");
        let inner = state.inner.lock().unwrap();
        assert!(matches!(inner.test_run_registrations[&run_id].verification_record,
            Some(TestRunVerificationAck::Settled { .. })), "presentation must not precede durable settlement");
        drop(inner);
        assert!(matches!(engram_verification_record_progress(&directory),
            Some(EngramVerificationRecordProgress::Settled { .. })));
        seen.store(true, std::sync::atomic::Ordering::SeqCst);
    }));
    let receipt = fixture.receive(true);
    assert!(observed.load(std::sync::atomic::Ordering::SeqCst));
    assert!(fixture.line().0.contains(&receipt.verifications[0].record_id));
}

#[test]
fn a_companion_root_reminder_uses_only_that_items_own_current_received_record() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for case in ["received", "not-durable", "primary-only", "dirty", "mirror-only", "generation", "binding"] {
        let mut fixture = RegisteredLauncherTurn::committed();
        fixture.background_reply();
        let receipt = if case == "not-durable" { fixture.receive_not_durable() } else { fixture.receive(true) };
        fixture.finish();
        let (primary, binding) = {
            let inner = fixture.turn.state.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.turn.session_id).unwrap();
            (inner.engram_work_source_roots[0].clone(), inner.sessions[index].engram.work_binding.clone().unwrap())
        };
        let other = test_control_work_binding("companion-notice", 1);
        let other_short_ref = "companion-item".to_owned();
        let held = EngramHeldClaims { items: [(&binding, &primary.short_ref), (&other, &other_short_ref)]
            .into_iter().map(|(binding, short)| EngramHeldClaim {
                work_id:binding.work_id.clone(), short_ref:short.clone(), claim_id:binding.claim_id.clone(),
                claim_fence:binding.claim_fence, focused:false, control_binding:Some(binding.clone()),
            }).collect(), omitted:0 };
        fixture.turn.transport.register_named_root_run(&other);
        fixture.turn.transport.replace_held_claims([Ok(held.clone())]);
        fixture.turn.state.name_engram_source_root(&fixture.turn.session_id, EngramSourceRootRequest {
            work:other.work_id.clone(), path:Some(Some(primary.root.clone())),
        }).unwrap();
        if case == "primary-only" {
            fixture.turn.state.name_engram_source_root(&fixture.turn.session_id, EngramSourceRootRequest {
                work:binding.work_id.clone(), path:Some(Some(primary.root.clone())),
            }).unwrap();
        }
        if case == "dirty" {
            fs::write(FsPath::new(&primary.root).join("README.md"), "changed\n").unwrap();
            fixture.capture_dirty_notice_source();
        }
        if case == "mirror-only" || case == "generation" {
            fixture.turn.state.inner.lock().unwrap().test_run_registrations.update_unindexed_fields(&fixture.run_id, |run| {
                if case == "mirror-only" { run.verification_record = None; }
                else { run.named_root.as_mut().unwrap().generation += 1; }
            });
        }
        let mut held = held;
        if case == "binding" { held.items[0].control_binding.as_mut().unwrap().work_revision += 1; }
        fixture.turn.transport.replace_held_claims([Ok(held)]);
        let selected = if case == "primary-only" { &binding } else { &other };
        // Naming changes the host association; this scripted ordinary service
        // also needs its canonical session selector moved to that association.
        // Otherwise it correctly refuses the closing observation before the
        // fixture's notice channel is sent.
        fixture.turn.transport.enable_named_roots(&fixture.turn.session_id, selected);
        fixture.turn.transport.set_current_work_binding(Some(selected.clone()));
        fixture.turn.begin("Check another item independently.");
        assert_eq!(fixture.turn.record(|record| record.engram.work_binding.clone()), Some(selected.clone()));
        assert_eq!(fixture.turn.transport.named_roots.lock().unwrap().as_ref().unwrap()
            .bindings.get(&fixture.turn.session_id), Some(&selected.claim_id));
        fixture.turn.record_mut(|record| record.engram.pending_source_root_line = None);
        let mut recorder = fixture.turn.recorder();
        recorder.command_started_in("companion-check", SIZE_TEST, Some(SIZE_TEST), Some(&primary.root)).unwrap();
        fixture.turn.wait_for_other_root_notices();
        let line = fixture.turn.record(|record| record.engram.pending_source_root_line.clone().unwrap_or_default());
        assert_eq!(line.contains("Make that item"), case != "received", "{case}: {line}");
        if case == "not-durable" {
            assert!(!line.contains(&receipt.verifications[0].record_id), "{line}");
        }
        assert!(fixture.turn.record(|record| record.engram.active_turn_checks.len() == 1), "primary admission is independent");
        recorder.command_completed_with_exit("companion-check", SIZE_TEST,
            "running 1 test\ntest result: ok. 1 passed", CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
        fixture.turn.wait_for_snapshots();
        assert_ordinary_observation_has_no_native_credit(&fixture.turn, &fixture.turn.finish());
        let prompt = fixture.turn.begin_prompt("Report the current check.").prompt;
        assert_eq!(prompt.contains("Make that item"), case != "received", "{case}: {prompt}");
        if case == "not-durable" {
            assert!(!prompt.contains(&receipt.verifications[0].record_id), "{prompt}");
        }
    }
}

#[test]
fn a_test_in_another_root_does_not_request_a_duplicate_of_the_bound_items_received_run() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for received in [false, true] {
        let mut fixture = RegisteredLauncherTurn::committed();
        fixture.background_reply();
        fixture.receive(true);
        fixture.finish();
        let (primary, binding) = {
            let inner = fixture.turn.state.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.turn.session_id).unwrap();
            (inner.engram_work_source_roots[0].clone(), inner.sessions[index].engram.work_binding.clone().unwrap())
        };
        if !received {
            fixture.turn.state.inner.lock().unwrap().test_run_registrations.update_unindexed_fields(
                &fixture.run_id, |run| run.verification_record = None);
        }
        let other = test_control_work_binding("separate-companion-notice", 1);
        let other_root = fixture.turn.root.join(".worktrees").join("companion");
        run_git_test_command(&fixture.turn.root, &["worktree", "add", "--quiet", "-b", "companion", other_root.to_str().unwrap()]);
        let held = EngramHeldClaims { items: [&binding, &other].into_iter().map(|binding| EngramHeldClaim {
            work_id:binding.work_id.clone(), short_ref:binding.work_id.clone(), claim_id:binding.claim_id.clone(),
            claim_fence:binding.claim_fence, focused:false, control_binding:Some(binding.clone()),
        }).collect(), omitted:0 };
        fixture.turn.transport.register_named_root_run(&other);
        fixture.turn.transport.replace_held_claims([Ok(held.clone())]);
        fixture.turn.state.name_engram_source_root(&fixture.turn.session_id, EngramSourceRootRequest {
            work:other.work_id.clone(), path:Some(Some(other_root.to_string_lossy().into_owned())),
        }).unwrap();
        fixture.turn.state.name_engram_source_root(&fixture.turn.session_id, EngramSourceRootRequest {
            work:binding.work_id.clone(), path:Some(Some(primary.root.clone())),
        }).unwrap();
        fixture.turn.transport.replace_held_claims([Ok(held)]);
        fixture.turn.begin("Observe another root without moving evidence.");
        fixture.turn.record_mut(|record| record.engram.pending_source_root_line = None);
        let mut recorder = fixture.turn.recorder();
        recorder.command_started_in("separate-companion", SIZE_TEST, Some(SIZE_TEST), Some(other_root.to_str().unwrap())).unwrap();
        fixture.turn.wait_for_other_root_notices();
        let line = fixture.turn.record(|record| record.engram.pending_source_root_line.clone().unwrap_or_default());
        assert_eq!(line.contains("Make that item"), !received, "{line}");
        assert!(fixture.turn.record(|record| record.engram.active_turn_checks.is_empty()),
            "a receipt cannot admit an ordinary check from another root");
        recorder.command_completed_with_exit("separate-companion", SIZE_TEST,
            "running 1 test\ntest result: ok. 1 passed", CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
        assert_ordinary_observation_has_no_native_credit(&fixture.turn, &fixture.turn.finish());
        let prompt = fixture.turn.begin_prompt("Report the unrelated check.").prompt;
        assert_eq!(prompt.contains("Make that item"), !received, "{prompt}");
    }
}

fn registered_launcher_before_index(passed: bool, correlated: bool) {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::start();
    if correlated { fixture.background_reply(); }
    if passed { fixture.write_results("passed"); }
    fixture.assert_unindexed();
    let (line, closed) = fixture.finish();
    fixture.assert_advice(&line);
    if correlated {
        assert!(!closed && !line.contains("closed with outcome Unknown"), "{line}");
        assert!(line.contains(if passed { "passed" } else { "still running" }), "{line}");
    } else {
        assert!(closed && line.contains("root") && line.contains("input"), "{line}");
    }
    let results: Value = serde_json::from_slice(&fs::read(fixture.directory.join("results.json")).unwrap()).unwrap();
    assert_eq!(results["state"], if passed { "passed" } else { "running" });
    assert_eq!(results.get("ended").is_some(), passed);
}

#[test]
fn a_registered_live_launcher_is_pending_at_turn_end_before_the_index_catches_up() {
    registered_launcher_before_index(false, true);
}

#[test]
fn a_registered_passed_launcher_names_its_terminal_result_at_turn_end_before_the_index_catches_up() {
    registered_launcher_before_index(true, true);
}

#[test]
fn a_registered_live_launcher_without_a_locator_is_disclosed_without_rerun_advice() {
    registered_launcher_before_index(false, false);
}

#[test]
fn a_registered_passed_launcher_without_a_locator_is_disclosed_without_rerun_advice() {
    registered_launcher_before_index(true, false);
}

#[test]
fn a_registered_failed_launcher_names_its_terminal_result_without_duplicating_it() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::start();
    fixture.background_reply();
    fixture.write_results("failed");
    let (line, closed) = fixture.finish();
    fixture.assert_advice(&line);
    assert!(line.contains("failed") && !closed, "{line}");
}

#[test]
fn a_registered_terminal_launcher_keeps_its_matching_retained_digest() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for terminal in ["passed", "failed"] {
        let mut fixture = RegisteredLauncherTurn::start();
        fixture.background_reply();
        fixture.write_results(terminal);
        fixture.turn.state.refresh_test_runs();
        let retained = fixture.turn.state.inner.lock().unwrap().test_run_observations.values().any(|row|
            row.artifact.as_ref().is_some_and(|artifact| artifact.run_id == fixture.run_id
                && artifact.terminal_digest.is_some()));
        assert!(retained);
        let (line, closed) = fixture.finish();
        fixture.assert_advice(&line);
        assert!(line.contains(terminal) && !closed, "{line}");
    }
}

#[test]
fn a_registered_launcher_running_card_without_a_nonterminal_exit_cannot_associate_output() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::start();
    fixture.recorder.command_completed_with_exit(KEY, COMMAND,
        &format!("RUN {}", test_run_display_path(&fixture.directory)),
        CommandStatus::Running, EngramCommandExit::Code(0)).unwrap();
    let associated = fixture.turn.state.inner.lock().unwrap().test_run_observations.values()
        .any(|row| row.artifact.is_some());
    assert!(!associated, "a running transcript card alone cannot supply an artifact or end");
    let (line, closed) = fixture.finish();
    fixture.assert_advice(&line);
    assert!(closed && !line.contains("correlated with"), "{line}");
}

#[test]
fn a_non_launcher_running_reply_keeps_its_original_unfinished_observation_unchanged() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let turn = EOCheckedTurn::start("non-launcher-running-reply", true);
    let mut recorder = turn.recorder();
    recorder.command_started(KEY, SIZE_TEST).unwrap();
    turn.wait_for_snapshots();
    let before = turn.state.inner.lock().unwrap().test_run_observations.values().cloned().collect::<Vec<_>>();
    assert_eq!(before.len(), 1);
    recorder.command_completed_with_exit(KEY, SIZE_TEST,
        &format!("RUN {}", test_run_display_path(&turn.root.join("not-a-run"))),
        CommandStatus::Running, EngramCommandExit::NotFinished).unwrap();
    let after = turn.state.inner.lock().unwrap().test_run_observations.values().cloned().collect::<Vec<_>>();
    assert_eq!(after, before, "a non-launcher card still enters no artifact or end owner");
    let open = turn.record(|record| record.engram.active_turn_checks.iter()
        .any(|check| check.key == KEY && check.end.is_none()));
    assert!(open);
    assert!(!EngramHost::is_nonterminal_launcher_reply(SIZE_TEST, Some(EngramCommandExit::NotFinished)));
}

#[test]
fn a_full_launcher_running_reply_preserves_the_prior_native_carry_path() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    const FULL: &str = "node scripts/test-launcher.mjs full";
    for named in [false, true] {
        let turn = if named { EOCheckedTurn::start_named("full-running-control") }
            else { EOCheckedTurn::start("full-running-control", true) };
        let mut recorder = turn.recorder();
        let cwd = turn.record(|record| record.engram.observed_source_root()
            .map(|root| root.root.clone()));
        recorder.command_started_in(KEY, FULL, Some(FULL), cwd.as_deref()).unwrap();
        turn.wait_for_snapshots();
        let before = turn.state.inner.lock().unwrap().test_run_observations.values().cloned().collect::<Vec<_>>();
        assert_eq!(before.len(), 1);
        recorder.command_completed_with_exit(KEY, FULL,
            &format!("RUN {}", test_run_display_path(&turn.root.join("not-a-run"))),
            CommandStatus::Running, EngramCommandExit::NotFinished).unwrap();
        let after = turn.state.inner.lock().unwrap().test_run_observations.values().cloned().collect::<Vec<_>>();
        assert_eq!(after, before, "full Running replies stay outside the launch-result path");
        let open = turn.record(|record| record.engram.active_turn_checks.iter()
            .any(|check| check.key == KEY && check.end.is_none()));
        assert!(open);
        let observation = turn.finish();
        assert_ordinary_observation_has_no_native_credit(&turn, &observation);
        let (running, carried) = turn.record(|record| (
            record.engram.running_codex_tests.iter().any(|test| test.key == KEY),
            record.engram.carried_checks.iter().any(|carry| carry.check.key == KEY && carry.ends_by_report),
        ));
        assert!(running, "the original process remains a possible writer");
        assert_eq!(carried, named, "only the prior named-root native carry is eligible");
        assert!(!EngramHost::is_nonterminal_launcher_reply(FULL, Some(EngramCommandExit::NotFinished)));
    }
}

#[test]
fn a_non_launcher_closure_keeps_its_original_advice_at_each_boundary() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let command = EngramHost::recognized_check(SIZE_TEST).unwrap();
    for boundary in ["turn-end", "next-start"] {
        let turn = EOCheckedTurn::start("non-launcher-closure", true);
        let mut recorder = turn.recorder();
        recorder.command_started(KEY, SIZE_TEST).unwrap();
        turn.wait_for_snapshots();
        if boundary == "turn-end" { let _ = turn.finish(); }
        else { turn.record_mut(EngramHost::turn_prompt_composing); }
        let line = turn.record(|record| record.engram.pending_source_root_line.clone().unwrap_or_default());
        assert!(line.contains("run it again to record it"), "{boundary}: {line}");
        assert!(!line.contains("registered launcher runs"), "{boundary}: {line}");
    }
    for line in [engram_closed_unended_carry_line(&command, "runtime cleared"),
        engram_reported_end_lost_to_restart_line(&command, false),
        engram_reported_end_lost_to_restart_line(&command, true),
        engram_late_end_line(&command, SIZE_TEST, Some(EngramCommandExit::Code(0)))]
    {
        assert!(line.contains("again to record it"), "{line}");
        assert!(!line.contains("registered launcher runs"), "{line}");
    }
}

#[test]
fn a_nonterminal_launcher_reply_preserves_the_closure_for_the_real_end_once() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::start();
    let (_, closed) = fixture.finish();
    assert!(closed);
    fixture.turn.record_mut(|record| record.engram.pending_source_root_line = None);
    let output = format!("RUN {}", test_run_display_path(&fixture.directory));
    fixture.recorder.command_completed_with_exit(KEY, COMMAND, &output,
        CommandStatus::Running, EngramCommandExit::NotFinished).unwrap();
    let (line, closed) = fixture.line();
    assert!(closed && !line.contains("an end ("), "{line}");
    fixture.recorder.command_completed_with_exit(KEY, COMMAND, &output,
        CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
    let (line, closed) = fixture.line();
    assert!(!closed && line.contains("an end (exit code 0)"), "{line}");
    assert!(!line.contains("run the check again"), "{line}");
    fixture.turn.record_mut(|record| record.engram.pending_source_root_line = None);
    fixture.recorder.command_completed_with_exit(KEY, COMMAND, &output,
        CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
    let (line, closed) = fixture.line();
    assert!(!closed && !line.contains("an end ("), "the real end is told exactly once: {line}");
}

#[test]
fn a_carried_launchers_eviction_and_settings_advice_cannot_invite_a_duplicate() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::with_turn(EOCheckedTurn::start_named("carried-launcher-advice"));
    fixture.background_reply();
    let (line, _) = fixture.finish();
    fixture.assert_advice(&line);
    let lines = fixture.turn.record(|record| {
        let carried = record.engram.carried_checks.iter().find(|carry| carry.check.key == KEY).unwrap();
        [carried.refusal_line("carried capacity exceeded"), carried.association_reset_line("settings changed")]
    });
    for line in lines {
        assert!(line.contains("registered launcher runs") && line.contains("rerun only if none"), "{line}");
        assert!(!line.contains("Run the test again") && !line.contains("run it again"), "{line}");
    }
    let turn = EOCheckedTurn::start_named("carried-non-launcher-advice");
    let mut recorder = turn.recorder();
    let cwd = turn.record(|record| record.engram.observed_source_root()
        .map(|root| root.root.clone()));
    recorder.command_started_in(KEY, SIZE_TEST, Some(SIZE_TEST), cwd.as_deref()).unwrap();
    turn.wait_for_snapshots();
    let _ = turn.finish();
    let lines = turn.record(|record| {
        let carried = record.engram.carried_checks.iter().find(|carry| carry.check.key == KEY).unwrap();
        [carried.refusal_line("carried capacity exceeded"), carried.association_reset_line("settings changed")]
    });
    assert!(lines[0].contains("Run the test again once it has ended"));
    assert!(lines[1].contains("run it again once the new settings apply"));
    assert!(lines.iter().all(|line| !line.contains("registered launcher runs")));
}

#[test]
fn a_registered_live_launcher_keeps_codex_writer_bookkeeping_and_successor_overlap() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::start();
    fixture.background_reply();
    let (line, closed) = fixture.finish();
    fixture.assert_advice(&line);
    assert!(!closed);
    let running = fixture.turn.record(|record| record.engram.running_codex_tests.iter()
        .any(|test| test.key == KEY));
    assert!(running, "refusing carry evidence cannot end the process");
    fixture.turn.begin("Run a successor check.");
    let mut recorder = fixture.turn.recorder();
    recorder.command_started("successor", SIZE_TEST).unwrap();
    let overlap = fixture.turn.record(|record| record.engram.active_turn_checks.iter()
        .find(|check| check.key == "successor").map(|check| check.overlap_cause.clone()));
    assert!(overlap.is_some_and(|cause| cause.is_some()), "the original process can still write");
}

#[test]
fn a_registered_live_launcher_in_a_named_root_keeps_native_reported_end_custody() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::with_turn(EOCheckedTurn::start_named("registered-named-carry"));
    fixture.background_reply();
    let (line, closed) = fixture.finish();
    fixture.assert_advice(&line);
    assert!(!closed, "{line}");
    let (running, carried) = fixture.turn.record(|record| (
        record.engram.running_codex_tests.iter().any(|test| test.key == KEY),
        record.engram.carried_checks.iter().any(|carry|
            carry.check.key == KEY && carry.ends_by_report && carry.check.end.is_none()),
    ));
    assert!(running && carried, "registered advice cannot replace native late-end custody");
}

#[test]
fn a_registered_launcher_on_each_terminal_path_keeps_its_exact_run_notice() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for terminal in ["failed", "cancelled", "exit", "marked", "atomic"] {
        let mut fixture = RegisteredLauncherTurn::start();
        fixture.background_reply();
        let (token, generation) = fixture.turn.record(|record|
            (record.runtime.runtime_token().unwrap(), record.active_turn_generation));
        let state = &fixture.turn.state;
        let session = &fixture.turn.session_id;
        match terminal {
            "failed" => state.fail_turn_if_runtime_matches(session, &token, "provider failed").unwrap(),
            "cancelled" => { state.stop_session(session).unwrap(); }
            "exit" => state.handle_runtime_exit_if_matches(session, &token, None).unwrap(),
            "marked" => state.mark_turn_error_if_runtime_matches(session, &token, "provider failed").unwrap(),
            _ => { assert!(state.fail_turn_if_runtime_matches_or_missing(session, &token, generation, "provider failed").unwrap()); }
        }
        let (line, closed) = fixture.line();
        fixture.assert_advice(&line);
        assert!(line.contains("still running") && !closed, "{terminal}: {line}");
    }
}

#[test]
fn a_registered_launcher_in_a_named_root_never_gets_unconditional_rerun_advice_on_terminal_close() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for correlated in [true, false] {
        for terminal in ["failed", "cancelled", "exit", "marked", "atomic"] {
            let mut fixture = RegisteredLauncherTurn::with_turn(EOCheckedTurn::start_named("registered-named-close"));
            if correlated { fixture.background_reply(); }
            let (token, generation) = fixture.turn.record(|record|
                (record.runtime.runtime_token().unwrap(), record.active_turn_generation));
            let state = &fixture.turn.state;
            let session = &fixture.turn.session_id;
            match terminal {
                "failed" => state.fail_turn_if_runtime_matches(session, &token, "provider failed").unwrap(),
                "cancelled" => { state.stop_session(session).unwrap(); }
                "exit" => state.handle_runtime_exit_if_matches(session, &token, None).unwrap(),
                "marked" => state.mark_turn_error_if_runtime_matches(session, &token, "provider failed").unwrap(),
                _ => { assert!(state.fail_turn_if_runtime_matches_or_missing(session, &token, generation, "provider failed").unwrap()); }
            }
            let (line, _) = fixture.line();
            fixture.assert_advice(&line);
            if correlated {
                assert!(line.contains("still running"), "{terminal}: {line}");
            } else {
                assert!(line.contains("registered") && line.contains("root") && line.contains("input"), "{terminal}: {line}");
            }
            let results: Value = serde_json::from_slice(&fs::read(fixture.directory.join("results.json")).unwrap()).unwrap();
            assert_eq!(results["state"], "running", "a provider clear cannot end the launcher run");
        }
    }
}

#[test]
fn a_registered_launcher_runtime_gone_restart_and_late_end_wording_never_invites_a_duplicate() {
    let command = EngramHost::recognized_check(COMMAND).unwrap();
    for line in [engram_closed_unended_carry_line(&command, "runtime cleared"),
        engram_reported_end_lost_to_restart_line(&command, false),
        engram_reported_end_lost_to_restart_line(&command, true),
        engram_late_end_line(&command, COMMAND, Some(EngramCommandExit::Code(0)))]
    {
        assert!(line.contains("registered launcher runs") && line.contains("rerun only if none"), "{line}");
        assert!(!line.contains("run it again") && !line.contains("run the check again"), "{line}");
    }
}

#[test]
fn a_registered_launcher_with_a_dead_writer_keeps_honest_unknown_closure() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::start();
    fixture.background_reply();
    // A reaped child supplies a real, inspectable dead pid on every platform.
    // An out-of-range pid cannot establish process death on Unix.
    let dead_writer = test_exit_success_child();
    fs::write(fixture.directory.join("results.json"), serde_json::to_vec(&json!({
        "runId": fixture.run_id, "state": "running", "started": fixture.started,
        "pid": dead_writer.id(), "heartbeat": { "at": "1970-01-01T00:00:00Z", "everyMs": 1000 },
        "stages": [{ "name": "focused", "state": "running" }],
    })).unwrap()).unwrap();
    assert_eq!(TestRunDisk::read(&fixture.directory, None).unwrap()
        .state(&test_run_process_writer_may_be_alive), TestRunState::Unknown);
    let (line, closed) = fixture.finish();
    fixture.assert_advice(&line);
    assert!(closed && !line.contains("correlated with"), "{line}");
}

#[test]
fn a_registered_launcher_with_a_line_key_cannot_claim_exact_correlation() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::start();
    fixture.background_reply();
    fixture.turn.record_mut(|record| record.engram.active_turn_checks.iter_mut()
        .find(|check| check.key == KEY).unwrap().keyed_by_line = true);
    let prepared = fixture.turn.state.prepare_registered_check_notices(&fixture.turn.session_id);
    assert_eq!(prepared.len(), 1);
    assert!(prepared[0].observation.is_none());
    let (line, closed) = fixture.finish();
    fixture.assert_advice(&line);
    assert!(closed && !line.contains("correlated with"), "{line}");
}

#[test]
fn a_registered_launcher_with_a_replaced_request_keeps_honest_unknown_closure() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::start();
    fixture.background_reply();
    let path = fixture.directory.join("request.json");
    let mut request: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    request["owner"] = json!("another-session");
    fs::write(path, serde_json::to_vec(&request).unwrap()).unwrap();
    let (line, closed) = fixture.finish();
    fixture.assert_advice(&line);
    assert!(closed && !line.contains("correlated with"), "{line}");
}

#[test]
fn a_registered_launcher_with_a_changed_terminal_digest_keeps_honest_unknown_closure() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::start();
    fixture.background_reply();
    fixture.write_results("passed");
    fixture.turn.state.refresh_test_runs();
    let retained = fixture.turn.state.inner.lock().unwrap().test_run_observations.values().any(|row|
        row.artifact.as_ref().is_some_and(|artifact| artifact.terminal_digest.is_some()));
    assert!(retained, "the existing owner retained the exact terminal digest");
    fixture.write_results("failed");
    let (line, closed) = fixture.finish();
    fixture.assert_advice(&line);
    assert!(closed && !line.contains("correlated with"), "{line}");
}

#[test]
fn a_registered_launcher_locator_cannot_be_borrowed_by_a_same_key_successor() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::start();
    fixture.background_reply();
    fixture.write_results("passed");
    fixture.recorder.command_completed_with_exit(KEY, COMMAND,
        &format!("RUN {}", test_run_display_path(&fixture.directory)),
        CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
    fixture.recorder.command_started(KEY, COMMAND).unwrap();
    fixture.turn.wait_for_snapshots();
    let prepared = fixture.turn.state.prepare_registered_check_notices(&fixture.turn.session_id);
    assert_eq!(prepared.len(), 2, "preparation retains the completed check and its successor");
    let completed = prepared.iter().find(|notice| notice.check.end.is_some()).unwrap();
    let successor = prepared.iter().find(|notice| notice.check.end.is_none()).unwrap();
    assert_ne!(completed.check.sequence, successor.check.sequence);
    assert!(completed.observation.is_some(), "the original invocation retains its locator");
    assert!(successor.observation.is_none(), "the previous start's locator is not this invocation's");
    let _ = fixture.turn.finish();
    let (line, closed) = fixture.line();
    fixture.assert_advice(&line);
    assert!(closed && !line.contains("correlated with"), "{line}");
}

#[test]
fn a_registered_launcher_preparation_rejects_changed_generation_runtime_and_owner_rows() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for change in ["generation", "runtime", "registration", "observation"] {
        let mut fixture = RegisteredLauncherTurn::start();
        fixture.background_reply();
        let prepared = fixture.turn.state.prepare_registered_check_notices(&fixture.turn.session_id);
        assert_eq!(prepared.len(), 1);
        let (lines, correlated) = {
            let mut inner = fixture.turn.state.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.turn.session_id).unwrap();
            match change {
                "generation" => inner.sessions[index].active_turn_generation += 1,
                "runtime" => inner.sessions[index].runtime = SessionRuntime::None,
                "registration" => { inner.test_run_registrations.remove(&fixture.run_id); }
                _ => { inner.test_run_observations.remove(&prepared[0].observation.as_ref().unwrap().execution_id); }
            }
            registered_check_notices_locked(&mut inner, index, &prepared)
        };
        assert!(correlated.is_empty(), "{change}");
        assert!(!lines.iter().any(|line| line.contains("correlated with")), "{change}");
    }
}
