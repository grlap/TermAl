// Disposable host/provider telemetry fixtures. No consumer suite is executed,
// and these synthetic artifacts are not consumer verification evidence.
use super::*;
use crate::tests::engram_declared_source_fixture::NativeSourceOwner;

#[path = "engram_declared_task_artifacts.rs"]
mod task_artifacts;

struct NativeFixture {
    source_owner: NativeSourceOwner,
    fixture: RunFixture,
    session: String,
    token: RuntimeToken,
    provenance: EngramObservationProvenance,
    claude: bool,
    claude_state: Mutex<ClaudeTurnState>,
}

#[test]
fn native_opening_authority_never_renews_the_capture_budget() {
    let requested = std::time::Instant::now();
    let remaining = requested + Duration::from_secs(1);
    assert_eq!(native_opening_authority_deadline(&EngramBudgetClock::Real, remaining, requested), remaining);
    let full = requested + NATIVE_S0_CAPTURE_BUDGET;
    assert_eq!(native_opening_authority_deadline(&EngramBudgetClock::Real, full, requested),
        requested + TEST_RUN_REGISTRATION_DURABILITY_BUDGET);
}

#[test]
fn native_opening_stage_diagnostics_are_bounded_and_keep_the_active_operation() {
    let timings = Arc::new(SourceReadTimings::default());
    for _ in 0..40 { drop(timings.begin("git_config")); }
    let active = timings.begin("git_cat_file");
    let summary = timings.summary();
    assert!(summary.contains("earlier_stages_omitted") && summary.contains("git_cat_file:running:"));
    assert!(summary.len() <= 2048);
    drop(active);
    assert!(!timings.summary().contains(":running:"));
}

#[test]
fn native_opening_without_declaration_skips_the_capture_worker() {
    let native = NativeFixture::new("native-no-declaration", false);
    fs::remove_file(native.fixture.root.join(ENGRAM_DECLARATION_FILE)).unwrap();
    let captures = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = captures.clone();
    *native.fixture.state.capture_lifetime.before_capture.lock().unwrap() = Some(Arc::new(move || {
        seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));
    native.fixture.state.prepare_declared_run_before_handoff(&native.session, &native.token, 1);
    let inner = native.fixture.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&native.session).unwrap()];
    assert!(record.engram.declared_run_turn.is_none());
    assert!(declared_run_opening_notice(record).is_none(), "non-applicable sessions get no native warning");
    assert_eq!(captures.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn native_opening_ignored_uncommitted_declaration_has_a_named_agent_notice() {
    let native = NativeFixture::new("native-uncommitted-declaration", false);
    fs::write(native.fixture.root.join(".gitignore"), "/.tmp/\n/termal-tests.toml\n").unwrap();
    run_git_test_command(&native.fixture.root, &["rm", "--cached", "--", ENGRAM_DECLARATION_FILE]);
    run_git_test_command(&native.fixture.root, &["add", "--", ".gitignore"]);
    run_git_test_command(&native.fixture.root, &["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
        "commit", "--quiet", "--no-gpg-sign", "-m", "ignored native declaration control"]);
    native.source_owner.refresh_commit();
    let source = native.source_owner.snapshot(std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT, None)
        .map_err(|error| format!("{error:#}"));
    assert!(source.is_err(), "native declaration membership refuses this uncommitted path");
    native.install_snapshot(source);
    native.fixture.state.prepare_declared_run_before_handoff(&native.session, &native.token, 1);
    let inner = native.fixture.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&native.session).unwrap()];
    let turn = record.engram.declared_run_turn.as_ref().unwrap();
    assert!(turn.snapshot.is_none());
    assert_eq!(turn.opening_refusal_code, Some("declaration_not_committed"));
    let notice = declared_run_opening_notice(record).unwrap();
    assert!(notice.contains("(declaration_not_committed)") && notice.contains("The prompt proceeds."), "{notice}");
}

#[test]
fn native_opening_expiry_notice_is_turn_scoped_for_both_providers() {
    for claude in [false, true] {
        let native = NativeFixture::new("native-opening-notice", claude);
        native.prepare();
        let expected = {
            let mut inner = native.fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&native.session).unwrap();
            let turn = inner.sessions[index].engram.declared_run_turn.as_mut().unwrap();
            turn.snapshot = None;
            turn.clone()
        };
        native.fixture.state.declared_run_opening_refused(&native.session, &expected,
            "declaration_not_committed: membership read failed: bounded read deadline exceeded (TimedOut)");
        let mut inner = native.fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&native.session).unwrap();
        let record = &mut inner.sessions[index];
        assert!(record.engram.declared_run_turn.as_ref().unwrap().snapshot.is_none());
        let notice = declared_run_opening_notice(record).unwrap();
        assert!(notice.contains("(opening_deadline)") && notice.contains("first tool activity of a later turn"), "{notice}");
        record.active_turn_generation += 1;
        assert!(declared_run_opening_notice(record).is_none(), "an old refusal cannot attach to another turn");
    }
}

#[test]
fn native_opening_publication_and_expiry_use_a_controlled_clock() {
    for elapsed_seconds in [9, 34, 36] {
        let expired = elapsed_seconds == 36;
        let native = NativeFixture::new("native-controlled-opening", false);
        // Exercise native snapshot assembly with the scripted shared owner,
        // separately from dispatch availability or real-producer latency.
        let snapshot = native.source_owner.snapshot(
            std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT, None).unwrap();
        let initial = std::time::Instant::now();
        let clock = Arc::new(Mutex::new(initial));
        let read_clock = clock.clone();
        native.fixture.state.inner.lock().unwrap().test_declared_run_clock =
            Some(Arc::new(move || *read_clock.lock().unwrap()));
        native.fixture.state.inner.lock().unwrap().test_declared_run_snapshot = Some(Arc::new(move |_, _| {
            // Controlled ordering: capture completes before publication, with
            // logical time either still valid or beyond the original deadline.
            // Nine seconds exceeds the former bound; 34 includes time for
            // opening stages around source; 36 exceeds the combined deadline.
            *clock.lock().unwrap() = initial + Duration::from_secs(elapsed_seconds);
            Ok(snapshot.clone())
        }));
        native.fixture.state.prepare_declared_run_before_handoff(&native.session, &native.token, 1);
        let inner = native.fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&native.session).unwrap()];
        let turn = record.engram.declared_run_turn.as_ref().unwrap();
        assert_eq!(turn.snapshot.is_some(), !expired);
        if expired {
            assert_eq!(turn.opening_refusal_code, Some("opening_deadline"));
            assert!(declared_run_opening_notice(record).unwrap().contains("S0 stages:"));
        }
    }
}

