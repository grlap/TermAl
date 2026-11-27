// Claude Bash test-launcher locators that the plain console path misses.
//
// Owns two narrow Claude-only routes to the existing artifact association
// (`observe_test_command_artifact_from` in test_run_artifacts.rs):
// - the launcher's own locator lines (RUN, STARTED, results:) in the visible
//   part of a Bash output Claude Code cut short, passed as output text;
// - at the end Claude reports for a background Bash task, the run directory
//   of the one host registration that matches the task's launch row, passed
//   as that typed registration directory, never as console text.
// The association validates either on disk exactly as it validates an uncut
// console locator. Nothing else from a cut output counts as evidence, and
// Claude Code's persisted output is never opened.
//
// Does not own the validation or attachment of a locator, the external Dart
// lane, verification settlement, or any other provider's output. New file;
// not split out of another.

use super::*;
use std::borrow::Cow;

/// Whether a trimmed output line is one of the launcher's own locators.
fn is_launcher_locator_line(line: &str) -> bool {
    line.starts_with("RUN ") || line.starts_with("STARTED ") || line.starts_with("results: ")
}

/// The launcher's own locator lines in `output`, and nothing else.
fn launcher_locator_lines(output: &str) -> String {
    output
        .lines()
        .map(str::trim)
        .filter(|line| is_launcher_locator_line(line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The ordinary test-launcher command a Claude Bash `line` runs, simple or
/// not, recognised in the Bash dialect as the launch row was when the call
/// started, so a quoted `|` reads alike. An external Dart launch is not one.
pub(crate) fn claude_launcher_command(line: &str) -> Option<EngramCheckCommand> {
    engram_check_command_in(line, EngramShellDialect::Bash)
        .filter(|command| EngramHost::launcher_arguments(command).is_some())
}

/// A recognised simple ordinary test-launcher command, the only command whose
/// locator these routes recover.
fn claude_ordinary_launcher(line: &str) -> Option<EngramCheckCommand> {
    claude_launcher_command(line).filter(|command| command.simple)
}

/// The focused stage's argv as bash passes it on, for the Claude Bash command
/// `line`: bash removes an unquoted backslash (`my\ crate` is one word) and
/// reduces `\\` inside double quotes. Only a direct launcher line is read. A
/// line that runs the launcher through a shell wrapper (`bash -c`,
/// `pwsh -Command`, `cmd /c`) names the shell as its program, never the
/// launcher's `node`, so it gives `None`: its run attaches nothing, and the
/// refusal is recorded.
pub(crate) fn claude_bash_focused_argv(line: &str) -> Option<Vec<String>> {
    let words = claude_bash_line_words(line)?;
    let (program, args) = words.split_first()?;
    engram_focused_launcher_words(&engram_program_name(program), args)?
        .ok()
        .map(<[String]>::to_vec)
}

/// The words of the test the Claude Bash command `line` runs, after its
/// one-call `pushd "DIR" &&` prefix, as bash passes them on.
fn claude_bash_line_words(line: &str) -> Option<Vec<String>> {
    // The line ends where bash ends it (`engram_bash_line_end`): a trailing
    // `\ ` is an escaped space inside the last word, and a final line
    // continuation is removed with its backslash.
    let line = engram_bash_line_end(line.trim_start());
    let test = engram_one_call_prefix(line).map_or(line, |(_, test)| test);
    engram_shell_words_as(test, true)
}

/// Whether the Claude Bash command `line` runs the launcher directly, its
/// program `node`, rather than through a shell wrapper whose program is the
/// shell (`bash -c`, `pwsh -Command`, `cmd /c`).
fn claude_bash_direct_launcher(line: &str) -> bool {
    claude_bash_line_words(line)
        .and_then(|words| words.into_iter().next())
        .is_some_and(|program| engram_program_name(&program) == "node")
}

/// Whether `line` is a simple ordinary launcher command, whose cut output
/// these routes read.
pub(crate) fn claude_ordinary_launcher_line(line: &str) -> bool {
    claude_ordinary_launcher(line).is_some()
}

/// Whether `line` runs an ordinary launcher at all, simple or not: its
/// background end either attaches its run or records why it does not.
pub(crate) fn claude_launcher_line(line: &str) -> bool {
    claude_launcher_command(line).is_some()
}

/// The output the artifact association reads, and whether it is cut. A
/// Claude Bash output of an ordinary launcher command that Claude Code cut is
/// reduced to the launcher's locator lines in its visible part and then read
/// as whole; every other cut output stays cut, which the association ignores.
pub(crate) fn test_command_artifact_output<'a>(
    provenance: &EngramObservationProvenance,
    command: &str,
    output: &'a str,
    output_cut: bool,
) -> (Cow<'a, str>, bool) {
    if output_cut
        && matches!(provenance, EngramObservationProvenance::Claude(_))
        && claude_ordinary_launcher_line(command)
    {
        return (Cow::Owned(launcher_locator_lines(output)), false);
    }
    (Cow::Borrowed(output), output_cut)
}

/// The output the end readers of a check read: its result lines and the run
/// records a launcher's `results:` line names. A cut Claude output keeps its
/// visible text for those diagnostics without its locator lines, so no record
/// is read from it there: its locators reach only the validated association
/// (`test_command_artifact_output`).
pub(crate) fn test_command_end_evidence_output<'a>(
    provenance: &EngramObservationProvenance,
    output: &'a str,
    output_cut: bool,
) -> Cow<'a, str> {
    if output_cut && matches!(provenance, EngramObservationProvenance::Claude(_)) {
        return Cow::Owned(
            output
                .lines()
                .filter(|line| !is_launcher_locator_line(line.trim()))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    Cow::Borrowed(output)
}

type TestRunTime = chrono::DateTime<chrono::Utc>;

impl AppState {
    /// At the end Claude reports for the background Bash task `key` of the
    /// runtime `token`, whose command `line` runs an ordinary launcher: its
    /// tool result marked only the launch, so no locator reached the launch
    /// row.
    /// The end means the launcher process ended, so its run has ended too.
    /// Takes the run directory from the one host registration of the row's
    /// session and worktree, in the launcher mode the command names, that
    /// started within the row's launch window and that no other launch row
    /// carries, when its run is terminal on disk with its end inside the
    /// row's lifetime; the association then validates it as a console locator,
    /// and requires a focused run's recorded command to be the row's own. A
    /// detached launch, whose end marks only its launch, a launch through a
    /// shell wrapper or in a compound line, zero or several such registrations (a registration whose
    /// start cannot be read counts as one), a sole one whose run has not ended
    /// within the row's lifetime, and one whose start another uncarried launch
    /// row's window also covers attach nothing and record why.
    pub(crate) fn attach_claude_background_launcher_artifact(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        key: &str,
        line: &str,
    ) {
        let Some(command) = claude_launcher_command(line) else {
            return;
        };
        let Some(words) = EngramHost::launcher_arguments(&command) else {
            return;
        };
        let provenance = EngramObservationProvenance::Claude(ClaudeObservationProvenance {
            token: token.clone(),
            origin: ClaudeWorkOrigin::Unattributed,
        });
        // The launcher mode the command itself names; a registration of
        // another mode is never its run.
        let TestRunCommandLauncher::Launcher(mode) = TestRunCommandLauncher::of(&command) else {
            return;
        };
        // Only the launcher's own options, before a focused run's `--`, can
        // detach it; a `--detach` after `--` is the test command's own word.
        let detached = words
            .iter()
            .skip(3)
            .take_while(|word| word.as_str() != "--")
            .any(|word| word == "--detach");
        let fingerprint = EngramHost::check_fingerprint(&command);
        let tool = sha256_hex(key.as_bytes());
        let time = |value: Option<&str>| value.and_then(EngramHost::check_timestamp);
        let slack = chrono::TimeDelta::seconds(ENGRAM_CARRIED_LAUNCH_SLACK_SECONDS);
        let (row, observed, candidates, rivals) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return;
            };
            let runtime = test_run_runtime_identity(&inner.sessions[index], &provenance);
            let mut rows = inner.test_run_observations.values().filter(|row| {
                row.summary.owner_session_id.as_deref() == Some(session_id)
                    && row.runtime_identity == runtime
                    && row.runtime_identity.is_some()
                    && row.tool_identity == tool
                    && row.command_fingerprint == fingerprint
            });
            let (Some(row), None) = (rows.next(), rows.next()) else {
                return;
            };
            // A row that already carries a run, or a native run that owns
            // its reservation, needs nothing from a registration.
            if row.artifact.is_some() || row.declared_native.is_some() {
                return;
            }
            let row = row.clone();
            let Some(observed) = time(row.summary.started_at.as_deref()) else {
                drop(inner);
                self.retain_test_command_artifact_refusal(
                    &row,
                    TestRunNativeCompletionRefusalCode::ArtifactObservationTimeUnavailable,
                    &[(TestRunNativeCompletionFact::ObservationTimeAvailable, false)],
                );
                return;
            };
            let worktree = test_run_path_key(&row.summary.worktree);
            let now = chrono::Utc::now();
            // A registration whose start cannot be read could have started
            // within the window, so it is one of the registrations counted.
            let candidates: Vec<(TestRunRegistration, Option<TestRunTime>)> = inner
                .test_run_registrations
                .values()
                .filter(|registration| {
                    registration.owning_session_id == session_id
                        && registration.declared_native.is_none()
                        && test_run_path_key(&registration.worktree) == worktree
                        && registration.preset == mode
                        && inner
                            .test_run_observations
                            .index()
                            .artifact_run(&registration.run_id)
                            .next()
                            .is_none()
                })
                .filter_map(|registration| {
                    let started = time(registration.started_at.as_deref());
                    started
                        .is_none_or(|started| started >= observed - slack && started <= now)
                        .then(|| (registration.clone(), started))
                })
                .collect();
            // The launch windows of the session's other uncarried launch rows
            // of the same worktree and launcher mode: a run whose start one of
            // them also covers is no unique match for this row. A rival whose
            // start cannot be read covers every run. A test runner run
            // directly launches no run; a row recorded before what it ran was
            // kept is compared by its preset, as any launcher of it could be.
            let rivals: Vec<Option<(TestRunTime, TestRunTime)>> = inner
                .test_run_observations
                .values()
                .filter(|other| {
                    other.execution_id != row.execution_id
                        && other.summary.owner_session_id.as_deref() == Some(session_id)
                        && other.artifact.is_none()
                        && other.declared_native.is_none()
                        && other
                            .launcher
                            .map_or(other.summary.preset == row.summary.preset, |launcher| {
                                launcher == TestRunCommandLauncher::Launcher(mode)
                            })
                        && test_run_path_key(&other.summary.worktree) == worktree
                })
                .map(|other| {
                    let started = time(other.summary.started_at.as_deref())?;
                    let ended = time(other.summary.ended_at.as_deref()).unwrap_or(now);
                    Some((started - slack, ended))
                })
                .collect();
            (row, observed, candidates, rivals)
        };
        use TestRunNativeCompletionFact::{
            LocatorPresent, ObservationMatches, SingleLocator, TerminalConfirmed,
        };
        use TestRunNativeCompletionRefusalCode::{
            ArtifactLocatorAmbiguous, ArtifactLocatorMissing, ArtifactRequestMismatch,
            TerminalNotConfirmed,
        };
        if detached {
            // A detached launch's task end is its launch, not its run's end.
            self.retain_test_command_artifact_refusal(
                &row,
                TerminalNotConfirmed,
                &[(TerminalConfirmed, false)],
            );
            return;
        }
        // Only one direct, simple launcher command is matched to a
        // registration, in every launcher mode: a wrapper's script reaches the
        // launcher through another shell's quoting, and a compound line's
        // other commands run around it.
        if !command.simple || !claude_bash_direct_launcher(line) {
            self.retain_test_command_artifact_refusal(
                &row,
                ArtifactRequestMismatch,
                &[(ObservationMatches, false)],
            );
            return;
        }
        // Exactly one registration this row's launcher could have run, counted
        // before any run is read from disk: a second one, even one whose run
        // has not ended, could be this row's, whose launcher may have died
        // before writing its results.
        let (registration, started) = match candidates.as_slice() {
            [(registration, started)] => (registration, *started),
            [] => {
                self.retain_test_command_artifact_refusal(
                    &row,
                    ArtifactLocatorMissing,
                    &[(LocatorPresent, false)],
                );
                return;
            }
            _ => {
                self.retain_test_command_artifact_refusal(
                    &row,
                    ArtifactLocatorAmbiguous,
                    &[(SingleLocator, false)],
                );
                return;
            }
        };
        // Off the lock, that one run is read only through the run root's guard
        // and the run reader's versioned regular-file read: it must have
        // ended, while this row's launcher could have run it.
        let root = EngramHost::run_artifact_directory(FsPath::new(&row.summary.worktree))
            .and_then(|root| fs::canonicalize(root).ok());
        let now = chrono::Utc::now();
        let ended = root.as_ref().is_some_and(|root| {
            let Ok(canonical) = fs::canonicalize(&registration.run_directory) else {
                return false;
            };
            if canonical != root.join(&registration.run_id) {
                return false;
            }
            TestRunDisk::read(&canonical, None)
                .filter(TestRunDisk::is_terminal)
                .and_then(|disk| disk.results)
                .and_then(|results| time(results.ended.as_deref()))
                .is_some_and(|ended| ended >= observed && ended <= now)
        });
        if !ended {
            self.retain_test_command_artifact_refusal(
                &row,
                ArtifactLocatorMissing,
                &[(LocatorPresent, false)],
            );
            return;
        }
        // Another uncarried launch row whose window covers the run's start
        // could equally have launched it; a start that cannot be read is
        // covered by every such window.
        let contested = rivals.iter().any(|window| match (window, started) {
            (Some((from, to)), Some(started)) => started >= *from && started <= *to,
            _ => true,
        });
        if contested {
            self.retain_test_command_artifact_refusal(
                &row,
                ArtifactLocatorAmbiguous,
                &[(SingleLocator, false)],
            );
            return;
        }
        // The association compares the registration's request identity with
        // the one request it reads from the run directory, and checks the
        // registration again when it attaches the run.
        self.observe_test_command_artifact_from(
            session_id,
            &provenance,
            key,
            line,
            TestCommandArtifactLocator::Registration {
                directory: FsPath::new(&registration.run_directory),
                request_identity: &registration.request_identity,
            },
            false,
        );
    }
}
