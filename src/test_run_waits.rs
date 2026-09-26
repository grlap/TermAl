// Run waits (docs/features/test-runs.md, slice 2 "Run waits"): a session
// registers a wait on test runs, ends its turn, stays available to the user,
// and is resumed with one bounded prompt when the runs settle.
//
// Owns: the wait record and its wire types, registration (validation, root
// sessions only, the bounded forced rescan, the 48 KiB bound on mandatory
// prompt content), the settle rule, the resume prompt and its budgets,
// consuming waits (completed, Stop, a removed or unavailable session), and
// dispatching resumes. The prompt
// queue and its dispatch rules are the delegation-wait ones
// (`queue_delegation_wait_resume_locked`), reused unchanged.
//
// Does not own: the index (test_runs.rs, whose rescan thread refreshes waits
// after each scan and exempts their runs from retention), delegation waits
// (delegations.rs, not widened), the card (test_run_cards.rs), or the waiting
// indicator (ui/).
//
// New module, not split from another file.

/// Most runs one wait may name.
const MAX_TEST_RUN_WAIT_RUNS: usize = 16;
/// Every run's header and commands must fit this, which leaves room for
/// excerpts in the resume prompt.
const TEST_RUN_WAIT_MANDATORY_MAX_BYTES: usize = 48 * 1024;
/// The whole resume prompt.
const TEST_RUN_WAIT_PROMPT_MAX_BYTES: usize = 64 * 1024;
/// One run's failure excerpt, at most.
const TEST_RUN_WAIT_EXCERPT_MAX_BYTES: usize = 8 * 1024;
/// How long registration waits for the forced rescan when an id is unknown.
#[cfg(not(test))]
const TEST_RUN_WAIT_FORCED_RESCAN_BOUND: Duration = Duration::from_secs(1);

/// `any` or `all`, the same values and default as a delegation wait.
type TestRunWaitMode = DelegationWaitMode;

/// A run as the wait saw it at registration, never updated, so a resume after
/// the run left the index still names its evidence.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunWaitRunLabel {
    run_id: String,
    run_dir: String,
    preset: TestRunPreset,
    worktree: String,
    owner_session_id: Option<String>,
    started_at: Option<String>,
}

/// A pending run wait. It exists only while pending.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunWaitRecord {
    id: String,
    /// The waiting session.
    session_id: String,
    /// Registration order, 1 to 16.
    run_ids: Vec<String>,
    mode: TestRunWaitMode,
    created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    /// Same order as `run_ids`.
    runs: Vec<TestRunWaitRunLabel>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum TestRunWaitConsumedReason {
    Completed,
    SessionStopped,
    SessionUnavailable,
    SessionRemoved,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateTestRunWaitRequest {
    run_ids: Vec<String>,
    #[serde(default)]
    mode: TestRunWaitMode,
    #[serde(default)]
    title: Option<String>,
}

/// The registration result: the contract's `{ waitId, runIds, mode }`, plus
/// the record and whether a resume was queued at once (runs already settled).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunWaitResponse {
    revision: u64,
    wait_id: String,
    run_ids: Vec<String>,
    mode: TestRunWaitMode,
    wait: TestRunWaitRecord,
    resume_prompt_queued: bool,
    resume_dispatch_requested: bool,
    server_instance_id: String,
}

/// A settled run's verdict, as the resume prompt names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TestRunWaitVerdict {
    Pass,
    Fail,
    Unknown(TestRunCardUnknownReason),
}

/// Whether a run is settled for a wait, and how. Passed and failed are; so is
/// unknown with `processGone` or `noPid`, and a run no longer indexed whose
/// directory is `gone` (it was indexed at registration, so it left the index
/// after). A run missing from the index whose directory is still there is
/// not settled: a rescan that was in flight at registration may have dropped
/// it before the wait's retention exemption applied, and the next scan
/// indexes it again. Running, unknown with `resultsUnreadable` or with no
/// reason are not settled either: the run may still finish, and the index
/// retries.
fn test_run_wait_verdict(summary: Option<&TestRunSummary>, gone: bool) -> Option<TestRunWaitVerdict> {
    let Some(summary) = summary else {
        return gone.then_some(TestRunWaitVerdict::Unknown(
            TestRunCardUnknownReason::NotIndexed,
        ));
    };
    match (summary.state, summary.unknown_reason) {
        (TestRunState::Passed, _) => Some(TestRunWaitVerdict::Pass),
        (TestRunState::Failed, _) => Some(TestRunWaitVerdict::Fail),
        (TestRunState::Unknown, Some(reason @ (TestRunUnknownReason::ProcessGone | TestRunUnknownReason::NoPid))) => {
            Some(TestRunWaitVerdict::Unknown(reason.into()))
        }
        _ => None,
    }
}

