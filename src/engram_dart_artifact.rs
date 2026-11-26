// Dart test's newline-delimited JSON reporter and the registered focused
// launcher's binding to its own bounded reporter image.
// Protocol: dart-lang/test, pkgs/test/doc/json_reporter.md. Unknown event types
// or versions are UNKNOWN; additive fields on known events remain compatible.

use super::*;
use crate::delegations::delegation_git_boundary::open_metadata_entry;

pub(crate) const ENGRAM_LAUNCHER_DART_REPORT: &str = "focused.dart.json";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExternalDartLauncher {
    pub(crate) script: String,
    pub(crate) commit: String,
    pub(crate) clean: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct EngramExternalDartLaunchProof {
    pub(crate) command_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) provider_location: Option<EngramExternalDartProviderLocation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) named_roots: Vec<EngramWorkSourceRoot>,
    pub(crate) provider_ended: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct EngramExternalDartProviderLocation {
    // Relative runtime directories resolve from the session workdir that
    // belonged to the original launch. Retain only a digest of the cwd.
    session_workdir: String,
    cwd_digest: String,
}

impl EngramExternalDartProviderLocation {
    pub(crate) fn capture(session_workdir: &str, cwd: &str) -> Option<Self> {
        Some(Self {
            session_workdir: session_workdir.to_owned(),
            cwd_digest: Self::cwd_digest(session_workdir, cwd)?,
        })
    }

    pub(crate) fn matches(&self, cwd: &str) -> bool {
        Self::cwd_digest(&self.session_workdir, cwd).as_deref() == Some(self.cwd_digest.as_str())
    }

    fn cwd_digest(session_workdir: &str, cwd: &str) -> Option<String> {
        let workdir = FsPath::new(session_workdir);
        let directory = workdir.join(cwd);
        if cwd.is_empty()
            || !workdir.is_absolute()
            || !directory.is_absolute()
            || engram_network_path(session_workdir)
            || engram_network_path(&directory.to_string_lossy())
        {
            return None;
        }
        let directory = fs::canonicalize(directory).ok()?;
        let path = directory.to_str()?;
        (!engram_network_path(path) && directory.is_dir()).then(|| sha256_hex(path.as_bytes()))
    }
}

/// Bind only the canonical entry script and exact SDK words. No text or
/// option values from the provider command enter the observation store.
pub(crate) fn engram_external_dart_command_digest(script: &str, sdk: &[String]) -> Option<String> {
    let script = fs::canonicalize(script).ok()?;
    let bytes = serde_json::to_vec(&(script.to_str()?, sdk)).ok()?;
    Some(sha256_hex(&bytes))
}

impl ExternalDartLauncher {
    fn measured_commit(&self) -> bool {
        matches!(self.commit.len(), 40 | 64)
            && self
                .commit
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    }

    pub(crate) fn clean_checkout(&self) -> bool {
        self.measured_commit() && self.clean == Some(true)
    }
}

pub(crate) fn engram_external_launcher_local_path(path: &str) -> bool {
    FsPath::new(path).is_absolute()
        && path.len() <= TEST_RUN_PATH_MAX_BYTES
        && !path.chars().any(char::is_control)
        && !engram_network_path(path)
}

pub(crate) fn engram_external_launcher_script(path: &str) -> bool {
    engram_external_launcher_local_path(path)
        && FsPath::new(path).ends_with("scripts/test-launcher.mjs")
}

pub(crate) fn engram_external_launcher_run_parent(root: &FsPath) -> Option<PathBuf> {
    let parent = EngramHost::run_artifact_directory(root)?;
    // Canonicalize the Git admin directory, keeping review-runs itself
    // literal so a redirected directory cannot adopt a sibling's runs.
    Some(fs::canonicalize(parent.parent()?).ok()?.join("review-runs"))
}

