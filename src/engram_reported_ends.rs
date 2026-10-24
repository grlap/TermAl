// A check's end as its runtime reports it, and a Codex test still running
// when its turn closes, carried until that end is reported.
//
// Owns: reading a reported end off the state lock (`engram_read_check_end`)
// and recording it on the check it ends (`engram_record_check_end`), for a
// check ending in its turn and for a carried one alike; carrying, at a turn's
// close, a Codex test whose end has not been reported yet
// (`engram_carry_running_codex_checks`), since Codex reports a command's end
// when its process exits, tagged with the turn that started it, which may
// have closed by then; taking such a late end for the carried check whose
// runtime key it names (`note_engram_reported_end`); keeping each Codex test
// still running at its turn's close, carried or not, until its end is
// reported or its runtime is gone (`EngramRunningCodexTest`), and naming it
// to a check that starts in its worktree (`engram_carried_running_key`); and
// settling a carried test from that end (`engram_settle_reported_end`).
//
// Does not own: which late Codex events reach here (the Codex event
// handlers, through `EngramHost::codex_reported_end`), the carried checks'
// record, fence, credit and restart marker (engram_carried_checks.rs), or
// closing an unended check with outcome Unknown (engram_turn_checks.rs).
//
// `engram_read_check_end` and `engram_record_check_end` were moved out of
// `note_engram_command_finished_with_capture` in engram_turn_checks.rs,
// unchanged; the rest is new.

/// What a command's reported end says about the check it may end, read off
/// the state lock from its output, which can be long
/// (`engram_read_check_end`).
struct EngramEndReading {
    build_completion: Option<(
        EngramCommandExit,
        Vec<String>,
        Option<EngramBuildDiagnostic>,
    )>,
    node_completion: Option<(
        EngramCommandExit,
        Vec<String>,
        bool,
        Option<EngramBuildDiagnostic>,
    )>,
    launcher_test_stages: Vec<String>,
    /// A launcher test's own terminal record says its run ran no stage
    /// (`engram_launcher_output_ran_no_stage`).
    launcher_ran_no_stage: bool,
    parsed: Option<(EngramCheckCommand, Vec<String>, bool)>,
}

/// Off-lock work still belongs to this exact grant, sequence and invocation.
fn engram_check_start_matches(
    check: &EngramTurnCheck,
    starting_check: Option<&EngramTurnCheck>,
) -> bool {
    starting_check.is_some_and(|start| {
        start.grant_id == check.grant_id && start.sequence == check.sequence
            && start.command == check.command && start.target == check.target
    })
}

/// The off-lock reading belongs to this exact start and names its command.
/// Qualified foreground ends require actual reported command text.
fn engram_check_end_matches(
    check: &EngramTurnCheck,
    reading: &EngramEndReading,
    starting_check: Option<&EngramTurnCheck>,
) -> bool {
    engram_check_start_matches(check, starting_check) && reading.parsed.as_ref()
        .is_some_and(|(finished, _, _)| finished == &check.command)
}

