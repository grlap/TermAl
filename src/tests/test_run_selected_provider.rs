// Synthetic provider frames exercise the production facade with a selector-
// bearing command and real registration/settlement owners. These fixture facts
// are neither an executed remote test nor verification credit.
use super::*;

pub(in crate::tests) async fn selected_provider_completion<F, Fut>(
    state: AppState,
    mode: MailboxTestMode,
    select: F,
) where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = String>,
{
    let root = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("selected-provider");
    fs::create_dir_all(&root).unwrap();
    run_git_test_command(&root, &["init", "--quiet"]);
    let project_id = create_test_project(&state, &root, "Selected provider");
    let fixture = RunFixture {
        state,
        root,
        project_id,
    };
    let runner = running_owner(&fixture, "selected-provider-runtime");
    let provenance = {
        let mut inner = fixture.state.inner.lock().unwrap();
        inner
            .projects
            .iter_mut()
            .find(|p| p.id == fixture.project_id)
            .unwrap()
            .engram = Some(EngramProjectSettings::default());
        let index = inner.find_session_index(&runner).unwrap();
        let record = &mut inner.sessions[index];
        EngramObservationProvenance::Runtime {
            token: record.runtime.runtime_token().unwrap(),
            turn_generation: record.active_turn_generation,
        }
    };
    // The caller sends through the real authenticated relay and reads the
    // recipient's actual handle. Encoding a local message would lack membership.
    let selector = select(runner.clone()).await;
    let canonical = MailboxRelayLocator::decode(&selector, true)
        .unwrap()
        .unwrap()
        .id;
    let command = match mode {
        MailboxTestMode::Focused => format!(
            "node scripts/test-launcher.mjs focused --input commit --request-message-id {selector} -- cargo test -j 8 --bin termal -- tests::test_runs::test_run_registration::native_completion:: --test-threads=2"
        ),
        MailboxTestMode::Full => format!(
            "node scripts/test-launcher.mjs full --input commit --request-message-id {selector}"
        ),
    };
    let cwd = test_run_display_path(&fs::canonicalize(&fixture.root).unwrap());
    let key = "selected-provider-tool";
    fixture.state.engram_host().observe(
        &runner,
        &provenance,
        EngramRecorderObservation::CommandStarted {
            key,
            ran: Some(&command),
            cwd: Some(&cwd),
        },
    );
    let observed = {
        let inner = fixture.state.inner.lock().unwrap();
        let rows: Vec<_> = inner
            .test_run_observations
            .values()
            .filter(|row| row.tool_identity == sha256_hex(key.as_bytes()))
            .cloned()
            .collect();
        assert_eq!(
            rows.len(),
            1,
            "the real selector-bearing provider start must create one observation, even with Engram disabled"
        );
        assert!(rows[0].runtime_identity.is_some());
        rows[0].execution_id.clone()
    };

    let id = format!("test-{}", Uuid::new_v4());
    let mut request = full_request(
        &fs::canonicalize(&fixture.root).unwrap(),
        &test_run_timestamp(),
    );
    request["owner"] = json!(runner);
    request["creatorPid"] = json!(std::process::id());
    request["detached"] = json!(false);
    request["input"] = json!("commit");
    request["requestMessageId"] = json!(selector);
    request["full"] = json!(mode == MailboxTestMode::Full);
    if mode == MailboxTestMode::Focused {
        request["stages"] = json!([{ "name": "focused" }]);
    }
    let directory = write_run(
        &fixture.runs_dir(),
        &id,
        request,
        Some(running_results(Some(717))),
    );
    let (status, ack): (StatusCode, Value) = request_json(
        &app_router(fixture.state.clone()),
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{runner}/test-runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "runId": id, "runDirectory": test_run_display_path(&directory),
                    "requestMessageId": selector,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ack}");
    assert_eq!(ack["registered"], true);
    {
        let inner = fixture.state.inner.lock().unwrap();
        let registration = &inner.test_run_registrations[&id];
        let origin = registration.native_origin.as_ref().unwrap();
        assert!(origin.project_scope.is_none());
        let causal = test_run_causal_request(registration).unwrap();
        assert_eq!(causal.request_message_id, selector);
        assert_eq!(causal.canonical_message_id, canonical);
        assert!(registration.native_completion.is_none());
    }
    let locator = format!("RUN {}", test_run_display_path(&directory));
    fixture.state.engram_host().observe(
        &runner,
        &provenance,
        EngramRecorderObservation::CommandFinished {
            key,
            command: &command,
            reported_command: Some(&command),
            output: &locator,
            exit: Some(EngramCommandExit::NotFinished),
            runtime_output_cut: false,
        },
    );
    assert_eq!(
        fixture.state.inner.lock().unwrap().test_run_observations[&observed]
            .artifact
            .as_ref()
            .unwrap()
            .run_id,
        id
    );

    let source = json!({
        "commit": "a".repeat(40), "tree": "b".repeat(40),
        "submodules": [], "changes": [],
    });
    let mut results = passed_results();
    results["runId"] = json!(id);
    results["ended"] = json!(test_run_timestamp());
    results["sourceIdBefore"] = source.clone();
    results["sourceIdAfter"] = source.clone();
    if mode == MailboxTestMode::Focused {
        results["stages"] = json!([{
            "name": "focused", "state": "passed", "code": 0,
            "tests": {"passed": 1, "failed": 0, "ignored": 0},
        }]);
    }
    fs::write(directory.join("results.json"), results.to_string()).unwrap();
    fixture.state.engram_host().observe(
        &runner,
        &provenance,
        EngramRecorderObservation::CommandFinished {
            key,
            command: &command,
            reported_command: Some(&command),
            output: &locator,
            exit: Some(EngramCommandExit::Code(0)),
            runtime_output_cut: false,
        },
    );
    fixture.refresh(&[]);
    let registration = fixture.state.inner.lock().unwrap().test_run_registrations[&id].clone();
    let retained = registration
        .native_completion
        .as_ref()
        .expect("the original observed execution must retain its native completion");
    assert_eq!(retained.execution_id, observed);
    assert_eq!(retained.completion.run_id, id);
    assert_eq!(
        retained.completion.result.verdict,
        native_test_completion::Verdict::Pass
    );
    assert_eq!(
        retained.completion.mode,
        match mode {
            MailboxTestMode::Focused => native_test_completion::Mode::Focused,
            MailboxTestMode::Full => native_test_completion::Mode::Full,
        }
    );
    let native_test_completion::Input::SourceId(input) = &retained.completion.input else {
        panic!("native completion must retain SourceId endpoints");
    };
    assert_eq!(
        serde_json::to_value(&input.source_id_before).unwrap(),
        source
    );
    assert_eq!(input.source_id_before, input.source_id_after);
    let durable = fixture
        .state
        .read_durable_native_test_run(&registration)
        .unwrap()
        .unwrap();
    assert_eq!(durable.completion.as_ref(), Some(&retained.completion));
    assert!(
        registration.verification_record.is_none(),
        "producer facts do not mint local credit"
    );
}
