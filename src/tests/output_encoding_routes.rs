//! Output-limit behavior through the actual HTTP router and SSE body.
use super::*;

#[tokio::test]
async fn state_read_overflow_is_not_a_mutation_and_retains_cors() {
    let state = test_app_state();
    let app = app_router(state);
    let response = request_response(
        &app,
        Request::get("/api/state")
            .header("origin", "http://localhost:8787")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let _budget = TestOutputBudget::new(64);
    let response = request_response(
        &app,
        Request::get("/api/state")
            .header("origin", "http://localhost:8787")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        response
            .headers()
            .contains_key(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN)
    );
    let error: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 2048).await.unwrap()).unwrap();
    assert_eq!(error["code"], "output_limit_exceeded");
    assert_eq!(error["mutationOutcome"], "not_applicable");
    assert_eq!(error["limitBytes"], 64);
}

#[tokio::test]
async fn a_real_file_mutation_survives_its_response_limit_failure() {
    let state = test_app_state();
    let path = FsPath::new(&state.default_workdir).join("bounded-response.txt");
    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Bounded response".to_owned()),
            root_path: state.default_workdir.clone(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    let app = app_router(state.clone());
    let request = json!({
        "path": path.to_str().unwrap(), "content": "baseline", "overwrite": true,
        "projectId": project.project_id,
    });
    let (status, baseline): (StatusCode, Value) = request_json(
        &app,
        Request::put("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&request).unwrap()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{baseline}");
    assert_eq!(baseline["content"], "baseline");
    assert_eq!(fs::read_to_string(&path).unwrap(), "baseline");

    let content = "written once even when the response cannot be sent".repeat(16);
    let request = json!({
        "path": path.to_str().unwrap(), "content": content, "overwrite": true,
        "projectId": project.project_id,
    });
    let _budget = TestOutputBudget::new(128);
    let response = app
        .oneshot(
            Request::put("/api/file")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let value: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 2048).await.unwrap()).unwrap();
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{value}");
    assert_eq!(value["code"], "output_limit_exceeded");
    assert_eq!(value["mutationOutcome"], "committed");
    assert_eq!(fs::read_to_string(path).unwrap(), content);
}

async fn next_frame(
    body: &mut std::pin::Pin<
        Box<impl futures_core::Stream<Item = Result<axum::body::Bytes, axum::Error>>>,
    >,
) -> Option<String> {
    std::future::poll_fn(|cx| body.as_mut().poll_next(cx))
        .await
        .map(|frame| String::from_utf8(frame.expect("SSE frame").to_vec()).unwrap())
}

#[tokio::test]
async fn oversized_initial_state_ends_with_one_terminal_event_and_no_partial_state() {
    let state = test_app_state();
    let _budget = TestOutputBudget::new(64);
    let response = app_router(state)
        .oneshot(
            Request::get("/api/events?stateDiff=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 2048).await.unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(text.starts_with("event: outputLimit\n"), "{text}");
    assert_eq!(text.matches("event:").count(), 1);
    assert!(!text.contains("event: state\n"));
}

#[tokio::test]
async fn oversized_delta_recovers_immediately_and_an_oversized_recovery_closes_the_stream() {
    let state = test_app_state();
    let response = app_router(state.clone())
        .oneshot(Request::get("/api/events").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let mut body = Box::pin(response.into_body().into_data_stream());
    assert!(
        next_frame(&mut body)
            .await
            .unwrap()
            .starts_with("event: state\n")
    );
    {
        let _budget = TestOutputBudget::new(64);
        state
            .state_broadcast_senders
            .send_delta_event(&DeltaEvent::TestRunRemoved {
                revision: 1,
                run_id: "oversized-test-run-id".repeat(8),
            });
    }
    // The one failed publication itself wakes recovery; no later state mutation.
    assert!(
        next_frame(&mut body)
            .await
            .unwrap()
            .starts_with("event: lagged\n")
    );
    assert!(
        next_frame(&mut body)
            .await
            .unwrap()
            .starts_with("event: state\n")
    );
    let _budget = TestOutputBudget::new(64);
    state
        .state_broadcast_senders
        .send_delta_event(&DeltaEvent::TestRunRemoved {
            revision: 2,
            run_id: "oversized-test-run-id".repeat(8),
        });
    assert!(
        next_frame(&mut body)
            .await
            .unwrap()
            .starts_with("event: lagged\n")
    );
    assert!(
        next_frame(&mut body)
            .await
            .unwrap()
            .starts_with("event: outputLimit\n")
    );
    assert!(next_frame(&mut body).await.is_none());
}
