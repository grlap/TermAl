// Diagnostic outcomes of launcher lifecycles that cannot earn verification.
// Uses the carried-check module's real recorder and named-root fixtures;
// owns the missing-claim and missing-grant boundary witnesses, not credit.

mod credit_corrections {
    use super::*;
    include!("engram_carried_credit_corrections.rs");
}

fn same_claim_refresh_outcome(label: &str, recovered_after_lapse: bool) {
    let (turn, worktree) = three_turns_with_a_terminal_carried_run(label);
    let original = turn.record(|record| record.engram.work_binding.clone().unwrap());
    let mut refreshed = original.clone();
    refreshed.work_revision += 1;
    if recovered_after_lapse {
        refreshed.claim_fence += 1;
    }
    *turn.transport.work_bindings.lock().unwrap() = [Ok(Some(refreshed.clone()))].into();
    turn.transport
        .responses
        .lock()
        .unwrap()
        .push_front(bind_reply("refreshed-a-token"));
    turn.transport.replace_held_claims([Ok(EngramHeldClaims {
        items: vec![EngramHeldClaim {
            work_id: refreshed.work_id.clone(),
            claim_id: refreshed.claim_id.clone(),
            claim_fence: refreshed.claim_fence,
            focused: true,
            control_binding: Some(refreshed.clone()),
            ..EngramHeldClaim::default()
        }],
        omitted: 0,
    })]);
    let run = review_runs(&worktree).join("test-carried");
    let checkpoint = finish_next_turn(&turn);
    if recovered_after_lapse {
        assert_refused(
            &turn,
            &checkpoint,
            "its claim is no longer the one this session holds",
        );
    } else {
        assert_credited(&turn, &checkpoint, &run, "succeeded");
    }
    assert_eq!(
        turn.record(|record| record.engram.work_binding.clone()),
        Some(refreshed)
    );
}

#[test]
fn a_live_revision_renewal_preserves_the_launch_claims_credit() {
    same_claim_refresh_outcome("live-renewal-credit", false);
}

#[test]
fn a_same_id_recovery_refuses_credit_launched_before_authority_lapsed() {
    // Engram preserves claim_id on recovery but advances its fence. A retained
    // old naming cannot prove that authority remained held through the gate.
    same_claim_refresh_outcome("recovered-fence-credit", true);
}

#[test]
fn checkpoint_held_read_uncertainty_defers_but_complete_absence_refuses() {
    for case in ["error", "omitted", "absent"] {
        let (turn, worktree) = named_turn(&format!("held-read-{case}"));
        launch_gate(&turn, &worktree, true);
        finish_run(&worktree, "passed", &"f".repeat(64));
        turn.state.poll_engram_carried_runs();
        let other = test_control_work_binding(&format!("held-other-{case}"), 1);
        turn.record_mut(|record| record.engram.work_binding = Some(other.clone()));
        let read = if case == "error" {
            Err(EngramTransportError::transport(
                "scripted held read unavailable",
            ))
        } else {
            Ok(EngramHeldClaims {
                items: vec![EngramHeldClaim {
                    work_id: other.work_id.clone(),
                    claim_id: other.claim_id.clone(),
                    claim_fence: other.claim_fence,
                    focused: true,
                    control_binding: Some(other),
                    ..EngramHeldClaim::default()
                }],
                omitted: u64::from(case == "omitted"),
            })
        };
        turn.transport.replace_held_claims([read]);
        let checkpoint = turn.finish();
        if case == "absent" {
            assert_refused(
                &turn,
                &checkpoint,
                "its claim is no longer the one this session holds",
            );
        } else {
            assert!(
                checkpoint.get("verification_evidence").is_none(),
                "{checkpoint:#}"
            );
            assert_eq!(turn.record(|record| record.engram.carried_checks.len()), 1);
            let line = turn
                .record(|record| record.engram.pending_source_root_line.clone())
                .unwrap();
            assert!(line.contains("once its held claim is confirmed"), "{line}");
        }
    }
}

