// Recognized commands refused at worktree admission must leave feedback.
// Drive the Codex item handlers and checkpoint without running a real gate.
use super::*;

const LAUNCHER: &str = "node scripts/test-launcher.mjs focused -- node --test";

fn named_turn(label: &str) -> (CheckedTurn, String) {
    let turn = CheckedTurn::start_with_opening(
        label,
        true,
        None,
        vec![checkpoint_reply(CHECK_GRANT)],
        1,
        true,
    );
    let root = turn.record(|record| {
        record
            .engram
            .active_turn_source_root
            .as_ref()
            .unwrap()
            .root
            .clone()
    });
    (turn, root)
}

fn command_item(command: &str, cwd: &str) -> Value {
    json!({"type":"commandExecution", "id":"focused", "command":command,
        "cwd":cwd, "status":"inProgress"})
}

fn complete(turn: &CheckedTurn, recorder: &mut SessionRecorder, item: &Value) {
    let mut ended = item.clone();
    ended["status"] = json!("completed");
    ended["exitCode"] = json!(0);
    ended["aggregatedOutput"] = json!("PASS synthetic exit=0\nfocused: passed\n");
    test_codex_item_completed(
        &ended,
        &turn.state,
        &turn.session_id,
        &mut CodexTurnState::default(),
        recorder,
    )
    .unwrap();
}

fn take_notice(turn: &CheckedTurn) -> Option<String> {
    let mut notice = None;
    turn.record_mut(|record| notice = record.engram.pending_source_root_line.take());
    notice
}

fn acp_update(
    turn: &CheckedTurn,
    recorder: &mut SessionRecorder,
    turn_state: &mut AcpTurnState,
    update: &Value,
) {
    let (input_tx, _input_rx) = std::sync::mpsc::channel();
    test_acp_session_update(
        update,
        &turn.state,
        &turn.session_id,
        &input_tx,
        turn_state,
        recorder,
        AcpAgent::Cursor,
    )
    .expect("the ACP update should apply");
}

#[test]
fn refused_admission_acp_first_description_reports_arguments_once_without_a_late_check() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for status in [None, Some("completed")] {
        let (turn, root) = named_turn("acp-first-refused-description");
        let command = format!("{LAUNCHER} --test-name-pattern ^MCP fixture.test.mjs");
        let mut recorder = turn.recorder();
        let mut turn_state = AcpTurnState::default();
        acp_update(
            &turn,
            &mut recorder,
            &mut turn_state,
            &json!({"sessionUpdate":"tool_call", "toolCallId":"described",
                "title":"Run tests", "kind":"execute", "status":"in_progress"}),
        );
        assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
        assert_eq!(take_notice(&turn), None);
        let mut update = json!({"sessionUpdate":"tool_call_update", "toolCallId":"described",
            "rawInput":{"command":command, "cwd":root},
            "rawOutput":{"exitCode":0}});
        if let Some(status) = status {
            update["status"] = json!(status);
        }
        acp_update(&turn, &mut recorder, &mut turn_state, &update);
        let notice = take_notice(&turn).expect("first description must explain refused arguments");
        assert!(
            notice.contains("unquoted shell expression or escape"),
            "{notice}"
        );
        assert!(notice.contains("Quote literal patterns"), "{notice}");
        assert!(notice.contains("no Test record will follow"), "{notice}");
        acp_update(&turn, &mut recorder, &mut turn_state, &update);
        update["status"] = json!("completed");
        acp_update(&turn, &mut recorder, &mut turn_state, &update);
        assert_eq!(take_notice(&turn), None, "one notice per command key");
        assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
        let checkpoint = turn.finish();
        assert!(
            checkpoint.get("verification_evidence").is_none(),
            "{checkpoint:#}"
        );
        assert_eq!(
            observations(&checkpoint).len(),
            1,
            "only the turn observation"
        );
    }
}

