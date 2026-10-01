// Which stages of a test launcher's full gate are its test stages, by the
// rule agreed with the Engram project (docs/test.md, "Which stages count as
// tests"): a stage counts when the run's request record gives it `kind`
// `test`; in a request record whose stages have no `kind` (TermAl's own
// launcher), the stages named `rust-tests` and `vitest` count. Owns that rule
// (`engram_launcher_test_stages`) and finding a foreground run's request
// record from the `results:` line of its summary
// (`engram_launcher_output_test_stages`). Does not own reading a carried
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
/// as before the rule. Reads the file system, so never under the state lock.
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
    engram_read_launcher_record(&directory.join("request.json"))
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .filter(|request| run.is_some() && request.get("runId").and_then(Value::as_str) == run)
        .map(|request| engram_launcher_test_stages(&request))
        .unwrap_or_default()
}
