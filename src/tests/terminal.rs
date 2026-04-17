//! Terminal subsystem tests.
//!
//! The terminal subsystem lets the user (or an agent) run shell commands
//! inside a session's workdir. Commands enter through two HTTP routes:
//! `POST /api/terminal/run` returns the full buffered response
//! synchronously, and `POST /api/terminal/run/stream` tails stdout/stderr
//! as a Server-Sent Events stream with incremental `output` frames and a
//! terminating `complete` or `error` frame.
//!
//! The implementation has two layers. (a) A shared output buffer
//! (`TerminalOutputBuffer` + `read_capped_*` helpers) accumulates
//! child-process bytes under a single lock, serving UTF-8-safe
//! incremental snapshots so multibyte characters never split across
//! SSE frame boundaries. (b) The HTTP route layer spawns each shell
//! under a platform-specific container — Windows Job Objects with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, Unix process groups with a
//! dedicated PGID — so timeout or shell-exit can reap the entire
//! process tree including grandchildren.
//!
//! Local and remote concurrency semaphores cap how many shells can run
//! at once so a prompt injection cannot fan out unbounded jobs. The
//! production code for both layers lives in `src/terminal.rs`
//! (extracted from `api.rs` earlier this session).

use super::*;

// Pins `read_capped_child_stdout_line` returning each newline-delimited
// record with its trailing `\n`, then the final newline-less tail at EOF.
// Guards against regressions that drop the terminator or miss the last
// partial line when the child exits without a closing newline.
#[test]
fn read_capped_child_stdout_line_reads_newline_delimited_and_eof_lines() {
    let mut reader = std::io::Cursor::new(
        br#"{"ok":true}
tail"#
            .to_vec(),
    );
    let mut line_buf = Vec::new();

    let first_bytes =
        read_capped_child_stdout_line(&mut reader, &mut line_buf, 64, "test stdout").unwrap();
    assert_eq!(
        first_bytes,
        br#"{"ok":true}
"#
        .len()
    );
    assert_eq!(
        String::from_utf8(line_buf.clone()).unwrap(),
        "{\"ok\":true}\n"
    );

    let second_bytes =
        read_capped_child_stdout_line(&mut reader, &mut line_buf, 64, "test stdout").unwrap();
    assert_eq!(second_bytes, b"tail".len());
    assert_eq!(String::from_utf8(line_buf).unwrap(), "tail");
}

// Pins `read_capped_child_stdout_line` silently draining any line that
// exceeds the cap while staying aligned for the next record. Guards
// against a regression that errors out or mis-aligns the reader on a
// runaway log line and tears down the session runtime.
#[test]
fn read_capped_child_stdout_line_drains_oversized_lines() {
    // An oversized line should be drained (not cause an error) so the
    // reader stays aligned and the runtime is not torn down.
    let mut reader = std::io::Cursor::new(b"abcdefghi\nnext\n".to_vec());
    let mut line_buf = Vec::new();

    let bytes_read =
        read_capped_child_stdout_line(&mut reader, &mut line_buf, 8, "test stdout").unwrap();
    assert_eq!(bytes_read, b"abcdefghi\n".len());
    assert!(
        line_buf.is_empty(),
        "oversized line should be drained, not buffered"
    );

    // The next normally-sized line should be readable.
    let next_bytes =
        read_capped_child_stdout_line(&mut reader, &mut line_buf, 8, "test stdout").unwrap();
    assert_eq!(next_bytes, b"next\n".len());
    assert_eq!(String::from_utf8(line_buf).unwrap(), "next\n");
}

// Pins `read_capped_child_stdout_line` draining an oversized line that
// ends at EOF with no trailing newline, then cleanly reporting EOF on
// the next call. Guards against an off-by-one that leaves the reader
// thinking there is still a partial record to return.
#[test]
fn read_capped_child_stdout_line_drains_oversized_line_at_eof() {
    // An oversized line that ends at EOF (no trailing newline) should also
    // be drained without error.
    let mut reader = std::io::Cursor::new(b"abcdefghi".to_vec());
    let mut line_buf = Vec::new();

    let bytes_read =
        read_capped_child_stdout_line(&mut reader, &mut line_buf, 8, "test stdout").unwrap();
    assert_eq!(bytes_read, b"abcdefghi".len());
    assert!(line_buf.is_empty());

    // Should signal EOF on the next call.
    let eof_bytes =
        read_capped_child_stdout_line(&mut reader, &mut line_buf, 8, "test stdout").unwrap();
    assert_eq!(eof_bytes, 0);
}

// Pins `read_capped_terminal_output` keeping the returned string at or
// under `TERMINAL_OUTPUT_MAX_BYTES`, flagging truncation once the cap
// is exceeded, and lossy-decoding invalid UTF-8 with the replacement
// character. Guards the memory ceiling on every terminal response body.
#[test]
fn read_capped_terminal_output_bounds_stored_prefix() {
    let (empty_output, empty_truncated) =
        read_capped_terminal_output(std::io::Cursor::new(Vec::<u8>::new())).unwrap();
    assert_eq!(empty_output, "");
    assert!(!empty_truncated);

    let (small_output, small_truncated) =
        read_capped_terminal_output(std::io::Cursor::new(b"hello terminal".to_vec())).unwrap();
    assert_eq!(small_output, "hello terminal");
    assert!(!small_truncated);

    let exact_output = "x".repeat(TERMINAL_OUTPUT_MAX_BYTES);
    let (exact_output_result, exact_truncated) =
        read_capped_terminal_output(std::io::Cursor::new(exact_output.clone().into_bytes()))
            .unwrap();
    assert_eq!(exact_output_result, exact_output);
    assert!(!exact_truncated);

    let oversized_prefix = "y".repeat(TERMINAL_OUTPUT_MAX_BYTES);
    let oversized_output = format!("{oversized_prefix}tail");
    let (oversized_output_result, oversized_truncated) =
        read_capped_terminal_output(std::io::Cursor::new(oversized_output.into_bytes())).unwrap();
    assert_eq!(oversized_output_result, oversized_prefix);
    assert!(oversized_truncated);

    let (invalid_utf8_output, invalid_utf8_truncated) =
        read_capped_terminal_output(std::io::Cursor::new(vec![b'o', 0xff, b'k'])).unwrap();
    assert_eq!(invalid_utf8_output, "o\u{fffd}k");
    assert!(!invalid_utf8_truncated);
}