#[test]
fn refused_admission_acp_changed_description_drops_the_check_and_reports_arguments_once() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for status in [None, Some("completed")] {
        let (turn, root) = named_turn("acp-changed-refused-description");
        let command = format!("{LAUNCHER} --test-name-pattern \"^MCP\" fixture.test.mjs");
        let mut recorder = turn.recorder();
        let mut turn_state = AcpTurnState::default();
        acp_update(
            &turn,
            &mut recorder,
            &mut turn_state,
            &json!({"sessionUpdate":"tool_call", "toolCallId":"changed",
                "title":"Run tests", "kind":"execute", "status":"in_progress",
                "rawInput":{"command":command, "cwd":root}}),
        );
        turn.wait_for_snapshots();
        assert_eq!(
            turn.record(|record| record.engram.active_turn_checks.len()),
            1
        );
        assert_eq!(take_notice(&turn), None, "valid start remains admitted");
        let refused = format!("{LAUNCHER} --test-name-pattern ^MCP fixture.test.mjs");
        let mut update = json!({"sessionUpdate":"tool_call_update", "toolCallId":"changed",
            "rawInput":{"command":refused, "cwd":root},
            "rawOutput":{"exitCode":0}});
        if let Some(status) = status {
            update["status"] = json!(status);
        }
        acp_update(&turn, &mut recorder, &mut turn_state, &update);
        let notice =
            take_notice(&turn).expect("changed description must explain refused arguments");
        assert!(
            notice.contains("unquoted shell expression or escape"),
            "{notice}"
        );
        assert!(notice.contains("no Test record will follow"), "{notice}");
        assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
        acp_update(&turn, &mut recorder, &mut turn_state, &update);
        update["status"] = json!("completed");
        acp_update(&turn, &mut recorder, &mut turn_state, &update);
        assert_eq!(take_notice(&turn), None, "one notice per command key");
        let checkpoint = turn.finish();
        assert!(
            checkpoint.get("verification_evidence").is_none(),
            "{checkpoint:#}"
        );
        assert_eq!(
            observations(&checkpoint).len(),
            1,
            "only the turn observation"
        );
    }
}

#[test]
fn refused_admission_acp_invalidation_before_refusal_does_not_consume_the_notice() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for status in [None, Some("completed")] {
        let (turn, root) = named_turn("acp-invalidated-before-refusal");
        let mut recorder = turn.recorder();
        let mut turn_state = AcpTurnState::default();
        for call in 0..2 {
            // Two actual ACP creations have protocol-unique toolCallIds.
            // The second call retains its own first refusal and admission.
            let call_id = format!("invalidated-{call}");
            let command = format!("{LAUNCHER} --test-name-pattern \"^MCP\" fixture.test.mjs");
            acp_update(
                &turn,
                &mut recorder,
                &mut turn_state,
                &json!({"sessionUpdate":"tool_call", "toolCallId":call_id,
                    "kind":"execute", "status":"in_progress",
                    "rawInput":{"command":command, "cwd":root}}),
            );
            turn.wait_for_snapshots();
            assert_eq!(
                turn.record(|record| record.engram.active_turn_checks.len()),
                1
            );
            let changed = format!("{LAUNCHER} --test-name-pattern \"^MCP.close\" fixture.test.mjs");
            acp_update(
                &turn,
                &mut recorder,
                &mut turn_state,
                &json!({"sessionUpdate":"tool_call_update", "toolCallId":call_id,
                    "rawInput":{"command":changed, "cwd":root}}),
            );
            assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
            assert_eq!(
                take_notice(&turn),
                None,
                "the changed valid command was not refused"
            );
            let refused = format!("{LAUNCHER} --test-name-pattern ^MCP.close fixture.test.mjs");
            let mut update = json!({"sessionUpdate":"tool_call_update", "toolCallId":call_id,
                "rawInput":{"command":refused, "cwd":root}, "rawOutput":{"exitCode":0}});
            if let Some(status) = status {
                update["status"] = json!(status);
            }
            acp_update(&turn, &mut recorder, &mut turn_state, &update);
            let notice =
                take_notice(&turn).expect("invalidation cannot suppress the first refusal");
            assert!(
                notice.contains("unquoted shell expression or escape"),
                "{notice}"
            );
            assert!(notice.contains("no Test record will follow"), "{notice}");
            acp_update(&turn, &mut recorder, &mut turn_state, &update);
            update["status"] = json!("completed");
            acp_update(&turn, &mut recorder, &mut turn_state, &update);
            assert_eq!(
                take_notice(&turn),
                None,
                "duplicate refusal must stay suppressed"
            );
            assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
        }
        let checkpoint = turn.finish();
        assert!(
            checkpoint.get("verification_evidence").is_none(),
            "{checkpoint:#}"
        );
        assert_eq!(
            observations(&checkpoint).len(),
            1,
            "only the turn observation"
        );
    }
}

