// Diagnostic-only launcher starts without a binding or grant. Owns bounded
// correlation and its refusal/tracking-loss lines, never source authority,
// check snapshots, carried verification, or persisted restart recovery.

#[derive(Clone, Copy, Debug)]
enum EngramLaunchMissingAuthority {
    Binding,
    Grant,
}

#[derive(Clone, Debug)]
struct EngramLaunchDiagnostic {
    key: String,
    command: EngramCheckCommand,
    runtime: Option<RuntimeToken>,
    turn_generation: u64,
    root: Option<PathBuf>,
    started_at: String,
    reason: EngramLaunchMissingAuthority,
}

// Location observation is independent of credit eligibility. Every candidate
// must resolve to a known directory in the same exact Git worktree; no fallback
// to the session's or claim's root supplies missing location information.
fn engram_launch_diagnostic_root(
    command: &EngramCheckCommand,
    workdir: &str,
    directories: Option<&[Option<String>]>,
) -> Option<PathBuf> {
    let one_call;
    let candidates = if let Some(directory) = command.directory.as_ref() {
        one_call = [Some(engram_resolve_shell_move(None, directory)?)];
        &one_call[..]
    } else {
        directories?
    };
    let mut observed: Option<PathBuf> = None;
    for directory in candidates {
        let resolved =
            engram_resolve_shell_move(Some(workdir), directory.as_deref().unwrap_or(workdir))?;
        let root = engram_worktree_root_path(FsPath::new(&resolved));
        if !root.join(".git").exists() {
            return None;
        }
        if observed
            .as_ref()
            .is_some_and(|prior| engram_exact_path_key(prior) != engram_exact_path_key(&root))
        {
            return None;
        }
        observed = Some(root);
    }
    observed
}

fn engram_launch_diagnostic_tracking_loss(
    record: &mut SessionRecord,
    mark: &EngramLaunchDiagnostic,
    why: &str,
) {
    let line = format!(
        "{ENGRAM_CHECK_CREDIT_LINE_PREFIX} command check {} observed starting at {}: {why}.",
        engram_check_fingerprint(&mark.command),
        mark.started_at,
    );
    eprintln!("engram> session={} {line}", record.session.id);
    record.engram.set_pending_source_root_line(line);
}

// Called only by an enabled Current observation. Retirement reports loss of
// diagnostic correlation, never a launch disposition or verification outcome.
fn engram_prune_launch_diagnostics(record: &mut SessionRecord) {
    let runtime = record.runtime.runtime_token();
    let generation = record.active_turn_generation;
    let mut position = 0;
    while position < record.engram.pending_launch_diagnostics.len() {
        let mark = &record.engram.pending_launch_diagnostics[position];
        if mark.runtime == runtime && mark.turn_generation == generation {
            position += 1;
        } else {
            let mark = record.engram.pending_launch_diagnostics.remove(position);
            engram_launch_diagnostic_tracking_loss(
                record,
                &mark,
                "its runtime or turn ended before its launch result; diagnostic tracking dropped",
            );
        }
    }
}

fn engram_note_launch_diagnostic(
    record: &mut SessionRecord,
    key: &str,
    command: EngramCheckCommand,
    root: Option<PathBuf>,
    reason: EngramLaunchMissingAuthority,
) {
    // Ordinary commands need no missing-authority lifecycle. Background
    // status is not known yet, so recognised full gates retain a start.
    if !engram_check_is_launcher_full(&command) {
        return;
    }
    if record.engram.pending_launch_diagnostics.len() >= ENGRAM_CARRIED_UNMATCHED_LAUNCH_LIMIT {
        let oldest = record.engram.pending_launch_diagnostics.remove(0);
        engram_launch_diagnostic_tracking_loss(
            record,
            &oldest,
            "the pending-command tracking limit dropped that command's diagnostic tracking",
        );
    }
    record
        .engram
        .pending_launch_diagnostics
        .push(EngramLaunchDiagnostic {
            key: key.to_owned(),
            command,
            runtime: record.runtime.runtime_token(),
            turn_generation: record.active_turn_generation,
            root,
            started_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            reason,
        });
}

fn engram_finish_launch_diagnostic(
    record: &mut SessionRecord,
    key: &str,
    command: &str,
    exit: Option<EngramCommandExit>,
    disposition: ClaudeObservationDisposition,
    enabled: bool,
) {
    if disposition != ClaudeObservationDisposition::Current {
        return;
    }
    if enabled {
        engram_prune_launch_diagnostics(record);
    }
    let Some(position) = record
        .engram
        .pending_launch_diagnostics
        .iter()
        .position(|mark| {
            mark.key == key
                && mark.runtime == record.runtime.runtime_token()
                && mark.turn_generation == record.active_turn_generation
        })
    else {
        return;
    };
    // Consume only this runtime/turn's start, once. A changed command never
    // borrows its identity. No completion-only fallback invents a start.
    let mark = record.engram.pending_launch_diagnostics.remove(position);
    if !enabled {
        return;
    }
    if engram_check_command(command).as_ref() != Some(&mark.command) {
        engram_launch_diagnostic_tracking_loss(
            record,
            &mark,
            "its command at completion did not match its start",
        );
        return;
    }
    let (why, unmatched) = match engram_launch_disposition(
        &mark.command,
        exit.unwrap_or(EngramCommandExit::Unknown),
    ) {
        EngramLaunchDisposition::Ordinary => return,
        EngramLaunchDisposition::Carry => (
            match mark.reason {
                EngramLaunchMissingAuthority::Binding => {
                    "it began without a binding to claimed work, so it was not carried"
                }
                EngramLaunchMissingAuthority::Grant => {
                    "the turn has no active grant at its start, so it was not carried"
                }
            },
            true,
        ),
        EngramLaunchDisposition::Drop(why) => (why, why == ENGRAM_LAUNCH_DROP_COMPOUND),
    };
    if unmatched && let Some(root) = mark.root {
        engram_note_unmatched_launch(record, &root, &mark.started_at);
    }
    let line = engram_dropped_launch_line(&mark.command, why);
    eprintln!("engram> session={} {line}", record.session.id);
    record.engram.set_pending_source_root_line(line);
}