#[test]
fn native_opening_observes_the_named_worktree_once_without_borrowing_project_root_metadata() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut native = NativeFixture::new("native-opening-distinct-root", false);
    let named_parent = native.fixture.root.join(".tmp");
    // Windows path identity checks canonicalize their input. Convert the
    // existing parent; Git creates the new child worktree below it.
    let named_path = format!("{}/named-root", identity_git_path(&named_parent).unwrap().replace('\\', "/"));
    run_git_test_command(&native.fixture.root,
        &["worktree", "add", "--quiet", "--detach", &named_path, "HEAD"]);
    let named_root = fs::canonicalize(named_parent.join("named-root")).unwrap();
    let (store, binding) = {
        let mut inner = native.fixture.state.inner.lock().unwrap();
        let store = inner.engram_work_source_roots[0].store.clone();
        let binding = inner.engram_work_naming_history[0].proofs[0].binding.clone();
        inner.engram_work_source_roots.clear();
        inner.engram_work_naming_history.clear();
        (store, binding)
    };
    named_root::name_root(&native.fixture, &store, &binding, &native.session, &named_root, 2);
    native.source_owner = NativeSourceOwner::new(&named_root);
    native.source_owner.install(&native.fixture.state, &native.fixture.project_id);
    native.prepare();
    let pending = {
        let mut inner = native.fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&native.session).unwrap();
        let pending = inner.sessions[index].engram.declared_run_turn.as_mut().unwrap();
        pending.snapshot = None;
        pending.clone()
    };
    let expected = native.source_owner.identity();
    let expected_cwd = engram_host_workdir(native.fixture.state.persistence_path.as_ref()).unwrap();
    let mut roots = Vec::new();
    let deadline = std::time::Instant::now() + NATIVE_S0_CAPTURE_BUDGET;
    let snapshot = native.fixture.state.capture_declared_run_opening_with_owner(
        &native.session, &pending, deadline, |_, cwd, args, until, _| {
            assert_eq!(cwd, expected_cwd, "the measured root is an argument, not the host process cwd");
            assert!(native.fixture.state.inner.is_not_held_by_current_thread_for_test(), "source IO holds no StateInner lock");
            assert_eq!(until, deadline, "the original aggregate deadline is retained");
            if args[0] == "--version" {
                return Ok(super::engram_source_owner::source_owner_reply(args));
            }
            assert_eq!(args[0], "source");
            let root = fs::canonicalize(&args[2]).unwrap();
            roots.push(root);
            Ok(super::engram_source_owner::source_owner_output(0, serde_json::to_vec(&json!({
                "version":1,"status":"clean","root":args[2],
                "source":format!("git-commit:{}", expected.commit),"tree":expected.tree,
                "entries":[],"refusal":null,"source_id":expected,
            })).unwrap()))
        });
    assert_eq!(roots, vec![named_root.clone()],
        "S0 must use one actual named-root observation, with no project-root measurement or second read");
    let (snapshot, _) = snapshot.unwrap();
    assert_eq!(snapshot.source.root, named_root);
    assert_eq!(snapshot.source.source_id, expected);
    assert_eq!(snapshot.source.source_owner, native.source_owner.pin);
}

#[test]
fn native_opening_rechecks_pending_root_fence_and_owner_authority_before_returning_s0() {
    for change in ["root", "fence", "configuration", "turn", "native-identity", "first-tool", "caller-root"] {
        let native = NativeFixture::new("native-opening-authority-recheck", false);
        native.prepare();
        let mut pending = {
            let mut inner = native.fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&native.session).unwrap();
            let pending = inner.sessions[index].engram.declared_run_turn.as_mut().unwrap();
            pending.snapshot = None;
            pending.clone()
        };
        if change == "caller-root" { pending.named.workspace_id = test_run_display_path(&native.fixture.root.join(".tmp")); }
        let expected = native.source_owner.identity();
        let mut captures = 0;
        let result = native.fixture.state.capture_declared_run_opening_with_owner(
            &native.session, &pending, std::time::Instant::now() + NATIVE_S0_CAPTURE_BUDGET,
            |_, _, args, _, _| {
                if args[0] == "--version" {
                    return Ok(super::engram_source_owner::source_owner_reply(args));
                }
                captures += 1;
                let mut inner = native.fixture.state.inner.try_lock_now().expect("off-lock owner capture");
                let index = inner.find_session_index(&native.session).unwrap();
                match change {
                    "root" => inner.engram_work_source_roots[0].generation += 1,
                    "fence" => inner.engram_work_source_roots[0].claim_fence += 1,
                    "configuration" => inner.projects.iter_mut().find(|project| project.id == native.fixture.project_id)
                        .unwrap().engram.as_mut().unwrap().binary_path = Some("different-owner".into()),
                    "turn" => inner.sessions[index].active_turn_generation += 1,
                    "native-identity" => inner.sessions[index].federation_native_creation_id = Uuid::new_v4().to_string(),
                    "first-tool" => inner.sessions[index].engram.declared_run_turn.as_mut().unwrap()
                        .first_tool.observe(Some("earlier-tool"), false),
                    _ => panic!("an arbitrary caller root must be refused before any source process"),
                }
                drop(inner);
                Ok(super::engram_source_owner::source_owner_output(0, serde_json::to_vec(&json!({
                    "version":1,"status":"clean","root":args[2],
                    "source":format!("git-commit:{}", expected.commit),"tree":expected.tree,
                    "entries":[],"refusal":null,"source_id":expected,
                })).unwrap()))
            });
        assert!(result.is_err(), "{change}: changed authority cannot publish S0");
        assert_eq!(captures, usize::from(change != "caller-root"), "{change}: no extra source read");
        let inner = native.fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&native.session).unwrap()];
        assert!(record.engram.declared_run_turn.as_ref().unwrap().snapshot.is_none(), "{change}");
    }
}

#[test]
fn native_opening_owner_authority_is_rechecked_at_final_publication() {
    let native = NativeFixture::new("native-opening-publication-authority", false);
    let snapshot = native.source_owner.snapshot(
        std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT, None).unwrap();
    let state = native.fixture.state.clone();
    let session = native.session.clone();
    native.fixture.state.inner.lock().unwrap().test_declared_run_snapshot = Some(Arc::new(move |_, _| {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        // Existing runtime/root/claim checks still match. The owner admission's
        // captured native identity must also survive the publication boundary.
        inner.sessions[index].federation_native_creation_id = Uuid::new_v4().to_string();
        Ok(snapshot.clone())
    }));
    native.fixture.state.prepare_declared_run_before_handoff(&native.session, &native.token, 1);
    let inner = native.fixture.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&native.session).unwrap()];
    assert!(record.engram.declared_run_turn.as_ref().unwrap().snapshot.is_none());
}

