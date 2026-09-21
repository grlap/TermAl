// Readiness admission, diagnostic route separation and disposable real-binary
// integration. New tests alongside engram_host_adapter; no live-store access.
use super::*;

fn ready_receipt(root: &FsPath) -> Value {
    fs::write(root.join(".engram-project"), "readiness-test\n").unwrap();
    let database = work_database_path(root, "readiness-test");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    fs::write(&database, "fixture").unwrap();
    serde_json::json!({
        "schema_version": 1, "scope": "readiness", "ready": true,
        "full_audit": "not_run", "mutation_enabled": false, "work_schema_version": 1,
        "project_id": "readiness-test", "database": fs::canonicalize(database).unwrap(),
        "host_path_policy": {"stored":"case_sensitive", "resolved":"case_sensitive", "status":"matched"},
        "control": {"required_assurance":"turn_gated"}
    })
}

#[test]
fn readiness_admission_rejects_unsupported_envelopes_paths_and_identity() {
    let root = TestTempRoot::create("readiness-admission");
    let valid = ready_receipt(root.path());
    let admit = |value: Value, premium| -> Result<EngramAuthorityStoreKey, String> {
        let receipt: EngramReadinessReceipt =
            serde_json::from_value(value).map_err(|e| e.to_string())?;
        validate_engram_readiness(
            &receipt,
            &root.path().join(".engram-project"),
            root.path(),
            premium,
        )
        .map_err(|e| e.message)
    };
    assert!(admit(valid.clone(), true).is_ok());
    assert!(
        admit(valid.clone(), false).is_ok(),
        "base access is unmediated, not control assurance"
    );
    for (pointer, replacement) in [
        ("/schema_version", json!(2)),
        ("/scope", json!("doctor")),
        ("/ready", json!(false)),
        ("/full_audit", json!("passed")),
        ("/mutation_enabled", json!(true)),
        ("/work_schema_version", json!(0)),
        ("/host_path_policy/status", json!("unresolved")),
        ("/host_path_policy/status", json!("unbound")),
        ("/host_path_policy/resolved", Value::Null),
        ("/host_path_policy/resolved", json!("different")),
        ("/project_id", json!("another-project")),
        ("/database", json!("relative.db")),
        ("/database", json!(root.path().join("wrong.db"))),
        ("/control/required_assurance", json!("action_gated")),
        ("/control/required_assurance", json!("unknown")),
    ] {
        let mut invalid = valid.clone();
        *invalid.pointer_mut(pointer).unwrap() = replacement;
        assert!(admit(invalid, true).is_err(), "{pointer} must fail closed");
    }
    let mut advisory = valid.clone();
    advisory["control"]["required_assurance"] = json!("advisory");
    assert!(admit(advisory.clone(), false).is_ok());
    assert!(admit(advisory, true).is_err());
    for field in [
        "scope",
        "full_audit",
        "control",
        "host_path_policy",
        "database",
        "work_schema_version",
    ] {
        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(admit(missing, true).is_err(), "missing {field}");
    }
}

fn fixture_project(state: &AppState) -> (String, PathBuf) {
    let root = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("readiness-project");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join(".engram-project"), "fixture-ready\n").unwrap();
    let database = work_database_path(&root, "fixture-ready");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    fs::write(database, "fixture database").unwrap();
    let id = create_test_project(state, &root, "Readiness");
    let mut inner = state.inner.lock().unwrap();
    inner.preferences.engram.binary_path =
        super::engram_host_adapter::real_engram_control_fixture_path()
            .to_string_lossy()
            .into_owned();
    inner.preferences.engram.home = root.to_string_lossy().into_owned();
    (id, root)
}

#[test]
fn diagnostic_previews_bound_bytes_without_splitting_utf8() {
    assert_eq!(
        engram_diagnostic_preview(b"exact", 5),
        ("exact".into(), false)
    );
    assert_eq!(
        engram_diagnostic_preview("abc💡tail".as_bytes(), 5),
        ("abc".into(), true)
    );
    let (preview, truncated) = engram_diagnostic_preview(&[0xff; 100], 10);
    assert!(truncated);
    assert!(preview.len() <= 10);
    let large = "💡".repeat(5000);
    let excerpt = engram_diagnostic_excerpt(large.as_bytes());
    assert!(excerpt.len() <= ENGRAM_DIAGNOSTIC_TEXT_LIMIT);
    assert!(excerpt.ends_with("[truncated]"));
    assert!(!excerpt.contains('\u{fffd}'));
    assert_eq!(engram_diagnostic_excerpt(b"small warning"), "small warning");
}

