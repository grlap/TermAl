// Host-assigned observed ids use the existing wait and resume queue. These
// tests exercise observation production and real declared artifact judging,
// without launcher registration or verification credit.
use super::*;

#[path = "late_declared.rs"]
mod late_declared;

const DECLARED: &str = "flutter test --file-reporter json:.termal-results/flutter.json";

fn start(fixture: &WaitFixture, key: &str) -> String {
    fs::write(fixture.root.join(".gitignore"), ".termal-results/\n").unwrap();
    fs::write(fixture.root.join("termal-tests.toml"), format!(
        "[[test]]\ncommand = '{DECLARED}'\ncwd = '.'\nartifact = '.termal-results/flutter.json'\n"
    )).unwrap();
    let (runtime, _commands) = test_codex_runtime_handle("observed-wait-runtime");
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        let record = &mut inner.sessions[index];
        record.session.agent = Agent::Codex;
        record.runtime = SessionRuntime::Codex(runtime);
        record.engram.unadmitted_active_turn = Some(EngramRuntimeObservedTurn::started(
            "observed-wait-turn".into(),
            record.active_turn_generation,
        ));
    }
    fixture.state.note_engram_command_started(
        &fixture.session,
        &EngramObservationProvenance::Ambient,
        key,
        Some(DECLARED),
        Some(&test_run_display_path(&fixture.root)),
    );
    let inner = fixture.state.inner.lock().unwrap();
    let row = inner
        .test_run_observations
        .values()
        .find(|row| row.tool_identity == sha256_hex(key.as_bytes()))
        .unwrap();
    assert!(
        row.declared_artifact.is_some(),
        "must exercise a matched declaration"
    );
    assert_eq!(row.summary.state, TestRunState::Running);
    row.execution_id.clone()
}

fn end(fixture: &WaitFixture, key: &str, exit: Option<EngramCommandExit>) {
    if let Some(EngramCommandExit::Code(code)) = exit {
        fs::create_dir_all(fixture.root.join(".termal-results")).unwrap();
        fs::write(
            fixture.root.join(".termal-results/flutter.json"),
            if code == 0 {
                include_bytes!("../fixtures/declared-dart/flutter-pass.jsonl").as_slice()
            } else {
                include_bytes!("../fixtures/declared-dart/flutter-fail.jsonl").as_slice()
            },
        )
        .unwrap();
    }
    fixture.state.note_engram_command_finished(
        &fixture.session,
        &EngramObservationProvenance::Ambient,
        key,
        DECLARED,
        "provider output is not the verdict",
        exit,
    );
}

#[test]
fn running_declared_observation_waits_then_queues_exactly_one_resume_without_launcher_artifacts() {
    let fixture = WaitFixture::new("observed-running");
    fixture.busy();
    let id = start(&fixture, "original-task");
    let rescans = std::cell::Cell::new(0);
    let response = fixture
        .state
        .create_test_run_wait_with(
            &fixture.session,
            serde_json::from_value(json!({"runIds": [&id]})).unwrap(),
            &|| rescans.set(rescans.get() + 1),
        )
        .unwrap();
    assert_eq!(
        rescans.get(),
        0,
        "an observation requires no artifact index rescan"
    );
    assert!(!response.resume_prompt_queued);
    assert!(response.wait.runs[0].observation.is_some());
    assert!(!fixture.state.inner.lock().unwrap().test_runs.scanned_once);
    fixture.state.refresh_test_run_waits();
    assert!(fixture.queued_prompts().is_empty());
    end(&fixture, "original-task", Some(EngramCommandExit::Code(0)));
    fixture.state.refresh_test_run_waits();
    assert!(fixture.pending_waits().is_empty());
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains(&id) && prompts[0].contains("PASS"));
    assert!(!prompts[0].contains("test-launcher.mjs summary"));
    assert!(!prompts[0].contains("test-launcher.mjs recover"));
    fixture.state.refresh_test_run_waits();
    fixture.state.refresh_test_run_waits();
    assert_eq!(fixture.queued_prompts(), prompts);
    let inner = fixture.state.inner.lock().unwrap();
    assert!(inner.test_runs.find(&id).is_none());
    assert!(inner.test_run_registrations.get(&id).is_none());
    assert!(inner.test_run_observations[&id].evidence.is_empty());
}