impl NativeFixture {
    fn new(label: &str, claude: bool) -> Self {
        let fixture = RunFixture::new(label);
        fs::write(fixture.root.join(".gitignore"), "/.tmp/\n").unwrap();
        fs::write(fixture.root.join("termal-tests.toml"),
            "[[test]]\ncommand = \"dotnet test\"\ncwd = \".\"\nartifact = \".tmp/test.trx\"\n").unwrap();
        run_git_test_command(&fixture.root, &["add", "--", ".gitignore", "termal-tests.toml"]);
        run_git_test_command(&fixture.root, &["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
            "commit", "--quiet", "--no-gpg-sign", "-m", "native telemetry fixture"]);
        let session = running_owner(&fixture, "native-declared-runtime");
        let token = if claude {
            let (runtime, _commands) = test_claude_runtime_handle("native-declared-runtime");
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
            RuntimeToken::Claude("native-declared-runtime".into())
        } else { RuntimeToken::Codex("native-declared-runtime".into()) };
        let store = EngramAuthorityStoreKey { database_path: fixture.root.join("tracker.db"), project_id: "native-test".into() };
        let binding = EngramControlWorkBinding { root_execution_id:"root-native".into(), work_id:"work-native".into(),
            run_id:"run-native".into(), work_revision:3, claim_id:"claim-native".into(), claim_fence:2 };
        named_root::name_root(&fixture, &store, &binding, &session, &fs::canonicalize(&fixture.root).unwrap(), 1);
        {
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].session.status = SessionStatus::Active;
            inner.sessions[index].active_turn_generation = 1;
            // Registered committed runs must work with ordinary control OFF.
            inner.sessions[index].engram.work_binding = None;
        }
        let provenance = if claude { EngramObservationProvenance::Claude(ClaudeObservationProvenance {
            token:token.clone(),origin:ClaudeWorkOrigin::Attempt {turn_generation:1},
        }) } else { EngramObservationProvenance::Runtime {token:token.clone(),turn_generation:1} };
        let source_owner = NativeSourceOwner::new(&fixture.root);
        source_owner.install(&fixture.state, &fixture.project_id);
        Self {fixture,session,token,provenance,claude,source_owner,claude_state:Mutex::new(ClaudeTurnState::default())}
    }

    fn prepare(&self) {
        // Script the owner value for protocol tests. M1's real parser/pin
        // checks and the declaration membership run; E1 owns Git conformance.
        self.source_owner.refresh_commit();
        let snapshot = self.source_owner.snapshot(
            std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT, None).unwrap();
        self.install_snapshot(Ok(snapshot));
        self.fixture.state.prepare_declared_run_before_handoff(&self.session, &self.token, 1);
        let inner = self.fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&self.session).unwrap()];
        let opening = record.engram.declared_run_turn.as_ref().unwrap();
        if opening.snapshot.is_none() { self.fixture.state.test_temp_root.as_ref().unwrap().retain_for_diagnosis(); }
        assert!(opening.snapshot.is_some(), "S0 for {}: {:?}", self.fixture.root.display(), opening.opening_refusal);
    }

    fn install_snapshot(&self, snapshot: std::result::Result<DeclaredRunSnapshot, String>) {
        self.fixture.state.inner.lock().unwrap().test_declared_run_snapshot = Some(Arc::new(move |_, _|
            snapshot.clone().map_err(|error| anyhow!(error))));
    }

    fn record_change(&self, path: &str, status: &str) {
        self.source_owner.record_file_change(path, status);
    }

    fn held_revision(&self, revision: i64) {
        let mut binding = {
            let inner = self.fixture.state.inner.lock().unwrap();
            inner.engram_work_naming_history[0].proofs[0].binding.clone()
        };
        binding.work_revision = revision;
        let transport = ScriptedEngramControlTransport::new([]);
        transport.replace_held_claims([Ok(EngramHeldClaims {items:vec![EngramHeldClaim {
            work_id:binding.work_id.clone(),short_ref:"native-test".into(),claim_id:binding.claim_id.clone(),
            claim_fence:binding.claim_fence,focused:false,control_binding:Some(binding),
        }],omitted:0})]);
        self.fixture.state.install_test_engram_transport(transport);
    }

    fn recorder(&self) -> SessionRecorder {
        let mut recorder = SessionRecorder::new(self.fixture.state.clone(), self.session.clone());
        recorder.recorder_state.observation_provenance = self.provenance.clone();
        recorder.recorder_state.native_tool_provenance = Some(self.provenance.clone());
        recorder
    }

    fn frame(&self) -> Value {
        if self.claude {
            let path = identity_git_path(&fs::canonicalize(&self.fixture.root).unwrap()).unwrap().replace('\\', "/");
            json!({"id":"native-tool","name":"Bash","timestamp":"2000-01-01T00:00:00Z",
                "input":{"command":format!("pushd \"{path}\" && dotnet test")}})
        } else {
            json!({"id":"native-tool","type":"commandExecution","command":"dotnet test",
                "cwd":test_run_display_path(&self.fixture.root),"timestamp":"2000-01-01T00:00:00Z"})
        }
    }

    fn start(&self, recorder: &mut SessionRecorder) {
        let frame = self.frame();
        if self.claude { register_claude_tool_use(&frame, &mut self.claude_state.lock().unwrap(), recorder).unwrap(); }
        else { handle_codex_app_server_item_started(&frame, recorder).unwrap(); }
    }

    fn execution(&self) -> Option<Arc<Mutex<DeclaredRunExecution>>> {
        let inner = self.fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&self.session).unwrap()];
        record.engram.declared_run_turn.as_ref().and_then(|turn| turn.execution.clone())
    }

    fn wait(&self) -> DeclaredRunReservation {
        let execution = self.execution().expect("original start reserved");
        let deadline = std::time::Instant::now() + crate::TEST_PHASE_DEADLOCK_GUARD;
        loop {
            let job = execution.lock().unwrap();
            assert!(job.failure.is_none(), "{:?}", job.failure);
            if !job.worker_active { return job.reservation.clone(); }
            drop(job);
            assert!(std::time::Instant::now() < deadline, "native worker did not settle");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn end(&self, recorder: &mut SessionRecorder, exit: i64) {
        if self.claude {
            handle_claude_tool_result(&json!({"message":{"content":[{"type":"tool_result",
                "tool_use_id":"native-tool", "is_error":exit != 0,
                "content":if exit == 0 {"actual provider output".to_owned()} else {format!("Exit code {exit}")}}]}}),
                &mut self.claude_state.lock().unwrap(), recorder).unwrap();
        } else {
            recorder.command_completed_with_exit("native-tool", "dotnet test", "actual provider output",
                if exit == 0 {CommandStatus::Success} else {CommandStatus::Error}, EngramCommandExit::Code(exit)).unwrap();
        }
    }

    fn finish(&self, recorder: &mut SessionRecorder, exit: i64) -> (DeclaredRunReservation, TestRunRegistration, Value, Value) {
        self.end(recorder, exit);
        self.finished()
    }

    fn finished(&self) -> (DeclaredRunReservation, TestRunRegistration, Value, Value) {
        let start = self.wait();
        let inner = self.fixture.state.inner.lock().unwrap();
        let registration = inner.test_run_registrations[&start.run_id].clone();
        drop(inner);
        let request = serde_json::from_slice(&fs::read(PathBuf::from(&start.run_directory).join("request.json")).unwrap()).unwrap();
        let results = serde_json::from_slice(&fs::read(PathBuf::from(&start.run_directory).join("results.json")).unwrap()).unwrap();
        (start, registration, request, results)
    }

    fn artifact(&self, text: &str) {
        fs::create_dir_all(self.fixture.root.join(".tmp")).unwrap();
        fs::write(self.fixture.root.join(".tmp/test.trx"), text).unwrap();
    }

    fn later_prompt(&self) {
        let mut inner = self.fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&self.session).unwrap();
        inner.sessions[index].active_turn_generation += 1;
        inner.sessions[index].engram.declared_run_turn = None;
    }

    fn task_end(&self) {
        let work = claude_frame_work(&json!({"type":"system","subtype":"task_notification",
            "status":"completed","tool_use_id":"native-tool"}), false);
        self.fixture.state.settle_claude_frame(&self.session, &self.token, &work);
    }

    fn original_results(&self, start: &DeclaredRunReservation) -> (TestRunRegistration, Value, Value) {
        let directory = PathBuf::from(&start.run_directory);
        let deadline = std::time::Instant::now() + crate::TEST_PHASE_DEADLOCK_GUARD;
        loop {
            let registration = self.fixture.state.inner.lock().unwrap()
                .test_run_registrations.get(&start.run_id).cloned();
            if let Some(registration) = registration
                && let TestRunJson::Parsed(results, _, _) = test_run_read_json(&directory.join("results.json"))
                && engram_verification_record_results_are_terminal(&registration, &results) {
                let request = serde_json::from_slice(&fs::read(directory.join("request.json")).unwrap()).unwrap();
                return (registration, request, results);
            }
            assert!(std::time::Instant::now() < deadline, "original task terminal was not frozen");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn native_provider_starts_register_once_from_persisted_host_time_and_freeze_the_real_artifact() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for claude in [false, true] {
        let fixture = NativeFixture::new("native-host-original", claude);
        fixture.held_revision(4);
        fixture.prepare();
        let mut recorder = fixture.recorder();
        fixture.start(&mut recorder);
        let original = fixture.wait();
        fixture.start(&mut recorder);
        assert_eq!(fixture.wait(), original, "duplicate start cannot move R");
        assert_ne!(original.registered_at, original.provider_time["timestamp"].as_str().unwrap());
        let persisted = load_state(fixture.fixture.state.persistence_path.as_path()).unwrap().unwrap();
        assert_eq!(persisted.test_run_observations[&original.execution_id].declared_native.as_ref(), Some(&original));
        fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
        let (start, registration, request, results) = fixture.finish(&mut recorder, 0);
        assert_eq!(registration.registered_at, start.registered_at);
        assert_eq!(results["stages"][0]["started"], start.registered_at);
        if claude {
            assert!(results["exitCode"].is_null(), "reported success is not a numeric OS exit");
            assert_eq!(results["stages"][0]["reportedSuccess"], true);
        }
        assert!(engram_verification_record_producer_refusal(&registration, &results).is_none());
        assert_eq!(request["observedCommand"], json!(["dotnet", "test"]));
        assert!(fs::canonicalize(&start.run_directory).unwrap()
            .starts_with(fs::canonicalize(fixture.fixture.root.join(".git")).unwrap()));
        let body = engram_verification_record_body(&registration, &request, &results).unwrap();
        assert_eq!(body.run.outcome, EngramVerificationRecordRunOutcome::Passed);
        if claude {
            assert!(!test_run_results_are_terminal(&results), "generic launcher terminal rules stay numeric");
            assert!(engram_verification_record_results_are_terminal(&registration, &results));
            assert!(matches!(engram_verification_record_prepare(&registration), EngramVerificationRecordPrepared::Ready(_)),
                "the real producer's pre-body terminal gate must admit the authenticated endpoint too");
            let mut generic = registration.clone();
            generic.declared_native = None;
            assert!(!engram_verification_record_results_are_terminal(&generic, &results),
                "a terminal snapshot without its host reservation grants no exception");
            assert!(matches!(engram_verification_record_prepare(&generic), EngramVerificationRecordPrepared::NotEligible),
                "a generic registration cannot adopt source_id input");
            let mut unreported = registration.clone();
            unreported.declared_terminal.as_mut().unwrap().reported_success = false;
            assert!(!engram_verification_record_results_are_terminal(&unreported, &results),
                "a result flag cannot supply the original ReportedSuccess");
            let mut rewritten = results.clone();
            rewritten["stages"][0]["reportedSuccess"] = json!(false);
            assert!(!engram_verification_record_results_are_terminal(&registration, &rewritten),
                "the result must preserve reported success");
            rewritten = results.clone();
            rewritten["laterValue"] = json!(true);
            assert!(!engram_verification_record_results_are_terminal(&registration, &rewritten),
                "even a shape-preserving rewrite must match the frozen results digest");
        } else {
            assert!(test_run_results_are_terminal(&results));
        }
        fixture.artifact(include_str!("fixtures/declared-trx/fail.trx"));
        assert_eq!(engram_verification_record_body(&registration, &request, &results).unwrap(), body,
            "a later artifact cannot change the original frozen judgment");
        let mut changed = results.clone();
        changed["exitCode"] = json!(1);
        assert!(engram_verification_record_body(&registration, &request, &changed).is_err());
        assert!(run_git_test_command_output(&fixture.fixture.root, &["status", "--porcelain"]).is_empty());
    }
}

#[test]
fn native_display_uses_the_frozen_artifact_verdict_instead_of_a_successful_exit() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for claude in [false, true] {
        for artifact in ["pass", "fail", "missing", "stale", "zero", "malformed"] {
            let fixture = NativeFixture::new(&format!("native-display-{claude}-{artifact}"), claude);
            fixture.held_revision(4);
            if artifact == "stale" { fixture.artifact(include_str!("fixtures/declared-trx/pass.trx")); }
            fixture.prepare();
            let mut recorder = fixture.recorder();
            fixture.start(&mut recorder);
            fixture.wait();
            match artifact {
                "pass" => fixture.artifact(include_str!("fixtures/declared-trx/pass.trx")),
                "fail" => fixture.artifact(include_str!("fixtures/declared-trx/fail.trx")),
                "zero" => fixture.artifact(include_str!("fixtures/declared-trx/zero.trx")),
                "malformed" => fixture.artifact("not a TRX result"),
                _ => {},
            }
            let (start, registration, request, results) = fixture.finish(&mut recorder, 0);
            let (expected_state, expected_outcome) = match artifact {
                "pass" => (TestRunState::Passed, EngramVerificationRecordRunOutcome::Passed),
                "fail" => (TestRunState::Failed, EngramVerificationRecordRunOutcome::Failed),
                _ => (TestRunState::Unknown, EngramVerificationRecordRunOutcome::Indeterminate),
            };
            assert_eq!(results["state"], serde_json::to_value(expected_state).unwrap(), "{claude}/{artifact}");
            assert_eq!(results["stages"][0]["state"], results["state"]);
            assert_eq!(results["exitCode"], if claude { Value::Null } else { json!(0) },
                "the original provider endpoint is retained");
            let body = engram_verification_record_body(&registration, &request, &results).unwrap();
            assert_eq!(body.run.outcome, expected_outcome, "{claude}/{artifact}");
            let inner = fixture.fixture.state.inner.lock().unwrap();
            assert_eq!(inner.test_run_observations[&start.execution_id].summary.state, expected_state,
                "the original native row uses the same frozen verdict");
            drop(inner);
            fixture.fixture.refresh(&[]);
            let indexed = fixture.fixture.state.test_run_summaries(None).into_iter()
                .find(|summary| summary.run_id == start.run_id).expect("the registered native run is indexed");
            if artifact != "pass" {
                assert_ne!(indexed.state, TestRunState::Passed, "{claude}/{artifact}: no false passing card");
            }
        }
    }
}

#[test]
fn native_worker_refusal_still_displays_the_original_end_artifact_verdict() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for claude in [false, true] {
        for artifact in ["pass", "fail", "missing", "stale", "zero", "malformed"] {
            let fixture = NativeFixture::new(&format!("native-refused-display-{claude}-{artifact}"), claude);
            fixture.held_revision(4);
            if artifact == "stale" { fixture.artifact(include_str!("fixtures/declared-trx/pass.trx")); }
            fixture.prepare();
            let mut recorder = fixture.recorder();
            let slots = fixture.fixture.state.inner.lock().unwrap().engram_turn_basis_captures_live.clone();
            slots.fetch_add(ENGRAM_TURN_BASIS_CAPTURE_LIMIT, std::sync::atomic::Ordering::SeqCst);
            fixture.start(&mut recorder);
            slots.fetch_sub(ENGRAM_TURN_BASIS_CAPTURE_LIMIT, std::sync::atomic::Ordering::SeqCst);
            let execution = fixture.execution().expect("the original start has an observation");
            let start = {
                let job = execution.lock().unwrap();
                assert!(job.failure.as_deref().is_some_and(|failure| failure.contains("capacity unavailable")));
                assert!(!job.worker_active);
                assert!(!job.registered);
                job.reservation.clone()
            };
            match artifact {
                "pass" => fixture.artifact(include_str!("fixtures/declared-trx/pass.trx")),
                "fail" => fixture.artifact(include_str!("fixtures/declared-trx/fail.trx")),
                "zero" => fixture.artifact(include_str!("fixtures/declared-trx/zero.trx")),
                "malformed" => fixture.artifact("not a TRX result"),
                _ => {},
            }
            fixture.end(&mut recorder, 0);
            let expected = match artifact {
                "pass" => TestRunState::Passed,
                "fail" => TestRunState::Failed,
                _ => TestRunState::Unknown,
            };
            let inner = fixture.fixture.state.inner.lock().unwrap();
            assert!(!inner.test_run_registrations.contains_key(&start.run_id), "capacity refusal earns no registration");
            let row = &inner.test_run_observations[&start.execution_id];
            assert_eq!(row.summary.state, expected, "{claude}/{artifact}");
            assert_eq!(row.summary.exit_code, if claude { None } else { Some(0) });
            assert!(row.summary.ended_at.is_some());
            drop(inner);
            let indexed = fixture.fixture.state.test_run_summaries(None).into_iter()
                .find(|summary| summary.run_id == start.execution_id).expect("the original observation remains visible");
            assert_eq!(indexed.state, expected, "{claude}/{artifact}: the visible card follows the artifact");
            assert!(execution.lock().unwrap().failure.is_some(), "artifact inspection cannot revive a failed worker");
        }
    }
}

