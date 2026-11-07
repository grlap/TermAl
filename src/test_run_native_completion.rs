// Native launcher settlement facts retained by the existing registration
// owner. These are observations, never verification qualification. No new
// result read, process probe, source capture or retrospective origin adoption.

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TestRunNativeCompletion {
    execution_id: String,
    completion: native_test_completion::Completion,
}

fn test_run_native_input(results: &Value) -> native_test_completion::Input {
    if results.get("sourceIdBefore").is_some() || results.get("sourceIdAfter").is_some() {
        return native_test_completion::Input::SourceId(native_test_completion::SourceInput {
            kind: native_test_completion::SourceInputKind::SourceId,
            source_id_before: test_run_source_id(results.get("sourceIdBefore")),
            source_id_after: test_run_source_id(results.get("sourceIdAfter")),
        });
    }
    let input = TestRunCommittedInput::read(results).filter(TestRunCommittedInput::coherent);
    let commit = input.and_then(|input| input.commit);
    let optional = |value: Option<&Value>, git: bool| {
        value.and_then(Value::as_str).filter(|text| if git {
            test_run_commit_id_valid(text)
        } else {
            text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }).map(str::to_owned)
    };
    // A checkout tree without an actual tested commit is not a tested-tree
    // fact. A plain/staged run does not acquire a committed-input identity.
    let tree = (commit.is_some() && results["source"]["commit"].as_str() == commit.as_deref())
        .then(|| optional(results.get("source").and_then(|source| source.get("tree")), true)).flatten();
    native_test_completion::Input::Legacy(native_test_completion::LegacyInput {
        commit, tree,
        before_fingerprint: optional(results.get("before"), false),
        after_fingerprint: optional(results.get("after"), false),
    })
}

fn test_run_native_completion_from_settlement(
    inner: &StateInner,
    registration: &TestRunRegistration,
    row: &TestRunCommandObservation,
    entry: &TestRunEntry,
) -> Option<TestRunNativeCompletion> {
    use native_test_completion::{Completion, Mode, NativeResult, Origin, Verdict};
    let origin = registration.native_origin.as_ref()?;
    let results = entry.disk.results.as_ref()?;
    if row.runtime_identity.is_none()
        || !inner.test_runs.registered.contains(&registration.run_id)
        || registration.project_id.as_deref() != Some(origin.project_id.as_str())
        || registration.owning_session_id != origin.native_session_id
        || registration.initiating_session_id != origin.native_session_id
        || !test_run_observation_matches_run(inner, &entry.summary, Some(&entry.disk), row)
        || test_run_matching_registration(inner, &entry.disk) != Some(registration)
        || entry.disk.read_failures != 0 || !entry.disk.is_terminal()
        || (row.summary.ended_at.is_some()
            && (row.summary.state != entry.summary.state || row.summary.exit_code != entry.summary.exit_code))
    {
        return None;
    }
    let mode = match registration.preset {
        TestRunPreset::Full => Mode::Full,
        TestRunPreset::Focused => Mode::Focused,
        TestRunPreset::Live => return None,
    };
    let verdict = match results.terminal_state? {
        TestRunState::Passed => Verdict::Pass,
        TestRunState::Failed if results.interrupted => Verdict::Interrupted,
        TestRunState::Failed => Verdict::Fail,
        _ => return None,
    };
    let completion = Completion {
        schema_version: 1,
        origin: Origin {
            host_id: origin.host.host_id.clone(),
            runner_session_id: origin.native_session_id.clone(),
        },
        run_id: registration.run_id.clone(), mode,
        result: NativeResult {
            verdict, terminal: true, exit: results.exit_code,
            reason: results.error.clone().filter(|reason| !reason.is_empty())
                .unwrap_or_else(|| if results.interrupted { "interrupted" }
                    else if verdict == Verdict::Pass { "passed" } else { "failed" }.to_owned()),
        },
        input: results.native_input.clone(),
        finished_at: results.ended.clone(),
    };
    completion.validate().ok()?;
    Some(TestRunNativeCompletion { execution_id: row.execution_id.clone(), completion })
}