#[test]
fn readiness_large_outputs_are_bounded_refusals_without_fallback() {
    for mode in [
        "fixture-diagnostic-large-refusal",
        "fixture-diagnostic-large-report",
    ] {
        let state = test_app_state();
        let (id, root) = fixture_project(&state);
        fs::write(root.join(".engram-project"), mode).unwrap();
        let draft = || UpdateProjectEngramSettingsRequest {
            enabled: true,
            turn_gated_control: true,
            acceptance_evaluation: None,
            binary_path: None,
            home: None,
            deadline_ms: None,
        };
        for error in [
            state
                .verify_project_engram_settings(&id, draft())
                .err()
                .expect("verify refuses"),
            state
                .patch_project_engram_settings(&id, draft())
                .err()
                .expect("save refuses"),
        ] {
            assert!(error.message.len() < 2 * ENGRAM_DIAGNOSTIC_TEXT_LIMIT + 256);
            assert!(error.message.contains("no Full Audit fallback"));
            assert!(error.message.contains(if mode.ends_with("refusal") {
                "[truncated]"
            } else {
                "16384-byte admission limit"
            }));
        }
        assert_eq!(
            fs::read_to_string(root.join("diagnostic-commands")).unwrap(),
            "readiness\nreadiness\n"
        );
        assert!(
            state
                .inner
                .lock()
                .unwrap()
                .find_project(&id)
                .unwrap()
                .engram
                .is_none()
        );
    }
}

#[tokio::test]
async fn full_audit_large_payloads_preserve_identity_with_bounded_previews() {
    for malformed in [false, true] {
        let state = test_app_state();
        let (id, root) = fixture_project(&state);
        let mode = if malformed {
            "fixture-diagnostic-large-refusal"
        } else {
            "fixture-diagnostic-large-report"
        };
        fs::write(root.join(".engram-project"), mode).unwrap();
        let database = work_database_path(&root, mode);
        fs::create_dir_all(database.parent().unwrap()).unwrap();
        fs::write(&database, "fixture database").unwrap();
        // Keep the test-owned temporary store alive through the assertions.
        let response = app_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{id}/engram/audit"))
                    .header("sec-fetch-site", "same-origin")
                    .header("x-termal-operator-action", "engram-full-audit")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 128 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        if malformed {
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(bytes.len() < 2 * ENGRAM_DIAGNOSTIC_TEXT_LIMIT);
            let body_text = String::from_utf8(bytes.to_vec()).unwrap();
            assert!(body_text.contains("Full Audit returned invalid JSON"));
            assert!(body_text.contains("[truncated]"));
        } else {
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["healthy"], true);
            assert_eq!(body["projectId"], mode);
            assert_eq!(
                body["database"],
                normalize_user_facing_path(&fs::canonicalize(database).unwrap())
                    .to_string_lossy()
                    .as_ref()
            );
            assert_eq!(body["reportTruncated"], true);
            assert!(
                body.get("report").is_none(),
                "raw report object must not cross API"
            );
            assert!(
                body["reportPreview"].as_str().unwrap().len() <= ENGRAM_DIAGNOSTIC_REPORT_LIMIT
            );
            assert!(body["warnings"].as_str().unwrap().len() <= ENGRAM_DIAGNOSTIC_TEXT_LIMIT);
            assert!(body["warnings"].as_str().unwrap().ends_with("[truncated]"));
            assert!(
                bytes.len() < 24 * 1024,
                "ASCII fixture response stays bounded"
            );
        }
    }
}

#[test]
fn diagnostic_declaration_reader_bounds_contents_independently_of_metadata() {
    let limit = ENGRAM_DIAGNOSTIC_DECLARATION_LIMIT;
    let valid = "x".repeat(limit);
    assert_eq!(
        read_engram_diagnostic_declaration_contents(valid.as_bytes()).unwrap(),
        valid
    );
    // Model contents that grew after metadata validation. Even a longer reader
    // must stop after the single oversize sentinel byte, not consume to EOF.
    let mut grown = io::Cursor::new(vec![b'x'; limit * 2]);
    let error = read_engram_diagnostic_declaration_contents(&mut grown).unwrap_err();
    assert!(error.message.contains("at most 4096 bytes"));
    assert_eq!(grown.position(), (limit + 1) as u64);
}

