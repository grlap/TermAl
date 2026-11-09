// Closing source changes use actual recorder snapshots and native registration.
// Artifacts here are fixture data, not evidence that Cargo or a producer ran.
use super::*;

const COMMAND: &str = "node scripts/test-launcher.mjs focused --input commit -- cargo test --lib";

fn owner_envelope(directory: &FsPath) -> Value {
    let mut envelope = super::super::engram_source_command::source_envelope(directory, "clean");
    let commit = run_git_test_command_output(directory, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let tree = run_git_test_command_output(directory, &["rev-parse", "HEAD^{tree}"])
        .trim()
        .to_owned();
    assert!(
        run_git_test_command_output(directory, &["status", "--porcelain"]).is_empty(),
        "the scripted owner fixture only reports actual clean checkouts"
    );
    envelope["source"] = json!(format!("git-commit:{commit}"));
    envelope["tree"] = json!(tree);
    envelope["source_id"]["commit"] = json!(commit);
    envelope["source_id"]["tree"] = envelope["tree"].clone();
    envelope
}

struct OwnerCheckedTurn {
    turn: EOCheckedTurn,
    owner_reply: Arc<Mutex<Value>>,
}

impl std::ops::Deref for OwnerCheckedTurn {
    type Target = EOCheckedTurn;
    fn deref(&self) -> &EOCheckedTurn {
        &self.turn
    }
}

fn start_named(label: &str) -> OwnerCheckedTurn {
    let turn = EOCheckedTurn::prepare_in(label, true, None, true);
    let EngramRootCapture::Recorded {
        workspace_id,
        state: EngramSourceRootState::Named,
        ..
    } = turn.state.engram_root_capture(&turn.session_id)
    else {
        panic!("the idle fixture already owns its confirmed named root");
    };
    // Git is fixture setup, outside the caller's production capture deadline.
    let owner_reply = Arc::new(Mutex::new(owner_envelope(FsPath::new(&workspace_id))));
    let reply = owner_reply.clone();
    {
        let mut inner = turn.state.inner.lock().unwrap();
        // Keep production owner parsing, custody and scope rechecks. Only the
        // subprocess reply is scripted; the older content-v1 hook is disabled.
        inner.test_engram_source_capture = None;
        inner.test_observation_source_owner = Some(Arc::new(move |_, _, args, _, _| {
            if args[0] == "--version" {
                return Ok(super::super::engram_source_owner::source_owner_reply(args));
            }
            let envelope = reply.lock().unwrap();
            assert_eq!(
                envelope["root"],
                engram_exact_path_key(FsPath::new(&args[2])),
                "the scripted owner replies only for its measured named root"
            );
            Ok(super::super::engram_source_owner::source_owner_output(
                0,
                serde_json::to_vec(&*envelope).unwrap(),
            ))
        }));
    }
    turn.begin("Run the checks.");
    OwnerCheckedTurn { turn, owner_reply }
}

fn root(turn: &EOCheckedTurn) -> PathBuf {
    turn.record(|record| PathBuf::from(&record.engram.observed_source_root().unwrap().root))
}

fn commit_content(turn: &OwnerCheckedTurn, content: &str) {
    let directory = root(turn);
    fs::write(directory.join("README.md"), content).unwrap();
    run_git_test_command(&directory, &["add", "README.md"]);
    run_git_test_command(&directory, &["commit", "--quiet", "-m", "fixture source"]);
    let envelope = owner_envelope(&directory);
    *turn.owner_reply.lock().unwrap() = envelope;
}

fn registered_check(turn: &EOCheckedTurn, key: &str) {
    registered_check_with_result_publication(turn, key, false);
}

fn registered_check_with_result_publication(
    turn: &EOCheckedTurn,
    key: &str,
    after_completion: bool,
) {
    let directory = root(turn);
    assert!(run_git_test_command_output(&directory, &["status", "--porcelain"]).is_empty());
    let commit = run_git_test_command_output(&directory, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let source = owner_envelope(&directory)["source_id"].clone();
    let mut recorder = turn.recorder();
    recorder
        .command_started_in(
            key,
            COMMAND,
            Some(COMMAND),
            Some(&directory.to_string_lossy()),
        )
        .unwrap();
    turn.wait_for_snapshots();
    let started = test_run_timestamp();
    let run_id = format!("test-{}", Uuid::new_v4());
    let run_dir = EngramHost::run_artifact_directory(&directory)
        .unwrap()
        .join(&run_id);
    fs::create_dir_all(&run_dir).unwrap();
    let argv = ["cargo", "test", "--lib"];
    let request = json!({
        "runId": run_id, "root": directory.to_string_lossy(), "owner": turn.session_id,
        "creatorPid": std::process::id(), "started": started,
        "input": "commit", "full": false, "detached": false,
        "sourceId": source,
        "stages": [{ "name": "focused", "command": argv[0], "args": &argv[1..] }],
    });
    fs::write(
        run_dir.join("request.json"),
        serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();
    turn.state
        .register_test_run(
            &turn.session_id,
            RegisterTestRunRequest {
                run_id: run_id.clone(),
                run_directory: test_run_display_path(&run_dir),
            },
        )
        .unwrap();
    // The launcher registers before starting a stage; its request's earlier
    // start identifies the invocation, not the stage's execution interval.
    let stage_started = test_run_timestamp();
    let ended = test_run_timestamp();
    let results = json!({
        "runId": run_id, "state": "passed", "exitCode": 0,
        "started": started, "ended": ended,
        "sourceIdBefore": source, "sourceIdAfter": source,
        "stages": [{ "name": "focused", "state": "passed", "code": 0,
            "command": argv, "started": stage_started, "ended": ended,
            "tests": { "runner": "cargo-libtest", "passed": 1, "failed": 0, "ignored": 0 } }],
        "input": { "kind": "commit", "commit": commit, "cleanBefore": true,
            "after": { "commit": commit, "clean": true, "paths": [] },
            "outcome": "clean", "eligible": true },
    });
    let publish_results = || {
        fs::write(
            run_dir.join("results.json"),
            serde_json::to_vec(&results).unwrap(),
        )
        .unwrap()
    };
    if !after_completion {
        publish_results();
    }
    recorder
        .command_completed_with_exit(
            key,
            COMMAND,
            &format!(
                "RUN {}\nfocused: passed (1 passed, 0 failed, 0 ignored)",
                test_run_display_path(&run_dir)
            ),
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .unwrap();
    // Completing a recorder command attaches its locator and immediately
    // scans the artifacts. Delay terminal publication in the race fixture so
    // that scan cannot retain the completion before closing preparation.
    if after_completion {
        publish_results();
    }
    turn.wait_for_snapshots();
    let registration = turn.state.inner.lock().unwrap().test_run_registrations[&run_id].clone();
    assert!(registration.named_root.is_some());
    let prepared = engram_verification_record_prepare(&registration);
    assert!(
        matches!(prepared, EngramVerificationRecordPrepared::Ready(_)),
        "the registered owner-backed fixture prepares a complete record"
    );
    assert!(
        turn.record(|record| record
            .engram
            .active_turn_checks
            .iter()
            .any(|check| check.key == key && check.end.is_some() && !check.overlapped)),
        "the actual recorder retained a completed, non-overlapped check"
    );
    assert!(
        turn.state
            .inner
            .lock()
            .unwrap()
            .test_run_observations
            .values()
            .any(|row| row
                .artifact
                .as_ref()
                .is_some_and(|artifact| artifact.run_id == run_id)),
        "the original launcher locator associated this registration with its command"
    );
}

#[test]
fn a_clean_registered_check_is_a_closing_baseline_without_a_prior_checkpoint() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let turn = start_named("registered-first-baseline");
    assert!(turn.record(|record| record.engram.runtime_checkpoint.is_none()));
    registered_check(&turn, "first-clean");
    let observation = turn.finish();
    assert!(
        observation["occurrence"]["source_change"].is_null(),
        "a clean registered baseline produces no source-change payload"
    );
}

#[test]
fn native_completion_retained_after_preparation_preserves_the_registered_closing_baseline() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let turn = start_named("registered-native-completion-race");
    {
        let inner = turn.state.inner.lock().unwrap();
        turn.state.persist_internal_locked(&inner).unwrap();
    }
    {
        let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
        let mut inner = turn.state.inner.lock().unwrap();
        bind_loaded_federation(
            &mut inner,
            &connection,
            turn.state.persistence_path.as_path(),
        )
        .unwrap();
    }
    registered_check_with_result_publication(&turn, "native-completion-race", true);
    let prepared = turn
        .state
        .prepare_registered_check_notices(&turn.session_id);
    let before = prepared
        .iter()
        .flat_map(|notice| &notice.runs)
        .map(|(run, _)| run.clone())
        .next()
        .expect("prepared original registration");
    assert!(before.native_origin.is_some());
    assert!(before.native_completion.is_none());
    let baselines = || {
        let inner = turn.state.inner.lock().unwrap();
        let index = inner.find_session_index(&turn.session_id).unwrap();
        let scope = EngramRuntimeClosingScope::capture(&inner, &turn.session_id).unwrap();
        registered_closing_baselines_locked(&inner, index, &prepared, &scope)
    };
    let original = baselines();
    assert_eq!(
        original.len(),
        2,
        "real registered check has both content endpoints"
    );
    // Drive the real scan between the off-lock preparation and closing CAS.
    // Its native completion is retained from the original provider/artifacts.
    turn.state.refresh_test_runs_with(&|_, _| false, &|_, _| {});
    let after = turn.state.inner.lock().unwrap().test_run_registrations[&before.run_id].clone();
    assert!(after.native_completion.is_some());
    assert_ne!(after, before);
    assert!(
        baselines() == original,
        "a factual completion append preserves the endpoints"
    );
    {
        let mut inner = turn.state.inner.lock().unwrap();
        inner.test_run_registrations.update(&before.run_id, |run| {
            run.request_identity.push_str("-replaced");
        });
    }
    assert!(
        baselines().is_empty(),
        "an actual registration identity change still fences closing"
    );
}

#[test]
fn an_edit_after_a_registered_check_reports_a_real_comparison() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let turn = start_named("registered-edited-baseline");
    registered_check(&turn, "before-edit");
    let before = turn.record(|record| {
        record
            .engram
            .active_turn_checks
            .iter()
            .find(|check| check.key == "before-edit")
            .unwrap()
            .end
            .as_ref()
            .unwrap()
            .end_basis
            .wait_until(std::time::Instant::now())
            .flatten()
            .unwrap()
    });
    commit_content(&turn, "changed after the check\n");
    let observation = turn.finish();
    let change = &observation["occurrence"]["source_change"];
    assert_eq!(change["detection"], "content_comparison");
    assert!(
        change["baseline"]["source_id"] == serde_json::to_value(before.source_id.unwrap()).unwrap(),
        "the original complete owner identity supplies the baseline"
    );
    assert!(
        change["baseline"]["source_id"] != change["sighting"]["source_basis"]["source_id"],
        "the post-check edit changes the complete owner identity"
    );
}

#[test]
fn every_registered_check_is_compared_even_when_the_source_returns_to_its_first_content() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let turn = start_named("registered-reverted-baseline");
    let original = fs::read_to_string(root(&turn).join("README.md")).unwrap();
    registered_check(&turn, "first-content");
    commit_content(&turn, "intermediate checked content\n");
    registered_check(&turn, "intermediate-content");
    commit_content(&turn, &original);
    registered_check(&turn, "first-content-again");
    let observation = turn.finish();
    let change = &observation["occurrence"]["source_change"];
    assert_eq!(change["detection"], "content_comparison");
    assert!(
        change["baseline"]["source_id"] != change["sighting"]["source_basis"]["source_id"],
        "the intermediate checked identity remains distinct after a revert"
    );
}