/// Reads, off the state lock, what the end of `command` with `output` and
/// `exit` says about `starting_check`, the check its key started, if one is
/// running: its result lines and the build or focused Node Test completion
/// of a check of either kind. Nothing is read when no check is running.
fn engram_read_check_end(
    starting_check: Option<&EngramTurnCheck>,
    session_id: &str,
    reported_command: Option<&str>,
    output: &str,
    exit: Option<EngramCommandExit>,
) -> EngramEndReading {
    // No command is different from a command that failed recognition. A
    // display title cannot satisfy the qualified completion association.
    let command = reported_command.unwrap_or("");
    let build_completion = starting_check
        .filter(|check| check.command.kind == EngramVerificationKind::Build)
        .map(|check| {
            if engram_check_command_like(command, &check.command).as_ref() != Some(&check.command) {
                (
                    EngramCommandExit::Unknown,
                    Vec::new(),
                    Some(EngramBuildDiagnostic::rejected(
                        exit.unwrap_or(EngramCommandExit::Unknown),
                        EngramBuildRejection::Association,
                    )),
                )
            } else {
                engram_build_completion(
                    check,
                    session_id,
                    output,
                    exit.unwrap_or(EngramCommandExit::Unknown),
                )
            }
        });
    let node_completion = starting_check
        .filter(|check| engram_is_focused_node_check(&check.command))
        .map(|check| {
            if engram_check_command_like(command, &check.command).as_ref() != Some(&check.command) {
                (EngramCommandExit::Unknown, Vec::new(), false, None)
            } else {
                engram_focused_node_completion(
                    check,
                    session_id,
                    output,
                    exit.unwrap_or(EngramCommandExit::Unknown),
                )
            }
        });
    // A launcher full gate's test stages are its own request record's,
    // read off the lock (`engram_launcher_output_test_stages`), for
    // whichever command the check's end is read as below.
    let launcher_test_stages =
        if starting_check.is_some_and(|check| check.command.kind == EngramVerificationKind::Test) {
            engram_launcher_output_test_stages(output)
        } else {
            Vec::new()
        };
    // Whether a launcher test's run ran no stage, by its own terminal record,
    // read off the lock too.
    let launcher_ran_no_stage = starting_check.is_some_and(|check| {
        check.command.kind == EngramVerificationKind::Test
            && engram_launcher_words(&check.command).is_some()
    }) && engram_launcher_output_ran_no_stage(output);
    let parsed = starting_check
        .and_then(|check| engram_check_command_like(command, &check.command))
        .map(|finished| {
            let result_lines = engram_check_result_lines(&finished.program, output);
            let showed_passing_tests =
                engram_check_showed_passing_tests(&finished, &result_lines, &launcher_test_stages);
            (finished, result_lines, showed_passing_tests)
        });
    EngramEndReading {
        build_completion,
        node_completion,
        launcher_test_stages,
        launcher_ran_no_stage,
        parsed,
    }
}

