//! `EngramHost`: what the rest of the host may ask of the Engram host
//! component's check machinery (docs/features/host-architecture.md, sections
//! 5.2 and 9).
//!
//! Owns the facade and nothing else: the typed observation a provider's
//! recorder reports, the host facts the routes, the turn dispatch, the
//! watcher and the test-run tick report, the exit a runtime's own result
//! states, and the two host lines a reset or a restart leaves for a carried
//! gate. Every operation calls the handler that did the work before the
//! facade existed, with the same arguments, at the same point and under the
//! same locks, so nothing about a check or its credit changes here.
//!
//! Does not own the checks, their captures, the overlap marking, the carried
//! fence or the checkpoint's composition: those stay in the fragments
//! `engram_turn_checks.rs`, `engram_carried_checks.rs`, `engram_reported_ends.rs`,
//! `engram_check_recognition.rs`, `engram_launcher_stages.rs` and
//! `engram_one_call.rs`, which only the
//! component and this facade name. Does not own turn admission, source roots,
//! settings or recovery, which keep their own entry points until their moves
//! (section 5.5).
//!
//! New module; nothing was split out of another file.
use super::*;

/// The Engram host component, as the rest of the host reaches it.
pub(crate) struct EngramHost<'a> {
    state: &'a AppState,
}

impl AppState {
    pub(crate) fn engram_host(&self) -> EngramHost<'_> {
        EngramHost { state: self }
    }
}