#[test]
fn no_registered_check_keeps_the_missing_baseline_assumption() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let turn = start_named("registered-absent-baseline");
    let observation = turn.finish();
    assert_eq!(
        observation["occurrence"]["source_change"]["detection"],
        "assumed_missing_baseline"
    );
}

#[test]
fn retained_failed_closings_do_not_disable_a_new_checks_baseline() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for (terminal, other_claim) in [(true, false), (true, true), (false, false)] {
        let turn = start_named(&format!("retained-closing-{terminal}-{other_claim}"));
        registered_check(&turn, "new-clean-check");
        let prepared = turn
            .state
            .prepare_registered_check_notices(&turn.session_id);
        let historical = {
            let mut inner = turn.state.inner.lock().unwrap();
            let index = inner.find_session_index(&turn.session_id).unwrap();
            let scope = EngramRuntimeClosingScope::capture(&inner, &turn.session_id).unwrap();
            let mut trace = inner.sessions[index]
                .engram
                .unadmitted_active_turn
                .clone()
                .unwrap();
            trace.host_turn_ref.push_str("-predecessor");
            trace.generation = trace.generation.saturating_sub(1);
            trace.commands.clear();
            let mut job = EngramRuntimeClosingJob::new(trace, None, scope.clone());
            if other_claim {
                job.scope
                    .binding
                    .as_mut()
                    .unwrap()
                    .claim_id
                    .push_str("-previous");
            }
            job.terminal_failure = terminal.then(|| "retained scope-change failure".into());
            inner.sessions[index]
                .engram
                .unadmitted_closing_jobs
                .push(job.clone());
            let endpoints = registered_closing_baselines_locked(&inner, index, &prepared, &scope);
            assert_eq!(
                endpoints.len(),
                if terminal { 2 } else { 0 },
                "terminal={terminal}, other_claim={other_claim}"
            );
            job
        };
        if terminal {
            let observation = turn.finish();
            assert!(
                observation["occurrence"]["source_change"].is_null(),
                "a terminal earlier report does not invent a source change"
            );
            assert!(
                turn.record(|record| record.engram.unadmitted_closing_jobs.contains(&historical)),
                "the historical failure remains available as a diagnostic"
            );
        }
        // The pending case checks selection without starting that older job's
        // worker or manufacturing a successful observation for it.
    }
}