/// Records on `check` its end, reported with `output` and `exit` and read as
/// `reading` while `starting_check` was the check its key started: its exit,
/// result lines and diagnostics, a closing snapshot of the worktree it ran in
/// taken now under `provenance`, and a declared check's artifact reading.
fn engram_record_check_end(
    check: &mut EngramTurnCheck,
    reading: EngramEndReading,
    starting_check: Option<&EngramTurnCheck>,
    output: &str,
    exit: Option<EngramCommandExit>,
    runtime_output_cut: bool,
    workers: &Arc<std::sync::atomic::AtomicUsize>,
    provenance: EngramRootCapture,
) {
    let qualified_command_matches = engram_check_end_matches(check, &reading, starting_check);
    let EngramEndReading {
        build_completion,
        node_completion,
        launcher_test_stages,
        launcher_ran_no_stage,
        parsed,
    } = reading;
    let (mut result_lines, mut showed_passing_tests) = match parsed {
        Some((finished, result_lines, showed_passing_tests)) if finished == check.command => {
            (result_lines, showed_passing_tests)
        }
        // The end names another command than the check's start (a reused
        // key): the check's own command decides how its output reads.
        _ => {
            let result_lines = engram_check_result_lines(&check.command.program, output);
            let showed_passing_tests = engram_check_showed_passing_tests(
                &check.command,
                &result_lines,
                &launcher_test_stages,
            );
            (result_lines, showed_passing_tests)
        }
    };
    let same_start = starting_check.is_some_and(|start| {
        start.grant_id == check.grant_id
            && start.sequence == check.sequence
            && start.command == check.command
            && start.target == check.target
    });
    let mut build_diagnostic = None;
    let check_exit = if check.command.literal_pipe && !qualified_command_matches {
        result_lines.clear();
        showed_passing_tests = false;
        let observed = exit.unwrap_or(EngramCommandExit::Unknown);
        build_diagnostic = Some(match check.command.kind {
            EngramVerificationKind::Test => EngramBuildDiagnostic::test_rejected(
                observed, EngramBuildRejection::Association,
            ),
            EngramVerificationKind::Build => EngramBuildDiagnostic::rejected(
                observed, EngramBuildRejection::Association,
            ),
        });
        EngramCommandExit::Unknown
    } else if let Some((node_exit, node_lines, node_passed, node_diagnostic)) =
        node_completion.filter(|_| same_start)
    {
        result_lines = node_lines;
        showed_passing_tests = node_passed;
        build_diagnostic = node_diagnostic;
        node_exit
    } else if engram_is_focused_node_check(&check.command)
        || (check.command.literal_pipe
            && check.command.kind == EngramVerificationKind::Test
            && check.command.program == "node")
    {
        // A qualified launcher line must take the focused validator. No
        // generic stage/count record can substitute for a rejected argv or
        // missing focused completion.
        result_lines.clear();
        showed_passing_tests = false;
        if check.command.literal_pipe {
            build_diagnostic = Some(EngramBuildDiagnostic::test_rejected(
                exit.unwrap_or(EngramCommandExit::Unknown), EngramBuildRejection::Argv,
            ));
        }
        EngramCommandExit::Unknown
    } else if let Some((build_exit, build_lines, diagnostic)) =
        build_completion.filter(|_| same_start)
    {
        result_lines = build_lines;
        build_diagnostic = diagnostic;
        build_exit
    } else if check.command.kind == EngramVerificationKind::Build {
        result_lines.clear();
        build_diagnostic = Some(EngramBuildDiagnostic::rejected(
            exit.unwrap_or(EngramCommandExit::Unknown),
            EngramBuildRejection::Association,
        ));
        EngramCommandExit::Unknown
    } else if !check.command.simple && check.command.normalized.contains('|') {
        // Keep the observed command exit diagnostic, without lending a
        // pipeline/wrapper's status to the test or generic passing text.
        result_lines.clear();
        showed_passing_tests = false;
        build_diagnostic = Some(EngramBuildDiagnostic::test_rejected(
            exit.unwrap_or(EngramCommandExit::Unknown), engram_pipe_rejection(&check.command),
        ));
        EngramCommandExit::Unknown
    } else {
        exit.unwrap_or(EngramCommandExit::Unknown)
    };
    let end_basis = if exit == Some(EngramCommandExit::NotFinished) {
        // A background run's result marks its launch; it is never
        // reported, so it takes no closing snapshot.
        EngramBasisCapture::settled(None)
    } else {
        // The worktree the check ran in, as its opening snapshot was.
        engram_spawn_basis_capture(check.target.basis_place(), workers, provenance)
    };
    // A declared check's declaration and artifact are read as its command
    // ends, on their own thread, like its closing snapshot.
    let declared = check
        .declared
        .as_deref()
        .filter(|_| check_exit != EngramCommandExit::NotFinished)
        .map(|binding| {
            let binding = binding.clone();
            EngramCapture::spawn(workers, move || {
                engram_declared_result(&binding, check_exit)
            })
        });
    // Read from the lines this end keeps, so it names only what they show.
    let focused_stage_line_missing = !showed_passing_tests
        && engram_focused_stage_line_missing(
            &check.command,
            &result_lines,
            &launcher_test_stages,
        );
    check.ended_at = Some(engram_interference_tick());
    check.runtime_output_cut = runtime_output_cut;
    check.end = Some(EngramTurnCheckEnd {
        completed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        exit: check_exit,
        build_diagnostic,
        result_lines,
        showed_passing_tests,
        focused_stage_line_missing,
        // Read for the check its key started; it holds for this one only
        // when this one is a launcher test too.
        launcher_ran_no_stage: launcher_ran_no_stage
            && check.command.kind == EngramVerificationKind::Test
            && engram_launcher_words(&check.command).is_some(),
        end_basis,
        declared,
    });
}

/// Whether `check` was keyed by its own command line, as Codex keys a command
/// whose item has no id: such a key names no one run, so a later end under it
/// cannot be taken as this check's.
fn engram_check_key_is_its_line(check: &EngramTurnCheck) -> bool {
    check.keyed_by_line || engram_check_command_like(&check.key, &check.command).as_ref() == Some(&check.command)
}