#[test]
fn refused_admission_codex_caret_and_arguments_have_one_actionable_outcome() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for (suffix, reason, remedy) in [
        (
            "--test-name-pattern ^MCP fixture.test.mjs",
            "unquoted shell expression or escape",
            "Quote literal patterns",
        ),
        (
            "--test-name-pattern=^MCP.close.deadline fixture.test.mjs",
            "unquoted shell expression or escape",
            "Quote literal patterns",
        ),
        (
            "\"../outside.test.mjs\"",
            "an argument names a path outside",
            "Avoid those expansion characters",
        ),
        (
            "\"$env:SUITE\"",
            "unsupported expansion syntax",
            "Avoid those expansion characters",
        ),
    ] {
        for wrapped in [false, true] {
            let (turn, root) = named_turn("refused-arguments");
            let inner = format!("{LAUNCHER} {suffix}");
            let command = if wrapped {
                // The PowerShell wrapper seen in the original host transcript.
                format!("\"C:\\Program Files\\PowerShell\\7\\pwsh.exe\" -Command '{inner}'")
            } else {
                inner
            };
            let check = engram_check_command(&command).expect("recognized before admission");
            assert!(
                engram_check_worktree_in(
                    &check,
                    &turn.root,
                    Some(&root),
                    turn.record(|record| record.engram.active_turn_source_root.clone())
                        .as_ref()
                        .map(|named| (FsPath::new(&named.root), named.common_dir_key.as_str()))
                )
                .is_none()
            );
            let item = command_item(&command, &root);
            let mut recorder = turn.recorder();
            test_codex_item_started(&item, &mut recorder).unwrap();
            let notice = take_notice(&turn).expect("a refused check must tell its caller why");
            assert!(
                notice.contains(reason) && notice.contains(remedy),
                "{notice}"
            );
            assert!(notice.contains("no Test record will follow"), "{notice}");
            assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));

            test_codex_item_started(&item, &mut recorder).unwrap();
            recorder
                .command_described("focused", Some(&command), Some(&root))
                .unwrap();
            complete(&turn, &mut recorder, &item);
            complete(&turn, &mut recorder, &item);
            assert_eq!(
                take_notice(&turn),
                None,
                "repeated lifecycle events must not repeat feedback"
            );
            let checkpoint = turn.finish();
            assert!(
                checkpoint.get("verification_evidence").is_none(),
                "{checkpoint:#}"
            );
            assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
            let rows = observations(&checkpoint);
            // The named turn still reports its own source observation. The
            // refused command must contribute no additional check outcome.
            assert_eq!(rows.len(), 1, "only the turn observation: {checkpoint:#}");
            assert_eq!(
                rows[0]["observation_id"],
                engram_turn_observation_id(&turn.session_id, CHECK_GRANT),
                "a refusal cannot create a check execution observation: {checkpoint:#}"
            );
            assert_ne!(
                rows[0]["action_fingerprint"],
                engram_check_fingerprint(&check),
                "the turn observation must not credit the refused command"
            );
        }
    }
}

#[test]
fn refused_admission_quoted_caret_codex_control_still_records() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, root) = named_turn("quoted-caret");
    let command = format!("{LAUNCHER} --test-name-pattern \"^MCP\" fixture.test.mjs");
    let item = command_item(&command, &root);
    let mut recorder = turn.recorder();
    test_codex_item_started(&item, &mut recorder).unwrap();
    turn.wait_for_snapshots();
    assert!(turn.record(|record| record.engram.active_turn_checks.len() == 1));
    assert_eq!(take_notice(&turn), None);
    complete(&turn, &mut recorder, &item);
    turn.wait_for_snapshots();
    // Synthetic stdout is not producer evidence: admission still yields one
    // honest Unknown verification, never PASS without validated artifacts.
    one_node_test(&turn.finish(), "unknown");
}

#[test]
fn refused_admission_external_root_codex_control_stays_refused() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, _) = named_turn("external-root");
    let command = format!("{LAUNCHER} --test-name-pattern \"^MCP\" fixture.test.mjs");
    let item = command_item(&command, &turn.root.to_string_lossy());
    let mut recorder = turn.recorder();
    test_codex_item_started(&item, &mut recorder).unwrap();
    let notice = take_notice(&turn).expect("outside-root placement remains refused");
    assert!(
        notice.contains("outside") && notice.contains("run the tests in that root"),
        "{notice}"
    );
    complete(&turn, &mut recorder, &item);
    assert_eq!(take_notice(&turn), None);
    assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
    assert!(turn.finish().get("verification_evidence").is_none());
}

