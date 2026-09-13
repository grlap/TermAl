// New Work read tests: metadata refusal, receipt normalization and argv-only
// transport. All stores and bindings below are isolated fixtures, never live.
use super::*;

#[tokio::test]
async fn work_routes_validate_and_detect_without_available_cli_permits() {
    let (state, project, _, _) = fixture();
    let limiter = Arc::new(tokio::sync::Semaphore::new(2));
    let app = app_router(state).layer(axum::Extension(WorkReadLimiter(limiter.clone())));
    let _permits = limiter.acquire_many(2).await.unwrap();
    for (suffix, expected) in [
        ("".to_owned(), StatusCode::OK),
        (
            format!("?search={}", "x".repeat(20_000)),
            StatusCode::BAD_REQUEST,
        ),
        ("/engram/w-one".to_owned(), StatusCode::BAD_REQUEST),
        (
            "/engram/w-one?readerSessionId=missing".to_owned(),
            StatusCode::CONFLICT,
        ),
    ] {
        let (status, _): (StatusCode, Value) = tokio::time::timeout(
            Duration::from_secs(2),
            request_json(
                &app,
                Request::builder()
                    .uri(format!("/api/projects/{project}/work{suffix}"))
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .await
        .expect("metadata route waited for CLI capacity");
        assert_eq!(status, expected);
    }
}

#[tokio::test]
async fn work_ready_http_routes_use_blocking_admission_and_release_private_permits() {
    let (state, project, session, root) = fixture();
    install_binding(&state, &project, &session, &root);
    let limiter = Arc::new(tokio::sync::Semaphore::new(1));
    let app = app_router(state).layer(axum::Extension(WorkReadLimiter(limiter.clone())));
    for suffix in [
        String::new(),
        format!("/engram/w-test?readerSessionId={session}"),
    ] {
        let (status, response): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .uri(format!("/api/projects/{project}/work{suffix}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{response}");
        if suffix.is_empty() {
            assert_eq!(response["page"]["items"][0]["shortRef"], "w-test");
            assert_eq!(response["readerSessionId"], session);
        } else {
            assert_eq!(response["status"]["work"]["shortRef"], "w-test");
        }
        assert_eq!(limiter.available_permits(), 1);
    }
    // Closing only this router's limiter proves ready requests actually use
    // the admission dependency (and cannot silently bypass block_on).
    limiter.close();
    for suffix in [
        String::new(),
        format!("/engram/w-test?readerSessionId={session}"),
    ] {
        let (status, response): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .uri(format!("/api/projects/{project}/work{suffix}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{response}");
        assert!(response.to_string().contains("limiter closed"));
    }
}

#[test]
fn work_detection_and_invalid_requests_do_not_request_cli_capacity() {
    let (state, project, _, _) = fixture();
    let never_admit =
        || -> Result<(), ApiError> { panic!("metadata-only request asked for a CLI permit") };
    let response = state
        .list_project_work_with_admission(&project, WorkListQuery::default(), never_admit)
        .unwrap();
    assert!(response.page.is_none());
    let error = state
        .list_project_work_with_admission(
            &project,
            WorkListQuery {
                search: Some("x".repeat(20_000)),
                ..Default::default()
            },
            never_admit,
        )
        .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    let error = state
        .read_project_work_detail_with_admission(
            &project,
            "-invalid",
            WorkDetailQuery {
                reader_session_id: "reader".into(),
                after: None,
            },
            never_admit,
        )
        .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    let error = state
        .read_project_work_detail_with_admission(
            &project,
            "w-one",
            WorkDetailQuery {
                reader_session_id: "reader".into(),
                after: None,
            },
            never_admit,
        )
        .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn work_replacement_read_waits_for_an_existing_read_to_release_capacity() {
    let limiter = Arc::new(tokio::sync::Semaphore::new(2));
    let first = limiter.clone().acquire_owned().await.unwrap();
    let second = limiter.clone().acquire_owned().await.unwrap();
    let replacement = acquire_work_read_permit_from(limiter.clone());
    tokio::pin!(replacement);
    assert!(
        std::future::Future::poll(
            replacement.as_mut(),
            &mut std::task::Context::from_waker(std::task::Waker::noop())
        )
        .is_pending()
    );
    drop(first);
    let admitted = replacement.await.unwrap();
    assert_eq!(limiter.available_permits(), 0);
    drop(admitted);
    drop(second);
    assert_eq!(limiter.available_permits(), 2);
}

#[test]
fn work_rejects_redirected_home_before_launching_a_read() {
    let (state, project, session, root) = fixture();
    install_binding(&state, &project, &session, &root);
    let mut target = state.work_read_snapshot(&project, None).unwrap().1.unwrap();
    let alias = root.parent().unwrap().join("work-home-alias");
    let other = root.parent().unwrap().join("different-work-home");
    let other_database = work_database_path(&other, &target.store.project_id);
    fs::create_dir_all(other_database.parent().unwrap()).unwrap();
    fs::write(&other_database, "different store").unwrap();
    let make_link = |destination: &FsPath| {
        #[cfg(windows)]
        if let Err(error) = std::os::windows::fs::symlink_dir(destination, &alias) {
            if super::delegation_validation::windows_symlink_privilege_unavailable(&error) {
                eprintln!(
                    "skipping redirected Work home assertion without symlink privilege: {error}"
                );
                return false;
            }
            panic!("cannot create Work fixture symlink: {error}");
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(destination, &alias).unwrap();
        true
    };
    if !make_link(&root) {
        return;
    }
    target.connection.home = alias.clone();
    validate_work_read_target(&target).unwrap();
    // Only remove the exact fixture link, never its target directory.
    #[cfg(windows)]
    fs::remove_dir(&alias).unwrap();
    #[cfg(unix)]
    fs::remove_file(&alias).unwrap();
    if !make_link(&other) {
        return;
    }
    let error = validate_work_read_target(&target).unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("store resolution changed"));
    assert!(
        target.store.database_path.is_file(),
        "the admitted store is still present"
    );
    assert_eq!(
        fs::read_to_string(other_database).unwrap(),
        "different store"
    );
    assert!(!root.join("work-read-args.txt").exists());
    assert!(!other.join("work-read-args.txt").exists());
    #[cfg(windows)]
    fs::remove_dir(&alias).unwrap();
    #[cfg(unix)]
    fs::remove_file(&alias).unwrap();
}

#[test]
fn work_shims_are_unavailable_and_oversized_combined_arguments_are_client_errors() {
    let (state, project, session, root) = fixture();
    install_binding(&state, &project, &session, &root);
    let mut target = state.work_read_snapshot(&project, None).unwrap().1.unwrap();
    let error = run_work_read_command(
        &target.connection,
        &[
            "ls".into(),
            format!("--search={}", "x".repeat(10_000)),
            format!("--label={}", "y".repeat(10_000)),
        ],
    )
    .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("too large"));
    for extension in ["cmd", "BAT"] {
        target.connection.binary_path = root.join(format!("engram.{extension}"));
        assert_eq!(
            validate_work_read_target(&target).unwrap_err().status,
            StatusCode::CONFLICT
        );
    }
    {
        let mut inner = state.inner.lock().unwrap();
        inner
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .unwrap()
            .engram
            .as_mut()
            .unwrap()
            .binary_path = Some(target.connection.binary_path.to_string_lossy().into_owned());
        let installed = engram_mcp_runtime_config_for_session_locked(&inner, &session)
            .unwrap()
            .installed;
        let index = inner.find_session_index(&session).unwrap();
        inner
            .session_mut_by_index(index)
            .unwrap()
            .engram_mcp_installed = Some(installed);
    }
    let response = state
        .list_project_work(&project, WorkListQuery::default())
        .unwrap();
    assert!(response.page.is_none());
    assert!(response.sources.iter().any(|s| s.source == "engram"
        && s.state == "unavailable"
        && s.message.contains("native Engram")));
    assert!(!root.join("work-read-args.txt").exists());
}

fn listing() -> Value {
    json!({"items":[{"work":{"work_id":"w-id", "short_ref":"w-one", "title":"Full title <script> inert",
        "kind":"bug","lifecycle":"open","priority":1,"labels":["decision"],"assigned_to":"actor-not-holder",
        "parent_id":null,"updated_at":"2026-09-13T00:00:00Z"},"availability":"blocked","blocked_by":["prerequisite"]}],
        "total":1,"shown_before":0,"more":false,"next":["engram work claim w-one"]})
}

#[test]
fn work_receipt_keeps_lifecycle_availability_and_assignment_separate() {
    let page = normalize_engram_work_page(listing()).unwrap();
    let row = &page.items[0];
    assert_eq!(row.lifecycle, "open");
    assert_eq!(row.availability, "blocked");
    assert_eq!(row.assigned_to.as_deref(), Some("actor-not-holder"));
    let wire = serde_json::to_value(page).unwrap();
    assert_eq!(wire["items"][0]["shortRef"], "w-one");
    assert!(wire.get("next").is_none());
    assert!(wire["items"][0].get("holder").is_none());
}

#[test]
fn work_receipt_errors_are_not_empty_lists_and_byte_cut_does_not_loop() {
    for malformed in [json!({}), json!([]), json!({"items":[],"total":0})] {
        assert_eq!(
            normalize_engram_work_page(malformed).unwrap_err().status,
            StatusCode::BAD_GATEWAY
        );
    }
    let mut bad = listing();
    bad["items"][0]["work"]["priority"] = json!(5);
    assert!(normalize_engram_work_page(bad).is_err());
    let mut duplicate = listing();
    let row = duplicate["items"][0].clone();
    duplicate["items"].as_array_mut().unwrap().push(row);
    duplicate["total"] = json!(2);
    assert!(normalize_engram_work_page(duplicate).is_err());
    let mut wrong_counts = listing();
    wrong_counts["more"] = json!(true);
    assert!(normalize_engram_work_page(wrong_counts).is_err());
    let page = normalize_engram_work_page(
        json!({"items":[],"total":1,"shown_before":0,"more":true,"hint":"use show"}),
    )
    .unwrap();
    assert!(page.more);
    assert!(page.after.is_none());
    assert_eq!(page.hint.as_deref(), Some("use show"));
}

#[test]
fn work_arguments_keep_filters_as_data_and_validate_continuations() {
    let query = WorkListQuery {
        search: Some("--init; $(claim) 'quoted' &".into()),
        label: Some("decision".into()),
        ..Default::default()
    };
    query.validate().unwrap();
    assert!(
        query
            .arguments()
            .contains(&"--search=--init; $(claim) 'quoted' &".to_owned())
    );
    assert_eq!(query.arguments()[0], "ls");
    assert!(
        WorkListQuery {
            after: Some("opaque".into()),
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        WorkListQuery {
            availability: Some("active".into()),
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        WorkListQuery {
            search: Some("x\ny".into()),
            ..Default::default()
        }
        .validate()
        .is_err()
    );
}

#[test]
fn work_detail_requires_window_and_keeps_continuation_separate() {
    let window = json!({"total":2,"shown":1,"newer":0,"older":1,"after":"opaque-notes",
        "read_cut":{"project_position":9,"observed_at":"2026-09-13T00:00:00Z","valid_until_ms":null}});
    let note = json!({"locator":"note-1","kind":"note","family":"notes","summary":"inert text",
        "created_at":"2026-09-13T00:00:00Z","by":"peer-label"});
    let value = json!({"status":{"work":{"short_ref":"w-one","title":"Work","outcome":"Goal",
        "acceptance":["Check"],"priority":2,"kind":"task","lifecycle":"open"},"availability":"ready"},
        "holder":"peer-not-a-session-id","notes":[note.clone()],"notes_window":window.clone()});
    let detail = normalize_engram_work_detail(value, false).unwrap();
    assert_eq!(detail.status.unwrap().work.acceptance, vec!["Check"]);
    assert_eq!(detail.holder.as_deref(), Some("peer-not-a-session-id"));
    let continuation =
        json!({"work":{"short_ref":"w-one","title":"Work"}, "notes":[note],"notes_window":window});
    assert!(normalize_engram_work_detail(continuation.clone(), false).is_err());
    assert!(
        normalize_engram_work_detail(continuation, true)
            .unwrap()
            .status
            .is_none()
    );
    assert!(normalize_engram_work_detail(json!({"status":null,"notes":[]}), true).is_err());
}

#[test]
fn work_notes_preserve_status_provenance_and_inert_references() {
    // Synthetic show --notes shape: both status rows use the same family.
    let note = json!({"locator":"owner", "kind":"status", "family":"observations",
        "summary":"See attached evidence", "created_at":"now", "status_owner":true,
        "non_holder":true, "refs":["report/path", "<script>do_not_run()</script>"]});
    let mut peer = note.clone();
    peer["locator"] = json!("peer");
    peer["status_owner"] = json!(false);
    let absent =
        json!({"locator":"unknown", "kind":"status", "family":"observations", "created_at":"now"});
    let detail = normalize_engram_work_detail(
        json!({"notes":[note,peer,absent],
        "notes_window":{"total":3,"shown":3,"newer":0,"older":0,
        "read_cut":{"project_position":1,"observed_at":"now"}}}),
        true,
    )
    .unwrap();
    let wire = serde_json::to_value(detail).unwrap();
    assert_eq!(wire["notes"][0]["statusOwner"], true);
    assert_eq!(wire["notes"][1]["statusOwner"], false);
    assert_eq!(wire["notes"][0]["nonHolder"], true);
    assert_eq!(
        wire["notes"][0]["refs"],
        json!(["report/path", "<script>do_not_run()</script>"])
    );
    assert_eq!(wire["notes"][0]["bodyOmitted"], false);
    assert_eq!(wire["notes"][0]["summaryTruncated"], false);
    for key in ["statusOwner", "nonHolder", "refs"] {
        assert!(wire["notes"][2][key].is_null());
    }
}

fn fixture() -> (AppState, String, String, PathBuf) {
    let state = test_app_state();
    let root = state
        .persistence_path
        .parent()
        .unwrap()
        .join("work-project");
    fs::create_dir_all(&root).unwrap();
    let project = create_test_project(&state, &root, "Work fixture");
    let session = create_test_project_session(&state, Agent::Codex, &project, &root);
    (state, project, session, root)
}

fn install_binding(state: &AppState, project: &str, session: &str, root: &FsPath) {
    fs::write(root.join(".engram-project"), "established-project\n").unwrap();
    // A metadata-only stand-in: no test opens this as a SQLite database.
    let database = work_database_path(root, "established-project");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    fs::write(&database, "established fixture store").unwrap();
    let binary = FsPath::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/tests/fixtures")
        .join(if cfg!(windows) {
            "work-reader.ps1"
        } else {
            "work-reader.sh"
        });
    let mut inner = state.inner.lock().unwrap();
    inner
        .projects
        .iter_mut()
        .find(|p| p.id == project)
        .unwrap()
        .engram = Some(EngramProjectSettings {
        enabled: true,
        binary_path: Some(binary.to_string_lossy().into_owned()),
        home: Some(root.to_string_lossy().into_owned()),
        authority_store_key: Some(EngramAuthorityStoreKey {
            database_path: normalize_user_facing_path(&fs::canonicalize(database).unwrap()),
            project_id: "established-project".into(),
        }),
        ..Default::default()
    });
    inner.engram_declared_project_ids.insert(project.into());
    let descriptor = engram_mcp_runtime_config_for_session_locked(&inner, session)
        .unwrap()
        .installed;
    let index = inner.find_session_index(session).unwrap();
    inner
        .session_mut_by_index(index)
        .unwrap()
        .engram_mcp_installed = Some(descriptor);
}

#[test]
fn work_detection_never_creates_store_or_binding_or_enables_engram() {
    let (state, project, session, root) = fixture();
    let empty = state
        .list_project_work(&project, WorkListQuery::default())
        .unwrap();
    assert!(empty.page.is_none());
    assert!(
        empty
            .sources
            .iter()
            .any(|s| s.source == "engram" && s.state == "absent")
    );
    assert!(!root.join("engram.db").exists());
    assert!(
        state
            .work_read_snapshot(&project, None)
            .unwrap()
            .1
            .is_none()
    );
    install_binding(&state, &project, &session, &root);
    let database = work_database_path(&root, "established-project");
    fs::remove_file(&database).unwrap();
    let missing = state
        .list_project_work(&project, WorkListQuery::default())
        .unwrap();
    assert!(missing.page.is_none());
    assert!(!database.exists());
    assert!(
        !root.join("work-read-args.txt").exists(),
        "must not spawn a detection probe"
    );
    fs::write(&database, "fixture").unwrap();
    fs::write(root.join(".engram-project"), "a-different-project").unwrap();
    assert!(
        state
            .list_project_work(&project, WorkListQuery::default())
            .unwrap()
            .page
            .is_none()
    );
    assert!(!root.join("work-read-args.txt").exists());
}

#[test]
fn work_reader_uses_established_identity_and_rejects_disabled_or_changed_binding() {
    let (state, project, session, root) = fixture();
    install_binding(&state, &project, &session, &root);
    let target = state.work_read_snapshot(&project, None).unwrap().1.unwrap();
    assert_eq!(target.connection.session_id, session);
    validate_work_read_target(&target).unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        inner
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .unwrap()
            .engram
            .as_mut()
            .unwrap()
            .enabled = false;
    }
    assert!(
        state
            .validate_work_read_still_current(&project, &target)
            .is_err()
    );
    let disabled = state
        .list_project_work(&project, WorkListQuery::default())
        .unwrap();
    assert!(disabled.sources.iter().any(|s| s.state == "disabled"));
    assert!(!root.join("work-read-args.txt").exists());
}

#[test]
fn work_list_process_returns_real_json_and_surfaces_stale_and_malformed_failures() {
    let (state, project, session, root) = fixture();
    install_binding(&state, &project, &session, &root);
    let result = state
        .list_project_work(
            &project,
            WorkListQuery {
                search: Some("quoted ' & data".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(result.page.unwrap().items.len(), 1);
    assert_eq!(result.reader_session_id.as_deref(), Some(session.as_str()));
    let argv = fs::read_to_string(root.join("work-read-args.txt")).unwrap();
    assert!(argv.lines().any(|line| line == "--search=quoted ' & data"));
    assert!(argv.contains(&session));
    for (filter, status) in [
        ("stale", StatusCode::CONFLICT),
        ("malformed", StatusCode::BAD_GATEWAY),
    ] {
        let error = state
            .list_project_work(
                &project,
                WorkListQuery {
                    search: Some(filter.into()),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert_eq!(error.status, status);
        assert!(error.message.contains("engram work ls"));
    }
}

#[test]
fn work_detail_process_passes_exact_arguments_and_rejects_stale_reader_or_cursor() {
    let (state, project, session, root) = fixture();
    install_binding(&state, &project, &session, &root);
    for after in [None, Some("older")] {
        let result = state
            .read_project_work_detail(
                &project,
                "w-test",
                WorkDetailQuery {
                    reader_session_id: session.clone(),
                    after: after.map(str::to_owned),
                },
            )
            .unwrap();
        assert_eq!(result.status.is_some(), after.is_none());
        let argv = fs::read_to_string(root.join("work-read-args.txt")).unwrap();
        let args = argv.lines().collect::<Vec<_>>();
        let start = args.iter().position(|arg| *arg == "show").unwrap();
        let mut expected = vec!["show", "w-test", "--notes", "--gates", "--json"];
        if after.is_some() {
            expected.push("--after=older");
        }
        assert_eq!(&args[start..], expected);
    }
    let stale = state
        .read_project_work_detail(
            &project,
            "w-test",
            WorkDetailQuery {
                reader_session_id: session.clone(),
                after: Some("stale".into()),
            },
        )
        .unwrap_err();
    assert_eq!(stale.status, StatusCode::CONFLICT);
    assert!(stale.message.contains("work_show_cursor_invalid"));
    for reference in [
        "".to_owned(),
        "-flag".to_owned(),
        "bad\nref".to_owned(),
        "x".repeat(257),
    ] {
        assert_eq!(
            state
                .read_project_work_detail(
                    &project,
                    &reference,
                    WorkDetailQuery {
                        reader_session_id: session.clone(),
                        after: None,
                    }
                )
                .unwrap_err()
                .status,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        state
            .read_project_work_detail(
                &project,
                "w-test",
                WorkDetailQuery {
                    reader_session_id: "missing-reader".into(),
                    after: Some("older".into()),
                }
            )
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        state
            .list_project_work(
                &project,
                WorkListQuery {
                    reader_session_id: Some("missing-reader".into()),
                    after: Some("older".into()),
                    ..Default::default()
                }
            )
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
}
