// Project-memory wire, isolation and HTTP regressions. Only isolated CLI
// fixtures are executed: no real stores or retained project memory are changed.
use super::*;

#[test]
fn memory_queries_are_bounded_and_positionals_cannot_become_options() {
    let query = WorkMemoryQuery {
        search: Some("--delete; $(evil)".into()),
        ..Default::default()
    };
    assert!(query.validate("engram").is_ok());
    assert_eq!(
        query.arguments(),
        ["memories", "--json", "--", "--delete; $(evil)"]
    );
    assert!(query.validate("beads").is_err());
    assert!(query.validate("other").is_err());
    for query in [
        WorkMemoryQuery {
            key: Some("key".into()),
            ..Default::default()
        },
        WorkMemoryQuery {
            after: Some("key".into()),
            search: Some("query".into()),
            reader_id: Some("id".into()),
            ..Default::default()
        },
        WorkMemoryQuery {
            search: Some("x".repeat(2049)),
            ..Default::default()
        },
        WorkMemoryQuery {
            search: Some("a\nb".into()),
            ..Default::default()
        },
    ] {
        assert!(query.validate("engram").is_err());
    }
}

#[test]
fn memory_receipts_preserve_source_semantics_and_fail_closed() {
    let list = WorkMemoryQuery::default();
    let page = normalize_work_memories("engram", &list, serde_json::json!({
        "memories":[{"key":"guide","revision":2,"first_line":"First", "remembered_at":"now","actor_id":"greg/termal"}],
        "next_after":null,"omitted_count":0,"exhausted":true
    })).unwrap();
    assert_eq!(page.items[0].summary, "First");
    assert!(page.items[0].body.is_none());
    let detail = WorkMemoryQuery {
        key: Some("guide".into()),
        reader_id: Some("reader".into()),
        ..Default::default()
    };
    let full = normalize_work_memories("engram", &detail, serde_json::json!({"key":"guide","revision":2,"body":"Full <script> inert","remembered_at":"now","actor_id":"greg/termal"})).unwrap();
    assert_eq!(full.items[0].body.as_deref(), Some("Full <script> inert"));
    assert!(normalize_work_memories("engram", &detail, serde_json::json!({"key":"wrong","revision":2,"body":"body","remembered_at":"now","actor_id":"greg/termal"})).is_err());
    assert!(
        normalize_work_memories(
            "engram",
            &list,
            serde_json::json!({"memories":[],"exhausted":true})
        )
        .is_err()
    );
    assert!(normalize_work_memories("engram", &list, serde_json::json!({"memories":[],"omitted_count":0,"exhausted":false,"next_after":"loop"})).is_err());
}

#[tokio::test]
async fn memory_http_uses_established_reader_paging_detail_and_rejects_stale_keys() {
    let (state, project, _, root) = super::work_visualizer::fixture();
    let limiter = Arc::new(tokio::sync::Semaphore::new(1));
    let app = app_router(state.clone()).layer(axum::Extension(WorkReadLimiter(limiter.clone())));
    let url = format!("/api/projects/{project}/work-memories/engram");
    let (status, absent): (StatusCode, Value) = request_json(
        &app,
        Request::builder().uri(&url).body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(absent["state"], "unavailable");
    super::work_visualizer::install_store(&state, &project, &root);
    let (status, first): (StatusCode, Value) = request_json(
        &app,
        Request::builder().uri(&url).body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["items"][0]["key"], "guide");
    assert!(first["items"][0]["body"].is_null());
    let reader = first["readerId"].as_str().unwrap();
    for (suffix, expected, key) in [
        (
            format!("?readerId={reader}&after=guide"),
            StatusCode::OK,
            "later",
        ),
        (
            format!("?readerId={reader}&key=guide"),
            StatusCode::OK,
            "guide",
        ),
        ("?readerId=stale&key=guide".into(), StatusCode::CONFLICT, ""),
        ("?key=guide".into(), StatusCode::BAD_REQUEST, ""),
        ("?unexpected=true".into(), StatusCode::BAD_REQUEST, ""),
    ] {
        let (status, response): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .uri(format!("{url}{suffix}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, expected, "{response}");
        if expected == StatusCode::OK {
            assert_eq!(response["items"][0]["key"], key);
        }
    }
    assert_eq!(limiter.available_permits(), 1);
    let args = fs::read_to_string(root.join("work-read-args.txt")).unwrap();
    assert!(
        args.contains("memories") && args.contains("--full") && args.contains("termal-work-view")
    );
    assert!(!args.contains("recall") && !args.contains("remember\n"));
}

#[test]
fn engram_memory_reads_stop_when_the_request_was_abandoned() {
    let (state, project, _, root) = super::work_visualizer::fixture();
    super::work_visualizer::install_store(&state, &project, &root);
    for already_abandoned in [true, false] {
        let abandoned = std::sync::atomic::AtomicBool::new(already_abandoned);
        let error = state
            .read_work_memories(
                &project,
                "engram",
                WorkMemoryQuery::default(),
                || {
                    assert!(
                        !already_abandoned,
                        "abandoned memory read requested admission"
                    );
                    abandoned.store(true, std::sync::atomic::Ordering::Relaxed);
                    Ok::<(), ApiError>(())
                },
                &abandoned,
            )
            .unwrap_err();
        assert_eq!(error.status.as_u16(), 499);
        assert!(!root.join("work-read-args.txt").exists());
    }
}
