// Foreground declarations with no project trace: drive the actual start/END
// producer and run-wait queue, using captured reporter artifacts, not Flutter.
use super::*;

const COMMAND: &str = "flutter test --file-reporter json:.termal-results/flutter.json";
const PASS: &[u8] = include_bytes!("../fixtures/declared-dart/flutter-pass.jsonl");
const FAIL: &[u8] = include_bytes!("../fixtures/declared-dart/flutter-fail.jsonl");
const INCOMPLETE: &[u8] = include_bytes!("../fixtures/declared-dart/flutter-truncated.jsonl");

fn fixture(observability_off: bool) -> WaitFixture {
    let fixture = WaitFixture::new("no-trace-declared");
    fs::write(fixture.root.join(".gitignore"), ".termal-results/\n").unwrap();
    fs::write(fixture.root.join("termal-tests.toml"), format!(
        "[[test]]\ncommand = '{COMMAND}'\ncwd = '.'\nartifact = '.termal-results/flutter.json'\n"
    )).unwrap();
    let (runtime, _commands) = test_codex_runtime_handle("no-trace-declared-runtime");
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        let project_id = inner.sessions[index].session.project_id.clone().unwrap();
        let project = inner
            .projects
            .iter_mut()
            .find(|p| p.id == project_id)
            .unwrap();
        project.engram = observability_off.then(|| EngramProjectSettings {
            enabled: true,
            execution_observability: false,
            ..Default::default()
        });
        inner.sessions[index].session.agent = Agent::Codex;
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
        assert!(!ordinary_runtime_observation_enabled(
            &inner,
            &fixture.session
        ));
        start_ordinary_runtime_observation(&mut inner, index);
        assert!(
            inner.sessions[index]
                .engram
                .unadmitted_active_turn
                .is_none()
        );
    }
    fixture.busy();
    fixture
}

fn start(fixture: &WaitFixture, key: &str, command: &str) -> String {
    fixture.state.note_engram_command_started(
        &fixture.session,
        &EngramObservationProvenance::Ambient,
        key,
        Some(command),
        Some(&test_run_display_path(&fixture.root)),
    );
    let inner = fixture.state.inner.lock().unwrap();
    let row = inner
        .test_run_observations
        .values()
        .find(|row| {
            row.tool_identity == sha256_hex(key.as_bytes()) && row.summary.ended_at.is_none()
        })
        .expect("the production start must create a test observation");
    row.execution_id.clone()
}

fn artifact(fixture: &WaitFixture, bytes: Option<&[u8]>) {
    if let Some(bytes) = bytes {
        fs::create_dir_all(fixture.root.join(".termal-results")).unwrap();
        fs::write(fixture.root.join(".termal-results/flutter.json"), bytes).unwrap();
    }
}

fn end(fixture: &WaitFixture, key: &str, command: &str, exit: Option<EngramCommandExit>) {
    fixture.state.note_engram_command_finished(
        &fixture.session,
        &EngramObservationProvenance::Ambient,
        key,
        command,
        "the provider's console is not an artifact verdict",
        exit,
    );
}

fn assert_settled(fixture: &WaitFixture, id: &str, state: TestRunState, reason: Option<&str>) {
    {
        let inner = fixture.state.inner.lock().unwrap();
        let row = &inner.test_run_observations[id];
        assert_eq!(row.summary.state, state);
        assert!(row.summary.ended_at.is_some());
        assert!(!row.declared_verdict_pending);
        if let Some(reason) = reason {
            assert!(
                row.summary.error.as_deref().unwrap_or("").contains(reason),
                "{:?}",
                row.summary.error
            );
        }
        assert!(row.evidence.is_empty());
        assert!(row.declared_native.is_none());
        assert!(inner.test_run_registrations.get(id).is_none());
    }
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1);
    let verdict = match state {
        TestRunState::Passed => "PASS",
        TestRunState::Failed => "FAIL",
        TestRunState::Unknown => "UNKNOWN",
        _ => panic!("expected a terminal verdict"),
    };
    assert!(prompts[0].contains(id));
    assert!(
        prompts[0].contains(&format!("- Verdict: {verdict}")),
        "{}",
        prompts[0]
    );
    fixture.state.refresh_test_run_waits();
    assert_eq!(
        fixture.queued_prompts(),
        prompts,
        "the same END resumes once"
    );
}

