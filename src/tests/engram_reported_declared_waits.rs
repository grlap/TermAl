// Real turn carrying and shared Codex late-item ingress, observed by the existing
// wait. Does not synthesize grants, launcher files or verification records.
use super::*;

const DECLARED: &str = "flutter test --file-reporter json:.termal-results/flutter.json";

fn carried_wait() -> (EOCheckedTurn, PathBuf, String) {
    let (turn, worktree) = two_turns("late-declared-carried-wait");
    fs::write(worktree.join(".gitignore"), ".termal-results/\n").unwrap();
    fs::write(worktree.join("termal-tests.toml"), format!(
        "[[test]]\ncommand = '{DECLARED}'\ncwd = '.'\nartifact = '.termal-results/flutter.json'\n"
    )).unwrap();
    run_git_test_command(&worktree, &["add", "--", ".gitignore", "termal-tests.toml"]);
    run_git_test_command(
        &worktree,
        &["commit", "--quiet", "-m", "declare test fixture"],
    );
    turn.recorder()
        .command_started_in(ITEM, DECLARED, Some(DECLARED), Some(&cwd(&worktree)))
        .expect("the original declared start is observed");
    turn.wait_for_snapshots();
    let id = {
        let inner = turn.state.inner.lock().unwrap();
        let row = inner
            .test_run_observations
            .values()
            .find(|row| row.tool_identity == sha256_hex(ITEM.as_bytes()))
            .unwrap();
        assert!(row.declared_artifact.is_some());
        row.execution_id.clone()
    };
    turn.finish();
    turn.record(|record| {
        let carried = record
            .engram
            .carried_checks
            .iter()
            .find(|carried| carried.check.key == ITEM)
            .expect("original test is carried");
        assert!(carried.check.declared.is_some());
    });
    dispatch_next_turn(&turn);
    turn.record(|record| {
        assert!(
            record
                .engram
                .unadmitted_active_turn
                .as_ref()
                .is_none_or(|trace| trace.commands.iter().all(|entry| entry.tool_key != ITEM)),
            "the successor trace cannot supply the original binding"
        );
    });
    let registered = turn
        .state
        .create_test_run_wait_with(
            &turn.session_id,
            serde_json::from_value(json!({"runIds": [&id]})).unwrap(),
            &|| {},
        )
        .unwrap();
    assert!(!registered.resume_prompt_queued);
    (turn, worktree, id)
}

fn artifact(worktree: &FsPath, passed: bool) {
    fs::create_dir_all(worktree.join(".termal-results")).unwrap();
    fs::write(
        worktree.join(".termal-results/flutter.json"),
        if passed {
            include_bytes!("fixtures/declared-dart/flutter-pass.jsonl").as_slice()
        } else {
            include_bytes!("fixtures/declared-dart/flutter-fail.jsonl").as_slice()
        },
    )
    .unwrap();
}

fn resumes(turn: &EOCheckedTurn, id: &str) -> Vec<String> {
    let inner = turn.state.inner.lock().unwrap();
    inner.sessions[inner.find_session_index(&turn.session_id).unwrap()]
        .queued_prompts
        .iter()
        .filter(|prompt| prompt.pending_prompt.text.contains(id))
        .map(|prompt| prompt.pending_prompt.text.clone())
        .collect()
}

fn assert_settled(turn: &EOCheckedTurn, id: &str, passed: Option<bool>, exit: Option<i64>) {
    turn.state.refresh_test_run_waits();
    let expected = match passed {
        Some(true) => TestRunState::Passed,
        Some(false) => TestRunState::Failed,
        None => TestRunState::Unknown,
    };
    {
        let inner = turn.state.inner.lock().unwrap();
        let row = &inner.test_run_observations[id];
        assert_eq!(row.summary.state, expected);
        assert!(!row.declared_verdict_pending);
        assert_eq!(row.summary.exit_code, exit);
        assert!(row.evidence.is_empty());
    }
    let queued = resumes(turn, id);
    assert_eq!(queued.len(), 1);
    let label = match passed {
        Some(true) => "PASS",
        Some(false) => "FAIL",
        None => "UNKNOWN",
    };
    assert!(
        queued[0].contains(&format!("- Verdict: {label}")),
        "{}",
        queued[0]
    );
    let reading = turn.record(|record| {
        record
            .engram
            .carried_checks
            .iter()
            .find(|carried| carried.check.key == ITEM)
            .unwrap()
            .check
            .end
            .as_ref()
            .unwrap()
            .declared
            .clone()
            .unwrap()
    });
    let result = reading
        .wait_until(std::time::Instant::now() + DEADLOCK_GUARD)
        .expect("the carried check's artifact read completed");
    assert_eq!(
        match result.verdict {
            EngramTrxVerdict::Pass => "PASS",
            EngramTrxVerdict::Fail(_) => "FAIL",
            EngramTrxVerdict::Unknown(_) => "UNKNOWN",
        },
        label,
        "the same END must agree on its carried check, row and wait"
    );
    assert_no_automatic_credit(turn);
}

#[test]
fn a_carried_declared_test_wait_uses_its_original_binding_after_the_next_turn_starts() {
    for passed in [false, true] {
        let (turn, worktree, id) = carried_wait();
        artifact(&worktree, passed);
        let runtime = shared_runtime(&turn);
        report_late_end_on(&turn, &runtime, THREAD, ITEM, DECLARED, 0);
        assert_settled(&turn, &id, Some(passed), Some(0));
    }
}

#[test]
fn a_carried_declared_wait_holds_during_artifact_read_and_keeps_its_original_terminal_result() {
    for (passed, exit, expected) in [
        (Some(true), EngramCommandExit::Code(0), Some(true)),
        (Some(false), EngramCommandExit::Code(0), Some(false)),
        (None, EngramCommandExit::Code(0), None),
        (Some(true), EngramCommandExit::Code(7), Some(false)),
        (Some(true), EngramCommandExit::Unknown, None),
    ] {
        let (turn, worktree, id) = carried_wait();
        if let Some(passed) = passed {
            artifact(&worktree, passed);
        }
        let interposed = std::cell::Cell::new(false);
        turn.state.note_engram_reported_end_with_settlement(
            &turn.session_id,
            ITEM,
            DECLARED,
            "provider status is not the artifact verdict",
            exit,
            || {
                interposed.set(true);
                {
                    let inner = turn.state.inner.lock().unwrap();
                    let row = &inner.test_run_observations[&id];
                    assert!(row.summary.ended_at.is_some());
                    assert!(row.declared_verdict_pending);
                }
                turn.state.refresh_test_run_waits();
                assert!(resumes(&turn, &id).is_empty());
                assert_eq!(turn.state.inner.lock().unwrap().test_run_waits.len(), 1);
            },
        );
        assert!(interposed.get());
        assert_settled(
            &turn,
            &id,
            expected,
            match exit {
                EngramCommandExit::Code(code) => Some(code),
                _ => None,
            },
        );
        let before = turn.state.inner.lock().unwrap().test_run_observations[&id].clone();
        let queued = resumes(&turn, &id);
        artifact(&worktree, passed != Some(true));
        let runtime = shared_runtime(&turn);
        report_late_end_on(&turn, &runtime, THREAD, ITEM, DECLARED, 0);
        turn.state.refresh_test_run_waits();
        assert_eq!(
            turn.state.inner.lock().unwrap().test_run_observations[&id],
            before
        );
        assert_eq!(resumes(&turn, &id), queued);
    }
}
