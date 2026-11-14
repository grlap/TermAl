// Genuine registration/provider owners with synthetic launcher telemetry.
// The selected message is created/read by the real S2b mailbox route.
use super::*;

pub(in crate::tests) async fn producer<F, Fut>(
    state: AppState,
    label: &str,
    select: F,
) -> (String, String, PathBuf)
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = String>,
{
    let root = state.test_temp_root.as_ref().unwrap().path().join(label);
    fs::create_dir_all(&root).unwrap();
    run_git_test_command(&root, &["init", "--quiet"]);
    let project_id = create_test_project(&state, &root, label);
    let fixture = RunFixture {
        state,
        root,
        project_id,
    };
    let session = running_owner(&fixture, "causal-native-wire-runtime");
    let message = select(session.clone()).await;
    observed_terminal::start(
        &fixture,
        &session,
        "causal-native-wire-tool",
        observed_terminal::LAUNCH,
    );
    let id = format!("test-{}", Uuid::new_v4());
    let mut request = full_request(
        &fs::canonicalize(&fixture.root).unwrap(),
        &test_run_timestamp(),
    );
    request["owner"] = json!(session);
    request["creatorPid"] = json!(std::process::id());
    request["input"] = json!("commit");
    request["requestMessageId"] = json!(message);
    let directory = write_run(
        &fixture.runs_dir(),
        &id,
        request,
        Some(running_results(Some(717))),
    );
    let (status, response): (StatusCode, Value) = request_json(
        &app_router(fixture.state.clone()),
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session}/test-runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"runId":id,"runDirectory":test_run_display_path(&directory),
                "requestMessageId":message})
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert!(status.is_success(), "{status}: {response}");
    // This fixture models a foreground provider. Its RUN output associates
    // the original artifact while the command is still running; it is not a
    // background-launch acknowledgement or a final no-binding proof.
    fixture.state.observe_test_command_artifact(
        &session,
        &EngramObservationProvenance::Ambient,
        "causal-native-wire-tool",
        observed_terminal::LAUNCH,
        &format!("RUN {}", test_run_display_path(&directory)),
        false,
    );
    (session, id, directory)
}

pub(in crate::tests) fn finish(
    state: &AppState,
    id: &str,
    directory: &FsPath,
    verdict: native_test_completion::Verdict,
    before: Value,
    after: Value,
) {
    observed_terminal::finish_artifact(
        directory,
        id,
        verdict == native_test_completion::Verdict::Pass,
    );
    let path = directory.join("results.json");
    let mut results: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    results["sourceIdBefore"] = before;
    results["sourceIdAfter"] = after;
    results["interrupted"] = json!(verdict == native_test_completion::Verdict::Interrupted);
    fs::write(path, results.to_string()).unwrap();
    let registration = state.inner.lock().unwrap().test_run_registrations[id].clone();
    state.note_engram_command_finished_with_capture(
        &registration.initiating_session_id,
        &EngramObservationProvenance::Ambient,
        "causal-native-wire-tool",
        observed_terminal::LAUNCH,
        &format!("RUN {}", test_run_display_path(directory)),
        Some(EngramCommandExit::Code(
            if verdict == native_test_completion::Verdict::Pass {
                0
            } else {
                101
            },
        )),
        false,
    );
    state.refresh_test_runs_with(&|_, _| false, &|_, _| {});
}

pub(in crate::tests) fn compact(state: &AppState, id: &str) {
    let registration = state.inner.lock().unwrap().test_run_registrations[id].clone();
    let fixture = RunFixture {
        state: state.clone(),
        root: PathBuf::from(&registration.worktree),
        project_id: registration.project_id.unwrap(),
    };
    assert_eq!(
        fixture.summary(id).registration.restart_would_void_evidence,
        Some(false),
        "the original foreground provider must have final no-binding proof before compaction"
    );
    compaction::fill_unbound_history(&fixture, TEST_RUN_TERMINAL_RUNS_PER_PROJECT + 1);
    fixture.refresh(&[]);
    assert!(
        !state
            .inner
            .lock()
            .unwrap()
            .test_run_registrations
            .contains_key(id)
    );
    assert!(state.inner.lock().unwrap().test_run_compacted[id].compacted);
}