#[test]
fn already_terminal_declared_observations_resume_immediately_for_pass_and_fail() {
    for code in [0, 7] {
        let fixture = WaitFixture::new("observed-terminal");
        fixture.busy();
        let id = start(&fixture, "terminal-task");
        end(
            &fixture,
            "terminal-task",
            Some(EngramCommandExit::Code(code)),
        );
        let response = fixture.register(&[&id], "all").unwrap();
        assert!(response.resume_prompt_queued);
        assert!(fixture.pending_waits().is_empty());
        let prompts = fixture.queued_prompts();
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains(if code == 0 { "PASS" } else { "FAIL" }));
        fixture.state.refresh_test_run_waits();
        assert_eq!(fixture.queued_prompts(), prompts);
    }
}

#[test]
fn observed_unknown_and_foreign_ids_are_refused_without_installing_a_wait() {
    let fixture = WaitFixture::new("observed-refusals");
    let error = fixture
        .register(&["observed-not-assigned"], "all")
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert!(error.message.contains("test_run_wait_observation_unknown"));
    let id = start(&fixture, "foreign-task");
    let root = fixture.root.join("other-project");
    fs::create_dir_all(&root).unwrap();
    let project = create_test_project(&fixture.state, &root, "Other");
    let other = create_test_project_session(&fixture.state, Agent::Claude, &project, &root);
    let error = fixture
        .state
        .create_test_run_wait_with(
            &other,
            serde_json::from_value(json!({"runIds": [&id]})).unwrap(),
            &|| {},
        )
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(
        error
            .message
            .contains("not in the waiting session's project")
    );
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn terminal_unknown_observation_is_delivered_honestly_without_inventing_an_exit() {
    let fixture = WaitFixture::new("observed-unknown");
    fixture.busy();
    let id = start(&fixture, "unknown-task");
    // The producer can still end a background declared task UNKNOWN. The
    // wait does not fix artifact settlement or upgrade that fact to PASS.
    end(&fixture, "unknown-task", None);
    assert!(
        fixture
            .register(&[&id], "all")
            .unwrap()
            .resume_prompt_queued
    );
    let prompt = fixture.queued_prompts().pop().unwrap();
    assert!(prompt.contains("UNKNOWN (host-observed execution ended without a verdict)"));
    assert!(prompt.contains("Exit code: none"));
    assert!(!prompt.contains("- Verdict: PASS"));
}

#[test]
fn an_observation_identity_replacement_does_not_supply_the_original_wait_verdict() {
    let fixture = WaitFixture::new("observed-replaced");
    fixture.busy();
    let id = start(&fixture, "replaced-task");
    fixture.register(&[&id], "all").unwrap();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        inner.test_run_observations.update(&id, |row| {
            row.command_fingerprint = "replacement".to_owned();
            row.summary.state = TestRunState::Passed;
            row.summary.ended_at = Some(test_run_timestamp());
        });
    }
    fixture.state.refresh_test_run_waits();
    let prompt = fixture.queued_prompts().pop().unwrap();
    assert!(prompt.contains("UNKNOWN (host observation unavailable or identity changed)"));
    assert!(!prompt.contains("- Verdict: PASS"));
}

#[test]
fn mixed_wait_reads_launcher_failure_even_when_observation_ended_unknown() {
    let fixture = WaitFixture::new("observed-mixed");
    fixture.busy();
    fixture.write("test-mixed", Some(failed("original launcher failure")));
    fixture.scan(&[]);
    let id = start(&fixture, "mixed-task");
    end(&fixture, "mixed-task", None);
    assert!(
        fixture
            .register(&[&id, "test-mixed"], "all")
            .unwrap()
            .resume_prompt_queued
    );
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("UNKNOWN (host-observed execution ended without a verdict)"));
    assert!(prompts[0].contains("FAIL"));
    assert!(prompts[0].contains("original launcher failure"));
}