pub(crate) fn engram_external_launcher_request(
    request: &Value,
) -> Result<Option<ExternalDartLauncher>, ApiError> {
    request
        .get("externalLauncher")
        .map(|value| {
            let invalid = || ApiError::bad_request("invalid external Dart launcher request");
            let unverified = || ApiError::bad_request(
                "external_launcher_provenance_unverified: the external launcher commit is unavailable",
            );
            if request.get("requestMessageId").is_some() {
                return Err(ApiError::bad_request(
                    "--request-message-id cannot be combined with --project-root",
                ));
            }
            if value.get("commit").and_then(Value::as_str).is_none() {
                return Err(unverified());
            }
            let launcher: ExternalDartLauncher =
                serde_json::from_value(value.clone()).map_err(|_| invalid())?;
            if !launcher.measured_commit() {
                return Err(unverified());
            }
            let Some([stage]) = request
                .get("stages")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
            else {
                return Err(invalid());
            };
            let program = test_run_str(stage, "command").ok_or_else(&invalid)?;
            let mut argv = vec![program];
            argv.extend(test_run_argv(stage.get("args")).ok_or_else(&invalid)?);
            if !engram_external_launcher_script(&launcher.script)
                || !request
                    .get("root")
                    .and_then(Value::as_str)
                    .is_some_and(engram_external_launcher_local_path)
                || !test_run_request_is_committed_input(request)
                || request.get("full").and_then(Value::as_bool) != Some(false)
                || request.get("detached").and_then(Value::as_bool) != Some(false)
                || request.get("scope").is_some_and(|scope| !scope.is_null())
                || request
                    .get("liveEngram")
                    .is_some_and(|live| !live.is_null())
                || request.get("nativeExecution").is_some()
                || stage.get("name").and_then(Value::as_str) != Some("focused")
                || !engram_supports_focused_dart_test(&argv)
            {
                return Err(invalid());
            }
            Ok(launcher)
        })
        .transpose()
}

pub(crate) fn engram_external_dart_verification_shape(
    launcher: &ExternalDartLauncher,
    request: &Value,
    results: &Value,
) -> Result<(), &'static str> {
    if !launcher.clean_checkout() {
        return Err(
            "external_launcher_provenance_unverified: expected a measured clean launcher checkout",
        );
    }
    if !matches!(engram_external_launcher_request(request), Ok(Some(observed)) if observed == *launcher)
        || !matches!(
            results.get("stages").and_then(Value::as_array).map(Vec::as_slice),
            Some([stage]) if stage.get("name").and_then(Value::as_str) == Some("focused")
        )
    {
        return Err("the external launcher must record exactly its registered focused Dart stage");
    }
    Ok(())
}

/// Snapshot names in the invocation's local registration store. This includes
/// retained names a successor can prove it holds at registration. Only the
/// matched registration can select a claim from the original names.
pub(crate) fn engram_external_dart_launch_roots(
    inner: &StateInner,
    origin: &TestRunCommandOrigin,
) -> Vec<EngramWorkSourceRoot> {
    let Some(store) = origin
        .project_id
        .as_deref()
        .and_then(|id| inner.projects.iter().find(|project| project.id == id))
        .filter(|project| project.remote_id == LOCAL_REMOTE_ID)
        .and_then(|project| project.engram.as_ref())
        .and_then(|settings| settings.authority_store_key.as_ref())
    else {
        return Vec::new();
    };
    inner
        .engram_work_source_roots
        .iter()
        .filter(|name| &name.store == store)
        .cloned()
        .collect()
}

/// Keep the original names for the typed native root. Their shared path can
/// place the observation, but only registration can resolve its binding.
pub(crate) fn engram_place_external_dart_launch(
    origin: &mut TestRunCommandOrigin,
    command: &EngramCheckCommand,
) {
    let Some(external) = engram_observed_dart_words(&command.program, &command.argv[1..])
        .and_then(|words| words.external)
    else {
        return;
    };
    origin.source_root = None;
    origin.work_binding = None;
    origin.validated_unbound = false;
    let Ok(requested) = fs::canonicalize(external.root) else {
        origin.external_dart_roots.clear();
        return;
    };
    origin
        .external_dart_roots
        .retain(|root| FsPath::new(&root.root) == requested.as_path());
}