#[test]
fn native_end_freezes_artifact_before_a_blocked_registration_worker() {
    struct Release(Option<std::sync::mpsc::Sender<()>>);
    impl Drop for Release {
        fn drop(&mut self) { if let Some(sender) = self.0.take() { let _ = sender.send(()); } }
    }
    for failing_artifact in [false, true] {
        let fixture = NativeFixture::new("native-end-boundary", false);
        fixture.held_revision(4);
        fixture.prepare();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut release = Release(Some(release_tx));
        let release_rx = Arc::new(Mutex::new(release_rx));
        let blocked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        *fixture.fixture.state.capture_lifetime.before_capture.lock().unwrap() = Some(Arc::new(move || {
            if std::thread::current().name() == Some("native-declared-run")
                && !blocked.swap(true, std::sync::atomic::Ordering::SeqCst) {
                ready_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv_timeout(crate::TEST_PHASE_DEADLOCK_GUARD).unwrap();
            }
        }));
        let mut recorder = fixture.recorder();
        fixture.start(&mut recorder);
        ready_rx.recv_timeout(crate::TEST_PHASE_DEADLOCK_GUARD).unwrap();
        if failing_artifact { fixture.artifact(include_str!("fixtures/declared-trx/fail.trx")); }
        recorder.command_completed_with_exit("native-tool", "dotnet test", "original END",
            CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
        // The worker has not registered or inspected anything yet. A later
        // passing artifact must not be borrowed by the completed invocation.
        fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
        release.0.take().unwrap().send(()).unwrap();
        let start = fixture.wait();
        let registration = fixture.fixture.state.inner.lock().unwrap().test_run_registrations[&start.run_id].clone();
        let directory = PathBuf::from(&start.run_directory);
        let request = serde_json::from_slice(&fs::read(directory.join("request.json")).unwrap()).unwrap();
        let results = serde_json::from_slice(&fs::read(directory.join("results.json")).unwrap()).unwrap();
        let body = engram_verification_record_body(&registration, &request, &results).unwrap();
        assert_ne!(body.run.outcome, EngramVerificationRecordRunOutcome::Passed,
            "a later PASS cannot replace the artifact observed at original END");
    }
}

#[test]
fn native_credit_compares_endpoints_independently_of_workspace_events() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for (restored, reported) in [(true, true), (false, true), (false, false)] {
        let fixture = NativeFixture::new("native-endpoint-only", false);
        let path = fixture.fixture.root.join("endpoint.txt");
        fs::write(&path, "original\n").unwrap();
        run_git_test_command(&fixture.fixture.root, &["add", "--", "endpoint.txt"]);
        run_git_test_command(&fixture.fixture.root, &["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
            "commit", "--quiet", "--no-gpg-sign", "-m", "native endpoint fixture"]);
        fixture.held_revision(4);
        fixture.prepare();
        let mut recorder = fixture.recorder();
        fixture.start(&mut recorder);
        fixture.wait();
        fs::write(&path, "changed\n").unwrap();
        if reported {
            // Exercise the consumer with a normalized incoming event. The
            // fixture itself may live under target, which the watch adapter
            // deliberately excludes before producing such events.
            let changes = workspace_file_changes_from_path(&path, WorkspaceFileChangeKind::Modified, &[]);
            assert!(!changes.is_empty());
            fixture.fixture.state.note_engram_workspace_file_changes(&changes);
        }
        if restored { fs::write(&path, "original\n").unwrap(); }
        else { fixture.record_change("endpoint.txt", " M"); }
        fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
        let (_, registration, request, results) = fixture.finish(&mut recorder, 0);
        let body = engram_verification_record_body(&registration, &request, &results);
        if restored {
            assert_eq!(body.unwrap().run.outcome, EngramVerificationRecordRunOutcome::Passed,
                "equal clean endpoints accept a fully restored edit even when its event was observed");
        } else {
            assert!(body.is_err(), "a changed full source_id refuses credit with or without an event");
        }
    }
}