#[test]
fn a_registered_check_with_failed_source_capture_keeps_the_assumption() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let turn = start_named("registered-failed-baseline");
    registered_check(&turn, "failed-source");
    turn.record_mut(|record| {
        let check = record
            .engram
            .active_turn_checks
            .iter_mut()
            .find(|check| check.key == "failed-source")
            .unwrap();
        check.end.as_mut().unwrap().end_basis = EngramBasisCapture::settled(None);
    });
    let observation = turn.finish();
    assert_eq!(
        observation["occurrence"]["source_change"]["detection"],
        "assumed_missing_baseline"
    );
}

#[test]
fn an_unregistered_check_does_not_erase_the_unknown_prefix() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let turn = start_named("unregistered-baseline");
    let directory = root(&turn);
    let mut recorder = turn.recorder();
    recorder
        .command_started_in(
            "unregistered",
            SIZE_TEST,
            Some(SIZE_TEST),
            Some(&directory.to_string_lossy()),
        )
        .unwrap();
    turn.wait_for_snapshots();
    recorder
        .command_completed_with_exit(
            "unregistered",
            SIZE_TEST,
            "running 1 test\ntest result: ok. 1 passed",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .unwrap();
    turn.wait_for_snapshots();
    let observation = turn.finish();
    assert_eq!(
        observation["occurrence"]["source_change"]["detection"],
        "assumed_missing_baseline"
    );
}

