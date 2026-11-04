// Telegram Markdown document API tests. This module owns coverage for
// `POST /api/sessions/{id}/telegram/documents`: file admission (traversal,
// roots, symlinks, type, size), the multipart body, Telegram link refusals,
// session refusals, Telegram API error mapping with token redaction, and the
// success path through a fake `TelegramDocumentSender`.
//
// It deliberately does not own relay, forwarding, or settings coverage; those
// stay in `telegram.rs`, `telegram_forwarding.rs`, and `telegram_settings.rs`.
// No test here reaches Telegram: the route's real client refuses under
// `cfg(test)`, and every send goes through the fake below.

use super::delegation_support::test_app_state_with_drained_delegation_codex_runtime;
use super::telegram_support::{
    create_telegram_settings_project_and_session, install_telegram_settings_fixture,
};
use super::*;
use std::cell::RefCell;

const TEST_BOT_TOKEN: &str = "987654:AAH-document-test-secret-token-0123456789";
const TEST_CHAT_ID: i64 = 4242;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SentTelegramDocument {
    chat_id: i64,
    file_name: String,
    bytes: Vec<u8>,
    caption: Option<String>,
}

enum FakeTelegramDocumentOutcome {
    MessageId(i64),
    ApiError {
        status: StatusCode,
        error_code: Option<i64>,
        description: &'static str,
    },
    TransportError(String),
}

struct FakeTelegramDocumentSender {
    outcome: FakeTelegramDocumentOutcome,
    sent: RefCell<Vec<SentTelegramDocument>>,
}

impl FakeTelegramDocumentSender {
    fn new(outcome: FakeTelegramDocumentOutcome) -> Self {
        Self {
            outcome,
            sent: RefCell::new(Vec::new()),
        }
    }

    fn succeeding(message_id: i64) -> Self {
        Self::new(FakeTelegramDocumentOutcome::MessageId(message_id))
    }
}

impl TelegramDocumentSender for &FakeTelegramDocumentSender {
    fn send_document(&self, chat_id: i64, upload: &TelegramDocumentUpload<'_>) -> Result<i64> {
        self.sent.borrow_mut().push(SentTelegramDocument {
            chat_id,
            file_name: upload.file_name.to_owned(),
            bytes: upload.bytes.to_vec(),
            caption: upload.caption.map(str::to_owned),
        });
        match &self.outcome {
            FakeTelegramDocumentOutcome::MessageId(message_id) => Ok(*message_id),
            FakeTelegramDocumentOutcome::ApiError {
                status,
                error_code,
                description,
            } => Err(anyhow::Error::new(TelegramApiError {
                method: "sendDocument".to_owned(),
                status: *status,
                error_code: *error_code,
                description: (*description).to_owned(),
            })),
            FakeTelegramDocumentOutcome::TransportError(message) => {
                Err(anyhow!("{message}")).context("failed to call Telegram `sendDocument`")
            }
        }
    }
}

/// Sends through `fake`, recording the token the sender was built with.
fn send_with_fake(
    state: &AppState,
    session_id: &str,
    path: &str,
    caption: Option<&str>,
    fake: &FakeTelegramDocumentSender,
) -> (
    std::result::Result<SendTelegramDocumentResponse, TelegramDocumentError>,
    Option<String>,
) {
    let mut connected_with = None;
    let result = state.send_telegram_markdown_document(
        session_id,
        SendTelegramDocumentRequest {
            path: path.to_owned(),
            caption: caption.map(str::to_owned),
        },
        |token| {
            connected_with = Some(token.to_owned());
            Ok(fake)
        },
    );
    (result, connected_with)
}

struct TelegramDocumentFixture {
    state: AppState,
    session_id: String,
    workdir: PathBuf,
}

fn telegram_document_fixture(
    enabled: bool,
    token: Option<&str>,
    chat_id: Option<i64>,
) -> TelegramDocumentFixture {
    let state = test_app_state();
    let (project_id, session_id) = create_telegram_settings_project_and_session(&state);
    install_telegram_settings_fixture(
        &state,
        TelegramUiConfig {
            enabled,
            forward_assistant_replies: false,
            subscribed_project_ids: vec![project_id.clone()],
            default_project_id: Some(project_id),
            default_session_id: None,
        },
        token,
        TelegramBotState {
            chat_id,
            ..TelegramBotState::default()
        },
    );
    let workdir = session_workdir(&state, &session_id);
    TelegramDocumentFixture {
        state,
        session_id,
        workdir,
    }
}