#[test]
fn first_post_gate_note_on_another_held_claim_defers_credit_to_launch_item() {
    const FOCUS_B_GRANT: &str = "focus-b-grant";
    const RETURN_A_GRANT: &str = "return-a-grant";
    let turn = CheckedTurn::start_with_opening(
        "held-focus-deferred",
        true,
        None,
        vec![
            checkpoint_reply(CHECK_GRANT),
            bind_reply("focus-b-token"),
            grant_reply(FOCUS_B_GRANT),
            begin_reply(FOCUS_B_GRANT),
            checkpoint_reply(FOCUS_B_GRANT),
            bind_reply("return-a-token"),
            grant_reply(RETURN_A_GRANT),
            begin_reply(RETURN_A_GRANT),
            checkpoint_reply(RETURN_A_GRANT),
        ],
        3,
        true,
    );
    let worktree = turn.root.join(".worktrees").join("wt");
    let launch_binding = turn.record(|record| record.engram.work_binding.clone().unwrap());
    let launch_named_root = turn.record(|record| record.engram.named_root.clone());
    let focused_binding = test_control_work_binding("held-focus-other", 1);

    // Both claims are named before launching; setup is not a post-result write.
    turn.record_mut(|record| record.engram.work_binding = Some(focused_binding.clone()));
    register_named_root(&turn, &worktree, 2);
    turn.record_mut(|record| {
        record.engram.work_binding = Some(launch_binding.clone());
        record.engram.named_root = launch_named_root;
    });
    turn.transport
        .enable_named_roots(&turn.session_id, &launch_binding);
    turn.transport.replace_held_claims([Ok(EngramHeldClaims {
        items: [&launch_binding, &focused_binding]
            .into_iter()
            .map(|binding| EngramHeldClaim {
                work_id: binding.work_id.clone(),
                short_ref: binding.work_id.clone(),
                claim_id: binding.claim_id.clone(),
                claim_fence: binding.claim_fence,
                focused: binding.claim_id == launch_binding.claim_id,
                control_binding: Some(binding.clone()),
            })
            .collect(),
        omitted: 0,
    })]);
    launch_gate(&turn, &worktree, true);
    let opening = turn.finish();
    assert!(opening.get("verification_evidence").is_none());
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    assert!(turn.record(|record| record.engram.carried_checks[0].terminal_digest.is_some()));

    // The held read after the note reports B's focus while retaining A's claim.
    let focus_b_claims = EngramHeldClaims {
        items: [&launch_binding, &focused_binding]
            .into_iter()
            .map(|binding| EngramHeldClaim {
                work_id: binding.work_id.clone(),
                short_ref: binding.work_id.clone(),
                claim_id: binding.claim_id.clone(),
                claim_fence: binding.claim_fence,
                focused: binding.claim_id == focused_binding.claim_id,
                control_binding: Some(binding.clone()),
            })
            .collect(),
        omitted: 0,
    };
    let selected_b = select_engram_held_binding(
        focus_b_claims.items.clone(),
        focus_b_claims.omitted,
        EngramBindingPreference {
            current: Some(&launch_binding),
            refused: &[],
        },
    );
    assert_eq!(selected_b, Some(focused_binding.clone()));
    *turn.transport.work_bindings.lock().unwrap() = [Ok(selected_b)].into();
    turn.transport
        .replace_held_claims([Ok(focus_b_claims.clone())]);

    // The first post-terminal tracker note selects held B. Its admission and
    // checkpoint use B's grant, never an intermediate checkpoint on A.
    let on_b = finish_next_turn(&turn);
    assert_eq!(on_b["grant_id"], FOCUS_B_GRANT, "{on_b:#}");
    assert!(
        on_b.get("verification_evidence").is_none(),
        "no gate evidence on B: {on_b:#}"
    );
    let line = turn.record(|record| record.engram.pending_source_root_line.clone());
    assert_eq!(
        turn.record(|record| record.engram.carried_checks.len()),
        1,
        "{line:?}"
    );
    let line = line.expect("a finished gate waiting for its launch item tells the holder");
    assert!(
        line.contains("finished") && line.contains("next checkpoint focused on"),
        "{line}"
    );
    assert!(
        line.contains(&launch_binding.work_id) || line.contains("w-carried"),
        "{line}"
    );
    assert!(!line.contains("earned no credit"), "{line}");
    turn.record(|record| assert!(!record.engram.carried_consumed_runs.contains(&run)));

    let mut focus_a_claims = focus_b_claims;
    for claim in &mut focus_a_claims.items {
        claim.focused = claim.claim_id == launch_binding.claim_id;
    }
    let selected_a = select_engram_held_binding(
        focus_a_claims.items.clone(),
        focus_a_claims.omitted,
        EngramBindingPreference {
            current: Some(&focused_binding),
            refused: &[],
        },
    );
    assert_eq!(selected_a, Some(launch_binding.clone()));
    *turn.transport.work_bindings.lock().unwrap() = [Ok(selected_a)].into();
    turn.transport.replace_held_claims([Ok(focus_a_claims)]);
    let on_a = finish_next_turn(&turn);
    assert_eq!(on_a["grant_id"], RETURN_A_GRANT, "{on_a:#}");
    assert_credited(&turn, &on_a, &run, "succeeded");
    assert_eq!(
        turn.record(|record| record.engram.work_binding.clone()),
        Some(launch_binding.clone())
    );
    // Engram's evidence writer derives the item's binding from the checkpoint
    // grant. The last real bind is A, and only A's checkpoint carries evidence.
    let last_bind = turn
        .transport
        .requests()
        .into_iter()
        .filter(|request| request.request["operation"] == "session_bind")
        .last()
        .unwrap()
        .request;
    assert_eq!(last_bind["work_binding"]["work_id"], launch_binding.work_id);
    assert_eq!(
        last_bind["work_binding"]["claim_id"],
        launch_binding.claim_id
    );
}