#[test]
fn incomplete_or_changed_registered_check_scope_keeps_the_assumption() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for mismatch in [
        "pending",
        "generation",
        "workspace",
        "overlap",
        "claim-fence",
        "rejected-receipt",
        "dirty-input",
        "missing-earlier-check",
    ] {
        eprintln!("REGISTERED_BASELINE_CASE {mismatch}");
        let turn = start_named(&format!("registered-{mismatch}"));
        registered_check(&turn, "scoped-check");
        match mismatch {
            "claim-fence" => {
                let mut inner = turn.state.inner.lock().unwrap();
                let id = inner.test_run_registrations.keys().next().unwrap().clone();
                inner
                    .test_run_registrations
                    .update(&id, |run| {
                        run.work_binding.as_mut().unwrap().claim_fence += 1
                    })
                    .unwrap();
            }
            "rejected-receipt" => {
                let mut inner = turn.state.inner.lock().unwrap();
                let id = inner.test_run_registrations.keys().next().unwrap().clone();
                inner
                    .test_run_registrations
                    .update(&id, |run| {
                        run.verification_record = Some(TestRunVerificationAck::Rejected {
                            detail: "fixture producer refusal".into(),
                        });
                    })
                    .unwrap();
            }
            "dirty-input" => {
                let directory = turn
                    .state
                    .inner
                    .lock()
                    .unwrap()
                    .test_run_registrations
                    .values()
                    .next()
                    .unwrap()
                    .run_directory
                    .clone();
                let path = PathBuf::from(directory).join("results.json");
                let mut results: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                results["input"]["outcome"] = json!("input_changed");
                results["input"]["after"]["clean"] = json!(false);
                results["input"]["after"]["paths"] =
                    json!([{ "status": " M", "path": "README.md" }]);
                results["input"]["eligible"] = json!(false);
                fs::write(path, serde_json::to_vec(&results).unwrap()).unwrap();
            }
            "missing-earlier-check" => turn.record_mut(|record| {
                let trace = record.engram.unadmitted_active_turn.as_mut().unwrap();
                let mut earlier = trace.commands[0].clone();
                earlier.tool_key = "discarded-earlier-check".into();
                // It represents a different observed command whose source
                // snapshots are no longer retained, not a duplicate wire ID.
                earlier.check.host_check_id.push_str("-earlier");
                trace.commands.insert(0, earlier);
            }),
            _ => turn.record_mut(|record| {
                let check = record
                    .engram
                    .active_turn_checks
                    .iter_mut()
                    .find(|check| check.key == "scoped-check")
                    .unwrap();
                if mismatch == "overlap" {
                    check.overlapped = true;
                    return;
                }
                let end = check.end.as_mut().unwrap();
                if mismatch == "pending" {
                    end.end_basis = Arc::new(EngramBasisCapture::default());
                } else {
                    let mut source = end
                        .end_basis
                        .wait_until(std::time::Instant::now())
                        .flatten()
                        .unwrap();
                    if mismatch == "generation" {
                        source.source_root_generation = Some(999);
                    } else {
                        source.workspace_id.push_str("-other");
                    }
                    end.end_basis = EngramBasisCapture::settled(Some(source));
                }
            }),
        }
        turn.record(|record| {
            let trace = record.engram.unadmitted_active_turn.as_ref().unwrap();
            let ids = trace
                .commands
                .iter()
                .map(|command| command.check.host_check_id.as_str())
                .collect::<HashSet<_>>();
            assert_eq!(
                ids.len(),
                trace.commands.len(),
                "{mismatch}: distinct observed commands"
            );
        });
        let observation = turn.finish();
        assert_eq!(
            observation["occurrence"]["source_change"]["detection"], "assumed_missing_baseline",
            "{mismatch}: an incomplete registered scope keeps the baseline unknown"
        );
    }
}