/// Each run's verdict, in registration order. `gone` answers, for a run no
/// longer indexed, whether it is gone from disk (`test_run_wait_run_gone`).
fn test_run_wait_verdicts(
    wait: &TestRunWaitRecord,
    summaries: &[Option<&TestRunSummary>],
    gone: impl Fn(&str) -> bool,
) -> Vec<Option<TestRunWaitVerdict>> {
    wait.run_ids
        .iter()
        .zip(summaries)
        .map(|(run_id, summary)| test_run_wait_verdict(*summary, summary.is_none() && gone(run_id)))
        .collect()
}

/// The directory a label's `runDir` names. That is the wire's display path
/// (`test_run_display_path`: no verbatim prefix, forward slashes), which is
/// an absolute path as it stands except a verbatim UNC path on Windows:
/// `\\?\UNC\server\share\...` displays as `UNC/server/share/...`, relative
/// on its own.
fn test_run_fs_path(display: &str) -> PathBuf {
    #[cfg(windows)]
    if let Some(rest) = display.strip_prefix("UNC/") {
        return PathBuf::from(format!(r"\\?\UNC\{}", rest.replace('/', r"\")));
    }
    PathBuf::from(display)
}

/// Whether a run that left the index is gone from disk: its request.json,
/// which is what indexes it, is missing (with its directory or alone) or
/// cannot be read. The launcher writes it by rename, and only while creating
/// the run: a non-terminal run then, which the index keeps through a failed
/// read. So one found unreadable here stays unreadable. A run still readable
/// on disk is indexed again by a later scan.
fn test_run_wait_run_gone(run_dir: &FsPath) -> bool {
    !matches!(
        test_run_read_json(&run_dir.join("request.json")),
        TestRunJson::Parsed(..)
    )
}

fn test_run_wait_verdict_label(verdict: Option<TestRunWaitVerdict>) -> String {
    match verdict {
        Some(TestRunWaitVerdict::Pass) => "PASS".to_owned(),
        Some(TestRunWaitVerdict::Fail) => "FAIL".to_owned(),
        Some(TestRunWaitVerdict::Unknown(reason)) => format!(
            "UNKNOWN ({})",
            match reason {
                TestRunCardUnknownReason::ProcessGone => "process gone",
                TestRunCardUnknownReason::ResultsUnreadable => "results unreadable",
                TestRunCardUnknownReason::NoPid => "no pid recorded",
                TestRunCardUnknownReason::NotIndexed => "not indexed",
            }
        ),
        None => "NOT SETTLED YET".to_owned(),
    }
}

fn test_run_wait_preset_label(preset: TestRunPreset) -> &'static str {
    match preset {
        TestRunPreset::Full => "full",
        TestRunPreset::Focused => "focused",
        TestRunPreset::Live => "live",
    }
}

/// One run's header and commands: never cut. `summary` is the run as
/// indexed now, if it still is; the label is the registration's. `failure`
/// is the first failure read from the run's results.json, when one was.
fn test_run_wait_run_section(
    label: &TestRunWaitRunLabel,
    summary: Option<&TestRunSummary>,
    verdict: Option<TestRunWaitVerdict>,
    failure: Option<&TestRunCardFailure>,
) -> String {
    let optional = |value: Option<&str>| value.map_or_else(|| "unknown".to_owned(), str::to_owned);
    let run_dir = summary.map_or(label.run_dir.as_str(), |summary| summary.run_dir.as_str());
    let mut lines = vec![
        format!(
            "### Run `{}` ({})",
            label.run_id,
            test_run_wait_preset_label(summary.map_or(label.preset, |summary| summary.preset))
        ),
        format!("- Verdict: {}", test_run_wait_verdict_label(verdict)),
        format!(
            "- Interrupted: {}",
            if summary.is_some_and(|summary| summary.interrupted) {
                "yes"
            } else {
                "no"
            }
        ),
        format!(
            "- Exit code: {}",
            summary
                .and_then(|summary| summary.exit_code)
                .map_or_else(|| "none".to_owned(), |code| code.to_string())
        ),
        format!(
            "- Started: {}",
            optional(
                summary
                    .and_then(|summary| summary.started_at.as_deref())
                    .or(label.started_at.as_deref())
            )
        ),
        format!(
            "- Ended: {}",
            optional(summary.and_then(|summary| summary.ended_at.as_deref()))
        ),
        format!("- Run directory: `{run_dir}`"),
    ];
    match verdict {
        Some(TestRunWaitVerdict::Fail) => {
            // The read names a preflight check or a stage past the summary's
            // 64; the summary's stages are the fallback when it names none.
            let first = failure
                .map(|failure| (failure.phase, failure.name.as_str()))
                .or_else(|| {
                    summary
                        .and_then(|summary| {
                            summary
                                .stages
                                .iter()
                                .find(|stage| stage.state == TestRunStageState::Failed)
                        })
                        .map(|stage| (TestRunCardFailurePhase::Stage, stage.name.as_str()))
                });
            if let Some((phase, name)) = first {
                let phase = match phase {
                    TestRunCardFailurePhase::Stage => "stage",
                    TestRunCardFailurePhase::Preflight => "preflight",
                };
                lines.push(format!("- First failing: {phase} `{name}`"));
            }
        }
        Some(TestRunWaitVerdict::Unknown(_)) | None => {
            lines.push(format!(
                "- Stage at the last observation: {}",
                summary
                    .and_then(|summary| summary.current_stage.as_deref())
                    .map_or_else(|| "none".to_owned(), |stage| format!("`{stage}`"))
            ));
        }
        Some(TestRunWaitVerdict::Pass) => {}
    }
    lines.push(format!(
        "- Inspect: `node scripts/test-launcher.mjs summary \"{run_dir}\"`"
    ));
    if matches!(verdict, Some(TestRunWaitVerdict::Unknown(_))) {
        lines.push(format!(
            "- Settle it: `node scripts/test-launcher.mjs recover \"{run_dir}\"` (never a pass)"
        ));
    }
    lines.join("\n")
}

/// The prompt's fixed part: the title, the wait's identity and one overview
/// line per run.
fn test_run_wait_prompt_head(
    wait: &TestRunWaitRecord,
    verdicts: &[Option<TestRunWaitVerdict>],
) -> String {
    let title = wait.title.as_deref().unwrap_or("Test run wait completed");
    let mode = match wait.mode {
        DelegationWaitMode::Any => "any",
        DelegationWaitMode::All => "all",
    };
    let overview = wait
        .runs
        .iter()
        .zip(verdicts)
        .map(|(label, verdict)| {
            format!(
                "- `{}`: {}",
                label.run_id,
                test_run_wait_verdict_label(*verdict)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{title}\n\nWait id: `{}`\nMode: `{mode}`\nSession: `{}`\n\nRuns:\n{overview}\n\nResults:\n",
        wait.id, wait.session_id
    )
}

/// The longest run of consecutive backticks in `text`.
fn test_run_wait_longest_backtick_run(text: &str) -> usize {
    text.split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0)
}

/// The first failure a read found for a run, if it found one.
fn test_run_wait_failure<'a>(
    failures: &'a HashMap<String, TestRunCardFailureRead>,
    run_id: &str,
) -> Option<&'a TestRunCardFailure> {
    match failures.get(run_id) {
        Some(TestRunCardFailureRead::Read {
            failure: Some(failure),
            ..
        }) => Some(failure),
        _ => None,
    }
}

/// The resume prompt. Headers and commands are never cut; each failed run's
/// excerpt gets min(8 KiB, (64 KiB minus everything else) / number of runs).
/// `UNKNOWN` is never phrased as a pass. `verdicts` are the runs' verdicts
/// (`test_run_wait_verdicts`), in registration order.
fn build_test_run_wait_prompt(
    wait: &TestRunWaitRecord,
    summaries: &[Option<&TestRunSummary>],
    verdicts: &[Option<TestRunWaitVerdict>],
    failures: &HashMap<String, TestRunCardFailureRead>,
) -> String {
    let head = test_run_wait_prompt_head(wait, verdicts);
    let sections: Vec<String> = wait
        .runs
        .iter()
        .zip(summaries)
        .zip(verdicts)
        .map(|((label, summary), verdict)| {
            test_run_wait_run_section(
                label,
                *summary,
                *verdict,
                test_run_wait_failure(failures, &label.run_id),
            )
        })
        .collect();
    let separator = "\n\n---\n\n";
    let mandatory = head.len()
        + sections.iter().map(String::len).sum::<usize>()
        + separator.len() * sections.len();
    let per_run = (TEST_RUN_WAIT_PROMPT_MAX_BYTES.saturating_sub(mandatory) / wait.runs.len().max(1))
        .min(TEST_RUN_WAIT_EXCERPT_MAX_BYTES);
    let parts: Vec<String> = sections
        .into_iter()
        .zip(&wait.runs)
        .zip(verdicts)
        .map(|((section, label), verdict)| {
            if *verdict != Some(TestRunWaitVerdict::Fail) {
                return section;
            }
            let failure = test_run_wait_failure(failures, &label.run_id);
            let excerpt = failure.map_or("", |failure| failure.excerpt.as_str());
            // Longer than any run of backticks in the excerpt, so test output
            // that contains a fence cannot close this one.
            let fence = "`".repeat(test_run_wait_longest_backtick_run(excerpt).max(2) + 1);
            // The fences and their label, with the longest note, are part of
            // the excerpt's budget.
            let frame = "\n\nDiagnostics (first failure) (cut):\n\n\n".len() + 2 * fence.len();
            if excerpt.is_empty() || per_run <= frame {
                return format!(
                    "{section}\n\nDiagnostics: not available here; see the summary command above."
                );
            }
            let (excerpt, cut) = test_run_card_cut(excerpt, per_run - frame);
            // Cut here, or already cut before: the launcher caps diagnostics,
            // and the read caps them at 8 KiB.
            let note = if cut || failure.is_some_and(|failure| failure.truncated) {
                " (cut)"
            } else {
                ""
            };
            format!("{section}\n\nDiagnostics (first failure){note}:\n{fence}\n{excerpt}\n{fence}")
        })
        .collect();
    let prompt = format!("{head}{}", parts.join(separator));
    // Defence in depth; the budgets above already keep it within the cap.
    truncate_to_byte_limit_with_marker(
        prompt,
        TEST_RUN_WAIT_PROMPT_MAX_BYTES,
        DELEGATION_WAIT_RESUME_TRUNCATED_MARKER,
    )
}

/// Whether a wait's runs are settled for its mode, from their verdicts.
fn test_run_wait_satisfied(wait: &TestRunWaitRecord, verdicts: &[Option<TestRunWaitVerdict>]) -> bool {
    let settled = verdicts.iter().filter(|verdict| verdict.is_some()).count();
    match wait.mode {
        DelegationWaitMode::Any => settled > 0,
        DelegationWaitMode::All => settled == wait.runs.len(),
    }
}

fn test_run_wait_summaries<'a>(
    inner: &'a StateInner,
    wait: &TestRunWaitRecord,
) -> Vec<Option<&'a TestRunSummary>> {
    wait.run_ids
        .iter()
        .map(|run_id| inner.test_runs.find(run_id).map(|entry| &entry.summary))
        .collect()
}

