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
//! `engram_turn_checks.rs`, `engram_carried_checks.rs`,
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
        command: &'a str,
        output: &'a str,
        exit: Option<EngramCommandExit>,
    },
    /// The command `key` will never report its end.
    CommandAbandoned { key: &'a str },
    /// The agent reported a file edit.
    WorkspaceEdit,
}

impl EngramHost<'_> {
    /// Reports what a recorder saw in `session_id`.
    pub(crate) fn observe(&self, session_id: &str, observation: EngramRecorderObservation<'_>) {
        match observation {
            EngramRecorderObservation::CommandStarted { key, ran, cwd } => {
                self.state
                    .note_engram_command_started(session_id, key, ran, cwd);
            }
            EngramRecorderObservation::CommandDescribed { key, ran, cwd } => {
                self.state
                    .note_engram_command_described(session_id, key, ran, cwd);
            }
            EngramRecorderObservation::CommandFinished {
                key,
                command,
                output,
                exit,
            } => {
                self.state
                    .note_engram_command_finished(session_id, key, command, output, exit);
            }
            EngramRecorderObservation::CommandAbandoned { key } => {
                self.state.note_engram_command_abandoned(session_id, key);
            }
            EngramRecorderObservation::WorkspaceEdit => {
                self.state.note_engram_workspace_edit(session_id);
            }
        }
    }

    /// The user wrote at `path` through TermAl itself: a file save, a Git
    /// file action, a terminal command starting or ending.
    pub(crate) fn host_write(&self, path: &FsPath) {
        self.state.note_engram_host_write(path);
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

    /// The watcher saw `changes` in a workspace. Its one caller, the watcher
    /// thread, is not built into tests, which call the handler directly.
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn workspace_files_changed(&self, changes: &[WorkspaceFileChangeEvent]) {
        self.state.note_engram_workspace_file_changes(changes);
    }

    /// The test-run index ticked: a carried gate's terminal record is read as
    /// soon as the tick finds it. Its one caller, the test-run index thread,
    /// is not built into tests, which call the poll directly.
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn test_run_tick(&self) {
        self.state.poll_engram_carried_runs();
        // A retained prompt whose delivery was withheld before handoff, and
        // whose grant is settled, is acknowledged and admitted again here
        // (`engram_abort_retry.rs`). Tests drive it with their own clock.
        self.state.engram_abort_retry_tick(chrono::Utc::now());
    }

    /// The exit a Codex command item states.
    pub(crate) fn codex_command_exit(item: &Value) -> EngramCommandExit {
        engram_codex_command_exit(item)
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

    /// The exit an ACP tool update states.
    pub(crate) fn acp_command_exit(update: &Value) -> EngramCommandExit {
        engram_acp_command_exit(update)
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