#[test]
fn diagnostic_declaration_reader_rejects_invalid_files_and_accepts_size_boundary() {
    let root = TestTempRoot::create("diagnostic-declaration");
    let marker = root.path().join(".engram-project");
    assert!(read_engram_diagnostic_declaration(root.path()).is_err());
    assert!(read_engram_diagnostic_declaration(&marker).is_err());
    for bytes in [b"".as_slice(), b" \n\t", &[0xff], &vec![b'x'; 4097]] {
        fs::write(&marker, bytes).unwrap();
        assert!(read_engram_diagnostic_declaration(&marker).is_err());
    }
    let valid = format!("{}\n", "x".repeat(4095));
    fs::write(&marker, &valid).unwrap();
    assert_eq!(read_engram_diagnostic_declaration(&marker).unwrap(), valid);
}

#[test]
fn diagnostic_declaration_invalid_preflight_never_launches_readiness_or_audit() {
    for directory in [false, true] {
        let state = test_app_state();
        let (id, root) = fixture_project(&state);
        let marker = root.join(".engram-project");
        if directory {
            fs::remove_file(&marker).unwrap();
            fs::create_dir(&marker).unwrap();
        } else {
            // A large file is rejected by metadata without allocating its size.
            fs::File::create(&marker)
                .unwrap()
                .set_len(16 * 1024 * 1024)
                .unwrap();
        }
        let binary = super::engram_host_adapter::real_engram_control_fixture_path();
        assert!(run_engram_readiness(&binary, &marker, &root, &root).is_err());
        let draft = || UpdateProjectEngramSettingsRequest {
            enabled: true,
            turn_gated_control: true,
            acceptance_evaluation: None,
            binary_path: None,
            home: None,
            deadline_ms: None,
        };
        for error in [
            state
                .verify_project_engram_settings(&id, draft())
                .err()
                .expect("verify refuses"),
            state
                .patch_project_engram_settings(&id, draft())
                .err()
                .expect("save refuses"),
            state
                .full_audit_project_engram(&id)
                .err()
                .expect("audit refuses"),
        ] {
            if !directory {
                assert!(
                    error.message.contains("at most 4096 bytes"),
                    "{}",
                    error.message
                );
            }
        }
        assert!(
            !root.join("diagnostic-commands").exists(),
            "no process must start"
        );
        assert!(
            state
                .inner
                .lock()
                .unwrap()
                .find_project(&id)
                .unwrap()
                .engram
                .is_none()
        );
    }
}

#[test]
fn diagnostic_declaration_growth_during_process_fails_closed() {
    for audit in [false, true] {
        let state = test_app_state();
        let (id, root) = fixture_project(&state);
        let mode = "fixture-diagnostic-marker-oversized";
        let marker = root.join(".engram-project");
        fs::write(&marker, mode).unwrap();
        let database = work_database_path(&root, mode);
        fs::create_dir_all(database.parent().unwrap()).unwrap();
        fs::write(database, "fixture database").unwrap();
        let error = if audit {
            state
                .full_audit_project_engram(&id)
                .err()
                .expect("audit refuses changed marker")
        } else {
            let binary = super::engram_host_adapter::real_engram_control_fixture_path();
            run_engram_readiness(&binary, &marker, &root, &root)
                .err()
                .expect("readiness refuses changed marker")
        };
        let expected = if audit {
            "at most 4096 bytes"
        } else {
            "declaration changed during readiness"
        };
        assert!(error.message.contains(expected), "{}", error.message);
        assert_eq!(fs::metadata(marker).unwrap().len(), 4097);
        assert_eq!(
            fs::read_to_string(root.join("diagnostic-commands")).unwrap(),
            if audit { "doctor\n" } else { "readiness\n" }
        );
    }
}

