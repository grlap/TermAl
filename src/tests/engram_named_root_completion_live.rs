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

#[path = "engram_source_observation_live.rs"]
mod source_observation;

struct CompletionFixture {
    live: LiveRootFixture,
    root: PathBuf,
    home: PathBuf,
    project_id: String,
}

impl CompletionFixture {
    fn command(&self, session_id: &str, args: &[&str], work: bool) -> Value {
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

    fn work(&self, session_id: &str, args: &[&str]) -> Value {
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

    fn canonical_read(
        &self,
        session: &str,
        binding: &EngramControlWorkBinding,
    ) -> EngramNamedRootReadResponse {
        let target = AppState::engram_binding_target_for_session_shape_locked(
            &self.live.state.inner.lock().unwrap(),
            session,
            true,
        )
        .unwrap()
        .unwrap();
        let read: EngramNamedRootReadResponse = parse_engram_result(
            self.live
                .transport
                .request(
                    &target.connection,
                    &EngramControlRequest::NamedRootRead {
                        routing_token: target.routing_token.clone().unwrap(),
                        run_id: binding.run_id.clone(),
                        claim_id: binding.claim_id.clone(),
                    },
                    target.settings.call_timeout(),
                )
                .expect("real canonical run is readable"),
        )
        .unwrap();
        read.validate_authority(
            target.settings.authority_store_key.as_ref().unwrap(),
            binding,
        )
        .unwrap();
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
        assert!(self.record(session_id, |record| {
            record.engram.active_turn_start_basis.is_some()
        }));
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

fn completion_fixture() -> CompletionFixture {
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
    }
    live.state
        .patch_project_engram_settings(
            &project_id,
            UpdateProjectEngramSettingsRequest {
                enabled: true,
                turn_gated_control: true,
                acceptance_evaluation: None,
                binary_path: Some(live_engram_binary().to_string_lossy().into_owned()),
                home: Some(home.to_string_lossy().into_owned()),
                deadline_ms: None,
            },
        )
        .expect("real readiness supplies the canonical store identity");
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
    let first_session = &fixture.live.session_id;
    let added = fixture.work(
        first_session,
        &[
            "add",
            "Check the recovered named worktree",
            "--accept",
            "A test passes in the named root",
            "--bind",
            "1=test",
            "--json",
        ],
    );
    let work_ref = added["work"]["short_ref"]
        .as_str()
        .or_else(|| added["short_ref"].as_str())
        .expect("work ref")
        .to_owned();
    fixture.work(first_session, &["claim", &work_ref, "--json"]);
    fixture.begin(first_session, "Make the earlier source change.");
    for state in ["claimed"] {
        assert!(
            fixture
                .live
                .transport
                .root_reads
                .lock()
                .unwrap()
                .iter()
                .any(|read| {
                    read.run.state == state
                        && read.named_root == EngramNamedRootState::None
                        && read.latest_event.is_none()
                }),
            "the real producer proves first-contact absence while {state}"
        );
    }
    let initial_binding = fixture.record(first_session, |record| {
        record.engram.work_binding.clone().unwrap()
    });
    fs::write(fixture.root.join("README.md"), "earlier changed content\n").unwrap();
    finish_live_root(&fixture.live.state, first_session);
    // A work checkpoint activates the canonical run. A control-turn checkpoint
    // closes the grant and stores observations, but does not activate that run.
    fixture.work(
        first_session,
        &[
            "note",
            &work_ref,
            "Earlier source change recorded before releasing this run",
            "--status",
            "--json",
        ],
    );
    let active = fixture.canonical_read(first_session, &initial_binding);
    assert_eq!(active.run.state, "active");
    assert_eq!(active.named_root, EngramNamedRootState::None);
    assert!(active.latest_event.is_none());
    let checkpoints = fixture.live.transport.requests_for("turn_checkpoint");
    let old = checkpoints.last().expect("old turn checkpoint");
    let old_observation = old["observations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|observation| observation["source_changed"] == true)
        .unwrap_or_else(|| panic!("the run retains a real earlier mutation: {old:#}"));
    let old_basis = old_observation["source_basis"].clone();
    assert!(
        old_basis["source_root_generation"].is_null(),
        "{old_basis:#}"
    );
    fixture.work(
        first_session,
        &[
            "update",
            &work_ref,
            "--release",
            "--reason",
            "Continue in a recovered session",
            "--json",
        ],
    );

    // New content is committed before the recovered session names its root.
    fs::write(
        fixture.root.join("README.md"),
        "newer content before naming\n",
    )
    .unwrap();
    run_git_test_command(&fixture.root, &["add", "README.md"]);
    run_git_test_command(
        &fixture.root,
        &["commit", "--quiet", "-m", "newer fixture content"],
    );
    let worktree = fixture.root.join(".worktrees").join("new-root");
    run_git_test_command(
        &fixture.root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "new-root",
            worktree.to_str().unwrap(),
        ],
    );
    let second_session = create_test_project_session(
        &fixture.live.state,
        Agent::Codex,
        &fixture.project_id,
        &fixture.root,
    );
    let _cleanup = LiveRootCleanup {
        transport: fixture.live.transport.clone(),
        session_id: second_session.clone(),
    };
    fixture.work(
        &second_session,
        &[
            "claim",
            &work_ref,
            "--recover",
            "Recover the released run at newer content",
            "--json",
        ],
    );
    fixture
        .live
        .state
        .ensure_engram_session_bound_off_lock(&second_session)
        .expect("recovered session binds")
        .expect("binding is enabled");
    let recovered = fixture.record(&second_session, |record| {
        record.engram.work_binding.clone().unwrap()
    });
    assert_eq!(
        recovered.run_id, initial_binding.run_id,
        "recovery preserves the old mutation's run"
    );
    assert_eq!(recovered.claim_id, initial_binding.claim_id);
    assert!(recovered.claim_fence > initial_binding.claim_fence);
    let named = fixture
        .live
        .state
        .name_engram_source_root(
            &second_session,
            EngramSourceRootRequest {
                work: work_ref.clone(),
                path: Some(Some(worktree.to_string_lossy().into_owned())),
            },
        )
        .expect("the production host names the real canonical run");
    assert!(named.generation > 0);
    // Release reopens the run and recovery claims it again. Checkpoint the
    // recovered holder before exercising its active named-root admission.
    fixture.work(
        &second_session,
        &[
            "note",
            &work_ref,
            "Recovered holder will check the named root without an edit",
            "--status",
            "--json",
        ],
    );
    let revision = named.source_revision.expect("named content measured");
    assert_ne!(old_basis["source_revision"], revision);
    let before = content_revision(&worktree)
        .expect("new worktree measures")
        .1;
    assert_eq!(before, revision);
    fixture.begin(&second_session, "Run the check without changing source.");
    assert!(fixture.live.transport.root_reads.lock().unwrap().iter().any(|read| {
        read.run.state == "active" && matches!(&read.named_root,
            EngramNamedRootState::Bound { generation, .. } if *generation == named.generation as i64)
            && read.latest_event.as_ref().is_some_and(|event| event.kind == EngramNamedRootKind::Bound)
    }), "the real active run confirms the named generation and Bound event");
    let target = AppState::engram_binding_target_for_session_shape_locked(
        &fixture.live.state.inner.lock().unwrap(),
        &second_session,
        true,
    )
    .unwrap()
    .unwrap();
    let cwd = fs::canonicalize(&worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut recorder = SessionRecorder::new(fixture.live.state.clone(), second_session.clone());
    recorder
        .command_started_in(
            "named-test",
            "cargo test --offline",
            Some("cargo test --offline"),
            Some(&cwd),
        )
        .unwrap();
    fixture.wait_for_check_capture(&second_session);
    let output = Command::new("cargo")
        .args(["test", "--offline"])
        .current_dir(&worktree)
        .output()
        .expect("real test launches");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("1 passed"), "{text}");
    recorder
        .command_completed_with_exit(
            "named-test",
            "cargo test --offline",
            &text,
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .unwrap();
    fixture.wait_for_check_capture(&second_session);
    finish_live_root(&fixture.live.state, &second_session);
    assert_eq!(
        content_revision(&worktree).unwrap().1,
        before,
        "the check did not edit source"
    );
    let checkpoints = fixture.live.transport.requests_for("turn_checkpoint");
    let checked = checkpoints.last().expect("new root checkpoint");
    assert_eq!(
        checked["verification_evidence"].as_array().map(Vec::len),
        Some(1),
        "{checked:#}"
    );
    let observations = checked["observations"].as_array().unwrap();
    assert!(
        observations
            .iter()
            .all(|observation| observation["source_changed"] == false),
        "no synthetic edit: {checked:#}"
    );
    let producer = observations
        .iter()
        .find(|observation| {
            observation["outcome"] == "succeeded"
                && observation["source_basis"]["source_revision"] == revision
        })
        .expect("check producer has the new basis");
    let basis = &producer["source_basis"];
    assert_eq!(basis["source_root_generation"], named.generation);
    assert_eq!(basis["source_root_state"], "named");
    assert_ne!(basis["workspace_id"], old_basis["workspace_id"]);
    let shown = fixture.work(&second_session, &["show", &work_ref, "--notes", "--json"]);
    let mut rows = Vec::new();
    objects_with_key(&shown, "verification", &mut rows);
    assert_eq!(rows.len(), 1, "one store-minted check: {shown:#}");
    assert_eq!(rows[0]["verification"]["result"], "passed");
    assert_eq!(rows[0]["verification"]["source_revision"], revision);
    let verification_id = rows[0]["locator"].as_str().unwrap();
    let stored = fixture.object(verification_id);
    eprintln!("real named-root verification: {stored:#}");
    assert_eq!(
        stored["source_basis"], *basis,
        "Engram retained the host's named basis"
    );
    let shown = fixture.work(&second_session, &["show", &work_ref, "--json"]);
    let acceptance = shown["acceptance_basis"]
        .as_i64()
        .expect("acceptance basis")
        .to_string();
    let evidence = shown["evidence_basis"]
        .as_i64()
        .expect("evidence cut")
        .to_string();
    fixture.work(
        &second_session,
        &[
            "evaluate",
            &work_ref,
            "--mode",
            "same-session",
            "--acceptance-basis",
            &acceptance,
            "--evidence-basis",
            &evidence,
            "--verdict",
            "1=pass:observed",
            "--rationale",
            "1=Real cargo test in the recovered named root passed without an edit",
            "--evidence",
            &format!("1={verification_id}"),
            "--source-fingerprint",
            &revision,
            "--attempt",
            "named-completion",
            "--json",
        ],
    );
    let done = fixture.work(
        &second_session,
        &[
            "done",
            &work_ref,
            "Named-root check passed without an edit",
            "--source-fingerprint",
            &revision,
            "--json",
        ],
    );
    eprintln!("real recovered named-root completion: {done:#}");
    let final_show = fixture.work(&second_session, &["show", &work_ref, "--json"]);
    assert_eq!(
        final_show["status"]["work"]["lifecycle"], "completed",
        "{final_show:#}"
    );
    assert_eq!(content_revision(&worktree).unwrap().1, before);
    let terminal: EngramNamedRootReadResponse = parse_engram_result(
        fixture
            .live
            .transport
            .request(
                &target.connection,
                &EngramControlRequest::NamedRootRead {
                    routing_token: target.routing_token.clone().unwrap(),
                    run_id: recovered.run_id.clone(),
                    claim_id: recovered.claim_id.clone(),
                },
                target.settings.call_timeout(),
            )
            .expect("completed canonical run is readable"),
    )
    .unwrap();
    assert_eq!(terminal.run.state, "completed");
    assert_eq!(terminal.named_root, EngramNamedRootState::None);
    assert!(
        terminal.latest_event.is_some(),
        "completion retains naming history"
    );
    terminal
        .validate_authority(
            target.settings.authority_store_key.as_ref().unwrap(),
            &recovered,
        )
        .unwrap();
}
