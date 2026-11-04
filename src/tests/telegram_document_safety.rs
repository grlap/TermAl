// Document API safety regressions, split from telegram_documents.rs.
// Owns deterministic open races, local-session admission and response wire
// checks. Uses its parent module's fixtures/fake sender; never sends to Telegram.

fn refused_after_replacement(replace: impl FnOnce(&FsPath, &FsPath)) {
    let fixture = connected_telegram_document_fixture();
    let outside = TestTempRoot::create("telegram-doc-race-outside");
    write_test_file(&outside.path().join("note.md"), b"# private outside\n");
    write_test_file(&fixture.workdir.join("part/note.md"), b"# original\n");
    let fake = FakeTelegramDocumentSender::succeeding(1);
    let mut connected = false;
    let result = fixture
        .state
        .send_telegram_markdown_document_with_admission(
            &fixture.session_id,
            SendTelegramDocumentRequest {
                path: "part/note.md".to_owned(),
                caption: None,
            },
            |_| {
                connected = true;
                Ok(&fake)
            },
            |requested, base, roots| {
                admit_telegram_markdown_document_before_open(requested, base, roots, || {
                    replace(&fixture.workdir, outside.path())
                })
            },
        );
    let error = result.expect_err("replacement must be refused");
    assert!(matches!(
        error.code(),
        "path_outside_allowed_roots" | "not_markdown"
    ));
    assert!(!connected, "a refused file must not construct the sender");
    assert!(fake.sent.borrow().is_empty());
}

#[test]
fn telegram_document_leaf_replacement_cannot_redirect_the_upload() {
    refused_after_replacement(|root, outside| {
        fs::rename(root.join("part/note.md"), root.join("part/original.md")).unwrap();
        create_test_file_symlink(&outside.join("note.md"), &root.join("part/note.md"));
    });
}

#[test]
fn telegram_document_ancestor_replacement_cannot_redirect_the_upload() {
    refused_after_replacement(|root, outside| {
        fs::rename(root.join("part"), root.join("original-part")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside, root.join("part")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(outside, root.join("part")).unwrap();
    });
}

#[cfg(unix)]
#[test]
fn telegram_document_kernel_path_is_not_resolved_again_after_observation() {
    use std::cell::Cell;
    let fixture = connected_telegram_document_fixture();
    let temp = TestTempRoot::create("telegram-doc-observed-race");
    let outside = temp.path().join("outside");
    let held = temp.path().join("held");
    let part = fixture.workdir.join("part");
    let original = fixture.workdir.join("original-part");
    write_test_file(&part.join("note.md"), b"# allowed\n");
    write_test_file(&outside.join("note.md"), b"# outside secret\n");
    let expected = canonical_root(&part.join("note.md"));
    let outside_expected = canonical_root(&outside.join("note.md"));
    let fake = FakeTelegramDocumentSender::succeeding(1);
    let mut connected = false;
    let observations = Cell::new(0);
    let result = fixture
        .state
        .send_telegram_markdown_document_with_admission(
            &fixture.session_id,
            SendTelegramDocumentRequest {
                path: "part/note.md".to_owned(),
                caption: None,
            },
            |_| {
                connected = true;
                Ok(&fake)
            },
            |requested, base, roots| {
                admit_telegram_markdown_document_observed(
                    requested,
                    base,
                    roots,
                    || {
                        fs::rename(&part, &original).unwrap();
                        std::os::unix::fs::symlink(&outside, &part).unwrap();
                    },
                    |observed| {
                        // The handle now supplies outside bytes. Rewrite the observed
                        // pathname so a SECOND lookup would instead see the allowed file.
                        assert_eq!(normalize_user_facing_path(observed), outside_expected);
                        observations.set(observations.get() + 1);
                        fs::rename(&outside, &held).unwrap();
                        fs::remove_file(&part).unwrap(); // remove the link, not its target
                        fs::rename(&original, &part).unwrap();
                        std::os::unix::fs::symlink(&part, &outside).unwrap();
                        assert_eq!(
                            canonical_root(observed),
                            expected,
                            "the former secondary canonicalize would accept this name"
                        );
                    },
                )
            },
        );
    assert_eq!(result.unwrap_err().code(), "path_outside_allowed_roots");
    assert_eq!(observations.get(), 1);
    assert!(!connected);
    assert!(fake.sent.borrow().is_empty());
}

#[cfg(unix)]
#[test]
fn telegram_document_fifo_replacement_is_refused_without_a_writer() {
    use std::os::unix::ffi::OsStrExt;
    refused_after_replacement(|root, _outside| {
        let path = root.join("part/note.md");
        fs::rename(&path, root.join("part/original.md")).unwrap();
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: path is NUL-terminated and retained for this synchronous call.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    });
}

#[test]
fn telegram_document_refuses_remote_project_unmapped_and_partial_proxy_before_files() {
    for mode in ["project", "proxy", "remote-only", "session-only"] {
        let fixture = connected_telegram_document_fixture();
        write_test_file(&fixture.workdir.join("note.md"), b"# existing local file\n");
        {
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.session_id).unwrap();
            let project_id = inner.sessions[index].session.project_id.clone().unwrap();
            if mode == "project" {
                let project = inner
                    .projects
                    .iter_mut()
                    .find(|project| project.id == project_id)
                    .unwrap();
                project.remote_id = "remote-host".to_owned();
            } else {
                let record = inner.session_mut_by_index(index).unwrap();
                record.session.project_id = None;
                record.remote_id = (mode != "session-only").then(|| "remote-host".to_owned());
                record.remote_session_id =
                    (mode != "remote-only").then(|| "remote-session".to_owned());
            }
        }
        let fake = FakeTelegramDocumentSender::succeeding(1);
        let (result, connected) =
            send_with_fake(&fixture.state, &fixture.session_id, "note.md", None, &fake);
        let error = result.expect_err(mode);
        assert_eq!(error.code(), "session_refused", "{mode}");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert!(connected.is_none());
        assert!(fake.sent.borrow().is_empty());
    }
}

