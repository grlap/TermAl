// Reading the test launcher's run directories (docs/features/test-runs.md,
// slice 1). Owns Git common-directory discovery from the filesystem, run
// directory listing, bounded parsing of request.json and results.json, the
// host verdict for non-terminal runs (process liveness), run detail and the
// stage-log tail with canonical path containment. Never writes to a run
// directory and never decides passed or failed itself: those come from the
// launcher's terminal results.json. Does not own the index, the state
// snapshot or deltas (`test_runs.rs`), or HTTP (`test_runs_api.rs`).

/// The most `request.json` or `results.json` bytes read for one run.
const TEST_RUN_JSON_MAX_BYTES: usize = 1024 * 1024;
/// A focused command in a summary keeps at most this many arguments...
const TEST_RUN_COMMAND_MAX_ITEMS: usize = 32;
/// ...and at most this many bytes in total.
const TEST_RUN_COMMAND_MAX_BYTES: usize = 512;
/// A summary's error text is cut to this many bytes.
const TEST_RUN_ERROR_MAX_BYTES: usize = 512;
/// The index keeps at most this many stages of a run.
const TEST_RUN_STAGES_MAX: usize = 64;
/// The longest identifier, session reference or timestamp the index accepts.
/// Identifiers are never cut, since a cut one could alias another: a run
/// whose run id or session reference is longer is not indexed, a stage with a
/// longer or invalid name is left out, and a longer timestamp reads as null.
const TEST_RUN_TEXT_MAX_BYTES: usize = 128;
/// The longest worktree path the index accepts; a run with a longer one is
/// not indexed.
const TEST_RUN_PATH_MAX_BYTES: usize = 4096;

/// `text` when it is within `max` bytes; never a cut copy.
fn test_run_within(text: String, max: usize) -> Option<String> {
    (text.len() <= max).then_some(text)
}

/// A launcher stage name: what the log route accepts.
fn test_run_valid_stage_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= TEST_RUN_TEXT_MAX_BYTES
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}
/// The default and largest stage-log tail served.
const TEST_RUN_LOG_TAIL_DEFAULT_BYTES: u64 = 64 * 1024;
const TEST_RUN_LOG_TAIL_MAX_BYTES: u64 = 1024 * 1024;

/// The size and modification time of a run file, which decide whether a
/// cached parse is still current.
type TestRunFileStamp = Option<(u64, std::time::SystemTime)>;

fn test_run_file_stamp(path: &FsPath) -> TestRunFileStamp {
    let metadata = fs::metadata(path).ok()?;
    Some((metadata.len(), metadata.modified().ok()?))
}

/// The Git common directory of the repository holding `root`, found from the
/// filesystem alone, so a periodic rescan never starts a process: the nearest
/// `.git` directory, or a `.git` file's `gitdir:` target and that directory's
/// `commondir`.
fn test_runs_git_common_dir(root: &FsPath) -> Option<PathBuf> {
    let mut current = Some(root);
    while let Some(directory) = current {
        let dot_git = directory.join(".git");
        match fs::metadata(&dot_git) {
            Ok(metadata) if metadata.is_dir() => return Some(dot_git),
            Ok(metadata) if metadata.is_file() => {
                let text = fs::read_to_string(&dot_git).ok()?;
                let gitdir = directory.join(text.trim().strip_prefix("gitdir:")?.trim());
                return Some(match fs::read_to_string(gitdir.join("commondir")) {
                    Ok(commondir) => gitdir.join(commondir.trim()),
                    Err(_) => gitdir,
                });
            }
            _ => current = directory.parent(),
        }
    }
    None
}

/// Every run directory under a common directory: its own `review-runs` and
/// each linked worktree's, sorted so a duplicate run id resolves the same way
/// on every scan.
fn test_runs_directories(common_dir: &FsPath) -> Vec<PathBuf> {
    let mut review_dirs = vec![common_dir.join("review-runs")];
    if let Ok(entries) = fs::read_dir(common_dir.join("worktrees")) {
        review_dirs.extend(
            entries
                .flatten()
                .map(|entry| entry.path().join("review-runs")),
        );
    }
    let mut runs = Vec::new();
    for review_dir in review_dirs {
        let Ok(entries) = fs::read_dir(&review_dir) else {
            continue;
        };
        runs.extend(entries.flatten().filter_map(|entry| {
            let is_run = entry.file_type().is_ok_and(|kind| kind.is_dir())
                && entry.file_name().to_string_lossy().starts_with("test-");
            is_run.then(|| entry.path())
        }));
    }
    runs.sort();
    runs
}

