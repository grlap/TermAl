// Dart test's newline-delimited JSON reporter and the registered focused
// launcher's binding to its own bounded reporter image.
// Protocol: dart-lang/test, pkgs/test/doc/json_reporter.md. Unknown event types
// or versions are UNKNOWN; additive fields on known events remain compatible.

use super::*;
use crate::delegations::delegation_git_boundary::open_metadata_entry;

pub(crate) const ENGRAM_LAUNCHER_DART_REPORT: &str = "focused.dart.json";

/// Read the run-owned image captured at stage close for verification and
/// receipt readback.
fn engram_launcher_dart_result(
    run_dir: &FsPath,
    stage: &Value,
) -> Result<(String, EngramTrxReading), &'static str> {
    let digest = stage
        .get("dartReportDigest")
        .and_then(Value::as_str)
        .ok_or("the launcher captured no Dart JSON reporter for this run")?;
    let path = run_dir.join(ENGRAM_LAUNCHER_DART_REPORT);
    let run_dir = fs::canonicalize(run_dir).map_err(|_| "the run directory is unreadable")?;
    let unreadable = "the captured Dart JSON reporter is missing or unreadable";
    let file = open_metadata_entry(&path).map_err(|_| unreadable)?;
    let metadata = file.metadata().map_err(|_| unreadable)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || review_freeze_is_reparse(&metadata)
        || fs::canonicalize(&path).ok().as_ref() != Some(&run_dir.join(ENGRAM_LAUNCHER_DART_REPORT))
    {
        return Err(unreadable);
    }
    let mut bytes = Vec::new();
    file.take(ENGRAM_ARTIFACT_MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unreadable)?;
    if bytes.len() > ENGRAM_ARTIFACT_MAX_BYTES {
        return Err("the captured Dart JSON reporter is too large");
    }
    if file_content_hash(&bytes) != digest {
        return Err("the captured Dart JSON reporter differs from its stage record");
    }
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    Ok((digest.to_owned(), engram_read_dart_json(bytes)))
}

fn engram_dart_runner(program: &str) -> Option<&'static str> {
    let name = program.rsplit(['/', '\\']).next()?.to_ascii_lowercase();
    match name.as_str() {
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
    engram_focused_dart_selectors(argv).is_some()
}

pub(crate) fn engram_focused_dart_selectors(argv: &[String]) -> Option<Vec<&str>> {
    let [program, subcommand, args @ ..] = argv else {
        return None;
    };
    let runner = engram_dart_runner(program).filter(|_| subcommand == "test")?;
    let mut seen = BTreeSet::new();
    let mut selectors = Vec::new();
    let mut args = args.iter().map(String::as_str);
    while let Some(arg) = args.next() {
        if arg.is_empty() || arg.chars().any(char::is_control) {
            return None;
        }
        if !arg.starts_with('-') {
            selectors.push(arg);
            continue;
        }
        let option = if arg == "-t" {
            "--tags"
        } else if arg.starts_with("--concurrency=") {
            "--concurrency"
        } else {
            arg
        };
        if !seen.insert(option) {
            return None;
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
                    return None;
                }
            }
            "--exclude-tags" | "--tags" | "--plain-name" => {
                let tags = args.next()?;
                if tags.trim().is_empty()
                    || tags.starts_with('-')
                    || tags.chars().any(char::is_control)
                {
                    return None;
                }
            }
            _ => return None,
        }
    }
    Some(selectors)
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
) {
    use EngramVerificationRecordCheckOutcome as Outcome;
    check.counts = None;
    if check.outcome == Outcome::NotRun {
        return;
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
            return;
        }
        None => Err("the Dart runner did not complete"),
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
        Err(reason) => EngramTrxVerdict::Unknown(reason.to_owned()),
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
