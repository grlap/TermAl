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
    limit: Option<usize>,
    after: Option<String>,
}

const TEST_RUN_LIST_DEFAULT_LIMIT: usize = 100;
const TEST_RUN_LIST_MAX_LIMIT: usize = 200;

/// A keyset position, scoped to the project filter. It remains usable if the
/// boundary row disappears. This is a live listing, not a frozen snapshot.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TestRunListCursor {
    project_id: Option<String>,
    started_at: Option<String>,
    run_id: String,
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
    let limit = query.limit.unwrap_or(TEST_RUN_LIST_DEFAULT_LIMIT);
    if limit == 0 {
        return Err(ApiError::bad_request(
            "test run list limit must be positive",
        ));
    }
    let limit = limit.min(TEST_RUN_LIST_MAX_LIMIT);
    let cursor = query
        .after
        .as_deref()
        .map(|encoded| {
            let invalid = || ApiError::bad_request("invalid test run list cursor");
            if encoded.len() > 4096 {
                return Err(invalid());
            }
            let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(encoded)
                .map_err(|_| invalid())?;
            let cursor: TestRunListCursor =
                serde_json::from_slice(&bytes).map_err(|_| invalid())?;
            if cursor.project_id != query.project_id || cursor.run_id.is_empty() {
                return Err(invalid());
            }
            Ok(cursor)
        })
        .transpose()?;
    let project = query.project_id.as_deref();
    let inner = state.inner.lock().expect("state mutex poisoned");
    let mut runs: Vec<_> = test_run_summaries_locked(&inner)
        .into_iter()
        .filter(|run| project.is_none_or(|id| run.project_id.as_deref() == Some(id)))
        .filter(|run| {
            cursor.as_ref().is_none_or(|cursor| {
                run.started_at < cursor.started_at
                    || (run.started_at == cursor.started_at && run.run_id > cursor.run_id)
            })
        })
        .take(limit + 1)
        .collect();
    let next_cursor = if runs.len() > limit {
        runs.truncate(limit);
        let last = runs.last().expect("positive page limit");
        Some(
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&TestRunListCursor {
                    project_id: query.project_id.clone(),
                    started_at: last.started_at.clone(),
                    run_id: last.run_id.clone(),
                })
                .expect("list cursor contains only strings"),
            ),
        )
    } else {
        None
    };
    Ok(Json(TestRunListResponse {
        runs,
        limit,
        next_cursor,
        history: test_run_history_locked(&inner, project),
    }))
}

