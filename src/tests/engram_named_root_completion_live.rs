//! A recovered run keeps its earlier mutation, but a check in a newly named
//! worktree must be attributed to that root without manufacturing another edit.
//! Uses the production host transport and a pinned Engram disposable store.
use super::*;

#[path = "engram_legacy_root_recovery_live.rs"]
mod legacy_recovery;

#[path = "engram_source_root_confirmation_live.rs"]
mod selection_confirmation;

#[path = "engram_named_root_sighting_live.rs"]
mod sighting_live;

#[path = "acceptance_evaluation_refresh_live.rs"]
mod acceptance_refresh_live;

#[path = "engram_acceptance_sub_agent_live.rs"]
mod sub_agent_live;

#[path = "engram_source_observation_live.rs"]
mod source_observation;

#[path = "engram_build_checks_live.rs"]
mod build_checks;

#[path = "test_run_registration_revision_live.rs"]
mod registration_revision;

pub(super) struct CompletionFixture {
    pub(super) live: LiveRootFixture,
    pub(super) root: PathBuf,
    pub(super) home: PathBuf,
    pub(super) project_id: String,
}

impl CompletionFixture {
    pub(super) fn command(&self, session_id: &str, args: &[&str], work: bool) -> Value {
        let (actor_id, actor_context) = {
            let inner = self.live.state.inner.lock().expect("state mutex poisoned");
            engram_runtime_actor_identity(
                &inner.preferences.engram.developer_name,
                &inner.sessions[inner
                    .find_session_index(session_id)
                    .expect("session exists")],
            )
        };
        let mut command = Command::new(live_engram_binary());
        command
            .arg("--project-file")
            .arg(self.root.join(".engram-project"))
            .arg("--home")
            .arg(&self.home);
        if work {
            command.args(["work", "--actor-id", &actor_id, "--session-id", session_id]);
            if let Some(context) = actor_context.as_deref() {
                command.args(["--actor-context", context]);
            }
        }
        let output = command.args(args).output().expect("Engram launches");
        assert!(
            output.status.success(),
            "Engram {args:?} failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "Engram {args:?}: {error}: {}",
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }

    pub(super) fn work(&self, session_id: &str, args: &[&str]) -> Value {
        self.command(session_id, args, true)
    }

    fn record<T>(&self, session_id: &str, read: impl FnOnce(&SessionRecord) -> T) -> T {
        let inner = self.live.state.inner.lock().expect("state mutex poisoned");
        read(
            &inner.sessions[inner
                .find_session_index(session_id)
                .expect("session exists")],
        )
    }

    pub(super) fn store(&self) -> EngramAuthorityStoreKey {
        self.live.state.inner.lock().unwrap().find_project(&self.project_id)
            .unwrap().engram.as_ref().unwrap().authority_store_key.clone().unwrap()
    }

    pub(super) fn target(&self, session: &str) -> EngramBindingTarget {
        AppState::engram_source_root_target_locked(&self.live.state.inner.lock().unwrap(), session)
            .unwrap().expect("the ordinary work service is configured")
    }

    pub(super) fn claim_binding(&self, session: &str) -> EngramControlWorkBinding {
        let target = self.target(session);
        target.adapter.read_work_session(&target.connection, &self.store(), target.settings.call_timeout())
            .expect("the actual ordinary producer identifies this session");
        target.adapter.read_work_binding(&target.connection, EngramBindingPreference { current: None },
            target.settings.call_timeout()).unwrap().expect("the real session holds a claim")
    }

    fn canonical_read(&self, session: &str, binding: &EngramControlWorkBinding) -> EngramNamedRootReadResponse {
        let target = self.target(session);
        let read: EngramNamedRootReadResponse = parse_engram_result(target.adapter.request(
            &target.connection, &EngramControlRequest::NamedRootRead {
                run_id: binding.run_id.clone(), claim_id: binding.claim_id.clone(),
            }, target.settings.call_timeout()).expect("real canonical run is readable")).unwrap();
        read.validate_authority(&self.store(), binding).unwrap();
        read
    }

    fn begin(&self, session_id: &str, prompt: &str) {
        let dispatch = dispatch_live_root(&self.live.state, session_id, prompt, None);
        deliver_turn_dispatch(&self.live.state, dispatch).expect("claimed prompt delivers");
        match self
            .live
            .receiver
            .try_recv()
            .expect("provider receives prompt")
        {
            CodexRuntimeCommand::Prompt {
                session_id: delivered,
                command,
            } => {
                assert_eq!(delivered, session_id);
                assert!(command.prompt.ends_with(prompt), "{}", command.prompt);
                assert!(
                    !command.prompt.contains("unconfirmed"),
                    "{}",
                    command.prompt
                );
            }
            _ => panic!("expected a provider prompt"),
        }
        assert!(self.live.receiver.try_recv().is_err());
        assert!(self.record(session_id, |record| record.engram.unadmitted_active_turn.is_some()));
    }

    fn wait_for_check_capture(&self, session_id: &str) {
        let captures = self.record(session_id, |record| {
            record
                .engram
                .active_turn_checks
                .iter()
                .flat_map(|check| {
                    std::iter::once(check.start_basis.clone())
                        .chain(check.end.as_ref().map(|end| end.end_basis.clone()))
                })
                .collect::<Vec<_>>()
        });
        assert!(!captures.is_empty(), "the real check is recognized");
        for capture in captures {
            capture.wait_until(std::time::Instant::now() + DEADLOCK_GUARD);
        }
    }

    fn prepare_registered_check(&self, session: &str, root: &FsPath, command: &str, observe: bool)
        -> (TestRunRegistration, EngramVerificationRecordFrozen)
    {
        let argv = engram_shell_words(command).unwrap();
        assert_eq!(argv.first().map(String::as_str), Some("cargo"));
        let clean_before = run_git_test_command_output(root, &["status", "--porcelain"]).is_empty();
        assert!(clean_before, "registered native checks start on a clean commit");
        let commit = run_git_test_command_output(root, &["rev-parse", "HEAD"]).trim().to_owned();
        let run_id = format!("test-live-{}", Uuid::new_v4().simple());
        let runs = PathBuf::from(run_git_test_command_output(root,
            &["rev-parse", "--path-format=absolute", "--git-path", "review-runs"]).trim());
        let directory = runs.join(&run_id);
        fs::create_dir_all(&directory).unwrap();
        let started = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let request = json!({
            "runId": run_id, "root": fs::canonicalize(root).unwrap().to_string_lossy(),
            "owner": session, "creatorPid": std::process::id(), "detached": false,
            "input": "commit", "full": false, "started": started,
            "command": argv, "stages": [{ "name": "focused", "command": argv[0], "args": &argv[1..] }],
        });
        fs::write(directory.join("request.json"), serde_json::to_vec(&request).unwrap()).unwrap();
        self.live.state.register_test_run(session, RegisterTestRunRequest {
            run_id: run_id.clone(), run_directory: directory.to_string_lossy().into_owned(),
        }).unwrap();
        let registration = self.live.state.inner.lock().unwrap().test_run_registrations[&run_id].clone();
        assert_eq!(registration.work_binding.as_ref(), Some(&self.claim_binding(session)));
        assert_eq!(registration.authority_store.as_ref(), Some(&self.store()));
        assert!(registration.named_root.is_some(), "the actual naming owner pins the registered root");
        let mut recorder = observe.then(|| SessionRecorder::new(self.live.state.clone(), session.to_owned()));
        if let Some(recorder) = &mut recorder {
            recorder.command_started_in(&run_id, command, Some(command), Some(&root.to_string_lossy())).unwrap();
            self.wait_for_check_capture(session);
        }
        let stage_started = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let output = Command::new(&argv[0]).args(&argv[1..]).current_dir(root)
            .env("CARGO_TARGET_DIR", root.join("target")).output().expect("actual native Cargo launches");
        let text = format!("{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        fs::write(directory.join("focused.log"), &text).unwrap();
        let code = output.status.code().expect("native command exit");
        if command.contains(" test") {
            assert!(code == 0 && text.contains("1 passed"), "the real embedded checked_content test passed: {text}");
        }
        let ended = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let after_commit = run_git_test_command_output(root, &["rev-parse", "HEAD"]).trim().to_owned();
        let dirty = run_git_test_command_output(root, &["status", "--porcelain"]);
        assert_eq!(after_commit, commit);
        assert!(dirty.is_empty(), "the real native check leaves source unchanged: {dirty}");
        let results = json!({
            "runId": run_id, "state": if code == 0 { "passed" } else { "failed" },
            "exitCode": code, "started": started, "ended": ended,
            "stages": [{ "name": "focused", "state": if code == 0 { "passed" } else { "failed" },
                "code": code, "command": argv, "started": stage_started, "ended": ended }],
            "input": { "kind": "commit", "commit": commit, "cleanBefore": clean_before,
                "after": { "commit": after_commit, "clean": true, "paths": [] },
                "outcome": "clean", "eligible": true },
        });
        fs::write(directory.join("results.json"), serde_json::to_vec(&results).unwrap()).unwrap();
        if let Some(recorder) = &mut recorder {
            recorder.command_completed_with_exit(&run_id, command, &text,
                if code == 0 { CommandStatus::Success } else { CommandStatus::Error },
                EngramCommandExit::Code(i64::from(code))).unwrap();
            self.wait_for_check_capture(session);
        }
        let EngramVerificationRecordPrepared::Ready(frozen) = engram_verification_record_prepare(&registration)
            else { panic!("the real registered artifact must produce a native verification body"); };
        (registration, frozen)
    }

    fn registered_check(&self, session: &str, root: &FsPath, command: &str)
        -> (EngramVerificationRecordBody, EngramVerificationRecordReceipt)
    {
        let (registration, frozen) = self.prepare_registered_check(session, root, command, true);
        let run_id = registration.run_id.clone();
        let route = self.live.state.engram_verification_record_route(&registration).unwrap();
        let sent = engram_verification_record_send(frozen.clone(), Ok(route.link()));
        let EngramVerificationRecordStep::Received { body, receipt } = &sent
            else { panic!("actual producer must answer the registered native check: {sent:?}"); };
        self.live.state.note_engram_verification_record_step(&registration, &sent);
        let loaded = load_state(self.live.state.persistence_path.as_path()).unwrap().unwrap();
        assert!(matches!(&loaded.test_run_registrations[&run_id].verification_record,
            Some(TestRunVerificationAck::Received { receipt: saved, .. }) if saved == receipt),
            "the actual receipt is durably retained on the registered run");
        let duplicate = engram_verification_record_send(frozen, Ok(route.link()));
        let EngramVerificationRecordStep::Received { body: again, receipt: replay } = duplicate
            else { panic!("exact native record replay returns its original receipt"); };
        assert_eq!(&again, body);
        assert_eq!(serde_json::to_value(&replay).unwrap(), serde_json::to_value(receipt).unwrap());
        assert_eq!(receipt.verifications.len(), 1);
        let positive = self.object(&receipt.verifications[0].record_id);
        for mismatch in ["dirty-input", "claim-fence", "same-key-different-body"] {
            let mut refused = body.clone();
            if mismatch != "same-key-different-body" {
                refused.registration.host_run_id = format!("{}-{mismatch}", body.registration.host_run_id);
            }
            match mismatch {
                "dirty-input" => refused.post_run.clean_before = false,
                "claim-fence" => refused.registration.binding.claim_fence += 1,
                _ => refused.run.command.push_str(" --changed-body"),
            }
            let request = EngramControlRequest::VerificationRecord {
                operation_key: engram_verification_record_key(&refused.registration.host_run_id),
                record: serde_json::to_value(&refused).unwrap(),
            };
            let target = self.target(session);
            let error = target.adapter.request(&target.connection, &request, target.settings.call_timeout())
                .expect_err("the real producer rejects invalid input, claim scope or an idempotency collision");
            assert_eq!(error.kind, EngramTransportErrorKind::Remote, "{mismatch}: {error:?}");
            assert_eq!(self.object(&receipt.verifications[0].record_id), positive);
        }

        assert_no_live_turn_gate(&self.live.transport);
        (body.clone(), receipt.clone())
    }

    fn wait_for_reports(&self, session: &str) {
        self.live.state.start_runtime_observation_worker(session);
        let guard = phase_sync::PollGuard::new();
        loop {
            let done = self.record(session, |record| {
                record.engram.unadmitted_closing_jobs.is_empty()
                    && record.engram.unadmitted_reports.iter().all(|report| report.is_settled())
                    && !record.engram.unadmitted_worker_running
            });
            if done {
                self.record(session, |record| {
                    for report in &record.engram.unadmitted_reports {
                        for envelope in &report.envelopes {
                            assert!(matches!(envelope.delivery, EngramRuntimeObservationDelivery::Recorded { .. }),
                                "a real positive witness requires an acknowledged producer receipt: {:?}", envelope.delivery);
                        }
                    }
                });
                break;
            }
            guard.wait("actual ordinary observation worker completes the persisted report");
        }
    }

    // Read only this fixture's disposable store, never an operator's live DB.
    fn object(&self, id: &str) -> Value {
        let database = {
            let inner = self.live.state.inner.lock().expect("state mutex poisoned");
            inner
                .find_project(&self.project_id)
                .unwrap()
                .engram
                .as_ref()
                .unwrap()
                .authority_store_key
                .as_ref()
                .unwrap()
                .database_path
                .clone()
        };
        assert!(database.starts_with(&self.home));
        let connection = rusqlite::Connection::open_with_flags(
            database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .expect("disposable store opens read-only");
        let bytes: Vec<u8> = connection
            .query_row(
                "SELECT canonical_json FROM objects WHERE object_id = ?1",
                [id],
                |row| row.get(0),
            )
            .expect("Engram retained the canonical record");
        serde_json::from_slice(&bytes).expect("canonical object is JSON")
    }
}

pub(super) fn completion_fixture() -> CompletionFixture {
    let suffix = "recovered-named-completion";
    let live = live_root_fixture(suffix, []);
    let base = live.state.test_temp_root.as_ref().unwrap().path();
    let root = base.join(format!("live-root-project-{suffix}"));
    let home = base.join(format!("live-root-home-{suffix}"));
    let project_id = {
        let inner = live.state.inner.lock().expect("state mutex poisoned");
        inner.sessions[inner.find_session_index(&live.session_id).unwrap()]
            .session
            .project_id
            .clone()
            .unwrap()
    };
    run_git_test_command(&root, &["init", "--quiet"]);
    run_git_test_command(&root, &["config", "user.email", "termal-tests@example.com"]);
    run_git_test_command(&root, &["config", "user.name", "TermAl tests"]);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"named-root-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "#[test]\nfn checked_content() { assert_eq!(2 + 2, 4); }\n",
    )
    .unwrap();
    fs::write(root.join("README.md"), "initial content\n").unwrap();
    fs::write(
        root.join(".gitignore"),
        "target/\nCargo.lock\n.worktrees/\n",
    )
    .unwrap();
    run_git_test_command(
        &root,
        &[
            "add",
            "Cargo.toml",
            "src/lib.rs",
            "README.md",
            ".gitignore",
            ".engram-project",
        ],
    );
    run_git_test_command(&root, &["commit", "--quiet", "-m", "initial fixture"]);
    {
        let mut inner = live.state.inner.lock().expect("state mutex poisoned");
        inner.preferences.engram.binary_path = live_engram_binary().to_string_lossy().into_owned();
        inner.preferences.engram.home = home.to_string_lossy().into_owned();
        // The prompt fixture installs settings without learning the store.
        // Start unconfigured so the real enable path must run readiness.
        inner
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
            .expect("completion project exists")
            .engram = None;
    }
    live.state
        .patch_project_engram_settings(
            &project_id,
            UpdateProjectEngramSettingsRequest {
                enabled: true,
                execution_observability: Some(true),
                acceptance_evaluation: None,
                binary_path: Some(live_engram_binary().to_string_lossy().into_owned()),
                home: Some(home.to_string_lossy().into_owned()),
                deadline_ms: None,
            },
        )
        .expect("real readiness supplies the canonical store identity");
    {
        let mut inner = live.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&live.session_id)
            .expect("completion session exists");
        // Enabling invalidates orientation; these witnesses isolate handoff
        // from the optional context page, as the prompt fixture does.
        assert!(!inner.sessions[index].engram.context_nudge_in_progress);
        inner.sessions[index].engram.pending_context_nudge = None;
        inner.sessions[index].engram.context_nudge_pending = false;
    }
    let fixture = CompletionFixture {
        live,
        root,
        home,
        project_id,
    };
    fixture.command(
        &fixture.live.session_id,
        &[
            "control-policy",
            "set-acceptance-evaluation",
            "--modes",
            "same-session",
            "--mechanical-basis",
            "observed",
            "--require-source-freshness",
            "--authorized-by",
            "disposable-host-completion-test",
            "--idempotency-key",
            "completion-policy",
        ],
        false,
    );
    fixture
}

fn objects_with_key<'a>(value: &'a Value, key: &str, found: &mut Vec<&'a Value>) {
    match value {
        Value::Object(map) => {
            if map.contains_key(key) {
                found.push(value);
            }
            for child in map.values() {
                objects_with_key(child, key, found);
            }
        }
        Value::Array(values) => {
            for child in values {
                objects_with_key(child, key, found);
            }
        }
        _ => {}
    }
}