#[test]
fn the_registered_closing_measurements_survive_job_serialization_without_retimestamping() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let turn = start_named("registered-durable-baseline");
    registered_check(&turn, "durable-check");
    let prepared = turn
        .state
        .prepare_registered_check_notices(&turn.session_id);
    let job = {
        let inner = turn.state.inner.lock().unwrap();
        let index = inner.find_session_index(&turn.session_id).unwrap();
        let scope = EngramRuntimeClosingScope::capture(&inner, &turn.session_id).unwrap();
        let trace = inner.sessions[index]
            .engram
            .unadmitted_active_turn
            .clone()
            .unwrap();
        let baselines = registered_closing_baselines_locked(&inner, index, &prepared, &scope);
        assert_eq!(
            baselines.len(),
            2,
            "actual start and after measurements are both retained"
        );
        assert!(
            baselines
                .iter()
                .all(|basis| basis.endpoint.source_id.is_some()
                    && basis.endpoint.source_revision.is_empty()),
            "new closing baselines retain owner identity"
        );
        let mut job = EngramRuntimeClosingJob::new(trace, None, scope);
        job.registered_check_baselines = baselines;
        job
    };
    let mut wire = serde_json::to_value(&job).unwrap();
    let restored: EngramRuntimeClosingJob = serde_json::from_value(wire.clone()).unwrap();
    assert!(
        restored.registered_check_baselines == job.registered_check_baselines,
        "job serialization preserves the complete retained endpoints"
    );
    let mut old = wire.clone();
    for basis in old["registered_check_baselines"].as_array_mut().unwrap() {
        basis["endpoint"]
            .as_object_mut()
            .unwrap()
            .remove("source_id");
        basis["endpoint"]["source_revision"] = json!("content-v1:stored-historical-baseline");
    }
    let historical: EngramRuntimeClosingJob = serde_json::from_value(old).unwrap();
    assert!(
        historical
            .registered_check_baselines
            .iter()
            .all(|basis| basis.endpoint.source_id.is_none()
                && basis.endpoint.source_revision == "content-v1:stored-historical-baseline")
    );
    wire.as_object_mut()
        .unwrap()
        .remove("registered_check_baselines");
    let historical: EngramRuntimeClosingJob = serde_json::from_value(wire).unwrap();
    assert!(
        historical.registered_check_baselines.is_empty(),
        "old jobs never invent check measurements"
    );
}