#[test]
fn native_no_snapshot_prior_activity_wrong_cwd_or_ambient_never_reserves() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for case in ["no-snapshot", "prior-read", "wrong-cwd", "ambient", "late-description"] {
        let fixture = NativeFixture::new(case, false);
        let mut recorder = fixture.recorder();
        if case != "no-snapshot" { fixture.prepare(); }
        if case == "prior-read" { recorder.native_tool_start(Some("earlier"), false, &json!({"name":"Read"})).unwrap(); }
        if case == "ambient" {
            recorder.recorder_state.native_tool_provenance = None;
            recorder.recorder_state.observation_provenance = EngramObservationProvenance::Ambient;
        }
        let mut frame = fixture.frame();
        if case == "wrong-cwd" { frame["cwd"] = json!(fixture.fixture.root.parent().unwrap().to_string_lossy()); }
        if case == "late-description" { frame["command"] = Value::Null; }
        recorder.native_tool_start(Some("native-tool"), true, &frame).unwrap();
        if case == "late-description" { recorder.native_tool_start(Some("native-tool"), true, &fixture.frame()).unwrap(); }
        assert!(fixture.execution().is_none(), "{case}");
        assert!(fixture.fixture.state.inner.lock().unwrap().test_run_registrations.is_empty());
    }
}

#[test]
fn native_credit_accepts_equal_dirty_source_ids_and_refuses_changed_dirty_content() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for changed in [false, true] {
        let fixture = NativeFixture::new("native-dirty-equality", false);
        fs::write(fixture.fixture.root.join("local.txt"), "local state before\n").unwrap();
        fixture.record_change("local.txt", "??");
        fixture.held_revision(4);
        fixture.prepare();
        let before = fixture.source_owner.identity();
        let mut recorder = fixture.recorder();
        fixture.start(&mut recorder);
        fixture.wait();
        if changed {
            fs::write(fixture.fixture.root.join("local.txt"), "different local state after\n").unwrap();
            fixture.record_change("local.txt", "??");
        }
        fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
        let (_, registration, request, results) = fixture.finish(&mut recorder, 0);
        assert_eq!(results["state"], "passed", "the original command result is retained");
        let body = engram_verification_record_body(&registration, &request, &results);
        if changed {
            assert!(body.is_err(), "equal root commits do not stand in for different local content");
        } else {
            let body = body.unwrap();
            assert_eq!(body.run.outcome, EngramVerificationRecordRunOutcome::Passed);
            let wire = serde_json::to_value(body).unwrap();
            assert_eq!(wire["input"], json!({"kind":"source_id","source_id":before}));
            assert_eq!(wire["post_run"], json!({"source_id":before}));
            assert!(wire["post_run"].get("clean_before").is_none());
        }
    }
}

#[test]
fn native_credit_compares_the_complete_child_object_without_requiring_a_clean_child() {
    let fixture = NativeFixture::new("native-child-equality", false);
    let mut before = fixture.source_owner.identity();
    before.submodules = vec![EngramSourceIdSubmodule {
        path:"child/nested".into(), commit:before.commit.clone(),
        changes:vec![EngramSourceIdChange {
            path:"local.txt".into(),status:"??".into(),origin:None,
            blob:Some(before.tree.clone()),mode:Some(EngramSourceIdMode::Regular),
        }],
    }];
    fixture.source_owner.replace(before.clone());
    fixture.held_revision(4);
    fixture.prepare();
    let mut recorder = fixture.recorder();
    fixture.start(&mut recorder);
    fixture.wait();
    fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
    let (_, registration, request, results) = fixture.finish(&mut recorder, 0);
    let body = engram_verification_record_body(&registration, &request, &results).unwrap();
    assert_eq!(serde_json::to_value(&body).unwrap()["input"]["source_id"], json!(before));
    for field in ["commit", "blob", "mode"] {
        let mut changed = registration.clone();
        let after = changed.declared_terminal.as_mut().unwrap().source.as_mut().unwrap();
        let child = &mut after.source_id.submodules[0];
        match field {
            "commit" => child.commit = before.tree.clone(),
            "blob" => child.changes[0].blob = Some(before.commit.clone()),
            "mode" => child.changes[0].mode = Some(EngramSourceIdMode::Executable),
            _ => unreachable!(),
        }
        let mut changed_results = results.clone();
        changed_results["post_run"]["source_id"] = json!(after.source_id);
        changed.declared_terminal.as_mut().unwrap().results_digest =
            file_content_hash(&serde_json::to_vec_pretty(&changed_results).unwrap());
        assert!(engram_verification_record_body(&changed, &request, &changed_results).is_err(),
            "a changed child {field} cannot borrow the equal root commit");
    }
}