async fn get_test_run(
    State(state): State<AppState>,
    AxumPath(run_id): AxumPath<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<TestRunDetailResponse>, ApiError> {
    let mut diagnostics = TestRunRegistrationDiagnostics::new(&state.server_instance_id);
    diagnostics.method = "GET";
    diagnostics.correlate(&headers);
    // Ordinary UI/API detail reads keep their existing error envelope and do
    // not emit a registration trace. The launcher's confirmation supplies
    // only a bounded, validated correlation nonce, never authority.
    diagnostics.enabled = diagnostics.operation_id != "unavailable";
    // Exact lookup canonicalizes two locators, and detail reads files. Keep
    // both on the blocking pool; neither operation holds StateInner.
    run_blocking_api(move || {
        let result = get_test_run_observed(state, &run_id, &mut diagnostics);
        match result {
            Err(mut error) => {
                if diagnostics.enabled {
                    diagnostics.report(&run_id, &mut error);
                }
                Err(error)
            }
            Ok(response) => {
                if diagnostics.enabled {
                    diagnostics.enter("get_response_ready");
                    eprintln!("[termal] test run query completed: {}", diagnostics.fields(&run_id, "query_complete", 200));
                }
                Ok(response)
            }
        }
    })
    .await
}

fn get_test_run_observed(
    state: AppState,
    run_id: &str,
    diagnostics: &mut TestRunRegistrationDiagnostics,
) -> Result<Json<TestRunDetailResponse>, ApiError> {
    diagnostics.enter("get_retained_lookup");
    {
        let inner = diagnostics.lock(&state.inner);
        let known_exact = inner.test_runs.find(run_id).is_some()
            || inner.test_runs.registered.contains(run_id);
        let eligible = |row: &&TestRunCompacted| {
            test_run_compact_is_installed(&inner, row)
                && (row.compacted || !known_exact)
        };
        let retained = inner.test_run_compacted.get(run_id).filter(eligible)
            .or_else(|| if known_exact { None } else {
                inner.test_run_compacted.values().filter(eligible)
                    .find(|row| test_run_summary_ids(&row.summary).any(|id| id == run_id))
            });
        if let Some(row) = retained {
            diagnostics.request_digest = row.request_identity.clone();
            let mut run = test_run_compacted_summary(&inner, row);
            test_run_enrich_artifact_summary(&inner, &mut run, None);
            return Ok(Json(TestRunDetailResponse {
                source: "retained",
                run,
                detail: TestRunDetail {
                    limitations: Some(if row.compacted {
                        format!(
                            "Compacted retained identity and proof. {} Full stage/log detail is omitted; artifacts were not deleted.",
                            row.reason
                        )
                    } else {
                        "Retained unresolved artifact identity; current stage/log detail is unavailable.".to_owned()
                    }),
                    ..TestRunDetail::default()
                },
            }));
        }
    }
    // Confirm this exact durable launch without projecting every historical
    // run. Neither branch writes the index or waits for its rescan mutex.
    diagnostics.enter("get_exact_lookup");
    if let Some(exact) = test_run_exact_query(&state, run_id, diagnostics)? {
        diagnostics.request_digest = exact.request_identity;
        return if let Some(directory) = exact.directory {
            diagnostics.enter("get_detail_dispatch");
            let started = std::time::Instant::now();
            let detail = test_run_detail(&directory);
            if diagnostics.enabled {
                eprintln!("[termal] test run query detail: runId={}; operationId={}; attemptId={}; serverInstance={}; phase=get_detail_read; phaseElapsedMs={}; category={}",
                    test_run_diagnostic_id(run_id), diagnostics.operation_id,
                    diagnostics.attempt_id, diagnostics.server_instance, started.elapsed().as_millis(),
                    if detail.is_ok() { "read_complete" } else { "read_failed" });
            }
            Ok(Json(TestRunDetailResponse {
                source: "index", run: exact.run, detail: detail?,
            }))
        } else {
            Ok(Json(TestRunDetailResponse {
                source: "registration", run: exact.run,
                detail: TestRunDetail {
                    limitations: Some("Durable registration; the background artifact index has not caught up.".to_owned()),
                    ..TestRunDetail::default()
                },
            }))
        };
    }
    diagnostics.enter("get_global_summary");
    let summaries = {
        let inner = diagnostics.lock(&state.inner);
        // A registration/index arriving after the captured absence must go
        // through the fenced exact lookup on a fresh request, never this tail.
        if inner.test_run_registrations.contains_key(run_id) || inner.test_runs.find(run_id).is_some() {
            return Err(ApiError::conflict("test run identity changed during query; retry"));
        }
        test_run_summaries_locked(&inner)
    };
    let run = summaries.into_iter().find(|run| run.run_id == run_id)
        .ok_or_else(|| ApiError::not_found("unknown test run"))?;
    if run.run_dir.is_empty() {
        return Ok(Json(TestRunDetailResponse { source: "observation", run, detail: TestRunDetail {
            limitations: Some("Host-observed provider execution; no launcher artifact directory was reported.".to_owned()),
            ..TestRunDetail::default()
        } }));
    }
    Ok(Json(TestRunDetailResponse { source: "retained", run, detail: TestRunDetail {
        limitations: Some("The durable registration is retained; current launcher artifacts are unavailable.".to_owned()),
        ..TestRunDetail::default()
    } }))
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