#[test]
fn terminal_carry_with_released_launch_claim_still_refuses() {
    let (turn, worktree) = named_turn("released-launch-claim");
    launch_gate(&turn, &worktree, true);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    turn.transport.replace_held_claims([Ok(EngramHeldClaims {
        items: Vec::new(),
        omitted: 0,
    })]);
    turn.record_mut(|record| {
        record.engram.work_binding = Some(test_control_work_binding("released-launch-other", 1))
    });
    let checkpoint = turn.finish();
    assert_refused(
        &turn,
        &checkpoint,
        "its claim is no longer the one this session holds",
    );
    turn.record(|record| assert!(record.engram.carried_consumed_runs.contains(&run)));
}

#[test]
fn terminal_carry_with_retained_old_root_refuses_a_different_current_claim() {
    let (turn, worktree) = named_turn("outcome-retained-claim");
    launch_gate(&turn, &worktree, true);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    let original_claim = turn.record(|record| record.engram.carried_checks[0].claim_id.clone());
    turn.record_mut(|record| {
        record.engram.work_binding.as_mut().unwrap().claim_id = "replacement-claim".to_owned();
    });
    let replacement = turn.record(|record| record.engram.work_binding.clone().unwrap());
    turn.transport.replace_held_claims([Ok(EngramHeldClaims {
        items: vec![EngramHeldClaim {
            work_id: replacement.work_id.clone(),
            claim_id: replacement.claim_id.clone(),
            claim_fence: replacement.claim_fence,
            focused: true,
            control_binding: Some(replacement),
            ..EngramHeldClaim::default()
        }],
        omitted: 0,
    })]);
    {
        let inner = turn.state.inner.lock().unwrap();
        assert!(
            inner
                .engram_work_source_roots
                .iter()
                .any(|entry| entry.claim_id == original_claim)
        );
    }
    let checkpoint = turn.finish();
    let line = turn
        .record(|record| record.engram.pending_source_root_line.clone())
        .expect("a terminal carry under another claim must report its refusal now");
    assert!(
        line.contains("its claim is no longer the one this session holds"),
        "{line}"
    );
    assert_refused(
        &turn,
        &checkpoint,
        "its claim is no longer the one this session holds",
    );
    turn.record(|record| assert!(record.engram.carried_consumed_runs.contains(&run)));
    turn.record_mut(|record| {
        let (credited, lines) =
            engram_take_settled_carried_checks(record, Vec::new(), &[], None, chrono::Utc::now());
        assert!(
            credited.is_empty() && lines.is_empty(),
            "a later drain must not repeat the refusal"
        );
    });
}

fn missing_authority_launch(
    label: &str,
    claimed: bool,
    has_grant: bool,
    detached: bool,
    gain_binding: bool,
) {
    let turn = CheckedTurn::start(label, claimed);
    let saved_binding = turn.record(|record| record.engram.work_binding.clone());
    turn.record_mut(|record| {
        if !claimed || gain_binding {
            record.engram.work_binding = None;
        }
        if !has_grant {
            record.engram.active_grant_id = None;
        }
    });
    let command = if detached { DETACHED_GATE } else { GATE };
    let cwd = fs::canonicalize(&turn.root)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut recorder = turn.recorder();
    let before_start = chrono::Utc::now();
    recorder
        .command_started_in(
            "missing-authority-launch",
            command,
            Some(command),
            Some(&cwd),
        )
        .unwrap();
    let after_start = chrono::Utc::now();
    if gain_binding {
        turn.record_mut(|record| record.engram.work_binding = saved_binding);
    }
    recorder
        .command_completed_with_exit(
            "missing-authority-launch",
            command,
            "STARTED",
            CommandStatus::Success,
            if detached {
                EngramCommandExit::Code(0)
            } else {
                EngramCommandExit::NotFinished
            },
        )
        .unwrap();
    let pending = turn
        .record(|record| record.engram.pending_source_root_line.clone())
        .expect("a confirmed launch without initial authority must report not carried");
    assert!(
        pending.contains("background full gate") && pending.contains("no credit"),
        "{pending}"
    );
    let reason = if !claimed || gain_binding {
        "began without a binding"
    } else {
        "has no active grant"
    };
    assert!(pending.contains(reason), "{pending}");
    turn.record(|record| {
        assert!(record.engram.active_turn_checks.is_empty());
        assert!(record.engram.carried_checks.is_empty());
        let unmatched = &record.engram.carried_unmatched_launches;
        assert_eq!(
            unmatched.len(),
            1,
            "one observed unmatched launch: {unmatched:?}"
        );
        assert_eq!(unmatched[0].0, engram_path_key(&turn.root));
        let observed_start =
            engram_parse_time(&unmatched[0].1).expect("a real observed start timestamp");
        assert!(observed_start >= before_start - chrono::Duration::milliseconds(1));
        assert!(observed_start <= after_start);
    });
    recorder
        .command_completed_with_exit(
            "missing-authority-launch",
            command,
            "STARTED",
            CommandStatus::Success,
            if detached {
                EngramCommandExit::Code(0)
            } else {
                EngramCommandExit::NotFinished
            },
        )
        .unwrap();
    assert_eq!(
        turn.record(|record| record.engram.pending_source_root_line.clone()),
        Some(pending)
    );
    assert_eq!(
        turn.record(|record| record.engram.carried_unmatched_launches.len()),
        1
    );
}

