// HTTP routes for test runs (docs/features/test-runs.md): the read-only run
// list, one run's detail and a bounded stage-log tail (slice 1), and
// registering a run wait (slice 2). Owns request parsing and the index
// lookups. Does not own reading run directories (`test_runs_disk.rs`), the
// index itself (`test_runs.rs`) or run waits (`test_run_waits.rs`), and never
// starts, cancels or reruns a run.

/// Registers a wait for the session in the path on one or more test runs;
/// the session is resumed with one bounded prompt when they settle.
async fn create_test_run_wait(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
    request: Result<Json<CreateTestRunWaitRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<TestRunWaitResponse>), ApiError> {
    let Json(request) =
        request.map_err(|rejection| api_json_rejection("test run wait request", rejection))?;
    let response =
        run_blocking_api(move || state.create_test_run_wait(&session_id, request)).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TestRunListQuery {
    project_id: Option<String>,
}

#[derive(Deserialize)]
struct TestRunLogQuery {
    tail: Option<u64>,
}

async fn get_test_runs(
    State(state): State<AppState>,
    query: Result<Query<TestRunListQuery>, QueryRejection>,
) -> Result<Json<TestRunListResponse>, ApiError> {
    let Query(query) = query.map_err(|err| api_query_rejection("test runs", err))?;
    Ok(Json(TestRunListResponse {
        runs: state.test_run_summaries(query.project_id.as_deref()),
    }))
}

async fn get_test_run(
    State(state): State<AppState>,
    AxumPath(run_id): AxumPath<String>,
) -> Result<Json<TestRunDetailResponse>, ApiError> {
    let (run, run_dir) = state
        .test_run_entry(&run_id)
        .ok_or_else(|| ApiError::not_found("unknown test run"))?;
    run_blocking_api(move || {
        Ok(Json(TestRunDetailResponse {
            run,
            detail: test_run_detail(&run_dir)?,
        }))
    })
    .await
}

async fn get_test_run_stage_log(
    State(state): State<AppState>,
    AxumPath((run_id, stage)): AxumPath<(String, String)>,
    query: Result<Query<TestRunLogQuery>, QueryRejection>,
) -> Result<Json<TestRunLogTail>, ApiError> {
    let Query(query) = query.map_err(|err| api_query_rejection("test run log", err))?;
    let (_, run_dir) = state
        .test_run_entry(&run_id)
        .ok_or_else(|| ApiError::not_found("unknown test run"))?;
    run_blocking_api(move || test_run_stage_log_tail(&run_dir, &stage, query.tail).map(Json)).await
}