#[test]
fn native_source_change_reason_reaches_withheld_record_and_turn_notice() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for kind in ["tracked_file", "untracked_file", "untracked_removed", "untracked_staged", "submodule_head", "many", "equal"] {
        let fixture = NativeFixture::new("native-source-change-reason", false);
        let path = match kind { "submodule_head" => "child/nested", "many" => "entry-0000-é.txt", _ => "source.txt" };
        if matches!(kind, "untracked_removed" | "untracked_staged") {
            fs::write(fixture.fixture.root.join(path), "untracked before\n").unwrap();
            fixture.record_change(path, "??");
        }
        if kind == "tracked_file" {
            fs::write(fixture.fixture.root.join(path), "before\n").unwrap();
            run_git_test_command(&fixture.fixture.root, &["add", "--", path]);
            run_git_test_command(&fixture.fixture.root, &["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                "commit", "--quiet", "--no-gpg-sign", "-m", "tracked source fixture"]);
            fixture.source_owner.refresh_commit();
        }
        if kind == "submodule_head" {
            let mut source = fixture.source_owner.identity();
            source.submodules.push(EngramSourceIdSubmodule { path:path.into(),commit:"1".repeat(40),changes:Vec::new() });
            fixture.source_owner.replace(source);
        }
        fixture.held_revision(4);
        fixture.prepare();
        let mut recorder = fixture.recorder();
        fixture.start(&mut recorder);
        fixture.wait();
        match kind {
            "tracked_file" | "untracked_file" => {
                fs::write(fixture.fixture.root.join(path), "after\n").unwrap();
                fixture.record_change(path, if kind == "untracked_file" { "??" } else { " M" });
            }
            "submodule_head" => {
                let mut source = fixture.source_owner.identity();
                source.submodules[0].commit = "2".repeat(40);
                fixture.source_owner.replace(source);
            }
            "untracked_removed" => {
                fs::remove_file(fixture.fixture.root.join(path)).unwrap();
                let mut source = fixture.source_owner.identity();
                source.changes.clear();
                fixture.source_owner.replace(source);
            }
            "untracked_staged" => fixture.record_change(path, "A "),
            "many" => {
                let mut source = fixture.source_owner.identity();
                source.changes = (0..128).map(|index| EngramSourceIdChange {
                    path:format!("entry-{index:04}-é.txt"),status:"??".into(),origin:None,
                    blob:Some(source.commit.clone()),mode:Some(EngramSourceIdMode::Regular),
                }).collect();
                fixture.source_owner.replace(source);
            }
            _ => {}
        }
        fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
        let (_, registration, request, results) = fixture.finish(&mut recorder, 0);
        let prepared = engram_verification_record_prepare(&registration);
        if kind == "equal" {
            assert!(matches!(prepared, EngramVerificationRecordPrepared::Ready(_)));
            assert_eq!(engram_verification_record_body(&registration, &request, &results).unwrap().run.outcome,
                EngramVerificationRecordRunOutcome::Passed);
            let inner = fixture.fixture.state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
            assert!(!record.engram.pending_source_root_line.as_deref().unwrap_or("").contains("source changed during the run"));
            continue;
        }
        assert_eq!(results["state"], "passed", "the artifact verdict stays factual");
        assert_eq!(results["nativeEligible"], false);
        let EngramVerificationRecordPrepared::Pending(EngramVerificationRecordPending::SourceChanged { detail, changes }) = prepared
            else { panic!("expected a specific source-change reason: {prepared:?}"); };
        assert!(detail.contains("source changed during the run") && detail.contains(path));
        let expected_kind = if kind.starts_with("untracked") || kind == "many" { "untracked_file" } else { kind };
        assert!(changes.iter().any(|change| change.kind == expected_kind && change.path == path && change.before != change.after));
        let progress = engram_verification_record_progress(FsPath::new(&registration.run_directory)).unwrap();
        let progress = serde_json::to_value(progress).unwrap();
        assert_eq!(progress["reason"]["kind"], "source_changed");
        if kind == "many" {
            assert_eq!(progress["reason"]["changes"].as_array().unwrap().len(), 128, "persist every changed entry");
            assert!(progress["reason"]["detail"].as_str().unwrap().contains("entry-0127-é.txt"));
        }
        assert!(!PathBuf::from(&registration.run_directory).join(ENGRAM_VERIFICATION_RECORD_BODY_FILE).exists(),
            "source drift still emits no verification body");
        let inner = fixture.fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
        let notice = record.engram.pending_source_root_line.as_deref().unwrap();
        assert!(notice.contains("source changed during the run") && notice.contains(path), "{notice}");
        assert!(notice.len() <= ENGRAM_CHECK_CREDIT_LINE_MAX_BYTES, "bounded first credit line");
        drop(inner);
        if kind == "many" {
            let mut inner = fixture.fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.session).unwrap();
            let engram = &mut inner.sessions[index].engram;
            engram.pending_source_root_line = None;
            let mut log = Vec::new();
            engram_publish_check_notice(&fixture.session, engram,
                format!("{ENGRAM_CHECK_CREDIT_LINE_PREFIX} {detail}"), &mut log);
            assert!(String::from_utf8(log).unwrap().contains("entry-0127-é.txt"), "complete diagnostic reaches host log");
            assert!(engram.pending_source_root_line.as_ref().unwrap().len() <= ENGRAM_CHECK_CREDIT_LINE_MAX_BYTES);
        }
        let mut wrong_request = request.clone();
        wrong_request["nativeSourceId"] = Value::Null;
        let refusal = engram_verification_record_body(&registration, &wrong_request, &results).unwrap_err();
        assert!(refusal.contains("host reservation") && !refusal.contains("source changed"),
            "reservation mismatch remains distinct: {refusal}");
    }
}

#[test]
fn native_full_source_overflow_is_named_without_truncation_or_a_commit_fallback() {
    let fixture = NativeFixture::new("native-source-overflow", false);
    let mut source = fixture.source_owner.identity();
    source.changes = (0..1600).map(|index| EngramSourceIdChange {
        path:format!("local-{index:05}.txt"),status:"??".into(),origin:None,
        blob:Some(source.commit.clone()),mode:Some(EngramSourceIdMode::Regular),
    }).collect();
    fixture.source_owner.replace(source.clone());
    fixture.held_revision(4);
    fixture.prepare();
    let mut recorder = fixture.recorder();
    fixture.start(&mut recorder);
    fixture.wait();
    fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
    let (_, registration, request, results) = fixture.finish(&mut recorder, 0);
    assert_eq!(results["input"]["source_id"], json!(source), "the original complete evidence is retained");
    assert_eq!(results["state"], "passed", "transport limits do not rewrite the original outcome");
    let error = engram_verification_record_body(&registration, &request, &results).unwrap_err();
    assert!(error.starts_with("native_verification_frame_too_large:"), "{error}");
    assert!(matches!(engram_verification_record_prepare(&registration),
        EngramVerificationRecordPrepared::Pending(EngramVerificationRecordPending::BodyNotBuilt { detail })
            if detail.starts_with("native_verification_frame_too_large:")));
}

#[test]
fn native_artifact_failure_and_changed_source_cannot_inherit_a_pass() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for case in ["missing-artifact", "failing-artifact", "dirty-after", "owner-unavailable", "changed-root", "changed-claim"] {
        let fixture = NativeFixture::new(case, false);
        fixture.prepare();
        let mut recorder = fixture.recorder();
        fixture.start(&mut recorder);
        fixture.wait();
        if case == "failing-artifact" { fixture.artifact(include_str!("fixtures/declared-trx/fail.trx")); }
        else if case != "missing-artifact" { fixture.artifact(include_str!("fixtures/declared-trx/pass.trx")); }
        if case == "dirty-after" {
            fs::write(fixture.fixture.root.join("new-source"), "dirty").unwrap();
            fixture.record_change("new-source", "??");
        }
        if case == "owner-unavailable" {
            fixture.fixture.state.inner.lock().unwrap().test_declared_run_source = Some(Arc::new(|_, _, _|
                anyhow::bail!("Engram source input_unavailable: scripted owner refused")));
        }
        if case == "changed-root" {
            let mut inner = fixture.fixture.state.inner.lock().unwrap();
            inner.engram_work_source_roots[0].generation += 1;
        }
        if case == "changed-claim" { fixture.held_revision(4); }
        let (_, registration, request, results) = fixture.finish(&mut recorder, 0);
        let body = engram_verification_record_body(&registration, &request, &results);
        match case {
            "missing-artifact" => assert_eq!(body.unwrap().run.outcome, EngramVerificationRecordRunOutcome::Indeterminate),
            "failing-artifact" => assert_eq!(body.unwrap().run.outcome, EngramVerificationRecordRunOutcome::Failed),
            "owner-unavailable" => {
                assert_eq!(results["state"], "passed");
                assert!(results["post_run"]["source_id"].is_null());
                assert!(results["nativeSourceUnavailableReason"].as_str().unwrap().contains("scripted owner refused"));
                assert!(body.unwrap_err().contains("scripted owner refused"));
            }
            _ => assert!(body.is_err(), "{case}"),
        }
    }
}