#[test]
fn unbound_detached_launch_reports_not_carried() {
    missing_authority_launch("outcome-unbound-detached", false, true, true, false);
}

#[test]
fn unbound_background_launch_reports_not_carried() {
    missing_authority_launch("outcome-unbound-background", false, true, false, false);
}

#[test]
fn bound_launch_without_grant_reports_the_missing_grant() {
    missing_authority_launch("outcome-no-grant", true, false, true, false);
}

#[test]
fn binding_acquired_after_start_does_not_hide_unbound_launch() {
    missing_authority_launch("outcome-bound-after-start", true, true, false, true);
}

fn diagnostic_start(turn: &CheckedTurn, key: &str, command: &str) {
    let cwd = fs::canonicalize(&turn.root)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    turn.state.note_engram_command_started(
        &turn.session_id,
        &EngramObservationProvenance::Ambient,
        key,
        Some(command),
        Some(&cwd),
    );
}

fn diagnostic_finish(turn: &CheckedTurn, key: &str, command: &str, exit: EngramCommandExit) {
    turn.state.note_engram_command_finished(
        &turn.session_id,
        &EngramObservationProvenance::Ambient,
        key,
        command,
        "STARTED",
        Some(exit),
    );
}

fn assert_no_verification_state(turn: &CheckedTurn) {
    turn.record(|record| {
        assert!(record.engram.active_turn_checks.is_empty());
        assert!(record.engram.carried_checks.is_empty());
    });
}

fn assert_diagnostic_windows(record: &SessionRecord, marks: &[EngramLaunchDiagnostic]) {
    let expected = marks.iter().filter_map(|mark| {
        Some((engram_path_key(mark.root.as_ref()?), mark.started_at.clone()))
    }).collect::<Vec<_>>();
    assert_eq!(record.engram.carried_unmatched_launches, expected,
        "retirement must preserve exactly the observed roots and original starts");
}

#[test]
fn duplicate_start_and_delayed_finish_keep_original_time_and_reason() {
    let turn = CheckedTurn::start("outcome-delayed", true);
    let binding = turn.record(|record| record.engram.work_binding.clone());
    turn.record_mut(|record| record.engram.work_binding = None);
    diagnostic_start(&turn, "delayed", DETACHED_GATE);
    let original = turn.record(|record| record.engram.pending_launch_diagnostics[0].clone());
    turn.record_mut(|record| record.engram.work_binding = binding);
    // This wire timestamp deliberately uses real UTC, not the budget clock.
    // Wait for a distinct published time so completion-time substitution
    // cannot pass by landing in the same millisecond as the start.
    let observed_start = engram_parse_time(&original.started_at).unwrap();
    let guard = phase_sync::PollGuard::new();
    while chrono::Utc::now() <= observed_start + chrono::Duration::milliseconds(1) {
        guard.wait("a distinct UTC time for the delayed launch result");
    }
    // A repeated start must not refresh the original observation.
    diagnostic_start(&turn, "delayed", DETACHED_GATE);
    turn.record(|record| {
        let marks = &record.engram.pending_launch_diagnostics;
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].started_at, original.started_at);
        assert!(matches!(
            marks[0].reason,
            EngramLaunchMissingAuthority::Binding
        ));
    });
    diagnostic_finish(&turn, "delayed", DETACHED_GATE, EngramCommandExit::Code(0));
    turn.record(|record| {
        assert_eq!(
            record.engram.carried_unmatched_launches[0].1,
            original.started_at
        );
        assert!(record.engram.pending_launch_diagnostics.is_empty());
        assert!(
            record
                .engram
                .pending_source_root_line
                .as_ref()
                .unwrap()
                .contains("began without a binding")
        );
    });
    assert_no_verification_state(&turn);
}

#[test]
fn failed_detached_and_compound_launches_report_their_actual_drop_cause() {
    for (label, command, exit, why, unmatched) in [
        (
            "outcome-failed",
            DETACHED_GATE.to_owned(),
            EngramCommandExit::Code(1),
            ENGRAM_LAUNCH_DROP_FAILED,
            0,
        ),
        (
            "outcome-compound",
            format!("{DETACHED_GATE}; echo done"),
            EngramCommandExit::Code(0),
            ENGRAM_LAUNCH_DROP_COMPOUND,
            1,
        ),
    ] {
        let turn = CheckedTurn::start(label, false);
        diagnostic_start(&turn, "dropped", &command);
        diagnostic_finish(&turn, "dropped", &command, exit);
        turn.record(|record| {
            let line = record
                .engram
                .pending_source_root_line
                .as_ref()
                .expect("actual Drop reason");
            assert!(line.contains(why), "{line}");
            assert!(!line.contains("began without a binding"), "{line}");
            assert_eq!(record.engram.carried_unmatched_launches.len(), unmatched);
            assert!(record.engram.pending_launch_diagnostics.is_empty());
        });
        assert_no_verification_state(&turn);
    }
}

