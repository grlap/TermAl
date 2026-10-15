//! Bounded global listing, continuation and unchanged exact-ID access.
use super::*;

async fn list(app: &Router, uri: &str) -> (StatusCode, Value) {
    request_json(
        app,
        Request::builder().uri(uri).body(Body::empty()).unwrap(),
    )
    .await
}

#[tokio::test]
async fn test_run_listing_is_bounded_and_continues_without_hiding_exact_ids() {
    let fixture = RunFixture::new("listing-pages");
    // Keep more active runs than the page cap: retention must not hide them.
    for n in 0..205 {
        write_run(
            &fixture.runs_dir(),
            &format!("test-page-{n:03}"),
            full_request(&fixture.root, "2026-09-25T10:00:00.000Z"),
            Some(running_results(Some(21))),
        );
    }
    fixture.refresh(&[21]);
    let app = app_router(fixture.state.clone());
    let (status, first) = list(&app, "/api/test-runs").await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["runs"].as_array().unwrap().len(), 100);
    assert_eq!(first["limit"], 100);
    let cursor = first["nextCursor"]
        .as_str()
        .expect("more active runs remain");
    // A newly registered run ahead of the cursor must not shift the next page.
    write_run(
        &fixture.runs_dir(),
        "test-newer",
        full_request(&fixture.root, "2026-09-25T11:00:00.000Z"),
        Some(running_results(Some(21))),
    );
    fixture.refresh(&[21]);
    let (status, wrong_scope) = list(
        &app,
        &format!(
            "/api/test-runs?projectId={}&after={cursor}",
            fixture.project_id,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{wrong_scope}");
    let uri = format!("/api/test-runs?limit=200&after={cursor}");
    let (status, rest) = list(&app, &uri).await;
    assert_eq!(status, StatusCode::OK, "{rest}");
    assert_eq!(rest["runs"].as_array().unwrap().len(), 105);
    assert!(rest["nextCursor"].is_null());
    let ids: Vec<_> = first["runs"]
        .as_array()
        .unwrap()
        .iter()
        .chain(rest["runs"].as_array().unwrap())
        .map(|run| run["runId"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        ids,
        (0..205)
            .map(|n| format!("test-page-{n:03}"))
            .collect::<Vec<_>>()
    );
    let (_, capped) = list(&app, "/api/test-runs?limit=999999").await;
    assert_eq!(capped["runs"].as_array().unwrap().len(), 200);
    assert_eq!(capped["limit"], 200);
    let (_, scoped) = list(
        &app,
        &format!("/api/test-runs?limit=2&projectId={}", fixture.project_id),
    )
    .await;
    assert_eq!(scoped["runs"].as_array().unwrap().len(), 2);
    let (_, foreign) = list(&app, "/api/test-runs?limit=2&projectId=another-project").await;
    assert_eq!(foreign["runs"], json!([]));
    assert!(foreign["nextCursor"].is_null());
    let (status, detail) = list(&app, "/api/test-runs/test-page-204").await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["run"]["runId"], "test-page-204");
    assert_eq!(detail["run"]["state"], "running");
    // Paging changes only this route, not live active-run visibility.
    assert_eq!(fixture.state.snapshot().test_runs.len(), 206);
}

#[tokio::test]
async fn test_run_listing_rejects_invalid_limits_and_cursors() {
    let fixture = RunFixture::new("listing-invalid");
    let app = app_router(fixture.state.clone());
    for query in ["limit=0", "limit=-1", "limit=bad", "after=not-a-cursor"] {
        let (status, body) = list(&app, &format!("/api/test-runs?{query}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {body}");
    }
}