#[tokio::test]
async fn readiness_verify_save_and_explicit_audit_routes_are_separate() {
    let state = test_app_state();
    let (id, root) = fixture_project(&state);
    let app = app_router(state.clone());
    for (path, method) in [("verify", "POST"), ("", "PATCH")] {
        let uri = format!(
            "/api/projects/{id}/engram{}",
            if path.is_empty() {
                String::new()
            } else {
                format!("/{path}")
            }
        );
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"enabled":true,"turnGatedControl":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(status, StatusCode::OK, "{body}");
        if path == "verify" {
            assert_eq!(body["fullAudit"], "not_run");
            assert_eq!(body["ready"], true);
            assert!(body.get("healthy").is_none());
            assert!(
                state
                    .inner
                    .lock()
                    .unwrap()
                    .find_project(&id)
                    .unwrap()
                    .engram
                    .is_none()
            );
        }
    }
    assert_eq!(
        fs::read_to_string(root.join("diagnostic-commands")).unwrap(),
        "readiness\nreadiness\n"
    );
    let denied = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{id}/engram/audit"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{id}/engram/audit"))
                .header("sec-fetch-site", "same-origin")
                .header("x-termal-operator-action", "engram-full-audit")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let audit: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(audit["healthy"], true);
    assert_eq!(
        fs::read_to_string(root.join("diagnostic-commands")).unwrap(),
        "readiness\nreadiness\ndoctor\n"
    );
}