#[test]
fn telegram_document_refuses_hidden_and_unresolvable_sessions_before_connecting() {
    for hidden in [true, false] {
        let fixture = connected_telegram_document_fixture();
        {
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.session_id).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            if hidden {
                record.hidden = true;
            } else {
                record.session.workdir = fixture
                    .workdir
                    .join("absent")
                    .to_string_lossy()
                    .into_owned();
            }
        }
        let fake = FakeTelegramDocumentSender::succeeding(1);
        let (result, connected) =
            send_with_fake(&fixture.state, &fixture.session_id, "note.md", None, &fake);
        let error = result.expect_err("session must be refused");
        assert_eq!(
            error.code(),
            if hidden {
                "session_not_found"
            } else {
                "session_refused"
            }
        );
        assert_eq!(
            error.status(),
            if hidden {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::CONFLICT
            }
        );
        assert!(connected.is_none());
        assert!(fake.sent.borrow().is_empty());
    }
}

#[test]
fn telegram_document_connect_failure_is_redacted_and_sends_nothing() {
    let fixture = connected_telegram_document_fixture();
    write_test_file(&fixture.workdir.join("note.md"), b"# note\n");
    let mut connection_attempts = 0;
    let result = fixture
        .state
        .send_telegram_markdown_document::<&FakeTelegramDocumentSender>(
            &fixture.session_id,
            SendTelegramDocumentRequest {
                path: "note.md".to_owned(),
                caption: None,
            },
            |_| {
                connection_attempts += 1;
                Err(anyhow!(
                    "https://api.telegram.org/bot{TEST_BOT_TOKEN}/sendDocument"
                ))
            },
        );
    let error = result.expect_err("client construction must fail");
    assert_eq!(error.code(), "internal_error");
    assert!(!error.message().contains(TEST_BOT_TOKEN));
    assert_eq!(connection_attempts, 1);
}

#[test]
fn telegram_document_caption_counts_utf16_units() {
    let fixture = connected_telegram_document_fixture();
    write_test_file(&fixture.workdir.join("note.md"), b"# note\n");
    let fake = FakeTelegramDocumentSender::succeeding(1);
    let (result, connected) = send_with_fake(
        &fixture.state,
        &fixture.session_id,
        "note.md",
        Some(&"😀".repeat(513)),
        &fake,
    );
    assert_eq!(result.unwrap_err().code(), "invalid_request");
    assert!(connected.is_none());
    assert!(fake.sent.borrow().is_empty());
    let (result, _) = send_with_fake(
        &fixture.state,
        &fixture.session_id,
        "note.md",
        Some(&"😀".repeat(512)),
        &fake,
    );
    assert!(result.is_ok());
    assert_eq!(fake.sent.borrow().len(), 1);
}