#[test]
fn foreground_full_gate_and_finish_without_start_invent_no_carried_diagnostic() {
    let turn = CheckedTurn::start("outcome-ordinary", false);
    diagnostic_start(&turn, "foreground", GATE);
    diagnostic_finish(&turn, "foreground", GATE, EngramCommandExit::Code(0));
    diagnostic_finish(
        &turn,
        "missing-start",
        DETACHED_GATE,
        EngramCommandExit::Code(0),
    );
    turn.record(|record| {
        assert!(record.engram.pending_source_root_line.is_none());
        assert!(record.engram.pending_launch_diagnostics.is_empty());
        assert!(record.engram.carried_unmatched_launches.is_empty());
    });
    assert_no_verification_state(&turn);
}

#[test]
fn missing_location_reports_only_a_line_without_inventing_a_root() {
    let turn = CheckedTurn::start("outcome-no-location", false);
    turn.record_mut(|record| {
        record.engram.shell_directory = Some(EngramShellDirectory {
            runtime: record.runtime.runtime_token(),
            directory: None,
            pending: None,
            lost_among: Vec::new(),
            lost_unbounded: true,
        });
    });
    turn.state.note_engram_command_started(
        &turn.session_id,
        &EngramObservationProvenance::Ambient,
        "unknown",
        Some(DETACHED_GATE),
        None,
    );
    turn.record(|record| assert!(record.engram.pending_launch_diagnostics[0].root.is_none()));
    diagnostic_finish(&turn, "unknown", DETACHED_GATE, EngramCommandExit::Code(0));
    turn.record(|record| {
        assert!(
            record
                .engram
                .pending_source_root_line
                .as_ref()
                .unwrap()
                .contains("began without a binding")
        );
        assert!(record.engram.carried_unmatched_launches.is_empty());
    });
    assert_no_verification_state(&turn);
}

#[test]
fn disabled_session_or_project_emits_no_missing_authority_diagnostic() {
    for at_finish in [false, true] {
        for project in [false, true] {
            let turn = CheckedTurn::start("outcome-disabled", false);
            let disable = || {
                let mut inner = turn.state.inner.lock().unwrap();
                let index = inner.find_session_index(&turn.session_id).unwrap();
                if project {
                    let project_id = inner.sessions[index].session.project_id.clone().unwrap();
                    inner
                        .projects
                        .iter_mut()
                        .find(|p| p.id == project_id)
                        .unwrap()
                        .engram = None;
                } else {
                    inner.sessions[index].engram.disabled_reason =
                        Some("fixture disabled".to_owned());
                }
            };
            if !at_finish {
                disable();
            }
            diagnostic_start(&turn, "disabled", DETACHED_GATE);
            if at_finish {
                disable();
            }
            diagnostic_finish(&turn, "disabled", DETACHED_GATE, EngramCommandExit::Code(0));
            turn.record(|record| {
                assert!(record.engram.pending_source_root_line.is_none());
                assert!(record.engram.pending_launch_diagnostics.is_empty());
                assert!(record.engram.carried_unmatched_launches.is_empty());
            });
            assert_no_verification_state(&turn);
        }
    }
}

#[test]
fn foreign_replaced_and_unowned_results_do_not_consume_current_diagnostics() {
    let turn = CheckedTurn::start("outcome-foreign", false);
    diagnostic_start(&turn, "owned", DETACHED_GATE);
    let current = claude_turn_provenance(&turn);
    for foreign in [
        ClaudeObservationProvenance {
            origin: ClaudeWorkOrigin::Attempt {
                turn_generation: turn.record(|r| r.active_turn_generation + 1),
            },
            ..current.clone()
        },
        ClaudeObservationProvenance {
            token: RuntimeToken::Claude("outcome-replaced".to_owned()),
            ..current.clone()
        },
    ] {
        turn.state.note_engram_command_finished(
            &turn.session_id,
            &EngramObservationProvenance::Claude(foreign),
            "owned",
            DETACHED_GATE,
            "STARTED",
            Some(EngramCommandExit::Code(0)),
        );
        turn.record(|record| {
            assert_eq!(record.engram.pending_launch_diagnostics.len(), 1);
            assert!(record.engram.pending_source_root_line.is_none());
            assert!(record.engram.carried_unmatched_launches.is_empty());
        });
    }
    turn.record_mut(|record| {
        record.unmediated_claude_turn = Some(UnmediatedClaudeTurn {
            adopted_generation: None,
            notice_message_id: "outcome-unowned".to_owned(),
        })
    });
    diagnostic_finish(&turn, "owned", DETACHED_GATE, EngramCommandExit::Code(0));
    turn.record(|record| {
        assert_eq!(record.engram.pending_launch_diagnostics.len(), 1);
        assert!(record.engram.pending_source_root_line.is_none());
        assert!(record.engram.carried_unmatched_launches.is_empty());
    });
    turn.record_mut(|record| record.unmediated_claude_turn = None);
    diagnostic_finish(&turn, "owned", DETACHED_GATE, EngramCommandExit::Code(0));
    assert!(turn.record(|record| record.engram.pending_source_root_line.is_some()));
    assert_no_verification_state(&turn);
}