/// What a provider's recorder saw in a session: one event of a command the
/// runtime runs, by the key the runtime gave it, or an edit the agent
/// reported, which names no path.
pub(crate) enum EngramRecorderObservation<'a> {
    /// The runtime started the command `key`, running `ran` in `cwd` where
    /// it said so.
    CommandStarted {
        key: &'a str,
        ran: Option<&'a str>,
        cwd: Option<&'a str>,
    },
    /// The runtime said again what the running command `key` runs or where,
    /// without starting or ending it.
    CommandDescribed {
        key: &'a str,
        ran: Option<&'a str>,
        cwd: Option<&'a str>,
    },
    /// The command `key` ended, with what the runtime said about its end.
    CommandFinished {
        key: &'a str,
        /// Transcript label, which may be only a provider's tool title.
        command: &'a str,
        /// Actual command text supplied at completion, separate from its label.
        reported_command: Option<&'a str>,
        output: &'a str,
        exit: Option<EngramCommandExit>,
        runtime_output_cut: bool,
    },
    /// The command `key` will never report its end.
    CommandAbandoned { key: &'a str },
    /// The agent reported a file edit, with the file its tool named where it
    /// named one. The target is shown as the tool gave it, for diagnostics
    /// only: an edit fences whatever it names.
    WorkspaceEdit { target: Option<&'a str> },
}

impl EngramHost<'_> {
    /// Reports what a recorder saw in `session_id`, produced as `provenance`
    /// says. Every handler reaches the observation, because where a command
    /// runs and what it may write fences carried gates and other sessions'
    /// checks whoever ran it. Whether the session's grant may take it is
    /// decided inside each handler, under the state lock in the section that
    /// applies it (`claude_observation_disposition`): work of another turn,
    /// of a turn no prompt owns or of a replaced runtime is attributed to no
    /// grant, and the live grant is excluded from it
    /// (`claude_outstanding_work.rs`).
    pub(crate) fn observe(
        &self,
        session_id: &str,
        provenance: &EngramObservationProvenance,
        observation: EngramRecorderObservation<'_>,
    ) {
        match observation {
            EngramRecorderObservation::CommandStarted { key, ran, cwd } => {
                self.state
                    .note_engram_command_started(session_id, provenance, key, ran, cwd);
            }
            EngramRecorderObservation::CommandDescribed { key, ran, cwd } => {
                self.state
                    .note_engram_command_described(session_id, provenance, key, ran, cwd);
            }
            EngramRecorderObservation::CommandFinished {
                key,
                command,
                reported_command,
                output,
                exit,
                runtime_output_cut,
            } => {
                self.state.note_engram_command_finished_reported(
                    session_id,
                    provenance,
                    key,
                    command,
                    reported_command,
                    output,
                    exit,
                    runtime_output_cut,
                );
            }
            EngramRecorderObservation::CommandAbandoned { key } => {
                self.state
                    .note_engram_command_abandoned(session_id, provenance, key);
            }
            EngramRecorderObservation::WorkspaceEdit { target } => {
                self.state
                    .note_engram_workspace_edit(session_id, provenance, target);
            }
        }
    }

    /// The user wrote at `path` through TermAl itself: a file save, a Git
    /// file action, a terminal command starting or ending.
    pub(crate) fn host_write(&self, path: &FsPath) {
        self.state.note_engram_host_write(path);
    }

    /// The session in `record` is composing the prompt of a turn it starts:
    /// a check an earlier turn left live with no end reported (its close
    /// repeated a cached report or was skipped) is closed with outcome
    /// Unknown, and the holder line that says so is published now, so this
    /// very prompt carries it. The closed checks leave the live checks, so the
    /// begin's clear does not close them again (`engram_close_unended_checks`).
    /// Called under the state lock, by the dispatch that holds it, before the
    /// prompt's host line is composed.
    pub(crate) fn turn_prompt_composing(record: &mut SessionRecord) {
        for line in
            engram_close_unended_checks(&mut record.engram, None, "the next turn's start")
        {
            engram_publish_check_notice(
                &record.session.id,
                &mut record.engram,
                line,
                &mut std::io::stderr(),
            );
        }
        record
            .engram
            .active_turn_checks
            .retain(|check| check.end.is_some());
    }

    /// The runtime `exited` of the session in `record` exited: every Codex
    /// test carried under it until its end is reported is closed with outcome
    /// Unknown, since no end can come from it any more, and its holder told
    /// (`engram_close_reported_end_carries`). Called under the state lock, by
    /// the exit handler, once it matched the exiting runtime to the session.
    pub(crate) fn runtime_exited(record: &mut SessionRecord, exited: &RuntimeToken) {
        engram_close_reported_end_carries(record, exited);
    }

    /// The runtime of the session in `record` was cleared (stopped, reset or
    /// gone): every Codex test carried under a runtime that is no longer the
    /// session's, still waiting for its end, is closed with outcome Unknown,
    /// since no end of it can reach the host any more, and its holder told
    /// (`engram_take_stranded_reported_ends`). Called under the state lock,
    /// by `SessionRecord::clear_runtime`, once the runtime is cleared.
    pub(crate) fn runtime_cleared(record: &mut SessionRecord) {
        let session_id = record.session.id.clone();
        for line in engram_take_stranded_reported_ends(record) {
            engram_publish_check_notice(
                &session_id,
                &mut record.engram,
                line,
                &mut std::io::stderr(),
            );
        }
    }

    /// A turn of `session_id` is about to start: its worktree is resolved
    /// off the state lock for the marks the start makes under it.
    pub(crate) fn turn_starting(&self, session_id: &str) {
        self.state.note_engram_session_worktree_off_lock(session_id);
    }

    /// The turn of the session at `index` started. Called under the state
    /// lock, by the dispatch that holds it.
    pub(crate) fn turn_started(inner: &mut StateInner, index: usize) {
        engram_note_turn_started(inner, index);
    }

    /// A turn of the session at `index` began beside the turn it is already
    /// running (a Claude turn no prompt owns): it may now write under a check
    /// another session of its worktree has open, while the commands of the
    /// session's own turn go on. Called under the state lock.
    pub(crate) fn turn_running_beside(inner: &mut StateInner, index: usize) {
        engram_mark_checks_overlapped_by(inner, index, EngramWriterAct::Presence);
    }

    /// Where the session in `record` may write now: its workdir's worktree,
    /// as last resolved (`None` when never resolved, so any), and its grant's
    /// named source root while one is held. For outstanding Claude work
    /// registered now (`claude_outstanding_work.rs`).
    pub(crate) fn session_write_locations(record: &SessionRecord) -> Vec<Option<String>> {
        let mut locations = vec![engram_session_worktree(record)];
        if let Some(root) = engram_turn_named_root_key(record) {
            let root = Some(root);
            if !locations.contains(&root) {
                locations.push(root);
            }
        }
        locations
    }

    /// The next instant of the interference clock
    /// (`engram_claude_interference.rs`).
    pub(crate) fn interference_tick() -> u64 {
        engram_interference_tick()
    }

    /// The outstanding Claude work `keys` of the session at `owner` was
    /// registered, promoted or placed further: every unpublished record and
    /// live grant it may overlap is reconciled with it
    /// (`engram_reconcile_claude_work`). Called under the state lock.
    pub(crate) fn reconcile_claude_work(inner: &mut StateInner, owner: usize, keys: &[String]) {
        engram_reconcile_claude_work(inner, owner, keys);
    }

    /// The deleted session `session_id` left its outstanding Claude work on
    /// the host (`engram_reconcile_orphaned_claude_work`). Called under the
    /// state lock, in the section that removes the session.
    pub(crate) fn reconcile_orphaned_claude_work(inner: &mut StateInner, session_id: &str) {
        engram_reconcile_orphaned_claude_work(inner, session_id);
    }

    /// Why a check of the session at `index` starting now in the worktree
    /// with key `root` would be fenced by another session's or a deleted
    /// session's Claude work (`engram_claude_current_restriction`). Called
    /// under the state lock.
    pub(crate) fn claude_current_restriction(
        inner: &StateInner,
        index: Option<usize>,
        root: &str,
    ) -> Option<ClaudeHazardCause> {
        engram_claude_current_restriction(inner, index, root)
    }

    /// Whether the command line `line` is a recognised, simple full-gate
    /// launch (`engram_is_simple_full_launcher`).
    pub(crate) fn is_simple_full_launcher_line(line: &str) -> bool {
        engram_check_command(line).is_some_and(|command| engram_is_simple_full_launcher(&command))
    }

    /// The existing command recognizer, for run visibility without granting credit.
    pub(crate) fn recognized_check(line: &str) -> Option<EngramCheckCommand> {
        engram_check_command(line)
    }

    /// The check machinery's canonical identity for an already recognized command.
    pub(crate) fn check_fingerprint(command: &EngramCheckCommand) -> String {
        engram_check_fingerprint(command)
    }

    /// The existing launcher's arguments, without adding a recognition rule.
    pub(crate) fn launcher_arguments(command: &EngramCheckCommand) -> Option<Vec<String>> {
        engram_launcher_words(command)
    }

    /// Whether a recognized command is a full launcher run.
    pub(crate) fn is_full_launcher(command: &EngramCheckCommand) -> bool {
        engram_check_is_launcher_full(command)
    }

    /// Whether this result describes a launch rather than the test run's end.
    pub(crate) fn is_launch_only(command: &EngramCheckCommand, exit: EngramCommandExit) -> bool {
        !matches!(engram_launch_disposition(command, exit), EngramLaunchDisposition::Ordinary)
    }

    /// Resolves the check component's artifact directory; call outside StateInner.
    pub(crate) fn run_artifact_directory(root: &FsPath) -> Option<PathBuf> {
        engram_git_run_directory(root)
    }

    /// Parses the timestamp format accepted by the existing carried-run matcher.
    pub(crate) fn check_timestamp(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
        engram_parse_time(value)
    }

    /// The host accepted the first registration of the launcher run in
    /// `run_directory`, already canonical: its place in the watcher order,
    /// which a watcher change exempted from that run's gate must precede
    /// (`engram_note_run_registered`). Reads no file.
    pub(crate) fn note_test_run_registered(run_directory: &FsPath) {
        engram_note_run_registered(run_directory);
    }

    /// When a watcher notification reaching the host now arrived. The
    /// watcher's notify callback takes it before the event waits behind
    /// earlier events (`engram_watcher_arrival_now`).
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn watcher_arrival_now() -> EngramWatcherArrival {
        engram_watcher_arrival_now()
    }

    /// The watcher saw `changes` in a workspace. Its one caller, the watcher
    /// thread, is not built into tests, which call the handler directly.
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn workspace_files_changed(&self, changes: &[WorkspaceFileChangeEvent]) {
        self.state.note_engram_workspace_file_changes(changes);
    }

    /// The watcher saw `changes` in a workspace, with its bounded diagnostic
    /// `observation`, in a notification that arrived at `arrival` (`None`
    /// when unknown: such a change fences as any change).
    pub(crate) fn observed_workspace_files_changed(
        &self,
        changes: &[WorkspaceFileChangeEvent],
        observation: &str,
        arrival: Option<EngramWatcherArrival>,
    ) {
        self.state
            .note_engram_observed_workspace_file_changes(changes, observation, arrival);
    }

    /// The test-run index ticked: a carried gate's terminal record is read as
    /// soon as the tick finds it. Its one caller, the test-run index thread,
    /// is not built into tests, which call the poll directly.
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn test_run_tick(&self) {
        self.state.poll_engram_carried_runs();
        // The automatic Engram retries have their own tick thread
        // (`spawn_engram_retry_tick`), so a slow Engram never holds up
        // carried-gate polling or the test-run index.
        self.state.schedule_engram_root_reclamation(false);
    }

    /// The exit a Codex command item states.
    pub(crate) fn codex_command_exit(item: &Value) -> EngramCommandExit {
        engram_codex_command_exit(item)
    }

    /// Codex reported, on `session_id`'s own thread, the end of the command
    /// its item `key` names in a turn that is no longer running, an end the
    /// turn's events otherwise drop. It is taken only for a Codex test of that
    /// session carried since its turn closed under exactly that item key
    /// (`note_engram_reported_end`); the transcript is not touched. Returns
    /// whether it was taken.
    pub(crate) fn codex_reported_end(
        &self,
        session_id: &str,
        key: &str,
        command: &str,
        output: &str,
        exit: EngramCommandExit,
    ) -> bool {
        self.state
            .note_engram_reported_end(session_id, key, command, output, exit)
    }

    /// The exit a Claude Bash tool result states.
    pub(crate) fn claude_command_exit(
        is_error: bool,
        interrupted: bool,
        background: bool,
        detail: &str,
    ) -> EngramCommandExit {
        engram_claude_command_exit(is_error, interrupted, background, detail)
    }

    /// The exit an ACP tool update states for `agent`.
    pub(crate) fn acp_command_exit(agent: AcpAgent, update: &Value) -> EngramCommandExit {
        engram_acp_command_exit(agent, update)
    }

    /// The host line for a background gate that was running when the host
    /// went down.
    pub(crate) fn carried_lost_to_restart_line(marker: &EngramCarriedLaunchMarker) -> String {
        engram_carried_lost_to_restart_line(marker)
    }

    /// The host lines for the carried gates `record` loses when its Engram
    /// state is reset, and `why`.
    pub(crate) fn carried_checks_reset_lines(record: &SessionRecord, why: &str) -> Vec<String> {
        engram_carried_checks_reset_lines(record, why)
    }
}