// Pins `terminal_output_delta_locked` returning only bytes that were
// appended since the previous emission and advancing `emitted_bytes`
// past the delivered window. Guards against the SSE stream replaying
// already-sent text or stalling after a flush.
#[test]
fn terminal_output_delta_advances_emitted_bytes() {
    let mut buffer = TerminalOutputBuffer {
        bytes: b"hello".to_vec(),
        emitted_bytes: 0,
        truncated: false,
    };

    assert_eq!(
        terminal_output_delta_locked(&mut buffer, false),
        Some("hello".to_owned())
    );
    assert_eq!(buffer.emitted_bytes, b"hello".len());
    assert_eq!(terminal_output_delta_locked(&mut buffer, false), None);

    buffer.bytes.extend_from_slice(b" world");
    assert_eq!(
        terminal_output_delta_locked(&mut buffer, false),
        Some(" world".to_owned())
    );
    assert_eq!(buffer.emitted_bytes, b"hello world".len());
}

// Pins `terminal_streamable_utf8_prefix_len` holding back a trailing
// partial multibyte sequence during mid-stream emission and releasing
// the full buffer on final flush. Guards against a split `\xc3` byte
// reaching the SSE client as a lone invalid UTF-8 fragment.
#[test]
fn terminal_streamable_utf8_prefix_len_holds_incomplete_multibyte_until_flush() {
    let complete = "ok \u{00e9}".as_bytes().to_vec();
    assert_eq!(
        terminal_streamable_utf8_prefix_len(&complete, false),
        complete.len()
    );

    let mut incomplete = b"ok ".to_vec();
    incomplete.push(0xc3);
    assert_eq!(terminal_streamable_utf8_prefix_len(&incomplete, false), 3);
    assert_eq!(
        terminal_streamable_utf8_prefix_len(&incomplete, true),
        incomplete.len()
    );
}

// Pins `validate_terminal_workdir` rejecting workdirs longer than
// `TERMINAL_WORKDIR_MAX_CHARS` with a 400 naming the cap. Guards against
// a megabyte-sized path flowing into canonicalization or the remote
// proxy when the `.chars().count()` check is dropped.
#[test]
fn validate_terminal_workdir_rejects_oversized_input() {
    // Pins the contract documented in `docs/architecture.md:131`:
    // `workdir` ≤ `TERMINAL_WORKDIR_MAX_CHARS` characters, enforced at
    // the validator layer. A regression that drops the `.chars().count()`
    // check should fail this test instead of silently accepting a
    // megabyte of path text that then flows into
    // `resolve_project_scoped_requested_path` (local) or over the wire
    // to the remote proxy.
    let within_cap = "a".repeat(TERMINAL_WORKDIR_MAX_CHARS);
    let accepted =
        validate_terminal_workdir(&within_cap).expect("cap-sized workdir should be accepted");
    assert_eq!(accepted.chars().count(), TERMINAL_WORKDIR_MAX_CHARS);

    let oversized = "a".repeat(TERMINAL_WORKDIR_MAX_CHARS + 1);
    let err =
        validate_terminal_workdir(&oversized).expect_err("oversized workdir should be rejected");
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(
        err.message
            .contains(&TERMINAL_WORKDIR_MAX_CHARS.to_string()),
        "error message should interpolate the cap, got: {}",
        err.message
    );
}

// Pins `validate_terminal_workdir` rejecting interior NUL bytes at the
// validator layer with a 400 that names the NUL problem. Guards against
// the byte reaching `fs::canonicalize` or the HTTP serializer and
// surfacing a less-clear OS-level error.
#[test]
fn validate_terminal_workdir_rejects_interior_nul_bytes() {
    // Pins the contract documented in `docs/architecture.md:131`:
    // `workdir` must have no interior NUL bytes, enforced at the
    // validator layer. Without this check the NUL would reach
    // `fs::canonicalize` (local) or the HTTP serializer (remote) and
    // produce a less-clear error.
    let with_nul = "/repo\0/malicious";
    let err = validate_terminal_workdir(with_nul).expect_err("NUL byte should be rejected");
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(
        err.message.contains("NUL"),
        "error message should name the NUL problem, got: {}",
        err.message
    );
}

struct ChannelTerminalReader {
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
}

impl std::io::Read for ChannelTerminalReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.rx.recv() {
            Ok(chunk) => {
                let take = chunk.len().min(buf.len());
                buf[..take].copy_from_slice(&chunk[..take]);
                Ok(take)
            }
            Err(_) => Ok(0),
        }
    }
}

struct ErrorAfterPrefixReader {
    emitted_prefix: bool,
}

impl std::io::Read for ErrorAfterPrefixReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if !self.emitted_prefix {
            self.emitted_prefix = true;
            let prefix = b"prefix-before-error";
            buf[..prefix.len()].copy_from_slice(prefix);
            return Ok(prefix.len());
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "reader failed after prefix",
        ))
    }
}