/// At the close of a Codex session's turn, carries each of its checks whose
/// end has not been reported yet, keyed by its Codex item, when it can be
/// carried (`engram_carry_refusal`): Codex reports a command's end when its
/// process exits, which may be after the turn that started it closed. The
/// check then ends by that reported end (`note_engram_reported_end`) and is
/// settled at a later checkpoint on the same claim. Returns the lines for its
/// holder. Only a check of `grant_id`, the grant whose turn closes, is
/// carried, as only its ends are taken in its turn; every check left behind
/// is closed with outcome Unknown by the caller (`engram_close_unended_checks`).
/// No other runtime's check is carried here.
fn engram_carry_running_codex_checks(record: &mut SessionRecord, grant_id: &str) -> Vec<String> {
    let mut lines = Vec::new();
    if record.session.agent != Agent::Codex {
        return lines;
    }
    // Every test of the grant still running, carried or not: refusing its
    // evidence does not end its process (`EngramRunningCodexTest`).
    if let Some(runtime) = record.runtime.runtime_token() {
        let running = record
            .engram
            .active_turn_checks
            .iter()
            .filter(|check| {
                check.grant_id == grant_id
                    && check.end.is_none()
                    && !engram_check_key_is_its_line(check)
            })
            .map(|check| EngramRunningCodexTest {
                key: check.key.clone(),
                runtime: runtime.clone(),
                root_key: engram_path_key(&check.target.root),
            })
            .collect::<Vec<_>>();
        for test in running {
            engram_note_running_codex_test(&mut record.engram, test);
        }
    }
    let mut position = 0;
    while position < record.engram.active_turn_checks.len() {
        let check = &record.engram.active_turn_checks[position];
        if check.grant_id != grant_id
            || check.end.is_some()
            || engram_check_key_is_its_line(check)
            || engram_carry_refusal(record, check).is_some()
        {
            position += 1;
            continue;
        }
        // The runtime it runs under, whose exit closes it: without one, no
        // exit could, so it is not carried.
        let (Some(binding), Some(turn_root), Some(runtime)) = (
            record.engram.work_binding.clone(),
            record
                .engram
                .active_turn_source_root
                .as_ref()
                .map(|root| root.generation),
            record.runtime.runtime_token(),
        ) else {
            position += 1;
            continue;
        };
        let running = record.engram.active_turn_checks.remove(position);
        lines.push(engram_carried_running_line(&running.command));
        // A change the watcher saw in its worktree while it ran in its turn
        // (a launcher full gate's `watcher_fence`) fences it once carried, as
        // it fences a launched gate's carry (`engram_carry_check`).
        let fence = running
            .watcher_fence
            .as_ref()
            .map(|cause| format!("{cause} (while it was running)"));
        let carried = EngramCarriedCheck {
            fence,
            ..engram_new_carried_check(&binding, turn_root, running, Some(runtime))
        };
        lines.extend(engram_push_carried_check(record, carried));
    }
    lines
}

/// The line the holder of a Codex test carried at its turn's close gets. It
/// stays carried, however many turns pass, until Codex reports its end: no
/// other fact says its process is gone (Codex reports none for an agent's
/// exec session), so it is told as still running, with what that holds up
/// and how to end the wait.
fn engram_carried_running_line(command: &EngramCheckCommand) -> String {
    format!(
        "{ENGRAM_CHECK_CREDIT_LINE_PREFIX} a Codex test still running when its turn closed, \
         a {} {} (check {}), is carried: it is recorded at a later checkpoint on this claim \
         once its end is reported, if nothing writes in its worktree before then. Until \
         then it is still running, and later checks in its worktree are overlapped until \
         it ends; ending its process, or Stopping the session (whose runtime exit closes \
         it), ends the wait.",
        command.program,
        command.kind.label(),
        engram_check_fingerprint(command)
    )
}

/// The line the holder of a carried Codex test gets when the host restarts
/// before it is settled: closed with outcome Unknown, as a check whose end
/// was never reported is (`engram_close_unended_checks`). Whether its end had
/// been reported (`ended`) decides only what the line says happened.
fn engram_reported_end_lost_to_restart_line(command: &EngramCheckCommand, ended: bool) -> String {
    engram_unrecorded_check_line(
        command,
        if ended {
            "its reported end was not settled before the host restarted, so it was closed with \
             outcome Unknown and is not credited; run it again to record it"
        } else {
            "no end was reported for it before the host restarted, so it was closed with outcome \
             Unknown and is not credited; run it again to record it"
        },
    )
}

/// The runtime `exited` of the session in `record` exited: no end can come
/// any more for a Codex test carried under it that is still waiting for one,
/// so each such test is closed with outcome Unknown, as a check whose end was
/// never reported is (`engram_close_unended_checks`), and its holder told. A
/// test whose end was already reported keeps it and is settled at the next
/// checkpoint on its claim as before; a test carried under another runtime of
/// the session, a newer one, is left alone, as is every launcher gate. Called
/// under the state lock, by the exit handler.
fn engram_close_reported_end_carries(record: &mut SessionRecord, exited: &RuntimeToken) {
    let session_id = record.session.id.clone();
    // Its tests no longer count as running: a stated limitation, since the
    // exit does not prove a child command ended (`EngramRunningCodexTest`).
    record
        .engram
        .running_codex_tests
        .retain(|running| &running.runtime != exited);
    let lines = engram_take_unended_carries(
        record,
        |carried| carried.runtime.as_ref() == Some(exited),
        "no end was reported for it before its runtime exited",
    );
    for line in lines {
        let line = engram_with_runtime_gone_caveat(&line, "exited");
        engram_publish_check_notice(
            &session_id,
            &mut record.engram,
            line,
            &mut std::io::stderr(),
        );
    }
}

