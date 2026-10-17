// Owns worker result metadata, evidence labels and current-turn observations.
// Does not collect Git output, grant commit authority or mint verification.
// Split from delegation prompt and terminal-result construction in delegations.rs.

fn worker_handoff_prompt(delegation: &DelegationRecord) -> String {
    let base = delegation.base_revision.as_deref().unwrap_or("unavailable");
    let worktree = worker_result_worktree_path(delegation).unwrap_or("unavailable");
    format!(
        "Worker handoff:\n\
- Host-recorded baseRevision: `{base}`; dedicated worktreePath: `{worktree}`.\n\
- In `Notes:` include `Git handoff:` with an ordinary Git diff or commit locator relative to that base, or explicitly say unavailable. Use the dedicated worktree, not the parent workspace.\n\
- An ordinary `git diff --binary <baseRevision>` omits untracked additions. Include new files explicitly through a task-authorized child-side intent-to-add/diff artifact, or an authorized local commit. Local staging and commits still require applicable task/repository authority; this guidance grants none.\n\
- An uncommitted workspace locator is mutable, not an immutable snapshot. Describe changes relative to the recorded base; neither workspace separation nor watcher paths prove who authored them.\n\
- In `Notes:` include `Child-reported native test outcome:` with the command, actual exit/counts and/or a result artifact locator, or explicitly say unavailable. Do not infer a test pass from completed status or Commands Run success.\n\
- This child-reported outcome is not host-recorded verification. Only the separate host/tracker record can establish that verification.\n\
- The parent chooses whether and how to integrate with ordinary Git. Completion and result retrieval do not stage, commit, import, merge, push, delete or synchronize either workspace."
    )
}

fn worker_result_worktree_path(delegation: &DelegationRecord) -> Option<&str> {
    match &delegation.write_policy {
        DelegationWritePolicy::IsolatedWorktree { worktree_path, .. } => worktree_path.as_deref(),
        _ => None,
    }
}

fn worker_current_attempt_messages(session: &Session) -> Option<&[Message]> {
    session
        .messages
        .iter()
        .rposition(|message| message.user_prompt_text().is_some())
        .map(|index| &session.messages[index + 1..])
}

/// Called once when terminal result construction snapshots workspace observations.
/// All input is resident state: no Git, filesystem lookup or verification lookup.
fn record_worker_handoff(
    inner: &StateInner,
    delegation: &DelegationRecord,
    result: &mut DelegationResult,
) {
    result.base_revision = delegation.base_revision.clone();
    result.worktree_path = worker_result_worktree_path(delegation).map(str::to_owned);
    result.worker_output_unavailable = Some(true);
    result.commands_run.clear();
    // Writable legacy changedFiles is watcher data too, not an attributable diff.
    result.changed_files.clear();
    result.observed_workspace_changes.clear();
    result.observed_workspace_changes_omitted = 0;
    if let Some(child) = inner
        .find_session_index(&delegation.child_session_id)
        .and_then(|index| inner.sessions.get(index))
    {
        // A queued follow-up is admitted before its prompt enters the transcript.
        // The recorded id remains authoritative even if its queue entry was
        // removed before failure/cancel settlement. Never fall back to an old
        // user message when that exact prompt has not been promoted.
        let attempt_messages = match delegation.queued_followup_prompt_id.as_deref() {
            Some(prompt_id) => child
                .session
                .messages
                .iter()
                .position(|message| {
                    matches!(message, Message::Text { id, author: Author::You, .. } if id == prompt_id)
                })
                .map(|index| &child.session.messages[index + 1..]),
            None => worker_current_attempt_messages(&child.session),
        };
        let unstarted_followup =
            delegation.queued_followup_prompt_id.is_some() && attempt_messages.is_none();
        result.worker_output_unavailable = Some(attempt_messages.is_none());
        let messages = attempt_messages.unwrap_or_default();
        if attempt_messages.is_none() {
            // A trimmed/missing prompt cannot establish current-attempt attribution.
            // Keep original prose in full output, never promote it as this handoff.
            result.notes.clear();
            result.notes.push(if unstarted_followup {
                "Worker attempt did not start, or its admitted prompt boundary is unavailable; current-attempt transcript evidence is unavailable."
                    .to_owned()
            } else {
                "Current-attempt boundary unavailable; child notes are not attributed to this attempt. Inspect full output separately.".to_owned()
            });
        }
        if result.status != DelegationStatus::Canceled {
            result.commands_run = messages
                .iter()
                .filter_map(|message| match message {
                    Message::Command {
                        command, status, ..
                    } => Some(DelegationCommandResult {
                        command: command.clone(),
                        status: status.label().to_owned(),
                    }),
                    _ => None,
                })
                .collect();
        }
        // The ordinary failure path retains only its summary. Recover Notes only
        // from this turn's latest failed packet; never reuse an earlier outcome.
        if attempt_messages.is_some()
            && result.status == DelegationStatus::Failed
            && let Some(parsed) = latest_assistant_message_delegation_result(&child.session)
            && parsed.status == DelegationStatus::Failed
        {
            for note in parsed.notes {
                if !result.notes.contains(&note) {
                    result.notes.push(note);
                }
            }
        }
        let mut observed = BTreeSet::new();
        for message in messages {
            if let Message::FileChanges { files, .. } = message {
                observed.extend(files.iter().map(|file| file.path.clone()));
            }
        }
        if attempt_messages.is_some() {
            observed.extend(child.active_turn_file_changes.keys().cloned());
        }
        result.observed_workspace_changes = observed.into_iter().collect();
    }
    if !result
        .notes
        .iter()
        .any(|note| note.trim().to_ascii_lowercase().starts_with("git handoff:"))
    {
        result.notes.push(
            "Git handoff: unavailable (the child supplied no locator for this attempt).".to_owned(),
        );
    }
    if !result.notes.iter().any(|note| {
        note.trim()
            .to_ascii_lowercase()
            .starts_with("child-reported native test outcome:")
    }) {
        result.notes.push("Child-reported native test outcome: unavailable (the child supplied no native outcome for this attempt).".to_owned());
    }
    result.notes.push("Worker handoff and native test outcome are child-reported, not host-recorded verification. commandsRun contains transcript statuses, not native test exit evidence; workspace watcher paths are observations, not an attributable Git diff.".to_owned());
    bound_delegation_workspace_observations(result);
}