/// Removes every pending run wait of a session whose turn the user stopped.
/// Runs and cards stay; nothing reactivates on its own.
fn consume_test_run_waits_for_stopped_session_locked(
    inner: &mut StateInner,
    session_id: &str,
) -> Vec<TestRunWaitRecord> {
    let (stopped, remaining) = std::mem::take(&mut inner.test_run_waits)
        .into_iter()
        .partition(|wait| wait.session_id == session_id);
    inner.test_run_waits = remaining;
    stopped
}

/// Where registration's validation failed.
enum TestRunWaitValidation {
    /// A run id is not in the index: worth one forced rescan.
    Unknown(String),
    Rejected(ApiError),
}

impl AppState {
    fn publish_test_run_waits_consumed(
        &self,
        revision: u64,
        waits: &[TestRunWaitRecord],
        reason: TestRunWaitConsumedReason,
    ) {
        for wait in waits {
            self.publish_delta(&DeltaEvent::TestRunWaitConsumed {
                revision,
                wait_id: wait.id.clone(),
                session_id: wait.session_id.clone(),
                reason,
            });
        }
    }

    /// Registers a run wait for `session_id`, the caller.
    fn create_test_run_wait(
        &self,
        session_id: &str,
        request: CreateTestRunWaitRequest,
    ) -> Result<TestRunWaitResponse, ApiError> {
        self.create_test_run_wait_with(session_id, request, &|| self.rescan_test_runs_bounded())
    }

