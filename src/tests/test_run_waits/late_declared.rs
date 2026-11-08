// Late Codex completions through the real END producer and existing wait.
// Artifacts and provider frames are disposable fixtures; no verification is minted.
use super::*;

fn artifact(fixture: &WaitFixture, passed: bool) {
    fs::create_dir_all(fixture.root.join(".termal-results")).unwrap();
    fs::write(
        fixture.root.join(".termal-results/flutter.json"),
        if passed {
            include_bytes!("../fixtures/declared-dart/flutter-pass.jsonl").as_slice()
        } else {
            include_bytes!("../fixtures/declared-dart/flutter-fail.jsonl").as_slice()
        },
    )
    .unwrap();
}

#[test]
fn late_declared_end_keeps_the_wait_pending_until_the_original_artifact_is_applied() {
    for (passed, exit, expected) in [
        (Some(true), EngramCommandExit::Code(0), "PASS"),
        (Some(false), EngramCommandExit::Code(0), "FAIL"),
        (None, EngramCommandExit::Code(0), "UNKNOWN"),
        (Some(true), EngramCommandExit::Code(7), "FAIL"),
        (Some(true), EngramCommandExit::Unknown, "UNKNOWN"),
    ] {
        let fixture = WaitFixture::new("late-declared-artifact");
        fixture.busy();
        let id = start(&fixture, "late-task");
        if let Some(passed) = passed {
            artifact(&fixture, passed);
        }
        // The Codex completion can arrive before TermAl closes this turn.
        // Once a turn closes, the carried-check fixture covers its owner.
        assert!(
            !fixture
                .register(&[&id], "all")
                .unwrap()
                .resume_prompt_queued
        );
        let interposed = std::cell::Cell::new(false);
        fixture.state.note_engram_reported_end_with_settlement(
            &fixture.session,
            "late-task",
            DECLARED,
            "shell success is not a verdict",
            exit,
            || {
                interposed.set(true);
                {
                    let inner = fixture.state.inner.lock().unwrap();
                    let row = &inner.test_run_observations[&id];
                    assert!(row.summary.ended_at.is_some());
                    assert!(row.declared_verdict_pending);
                }
                fixture.state.refresh_test_run_waits();
                assert!(fixture.queued_prompts().is_empty());
                assert_eq!(fixture.pending_waits().len(), 1);
            },
        );
        assert!(
            interposed.get(),
            "the real late END producer reached the settlement boundary"
        );
        fixture.state.refresh_test_run_waits();
        let prompts = fixture.queued_prompts();
        assert_eq!(prompts.len(), 1);
        assert!(
            prompts[0].contains(&format!("- Verdict: {expected}")),
            "{}",
            prompts[0]
        );
        {
            let inner = fixture.state.inner.lock().unwrap();
            let row = &inner.test_run_observations[&id];
            assert!(!row.declared_verdict_pending);
            assert_eq!(
                row.summary.exit_code,
                match exit {
                    EngramCommandExit::Code(code) => Some(code),
                    _ => None,
                }
            );
            assert!(row.evidence.is_empty());
        }
        artifact(&fixture, passed != Some(true));
        fixture.state.note_engram_reported_end(
            &fixture.session,
            "late-task",
            DECLARED,
            "duplicate",
            EngramCommandExit::Code(0),
        );
        fixture.state.refresh_test_run_waits();
        assert_eq!(fixture.queued_prompts(), prompts);
    }
}

#[test]
fn a_completed_earlier_use_of_the_same_key_does_not_hide_the_current_binding() {
    let fixture = WaitFixture::new("late-declared-reused-key");
    fixture.busy();
    let id = start(&fixture, "reused-key");
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let record = inner.session_mut(&fixture.session).unwrap();
        let trace = record.engram.unadmitted_active_turn.as_mut().unwrap();
        let mut earlier = trace.commands.last().unwrap().clone();
        earlier.invocation_ended = true;
        earlier.check.finished_at = Some(test_run_timestamp());
        earlier.check.host_check_id = "completed-earlier-occurrence".into();
        earlier.declared.as_mut().unwrap().started -= Duration::from_secs(1);
        trace.commands.insert(0, earlier);
    }
    artifact(&fixture, true);
    fixture.register(&[&id], "all").unwrap();
    fixture.state.note_engram_reported_end(
        &fixture.session,
        "reused-key",
        DECLARED,
        "completed",
        EngramCommandExit::Code(0),
    );
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("- Verdict: PASS"));
    let inner = fixture.state.inner.lock().unwrap();
    let row = &inner.test_run_observations[&id];
    assert_eq!(row.summary.state, TestRunState::Passed);
    assert!(row.evidence.is_empty());
}

#[test]
fn a_late_declared_end_without_its_original_owner_is_unknown_even_with_a_pass_artifact() {
    for replacement_trace in [false, true] {
        let fixture = WaitFixture::new("late-declared-missing-owner");
        fixture.busy();
        let id = start(&fixture, "original-task");
        artifact(&fixture, true);
        {
            let mut inner = fixture.state.inner.lock().unwrap();
            let record = inner.session_mut(&fixture.session).unwrap();
            record.active_turn_generation += 1;
            if replacement_trace {
                let trace = record.engram.unadmitted_active_turn.as_mut().unwrap();
                trace.generation = record.active_turn_generation;
                trace.host_turn_ref = "successor-turn".into();
            } else {
                record.engram.unadmitted_active_turn = None;
            }
        }
        fixture.register(&[&id], "all").unwrap();
        fixture.state.note_engram_reported_end(
            &fixture.session,
            "original-task",
            DECLARED,
            "exit zero",
            EngramCommandExit::Code(0),
        );
        fixture.state.refresh_test_run_waits();
        let prompts = fixture.queued_prompts();
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains("- Verdict: UNKNOWN"));
        let inner = fixture.state.inner.lock().unwrap();
        let row = &inner.test_run_observations[&id];
        assert_eq!(row.summary.state, TestRunState::Unknown);
        assert_eq!(
            row.summary.error.as_deref(),
            Some("declared late completion has no retained original artifact binding")
        );
        assert!(!row.declared_verdict_pending);
    }
}

#[test]
fn late_declared_completion_cannot_borrow_another_command_or_replaced_runtime() {
    for replace_runtime in [false, true] {
        let fixture = WaitFixture::new("late-declared-identity");
        let id = start(&fixture, "identity-task");
        artifact(&fixture, true);
        if replace_runtime {
            let (runtime, _commands) = test_codex_runtime_handle("replacement-runtime");
            let mut inner = fixture.state.inner.lock().unwrap();
            inner.session_mut(&fixture.session).unwrap().runtime = SessionRuntime::Codex(runtime);
        }
        fixture.state.note_engram_reported_end(
            &fixture.session,
            "identity-task",
            "flutter test --wrong-command",
            "not the original result",
            EngramCommandExit::Code(0),
        );
        let inner = fixture.state.inner.lock().unwrap();
        let row = &inner.test_run_observations[&id];
        assert_eq!(
            row.summary.state,
            if replace_runtime {
                TestRunState::Running
            } else {
                TestRunState::Unknown
            }
        );
        assert_eq!(row.summary.ended_at.is_none(), replace_runtime);
        if !replace_runtime {
            assert_eq!(
                row.summary.error.as_deref(),
                Some("declared late completion does not match its original command")
            );
        }
    }
}