#[test]
fn telegram_document_bad_path_is_a_typed_caller_refusal() {
    let temp = TestTempRoot::create("telegram-doc-bad-path");
    write_test_file(&temp.path().join("note.md"), b"# note\n");
    assert_eq!(
        admit_in(temp.path(), "note.md/child.md")
            .unwrap_err()
            .code(),
        "file_not_found"
    );
    #[cfg(windows)]
    assert_eq!(
        admit_in(temp.path(), "a<b.md").unwrap_err().code(),
        "invalid_request"
    );
}

#[cfg(windows)]
#[test]
fn telegram_document_rejects_network_and_device_names_before_io() {
    let temp = TestTempRoot::create("telegram-doc-namespace");
    for path in [
        r"\\unreachable.invalid\share\note.md",
        r"\\?\UNC\unreachable.invalid\share\note.md",
        r"\\.\pipe\note.md",
        r"\\?\GLOBALROOT\Device\note.md",
    ] {
        assert_eq!(
            admit_in(temp.path(), path).unwrap_err().code(),
            "path_outside_allowed_roots"
        );
    }
    assert_eq!(
        admit_in(temp.path(), "C:note.md").unwrap_err().code(),
        "invalid_request"
    );
}

impl TelegramDocumentSender for FakeTelegramDocumentSender {
    fn send_document(&self, chat: i64, upload: &TelegramDocumentUpload<'_>) -> Result<i64> {
        <&FakeTelegramDocumentSender as TelegramDocumentSender>::send_document(&self, chat, upload)
    }
}

#[tokio::test]
async fn telegram_document_real_handler_preserves_success_and_extra_error_wire_keys() {
    let fixture = connected_telegram_document_fixture();
    write_test_file(&fixture.workdir.join("note.md"), b"# note\n");
    write_test_file(
        &fixture.workdir.join("large.md"),
        &vec![b'x'; TELEGRAM_DOCUMENT_MAX_BYTES as usize + 1],
    );
    for api_error in [false, true] {
        let app = Router::new()
            .route(
                "/api/sessions/{id}/telegram/documents",
                post(move |path, state, request| {
                    send_session_telegram_document_with_sender(path, state, request, move |_| {
                        Ok(if api_error {
                            FakeTelegramDocumentSender::new(FakeTelegramDocumentOutcome::ApiError {
                                status: StatusCode::BAD_REQUEST,
                                error_code: Some(400),
                                description: "fake refusal",
                            })
                        } else {
                            FakeTelegramDocumentSender::succeeding(77)
                        })
                    })
                }),
            )
            .with_state(fixture.state.clone());
        let uri = format!("/api/sessions/{}/telegram/documents", fixture.session_id);
        for file in ["note.md", "large.md"] {
            let (status, body): (StatusCode, Value) = request_json(
                &app,
                Request::post(&uri)
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"path": file}).to_string()))
                    .unwrap(),
            )
            .await;
            if file == "large.md" {
                assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
                assert_eq!(body["sizeBytes"], TELEGRAM_DOCUMENT_MAX_BYTES + 1);
                assert_eq!(body["maxBytes"], TELEGRAM_DOCUMENT_MAX_BYTES);
                assert_eq!(body["code"], "file_too_large");
            } else if api_error {
                assert_eq!(status, StatusCode::BAD_GATEWAY);
                assert_eq!(body["code"], "telegram_api_error");
                assert_eq!(body["telegramErrorCode"], 400);
            } else {
                assert_eq!(status, StatusCode::OK);
                assert_eq!(body["messageId"], 77);
                assert_eq!(body["fileName"], "note.md");
                assert_eq!(body["sizeBytes"], 7);
                assert!(body["path"].as_str().is_some());
                assert_eq!(body.as_object().unwrap().len(), 4);
            }
            assert!(!body.to_string().contains(TEST_BOT_TOKEN));
        }
    }
}
