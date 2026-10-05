// Which stages of a test launcher's full gate are its test stages, by the
// rule agreed with the Engram project (docs/test.md, "Which stages count as
// tests"): a stage counts when the run's request record gives it `kind`
// `test`; in a request record whose stages have no `kind` (TermAl's own
// launcher), the stages named `rust-tests` and `vitest` count. Owns that rule
// (`engram_launcher_test_stages`), finding a foreground run's request
// record from the `results:` line of its summary
// (`engram_launcher_output_test_stages`), and the focused-run rule: a focused
// run's one stage counts only when the run's own results.json records it
// passed with test counts showing at least one test passed and none failed
// (`engram_launcher_focused_record_passed`). Does not own reading a carried
// run's records or judging them (`engram_read_carried_run` in
// `engram_carried_checks.rs`), the bounded record read it shares with that
// (`engram_read_launcher_record`), or whether a check's result lines show
// passing tests (`engram_check_showed_passing_tests` in
// `engram_check_recognition.rs`). New module: the rule was a list of the two
// names in `engram_check_recognition.rs`, which held for TermAl's launcher
// only.

/// The test stages of a request record whose stages give no `kind`: TermAl's
/// own launcher's names for them.
const ENGRAM_LAUNCHER_FULL_TEST_STAGES: &[&str] = &["rust-tests", "vitest"];

/// The names of the test stages `request`, a launcher run's request record,
/// lists, in its order: those it gives `kind` `test` when any stage gives a
/// `kind`, else those named in `ENGRAM_LAUNCHER_FULL_TEST_STAGES`. A record
/// that gives kinds but marks no stage `test` has none, and neither has one
/// with no stages, so a run of either is never credited as a passed test
/// check.
fn engram_launcher_test_stages(request: &Value) -> Vec<String> {
    let stages = request
        .get("stages")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let kinded = stages.iter().any(|stage| stage.get("kind").is_some());
    stages
        .iter()
        .filter_map(|stage| {
            let name = stage.get("name").and_then(Value::as_str)?;
            let test = if kinded {
                stage.get("kind").and_then(Value::as_str) == Some("test")
            } else {
                ENGRAM_LAUNCHER_FULL_TEST_STAGES.contains(&name)
            };
            test.then(|| name.to_owned())
        })
        .collect()
}

/// The test stages of the foreground launcher run whose output is `output`,
/// by its own request record: the `request.json` beside the `results.json`
/// that its summary's last `results: PATH` line names, an absolute local path
/// in a directory named for the run the record names. No stage when that
/// record cannot be read, is not that run's, or lies on a network path (never
/// resolved here, on the runtime's event reader: `engram_network_path`). An
/// output with no `results:` line comes from a launcher that names no run
/// directory, and gets the stages named in `ENGRAM_LAUNCHER_FULL_TEST_STAGES`,
/// as before the rule. A focused run's record yields its `focused` stage only
/// when the `results.json` it names backs the pass with counts
/// (`engram_launcher_focused_record_passed`), else no stage, so the summary's
/// own text never earns credit. Reads the file system, so never under the
/// state lock.
fn engram_launcher_output_test_stages(output: &str) -> Vec<String> {
    let Some(results) = output
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix("results: "))
    else {
        return ENGRAM_LAUNCHER_FULL_TEST_STAGES
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
    };
    let results = FsPath::new(results.trim());
    if !results.is_absolute()
        || results.file_name().and_then(|name| name.to_str()) != Some("results.json")
        || engram_network_path(&results.to_string_lossy())
    {
        return Vec::new();
    }
    let Some(directory) = results.parent() else {
        return Vec::new();
    };
    let run = directory.file_name().and_then(|name| name.to_str());
    let read = |name: &str| {
        engram_read_launcher_record(&directory.join(name))
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .filter(|record| run.is_some() && record.get("runId").and_then(Value::as_str) == run)
    };
    let Some(request) = read("request.json") else {
        return Vec::new();
    };
    if engram_launcher_is_focused_request(&request) {
        // The summary's own verdict, its first, must be this run's pass: a
        // `results:` line printed later, inside a stage's diagnostics, cannot
        // lend another run's counts to this one.
        let verdict_is_this_runs_pass =
            run.is_some_and(|run| engram_launcher_first_verdict(output) == Some(("PASS", run, "exit=0")));
        return read("results.json")
            .filter(|results| verdict_is_this_runs_pass && engram_launcher_focused_record_passed(results))
            .map(|_| vec![ENGRAM_LAUNCHER_FOCUSED_STAGE.to_owned()])
            .unwrap_or_default();
    }
    // The name `focused` is reserved for a focused run's backed stage: no
    // other record's stage of that name is ever a test stage.
    engram_launcher_test_stages(&request)
        .into_iter()
        .filter(|stage| stage != ENGRAM_LAUNCHER_FOCUSED_STAGE)
        .collect()
}

/// The first verdict line of a launcher summary, `PASS|FAIL RUN exit=CODE`,
/// as its three words: the summary prints its own verdict before any stage
/// diagnostics.
fn engram_launcher_first_verdict(output: &str) -> Option<(&str, &str, &str)> {
    output.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        let verdict = words.next().filter(|word| matches!(*word, "PASS" | "FAIL"))?;
        let run = words.next()?;
        let exit = words.next().filter(|word| word.starts_with("exit="))?;
        words.next().is_none().then_some((verdict, run, exit))
    })
}

/// The one stage of a focused launcher run.
const ENGRAM_LAUNCHER_FOCUSED_STAGE: &str = "focused";

/// Whether `request` is a focused run's record: not a full gate, with exactly
/// one stage, named `focused`.
fn engram_launcher_is_focused_request(request: &Value) -> bool {
    let stages = request.get("stages").and_then(Value::as_array);
    request.get("full").and_then(Value::as_bool) == Some(false)
        && stages.is_some_and(|stages| {
            stages.len() == 1
                && stages[0].get("name").and_then(Value::as_str)
                    == Some(ENGRAM_LAUNCHER_FOCUSED_STAGE)
        })
}

/// Whether a focused run's `results` record backs a pass: the run passed with
/// exit 0, its one `focused` stage passed with exit 0, and the counts the
/// launcher read from the wrapped runner show at least one test passed and
/// none failed. A focused run executes whatever follows `--`, and its exit
/// alone says nothing about how many tests ran, so a record with no counts,
/// zero passed or any failed is not a pass.
fn engram_launcher_focused_record_passed(results: &Value) -> bool {
    let stages = results.get("stages").and_then(Value::as_array);
    let Some([stage]) = stages.map(Vec::as_slice) else {
        return false;
    };
    let count = |name: &str| {
        stage
            .get("tests")
            .and_then(|tests| tests.get(name))
            .and_then(Value::as_u64)
    };
    results.get("state").and_then(Value::as_str) == Some("passed")
        && results.get("exitCode").and_then(Value::as_i64) == Some(0)
        && stage.get("name").and_then(Value::as_str) == Some(ENGRAM_LAUNCHER_FOCUSED_STAGE)
        && stage.get("state").and_then(Value::as_str) == Some("passed")
        && stage.get("code").and_then(Value::as_i64) == Some(0)
        && count("passed").is_some_and(|passed| passed > 0)
        && count("failed") == Some(0)
}