fn connected_telegram_document_fixture() -> TelegramDocumentFixture {
    telegram_document_fixture(true, Some(TEST_BOT_TOKEN), Some(TEST_CHAT_ID))
}

fn session_workdir(state: &AppState, session_id: &str) -> PathBuf {
    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(session_id)
        .expect("fixture session should exist");
    PathBuf::from(&inner.sessions[index].session.workdir)
}

fn canonical_root(path: &FsPath) -> PathBuf {
    normalize_user_facing_path(&fs::canonicalize(path).expect("test root should resolve"))
}

fn write_test_file(path: &FsPath, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("test parent directory should create");
    }
    fs::write(path, bytes).expect("test file should write");
}

fn create_test_file_symlink(target: &FsPath, link: &FsPath) {
    #[cfg(unix)]
    let created = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let created = std::os::windows::fs::symlink_file(target, link);
    created.unwrap_or_else(|err| {
        panic!("symlink witness unavailable: {err}; this test is never skipped")
    });
}

fn admit_in(
    root: &FsPath,
    requested: &str,
) -> std::result::Result<AdmittedTelegramDocument, TelegramDocumentError> {
    let root = canonical_root(root);
    admit_telegram_markdown_document(requested, &root, std::slice::from_ref(&root))
}

// ---- file admission -------------------------------------------------------

#[test]
fn telegram_document_admits_relative_and_absolute_markdown_inside_root() {
    let temp = TestTempRoot::create("telegram-doc-admit");
    let file = temp.path().join("notes").join("Report.MD");
    write_test_file(&file, b"# Report\n\nAll green.\n");

    let relative = admit_in(temp.path(), "notes/Report.MD").expect("relative path should admit");
    assert_eq!(relative.file_name, "Report.MD");
    assert_eq!(relative.bytes, b"# Report\n\nAll green.\n");
    assert_eq!(relative.canonical_path, canonical_root(&file));

    let absolute = admit_in(temp.path(), &file.to_string_lossy())
        .expect("absolute path inside the root should admit");
    assert_eq!(absolute.canonical_path, relative.canonical_path);
}

#[test]
fn telegram_document_refuses_traversal_and_paths_outside_the_roots() {
    let temp = TestTempRoot::create("telegram-doc-outside");
    let root = temp.path().join("root");
    let outside = temp.path().join("outside").join("secret.md");
    write_test_file(&root.join("inside.md"), b"# inside\n");
    write_test_file(&outside, b"# secret\n");

    for requested in ["../outside/secret.md", "sub/../inside.md"] {
        let err = admit_in(&root, requested).expect_err("traversal should be refused");
        assert_eq!(err.code(), "path_outside_allowed_roots", "{requested}");
        assert_eq!(err.status(), StatusCode::FORBIDDEN);
    }

    let err = admit_in(&root, &outside.to_string_lossy())
        .expect_err("absolute path outside the roots should be refused");
    assert_eq!(err.code(), "path_outside_allowed_roots");
}

#[test]
fn telegram_document_judges_symlinks_by_their_canonical_target() {
    let temp = TestTempRoot::create("telegram-doc-symlink");
    let root = temp.path().join("root");
    let outside = temp.path().join("outside").join("secret.md");
    write_test_file(&outside, b"# secret\n");
    write_test_file(&root.join("plain.txt"), b"not markdown\n");

    let escape = root.join("escape.md");
    create_test_file_symlink(&outside, &escape);
    let err = admit_in(&root, "escape.md").expect_err("symlink escape should be refused");
    assert_eq!(err.code(), "path_outside_allowed_roots");

    let disguised = root.join("disguised.md");
    create_test_file_symlink(&root.join("plain.txt"), &disguised);
    let err = admit_in(&root, "disguised.md")
        .expect_err("a .md link to a non-Markdown file should be refused");
    assert_eq!(err.code(), "not_markdown");
}