#[test]
fn native_metadata_admission_precedes_writes_and_refuses_a_redirect() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = NativeFixture::new("native-metadata-containment", false);
    fixture.prepare();
    let mut recorder = fixture.recorder();
    fixture.start(&mut recorder);
    let mut start = fixture.wait();
    let outside = fixture.fixture.root.join(".tmp/metadata-outside");
    fs::create_dir_all(&outside).unwrap();
    start.run_directory = test_run_display_path(&outside.join("review-runs").join(&start.run_id));
    assert!(declared_run_prepare_directory(&start).is_err());
    assert!(!outside.join("review-runs").exists(), "no directory may be created outside Git metadata");
    let admin = fixture.fixture.root.join(".git/worktrees/native-containment");
    fs::create_dir_all(&admin).unwrap();
    let link = admin.join("review-runs");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    #[cfg(windows)]
    {
        let created = Command::new("cmd.exe").args(["/d", "/c", "mklink", "/J"])
            .arg(link.to_string_lossy().replace('/', "\\"))
            .arg(outside.to_string_lossy().replace('/', "\\"))
            .creation_flags(0x08000000).output().unwrap();
        assert!(created.status.success(), "{}", String::from_utf8_lossy(&created.stderr));
    }
    start.run_directory = test_run_display_path(&link.join(&start.run_id));
    assert!(declared_run_prepare_directory(&start).is_err(), "resolved redirect is outside Git metadata");
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0, "neither request nor results was written through the redirect");
}

#[test]
fn native_artifact_verdict_does_not_depend_on_provider_console_or_exit() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for case in ["cut", "no-preview", "background", "moved", "no-exit", "nonzero", "killed", "missing", "failing"] {
        let fixture = NativeFixture::new(case, true);
        fixture.held_revision(4);
        fixture.prepare();
        let mut recorder = fixture.recorder();
        let mut frame = fixture.frame();
        if case == "background" { frame["input"]["run_in_background"] = json!(true); }
        register_claude_tool_use(&frame, &mut fixture.claude_state.lock().unwrap(), &mut recorder).unwrap();
        fixture.wait();
        match case {
            "missing" => {},
            "failing" => fixture.artifact(include_str!("fixtures/declared-trx/fail.trx")),
            _ => fixture.artifact(include_str!("fixtures/declared-trx/pass.trx")),
        }
        let mut result = json!({"stdout":"preview contains no test verdict"});
        if case == "cut" {
            result["persistedOutputPath"] = json!("/a/provider/path/that/must/not/be/opened");
            result["persistedOutputSize"] = json!(1_500_000);
        }
        if case == "no-preview" { result = json!({}); }
        if case == "moved" { result["backgroundTaskId"] = json!("original-task"); }
        if case == "killed" { result["interrupted"] = json!(true); }
        handle_claude_tool_result(&json!({"tool_use_result":result,"message":{"content":[{
            "type":"tool_result","tool_use_id":"native-tool",
            "is_error":matches!(case, "no-exit" | "nonzero" | "killed"),
            "content":if case == "nonzero" {"Exit code 1"} else {""},
        }]}}), &mut fixture.claude_state.lock().unwrap(), &mut recorder).unwrap();
        if matches!(case, "background" | "moved") {
            assert!(fixture.execution().unwrap().lock().unwrap().end.is_none(), "launch acknowledgement is not END");
            let work = claude_frame_work(&json!({"type":"system","subtype":"task_notification",
                "status":"completed","tool_use_id":"native-tool"}), false);
            fixture.fixture.state.settle_claude_frame(&fixture.session, &fixture.token, &work);
        }
        let (start, registration, request, results) = fixture.finished();
        let body = engram_verification_record_body(&registration, &request, &results).unwrap();
        let expected = match case {
            "killed" | "missing" => EngramVerificationRecordRunOutcome::Indeterminate,
            "failing" => EngramVerificationRecordRunOutcome::Failed,
            _ => EngramVerificationRecordRunOutcome::Passed,
        };
        assert_eq!(body.run.outcome, expected, "{case}");
        assert_eq!(results["interrupted"], case == "killed", "{case}: only actual interruption");
        assert!(matches!(engram_verification_record_prepare(&registration), EngramVerificationRecordPrepared::Ready(_)), "{case}");
        let inner = fixture.fixture.state.inner.lock().unwrap();
        let row = &inner.test_run_observations[&start.execution_id];
        assert!(row.summary.ended_at.is_some(), "{case}");
        assert_eq!(row.summary.state == TestRunState::Passed, expected == EngramVerificationRecordRunOutcome::Passed, "{case}");
    }
}

#[test]
fn native_background_task_uses_its_original_reservation_after_a_later_prompt() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for interrupted in [false, true] {
        let fixture = NativeFixture::new("native-task-end", true);
        fixture.held_revision(4);
        fixture.prepare();
        let mut recorder = fixture.recorder();
        fixture.start(&mut recorder);
        let start = fixture.wait();
        handle_claude_tool_result(&json!({"tool_use_result":{"backgroundTaskId":"task"},
            "message":{"content":[{"type":"tool_result","tool_use_id":"native-tool","is_error":false,"content":""}]}}),
            &mut fixture.claude_state.lock().unwrap(), &mut recorder).unwrap();
        assert!(fixture.execution().unwrap().lock().unwrap().end.is_none(), "launch alone has no artifact END");
        {
            let mut inner = fixture.fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.session).unwrap();
            inner.sessions[index].active_turn_generation += 1;
            inner.sessions[index].engram.declared_run_turn = None;
        }
        fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
        let work = claude_frame_work(&json!({"type":"system","subtype":"task_notification",
            "status":if interrupted {"killed"} else {"completed"},"tool_use_id":"native-tool"}), false);
        fixture.fixture.state.settle_claude_frame(&fixture.session, &fixture.token, &work);
        let deadline = std::time::Instant::now() + crate::TEST_PHASE_DEADLOCK_GUARD;
        let registration = loop {
            let current = fixture.fixture.state.inner.lock().unwrap().test_run_registrations[&start.run_id].clone();
            if let TestRunJson::Parsed(results, _, _) = test_run_read_json(&PathBuf::from(&start.run_directory).join("results.json"))
                && engram_verification_record_results_are_terminal(&current, &results) { break current; }
            assert!(std::time::Instant::now() < deadline, "original task terminal was not frozen");
            std::thread::sleep(Duration::from_millis(5));
        };
        let request = serde_json::from_slice(&fs::read(PathBuf::from(&start.run_directory).join("request.json")).unwrap()).unwrap();
        let results = serde_json::from_slice(&fs::read(PathBuf::from(&start.run_directory).join("results.json")).unwrap()).unwrap();
        let body = engram_verification_record_body(&registration, &request, &results).unwrap();
        assert_eq!(body.run.outcome, if interrupted { EngramVerificationRecordRunOutcome::Indeterminate } else { EngramVerificationRecordRunOutcome::Passed });
        assert_eq!(registration.declared_native.as_ref(), Some(&start));
        let inner = fixture.fixture.state.inner.lock().unwrap();
        let row = &inner.test_run_observations[&start.execution_id];
        assert!(row.summary.error.is_none(), "a valid native artifact must not receive the no-verdict task notice");
        assert_eq!(row.summary.state == TestRunState::Passed, !interrupted);
    }
}