fn wait_for_terminal_output_snapshot(
    buffer: &SharedTerminalOutputBuffer,
    expected_output: &str,
    expected_truncated: bool,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let snapshot = snapshot_terminal_output_buffer(buffer);
        if snapshot == (expected_output.to_owned(), expected_truncated) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "terminal output buffer snapshot stayed at {snapshot:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

// Pins `join_terminal_output_reader` returning the partial bytes that
// had accumulated in the shared buffer when the reader timeout fires,
// with the truncated flag set. Guards against the timeout path
// discarding in-flight stdout the caller would otherwise surface.
#[test]
fn terminal_output_reader_timeout_returns_non_empty_shared_prefix() {
    let (tx, rx) = std::sync::mpsc::channel();
    let buffer = new_terminal_output_buffer();
    let reader_buffer = buffer.clone();
    // Completion channel matches production: the reader closure sends
    // `()` on exit so the main thread can block in `recv_timeout`
    // instead of polling. Sender is held by the closure; we pass the
    // receiver into `join_terminal_output_reader`.
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let reader = std::thread::spawn(move || {
        let result = read_capped_terminal_output_into(ChannelTerminalReader { rx }, &reader_buffer);
        let _ = done_tx.send(());
        result
    });

    tx.send(b"prefix-".to_vec()).unwrap();
    tx.send(b"more".to_vec()).unwrap();
    wait_for_terminal_output_snapshot(&buffer, "prefix-more", false);

    let started = std::time::Instant::now();
    let (output, truncated) = join_terminal_output_reader(
        reader,
        done_rx,
        buffer,
        "stdout",
        Duration::from_millis(100),
        None,
    )
    .expect("reader timeout should return buffered output");

    assert_eq!(output, "prefix-more");
    assert!(truncated);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "timeout path should return promptly"
    );

    drop(tx);
}

// Pins `join_terminal_output_reader` surfacing the bytes read before
// an `io::Read` error with the truncated flag, rather than returning
// an empty body. Guards against a stream that fails mid-read hiding
// the prefix the child process successfully emitted.
#[test]
fn terminal_output_reader_error_returns_non_empty_shared_prefix() {
    let buffer = new_terminal_output_buffer();
    let reader_buffer = buffer.clone();
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let reader = std::thread::spawn(move || {
        let result = read_capped_terminal_output_into(
            ErrorAfterPrefixReader {
                emitted_prefix: false,
            },
            &reader_buffer,
        );
        let _ = done_tx.send(());
        result
    });

    let (output, truncated) = join_terminal_output_reader(
        reader,
        done_rx,
        buffer,
        "stdout",
        Duration::from_secs(1),
        None,
    )
    .expect("reader error should return buffered prefix as truncated output");

    assert_eq!(output, "prefix-before-error");
    assert!(truncated);
}

// Pins `join_terminal_output_reader` translating a panicked reader
// thread into a 500 whose message names the panic. Guards against a
// reader crash producing either a silent empty body or a deadlock
// waiting on a completion signal that will never arrive.
#[test]
fn terminal_output_reader_disconnected_reports_reader_panic() {
    let buffer = new_terminal_output_buffer();
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let reader = std::thread::spawn(move || -> std::io::Result<()> {
        let _hold_sender = done_tx;
        panic!("reader exploded before completion signal");
    });

    let err = join_terminal_output_reader(
        reader,
        done_rx,
        buffer,
        "stdout",
        Duration::from_secs(1),
        None,
    )
    .expect_err("reader panic should surface as an internal error");

    assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        err.message.contains("reader panicked"),
        "error message should name the reader panic, got: {}",
        err.message
    );
}

// Pins `read_capped_terminal_output_into` writing the capped prefix
// into the shared buffer and `snapshot_terminal_output_buffer` reading
// it back with the truncated flag set. Guards the cross-thread contract
// between the reader worker and the main thread's final snapshot.
#[test]
fn terminal_output_shared_buffer_round_trips_from_reader_thread() {
    let buffer = new_terminal_output_buffer();
    let reader_buffer = buffer.clone();
    let oversized_prefix = "z".repeat(TERMINAL_OUTPUT_MAX_BYTES);
    let oversized_output = format!("{oversized_prefix}tail");
    let reader = std::thread::spawn(move || {
        read_capped_terminal_output_into(
            std::io::Cursor::new(oversized_output.into_bytes()),
            &reader_buffer,
        )
    });

    reader
        .join()
        .expect("reader thread should not panic")
        .expect("reader thread should finish successfully");
    let (output, truncated) = snapshot_terminal_output_buffer(&buffer);

    assert_eq!(output, oversized_prefix);
    assert!(truncated);
}

/// Yields a single fixed-size chunk per `read()` call, sleeping briefly
/// between chunks so intermediate snapshots on the main thread have a
/// chance to observe the in-progress shared buffer state. Used to
/// exercise the `TerminalOutputBuffer` concurrency contract: one writer
/// thread appending via `read_capped_terminal_output_into`, one
/// snapshotter thread reading via `snapshot_terminal_output_buffer`.
struct SlowChunkedReader {
    chunk: &'static [u8],
    remaining: usize,
}

impl std::io::Read for SlowChunkedReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        std::thread::sleep(Duration::from_micros(200));
        let take = self.chunk.len().min(buf.len());
        buf[..take].copy_from_slice(&self.chunk[..take]);
        self.remaining -= 1;
        Ok(take)
    }
}