#[test]
fn an_installed_retained_observation_can_supply_the_same_terminal_wait() {
    let fixture = WaitFixture::new("observed-retained");
    fixture.busy();
    let id = start(&fixture, "retained-task");
    end(&fixture, "retained-task", Some(EngramCommandExit::Code(0)));
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        // Move the genuine terminal row into the existing retained store.
        // No native verification receipt or final-unbound proof is added.
        let row = inner.test_run_observations.remove(&id).unwrap();
        inner.test_run_compacted.insert(
            id.clone(),
            TestRunCompacted {
                compacted: true,
                request_identity: None,
                summary: row.summary.clone(),
                registration: None,
                observations: vec![row],
                request_stamp: None,
                results_stamp: None,
                reason: "retained fixture".to_owned(),
                artifact_conflict: false,
                artifact_unavailable: false,
            },
        );
    }
    assert!(
        fixture.register(&[&id], "all").is_err(),
        "uninstalled retained state cannot authorize a wait"
    );
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_runs
        .compactions_durable
        .insert(id.clone());
    assert!(
        fixture
            .register(&[&id], "all")
            .unwrap()
            .resume_prompt_queued
    );
    assert!(fixture.queued_prompts()[0].contains("- Verdict: PASS"));
}

#[test]
fn interrupted_observation_monitoring_resumes_unknown_without_an_artifact_scan() {
    let fixture = WaitFixture::new("observed-interrupted");
    fixture.busy();
    let id = start(&fixture, "interrupted-task");
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_observations
        .update(&id, test_run_observation_boot);
    assert!(
        fixture
            .register(&[&id], "all")
            .unwrap()
            .resume_prompt_queued
    );
    let prompt = fixture.queued_prompts().pop().unwrap();
    assert!(prompt.contains("UNKNOWN (host observation monitoring interrupted)"));
    assert!(prompt.contains("Exit code: none"));
    assert!(!prompt.contains("- Verdict: PASS"));
}

#[test]
fn a_wait_between_provider_end_and_declared_verdict_never_consumes_provisional_pass() {
    for (artifact, expected) in [
        (
            Some(include_bytes!("../fixtures/declared-dart/flutter-fail.jsonl").as_slice()),
            "FAIL",
        ),
        (None, "UNKNOWN"),
        (
            Some(include_bytes!("../fixtures/declared-dart/flutter-pass.jsonl").as_slice()),
            "PASS",
        ),
    ] {
        let fixture = WaitFixture::new("observed-settling");
        fixture.busy();
        let id = start(&fixture, "settling-task");
        let binding = {
            let inner = fixture.state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
            runtime_unadmitted_trace(record, &EngramObservationProvenance::Ambient)
                .unwrap()
                .commands
                .iter()
                .find(|entry| entry.tool_key == "settling-task")
                .unwrap()
                .declared
                .clone()
                .unwrap()
        };
        // Pause at the real producer's two phases. This deliberately admits
        // and refreshes a wait while END is visible and its artifact is not.
        let ended = fixture
            .state
            .observe_test_command_end_awaiting_declared_verdict(
                &fixture.session,
                &EngramObservationProvenance::Ambient,
                "settling-task",
                Some(EngramCommandExit::Code(0)),
            );
        assert_eq!(ended, vec![id.clone()]);
        {
            let inner = fixture.state.inner.lock().unwrap();
            assert!(inner.test_run_observations[&id].summary.ended_at.is_some());
            assert_eq!(
                inner.test_run_observations[&id].summary.state,
                TestRunState::Passed
            );
        }
        let response = fixture.register(&[&id], "all").unwrap();
        assert!(!response.resume_prompt_queued);
        fixture.state.refresh_test_run_waits();
        fixture.state.refresh_test_run_waits();
        assert!(
            fixture.queued_prompts().is_empty(),
            "exit 0 is still provisional"
        );
        assert_eq!(fixture.pending_waits().len(), 1);
        if let Some(bytes) = artifact {
            fs::create_dir_all(fixture.root.join(".termal-results")).unwrap();
            fs::write(fixture.root.join(".termal-results/flutter.json"), bytes).unwrap();
        }
        let result = engram_declared_result(&binding, EngramCommandExit::Code(0));
        fixture.state.apply_declared_verdict_to_test_run(
            &ended,
            engram_declared_outcome(EngramCommandExit::Code(0), &result),
        );
        fixture.state.refresh_test_run_waits();
        let prompts = fixture.queued_prompts();
        assert_eq!(prompts.len(), 1);
        assert!(
            prompts[0].contains(&format!("- Verdict: {expected}")),
            "{}",
            prompts[0]
        );
        fixture.state.refresh_test_run_waits();
        assert_eq!(fixture.queued_prompts(), prompts);
        assert!(
            !fixture.state.inner.lock().unwrap().test_run_observations[&id]
                .declared_verdict_pending,
            "even PASS matching the provisional state must finish settlement"
        );
    }
}

