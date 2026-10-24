// Lexical and recorder witnesses only; no batch program or shell is executed.
use super::*;

fn assert_pipe_is_not_eligible(command: &str) {
    let check = engram_check_command_in(command, EngramShellDialect::Bash)
        .expect("the runner remains observable as a test");
    assert!(
        !check.simple,
        "a downstream shell may reinterpret the pipe: {command}"
    );
    assert!(
        !check.literal_pipe,
        "no qualified pipe permission: {command}"
    );
    for exit in [
        EngramCommandExit::Code(0),
        EngramCommandExit::ReportedSuccess,
    ] {
        assert_eq!(
            engram_check_command_outcome(
                &check,
                exit,
                &["test result: ok. 1 passed; 0 failed".to_owned()]
            ),
            Some(EngramExecutionOutcome::Unknown),
            "{command}"
        );
    }
    let directory = if cfg!(windows) {
        "C:/termal-tests"
    } else {
        "/termal-tests"
    };
    assert!(
        engram_check_command_in(
            &format!("pushd \"{directory}\" && {command}"),
            EngramShellDialect::Bash
        )
        .is_none(),
        "one-call cannot grant shell containment"
    );
}

#[test]
fn medium_repair_batch_shims_cannot_gain_literal_pipe_permission() {
    for command in [
        "npx.cmd vitest run -t 'x|ver'",
        "npm.CmD test -- -t 'x|ver'",
        "pnpm.bat test -- -t 'x|ver'",
        "yarn.BAT test -t 'x|ver'",
        "cargo.cmd test -- 'x|ver'",
        "pytest.bat -k 'x|ver'",
        "'C:/runner path/npx.CMD' vitest run -t 'x|ver'",
        "node.cmd scripts/test-launcher.mjs focused -- node --test --test-name-pattern='x|ver' fixture.mjs",
    ] {
        assert_pipe_is_not_eligible(command);
    }
}

#[test]
fn medium_repair_script_runners_do_not_infer_their_configured_shell() {
    for command in [
        "npm test -- -t 'x|ver'",
        "npm.exe run test -- -t 'x|ver'",
        "pnpm test -- -t 'x|ver'",
        "yarn run test -t 'x|ver'",
        "npx vitest run -t 'x|ver'",
        "node scripts/test-launcher.mjs focused -- npm.cmd test -- -t 'x|ver'",
        "node scripts/test-launcher.mjs focused -- pnpm test -- -t 'x|ver'",
    ] {
        assert_pipe_is_not_eligible(command);
    }
}

#[test]
fn medium_repair_batch_success_report_cannot_forge_passing_credit() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let turn = CheckedTurn::start("quoted-batch-shim-forgery", true);
    let provenance = turn.record(|record| {
        EngramObservationProvenance::Claude(ClaudeObservationProvenance {
            token: record.runtime.runtime_token().unwrap(),
            origin: ClaudeWorkOrigin::Attempt {
                turn_generation: record.active_turn_generation,
            },
        })
    });
    let command = "npm.cmd test -- -t 'x|ver'";
    let mut recorder = turn.recorder();
    recorder.set_observation_provenance(provenance);
    recorder
        .command_started_in(
            "batch",
            command,
            Some(command),
            Some(&turn.root.to_string_lossy()),
        )
        .unwrap();
    turn.wait_for_snapshots();
    recorder
        .command_completed_with_exit(
            "batch",
            command,
            "Tests: 1 passed, 1 total",
            CommandStatus::Success,
            EngramCommandExit::ReportedSuccess,
        )
        .unwrap();
    turn.wait_for_snapshots();
    let checkpoint = turn.finish();
    one_node_test(&checkpoint, "unknown");
    for row in checkpoint["verification_evidence"].as_array().unwrap() {
        assert!(row["refs"]
            .as_array()
            .unwrap()
            .contains(&json!(format!("command:{command}"))));
        let id = &row["producer_observation"]["observation_id"];
        let producers = observations(&checkpoint)
            .into_iter()
            .filter(|producer| &producer["observation_id"] == id)
            .collect::<Vec<_>>();
        assert_eq!(
            producers.len(),
            1,
            "the evidence must name its exact producer"
        );
        assert_eq!(
            producers[0]["outcome"], "unknown",
            "no passing shim credit: {checkpoint:#}"
        );
        assert!(row["summary"]
            .as_str()
            .unwrap()
            .contains("pipe containment validation rejected"));
    }
}

#[test]
fn medium_repair_native_executable_keeps_quoted_pipe_outcomes() {
    for command in ["cargo test -- 'x|ver'", "cargo.exe test -- 'x|ver'"] {
        let check = engram_check_command_in(command, EngramShellDialect::Bash).unwrap();
        assert!(check.simple && check.literal_pipe);
        for (code, outcome) in [
            (0, EngramExecutionOutcome::Succeeded),
            (1, EngramExecutionOutcome::Failed),
        ] {
            assert_eq!(
                engram_check_command_outcome(
                    &check,
                    EngramCommandExit::Code(code),
                    &["test result: ok. 1 passed; 0 failed".to_owned()]
                ),
                Some(outcome)
            );
        }
    }
}