// Pins the `TerminalOutputBuffer` concurrency contract: every
// intermediate `snapshot_terminal_output_buffer` is a valid prefix of
// the final buffer, never a torn chunk. Guards against a regression
// that splits the length-check and the append into separate locks.
#[test]
fn terminal_output_buffer_supports_concurrent_writer_and_snapshotter() {
    // Exercise the shared-buffer concurrency contract: while a reader
    // thread appends into the shared buffer via
    // `read_capped_terminal_output_into`, the main thread takes
    // intermediate snapshots via `snapshot_terminal_output_buffer` and
    // verifies each one is a valid prefix of the final buffer. A regression
    // that split the `guard.bytes.len()` check from the adjacent
    // `extend_from_slice` into separate lock acquisitions would let a
    // snapshot observe a partially-appended chunk, breaking the prefix
    // invariant. Locking in a single critical section — the current
    // implementation — preserves it.
    const CHUNK: &[u8] = b"0123456789abcdef";
    const CHUNK_COUNT: usize = 128;

    let buffer = new_terminal_output_buffer();
    let writer_buffer = buffer.clone();
    let writer = std::thread::spawn(move || {
        let reader = SlowChunkedReader {
            chunk: CHUNK,
            remaining: CHUNK_COUNT,
        };
        read_capped_terminal_output_into(reader, &writer_buffer)
    });

    // Capture at least one snapshot before the writer can finish, and
    // keep capturing until it does. Every snapshot MUST be a valid
    // prefix of the final buffer.
    let mut intermediate_snapshots = Vec::new();
    let (early, _) = snapshot_terminal_output_buffer(&buffer);
    intermediate_snapshots.push(early);
    while !writer.is_finished() {
        let (output, _) = snapshot_terminal_output_buffer(&buffer);
        intermediate_snapshots.push(output);
        // Yield between snapshots so the writer thread can make progress
        // on single-core CI runners and on Windows where the default
        // timer resolution is ~15.6ms: a tight hot-spin here would
        // starve the 200μs `SlowChunkedReader` sleep and leave the
        // snapshotter spinning in kernel-mode before any chunk is
        // appended to the buffer.
        std::thread::yield_now();
    }
    writer
        .join()
        .expect("writer thread should not panic")
        .expect("writer thread should finish successfully");

    let (final_output, final_truncated) = snapshot_terminal_output_buffer(&buffer);
    assert!(!final_truncated);
    assert_eq!(final_output.len(), CHUNK.len() * CHUNK_COUNT);

    for snapshot in &intermediate_snapshots {
        // The writer appends one full 16-byte chunk per lock acquisition,
        // so any snapshot taken between chunks must see a whole number of
        // chunks — never a torn 7-byte prefix or a shifted 14-byte window.
        assert_eq!(
            snapshot.len() % CHUNK.len(),
            0,
            "intermediate snapshot should not tear a chunk: got len {} bytes",
            snapshot.len()
        );
        assert!(
            final_output.starts_with(snapshot),
            "intermediate snapshot {:?} should be a prefix of the final buffer",
            snapshot,
        );
    }
}

