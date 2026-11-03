// Dart test's newline-delimited JSON reporter, selected from the same bounded
// artifact image as TRX. No file, command, declaration or lifecycle ownership.
// Protocol: dart-lang/test, pkgs/test/doc/json_reporter.md. Unknown event types
// or versions are UNKNOWN; additive fields on known events remain compatible.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum EngramDeclaredArtifactFormat {
    #[default]
    Trx,
    DartJson,
}

#[cfg(test)]
fn engram_declared_counter_description(format: EngramDeclaredArtifactFormat, counters: &EngramTrxCounters) -> String {
    if format == EngramDeclaredArtifactFormat::Trx { return counters.describe(); }
    ["total", "executed", "passed", "failed", "error", "notExecuted"].iter()
        .map(|name| format!("{name}={}", counters.get(name))).collect::<Vec<_>>().join(",")
}

fn engram_read_declared_artifact(bytes: &[u8]) -> (EngramDeclaredArtifactFormat, EngramTrxReading) {
    let content = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    if content.iter().find(|byte| !byte.is_ascii_whitespace()) == Some(&b'{') {
        (EngramDeclaredArtifactFormat::DartJson, engram_read_dart_json(content))
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
    Start { #[serde(rename = "protocolVersion")] protocol_version: String, pid: u64 },
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
        #[serde(rename = "testID")] test_id: u64,
        result: String,
        hidden: bool,
        skipped: bool,
    },
    #[serde(rename = "error")]
    Error {
        #[serde(rename = "testID")] test_id: u64,
        error: String,
        #[serde(rename = "stackTrace")] stack_trace: String,
        #[serde(rename = "isFailure")] is_failure: bool,
    },
    #[serde(rename = "print")]
    Print {
        #[serde(rename = "testID")] test_id: u64,
        #[serde(rename = "messageType")] message_type: String,
        message: String,
    },
    #[serde(rename = "debug")]
    Debug { #[serde(rename = "suiteID")] suite_id: u64 },
    #[serde(rename = "done")]
    Done { success: Option<bool> },
}

#[derive(Deserialize)]
struct EngramDartSuite { id: u64, platform: String, path: String }
#[derive(Deserialize)]
struct EngramDartGroup {
    id: u64,
    #[serde(rename = "suiteID")] suite_id: u64,
    #[serde(rename = "testCount")] test_count: u64,
}
#[derive(Deserialize)]
struct EngramDartTest {
    id: u64,
    name: String,
    #[serde(rename = "suiteID")] suite_id: u64,
    #[serde(rename = "groupIDs")] group_ids: Vec<u64>,
}

#[derive(Default)]
struct EngramDartTestResult {
    // The protocol permits an error after testDone, including a hidden test.
    done: Option<(String, bool, bool)>,
    failure: bool,
    error: bool,
}

fn engram_read_dart_json(bytes: &[u8]) -> EngramTrxReading {
    let unknown = |reason: &str| EngramTrxReading {
        verdict: EngramTrxVerdict::Unknown(reason.to_owned()), outcome: None, counters: None,
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
            EngramDartEventBody::Start { protocol_version, pid } => {
                let _ = pid;
                if started || protocol_version != "0.1.1" {
                    return unknown("the Dart JSON reporter start or protocol version is unsupported");
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
                if !suites.contains(&group.suite_id) || groups.insert(group.id, group.suite_id).is_some() {
                    return unknown("the Dart JSON reporter has an inconsistent group");
                }
            }
            EngramDartEventBody::TestStart { test } => {
                let _ = test.name;
                if !suites.contains(&test.suite_id)
                    || test.group_ids.iter().any(|id| groups.get(id) != Some(&test.suite_id))
                    || tests.insert(test.id, EngramDartTestResult::default()).is_some()
                {
                    return unknown("the Dart JSON reporter has an inconsistent testStart");
                }
            }
            EngramDartEventBody::TestDone { test_id, result, hidden, skipped } => {
                let Some(test) = tests.get_mut(&test_id) else {
                    return unknown("the Dart JSON reporter testDone has no matching testStart");
                };
                if test.done.is_some() || !matches!(result.as_str(), "success" | "failure" | "error") {
                    return unknown("the Dart JSON reporter has an inconsistent or unknown testDone");
                }
                test.failure |= result == "failure";
                test.error |= result == "error";
                test.done = Some((result, hidden, skipped));
            }
            EngramDartEventBody::Error { test_id, error, stack_trace, is_failure } => {
                let _ = (error, stack_trace);
                let Some(test) = tests.get_mut(&test_id) else {
                    return unknown("the Dart JSON reporter error has no matching testStart");
                };
                test.failure |= is_failure;
                test.error |= !is_failure;
            }
            EngramDartEventBody::Print { test_id, message_type, message } => {
                let _ = message;
                if !tests.contains_key(&test_id) || !matches!(message_type.as_str(), "print" | "skip") {
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
    if tests.values().any(|test| test.done.is_none())
        || suite_count != Some(suites.len() as u64)
    {
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
        if *hidden && !test.failure && !test.error { continue; }
        total += 1;
        if *skipped { continue; }
        executed += 1;
        if test.error { errors += 1; }
        else if test.failure { failed += 1; }
        else { passed += 1; }
    }
    let verdict = if any_failure {
        EngramTrxVerdict::Fail("the Dart JSON reporter records a failed test, error or run".to_owned())
    } else if executed == 0 || passed == 0 {
        EngramTrxVerdict::Unknown("the Dart JSON reporter executed no non-hidden, non-skipped test".to_owned())
    } else { EngramTrxVerdict::Pass };
    EngramTrxReading {
        verdict,
        outcome: Some(if success { "success" } else { "failure" }.to_owned()),
        counters: Some(EngramTrxCounters { values: vec![
            ("total".into(), total), ("executed".into(), executed), ("passed".into(), passed),
            ("failed".into(), failed), ("error".into(), errors), ("notExecuted".into(), total - executed),
        ] }),
    }
}