#[test]
fn project_environment_late_description_retains_original_applied_names() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for off in [false, true] {
        for original_value in [None, Some(" \t"), Some("original-runtime-fixture-value")] {
            let fixture = fixture(off);
            let path = fixture.root.join("termal-tests.toml");
            let declaration = fs::read_to_string(&path).unwrap();
            fs::write(path, format!("{declaration}env = ['SUPABASE_TEST_URL']\n")).unwrap();
            let selection = |value: Option<&str>| AppliedProjectEnvironment {
                values: ProjectEnvironment(
                    value
                        .map(|value| ("SUPABASE_TEST_URL".to_owned(), value.to_owned()))
                        .into_iter()
                        .collect(),
                ),
            };
            let configured = original_value.is_some_and(|value| !value.trim().is_empty());
            let set_selection = |applied| {
                let mut inner = fixture.state.inner.lock().unwrap();
                let index = inner.find_session_index(&fixture.session).unwrap();
                inner.sessions[index].project_environment = applied;
            };
            set_selection(selection(original_value));
            let key = "late-environment-description";
            fixture.state.note_engram_command_started(
                &fixture.session,
                &EngramObservationProvenance::Ambient,
                key,
                None,
                Some(&test_run_display_path(&fixture.root)),
            );
            {
                let inner = fixture.state.inner.lock().unwrap();
                let pending = inner
                    .test_runs
                    .unclassified_commands
                    .values()
                    .next()
                    .unwrap();
                assert_eq!(
                    pending
                        .project_environment_present
                        .contains("SUPABASE_TEST_URL"),
                    configured
                );
                assert!(!format!("{pending:?}").contains("original-runtime-fixture-value"));
            }
            // A repeated placeholder and the later description must not borrow
            // a different selection applied since the original tool start.
            set_selection(selection(if configured {
                None
            } else {
                Some("later-runtime-value")
            }));
            fixture.state.note_engram_command_started(
                &fixture.session,
                &EngramObservationProvenance::Ambient,
                key,
                None,
                Some(&test_run_display_path(&fixture.root)),
            );
            for _ in 0..2 {
                assert_eq!(
                    fixture.state.observe_test_command_description(
                        &fixture.session,
                        &EngramObservationProvenance::Ambient,
                        key,
                        Some(COMMAND),
                        Some(&test_run_display_path(&fixture.root))
                    ),
                    TestCommandOriginalStart::ThisTurn
                );
            }
            let id = {
                let inner = fixture.state.inner.lock().unwrap();
                let rows: Vec<_> = inner
                    .test_run_observations
                    .values()
                    .filter(|row| row.tool_identity == sha256_hex(key.as_bytes()))
                    .collect();
                assert_eq!(
                    rows.len(),
                    1,
                    "duplicate descriptions keep one original observation"
                );
                let binding = rows[0].declared_binding.as_ref().unwrap();
                assert_eq!(binding.environment_missing.is_none(), configured);
                rows[0].execution_id.clone()
            };
            fixture.register(&[&id], "all").unwrap();
            artifact(&fixture, Some(PASS));
            end(&fixture, key, COMMAND, Some(EngramCommandExit::Code(0)));
            assert_settled(
                &fixture,
                &id,
                if configured {
                    TestRunState::Passed
                } else {
                    TestRunState::Unknown
                },
                (!configured).then_some("environment_missing:SUPABASE_TEST_URL"),
            );
        }
    }
}

#[test]
fn no_trace_foreground_rows_and_waits_use_the_existing_artifact_and_exit_judge() {
    let cases = [
        (
            Some(PASS),
            Some(EngramCommandExit::Code(0)),
            TestRunState::Passed,
            None,
        ),
        (
            Some(PASS),
            Some(EngramCommandExit::Code(7)),
            TestRunState::Failed,
            None,
        ),
        (
            Some(FAIL),
            Some(EngramCommandExit::Code(0)),
            TestRunState::Failed,
            None,
        ),
        (
            None,
            Some(EngramCommandExit::Code(0)),
            TestRunState::Unknown,
            Some("missing at the end"),
        ),
        (
            Some(b"{invalid JSON".as_slice()),
            Some(EngramCommandExit::Code(0)),
            TestRunState::Unknown,
            Some("malformed or unknown event"),
        ),
        (
            Some(INCOMPLETE),
            Some(EngramCommandExit::Code(0)),
            TestRunState::Unknown,
            Some("done event"),
        ),
        (
            Some(PASS),
            None,
            TestRunState::Unknown,
            Some("without an exit status"),
        ),
        (
            Some(PASS),
            Some(EngramCommandExit::Unknown),
            TestRunState::Unknown,
            Some("without an exit status"),
        ),
        (
            Some(PASS),
            Some(EngramCommandExit::ReportedSuccess),
            TestRunState::Passed,
            None,
        ),
    ];
    for off in [false, true] {
        for (bytes, exit, expected, reason) in cases {
            let fixture = fixture(off);
            let id = start(&fixture, "foreground", COMMAND);
            {
                let inner = fixture.state.inner.lock().unwrap();
                assert!(inner.test_run_observations[&id].declared_binding.is_some());
            }
            assert!(
                !fixture
                    .register(&[&id], "all")
                    .unwrap()
                    .resume_prompt_queued
            );
            artifact(&fixture, bytes);
            end(&fixture, "foreground", COMMAND, exit);
            assert_settled(&fixture, &id, expected, reason);
            let inner = fixture.state.inner.lock().unwrap();
            let row = &inner.test_run_observations[&id];
            assert_eq!(
                row.summary.exit_code,
                match exit {
                    Some(EngramCommandExit::Code(code)) => Some(code),
                    _ => None,
                }
            );
        }
    }
}