#[test]
fn excluded_starts_create_no_diagnostic_and_a_new_turn_cannot_borrow_a_key() {
    let turn = CheckedTurn::start("outcome-excluded-start", false);
    let cwd = turn.root.to_string_lossy().into_owned();
    let current = claude_turn_provenance(&turn);
    for provenance in [
        ClaudeObservationProvenance {
            origin: ClaudeWorkOrigin::Unattributed,
            ..current.clone()
        },
        ClaudeObservationProvenance {
            token: RuntimeToken::Claude("outcome-stale-start".to_owned()),
            ..current
        },
    ] {
        turn.state.note_engram_command_started(
            &turn.session_id,
            &EngramObservationProvenance::Claude(provenance),
            "excluded",
            Some(DETACHED_GATE),
            Some(&cwd),
        );
    }
    turn.record_mut(|record| {
        record.unmediated_claude_turn = Some(UnmediatedClaudeTurn {
            adopted_generation: None,
            notice_message_id: "outcome-unowned-start".to_owned(),
        })
    });
    diagnostic_start(&turn, "excluded", DETACHED_GATE);
    turn.record_mut(|record| record.unmediated_claude_turn = None);
    assert!(turn.record(|record| record.engram.pending_launch_diagnostics.is_empty()));
    diagnostic_start(&turn, "reused", DETACHED_GATE);
    let original = turn.record(|record| record.engram.pending_launch_diagnostics[0].clone());
    turn.record_mut(|record| record.active_turn_generation += 1);
    diagnostic_finish(&turn, "reused", DETACHED_GATE, EngramCommandExit::Code(0));
    turn.record(|record| {
        assert!(record.engram.pending_source_root_line.as_ref().unwrap().contains(
            "its runtime or turn ended before its launch result; diagnostic tracking dropped"
        ));
        assert_diagnostic_windows(record, &[original.clone()]);
        assert!(record.engram.pending_launch_diagnostics.is_empty());
    });
    assert_no_verification_state(&turn);
}

#[test]
fn changed_command_with_the_same_key_cannot_borrow_a_launch_start() {
    let turn = CheckedTurn::start("outcome-changed-command", false);
    diagnostic_start(&turn, "changed", DETACHED_GATE);
    let original = turn.record(|record| record.engram.pending_launch_diagnostics[0].clone());
    diagnostic_finish(&turn, "changed", GATE, EngramCommandExit::NotFinished);
    turn.record(|record| {
        assert!(
            record
                .engram
                .pending_source_root_line
                .as_ref()
                .unwrap()
                .contains("its command at completion did not match its start")
        );
        assert_diagnostic_windows(record, &[original.clone()]);
        assert!(record.engram.pending_launch_diagnostics.is_empty());
    });
    assert_no_verification_state(&turn);
}

#[test]
fn abandonment_retires_only_its_own_runtime_and_turn_start() {
    let turn = CheckedTurn::start("outcome-abandoned", false);
    diagnostic_start(&turn, "abandoned", DETACHED_GATE);
    let original = turn.record(|record| record.engram.pending_launch_diagnostics[0].clone());
    let original_generation = turn.record(|record| record.active_turn_generation);
    turn.record_mut(|record| record.active_turn_generation += 1);
    turn.state.note_engram_command_abandoned(
        &turn.session_id,
        &EngramObservationProvenance::Ambient,
        "abandoned",
    );
    assert_eq!(
        turn.record(|record| record.engram.pending_launch_diagnostics.len()),
        1
    );
    turn.record(|record| assert!(record.engram.carried_unmatched_launches.is_empty()));
    turn.record_mut(|record| record.active_turn_generation = original_generation);
    turn.state.note_engram_command_abandoned(
        &turn.session_id,
        &EngramObservationProvenance::Ambient,
        "abandoned",
    );
    diagnostic_finish(
        &turn,
        "abandoned",
        DETACHED_GATE,
        EngramCommandExit::Code(0),
    );
    turn.record(|record| {
        assert!(record.engram.pending_launch_diagnostics.is_empty());
        assert!(record.engram.pending_source_root_line.is_none());
        assert_diagnostic_windows(record, &[original.clone()]);
    });
    assert_no_verification_state(&turn);
}

#[test]
fn a_replacement_runtime_cannot_take_a_pending_start_with_the_same_key() {
    let turn = CheckedTurn::start("outcome-replaced-runtime", false);
    diagnostic_start(&turn, "reused", DETACHED_GATE);
    let original = turn.record(|record| record.engram.pending_launch_diagnostics[0].clone());
    turn.record_mut(|record| {
        let SessionRuntime::Codex(runtime) = &mut record.runtime else {
            panic!("fixture starts a Codex runtime");
        };
        runtime.runtime_id.push_str("-replacement");
    });
    diagnostic_finish(&turn, "reused", DETACHED_GATE, EngramCommandExit::Code(0));
    turn.record(|record| {
        assert!(record.engram.pending_source_root_line.as_ref().unwrap().contains(
            "its runtime or turn ended before its launch result; diagnostic tracking dropped"
        ));
        assert_diagnostic_windows(record, &[original.clone()]);
        assert!(record.engram.pending_launch_diagnostics.is_empty());
    });
    assert_no_verification_state(&turn);
}