#[test]
fn readiness_rejects_stale_settings_snapshot() {
    let state = test_app_state();
    let (id, _) = fixture_project(&state);
    let (project, host) = {
        let inner = state.inner.lock().unwrap();
        (
            inner.find_project(&id).unwrap().clone(),
            inner.preferences.engram.clone(),
        )
    };
    assert!(
        state
            .validate_engram_diagnostic_snapshot(&project, &host)
            .is_ok()
    );
    state
        .inner
        .lock()
        .unwrap()
        .preferences
        .engram
        .home
        .push_str("-changed");
    assert_eq!(
        state
            .validate_engram_diagnostic_snapshot(&project, &host)
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn readiness_busy_does_not_block_audit_or_disable() {
    let state = test_app_state();
    let (id, _) = fixture_project(&state);
    let app = Router::new()
        .route("/verify/{id}", post(verify_project_engram_settings))
        .route("/settings/{id}", patch(update_project_engram_settings))
        .route("/audit/{id}", post(full_audit_project_engram))
        .with_state(state)
        .layer(axum::Extension(EngramReadinessLimiter(Arc::new(
            tokio::sync::Semaphore::new(0),
        ))))
        .layer(axum::Extension(EngramAuditLimiter(Arc::new(
            tokio::sync::Semaphore::new(1),
        ))));
    for (route, method, body, status) in [
        (
            "verify",
            "POST",
            r#"{"enabled":true}"#,
            StatusCode::TOO_MANY_REQUESTS,
        ),
        (
            "settings",
            "PATCH",
            r#"{"enabled":true}"#,
            StatusCode::TOO_MANY_REQUESTS,
        ),
        ("audit", "POST", "", StatusCode::OK),
        ("settings", "PATCH", r#"{"enabled":false}"#, StatusCode::OK),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(format!("/{route}/{id}"))
                    .header("content-type", "application/json")
                    .header("sec-fetch-site", "same-origin")
                    .header("x-termal-operator-action", "engram-full-audit")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{route} {body}");
    }
}

#[test]
fn readiness_transport_timeout_is_a_refusal() {
    let root = TestTempRoot::create("readiness-timeout");
    let fixture = FsPath::new(env!("CARGO_MANIFEST_DIR")).join(if cfg!(windows) {
        "src/tests/fixtures/engram-doctor-slow-fixture.ps1"
    } else {
        "src/tests/fixtures/engram-doctor-slow-fixture.sh"
    });
    let result = run_engram_diagnostic_within(
        &fixture,
        &root.path().join(".engram-project"),
        root.path(),
        root.path(),
        "readiness",
        Duration::ZERO,
    );
    assert!(
        result
            .unwrap_err()
            .message
            .contains("Engram readiness exceeded")
    );
}

#[test]
fn full_audit_retains_an_unhealthy_report_and_stderr_disclosures() {
    let state = test_app_state();
    let (id, root) = fixture_project(&state);
    fs::write(root.join(".engram-project"), "fixture-audit-unhealthy").unwrap();
    let database = work_database_path(&root, "fixture-audit-unhealthy");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    fs::write(database, "fixture database").unwrap();
    let result = state.full_audit_project_engram(&id).unwrap();
    assert!(!result.healthy);
    assert!(!result.report_truncated);
    assert_eq!(
        serde_json::from_str::<Value>(&result.report_preview).unwrap()["healthy"],
        false
    );
    assert!(result.warnings.contains("redactor"));
    assert!(
        state
            .inner
            .lock()
            .unwrap()
            .find_project(&id)
            .unwrap()
            .engram
            .is_none()
    );
}

#[test]
fn readiness_process_failures_never_fall_back_to_doctor_or_save() {
    for mode in [
        "fixture-readiness-old",
        "fixture-readiness-malformed",
        "fixture-readiness-refusal",
    ] {
        let state = test_app_state();
        let (id, root) = fixture_project(&state);
        fs::write(root.join(".engram-project"), mode).unwrap();
        let error = state
            .patch_project_engram_settings(
                &id,
                UpdateProjectEngramSettingsRequest {
                    enabled: true,
                    turn_gated_control: true,
                    acceptance_evaluation: None,
                    binary_path: None,
                    home: None,
                    deadline_ms: None,
                },
            )
            .err()
            .expect("must refuse");
        assert!(
            error.message.contains("no Full Audit fallback"),
            "{}",
            error.message
        );
        assert_eq!(
            fs::read_to_string(root.join("diagnostic-commands")).unwrap(),
            "readiness\n"
        );
        assert!(
            state
                .inner
                .lock()
                .unwrap()
                .find_project(&id)
                .unwrap()
                .engram
                .is_none()
        );
    }
}

#[test]
#[ignore = "requires TERMAL_TEST_LIVE_ENGRAM_BINARY; creates only a disposable store"]
fn real_readiness_verify_save_audit_disposable_store() {
    let binary = PathBuf::from(
        std::env::var_os("TERMAL_TEST_LIVE_ENGRAM_BINARY").expect("candidate binary"),
    );
    assert!(binary.is_absolute());
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("real-readiness");
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    let marker = root.join(".engram-project");
    fs::write(&marker, "termal-readiness-disposable\n").unwrap();
    let init = Command::new(&binary)
        .arg("--project-file")
        .arg(&marker)
        .arg("--home")
        .arg(&home)
        .args([
            "init",
            "--required-assurance",
            "turn_gated",
            "--authorized-by",
            "termal-test",
        ])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let id = create_test_project(&state, &root, "Disposable readiness integration");
    {
        let mut inner = state.inner.lock().unwrap();
        inner.preferences.engram.binary_path = binary.to_string_lossy().into_owned();
        inner.preferences.engram.home = home.to_string_lossy().into_owned();
    }
    let draft = || UpdateProjectEngramSettingsRequest {
        enabled: true,
        turn_gated_control: true,
        acceptance_evaluation: None,
        binary_path: None,
        home: None,
        deadline_ms: None,
    };
    let database = work_database_path(&home, "termal-readiness-disposable");
    let before = fs::read(&database).unwrap();
    let verify = state.verify_project_engram_settings(&id, draft()).unwrap();
    assert!(verify.verified);
    assert_eq!(verify.full_audit, "not_run");
    assert_eq!(fs::read(&database).unwrap(), before);
    let started = std::time::Instant::now();
    state.patch_project_engram_settings(&id, draft()).unwrap();
    let save_ms = started.elapsed().as_millis();
    {
        let inner = state.inner.lock().unwrap();
        let saved = inner.find_project(&id).unwrap().engram.as_ref().unwrap();
        assert!(saved.enabled && saved.turn_gated_control);
        assert_eq!(
            saved.authority_store_key.as_ref().unwrap().project_id,
            "termal-readiness-disposable"
        );
    }
    let audit = state.full_audit_project_engram(&id).unwrap();
    assert!(audit.healthy);
    assert!(!audit.report_truncated);
    assert!(
        serde_json::from_str::<Value>(&audit.report_preview)
            .unwrap()
            .get("healthy")
            .is_some()
    );
    let disk = load_state(&state.persistence_path).unwrap().unwrap();
    let persisted = disk
        .projects
        .iter()
        .find(|p| p.id == id)
        .unwrap()
        .engram
        .as_ref()
        .unwrap();
    assert!(
        persisted.enabled && persisted.turn_gated_control,
        "saved settings survive reloading the disposable TermAl database"
    );
    println!(
        "DISPOSABLE evidence: verify={}ms save={}ms audit={}ms enabled=true turnGatedControl=true; not a live-host acceptance",
        verify.elapsed_ms, save_ms, audit.elapsed_ms
    );
}