pub(crate) fn engram_external_dart_registered_launch<'a>(
    inner: &'a StateInner,
    row: &'a TestRunCommandObservation,
    artifact: &TestRunObservedArtifact,
) -> Option<(&'a EngramWorkSourceRoot, &'a EngramControlWorkBinding)> {
    let launch = row.external_dart_launch.as_ref()?;
    let registration = inner.test_run_registrations.get(&artifact.run_id)?;
    let named = registration.named_root.as_ref()?;
    let binding = registration.work_binding.as_ref()?;
    let store = registration.authority_store.as_ref()?;
    if registration.external_launcher.is_none()
        || row.summary.owner_session_id.as_deref()
            != Some(registration.initiating_session_id.as_str())
        || row.summary.project_id != registration.project_id
        || row.summary.worktree != registration.worktree
        || artifact.run_directory != registration.run_directory
        || artifact.request_identity.as_ref() != Some(&registration.request_identity)
    {
        return None;
    }
    // Registration has already verified the holder and renewal fence. Its
    // exact name must have been visible when the original invocation began.
    let name = launch.named_roots.iter().find(|name| {
        name.root == named.workspace_id
            && name.generation == named.generation
            && name.work_id == binding.work_id
            && name.claim_id == binding.claim_id
            && name.claim_fence <= binding.claim_fence
            && &name.store == store
    })?;
    Some((name, binding))
}

pub(crate) fn engram_external_launch_terminal_digest<'a>(
    inner: &'a StateInner,
    registration: &TestRunRegistration,
) -> Option<&'a str> {
    registration.external_launcher.as_ref()?;
    let (Some(named), Some(binding), Some(store)) = (
        registration.named_root.as_ref(),
        registration.work_binding.as_ref(),
        registration.authority_store.as_ref(),
    ) else {
        return None;
    };
    let mut rows = inner
        .test_run_observations
        .index()
        .artifact_run(&registration.run_id);
    let key = rows.next()?;
    if rows.next().is_some() {
        return None;
    }
    let row = inner.test_run_observations.get(key).filter(|row| {
        row.declared_native.is_none()
            && row.launch_only
            && row.runtime_identity.is_some()
            && row.summary.project_id == registration.project_id
            && row.summary.worktree == registration.worktree
            && engram_verification_record_row_matches(row, registration)
            && row.work_binding.as_ref() == Some(binding)
            && row
                .artifact
                .as_ref()
                .is_some_and(|artifact| artifact.run_directory == registration.run_directory)
            && row.source_root.as_ref().is_some_and(|root| {
                root.root == named.workspace_id
                    && root.generation == named.generation
                    && root.work_id == binding.work_id
                    && root.claim_id == binding.claim_id
                    && &root.store == store
            })
    })?;
    row.artifact.as_ref()?.terminal_digest.as_deref()
}