/// Closes, with outcome Unknown, each Codex test of `record` carried until
/// its end is reported whose end can no longer reach the host: one still
/// waiting for its end, carried under a runtime that is no longer the
/// session's. The shared Codex router drops every event of a runtime that is
/// not the session's own, so once its runtime was stopped or replaced, no end
/// of such a test can arrive. A test whose end was reported is settled as
/// before. Returns the lines for its holder. Called under the state lock when
/// the session's runtime is cleared (`EngramHost::runtime_cleared`); every
/// checkpoint applies the same rule to each carried check in turn
/// (`engram_reported_end_stranded`, in `engram_take_settled_carried_checks`).
fn engram_take_stranded_reported_ends(record: &mut SessionRecord) -> Vec<String> {
    let current = record.runtime.runtime_token();
    // A stopped or replaced runtime's tests no longer count as running for
    // this session: a stated limitation, since a shared runtime outlives the
    // clear (`EngramRunningCodexTest`).
    record
        .engram
        .running_codex_tests
        .retain(|running| Some(&running.runtime) == current.as_ref());
    let carried = std::mem::take(&mut record.engram.carried_checks);
    let (closed, kept): (Vec<_>, Vec<_>) = carried
        .into_iter()
        .partition(|carried| engram_reported_end_stranded(carried, current.as_ref()));
    record.engram.carried_checks = kept;
    closed
        .iter()
        .map(|carried| engram_stranded_reported_end_line(&carried.check.command, current.as_ref()))
        .collect()
}

/// Whether `carried` is a Codex test still waiting for its end under a
/// runtime other than `current`, the session's: no end of it can reach the
/// host any more (`engram_take_stranded_reported_ends`).
fn engram_reported_end_stranded(
    carried: &EngramCarriedCheck,
    current: Option<&RuntimeToken>,
) -> bool {
    carried.ends_by_report && !carried.end_reported() && carried.runtime.as_ref() != current
}

/// The line closing a stranded carried Codex test with outcome Unknown;
/// `current`, the session's runtime now, tells whether its runtime was
/// cleared (none) or replaced (another).
fn engram_stranded_reported_end_line(
    command: &EngramCheckCommand,
    current: Option<&RuntimeToken>,
) -> String {
    engram_with_runtime_gone_caveat(
        &engram_closed_unended_carry_line(
            command,
            "no end can reach the host since its runtime was stopped or replaced",
        ),
        if current.is_none() { "cleared" } else { "replaced" },
    )
}

/// `line`, closing a carried Codex test still waiting for its end, with what
/// its runtime's going (`gone`: cleared, replaced or exited) does not prove:
/// the host stops counting the test as running, but its command may not have
/// ended, so a later check in its worktree may read what it still writes
/// (`EngramRunningCodexTest`).
fn engram_with_runtime_gone_caveat(line: &str, gone: &str) -> String {
    let line = line.strip_suffix('.').unwrap_or(line);
    format!(
        "{line}; its runtime was {gone}: this does not prove the command ended; a later \
         check in this worktree may read what it still writes."
    )
}

/// Takes out of `record` each Codex test carried until its end is reported
/// that is still waiting for its end and that `closes` selects, and returns
/// the line closing each with outcome Unknown because `why`.
fn engram_take_unended_carries(
    record: &mut SessionRecord,
    closes: impl Fn(&EngramCarriedCheck) -> bool,
    why: &str,
) -> Vec<String> {
    let carried = std::mem::take(&mut record.engram.carried_checks);
    let (closed, kept): (Vec<_>, Vec<_>) = carried
        .into_iter()
        .partition(|carried| carried.ends_by_report && !carried.end_reported() && closes(carried));
    record.engram.carried_checks = kept;
    closed
        .iter()
        .map(|carried| engram_closed_unended_carry_line(&carried.check.command, why))
        .collect()
}

/// The line closing a carried Codex test that never got its end with outcome
/// Unknown because `why`, in the words of a check whose end was never
/// reported (`engram_close_unended_checks`).
fn engram_closed_unended_carry_line(command: &EngramCheckCommand, why: &str) -> String {
    engram_unrecorded_check_line(
        command,
        &format!(
            "{why}, so it was closed with outcome Unknown and is not credited; run it again to \
             record it"
        ),
    )
}