#[test]
fn native_background_ack_with_a_complete_artifact_waits_for_after_run_source() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for background in [false, true] {
        for changed in [false, true] {
            let fixture = NativeFixture::new("native-background-after-source", true);
            fixture.held_revision(4);
            fixture.prepare();
            let captures = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let observed = captures.clone();
            let owner = fixture.source_owner.clone();
            fixture.fixture.state.inner.lock().unwrap().test_declared_run_source = Some(Arc::new(move |binding, _, deadline| {
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                owner.source(binding, deadline)
            }));
            let mut recorder = fixture.recorder();
            let mut frame = fixture.frame();
            if background { frame["input"]["run_in_background"] = json!(true); }
            register_claude_tool_use(&frame, &mut fixture.claude_state.lock().unwrap(), &mut recorder).unwrap();
            let start = fixture.wait();
            fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
            handle_claude_tool_result(&json!({"tool_use_result":if background { json!({}) } else { json!({"backgroundTaskId":"task"}) },
                "message":{"content":[{"type":"tool_result","tool_use_id":"native-tool","is_error":false,"content":""}]}}),
                &mut fixture.claude_state.lock().unwrap(), &mut recorder).unwrap();
            assert_eq!(captures.load(std::sync::atomic::Ordering::SeqCst), 0, "launch ACK cannot capture after-run source");
            assert!(fixture.execution().unwrap().lock().unwrap().end.is_none());
            assert!(fixture.fixture.state.inner.lock().unwrap().test_run_registrations[&start.run_id]
                .declared_terminal.is_none(), "complete artifact alone cannot publish a terminal");
            if changed {
                fs::write(fixture.fixture.root.join("during-background.txt"), "changed after launch ACK\n").unwrap();
                fixture.record_change("during-background.txt", "??");
            }
            fixture.task_end();
            let (registration, request, results) = fixture.original_results(&start);
            assert_eq!(captures.load(std::sync::atomic::Ordering::SeqCst), 1);
            let body = engram_verification_record_body(&registration, &request, &results);
            if changed { assert!(body.is_err(), "source changed before original task completion"); }
            else { assert_eq!(body.unwrap().run.outcome, EngramVerificationRecordRunOutcome::Passed); }
        }
    }
}

#[test]
fn native_task_completion_after_a_later_prompt_preserves_original_refusal_and_failure() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for capacity_failure in [false, true] {
        let fixture = NativeFixture::new("native-task-original-refusal", true);
        fixture.held_revision(4);
        fixture.prepare();
        let slots = fixture.fixture.state.inner.lock().unwrap().engram_turn_basis_captures_live.clone();
        let mut recorder = fixture.recorder();
        if capacity_failure { slots.fetch_add(ENGRAM_TURN_BASIS_CAPTURE_LIMIT, std::sync::atomic::Ordering::SeqCst); }
        fixture.start(&mut recorder);
        if capacity_failure { slots.fetch_sub(ENGRAM_TURN_BASIS_CAPTURE_LIMIT, std::sync::atomic::Ordering::SeqCst); }
        else { fixture.wait(); }
        let execution = fixture.execution().unwrap();
        let start = execution.lock().unwrap().reservation.clone();
        if !capacity_failure {
            let mut contradictory = fixture.frame();
            contradictory["input"]["command"] = json!("dotnet test --different-command");
            recorder.native_tool_start(Some("native-tool"), true, &contradictory).unwrap();
        }
        handle_claude_tool_result(&json!({"tool_use_result":{"backgroundTaskId":"task"},
            "message":{"content":[{"type":"tool_result","tool_use_id":"native-tool","is_error":false,"content":""}]}}),
            &mut fixture.claude_state.lock().unwrap(), &mut recorder).unwrap();
        fixture.later_prompt();
        fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
        fixture.task_end();
        let job = execution.lock().unwrap();
        assert!(if capacity_failure { job.failure.is_some() } else { job.verification_refusal.is_some() });
        assert!(!job.worker_active, "task END must not start a replacement worker for a refused execution");
        drop(job);
        let inner = fixture.fixture.state.inner.lock().unwrap();
        let row = &inner.test_run_observations[&start.execution_id];
        assert!(row.summary.ended_at.is_some(), "the actual task END remains observed");
        assert!(inner.test_run_registrations.get(&start.run_id)
            .is_none_or(|registration| registration.declared_terminal.is_none()),
            "a later prompt cannot revive native credit");
        if capacity_failure { assert!(!inner.test_run_registrations.contains_key(&start.run_id)); }
    }
}

#[test]
fn native_task_end_during_pending_registration_keeps_the_original_worker_across_prompts() {
    struct Release(Option<std::sync::mpsc::Sender<()>>);
    impl Drop for Release {
        fn drop(&mut self) { if let Some(sender) = self.0.take() { let _ = sender.send(()); } }
    }
    let fixture = NativeFixture::new("native-task-pending-registration", true);
    fixture.held_revision(4);
    fixture.prepare();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let mut release = Release(Some(release_tx));
    let release_rx = Arc::new(Mutex::new(release_rx));
    let blocked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    *fixture.fixture.state.capture_lifetime.before_capture.lock().unwrap() = Some(Arc::new(move || {
        if std::thread::current().name() == Some("native-declared-run")
            && !blocked.swap(true, std::sync::atomic::Ordering::SeqCst) {
            ready_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv_timeout(crate::TEST_PHASE_DEADLOCK_GUARD).unwrap();
        }
    }));
    let mut recorder = fixture.recorder();
    fixture.start(&mut recorder);
    ready_rx.recv_timeout(crate::TEST_PHASE_DEADLOCK_GUARD).unwrap();
    let execution = fixture.execution().unwrap();
    let start = execution.lock().unwrap().reservation.clone();
    fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
    handle_claude_tool_result(&json!({"tool_use_result":{"backgroundTaskId":"task"},
        "message":{"content":[{"type":"tool_result","tool_use_id":"native-tool","is_error":false,"content":""}]}}),
        &mut fixture.claude_state.lock().unwrap(), &mut recorder).unwrap();
    fixture.later_prompt();
    fixture.task_end();
    {
        let job = execution.lock().unwrap();
        assert!(job.end.is_some(), "original task END freezes while registration waits");
        assert!(job.worker_active && !job.registered);
    }
    assert!(!fixture.fixture.state.inner.lock().unwrap().test_run_registrations.contains_key(&start.run_id));
    release.0.take().unwrap().send(()).unwrap();
    let (registration, request, results) = fixture.original_results(&start);
    assert_eq!(registration.declared_native.as_ref(), Some(&start));
    assert_eq!(engram_verification_record_body(&registration, &request, &results).unwrap().run.outcome,
        EngramVerificationRecordRunOutcome::Passed);
}

#[test]
fn native_declared_known_runners_and_prebuilt_options_use_their_artifact() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for command in ["cargo test", "dotnet test --no-build", "dotnet test bin/Tests.dll"] {
        let fixture = NativeFixture::new("native-declared-options", false);
        fs::write(fixture.fixture.root.join(ENGRAM_DECLARATION_FILE),
            format!("[[test]]\ncommand = {command:?}\ncwd = \".\"\nartifact = \".tmp/test.trx\"\n")).unwrap();
        run_git_test_command(&fixture.fixture.root, &["add", "--", ENGRAM_DECLARATION_FILE]);
        run_git_test_command(&fixture.fixture.root, &["-c","user.name=Fixture","-c","user.email=fixture@example.invalid", "commit","--quiet","--no-gpg-sign","-m","declared runner fixture"]);
        fixture.held_revision(4);
        fixture.prepare();
        let mut recorder = fixture.recorder();
        let mut frame = fixture.frame();
        frame["command"] = json!(command);
        recorder.native_tool_start(Some("native-tool"), true, &frame).unwrap();
        fixture.wait();
        fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
        recorder.command_completed_with_exit("native-tool", command, "", CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
        let (_, registration, request, results) = fixture.finished();
        assert_eq!(engram_verification_record_body(&registration, &request, &results).unwrap().run.outcome, EngramVerificationRecordRunOutcome::Passed, "{command}");
    }
}

#[test]
fn a_valid_native_artifact_larger_than_four_mib_retains_its_verdict_and_counts() {
    let fixture = NativeFixture::new("native-large-artifact", false);
    fixture.held_revision(4);
    fixture.prepare();
    let mut recorder = fixture.recorder();
    fixture.start(&mut recorder);
    fixture.wait();
    let artifact = include_str!("fixtures/declared-trx/pass.trx")
        .replace("</TestRun>", &format!("<!--{}--></TestRun>", "x".repeat(5 * 1024 * 1024)));
    assert!(artifact.len() > 4 * 1024 * 1024);
    fixture.artifact(&artifact);
    let (_, registration, request, results) = fixture.finish(&mut recorder, 0);
    let body = engram_verification_record_body(&registration, &request, &results).unwrap();
    assert_eq!(body.run.outcome, EngramVerificationRecordRunOutcome::Passed);
    assert!(body.checks[0].counts.as_ref().is_some_and(|counts| counts["passed"] > 0));
}