#[test]
fn refused_admission_accumulated_test_and_build_notices_preserve_root_guidance() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for named in [false, true] {
        let (turn, root) = if named {
            named_turn("refusal-retention")
        } else {
            let turn = CheckedTurn::start("unnamed-refusal-retention", true);
            let root = turn.root.to_string_lossy().into_owned();
            (turn, root)
        };
        let bind = engram_source_root_line(None, "fixture work", &root, false);
        turn.record_mut(|record| record.engram.set_pending_source_root_line(bind.clone()));
        let mut recorder = turn.recorder();
        for index in 0..8 {
            let command = if index % 2 == 0 {
                format!("{LAUNCHER} --test-name-pattern ^case{index} fixture.test.mjs")
            } else {
                format!("cargo build ^case{index}")
            };
            let mut item = command_item(&command, &root);
            item["id"] = json!(format!("refusal-{index}"));
            test_codex_item_started(&item, &mut recorder).unwrap();
            test_codex_item_started(&item, &mut recorder).unwrap();
            let pending = turn
                .record(|record| record.engram.pending_source_root_line.clone())
                .unwrap();
            let lines = pending.lines().collect::<Vec<_>>();
            assert_eq!(
                lines.len(),
                2,
                "root guidance plus latest refusal: {pending}"
            );
            assert_eq!(lines[0], bind);
            assert!(engram_is_uncredited_check_line(lines[1]), "{pending}");
            assert!(lines[1].contains(&format!("^case{index}")), "{pending}");
            assert!(
                lines[1].contains(if index % 2 == 0 {
                    "no Test record will follow"
                } else {
                    "no Build record will follow"
                }),
                "{pending}"
            );
            complete(&turn, &mut recorder, &item);
        }
        assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
        assert!(turn.finish().get("verification_evidence").is_none());
    }
}

#[test]
fn refused_admission_placement_feedback_does_not_block_later_same_key_admission() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, root) = named_turn("late-placement");
    let command = format!("{LAUNCHER} --test-name-pattern \"^MCP\" fixture.test.mjs");
    let mut item = command_item(&command, &turn.root.to_string_lossy());
    let mut recorder = turn.recorder();
    test_codex_item_started(&item, &mut recorder).unwrap();
    assert!(take_notice(&turn).unwrap().contains("outside"));
    assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
    item["cwd"] = json!(root);
    test_codex_item_started(&item, &mut recorder).unwrap();
    turn.wait_for_snapshots();
    assert!(turn.record(|record| record.engram.active_turn_checks.len() == 1));
    assert!(turn.record(
        |record| record.engram.active_turn_checks[0].overlap_cause.as_deref()
            == Some("its command was named a test only by a later start")
    ));
    assert_eq!(take_notice(&turn), None);
    complete(&turn, &mut recorder, &item);
    turn.wait_for_snapshots();
    one_node_test(&turn.finish(), "unknown");
}

#[test]
fn refused_admission_quoted_expansion_characters_get_an_accurate_remedy() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let turn = CheckedTurn::start("literal-expansion-refusal", true);
    let root = turn.root.to_string_lossy().into_owned();
    for (index, value) in ["^MCP$", "100%", "case!", "~suite"].into_iter().enumerate() {
        let command = format!("{LAUNCHER} --test-name-pattern \"{value}\" fixture.test.mjs");
        let mut item = command_item(&command, &root);
        item["id"] = json!(format!("literal-{index}"));
        let mut recorder = turn.recorder();
        test_codex_item_started(&item, &mut recorder).unwrap();
        let notice = take_notice(&turn).expect("quoted expansion characters stay refused");
        assert!(
            notice.contains("unsupported expansion syntax") && notice.contains("even if quoted"),
            "{notice}"
        );
        assert!(
            notice.contains("Avoid those expansion characters"),
            "{notice}"
        );
        assert!(
            !notice.contains("named source root"),
            "unnamed-root guidance: {notice}"
        );
        complete(&turn, &mut recorder, &item);
    }
    assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
    assert!(turn.finish().get("verification_evidence").is_none());
}

#[test]
fn refused_admission_shared_root_notice_keeps_the_primary_check_admitted() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, root) = named_turn("shared-root-notice");
    let mut other = turn.state.inner.lock().unwrap().engram_work_source_roots[0].clone();
    other.short_ref = "other-held-item".to_owned();
    other.work_id = "other-work".to_owned();
    other.claim_id = "other-claim".to_owned();
    turn.record_mut(|record| record.engram.active_turn_other_source_roots.push(other));
    let command = format!("{LAUNCHER} --test-name-pattern \"^MCP\" fixture.test.mjs");
    let item = command_item(&command, &root);
    let mut recorder = turn.recorder();
    test_codex_item_started(&item, &mut recorder).unwrap();
    turn.wait_for_snapshots();
    let notice = take_notice(&turn).expect("the other held item receives routing guidance");
    assert!(
        notice.contains("shared named root") && notice.contains("other-held-item"),
        "{notice}"
    );
    assert!(turn.record(|record| record.engram.active_turn_checks.len() == 1));
    complete(&turn, &mut recorder, &item);
    turn.wait_for_snapshots();
    one_node_test(&turn.finish(), "unknown");
}