#[test]
fn a_restart_during_declared_settlement_delivers_unknown_instead_of_provisional_pass() {
    let fixture = WaitFixture::new("observed-settlement-restart");
    fixture.busy();
    let id = start(&fixture, "settlement-restart-task");
    fixture
        .state
        .observe_test_command_end_awaiting_declared_verdict(
            &fixture.session,
            &EngramObservationProvenance::Ambient,
            "settlement-restart-task",
            Some(EngramCommandExit::Code(0)),
        );
    let original = fixture.state.inner.lock().unwrap().test_run_observations[&id].clone();
    // Persistence includes the producer phase; the normal boot owner marks
    // its unfinished verdict interrupted rather than treating exit 0 as final.
    let mut restored: TestRunCommandObservation =
        serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
    test_run_observation_boot(&mut restored);
    assert!(restored.declared_verdict_pending);
    assert_eq!(restored.summary.state, TestRunState::Unknown);
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_observations
        .insert(id.clone(), restored);
    assert!(
        fixture
            .register(&[&id], "all")
            .unwrap()
            .resume_prompt_queued
    );
    let prompt = fixture.queued_prompts().pop().unwrap();
    assert!(prompt.contains("UNKNOWN (host observation monitoring interrupted)"));
    assert!(!prompt.contains("- Verdict: PASS"));
}

#[test]
fn a_settling_declared_observation_stays_live_until_its_artifact_verdict_is_applied() {
    let fixture = WaitFixture::new("observed-settlement-retention");
    let id = start(&fixture, "settlement-retention-task");
    let binding = {
        let inner = fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
        runtime_unadmitted_trace(record, &EngramObservationProvenance::Ambient)
            .unwrap()
            .commands
            .iter()
            .find(|entry| entry.tool_key == "settlement-retention-task")
            .unwrap()
            .declared
            .clone()
            .unwrap()
    };
    let ended = fixture
        .state
        .observe_test_command_end_awaiting_declared_verdict(
            &fixture.session,
            &EngramObservationProvenance::Ambient,
            "settlement-retention-task",
            Some(EngramCommandExit::Code(0)),
        );
    let eligible = || {
        let inner = fixture.state.inner.lock().unwrap();
        assert!(inner.test_runs.observations_durable.contains(&id));
        let mut candidate = inner.test_run_observations[&id].summary.clone();
        // Isolate the settlement hold from unrelated restart-risk policy.
        // This is an eligibility argument, never published verification.
        candidate.registration.restart_would_void_evidence = Some(false);
        test_run_compaction_eligible(&inner, &candidate)
    };
    assert!(
        !eligible(),
        "durable END alone cannot retire the producer's pending row"
    );
    fs::create_dir_all(fixture.root.join(".termal-results")).unwrap();
    fs::write(
        fixture.root.join(".termal-results/flutter.json"),
        include_bytes!("../fixtures/declared-dart/flutter-pass.jsonl"),
    )
    .unwrap();
    let result = engram_declared_result(&binding, EngramCommandExit::Code(0));
    fixture.state.apply_declared_verdict_to_test_run(
        &ended,
        engram_declared_outcome(EngramCommandExit::Code(0), &result),
    );
    assert!(
        eligible(),
        "positive control: the same row becomes retainable after settlement"
    );
}

#[test]
fn runtime_loss_during_declared_settlement_resumes_unknown_without_a_restart() {
    let fixture = WaitFixture::new("observed-settlement-runtime-loss");
    fixture.busy();
    let id = start(&fixture, "settlement-lost-task");
    fixture
        .state
        .observe_test_command_end_awaiting_declared_verdict(
            &fixture.session,
            &EngramObservationProvenance::Ambient,
            "settlement-lost-task",
            Some(EngramCommandExit::Code(0)),
        );
    assert!(
        !fixture
            .register(&[&id], "all")
            .unwrap()
            .resume_prompt_queued
    );
    let (replacement, _commands) = test_codex_runtime_handle("replacement-wait-runtime");
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Codex(replacement);
    }
    fixture.state.step_test_command_observation_persistence();
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("UNKNOWN (host observation monitoring interrupted)"));
    assert!(!prompts[0].contains("- Verdict: PASS"));
    fixture.state.step_test_command_observation_persistence();
    fixture.state.refresh_test_run_waits();
    assert_eq!(fixture.queued_prompts(), prompts);
}