#[test]
fn diagnostic_cap_reports_tracking_loss_without_inventing_a_launch_outcome() {
    let turn = CheckedTurn::start("outcome-cap", false);
    diagnostic_start(&turn, "cap-0", DETACHED_GATE);
    let oldest = turn.record(|record| record.engram.pending_launch_diagnostics[0].clone());
    for index in 1..=ENGRAM_CARRIED_UNMATCHED_LAUNCH_LIMIT {
        diagnostic_start(&turn, &format!("cap-{index}"), DETACHED_GATE);
    }
    let tracking_loss = turn.record(|record| {
        let marks = &record.engram.pending_launch_diagnostics;
        assert_eq!(marks.len(), ENGRAM_CARRIED_UNMATCHED_LAUNCH_LIMIT);
        assert_eq!(marks[0].key, "cap-1");
        assert_eq!(
            marks.last().unwrap().key,
            format!("cap-{}", ENGRAM_CARRIED_UNMATCHED_LAUNCH_LIMIT)
        );
        assert_diagnostic_windows(record, &[oldest.clone()]);
        let line = record
            .engram
            .pending_source_root_line
            .clone()
            .expect("eviction is visible");
        assert!(line.contains("pending-command tracking limit"), "{line}");
        assert_eq!(line.matches("diagnostic tracking").count(), 1);
        assert!(!line.contains("background full gate") && !line.contains("earned no credit"));
        line
    });
    diagnostic_finish(&turn, "cap-0", DETACHED_GATE, EngramCommandExit::Code(0));
    assert_eq!(
        turn.record(|record| record.engram.pending_source_root_line.clone()),
        Some(tracking_loss)
    );
    turn.record(|record| assert_diagnostic_windows(record, &[oldest.clone()]));
    let newest = turn.record(|record| record.engram.pending_launch_diagnostics.last().unwrap().clone());
    diagnostic_finish(
        &turn,
        &format!("cap-{}", ENGRAM_CARRIED_UNMATCHED_LAUNCH_LIMIT),
        DETACHED_GATE,
        EngramCommandExit::Code(0),
    );
    turn.record(|record| {
        assert_eq!(
            record.engram.pending_launch_diagnostics.len(),
            ENGRAM_CARRIED_UNMATCHED_LAUNCH_LIMIT - 1
        );
        assert_diagnostic_windows(record, &[oldest.clone(), newest.clone()]);
        assert!(
            record
                .engram
                .pending_source_root_line
                .as_ref()
                .unwrap()
                .contains("began without a binding")
        );
    });
    assert_no_verification_state(&turn);
}

#[test]
fn a_retained_terminal_carry_without_binding_keeps_existing_fence_and_expiry_refusals() {
    for fenced in [false, true] {
        let (turn, worktree) = named_turn("outcome-absent-binding");
        launch_gate(&turn, &worktree, true);
        let run = finish_run(&worktree, "passed", &"f".repeat(64));
        turn.state.poll_engram_carried_runs();
        turn.record_mut(|record| {
            record.engram.work_binding = None;
            let generation = record.engram.carried_checks[0].root_generation;
            let (credited, lines) = engram_take_settled_carried_checks(
                record,
                Vec::new(),
                &[Some(generation)],
                None,
                chrono::Utc::now(),
            );
            assert!(credited.is_empty() && lines.is_empty());
            assert_eq!(record.engram.carried_checks.len(), 1);
            if fenced {
                record.engram.carried_checks[0].fence = Some("original fence cause".to_owned());
            }
            let now = chrono::Utc::now()
                + if fenced {
                    chrono::Duration::zero()
                } else {
                    chrono::Duration::hours(7)
                };
            let (credited, lines) = engram_take_settled_carried_checks(
                record,
                Vec::new(),
                &[Some(generation)],
                None,
                now,
            );
            assert!(credited.is_empty());
            assert_eq!(lines.len(), 1);
            assert!(
                lines[0].contains(if fenced {
                    "original fence cause"
                } else {
                    "six hours"
                }),
                "{lines:?}"
            );
            assert!(!lines[0].contains("no longer the one this session holds"));
            assert!(record.engram.carried_checks.is_empty());
            assert!(record.engram.carried_consumed_runs.contains(&run));
        });
    }
}

#[test]
fn the_existing_fence_wins_over_a_different_claim_and_is_consumed_once() {
    let (turn, worktree) = named_turn("outcome-fence-first");
    launch_gate(&turn, &worktree, true);
    finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    turn.record_mut(|record| {
        record.engram.work_binding.as_mut().unwrap().claim_id = "other-claim".to_owned();
        record.engram.carried_checks[0].fence = Some("earlier exact cause".to_owned());
    });
    let checkpoint = turn.finish();
    assert_refused(&turn, &checkpoint, "earlier exact cause");
    let pending = turn.record(|record| record.engram.pending_source_root_line.clone().unwrap());
    assert!(
        !pending.contains("no longer the one this session holds"),
        "{pending}"
    );
    turn.record_mut(|record| {
        let (credited, lines) =
            engram_take_settled_carried_checks(record, Vec::new(), &[], None, chrono::Utc::now());
        assert!(credited.is_empty() && lines.is_empty());
    });
}