enum EngramDartReportError {
    Retry,
    Unusable(&'static str),
}

impl EngramDartReportError {
    fn from_io(error: io::Error, unusable: &'static str) -> Self {
        if matches!(
            error.kind(),
            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
        ) {
            return Self::Retry;
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::{ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION};
            if error.raw_os_error().is_some_and(|code| {
                code == ERROR_SHARING_VIOLATION as i32 || code == ERROR_LOCK_VIOLATION as i32
            }) {
                return Self::Retry;
            }
        }
        Self::Unusable(unusable)
    }
}

/// Read the run-owned image captured at stage close for verification and
/// receipt readback.
fn engram_launcher_dart_result(
    run_dir: &FsPath,
    stage: &Value,
) -> Result<(String, EngramTrxReading), EngramDartReportError> {
    use EngramDartReportError::Unusable;
    let digest = stage
        .get("dartReportDigest")
        .and_then(Value::as_str)
        .ok_or(Unusable(
            "the launcher captured no Dart JSON reporter for this run",
        ))?;
    let path = run_dir.join(ENGRAM_LAUNCHER_DART_REPORT);
    let run_dir = fs::canonicalize(run_dir).map_err(|error| {
        EngramDartReportError::from_io(error, "the run directory is unreadable")
    })?;
    let unreadable = "the captured Dart JSON reporter is missing or unreadable";
    let report_io = |error| EngramDartReportError::from_io(error, unreadable);
    let file = open_metadata_entry(&path).map_err(report_io)?;
    let metadata = file.metadata().map_err(report_io)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || review_freeze_is_reparse(&metadata)
    {
        return Err(Unusable(unreadable));
    }
    if fs::canonicalize(&path).map_err(report_io)? != run_dir.join(ENGRAM_LAUNCHER_DART_REPORT) {
        return Err(Unusable(unreadable));
    }
    let mut bytes = Vec::new();
    file.take(ENGRAM_ARTIFACT_MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(report_io)?;
    if bytes.len() > ENGRAM_ARTIFACT_MAX_BYTES {
        return Err(Unusable("the captured Dart JSON reporter is too large"));
    }
    if file_content_hash(&bytes) != digest {
        return Err(Unusable(
            "the captured Dart JSON reporter differs from its stage record",
        ));
    }
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    Ok((digest.to_owned(), engram_read_dart_json(bytes)))
}

fn engram_dart_runner(program: &str) -> Option<&'static str> {
    let name = FsPath::new(program).file_name()?.to_str()?;
    let windows_name = cfg!(windows).then(|| name.to_ascii_lowercase());
    match windows_name.as_deref().unwrap_or(name) {
        "flutter" => Some("flutter"),
        "dart" => Some("dart"),
        "flutter.bat" if cfg!(windows) => Some("flutter"),
        "dart.exe" | "dart.bat" if cfg!(windows) => Some("dart"),
        _ => None,
    }
}

pub(crate) fn engram_is_focused_dart_sdk_test(argv: &[String]) -> bool {
    matches!(argv, [program, subcommand, ..]
        if subcommand == "test" && engram_dart_runner(program).is_some())
}

/// The requested argv for a launcher-owned reporter. The launcher adds
/// the reporter itself, so a caller-supplied reporter is unsupported.
pub(crate) fn engram_supports_focused_dart_test(argv: &[String]) -> bool {
    let [program, subcommand, args @ ..] = argv else {
        return false;
    };
    let Some(runner) = engram_dart_runner(program).filter(|_| subcommand == "test") else {
        return false;
    };
    let mut seen = BTreeSet::new();
    let mut args = args.iter().map(String::as_str);
    while let Some(arg) = args.next() {
        if !arg.starts_with('-') || arg.chars().any(char::is_control) {
            return false;
        }
        let option = if arg == "-t" {
            "--tags"
        } else if arg.starts_with("--concurrency=") {
            "--concurrency"
        } else {
            arg
        };
        if !seen.insert(option) {
            return false;
        }
        match option {
            "--no-pub" if runner == "flutter" => {}
            "--run-skipped" => {}
            "--concurrency" => {
                let valid = arg.strip_prefix("--concurrency=").is_some_and(|count| {
                    count.bytes().all(|byte| byte.is_ascii_digit())
                        && count.parse::<u32>().is_ok_and(|count| count != 0)
                });
                if !valid {
                    return false;
                }
            }
            "--exclude-tags" | "--tags" | "--plain-name" => {
                let Some(tags) = args.next() else {
                    return false;
                };
                if tags.trim().is_empty()
                    || tags.starts_with('-')
                    || tags.chars().any(char::is_control)
                {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

/// Bind the effective command to its requested runner and args plus exactly
/// this run's reporter suffix. Keep the run-specific path out of the stable
/// verification command.
pub(crate) fn engram_focused_dart_check_argv(
    run_dir: &FsPath,
    root: &FsPath,
    stage: &Value,
    planned: &[String],
) -> Option<Vec<String>> {
    let observed = test_run_argv(stage.get("command"))?;
    let [program, args @ .., flag, value] = observed.as_slice() else {
        return None;
    };
    let cwd = FsPath::new(stage.get("cwd")?.as_str()?);
    if !root.is_absolute()
        || !cwd.is_absolute()
        || fs::canonicalize(root).ok()? != fs::canonicalize(cwd).ok()?
    {
        return None;
    }
    if flag != "--file-reporter"
        || args != &planned[1..]
        || engram_dart_runner(program) != engram_dart_runner(&planned[0])
        || !engram_explicit_dart_runner_matches(cwd, &planned[0], program)
    {
        return None;
    }
    let path = FsPath::new(value.strip_prefix("json:")?);
    let owned = fs::canonicalize(run_dir)
        .ok()?
        .join(ENGRAM_LAUNCHER_DART_REPORT);
    let matches_owned = if cfg!(windows) {
        engram_exact_path_key(path) == engram_exact_path_key(&owned)
    } else {
        path == owned
    };
    if !path.is_absolute() || !matches_owned {
        return None;
    }
    let mut command = observed;
    command.truncate(planned.len());
    Some(command)
}

fn engram_explicit_dart_runner_matches(cwd: &FsPath, requested: &str, observed: &str) -> bool {
    if !requested.contains(['/', '\\']) && !FsPath::new(requested).is_absolute() {
        // The launcher resolved this name through PATH.
        return true;
    }
    let mut path = engram_lexical_path(&cwd.join(requested));
    let executable = FsPath::new(observed);
    if !executable.is_absolute() {
        return false;
    }
    if cfg!(windows) && path.extension().is_none() {
        // The launcher adds only the appropriate supported SDK suffixes.
        // Runner-name equality is already checked by the caller.
        let Some(suffix) = executable.extension().and_then(|suffix| suffix.to_str()) else {
            return false;
        };
        if !suffix.eq_ignore_ascii_case("bat")
            && !(engram_dart_runner(requested) == Some("dart")
                && suffix.eq_ignore_ascii_case("exe"))
        {
            return false;
        }
        path.set_extension(suffix);
    }
    if cfg!(windows) {
        engram_exact_path_key(&path) == engram_exact_path_key(executable)
    } else {
        path.as_path() == executable
    }
}

pub(crate) fn engram_focused_dart_check_result(
    run_dir: &FsPath,
    stage: &Value,
    check: &mut EngramVerificationRecordCheck,
) -> Result<(), EngramVerificationRecordBuildError> {
    use EngramVerificationRecordCheckOutcome as Outcome;
    check.counts = None;
    if check.outcome == Outcome::NotRun {
        return Ok(());
    }
    let completed = matches!(check.outcome, Outcome::Passed | Outcome::Failed)
        && stage.get("interrupted").and_then(Value::as_bool) != Some(true)
        && ["signal", "error"]
            .iter()
            .all(|key| stage.get(key).is_none_or(Value::is_null));
    let result = match stage
        .get("code")
        .and_then(Value::as_i64)
        .filter(|_| completed)
    {
        Some(0) => engram_launcher_dart_result(run_dir, stage),
        Some(code) => {
            check.outcome = Outcome::Failed;
            check.summary = Some(format!("focused: failed (the command exited {code})"));
            return Ok(());
        }
        None => Err(EngramDartReportError::Unusable(
            "the Dart runner did not complete",
        )),
    };
    let verdict = match result {
        Ok((digest, reading)) => {
            check
                .refs
                .push(format!("artifact:{ENGRAM_LAUNCHER_DART_REPORT}"));
            check.refs.push(format!("artifact_digest:{digest}"));
            check.counts = reading
                .counters
                .map(|values| values.values.into_iter().collect());
            reading.verdict
        }
        Err(EngramDartReportError::Unusable(reason)) => {
            EngramTrxVerdict::Unknown(reason.to_owned())
        }
        Err(EngramDartReportError::Retry) => {
            return Err(EngramVerificationRecordBuildError::RetryDartReporter);
        }
    };
    let (outcome, text) = match verdict {
        EngramTrxVerdict::Pass => (Outcome::Passed, "passed (Dart JSON reporter)".to_owned()),
        EngramTrxVerdict::Fail(reason) => (Outcome::Failed, format!("failed ({reason})")),
        EngramTrxVerdict::Unknown(reason) => {
            (Outcome::Indeterminate, format!("unknown ({reason})"))
        }
    };
    check.outcome = outcome;
    check.summary = Some(format!("focused: {text}"));
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum EngramDeclaredArtifactFormat {
    #[default]
    Trx,
    DartJson,
}

pub(crate) fn engram_declared_counter_description(
    format: EngramDeclaredArtifactFormat,
    counters: &EngramTrxCounters,
) -> String {
    if format == EngramDeclaredArtifactFormat::Trx {
        return counters.describe();
    }
    [
        "total",
        "executed",
        "passed",
        "failed",
        "error",
        "notExecuted",
    ]
    .iter()
    .map(|name| format!("{name}={}", counters.get(name)))
    .collect::<Vec<_>>()
    .join(",")
}

pub(crate) fn engram_read_declared_artifact(
    bytes: &[u8],
) -> (EngramDeclaredArtifactFormat, EngramTrxReading) {
    let content = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    if content.iter().find(|byte| !byte.is_ascii_whitespace()) == Some(&b'{') {
        (
            EngramDeclaredArtifactFormat::DartJson,
            engram_read_dart_json(content),
        )
    } else {
        // Pass the original bytes through, including malformed/non-JSON input.
        // The existing reader and all its reasons and counters are unchanged.
        (EngramDeclaredArtifactFormat::Trx, engram_read_trx(bytes))
    }
}

#[derive(Deserialize)]
struct EngramDartEvent {
    time: u64,
    #[serde(flatten)]
    body: EngramDartEventBody,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum EngramDartEventBody {
    #[serde(rename = "start")]
    Start {
        #[serde(rename = "protocolVersion")]
        protocol_version: String,
        pid: u64,
    },
    #[serde(rename = "allSuites")]
    AllSuites { count: u64 },
    #[serde(rename = "suite")]
    Suite { suite: EngramDartSuite },
    #[serde(rename = "group")]
    Group { group: EngramDartGroup },
    #[serde(rename = "testStart")]
    TestStart { test: EngramDartTest },
    #[serde(rename = "testDone")]
    TestDone {
        #[serde(rename = "testID")]
        test_id: u64,
        result: String,
        hidden: bool,
        skipped: bool,
    },
    #[serde(rename = "error")]
    Error {
        #[serde(rename = "testID")]
        test_id: u64,
        error: String,
        #[serde(rename = "stackTrace")]
        stack_trace: String,
        #[serde(rename = "isFailure")]
        is_failure: bool,
    },
    #[serde(rename = "print")]
    Print {
        #[serde(rename = "testID")]
        test_id: u64,
        #[serde(rename = "messageType")]
        message_type: String,
        message: String,
    },
    #[serde(rename = "debug")]
    Debug {
        #[serde(rename = "suiteID")]
        suite_id: u64,
    },
    #[serde(rename = "done")]
    Done { success: Option<bool> },
}

#[derive(Deserialize)]
struct EngramDartSuite {
    id: u64,
    platform: String,
    path: String,
}
#[derive(Deserialize)]
struct EngramDartGroup {
    id: u64,
    #[serde(rename = "suiteID")]
    suite_id: u64,
    #[serde(rename = "testCount")]
    test_count: u64,
}
#[derive(Deserialize)]
struct EngramDartTest {
    id: u64,
    name: String,
    #[serde(rename = "suiteID")]
    suite_id: u64,
    #[serde(rename = "groupIDs")]
    group_ids: Vec<u64>,
}

#[derive(Default)]
struct EngramDartTestResult {
    // The protocol permits an error after testDone, including a hidden test.
    done: Option<(String, bool, bool)>,
    failure: bool,
    error: bool,
}

pub(crate) fn engram_read_dart_json(bytes: &[u8]) -> EngramTrxReading {
    let unknown = |reason: &str| EngramTrxReading {
        verdict: EngramTrxVerdict::Unknown(reason.to_owned()),
        outcome: None,
        counters: None,
    };
    if bytes.len() > ENGRAM_ARTIFACT_MAX_BYTES {
        return unknown("the artifact is too large");
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return unknown("the Dart JSON reporter stream is not UTF-8");
    };
    let mut started = false;
    let mut done = None;
    let mut suites = BTreeSet::new();
    let mut groups = BTreeMap::new();
    let mut suite_count = None;
    let mut tests: BTreeMap<u64, EngramDartTestResult> = BTreeMap::new();
    for line in text.lines() {
        if done.is_some() {
            return unknown("the Dart JSON reporter has an event after done");
        }
        let Ok(event) = serde_json::from_str::<EngramDartEvent>(line) else {
            return unknown("the Dart JSON reporter has a malformed or unknown event");
        };
        let _ = event.time;
        if !started && !matches!(event.body, EngramDartEventBody::Start { .. }) {
            return unknown("the Dart JSON reporter is missing its start event");
        }
        match event.body {
            EngramDartEventBody::Start {
                protocol_version,
                pid,
            } => {
                let _ = pid;
                if started || protocol_version != "0.1.1" {
                    return unknown(
                        "the Dart JSON reporter start or protocol version is unsupported",
                    );
                }
                started = true;
            }
            EngramDartEventBody::AllSuites { count } => {
                if suite_count.replace(count).is_some() {
                    return unknown("the Dart JSON reporter repeats its suite count");
                }
            }
            EngramDartEventBody::Suite { suite } => {
                let _ = (suite.platform, suite.path);
                if !suites.insert(suite.id) {
                    return unknown("the Dart JSON reporter repeats a suite id");
                }
            }
            EngramDartEventBody::Group { group } => {
                let _ = group.test_count;
                if !suites.contains(&group.suite_id)
                    || groups.insert(group.id, group.suite_id).is_some()
                {
                    return unknown("the Dart JSON reporter has an inconsistent group");
                }
            }
            EngramDartEventBody::TestStart { test } => {
                let _ = test.name;
                if !suites.contains(&test.suite_id)
                    || test
                        .group_ids
                        .iter()
                        .any(|id| groups.get(id) != Some(&test.suite_id))
                    || tests
                        .insert(test.id, EngramDartTestResult::default())
                        .is_some()
                {
                    return unknown("the Dart JSON reporter has an inconsistent testStart");
                }
            }
            EngramDartEventBody::TestDone {
                test_id,
                result,
                hidden,
                skipped,
            } => {
                let Some(test) = tests.get_mut(&test_id) else {
                    return unknown("the Dart JSON reporter testDone has no matching testStart");
                };
                if test.done.is_some()
                    || !matches!(result.as_str(), "success" | "failure" | "error")
                {
                    return unknown(
                        "the Dart JSON reporter has an inconsistent or unknown testDone",
                    );
                }
                test.failure |= result == "failure";
                test.error |= result == "error";
                test.done = Some((result, hidden, skipped));
            }
            EngramDartEventBody::Error {
                test_id,
                error,
                stack_trace,
                is_failure,
            } => {
                let _ = (error, stack_trace);
                let Some(test) = tests.get_mut(&test_id) else {
                    return unknown("the Dart JSON reporter error has no matching testStart");
                };
                test.failure |= is_failure;
                test.error |= !is_failure;
            }
            EngramDartEventBody::Print {
                test_id,
                message_type,
                message,
            } => {
                let _ = message;
                if !tests.contains_key(&test_id)
                    || !matches!(message_type.as_str(), "print" | "skip")
                {
                    return unknown("the Dart JSON reporter has an unsupported print event");
                }
            }
            EngramDartEventBody::Debug { suite_id } => {
                if !suites.contains(&suite_id) {
                    return unknown("the Dart JSON reporter debug event has no suite");
                }
            }
            EngramDartEventBody::Done { success } => done = Some(success),
        }
    }
    let Some(Some(success)) = done else {
        return unknown("the Dart JSON reporter has no completed done event");
    };
    if tests.values().any(|test| test.done.is_none()) || suite_count != Some(suites.len() as u64) {
        return unknown("the Dart JSON reporter stream is incomplete or inconsistent");
    }
    let mut total = 0;
    let mut executed = 0;
    let mut passed = 0;
    let mut failed = 0;
    let mut errors = 0;
    let mut any_failure = !success;
    for test in tests.values() {
        any_failure |= test.failure || test.error;
        let (_, hidden, skipped) = test.done.as_ref().expect("complete test");
        // A late error makes a formerly hidden load/setup test visible.
        if *hidden && !test.failure && !test.error {
            continue;
        }
        total += 1;
        if *skipped {
            continue;
        }
        executed += 1;
        if test.error {
            errors += 1;
        } else if test.failure {
            failed += 1;
        } else {
            passed += 1;
        }
    }
    let verdict = if any_failure {
        EngramTrxVerdict::Fail(
            "the Dart JSON reporter records a failed test, error or run".to_owned(),
        )
    } else if executed == 0 || passed == 0 {
        EngramTrxVerdict::Unknown(
            "the Dart JSON reporter executed no non-hidden, non-skipped test".to_owned(),
        )
    } else {
        EngramTrxVerdict::Pass
    };
    EngramTrxReading {
        verdict,
        outcome: Some(if success { "success" } else { "failure" }.to_owned()),
        counters: Some(EngramTrxCounters {
            values: vec![
                ("total".into(), total),
                ("executed".into(), executed),
                ("passed".into(), passed),
                ("failed".into(), failed),
                ("error".into(), errors),
                ("notExecuted".into(), total - executed),
            ],
        }),
    }
}