#[test]
fn telegram_document_refuses_non_markdown_files() {
    let temp = TestTempRoot::create("telegram-doc-type");
    write_test_file(&temp.path().join("notes.txt"), b"plain\n");
    write_test_file(&temp.path().join("binary.md"), b"# head\0tail\n");
    write_test_file(&temp.path().join("latin1.md"), &[b'#', b' ', 0xE9, b'\n']);
    fs::create_dir_all(temp.path().join("folder.md")).expect("directory should create");

    for requested in ["notes.txt", "binary.md", "latin1.md", "folder.md"] {
        let err = admit_in(temp.path(), requested).expect_err("non-Markdown should be refused");
        assert_eq!(err.code(), "not_markdown", "{requested}");
        assert_eq!(err.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
}

#[test]
fn telegram_document_refuses_missing_empty_and_blank_requests() {
    let temp = TestTempRoot::create("telegram-doc-missing");
    write_test_file(&temp.path().join("empty.md"), b"");

    let err = admit_in(temp.path(), "absent.md").expect_err("missing file should be refused");
    assert_eq!(err.code(), "file_not_found");
    assert_eq!(err.status(), StatusCode::NOT_FOUND);

    let err = admit_in(temp.path(), "empty.md").expect_err("empty file should be refused");
    assert_eq!(err.code(), "file_empty");

    let err = admit_in(temp.path(), "   ").expect_err("blank path should be refused");
    assert_eq!(err.code(), "invalid_request");
}

#[test]
fn telegram_document_enforces_the_size_cap_at_its_boundary() {
    let temp = TestTempRoot::create("telegram-doc-size");
    let limit = TELEGRAM_DOCUMENT_MAX_BYTES as usize;
    write_test_file(&temp.path().join("at-limit.md"), &vec![b'a'; limit]);
    write_test_file(&temp.path().join("over-limit.md"), &vec![b'a'; limit + 1]);

    let admitted = admit_in(temp.path(), "at-limit.md").expect("a file at the cap should admit");
    assert_eq!(admitted.bytes.len(), limit);

    let err = admit_in(temp.path(), "over-limit.md").expect_err("over the cap should be refused");
    assert_eq!(err.code(), "file_too_large");
    assert_eq!(err.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(matches!(
        err,
        TelegramDocumentError::TooLarge { size_bytes } if size_bytes == limit as u64 + 1
    ));
}

// ---- multipart body -------------------------------------------------------

#[test]
fn telegram_document_multipart_body_carries_chat_caption_and_sanitized_file() {
    let upload = TelegramDocumentUpload {
        file_name: "we\"ird\\na\rme.md",
        bytes: b"# Title\r\n\r\nBody\n",
        caption: Some("Daily report"),
    };
    let (content_type, body) = telegram_document_multipart_body(TEST_CHAT_ID, &upload);
    let boundary = content_type
        .strip_prefix("multipart/form-data; boundary=")
        .expect("content type should carry the boundary");
    let body = String::from_utf8(body).expect("test body is UTF-8");

    let expected = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"chat_id\"\r\n\r\n4242\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"caption\"\r\n\r\nDaily report\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"document\"; filename=\"we_ird_na_me.md\"\r\n\
         Content-Type: text/markdown; charset=utf-8\r\n\r\n# Title\r\n\r\nBody\n\r\n--{boundary}--\r\n"
    );
    assert_eq!(body, expected);

    let without_caption = TelegramDocumentUpload {
        caption: None,
        ..upload
    };
    let (_, body) = telegram_document_multipart_body(TEST_CHAT_ID, &without_caption);
    assert!(
        !String::from_utf8(body)
            .unwrap()
            .contains("name=\"caption\"")
    );
}

// ---- end-to-end through AppState with a fake sender -------------------------

#[test]
fn telegram_document_sends_markdown_to_the_linked_chat() {
    let fixture = connected_telegram_document_fixture();
    write_test_file(
        &fixture.workdir.join("docs").join("report.md"),
        b"# Report\n\nAll green.\n",
    );
    let fake = FakeTelegramDocumentSender::succeeding(77);

    let (result, connected_with) = send_with_fake(
        &fixture.state,
        &fixture.session_id,
        "docs/report.md",
        Some("  Daily report  "),
        &fake,
    );
    let response = result.expect("send should succeed");

    assert_eq!(connected_with.as_deref(), Some(TEST_BOT_TOKEN));
    assert_eq!(response.message_id, 77);
    assert_eq!(response.file_name, "report.md");
    assert_eq!(response.size_bytes, 21);
    assert_eq!(
        PathBuf::from(&response.path),
        canonical_root(&fixture.workdir.join("docs").join("report.md"))
    );
    assert_eq!(
        fake.sent.borrow().as_slice(),
        &[SentTelegramDocument {
            chat_id: TEST_CHAT_ID,
            file_name: "report.md".to_owned(),
            bytes: b"# Report\n\nAll green.\n".to_vec(),
            caption: Some("Daily report".to_owned()),
        }]
    );
    let wire = serde_json::to_string(&response).expect("response should encode");
    assert!(
        !wire.contains(TEST_BOT_TOKEN),
        "the response must not carry the token"
    );
}

#[test]
fn telegram_document_allows_the_session_workdir_outside_the_project_root() {
    let fixture = connected_telegram_document_fixture();
    let worktree = TestTempRoot::create("telegram-doc-worktree");
    write_test_file(&worktree.path().join("handoff.md"), b"# Handoff\n");
    write_test_file(&fixture.workdir.join("project.md"), b"# Project\n");
    {
        let mut inner = fixture.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&fixture.session_id)
            .expect("fixture session should exist");
        inner.sessions[index].session.workdir = worktree.path().to_string_lossy().into_owned();
    }
    let fake = FakeTelegramDocumentSender::succeeding(5);

    let (result, _) = send_with_fake(
        &fixture.state,
        &fixture.session_id,
        "handoff.md",
        None,
        &fake,
    );
    assert_eq!(
        result.expect("workdir file should send").file_name,
        "handoff.md"
    );

    let project_file = fixture.workdir.join("project.md");
    let (result, _) = send_with_fake(
        &fixture.state,
        &fixture.session_id,
        &project_file.to_string_lossy(),
        None,
        &fake,
    );
    assert_eq!(
        result.expect("project-root file should send").file_name,
        "project.md"
    );
    assert_eq!(fake.sent.borrow().len(), 2);
    assert_eq!(fake.sent.borrow()[0].caption, None);
}

#[test]
fn telegram_document_refuses_when_telegram_is_not_configured_or_connected() {
    let cases = [
        (
            telegram_document_fixture(true, None, Some(TEST_CHAT_ID)),
            "telegram_not_configured",
        ),
        (
            telegram_document_fixture(false, Some(TEST_BOT_TOKEN), Some(TEST_CHAT_ID)),
            "telegram_not_connected",
        ),
        (
            telegram_document_fixture(true, Some(TEST_BOT_TOKEN), None),
            "telegram_not_connected",
        ),
    ];
    for (fixture, code) in cases {
        write_test_file(&fixture.workdir.join("note.md"), b"# note\n");
        let fake = FakeTelegramDocumentSender::succeeding(1);
        let (result, connected_with) =
            send_with_fake(&fixture.state, &fixture.session_id, "note.md", None, &fake);
        let err = result.expect_err("an unlinked Telegram must refuse");
        assert_eq!(err.code(), code);
        assert_eq!(err.status(), StatusCode::CONFLICT);
        assert_eq!(connected_with, None, "no client may be built for {code}");
        assert!(
            fake.sent.borrow().is_empty(),
            "nothing may be sent for {code}"
        );
    }
}

#[test]
fn telegram_document_refusals_before_upload_send_nothing() {
    let fixture = connected_telegram_document_fixture();
    let outside = TestTempRoot::create("telegram-doc-outside-session");
    write_test_file(&outside.path().join("secret.md"), b"# secret\n");
    write_test_file(&fixture.workdir.join("notes.txt"), b"plain\n");
    write_test_file(&fixture.workdir.join("note.md"), b"# note\n");
    let outside_path = outside
        .path()
        .join("secret.md")
        .to_string_lossy()
        .into_owned();
    let long_caption = "x".repeat(TELEGRAM_DOCUMENT_CAPTION_MAX_UTF16_UNITS + 1);

    let cases: [(&str, &str, Option<&str>, &str); 5] = [
        (
            fixture.session_id.as_str(),
            outside_path.as_str(),
            None,
            "path_outside_allowed_roots",
        ),
        (
            fixture.session_id.as_str(),
            "../escape.md",
            None,
            "path_outside_allowed_roots",
        ),
        (
            fixture.session_id.as_str(),
            "notes.txt",
            None,
            "not_markdown",
        ),
        (
            fixture.session_id.as_str(),
            "note.md",
            Some(long_caption.as_str()),
            "invalid_request",
        ),
        (
            "session-does-not-exist",
            "note.md",
            None,
            "session_not_found",
        ),
    ];
    for (session_id, path, caption, code) in cases {
        let fake = FakeTelegramDocumentSender::succeeding(1);
        let (result, connected_with) =
            send_with_fake(&fixture.state, session_id, path, caption, &fake);
        let err = result.expect_err("refusal expected");
        assert_eq!(err.code(), code, "{path}");
        assert_eq!(connected_with, None, "no client may be built for {code}");
        assert!(
            fake.sent.borrow().is_empty(),
            "nothing may be sent for {code}"
        );
    }

    let at_limit = "y".repeat(TELEGRAM_DOCUMENT_CAPTION_MAX_UTF16_UNITS);
    let fake = FakeTelegramDocumentSender::succeeding(9);
    let (result, _) = send_with_fake(
        &fixture.state,
        &fixture.session_id,
        "note.md",
        Some(&at_limit),
        &fake,
    );
    result.expect("a caption at the limit should send");
}

#[test]
fn telegram_document_maps_telegram_api_errors_without_leaking_the_token() {
    let fixture = connected_telegram_document_fixture();
    write_test_file(&fixture.workdir.join("note.md"), b"# note\n");

    let api_fake = FakeTelegramDocumentSender::new(FakeTelegramDocumentOutcome::ApiError {
        status: StatusCode::BAD_REQUEST,
        error_code: Some(400),
        description: "Bad Request: chat not found",
    });
    let (result, _) = send_with_fake(
        &fixture.state,
        &fixture.session_id,
        "note.md",
        None,
        &api_fake,
    );
    let err = result.expect_err("Telegram rejection should surface");
    assert_eq!(err.code(), "telegram_api_error");
    assert_eq!(err.status(), StatusCode::BAD_GATEWAY);
    assert!(matches!(
        err,
        TelegramDocumentError::TelegramApi {
            error_code: Some(400),
            ..
        }
    ));
    assert!(
        err.message().contains("chat not found"),
        "{}",
        err.message()
    );

    let transport_fake = FakeTelegramDocumentSender::new(
        FakeTelegramDocumentOutcome::TransportError(format!(
            "error sending request for url (https://api.telegram.org/bot{TEST_BOT_TOKEN}/sendDocument)"
        )),
    );
    let (result, _) = send_with_fake(
        &fixture.state,
        &fixture.session_id,
        "note.md",
        None,
        &transport_fake,
    );
    let err = result.expect_err("transport failure should surface");
    assert_eq!(err.code(), "telegram_api_error");
    assert!(!err.message().contains(TEST_BOT_TOKEN), "{}", err.message());
    assert!(
        err.message().contains("/bot<redacted>/sendDocument"),
        "{}",
        err.message()
    );
}

#[test]
fn telegram_document_refuses_read_only_delegated_sessions() {
    let state = test_app_state_with_drained_delegation_codex_runtime("telegram-doc-read-only");
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Do not send.".to_owned(),
                title: Some("Read-only Telegram".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let fake = FakeTelegramDocumentSender::succeeding(1);

    let (result, connected_with) = send_with_fake(
        &state,
        &created.delegation.child_session_id,
        "note.md",
        None,
        &fake,
    );
    let err = result.expect_err("read-only child should be refused");
    assert_eq!(err.code(), "session_refused");
    assert_eq!(err.status(), StatusCode::FORBIDDEN);
    assert!(
        err.message().contains("read-only delegated sessions"),
        "{}",
        err.message()
    );
    assert_eq!(connected_with, None);
    assert!(fake.sent.borrow().is_empty());
}

// ---- HTTP route -----------------------------------------------------------

#[tokio::test]
async fn telegram_document_route_returns_typed_error_envelopes() {
    let fixture = telegram_document_fixture(true, None, Some(TEST_CHAT_ID));
    write_test_file(&fixture.workdir.join("note.md"), b"# note\n");
    let app = app_router(fixture.state.clone());
    let uri = format!("/api/sessions/{}/telegram/documents", fixture.session_id);

    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::post(uri.as_str())
            .header("content-type", "application/json")
            .body(Body::from(json!({ "path": "note.md" }).to_string()))
            .expect("request should build"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "telegram_not_configured");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty())
    );

    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::post(uri.as_str())
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "path": "note.md", "chatId": 1 }).to_string(),
            ))
            .expect("request should build"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_request");
}

// Additional admission-race and real handler-wire coverage stays in this test
// module so the telegram_documents filter selects every document API test.
mod safety {
    use super::*;
    include!("telegram_document_safety.rs");
}