/// A Codex test of a session still running when its turn closed, carried or
/// not (`EngramSessionState::running_codex_tests`): its Codex item key, the
/// runtime it runs under and the key of its worktree. It stands until its
/// end is reported under that key (`engram_forget_running_codex_test`) or
/// its runtime exits or is replaced; refusing, evicting or expiring its
/// carried evidence never removes it, since none of those ends its process.
///
/// Stated limitation: a runtime's exit, clear or replacement removes it
/// without proving its command ended. A shared Codex runtime's process
/// outlives a session's detach, and Stop interrupts only the active turn, not
/// a command left running from an earlier one; even a dedicated runtime's
/// exit can orphan the child. The host cannot observe either, so a later
/// check in its worktree may read what such a command still writes; the
/// holder is told so when a carried test is closed that way
/// (`engram_with_runtime_gone_caveat`). Keeping it instead would leave no release
/// but a host restart; ending such commands is a later change.
#[derive(Clone, Debug)]
struct EngramRunningCodexTest {
    key: String,
    runtime: RuntimeToken,
    root_key: String,
}

/// Notes `test` as still running, replacing any test under the same key.
/// Nothing else bounds the record: a count, an age or a slot limit would
/// drop a test whose process may still write, which is what it is kept
/// against. It leaves only by its end or its runtime's going.
fn engram_note_running_codex_test(engram: &mut EngramSessionState, test: EngramRunningCodexTest) {
    engram
        .running_codex_tests
        .retain(|running| running.key != test.key);
    engram.running_codex_tests.push(test);
}

/// The Codex test running under the item key `key` ended: it overlaps nothing
/// that starts after.
fn engram_forget_running_codex_test(engram: &mut EngramSessionState, key: &str) {
    engram.running_codex_tests.retain(|running| running.key != key);
}

/// The runtime key of a Codex test of this session still running when its
/// turn closed, under `current`, the session's runtime, in the worktree whose
/// path key is `root_key`, other than `starting`, the key of the check that
/// starts: it is still running there, so that check is overlapped by it, as
/// by a command still running in its turn (`note_engram_command_started`),
/// whether or not its evidence is still carried. A test whose end was
/// reported, or whose runtime is gone, overlaps nothing that starts after.
fn engram_carried_running_key(
    engram: &EngramSessionState,
    current: Option<&RuntimeToken>,
    root_key: &str,
    starting: &str,
) -> Option<String> {
    engram
        .running_codex_tests
        .iter()
        .find(|running| {
            Some(&running.runtime) == current
                && running.root_key == root_key
                && running.key != starting
        })
        .map(|running| running.key.clone())
}

/// Whether `carried` waits for the end reported under the runtime key `key`.
fn engram_carried_awaits_report(carried: &EngramCarriedCheck, key: &str) -> bool {
    carried.ends_by_report && carried.check.key == key && carried.check.end.is_none()
}

/// The provenance a carried check's closing snapshot is stamped with: that of
/// its opening snapshot, so the two compare as one root identity. Unconfirmed
/// when the opening snapshot is not ready now, which refuses the check.
fn engram_carried_start_provenance(check: &EngramTurnCheck) -> EngramRootCapture {
    match check.start_basis.wait_until(std::time::Instant::now()) {
        Some(Some(start)) => match (start.source_root_generation, start.source_root_state) {
            (Some(generation), Some(state)) => EngramRootCapture::Recorded {
                generation,
                state,
                workspace_id: start.workspace_id.clone(),
            },
            (None, None) => EngramRootCapture::Unnamed,
            _ => EngramRootCapture::Unconfirmed,
        },
        _ => EngramRootCapture::Unconfirmed,
    }
}