#[test]
fn no_trace_production_end_persists_settlement_before_a_wait_can_consume_it() {
    for bytes in [Some(FAIL), None] {
        let fixture = fixture(false);
        let id = start(&fixture, "foreground", COMMAND);
        fixture.register(&[&id], "all").unwrap();
        artifact(&fixture, bytes);
        let reached = std::cell::Cell::new(false);
        fixture
            .state
            .note_engram_command_finished_reported_with_observation_hook(
                &fixture.session,
                &EngramObservationProvenance::Ambient,
                "foreground",
                COMMAND,
                Some(COMMAND),
                "",
                Some(EngramCommandExit::Code(0)),
                false,
                || {
                    reached.set(true);
                    let row =
                        fixture.state.inner.lock().unwrap().test_run_observations[&id].clone();
                    assert!(row.declared_verdict_pending);
                    assert_eq!(
                        row.summary.state,
                        TestRunState::Passed,
                        "exercise the provisional PASS window"
                    );
                    fixture.state.refresh_test_run_waits();
                    assert!(fixture.queued_prompts().is_empty());
                    assert_eq!(fixture.pending_waits().len(), 1);
                },
            );
        assert!(reached.get(), "must exercise the production END choice");
        assert_settled(
            &fixture,
            &id,
            if bytes.is_some() {
                TestRunState::Failed
            } else {
                TestRunState::Unknown
            },
            None,
        );
    }
}

#[test]
fn no_trace_completion_never_reconstructs_a_missing_original_binding() {
    let fixture = fixture(false);
    let id = start(&fixture, "foreground", COMMAND);
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_observations
        .update(&id, |row| {
            row.declared_binding = None;
        });
    // A fresh bind would read FAIL; the absent original must remain UNKNOWN.
    artifact(&fixture, Some(FAIL));
    fixture.register(&[&id], "all").unwrap();
    end(
        &fixture,
        "foreground",
        COMMAND,
        Some(EngramCommandExit::Code(0)),
    );
    assert_settled(
        &fixture,
        &id,
        TestRunState::Unknown,
        Some("no retained original artifact binding"),
    );
}

#[test]
fn no_trace_mismatched_completion_and_changed_declaration_fail_closed() {
    for changed_declaration in [false, true] {
        let fixture = fixture(false);
        let id = start(&fixture, "foreground", COMMAND);
        artifact(&fixture, Some(PASS));
        if changed_declaration {
            fs::write(
                fixture.root.join("termal-tests.toml"),
                "# replaced after start\n",
            )
            .unwrap();
        }
        fixture.register(&[&id], "all").unwrap();
        end(
            &fixture,
            "foreground",
            if changed_declaration {
                COMMAND
            } else {
                "cargo test"
            },
            Some(EngramCommandExit::Code(0)),
        );
        assert_settled(
            &fixture,
            &id,
            TestRunState::Unknown,
            Some(if changed_declaration {
                "declaration changed"
            } else {
                "does not match its original command"
            }),
        );
    }
}

#[test]
fn no_trace_builtin_commands_keep_their_plain_exit_outcome() {
    let fixture = fixture(false);
    let id = start(&fixture, "builtin", "cargo test");
    assert!(
        fixture.state.inner.lock().unwrap().test_run_observations[&id]
            .declared_artifact
            .is_none()
    );
    fixture.register(&[&id], "all").unwrap();
    end(
        &fixture,
        "builtin",
        "cargo test",
        Some(EngramCommandExit::Code(0)),
    );
    assert_settled(&fixture, &id, TestRunState::Passed, None);
}

#[test]
fn a_traced_foreground_completion_keeps_its_existing_trace_owner() {
    let fixture = fixture(false);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        let record = &mut inner.sessions[index];
        record.engram.unadmitted_active_turn = Some(EngramRuntimeObservedTurn::started(
            "existing-trace".into(),
            record.active_turn_generation,
        ));
    }
    let id = start(&fixture, "traced", COMMAND);
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_observations
        .update(&id, |row| {
            row.declared_binding = None;
        });
    artifact(&fixture, Some(FAIL));
    fixture.register(&[&id], "all").unwrap();
    end(
        &fixture,
        "traced",
        COMMAND,
        Some(EngramCommandExit::Code(0)),
    );
    assert_settled(&fixture, &id, TestRunState::Failed, None);
}

#[test]
fn a_duplicate_no_trace_end_cannot_read_a_replacement_artifact() {
    let fixture = fixture(false);
    let id = start(&fixture, "foreground", COMMAND);
    artifact(&fixture, Some(FAIL));
    fixture.register(&[&id], "all").unwrap();
    end(
        &fixture,
        "foreground",
        COMMAND,
        Some(EngramCommandExit::Code(0)),
    );
    assert_settled(&fixture, &id, TestRunState::Failed, None);
    artifact(&fixture, Some(PASS));
    end(
        &fixture,
        "foreground",
        COMMAND,
        Some(EngramCommandExit::Code(0)),
    );
    assert_settled(&fixture, &id, TestRunState::Failed, None);
}