#[test]
fn a_nonterminal_carry_under_a_different_claim_keeps_its_existing_wait() {
    let (turn, worktree) = named_turn("outcome-nonterminal");
    launch_gate(&turn, &worktree, true);
    turn.record_mut(|record| {
        record.engram.work_binding.as_mut().unwrap().claim_id = "other-claim".to_owned();
        let generation = record.engram.carried_checks[0].root_generation;
        let (credited, lines) = engram_take_settled_carried_checks(
            record,
            Vec::new(),
            &[Some(generation)],
            None,
            chrono::Utc::now(),
        );
        assert!(credited.is_empty() && lines.is_empty());
        assert_eq!(record.engram.carried_checks.len(), 1);
        assert!(record.engram.carried_consumed_runs.is_empty());
        assert!(record.engram.carried_unmatched_launches.is_empty());
    });
}

#[test]
fn final_lock_does_not_credit_an_off_lock_candidate_after_claim_loss() {
    for present in [false, true] {
        let (turn, worktree) = named_turn("outcome-off-lock-claim");
        launch_gate(&turn, &worktree, true);
        finish_run(&worktree, "passed", &"f".repeat(64));
        turn.state.poll_engram_carried_runs();
        let (copy, workers) = turn.record(|record| {
            (
                record.engram.carried_checks[0].clone(),
                record.engram.capture_workers.clone(),
            )
        });
        let result = engram_settle_carried_check(
            &copy,
            &workers,
            std::time::Instant::now() + DEADLOCK_GUARD,
        );
        assert!(
            matches!(&result, Ok(Some(_))),
            "the real off-lock settlement must produce a success candidate"
        );
        turn.record_mut(|record| {
            if present {
                record.engram.work_binding.as_mut().unwrap().claim_id = "other-claim".to_owned();
            } else {
                record.engram.work_binding = None;
            }
            let (credited, lines) = engram_take_settled_carried_checks(
                record,
                vec![(copy.check.grant_id.clone(), copy.check.sequence, result)],
                &[Some(copy.root_generation)],
                Some(&EngramHeldClaims {
                    items: Vec::new(),
                    omitted: 0,
                }),
                chrono::Utc::now(),
            );
            assert!(credited.is_empty());
            if present {
                assert_eq!(lines.len(), 1);
                assert!(lines[0].contains("no longer the one this session holds"));
                assert!(record.engram.carried_checks.is_empty());
            } else {
                assert!(lines.is_empty());
                assert_eq!(record.engram.carried_checks.len(), 1);
            }
        });
    }
}

#[test]
fn unchanged_same_claim_still_credits_a_carried_gate_at_the_next_checkpoint() {
    a_gate_launched_in_one_turn_is_credited_at_the_next_turns_checkpoint();
}

#[test]
fn bound_unmeasured_launch_keeps_its_existing_refusal() {
    let turn = CheckedTurn::start("outcome-unmeasured", true);
    diagnostic_start(&turn, "unmeasured", DETACHED_GATE);
    turn.wait_for_snapshots();
    diagnostic_finish(
        &turn,
        "unmeasured",
        DETACHED_GATE,
        EngramCommandExit::Code(0),
    );
    turn.record(|record| {
        assert!(record.engram.pending_launch_diagnostics.is_empty());
        assert!(
            record
                .engram
                .pending_source_root_line
                .as_ref()
                .unwrap()
                .contains("not measured in a named source root")
        );
        assert_eq!(record.engram.carried_unmatched_launches.len(), 1);
    });
    assert_no_verification_state(&turn);
}

#[test]
fn unconfirmed_launch_claim_waits_only_until_existing_carried_expiry() {
    let (turn, worktree) = named_turn("unknown-held-expiry");
    launch_gate(&turn, &worktree, true);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    turn.record_mut(|record| {
        record.engram.work_binding.as_mut().unwrap().claim_id = "other-held-claim".to_owned();
        let generation = record.engram.carried_checks[0].root_generation;
        let now = chrono::Utc::now();
        let (credited, lines) =
            engram_take_settled_carried_checks(record, Vec::new(), &[Some(generation)], None, now);
        assert!(credited.is_empty());
        assert_eq!(record.engram.carried_checks.len(), 1);
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].contains("once its held claim is confirmed"),
            "{lines:?}"
        );
        assert!(!record.engram.carried_consumed_runs.contains(&run));
        let (credited, lines) = engram_take_settled_carried_checks(
            record,
            Vec::new(),
            &[Some(generation)],
            None,
            now + chrono::Duration::hours(7),
        );
        assert!(credited.is_empty());
        assert!(record.engram.carried_checks.is_empty());
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("six hours"), "{lines:?}");
        assert!(record.engram.carried_consumed_runs.contains(&run));
    });
}
