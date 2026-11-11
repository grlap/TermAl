// HTTP behavior controls for aggregate closing custody; fixtures never use a live store.
use super::*;
use tower::ServiceExt;

fn job(adopted_at: &str, terminal: bool) -> EngramRuntimeClosingJob {
    let mut trace = EngramRuntimeObservedTurn::adopted("private-notice".into(), 1);
    trace.host_turn_ref = "private-runtime-turn".into();
    trace.adopted_at = adopted_at.into();
    let scope = EngramRuntimeClosingScope {
        binding: None,
        store: EngramAuthorityStoreKey {
            database_path: "private-database".into(),
            project_id: "private-project".into(),
        },
        connection: EngramConnectionConfig {
            binary_path: "private-binary".into(),
            project_file: "private-project-file".into(),
            home: "private-home".into(),
            project_root: "private-root".into(),
            actor_id: "private-actor".into(),
            actor_context: Some("private-context".into()),
            session_id: "private-session".into(),
        },
        historical_fields: BTreeMap::new(),
        root: EngramNamedRootState::None,
        workdir: "private-workdir".into(),
    };
    let mut job = EngramRuntimeClosingJob::new(trace, None, scope);
    job.attempts = 8;
    job.last_error = Some("private-error runtime_observation_source_refused".into());
    job.terminal_failure = terminal.then(|| "private-terminal-error".into());
    job
}

async fn get_counts(state: &AppState) -> Value {
    let response = app_router(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/diagnostics/closing-jobs")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024)
        .await
        .unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(!text.contains("private-"), "diagnostics leaked job payload");
    serde_json::from_slice(&bytes).unwrap()
}

fn retained(state: &AppState) -> (u64, Vec<Vec<u8>>) {
    let inner = state.inner.lock().unwrap();
    (
        inner.revision,
        inner
            .sessions
            .iter()
            .map(|record| serde_json::to_vec(&record.engram.unadmitted_closing_jobs).unwrap())
            .collect(),
    )
}

#[tokio::test]
async fn closing_jobs_http_counts_resident_states_utc_days_without_mutation_or_payload() {
    let state = crate::tests::test_app_state();
    {
        let mut inner = state.inner.lock().unwrap();
        for agent in [Agent::Codex, Agent::Claude] {
            inner.create_session(agent, None, state.default_workdir.clone(), None, None);
        }
        inner.sessions[0].engram.unadmitted_closing_jobs = vec![
            job("2026-11-01T23:30:00-02:00", true),
            job("2026-11-02T00:00:00Z", false),
            job("2026-11-02T00:30:00+02:00", false),
        ];
        inner.sessions[1].engram.unadmitted_closing_jobs = vec![
            job("private-invalid-date", true),
            job("2026-02-30T00:00:00Z", false),
            job("2026-11-02T12:00:00.000Z", true),
        ];
        // Presence, not nonempty text, defines terminal custody.
        inner.sessions[1].engram.unadmitted_closing_jobs[2].terminal_failure = Some(String::new());
    }
    let before = retained(&state);
    let body = get_counts(&state).await;
    assert_eq!(body["serverInstanceId"], state.server_instance_id);
    assert_eq!(body["revision"], before.0);
    assert_eq!(body["total"], json!({"terminal":3,"retrying":3}));
    assert_eq!(
        body["adoptedDay"],
        json!({
            "2026-11-01":{"terminal":0,"retrying":1},
            "2026-11-02":{"terminal":2,"retrying":1},
            "unknown":{"terminal":1,"retrying":1}
        })
    );
    let reasons = body["byReason"].as_object().unwrap();
    assert_eq!(reasons.len(), 4);
    for key in ["focusScopeChanged", "sourceRefused", "maxAttempts", "other"] {
        assert!(reasons[key].is_u64());
    }
    assert_eq!(
        reasons.values().map(|n| n.as_u64().unwrap()).sum::<u64>(),
        3
    );
    assert_eq!(body.as_object().unwrap().len(), 5);
    assert_eq!(get_counts(&state).await, body);
    assert_eq!(retained(&state), before);
}

#[tokio::test]
async fn closing_jobs_http_distinguishes_terminal_reasons_and_retrying_errors() {
    let state = crate::tests::test_app_state();
    {
        let mut inner = state.inner.lock().unwrap();
        inner.create_session(
            Agent::Codex,
            None,
            state.default_workdir.clone(),
            None,
            None,
        );
        let mut scope = job("2026-11-02T00:00:00Z", true);
        scope.terminal_failure =
            Some(runtime_observation_scope_changed(&scope.scope, "private-current").message);
        let mut source = job("2026-11-02T00:00:00Z", true);
        source.terminal_failure = Some(SourceBasisRefusal::ConfiguredGitFilters.to_string());
        let exhausted = job("2026-11-02T00:00:00Z", true);
        let mut other = job("2026-11-02T00:00:00Z", true);
        other.attempts = 1;
        // A code embedded in arbitrary text is not a producer's constant prefix.
        other.terminal_failure = Some("private-text runtime_observation_scope_changed".into());
        let retrying = job("2026-11-02T00:00:00Z", false);
        inner.sessions[0].engram.unadmitted_closing_jobs =
            vec![scope, source, exhausted, other, retrying];
    }
    let before = retained(&state);
    let body = get_counts(&state).await;
    assert_eq!(body["total"], json!({"terminal":4,"retrying":1}));
    assert_eq!(
        body["byReason"],
        json!({
            "focusScopeChanged":1,"sourceRefused":1,"maxAttempts":1,"other":1
        })
    );
    assert_eq!(retained(&state), before);
}

#[tokio::test]
async fn closing_jobs_http_empty_census_is_zero_and_post_is_refused() {
    let state = crate::tests::test_app_state();
    let before = retained(&state);
    let body = get_counts(&state).await;
    assert_eq!(body["total"], json!({"terminal":0,"retrying":0}));
    assert_eq!(body["adoptedDay"], json!({}));
    assert_eq!(
        body["byReason"],
        json!({
            "focusScopeChanged":0,"sourceRefused":0,"maxAttempts":0,"other":0
        })
    );
    let response = app_router(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/diagnostics/closing-jobs")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(retained(&state), before);
}