/// `path` as a comparison key: forward slashes, no trailing slash, no Windows
/// verbatim prefix, and lower case on Windows, whose paths ignore case.
fn test_run_path_key(path: &str) -> String {
    let path = path
        .strip_prefix(r"\\?\")
        .unwrap_or(path)
        .replace('\\', "/");
    let path = path.trim_end_matches('/').to_owned();
    if cfg!(windows) {
        path.to_lowercase()
    } else {
        path
    }
}

/// A path as the wire shows it: forward slashes, no verbatim prefix.
fn test_run_display_path(path: &FsPath) -> String {
    let text = path.to_string_lossy();
    text.strip_prefix(r"\\?\")
        .unwrap_or(&text)
        .replace('\\', "/")
}

fn test_run_truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// One read of a run's JSON file.
enum TestRunJson {
    /// The parsed file, the digest of the bytes it was parsed from, and the
    /// stamp of the file version those bytes came from. A stamp taken before
    /// or after the read can belong to another version when the launcher
    /// replaces the file in between; this one cannot.
    Parsed(Value, String, TestRunFileStamp),
    /// The file does not exist.
    Missing,
    /// The file could not be read whole or parsed: the launcher replaced it
    /// during the read, or its content is malformed.
    Failed,
    /// The file is larger than TermAl reads. Unlike a failed read this is
    /// not a race, so retrying cannot help until the file changes.
    TooLarge,
}

fn test_run_read_json(path: &FsPath) -> TestRunJson {
    if fs::metadata(path).is_ok_and(|metadata| metadata.len() > TEST_RUN_JSON_MAX_BYTES as u64) {
        return TestRunJson::TooLarge;
    }
    match review_freeze_file_versioned(path, TEST_RUN_JSON_MAX_BYTES) {
        Ok((bytes, version)) => match serde_json::from_slice(&bytes) {
            Ok(value) => {
                let stamp = version
                    .modified()
                    .ok()
                    .map(|modified| (version.len(), modified));
                TestRunJson::Parsed(value, file_content_hash(&bytes), stamp)
            }
            Err(_) => TestRunJson::Failed,
        },
        Err(err)
            if err
                .downcast_ref::<io::Error>()
                .is_some_and(|err| err.kind() == io::ErrorKind::NotFound) =>
        {
            TestRunJson::Missing
        }
        Err(_) => TestRunJson::Failed,
    }
}

/// JavaScript truthiness, as the launcher's `Boolean(value)` judges it.
fn test_run_js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// The launcher's `Number.isInteger(value)`.
fn test_run_js_integer(value: &Value) -> bool {
    value
        .as_f64()
        .is_some_and(|value| value.is_finite() && value.fract() == 0.0)
}

/// `Passed` or `Failed` for terminal results, `None` otherwise.
fn test_run_terminal_state(results: &Value) -> Option<TestRunState> {
    test_run_results_are_terminal(results).then(|| {
        match results.get("state").and_then(Value::as_str) {
            Some("passed") => TestRunState::Passed,
            _ => TestRunState::Failed,
        }
    })
}

/// The launcher's `isTerminal(result)`.
fn test_run_results_are_terminal(results: &Value) -> bool {
    matches!(
        results.get("state").and_then(Value::as_str),
        Some("passed" | "failed")
    ) && results.get("ended").is_some_and(test_run_js_truthy)
        && results.get("exitCode").is_some_and(test_run_js_integer)
}

fn test_run_str(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn test_run_argv(value: Option<&Value>) -> Option<Vec<String>> {
    value?
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect()
}

/// A positive pid, or nothing: a missing or non-positive pid names no process.
fn test_run_pid(value: &Value, key: &str) -> Option<u32> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
}

/// What the index keeps of a run's `results.json`: the summary fields, each
/// bounded, and never the parsed file. The detail and log routes read the
/// file afresh. What is kept per run is bounded, listed or not; the total
/// grows with the number of run directories.
#[derive(Clone, Debug)]
struct TestRunResultsExtract {
    /// The digest of the bytes this extract was read from (`detailVersion`).
    digest: String,
    /// `Passed` or `Failed` when the results are terminal.
    terminal_state: Option<TestRunState>,
    /// The first `TEST_RUN_STAGES_MAX` stages with valid names.
    stages: Vec<TestRunStageSummary>,
    /// The running stage, found among all stages, not only the kept ones.
    current_stage: Option<String>,
    interrupted: bool,
    ended: Option<String>,
    exit_code: Option<i64>,
    error: Option<String>,
    pid: Option<u32>,
    started: Option<String>,
}

impl TestRunResultsExtract {
    fn new(results: &Value, digest: String) -> Self {
        let text = |key: &str| {
            test_run_str(results, key)
                .and_then(|text| test_run_within(text, TEST_RUN_TEXT_MAX_BYTES))
        };
        let valid_stages = || {
            results
                .get("stages")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(test_run_stage_summary)
                .filter(|stage| test_run_valid_stage_name(&stage.name))
        };
        Self {
            digest,
            terminal_state: test_run_terminal_state(results),
            current_stage: valid_stages()
                .find(|stage| stage.state == TestRunStageState::Running)
                .map(|stage| stage.name),
            stages: valid_stages()
                .take(TEST_RUN_STAGES_MAX)
                .map(|stage| TestRunStageSummary {
                    started_at: stage
                        .started_at
                        .and_then(|text| test_run_within(text, TEST_RUN_TEXT_MAX_BYTES)),
                    ended_at: stage
                        .ended_at
                        .and_then(|text| test_run_within(text, TEST_RUN_TEXT_MAX_BYTES)),
                    ..stage
                })
                .collect(),
            interrupted: results
                .get("interrupted")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            ended: text("ended"),
            exit_code: results.get("exitCode").and_then(Value::as_i64),
            error: test_run_str(results, "error")
                .map(|error| test_run_truncate(&error, TEST_RUN_ERROR_MAX_BYTES)),
            pid: test_run_pid(results, "pid"),
            started: text("started"),
        }
    }
}

/// What one run directory says, parsed once per change of its files.
#[derive(Clone, Debug)]
struct TestRunDisk {
    run_id: String,
    run_dir: PathBuf,
    request_stamp: TestRunFileStamp,
    results_stamp: TestRunFileStamp,
    worktree: String,
    preset: TestRunPreset,
    command: Option<Vec<String>>,
    command_truncated: bool,
    detached: Option<bool>,
    creator_pid: Option<u32>,
    owner: Option<String>,
    notify_to: Option<String>,
    /// The extract of `results.json`, when readable.
    results: Option<TestRunResultsExtract>,
    started_at: Option<String>,
    /// Consecutive rescans whose read of a file failed. Nonzero means the
    /// next rescan reads the files again whatever their stamps, so a failed
    /// read is never cached as if it were an answer.
    read_failures: u8,
    /// Set when these results were read again after the liveness check found
    /// their responsible process gone (`Some(pid)`), or found no pid at all
    /// (`None`). Such a read is final for that process, so an unchanged run
    /// is not read again on every rescan.
    confirmed_unknown: Option<Option<u32>>,
}

impl TestRunDisk {
    /// Reads `run_dir`; `previous` is the last read of the same directory.
    ///
    /// The launcher replaces its files by atomic rename, and a read can still
    /// lose a race with a replacement (on Windows, opening a file mid-rename
    /// can fail). One failed read cannot tell such a race from a file that is
    /// malformed or gone for good, so the policy is a bounded grace on stale
    /// evidence: after a good read, one failed rescan keeps that read, sends
    /// nothing, and reads again; nothing flickers, and a run is not removed
    /// and re-added over one race. If the next read fails as well, the stale
    /// evidence is given up: results that cannot be read leave the run
    /// `unknown` with no stages, and a request that cannot be read drops the
    /// run. An earlier pass is never shown past one rescan of unreadable
    /// results, and every failed read is tried again on the next rescan.
    /// `None` when the run has no readable request to report.
    fn read(run_dir: &FsPath, previous: Option<&Self>) -> Option<Self> {
        Self::read_pausing(run_dir, previous, &|| {})
    }

    /// `read`, calling `after_stamp` between the stamp taken before the reads
    /// and the reads themselves: the window in which the launcher can replace
    /// a file, which a test uses to replace one deterministically.
    fn read_pausing(run_dir: &FsPath, previous: Option<&Self>, after_stamp: &dyn Fn()) -> Option<Self> {
        let request_path = run_dir.join("request.json");
        let results_path = run_dir.join("results.json");
        // Stamped before the read: it is kept only when there are no parsed
        // results, which carry the stamp of their own version.
        let results_stamp = test_run_file_stamp(&results_path);
        after_stamp();
        // Only evidence from a good read earns the grace.
        let grace = previous.filter(|previous| previous.read_failures == 0);
        let failures = previous.map_or(1, |previous| previous.read_failures.saturating_add(1));
        let (request, request_stamp) = match test_run_read_json(&request_path) {
            TestRunJson::Parsed(request, _, stamp) => (request, stamp),
            // The run directory is still listed (a removed directory leaves
            // the index at once), so a missing request is a failed read like
            // any other.
            TestRunJson::Missing | TestRunJson::Failed => {
                return grace.map(|previous| Self {
                    read_failures: 1,
                    ..previous.clone()
                });
            }
            // Deterministic, not a race: no grace, and nothing to report.
            TestRunJson::TooLarge => return None,
        };
        let run_id = test_run_str(&request, "runId").unwrap_or_else(|| {
            run_dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });
        // Identifiers the index cannot hold whole are unsupported, never cut.
        let run_id = test_run_within(run_id, TEST_RUN_TEXT_MAX_BYTES)?;
        let preset = if request.get("full").and_then(Value::as_bool) == Some(true) {
            TestRunPreset::Full
        } else if request
            .get("liveEngram")
            .is_some_and(|live| !live.is_null())
        {
            TestRunPreset::Live
        } else {
            TestRunPreset::Focused
        };
        let (command, command_truncated) = if preset == TestRunPreset::Focused {
            let stage = request
                .get("stages")
                .and_then(Value::as_array)
                .and_then(|stages| stages.first());
            let argv = stage.and_then(|stage| {
                let mut argv = vec![test_run_str(stage, "command")?];
                argv.extend(test_run_argv(stage.get("args")).unwrap_or_default());
                Some(argv)
            });
            match argv {
                Some(argv) => {
                    let mut kept = Vec::new();
                    let mut bytes = 0;
                    for item in &argv {
                        if kept.len() == TEST_RUN_COMMAND_MAX_ITEMS
                            || bytes + item.len() > TEST_RUN_COMMAND_MAX_BYTES
                        {
                            break;
                        }
                        bytes += item.len();
                        kept.push(item.clone());
                    }
                    let truncated = kept.len() < argv.len();
                    (Some(kept), truncated)
                }
                None => (None, false),
            }
        } else {
            (None, false)
        };
        let mut read_failures = 0;
        let (results, results_stamp) = match test_run_read_json(&results_path) {
            // The pid in these results is judged against when this version
            // was written, never against a stamp of a replaced version.
            TestRunJson::Parsed(results, digest, stamp) => {
                (Some(TestRunResultsExtract::new(&results, digest)), stamp)
            }
            // A run is created with both files, but a reader may still find
            // no results. Missing results are an answer only while no good
            // read ever found them; results that vanished are a failed read.
            TestRunJson::Missing
                if previous.is_none_or(|previous| {
                    previous.results.is_none() && previous.read_failures == 0
                }) =>
            {
                (None, results_stamp)
            }
            // Deterministic, not a race: unknown at once, with no grace, and
            // not read again until the file changes.
            TestRunJson::TooLarge => (None, results_stamp),
            TestRunJson::Missing | TestRunJson::Failed => match grace {
                // The grace: the last good results stand for one rescan.
                Some(grace) => {
                    read_failures = 1;
                    (grace.results.clone(), grace.results_stamp)
                }
                // No evidence to keep: unknown, read again next rescan.
                None => {
                    read_failures = failures;
                    (None, results_stamp)
                }
            },
        };
        // A session reference or worktree the index cannot hold whole makes
        // the run unsupported; a cut one could name another session or tree.
        let reference = |key: &str| -> Result<Option<String>, ()> {
            match test_run_str(&request, key).filter(|text| !text.trim().is_empty()) {
                None => Ok(None),
                Some(text) => test_run_within(text, TEST_RUN_TEXT_MAX_BYTES)
                    .map(Some)
                    .ok_or(()),
            }
        };
        let owner = reference("owner").ok()?;
        let notify_to = reference("notifyTo").ok()?;
        let worktree = match test_run_str(&request, "root") {
            None => String::new(),
            Some(root) => test_run_within(
                test_run_display_path(FsPath::new(&root)),
                TEST_RUN_PATH_MAX_BYTES,
            )?,
        };
        // A timestamp is not an identifier: an over-long one reads as null.
        let started_at = test_run_str(&request, "started")
            .and_then(|text| test_run_within(text, TEST_RUN_TEXT_MAX_BYTES))
            .or_else(|| results.as_ref().and_then(|results| results.started.clone()));
        Some(Self {
            run_id,
            run_dir: run_dir.to_path_buf(),
            request_stamp,
            results_stamp,
            worktree,
            preset,
            command,
            command_truncated,
            detached: request.get("detached").and_then(Value::as_bool),
            creator_pid: test_run_pid(&request, "creatorPid"),
            owner,
            notify_to,
            results,
            started_at,
            read_failures,
            confirmed_unknown: None,
        })
    }

    /// Whether an `unknown` verdict may rest on results read before the
    /// liveness check. A launcher writes its terminal results just before it
    /// exits, so results read before the check that found its process gone
    /// (or found no pid) can predate that write. Such a verdict needs one more
    /// read after the check, unless this read already was one for the same
    /// process.
    fn unknown_needs_read_after_check(&self, state: TestRunState) -> bool {
        state == TestRunState::Unknown
            && self.results.is_some()
            && self.confirmed_unknown != Some(self.responsible_pid().map(|(pid, _)| pid))
    }

    /// Whether `results.json` is terminal, exactly as the launcher's
    /// `isTerminal` judges it: a passed or failed state, a truthy `ended`
    /// and an integer `exitCode`.
    fn is_terminal(&self) -> bool {
        self.results
            .as_ref()
            .is_some_and(|results| results.terminal_state.is_some())
    }

    /// The process responsible for a non-terminal run, with when the file
    /// that recorded its pid was last written: `results.json`'s pid, else the
    /// process that created the run (`request.json`'s `creatorPid`).
    fn responsible_pid(&self) -> Option<(u32, Option<std::time::SystemTime>)> {
        let written = |stamp: TestRunFileStamp| stamp.map(|(_, modified)| modified);
        match self.results.as_ref().and_then(|results| results.pid) {
            Some(pid) => Some((pid, written(self.results_stamp))),
            None => self
                .creator_pid
                .map(|pid| (pid, written(self.request_stamp))),
        }
    }

    /// The run's state. Passed and failed come only from a terminal
    /// `results.json`; a non-terminal run is running while its responsible
    /// process may still be the one that recorded its pid, and unknown
    /// otherwise, including when no pid was recorded anywhere.
    fn state(&self, writer_may_be_alive: &TestRunLiveness) -> TestRunState {
        if let Some(terminal) = self
            .results
            .as_ref()
            .and_then(|results| results.terminal_state)
        {
            return terminal;
        }
        match (self.results.as_ref(), self.responsible_pid()) {
            (Some(_), Some((pid, written))) if writer_may_be_alive(pid, written) => {
                TestRunState::Running
            }
            _ => TestRunState::Unknown,
        }
    }

    fn stages(&self) -> Vec<TestRunStageSummary> {
        self.results
            .as_ref()
            .map(|results| results.stages.clone())
            .unwrap_or_default()
    }
}

fn test_run_stage_state(value: Option<&str>) -> TestRunStageState {
    match value {
        Some("running") => TestRunStageState::Running,
        Some("passed") => TestRunStageState::Passed,
        Some("failed") => TestRunStageState::Failed,
        _ => TestRunStageState::Unrun,
    }
}

fn test_run_stage_summary(stage: &Value) -> Option<TestRunStageSummary> {
    Some(TestRunStageSummary {
        name: test_run_str(stage, "name")?,
        state: test_run_stage_state(stage.get("state").and_then(Value::as_str)),
        exit_code: stage.get("code").and_then(Value::as_i64),
        started_at: test_run_str(stage, "started"),
        ended_at: test_run_str(stage, "ended"),
    })
}

fn test_run_diagnostics(value: Option<&Value>) -> Option<TestRunDiagnostics> {
    let value = value?;
    Some(TestRunDiagnostics {
        text: test_run_str(value, "text")?,
        truncated: value
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// A log path the launcher recorded, relative to the run directory when it
/// lies inside it. The launcher spells the directory as Git reported it, which
/// can differ from the index's spelling in case or prefix, so a lexical
/// mismatch is retried on canonical paths.
fn test_run_relative_log(run_dir: &FsPath, log: Option<String>) -> Option<String> {
    let log = log?;
    let path = FsPath::new(&log);
    let relative = path
        .strip_prefix(run_dir)
        .map(FsPath::to_path_buf)
        .ok()
        .or_else(|| {
            let run_dir = fs::canonicalize(run_dir).ok()?;
            fs::canonicalize(path)
                .ok()?
                .strip_prefix(&run_dir)
                .map(FsPath::to_path_buf)
                .ok()
        });
    Some(match relative {
        Some(relative) => relative.to_string_lossy().replace('\\', "/"),
        None => test_run_display_path(path),
    })
}

/// Refusal for a results file that exists but could not be read whole: an
/// empty detail would read as "no stages", so the caller is told to retry.
fn test_run_results_unreadable() -> ApiError {
    ApiError::conflict("the run's results could not be read, possibly mid-replacement; retry")
}

/// Refusal for a results file over the read limit: not retryable until the
/// file changes.
fn test_run_results_too_large() -> ApiError {
    ApiError::from_status(
        StatusCode::UNPROCESSABLE_ENTITY,
        format!(
            "the run's results exceed the {} MiB read limit",
            TEST_RUN_JSON_MAX_BYTES / (1024 * 1024)
        ),
    )
}

/// The run detail: every stage with command, working directory, log and
/// diagnostics, the preflight probes, and the fingerprints. Empty when the
/// run has no results yet; 409 when they exist but could not be read.
fn test_run_detail(run_dir: &FsPath) -> Result<TestRunDetail, ApiError> {
    let results = match test_run_read_json(&run_dir.join("results.json")) {
        TestRunJson::Parsed(results, _, _) => results,
        TestRunJson::Missing => return Ok(TestRunDetail::default()),
        TestRunJson::Failed => return Err(test_run_results_unreadable()),
        TestRunJson::TooLarge => return Err(test_run_results_too_large()),
    };
    let stages = results
        .get("stages")
        .and_then(Value::as_array)
        .map(|stages| {
            stages
                .iter()
                .filter_map(|stage| {
                    Some(TestRunStageDetail {
                        summary: test_run_stage_summary(stage)?,
                        command: test_run_argv(stage.get("command")),
                        cwd: test_run_str(stage, "cwd")
                            .map(|cwd| test_run_display_path(FsPath::new(&cwd))),
                        log: test_run_relative_log(run_dir, test_run_str(stage, "log")),
                        diagnostics: test_run_diagnostics(stage.get("diagnostics")),
                        error: test_run_str(stage, "error"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let preflight = results
        .get("preflight")
        .and_then(Value::as_array)
        .map(|probes| {
            probes
                .iter()
                .filter_map(|probe| {
                    Some(TestRunPreflight {
                        name: test_run_str(probe, "name")?,
                        command: test_run_argv(probe.get("command")),
                        exit_code: probe.get("code").and_then(Value::as_i64),
                        log: test_run_relative_log(run_dir, test_run_str(probe, "log")),
                        diagnostics: test_run_diagnostics(probe.get("diagnostics")),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(TestRunDetail {
        stages,
        preflight,
        expected_fingerprint: test_run_str(&results, "expectedFingerprint"),
        before: test_run_str(&results, "before"),
        after: test_run_str(&results, "after"),
        limitations: test_run_str(&results, "limitations"),
    })
}

/// The last `tail` bytes of stage `name`'s log, read only when the log path
/// `results.json` records resolves, symlinks and junctions included, inside
/// the run directory.
fn test_run_stage_log_tail(
    run_dir: &FsPath,
    name: &str,
    tail: Option<u64>,
) -> Result<TestRunLogTail, ApiError> {
    use std::io::{Read, Seek, SeekFrom};
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(ApiError::bad_request("invalid stage name"));
    }
    let results = match test_run_read_json(&run_dir.join("results.json")) {
        TestRunJson::Parsed(results, _, _) => results,
        TestRunJson::Missing => return Err(ApiError::not_found("run results are unavailable")),
        TestRunJson::Failed => return Err(test_run_results_unreadable()),
        TestRunJson::TooLarge => return Err(test_run_results_too_large()),
    };
    let log = results
        .get("stages")
        .and_then(Value::as_array)
        .and_then(|stages| {
            stages
                .iter()
                .find(|stage| stage.get("name").and_then(Value::as_str) == Some(name))
        })
        .ok_or_else(|| ApiError::not_found("unknown stage"))?
        .get("log")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::not_found("the stage has no log"))?
        .to_owned();
    let canonical_run_dir = fs::canonicalize(run_dir)
        .map_err(|_| ApiError::not_found("the run directory is unavailable"))?;
    let canonical_log = fs::canonicalize(run_dir.join(&log))
        .map_err(|_| ApiError::not_found("the stage has no log"))?;
    if !canonical_log.starts_with(&canonical_run_dir) {
        return Err(ApiError::bad_request(
            "the stage log lies outside its run directory",
        ));
    }
    let mut file =
        fs::File::open(&canonical_log).map_err(|_| ApiError::not_found("the stage has no log"))?;
    let size = file
        .metadata()
        .map_err(|err| ApiError::internal(format!("failed to read the stage log: {err}")))?
        .len();
    let tail = tail
        .unwrap_or(TEST_RUN_LOG_TAIL_DEFAULT_BYTES)
        .min(TEST_RUN_LOG_TAIL_MAX_BYTES);
    let start = size.saturating_sub(tail);
    file.seek(SeekFrom::Start(start))
        .map_err(|err| ApiError::internal(format!("failed to read the stage log: {err}")))?;
    let mut bytes = Vec::new();
    file.take(tail)
        .read_to_end(&mut bytes)
        .map_err(|err| ApiError::internal(format!("failed to read the stage log: {err}")))?;
    // Move the cut forward past UTF-8 continuation bytes, so the tail starts
    // at a character boundary.
    let skip = if start > 0 {
        bytes
            .iter()
            .take(3)
            .take_while(|byte| (**byte & 0xC0) == 0x80)
            .count()
    } else {
        0
    };
    Ok(TestRunLogTail {
        text: String::from_utf8_lossy(&bytes[skip..]).into_owned(),
        truncated: start > 0,
        size,
    })
}

/// Judges whether the process at a recorded pid may still be the one that
/// recorded it: the pid, and when the file carrying it was last written.
type TestRunLiveness = dyn Fn(u32, Option<std::time::SystemTime>) -> bool;

/// Slack between a file's modification time and the creation time of the
/// process that wrote it, for coarse file and boot-time resolution.
const TEST_RUN_PID_REUSE_MARGIN: Duration = Duration::from_secs(2);

/// Whether the process at `pid` may still be the one that wrote the file
/// carrying `pid` at `written_at`. It must be alive, as far as anything
/// proves, and it must not have been created after that write: the process
/// that wrote a pid existed when it wrote it, so a process created later
/// only reuses the number. Only proof counts: when either time is unknown,
/// the process may be the writer. This only ever turns running into unknown,
/// and never decides passed or failed.
fn test_run_writer_may_be_alive(
    pid: u32,
    written_at: Option<std::time::SystemTime>,
    may_be_alive: &dyn Fn(u32) -> bool,
    created_at: &dyn Fn(u32) -> Option<std::time::SystemTime>,
) -> bool {
    if !may_be_alive(pid) {
        return false;
    }
    // A modification time can be set to anything; one too late to add the
    // margin to proves nothing, so it cannot turn the run unknown.
    match (
        written_at.and_then(|written| written.checked_add(TEST_RUN_PID_REUSE_MARGIN)),
        created_at(pid),
    ) {
        (Some(latest_writer_creation), Some(created)) => created <= latest_writer_creation,
        _ => true,
    }
}

/// `test_run_writer_may_be_alive` against the real process table.
fn test_run_process_writer_may_be_alive(
    pid: u32,
    written_at: Option<std::time::SystemTime>,
) -> bool {
    test_run_writer_may_be_alive(
        pid,
        written_at,
        &test_run_process_may_be_alive,
        &test_run_process_created_at,
    )
}

/// When the live process `pid` was created, if that can be read.
#[cfg(windows)]
fn test_run_process_created_at(pid: u32) -> Option<std::time::SystemTime> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // FILETIME counts 100 ns intervals since 1601-01-01.
    const UNIX_EPOCH_FILETIME: u64 = 116_444_736_000_000_000;
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    let read =
        unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) };
    unsafe { CloseHandle(handle) };
    if read == 0 {
        return None;
    }
    let intervals = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    let since_unix = intervals.checked_sub(UNIX_EPOCH_FILETIME)?;
    std::time::UNIX_EPOCH.checked_add(Duration::from_nanos(since_unix.checked_mul(100)?))
}

/// When the live process `pid` was created, if that can be read: its start
/// time in clock ticks after boot (`/proc/<pid>/stat` field 22) plus the boot
/// time (`btime` in `/proc/stat`, whole seconds, so never later than true).
#[cfg(target_os = "linux")]
fn test_run_process_created_at(pid: u32) -> Option<std::time::SystemTime> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name is parenthesised and may itself hold spaces or
    // parentheses; the fields after it start at field 3.
    let after_command = stat.get(stat.rfind(')')? + 1..)?;
    let start_ticks: u64 = after_command.split_whitespace().nth(22 - 3)?.parse().ok()?;
    let boot_seconds: u64 = fs::read_to_string("/proc/stat")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()?;
    let ticks_per_second = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).ok()?;
    if ticks_per_second == 0 {
        return None;
    }
    let since_boot = Duration::from_secs(start_ticks / ticks_per_second)
        + Duration::from_nanos((start_ticks % ticks_per_second) * 1_000_000_000 / ticks_per_second);
    std::time::UNIX_EPOCH
        .checked_add(Duration::from_secs(boot_seconds))?
        .checked_add(since_boot)
}

/// When the live process `pid` was created, if that can be read.
#[cfg(target_os = "macos")]
fn test_run_process_created_at(pid: u32) -> Option<std::time::SystemTime> {
    let pid = libc::c_int::try_from(pid).ok()?;
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    if read != size {
        return None;
    }
    std::time::UNIX_EPOCH
        .checked_add(Duration::from_secs(info.pbi_start_tvsec))?
        .checked_add(Duration::from_micros(info.pbi_start_tvusec))
}

/// Elsewhere the creation time is not read, so pid reuse is not detected.
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn test_run_process_created_at(_pid: u32) -> Option<std::time::SystemTime> {
    None
}

/// Whether process `pid` may be alive. Only proof of exit counts, as in the
/// launcher's `processMayBeAlive`: a process that cannot be inspected is
/// alive.
#[cfg(windows)]
fn test_run_process_may_be_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, GetLastError, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        // No such process; any other failure (access denied) is not proof.
        return unsafe { GetLastError() } != ERROR_INVALID_PARAMETER;
    }
    let mut exit_code = 0;
    let read = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
    unsafe { CloseHandle(handle) };
    read == 0 || exit_code == STILL_ACTIVE as u32
}

#[cfg(not(windows))]
fn test_run_process_may_be_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return true;
    };
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}