// Pins the `POST /api/terminal/run` validator rejecting blank
// command, blank workdir, oversized workdir, NUL in workdir, oversized
// command, and a workdir outside the project root. Guards against any
// of these slipping past the handler and reaching the shell layer.
#[tokio::test]
async fn terminal_run_route_rejects_invalid_requests() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-terminal-validation-{}", Uuid::new_v4()));
    let outside_root = std::env::temp_dir().join(format!(
        "termal-terminal-validation-outside-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&outside_root).unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Terminal".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    let app = app_router(state);
    let project_id = project.project_id;
    let root_path = root.to_string_lossy().into_owned();
    let outside_path = outside_root.to_string_lossy().into_owned();

    let (empty_status, empty_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "   ",
                    "projectId": project_id,
                    "workdir": root_path,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(empty_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        empty_response,
        json!({ "error": "terminal command cannot be empty" })
    );

    let (empty_workdir_status, empty_workdir_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo ok",
                    "projectId": project_id,
                    "workdir": "   ",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(empty_workdir_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        empty_workdir_response,
        json!({ "error": "terminal workdir cannot be empty" })
    );

    let oversized_workdir = "a".repeat(TERMINAL_WORKDIR_MAX_CHARS + 1);
    let (oversized_workdir_status, oversized_workdir_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo ok",
                    "projectId": project_id,
                    "workdir": oversized_workdir,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(oversized_workdir_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        oversized_workdir_response,
        json!({
            "error": format!(
                "terminal workdir cannot exceed {TERMINAL_WORKDIR_MAX_CHARS} characters"
            )
        })
    );

    let (nul_workdir_status, nul_workdir_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo ok",
                    "projectId": project_id,
                    "workdir": "/repo\0/bad",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(nul_workdir_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        nul_workdir_response,
        json!({ "error": "terminal workdir cannot contain NUL bytes" })
    );

    let (oversized_status, oversized_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "x".repeat(TERMINAL_COMMAND_MAX_CHARS + 1),
                    "projectId": project_id,
                    "workdir": root_path,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(oversized_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        oversized_response,
        json!({
            "error": format!(
                "terminal command cannot exceed {TERMINAL_COMMAND_MAX_CHARS} characters"
            )
        })
    );

    let (outside_status, outside_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo ok",
                    "projectId": project_id,
                    "workdir": outside_path,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(outside_status, StatusCode::BAD_REQUEST);
    assert!(
        outside_response["error"]
            .as_str()
            .unwrap()
            .contains("must stay inside project")
    );

    let (multibyte_status, multibyte_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "\u{00e9}".repeat(TERMINAL_COMMAND_MAX_CHARS),
                    "projectId": project_id,
                    "workdir": outside_path,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(multibyte_status, StatusCode::BAD_REQUEST);
    let multibyte_error = multibyte_response["error"].as_str().unwrap();
    assert!(
        multibyte_error.contains("must stay inside project"),
        "expected scope validation, got {multibyte_error:?}"
    );
    assert!(!multibyte_error.contains("cannot exceed"));

    // The leading `#` is load-bearing: it makes the 20K-char body a shell
    // comment, proving character-count validation without executing it.
    let valid_multibyte_command = format!("#{}", "\u{00e9}".repeat(TERMINAL_COMMAND_MAX_CHARS - 1));
    let (valid_multibyte_status, valid_multibyte_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": valid_multibyte_command,
                    "projectId": project_id,
                    "workdir": root_path,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(valid_multibyte_status, StatusCode::OK);
    assert_eq!(
        valid_multibyte_response["command"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        TERMINAL_COMMAND_MAX_CHARS
    );

    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(outside_root).unwrap();
}

// Pins the `POST /api/terminal/run` handler running the same
// blank / oversized / NUL validation on remote-scoped projects before
// contacting the SSH-forwarded backend. Guards against a malformed
// request leaking onto the wire and wasting a remote round trip.
#[tokio::test]
async fn terminal_run_route_validates_remote_scoped_requests_before_proxying() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Terminal",
        "remote-project-1",
    );
    let app = app_router(state);

    let (empty_status, empty_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "   ",
                    "projectId": project_id,
                    "workdir": "/remote/repo",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(empty_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        empty_response,
        json!({ "error": "terminal command cannot be empty" })
    );

    let (empty_workdir_status, empty_workdir_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo ok",
                    "projectId": project_id,
                    "workdir": "   ",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(empty_workdir_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        empty_workdir_response,
        json!({ "error": "terminal workdir cannot be empty" })
    );

    let oversized_workdir = "a".repeat(TERMINAL_WORKDIR_MAX_CHARS + 1);
    let (oversized_workdir_status, oversized_workdir_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo ok",
                    "projectId": project_id,
                    "workdir": oversized_workdir,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(oversized_workdir_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        oversized_workdir_response,
        json!({
            "error": format!(
                "terminal workdir cannot exceed {TERMINAL_WORKDIR_MAX_CHARS} characters"
            )
        })
    );

    let (nul_workdir_status, nul_workdir_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo ok",
                    "projectId": project_id,
                    "workdir": "/remote\0/repo",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(nul_workdir_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        nul_workdir_response,
        json!({ "error": "terminal workdir cannot contain NUL bytes" })
    );

    let (oversized_status, oversized_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "x".repeat(TERMINAL_COMMAND_MAX_CHARS + 1),
                    "projectId": project_id,
                    "workdir": "/remote/repo",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(oversized_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        oversized_response,
        json!({
            "error": format!(
                "terminal command cannot exceed {TERMINAL_COMMAND_MAX_CHARS} characters"
            )
        })
    );
}

#[cfg(windows)]
fn terminal_exact_stdout_command(text: &str) -> String {
    format!("[Console]::Out.Write('{}')", text.replace('\'', "''"))
}

#[cfg(not(windows))]
fn terminal_exact_stdout_command(text: &str) -> String {
    format!("printf %s {}", shell_single_quote(text))
}

// Pins `POST /api/terminal/run/stream` emitting at least one `output`
// SSE frame before the single `complete` frame and never emitting
// `error` on success. Guards against the SSE pipeline buffering all
// stdout until completion, defeating the point of the stream route.
#[tokio::test]
async fn terminal_run_stream_route_emits_output_before_complete() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-terminal-stream-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let command = terminal_exact_stdout_command("stream-ok");
    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Terminal Stream".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    let app = app_router(state);
    let response = request_response(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run/stream")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": command.clone(),
                    "projectId": project.project_id,
                    "workdir": root.to_string_lossy(),
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let events = collect_sse_events(response).await;
    let output_index = events
        .iter()
        .position(|(event_name, _)| event_name == "output")
        .expect("stream should emit stdout before completion");
    let complete_events = events
        .iter()
        .enumerate()
        .filter(|(_, (event_name, _))| event_name == "complete")
        .collect::<Vec<_>>();
    assert_eq!(complete_events.len(), 1, "events: {events:?}");
    assert!(
        events.iter().all(|(event_name, _)| event_name != "error"),
        "successful stream should not emit an error: {events:?}"
    );
    assert!(
        output_index < complete_events[0].0,
        "stdout should arrive before completion: {events:?}"
    );
    let stdout = events
        .iter()
        .filter(|(event_name, _)| event_name == "output")
        .map(|(_, event_data)| {
            let output: Value =
                serde_json::from_str(event_data).expect("output event should decode");
            assert_eq!(output["stream"], Value::String("stdout".to_owned()));
            output["text"].as_str().unwrap().to_owned()
        })
        .collect::<String>();
    assert_eq!(stdout, "stream-ok");

    let complete_data = &complete_events[0].1.1;
    let complete = serde_json::from_str::<TerminalCommandResponse>(complete_data)
        .expect("complete event should decode");
    assert_eq!(complete.command, command);
    assert_eq!(complete.stdout, "stream-ok");
    assert!(complete.success);

    fs::remove_dir_all(root).unwrap();
}

// Pins `POST /api/terminal/run/stream` returning a 400 JSON body for
// a workdir outside the project before opening the SSE stream. Guards
// against validation errors being smuggled inside a 200 SSE response
// where the client would have to parse an `error` event to notice.
#[tokio::test]
async fn terminal_run_stream_route_returns_http_error_for_bad_workdir() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-terminal-stream-bad-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let outside = root
        .parent()
        .expect("temp project root should have a parent")
        .to_string_lossy()
        .into_owned();
    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Terminal Stream Bad Workdir".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    let app = app_router(state);

    let (status, response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run/stream")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo stream-ok",
                    "projectId": project.project_id,
                    "workdir": outside,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("must stay inside project"),
        "unexpected error body: {response}"
    );

    fs::remove_dir_all(root).unwrap();
}

// Pins `POST /api/terminal/run/stream` returning 429 with the
// local-vs-remote specific error string when the matching
// `TERMINAL_*_COMMAND_CONCURRENCY_LIMIT` semaphore is saturated.
// Guards against a prompt injection fanning out unbounded SSE jobs.
#[tokio::test]
async fn terminal_run_stream_route_limits_local_and_remote_concurrent_commands() {
    let state = test_app_state();
    let root =
        std::env::temp_dir().join(format!("termal-terminal-stream-limit-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Terminal Stream Limit".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    let remote = RemoteConfig {
        id: "ssh-stream-limit".to_owned(),
        name: "SSH Stream Limit".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let remote_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Stream Limit",
        "remote-stream-limit-project",
    );
    let local_permits = (0..TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT)
        .map(|_| {
            state
                .terminal_local_command_semaphore
                .clone()
                .try_acquire_owned()
                .expect("test should acquire local stream permit")
        })
        .collect::<Vec<_>>();
    let remote_permits = (0..TERMINAL_REMOTE_COMMAND_CONCURRENCY_LIMIT)
        .map(|_| {
            state
                .terminal_remote_command_semaphore
                .clone()
                .try_acquire_owned()
                .expect("test should acquire remote stream permit")
        })
        .collect::<Vec<_>>();
    let app = app_router(state);

    let (local_status, local_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run/stream")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo local",
                    "projectId": project.project_id,
                    "workdir": root.to_string_lossy(),
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(local_status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        local_response["error"]
            .as_str()
            .unwrap()
            .contains("too many local terminal commands"),
        "unexpected local stream 429 body: {local_response}"
    );

    let (remote_status, remote_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run/stream")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo remote",
                    "projectId": remote_project_id,
                    "workdir": "/remote/repo",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(remote_status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        remote_response["error"]
            .as_str()
            .unwrap()
            .contains("too many remote terminal commands"),
        "unexpected remote stream 429 body: {remote_response}"
    );

    drop(local_permits);
    drop(remote_permits);
    fs::remove_dir_all(root).unwrap();
}

// Pins `POST /api/terminal/run/stream` emitting exactly one `error`
// SSE frame (and zero `complete` frames) when the child process fails
// to spawn because the workdir is a file. Guards against the client
// seeing both a `complete` and an `error`, or neither, on spawn failure.
#[tokio::test]
async fn terminal_run_stream_route_emits_error_without_complete_when_spawn_fails() {
    let state = test_app_state();
    let root =
        std::env::temp_dir().join(format!("termal-terminal-stream-error-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let not_a_directory = root.join("not-a-directory.txt");
    fs::write(&not_a_directory, "not a directory").unwrap();
    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Terminal Stream Error".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    let app = app_router(state);

    let response = request_response(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run/stream")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": terminal_exact_stdout_command("unreachable"),
                    "projectId": project.project_id,
                    "workdir": not_a_directory.to_string_lossy(),
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let events = collect_sse_events(response).await;
    let error_events = events
        .iter()
        .filter(|(event_name, _)| event_name == "error")
        .collect::<Vec<_>>();
    assert_eq!(error_events.len(), 1, "events: {events:?}");
    assert!(
        events
            .iter()
            .all(|(event_name, _)| event_name != "complete"),
        "spawn failure should not emit complete: {events:?}"
    );
    let payload: Value =
        serde_json::from_str(&error_events[0].1).expect("error event should decode");
    assert_eq!(
        payload["status"],
        Value::from(StatusCode::INTERNAL_SERVER_ERROR.as_u16())
    );
    assert!(
        payload["error"]
            .as_str()
            .unwrap()
            .contains("failed to start terminal command"),
        "unexpected error event payload: {payload}"
    );

    fs::remove_dir_all(root).unwrap();
}
// Pins `POST /api/terminal/run` returning 429 with an error string
// that interpolates `TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT`, and
// releasing its permit once the request completes so a later call
// succeeds. Also verifies the local and remote semaphores stay
// independent. Guards the permit accounting and user-facing error.
#[tokio::test]
async fn terminal_run_route_limits_concurrent_commands() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-terminal-limit-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Terminal Limit".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    let remote_captured_body = Arc::new(Mutex::new(None::<String>));
    let remote_captured_for_server = remote_captured_body.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let remote_port = listener.local_addr().expect("listener addr").port();
    let remote_command = "echo remote";
    let remote_response_body = serde_json::to_string(&TerminalCommandResponse {
        command: remote_command.to_owned(),
        duration_ms: 7,
        exit_code: Some(0),
        output_truncated: false,
        shell: "sh".to_owned(),
        stderr: String::new(),
        stdout: "remote\n".to_owned(),
        success: true,
        timed_out: false,
        workdir: "/remote/repo".to_owned(),
    })
    .expect("terminal response should encode");
    let remote_server = std::thread::spawn(move || {
        // Loop until the terminal run request is captured rather than
        // hard-coding the number of proxy round-trips (see
        // `terminal_run_route_proxies_valid_remote_multibyte_commands` for
        // the full rationale).
        loop {
            let mut stream = accept_test_connection_with_timeout(
                &listener,
                "terminal limit remote listener",
                std::time::Duration::from_secs(10),
            );
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                let bytes_read = stream.read(&mut chunk).expect("request should read");
                assert!(bytes_read > 0, "request closed before headers completed");
                buffer.extend_from_slice(&chunk[..bytes_read]);
                if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end;
                }
            };
            let headers = String::from_utf8_lossy(&buffer[..header_end]);
            let request_line = headers
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then_some(value.trim())
                        .and_then(|value| value.parse::<usize>().ok())
                })
                .unwrap_or(0);
            let body_start = header_end + 4;
            while buffer.len() < body_start + content_length {
                let bytes_read = stream.read(&mut chunk).expect("request body should read");
                if bytes_read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..bytes_read]);
            }
            let body = String::from_utf8_lossy(&buffer[body_start..body_start + content_length])
                .to_string();

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 11\r\n\r\n{\"ok\":true}",
                    )
                    .expect("health response should write");
                continue;
            }

            if request_line.starts_with("POST /api/terminal/run ") {
                *remote_captured_for_server
                    .lock()
                    .expect("capture mutex poisoned") = Some(body);
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            remote_response_body.len(),
                            remote_response_body
                        )
                        .as_bytes(),
                    )
                    .expect("terminal response should write");
                break;
            }

            panic!("unexpected request: {request_line}");
        }
    });
    let remote = RemoteConfig {
        id: "ssh-terminal-limit".to_owned(),
        name: "SSH Terminal Limit".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let remote_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Terminal Limit",
        "remote-terminal-limit-project",
    );
    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port: remote_port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(true),
                event_bridge_shutdown: AtomicBool::new(false),
                supports_inline_orchestrator_templates: Mutex::new(None),
            }),
        );
    let mut permits = (0..TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT)
        .map(|_| {
            state
                .terminal_local_command_semaphore
                .clone()
                .try_acquire_owned()
                .expect("test should acquire local terminal permit")
        })
        .collect::<Vec<_>>();
    let semaphore_state = state.clone();
    let project_id = project.project_id;
    let root_path = root.to_string_lossy().into_owned();
    let app = app_router(state);

    let (status, response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo ok",
                    "projectId": project_id,
                    "workdir": root_path,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let error_body = response["error"]
        .as_str()
        .expect("429 response should include error string");
    assert!(
        error_body.contains("too many local terminal commands"),
        "unexpected 429 body {error_body:?}"
    );
    // Pin the interpolated limit substring so a future `format!` typo or a
    // silent divergence between the string literal and the constant would
    // be caught instead of silently dropping the count.
    assert!(
        error_body.contains(&format!(
            "limit is {TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT}"
        )),
        "429 body {error_body:?} should interpolate TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT"
    );

    drop(permits.pop());
    let (released_status, released_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo released",
                    "projectId": project_id,
                    "workdir": root_path,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(released_status, StatusCode::OK);
    assert_eq!(
        released_response["command"],
        Value::String("echo released".to_owned())
    );
    assert!(
        released_response["stdout"]
            .as_str()
            .unwrap()
            .contains("released")
    );

    permits.push(
        semaphore_state
            .terminal_local_command_semaphore
            .clone()
            .try_acquire_owned()
            .expect("successful command should release its local terminal permit"),
    );
    let (relimited_status, relimited_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo blocked-again",
                    "projectId": project_id,
                    "workdir": root_path,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(relimited_status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        relimited_response["error"]
            .as_str()
            .unwrap()
            .contains("too many local terminal commands")
    );
    drop(permits);

    let remote_permits = (0..TERMINAL_REMOTE_COMMAND_CONCURRENCY_LIMIT)
        .map(|_| {
            semaphore_state
                .terminal_remote_command_semaphore
                .clone()
                .try_acquire_owned()
                .expect("test should acquire remote terminal permit")
        })
        .collect::<Vec<_>>();
    let (local_status, local_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo local",
                    "projectId": project_id,
                    "workdir": root_path,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(local_status, StatusCode::OK);
    assert!(local_response["stdout"].as_str().unwrap().contains("local"));
    drop(remote_permits);

    let local_permits = (0..TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT)
        .map(|_| {
            semaphore_state
                .terminal_local_command_semaphore
                .clone()
                .try_acquire_owned()
                .expect("test should reacquire local terminal permit")
        })
        .collect::<Vec<_>>();
    let (remote_status, remote_response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": remote_command,
                    "projectId": remote_project_id,
                    "workdir": "/remote/repo",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(remote_status, StatusCode::OK);
    assert_eq!(
        remote_response["stdout"],
        Value::String("remote\n".to_owned())
    );
    let remote_captured: Value = serde_json::from_str(
        remote_captured_body
            .lock()
            .expect("capture mutex poisoned")
            .as_ref()
            .expect("remote request should be captured"),
    )
    .expect("remote request body should decode");
    assert_eq!(
        remote_captured["command"],
        Value::String(remote_command.to_owned())
    );
    drop(local_permits);
    join_test_server(remote_server);
    fs::remove_dir_all(root).unwrap();
}

// Pins `run_terminal_shell_command` executing a trivial `echo` inside
// the supplied root, returning stdout containing the echoed text with
// `timed_out == false`, `output_truncated == false`, and a workdir
// string that preserves the raw input without a Windows `\\?\` prefix.
#[test]
fn run_terminal_shell_command_runs_trivial_local_command() {
    let root = std::env::temp_dir().join(format!("termal-terminal-runner-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();

    let response =
        run_terminal_shell_command("echo ok", &root).expect("terminal command should run");

    assert_eq!(response.command, "echo ok");
    assert!(!response.timed_out);
    assert!(response.exit_code.is_some());
    assert!(response.stdout.contains("ok"));
    assert!(!response.output_truncated);
    // The production `run_terminal_shell_command` returns
    // `normalize_user_facing_path(workdir)` on the uncanonicalized input,
    // so don't couple this assertion to `canonicalize`: on Windows CI
    // runners where `%TEMP%` is a junction or symlink, canonicalize
    // resolves the link while the response preserves the raw form. Assert
    // the load-bearing properties directly: the response contains our
    // test-dir tag and is not returned in Windows verbatim-prefix form.
    assert!(
        response.workdir.contains("termal-terminal-runner-"),
        "workdir {:?} should contain the test-dir tag",
        response.workdir
    );
    assert!(
        !response.workdir.starts_with(r"\\?\"),
        "workdir {:?} should not be in Windows verbatim-prefix form",
        response.workdir
    );

    fs::remove_dir_all(root).unwrap();
}

// Pins `run_terminal_shell_command_with_timeout` tearing down the
// entire process tree — including a backgrounded grandchild that would
// otherwise outlive the shell — via the Windows Job Object on kill and
// the Unix PGID on `killpg`. Guards against a timeout leaving an
// orphaned writer that eventually creates the marker file.
#[test]
fn run_terminal_shell_command_timeout_kills_process_tree() {
    // Margin budget for this test, tuned for Windows CI: the shell has 500ms
    // to reach `Start-Process` before the timeout fires, and the grandchild
    // sleeps for 1500ms before touching the marker, giving us a 1000ms
    // margin for PowerShell startup + JIT + Job assignment + ResumeThread +
    // command parse + process creation + child startup. Do NOT shrink these
    // numbers without validating against a cold Windows CI agent (first
    // PowerShell launch, unjitted .NET, AV first-scan), which is the worst
    // case. The `assert_path_absent_throughout` window (2500ms) then
    // continuously asserts the marker stays absent for the rest of the
    // grandchild's scheduled sleep + a safety margin.
    let root = std::env::temp_dir().join(format!("termal-terminal-timeout-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let marker = root.join("orphan-marker.txt");
    let command = terminal_timeout_process_tree_command(&marker);

    let response =
        run_terminal_shell_command_with_timeout(&command, &root, Duration::from_millis(500))
            .expect("timeout command should return a response");

    assert!(response.timed_out);
    assert!(!response.success);
    assert_path_absent_throughout(
        &marker,
        Duration::from_millis(2_500),
        "grandchild process should not survive terminal timeout",
    );

    fs::remove_dir_all(root).unwrap();
}

// Pins `run_terminal_shell_command_with_timeout` succeeding on a
// normal shell exit and — on Windows — still reaping a backgrounded
// grandchild via `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. Unix deliberately
// skips `killpg` on clean exit (PID-reuse race), so the marker is
// allowed on Unix; the Windows guarantee is the load-bearing assertion.
#[test]
fn run_terminal_shell_command_cleans_up_background_children_after_shell_exit() {
    let root = std::env::temp_dir().join(format!("termal-terminal-background-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let marker = root.join("background-marker.txt");
    let command = terminal_background_process_tree_command(&marker);

    let response = run_terminal_shell_command_with_timeout(&command, &root, Duration::from_secs(3))
        .expect("background command should return a response");

    assert!(!response.timed_out);
    assert!(response.success);
    assert!(
        response.stdout.contains("done"),
        "expected parent shell output, got {:?}",
        response.stdout
    );

    // Windows: the Job Object terminates every process assigned to it when
    // the shell exits (JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE), so the
    // background grandchild must be gone by the time we reach the marker
    // check. Unix: we deliberately skip `killpg` on the clean-exit path to
    // avoid racing with PID reuse (see `TerminalProcessTree::cleanup_after_shell_exit`),
    // so a backgrounded grandchild is allowed to re-parent to init and
    // finish on its own schedule. Assert the Windows guarantee, and simply
    // accept that the marker may exist on Unix.
    #[cfg(windows)]
    assert_path_absent_throughout(
        &marker,
        Duration::from_millis(2_500),
        "background grandchild process should not survive terminal command completion on Windows",
    );

    // Best-effort cleanup: on Unix the backgrounded subshell may still be
    // holding the temp directory open. Retry a few times so we don't flake
    // when the grandchild is slow to finish writing the marker.
    for attempt in 0..10 {
        match fs::remove_dir_all(&root) {
            Ok(()) => break,
            Err(err) if attempt == 9 => panic!("failed to remove temp dir {root:?}: {err}"),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// Polls `path` every 50ms for the entire `timeout` window, asserting that
/// it remains absent on every tick. This is deliberately a continuous
/// assertion rather than a poll-with-early-exit helper: the terminal
/// process-tree tests need to prove that a backgrounded grandchild could
/// not have created the marker during the window, not merely that the
/// marker was absent at some instant before the deadline. The helper runs
/// for the full `timeout` even on a warm machine where the kill landed in
/// microseconds.
///
/// `timeout` MUST be substantially larger than the poll interval so the
/// window observes multiple ticks — otherwise the test would trivially
/// pass on a broken kill. We assert this up front so that any future
/// shrinking of the window (or growing of the internal sleep) fails fast
/// with a clear message instead of silently weakening the test.
fn assert_path_absent_throughout(path: &FsPath, timeout: Duration, message: &str) {
    const POLL_INTERVAL: Duration = Duration::from_millis(50);
    const MIN_POLLS: u32 = 4;
    assert!(
        timeout >= POLL_INTERVAL.saturating_mul(MIN_POLLS),
        "assert_path_absent_throughout timeout {timeout:?} is too small for \
         {MIN_POLLS} polls at {POLL_INTERVAL:?}; widen the window or shorten \
         the poll interval"
    );
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        assert!(!path.exists(), "{message}");
        std::thread::sleep(POLL_INTERVAL);
    }
    assert!(!path.exists(), "{message}");
}

#[cfg(windows)]
fn terminal_timeout_process_tree_command(marker: &FsPath) -> String {
    let marker = marker.to_string_lossy().replace('\'', "''");
    format!(
        "Start-Process -FilePath powershell.exe -ArgumentList @('-NoLogo','-NoProfile','-Command','Start-Sleep -Milliseconds 1500; Set-Content -LiteralPath ''{marker}'' done') -WindowStyle Hidden; Start-Sleep -Seconds 5"
    )
}

#[cfg(not(windows))]
fn terminal_timeout_process_tree_command(marker: &FsPath) -> String {
    format!(
        "(sleep 1.5; touch {}) & sleep 5",
        shell_single_quote(marker.to_string_lossy().as_ref())
    )
}

#[cfg(windows)]
fn terminal_background_process_tree_command(marker: &FsPath) -> String {
    let marker = marker.to_string_lossy().replace('\'', "''");
    format!(
        "Start-Process -FilePath powershell.exe -ArgumentList @('-NoLogo','-NoProfile','-Command','Start-Sleep -Milliseconds 1500; Set-Content -LiteralPath ''{marker}'' done') -WindowStyle Hidden; Write-Output done"
    )
}

#[cfg(not(windows))]
fn terminal_background_process_tree_command(marker: &FsPath) -> String {
    format!(
        "(sleep 1.5; touch {}) & echo done",
        shell_single_quote(marker.to_string_lossy().as_ref())
    )
}

#[cfg(not(windows))]
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