/// Settles `carried`, a Codex test carried until its end was reported, from
/// that end, as a check ending in its turn is resolved
/// (`engram_resolve_turn_checks_with_notices`): its snapshots, waited for
/// until `deadline`, must show one unchanged source, and its outcome comes
/// from its exit and result lines, or a declared check's artifact. `Ok(None)`
/// when a snapshot or the artifact reading is not ready in time, so the next
/// checkpoint tries again.
fn engram_settle_reported_end(
    carried: &EngramCarriedCheck,
    deadline: std::time::Instant,
) -> Result<Option<EngramResolvedCheck>, String> {
    let check = &carried.check;
    let Some(mut end) = check.end.clone() else {
        return Ok(None);
    };
    if let Some(capture) = &end.declared {
        if capture.wait_until(deadline).is_none() {
            return Ok(None);
        }
        end.declared = engram_declared_result_of(&end).map(EngramCapture::settled);
    }
    let start = match check.start_basis.wait_until(deadline) {
        Some(Some(start)) => start,
        Some(None) => return Err("its start source snapshot could not be captured".to_owned()),
        None => return Ok(None),
    };
    let finish = match end.end_basis.wait_until(deadline) {
        Some(Some(finish)) => finish,
        Some(None) => return Err("its finish source snapshot could not be captured".to_owned()),
        None => return Ok(None),
    };
    if start != finish {
        return Err(
            "its source snapshots differ, so the result belongs to no single revision".to_owned(),
        );
    }
    let outcome = match engram_declared_result_of(&end) {
        Some(result) => engram_declared_outcome(end.exit, &result),
        None => engram_check_end_outcome(&check.command, &end),
    }
    .ok_or_else(|| "its reported end marks no result the host can judge".to_owned())?;
    // As for a check in its turn: success needs positive evidence that tests
    // ran and passed, and an overlapped check's outcome is unknown.
    let ran_successfully = outcome == EngramExecutionOutcome::Succeeded
        || engram_declared_unknown_reason(&end).is_some();
    let outcome = if check.overlapped
        || (check.command.kind == EngramVerificationKind::Test
            && outcome == EngramExecutionOutcome::Succeeded
            && end.declared.is_none()
            && !end.showed_passing_tests)
    {
        EngramExecutionOutcome::Unknown
    } else {
        outcome
    };
    Ok(Some(EngramResolvedCheck {
        toolchain: check.toolchain.wait_until(deadline).flatten(),
        check: check.clone(),
        end,
        outcome,
        basis: start,
        ran_successfully,
    }))
}

impl AppState {
    /// Codex reported, for `session_id`'s thread, the end of the command its
    /// item `key` names, `command` with `output` and `exit`, whichever turn
    /// it names. A Codex test still running under that key since its turn
    /// closed runs no more, carried or not (`EngramRunningCodexTest`). When
    /// it ends a Codex test of this session carried
    /// since its turn closed (`engram_carry_running_codex_checks`), the end
    /// is recorded on it, its closing snapshot taken now, and the command no
    /// longer counts as running; the test is then settled at a later
    /// checkpoint on its claim. Returns whether it ended one; any other late
    /// end is left as it was, dropped.
    fn note_engram_reported_end(
        &self,
        session_id: &str,
        key: &str,
        command: &str,
        output: &str,
        exit: EngramCommandExit,
    ) -> bool {
        self.observe_test_command_end(session_id, &EngramObservationProvenance::Ambient, key, Some(exit));
        self.observe_test_command_artifact(
            session_id,
            &EngramObservationProvenance::Ambient,
            key,
            command,
            output,
            false,
        );
        // The output is read off the lock, for a carried test only.
        let started = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return false;
            };
            // Its process ended, whether or not its evidence is still carried.
            engram_forget_running_codex_test(&mut inner.sessions[index].engram, key);
            inner.sessions[index]
                .engram
                .carried_checks
                .iter()
                .find(|carried| engram_carried_awaits_report(carried, key))
                .map(|carried| carried.check.clone())
        };
        let Some(started) = started else {
            return false;
        };
        let reading =
            engram_read_check_end(Some(&started), session_id, Some(command), output, Some(exit));
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return false;
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let workers = record.engram.capture_workers.clone();
        let engram = &mut record.engram;
        let Some(carried) = engram.carried_checks.iter_mut().find(|carried| {
            engram_carried_awaits_report(carried, key)
                && carried.check.grant_id == started.grant_id
                && carried.check.sequence == started.sequence
        }) else {
            return false;
        };
        let provenance = engram_carried_start_provenance(&carried.check);
        engram_record_check_end(
            &mut carried.check,
            reading,
            Some(&started),
            output,
            Some(exit),
            false,
            &workers,
            provenance,
        );
        carried.terminal_at = Some(engram_interference_tick());
        engram.running_command_keys.remove(key);
        engram.withheld_command_keys.remove(key);
        engram
            .running_command_worktrees
            .retain(|(running, _)| running != key);
        true
    }
}