#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn recovered_claim_check_in_a_newly_named_root_completes_without_an_edit() {
    let fixture = completion_fixture();
    let first = &fixture.live.session_id;
    let added = fixture.work(first, &["add", "Check the recovered named worktree",
        "--accept", "A test passes in the named root", "--bind", "1=test", "--json"]);
    let work = added["work"]["short_ref"].as_str().or_else(|| added["short_ref"].as_str()).unwrap();
    fixture.work(first, &["claim", work, "--json"]);
    let original = fixture.claim_binding(first);
    let unnamed = fixture.canonical_read(first, &original);
    assert_eq!(unnamed.named_root, EngramNamedRootState::None);
    assert!(unnamed.latest_event.is_none());
    fixture.begin(first, "Make the earlier source change.");
    fs::write(fixture.root.join("README.md"), "earlier changed content\n").unwrap();
    finish_live_root(&fixture.live.state, first);
    fixture.wait_for_reports(first);
    let reports = fixture.live.transport.requests_for("execution_observe");
    assert!(!reports.is_empty(), "the real producer receives the earlier ordinary mutation");
    let earlier = reports.clone();
    fixture.work(first, &["note", work, "Earlier source change retained before release", "--status", "--json"]);
    fixture.work(first, &["update", work, "--release", "--reason", "Continue in a recovered session", "--json"]);
    fs::write(fixture.root.join("README.md"), "newer content before naming\n").unwrap();
    run_git_test_command(&fixture.root, &["add", "README.md"]);
    run_git_test_command(&fixture.root, &["commit", "--quiet", "-m", "newer fixture content"]);
    let worktree = fixture.root.join(".worktrees/new-root");
    run_git_test_command(&fixture.root, &["worktree", "add", "--quiet", "-b", "new-root", worktree.to_str().unwrap()]);
    let second = create_test_project_session(&fixture.live.state, Agent::Codex, &fixture.project_id, &fixture.root);
    let _cleanup = LiveRootCleanup { transport: fixture.live.transport.clone(), session_id: second.clone() };
    fixture.work(&second, &["claim", work, "--recover", "Recover the released run at newer content", "--json"]);
    let recovered = fixture.claim_binding(&second);
    assert_eq!(recovered.run_id, original.run_id);
    assert_eq!(recovered.claim_id, original.claim_id);
    assert!(recovered.claim_fence > original.claim_fence);
    let named = fixture.live.state.name_engram_source_root(&second, EngramSourceRootRequest {
        work: work.to_owned(), path: Some(Some(worktree.to_string_lossy().into_owned())),
    }).unwrap();
    let bound = fixture.canonical_read(&second, &recovered);
    assert!(matches!(bound.named_root, EngramNamedRootState::Bound { generation, .. }
        if generation == named.generation as i64));
    fixture.work(&second, &["note", work, "Recovered holder checks the named root without an edit", "--status", "--json"]);
    let before = content_revision(&worktree).unwrap().1;
    fixture.begin(&second, "Run the real check without changing source.");
    let (body, receipt) = fixture.registered_check(&second, &worktree, "cargo test --offline");
    assert_eq!(body.registration.binding, recovered);
    assert_eq!(body.registration.root.generation, named.generation as i64);
    let stored = fixture.object(&receipt.verifications[0].record_id);
    assert_eq!(stored["source_basis"], serde_json::to_value(engram_verification_record_basis(&body).unwrap()).unwrap());
    assert_eq!(stored["check_kind"], "test");
    assert_eq!(content_revision(&worktree).unwrap().1, before);
    assert!(worktree.join(".git").is_file(), "naming never removes the worktree");
    for prior in earlier { assert!(fixture.live.transport.requests_for("execution_observe").contains(&prior)); }
    finish_live_root(&fixture.live.state, &second);
    fixture.wait_for_reports(&second);
    let shown = fixture.work(&second, &["show", work, "--json"]);
    let acceptance = shown["acceptance_basis"].as_i64().unwrap().to_string();
    let evidence = shown["evidence_basis"].as_i64().unwrap().to_string();
    let EngramVerificationRecordInput::Commit { commit } = &body.input else { panic!("the registered native witness requires committed input"); };
    let verification = &receipt.verifications[0].record_id;
    fixture.work(&second, &[
        "evaluate", work, "--mode", "same-session", "--acceptance-basis", &acceptance,
        "--evidence-basis", &evidence, "--verdict", "1=pass:observed", "--rationale",
        "1=Real registered cargo test in the recovered named root passed without an edit",
        "--evidence", &format!("1={verification}"), "--source-fingerprint", commit,
        "--attempt", "named-completion", "--json",
    ]);
    let evaluated = fixture.canonical_read(&second, &recovered);
    assert_eq!(evaluated.run.state, "active", "evaluation alone does not complete the run");
    assert!(matches!(evaluated.named_root, EngramNamedRootState::Bound { .. }));
    let retained = fixture.live.state.inner.lock().unwrap().engram_work_source_roots.iter()
        .find(|entry| entry.claim_id == recovered.claim_id).unwrap().clone();
    let plan = {
        let now = fixture.live.state.engram_budget_clock().now() + ENGRAM_ROOT_RECLAMATION_COOLDOWN;
        engram_root_reclamation_plan_locked(&fixture.live.state.inner.lock().unwrap(), now)
    };
    fixture.live.state.run_engram_root_reclamation_pass(plan);
    assert!(fixture.live.state.inner.lock().unwrap().engram_work_source_roots.contains(&retained));
    fixture.work(&second, &["done", work, "Named-root registered check passed without an edit",
        "--source-fingerprint", commit, "--json"]);
    let final_show = fixture.work(&second, &["show", work, "--json"]);
    assert_eq!(final_show["status"]["work"]["lifecycle"], "completed", "{final_show:#}");
    let terminal = fixture.canonical_read(&second, &recovered);
    assert_eq!(terminal.run.state, "completed");
    assert_eq!(terminal.named_root, EngramNamedRootState::None);
    assert!(terminal.latest_event.is_some(), "completion retains canonical naming history");
    let plan = {
        let now = fixture.live.state.engram_budget_clock().now() + ENGRAM_ROOT_RECLAMATION_COOLDOWN;
        engram_root_reclamation_plan_locked(&fixture.live.state.inner.lock().unwrap(), now)
    };
    assert!(plan.iter().any(|task| task.root == retained), "the real planner chooses canonical reconciliation");
    let reads_before = fixture.live.transport.root_reads.lock().unwrap().len();
    fixture.live.state.run_engram_root_reclamation_pass(plan);
    assert!(fixture.live.transport.root_reads.lock().unwrap()[reads_before..].iter()
        .any(|read| read.run_id == recovered.run_id && read.run.state == "completed"));
    let notice_id = {
        let inner = fixture.live.state.inner.lock().unwrap();
        assert!(!inner.engram_work_source_roots.contains(&retained));
        let notices = inner.engram_work_naming_history.iter().flat_map(|history| &history.retirements)
            .filter(|notice| notice.selection == retained).collect::<Vec<_>>();
        assert_eq!(notices.len(), 1);
        assert!(notices[0].published);
        assert_eq!(notices[0].binding, recovered);
        assert_eq!(notices[0].read.run.state, "completed");
        assert!(notices[0].line.contains("its run completed"));
        notices[0].id.clone()
    };
    let later = fixture.work(&second, &["add", "Continue after named completion", "--json"]);
    let later_work = later["work"]["short_ref"].as_str().or_else(|| later["short_ref"].as_str()).unwrap();
    fixture.work(&second, &["claim", later_work, "--json"]);
    fixture.live.state.name_engram_source_root(&second, EngramSourceRootRequest {
        work: later_work.to_owned(), path: Some(Some(worktree.to_string_lossy().into_owned())),
    }).unwrap();
    let inner = fixture.live.state.inner.lock().unwrap();
    let notices = inner.engram_work_naming_history.iter().flat_map(|history| &history.retirements)
        .filter(|notice| notice.selection == retained).collect::<Vec<_>>();
    assert_eq!(notices.len(), 1, "ordinary naming does not retire the completed selection twice");
    assert_eq!(notices[0].id, notice_id);
    assert!(notices[0].published);
    drop(inner);
    assert!(worktree.join(".git").is_file());
    assert_eq!(content_revision(&worktree).unwrap().1, before);
    assert_no_live_turn_gate(&fixture.live.transport);
}