    /// `create_test_run_wait` with the forced rescan injected, so tests fix
    /// liveness.
    fn create_test_run_wait_with(
        &self,
        session_id: &str,
        request: CreateTestRunWaitRequest,
        forced_rescan: &dyn Fn(),
    ) -> Result<TestRunWaitResponse, ApiError> {
        let session_id = normalize_optional_identifier(Some(session_id))
            .ok_or_else(|| ApiError::bad_request("session id is required"))?
            .to_owned();
        let run_ids = normalize_test_run_wait_ids(request.run_ids)?;
        let title = request.title.as_deref().and_then(non_empty_trimmed);
        if title
            .as_ref()
            .is_some_and(|value| value.chars().count() > MAX_DELEGATION_TITLE_CHARS)
        {
            return Err(ApiError::bad_request(format!(
                "test run wait title must be at most {MAX_DELEGATION_TITLE_CHARS} characters"
            )));
        }
        let mode = request.mode;
        let build = |inner: &StateInner| -> std::result::Result<TestRunWaitRecord, TestRunWaitValidation> {
            match delegation_wait_parent_eligibility_locked(inner, &session_id) {
                DelegationWaitParentEligibility::Eligible => {}
                DelegationWaitParentEligibility::Missing => {
                    return Err(TestRunWaitValidation::Rejected(ApiError::local_session_missing()));
                }
                DelegationWaitParentEligibility::Unavailable => {
                    return Err(TestRunWaitValidation::Rejected(ApiError::conflict(
                        "the waiting session is archived; unarchive it before waiting on runs",
                    )));
                }
            }
            let index = inner
                .find_visible_session_index(&session_id)
                .expect("an eligible session is visible");
            // Root sessions only, the mailbox rule: a delegation child that
            // ended its turn to wait would read as finished to its parent,
            // then start a new turn when resumed. A child runs its gates in
            // the foreground instead.
            if inner.sessions[index].session.parent_delegation_id.is_some()
                || inner
                    .find_delegation_index_by_child_session_id(&session_id)
                    .is_some()
            {
                return Err(TestRunWaitValidation::Rejected(ApiError::bad_request(
                    "only a root session can wait on test runs; a delegation child runs its gates in the foreground",
                )));
            }
            let Some(project_id) = inner.sessions[index].session.project_id.clone() else {
                return Err(TestRunWaitValidation::Rejected(ApiError::bad_request(
                    "the waiting session has no project; only a session in a project can wait on its runs",
                )));
            };
            let mut runs = Vec::with_capacity(run_ids.len());
            for run_id in &run_ids {
                let Some(entry) = inner.test_runs.find(run_id) else {
                    return Err(TestRunWaitValidation::Unknown(run_id.clone()));
                };
                let summary = &entry.summary;
                if summary.project_id.as_deref() != Some(project_id.as_str()) {
                    return Err(TestRunWaitValidation::Rejected(ApiError::bad_request(format!(
                        "test run `{run_id}` is not in the waiting session's project"
                    ))));
                }
                runs.push(TestRunWaitRunLabel {
                    run_id: run_id.clone(),
                    run_dir: summary.run_dir.clone(),
                    preset: summary.preset,
                    worktree: summary.worktree.clone(),
                    owner_session_id: summary.owner_session_id.clone(),
                    started_at: summary.started_at.clone(),
                });
            }
            let wait = TestRunWaitRecord {
                id: format!("test-run-wait-{}", Uuid::new_v4()),
                session_id: session_id.clone(),
                run_ids: run_ids.clone(),
                mode,
                created_at: stamp_now(),
                title: title.clone(),
                runs,
            };
            // Headers and commands are never cut, so they must leave room for
            // excerpts: measured with the longest verdict label.
            let worst = vec![Some(TestRunWaitVerdict::Unknown(TestRunCardUnknownReason::ResultsUnreadable)); wait.runs.len()];
            let mandatory = test_run_wait_prompt_head(&wait, &worst).len()
                + wait
                    .runs
                    .iter()
                    .map(|label| {
                        test_run_wait_run_section(
                            label,
                            inner.test_runs.find(&label.run_id).map(|entry| &entry.summary),
                            Some(TestRunWaitVerdict::Unknown(TestRunCardUnknownReason::ResultsUnreadable)),
                            None,
                        )
                        .len()
                            + 8
                    })
                    .sum::<usize>();
            if mandatory > TEST_RUN_WAIT_MANDATORY_MAX_BYTES {
                return Err(TestRunWaitValidation::Rejected(ApiError::bad_request(format!(
                    "the wait's run headers and commands need {mandatory} bytes, over the {TEST_RUN_WAIT_MANDATORY_MAX_BYTES}-byte limit; wait on fewer runs"
                ))));
            }
            Ok(wait)
        };
        let validated = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            build(&inner)
        };
        // An id not indexed yet (a run launched just now) gets one forced,
        // bounded rescan before the call is rejected, so the agent is never
        // pushed into polling.
        if let Err(TestRunWaitValidation::Unknown(_)) = validated {
            forced_rescan();
        }
        let (revision, wait) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let wait = match build(&inner) {
                Ok(wait) => wait,
                Err(TestRunWaitValidation::Unknown(run_id)) => {
                    return Err(ApiError::not_found(format!(
                        "test run `{run_id}` is not indexed"
                    )));
                }
                Err(TestRunWaitValidation::Rejected(error)) => return Err(error),
            };
            inner.test_run_waits.push(wait.clone());
            let revision = self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist the test run wait: {err:#}"))
            })?;
            self.publish_delta(&DeltaEvent::TestRunWaitCreated {
                revision,
                wait: wait.clone(),
            });
            (revision, wait)
        };
        // Runs that already settled resume at once, which is also how a
        // session re-fetches a verdict.
        let refresh = self.refresh_test_run_waits();
        let queued = refresh.queue_results.get(&wait.id).copied().unwrap_or_default();
        Ok(TestRunWaitResponse {
            revision: refresh.revision.unwrap_or(revision),
            wait_id: wait.id.clone(),
            run_ids: wait.run_ids.clone(),
            mode: wait.mode,
            wait,
            resume_prompt_queued: queued.prompt_queued,
            resume_dispatch_requested: queued.dispatch_requested,
            server_instance_id: self.server_instance_id.clone(),
        })
    }

    /// One rescan, forced by a registration naming an id not indexed yet,
    /// waited on for at most a second. Rescans are serialized, so it never
    /// commits over a newer scan; if it takes longer it finishes in the
    /// background and the registration proceeds with the index as it is.
    #[cfg(not(test))]
    fn rescan_test_runs_bounded(&self) {
        let state = self.clone();
        let (done_tx, done_rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("termal-test-runs-forced".to_owned())
            .spawn(move || {
                state.refresh_test_runs();
                let _ = done_tx.send(());
            });
        if spawned.is_ok() {
            let _ = done_rx.recv_timeout(TEST_RUN_WAIT_FORCED_RESCAN_BOUND);
        }
    }

    #[cfg(test)]
    fn rescan_test_runs_bounded(&self) {}

    /// Resumes and consumes the waits whose runs settled, and consumes those
    /// whose session was removed or became unavailable. Called after every
    /// rescan and after a registration, never before this process's first
    /// full scan (an empty index would read every run as not indexed).
    /// Disk is read without the state lock: failure excerpts, and whether a
    /// run no longer indexed is gone.
    fn refresh_test_run_waits(&self) -> TestRunWaitRefresh {
        self.refresh_test_run_waits_pausing(&|| {})
    }

    /// `refresh_test_run_waits`, calling `after_pick` once the reads are
    /// picked and before they are made: the window in which a scan can
    /// change the index, which a test uses to change it deterministically.
    fn refresh_test_run_waits_pausing(&self, after_pick: &dyn Fn()) -> TestRunWaitRefresh {
        // Which runs left the index, and which excerpts the waits that may
        // be done need. A wait may be done if it is satisfied with every run
        // not indexed taken as gone: an upper bound, so no needed read is
        // missed.
        let (checks, reads) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if !inner.test_runs.scanned_once || inner.test_run_waits.is_empty() {
                return TestRunWaitRefresh::default();
            }
            let mut checks: BTreeMap<String, PathBuf> = BTreeMap::new();
            let mut reads: BTreeMap<String, PathBuf> = BTreeMap::new();
            for wait in &inner.test_run_waits {
                let summaries = test_run_wait_summaries(&inner, wait);
                for (label, summary) in wait.runs.iter().zip(&summaries) {
                    if summary.is_none() {
                        checks.insert(label.run_id.clone(), test_run_fs_path(&label.run_dir));
                    }
                }
                let verdicts = test_run_wait_verdicts(wait, &summaries, |_| true);
                if !test_run_wait_satisfied(wait, &verdicts) {
                    continue;
                }
                for summary in summaries.into_iter().flatten() {
                    if summary.state == TestRunState::Failed {
                        if let Some(entry) = inner.test_runs.find(&summary.run_id) {
                            reads.insert(summary.run_id.clone(), entry.disk.run_dir.clone());
                        }
                    }
                }
            }
            (checks, reads)
        };
        after_pick();
        let gone: HashSet<String> = checks
            .into_iter()
            .filter(|(_, run_dir)| test_run_wait_run_gone(run_dir))
            .map(|(run_id, _)| run_id)
            .collect();
        let failures: HashMap<String, TestRunCardFailureRead> = reads
            .into_iter()
            .map(|(run_id, run_dir)| {
                let read = test_run_failure_read(&run_dir, TEST_RUN_WAIT_EXCERPT_MAX_BYTES);
                (run_id, read)
            })
            .collect();
        let mut refresh = TestRunWaitRefresh::default();
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let waits = std::mem::take(&mut inner.test_run_waits);
            let mut remaining = Vec::with_capacity(waits.len());
            let mut consumed: Vec<(TestRunWaitRecord, TestRunWaitConsumedReason)> = Vec::new();
            for wait in waits {
                match delegation_wait_parent_eligibility_locked(&inner, &wait.session_id) {
                    DelegationWaitParentEligibility::Eligible => {}
                    DelegationWaitParentEligibility::Missing => {
                        consumed.push((wait, TestRunWaitConsumedReason::SessionRemoved));
                        continue;
                    }
                    DelegationWaitParentEligibility::Unavailable => {
                        consumed.push((wait, TestRunWaitConsumedReason::SessionUnavailable));
                        continue;
                    }
                }
                let summaries = test_run_wait_summaries(&inner, &wait);
                // A run that left the index since the check above was not
                // checked: not gone yet, so the next refresh checks it. Once
                // the waiting session's project is removed (its sessions
                // stay, without a project), its runs are never indexed again,
                // whatever is on disk.
                let project_removed = inner
                    .find_visible_session_index(&wait.session_id)
                    .and_then(|index| inner.sessions[index].session.project_id.as_deref())
                    .is_none_or(|project_id| inner.find_project(project_id).is_none());
                let verdicts = test_run_wait_verdicts(&wait, &summaries, |run_id| {
                    project_removed || gone.contains(run_id)
                });
                if !test_run_wait_satisfied(&wait, &verdicts) {
                    remaining.push(wait);
                    continue;
                }
                // A run that failed after the reads were picked has no read
                // yet: the next refresh reads it, rather than resuming
                // without diagnostics that are there. A read that was made is
                // an answer, even one that found nothing, and even one that
                // could not read results.json whole (Unavailable: a
                // replacement race, or a file over the 1 MiB read limit).
                // Waiting on those could defer the resume for good, so the
                // prompt says the diagnostics are not available here instead.
                let unread_failure = wait
                    .run_ids
                    .iter()
                    .zip(&verdicts)
                    .any(|(run_id, verdict)| {
                        *verdict == Some(TestRunWaitVerdict::Fail) && !failures.contains_key(run_id)
                    });
                if unread_failure {
                    remaining.push(wait);
                    continue;
                }
                let prompt = build_test_run_wait_prompt(&wait, &summaries, &verdicts, &failures);
                let queued = queue_delegation_wait_resume_locked(&mut inner, &wait.session_id, prompt);
                if queued.dispatch_requested {
                    refresh.dispatch_sessions.push(wait.session_id.clone());
                }
                refresh.queue_results.insert(wait.id.clone(), queued);
                consumed.push((wait, TestRunWaitConsumedReason::Completed));
            }
            inner.test_run_waits = remaining;
            if consumed.is_empty() {
                return refresh;
            }
            match self.commit_locked(&mut inner) {
                Ok(revision) => {
                    for (wait, reason) in &consumed {
                        self.publish_test_run_waits_consumed(revision, std::slice::from_ref(wait), *reason);
                    }
                    refresh.revision = Some(revision);
                }
                Err(err) => {
                    // The queued prompts and removals stay in memory and reach
                    // disk with the next commit; clients resync on the gap.
                    eprintln!("test run wait> failed to record consumed waits: {err:#}");
                    refresh.revision = Some(inner.revision);
                }
            }
        }
        if let Some(revision) = refresh.revision {
            self.dispatch_test_run_wait_resumes(revision, refresh.dispatch_sessions.clone());
        }
        refresh
    }

    /// Starts a queued resume now for a session that is idle and not
    /// latched, as for delegation waits.
    fn dispatch_test_run_wait_resumes(&self, revision: u64, session_ids: Vec<String>) {
        let mut seen = BTreeSet::new();
        for session_id in session_ids {
            if !seen.insert(session_id.clone()) {
                continue;
            }
            let error = match self.dispatch_next_queued_turn(&session_id, false) {
                Ok(Some(dispatch)) => deliver_turn_dispatch(self, dispatch)
                    .into_background_result("test run wait resume")
                    .err()
                    .map(|err| {
                        format!(
                            "failed to dispatch the queued resume for session `{session_id}`: {}",
                            err.message
                        )
                    }),
                Ok(None) => None,
                Err(err) => Some(format!(
                    "failed to inspect the queued resume for session `{session_id}`: {err:#}"
                )),
            };
            if let Some(error) = error {
                eprintln!("test run wait warning> {error}");
                self.publish_delta(&DeltaEvent::TestRunWaitResumeDispatchFailed {
                    revision,
                    session_id: session_id.clone(),
                    error,
                });
            }
        }
    }
}

/// What a refresh did: the revision of its commit (when it consumed a
/// wait), the queue result per resumed wait, and the sessions to dispatch.
#[derive(Default)]
struct TestRunWaitRefresh {
    revision: Option<u64>,
    queue_results: BTreeMap<String, DelegationWaitQueueResult>,
    dispatch_sessions: Vec<String>,
}

fn normalize_test_run_wait_ids(ids: Vec<String>) -> Result<Vec<String>, ApiError> {
    let mut normalized = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(id) = non_empty_trimmed(&id) else {
            return Err(ApiError::bad_request("test run ids cannot be empty"));
        };
        // Distinct ids, as for delegation waits: a repeat is dropped.
        if !normalized.contains(&id) {
            normalized.push(id);
        }
    }
    if normalized.is_empty() || normalized.len() > MAX_TEST_RUN_WAIT_RUNS {
        return Err(ApiError::bad_request(format!(
            "a test run wait names 1 to {MAX_TEST_RUN_WAIT_RUNS} runs"
        )));
    }
    Ok(normalized)
}
